//! The end of the compile pipeline (SPEC §2.1): the fork after Step
//! styling, the Emit stage, and the query that compiles a build.
//!
//! The stages up to Step styling are queries of `folio-core` (see
//! [`folio_core::pipeline`]). After them the pipeline forks by target:
//!
//! - **Fixed-layout targets** (PDF, live) go through Layout: the text
//!   stage, the paragraph builder and the page builder read the
//!   step-styled tree, and [`emit`] hands the pages to the target's
//!   writer. PDF writes a file, tagged with the structure of the text
//!   stage's blocks ([`structure`]); the live renderer (M7) will take the
//!   pages and writes none.
//! - **Office targets** (DOCX, PPTX) take the step-styled tree itself,
//!   using Layout only for measurement. Their writers come with M4 and M5;
//!   until then a build for them styles the document, reports what it
//!   finds, and produces nothing.
//!
//! A build is checked ([`check_build`]) before it is emitted: the check
//! runs every stage up to the writer and the writer's own check
//! ([`folio_pdf::preflight`]), and reports every diagnostic that emitting
//! would, without writing. `folio check`, the language server and the
//! preview stop there; [`compile_build`] goes on to [`emit`].

use std::ops::Range;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{FontId, Location, QualifiedName, SourceRef, Target};
use folio_core::db::{Build, Bytes, Db, build_output, source_file};
use folio_core::diag::{self, Diagnostic};
use folio_core::eval::Value;
use folio_core::fonts::{Fonts, fonts};
use folio_core::layout::{GridLayout, Pages, lines, pages};
use folio_core::model::document;
use folio_core::pipeline::resolution;
use folio_core::pipeline::step_styled;
use folio_core::project::ProjectPath;
use folio_core::resolve::Resolution;
use folio_core::span_map::{source_ref, span};
use folio_core::text::{TableRole, Text, TextBlock, TextFlow, TextMath, text};
use folio_pdf::{Destination, Link, ListKind, Navigation, Preflight, Role, Span2, Tag};
use folio_syntax::{DiagSpan, Span, SubRange};

/// What compiling a build produces, without its diagnostics, which
/// [`crate::compile`] collects.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct Built {
    /// The source files the build read.
    pub sources: EcoVec<ProjectPath>,
    /// The file the build writes.
    pub output: Option<Bytes>,
}

/// Compiles a build: checks it ([`check_build`]) and emits it. The
/// diagnostics of the build are those of this query and of every query it
/// depends on.
#[salsa::tracked(returns(ref))]
pub(crate) fn compile_build(db: &dyn Db, build: Build<'_>) -> Built {
    let sources = check_build(db, build).sources.clone();
    let output = emit(db, build).clone();
    Built { sources, output }
}

/// What checking a build finds, without its diagnostics.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct Checked {
    /// The source files the build read.
    pub sources: EcoVec<ProjectPath>,
    /// Whether emitting the build would write a file.
    pub writes: bool,
}

/// Checks a build without emitting it: checks what it reads
/// ([`build_output`]) and runs it through every stage before Emit, and for
/// PDF the writer's check. Its diagnostics are those that
/// [`compile_build`] reports, but for the few that only writing finds: an
/// error of the writer itself, such as a font it cannot subset, and a
/// reason not to claim PDF/UA-1 that the writer's check does not foresee.
#[salsa::tracked(returns(ref))]
pub(crate) fn check_build(db: &dyn Db, build: Build<'_>) -> Checked {
    let checked = build_output(db, build);
    let writes = *check_target(db, build);
    Checked {
        sources: checked.sources.clone(),
        writes,
    }
}

/// Runs a build through every stage before Emit, and for PDF the writer's
/// check: whether emitting it would write a file, which only PDF does, and
/// only when the document could be styled. A target that has no writer
/// yet is reported here.
#[salsa::tracked]
pub(crate) fn check_target(db: &dyn Db, build: Build<'_>) -> bool {
    let target = build.target(db);
    match target {
        Target::Pdf => pdf_check(db, build).is_some(),
        Target::Live => {
            if pages(db, build).is_some() {
                unsupported(
                    db,
                    build,
                    "the live target is not implemented yet: this build lays the \
                     document out but presents nothing",
                );
            }
            false
        }
        Target::Docx | Target::Pptx => {
            if step_styled(db, build).is_some() {
                let format = target.as_str().to_uppercase();
                unsupported(
                    db,
                    build,
                    eco_format!(
                        "{format} output is not implemented yet: this build checks the \
                         document but writes no file"
                    ),
                );
            }
            false
        }
    }
}

/// What the PDF writer reads of a build besides its pages and fonts: the
/// structure, the metadata, and where problems of the whole document are
/// reported.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct PdfInput {
    structure: Tag,
    info: folio_pdf::Info,
    navigation: Navigation,
    whole: DiagSpan,
}

impl PdfInput {
    /// The document the writer writes, of these pages and fonts.
    fn document<'a>(
        &'a self,
        pages: &'a Pages,
        fonts: &'a FontTable<'a>,
    ) -> folio_pdf::Document<'a> {
        folio_pdf::Document {
            pages: pages.pages.iter().map(|page| &page.fragment).collect(),
            fonts,
            structure: &self.structure,
            info: self.info.clone(),
            navigation: &self.navigation,
        }
    }

    /// Where the writer's problem with `source` is reported.
    fn at(&self, source: SourceRef) -> DiagSpan {
        if source.is_detached() {
            self.whole
        } else {
            span(source).into()
        }
    }

    /// Reports a warning of the writer.
    fn warn(&self, db: &dyn Db, warning: &folio_pdf::Warning) {
        let mut diagnostic = Diagnostic::warning(self.at(warning.source), warning.message.clone());
        if let Some(hint) = &warning.hint {
            diagnostic = diagnostic.with_hint(hint.clone());
        }
        diag::report(db, diagnostic);
    }
}

/// What the PDF writer reads of a PDF build besides its pages and fonts,
/// or `None` when the document could not be styled.
#[salsa::tracked(returns(ref))]
pub(crate) fn pdf_input(db: &dyn Db, build: Build<'_>) -> Option<PdfInput> {
    let text = text(db, build).as_ref()?;
    let project = build.project(db);
    let doc = document(db, project, build.variant(db).clone()).as_ref()?;
    let grids: Vec<GridLayout> = lines(db, build)
        .as_ref()
        .map(|l| l.grids.iter().cloned().collect())
        .unwrap_or_default();
    let structure = structure(&text.root, text, &grids);
    let plain = |field: &str| match doc.root.field(field) {
        Some(Value::Content(content)) => Some(content.plain_text()),
        Some(Value::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let lang = project.manifest(db).project.lang.v.as_str();
    // The title as it is shown, with the language's quotation marks, if
    // it is shown.
    let shown = text.root.children.iter().find_map(|child| match child {
        TextFlow::Block(block) if block.part.as_ref().is_some_and(|p| p.as_str() == "title") => {
            block_text(block, text)
        }
        _ => None,
    });
    let info = folio_pdf::Info {
        title: shown.or_else(|| plain("title").and_then(clean)),
        subject: plain("subject").and_then(clean),
        lang: Some(lang.into()),
    };
    let navigation = resolution(db, build).map(navigation).unwrap_or_default();
    // Problems without a source of their own concern the document.
    let whole = document_span(db, build, doc.root.span);
    Some(PdfInput {
        structure,
        info,
        navigation,
        whole,
    })
}

/// The PDF writer's check of a build ([`folio_pdf::preflight`]), which
/// reports the warnings that writing it gives; or `None` when the document
/// could not be styled.
#[salsa::tracked(returns(ref))]
pub(crate) fn pdf_check(db: &dyn Db, build: Build<'_>) -> Option<Preflight> {
    let pages = pages(db, build).as_ref()?;
    let input = pdf_input(db, build).as_ref()?;
    let fonts = FontTable(fonts(db, build.project(db)));
    // An edit stops the query between pages (SPEC §2.3).
    let check = &mut || db.unwind_if_revision_cancelled();
    let checked = folio_pdf::preflight_with(&input.document(pages, &fonts), check);
    for warning in &checked.warnings {
        input.warn(db, warning);
    }
    Some(checked)
}

/// Emits a build (stage 9): the file its target writes, or `None` when the
/// document could not be styled, or the target writes no file or has no
/// writer yet. What the build's check reports ([`check_build`]) is not
/// reported again.
#[salsa::tracked(returns(ref))]
pub(crate) fn emit(db: &dyn Db, build: Build<'_>) -> Option<Bytes> {
    match build.target(db) {
        Target::Pdf => {
            let checked = pdf_check(db, build).as_ref()?;
            let pages = pages(db, build).as_ref()?;
            let input = pdf_input(db, build).as_ref()?;
            let fonts = FontTable(fonts(db, build.project(db)));
            let document = input.document(pages, &fonts);
            // An edit stops the query between pages (SPEC §2.3).
            let check = &mut || db.unwind_if_revision_cancelled();
            match folio_pdf::write_checked_with(&document, checked, check) {
                Ok(written) => {
                    // Those the check foresaw come first.
                    for warning in written.warnings.iter().skip(checked.warnings.len()) {
                        input.warn(db, warning);
                    }
                    Some(written.bytes.into())
                }
                Err(error) => {
                    let at = input.at(error.source);
                    diag::report(db, Diagnostic::error(at, error.message));
                    None
                }
            }
        }
        Target::Live | Target::Docx | Target::Pptx => None,
    }
}

/// Where a problem of the whole document is reported: at its `#doc` call,
/// or, without one, at the first character of the entry file. `root` is
/// the span of the document's root element.
fn document_span(db: &dyn Db, build: Build<'_>, root: Span) -> DiagSpan {
    let project = build.project(db);
    let entry = project.manifest(db).project.entry.v.clone();
    let Some(file) = source_file(db, project, entry) else {
        return root.into();
    };
    let source = file.source(db);
    if source.root().span() != root {
        return root.into();
    }
    let first = source.text().chars().next().map_or(0, char::len_utf8);
    DiagSpan::from_span(root, SubRange::new(0, first))
}

/// How a reader moves through a document (SPEC §11.1): a named destination
/// for each label the build shows, at the element it names (an equation's
/// line goes to its equation), and a link for each reference, over the
/// text Resolve gave it, to its label's destination. A reference that does
/// not resolve goes nowhere.
///
/// Every page is labelled with its plain number, for which the writer
/// writes no labels.
fn navigation(resolution: &Resolution) -> Navigation {
    let mut destinations: Vec<Destination> = resolution
        .labels
        .iter()
        .map(|labelled| Destination {
            name: labelled.label.clone(),
            location: labelled.target.location,
        })
        .collect();
    let mut links: Vec<Link> = resolution
        .references
        .iter()
        .map(|reference| Link {
            source: reference.location.source,
            destination: reference.target.map(|_| reference.label.clone()),
            text: reference.text.clone(),
        })
        .collect();
    entry_links(resolution, &mut destinations, &mut links);
    Navigation {
        destinations,
        links,
        page_labels: Vec::new(),
    }
}

/// The links of the entries of outlines and figure lists (SPEC §4.5): each
/// entry goes to the instance it lists, by its label, or else by a name
/// made for it, which no label can be, as a label has no spaces. The
/// entries of a list are glyphs of the list's source, one link after
/// another in order, each as long as its entry's text.
fn entry_links(
    resolution: &Resolution,
    destinations: &mut Vec<Destination>,
    links: &mut Vec<Link>,
) {
    let mut named: std::collections::HashMap<Location, EcoString> = resolution
        .labels
        .iter()
        .filter(|l| l.target.line.is_none())
        .map(|l| (l.target.location, l.label.clone()))
        .collect();
    for query in &resolution.queries {
        let folio_core::resolve::QueryResult::List(entries) = &query.result else {
            continue;
        };
        for entry in entries {
            let name = named
                .entry(entry.target)
                .or_insert_with(|| {
                    let name = eco_format!("entry {}", destinations.len());
                    destinations.push(Destination {
                        name: name.clone(),
                        location: entry.target,
                    });
                    name
                })
                .clone();
            links.push(Link {
                source: query.location.source,
                destination: Some(name),
                text: entry.text(),
            });
        }
    }
}

/// The structure of a document for the PDF writer: a node for every block
/// of the text stage, whose boxes the page builder's fragments are, with
/// the role of its element or part (SPEC §11.1), and a node for every
/// inline formula, whose chunks are boxes too, and every image. A formula's
/// text is its alternate description, a reading of its math tree
/// ([`formula_alt`]). An image's is its `alt` text.
///
/// A grid is a table (SPEC §7.4, §11.1): a `TR` for each of its rows, as
/// `grids` place its cells, holding the cells that start in it, `TH` in
/// its header rows and `TD` in the others. A render template's header is
/// a group, and so are its rows and their cells (SPEC §5.3).
pub(crate) fn structure(block: &TextBlock, text: &Text, grids: &[GridLayout]) -> Tag {
    let s = Structure { text, grids };
    s.of(block, false)
}

/// What the structure of a build is made from.
struct Structure<'a> {
    text: &'a Text,
    grids: &'a [GridLayout],
}

impl Structure<'_> {
    fn of(&self, block: &TextBlock, in_note: bool) -> Tag {
        structure_of(block, self, in_note)
    }

    /// The rows of a grid's table: each a `TR` of the grid's location,
    /// which no box names but the grid's own, holding its cells.
    fn table(&self, grid: &TextBlock, tag: &mut Tag, in_note: bool) -> bool {
        let Some(layout) = self.grids.iter().find(|g| g.location == grid.location) else {
            return false;
        };
        let mut rows: Vec<Tag> = (0..layout.rows)
            .map(|_| Tag {
                boxless: true,
                ..Tag::new(Role::TableRow, located(grid.span, grid.location))
            })
            .collect();
        for (child, place) in grid.children.iter().zip(layout.places.iter()) {
            // A block that stands for a cell without being one is its own
            // node in a cell without a box. A paragraph of the grid's own
            // has no block, which the document model never makes.
            let TextFlow::Block(cell) = child else {
                continue;
            };
            let span = Span2 {
                rows: u32::try_from(place.rowspan).unwrap_or(1),
                columns: u32::try_from(place.colspan).unwrap_or(1),
            };
            let role = if place.row < layout.header_rows {
                Role::TableHeader(span)
            } else {
                Role::TableCell(span)
            };
            let inner = self.of(cell, in_note);
            let cell_tag = if matches!(cell.table, Some(TableRole::Cell(_))) {
                Tag { role, ..inner }
            } else {
                // It names no box, so the block's box is its node's.
                Tag {
                    boxless: true,
                    children: vec![inner],
                    ..Tag::new(role, Location::DETACHED)
                }
            };
            if let Some(row) = rows.get_mut(place.row) {
                row.children.push(cell_tag);
            }
        }
        tag.children
            .extend(rows.into_iter().filter(|row| !row.children.is_empty()));
        true
    }
}

/// The location a structure node of the element at `span`, instance
/// `location`, names: the node of its span, so that an element without a
/// location still names its own node rather than none.
fn located(span: Span, location: Location) -> Location {
    Location {
        source: source_ref(span),
        instance: location.instance,
    }
}

/// The structure of a block, as [`structure`] gives it; `in_note` for the
/// blocks of a footnote, whose own footnotes are not placed, so they have
/// no node (SPEC §7.8).
fn structure_of(block: &TextBlock, s: &Structure<'_>, in_note: bool) -> Tag {
    let text = s.text;
    let role = role(&block.element, block.part.as_ref().map(|p| p.as_str()));
    // The node of its span, as its boxes name it, with its instance.
    let mut tag = Tag::new(role, located(block.span, block.location));
    match role {
        Role::Heading => tag.text = block_text(block, text),
        Role::Formula => tag.text = display_math(block, text).and_then(formula_alt),
        _ => {}
    }
    // A render template's row sets its cells side by side: it is layout,
    // not a table, so it and its cells are groups (SPEC §5.3). A
    // template's `grid` is a table.
    let template_row = block.part.as_ref().is_some_and(|p| p.as_str() == "row");
    if matches!(block.table, Some(TableRole::Grid { .. })) && !template_row {
        tag.role = Role::Table;
        if s.table(block, &mut tag, in_note) {
            return tag;
        }
        tag.role = Role::Group;
    }
    for child in &block.children {
        match child {
            TextFlow::Block(block) => {
                tag.children.push(s.of(block, in_note));
            }
            TextFlow::Paragraph(index) => {
                let paragraph = &text.paragraphs[*index];
                // A formula's first chunk, as the others continue it, and
                // each image, with its alt text as its description.
                let objects = paragraph.objects.iter().filter_map(|object| {
                    let (role, description) = if let Some(m) = object.math.filter(|m| m.chunk == 0)
                    {
                        let math = text.maths.get(m.formula);
                        (Role::Formula, math.and_then(formula_alt))
                    } else if let Some(image) = &object.image {
                        (Role::Figure, image.alt.clone().and_then(clean))
                    } else {
                        return None;
                    };
                    let mut child = Tag::new(role, located(object.span, object.location));
                    child.text = description;
                    Some((object.at, child))
                });
                // A footnote's note, whose boxes the page builder draws
                // inside the paragraph's on the page of its anchor
                // (SPEC §7.8, §11.1).
                let notes = paragraph.notes.iter().filter(|_| !in_note).map(|anchor| {
                    let mut note = s.of(&text.notes[anchor.note], true);
                    // Drawn after its page's body (SPEC §7.8).
                    note.foot = true;
                    (anchor.range.start, note)
                });
                let mut children: Vec<(usize, Tag)> = objects.chain(notes).collect();
                children.sort_by_key(|(at, _)| *at);
                tag.children
                    .extend(children.into_iter().map(|(_, child)| child));
            }
        }
    }
    tag
}

/// The formula of a display, a block of a `math` element: that of its
/// instance, so that each instance of a formula made in a loop or by a
/// closure reads as evaluation made it.
fn display_math<'a>(block: &TextBlock, text: &'a Text) -> Option<&'a TextMath> {
    text.maths
        .iter()
        .find(|m| m.span == block.span && m.location == block.location)
        .or_else(|| text.maths.iter().find(|m| m.span == block.span))
}

/// The alternate description of a formula: a reading of its evaluated
/// math tree, with its quantities in the number format of its text
/// ([`folio_math::Formula::reading`]). It is never read from the source,
/// so that comments, script code and what another variant shows do not
/// reach it (SPEC §6.4, §11.1).
fn formula_alt(math: &TextMath) -> Option<EcoString> {
    clean(math.formula.reading(&math.settings.number).into())
}

/// The text of the paragraphs a block holds directly, as [`clean`] gives
/// it: its inline formulas as their alternate descriptions
/// ([`formula_alt`]) and its images as their `alt` text, without the
/// anchors of footnotes. A heading's title and bookmark are
/// this text (SPEC §11.1).
fn block_text(block: &TextBlock, text: &Text) -> Option<EcoString> {
    let mut words = String::new();
    for child in &block.children {
        if let TextFlow::Paragraph(index) = child {
            let paragraph = &text.paragraphs[*index];
            let shown = paragraph.shaped.text();
            // What replaces parts of the text, in order: an anchor by
            // nothing, a formula's first chunk by its reading and its other
            // chunks by nothing, an image by its description.
            let mut splices: Vec<(Range<usize>, Option<EcoString>)> = paragraph
                .notes
                .iter()
                .map(|anchor| (anchor.range.clone(), None))
                .collect();
            for object in &paragraph.objects {
                let end = shown[object.at..]
                    .chars()
                    .next()
                    .map_or(object.at, |c| object.at + c.len_utf8());
                let with = if let Some(m) = object.math {
                    if m.chunk > 0 {
                        None
                    } else {
                        text.maths.get(m.formula).and_then(formula_alt)
                    }
                } else if let Some(image) = &object.image {
                    image.alt.clone()
                } else {
                    continue;
                };
                splices.push((object.at..end, with));
            }
            splices.sort_by_key(|(range, _)| range.start);
            let mut at = 0;
            words.push(' ');
            for (range, with) in splices {
                if range.start < at {
                    continue;
                }
                words.push_str(&shown[at..range.start]);
                if let Some(with) = with {
                    words.push_str(&with);
                }
                at = range.end;
            }
            words.push_str(&shown[at..]);
        }
    }
    clean(words.into())
}

/// The structure role of an element, or of its part `part`. An element
/// that PDF has no standard type for, such as a component's, is a group.
fn role(element: &QualifiedName, part: Option<&str>) -> Role {
    let builtin = match element.namespace {
        None => element.name.as_str(),
        Some(_) => "",
    };
    match (builtin, part) {
        ("doc", None) => Role::Document,
        ("doc", Some("title")) => Role::Title,
        ("section", None) => Role::Section,
        ("heading", None) => Role::Heading,
        ("paragraph", None) => Role::Paragraph,
        ("list", None) => Role::List(ListKind::Bulleted),
        ("enum", None) => Role::List(ListKind::Numbered),
        ("terms", None) => Role::List(ListKind::Terms),
        ("list-item" | "enum-item" | "terms-item", None) => Role::ListItem,
        ("terms-item", Some("term")) => Role::Label,
        ("footnote" | "sidenote", None) => Role::Note,
        ("raw", None) => Role::Code,
        ("math", None) => Role::Formula,
        (_, Some("caption")) => Role::Caption,
        _ => Role::Group,
    }
}

/// Text for metadata and bookmarks: line breaks and runs of spaces as one
/// space, objects (U+FFFC) left out, and `None` if nothing is left.
fn clean(text: EcoString) -> Option<EcoString> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '\u{FFFC}')
        .filter(|w| !w.is_empty())
        .collect();
    (!words.is_empty()).then(|| words.join(" ").into())
}

/// Warns that a build's target has no writer yet, at the target in the
/// variant's `targets`.
fn unsupported(db: &dyn Db, build: Build<'_>, message: impl Into<EcoString>) {
    let target = build.target(db);
    let manifest = build.project(db).manifest(db);
    let span = build
        .variant(db)
        .as_ref()
        .and_then(|name| manifest.variants.iter().find(|v| &v.name.v == name))
        .and_then(|variant| variant.targets.iter().find(|t| t.v == target))
        .map_or_else(DiagSpan::detached, |t| t.span);
    diag::report(db, Diagnostic::warning(span, message));
}

/// The fonts of a build for a writer: its faces, by the index that glyph
/// runs name them by.
struct FontTable<'a>(&'a Fonts);

impl folio_pdf::FontTable for FontTable<'_> {
    fn font(&self, id: FontId) -> Option<folio_fonts::Font> {
        self.0.faces().get(usize::try_from(id.0).ok()?)?.font()
    }
}
