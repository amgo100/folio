//! The text stage (SPEC §7.2): a build's paragraphs, itemised, shaped and
//! with their break opportunities, by `folio-text`.
//!
//! - **Paragraphs**: every run of inline content in the styled document is
//!   one paragraph: the body of a `paragraph` or `heading`, and inline
//!   content fields such as a caption. Its text joins the text of the inline
//!   elements in it (`emph`, `strong`, `raw`, a component's inline
//!   elements), each in its own style. A space is U+0020, a forced line
//!   break U+2028, and an element without text (inline math, an image, a
//!   note's or float's anchor) the object character, which layout sizes.
//!   The blocks of notes and floats anchored in a paragraph are paragraphs
//!   of their own.
//! - **Footnotes** (SPEC §7.8): a footnote's anchor is its marker, set in
//!   the footnote's style where it stands ([`TextParagraph::notes`]), and
//!   its blocks are a flow of their own ([`Text::notes`]), which the page
//!   builder places at the foot of the anchor's page. Their paragraphs are
//!   paragraphs of the build like any other, after the paragraph they are
//!   anchored in.
//! - **Spaces** (SPEC §6.3): the block of an element or part whose content
//!   `hidden` or `answer-space` left out holds nothing. The content it
//!   measures, as Select kept it, is collected as a footnote's blocks are,
//!   into a tree of its own in no block of the document
//!   ([`Text::spaces`]): its paragraphs and formulas are the build's, so
//!   they are shaped, broken and laid out like any other, but no writer
//!   reaches them.
//! - **Generated text** (SPEC §4.5): a reference is the text the Resolve
//!   stage gave it (`Figure 2`), and an element's marker (its number, a
//!   list item's bullet, a figure's word and number) starts its first
//!   paragraph, or its caption, in the element's style
//!   ([`crate::resolve`]). Both stand for the whole node in the span map.
//! - **Render templates** (SPEC §5.3): an element the Style stage gave a
//!   template is collected from its expansion: its header, a block of the
//!   part `header`, then its body in the element's own block. Text,
//!   numbers and inline fields run into paragraphs; slots are the blocks
//!   of their parts; rows and grids are grids of cells, rules and spaces
//!   empty blocks that layout draws with their borders and heights. The
//!   element's marker is not set: the template places its number with
//!   `counter()`.
//! - **Quotation marks**: a quote in markup becomes its language's mark,
//!   opening or closing by the characters around it ([`Lang::quotes`]).
//! - **Fonts**: each run of one style is set in its `font` chain, split by
//!   fallback ([`Fonts::resolve`]); the font stage checks the segments this
//!   gives ([`crate::fonts::check_fonts`]), so every character set.
//! - **Language and hyphenation**: `lang` (or the manifest's language when
//!   it is `auto`) chooses glyph forms, hyphenation patterns and line-end
//!   rules; `hyphenate: auto` hyphenates justified paragraphs.
//!
//! The stage is two queries: [`collect_text`] collects the paragraphs and
//! sets them in fonts, for every target, and [`text()`] shapes them, for
//! the targets that are laid out. Shaping is memoised per paragraph in the
//! database's [`Cache`]: the query hands every paragraph of the build to
//! it, the new ones are shaped in parallel with `rayon` (on the query's
//! own thread when the shaping pool has one thread, as with
//! `RAYON_NUM_THREADS=1`), and an edit re-shapes only the paragraphs it
//! changed. The queries themselves are
//! memoised like any other.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, ElementKind, Location, Name, QualifiedName, Target, Value};
use folio_syntax::Span;
use folio_text::{Feature, Lang, OBJECT, Paragraph, Segment, SegmentKind, Shaped, TextFont};

use crate::db::{Build, Db, Project, data_file};
use crate::diag::{self, Diagnostic};
use crate::eval::{self, Content, Element, Node};
use crate::fonts::{Fonts, Request, fonts};
use crate::image::{LoadedImage, load_image};
use crate::locate::StyledChildren;
use crate::pipeline::{resolution, step_styled};
use crate::resolve::{Resolution, UNRESOLVED};
use crate::style::{
    BlockKind, Child, ComputedStyle, Expansion, HEADER_PART, Piece as TemplatePiece, Properties,
    StyledNode, TemplateBlock, content_elements, styled_children,
};
use crate::variants::select::{Measure, Space};

pub use crate::variants::select::SpaceKind;

pub use folio_text::Cache;

/// The shaped paragraphs of a build, in document order: an element's body
/// and content fields in the order they were given, and the blocks of a
/// note or float after the paragraph it is anchored in.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Text {
    /// The paragraphs.
    pub paragraphs: EcoVec<TextParagraph>,
    /// The blocks that hold them: the `doc` element, with a block for
    /// every element whose content the paragraphs are or are in, and for
    /// every part of one (a content field other than the body). Layout
    /// draws their boxes and keeps their constraints (SPEC §7.1).
    pub root: TextBlock,
    /// The formulas, inline and display, in document order: what math
    /// layout sets (SPEC §8.2).
    pub maths: EcoVec<TextMath>,
    /// The footnotes, each the block of its `footnote` element, in the
    /// order of their anchors (SPEC §7.8). They are in no block of
    /// [`Text::root`]: the page builder places them at the foot of a page.
    pub notes: EcoVec<TextBlock>,
    /// For each footnote, the paragraph of the mark that starts the part of
    /// it that runs onto the next page (*(cont.)*), in the style and
    /// language of its first paragraph; `None` for a note without
    /// paragraphs. It is in no block either.
    pub continued: EcoVec<Option<usize>>,
    /// The blocks whose content `hidden` or `answer-space` left out, each
    /// with the content it measures (SPEC §6.3), in the order their blocks
    /// close: a space inside content that another measures, or a part's,
    /// comes before its element's.
    pub spaces: EcoVec<TextSpace>,
}

/// A block whose content `hidden` or `answer-space` left out (SPEC §6.3):
/// an element's block, or a part's, which holds nothing, and which layout
/// sizes. The content it measures is collected as the variant would show
/// it, with paragraphs and formulas of the build's text, but it is in no
/// block of [`Text::root`]: layout measures it in a discarded layout and
/// draws none of it, so it never reaches the output (SPEC §6.4).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextSpace {
    /// The element instance of the block.
    pub location: Location,
    /// For a part's block, the part.
    pub part: Option<Name>,
    /// What keeps its space.
    pub kind: SpaceKind,
    /// The block the content would be, which layout measures; `None` where
    /// the space's height does not depend on the content.
    pub measure: Option<TextBlock>,
}

/// The anchor of a footnote in a paragraph: its marker, or an object
/// character where it has none.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextAnchor {
    /// The bytes of the paragraph's text that the anchor is.
    pub range: Range<usize>,
    /// The footnote, as an index into [`Text::notes`].
    pub note: usize,
}

/// A formula of a build, as math layout reads it: an inline formula, which
/// stands in its paragraph as the objects of its chunks
/// ([`TextObject::math`]), or a display, the content of its element's
/// block.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextMath {
    /// The formula.
    pub formula: folio_math::Formula,
    /// What its `math` element's computed style gives it: the font size,
    /// paint and leading it is set with. Only these, rather than the whole
    /// style, so that comparing a build's formulas across an edit is
    /// cheap.
    pub settings: MathSettings,
    /// The element.
    pub span: Span,
    /// The element instance.
    pub location: Location,
    /// Whether it is a display.
    pub display: bool,
    /// For a display that starts an element with a marker and no paragraph
    /// before it, such as a list item that holds only an equation: the
    /// paragraph of the markers, as an index into [`Text::paragraphs`],
    /// which is in no block. Layout sets it beside the display, at its
    /// first line (SPEC §4.5, D-326-1).
    pub lead: Option<usize>,
}

/// What a formula is set with, from its element's style.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct MathSettings {
    /// The font size: `font-size`.
    pub size: folio_component::Abs,
    /// The paint of its glyphs and rules: `color`.
    pub fill: folio_component::Paint,
    /// The space between its lines: `leading`.
    pub leading: folio_component::Abs,
    /// The families it may be set in, in order: `math-font`.
    pub families: EcoVec<EcoString>,
    /// How its vectors are set: the `math-style` of its part `vector`.
    pub vector: folio_math::layout::VectorStyle,
    /// How the numbers of its quantities are written: by its language and
    /// `decimal-marker`.
    pub number: folio_math::NumberFormat,
}

/// The part of an inline formula an object character stands for.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct MathChunk {
    /// The formula, as an index into [`Text::maths`].
    pub formula: usize,
    /// Which of its chunks, between which a line may break
    /// ([`folio_math::layout::inline_chunks`]).
    pub chunk: usize,
}

/// A block of the document and what it holds, in document order: the
/// paragraphs of its content, and its child blocks. The blocks of a note
/// or float anchored in a paragraph follow that paragraph.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextBlock {
    /// The computed style of the element or part: its block properties
    /// (`space-before`, `padding`, `border`, `keep-with-next`, ...) are
    /// the block's.
    pub style: ComputedStyle,
    /// The element, or the element whose part the block is.
    pub span: Span,
    /// The location of that element: which instance of it the block is.
    /// The blocks of its parts share it, and [`Self::part`] tells them
    /// apart.
    pub location: Location,
    /// The name of that element: what the block is, for the writers that
    /// tag output by meaning, such as PDF's structure tree (SPEC §11.1).
    pub element: QualifiedName,
    /// For a part, the content field it holds; `None` for the element's own
    /// block.
    pub part: Option<Name>,
    /// What it holds, in order.
    pub children: EcoVec<TextFlow>,
    /// What the element's own block is in a table (SPEC §7.4): a grid or
    /// a cell, with the fields layout reads; `None` for any other block
    /// and for parts.
    pub table: Option<TableRole>,
}

/// What a block is in a table (SPEC §7.4).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum TableRole {
    /// A `grid`, whose blocks are its cells.
    Grid {
        /// How many of its first rows are header rows, which are set again
        /// at the top of every page that continues it.
        header_rows: usize,
    },
    /// A `cell` of a grid.
    Cell(CellSpec),
}

/// Where a cell stands in its grid and how far it spans, as its fields
/// give them: spans of at least one track, and positions counted from 1.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct CellSpec {
    /// The columns it spans.
    pub colspan: usize,
    /// The rows it spans.
    pub rowspan: usize,
    /// Its first column, from 1, where it is placed explicitly.
    pub column: Option<usize>,
    /// Its first row, from 1, where it is placed explicitly.
    pub row: Option<usize>,
}

/// The most tracks a span or position of a cell counts: more is taken as
/// this many, with a warning, so that a mistyped number cannot make a
/// grid of millions of tracks.
pub const MAX_TRACKS: usize = 1000;

/// What a block holds.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum TextFlow {
    /// A paragraph, as an index into [`Text::paragraphs`].
    Paragraph(usize),
    /// A child block.
    Block(TextBlock),
}

/// One paragraph of a build, shaped.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextParagraph {
    /// The paragraph shaped, shared with every build and revision that has
    /// an equal paragraph.
    pub shaped: Arc<Shaped>,
    /// The style of the element (or the part of one) whose content the
    /// paragraph is: its paragraph properties (`leading`, `indent`,
    /// `align`, `justify`) are the paragraph's.
    pub style: ComputedStyle,
    /// The element whose content the paragraph is.
    pub span: Span,
    /// The location of that element. The paragraphs of its body and of its
    /// parts share it.
    pub location: Location,
    /// For each segment of the paragraph ([`Paragraph::segments`]), the
    /// style its text is set in and its face.
    pub segments: EcoVec<SegmentStyle>,
    /// Where the paragraph's text came from: ranges of it that are the text
    /// of a source node, in order. Objects are in no range.
    pub sources: EcoVec<TextSource>,
    /// The element each object character of the text stands for, in the
    /// order of the text.
    pub objects: EcoVec<TextObject>,
    /// The anchors of the footnotes in the paragraph, in the order of the
    /// text.
    pub notes: EcoVec<TextAnchor>,
}

/// An object character of a paragraph's text (SPEC §7.2): content that is
/// not text, which layout sizes, such as inline maths, an image, a
/// reference, or the anchor of a note or float.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextObject {
    /// Where the character is in the paragraph's text.
    pub at: usize,
    /// The element it stands for.
    pub element: QualifiedName,
    /// The element's node.
    pub span: Span,
    /// The element instance.
    pub location: Location,
    /// For a chunk of an inline formula, which.
    pub math: Option<MathChunk>,
    /// For an image, the image, which layout sizes.
    pub image: Option<TextImage>,
}

/// An image as an object of a paragraph: the image read, and what its
/// element asks of its size and description.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextImage {
    /// The image and its natural size.
    pub loaded: LoadedImage,
    /// The computed `width` of its element: `auto`, a length, or a ratio
    /// or number of the width of its paragraph's lines (SPEC §7.2).
    pub width: Value,
    /// Its `alt` text, the description PDF/UA-1 asks for (SPEC §11.1).
    pub alt: Option<EcoString>,
}

/// The style and face of a segment of a paragraph.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SegmentStyle {
    /// The computed style of its text.
    pub style: ComputedStyle,
    /// Its face, as an index into [`Fonts::faces`], or `None` for an object
    /// or text that no font of its chain has.
    pub face: Option<usize>,
}

/// A range of a paragraph's text that is the text of a source node.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextSource {
    /// The byte range in the paragraph's text.
    pub range: Range<usize>,
    /// The node: a text node of markup, a space, a quotation mark, a
    /// `raw` element, or the expression a string came from.
    pub span: Span,
    /// The bytes of the node's text that the range is. As long as `range`
    /// where the paragraph has the node's text as it is; else, as for the
    /// line breaks of `raw`, the bytes the range stands for as a whole, or
    /// [`TextSource::WHOLE`] for text that the node gave in no way the span
    /// map can follow.
    pub within: Range<usize>,
    /// The innermost element instance the text is in, for provenance (SPEC
    /// §6.4), or [`Location::DETACHED`] for text of the paragraph's own
    /// element ([`TextParagraph::location`]).
    pub location: Location,
}

impl TextSource {
    /// The bytes of a node's text that stand for the whole node: they run
    /// past the end of any text, so the span map resolves them to all of
    /// it ([`crate::span_map::text_range`]).
    pub const WHOLE: Range<usize> = 0..usize::MAX;

    /// The bytes of the node's text that the part `part` of the
    /// paragraph's text shows, where `part` starts in this range: the same
    /// part of the node's text, cut at the end of the range, or all of
    /// [`Self::within`] where the paragraph does not have the node's text
    /// as it is.
    pub fn within(&self, part: Range<usize>) -> Range<usize> {
        if self.range.len() != self.within.len() {
            return self.within.clone();
        }
        let start = part.start.clamp(self.range.start, self.range.end);
        let end = part.end.clamp(start, self.range.end);
        let base = self.within.start;
        base + (start - self.range.start)..base + (end - self.range.start)
    }
}

/// The paragraphs of a build, shaped, or `None` when the document could not
/// be styled: those of [`collect_text`], each shaped.
#[salsa::tracked(returns(ref))]
pub fn text(db: &dyn Db, build: Build<'_>) -> Option<Text> {
    let collected = collect_text(db, build).as_ref()?;
    let inputs: Vec<Paragraph> = collected
        .paragraphs
        .iter()
        .map(|p| p.paragraph.clone())
        .collect();
    // An edit stops the query between paragraphs (SPEC §2.3).
    let shaped = db
        .text_cache()
        .shape_all_checked(&inputs, &mut || db.unwind_if_revision_cancelled());
    let paragraphs = collected
        .paragraphs
        .iter()
        .zip(shaped)
        .map(|(p, shaped)| TextParagraph {
            shaped,
            style: p.style.clone(),
            span: p.span,
            location: p.location,
            segments: p.segments.clone(),
            sources: p.sources.clone(),
            objects: p.objects.clone(),
            notes: p.notes.clone(),
        })
        .collect();
    Some(Text {
        paragraphs,
        root: collected.root.clone(),
        maths: collected.maths.clone(),
        notes: collected.notes.clone(),
        continued: collected.continued.clone(),
        spaces: collected.spaces.clone(),
    })
}

/// The paragraphs of a build before shaping, with the blocks that hold them.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Collected {
    /// The paragraphs, in the order of [`Text::paragraphs`].
    pub paragraphs: EcoVec<CollectedParagraph>,
    /// The blocks that hold them, as [`Text::root`].
    pub root: TextBlock,
    /// The formulas, as [`Text::maths`].
    pub maths: EcoVec<TextMath>,
    /// The footnotes, as [`Text::notes`].
    pub notes: EcoVec<TextBlock>,
    /// The marks of continued footnotes, as [`Text::continued`].
    pub continued: EcoVec<Option<usize>>,
    /// The spaces, as [`Text::spaces`].
    pub spaces: EcoVec<TextSpace>,
}

/// A paragraph of a build, split into segments set in their fonts, before
/// shaping. Its fields are those of [`TextParagraph`].
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CollectedParagraph {
    /// Its text and segments, as shaping takes them.
    pub paragraph: Paragraph,
    /// As [`TextParagraph::style`].
    pub style: ComputedStyle,
    /// As [`TextParagraph::span`].
    pub span: Span,
    /// As [`TextParagraph::location`].
    pub location: Location,
    /// As [`TextParagraph::segments`].
    pub segments: EcoVec<SegmentStyle>,
    /// As [`TextParagraph::sources`].
    pub sources: EcoVec<TextSource>,
    /// As [`TextParagraph::objects`].
    pub objects: EcoVec<TextObject>,
    /// As [`TextParagraph::notes`].
    pub notes: EcoVec<TextAnchor>,
}

/// The paragraphs of a build, collected from the styled tree and set in
/// fonts by fallback but not shaped, or `None` when the document could not
/// be styled. Every build checks the fonts of their segments
/// ([`crate::fonts::check_fonts`]); the targets that are laid out shape
/// them ([`text()`]). For those targets, reports `features` that are not
/// OpenType feature settings, `lang` values that are not language tags,
/// and `hyphenate: true` for a language without hyphenation patterns:
/// what shaping and breaking read, which DOCX and PPTX leave to Word and
/// PowerPoint; and elements that layout does not draw yet.
#[salsa::tracked(returns(ref))]
pub fn collect_text(db: &dyn Db, build: Build<'_>) -> Option<Collected> {
    let project = build.project(db);
    let styled = step_styled(db, build)?;
    let resolution = resolution(db, build)?;
    let root = crate::pipeline::selected_root(db, build)?;
    let spaces = crate::pipeline::select(db, build.base(db))
        .as_ref()
        .map(|selected| {
            selected
                .spaces
                .iter()
                .map(|space| ((space.location, space.part.clone()), space))
                .collect()
        })
        .unwrap_or_default();
    let fonts = fonts(db, project);
    let default_lang =
        Lang::new(project.manifest(db).project.lang.v.as_str()).unwrap_or_else(Lang::english);

    let mut collector = Collector {
        db,
        project,
        props: &styled.properties,
        fonts,
        cache: db.text_cache(),
        default_lang,
        chains: HashMap::new(),
        settings: HashMap::new(),
        reported: HashSet::new(),
        laid_out: matches!(build.target(db), Target::Pdf | Target::Live),
        pending: Vec::new(),
        blocks: Vec::new(),
        root: None,
        owner: Span::detached(),
        resolution,
        markers: Vec::new(),
        maths: Vec::new(),
        notes: Vec::new(),
        in_note: false,
        continued: Vec::new(),
        template_grids: 0,
        placed: Vec::new(),
        spaces,
        measuring: None,
        measured: Vec::new(),
    };
    collector.element(root, &styled.root);

    let root = collector.root.take().expect("the root is a block");
    Some(Collected {
        paragraphs: collector.pending.into_iter().collect(),
        root,
        maths: collector.maths.into_iter().collect(),
        notes: collector
            .notes
            .into_iter()
            .map(|note| note.expect("every footnote is collected"))
            .collect(),
        continued: collector.continued.into_iter().collect(),
        spaces: collector.measured.into_iter().collect(),
    })
}

/// What shaping and breaking read of a style.
#[derive(Clone)]
struct Settings {
    lang: Lang,
    hyphenate: bool,
    features: EcoVec<Feature>,
}

struct Collector<'a> {
    db: &'a dyn Db,
    project: Project,
    props: &'a Properties,
    fonts: &'a Fonts,
    cache: &'a Cache,
    default_lang: Lang,
    /// The faces of each font request's chain.
    chains: HashMap<Request, EcoVec<usize>>,
    /// The settings of each style.
    settings: HashMap<ComputedStyle, Settings>,
    /// Values already reported, so each is reported once per build.
    reported: HashSet<EcoString>,
    /// Whether Folio lays the build out: only then are values that shaping
    /// and breaking cannot use, and elements it does not draw, reported.
    laid_out: bool,
    pending: Vec<CollectedParagraph>,
    /// The blocks being collected, innermost last.
    blocks: Vec<TextBlock>,
    /// The root block, once it is done.
    root: Option<TextBlock>,
    /// The innermost element being collected that the document model did
    /// not make: the source of text that records no node of its own.
    owner: Span,
    /// What the Resolve stage generated.
    resolution: &'a Resolution,
    /// The markers waiting for the next paragraph, outermost first.
    markers: Vec<Pending>,
    /// The formulas, in document order.
    maths: Vec<TextMath>,
    /// The footnotes, each `None` from its anchor until its blocks are
    /// collected, after the paragraph it is anchored in.
    notes: Vec<Option<TextBlock>>,
    /// Whether it collects a footnote's blocks.
    in_note: bool,
    /// The paragraph of each footnote's continuation mark, as
    /// [`Text::continued`].
    continued: Vec<Option<usize>>,
    /// The content fields the template of the innermost templated element
    /// being collected has placed: each is collected where it is first
    /// placed only, so that no element instance is collected twice.
    placed: Vec<Name>,
    /// How many grids the template of the innermost templated element
    /// being collected has made, which tells their locations apart.
    template_grids: u64,
    /// The spaces Select made, by element instance and part.
    spaces: HashMap<(Location, Option<Name>), &'a Space>,
    /// The element whose content is being collected for measuring, which
    /// is not its own space there.
    measuring: Option<Location>,
    /// The spaces collected, as [`Text::spaces`].
    measured: Vec<TextSpace>,
}

/// An element anchored in a paragraph whose content is blocks, and for a
/// footnote, its index in [`Text::notes`].
type Anchored<'a> = (&'a Element, &'a StyledNode, Option<usize>);

/// A marker waiting for the paragraph it starts.
struct Pending {
    text: EcoString,
    style: ComputedStyle,
    span: Span,
    /// The instance of the element whose marker it is.
    location: Location,
}

/// A paragraph's text as it is collected.
#[derive(Default)]
struct Builder {
    text: String,
    /// Ranges of one style and kind, in order, covering the text.
    pieces: Vec<Piece>,
    sources: Vec<TextSource>,
    /// The elements its object characters stand for.
    objects: Vec<TextObject>,
    /// The anchors of its footnotes.
    anchors: Vec<TextAnchor>,
    /// A quotation mark whose form waits for the character after it.
    quote: Option<Quote>,
    /// The innermost inline element instance that text appended now is
    /// in, or [`Location::DETACHED`] for the paragraph's own element.
    within: Location,
}

/// A quotation mark of markup, in a style and its language.
struct Quote {
    double: bool,
    style: ComputedStyle,
    lang: Lang,
    span: Span,
}

struct Piece {
    range: Range<usize>,
    style: ComputedStyle,
    object: bool,
    /// Where to report a problem with the piece's style.
    span: Span,
}

impl Collector<'_> {
    /// Opens a block of a style: the box of an element, or of its part
    /// `part`.
    fn open(&mut self, style: &ComputedStyle, element: &Element, part: Option<Name>) {
        let table = if part.is_none() {
            self.table_role(element)
        } else {
            None
        };
        self.blocks.push(TextBlock {
            style: style.clone(),
            span: element.span,
            location: element.location,
            element: element.name.clone(),
            table,
            part,
            children: EcoVec::new(),
        });
    }

    /// What an element's block is in a table, with its fields checked: a
    /// span or position below 1, or above [`MAX_TRACKS`], is a warning, and
    /// taken as the nearest it may be.
    fn table_role(&mut self, element: &Element) -> Option<TableRole> {
        let mut int = |field: &str, min: i64| -> Option<usize> {
            let eval::Value::Int(n) = element.field(field)? else {
                return None;
            };
            let max = MAX_TRACKS as i64;
            if *n < min || *n > max {
                self.report_once(
                    eco_format!("table-field:{:?}:{field}", element.location),
                    Diagnostic::warning(
                        element.span,
                        eco_format!(
                            "`{field}` of `{}` is {n}; it is taken as {}",
                            element.name,
                            (*n).clamp(min, max)
                        ),
                    )
                    .with_hint(eco_format!("it counts from {min} to {max}")),
                );
            }
            usize::try_from((*n).clamp(min, max)).ok()
        };
        if is_builtin(&element.name, "grid") {
            return Some(TableRole::Grid {
                header_rows: int("header-rows", 0).unwrap_or(0),
            });
        }
        if is_builtin(&element.name, "cell") {
            return Some(TableRole::Cell(CellSpec {
                colspan: int("colspan", 1).unwrap_or(1),
                rowspan: int("rowspan", 1).unwrap_or(1),
                column: int("column", 1),
                row: int("row", 1),
            }));
        }
        None
    }

    /// Closes the innermost block and adds it to its parent.
    fn close(&mut self) {
        let block = self.blocks.pop().expect("a block is open");
        match self.blocks.last_mut() {
            Some(parent) => parent.children.push(TextFlow::Block(block)),
            None => self.root = Some(block),
        }
    }

    /// Collects the paragraphs of an element and its descendants, in a
    /// block of the element's.
    fn element(&mut self, element: &Element, node: &StyledNode) {
        if !is_builtin(&element.name, "image") {
            self.check_width(element, &node.style);
        }
        let space = match self.measuring {
            Some(location) if location == element.location => None,
            _ => self.spaces.get(&(element.location, None)).copied(),
        };
        self.open(&node.style, element, None);
        // An emptied grid lays out no cells; the grid it measures does.
        if space.is_some()
            && let Some(block) = self.blocks.last_mut()
            && matches!(block.table, Some(TableRole::Grid { .. }))
        {
            block.table = None;
        }
        let owner = self.own(element);
        if let Some(expansion) = &node.template {
            self.templated(element, node, expansion);
            self.owner = owner;
            self.close();
            return;
        }
        // A marker that no paragraph of the element takes is dropped with
        // it. A space has no paragraph to take one.
        let markers = self.markers.len();
        if let Some(marker) = self.resolution.marker(element.location)
            && marker.part.is_none()
            && space.is_none()
        {
            self.markers.push(Pending {
                text: marker.text.clone(),
                style: node.style.clone(),
                span: element.span,
                location: element.location,
            });
        }
        self.element_content(element, node);
        if self.markers.len() > markers {
            // No paragraph or display of the element took its marker.
            self.report_once(
                eco_format!("marker-not-set:{:?}", element.location),
                Diagnostic::warning(
                    element.span,
                    "this element's marker is not set, as it holds no text or display to \
                     start",
                )
                .with_hint("start it with text, or hide the marker with `marker: none`"),
            );
        }
        self.markers.truncate(markers);
        self.owner = owner;
        self.close();
        if let Some(space) = space {
            self.space(space, element);
        }
    }

    /// Records a space of `element` (SPEC §6.3), with the block of the
    /// content it measures, collected into a tree of its own, in no block
    /// of the document's, as a footnote's blocks are; markers waiting for
    /// a paragraph do not reach it.
    fn space(&mut self, space: &Space, element: &Element) {
        let measure = space.measure.as_ref().and_then(|measure| {
            let blocks = std::mem::take(&mut self.blocks);
            let root = self.root.take();
            let markers = std::mem::take(&mut self.markers);
            match measure {
                Measure::Element(shown, node) => {
                    let measuring = self.measuring.replace(element.location);
                    self.element(shown, node);
                    self.measuring = measuring;
                }
                Measure::Part(content, node) => {
                    let mut kids = StyledChildren::new(node);
                    self.open(&node.style, element, space.part.clone());
                    let owner = self.own(element);
                    // The part's marker starts it, as where it is shown.
                    if let Some(marker) = self.resolution.marker(element.location)
                        && marker.part == space.part
                    {
                        self.markers.push(Pending {
                            text: marker.text.clone(),
                            style: node.style.clone(),
                            span: element.span,
                            location: element.location,
                        });
                    }
                    self.content(content, &node.style, element, &mut kids);
                    self.markers.clear();
                    self.owner = owner;
                    self.close();
                }
            }
            let measured = self.root.take();
            self.root = root;
            self.blocks = blocks;
            self.markers = markers;
            measured
        });
        self.measured.push(TextSpace {
            location: space.location,
            part: space.part.clone(),
            kind: space.kind,
            measure,
        });
    }

    /// Collects an element shown with a render template (SPEC §5.3): its
    /// header, a block of the part [`HEADER_PART`], then its body, or its
    /// content where the template has no body, in its own block, which is
    /// open.
    fn templated(&mut self, element: &Element, node: &StyledNode, expansion: &Expansion) {
        // The grids of this instance are counted from its first, so that
        // their locations do not depend on what comes before it.
        let grids = std::mem::replace(&mut self.template_grids, 0);
        let placed = std::mem::take(&mut self.placed);
        self.templated_content(element, node, expansion);
        self.template_grids = grids;
        self.placed = placed;
    }

    fn templated_content(&mut self, element: &Element, node: &StyledNode, expansion: &Expansion) {
        if let Some(header) = &expansion.header {
            self.blocks.push(TextBlock {
                style: header.style.clone(),
                span: element.span,
                location: element.location,
                element: element.name.clone(),
                part: Name::new(HEADER_PART),
                children: EcoVec::new(),
                table: None,
            });
            self.pieces(element, node, &header.items, &header.style);
            self.close();
        }
        match &expansion.body {
            Some(body) => self.pieces(element, node, body, &node.style),
            None => self.element_content(element, node),
        }
    }

    /// Collects the pieces of an expanded template into the innermost open
    /// block, whose style is `style`: runs of text, numbers, inline fields
    /// and images become paragraphs, and the other pieces blocks.
    fn pieces(
        &mut self,
        element: &Element,
        node: &StyledNode,
        pieces: &[TemplatePiece],
        style: &ComputedStyle,
    ) {
        let mut builder: Option<Builder> = None;
        let mut anchored = Vec::new();
        for piece in pieces {
            match piece {
                TemplatePiece::Text { text, style } => {
                    let b = builder.get_or_insert_with(|| self.builder());
                    b.push_part(text, style, element.span, TextSource::WHOLE, false);
                }
                TemplatePiece::Counter { style } => {
                    if let Some(number) = self.counter_text(element) {
                        let b = builder.get_or_insert_with(|| self.builder());
                        b.push_part(&number, style, element.span, TextSource::WHOLE, false);
                    }
                }
                TemplatePiece::Field { name, style } => {
                    if !self.place(name) {
                        continue;
                    }
                    let Some(eval::Value::Content(content)) = element.field(name.as_str()) else {
                        continue;
                    };
                    let Some(part) = part_node(node, name) else {
                        continue;
                    };
                    let mut kids = StyledChildren::new(part);
                    if is_inline(content) {
                        let b = builder.get_or_insert_with(|| self.builder());
                        self.inline(content, style, element.span, &mut kids, b, &mut anchored);
                    } else {
                        self.flush(builder.take(), style, element, &mut anchored);
                        self.slot(element, node, name, content);
                    }
                }
                TemplatePiece::Image {
                    path,
                    width,
                    alt,
                    style,
                    span,
                } => {
                    let image = self.template_image(path, width, alt, *span);
                    let b = builder.get_or_insert_with(|| self.builder());
                    b.object_with(style, element, element.location, None, image);
                }
                TemplatePiece::Slot { name, .. } => {
                    if !self.place(name) {
                        continue;
                    }
                    self.flush(builder.take(), style, element, &mut anchored);
                    if let Some(eval::Value::Content(content)) = element.field(name.as_str()) {
                        self.slot(element, node, name, content);
                    }
                }
                TemplatePiece::Block(block) => {
                    self.flush(builder.take(), style, element, &mut anchored);
                    self.template_block(element, node, block);
                }
            }
        }
        self.flush(builder, style, element, &mut anchored);
    }

    /// Records that the current template places a content field, and
    /// whether it is the first time: a field placed twice is collected the
    /// first time only.
    fn place(&mut self, name: &Name) -> bool {
        if self.placed.contains(name) {
            return false;
        }
        self.placed.push(name.clone());
        true
    }

    /// The number of an element as a template's `counter()` shows it: its
    /// formatted number, or its values as `1.1` would show them where it
    /// has no `numbering`. `None` for an element without a counter.
    fn counter_text(&self, element: &Element) -> Option<EcoString> {
        let number = self
            .resolution
            .number(crate::resolve::Target::element(element.location))?;
        Some(match &number.number {
            Some(text) => text.clone(),
            None => number
                .values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(".")
                .into(),
        })
    }

    /// Adds the pieces of an inline element's template to the paragraph
    /// it stands in: its header's, then its body's. A block a template
    /// makes in a line, such as a row, gives its text only.
    fn inline_pieces<'a>(
        &mut self,
        element: &'a Element,
        node: &'a StyledNode,
        pieces: &[TemplatePiece],
        out: &mut Builder,
        anchored: &mut Vec<Anchored<'a>>,
    ) {
        for piece in pieces {
            match piece {
                TemplatePiece::Text { text, style } => {
                    out.push_part(text, style, element.span, TextSource::WHOLE, false);
                }
                TemplatePiece::Counter { style } => {
                    if let Some(number) = self.counter_text(element) {
                        out.push_part(&number, style, element.span, TextSource::WHOLE, false);
                    }
                }
                TemplatePiece::Field { name, style } => {
                    if !self.place(name) {
                        continue;
                    }
                    // Blocks cannot stand in a line: a content field of
                    // blocks is left out of an inline element's template.
                    if let (Some(eval::Value::Content(content)), Some(part)) =
                        (element.field(name.as_str()), part_node(node, name))
                        && is_inline(content)
                    {
                        let mut kids = StyledChildren::new(part);
                        self.inline(content, style, element.span, &mut kids, out, anchored);
                    }
                }
                TemplatePiece::Slot { name, style } => {
                    if !self.place(name) {
                        continue;
                    }
                    let Some(eval::Value::Content(content)) = element.field(name.as_str()) else {
                        continue;
                    };
                    let kids_node = if name.as_str() == BODY {
                        Some(node)
                    } else {
                        part_node(node, name)
                    };
                    if let Some(kids_node) = kids_node
                        && is_inline(content)
                    {
                        let mut kids = StyledChildren::new(kids_node);
                        self.inline(content, style, element.span, &mut kids, out, anchored);
                    }
                }
                TemplatePiece::Image {
                    path,
                    width,
                    alt,
                    style,
                    span,
                } => {
                    let image = self.template_image(path, width, alt, *span);
                    out.object_with(style, element, element.location, None, image);
                }
                TemplatePiece::Block(block) => {
                    self.inline_pieces(element, node, &block.children, out, anchored);
                }
            }
        }
    }

    /// Finishes a paragraph of a template's pieces, if one was started,
    /// and collects the notes and floats anchored in it.
    fn flush<'a>(
        &mut self,
        builder: Option<Builder>,
        style: &ComputedStyle,
        element: &Element,
        anchored: &mut Vec<Anchored<'a>>,
    ) {
        let Some(builder) = builder else {
            return;
        };
        self.finish(builder, style, element);
        for (element, node, note) in std::mem::take(anchored) {
            match note {
                Some(index) => self.note(element, node, index),
                None => self.element(element, node),
            }
        }
    }

    /// Collects the body or a part of an element where a template places
    /// it: the body in the element's block, a part in a block of its own
    /// in the part's style.
    fn slot(&mut self, element: &Element, node: &StyledNode, name: &Name, content: &Content) {
        if name.as_str() == BODY {
            let mut kids = StyledChildren::new(node);
            self.content(content, &node.style, element, &mut kids);
            return;
        }
        let Some(part) = part_node(node, name) else {
            return;
        };
        let mut kids = StyledChildren::new(part);
        self.open(&part.style, element, Some(name.clone()));
        self.content(content, &part.style, element, &mut kids);
        self.close();
        // A part shown as answer space has its space where the template
        // places it (SPEC §6.3).
        if let Some(space) = self
            .spaces
            .get(&(element.location, Some(name.clone())))
            .copied()
        {
            self.space(space, element);
        }
    }

    /// Collects a block of a template: a row or grid as a grid of cells
    /// (SPEC §7.4), the others as blocks of their part.
    fn template_block(&mut self, element: &Element, node: &StyledNode, block: &TemplateBlock) {
        let table = match block.kind {
            BlockKind::Row | BlockKind::Grid => Some(TableRole::Grid { header_rows: 0 }),
            BlockKind::Cell => Some(TableRole::Cell(CellSpec {
                colspan: 1,
                rowspan: 1,
                column: None,
                row: None,
            })),
            _ => None,
        };
        // Layout lays a grid out by its location, so each grid of an
        // instance has one of its own, made from the instance's.
        let location = if block.kind.is_grid() {
            self.template_grids += 1;
            Location {
                source: element.location.source,
                instance: element.location.instance
                    ^ 0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(self.template_grids),
            }
        } else {
            element.location
        };
        self.blocks.push(TextBlock {
            style: block.style.clone(),
            span: element.span,
            location,
            element: element.name.clone(),
            part: Name::new(block.kind.part()),
            children: EcoVec::new(),
            table,
        });
        self.pieces(element, node, &block.children, &block.style);
        self.close();
    }

    /// The image a template's `image(..)` names, read; `None`, with an
    /// error at the primitive, for a file that is missing or not an image
    /// Folio reads.
    fn template_image(
        &mut self,
        path: &str,
        width: &Value,
        alt: &Option<EcoString>,
        span: Span,
    ) -> Option<TextImage> {
        // Reported once per primitive, however many elements it shows in,
        // and only where Folio lays the build out.
        let report = |this: &mut Self, message: EcoString, error: bool| {
            if this.laid_out {
                let diagnostic = if error {
                    Diagnostic::error(span, message.clone())
                } else {
                    Diagnostic::warning(span, message.clone()).with_hint(
                        "convert the text to paths (outlines) in the program that made the image",
                    )
                };
                this.report_once(eco_format!("template-image:{span:?}:{message}"), diagnostic);
            }
        };
        let Some(project_path) = crate::project::ProjectPath::new(path) else {
            report(
                self,
                eco_format!("`{path}` is not a path in the project"),
                true,
            );
            return None;
        };
        let Some(file) = data_file(self.db, self.project, project_path) else {
            report(self, eco_format!("the image `{path}` does not exist"), true);
            return None;
        };
        match load_image(self.db, file) {
            Ok(loaded) => {
                if loaded.text {
                    report(
                        self,
                        "the text of this SVG image is not drawn: Folio does not set text in SVG images yet".into(),
                        false,
                    );
                }
                Some(TextImage {
                    loaded: loaded.clone(),
                    width: width.clone(),
                    alt: alt.clone(),
                })
            }
            Err(message) => {
                report(self, message.clone(), true);
                None
            }
        }
    }

    /// The paragraph of the markers waiting for one, beside a display: in
    /// no block, as a display's block holds no paragraphs, and the content
    /// of the innermost marker's element, in its style, whose number it is
    /// (SPEC §4.5, D-326-1). `None` when no marker waits.
    fn lead(&mut self) -> Option<usize> {
        let owner = self.markers.last()?;
        let (style, span, location) = (owner.style.clone(), owner.span, owner.location);
        let builder = self.builder();
        let blocks = std::mem::take(&mut self.blocks);
        let index = self.pending.len();
        self.finish_as(builder, &style, span, location);
        self.blocks = blocks;
        (self.pending.len() > index).then_some(index)
    }

    /// Makes an element the owner of text without a node of its own,
    /// unless the document model made it (a paragraph, list or section,
    /// whose span is that of its first node or heading), and returns the
    /// previous owner.
    fn own(&mut self, element: &Element) -> Span {
        let previous = self.owner;
        let made = ["paragraph", "list", "enum", "terms", "section"]
            .iter()
            .any(|name| is_builtin(&element.name, name));
        if !made && !element.span.is_detached() {
            self.owner = element.span;
        }
        previous
    }

    fn element_content(&mut self, element: &Element, node: &StyledNode) {
        // A code block is a paragraph whose lines end in forced breaks.
        if let Some(text) = raw_text(element) {
            let mut builder = self.builder();
            builder.raw(text, &node.style, element.span);
            self.finish(builder, &node.style, element);
            return;
        }
        // An element with no content to set, such as a component's block
        // without fields of content: layout draws its box, and nothing in
        // it. A misplaced `#doc` is reported by the document model.
        if !is_builtin(&element.name, "doc")
            && !element
                .fields
                .iter()
                .any(|(_, value)| matches!(value, eval::Value::Content(_)))
        {
            // A display is laid out by math layout, as its block's
            // content; images are reported as such. The markers waiting
            // for a paragraph go beside it.
            if let Some(index) = self.formula(element, node, true) {
                self.maths[index].lead = self.lead();
                return;
            }
            // An outline or figure list (SPEC §4.5): a paragraph for each
            // entry Resolve listed, in the list's style.
            if is_builtin(&element.name, "outline") || is_builtin(&element.name, "list-of") {
                if let Some(crate::resolve::QueryResult::List(entries)) =
                    self.resolution.query(element.location).map(|q| &q.result)
                {
                    for entry in entries {
                        let mut builder = self.builder();
                        builder.push_part(
                            &entry.text(),
                            &node.style,
                            element.span,
                            TextSource::WHOLE,
                            false,
                        );
                        self.finish(builder, &node.style, element);
                    }
                }
                return;
            }
            // An image where blocks stand is a paragraph of its own.
            if is_builtin(&element.name, "image") {
                let mut builder = self.builder();
                let image = self.image(element, node);
                builder.object_with(&node.style, element, node.location, None, image);
                self.finish(builder, &node.style, element);
                return;
            }
            if is_builtin(&element.name, "math") {
                self.not_laid_out(element);
                return;
            }
            // An empty grid or cell is laid out as such (SPEC §7.4).
            if is_builtin(&element.name, "grid") || is_builtin(&element.name, "cell") {
                return;
            }
            self.report_once(
                eco_format!("not-laid-out:{}", element.name),
                Diagnostic::warning(
                    element.span,
                    eco_format!(
                        "`{}` is not laid out yet, so the output shows at most its box",
                        element.name
                    ),
                )
                .with_hint("this version of Folio checks it but does not draw what it holds"),
            );
            return;
        }
        // The body and the parts, in the order of the element's fields, as
        // they are written. Each element and part finds its styled node by
        // location or name, so a node that a later stage dropped or added
        // shifts no other (`crate::locate`).
        let parts = element
            .fields
            .iter()
            .any(|(key, value)| key != BODY && matches!(value, eval::Value::Content(_)));
        let kids = if parts {
            styled_children(element, node)
        } else {
            Vec::new()
        };
        for (key, value) in element.fields.iter() {
            let eval::Value::Content(content) = value else {
                continue;
            };
            if key == BODY {
                let mut body_kids = StyledChildren::new(node);
                self.content(content, &node.style, element, &mut body_kids);
            } else if let Some(field) = Name::new(key)
                && let Some(part_node) = kids.iter().find_map(|(child, node)| match child {
                    Child::Part(name, _) if *name == key.as_str() => Some(*node),
                    _ => None,
                })
            {
                let mut part_kids = StyledChildren::new(part_node);
                self.open(&part_node.style, element, Some(field.clone()));
                let markers = self.markers.len();
                // An element shown with a template has no marker (SPEC
                // §5.3): the template places its number.
                if let Some(marker) = self.resolution.marker(element.location)
                    && marker.part.as_ref() == Some(&field)
                    && node.template.is_none()
                {
                    self.markers.push(Pending {
                        text: marker.text.clone(),
                        style: part_node.style.clone(),
                        span: element.span,
                        location: element.location,
                    });
                }
                self.content(content, &part_node.style, element, &mut part_kids);
                self.markers.truncate(markers);
                self.close();
                if let Some(space) = self.spaces.get(&(element.location, Some(field))).copied() {
                    self.space(space, element);
                }
            }
        }
    }

    /// Collects the paragraphs of content, the content of `owner`: one
    /// paragraph if it is inline, else those of its blocks. `kids` finds
    /// the styled node of each element of the content.
    fn content<'a>(
        &mut self,
        content: &'a Content,
        style: &ComputedStyle,
        owner: &Element,
        kids: &mut StyledChildren<'a>,
    ) {
        if !is_inline(content) {
            for element in content_elements(content) {
                if let Some(node) = kids.element(element) {
                    self.element(element, node);
                }
            }
            return;
        }
        let mut builder = self.builder();
        let mut anchored = Vec::new();
        self.inline(
            content,
            style,
            owner.span,
            kids,
            &mut builder,
            &mut anchored,
        );
        self.finish(builder, style, owner);
        for (element, node, note) in anchored {
            match note {
                Some(index) => self.note(element, node, index),
                None => self.element(element, node),
            }
        }
    }

    /// Collects the blocks of footnote `index` into a tree of their own,
    /// whose root is the footnote's block (SPEC §7.8). The block, and the
    /// marker before the note, take the note's style, which inherits from
    /// the document rather than from the anchor, but for the anchor's
    /// `lang`, and its `font` in another language
    /// ([`StyledNode::note_style`]).
    fn note(&mut self, element: &Element, node: &StyledNode, index: usize) {
        let restyled;
        let node = match node.note_style() {
            Some(note) => {
                restyled = StyledNode {
                    style: note.style.clone(),
                    ..node.clone()
                };
                &restyled
            }
            None => node,
        };
        let blocks = std::mem::take(&mut self.blocks);
        let root = self.root.take();
        let markers = std::mem::take(&mut self.markers);
        let in_note = std::mem::replace(&mut self.in_note, true);
        self.element(element, node);
        self.in_note = in_note;
        let note = self.root.take();
        let mark = note
            .as_ref()
            .and_then(|note| self.continuation(note, element));
        self.continued[index] = mark;
        self.root = root;
        self.blocks = blocks;
        self.markers = markers;
        self.notes[index] = note;
    }

    /// The paragraph of the mark that starts the part of a footnote that
    /// runs onto the next page: the word `continued` in the language and
    /// style of the note's first paragraph (SPEC §7.8). Called with no
    /// block open, so that the paragraph is in none.
    fn continuation(&mut self, note: &TextBlock, element: &Element) -> Option<usize> {
        fn first(block: &TextBlock) -> Option<usize> {
            block.children.iter().find_map(|child| match child {
                TextFlow::Paragraph(i) => Some(*i),
                TextFlow::Block(b) => first(b),
            })
        }
        let style = self.pending.get(first(note)?)?.style.clone();
        let lang = self.settings(&style, element.span).lang;
        let word = crate::locale::word(&lang, "continued")?;
        let mut builder = Builder::default();
        builder.push_part(word.text, &style, element.span, TextSource::WHOLE, false);
        let index = self.pending.len();
        self.finish(builder, &style, element);
        (self.pending.len() > index).then_some(index)
    }

    /// Adds inline content to a paragraph. Elements anchored in it whose
    /// content is blocks (notes, floats) go to `anchored`, a footnote with
    /// its index in [`Text::notes`].
    fn inline<'a>(
        &mut self,
        content: &'a Content,
        style: &ComputedStyle,
        span: Span,
        kids: &mut StyledChildren<'a>,
        out: &mut Builder,
        anchored: &mut Vec<Anchored<'a>>,
    ) {
        for node in content.nodes() {
            match node {
                // Text that records no node, such as a string joined to
                // content, stands for the whole element that holds it.
                Node::Text { text, span: own } if own.is_detached() => {
                    out.push_part(text, style, self.owner, TextSource::WHOLE, false);
                }
                Node::Text { text, span: own } => out.push(text, style, *own),
                // A space next to a forced line break would start or end a
                // line, so it is dropped.
                Node::Space(own) | Node::Parbreak(own) => {
                    if !out.text.ends_with('\u{2028}') {
                        out.push_implied(" ", style, own.or(span));
                    }
                }
                Node::Linebreak(own) => {
                    out.trim_space();
                    out.push_implied("\u{2028}", style, own.or(span));
                }
                Node::Quote { double, span: own } => {
                    let lang = self.settings(style, span).lang;
                    out.quote(Quote {
                        double: *double,
                        style: style.clone(),
                        lang,
                        span: own.or(span),
                    });
                }
                Node::Element(element) => {
                    // An element a later stage dropped has no node, and
                    // shows nothing.
                    let Some(node) = kids.element(element) else {
                        continue;
                    };
                    self.inline_element(element, node, out, anchored);
                }
                Node::Slot(_) | Node::Rule(_) => {}
            }
        }
    }

    /// Collects an inline element: its text comes from its instance.
    fn inline_element<'a>(
        &mut self,
        element: &'a Element,
        node: &'a StyledNode,
        out: &mut Builder,
        anchored: &mut Vec<Anchored<'a>>,
    ) {
        // A node without a location, such as generated text, is in the
        // instance around it.
        let within = if node.location == Location::DETACHED {
            out.within
        } else {
            node.location
        };
        let outer = std::mem::replace(&mut out.within, within);
        self.inline_instance(element, node, out, anchored);
        out.within = outer;
    }

    fn inline_instance<'a>(
        &mut self,
        element: &'a Element,
        node: &'a StyledNode,
        out: &mut Builder,
        anchored: &mut Vec<Anchored<'a>>,
    ) {
        let style = &node.style;
        if !is_builtin(&element.name, "image") {
            self.check_width(element, style);
        }
        if element.kind == ElementKind::Inline
            && let Some(expansion) = &node.template
            && expansion.body.is_some()
        {
            let owner = self.own(element);
            let placed = std::mem::take(&mut self.placed);
            if let Some(header) = &expansion.header {
                self.inline_pieces(element, node, &header.items, out, anchored);
            }
            if let Some(body) = &expansion.body {
                self.inline_pieces(element, node, body, out, anchored);
            }
            self.placed = placed;
            self.owner = owner;
            return;
        }
        match element.kind {
            ElementKind::Inline => {}
            // A footnote: its marker here, its blocks at the foot of the
            // page.
            ElementKind::Note if is_builtin(&element.name, "footnote") => {
                if self.in_note {
                    self.report_once(
                        "footnote-in-footnote".into(),
                        Diagnostic::warning(
                            element.span,
                            "a footnote inside a footnote is not placed, so the output shows \
                             only its marker",
                        )
                        .with_hint("write its text into the outer footnote"),
                    );
                }
                let index = self.notes.len();
                self.notes.push(None);
                self.continued.push(None);
                let marker = self
                    .resolution
                    .marker(element.location)
                    .filter(|m| m.part.is_none())
                    .map(|m| m.text.trim_end_matches(crate::resolve::NBSP))
                    .filter(|text| !text.is_empty());
                // A quotation mark before the anchor is not part of it.
                out.flush_quote(Some(
                    marker.and_then(|t| t.chars().next()).unwrap_or(OBJECT),
                ));
                let start = out.text.len();
                match marker {
                    Some(text) => {
                        out.push_part(text, style, element.span, TextSource::WHOLE, false);
                    }
                    None => out.object(style, element, node.location),
                }
                out.anchors.push(TextAnchor {
                    range: start..out.text.len(),
                    note: index,
                });
                anchored.push((element, node, Some(index)));
                return;
            }
            // A note or float anchored here: its content is blocks.
            _ => {
                out.object(style, element, node.location);
                anchored.push((element, node, None));
                return;
            }
        }
        if let Some(text) = raw_text(element) {
            out.raw(text, style, element.span);
            return;
        }
        // A total shows the sum Resolve gave it (SPEC §4.5).
        if is_builtin(&element.name, "total") {
            let text = match self.resolution.query(element.location).map(|q| &q.result) {
                Some(crate::resolve::QueryResult::Total(text)) => text.as_str(),
                _ => UNRESOLVED,
            };
            out.push_part(text, style, element.span, TextSource::WHOLE, false);
            return;
        }
        // A reference shows the text Resolve gave it.
        if is_builtin(&element.name, "ref") {
            let text = self
                .resolution
                .reference(element.location)
                .map_or(UNRESOLVED, |r| r.text.as_str());
            out.push_part(text, style, element.span, TextSource::WHOLE, false);
            return;
        }
        if is_builtin(&element.name, "link")
            && let Some(eval::Value::Str(dest)) = element.field("dest")
        {
            out.push(dest, style, element.span);
            return;
        }
        if let Some(eval::Value::Content(body)) = element.field(BODY) {
            let mut body_kids = StyledChildren::new(node);
            let owner = self.own(element);
            self.inline(body, style, element.span, &mut body_kids, out, anchored);
            self.owner = owner;
            return;
        }
        // Inline math: an object for each of its chunks, between which a
        // line may break, which math layout sizes.
        if let Some(formula) = self.formula(element, node, false) {
            let chunks = folio_math::layout::inline_chunks(&self.maths[formula].formula);
            for chunk in 0..chunks {
                out.object_with(
                    style,
                    element,
                    node.location,
                    Some(MathChunk { formula, chunk }),
                    None,
                );
            }
            return;
        }
        // An image: an object layout will size.
        if is_builtin(&element.name, "image") {
            let image = self.image(element, node);
            out.object_with(style, element, node.location, None, image);
            return;
        }
        self.not_laid_out(element);
        out.object(style, element, node.location);
    }

    /// The image of an `image` element, read, with the `width` and `alt`
    /// it asks for; `None`, with an error at the element, for a file that
    /// is not an image Folio reads. Evaluation checked that the file
    /// exists.
    fn image(&mut self, element: &Element, node: &StyledNode) -> Option<TextImage> {
        let Some(eval::Value::Str(path)) = element.field("path") else {
            return None;
        };
        let path = crate::project::ProjectPath::new(path.as_str())?;
        let file = data_file(self.db, self.project, path)?;
        match load_image(self.db, file) {
            Ok(loaded) if loaded.text && self.laid_out => {
                diag::report(
                    self.db,
                    Diagnostic::warning(
                        element.span,
                        "the text of this SVG image is not drawn: Folio does not set text in SVG images yet",
                    )
                    .with_hint("convert the text to paths (outlines) in the program that made the image"),
                );
                Some(self.text_image(loaded, element, node))
            }
            Ok(loaded) => Some(self.text_image(loaded, element, node)),
            Err(message) => {
                if self.laid_out {
                    diag::report(self.db, Diagnostic::error(element.span, message.clone()));
                }
                None
            }
        }
    }

    /// An image read, as an object of its paragraph: with its element's
    /// `width` and `alt`.
    fn text_image(&self, loaded: &LoadedImage, element: &Element, node: &StyledNode) -> TextImage {
        TextImage {
            loaded: loaded.clone(),
            width: node
                .style
                .value(self.props, self.props.builtin("width"))
                .cloned()
                .unwrap_or(Value::Auto),
            alt: match element.field("alt") {
                Some(eval::Value::Str(alt)) => Some(alt.clone()),
                _ => None,
            },
        }
    }

    /// Records the formula of a `math` element, and returns its index in
    /// [`Text::maths`]; `None` for an element that is no formula.
    fn formula(&mut self, element: &Element, node: &StyledNode, display: bool) -> Option<usize> {
        if !is_builtin(&element.name, "math") {
            return None;
        }
        let Some(eval::Value::Math(formula)) = element.field("tree") else {
            return None;
        };
        let lang = self.settings(&node.style, element.span).lang;
        let settings = crate::layout::math_settings(
            &node.style,
            node.vector_style().map(|v| &v.style),
            &lang,
            self.props,
        );
        self.maths.push(TextMath {
            formula: formula.clone(),
            settings,
            span: element.span,
            location: node.location,
            display,
            lead: None,
        });
        Some(self.maths.len() - 1)
    }

    /// Warns, once per build and element, that layout does not draw an
    /// element yet, so the output leaves it out (SPEC §7.9): the boxes of
    /// content that no stage lays out (#214).
    fn not_laid_out(&mut self, element: &Element) {
        let name = &element.name;
        let message = eco_format!("`{name}` is not laid out yet, so the output leaves it out");
        self.report_once(
            eco_format!("not-laid-out:{name}"),
            Diagnostic::warning(element.span, message)
                .with_hint("this version of Folio checks it but does not draw it"),
        );
    }

    /// Warns, once per build and element, that an element other than an
    /// image has a `width`, which only images read so far (SPEC §7.2,
    /// §7.9): the widths of blocks come with the work on page layout. The
    /// style stage cannot tell, as it reports a declaration without knowing
    /// which elements it applies to.
    fn check_width(&mut self, element: &Element, style: &ComputedStyle) {
        let width = style.value(self.props, self.props.builtin("width"));
        if width.is_none_or(|w| *w == Value::Auto) {
            return;
        }
        // Every target: no writer reads it either.
        let name = &element.name;
        if self.reported.insert(eco_format!("width:{name}")) {
            diag::report(
                self.db,
                Diagnostic::warning(
                    element.span,
                    eco_format!("`width` is not implemented yet for `{name}`, so it has no effect"),
                )
                .with_hint("only images are sized by `width` so far"),
            );
        }
    }

    /// A new paragraph, which starts with the markers waiting for one.
    fn builder(&mut self) -> Builder {
        let mut builder = Builder::default();
        for marker in self.markers.drain(..) {
            builder.push_part(
                &marker.text,
                &marker.style,
                marker.span,
                TextSource::WHOLE,
                false,
            );
        }
        builder
    }

    /// Turns a collected paragraph into segments, with fonts by fallback.
    /// `style` is that of the element whose content it is, `owner`.
    fn finish(&mut self, builder: Builder, style: &ComputedStyle, owner: &Element) {
        self.finish_as(builder, style, owner.span, owner.location);
    }

    /// As [`Self::finish`], for the content of the element instance at
    /// `location`, written at `span`.
    fn finish_as(
        &mut self,
        mut builder: Builder,
        style: &ComputedStyle,
        span: Span,
        location: Location,
    ) {
        builder.flush_quote(None);
        if builder.text.is_empty() {
            return;
        }
        let text = builder.text.as_str();
        let mut segments = EcoVec::new();
        let mut styles = EcoVec::new();
        for piece in &builder.pieces {
            let settings = self.settings(&piece.style, piece.span);
            let lang = Some(settings.lang.clone());
            if piece.object {
                segments.push(Segment {
                    range: piece.range.clone(),
                    kind: SegmentKind::Object,
                    lang,
                    hyphenate: settings.hyphenate,
                });
                styles.push(SegmentStyle {
                    style: piece.style.clone(),
                    face: None,
                });
                continue;
            }
            let chain = self.chain(&piece.style);
            for run in self.fonts.resolve(&chain, &text[piece.range.clone()]) {
                let range = piece.range.start + run.range.start..piece.range.start + run.range.end;
                let font = run.face.and_then(|face| self.text_font(face));
                segments.push(Segment {
                    range,
                    kind: SegmentKind::Text {
                        font,
                        features: settings.features.clone(),
                    },
                    lang: lang.clone(),
                    hyphenate: settings.hyphenate,
                });
                styles.push(SegmentStyle {
                    style: piece.style.clone(),
                    face: run.face,
                });
            }
        }
        let index = self.pending.len();
        if let Some(block) = self.blocks.last_mut() {
            block.children.push(TextFlow::Paragraph(index));
        }
        self.pending.push(CollectedParagraph {
            paragraph: Paragraph {
                text: builder.text.as_str().into(),
                segments,
            },
            style: style.clone(),
            span,
            location,
            segments: styles,
            sources: builder.sources.into_iter().collect(),
            objects: builder.objects.into_iter().collect(),
            notes: builder.anchors.into_iter().collect(),
        });
    }

    /// The shaping handle of a face.
    fn text_font(&self, face: usize) -> Option<TextFont> {
        let font = self.fonts.faces()[face].font()?;
        self.cache.font(font.data(), font.index())
    }

    /// The faces of a style's `font` chain.
    fn chain(&mut self, style: &ComputedStyle) -> EcoVec<usize> {
        let request = Request::of(style, self.props);
        let fonts = self.fonts;
        self.chains
            .entry(request)
            .or_insert_with_key(|request| {
                request
                    .families
                    .iter()
                    .filter_map(|family| fonts.select(family, request.variant))
                    .collect()
            })
            .clone()
    }

    /// What shaping and breaking read of a style, reporting values they
    /// cannot use at `span`.
    fn settings(&mut self, style: &ComputedStyle, span: Span) -> Settings {
        if let Some(settings) = self.settings.get(style) {
            return settings.clone();
        }
        let value = |name: &str| style.value(self.props, self.props.builtin(name));
        let lang = match value("lang") {
            Some(Value::Str(tag)) => match Lang::new(tag) {
                Some(lang) => lang,
                None => {
                    self.report_once(
                        eco_format!("lang:{tag}"),
                        Diagnostic::warning(span, eco_format!("`{tag}` is not a language tag"))
                            .with_hint("write a BCP 47 tag, such as `en`, `pl` or `en-GB`"),
                    );
                    self.default_lang.clone()
                }
            },
            _ => self.default_lang.clone(),
        };
        let justify = matches!(value("justify"), Some(Value::Bool(true)));
        let hyphenate = match value("hyphenate") {
            Some(Value::Bool(b)) => {
                if *b && lang.hyphenation().is_none() {
                    self.report_once(
                        eco_format!("hyphenate:{}", lang.primary()),
                        Diagnostic::warning(
                            span,
                            eco_format!(
                                "there are no hyphenation patterns for the language `{lang}`"
                            ),
                        )
                        .with_hint("the text is not hyphenated; set `hyphenate: false` to say so"),
                    );
                }
                *b
            }
            _ => justify,
        };
        let mut features = EcoVec::new();
        if let Some(Value::Array(items)) = value("features") {
            for item in items {
                let Value::Str(text) = item else { continue };
                match Feature::parse(text) {
                    Some(feature) => features.push(feature),
                    None => self.report_once(
                        eco_format!("feature:{text}"),
                        Diagnostic::warning(
                            span,
                            eco_format!("`{text}` is not an OpenType feature setting"),
                        )
                        .with_hint(
                            "write a tag of up to four characters, such as `smcp`, `-liga` or `ss01=2`",
                        ),
                    ),
                }
            }
        }
        let settings = Settings {
            lang,
            hyphenate,
            features,
        };
        self.settings.insert(style.clone(), settings.clone());
        settings
    }

    fn report_once(&mut self, key: EcoString, diagnostic: Diagnostic) {
        if self.laid_out && self.reported.insert(key) {
            diag::report(self.db, diagnostic);
        }
    }
}

impl Builder {
    /// Appends text in a style; `span` is the node it is the text of.
    fn push(&mut self, text: &str, style: &ComputedStyle, span: Span) {
        self.push_part(text, style, span, 0..text.len(), false);
    }

    /// Appends a space or line break that markup implies, the text of node
    /// `span`. Problems with its style are reported at the text around it,
    /// not at the whitespace.
    fn push_implied(&mut self, text: &str, style: &ComputedStyle, span: Span) {
        self.push_part(text, style, span, 0..text.len(), true);
    }

    /// Appends text in a style that is the bytes `within` of the text of
    /// node `span` (see [`TextSource::within`]); `implied` as for
    /// [`Self::push_implied`].
    fn push_part(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        span: Span,
        within: Range<usize>,
        implied: bool,
    ) {
        if text.is_empty() {
            return;
        }
        self.flush_quote(text.chars().next());
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        if !span.is_detached() {
            self.sources.push(TextSource {
                range: start..end,
                span,
                within,
                location: self.within,
            });
        }
        let report = if implied { Span::detached() } else { span };
        self.extend(start..end, style, false, report);
    }

    /// Appends the text of a `raw` element, `text`, with its line breaks as
    /// forced ones. Each line is the same part of the element's text, and
    /// each break stands for the newline it replaces.
    fn raw(&mut self, text: &str, style: &ComputedStyle, span: Span) {
        let mut at = 0;
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.push_part("\u{2028}", style, span, at - 1..at, false);
            }
            self.push_part(line, style, span, at..at + line.len(), false);
            at += line.len() + 1;
        }
    }

    /// Removes a space that ends the text, which markup's spaces give.
    fn trim_space(&mut self) {
        if self.quote.is_some() || !self.text.ends_with(' ') {
            return;
        }
        let end = self.text.len() - 1;
        self.text.truncate(end);
        if let Some(last) = self.pieces.last_mut() {
            last.range.end = end;
            if last.range.is_empty() {
                self.pieces.pop();
            }
        }
        if let Some(last) = self.sources.last_mut()
            && last.range.end > end
        {
            if last.range.len() == last.within.len() {
                last.within.end -= last.range.end - end;
            }
            last.range.end = end;
            if last.range.is_empty() {
                self.sources.pop();
            }
        }
    }

    /// Appends an object in a style, standing for the instance `location`
    /// of `element`.
    fn object(&mut self, style: &ComputedStyle, element: &Element, location: Location) {
        self.object_with(style, element, location, None, None);
    }

    /// An object character for an element, for a chunk of its formula, or
    /// for its image.
    fn object_with(
        &mut self,
        style: &ComputedStyle,
        element: &Element,
        location: Location,
        math: Option<MathChunk>,
        image: Option<TextImage>,
    ) {
        self.flush_quote(Some(OBJECT));
        let start = self.text.len();
        self.text.push(OBJECT);
        self.objects.push(TextObject {
            at: start,
            element: element.name.clone(),
            span: element.span,
            location,
            math,
            image,
        });
        self.extend(start..self.text.len(), style, true, element.span);
    }

    fn extend(&mut self, range: Range<usize>, style: &ComputedStyle, object: bool, span: Span) {
        if let Some(last) = self.pieces.last_mut()
            && !object
            && !last.object
            && last.range.end == range.start
            && (last.style.ptr_eq(style) || last.style == *style)
        {
            last.range.end = range.end;
            if last.span.is_detached() {
                last.span = span;
            }
            return;
        }
        self.pieces.push(Piece {
            range,
            style: style.clone(),
            object,
            span,
        });
    }

    /// Adds a quotation mark, whose form waits for the character after it.
    fn quote(&mut self, quote: Quote) {
        let mark = if quote.double { '"' } else { '\'' };
        self.flush_quote(Some(mark));
        self.quote = Some(quote);
    }

    /// Writes the pending quotation mark in its language's form, given the
    /// character after it.
    fn flush_quote(&mut self, after: Option<char>) {
        let Some(quote) = self.quote.take() else {
            return;
        };
        let before = self.text.chars().next_back();
        let mark = quote.lang.quotes().mark(quote.double, before, after);
        let mut buf = [0; 4];
        self.push(mark.encode_utf8(&mut buf), &quote.style, quote.span);
    }
}

/// The node of an element's part `name`, if it has one.
fn part_node<'a>(node: &'a StyledNode, name: &Name) -> Option<&'a StyledNode> {
    node.children
        .iter()
        .find(|child| matches!(&child.name, crate::style::NodeName::Part(n) if n == name))
}

/// The text of a `raw` element without the line breaks it ends with.
fn raw_text(element: &Element) -> Option<&str> {
    if !is_builtin(&element.name, "raw") {
        return None;
    }
    let Some(eval::Value::Str(text)) = element.field("text") else {
        return None;
    };
    Some(text.trim_end_matches('\n'))
}

/// Whether an element is the built-in element of a name.
fn is_builtin(name: &QualifiedName, builtin: &str) -> bool {
    name.namespace.is_none() && name.name.as_str() == builtin
}

/// Whether content is inline: it has text, or an inline element or a note.
/// Content of blocks has neither: the document model groups inline content
/// between blocks into paragraphs.
fn is_inline(content: &Content) -> bool {
    content.nodes().iter().any(|node| match node {
        Node::Text { .. } | Node::Space(_) | Node::Linebreak(_) | Node::Quote { .. } => true,
        Node::Element(element) => matches!(element.kind, ElementKind::Inline | ElementKind::Note),
        _ => false,
    })
}
