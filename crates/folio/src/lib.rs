//! The Folio compiler library: the memoised compile pipeline
//! ([`compile`]) and the [`World`] that supplies sources, fonts, files and
//! components (SPEC §2.2).
//!
//! A host builds a [`World`] from the project's manifest and lockfile, sets
//! the sources and files it reads from disk, and compiles a variant for a
//! target. Every stage is a memoised query (SPEC §2.3): after an edit,
//! compiling again re-runs only the queries the edit affects, and stops early
//! where a re-run query returns what it returned before. An edit cancels
//! compilations still running on other handles of the world.
//!
//! ```
//! use folio::{Name, Target, World};
//!
//! let mut world = World::new(
//!     "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
//!      [variants.exam]\ntargets = [\"pdf\"]\n",
//! )
//! .unwrap();
//! world.set_source("main.fol", "= Forces\n\nA ball is thrown.\n").unwrap();
//!
//! let exam = Name::new("exam").unwrap();
//! let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
//! assert!(!compiled.has_errors());
//! assert!(compiled.output.unwrap().starts_with(b"%PDF-"));
//! ```

#![forbid(unsafe_code)]

pub mod diag;
pub mod loader;
mod pipeline;
pub mod span_map;
pub mod toolchain;
mod world;

use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};
use std::panic::AssertUnwindSafe;

use ecow::EcoVec;
use folio_core::db::Read;
use folio_core::diag::Reported;
use rayon::prelude::*;

pub use folio_component::{
    Abs, Fragment, FragmentItem, GlyphRun, Name, Point, Ratio, Registry, Size, SourceRef, Target,
};
pub use folio_core::db::{BuildMode, Bytes, FileKind, FileRead};
pub use folio_core::diag::{Diagnostic, Edit, Fix, Related, Severity};
pub use folio_core::eval;
pub use folio_core::eval::Limits;
pub use folio_core::fonts::{Face, FontRun, Fonts, Pin};
pub use folio_core::layout;
pub use folio_core::layout::{Line, LineRef, Lines, Page, PageMaster, Pages, ParagraphLayout};
pub use folio_core::model;
pub use folio_core::project::{Date, Lockfile, Manifest, OutputPattern, ProjectError, ProjectPath};
pub use folio_core::style;
pub use folio_core::text::{SegmentStyle, Text, TextBlock, TextFlow, TextParagraph, TextSource};
pub use folio_fonts::{
    BUNDLED_FAMILIES, FaceKey, FontCache, FontFile, FontHash, FontOrigin, FontStyle, FontVariant,
    PROJECT_FONT_DIR, SystemFont, bundled as bundled_fonts, project_fonts, system_fonts,
};
pub use folio_syntax::{FileId, NodeId, Source, Span};
pub use folio_text::{
    Break, BreakKind, Feature, Glyph, Item, ItemKind, Lang, Paragraph, Segment, SegmentKind,
    Shaped, mark_breaks,
};

pub use self::world::{Event, Snapshot, World, WorldBuilder, WorldError};

/// The version of this compiler, which `folio.toml` pins for byte-identical
/// output (SPEC §2.4).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compiles a variant of the world's document for a target, through
/// every stage of SPEC §2.1. The variant is `None` only for a project that
/// declares no variants.
///
/// Problems in the document are diagnostics in the result. Compilation is
/// partial (SPEC §12.2): a construct that fails becomes an error marker in
/// place and the rest still compiles, so a target that writes a file has
/// output unless nothing could be produced, as when the entry file is
/// missing ([`Compiled::output`] says which targets write none yet). Fails
/// only when an edit to the world cancels the compilation.
pub fn compile(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Compiled, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        let built = pipeline::compile_build(db, build);
        // A diagnostic that two queries report alike, such as a missing
        // imported file that both the file search and evaluation find, is
        // kept once.
        let mut seen = HashSet::new();
        let diagnostics = pipeline::compile_build::accumulated::<Reported>(db, build)
            .into_iter()
            .filter(|reported| seen.insert(&reported.0))
            .map(|reported| reported.0.clone())
            .collect();
        // A file read twice, or read as a source and as data, is listed
        // where it was first read.
        let mut read = HashSet::new();
        let files = pipeline::compile_build::accumulated::<Read>(db, build)
            .into_iter()
            .filter(|r| read.insert(&r.0.path))
            .map(|r| r.0.clone())
            .collect();
        Compiled {
            sources: built.sources.clone(),
            files,
            output: built.output.clone(),
            diagnostics,
        }
    })
}

/// What compiling a build produces: its output and its diagnostics.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Compiled {
    /// The source files the build read, in the order they were found. A
    /// watcher recompiles when one of them changes.
    pub sources: EcoVec<ProjectPath>,
    /// Every file the build read or tried to read, with how it read it, in
    /// the order they were first read: its sources, and the data files and
    /// images that evaluation read. A file named but missing is listed too.
    /// A watcher rebuilds when one of them is added, changed or removed.
    pub files: EcoVec<FileRead>,
    /// The file the target writes: a PDF for [`Target::Pdf`]. `None` when
    /// the document could not be styled, or the target writes no file or
    /// has no writer yet (DOCX and PPTX until M4 and M5, live until M7).
    pub output: Option<Bytes>,
    /// Every problem found, errors and warnings, in the order the queries
    /// that reported them first ran.
    pub diagnostics: EcoVec<Diagnostic>,
}

impl Compiled {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Evaluates a variant of the world's document into its content tree
/// (SPEC §2.1), or `None` when it could not be evaluated. The variant is
/// `None` only for a project that declares no variants.
///
/// The problems found are the diagnostics that [`compile`] reports for the
/// variant. Evaluation does not depend on the target, so every target of a
/// variant shares it. Fails only when an edit to the world cancels it.
pub fn evaluate(world: &World, variant: Option<&Name>) -> Result<Option<eval::Content>, Cancelled> {
    catch(|| eval::evaluate(world.db(), world.project(), variant.cloned()).clone())
}

/// Builds the document tree of a variant of the world's document (SPEC
/// §4.2), or `None` when it could not be evaluated. The variant is `None`
/// only for a project that declares no variants.
///
/// The problems found are the diagnostics that [`compile`] reports for the
/// variant. Like evaluation, it does not depend on the target. Fails only
/// when an edit to the world cancels it.
pub fn document(
    world: &World,
    variant: Option<&Name>,
) -> Result<Option<model::Document>, Cancelled> {
    catch(|| model::document(world.db(), world.project(), variant.cloned()).clone())
}

/// Styles a variant of the world's document for a target (SPEC §2.1,
/// §5): the document tree with a computed style for every node, or `None`
/// when the document could not be built. The variant is `None` only for a
/// project that declares no variants.
///
/// The problems found are the diagnostics that [`compile`] reports for the
/// build. Fails only when an edit to the world cancels it.
pub fn styles(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Option<style::Styled>, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        style::styles(db, build).clone()
    })
}

/// The faces the world's project can use (SPEC §2.5): the bundled
/// families, the project's and locked fonts, and the system fonts, each with
/// what pins it.
///
/// The problems found (files that hold no font, locked fonts that are
/// missing or changed) are diagnostics that [`compile`] reports. Fails only
/// when an edit to the world cancels it.
pub fn fonts(world: &World) -> Result<Fonts, Cancelled> {
    catch(|| folio_core::fonts::fonts(world.db(), world.project()).clone())
}

/// Shapes the paragraphs of a variant of the world's document for a target
/// (SPEC §7.2): each itemised by script and direction, set in its fonts by
/// fallback, shaped, and with its line-break opportunities and hyphenation
/// points; or `None` when the document could not be styled. The variant
/// is `None` only for a project that declares no variants.
///
/// Paragraphs equal to ones shaped before in this world share their shaped
/// form, so after an edit only the changed paragraphs are shaped again. The
/// problems found are diagnostics that [`compile`] reports. Fails only when
/// an edit to the world cancels it.
pub fn text(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Option<Text>, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        folio_core::text::text(db, build).clone()
    })
}

/// Breaks the paragraphs of a variant of the world's document into lines
/// for a target (SPEC §7.2), by Knuth–Plass total fit at the width of the
/// default page master's body; or `None` when the document could not be
/// styled. The variant is `None` only for a project that declares no
/// variants.
///
/// Paragraphs whose content, style and widths are equal to ones broken
/// before in this world share their lines, so after an edit only the
/// changed paragraphs are broken again. Overfull and underfull lines are
/// diagnostics that [`compile`] reports. Fails only when an edit to the
/// world cancels it.
pub fn lines(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Option<Lines>, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        layout::lines(db, build).clone()
    })
}

/// Lays out a variant of the world's document for a target into pages
/// (SPEC §7.1): its blocks and lines broken into pages of the default page
/// master, strictly forward, each page a fragment holding nested
/// fragments for its blocks, with their glyph runs, backgrounds, borders,
/// baselines and keep data; or `None` when the document could not be
/// styled. The variant is `None` only for a project that declares no
/// variants.
///
/// Pages whose content is equal to pages laid out before in this world are
/// taken from the page cache. Constraints that a page break had to relax
/// (widows, orphans, keeps) and content that does not fit on a page are
/// diagnostics that [`compile`] reports. Fails only when an edit to the
/// world cancels it.
pub fn pages(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Option<Pages>, Cancelled> {
    catch(|| {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        layout::pages(db, build).clone()
    })
}

/// Compiles every variant and target the manifest declares
/// ([`World::builds`]), in parallel, each on its own handle of the world.
/// Queries that do not read the variant or target run once and are shared.
pub fn compile_all(world: &World) -> Vec<Output> {
    compile_builds(world, world.builds())
}

/// Compiles builds of the world's document, each a variant (`None` only
/// for a project without variants) and a target, as [`compile_all`] does:
/// in parallel, each on its own handle of the world. The outputs are in
/// the order of the builds.
pub fn compile_builds(
    world: &World,
    builds: impl IntoIterator<Item = (Option<Name>, Target)>,
) -> Vec<Output> {
    // A handle is not `Sync`, so each build takes its own.
    let builds: Vec<_> = builds
        .into_iter()
        .map(|build| (world.clone(), build))
        .collect();
    builds
        .into_par_iter()
        .map(|(world, (variant, target))| {
            let result = compile(&world, target, variant.as_ref());
            Output {
                variant,
                target,
                result,
            }
        })
        .collect()
}

/// The result of one build of [`compile_all`] or [`compile_builds`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Output {
    /// The variant, or `None` for a project without variants.
    pub variant: Option<Name>,
    /// The target.
    pub target: Target,
    /// What compiling it produced.
    pub result: Result<Compiled, Cancelled>,
}

/// A compilation was stopped by an edit to the world, so its result would
/// have been stale. Compile again to get a current one.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Cancelled;

impl Display for Cancelled {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("the compilation was cancelled by an edit")
    }
}

impl std::error::Error for Cancelled {}

/// Runs queries, turning salsa's cancellation unwind into [`Cancelled`].
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, Cancelled> {
    salsa::Cancelled::catch(AssertUnwindSafe(f)).map_err(|cancelled| match cancelled {
        salsa::Cancelled::PropagatedPanic => {
            panic!("a query panicked on another thread")
        }
        _ => Cancelled,
    })
}
