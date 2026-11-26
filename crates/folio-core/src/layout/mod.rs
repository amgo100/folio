//! The layout stage (SPEC §7): the paragraph builder of §7.2, which breaks
//! shaped paragraphs into lines, and the page builder of §7.1, which breaks
//! a build's blocks and lines into pages of fragments.
//!
//! - **Line breaking** ([`linebreak`]): Knuth–Plass total fit over each
//!   paragraph's break opportunities, with a width per line index (the last
//!   repeating), so the irregular shapes of text wrap (§7.7) need no other
//!   algorithm. It runs in TeX's passes: without hyphenation, with it, and
//!   a final pass that breaks any paragraph, preferring loose lines to
//!   overfull ones. Past a deadline, total fit gives way to first fit,
//!   which the editor's typing path uses (§7.2).
//! - **Paragraph properties**: `leading` (the space between one line's
//!   descent and the next line's ascent), `indent` (of the first line),
//!   `align` (of lines that are not stretched: ragged ones and a justified
//!   paragraph's last), `justify`, and `protrusion`, which hangs
//!   punctuation and hyphens into the margin. `hyphenate` decides which
//!   hyphenation points the text stage finds.
//! - **The cache** ([`Cache`]): each paragraph's lines, keyed on its
//!   content, the style values the builder reads, and its line widths
//!   (SPEC §2.3). It is shared by every handle of the database, so an edit
//!   re-breaks only the paragraphs it changed.
//! - **Diagnostics**: overfull and underfull lines, at the source of the
//!   text they end with (§7.9). Each layout diagnostic has a code; those
//!   in [`DETAIL_CODES`] are shown only on request (`folio check
//!   --layout`), the others always.
//!
//! The [`lines()`] query breaks each of a build's paragraphs, in parallel,
//! at the content width of its block: the default page master's body less
//! the padding and borders of the blocks around it. It breaks by total
//! fit, without a deadline, so that its result is deterministic (§2.4).
//! The page builder reads its lines wherever a region has those widths.
//!
//! The page builder ([`pages()`]) works on one master with one body
//! region, and places figures in the flow where they stand:
//!
//! - **The flow**: the build's blocks and paragraphs flattened into leaves
//!   (paragraphs, and blocks without them) and the boundaries between
//!   them, which hold the blocks' spaces, padding and borders, and the
//!   cost of a page break there. Spaces collapse where they meet and are
//!   dropped at a page break; a block broken across pages is sliced.
//! - **Regions**: the page builder breaks each paragraph into lines at the
//!   width of the region it starts in, through the line-break cache, and a
//!   paragraph that continues on the next page keeps those breaks. The
//!   height left for the flow can shrink as a page is filled, as footnotes
//!   will make it; [`Regions`] holds test hooks that vary a page's width
//!   and reserve space at its foot.
//! - **Breaking**: strictly forward, each page at its cheapest break, where
//!   a break costs how empty it leaves the page plus the penalties of the
//!   widow, orphan and keep constraints it goes against ([`WIDOW`],
//!   [`KEEP`], [`UNBREAKABLE`]); `break-before` forces one. Constraints are
//!   costs, not rules: the least-bad page wins, and the constraints it
//!   relaxes are warnings (§7.2).
//! - **Fragments**: each page a [`Fragment`](folio_component::Fragment)
//!   of the master's size, holding its body, which holds a fragment for
//!   each block with its background, borders, glyph runs, baselines and
//!   keep data.
//! - **The page cache** ([`PageCache`]): each page, keyed on the master,
//!   its region, the state coming into it (where it starts in the flow,
//!   which may be inside a paragraph) and the leaves and boundaries its
//!   builder read, so an edit re-breaks only the pages whose content it
//!   changed (§2.3).
//!
//! This is the stage that becomes `folio-layout` (SPEC §2.2) once its
//! interface settles.

mod cache;
mod flow;
pub mod linebreak;
mod master;
mod math;
mod page;
mod paragraph;
mod set;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use ecow::{EcoVec, eco_format};
use folio_component::{Abs, Value};
use folio_syntax::{DiagSpan, SubRange};

pub use self::cache::{Cache, Stats};
pub use self::flow::{KEEP, UNBREAKABLE, WIDOW};
pub use self::master::{BlockBox, BreakBefore, PageMaster, Sides};
use self::math::maths;
pub use self::math::{Display, LaidMath, MATH_FONT, Maths, Number};
pub(crate) use self::math::{math_face, math_settings};
pub use self::page::{
    FILL, LineRef, Page, PageCache, PageStats, Pages, Regions, SizeBlock, SizeObject, pages,
    paginate,
};
pub use self::paragraph::{
    Align, Line, LineSlot, ObjectBreak, ObjectMetrics, ParagraphInput, ParagraphLayout,
    ParagraphStyle, SegmentMetrics, layout_paragraph, object_offsets,
};

/// The objects the [`Regions::object`] hook sizes, here so that the test
/// hooks' types are all in this module.
pub use crate::text::TextObject;

use crate::db::{Build, Db, Project};
use crate::diag::{self, Diagnostic};
use crate::fonts::{Fonts, fonts};
use crate::pipeline::step_styled;
use crate::style::{ComputedStyle, Properties};
use crate::text::{TextParagraph, TextSource, text};

/// How long total fit may take for a paragraph on the editor's typing path
/// before the paragraph is broken greedily instead (SPEC §7.2): pass
/// `Instant::now() + TYPING_BUDGET` as the deadline of
/// [`Cache::layout`]. Greedy breaks are not cached, so the paragraph gets
/// total fit's breaks when it is next laid out without a deadline.
pub const TYPING_BUDGET: Duration = Duration::from_millis(5);

/// The paragraphs of a build broken into lines, in document order.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Lines {
    /// The width of the default page master's body.
    pub width: Abs,
    /// The paragraphs, in the order of the build's [`Text`](crate::text::Text),
    /// each shared with every build and revision that breaks an equal
    /// paragraph in the same width.
    pub paragraphs: EcoVec<Arc<ParagraphLayout>>,
    /// The slot of each paragraph's lines: the content width of the block
    /// whose content it is, and where that starts from the left of the
    /// body. A line's `x` is relative to its slot.
    pub slots: EcoVec<LineSlot>,
    /// The build's formulas laid out ([`Maths`]): the paragraphs were
    /// broken with their inline formulas' sizes, and the page builder
    /// draws them and sizes the blocks of displays.
    pub maths: Maths,
}

/// The paragraphs of a build broken into lines of the body's width, or
/// `None` when the document could not be styled. Reports overfull and
/// underfull lines, except for a layout pass of the page-number fixpoint
/// ([`Build::is_pass`]).
#[salsa::tracked(returns(ref))]
pub fn lines(db: &dyn Db, build: Build<'_>) -> Option<Lines> {
    let text = text(db, build).as_ref()?;
    let styled = step_styled(db, build)?;
    let props = &styled.properties;
    let fonts = fonts(db, build.project(db));
    let maths = maths(db, build).unwrap_or_default();

    let width = PageMaster::of(&styled.page, props).body.size.width;
    let (slots, display_widths) =
        flow::slots(&text.root, props, width, text.paragraphs.len(), &text.maths);
    let slots: EcoVec<LineSlot> = slots.into();
    // The paragraph builder places lines relative to their slots.
    let widths: Vec<[LineSlot; 1]> = slots
        .iter()
        .map(|s| {
            [LineSlot {
                offset: Abs::zero(),
                width: s.width,
            }]
        })
        .collect();
    let mut metrics = FaceMetrics::new(fonts);
    let inputs: Vec<ParagraphInput<'_>> = text
        .paragraphs
        .iter()
        .map(|p| {
            let mut input = input_with(p, props, &mut metrics);
            input.objects = maths.metrics(&p.objects);
            input
        })
        .collect();
    let jobs: Vec<(ParagraphInput<'_>, &[LineSlot])> = inputs
        .into_iter()
        .zip(&widths)
        .map(|(input, slot)| (input, &slot[..]))
        .collect();
    // An edit stops the query between paragraphs (SPEC §2.3).
    let paragraphs: EcoVec<Arc<ParagraphLayout>> = db
        .layout_cache()
        .layout_all_checked(&jobs, &mut || db.unwind_if_revision_cancelled())
        .into_iter()
        .collect();

    let lines = Lines {
        width,
        paragraphs,
        slots,
        maths,
    };
    // A layout pass of the page-number fixpoint reports nothing: the build
    // reports its own lines (`crate::pipeline::settle`).
    if build.is_pass(db) {
        return Some(lines);
    }
    for (paragraph, layout) in text.paragraphs.iter().zip(&lines.paragraphs) {
        report_lines(db, build.project(db), paragraph, layout);
    }
    // A display wider than its block is an overfull line (SPEC §7.9).
    for (i, content) in display_widths {
        let maths = &lines.maths;
        let Some(display) = maths.display(i) else {
            continue;
        };
        let overflow = math::display_overflow(display, content);
        if overflow.to_pt() > 0.1 {
            diag::report(
                db,
                Diagnostic::warning(
                    text.maths[i].span,
                    eco_format!(
                        "this display is overfull: {:.1}pt wider than its space",
                        overflow.to_pt()
                    ),
                )
                .with_code(codes::OVERFULL_LINE)
                .with_hint("break it into lines with `\\`, or align its lines at `&`"),
            );
        }
    }
    Some(lines)
}

/// What the paragraph builder reads of a paragraph of the text stage: its
/// shaped text, the paragraph properties of its style, and the font size,
/// tracking and vertical metrics of each segment.
pub fn input<'a>(
    paragraph: &'a TextParagraph,
    props: &Properties,
    fonts: &Fonts,
) -> ParagraphInput<'a> {
    input_with(paragraph, props, &mut FaceMetrics::new(fonts))
}

fn input_with<'a>(
    paragraph: &'a TextParagraph,
    props: &Properties,
    metrics: &mut FaceMetrics<'_>,
) -> ParagraphInput<'a> {
    ParagraphInput {
        shaped: &paragraph.shaped,
        style: paragraph_style(&paragraph.style, props),
        segments: paragraph
            .segments
            .iter()
            .map(|s| {
                let size = length(&s.style, props, "font-size");
                let (ascent, descent) = s
                    .face
                    .and_then(|face| metrics.get(face))
                    .unwrap_or((FALLBACK_ASCENT, FALLBACK_DESCENT));
                SegmentMetrics {
                    size,
                    tracking: length(&s.style, props, "tracking"),
                    ascent: size * ascent,
                    descent: size * descent,
                }
            })
            .collect(),
        objects: EcoVec::new(),
    }
}

/// The ascent and descent of faces in ems, each read once.
struct FaceMetrics<'a> {
    fonts: &'a Fonts,
    faces: HashMap<usize, Option<(f64, f64)>>,
}

impl<'a> FaceMetrics<'a> {
    fn new(fonts: &'a Fonts) -> Self {
        Self {
            fonts,
            faces: HashMap::new(),
        }
    }

    fn get(&mut self, face: usize) -> Option<(f64, f64)> {
        let fonts = self.fonts;
        *self.faces.entry(face).or_insert_with(|| {
            let font = fonts.faces()[face].font()?;
            let m = font.metrics();
            Some((m.to_em(m.ascender), -m.to_em(m.descender)))
        })
    }
}

/// The ascent of a segment without a face, in ems.
const FALLBACK_ASCENT: f64 = 0.8;
/// Its descent.
const FALLBACK_DESCENT: f64 = 0.2;

/// What the paragraph builder reads of a paragraph's style.
fn paragraph_style(style: &ComputedStyle, props: &Properties) -> ParagraphStyle {
    let value = |name: &str| style.value(props, props.builtin(name));
    let align = match value("align") {
        Some(Value::Keyword(k)) if k.as_str() == "center" => Align::Center,
        Some(Value::Keyword(k)) if k.as_str() == "end" => Align::End,
        _ => Align::Start,
    };
    ParagraphStyle {
        leading: length(style, props, "leading"),
        indent: length(style, props, "indent"),
        align,
        justify: matches!(value("justify"), Some(Value::Bool(true))),
        protrusion: matches!(value("protrusion"), Some(Value::Bool(true))),
        font_size: length(style, props, "font-size"),
    }
}

use self::master::length;

/// The codes of the layout diagnostics (SPEC §7.9,
/// `docs/diagnostics-json.md`).
pub mod codes {
    /// A line wider than its space.
    pub const OVERFULL_LINE: &str = "overfull-line";
    /// A justified line stretched too far, or with nothing to stretch.
    pub const UNDERFULL_LINE: &str = "underfull-line";
    /// A page break that leaves fewer lines at a page's top than `widows`.
    pub const WIDOW: &str = "widow";
    /// A page break that leaves fewer lines at a page's foot than
    /// `orphans`.
    pub const ORPHAN: &str = "orphan";
    /// A page break between a block with `keep-with-next` and the next.
    pub const BROKEN_KEEP_WITH_NEXT: &str = "broken-keep-with-next";
    /// A page break inside a block with `keep-together`.
    pub const BROKEN_KEEP_TOGETHER: &str = "broken-keep-together";
    /// A page break inside a block with `breakable: false`.
    pub const BROKEN_UNBREAKABLE: &str = "broken-unbreakable";
    /// Content taller than the space a page has for it.
    pub const PAGE_OVERFLOW: &str = "page-overflow";
}

/// The codes of the layout diagnostics that a front end shows only when
/// asked to (SPEC §7.9): underfull lines, and widow, orphan, keep and
/// unbreakable constraints that a page break relaxed. Content that runs
/// off its line or page ([`codes::OVERFULL_LINE`],
/// [`codes::PAGE_OVERFLOW`]) is not among them: it is always shown.
pub const DETAIL_CODES: [&str; 6] = [
    codes::UNDERFULL_LINE,
    codes::WIDOW,
    codes::ORPHAN,
    codes::BROKEN_KEEP_WITH_NEXT,
    codes::BROKEN_KEEP_TOGETHER,
    codes::BROKEN_UNBREAKABLE,
];

/// Reports a paragraph's overfull and underfull lines (SPEC §7.9).
fn report_lines(
    db: &dyn Db,
    project: Project,
    paragraph: &TextParagraph,
    layout: &ParagraphLayout,
) {
    for line in &layout.lines {
        if line.overflow > Abs::zero() {
            diag::report(
                db,
                Diagnostic::warning(
                    line_span(db, project, paragraph, line),
                    eco_format!(
                        "this line is overfull: {:.1}pt wider than its space",
                        line.overflow.to_pt()
                    ),
                )
                .with_code(codes::OVERFULL_LINE)
                .with_hint(
                    "there is no break that fits; allow hyphenation with \
                     `hyphenate: true`, or reword the text",
                ),
            );
        } else if line.underfull {
            let message = if line.glue.get() > 0.0 {
                let factor = line.space(Abs::pt(1.0)).to_pt();
                eco_format!(
                    "this line is underfull: its spaces are stretched to {factor:.1} \
                     times their width"
                )
            } else {
                "this line is underfull: it has no spaces to stretch to its width".into()
            };
            diag::report(
                db,
                Diagnostic::warning(line_span(db, project, paragraph, line), message)
                    .with_code(codes::UNDERFULL_LINE)
                    .with_hint(
                        "allow hyphenation with `hyphenate: true`, set `justify: false`, \
                         or reword the text",
                    ),
            );
        }
    }
}

/// The source of the text a line ends with: the part of the source node
/// that is on the line where the span map can tell it
/// ([`crate::span_map::text_range`]), else the whole node; or the
/// paragraph's element when the line's text comes from no node.
fn line_span(db: &dyn Db, project: Project, paragraph: &TextParagraph, line: &Line) -> DiagSpan {
    let end = line.content_end.max(line.range.start + 1);
    let Some(source) = paragraph
        .sources
        .iter()
        .rev()
        .find(|s| s.range.start < end && line.range.start < s.range.end)
    else {
        return paragraph.span.into();
    };
    let part = line.range.start.max(source.range.start)..end.min(source.range.end);
    narrow(db, project, source, part).unwrap_or_else(|| source.span.into())
}

/// The part `part` of a paragraph's text, which starts in `source`, as a
/// sub-range of the source node it comes from.
fn narrow(
    db: &dyn Db,
    project: Project,
    source: &TextSource,
    part: Range<usize>,
) -> Option<DiagSpan> {
    let id = source.span.id()?;
    let file = project
        .sources(db)
        .values()
        .find(|f| f.source(db).id() == id)?;
    let file = file.source(db);
    let node = file.find(source.span)?.range();
    let bytes = crate::span_map::text_range(file, source.span, source.within(part))?;
    let sub = SubRange::new(bytes.start - node.start, bytes.end - node.start)?;
    Some(DiagSpan::from_span(source.span, Some(sub)))
}
