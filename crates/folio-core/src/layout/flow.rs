//! The flow of a build (SPEC §7.1): its blocks and lines flattened into
//! what the page builder breaks.
//!
//! The flow is a sequence of *leaves*, the things a page holds whole (the
//! lines of paragraphs, and empty blocks), with a *boundary* before each
//! and one after the last. A boundary says what lies between two leaves:
//! the leading between lines, and where blocks end and start, their
//! padding, borders and spaces. It also says what a page break there costs.
//! Pages break only at boundaries, so a line is never split, and the page
//! builder decides by these costs alone.
//!
//! **Spaces.** `space-after` and `space-before` meet at a boundary and
//! collapse to the largest of the spaces that meet, unless padding or a
//! border separates them; a block's space lies outside its box. Half the
//! leading of a paragraph lies above its first line and below its last,
//! inside its block, so consecutive paragraphs are a leading apart where
//! no space separates them. At a page break the spaces and the leading of
//! the boundary are dropped, the padding and borders of the blocks that
//! end there stay on the page, and those of the blocks that start there
//! move to the next. The decoration of a block broken across pages is
//! sliced: its top border is drawn where it starts, its bottom border where
//! it ends.
//!
//! **Costs.** A break between the lines of a paragraph that leaves fewer
//! than `orphans` lines on the page or fewer than `widows` on the next
//! costs [`WIDOW`]; a break directly after a block with `keep-with-next`,
//! or inside one with `keep-together`, costs [`KEEP`]; inside a block with
//! `breakable: false`, [`UNBREAKABLE`]. A block with `break-before: page`
//! forces a break before it; `odd-page` and `even-page` also ask for a page
//! of that parity, and `column` breaks nothing in a body of one column. Costs add up, and they are costs, not rules: the page
//! builder takes the cheapest break and reports the constraints it had to
//! relax (SPEC §7.2).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};

use ecow::EcoVec;
use folio_component::{Abs, Color, Em, Paint, SourceRef, Value};

use crate::span_map::source_ref;
use folio_syntax::Span;
use folio_text::{Glyph, ItemKind, Shaped};

use super::cache::Cache;
use super::master::{BlockBox, BreakBefore};
use super::paragraph::{LineSlot, ParagraphLayout};
use super::{FaceMetrics, input_with, paragraph_style};
use crate::fonts::Fonts;
use crate::style::{ComputedStyle, Properties};
use crate::text::{Text, TextBlock, TextFlow, TextSource};

/// The cost of a break that leaves a widow or an orphan (TeX's
/// `\widowpenalty` and `\clubpenalty`).
pub const WIDOW: f64 = 150.0;
/// The cost of a break that separates a block with `keep-with-next` from
/// the next, or breaks one with `keep-together`: more than any page fill
/// costs.
pub const KEEP: f64 = 10_000.0;
/// The cost of a break inside a block with `breakable: false`.
pub const UNBREAKABLE: f64 = 100_000.0;

/// The width and offset of every paragraph's lines: the content width of
/// the block whose content it is, from the left of the body.
pub(super) fn slots(
    root: &TextBlock,
    props: &Properties,
    width: Abs,
    count: usize,
) -> Vec<LineSlot> {
    fn walk(block: &TextBlock, boxed: &BlockBox, props: &Properties, out: &mut [LineSlot]) {
        let (offset, width) = boxed.content();
        for child in &block.children {
            match child {
                TextFlow::Paragraph(i) => {
                    if let Some(slot) = out.get_mut(*i) {
                        *slot = LineSlot { offset, width };
                    }
                }
                TextFlow::Block(b) => walk(b, &boxed.child(&b.style, props, b.span), props, out),
            }
        }
    }
    let mut out = vec![
        LineSlot {
            offset: Abs::zero(),
            width,
        };
        count
    ];
    let root_box = BlockBox::of(&root.style, props, root.span, Abs::zero(), width);
    walk(root, &root_box, props, &mut out);
    out
}

/// A paragraph broken into lines, with what setting its lines needs.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct ParagraphInfo {
    /// The paragraph shaped.
    pub shaped: Arc<Shaped>,
    /// Its lines.
    pub layout: Arc<ParagraphLayout>,
    /// The slot its lines are placed in, from the left of the body.
    pub slot: LineSlot,
    /// The space between one line's descent and the next line's ascent.
    pub leading: Abs,
    /// The indent of the first line.
    pub indent: Abs,
    /// The fewest lines that may start a page.
    pub widows: usize,
    /// The fewest lines that may end a page.
    pub orphans: usize,
    /// How each segment of the paragraph is painted.
    pub segments: EcoVec<SegmentPaint>,
    /// Ranges of the text that are the text of a source node, in order.
    pub sources: EcoVec<TextSource>,
    /// The element whose content the paragraph is.
    pub span: Span,
}

/// How a segment of a paragraph is set.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct SegmentPaint {
    /// Its face, as an index into [`Fonts::faces`].
    pub face: Option<u32>,
    /// Its font size.
    pub size: Abs,
    /// The space added after every character.
    pub tracking: Abs,
    /// The paint of its glyphs.
    pub fill: Paint,
    /// The hyphens its lines need, inserted ones (`-`) and the explicit
    /// ones Polish repeats: each with its glyphs in the segment's font, in
    /// font units of the units per em they come with.
    pub hyphens: EcoVec<(char, EcoVec<Glyph>, u16)>,
}

impl ParagraphInfo {
    /// The source of the bytes `range` of the paragraph's text, as a glyph
    /// that shows them names it ([`folio_component::Glyph::source`]): the
    /// node the first byte comes from, with the bytes of its text that the
    /// range shows up to the node's end; or the whole of the paragraph's
    /// element where no node gave the text. An empty range, such as where a
    /// break inserts a hyphen, is a position in the node of the text before
    /// it.
    pub(super) fn source(&self, range: Range<usize>) -> (SourceRef, Range<u32>) {
        let at = range.start;
        let probe = if range.is_empty() {
            at.saturating_sub(1)
        } else {
            at
        };
        let i = self.sources.partition_point(|s| s.range.end <= probe);
        // An empty range stays empty, even where the node's text is not
        // the paragraph's as it is: it is a position, and the PDF writer
        // tells a hyphen a break inserts by it.
        let (span, within) = match self.sources.get(i) {
            Some(source) if source.range.start <= probe => {
                (source.span, source.within(range.clone()))
            }
            _ => (self.span, TextSource::WHOLE),
        };
        let within = if range.is_empty() {
            within.end..within.end
        } else {
            within
        };
        (source_ref(span), offset(within.start)..offset(within.end))
    }
}

fn offset(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// What a page holds whole.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) enum Leaf {
    /// A line of a paragraph.
    Line {
        /// The paragraph.
        paragraph: Arc<ParagraphInfo>,
        /// The index of the line.
        line: usize,
    },
    /// A block without lines, which is as tall as its padding and borders.
    Empty,
}

impl Leaf {
    /// How tall it is: a line from its ascent to its descent.
    pub(super) fn height(&self) -> Abs {
        match self {
            Self::Line { paragraph, line } => {
                let line = &paragraph.layout.lines[*line];
                line.ascent + line.descent
            }
            Self::Empty => Abs::zero(),
        }
    }

    /// A cheap fingerprint for looking it up: equal leaves of the same
    /// shared paragraph have equal ones.
    pub(super) fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            Self::Line { paragraph, line } => {
                (Arc::as_ptr(&paragraph.layout) as usize).hash(&mut hasher);
                (Arc::as_ptr(&paragraph.shaped) as usize).hash(&mut hasher);
                line.hash(&mut hasher);
            }
            Self::Empty => 0u8.hash(&mut hasher),
        }
        hasher.finish()
    }
}

/// One step of what lies between two leaves.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) enum Part {
    /// Leading: kept between leaves, dropped at a page break.
    Lead(Abs),
    /// A block's space: it collapses with the spaces it meets and is
    /// dropped at a page break.
    Space(Abs),
    /// Padding and borders: always kept.
    Solid(Abs),
    /// The innermost open block ends.
    Close,
    /// A block starts.
    Open(Arc<BlockBox>),
}

/// A constraint that a page break goes against.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(super) enum Constraint {
    /// It leaves fewer than `widows` lines of a paragraph on the next page.
    Widow(Span),
    /// It leaves fewer than `orphans` lines of a paragraph on the page.
    Orphan(Span),
    /// It separates a block with `keep-with-next` from the next.
    KeepWithNext(Span),
    /// It breaks a block with `keep-together`.
    KeepTogether(Span),
    /// It breaks a block with `breakable: false`.
    Unbreakable(Span),
}

/// What lies before a leaf, or after the last, and what a page break there
/// costs.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Boundary {
    /// What lies there, in order: the blocks that end, innermost first,
    /// then the blocks that start, outermost first.
    pub parts: EcoVec<Part>,
    /// The cost of a break here.
    pub penalty: f64,
    /// Whether a break here is forced.
    pub forced: bool,
    /// Whether the page after a forced break here must be odd (`true`) or
    /// even (`false`), if either.
    pub parity: Option<bool>,
    /// The constraints a break here goes against.
    pub constraints: EcoVec<Constraint>,
}

impl Hash for Boundary {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.parts.hash(state);
        self.penalty.to_bits().hash(state);
        self.forced.hash(state);
        self.parity.hash(state);
        self.constraints.hash(state);
    }
}

// Penalties are finite sums of the constants above, never NaN.
impl Eq for Boundary {}

/// How tall a run of parts is: kept, or at a page break.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(super) enum Walk {
    /// Between two leaves on one page: everything.
    Through,
    /// At the bottom of a page: the padding and borders of the blocks
    /// that end.
    Bottom,
    /// At the top of the next page: the padding and borders of the blocks
    /// that start.
    Top,
}

impl Boundary {
    /// Walks the parts as they lie on a page, calling `f` with each block
    /// that ends (`Close`, at the bottom of its box) or starts (`Open`, at
    /// the top of its box), and returns the height they take. Collapsed
    /// spaces lie outside the boxes they meet.
    pub(super) fn walk(&self, how: Walk, mut f: impl FnMut(&Part, Abs)) -> Abs {
        let mut y = Abs::zero();
        let mut pending = Abs::zero();
        // Blocks that start and wait for the spaces before their tops to
        // be settled.
        let mut opening: Vec<&Part> = Vec::new();
        let first_open = self
            .parts
            .iter()
            .position(|p| matches!(p, Part::Open(_)))
            .unwrap_or(self.parts.len());
        let parts = match how {
            Walk::Through => &self.parts[..],
            Walk::Bottom => &self.parts[..first_open],
            Walk::Top => &self.parts[first_open..],
        };
        let settle = |y: &mut Abs,
                      pending: &mut Abs,
                      opening: &mut Vec<&Part>,
                      f: &mut dyn FnMut(&Part, Abs)| {
            *y += *pending;
            *pending = Abs::zero();
            for part in opening.drain(..) {
                f(part, *y);
            }
        };
        for part in parts {
            match part {
                Part::Space(s) => {
                    if how == Walk::Through {
                        pending = pending.max(*s);
                    }
                }
                Part::Lead(h) => {
                    if how == Walk::Through && *h != Abs::zero() {
                        settle(&mut y, &mut pending, &mut opening, &mut f);
                        y += *h;
                    }
                }
                Part::Solid(h) => {
                    if *h != Abs::zero() {
                        settle(&mut y, &mut pending, &mut opening, &mut f);
                        y += *h;
                    }
                }
                Part::Close => f(part, y),
                Part::Open(_) => opening.push(part),
            }
        }
        settle(&mut y, &mut pending, &mut opening, &mut f);
        y
    }

    /// The height between the leaves on one page.
    pub(super) fn gap(&self) -> Abs {
        self.walk(Walk::Through, |_, _| {})
    }

    /// How many blocks start here.
    pub(super) fn opens(&self) -> usize {
        self.parts
            .iter()
            .filter(|p| matches!(p, Part::Open(_)))
            .count()
    }
}

/// A leaf, with the boundary before it and the blocks it is in.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct Item {
    /// What lies before it.
    pub before: Boundary,
    /// The leaf.
    pub leaf: Leaf,
    /// The blocks it is in, outermost first.
    pub stack: EcoVec<Arc<BlockBox>>,
}

/// Where a leaf comes from, for the pages' line lists.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(super) enum Origin {
    /// Line `line` of paragraph `paragraph` of the build's text.
    Line {
        /// The paragraph.
        paragraph: usize,
        /// The line.
        line: usize,
    },
    /// An empty block.
    Empty,
}

/// The flow of a build.
#[derive(Debug, Clone)]
pub(super) struct Flow {
    /// The leaves, with what lies before each; at least one.
    pub items: Vec<Item>,
    /// What lies after the last leaf.
    pub end: Boundary,
    /// Where each leaf comes from.
    pub origins: Vec<Origin>,
}

impl Flow {
    /// Flattens a build's blocks and lines. `layouts` and `slots` hold the
    /// lines and line slots of each paragraph of `text`; `caches` memoise
    /// the hyphens they need and share what earlier flows built.
    pub(super) fn new(
        text: &Text,
        caches: Caches<'_>,
        layouts: &[Arc<ParagraphLayout>],
        slots: &[LineSlot],
        props: &Properties,
        fonts: &Fonts,
        width: Abs,
    ) -> Self {
        let Caches { cache, shared, now } = caches;
        let mut builder = Builder {
            text,
            cache,
            shared,
            now,
            layouts,
            slots,
            props,
            metrics: FaceMetrics::new(fonts),
            stack: EcoVec::new(),
            pending: Vec::new(),
            prev: None,
            items: Vec::new(),
            origins: Vec::new(),
        };
        let root = &text.root;
        let root_box = shared.block(
            Arc::new(BlockBox::of(
                &root.style,
                props,
                root.span,
                Abs::zero(),
                width,
            )),
            now,
        );
        builder.block(root, root_box);
        let end = builder.boundary(None);
        Flow {
            items: builder.items,
            end,
            origins: builder.origins,
        }
    }

    /// The boundary before leaf `i`, or after the last for `i == len`.
    pub(super) fn boundary(&self, i: usize) -> &Boundary {
        self.items.get(i).map_or(&self.end, |item| &item.before)
    }
}

/// The paragraphs and blocks of recent flows, so that one equal to a
/// paragraph or block of an earlier flow is the same `Arc`. A flow is built
/// anew after every edit; with its unchanged parts shared, the page cache
/// compares the leaves and boundaries a page read by pointer rather than
/// by content. Kept with the page cache, by its clock.
#[derive(Default)]
pub(super) struct Shared {
    paragraphs: Interned<ParagraphInfo>,
    blocks: Interned<BlockBox>,
}

/// Values by a hash of their content, each with the clock when a flow last
/// used it.
type Interned<T> = Mutex<HashMap<u64, Vec<(Arc<T>, u64)>>>;

/// The caches a flow is built with.
#[derive(Copy, Clone)]
pub(super) struct Caches<'a> {
    /// Memoises the hyphens that lines need.
    pub cache: &'a Cache,
    /// Shares paragraphs and blocks with earlier flows.
    pub shared: &'a Shared,
    /// The page cache's clock, for `shared`.
    pub now: u64,
}

impl Shared {
    /// A paragraph, as the `Arc` of an equal one from a recent flow if
    /// there is one. `now` is the page cache's clock.
    pub(super) fn paragraph(&self, info: ParagraphInfo, now: u64) -> Arc<ParagraphInfo> {
        // The shaped paragraph and its lines are shared by their caches,
        // so they are keyed by pointer, with where the paragraph is; the
        // rest is compared only among paragraphs with that key. Equality
        // still compares the shared parts by content where the pointers
        // differ.
        let mut hasher = Fx::default();
        (Arc::as_ptr(&info.shaped) as usize).hash(&mut hasher);
        (Arc::as_ptr(&info.layout) as usize).hash(&mut hasher);
        info.slot.hash(&mut hasher);
        info.span.hash(&mut hasher);
        intern(&self.paragraphs, hasher.finish(), info, now)
    }

    /// A block's box, as the `Arc` of an equal one from a recent flow if
    /// there is one.
    pub(super) fn block(&self, block: Arc<BlockBox>, now: u64) -> Arc<BlockBox> {
        let mut hasher = Fx::default();
        block.hash(&mut hasher);
        let key = hasher.finish();
        let mut map = lock(&self.blocks);
        let bucket = map.entry(key).or_default();
        if let Some((shared, used)) = bucket.iter_mut().find(|(b, _)| *b == block) {
            *used = now;
            return shared.clone();
        }
        bucket.push((block.clone(), now));
        block
    }

    /// Forgets what no flow has used in the last `age` ticks of the clock.
    pub(super) fn sweep(&self, now: u64, age: u64) {
        fn sweep<T>(map: &Interned<T>, now: u64, age: u64) {
            let mut map = lock(map);
            for bucket in map.values_mut() {
                bucket.retain(|(_, used)| used + age >= now);
            }
            map.retain(|_, bucket| !bucket.is_empty());
        }
        sweep(&self.paragraphs, now, age);
        sweep(&self.blocks, now, age);
    }
}

/// The `Arc` of a value equal to `value` in `map` under `key`, or of
/// `value`, now kept there.
fn intern<T: Eq>(map: &Interned<T>, key: u64, value: T, now: u64) -> Arc<T> {
    let mut map = lock(map);
    let bucket = map.entry(key).or_default();
    if let Some((shared, used)) = bucket.iter_mut().find(|(v, _)| **v == value) {
        *used = now;
        return shared.clone();
    }
    let value = Arc::new(value);
    bucket.push((value.clone(), now));
    value
}

/// A fast hasher for hashes kept in memory, such as change detection and
/// lookup keys, never written out: each word is mixed in by a rotation,
/// an exclusive or and a multiplication by an odd constant, as rustc's
/// `FxHasher` does, from a state that is not zero. Each step is a
/// bijection of the state for a given word and of the word for a given
/// state, so two inputs of the same shape that differ in one word always
/// hash differently; bytes are mixed in with their length, so byte strings
/// of different lengths do not run together.
#[derive(Debug, Copy, Clone)]
pub(super) struct Fx(u64);

impl Default for Fx {
    fn default() -> Self {
        Self(0x243f_6a88_85a3_08d3)
    }
}

impl Fx {
    const K: u64 = 0x517c_c1b7_2722_0a95;

    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(Self::K);
    }
}

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        self.mix(bytes.len() as u64);
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.mix(u64::from_le_bytes(chunk.try_into().expect("eight bytes")));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.mix(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.mix(u64::from(n));
    }

    fn write_u16(&mut self, n: u16) {
        self.mix(u64::from(n));
    }

    fn write_u32(&mut self, n: u32) {
        self.mix(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the boundary being built has seen since the last leaf.
enum Event {
    Close(Arc<BlockBox>),
    Open(Arc<BlockBox>),
}

struct Builder<'a> {
    text: &'a Text,
    cache: &'a Cache,
    shared: &'a Shared,
    /// The page cache's clock, for [`Shared`].
    now: u64,
    layouts: &'a [Arc<ParagraphLayout>],
    slots: &'a [LineSlot],
    props: &'a Properties,
    metrics: FaceMetrics<'a>,
    /// The blocks open, outermost first.
    stack: EcoVec<Arc<BlockBox>>,
    /// The blocks that closed and opened since the last leaf.
    pending: Vec<Event>,
    /// The last leaf.
    prev: Option<Leaf>,
    items: Vec<Item>,
    origins: Vec<Origin>,
}

impl Builder<'_> {
    fn block(&mut self, block: &TextBlock, boxed: Arc<BlockBox>) {
        self.stack.push(boxed.clone());
        self.pending.push(Event::Open(boxed.clone()));
        let before = self.items.len();
        for child in &block.children {
            match child {
                TextFlow::Paragraph(i) => self.paragraph(*i),
                TextFlow::Block(b) => {
                    let child = boxed.child(&b.style, self.props, b.span);
                    let child = self.shared.block(child, self.now);
                    self.block(b, child);
                }
            }
        }
        if self.items.len() == before {
            self.leaf(Leaf::Empty, Origin::Empty);
        }
        self.stack.pop();
        self.pending.push(Event::Close(boxed));
    }

    fn paragraph(&mut self, index: usize) {
        let text = self.text;
        let paragraph = &text.paragraphs[index];
        let layout = self.layouts[index].clone();
        if layout.lines.is_empty() {
            return;
        }
        let input = input_with(paragraph, self.props, &mut self.metrics);
        let style = paragraph_style(&paragraph.style, self.props);
        let count = |name: &str| match paragraph.style.value(self.props, self.props.builtin(name)) {
            Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(0),
            _ => 0,
        };
        // The hyphens each segment's lines need: an inserted one at a
        // line's end, and the explicit one a line repeats at its start.
        let segments = &paragraph.shaped.paragraph.segments;
        let mut needs: Vec<Vec<char>> = vec![Vec::new(); segments.len()];
        let segment_at = |offset: usize| {
            segments
                .partition_point(|s| s.range.end < offset)
                .min(segments.len().saturating_sub(1))
        };
        let text = paragraph.shaped.text();
        for line in &layout.lines {
            let mut need = |at: usize, c: char| {
                let list = &mut needs[segment_at(at)];
                if !list.contains(&c) {
                    list.push(c);
                }
            };
            if line.hyphen {
                need(line.content_end, '-');
            }
            if line.repeated_hyphen
                && let Some(c) = text[..line.range.start].chars().next_back()
            {
                need(line.range.start, c);
            }
        }
        let cache = self.cache;
        let paints = paragraph
            .segments
            .iter()
            .zip(&input.segments)
            .enumerate()
            .map(|(i, (seg, metrics))| SegmentPaint {
                face: seg.face.and_then(|f| u32::try_from(f).ok()),
                size: metrics.size,
                tracking: metrics.tracking,
                fill: fill(&seg.style, self.props),
                hyphens: needs
                    .get(i)
                    .into_iter()
                    .flatten()
                    .filter_map(|&c| {
                        let (glyphs, units) = cache.hyphen(segments.get(i)?, c)?;
                        Some((c, glyphs, units))
                    })
                    .collect(),
            })
            .collect();
        let info = self.shared.paragraph(
            ParagraphInfo {
                shaped: paragraph.shaped.clone(),
                layout: layout.clone(),
                slot: self.slots[index],
                leading: style.leading,
                indent: style.indent,
                widows: count("widows"),
                orphans: count("orphans"),
                segments: paints,
                sources: paragraph.sources.clone(),
                span: paragraph.span,
            },
            self.now,
        );
        for line in 0..layout.lines.len() {
            self.leaf(
                Leaf::Line {
                    paragraph: info.clone(),
                    line,
                },
                Origin::Line {
                    paragraph: index,
                    line,
                },
            );
        }
    }

    fn leaf(&mut self, leaf: Leaf, origin: Origin) {
        let before = self.boundary(Some(&leaf));
        self.items.push(Item {
            before,
            leaf: leaf.clone(),
            stack: self.stack.clone(),
        });
        self.origins.push(origin);
        self.prev = Some(leaf);
    }

    /// The boundary between the last leaf and `next`: what closed and
    /// opened since, and its cost.
    fn boundary(&mut self, next: Option<&Leaf>) -> Boundary {
        let events = std::mem::take(&mut self.pending);
        let mut out = Boundary::default();
        let prev = self.prev.as_ref();

        // Two lines of one paragraph: the next line of the same shared
        // paragraph. Equal paragraphs share one `Arc` (`Shared`), so two
        // paragraphs in a row may too; the line numbers tell them apart.
        if let (
            Some(Leaf::Line {
                paragraph: a,
                line: i,
            }),
            Some(Leaf::Line {
                paragraph: b,
                line: j,
            }),
        ) = (prev, next)
            && Arc::ptr_eq(a, b)
            && *j == *i + 1
            && events.is_empty()
        {
            let lines = &a.layout.lines;
            let (above, below) = (&lines[*i], &lines[*j]);
            let lead = below.baseline - above.baseline - above.descent - below.ascent;
            out.parts.push(Part::Lead(lead.max(Abs::zero())));
            let before = *j;
            let after = lines.len() - *j;
            if before < a.orphans {
                out.penalty += WIDOW;
                out.constraints.push(Constraint::Orphan(a.span));
            }
            if after < a.widows {
                out.penalty += WIDOW;
                out.constraints.push(Constraint::Widow(a.span));
            }
            self.keeps(&mut out);
            return out;
        }

        if let Some(Leaf::Line { paragraph, .. }) = prev {
            out.parts.push(Part::Lead(paragraph.leading / 2.0));
        }
        for event in &events {
            match event {
                Event::Close(b) => {
                    out.parts.push(Part::Solid(b.inset().bottom));
                    out.parts.push(Part::Close);
                    out.parts.push(Part::Space(b.space_after));
                    if b.keep_with_next && next.is_some() {
                        out.penalty += KEEP;
                        out.constraints.push(Constraint::KeepWithNext(b.span));
                    }
                }
                Event::Open(b) => {
                    out.parts.push(Part::Space(b.space_before));
                    out.parts.push(Part::Open(b.clone()));
                    out.parts.push(Part::Solid(b.inset().top));
                    if prev.is_some() {
                        match b.break_before {
                            BreakBefore::Auto => {}
                            BreakBefore::Page => out.forced = true,
                            BreakBefore::OddPage => {
                                out.forced = true;
                                out.parity = Some(true);
                            }
                            BreakBefore::EvenPage => {
                                out.forced = true;
                                out.parity = Some(false);
                            }
                        }
                    }
                }
            }
        }
        if let Some(Leaf::Line { paragraph, .. }) = next {
            out.parts.push(Part::Lead(paragraph.leading / 2.0));
        }
        if prev.is_some() && next.is_some() {
            self.keeps(&mut out);
        }
        out
    }

    /// Adds the cost of breaking the blocks that stay open across a
    /// boundary: those open both before and after it. `self.stack` holds
    /// the blocks open after it; the ones that opened at it are not yet
    /// part of the stack of a leaf before it.
    fn keeps(&self, out: &mut Boundary) {
        let opened = out.opens();
        let across = self.stack.len().saturating_sub(opened);
        let Some(prev) = self.items.last() else {
            return;
        };
        for (a, b) in prev.stack.iter().zip(&self.stack[..across]) {
            if !Arc::ptr_eq(a, b) {
                break;
            }
            if !b.breakable {
                out.penalty += UNBREAKABLE;
                out.constraints.push(Constraint::Unbreakable(b.span));
            } else if b.keep_together {
                out.penalty += KEEP;
                out.constraints.push(Constraint::KeepTogether(b.span));
            }
        }
    }
}

/// The paint of text in a style: its `color`.
fn fill(style: &ComputedStyle, props: &Properties) -> Paint {
    match style.value(props, props.builtin("color")) {
        Some(Value::Color(color)) => Paint::Solid(*color),
        _ => Paint::Solid(Color::BLACK),
    }
}

/// A length in font units of a face at a size, as ems.
pub(super) fn em(units: i32, units_per_em: u16) -> Em {
    Em::new(f64::from(units) / f64::from(units_per_em.max(1)))
}

/// Whether an item of a shaped paragraph holds glyphs.
pub(super) fn glyphs(kind: &ItemKind) -> Option<(&EcoVec<Glyph>, u16)> {
    match kind {
        ItemKind::Glyphs {
            glyphs,
            units_per_em,
        } => Some((glyphs, *units_per_em)),
        _ => None,
    }
}
