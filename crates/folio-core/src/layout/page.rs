//! The page builder (SPEC §7.1, §2.3): a build's flow broken into pages of
//! one master, strictly forward, and each page assembled into a fragment.
//!
//! **Breaking.** Each page is filled from where the previous one ended. Its
//! builder considers every boundary at which the page could end, from its
//! first leaf until the content would no longer fit in the body, and takes
//! the cheapest: the cost of a break is how empty it leaves the page
//! ([`FILL`] times the square of the empty fraction) plus the penalties of
//! the constraints it goes against ([`super::flow`]). Ties go to the later
//! break, a forced break ends the page where it stands, and the end of the
//! flow ends the last. No page is reconsidered once it is broken, which is
//! what makes the page cache and, later, the convergence short-circuit
//! sound (§2.3). A leaf that does not fit on an empty page is placed anyway,
//! overflowing it, with a warning.
//!
//! **Fragments.** A page is a fragment of the master's size holding one
//! fragment for its body, which holds a fragment for each block on the
//! page, nested as the blocks are, with its background and borders, its
//! lines as glyph runs, its baselines and its keep data.
//!
//! **The cache** ([`PageCache`]): each page, keyed on the master, whether it
//! is the first, and the leaves and boundaries its builder read, from its
//! first leaf to the boundary after the last it considered. A page whose
//! content is unchanged, wherever an edit has moved it, is found there and
//! not broken or assembled again. The full two-part flow state (§2.3) comes
//! with page furniture and floats.

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ecow::{EcoVec, eco_format};
use folio_component::{Abs, Fragment, FragmentItem, Path, Point, Shape, Size, SourceRef, Stroke};
use folio_syntax::Span;

use super::flow::{Boundary, Constraint, Flow, Item, KEEP, Leaf, Origin, Part, Walk};
use super::lines;
use super::master::{BlockBox, PageMaster};
use super::set::set_line;
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::fonts::fonts;
use crate::pipeline::step_styled;
use crate::span_map::source_ref;
use crate::text::text;

/// How much a page's emptiness costs: an empty page would cost this much.
pub const FILL: f64 = 1_000.0;

/// How many calls of [`pages()`] a page is kept without being asked for.
const MAX_AGE: u64 = 16;

/// Lengths closer than this, in points, are equal.
const EPSILON: f64 = 1e-6;

/// A build's pages.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Pages {
    /// The master every page is made from.
    pub master: PageMaster,
    /// The pages, at least one.
    pub pages: EcoVec<Page>,
}

/// One page.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Page {
    /// The page: a fragment of the master's size, holding the body's
    /// fragment at the body's position.
    pub fragment: Fragment,
    /// The lines on the page, in order.
    pub lines: EcoVec<LineRef>,
}

/// A line of a build's text.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct LineRef {
    /// The paragraph, as an index into the build's text.
    pub paragraph: usize,
    /// The line of the paragraph.
    pub line: usize,
}

/// A page as its builder made it: it does not depend on where in the flow
/// the page is.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct PageOut {
    fragment: Fragment,
    /// How many leaves the page holds.
    leaves: usize,
    /// The constraints the break after the page goes against.
    relaxed: EcoVec<Constraint>,
    /// The source of the leaf that overflows the page, and by how much.
    overflow: Option<(Span, Abs)>,
}

/// The pages of a build, or `None` when the document could not be styled.
/// Reports constraints that a page break had to relax, and content that
/// does not fit on a page.
#[salsa::tracked(returns(ref))]
pub fn pages(db: &dyn Db, build: Build<'_>) -> Option<Pages> {
    let lines = lines(db, build).as_ref()?;
    let text = text(db, build).as_ref()?;
    let styled = step_styled(db, build)?;
    let props = &styled.properties;
    let fonts = fonts(db, build.project(db));
    let (master, requested) = PageMaster::checked(&styled.page, props);
    if let Some(requested) = requested {
        report_page_size(db, build, master, requested, text.root.span);
    }
    let flow = Flow::new(
        text,
        db.layout_cache(),
        &lines.paragraphs,
        &lines.slots,
        props,
        fonts,
        master.body.size.width,
    );

    // The page and its body are boxes of the document.
    let root = source_ref(text.root.span);
    let cache = db.page_cache();
    let now = cache.tick();
    let mut pages = EcoVec::new();
    let mut start = 0;
    while start < flow.items.len() {
        // `break-before: odd-page` and `even-page` pad with a blank page.
        if start > 0
            && let Some(odd) = flow.boundary(start).parity
            && (pages.len() % 2 == 0) != odd
        {
            pages.push(Page {
                fragment: dress(&blank(&master), root),
                lines: EcoVec::new(),
            });
        }
        let out = cache.page(&flow, master, start, start == 0, now);
        report(db, &out);
        let fragment = dress(&out.fragment, root);
        let lines = flow.origins[start..start + out.leaves]
            .iter()
            .filter_map(|origin| match *origin {
                Origin::Line { paragraph, line } => Some(LineRef { paragraph, line }),
                Origin::Empty => None,
            })
            .collect();
        pages.push(Page { fragment, lines });
        start += out.leaves;
    }
    cache.sweep(now);
    Some(Pages { master, pages })
}

/// A page and its body as boxes of the document's root, `root`. Pages come
/// from the cache whatever document they were made for, so they are
/// dressed afterwards.
fn dress(page: &Fragment, root: SourceRef) -> Fragment {
    let mut page = page.clone().with_source(root);
    for (_, item) in page.items.make_mut() {
        if let FragmentItem::Group(body) = item {
            body.source = root;
        }
    }
    page
}

/// A page without content.
fn blank(master: &PageMaster) -> Fragment {
    let mut page = Fragment::new(master.size);
    page.push_fragment(master.body.origin, Fragment::new(master.body.size));
    page
}

/// Reports a page size outside PDF's limits (SPEC §11.1), at the last
/// `page-size` declaration of the cascade, or at the document when the
/// size is the default.
fn report_page_size(
    db: &dyn Db,
    build: Build<'_>,
    master: PageMaster,
    requested: Size,
    root: Span,
) {
    let cascade = crate::style::cascade(db, build);
    let page_size = cascade.properties.builtin("page-size");
    let span = cascade
        .rules
        .iter()
        .flat_map(|rule| &rule.declarations)
        .filter(|decl| decl.prop == page_size)
        .last()
        .map_or(root, |decl| decl.span);
    let side = |a: Abs| {
        if a.to_pt().abs() < 1e6 {
            eco_format!("{:.1}pt", a.to_pt())
        } else {
            eco_format!("{:e}pt", a.to_pt())
        }
    };
    diag::report(
        db,
        Diagnostic::warning(
            span,
            eco_format!(
                "this page size, {} by {}, is outside what PDF allows; the pages are {} by {}",
                side(requested.width),
                side(requested.height),
                side(master.size.width),
                side(master.size.height),
            ),
        )
        .with_hint("a side of a PDF page is 3pt to 14,400pt (200in)"),
    );
}

/// Reports what a page relaxed or could not fit.
fn report(db: &dyn Db, out: &PageOut) {
    for constraint in &out.relaxed {
        let (span, message, hint) = match *constraint {
            Constraint::Widow(span) => (
                span,
                "a page break leaves fewer lines of this paragraph at the top of a page \
                 than `widows` asks for",
                "no better break was found; reword the text or allow fewer lines with `widows`",
            ),
            Constraint::Orphan(span) => (
                span,
                "a page break leaves fewer lines of this paragraph at the foot of a page \
                 than `orphans` asks for",
                "no better break was found; reword the text or allow fewer lines with `orphans`",
            ),
            Constraint::KeepWithNext(span) => (
                span,
                "a page break separates this block from the next despite `keep-with-next`",
                "the blocks kept together do not fit on one page",
            ),
            Constraint::KeepTogether(span) => (
                span,
                "a page break falls inside this block despite `keep-together`",
                "the block does not fit on one page",
            ),
            Constraint::Unbreakable(span) => (
                span,
                "a page break falls inside this block despite `breakable: false`",
                "the block does not fit on one page",
            ),
        };
        diag::report(db, Diagnostic::warning(span, message).with_hint(hint));
    }
    if let Some((span, by)) = out.overflow {
        diag::report(
            db,
            Diagnostic::warning(
                span,
                eco_format!(
                    "this does not fit on a page: it is {:.1}pt taller than the space left \
                     for it",
                    by.to_pt()
                ),
            )
            .with_hint("make it smaller, or the page's margins"),
        );
    }
}

/// Where a page ends.
struct Break {
    /// The leaf after the page's last.
    end: usize,
    /// The leaf after the last whose boundary after it the builder read.
    read: usize,
    /// Whether the break is a forced one or the end of the flow.
    forced: bool,
    /// How far the page's content is taller than the body.
    overflow: Abs,
}

/// Finds where the page that starts at leaf `start` ends.
fn break_page(flow: &Flow, master: &PageMaster, start: usize, first: bool) -> Break {
    let height = master.body.size.height;
    let n = flow.items.len();
    let before = &flow.items[start].before;
    let mut y = if first {
        before.gap()
    } else {
        before.walk(Walk::Top, |_, _| {})
    };
    let mut best: Option<(usize, f64)> = None;
    let mut k = start;
    loop {
        y += flow.items[k].leaf.height();
        let boundary = flow.boundary(k + 1);
        let bottom = y + boundary.walk(Walk::Bottom, |_, _| {});
        let fits = bottom.to_pt() <= height.to_pt() + EPSILON;
        if !fits && k > start {
            let (end, _) = best.expect("a break before the leaf that overflows");
            return Break {
                end,
                read: k + 1,
                forced: false,
                overflow: Abs::zero(),
            };
        }
        let overflow = if fits { Abs::zero() } else { bottom - height };
        if k + 1 == n || boundary.forced {
            return Break {
                end: k + 1,
                read: k + 1,
                forced: true,
                overflow,
            };
        }
        let empty = ((height - bottom).to_pt() / height.to_pt()).max(0.0);
        let cost = FILL * empty * empty + boundary.penalty;
        if best.is_none_or(|(_, c)| cost <= c) {
            best = Some((k + 1, cost));
        }
        if !fits {
            // The first leaf alone overflows the page.
            return Break {
                end: k + 1,
                read: k + 1,
                forced: false,
                overflow,
            };
        }
        y += boundary.gap();
        k += 1;
    }
}

/// Breaks and assembles the page that starts at leaf `start`.
fn layout_page(flow: &Flow, master: &PageMaster, start: usize, first: bool) -> (PageOut, usize) {
    let found = break_page(flow, master, start, first);
    let fragment = assemble(flow, master, start, found.end, first);
    let relaxed = if found.forced {
        EcoVec::new()
    } else {
        flow.boundary(found.end).constraints.clone()
    };
    let overflow = (found.overflow > Abs::zero()).then(|| {
        let item = &flow.items[start];
        let span = match &item.leaf {
            Leaf::Line { paragraph, .. } => paragraph.span,
            Leaf::Empty => item.stack.last().map_or(Span::detached(), |b| b.span),
        };
        (span, found.overflow)
    });
    let out = PageOut {
        fragment,
        leaves: found.end - start,
        relaxed,
        overflow,
    };
    (out, found.read)
}

/// A block open on the page being assembled.
struct OpenBox {
    block: Arc<BlockBox>,
    /// The top of its box on the page, from the top of the body.
    top: Abs,
    /// Whether it started on an earlier page.
    continued: bool,
    /// Its content, positioned from the top-left of the body.
    items: Vec<(Point, FragmentItem)>,
    /// Its first and last baselines, from the top of the body.
    baselines: Option<(Abs, Abs)>,
}

/// A page being assembled.
struct Assembler {
    open: Vec<OpenBox>,
    /// The body's content.
    items: Vec<(Point, FragmentItem)>,
    baselines: Option<(Abs, Abs)>,
}

impl Assembler {
    /// Handles a block that ends or starts at `y`.
    fn event(&mut self, part: &Part, y: Abs) {
        match part {
            Part::Open(block) => self.open.push(OpenBox {
                block: block.clone(),
                top: y,
                continued: false,
                items: Vec::new(),
                baselines: None,
            }),
            Part::Close => self.close(y, false),
            _ => {}
        }
    }

    /// Ends the innermost block at `y`: where its box ends, or the foot of
    /// the page's content when it continues on the next page.
    fn close(&mut self, y: Abs, continues: bool) {
        let Some(open) = self.open.pop() else {
            return;
        };
        let block = &open.block;
        let top = open.top;
        let height = (y - top).max(Abs::zero());
        let size = Size::new(block.width, height);
        let mut fragment = Fragment::new(size).with_source(source_ref(block.span));
        decorate(&mut fragment, block, !open.continued, !continues);
        let origin = Point::new(block.x, top);
        for (pos, item) in open.items {
            fragment.push(Point::new(pos.x - origin.x, pos.y - origin.y), item);
        }
        if let Some((first, last)) = open.baselines {
            fragment.first_baseline = first - top;
            fragment.last_baseline = last - top;
        }
        fragment.keep_with_next = block.keep_with_next && !continues;
        if fragment.keep_with_next {
            fragment.break_penalty_after = KEEP as i32;
        }
        self.add(origin, FragmentItem::Group(fragment));
    }

    /// Adds an item to the innermost open block, or to the body.
    fn add(&mut self, pos: Point, item: FragmentItem) {
        match self.open.last_mut() {
            Some(open) => open.items.push((pos, item)),
            None => self.items.push((pos, item)),
        }
    }

    /// Records a baseline in every open block and the body.
    fn baseline(&mut self, y: Abs) {
        let mark = |b: &mut Option<(Abs, Abs)>| {
            *b = Some(b.map_or((y, y), |(first, _)| (first, y)));
        };
        for open in &mut self.open {
            mark(&mut open.baselines);
        }
        mark(&mut self.baselines);
    }
}

/// Assembles the page of leaves `start..end`.
fn assemble(flow: &Flow, master: &PageMaster, start: usize, end: usize, first: bool) -> Fragment {
    let mut asm = Assembler {
        open: Vec::new(),
        items: Vec::new(),
        baselines: None,
    };
    let mut y = Abs::zero();
    let head = &flow.items[start];
    if first {
        y += head.before.walk(Walk::Through, |p, at| asm.event(p, at));
    } else {
        let continued = head.stack.len().saturating_sub(head.before.opens());
        for block in &head.stack[..continued] {
            asm.open.push(OpenBox {
                block: block.clone(),
                top: Abs::zero(),
                continued: true,
                items: Vec::new(),
                baselines: None,
            });
        }
        y += head.before.walk(Walk::Top, |p, at| asm.event(p, y + at));
    }
    for k in start..end {
        let item: &Item = &flow.items[k];
        if let Leaf::Line { paragraph, line } = &item.leaf {
            let baseline = y + paragraph.layout.lines[*line].ascent;
            let mut runs = Vec::new();
            set_line(paragraph, *line, baseline, &mut runs);
            for (pos, run) in runs {
                asm.add(pos, run);
            }
            asm.baseline(baseline);
        }
        y += item.leaf.height();
        if k + 1 < end {
            let base = y;
            y += flow
                .boundary(k + 1)
                .walk(Walk::Through, |p, at| asm.event(p, base + at));
        }
    }
    let base = y;
    y += flow
        .boundary(end)
        .walk(Walk::Bottom, |p, at| asm.event(p, base + at));
    while !asm.open.is_empty() {
        asm.close(y, true);
    }

    let body = master.body;
    let mut content = Fragment::new(body.size);
    for (pos, item) in asm.items {
        content.push(pos, item);
    }
    if let Some((first, last)) = asm.baselines {
        content.first_baseline = first;
        content.last_baseline = last;
    }
    let mut page = Fragment::new(master.size);
    page.push_fragment(body.origin, content);
    page
}

/// Draws a block's background and borders into its fragment: the top
/// border only where the block starts, the bottom one only where it ends.
fn decorate(fragment: &mut Fragment, block: &BlockBox, starts: bool, ends: bool) {
    let source = source_ref(block.span);
    let size = fragment.size;
    if let Some(paint) = &block.background {
        fragment.push(
            Point::zero(),
            FragmentItem::Shape {
                shape: Shape::filled(Path::rect(size.width, size.height), paint.clone()),
                source,
            },
        );
    }
    let (w, h) = (size.width, size.height);
    let mut line = |stroke: &Stroke, from: Point, to: Point| {
        let mut path = Path::new();
        path.move_to(from);
        path.line_to(to);
        fragment.push(
            Point::zero(),
            FragmentItem::Shape {
                shape: Shape::stroked(path, stroke.clone()),
                source,
            },
        );
    };
    let half = |s: &Stroke| s.thickness / 2.0;
    if starts && let Some(s) = &block.border.top {
        line(s, Point::new(Abs::zero(), half(s)), Point::new(w, half(s)));
    }
    if ends && let Some(s) = &block.border.bottom {
        line(
            s,
            Point::new(Abs::zero(), h - half(s)),
            Point::new(w, h - half(s)),
        );
    }
    if let Some(s) = &block.border.left {
        line(s, Point::new(half(s), Abs::zero()), Point::new(half(s), h));
    }
    if let Some(s) = &block.border.right {
        line(
            s,
            Point::new(w - half(s), Abs::zero()),
            Point::new(w - half(s), h),
        );
    }
}

/// Pages broken and assembled, memoised.
///
/// The key is what decides a page: the master, whether it is the first page
/// (whose leading spaces are kept), and the leaves and boundaries its
/// builder read, from its first leaf to the boundary after the last leaf it
/// considered. Pages are found by their master, their first-page flag and
/// their first leaf, then checked against the rest of the key. Shared by
/// every handle of the database.
#[derive(Default)]
pub struct PageCache {
    entries: Mutex<HashMap<(PageMaster, bool, u64), Vec<Entry>>>,
    /// Counts the calls of `pages`.
    clock: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

struct Entry {
    /// The items the builder read.
    items: Vec<Item>,
    /// The boundary after the last leaf, when the builder read it.
    end: Option<Boundary>,
    out: Arc<PageOut>,
    /// The clock when it was last asked for.
    used: u64,
}

/// How often the cache had a page.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct PageStats {
    /// Pages that were in the cache.
    pub hits: u64,
    /// Pages that were broken and assembled.
    pub misses: u64,
}

impl PageCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a call of `pages`.
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Forgets the pages that no call has asked for in a while.
    fn sweep(&self, now: u64) {
        if now.is_multiple_of(4) {
            let mut entries = lock(&self.entries);
            for bucket in entries.values_mut() {
                bucket.retain(|e| e.used + MAX_AGE >= now);
            }
            entries.retain(|_, bucket| !bucket.is_empty());
        }
    }

    /// The page that starts at leaf `start` of a flow: memoised, or made
    /// now.
    fn page(
        &self,
        flow: &Flow,
        master: PageMaster,
        start: usize,
        first: bool,
        now: u64,
    ) -> Arc<PageOut> {
        let key = (master, first, flow.items[start].leaf.fingerprint());
        if let Some(bucket) = lock(&self.entries).get_mut(&key)
            && let Some(entry) = bucket.iter_mut().find(|e| e.matches(flow, start))
        {
            entry.used = now;
            self.hits.fetch_add(1, Ordering::Relaxed);
            return entry.out.clone();
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let (out, read) = layout_page(flow, &master, start, first);
        let out = Arc::new(out);
        let n = flow.items.len();
        let stop = (read + 1).min(n);
        let entry = Entry {
            items: flow.items[start..stop].to_vec(),
            end: (read == n).then(|| flow.end.clone()),
            out: out.clone(),
            used: now,
        };
        lock(&self.entries).entry(key).or_default().push(entry);
        out
    }

    /// How often the cache had a page since it was made.
    pub fn stats(&self) -> PageStats {
        PageStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// The number of pages held.
    pub fn len(&self) -> usize {
        lock(&self.entries).values().map(Vec::len).sum()
    }

    /// Whether no page is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Entry {
    /// Whether the flow from leaf `start` on has what this page's builder
    /// read.
    fn matches(&self, flow: &Flow, start: usize) -> bool {
        let stop = start + self.items.len();
        if stop > flow.items.len() || flow.items[start..stop] != self.items[..] {
            return false;
        }
        match &self.end {
            Some(end) => stop == flow.items.len() && flow.end == *end,
            None => true,
        }
    }
}

impl Debug for PageCache {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageCache")
            .field("pages", &self.len())
            .field("stats", &self.stats())
            .finish()
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary(parts: Vec<Part>) -> Boundary {
        Boundary {
            parts: parts.into_iter().collect(),
            ..Boundary::default()
        }
    }

    #[test]
    fn spaces_collapse_and_drop_at_breaks() {
        let pt = Abs::pt;
        let block = Arc::new(BlockBox {
            span: Span::detached(),
            x: Abs::zero(),
            width: pt(100.0),
            space_before: Abs::zero(),
            space_after: Abs::zero(),
            padding: Default::default(),
            border: Default::default(),
            background: None,
            keep_with_next: false,
            keep_together: false,
            breakable: true,
            break_before: crate::layout::BreakBefore::Auto,
        });
        // A paragraph ends (space after 6pt), a section with padding 2pt
        // starts (space before 10pt), then its heading (space before 4pt).
        let b = boundary(vec![
            Part::Lead(pt(1.0)),
            Part::Solid(Abs::zero()),
            Part::Close,
            Part::Space(pt(6.0)),
            Part::Space(pt(10.0)),
            Part::Open(block.clone()),
            Part::Solid(pt(2.0)),
            Part::Space(pt(4.0)),
            Part::Open(block),
            Part::Solid(Abs::zero()),
            Part::Lead(pt(1.0)),
        ]);
        let mut events = Vec::new();
        let gap = b.walk(Walk::Through, |p, y| {
            events.push((matches!(p, Part::Open(_)), y.to_pt()))
        });
        // 1 + max(6, 10) + 2 + 4 + 1.
        assert_eq!(gap, pt(18.0));
        assert_eq!(events, [(false, 1.0), (true, 11.0), (true, 17.0)]);
        assert_eq!(b.walk(Walk::Bottom, |_, _| {}), Abs::zero());
        let mut tops = Vec::new();
        assert_eq!(b.walk(Walk::Top, |_, y| tops.push(y.to_pt())), pt(2.0));
        assert_eq!(tops, [0.0, 2.0]);
    }
}
