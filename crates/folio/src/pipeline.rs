//! The end of the compile pipeline (SPEC §2.1): the fork after Step
//! styling, the Emit stage, and the query that compiles a build.
//!
//! The stages up to Step styling are queries of `folio-core` (see
//! [`folio_core::pipeline`]). After them the pipeline forks by target:
//!
//! - **Fixed-layout targets** (PDF, live) go through Layout: the text
//!   stage, the paragraph builder and the page builder read the
//!   step-styled tree, and [`emit`] hands the pages to the target's
//!   writer. PDF writes a file; the live renderer (M7) will take the pages
//!   and writes none.
//! - **Office targets** (DOCX, PPTX) take the step-styled tree itself,
//!   using Layout only for measurement. Their writers come with M4 and M5;
//!   until then a build for them styles the document, reports what it
//!   finds, and produces nothing.

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{FontId, Target};
use folio_core::db::{Build, Bytes, Db, build_output};
use folio_core::diag::{self, Diagnostic};
use folio_core::fonts::{Fonts, fonts};
use folio_core::layout::pages;
use folio_core::pipeline::step_styled;
use folio_core::project::ProjectPath;
use folio_syntax::DiagSpan;

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
            let fonts = FontTable(fonts(db, build.project(db)));
            let document = folio_pdf::Document {
                pages: pages.pages.iter().map(|page| &page.fragment).collect(),
                fonts: &fonts,
            };
            Some(folio_pdf::write(&document).into())
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
