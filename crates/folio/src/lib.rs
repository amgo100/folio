//! The Folio compiler library: the memoised compile pipeline
//! ([`compile`]) and the [`World`] that supplies sources, fonts, files and
//! components (SPEC §2.2).
//!
//! A host builds a [`World`] from the project's manifest and lockfile, sets
//! the sources and files it reads from disk, and compiles a variant for a
//! target, or only checks it ([`check`]): every stage but Emit, with the
//! same diagnostics and no output. Every stage is a memoised query (SPEC
//! §2.3): after an edit, compiling again re-runs only the queries the edit
//! affects, and stops early where a re-run query returns what it returned
//! before. An edit cancels compilations still running on other handles of
//! the world.
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

use std::any::Any;
use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};
use std::panic::{self, AssertUnwindSafe};

use ecow::{EcoString, EcoVec, eco_format};
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
pub use folio_core::locate;
pub use folio_core::model;
pub use folio_core::project::{Date, Lockfile, Manifest, OutputPattern, ProjectError, ProjectPath};
pub use folio_core::resolve::{
    Labelled, Marker, Number, Reference, Resolution, Target as ResolveTarget,
};
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
///
/// A panic in a query, a bug in the compiler, unwinds out of it; in a
/// long-lived world ([`WorldBuilder::long_lived`]) it is instead an error
/// with the code `internal-error`, which stands in place of the build's
/// diagnostics, with the last output the build was compiled to, if any.
/// Its sources and files are then what a watcher should watch: those the
/// build last read, and every other source and file of the world.
pub fn compile(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Compiled, Cancelled> {
    let Some(kept) = world.kept() else {
        return catch(world, || compile_now(world, target, variant));
    };
    let build = (variant.cloned(), target);
    match isolate(world, || compile_now(world, target, variant))? {
        Ok(compiled) => {
            kept.update(build, &world.builds(), |last| {
                last.sources = compiled.sources.clone();
                last.files = compiled.files.clone();
                if compiled.output.is_some() {
                    last.output = compiled.output.clone();
                }
            });
            Ok(compiled)
        }
        Err(panic) => {
            let last = kept.get(&build);
            let kept_output = last.output.is_some();
            let (sources, files) = watched(world, last.sources, last.files);
            Ok(Compiled {
                sources,
                files,
                output: last.output,
                diagnostics: EcoVec::from([internal_error(
                    &panic,
                    kept_output.then_some("the last output this build was compiled to is kept"),
                )]),
            })
        }
    }
}

/// Compiles a build, letting panics unwind.
fn compile_now(world: &World, target: Target, variant: Option<&Name>) -> Compiled {
    let db = world.db();
    let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
    let built = pipeline::compile_build(db, build);
    let mut diagnostics = collect(pipeline::compile_build::accumulated::<Reported>(db, build));
    diagnostics.extend(pairing(world, variant));
    let mut read = pipeline::compile_build::accumulated::<Read>(db, build);
    read.extend(pairing_reads(world, variant));
    let files = files(read);
    Compiled {
        sources: built.sources.clone(),
        files,
        output: built.output.clone(),
        diagnostics,
    }
}

/// Checks a variant of the world's document for a target without
/// emitting it: runs every stage of SPEC §2.1 up to Emit, lays out the
/// pages of a fixed-layout target, and for PDF runs the writer's check of
/// the pages, but writes nothing. The variant is `None` only for a
/// project that declares no variants.
///
/// The diagnostics are those that [`compile`] reports for the build, but
/// for the few that only writing the file finds: an error of the writer
/// itself, such as a font it cannot subset, and a reason for a PDF not to
/// claim PDF/UA-1 that the writer's check does not foresee. After an edit,
/// checking again re-runs only what the edit affects; compiling the same
/// build afterwards reuses the check and only emits. Fails only when an
/// edit to the world cancels the check.
///
/// A panic in a query unwinds out of it, or in a long-lived world is an
/// error with the code `internal-error`, as [`compile`] says.
pub fn check(world: &World, target: Target, variant: Option<&Name>) -> Result<Checked, Cancelled> {
    let Some(kept) = world.kept() else {
        return catch(world, || check_now(world, target, variant));
    };
    let build = (variant.cloned(), target);
    match isolate(world, || check_now(world, target, variant))? {
        Ok(checked) => {
            kept.update(build, &world.builds(), |last| {
                last.sources = checked.sources.clone();
                last.files = checked.files.clone();
            });
            Ok(checked)
        }
        Err(panic) => {
            let last = kept.get(&build);
            // Compiling now would panic alike, and answer with the kept
            // output.
            let writes = last.output.is_some();
            let (sources, files) = watched(world, last.sources, last.files);
            Ok(Checked {
                sources,
                files,
                writes,
                diagnostics: EcoVec::from([internal_error(
                    &panic,
                    writes.then_some(
                        "compiling this build answers with the last output it was compiled to",
                    ),
                )]),
            })
        }
    }
}

/// Checks a variant of the world's document for a target, as [`check`]
/// does, and keeps the diagnostics that `layout` shows.
pub fn check_with(
    world: &World,
    target: Target,
    variant: Option<&Name>,
    layout: LayoutDiagnostics,
) -> Result<Checked, Cancelled> {
    let mut checked = check(world, target, variant)?;
    checked.diagnostics = layout.filter(checked.diagnostics);
    Ok(checked)
}

/// Which layout diagnostics a front end shows (SPEC §7.9).
///
/// [`compile`] and [`check`] report every diagnostic. Content that runs
/// off its line or page (`overfull-line`, `page-overflow`) is always
/// shown; underfull lines and relaxed widow, orphan, keep and unbreakable
/// constraints ([`layout::DETAIL_CODES`]) only when asked for, as `folio
/// check --layout` asks. `folio build` and plain `folio check` show
/// [`LayoutDiagnostics::Serious`]; the language server and the preview
/// apply the same rule through [`check_with`] or
/// [`LayoutDiagnostics::filter`].
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum LayoutDiagnostics {
    /// Only the layout diagnostics of content that runs off its line or
    /// page; every other diagnostic is kept.
    #[default]
    Serious,
    /// Every diagnostic.
    All,
}

impl LayoutDiagnostics {
    /// Whether a diagnostic is shown.
    pub fn shows(self, diagnostic: &Diagnostic) -> bool {
        self == Self::All
            || !diagnostic
                .code
                .as_deref()
                .is_some_and(|code| layout::DETAIL_CODES.contains(&code))
    }

    /// The diagnostics that are shown, in their order.
    pub fn filter(self, diagnostics: EcoVec<Diagnostic>) -> EcoVec<Diagnostic> {
        if diagnostics.iter().all(|d| self.shows(d)) {
            return diagnostics;
        }
        diagnostics.into_iter().filter(|d| self.shows(d)).collect()
    }
}

/// Checks a build, letting panics unwind.
fn check_now(world: &World, target: Target, variant: Option<&Name>) -> Checked {
    let db = world.db();
    let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
    let checked = pipeline::check_build(db, build);
    let mut diagnostics = collect(pipeline::check_build::accumulated::<Reported>(db, build));
    diagnostics.extend(pairing(world, variant));
    Checked {
        sources: checked.sources.clone(),
        files: files({
            let mut read = pipeline::check_build::accumulated::<Read>(db, build);
            read.extend(pairing_reads(world, variant));
            read
        }),
        writes: checked.writes,
        diagnostics,
    }
}

/// The problems of pairing a variant's instances with those of the other
/// variants of its numbering group (SPEC §6.1). The check reads the other
/// variants' documents, so it runs beside the build, not in it: what
/// evaluating them reports belongs to their own builds.
fn pairing(world: &World, variant: Option<&Name>) -> EcoVec<Diagnostic> {
    folio_core::variants::check_pairing(world.db(), world.project(), variant.cloned()).clone()
}

/// The files the pairing check read for the other variants of the group,
/// so that a watcher checks the build again when one of them changes.
fn pairing_reads<'a>(world: &'a World, variant: Option<&Name>) -> Vec<&'a Read> {
    folio_core::variants::check_pairing::accumulated::<Read>(
        world.db(),
        world.project(),
        variant.cloned(),
    )
}

/// The diagnostics a query and the queries it depends on reported, each
/// once.
fn collect(reported: Vec<&Reported>) -> EcoVec<Diagnostic> {
    // A diagnostic that two queries report alike, such as a missing
    // imported file that both the file search and evaluation find, is kept
    // once.
    let mut seen = HashSet::new();
    reported
        .into_iter()
        .filter(|reported| seen.insert(&reported.0))
        .map(|reported| reported.0.clone())
        .collect()
}

/// The files a query and the queries it depends on read, each once: a
/// file read twice, or read as a source and as data, is listed where it
/// was first read.
fn files(read: Vec<&Read>) -> EcoVec<FileRead> {
    let mut seen = HashSet::new();
    read.into_iter()
        .filter(|r| seen.insert(&r.0.path))
        .map(|r| r.0.clone())
        .collect()
}

/// What checking a build finds ([`check`]): its diagnostics, without
/// output.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Checked {
    /// The source files the build read, in the order they were found. A
    /// watcher checks again when one of them changes.
    pub sources: EcoVec<ProjectPath>,
    /// Every file the build read or tried to read, with how it read it, as
    /// [`Compiled::files`] lists them.
    pub files: EcoVec<FileRead>,
    /// Whether compiling the build would give a file
    /// ([`Compiled::output`]): its target has a writer, which only PDF has
    /// yet, and the document could be styled. An error of the writer
    /// itself may still leave it without one.
    pub writes: bool,
    /// Every problem found, errors and warnings, in the order the queries
    /// that reported them first ran.
    pub diagnostics: EcoVec<Diagnostic>,
}

impl Checked {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
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
    catch(world, || {
        eval::evaluate(world.db(), world.project(), variant.cloned()).clone()
    })
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
    catch(world, || {
        model::document(world.db(), world.project(), variant.cloned()).clone()
    })
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
    catch(world, || {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        style::styles(db, build).clone()
    })
}

/// Resolves the counters, markers, labels and cross-references of a variant
/// of the world's document for a target (SPEC §2.1, §4.5): each numbered
/// element's values and number, the marker set before its content, what
/// each label names, and the text each `@label` shows; or `None` when the
/// document could not be styled. Everything is keyed by location. The
/// variant is `None` only for a project that declares no variants.
///
/// The problems found (references that do not resolve, counters that do
/// not exist) are diagnostics that [`compile`] reports for the build. Fails
/// only when an edit to the world cancels it.
pub fn resolve(
    world: &World,
    target: Target,
    variant: Option<&Name>,
) -> Result<Option<Resolution>, Cancelled> {
    catch(world, || {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        folio_core::pipeline::resolution(db, build).cloned()
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
    catch(world, || {
        folio_core::fonts::fonts(world.db(), world.project()).clone()
    })
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
    catch(world, || {
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
    catch(world, || {
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
    catch(world, || {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        layout::pages(db, build).clone()
    })
}

/// Lays out the pages of a variant of the world's document for a target
/// through the page with index `last`, or all of them when there are no
/// more ([`layout::pages_through`]): what the preview shows first (SPEC
/// §12.2). Each page is the page [`pages`] gives, and [`pages`] finds the
/// pages laid out here in the page cache afterwards. `None` when the
/// document could not be styled, or, in a long-lived world
/// ([`WorldBuilder::long_lived`]), when a query panicked, which
/// [`check`] then reports. It reports no diagnostics. Fails only when an
/// edit to the world cancels it.
pub fn pages_through(
    world: &World,
    target: Target,
    variant: Option<&Name>,
    last: usize,
) -> Result<Option<layout::FirstPages>, Cancelled> {
    let run = || {
        let db = world.db();
        let build = folio_core::db::Build::new(db, world.project(), target, variant.cloned());
        layout::pages_through(db, build, last)
    };
    match world.kept() {
        None => catch(world, run),
        Some(_) => Ok(isolate(world, run)?.ok().flatten()),
    }
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
    run_builds(world, builds, compile)
}

/// Checks builds of the world's document without emitting them
/// ([`check`]), as [`compile_builds`] compiles them: in parallel, each on
/// its own handle of the world, the results in the order of the builds.
pub fn check_builds(
    world: &World,
    builds: impl IntoIterator<Item = (Option<Name>, Target)>,
) -> Vec<Output<Checked>> {
    run_builds(world, builds, check)
}

/// Runs `run` on builds in parallel, each on its own handle of the world.
fn run_builds<T: Send>(
    world: &World,
    builds: impl IntoIterator<Item = (Option<Name>, Target)>,
    run: fn(&World, Target, Option<&Name>) -> Result<T, Cancelled>,
) -> Vec<Output<T>> {
    // A handle is not `Sync`, so each build takes its own.
    let builds: Vec<_> = builds
        .into_iter()
        .map(|build| (world.clone(), build))
        .collect();
    builds
        .into_par_iter()
        .map(|(world, (variant, target))| {
            let result = run(&world, target, variant.as_ref());
            Output {
                variant,
                target,
                result,
            }
        })
        .collect()
}

/// The result of one build of [`compile_all`] or [`compile_builds`]
/// (what it compiled to), or of [`check_builds`] (what checking it found).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Output<T = Compiled> {
    /// The variant, or `None` for a project without variants.
    pub variant: Option<Name>,
    /// The target.
    pub target: Target,
    /// What compiling or checking it produced.
    pub result: Result<T, Cancelled>,
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
/// A panic in a query unwinds on.
fn catch<T>(world: &World, f: impl FnOnce() -> T) -> Result<T, Cancelled> {
    salsa::Cancelled::catch(AssertUnwindSafe(f)).map_err(|cancelled| match cancelled {
        salsa::Cancelled::PropagatedPanic if !cancelled_by_edit(world) => {
            panic!("a query panicked earlier or on another thread")
        }
        _ => Cancelled,
    })
}

/// Whether an edit has cancelled the world's revision.
///
/// A query that an edit stops on one thread gives up its claim as if it
/// had panicked, so the queries on other threads that wait for it see a
/// panic on another thread: this tells the two apart. The edit waits for
/// this handle, so the revision stays cancelled while it is asked. A real
/// panic that an edit happens to follow is taken for a cancellation; the
/// next compilation meets it again.
fn cancelled_by_edit(world: &World) -> bool {
    salsa::Cancelled::catch(AssertUnwindSafe(|| {
        salsa::Database::unwind_if_revision_cancelled(world.db())
    }))
    .is_err()
}

/// Runs queries for a long-lived world, turning salsa's cancellation
/// unwind into [`Cancelled`] and a panic in a query, on this thread or
/// another, into what it said.
fn isolate<T>(world: &World, f: impl FnOnce() -> T) -> Result<Result<T, EcoString>, Cancelled> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Ok(Ok(value)),
        Err(payload) => match payload.downcast::<salsa::Cancelled>() {
            Ok(cancelled) => match *cancelled {
                salsa::Cancelled::PropagatedPanic if !cancelled_by_edit(world) => {
                    Ok(Err("a query panicked earlier or on another thread".into()))
                }
                _ => Err(Cancelled),
            },
            Err(payload) => Ok(Err(panic_message(&*payload))),
        },
    }
}

/// What a panic said, when it said it with a string.
fn panic_message(payload: &(dyn Any + Send)) -> EcoString {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).into()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str().into()
    } else {
        "a query panicked".into()
    }
}

/// What a watcher watches after a build panicked: the sources and files
/// that its last compilation or check that did not panic read, and then
/// every other source and file of the world, since the build that panicked
/// may have read more (SPEC §12.2). The kind of a file that the build is
/// not known to have read is told by its extension. A file that the build
/// that panicked named but the world lacks is not listed: a watcher that
/// needs it watches the project's directory. `folio.toml`, `folio.lock`
/// and the fonts are not listed either; a watcher always watches them.
fn watched(
    world: &World,
    mut sources: EcoVec<ProjectPath>,
    mut files: EcoVec<FileRead>,
) -> (EcoVec<ProjectPath>, EcoVec<FileRead>) {
    let mut seen: HashSet<ProjectPath> = files.iter().map(|f| f.path.clone()).collect();
    let listed: HashSet<ProjectPath> = sources.iter().cloned().collect();
    for path in world.source_paths() {
        if !listed.contains(path) {
            sources.push(path.clone());
        }
        if seen.insert(path.clone()) {
            let kind = match extension(path).as_deref() {
                Some("fsty") => FileKind::Stylesheet,
                _ => FileKind::Content,
            };
            files.push(FileRead {
                path: path.clone(),
                kind,
            });
        }
    }
    for path in world.file_paths() {
        if seen.insert(path.clone()) {
            let kind = match extension(path).as_deref() {
                Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg") => FileKind::Image,
                _ => FileKind::Data,
            };
            files.push(FileRead {
                path: path.clone(),
                kind,
            });
        }
    }
    (sources, files)
}

/// A path's extension, in lower case.
fn extension(path: &ProjectPath) -> Option<String> {
    let (_, extension) = path.as_str().rsplit_once('.')?;
    (!extension.contains('/')).then(|| extension.to_ascii_lowercase())
}

/// The error that stands for a panic in a query in a long-lived world: a
/// bug in the compiler, not in the document, so it points at no source.
/// `kept` is a hint on the output kept, when there is one.
fn internal_error(message: &str, kept: Option<&str>) -> Diagnostic {
    let mut error = Diagnostic::error(
        folio_syntax::DiagSpan::detached(),
        eco_format!("internal error: the compiler panicked: {message}"),
    );
    error.code = Some("internal-error".into());
    error.hints.push(
        "this is a bug in Folio, not in the document; editing the document may work around it"
            .into(),
    );
    error.hints.extend(kept.map(EcoString::from));
    error
}
