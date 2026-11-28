//! The leak check (SPEC §6.4): content that a variant does not show never
//! reaches its outputs.
//!
//! The check has two lines:
//!
//! - **Provenance.** Every item the PDF writer is given names where it came
//!   from: each box and image the element instance it is a box of, each
//!   glyph its text node and the innermost instance it is in, each node of
//!   the structure (and so each bookmark and alternate description) its
//!   instance, each destination and link its own, and the metadata the
//!   nodes it was read from ([`folio_pdf::Info::provenance`]). An item that
//!   names an instance or a text node the variant does not show is a leak
//!   (`leak`).
//! - **Text.** The written file is read back as a reader sees it
//!   ([`folio_pdf::extract_text`]: page text through the fonts' `ToUnicode`
//!   maps, and every text string and the XMP metadata), and searched for
//!   the text of what the variant does not show: any six words in a row of
//!   it, or any 30 letters and digits, compared without case, spaces or
//!   punctuation (`leak-text`). A sequence that the variant also shows, as
//!   when an answer repeats the question's words, is not a leak.
//!
//! What a variant does not show is what Select leaves out of its document
//! tree or replaces ([`folio_core::variants::select`]), and what the
//! project's other variants show that it does not, such as an answer
//! written in `#if variant == "markscheme" [..]`: the instances and text
//! nodes of their selected trees, of the same version, that its own
//! selected tree lacks. Instances are told apart by location, which is the
//! same in every variant (D-339-1), so of two instances made from one
//! span, one hidden and one shown, only the hidden one counts.
//!
//! Only PDF is checked: the Office targets and live clients have no writer
//! yet, and their checks come with them (SPEC §15).

use std::collections::HashSet;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{FragmentItem, Location, SourceRef, Target};
use folio_core::db::{Build, Db};
use folio_core::diag::Diagnostic;
use folio_core::eval::{Content, Element, Node, Value};
use folio_core::fonts::fonts;
use folio_core::layout::pages;
use folio_core::model::document;
use folio_core::pipeline::selected_root;
use folio_core::span_map::{source_ref, span};
use folio_pdf::{Document, Extracted, Provenance, Tag};
use folio_syntax::Span;

use crate::pipeline::{FontTable, emit, pdf_input};

/// The code of a leak that provenance finds.
pub const LEAK: &str = "leak";

/// The code of hidden text found in an output.
pub const LEAK_TEXT: &str = "leak-text";

/// How many words in a row of hidden text make a leak.
pub const WORDS: usize = 6;

/// How many letters and digits in a row of hidden text make a leak.
pub const CHARS: usize = 30;

/// Checks a build for leaks (SPEC §6.4): its diagnostics, each an error
/// with the code [`LEAK`] or [`LEAK_TEXT`] at the hidden node. A build
/// that could not be styled or written has nothing to check.
#[salsa::tracked(returns(ref))]
pub(crate) fn leaks(db: &dyn Db, build: Build<'_>) -> EcoVec<Diagnostic> {
    match build.target(db) {
        Target::Pdf => pdf_leaks(db, build).unwrap_or_default(),
        Target::Live | Target::Docx | Target::Pptx => EcoVec::new(),
    }
}

fn pdf_leaks(db: &dyn Db, build: Build<'_>) -> Option<EcoVec<Diagnostic>> {
    let hidden = Hidden::of(db, build)?;
    if hidden.is_empty() {
        return Some(EcoVec::new());
    }
    let variant = build.variant(db).clone();
    let mut found = Found::new(variant.as_ref().map(|v| v.as_str()));
    let pages = pages(db, build).as_ref()?;
    let input = pdf_input(db, build).as_ref()?;
    let fonts = FontTable(fonts(db, build.project(db)));
    let document = input.document(pages, &fonts);
    provenance(&hidden, &document, &mut found);
    if let Some(bytes) = emit(db, build)
        && let Some(extracted) = folio_pdf::extract_text(bytes)
    {
        search(&hidden, &extracted, &[], &mut found);
    }
    Some(found.diagnostics)
}

/// What a variant does not show: the element instances and text nodes,
/// and the text they hold.
#[derive(Debug, Default)]
pub(crate) struct Hidden {
    /// The element instances the variant does not show.
    locations: HashSet<Location>,
    /// The text nodes (and other nodes that give text, such as a `raw`
    /// element) of which the variant shows no instance.
    sources: HashSet<SourceRef>,
    /// The text of what the variant does not show, in passages: each
    /// hidden element instance whole, and each run of hidden text in
    /// content shown, with the node it starts at.
    passages: Vec<(Span, EcoString)>,
    /// What the variant shows, normalised ([`normalise`]).
    shown: String,
}

/// What a variant shows: the element instances and the nodes of its
/// selected tree.
#[derive(Default)]
struct Kept {
    locations: HashSet<Location>,
    sources: HashSet<SourceRef>,
}

impl Hidden {
    /// What a build's variant does not show, or `None` when its document
    /// could not be styled.
    pub(crate) fn of(db: &dyn Db, build: Build<'_>) -> Option<Self> {
        let build = build.base(db);
        let shown = selected_root(db, build)?;
        let mut kept = Kept::default();
        let mut text = String::new();
        keep(shown, &mut kept, &mut text);
        let mut hidden = Hidden {
            shown: normalise(&text),
            ..Hidden::default()
        };
        let mut seen = HashSet::new();
        let mut walk = |root: &Element, hidden: &mut Hidden| {
            hidden.element(root, &kept, &mut seen);
        };
        // What Select leaves out of the variant's own tree.
        let project = build.project(db);
        let version = build.version(db).clone();
        if let Some(doc) =
            document(db, project, build.variant(db).clone(), version.clone()).as_ref()
        {
            walk(&doc.root, &mut hidden);
        }
        // What the other variants show.
        let manifest = project.manifest(db);
        for other in &manifest.variants {
            let name = &other.name.v;
            if Some(name) == build.variant(db).as_ref() {
                continue;
            }
            let target = other
                .targets
                .iter()
                .map(|t| t.v)
                .find(|&t| t == build.target(db))
                .or_else(|| other.targets.first().map(|t| t.v))
                .unwrap_or(build.target(db));
            let theirs = Build::versioned(db, project, target, Some(name.clone()), version.clone());
            if let Some(root) = selected_root(db, theirs) {
                walk(root, &mut hidden);
            }
        }
        Some(hidden)
    }

    /// Whether the variant shows everything.
    fn is_empty(&self) -> bool {
        self.locations.is_empty() && self.sources.is_empty() && self.passages.is_empty()
    }

    /// Notes what of an element the variant does not show; `seen` holds the
    /// hidden nodes already noted, which another variant's tree may hold
    /// again.
    fn element(&mut self, element: &Element, kept: &Kept, seen: &mut HashSet<(Location, Span)>) {
        if element.location != Location::DETACHED && !kept.locations.contains(&element.location) {
            if seen.insert((element.location, element.span)) {
                self.locations.insert(element.location);
                let mut text = String::new();
                self.all(element, kept, &mut text);
                if !text.trim().is_empty() {
                    self.passages.push((element.span, text.into()));
                }
            }
            return;
        }
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                self.content(content, kept, seen);
            }
        }
    }

    /// Notes the hidden nodes of content of a shown element: hidden
    /// elements as passages of their own, and runs of hidden text.
    fn content(&mut self, content: &Content, kept: &Kept, seen: &mut HashSet<(Location, Span)>) {
        let mut run: Option<(Span, String)> = None;
        fn flush(
            run: &mut Option<(Span, String)>,
            hidden: &mut Hidden,
            seen: &mut HashSet<(Location, Span)>,
        ) {
            if let Some((at, text)) = run.take()
                && !text.trim().is_empty()
                && seen.insert((Location::DETACHED, at))
            {
                hidden.passages.push((at, text.into()));
            }
        }
        for node in content.nodes() {
            match node {
                Node::Text { text, span } if !kept.sources.contains(&source_ref(*span)) => {
                    self.sources.insert(source_ref(*span));
                    run.get_or_insert_with(|| (*span, String::new()))
                        .1
                        .push_str(text);
                }
                Node::Space(_) | Node::Linebreak(_) | Node::Parbreak(_) => {
                    if let Some((_, text)) = &mut run {
                        text.push(' ');
                    }
                }
                Node::Element(element) => {
                    flush(&mut run, self, seen);
                    self.element(element, kept, seen);
                }
                Node::Slot(slot) => {
                    flush(&mut run, self, seen);
                    self.content(&slot.body, kept, seen);
                }
                _ => flush(&mut run, self, seen),
            }
        }
        flush(&mut run, self, seen);
    }

    /// Notes everything of a hidden element, and writes its text.
    fn all(&mut self, element: &Element, kept: &Kept, text: &mut String) {
        if element.location != Location::DETACHED && !kept.locations.contains(&element.location) {
            self.locations.insert(element.location);
        }
        let own = source_ref(element.span);
        if !kept.sources.contains(&own) {
            self.sources.insert(own);
        }
        shown_fields(element, text);
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                self.all_content(content, kept, text);
            }
        }
    }

    fn all_content(&mut self, content: &Content, kept: &Kept, text: &mut String) {
        for node in content.nodes() {
            match node {
                Node::Text { text: t, span } => {
                    if !kept.sources.contains(&source_ref(*span)) {
                        self.sources.insert(source_ref(*span));
                    }
                    text.push_str(t);
                }
                Node::Space(_) | Node::Linebreak(_) | Node::Parbreak(_) => text.push(' '),
                Node::Element(element) => {
                    text.push(' ');
                    self.all(element, kept, text);
                    text.push(' ');
                }
                Node::Slot(slot) => self.all_content(&slot.body, kept, text),
                _ => {}
            }
        }
    }
}

/// Notes what a variant's selected tree shows, and writes its text.
fn keep(element: &Element, kept: &mut Kept, text: &mut String) {
    kept.locations.insert(element.location);
    kept.sources.insert(source_ref(element.span));
    shown_fields(element, text);
    for (_, value) in element.fields.iter() {
        if let Value::Content(content) = value {
            keep_content(content, kept, text);
        }
    }
}

fn keep_content(content: &Content, kept: &mut Kept, text: &mut String) {
    for node in content.nodes() {
        match node {
            Node::Text { text: t, span } => {
                kept.sources.insert(source_ref(*span));
                text.push_str(t);
            }
            Node::Space(_) | Node::Linebreak(_) | Node::Parbreak(_) => text.push(' '),
            Node::Element(element) => {
                text.push(' ');
                keep(element, kept, text);
                text.push(' ');
            }
            Node::Slot(slot) => keep_content(&slot.body, kept, text),
            _ => {}
        }
    }
}

/// Writes the text of the fields a built-in element shows other than as
/// content: a code block's text, a formula's reading, an image's
/// description.
fn shown_fields(element: &Element, text: &mut String) {
    if element.name.namespace.is_some() {
        return;
    }
    let mut push = |value: Option<&Value>| match value {
        Some(Value::Str(s)) => {
            text.push(' ');
            text.push_str(s);
            text.push(' ');
        }
        Some(Value::Math(formula)) => {
            text.push(' ');
            text.push_str(&formula.reading(&Default::default()));
            text.push(' ');
        }
        _ => {}
    };
    match element.name.name.as_str() {
        "raw" => push(element.field("text")),
        "math" => push(element.field("tree")),
        "image" => push(element.field("alt")),
        _ => {}
    }
}

/// The leaks found, as diagnostics, one for each hidden node.
struct Found {
    variant: EcoString,
    reported: HashSet<(Span, &'static str)>,
    diagnostics: EcoVec<Diagnostic>,
}

impl Found {
    fn new(variant: Option<&str>) -> Self {
        Self {
            variant: match variant {
                Some(name) => eco_format!("variant `{name}`"),
                None => "the document".into(),
            },
            reported: HashSet::new(),
            diagnostics: EcoVec::new(),
        }
    }

    /// Reports that an item of `what` came from the hidden node `at`.
    fn provenance(&mut self, at: Span, what: &'static str) {
        if !self.reported.insert((at, LEAK)) {
            return;
        }
        let message = eco_format!(
            "content that {} does not show reaches its PDF, in {what}",
            self.variant
        );
        self.diagnostics.push(
            Diagnostic::error(at, message)
                .with_code(LEAK)
                .with_hint("this is a bug in Folio or in a component; please report it"),
        );
    }

    /// Reports that the text `words` of the hidden node `at` is in the PDF.
    fn text(&mut self, at: Span, words: &str) {
        if !self.reported.insert((at, LEAK_TEXT)) {
            return;
        }
        let message = eco_format!(
            "text that {} does not show is in its PDF: \u{201c}{words}\u{201d}",
            self.variant
        );
        self.diagnostics
            .push(Diagnostic::error(at, message).with_code(LEAK_TEXT));
    }
}

/// Checks the provenance of everything the PDF writer is given.
fn provenance(hidden: &Hidden, document: &Document<'_>, found: &mut Found) {
    let instance = |location: Location| -> Option<Span> {
        (location != Location::DETACHED && hidden.locations.contains(&location))
            .then(|| span(location.source))
    };
    let node = |source: SourceRef| -> Option<Span> {
        (!source.is_detached() && hidden.sources.contains(&source)).then(|| span(source))
    };
    for page in &document.pages {
        fragment(page, &instance, &node, found);
    }
    tag(document.structure, &instance, found);
    for destination in &document.navigation.destinations {
        if let Some(at) = instance(destination.location) {
            found.provenance(at, "a named destination");
        }
    }
    for link in &document.navigation.links {
        if let Some(at) = node(link.source) {
            found.provenance(at, "a link");
        }
    }
    for Provenance { source, location } in &document.info.provenance {
        if let Some(at) = instance(*location).or_else(|| node(*source)) {
            found.provenance(at, "the document's metadata");
        }
    }
}

fn fragment(
    fragment: &folio_component::Fragment,
    instance: &impl Fn(Location) -> Option<Span>,
    node: &impl Fn(SourceRef) -> Option<Span>,
    found: &mut Found,
) {
    if let Some(at) = instance(fragment.location()) {
        found.provenance(at, "a box on a page");
    }
    for (_, item) in &fragment.items {
        match item {
            FragmentItem::Group(inner) => self::fragment(inner, instance, node, found),
            FragmentItem::Text(run) => {
                for (i, glyph) in run.glyphs.iter().enumerate() {
                    let at = instance(run.instance(i)).or_else(|| node(glyph.source.0));
                    if let Some(at) = at {
                        found.provenance(at, "the text of a page");
                    }
                }
            }
            FragmentItem::Shape { source, .. } => {
                if let Some(at) = node(*source) {
                    found.provenance(at, "a shape on a page");
                }
            }
            FragmentItem::Object(object) => {
                if let Some(at) = instance(object.location) {
                    found.provenance(at, "an object on a page");
                }
            }
            FragmentItem::Image(image) => {
                if let Some(at) = instance(image.location) {
                    found.provenance(at, "an image");
                }
            }
        }
    }
}

fn tag(tag: &Tag, instance: &impl Fn(Location) -> Option<Span>, found: &mut Found) {
    let location = Location {
        source: tag.source,
        instance: tag.instance,
    };
    if let Some(at) = instance(location) {
        let what = match tag.role {
            folio_pdf::Role::Heading => "a bookmark and a structure element",
            folio_pdf::Role::Figure | folio_pdf::Role::Formula if tag.text.is_some() => {
                "an alternate description"
            }
            _ => "a structure element",
        };
        found.provenance(at, what);
    }
    for child in &tag.children {
        self::tag(child, instance, found);
    }
}

/// Searches the text of a PDF for the text the variant does not show.
/// `allow` holds text that may appear, normalised.
fn search(hidden: &Hidden, extracted: &Extracted, allow: &[String], found: &mut Found) {
    let mut text = String::new();
    for part in extracted.pages.iter().chain(&extracted.strings) {
        // Text of different pages and strings never runs together.
        text.push_str(&normalise(part));
        text.push('\u{0}');
    }
    let chars: Vec<char> = text.chars().collect();
    let windows: HashSet<&[char]> = chars.windows(CHARS).collect();
    let leaked =
        |window: &str| !hidden.shown.contains(window) && !allow.iter().any(|a| a.contains(window));
    for (at, passage) in &hidden.passages {
        // The words of the passage as written, and as compared.
        let written: Vec<&str> = passage.split_whitespace().collect();
        let words: Vec<String> = written.iter().map(|w| normalise(w)).collect();
        // Where each word starts in the passage's compared characters.
        let mut starts = Vec::with_capacity(words.len());
        let mut all: Vec<char> = Vec::new();
        for word in &words {
            starts.push(all.len());
            all.extend(word.chars());
        }
        // The written words from the one that holds character `from` to
        // the one that holds character `to - 1`.
        let quote = |from: usize, to: usize| -> String {
            let first = starts.partition_point(|&s| s <= from).saturating_sub(1);
            let last = starts.partition_point(|&s| s < to).saturating_sub(1);
            written[first..=last.max(first)].join(" ")
        };
        // Any 30 letters and digits in a row.
        let mut hit = all
            .windows(CHARS)
            .enumerate()
            .filter(|(_, w)| windows.contains(w))
            .find(|(_, w)| leaked(&w.iter().collect::<String>()))
            .map(|(i, _)| quote(i, i + CHARS));
        // Any six words in a row with a letter or digit each; those of 30
        // characters or more were looked for above.
        if hit.is_none() {
            let mut i = 0;
            while hit.is_none() && i + WORDS <= words.len() {
                let mut run = Vec::new();
                let mut j = i;
                while run.len() < WORDS && j < words.len() {
                    if !words[j].is_empty() {
                        run.push(j);
                    }
                    j += 1;
                }
                if run.len() == WORDS {
                    let joined: String = run.iter().map(|&k| words[k].as_str()).collect();
                    if joined.chars().count() < CHARS
                        && text.contains(joined.as_str())
                        && leaked(&joined)
                    {
                        hit = Some(written[run[0]..=run[WORDS - 1]].join(" "));
                    }
                }
                i += 1;
            }
        }
        if let Some(words) = hit {
            found.text(*at, &words);
        }
    }
}

/// Text as the text search compares it: its letters and digits, in lower
/// case, without spaces, punctuation or marks, so that the way an output
/// breaks lines, hyphenates or spaces words does not matter.
pub(crate) fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use folio_component::Name;
    use folio_pdf::Role;

    use super::*;
    use crate::World;

    const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                            compiler = \"0.0.0\"\nlang = \"en\"\n\
                            styles = [\"paper.fsty\"]\n\n\
                            [variants.exam]\ntargets = [\"pdf\"]\n\n\
                            [variants.markscheme]\ntargets = [\"pdf\"]\n";

    /// The answer the exam hides.
    const ANSWER: &str = "The speed is nineteen point six metres per second downwards";

    fn world() -> World {
        let mut world = World::new(MANIFEST).unwrap();
        world
            .set_source(
                "paper.fsty",
                "figure.answer { display: none; }\n\
                 @variant markscheme { figure.answer { display: block; } }\n",
            )
            .unwrap();
        world
            .set_source(
                "main.fol",
                &format!(
                    "#doc(title: [Forces])\n\n= Question one\n\n\
                     A ball is dropped from rest. Find its speed $v = g t$ after two seconds.\n\n\
                     #figure(class: \"answer\")[{ANSWER}.]\n"
                ),
            )
            .unwrap();
        world
    }

    /// The location of the first `figure` of an element's tree.
    fn figure(element: &Element) -> Option<Location> {
        if element.name.name.as_str() == "figure" {
            return Some(element.location);
        }
        element.fields.iter().find_map(|(_, value)| match value {
            Value::Content(content) => content.nodes().iter().find_map(|node| match node {
                Node::Element(inner) => figure(inner),
                _ => None,
            }),
            _ => None,
        })
    }

    /// Runs `test` on the exam build of [`world`], its hidden content,
    /// the location of the hidden answer, and the writer's input.
    fn exam(test: impl FnOnce(&dyn Db, Build<'_>, &Hidden, Location, PdfInputCopy)) {
        let world = world();
        let db = world.db();
        let variant = Name::new("exam").unwrap();
        let build = Build::new(db, world.project(), Target::Pdf, Some(variant));
        let hidden = Hidden::of(db, build).unwrap();
        let doc = document(db, build.project(db), build.variant(db).clone(), None)
            .as_ref()
            .unwrap();
        let answer = figure(&doc.root).unwrap();
        assert!(hidden.locations.contains(&answer));
        let input = pdf_input(db, build).clone().unwrap();
        test(db, build, &hidden, answer, input);
    }

    type PdfInputCopy = crate::pipeline::PdfInput;

    /// The leaks that provenance and the text search find in a build
    /// whose writer's input `input` is.
    fn leaks_of(
        db: &dyn Db,
        build: Build<'_>,
        hidden: &Hidden,
        input: &PdfInputCopy,
    ) -> Vec<(EcoString, Option<EcoString>)> {
        let pages = pages(db, build).as_ref().unwrap();
        let fonts = FontTable(fonts(db, build.project(db)));
        let document = input.document(pages, &fonts);
        let mut found = Found::new(Some("exam"));
        provenance(hidden, &document, &mut found);
        let written = folio_pdf::write(&document).unwrap();
        let extracted = folio_pdf::extract_text(&written.bytes).unwrap();
        search(hidden, &extracted, &[], &mut found);
        found
            .diagnostics
            .iter()
            .map(|d| (d.message.clone(), d.code.clone()))
            .collect()
    }

    fn codes(found: &[(EcoString, Option<EcoString>)]) -> Vec<&str> {
        let mut codes: Vec<&str> = found.iter().filter_map(|(_, c)| c.as_deref()).collect();
        codes.sort_unstable();
        codes
    }

    #[test]
    fn a_build_that_hides_its_answer_does_not_leak() {
        exam(|db, build, hidden, _, input| {
            assert_eq!(leaks_of(db, build, hidden, &input), vec![]);
            assert!(leaks(db, build).is_empty());
        });
    }

    /// The first node of a role in a structure.
    fn find(tag: &mut Tag, role: Role) -> Option<&mut Tag> {
        if tag.role == role {
            return Some(tag);
        }
        tag.children.iter_mut().find_map(|child| find(child, role))
    }

    /// The leaks provenance finds in a build whose writer's input `input`
    /// is.
    fn by_provenance(
        db: &dyn Db,
        build: Build<'_>,
        hidden: &Hidden,
        input: &PdfInputCopy,
    ) -> Vec<EcoString> {
        let pages = pages(db, build).as_ref().unwrap();
        let fonts = FontTable(fonts(db, build.project(db)));
        let mut found = Found::new(Some("exam"));
        provenance(hidden, &input.document(pages, &fonts), &mut found);
        found
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn a_hidden_answer_in_alt_text_is_caught() {
        exam(|db, build, hidden, answer, mut input| {
            // The formula's alternate description is the answer.
            let tag = find(&mut input.structure, Role::Formula).unwrap();
            tag.text = Some(ANSWER.into());
            let found = leaks_of(db, build, hidden, &input);
            assert_eq!(codes(&found), [LEAK_TEXT], "{found:?}");
            // A node of the hidden answer, with an alternate description.
            let mut tag = Tag::new(Role::Figure, answer);
            tag.text = Some(ANSWER.into());
            input.structure.children.push(tag);
            let found = by_provenance(db, build, hidden, &input);
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(found[0].contains("an alternate description"), "{found:?}");
        });
    }

    #[test]
    fn a_hidden_answer_in_a_bookmark_is_caught() {
        exam(|db, build, hidden, answer, mut input| {
            // The heading's bookmark shows the answer.
            let tag = find(&mut input.structure, Role::Heading).unwrap();
            tag.text = Some(ANSWER.into());
            let found = leaks_of(db, build, hidden, &input);
            assert_eq!(codes(&found), [LEAK_TEXT], "{found:?}");
            // The heading names the hidden answer as its source.
            let tag = find(&mut input.structure, Role::Heading).unwrap();
            tag.source = answer.source;
            tag.instance = answer.instance;
            let found = by_provenance(db, build, hidden, &input);
            assert_eq!(found.len(), 1, "{found:?}");
            assert!(found[0].contains("a bookmark"), "{found:?}");
        });
    }

    #[test]
    fn a_hidden_answer_in_the_metadata_is_caught() {
        exam(|db, build, hidden, answer, mut input| {
            input.info.subject = Some(ANSWER.into());
            input.info.provenance.push(Provenance {
                source: answer.source,
                location: answer,
            });
            let found = leaks_of(db, build, hidden, &input);
            assert_eq!(codes(&found), [LEAK, LEAK_TEXT], "{found:?}");
            assert!(
                found.iter().any(|(m, _)| m.contains("metadata")),
                "{found:?}"
            );
            assert!(
                found.iter().any(|(m, _)| m.contains("nineteen point six")),
                "{found:?}"
            );
        });
    }

    #[test]
    fn page_text_is_read_through_the_fonts() {
        let world = world();
        let db = world.db();
        let variant = Name::new("markscheme").unwrap();
        let build = Build::new(db, world.project(), Target::Pdf, Some(variant));
        let bytes = emit(db, build).as_ref().unwrap();
        let extracted = folio_pdf::extract_text(bytes).unwrap();
        let pages = normalise(&extracted.pages.concat());
        assert!(pages.contains(&normalise(ANSWER)), "{:?}", extracted.pages);
        // The title is a string of the file: in the Info dictionary.
        assert!(extracted.strings.iter().any(|s| s == "Forces"));
        assert!(!pages.is_empty() && !leaks(db, build).iter().any(|d| d.is_error()));
    }

    #[test]
    fn a_box_of_hidden_content_is_caught() {
        exam(|db, build, hidden, answer, _| {
            let pages = pages(db, build).as_ref().unwrap();
            let mut page = pages.pages[0].fragment.clone();
            page.push_fragment(
                folio_component::Point::zero(),
                folio_component::Fragment::new(folio_component::Size::zero()).with_location(answer),
            );
            let input = pdf_input(db, build).as_ref().unwrap();
            let fonts = FontTable(fonts(db, build.project(db)));
            let mut document = input.document(pages, &fonts);
            document.pages[0] = &page;
            let mut found = Found::new(Some("exam"));
            provenance(hidden, &document, &mut found);
            assert_eq!(found.diagnostics.len(), 1);
            assert!(found.diagnostics[0].message.contains("a box on a page"));
            assert_eq!(found.diagnostics[0].code.as_deref(), Some(LEAK));
        });
    }

    #[test]
    fn text_is_compared_without_case_spaces_or_punctuation() {
        assert_eq!(normalise("Nine-teen  Point,\nSIX"), "nineteenpointsix");
        assert_eq!(normalise("Łódź 9.8 m/s²"), "łódź98ms²");
    }
}
