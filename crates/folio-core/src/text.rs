//! The text stage (SPEC §7.2): a build's paragraphs, itemised, shaped and
//! with their break opportunities, by `folio-text`.
//!
//! - **Paragraphs**: every run of inline content in the styled document is
//!   one paragraph: the body of a `paragraph` or `heading`, and inline
//!   content fields such as a caption. Its text joins the text of the inline
//!   elements in it (`emph`, `strong`, `raw`, a component's inline
//!   elements), each in its own style. A space is U+0020, a forced line
//!   break U+2028, and an element without text (inline math, an image, a
//!   reference, a note's or float's anchor) the object character, which
//!   layout sizes. The blocks of notes and floats anchored in a paragraph
//!   are paragraphs of their own.
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
//! it, the new ones are shaped in parallel with `rayon`, and an edit
//! re-shapes only the paragraphs it changed. The queries themselves are
//! memoised like any other.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, ElementKind, Name, QualifiedName, Target, Value};
use folio_syntax::Span;
use folio_text::{Feature, Lang, OBJECT, Paragraph, Segment, SegmentKind, Shaped, TextFont};

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{self, Content, Element, Node};
use crate::fonts::{Fonts, Request, fonts};
use crate::model::document;
use crate::pipeline::step_styled;
use crate::style::{
    Child, ComputedStyle, Properties, StyledNode, content_elements, styled_children,
};

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
    /// The name of that element: what the block is, for the writers that
    /// tag output by meaning, such as PDF's structure tree (SPEC §11.1).
    pub element: QualifiedName,
    /// For a part, the content field it holds; `None` for the element's own
    /// block.
    pub part: Option<Name>,
    /// What it holds, in order.
    pub children: EcoVec<TextFlow>,
}

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
    /// For each segment of the paragraph ([`Paragraph::segments`]), the
    /// style its text is set in and its face.
    pub segments: EcoVec<SegmentStyle>,
    /// Where the paragraph's text came from: ranges of it that are the text
    /// of a source node, in order. Objects are in no range.
    pub sources: EcoVec<TextSource>,
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
    let shaped = db.text_cache().shape_all(&inputs);
    let paragraphs = collected
        .paragraphs
        .iter()
        .zip(shaped)
        .map(|(p, shaped)| TextParagraph {
            shaped,
            style: p.style.clone(),
            span: p.span,
            segments: p.segments.clone(),
            sources: p.sources.clone(),
        })
        .collect();
    Some(Text {
        paragraphs,
        root: collected.root.clone(),
    })
}

/// The paragraphs of a build before shaping, with the blocks that hold them.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Collected {
    /// The paragraphs, in the order of [`Text::paragraphs`].
    pub paragraphs: EcoVec<CollectedParagraph>,
    /// The blocks that hold them, as [`Text::root`].
    pub root: TextBlock,
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
    /// As [`TextParagraph::segments`].
    pub segments: EcoVec<SegmentStyle>,
    /// As [`TextParagraph::sources`].
    pub sources: EcoVec<TextSource>,
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
    let doc = document(db, project, build.variant(db).clone()).as_ref()?;
    let fonts = fonts(db, project);
    let default_lang =
        Lang::new(project.manifest(db).project.lang.v.as_str()).unwrap_or_else(Lang::english);

    let mut collector = Collector {
        db,
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
    };
    collector.element(&doc.root, &styled.root);

    let root = collector.root.take().expect("the root is a block");
    Some(Collected {
        paragraphs: collector.pending.into_iter().collect(),
        root,
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
}

/// A paragraph's text as it is collected.
#[derive(Default)]
struct Builder {
    text: String,
    /// Ranges of one style and kind, in order, covering the text.
    pieces: Vec<Piece>,
    sources: Vec<TextSource>,
    /// A quotation mark whose form waits for the character after it.
    quote: Option<Quote>,
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
        self.blocks.push(TextBlock {
            style: style.clone(),
            span: element.span,
            element: element.name.clone(),
            part,
            children: EcoVec::new(),
        });
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
        self.open(&node.style, element, None);
        let owner = self.own(element);
        self.element_content(element, node);
        self.owner = owner;
        self.close();
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
            let mut builder = Builder::default();
            builder.raw(text, &node.style, element.span);
            self.finish(builder, &node.style, element.span);
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
        // they are written.
        let kids = styled_children(element, node);
        let mut parts = kids.iter().filter_map(|(child, node)| match child {
            Child::Part(_, content) => Some((*content, *node)),
            Child::Element(_) => None,
        });
        for (key, value) in element.fields.iter() {
            let eval::Value::Content(content) = value else {
                continue;
            };
            if key == BODY {
                let mut body_kids = kids.iter().filter_map(|(child, node)| match child {
                    Child::Element(e) => Some((*e, *node)),
                    Child::Part(..) => None,
                });
                self.content(content, &node.style, element.span, &mut body_kids);
            } else if let Some(field) = Name::new(key)
                && let Some((part, part_node)) = parts.next()
            {
                let mut part_kids = content_elements(part).zip(part_node.children.iter());
                self.open(&part_node.style, element, Some(field));
                self.content(part, &part_node.style, element.span, &mut part_kids);
                self.close();
            }
        }
    }

    /// Collects the paragraphs of content: one paragraph if it is inline,
    /// else those of its blocks. `kids` yields the styled node of each
    /// element of the content, in order.
    fn content<'a>(
        &mut self,
        content: &'a Content,
        style: &ComputedStyle,
        span: Span,
        kids: &mut dyn Iterator<Item = (&'a Element, &'a StyledNode)>,
    ) {
        if !is_inline(content) {
            for node in content.nodes() {
                if let Node::Element(_) = node
                    && let Some((element, node)) = kids.next()
                {
                    self.element(element, node);
                }
            }
            return;
        }
        let mut builder = Builder::default();
        let mut anchored = Vec::new();
        self.inline(content, style, span, kids, &mut builder, &mut anchored);
        self.finish(builder, style, span);
        for (element, node) in anchored {
            self.element(element, node);
        }
    }

    /// Adds inline content to a paragraph. Elements anchored in it whose
    /// content is blocks (notes, floats) go to `anchored`.
    fn inline<'a>(
        &mut self,
        content: &'a Content,
        style: &ComputedStyle,
        span: Span,
        kids: &mut dyn Iterator<Item = (&'a Element, &'a StyledNode)>,
        out: &mut Builder,
        anchored: &mut Vec<(&'a Element, &'a StyledNode)>,
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
                    let Some((_, node)) = kids.next() else {
                        continue;
                    };
                    self.inline_element(element, node, out, anchored);
                }
                Node::Slot(_) | Node::Rule(_) => {}
            }
        }
    }

    fn inline_element<'a>(
        &mut self,
        element: &'a Element,
        node: &'a StyledNode,
        out: &mut Builder,
        anchored: &mut Vec<(&'a Element, &'a StyledNode)>,
    ) {
        let style = &node.style;
        match element.kind {
            ElementKind::Inline => {}
            // A note or float anchored here: its content is blocks.
            _ => {
                out.object(style, element.span);
                anchored.push((element, node));
                return;
            }
        }
        if let Some(text) = raw_text(element) {
            out.raw(text, style, element.span);
            return;
        }
        if is_builtin(&element.name, "link")
            && let Some(eval::Value::Str(dest)) = element.field("dest")
        {
            out.push(dest, style, element.span);
            return;
        }
        if let Some(eval::Value::Content(body)) = element.field(BODY) {
            let kids = styled_children(element, node);
            let mut body_kids = kids.iter().filter_map(|(child, node)| match child {
                Child::Element(e) => Some((*e, *node)),
                Child::Part(..) => None,
            });
            let owner = self.own(element);
            self.inline(body, style, element.span, &mut body_kids, out, anchored);
            self.owner = owner;
            return;
        }
        // Inline math, an image, a reference: an object layout sizes.
        self.not_laid_out(element);
        out.object(style, element.span);
    }

    /// Warns, once per build and element, that layout does not draw an
    /// element yet, so the output leaves it out (SPEC §7.9). Each warning
    /// goes with the work that lays its element out: maths with
    /// `folio-math` (#43), references with the Resolve stage (#44), images
    /// with figures and images (#47), and the boxes of other content with
    /// #214.
    fn not_laid_out(&mut self, element: &Element) {
        let name = &element.name;
        let builtin = |text: &str| is_builtin(name, text);
        let message = if builtin("math") {
            EcoString::from("maths is not laid out yet, so the output leaves it out")
        } else if builtin("ref") {
            EcoString::from("references are not laid out yet, so the output leaves them out")
        } else if builtin("image") {
            EcoString::from("images are not laid out yet, so the output leaves them out")
        } else {
            eco_format!("`{name}` is not laid out yet, so the output leaves it out")
        };
        self.report_once(
            eco_format!("not-laid-out:{name}"),
            Diagnostic::warning(element.span, message)
                .with_hint("this version of Folio checks it but does not draw it"),
        );
    }

    /// Turns a collected paragraph into segments, with fonts by fallback.
    /// `style` and `span` are those of the element whose content it is.
    fn finish(&mut self, mut builder: Builder, style: &ComputedStyle, span: Span) {
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
            segments: styles,
            sources: builder.sources.into_iter().collect(),
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

    /// Appends an object in a style.
    fn object(&mut self, style: &ComputedStyle, span: Span) {
        self.flush_quote(Some(OBJECT));
        let start = self.text.len();
        self.text.push(OBJECT);
        self.extend(start..self.text.len(), style, true, span);
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
