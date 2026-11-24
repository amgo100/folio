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
//!   fallback ([`Fonts::resolve`]); the font stage reports what is missing.
//! - **Language and hyphenation**: `lang` (or the manifest's language when
//!   it is `auto`) chooses glyph forms, hyphenation patterns and line-end
//!   rules; `hyphenate: auto` hyphenates justified paragraphs.
//!
//! Shaping is memoised per paragraph in the database's [`Cache`]: the query
//! hands every paragraph of the build to it, the new ones are shaped in
//! parallel with `rayon`, and an edit re-shapes only the paragraphs it
//! changed. The query itself is memoised like any other.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, ElementKind, QualifiedName, Value};
use folio_syntax::Span;
use folio_text::{Feature, Lang, OBJECT, Paragraph, Segment, SegmentKind, Shaped, TextFont};

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{self, Content, Element, Node};
use crate::fonts::{Fonts, Request, fonts};
use crate::model::document;
use crate::style::{
    Child, ComputedStyle, Properties, StyledNode, content_elements, styled_children, styles,
};

pub use folio_text::Cache;

/// The shaped paragraphs of a build, in document order.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Text {
    /// The paragraphs.
    pub paragraphs: EcoVec<TextParagraph>,
}

/// One paragraph of a build, shaped.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextParagraph {
    /// The paragraph shaped, shared with every build and revision that has
    /// an equal paragraph.
    pub shaped: Arc<Shaped>,
    /// For each segment of the paragraph ([`Paragraph::segments`]), the
    /// style its text is set in and its face.
    pub segments: EcoVec<SegmentStyle>,
    /// Where the paragraph's text came from: ranges of it that are the text
    /// of a source node, in order. Spaces, quotation marks and objects that
    /// markup implies are in no range.
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
    /// The node: a text node of markup, or a `raw` element.
    pub span: Span,
}

/// The paragraphs of a build, shaped, or `None` when the document could not
/// be styled. Reports `features` that are not OpenType feature settings,
/// `lang` values that are not language tags, and `hyphenate: true` for a
/// language without hyphenation patterns.
#[salsa::tracked(returns(ref))]
pub fn text(db: &dyn Db, build: Build<'_>) -> Option<Text> {
    let project = build.project(db);
    let styled = styles(db, build).as_ref()?;
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
        pending: Vec::new(),
    };
    collector.element(&doc.root, &styled.root);

    let pending = collector.pending;
    let inputs: Vec<Paragraph> = pending.iter().map(|p| p.paragraph.clone()).collect();
    let shaped = db.text_cache().shape_all(&inputs);
    let paragraphs = pending
        .into_iter()
        .zip(shaped)
        .map(|(p, shaped)| TextParagraph {
            shaped,
            segments: p.segments,
            sources: p.sources,
        })
        .collect();
    Some(Text { paragraphs })
}

/// A paragraph made, before shaping.
struct Pending {
    paragraph: Paragraph,
    segments: EcoVec<SegmentStyle>,
    sources: EcoVec<TextSource>,
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
    pending: Vec<Pending>,
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
    /// Collects the paragraphs of an element and its descendants.
    fn element(&mut self, element: &Element, node: &StyledNode) {
        // A code block is a paragraph whose lines end in forced breaks.
        if let Some(text) = raw_text(element) {
            let mut builder = Builder::default();
            builder.push(&text, &node.style, Some(element.span));
            self.finish(builder);
            return;
        }
        let kids = styled_children(element, node);
        if let Some(eval::Value::Content(body)) = element.field(BODY) {
            let mut body_kids = kids.iter().filter_map(|(child, node)| match child {
                Child::Element(e) => Some((*e, *node)),
                Child::Part(..) => None,
            });
            self.content(body, &node.style, element.span, &mut body_kids);
        }
        for (child, node) in &kids {
            if let Child::Part(_, content) = child {
                let mut part_kids = content_elements(content).zip(node.children.iter());
                self.content(content, &node.style, element.span, &mut part_kids);
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
        self.finish(builder);
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
                Node::Text { text, span } => out.push(text, style, Some(*span)),
                // A space next to a forced line break would start or end a
                // line, so it is dropped.
                Node::Space | Node::Parbreak => {
                    if !out.text.ends_with('\u{2028}') {
                        out.push(" ", style, None);
                    }
                }
                Node::Linebreak => {
                    out.trim_space();
                    out.push("\u{2028}", style, None);
                }
                Node::Quote { double } => {
                    let lang = self.settings(style, span).lang;
                    out.quote(Quote {
                        double: *double,
                        style: style.clone(),
                        lang,
                        span,
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
            out.push(&text, style, Some(element.span));
            return;
        }
        if is_builtin(&element.name, "link")
            && let Some(eval::Value::Str(dest)) = element.field("dest")
        {
            out.push(dest, style, Some(element.span));
            return;
        }
        if let Some(eval::Value::Content(body)) = element.field(BODY) {
            let kids = styled_children(element, node);
            let mut body_kids = kids.iter().filter_map(|(child, node)| match child {
                Child::Element(e) => Some((*e, *node)),
                Child::Part(..) => None,
            });
            self.inline(body, style, element.span, &mut body_kids, out, anchored);
            return;
        }
        // Inline math, an image, a reference: an object layout sizes.
        out.object(style, element.span);
    }

    /// Turns a collected paragraph into segments, with fonts by fallback.
    fn finish(&mut self, mut builder: Builder) {
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
        self.pending.push(Pending {
            paragraph: Paragraph {
                text: builder.text.as_str().into(),
                segments,
            },
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
        if self.reported.insert(key) {
            diag::report(self.db, diagnostic);
        }
    }
}

impl Builder {
    /// Appends text in a style; `span` is the node it is the text of.
    fn push(&mut self, text: &str, style: &ComputedStyle, span: Option<Span>) {
        if text.is_empty() {
            return;
        }
        self.flush_quote(text.chars().next());
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        if let Some(span) = span {
            self.sources.push(TextSource {
                range: start..end,
                span,
            });
        }
        self.extend(
            start..end,
            style,
            false,
            span.unwrap_or_else(Span::detached),
        );
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
        self.push(mark.encode_utf8(&mut buf), &quote.style, None);
        if let Some(last) = self.pieces.last_mut()
            && last.span.is_detached()
        {
            last.span = quote.span;
        }
    }
}

/// The text of a `raw` element, with its line breaks as forced ones.
fn raw_text(element: &Element) -> Option<EcoString> {
    if !is_builtin(&element.name, "raw") {
        return None;
    }
    let Some(eval::Value::Str(text)) = element.field("text") else {
        return None;
    };
    Some(text.trim_end_matches('\n').replace('\n', "\u{2028}").into())
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
        Node::Text { .. } | Node::Space | Node::Linebreak | Node::Quote { .. } => true,
        Node::Element(element) => matches!(element.kind, ElementKind::Inline | ElementKind::Note),
        _ => false,
    })
}
