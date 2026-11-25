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

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{FontId, QualifiedName, SourceRef, Target};
use folio_core::db::{Build, Bytes, Db, build_output, source_file};
use folio_core::diag::{self, Diagnostic};
use folio_core::eval::Value;
use folio_core::fonts::{Fonts, fonts};
use folio_core::layout::pages;
use folio_core::model::document;
use folio_core::pipeline::step_styled;
use folio_core::project::ProjectPath;
use folio_core::span_map::{source_ref, span};
use folio_core::text::{Text, TextBlock, TextFlow, text};
use folio_pdf::{ListKind, Role, Tag};
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

/// Compiles a build: checks what it reads ([`build_output`]) and runs it
/// through every stage to [`emit`]. The diagnostics of the build are those
/// of this query and of every query it depends on.
#[salsa::tracked(returns(ref))]
pub(crate) fn compile_build(db: &dyn Db, build: Build<'_>) -> Built {
    let checked = build_output(db, build);
    let output = emit(db, build).clone();
    Built {
        sources: checked.sources.clone(),
        output,
    }
}

/// Emits a build (stage 9): the file its target writes, or `None` when the
/// document could not be styled, or the target writes no file or has no
/// writer yet.
#[salsa::tracked(returns(ref))]
pub(crate) fn emit(db: &dyn Db, build: Build<'_>) -> Option<Bytes> {
    let target = build.target(db);
    match target {
        Target::Pdf => {
            let pages = pages(db, build).as_ref()?;
            let text = text(db, build).as_ref()?;
            let project = build.project(db);
            let doc = document(db, project, build.variant(db).clone()).as_ref()?;
            let fonts = FontTable(fonts(db, project));
            let structure = structure(&text.root, text);
            let plain = |field: &str| match doc.root.field(field) {
                Some(Value::Content(content)) => Some(content.plain_text()),
                Some(Value::Str(text)) => Some(text.clone()),
                _ => None,
            };
            let lang = project.manifest(db).project.lang.v.as_str();
            let info = folio_pdf::Info {
                title: plain("title").and_then(clean),
                subject: plain("subject").and_then(clean),
                lang: Some(lang.into()),
            };
            let document = folio_pdf::Document {
                pages: pages.pages.iter().map(|page| &page.fragment).collect(),
                fonts: &fonts,
                structure: &structure,
                info,
            };
            // Problems without a source of their own concern the document.
            let whole = document_span(db, build, doc.root.span);
            let at = |source: SourceRef| {
                if source.is_detached() {
                    whole
                } else {
                    span(source).into()
                }
            };
            match folio_pdf::write(&document) {
                Ok(written) => {
                    for warning in written.warnings {
                        let mut diagnostic =
                            Diagnostic::warning(at(warning.source), warning.message);
                        if let Some(hint) = warning.hint {
                            diagnostic = diagnostic.with_hint(hint);
                        }
                        diag::report(db, diagnostic);
                    }
                    Some(written.bytes.into())
                }
                Err(error) => {
                    diag::report(db, Diagnostic::error(at(error.source), error.message));
                    None
                }
            }
        }
        Target::Live => {
            pages(db, build).as_ref()?;
            unsupported(
                db,
                build,
                "the live target is not implemented yet: this build lays the \
                 document out but presents nothing",
            );
            None
        }
        Target::Docx | Target::Pptx => {
            step_styled(db, build)?;
            let format = target.as_str().to_uppercase();
            unsupported(
                db,
                build,
                eco_format!(
                    "{format} output is not implemented yet: this build checks the \
                     document but writes no file"
                ),
            );
            None
        }
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

/// The structure of a document for the PDF writer: a node for every block
/// of the text stage, whose boxes the page builder's fragments are, with
/// the role of its element or part (SPEC §11.1).
pub(crate) fn structure(block: &TextBlock, text: &Text) -> Tag {
    let role = role(&block.element, block.part.as_ref().map(|p| p.as_str()));
    let mut tag = Tag::new(role, source_ref(block.span));
    if role == Role::Heading {
        let mut words = String::new();
        for child in &block.children {
            if let TextFlow::Paragraph(index) = child {
                words.push(' ');
                words.push_str(text.paragraphs[*index].shaped.text());
            }
        }
        tag.text = clean(words.into());
    }
    tag.children = block
        .children
        .iter()
        .filter_map(|child| match child {
            TextFlow::Block(block) => Some(structure(block, text)),
            TextFlow::Paragraph(_) => None,
        })
        .collect();
    tag
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
        ("sidenote", None) => Role::Note,
        ("raw", None) => Role::Code,
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
