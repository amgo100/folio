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
//!   text they end with (§7.9).
//!
//! The [`lines()`] query breaks each of a build's paragraphs at the content
//! width of its block: the default page master's body less the padding and
//! borders of the blocks around it. It breaks by total fit, without a
//! deadline, so that its result is deterministic (§2.4).
//!
//! The page builder ([`pages()`]) works on one master with one body
//! region, and places figures in the flow where they stand:
//!
//! - **The flow**: the build's blocks and lines flattened into leaves
//!   (lines, and blocks without lines) and the boundaries between them,
//!   which hold the leading, the blocks' spaces, padding and borders, and
//!   the cost of a page break there. Spaces collapse where they meet and
//!   are dropped at a page break; a block broken across pages is sliced.
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
//! - **The page cache** ([`PageCache`]): each page, keyed on the master and
//!   the leaves and boundaries its builder read, so an edit re-breaks only
//!   the pages whose content it changed (§2.3).
//!
//! This is the stage that becomes `folio-layout` (SPEC §2.2) once its
//! interface settles.

mod cache;
mod flow;
pub mod linebreak;
mod master;
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
pub use self::master::{BlockBox, PageMaster, Sides};
pub use self::page::{FILL, LineRef, Page, PageCache, PageStats, Pages, pages};
pub use self::paragraph::{
    Align, Line, LineSlot, ParagraphInput, ParagraphLayout, ParagraphStyle, SegmentMetrics,
    layout_paragraph,
};

use crate::db::{Build, Db, Project};
use crate::diag::{self, Diagnostic};
use crate::fonts::{Fonts, fonts};
use crate::style::{ComputedStyle, Properties, styles};
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
}

/// The paragraphs of a build broken into lines of the body's width, or
/// `None` when the document could not be styled. Reports overfull and
/// underfull lines.
#[salsa::tracked(returns(ref))]
pub fn lines(db: &dyn Db, build: Build<'_>) -> Option<Lines> {
    let text = text(db, build).as_ref()?;
    let styled = styles(db, build).as_ref()?;
    let props = &styled.properties;
    let fonts = fonts(db, build.project(db));

    let width = PageMaster::of(&styled.page, props).body.size.width;
    let slots: EcoVec<LineSlot> =
        flow::slots(&text.root, props, width, text.paragraphs.len()).into();
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
        .map(|p| input_with(p, props, &mut metrics))
        .collect();
    let jobs: Vec<(ParagraphInput<'_>, &[LineSlot])> = inputs
        .into_iter()
        .zip(&widths)
        .map(|(input, slot)| (input, &slot[..]))
        .collect();
    let paragraphs: EcoVec<Arc<ParagraphLayout>> =
        db.layout_cache().layout_all(&jobs).into_iter().collect();

    for (paragraph, layout) in text.paragraphs.iter().zip(&paragraphs) {
        report_lines(db, build.project(db), paragraph, layout);
    }
    Some(Lines {
        width,
        paragraphs,
        slots,
    })
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
                Diagnostic::warning(line_span(db, project, paragraph, line), message).with_hint(
                    "allow hyphenation with `hyphenate: true`, set `justify: false`, \
                     or reword the text",
                ),
            );
        }
    }
}

/// The source of the text a line ends with: the part of the source node
/// that is on the line where the node's source is the paragraph's text,
/// else the whole node; or the paragraph's element when the line's text
/// comes from no node.
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
    narrow(db, project, paragraph.shaped.text(), source, part).unwrap_or_else(|| source.span.into())
}

/// The part `part` of a paragraph's text as a sub-range of the source node
/// it comes from, if the node's source starts with the text it gave: a
/// text node of markup does, while a string from a script does not.
fn narrow(
    db: &dyn Db,
    project: Project,
    text: &str,
    source: &TextSource,
    part: Range<usize>,
) -> Option<DiagSpan> {
    let id = source.span.id()?;
    let file = project
        .sources(db)
        .values()
        .find(|f| f.source(db).id() == id)?;
    let file = file.source(db);
    let node = file.find(source.span)?;
    let written = file.text().get(node.range())?;
    if !written.starts_with(text.get(source.range.clone())?) {
        return None;
    }
    let sub = SubRange::new(
        part.start - source.range.start,
        part.end - source.range.start,
    )?;
    Some(DiagSpan::from_span(source.span, Some(sub)))
}
