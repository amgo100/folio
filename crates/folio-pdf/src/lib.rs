//! PDF output through `krilla` (SPEC §11.1): tagging, PDF/UA, PDF/A, font
//! subsetting, colour management and import of PDF figures.
//!
//! The Emit stage (SPEC §2.1) hands [`write()`] a build's pages as
//! fragments, the fonts their glyph runs name, the document's structure
//! and its metadata. The writer:
//!
//! - draws every glyph run in its font, which krilla embeds as a subset
//!   with a ToUnicode map, so text can be searched and copied (§2.5); a
//!   hyphen that a line break inserts has the actual text U+00AD, as ISO
//!   32000 asks, so a hyphenated word extracts whole, and a hyphen that a
//!   line repeats (Polish, at a break after an explicit hyphen) has none;
//! - tags the pages from the first version: the structure tree is built
//!   from the semantic tree ([`Tag`]), every glyph run is marked content of
//!   the element whose box holds it, and shapes, which are the backgrounds
//!   and borders of boxes, are artifacts (see the `tags` module);
//! - writes bookmarks from headings, nested as their sections are, and the
//!   title, subject and language as metadata;
//! - navigates ([`Navigation`]): a named destination for each label, at
//!   the first box of the element it names; a link annotation over the
//!   text of each reference to it, tagged as a `Link` structure element
//!   with the reference's text as its alternative description; and page
//!   labels, written for every page when some page's label differs from
//!   its plain number (1, 2, 3), which viewers show without labels;
//! - targets PDF/UA-1 on PDF 1.7. PDF/UA requires a title, so a document
//!   without one is written tagged but without claiming PDF/UA-1; that is
//!   no warning, as titles are optional. Where a document with a title
//!   cannot meet PDF/UA-1 (a heading without text, a character that maps
//!   to no Unicode value), the writer says why and writes the same tagged
//!   PDF without the claim.
//!
//! Before it writes, the writer checks the document ([`preflight()`]): it
//! walks the pages as it would draw them, without drawing, and finds what
//! it will warn about, including why a document with a title cannot claim
//! PDF/UA-1. So a build that only checks a document (`folio check`) gets
//! the writer's warnings without a file, and a document that cannot meet
//! PDF/UA-1 is written once, without the claim, rather than written under
//! the validator and again without it. Only a failure the check does not
//! foresee still costs the second writing.
//!
//! Like every writer it reads no clock, so the same input gives the same
//! bytes (SPEC §2.4): the PDF has no dates, and its IDs are hashes of its
//! content.

#![forbid(unsafe_code)]

mod tags;

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Debug, Formatter};
use std::num::{NonZeroU32, NonZeroU64};
use std::ops::Range;
use std::sync::Arc;

use ecow::EcoString;
use folio_component::{
    Abs, FillRule, FontId, Fragment, FragmentItem, GlyphRun, LineCap, LineJoin, Location, Paint,
    PathItem, Point, Shape, SourceRef,
};
use folio_fonts::Font;
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::configure::{
    Accessibility, ConfigurationBuilder, PdfVersion, ValidationError, Validator,
};
use krilla::destination::{NamedDestination, XyzDestination};
use krilla::error::KrillaError;
use krilla::geom::{PathBuilder, Rect, Size, Transform};
use krilla::metadata::Metadata;
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{self, Fill, Stroke, StrokeDash};
use krilla::surface::Surface;
use krilla::tagging::{ArtifactType, ContentTag, SpanTag};
use krilla::text::{GlyphId, KrillaGlyph};
use krilla::{Data, SerializeSettings};

use self::tags::Tagger;

/// What the PDF writer writes: a build's pages, in order, the fonts their
/// glyph runs name, the document's structure and its metadata.
pub struct Document<'a> {
    /// The pages: fragments whose size is the page's, holding everything
    /// on it. A page and the regions directly in it, such as its body, are
    /// not boxes of the structure; the groups in the regions are, as
    /// [`Tag`] says.
    pub pages: Vec<&'a Fragment>,
    /// The fonts that glyph runs name by [`FontId`].
    pub fonts: &'a dyn FontTable,
    /// The document's structure: the semantic tree whose boxes the
    /// fragments are.
    pub structure: &'a Tag,
    /// The document's metadata.
    pub info: Info,
    /// Its destinations, links and page labels.
    pub navigation: &'a Navigation,
}

impl Debug for Document<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("pages", &self.pages)
            .field("structure", &self.structure)
            .field("info", &self.info)
            .field("navigation", &self.navigation)
            .finish_non_exhaustive()
    }
}

/// The fonts of a build, by the ids that glyph runs name them by.
pub trait FontTable {
    /// The font with an id, or `None` when it cannot be loaded.
    fn font(&self, id: FontId) -> Option<Font>;
}

/// A document's metadata.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Info {
    /// The title, which PDF/UA requires and viewers show instead of the
    /// file name.
    pub title: Option<EcoString>,
    /// What the document is about.
    pub subject: Option<EcoString>,
    /// The language of its text, as a BCP 47 tag.
    pub lang: Option<EcoString>,
}

/// How a reader moves through a document (SPEC §11.1): named destinations
/// for labels, links for references, and page labels.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Navigation {
    /// The named destinations, each at the first box of an element
    /// instance, in document order. A destination whose element has no box
    /// on the pages is left out, and so are the links to it.
    pub destinations: Vec<Destination>,
    /// The links, in document order.
    pub links: Vec<Link>,
    /// The label of each page, in order; or empty, as when every page is
    /// labelled with its plain number (1, 2, 3), for which the writer
    /// writes no labels, as viewers show those without them.
    pub page_labels: Vec<PageLabel>,
}

/// A named destination: the place a link goes to.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Destination {
    /// Its name, which other documents can link to as well: a label,
    /// without angle brackets (`fig:ball`).
    pub name: EcoString,
    /// The element instance at whose first box it is: the top left corner
    /// of the first group or object of it that the pages draw, or else of
    /// the first glyph run of its source.
    /// [`Location::UNKNOWN_INSTANCE`] matches a box of any instance.
    pub location: Location,
}

/// A link: text whose glyphs come from one source, which goes to a named
/// destination when clicked.
///
/// The glyphs of a link name its source ([`folio_component::Glyph::source`]),
/// as those of a reference name the `ref` element's node. A source may make
/// several links, as a reference in a loop does: the writer gives each run
/// of glyphs of the source, one after another in the order drawn, the next
/// link of that source, and the last one to any after them. A run ends
/// where a glyph of another source follows, or where its glyphs have shown
/// as many bytes as the link's text has. A run may break across lines and
/// pages; it is one `Link` element, with an annotation for each line.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Link {
    /// The source of its glyphs.
    pub source: SourceRef,
    /// The name of the destination it goes to; `None` for text that goes
    /// nowhere, such as a reference that does not resolve, which keeps the
    /// order of the source's links.
    pub destination: Option<EcoString>,
    /// The text it shows (`Figure 3`): where a run of its source's glyphs
    /// ends, by the bytes of text they show, and its alternative
    /// description, which PDF/UA requires, with plain spaces (or its
    /// destination's name, where it has no words).
    pub text: EcoString,
}

/// The label of a page, as a viewer shows it in its page box: a prefix and
/// a number (SPEC §10.1, §11.1).
///
/// Front matter is `i`, `ii` ([`PageNumbering::LowerRoman`]), the body
/// `1`, `2`; the step pages of a slide are `4a`, `4b`, … `4z`, `4aa`
/// (prefix `4`, [`PageNumbering::LowerAlpha`]), and its sample pages
/// `4-s1`, `4-s2` (prefix `4-s`, [`PageNumbering::Arabic`]).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct PageLabel {
    /// The text before the number.
    pub prefix: EcoString,
    /// How the number is written; `None` for a label that is its prefix
    /// alone.
    pub numbering: Option<PageNumbering>,
    /// The number, from 1; 0 writes no number.
    pub number: u32,
}

/// How a page label writes its number.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum PageNumbering {
    /// 1, 2, 3.
    Arabic,
    /// i, ii, iii.
    LowerRoman,
    /// I, II, III.
    UpperRoman,
    /// a, b, … z, aa, ab: letters counted as Beamer and spreadsheets count
    /// them, so that `aa` follows `z` (PDF's own letters would give `aa`,
    /// `bb`; the writer writes such labels as prefixes).
    LowerAlpha,
    /// A, B, … Z, AA, AB.
    UpperAlpha,
}

impl PageLabel {
    /// A page's plain number: `1`, `2`.
    pub fn arabic(number: u32) -> Self {
        Self {
            prefix: EcoString::new(),
            numbering: Some(PageNumbering::Arabic),
            number,
        }
    }

    /// The label as a viewer shows it.
    pub fn text(&self) -> EcoString {
        let mut text = self.prefix.clone();
        if let Some(numbering) = self.numbering
            && self.number > 0
        {
            text.push_str(&numbering.format(self.number));
        }
        text
    }

    /// The label as PDF writes it: a numbering style that PDF has, with the
    /// number as its start, or the whole label as the prefix.
    fn to_krilla(&self) -> krilla::page::PageLabel {
        use krilla::page::NumberingStyle as S;
        let style = match self.numbering {
            _ if self.number == 0 => None,
            Some(PageNumbering::Arabic) => Some(S::Arabic),
            Some(PageNumbering::LowerRoman) => Some(S::LowerRoman),
            Some(PageNumbering::UpperRoman) => Some(S::UpperRoman),
            // PDF's letters differ from Folio's after `z`.
            Some(PageNumbering::LowerAlpha) if self.number <= 26 => Some(S::LowerAlpha),
            Some(PageNumbering::UpperAlpha) if self.number <= 26 => Some(S::UpperAlpha),
            Some(PageNumbering::LowerAlpha | PageNumbering::UpperAlpha) | None => None,
        };
        let (prefix, offset) = match style {
            Some(style) => (
                self.prefix.clone(),
                NonZeroU32::new(self.number).map(|n| (style, n)),
            ),
            None => (self.text(), None),
        };
        let prefix = (!prefix.is_empty()).then(|| prefix.to_string());
        match offset {
            Some((style, number)) => {
                krilla::page::PageLabel::new(Some(style), prefix, Some(number))
            }
            None => krilla::page::PageLabel::new(None, prefix, None),
        }
    }
}

impl PageNumbering {
    /// A number, from 1, written this way.
    pub fn format(self, number: u32) -> EcoString {
        match self {
            Self::Arabic => eco(number.to_string()),
            Self::LowerRoman => roman(number).to_lowercase(),
            Self::UpperRoman => roman(number),
            Self::LowerAlpha => letters(number, b'a'),
            Self::UpperAlpha => letters(number, b'A'),
        }
    }
}

/// A number in Roman numerals, upper case; above 3999, as many `M`s as it
/// takes.
fn roman(mut number: u32) -> EcoString {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut text = EcoString::new();
    for (value, numeral) in NUMERALS {
        while number >= value {
            text.push_str(numeral);
            number -= value;
        }
    }
    text
}

/// A number, from 1, in letters from `first`: a … z, aa, ab, … zz, aaa.
fn letters(number: u32, first: u8) -> EcoString {
    let mut bytes = Vec::new();
    let mut n = number;
    while n > 0 {
        n -= 1;
        bytes.push(first + (n % 26) as u8);
        n /= 26;
    }
    bytes.reverse();
    String::from_utf8(bytes).map_or_else(|_| EcoString::new(), Into::into)
}

/// A node of the document's structure: an element, or a part of one,
/// whose boxes are fragments with its source.
///
/// Fragments name the node they are a box of by its source, and a source
/// may be shared by an element and its first child (a section and its
/// heading) or by an element and its parts. The writer tells them apart by
/// nesting and order: a fragment within a node's box is a box of one of the
/// node's children with its source, in document order; only a box at the
/// top of a page continues a child from an earlier page. A fragment that
/// matches no child belongs to the node around it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Tag {
    /// What the node is.
    pub role: Role,
    /// The source its fragments name.
    pub source: SourceRef,
    /// Which instance of the source's element the node is, as its
    /// fragments name it ([`Fragment::location`]): the blocks that a loop
    /// or the calls of a closure make share a source and differ here.
    /// [`Location::UNKNOWN_INSTANCE`] matches a box of any instance.
    pub instance: u32,
    /// The node's text where the writer needs it whole: a heading's, for
    /// its structure element's title and its bookmark.
    pub text: Option<EcoString>,
    /// The nodes it holds, in document order.
    pub children: Vec<Tag>,
}

impl Tag {
    /// A node without text or children, of any instance of `source`
    /// ([`Location::UNKNOWN_INSTANCE`]).
    pub fn new(role: Role, source: SourceRef) -> Self {
        Self {
            role,
            source,
            instance: Location::UNKNOWN_INSTANCE,
            text: None,
            children: Vec::new(),
        }
    }
}

/// What a node of the structure is, as PDF's standard structure types
/// know it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Role {
    /// The document (`Document`).
    Document,
    /// The document's title as it is shown (`Title`).
    Title,
    /// A heading and what follows it (`Sect`). The sections around a
    /// heading give its level.
    Section,
    /// A heading (`H1` to `H6`, by the sections it is in).
    Heading,
    /// A paragraph (`P`).
    Paragraph,
    /// A list (`L`).
    List(ListKind),
    /// An item of a list (`LI`). What it holds that is not a [`Role::Label`]
    /// is its body (`LBody`).
    ListItem,
    /// The label of a list item, such as a term (`Lbl`).
    Label,
    /// A caption (`Caption`).
    Caption,
    /// A note (`Note`).
    Note,
    /// A block of code (`Code`).
    Code,
    /// Anything else (`Div`): an element without a standard meaning, such
    /// as most components' elements.
    Group,
}

/// How a list labels its items.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ListKind {
    /// With bullets.
    Bulleted,
    /// With numbers.
    Numbered,
    /// With terms, as a description list.
    Terms,
}

/// A PDF, and what the writer could not do as asked.
#[derive(Debug, Clone)]
pub struct Written {
    /// The file.
    pub bytes: Vec<u8>,
    /// Whether the file claims PDF/UA-1.
    pub pdf_ua: bool,
    /// Problems that did not stop the writer: content it left out, and why
    /// a document with a title does not claim PDF/UA-1.
    pub warnings: Vec<Warning>,
}

/// A problem the writer found.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Warning {
    /// The source of the content it concerns, or [`SourceRef::DETACHED`]
    /// for the whole document.
    pub source: SourceRef,
    /// What is wrong.
    pub message: EcoString,
    /// How to fix it, if the writer can tell.
    pub hint: Option<EcoString>,
}

/// An error that stopped the writer, such as a font that krilla cannot
/// subset.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Error {
    /// The source of the content it concerns, or [`SourceRef::DETACHED`].
    pub source: SourceRef,
    /// What went wrong.
    pub message: EcoString,
}

/// What the writer finds in a document before it writes it: whether the
/// file will claim PDF/UA-1, and the warnings writing it gives.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Preflight {
    /// Whether the file is expected to claim PDF/UA-1: the document has a
    /// title and the check found nothing that PDF/UA-1 does not allow.
    pub pdf_ua: bool,
    /// The problems writing the document gives: content the writer leaves
    /// out, and why a document with a title does not claim PDF/UA-1.
    pub warnings: Vec<Warning>,
}

/// Checks a document as [`write()`] would write it, without writing it:
/// the warnings that writing gives, and whether the file can claim
/// PDF/UA-1.
///
/// The check walks the pages as the writer draws them, matching
/// fragments to the structure, loading fonts, and reading every glyph
/// the PDF will show, and finds what PDF/UA-1 does not allow there: a
/// heading with content but no text, a glyph that is the font's
/// `.notdef`, and a glyph whose text, where it is first shown, is no
/// character or one that PDF/UA-1 does not allow. It costs a small part
/// of writing, as it neither encodes nor subsets anything.
///
/// These are the reasons the content Folio writes today can fail PDF/UA-1.
/// A document the check expects to fail is written once, without the
/// validator, so a reason only the validator knows is not reported with
/// them; content that brings new reasons, such as figures without alt
/// text, must teach the check to find them. A reason the check misses in
/// a document it expects to pass is still found, by writing it under the
/// validator, and costs a second writing.
pub fn preflight(document: &Document<'_>) -> Preflight {
    preflight_with(document, &mut || {})
}

/// Checks a document as [`preflight()`] does, calling `check` before each
/// page it walks.
///
/// `check` may unwind, as a database query that its revision has
/// cancelled does (SPEC §2.3), to stop checking a document that an edit
/// has made stale.
pub fn preflight_with(document: &Document<'_>, check: &mut dyn FnMut()) -> Preflight {
    // The tagger needs the ids of marked content, which only a surface
    // gives: they come from one page of a document that is never written.
    let settings = SerializeSettings {
        enable_tagging: true,
        compress_content_streams: false,
        ..SerializeSettings::default()
    };
    let mut scratch = krilla::Document::new_with(settings);
    let mut writer = Writer::new(document, true);
    {
        let size = Size::from_wh(1.0, 1.0).expect("a page of one point is valid");
        let mut kpage = scratch.start_page_with(PageSettings::new(size));
        let mut surface = kpage.surface();
        for (index, page) in document.pages.iter().enumerate() {
            check();
            writer.tagger.start_page(index);
            writer.page(&mut surface, page);
        }
        surface.finish();
        kpage.finish();
    }
    let Writer {
        tagger,
        mut warnings,
        names,
        dry,
        ..
    } = writer;
    if document.info.title.is_none() {
        return Preflight {
            pdf_ua: false,
            warnings,
        };
    }
    let (_, _, untitled) = tagger.finish(document.info.lang.as_deref());
    let dry = dry.expect("the check walks dry");
    let mut errors = dry.notdef;
    errors.extend(dry.unmapped);
    if !untitled.is_empty() {
        errors.push(ValidationError::MissingHeadingTitle);
    }
    let why = reasons(&errors, &untitled, &names);
    let pdf_ua = why.is_empty();
    warnings.extend(why);
    Preflight { pdf_ua, warnings }
}

/// Writes a document as a tagged PDF 1.7, conforming to PDF/UA-1 where
/// the document allows.
pub fn write(document: &Document<'_>) -> Result<Written, Error> {
    write_checked(document, &preflight(document))
}

/// Writes a document that [`preflight()`] has checked, with what the check
/// found: a document that the check expects to claim PDF/UA-1 is written
/// under the validator, and any other once, without the claim. `checked`
/// must be the check of this document.
///
/// The warnings of the result are those of `checked`, in their order,
/// followed by any that only writing found: the reasons not to claim
/// PDF/UA-1 that the check did not foresee, each once.
pub fn write_checked(document: &Document<'_>, checked: &Preflight) -> Result<Written, Error> {
    write_checked_with(document, checked, &mut || {})
}

/// Writes a document that [`preflight()`] has checked, as
/// [`write_checked`] does, calling `check` before each page it writes and
/// before it finishes the file.
///
/// `check` may unwind, as a database query that its revision has
/// cancelled does (SPEC §2.3), to stop writing a document that an edit has
/// made stale.
pub fn write_checked_with(
    document: &Document<'_>,
    checked: &Preflight,
    check: &mut dyn FnMut(),
) -> Result<Written, Error> {
    let mut warnings = checked.warnings.clone();
    // PDF/UA requires a title, which documents need not have: one without
    // is written tagged, without the claim and without a warning.
    if !checked.pdf_ua || document.info.title.is_none() {
        let rendered = render(document, false, check);
        let bytes = rendered.result.map_err(|e| failure(e, &rendered.names))?;
        return Ok(Written {
            bytes,
            pdf_ua: false,
            warnings,
        });
    }
    let first = render(document, true, check);
    let (bytes, pdf_ua) = match first.result {
        Ok(bytes) => (bytes, true),
        Err(KrillaError::Validation(errors)) => {
            // A failure the check did not foresee: the document is written
            // again, without the claim.
            let errors: Vec<_> = errors.into_iter().map(|(error, _)| error).collect();
            let why = reasons(&errors, &first.untitled, &first.names);
            for warning in why {
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
            let second = render(document, false, check);
            let bytes = second.result.map_err(|e| failure(e, &second.names))?;
            (bytes, false)
        }
        Err(other) => return Err(failure(other, &first.names)),
    };
    Ok(Written {
        bytes,
        pdf_ua,
        warnings,
    })
}

/// Why a document is not PDF/UA-1, from the validator's errors, each once.
/// `untitled` holds the sources of the headings without text, which the
/// validator does not name.
fn reasons(
    errors: &[ValidationError],
    untitled: &[SourceRef],
    names: &[(krilla::text::Font, EcoString)],
) -> Vec<Warning> {
    let mut why = Vec::new();
    for error in errors {
        let found = match error {
            // krilla does not say which: the tagger does.
            ValidationError::MissingHeadingTitle if !untitled.is_empty() => untitled
                .iter()
                .map(|&source| untitled_heading(source))
                .collect(),
            _ => vec![not_ua(error, names)],
        };
        for warning in found {
            if !why.contains(&warning) {
                why.push(warning);
            }
        }
    }
    why
}

/// What writing the document once gave.
struct Rendered {
    result: Result<Vec<u8>, KrillaError>,
    /// The PostScript names of the fonts krilla was given, for messages.
    names: Vec<(krilla::text::Font, EcoString)>,
    /// The sources of headings without text.
    untitled: Vec<SourceRef>,
}

/// Writes the document, under the PDF/UA-1 validator or none, calling
/// `check` before each page and before finishing the file.
fn render(document: &Document<'_>, ua: bool, check: &mut dyn FnMut()) -> Rendered {
    render_checked(document, ua, &mut |_, _| {}, check)
}

/// The headings of a document, for its outline: level, text, page and
/// top.
type Headings = [tags::Heading];

/// Writes the document, showing `inspect` the tag tree and the headings
/// before they are written.
#[cfg(test)]
fn render_with(
    document: &Document<'_>,
    ua: bool,
    inspect: &mut dyn FnMut(&krilla::tagging::TagTree, &Headings),
) -> Rendered {
    render_checked(document, ua, inspect, &mut || {})
}

/// Writes the document as [`render_with`] does, calling `check` before
/// each page and before finishing the file.
fn render_checked(
    document: &Document<'_>,
    ua: bool,
    inspect: &mut dyn FnMut(&krilla::tagging::TagTree, &Headings),
    check: &mut dyn FnMut(),
) -> Rendered {
    #[cfg(test)]
    RENDERS.with(|renders| renders.set(renders.get() + 1));
    let mut builder = ConfigurationBuilder::new().with_version(PdfVersion::Pdf17);
    if ua {
        builder = builder.set_validator(Validator::Ua(Accessibility::UA1));
    }
    let configuration = builder
        .finish()
        .expect("PDF/UA-1 is a valid configuration of PDF 1.7");
    let settings = SerializeSettings {
        configuration,
        enable_tagging: true,
        // Unit tests read the content streams.
        compress_content_streams: !cfg!(test),
        ..SerializeSettings::default()
    };
    let mut pdf = krilla::Document::new_with(settings);
    let mut writer = Writer::new(document, false);
    let labels = page_labels(document);
    for (index, page) in document.pages.iter().enumerate() {
        check();
        let size = Size::from_wh(pt(page.size.width), pt(page.size.height))
            .or_else(|| Size::from_wh(1.0, 1.0))
            .expect("a page of one point is valid");
        let mut settings = PageSettings::new(size);
        if let Some(labels) = &labels {
            settings = settings.with_page_label(labels[index].to_krilla());
        }
        let mut kpage = pdf.start_page_with(settings);
        let mut surface = kpage.surface();
        writer.tagger.start_page(index);
        writer.page(&mut surface, page);
        surface.finish();
        // An annotation for each line of a link, as viewers that ignore
        // QuadPoints would make one over several lines active over the
        // whole rectangle around them.
        for (occurrence, areas) in writer.links.areas.drain(..) {
            let link = &document.navigation.links[writer.links.occurrences[occurrence]];
            for area in areas {
                let Some(annotation) = link_annotation(link, &writer.links.places, area) else {
                    continue;
                };
                let id = kpage.add_tagged_annotation(annotation);
                writer.tagger.annotation(occurrence, id);
            }
        }
        kpage.finish();
    }
    check();
    for destination in &document.navigation.destinations {
        if let Some(place) = writer.links.places.get(&destination.name) {
            // The names are the labels, which are unique.
            let named = NamedDestination::new(destination.name.to_string(), place.clone());
            let _ = pdf.register_named_destination(named);
        }
    }
    // The warnings are the check's, which walked the same pages.
    let Writer { tagger, names, .. } = writer;
    let (tree, headings, untitled) = tagger.finish(document.info.lang.as_deref());
    inspect(&tree, &headings);
    pdf.set_tag_tree(tree);
    let outline = tags::outline(headings);
    pdf.set_outline(outline);
    pdf.set_metadata(metadata(&document.info));
    Rendered {
        result: pdf.finish(),
        names,
        untitled,
    }
}

/// The label of each page, or `None` when every page is labelled with its
/// plain number, which viewers show without labels.
fn page_labels(document: &Document<'_>) -> Option<Vec<PageLabel>> {
    let given = &document.navigation.page_labels;
    let labels: Vec<PageLabel> = (0..document.pages.len())
        .map(|i| {
            given
                .get(i)
                .cloned()
                .unwrap_or_else(|| PageLabel::arabic(page_number(i)))
        })
        .collect();
    let plain = labels
        .iter()
        .enumerate()
        .all(|(i, label)| *label == PageLabel::arabic(page_number(i)));
    (!plain).then_some(labels)
}

/// The plain number of the page at `index`.
fn page_number(index: usize) -> u32 {
    u32::try_from(index).map_or(u32::MAX, |i| i.saturating_add(1))
}

/// The annotation of a link over an area of a page, or `None` when it
/// goes nowhere.
fn link_annotation(
    link: &Link,
    places: &HashMap<EcoString, XyzDestination>,
    area: Rect,
) -> Option<Annotation> {
    let name = link.destination.as_ref()?;
    let place = places.get(name)?;
    let target = Target::Destination(krilla::destination::Destination::Named(
        NamedDestination::new(name.to_string(), place.clone()),
    ));
    // PDF/UA-1 requires a description: the text with plain spaces, for
    // screen readers, or the label where the text has nothing to read.
    let words: Vec<&str> = link.text.split_whitespace().collect();
    let alt = if words.is_empty() {
        name.to_string()
    } else {
        words.join(" ")
    };
    Some(Annotation::new_link(
        LinkAnnotation::new(area, target),
        Some(alt),
    ))
}

/// The document information and XMP metadata: no dates and no producer
/// version, so that they depend on the document alone.
fn metadata(info: &Info) -> Metadata {
    let mut metadata = Metadata::new().creator("Folio".into());
    if let Some(title) = &info.title {
        metadata = metadata.title(title.to_string());
    }
    if let Some(subject) = &info.subject {
        metadata = metadata.description(subject.to_string());
    }
    if let Some(lang) = &info.lang {
        metadata = metadata.language(lang.to_string());
    }
    metadata
}

#[cfg(test)]
thread_local! {
    /// How many times this thread has written a document, for tests.
    static RENDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Pages being drawn.
struct Writer<'a> {
    fonts: &'a dyn FontTable,
    /// Each font krilla has been given, or `None` when it cannot be used.
    loaded: HashMap<FontId, Option<krilla::text::Font>>,
    /// The PostScript name of each font krilla has been given.
    names: Vec<(krilla::text::Font, EcoString)>,
    tagger: Tagger<'a>,
    warnings: Vec<Warning>,
    /// The source of the last glyph drawn.
    last: Option<(SourceRef, Range<u32>)>,
    /// The links of the document, as the pages draw them.
    links: Links<'a>,
    /// The ascender and descender of each font, in ems, for the areas of
    /// links.
    extents: HashMap<FontId, (f32, f32)>,
    /// For [`preflight()`], which walks the pages without drawing them:
    /// what PDF/UA-1 does not allow in the glyphs walked.
    dry: Option<Dry>,
}

/// The links of a document as the pages draw them: which link each run of
/// glyphs of a link's source is, and the areas of the links on the page
/// being drawn.
struct Links<'a> {
    /// Where each named destination is.
    places: HashMap<EcoString, XyzDestination>,
    /// For each source of links, its links in order, by index into
    /// [`Navigation::links`], and how many runs of its glyphs have been
    /// drawn.
    sources: HashMap<SourceRef, (Vec<usize>, usize)>,
    /// The link of each run of glyphs drawn that goes somewhere, by index
    /// into [`Navigation::links`]; a run's index here names it.
    occurrences: Vec<usize>,
    /// The source of the last glyph drawn, the run it is in if it goes
    /// somewhere, and how many bytes of its link's text the run has still
    /// to show.
    current: Option<(SourceRef, Option<usize>, usize)>,
    /// The areas of the runs on the page being drawn, in the order they
    /// start.
    areas: Vec<(usize, Vec<Rect>)>,
    navigation: &'a Navigation,
}

impl<'a> Links<'a> {
    fn new(document: &Document<'a>) -> Self {
        let navigation = document.navigation;
        let mut sources: HashMap<SourceRef, (Vec<usize>, usize)> = HashMap::new();
        // Glyphs from no node are no link's.
        for (i, link) in navigation.links.iter().enumerate() {
            if !link.source.is_detached() {
                sources.entry(link.source).or_default().0.push(i);
            }
        }
        Self {
            places: places(document),
            sources,
            occurrences: Vec::new(),
            current: None,
            areas: Vec::new(),
            navigation,
        }
    }

    /// The run of links a glyph of `source` that shows `bytes` bytes of
    /// text, drawn next, is in: the run of the glyph before if that has the
    /// same source and its link's text is not all shown yet, else a new
    /// one; `None` if it goes nowhere. So two links of one source that
    /// meet, as references of a loop can at a line break, stay two.
    fn glyph(&mut self, source: SourceRef, bytes: usize) -> Option<usize> {
        if let Some((last, run, left)) = &mut self.current
            && *last == source
            && *left > 0
        {
            *left = left.saturating_sub(bytes);
            return *run;
        }
        let Some((links, drawn)) = self.sources.get_mut(&source) else {
            self.current = None;
            return None;
        };
        let link = links[(*drawn).min(links.len() - 1)];
        *drawn += 1;
        // A link without text runs until a glyph of another source.
        let text = match self.navigation.links[link].text.len() {
            0 => usize::MAX,
            n => n,
        };
        let goes = self.navigation.links[link]
            .destination
            .as_ref()
            .is_some_and(|name| self.places.contains_key(name));
        let run = goes.then(|| {
            self.occurrences.push(link);
            self.occurrences.len() - 1
        });
        self.current = Some((source, run, text.saturating_sub(bytes)));
        run
    }

    /// Adds an area of the page to run `run`.
    fn area(&mut self, run: usize, area: Rect) {
        match self.areas.iter_mut().find(|(r, _)| *r == run) {
            Some((_, areas)) => areas.push(area),
            None => self.areas.push((run, vec![area])),
        }
    }
}

/// Where each named destination of a document is: the top left corner of
/// the first group or object of its element instance that the pages draw,
/// or else of the first glyph run of its source.
fn places(document: &Document<'_>) -> HashMap<EcoString, XyzDestination> {
    let destinations = &document.navigation.destinations;
    let mut places = HashMap::new();
    if destinations.is_empty() {
        return places;
    }
    let mut by_source: HashMap<SourceRef, Vec<usize>> = HashMap::new();
    // A destination from no node would be at the first box from none.
    for (i, destination) in destinations.iter().enumerate() {
        if !destination.location.source.is_detached() {
            by_source
                .entry(destination.location.source)
                .or_default()
                .push(i);
        }
    }
    let mut boxes: Vec<Option<(usize, Point)>> = vec![None; destinations.len()];
    let mut runs: Vec<Option<(usize, Point)>> = vec![None; destinations.len()];
    fn walk(fragment: &Fragment, pos: Point, find: &mut dyn FnMut(Location, bool, Point)) {
        for (offset, item) in fragment.items.iter() {
            let at = pos + *offset;
            match item {
                FragmentItem::Group(group) => {
                    find(group.location(), true, at);
                    walk(group, at, find);
                }
                FragmentItem::Object(object) => find(object.location, true, at),
                FragmentItem::Text(run) => {
                    if let Some(glyph) = run.glyphs.first() {
                        let location = Location {
                            source: glyph.source.0,
                            instance: Location::UNKNOWN_INSTANCE,
                        };
                        let top = Point::new(at.x, at.y - run.size);
                        find(location, false, top);
                    }
                }
                FragmentItem::Shape { .. } => {}
            }
        }
    }
    for (index, page) in document.pages.iter().enumerate() {
        let mut find = |location: Location, boxed: bool, at: Point| {
            let Some(candidates) = by_source.get(&location.source) else {
                return;
            };
            for &i in candidates {
                let wanted = destinations[i].location.instance;
                let matches = wanted == location.instance
                    || wanted == Location::UNKNOWN_INSTANCE
                    || location.instance == Location::UNKNOWN_INSTANCE;
                let slot = if boxed { &mut boxes[i] } else { &mut runs[i] };
                if matches && slot.is_none() {
                    *slot = Some((index, at));
                }
            }
        };
        walk(page, Point::zero(), &mut find);
    }
    for (i, destination) in destinations.iter().enumerate() {
        if let Some((page, at)) = boxes[i].or(runs[i]) {
            let point = krilla::geom::Point::from_xy(pt(at.x), pt(at.y));
            places
                .entry(destination.name.clone())
                .or_insert_with(|| XyzDestination::new(page, point));
        }
    }
    places
}

/// What a walk without drawing finds in the glyphs that PDF/UA-1 does not
/// allow, as the validator would report it.
#[derive(Default)]
struct Dry {
    /// Whether to check the glyphs: only for a document with a title,
    /// which alone can claim PDF/UA-1.
    glyphs: bool,
    /// Glyphs that are their font's `.notdef`, in the order drawn.
    notdef: Vec<ValidationError>,
    /// Glyphs whose text, where each is first shown, is not a valid
    /// mapping to Unicode, in that order.
    unmapped: Vec<ValidationError>,
    /// The glyphs shown so far, a set of glyph ids for each font as krilla
    /// knows it, which keeps one ToUnicode map for each: the first showing
    /// of a glyph gives its mapping. A document uses few fonts, so they
    /// are searched in order.
    mapped: Vec<(krilla::text::Font, Vec<u64>)>,
    /// The errors found, to report each once.
    seen: HashSet<ValidationError>,
}

impl Dry {
    /// Notes the glyphs of a run set in `font`, as the validator checks
    /// them: in the content stream, whether each is the `.notdef` glyph;
    /// in the ToUnicode map, which holds the text of a glyph's first
    /// showing, whether that is a character PDF/UA allows.
    fn run(&mut self, run: &GlyphRun, font: &krilla::text::Font, last_resort: bool) {
        let index = match self.mapped.iter().position(|(f, _)| f == font) {
            Some(index) => index,
            None => {
                let none = vec![0; usize::from(u16::MAX) / 64 + 1];
                self.mapped.push((font.clone(), none));
                self.mapped.len() - 1
            }
        };
        for g in run.glyphs.iter() {
            let text = run.text.get(usize_range(&g.range)).unwrap_or_default();
            let location = location(g.source.0);
            if g.id == 0 || last_resort {
                let error =
                    ValidationError::ContainsNotDefGlyph(font.clone(), location, text.into());
                if self.seen.insert(error.clone()) {
                    self.notdef.push(error);
                }
            }
            // The ToUnicode map has no entry for the `.notdef` glyph,
            // which is reported above.
            if g.id == 0 {
                continue;
            }
            let bits = &mut self.mapped[index].1;
            let (word, bit) = (usize::from(g.id) / 64, 1u64 << (g.id % 64));
            if bits[word] & bit != 0 {
                continue;
            }
            bits[word] |= bit;
            let gid = GlyphId::new(u32::from(g.id));
            // The validator names the last such character.
            let invalid = text
                .chars()
                .rev()
                .find(|&c| matches!(c as u32, 0x0 | 0xFEFF | 0xFFFE));
            let error = match invalid {
                Some(c) => ValidationError::InvalidCodepointMapping(font.clone(), gid, c, location),
                None if text.is_empty() => {
                    ValidationError::NoCodepointMapping(font.clone(), gid, location)
                }
                None => continue,
            };
            if self.seen.insert(error.clone()) {
                self.unmapped.push(error);
            }
        }
    }
}

impl<'a> Writer<'a> {
    /// A writer for a document, which draws, or for [`preflight()`] walks
    /// without drawing (`dry`).
    fn new(document: &Document<'a>, dry: bool) -> Self {
        Self {
            fonts: document.fonts,
            loaded: HashMap::new(),
            names: Vec::new(),
            tagger: Tagger::new(document.structure),
            warnings: Vec::new(),
            last: None,
            links: Links::new(document),
            extents: HashMap::new(),
            dry: dry.then(|| Dry {
                glyphs: document.info.title.is_some(),
                ..Dry::default()
            }),
        }
    }
}

/// The thickness of the frame drawn for an object whose content is not
/// laid out yet, inside its box.
const OBJECT_FRAME: Abs = Abs::pt(0.5);

impl Writer<'_> {
    /// Draws a page: its regions' content as boxes of the top of the
    /// structure.
    fn page(&mut self, surface: &mut Surface<'_>, page: &Fragment) {
        for (offset, item) in page.items.iter() {
            match item {
                FragmentItem::Group(region) => {
                    self.fragment(surface, region, *offset, Tagger::TOP);
                }
                other => self.item(surface, other, *offset, Tagger::TOP),
            }
        }
    }

    /// Draws a fragment's content with its top-left corner at `pos`, as
    /// content of the structure node `node`.
    fn fragment(
        &mut self,
        surface: &mut Surface<'_>,
        fragment: &Fragment,
        pos: Point,
        node: usize,
    ) {
        for (offset, item) in fragment.items.iter() {
            self.item(surface, item, pos + *offset, node);
        }
    }

    /// Draws an item at `at` within a box of `node`.
    fn item(&mut self, surface: &mut Surface<'_>, item: &FragmentItem, at: Point, node: usize) {
        match item {
            FragmentItem::Group(group) => {
                let child = self.tagger.enter(node, group.location(), at.y);
                self.fragment(surface, group, at, child);
                self.tagger.advance();
            }
            FragmentItem::Text(run) => self.text(surface, run, at, node),
            FragmentItem::Shape { .. } if self.dry.is_some() => {}
            FragmentItem::Shape { shape, .. } => {
                // Shapes are the backgrounds and borders of boxes.
                surface.start_tagged(ContentTag::Artifact(krilla::tagging::Artifact::with_kind(
                    ArtifactType::Layout,
                )));
                draw_shape(surface, shape, at);
                surface.end_tagged();
            }
            FragmentItem::Object(_) if self.dry.is_some() => {}
            FragmentItem::Object(object) => {
                // No stage lays out the content of an object yet: its box
                // is drawn as an empty frame, which is not content.
                let half = OBJECT_FRAME / 2.0;
                let inner = |side: Abs| (side - OBJECT_FRAME).max(Abs::zero());
                let path = folio_component::Path::rect(
                    inner(object.size.width),
                    inner(object.size.height),
                );
                let frame = Shape::stroked(
                    path,
                    folio_component::Stroke::solid(folio_component::Color::BLACK, OBJECT_FRAME),
                );
                surface.start_tagged(ContentTag::Artifact(krilla::tagging::Artifact::with_kind(
                    ArtifactType::Layout,
                )));
                draw_shape(surface, &frame, at + Point::new(half, half));
                surface.end_tagged();
            }
        }
    }

    /// Draws a glyph run with its baseline starting at `at`, as content of
    /// the structure node `node`.
    fn text(&mut self, surface: &mut Surface<'_>, run: &GlyphRun, at: Point, node: usize) {
        self.tagger.advance();
        // Two hyphens at line ends are not text: the one a break inserts,
        // which names an empty range of its source, and the one a line
        // repeats (Polish, after a break at an explicit hyphen), which shows
        // the same bytes of its source as the glyph drawn just before it.
        // That is told only where the source shows exactly the hyphen's
        // bytes, as text written as it is does; a hyphen from a node that
        // maps as a whole is left as text. Their glyphs keep the text `-`,
        // so that the font's ToUnicode map stays one character per glyph,
        // and the span's actual text says what they are: U+00AD, as ISO
        // 32000 asks of hyphens that layout adds, and nothing. So a
        // hyphenated word extracts whole.
        let hyphen = !run.glyphs.is_empty() && matches!(run.text.as_str(), "-" | "\u{2010}");
        let inserted = hyphen && run.glyphs.iter().all(|g| g.source.1.is_empty());
        let repeated = hyphen
            && !inserted
            && self.last.as_ref().is_some_and(|last| {
                run.glyphs
                    .iter()
                    .all(|g| &g.source == last && g.source.1.len() == run.text.len())
            });
        self.last = run.glyphs.last().map(|g| g.source.clone());
        // The run is drawn in pieces, each of glyphs of one link or of
        // none, as the text of a link is content of its `Link` element.
        let mut pieces: Vec<(Range<usize>, Option<usize>)> = Vec::new();
        for (i, g) in run.glyphs.iter().enumerate() {
            let link = self.links.glyph(g.source.0, g.range.len());
            match pieces.last_mut() {
                Some((range, last)) if *last == link => range.end = i + 1,
                _ => pieces.push((i..i + 1, link)),
            }
        }
        let Some(font) = self.font(run) else {
            return;
        };
        if let Some(dry) = &mut self.dry {
            // Only a document with a title can claim PDF/UA-1.
            if dry.glyphs {
                let last_resort = name(&font, &self.names) == "LastResort";
                dry.run(run, &font, last_resort);
            }
            for (_, link) in pieces {
                let id = surface.start_tagged(ContentTag::Span(SpanTag::empty()));
                surface.end_tagged();
                self.tagger.content(node, id, link);
            }
            return;
        }
        let size = pt(run.size);
        let actual = if inserted {
            Some("\u{AD}")
        } else if repeated {
            Some("")
        } else {
            None
        };
        let Paint::Solid(color) = &run.fill;
        surface.set_fill(Some(Fill {
            paint: rgb::Color::new(color.r, color.g, color.b).into(),
            opacity: NormalizedF32::new(f32::from(color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
            rule: paint::FillRule::NonZero,
        }));
        surface.set_stroke(None);
        let (ascender, descender) = self.extents.get(&run.font).copied().unwrap_or((0.8, -0.2));
        let mut x = pt(at.x);
        for (range, link) in pieces {
            let glyphs: Vec<KrillaGlyph> = run.glyphs[range]
                .iter()
                .map(|g| {
                    KrillaGlyph::new(
                        GlyphId::new(u32::from(g.id)),
                        g.x_advance.get() as f32,
                        g.x_offset.get() as f32,
                        g.y_offset.get() as f32,
                        0.0,
                        usize_range(&g.range),
                        location(g.source.0),
                    )
                })
                .collect();
            let width: f32 = glyphs.iter().map(|g| g.x_advance * size).sum();
            let span = SpanTag::empty().with_actual_text(actual);
            let id = surface.start_tagged(ContentTag::Span(span));
            surface.draw_glyphs(
                krilla::geom::Point::from_xy(x, pt(at.y)),
                &glyphs,
                font.clone(),
                &run.text,
                size,
                false,
            );
            surface.end_tagged();
            self.tagger.content(node, id, link);
            if let Some(link) = link {
                let top = pt(at.y) - ascender * size;
                let height = (ascender - descender) * size;
                if let Some(area) = Rect::from_xywh(x, top, width.max(0.0), height) {
                    self.links.area(link, area);
                }
            }
            x += width;
        }
    }

    /// The font of a glyph run as krilla knows it, loaded once.
    fn font(&mut self, run: &GlyphRun) -> Option<krilla::text::Font> {
        let fonts = self.fonts;
        let warnings = &mut self.warnings;
        let names = &mut self.names;
        let extents = &mut self.extents;
        let source = run
            .glyphs
            .first()
            .map_or(SourceRef::DETACHED, |g| g.source.0);
        self.loaded
            .entry(run.font)
            .or_insert_with(|| {
                let Some(font) = fonts.font(run.font) else {
                    warnings.push(Warning {
                        source,
                        message: "this text's font cannot be loaded, so the PDF leaves it out"
                            .into(),
                        hint: None,
                    });
                    return None;
                };
                let metrics = font.metrics();
                let em = f32::from(metrics.units_per_em.max(1));
                extents.insert(
                    run.font,
                    (
                        f32::from(metrics.ascender) / em,
                        f32::from(metrics.descender) / em,
                    ),
                );
                // The font stage reports fonts that forbid embedding.
                if !font.info().embedding.allows_embedding() {
                    return None;
                }
                let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(font.data().clone());
                let loaded = krilla::text::Font::new(Data::from(data), font.index());
                match &loaded {
                    Some(loaded) => {
                        names.push((loaded.clone(), font.info().postscript_name.clone()))
                    }
                    None => warnings.push(Warning {
                        source,
                        message: eco(format!(
                            "the PDF writer cannot read the font `{}`, so it leaves out the \
                             text set in it",
                            font.info().postscript_name
                        )),
                        hint: None,
                    }),
                }
                loaded
            })
            .clone()
    }
}

/// Draws a shape whose path has its origin at `at`.
fn draw_shape(surface: &mut Surface<'_>, shape: &Shape, at: Point) {
    let mut builder = PathBuilder::new();
    for item in shape.path.0.iter() {
        match *item {
            PathItem::MoveTo(p) => builder.move_to(pt(p.x), pt(p.y)),
            PathItem::LineTo(p) => builder.line_to(pt(p.x), pt(p.y)),
            PathItem::CubicTo(a, b, p) => {
                builder.cubic_to(pt(a.x), pt(a.y), pt(b.x), pt(b.y), pt(p.x), pt(p.y));
            }
            PathItem::ClosePath => builder.close(),
        }
    }
    let Some(path) = builder.finish() else {
        return;
    };
    surface.push_transform(&Transform::from_translate(pt(at.x), pt(at.y)));
    surface.set_fill(shape.fill.as_ref().map(|fill| {
        let (paint, opacity) = paint(fill);
        Fill {
            paint,
            opacity,
            rule: match shape.fill_rule {
                FillRule::NonZero => paint::FillRule::NonZero,
                FillRule::EvenOdd => paint::FillRule::EvenOdd,
            },
        }
    }));
    surface.set_stroke(shape.stroke.as_ref().map(|stroke| {
        let (paint, opacity) = paint(&stroke.paint);
        Stroke {
            paint,
            width: pt(stroke.thickness),
            miter_limit: stroke.miter_limit.get() as f32,
            line_cap: match stroke.cap {
                LineCap::Butt => paint::LineCap::Butt,
                LineCap::Round => paint::LineCap::Round,
                LineCap::Square => paint::LineCap::Square,
            },
            line_join: match stroke.join {
                LineJoin::Miter => paint::LineJoin::Miter,
                LineJoin::Round => paint::LineJoin::Round,
                LineJoin::Bevel => paint::LineJoin::Bevel,
            },
            opacity,
            dash: stroke.dash.as_ref().map(|dash| StrokeDash {
                array: dash.array.iter().map(|&l| pt(l)).collect(),
                offset: pt(dash.phase),
            }),
        }
    }));
    surface.draw_path(&path);
    surface.pop();
}

/// A paint as krilla's, with its opacity.
fn paint(paint: &Paint) -> (paint::Paint, NormalizedF32) {
    let Paint::Solid(color) = paint;
    (
        rgb::Color::new(color.r, color.g, color.b).into(),
        NormalizedF32::new(f32::from(color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
    )
}

/// A length in points, as krilla takes it.
fn pt(length: Abs) -> f32 {
    let value = length.to_pt() as f32;
    if value.is_finite() { value } else { 0.0 }
}

fn usize_range(range: &Range<u32>) -> Range<usize> {
    range.start as usize..range.end as usize
}

/// The location krilla reports problems at: a source node.
fn location(source: SourceRef) -> Option<NonZeroU64> {
    NonZeroU64::new(source.0)
}

/// The source a location stands for.
fn source(location: Option<NonZeroU64>) -> SourceRef {
    location.map_or(SourceRef::DETACHED, |l| SourceRef(l.get()))
}

fn eco(text: String) -> EcoString {
    text.into()
}

/// The PostScript name of a font krilla was given.
fn name(font: &krilla::text::Font, names: &[(krilla::text::Font, EcoString)]) -> EcoString {
    names
        .iter()
        .find(|(f, _)| f == font)
        .map_or_else(|| "?".into(), |(_, name)| name.clone())
}

/// The warning for a heading without text, which PDF/UA-1 does not allow.
fn untitled_heading(source: SourceRef) -> Warning {
    Warning {
        source,
        message: "the PDF does not claim PDF/UA-1, because this heading has no text".into(),
        hint: Some("write the heading's text, or remove the heading".into()),
    }
}

/// Why a document is not PDF/UA-1, as a warning.
fn not_ua(error: &ValidationError, names: &[(krilla::text::Font, EcoString)]) -> Warning {
    let warning = |source: SourceRef, what: &str, hint: Option<&str>| Warning {
        source,
        message: eco(format!("the PDF does not claim PDF/UA-1, because {what}")),
        hint: hint.map(EcoString::from),
    };
    match error {
        ValidationError::NoDocumentTitle => warning(
            SourceRef::DETACHED,
            "the document has no title",
            Some("give it one with `#doc(title: [..])`"),
        ),
        ValidationError::MissingHeadingTitle => warning(
            SourceRef::DETACHED,
            "a heading has no text",
            Some("write the heading's text, or remove the heading"),
        ),
        ValidationError::ContainsNotDefGlyph(_, location, _) => warning(
            source(*location),
            "this text has a character its font has no glyph for",
            None,
        ),
        ValidationError::NoCodepointMapping(_, _, location) => warning(
            source(*location),
            "a glyph of this text stands for no character",
            None,
        ),
        ValidationError::InvalidCodepointMapping(_, _, c, location) => warning(
            source(*location),
            &format!(
                "this text has the character U+{:04X}, which PDF/UA does not allow",
                *c as u32
            ),
            Some("remove the character"),
        ),
        ValidationError::RestrictedLicense(font) => warning(
            SourceRef::DETACHED,
            &format!(
                "the licence of the font `{}` does not allow embedding it",
                name(font, names)
            ),
            None,
        ),
        other => warning(SourceRef::DETACHED, &format!("of {other:?}"), None),
    }
}

/// A krilla error that stopped the writer.
fn failure(error: KrillaError, names: &[(krilla::text::Font, EcoString)]) -> Error {
    match error {
        KrillaError::Font(font, message) => Error {
            source: SourceRef::DETACHED,
            message: eco(format!(
                "the PDF writer cannot embed the font `{}`: {message}",
                name(&font, names)
            )),
        },
        other => Error {
            source: SourceRef::DETACHED,
            message: eco(format!("the PDF writer failed: {other:?}")),
        },
    }
}

#[cfg(test)]
mod tests;

/// Tests of the check before writing: that it foresees what the
/// validator finds, so that a document is written once.
#[cfg(test)]
mod preflight_tests {
    use folio_component::{Color, Em, Glyph, Paint, Size};
    use folio_fonts::bundled;

    use super::*;

    /// Every id is the first bundled face.
    struct Fonts;

    impl FontTable for Fonts {
        fn font(&self, _: FontId) -> Option<Font> {
            bundled()[0].faces().ok()?.first().cloned()
        }
    }

    /// A glyph run from node `source`: each glyph an id and the bytes of
    /// `text` it shows.
    fn run(text: &str, source: u64, glyphs: &[(u16, Range<u32>)]) -> FragmentItem {
        FragmentItem::Text(GlyphRun {
            font: FontId(0),
            size: Abs::pt(10.0),
            fill: Paint::Solid(Color::BLACK),
            text: text.into(),
            glyphs: glyphs
                .iter()
                .map(|(id, range)| Glyph {
                    id: *id,
                    x_advance: Em::new(0.5),
                    x_offset: Em::new(0.0),
                    y_offset: Em::new(0.0),
                    range: range.clone(),
                    source: (SourceRef(source), range.clone()),
                })
                .collect(),
        })
    }

    /// A glyph per character, its id the character's.
    fn plain(text: &str, source: u64) -> FragmentItem {
        let glyphs: Vec<_> = text
            .char_indices()
            .map(|(i, c)| (c as u16, i as u32..(i + c.len_utf8()) as u32))
            .collect();
        run(text, source, &glyphs)
    }

    fn boxed(source: u64, items: Vec<FragmentItem>) -> FragmentItem {
        let mut fragment =
            Fragment::new(Size::new(Abs::pt(300.0), Abs::pt(100.0))).with_source(SourceRef(source));
        for (i, item) in items.into_iter().enumerate() {
            fragment.push(
                Point::new(Abs::zero(), Abs::pt(12.0 * i as f64 + 10.0)),
                item,
            );
        }
        FragmentItem::Group(fragment)
    }

    fn page(items: Vec<FragmentItem>) -> Fragment {
        let mut page =
            Fragment::new(Size::new(Abs::pt(400.0), Abs::pt(600.0))).with_source(SourceRef(1));
        page.push(Point::new(Abs::pt(50.0), Abs::pt(50.0)), boxed(1, items));
        page
    }

    /// Checks a document, a section with heading `heading` (node 2,
    /// without text when `None`) and a paragraph (node 3) holding `body`:
    /// that the check foresees whether it claims PDF/UA-1 (`ua`) and the
    /// warnings, in the validator's order, and that the document is then
    /// written once.
    fn check(ua: bool, heading: Option<&str>, body: FragmentItem) {
        let mut head = Tag::new(Role::Heading, SourceRef(2));
        head.text = heading.map(Into::into);
        let mut section = Tag::new(Role::Section, SourceRef(2));
        section.children = vec![head, Tag::new(Role::Paragraph, SourceRef(3))];
        let mut root = Tag::new(Role::Document, SourceRef(1));
        root.children = vec![section];
        let section = boxed(
            2,
            vec![
                boxed(2, vec![plain("\u{a0}Forces", 2)]),
                boxed(3, vec![body]),
            ],
        );
        let pages = [page(vec![boxed(1, vec![section])])];
        let document = Document {
            pages: pages.iter().collect(),
            fonts: &Fonts,
            structure: &root,
            info: Info {
                title: Some("Forces".into()),
                subject: None,
                lang: Some("en".into()),
            },
            navigation: &Navigation::default(),
        };
        let checked = preflight(&document);
        // What the validator finds, as the writer found it before the
        // check: written under the validator, whatever the check found.
        let validated = write_checked(
            &document,
            &Preflight {
                pdf_ua: true,
                warnings: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(validated.pdf_ua, ua, "{:?}", validated.warnings);
        assert_eq!(checked.pdf_ua, ua, "{:?}", checked.warnings);
        assert_eq!(checked.warnings, validated.warnings);
        // Written after the check, the document is written once.
        RENDERS.with(|r| r.set(0));
        let written = write_checked(&document, &checked).unwrap();
        assert_eq!(RENDERS.with(|r| r.get()), 1);
        assert_eq!(written.pdf_ua, checked.pdf_ua);
        assert_eq!(written.warnings, checked.warnings);
        assert_eq!(written.bytes, validated.bytes);
    }

    #[test]
    fn foresees_a_document_that_claims_pdf_ua_1() {
        check(true, Some("Forces"), plain("A ball falls.", 3));
    }

    #[test]
    fn foresees_a_heading_without_text() {
        check(false, None, plain("A ball falls.", 3));
    }

    #[test]
    fn foresees_a_notdef_glyph() {
        check(
            false,
            Some("Forces"),
            run("ab", 3, &[(0, 0..1), (98, 1..2)]),
        );
    }

    /// The `.notdef` glyph is reported once, not again for its text,
    /// which the ToUnicode map does not hold.
    #[test]
    fn foresees_a_notdef_glyph_without_text() {
        let text = "a\u{feff}b";
        let glyphs = [(97, 0..1), (0, 1..4), (0, 4..4), (98, 4..5)];
        check(false, Some("Forces"), run(text, 3, &glyphs));
    }

    /// Reasons of every kind together, in the validator's order.
    #[test]
    fn foresees_several_reasons_in_order() {
        let text = "ab\u{feff}";
        let glyphs = [(0, 0..1), (120, 1..1), (98, 1..2), (300, 2..5)];
        check(false, None, run(text, 3, &glyphs));
    }

    #[test]
    fn foresees_a_glyph_without_text() {
        let glyphs = [(97, 0..1), (120, 1..1), (98, 1..2)];
        check(false, Some("Forces"), run("ab", 3, &glyphs));
    }

    #[test]
    fn foresees_a_character_pdf_ua_does_not_allow() {
        let text = "a\u{feff}b";
        check(
            false,
            Some("Forces"),
            run(text, 3, &[(97, 0..1), (300, 1..4), (98, 4..5)]),
        );
    }

    /// A glyph's mapping is the text it first shows, as in the ToUnicode
    /// map: a later showing with no text is no problem.
    #[test]
    fn maps_a_glyph_by_its_first_showing() {
        let glyphs = [(97, 0..1), (97, 1..1), (97, 1..2)];
        check(true, Some("Forces"), run("aa", 3, &glyphs));
    }
}
