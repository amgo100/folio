//! The query database every stage runs in (SPEC §2.3).
//!
//! Compilation is a set of memoised queries in a demand-driven database built
//! on `salsa`. Queries record which inputs they read, so an edit re-runs only
//! the queries whose inputs changed, and a query that re-runs and returns an
//! equal value does not re-run the queries that depend on it (early cutoff).
//! A write to an input cancels the queries still running for the previous
//! revision.
//!
//! # Inputs
//!
//! - A [`Project`] holds what a build reads besides source text: the manifest
//!   and lockfile, the map from paths to files, fonts and the component
//!   registry, and how the build runs ([`BuildMode`]). Each field is tracked
//!   on its own, with a durability that says how often it changes: bundled
//!   fonts and the components compiled into the binary (the first-party
//!   ones, SPEC §2.2) are [`Durability::HIGH`], so after an edit the queries
//!   that depend only on them are validated without walking their
//!   dependencies; the manifest, lockfile, project and system fonts are
//!   [`Durability::MEDIUM`]; sources and data files are [`Durability::LOW`].
//! - A [`SourceFile`] holds one `.fol` or `.fsty` file as a parsed
//!   [`Source`]. The host edits it in place ([`Source::edit`]), which reparses
//!   only the smallest enclosing block, and sets the result.
//! - A [`DataFile`] holds the contents of an image or data file: bytes the
//!   host set, or a file on disk that is read the first time a build names
//!   it ([`FileContent`], SPEC §12.1).
//!
//! The variant and the target are not settable inputs but keys: a [`Build`]
//! interns a project with a target and an optional variant, and the
//! per-variant queries take it as their argument. A query that does not read
//! the variant therefore runs once for all variants, and every variant of a
//! revision can be compiled at the same time on its own database handle
//! (SPEC §2.1).
//!
//! # Diagnostics
//!
//! Queries report problems as they find them ([`diag::report`]) rather than
//! returning them, so a query whose result is unchanged after an edit is cut
//! off even when its diagnostics changed. The `folio` crate's compile query
//! runs a build through every stage (see [`crate::pipeline`]) and collects
//! the diagnostics of every query the build depends on.

// salsa generates the constructors, builders and setters of inputs and
// interned structs.
#![allow(missing_docs)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::{self, Debug, Formatter};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Registry, Target};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{DiagSpan, Source, Span, SyntaxKind, SyntaxNode};
pub use salsa::Durability;

use crate::component::ComponentHost;
use crate::diag::{self, Diagnostic};
use crate::eval::Limits;
use crate::project::{Date, Lockfile, Manifest, Name, ProjectPath, Spanned};
pub use folio_fonts::{FontFile, SystemFont};

/// The database that the compile queries run in. The `folio` crate provides
/// the implementation; each stage extends it when it moves into its own
/// crate.
#[salsa::db]
pub trait Db: salsa::Database {
    /// The memoised shaped paragraphs and shaping fonts that every handle of
    /// the database shares (SPEC §2.3). Shaping is a pure function of a
    /// paragraph, so the cache holds no state that queries could observe
    /// besides its results.
    fn text_cache(&self) -> &crate::text::Cache;

    /// The memoised line breaks of paragraphs that every handle of the
    /// database shares (SPEC §2.3). Like shaping, breaking is a pure
    /// function of its key.
    fn layout_cache(&self) -> &crate::layout::Cache;

    /// The page cache (SPEC §2.3), shared by every handle of the database
    /// like the line-break cache.
    fn page_cache(&self) -> &crate::layout::PageCache;

    /// What the world has read of the system faces its documents use,
    /// shared by every handle of the database: each face is read at most
    /// once, so that the queries of a build agree on it.
    fn system_faces(&self) -> &crate::fonts::SystemFaces;
}

/// What a build reads besides source text, one field per kind of input.
#[salsa::input(debug)]
pub struct Project {
    /// `folio.toml`. [`Durability::MEDIUM`].
    #[returns(ref)]
    pub manifest: Manifest,
    /// `folio.lock`, or an empty lockfile when there is none.
    /// [`Durability::MEDIUM`].
    #[returns(ref)]
    pub lockfile: Lockfile,
    /// The project's `.fol` and `.fsty` files by path. Changes only when a
    /// file is added or removed. [`Durability::LOW`].
    #[returns(ref)]
    pub sources: BTreeMap<ProjectPath, SourceFile>,
    /// The project's other files (images, data) by path.
    /// [`Durability::LOW`].
    #[returns(ref)]
    pub files: BTreeMap<ProjectPath, DataFile>,
    /// The fonts bundled with the compiler. [`Durability::HIGH`].
    #[returns(ref)]
    pub bundled_fonts: EcoVec<FontFile>,
    /// The fonts of the project's font directory and those the font cache
    /// has for `folio.lock`. [`Durability::MEDIUM`].
    #[returns(ref)]
    pub fonts: EcoVec<FontFile>,
    /// The faces installed on the machine, which are read only when a
    /// document uses them. [`Durability::MEDIUM`].
    #[returns(ref)]
    pub system_fonts: EcoVec<SystemFont>,
    /// How the build runs. [`Durability::MEDIUM`].
    pub mode: BuildMode,
    /// The components the compiler hosts. [`Durability::HIGH`].
    #[returns(ref)]
    pub components: Registry,
    /// The date scripts see as `today` when the host sets one, as a command
    /// line does; otherwise the manifest's `project.today` (SPEC §4.4).
    /// [`Durability::MEDIUM`].
    pub today: Option<Date>,
    /// The limits of evaluation. [`Durability::HIGH`].
    pub limits: Limits,
}

/// A content (`.fol`) or stylesheet (`.fsty`) file.
#[salsa::input(debug)]
pub struct SourceFile {
    /// Where the file is in the project. Never changes.
    #[returns(ref)]
    pub path: ProjectPath,
    /// The file's text and syntax tree. [`Durability::LOW`].
    #[returns(ref)]
    pub source: Source,
}

/// An image or data file.
#[salsa::input(debug)]
pub struct DataFile {
    /// Where the file is in the project. Never changes.
    #[returns(ref)]
    pub path: ProjectPath,
    /// The file's contents, read on demand when they are on disk.
    /// [`Durability::LOW`].
    #[returns(ref)]
    pub content: FileContent,
}

impl DataFile {
    /// The file's contents, read from disk if no query has read them yet;
    /// empty when they cannot be read, which [`data_file`] reports.
    pub fn bytes(self, db: &dyn Db) -> &Bytes {
        static EMPTY: LazyLock<Bytes> = LazyLock::new(|| Bytes::from(Vec::new()));
        self.content(db).load().unwrap_or(&EMPTY)
    }
}

/// The contents of an image or data file: bytes the host set, or a file on
/// disk, read the first time a query asks for it and then kept, so that the
/// queries of a revision agree on it (SPEC §12.1). A file that changes on
/// disk is seen when the host sets a new content, as a watcher does on a
/// file-system event. Cheap to clone: clones share what was read.
#[derive(Clone)]
pub struct FileContent(Arc<Content>);

enum Content {
    Bytes(Bytes),
    Disk {
        path: PathBuf,
        /// The directory the file must still be in, by canonical path,
        /// when it is read.
        within: Option<PathBuf>,
        read: OnceLock<Result<Bytes, EcoString>>,
    },
}

impl FileContent {
    /// Contents the host has in memory.
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self(Arc::new(Content::Bytes(bytes.into())))
    }

    /// The contents of a file on disk, not read until a query asks for
    /// them.
    pub fn on_disk(path: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Content::Disk {
            path: path.into(),
            within: None,
            read: OnceLock::new(),
        }))
    }

    /// The contents of a file on disk, not read until a query asks for
    /// them, and then read only if the file, links resolved, is still
    /// inside `root`, a canonical path: a file of the project that was
    /// replaced by a link to somewhere else meanwhile is not read (SPEC
    /// §12.1, §14.3).
    pub fn in_project(path: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Content::Disk {
            path: path.into(),
            within: Some(root.into()),
            read: OnceLock::new(),
        }))
    }

    /// The bytes, read from disk on the first call; `None` when the file
    /// cannot be read.
    pub fn load(&self) -> Option<&Bytes> {
        self.try_load().ok()
    }

    /// The bytes, read from disk on the first call, or why the file cannot
    /// be read.
    pub fn try_load(&self) -> Result<&Bytes, &EcoString> {
        match &*self.0 {
            Content::Bytes(bytes) => Ok(bytes),
            Content::Disk { path, within, read } => read
                .get_or_init(|| {
                    if let Some(root) = within {
                        let canonical = path.canonicalize().map_err(|e| eco_format!("{e}"))?;
                        if !canonical.starts_with(root) {
                            return Err("it leads outside the project".into());
                        }
                    }
                    std::fs::read(path)
                        .map(Bytes::from)
                        .map_err(|e| eco_format!("{e}"))
                })
                .as_ref(),
        }
    }

    /// What reading gave, without reading: `None` when the contents are on
    /// disk and nothing has asked for them yet.
    pub fn loaded(&self) -> Option<Result<&Bytes, &EcoString>> {
        match &*self.0 {
            Content::Bytes(bytes) => Some(Ok(bytes)),
            Content::Disk { read, .. } => read.get().map(Result::as_ref),
        }
    }

    /// The file on disk the contents come from, if they do.
    pub fn disk_path(&self) -> Option<&PathBuf> {
        match &*self.0 {
            Content::Bytes(_) => None,
            Content::Disk { path, .. } => Some(path),
        }
    }
}

impl Debug for FileContent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            Content::Bytes(bytes) => bytes.fmt(f),
            Content::Disk { path, read, .. } => match read.get() {
                Some(Ok(bytes)) => write!(f, "{bytes:?} from {}", path.display()),
                Some(Err(e)) => write!(f, "unreadable {}: {e}", path.display()),
                None => write!(f, "unread {}", path.display()),
            },
        }
    }
}

/// One output of a project: its target, and its variant, or none when the
/// manifest declares no variants.
///
/// A build may also be a layout pass of the page-number fixpoint (SPEC
/// §2.3): the same output, with its page-number references set to the
/// page numbers of the pass before ([`crate::resolve::page_refs`]). The
/// build itself is made with [`Build::new`] and has no pass.
#[salsa::interned(debug, constructor = with_pass)]
pub struct Build<'db> {
    #[returns(copy)]
    pub project: Project,
    #[returns(copy)]
    pub target: Target,
    #[returns(ref)]
    pub variant: Option<Name>,
    /// For a layout pass, the page numbers it sets its page-number
    /// references with; `None` for the build itself.
    #[returns(ref)]
    pub pass: Option<crate::resolve::PageNumbers>,
}

impl<'db> Build<'db> {
    /// The build of a project's output for a target and a variant.
    pub fn new(db: &'db dyn Db, project: Project, target: Target, variant: Option<Name>) -> Self {
        Self::with_pass(db, project, target, variant, None)
    }

    /// The build that a layout pass is a pass of, or the build itself.
    pub fn base(self, db: &'db dyn Db) -> Self {
        if self.pass(db).is_none() {
            return self;
        }
        Self::new(
            db,
            self.project(db),
            self.target(db),
            self.variant(db).clone(),
        )
    }

    /// Whether the build is a layout pass of the page-number fixpoint,
    /// whose diagnostics are those of the build it is a pass of. The stages
    /// whose diagnostics depend on the text of page-number references, the
    /// paragraph and page builders, report nothing for a pass.
    pub fn is_pass(self, db: &'db dyn Db) -> bool {
        self.pass(db).is_some()
    }
}

/// The contents of a file, cheap to clone.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Bytes(Arc<[u8]>);

impl Bytes {
    /// The bytes, shared rather than copied.
    pub fn shared(&self) -> Arc<[u8]> {
        self.0.clone()
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl From<&[u8]> for Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }
}

impl Debug for Bytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({})", self.0.len())
    }
}

/// How the host runs a build, which decides what an unpinned input costs:
/// a system font that `folio.lock` does not lock is allowed while
/// watching, a warning in a build, and an error in a CI build or with
/// strict formatting (SPEC §2.4).
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum BuildMode {
    /// Watch mode and previews: output is not kept.
    Watch,
    /// A build that writes output.
    #[default]
    Build,
    /// A build on a CI server, which must be reproducible.
    Ci,
}

/// A file that a source names literally, by an import, an include or a
/// stylesheet `@import`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Dependency {
    /// The file, resolved against the importing file's directory.
    pub path: ProjectPath,
    /// The path's string in the importing file.
    pub span: Span,
}

/// What the build query ([`build_output`]) produces: what a build reads,
/// checked. The stages that turn it into output are queries of their own
/// (see [`crate::pipeline`]).
///
/// Every file a build reads, sources and the data files and images that
/// evaluation reads too, is reported as a [`Read`] by the query that reads
/// it; collect them with `accumulated::<Read>` on the query that compiles
/// the build.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct BuildOutput {
    /// The source files the build read, in the order they were found. A
    /// watcher recompiles when one of them changes.
    pub sources: EcoVec<ProjectPath>,
}

/// How a build reads a file.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FileKind {
    /// A content file (`.fol`): the entry file, or one imported or
    /// included.
    Content,
    /// A stylesheet (`.fsty`): the project's, a variant's, or one imported.
    Stylesheet,
    /// A file read as data or text, by `read`, `json`, `csv` or `toml`.
    Data,
    /// An image, named by an `image` element.
    Image,
}

/// A file that a build read, or tried to read: a file named but missing is
/// listed too, so that a watcher rebuilds when it appears.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FileRead {
    /// Where the file is in the project.
    pub path: ProjectPath,
    /// How the build reads it.
    pub kind: FileKind,
}

/// A file read as reported by a query: the salsa accumulator that
/// [`report_read`] adds to. Like diagnostics, reads are stored with the
/// memo of the query that made them, so a query that is not re-run keeps
/// them.
#[salsa::accumulator]
#[derive(Debug)]
pub struct Read(pub FileRead);

/// Reports that the running query read, or tried to read, a file.
///
/// # Panics
///
/// Panics when no query is running.
pub fn report_read(db: &dyn Db, path: ProjectPath, kind: FileKind) {
    use salsa::Accumulator;
    Read(FileRead { path, kind }).accumulate(db);
}

/// The source file at a path, if the project has one.
///
/// Re-runs when a file is added or removed, but returns the same file for
/// paths that still exist, so queries that look files up are cut off.
#[salsa::tracked(returns(copy))]
pub fn source_file(db: &dyn Db, project: Project, path: ProjectPath) -> Option<SourceFile> {
    project.sources(db).get(&path).copied()
}

/// The data file at a path, if the project has one and it can be read.
/// Reads it if no query has yet (SPEC §12.1), and reports a file that
/// cannot be read, which is then not found.
#[salsa::tracked(returns(copy))]
pub fn data_file(db: &dyn Db, project: Project, path: ProjectPath) -> Option<DataFile> {
    let file = project.files(db).get(&path).copied()?;
    if let Err(why) = file.content(db).try_load() {
        diag::report(
            db,
            Diagnostic::error(
                DiagSpan::detached(),
                eco_format!("cannot read `{}`: {why}", path.as_str()),
            ),
        );
        return None;
    }
    Some(file)
}

/// The files a source names literally: string paths of `#import` and
/// `#include` in content, and of `@import` in stylesheets. These are all the
/// files it imports or includes, because evaluation accepts only literal
/// paths there (SPEC §4.4); packages (`@scope/name`) resolve through the
/// lockfile.
///
/// Spans are numbers in the syntax tree, not offsets, and an incremental
/// reparse renumbers only the block it reparses. An edit outside the blocks
/// that hold imports therefore gives an equal result, and the queries that
/// depend on it are not re-run.
#[salsa::tracked(returns(ref))]
pub fn dependencies(db: &dyn Db, file: SourceFile) -> EcoVec<Dependency> {
    let path = file.path(db);
    let mut deps = EcoVec::new();
    for_each_import(file.source(db).root(), &mut |text, span| {
        if let Some(path) = path.join(text) {
            deps.push(Dependency { path, span });
        }
    });
    deps
}

/// Reports the syntax errors and warnings of a source, and the literal
/// import paths in it that leave the project.
///
/// A language server collects them for one file with
/// `check_source::accumulated::<Reported>(db, file)`.
#[salsa::tracked]
pub fn check_source(db: &dyn Db, file: SourceFile) {
    let path = file.path(db);
    let root = file.source(db).root();
    let (errors, warnings) = root.errors_and_warnings();
    for diag in errors.into_iter().chain(warnings) {
        diag::report(db, diag.into());
    }
    for_each_import(root, &mut |text, span| {
        if path.join(text).is_none() {
            diag::report(
                db,
                Diagnostic::error(span, eco_format!("`{text}` is not a path in the project"))
                    .with_hint("Folio reads files only inside the project directory"),
            );
        }
    });
}

/// Calls `f` with the text and span of every literal import path in a tree.
fn for_each_import(node: &SyntaxNode, f: &mut impl FnMut(&str, Span)) {
    let source = match node.kind() {
        SyntaxKind::ModuleImport => node.cast::<ast::ModuleImport<'_>>().map(|i| i.source()),
        SyntaxKind::ModuleInclude => node.cast::<ast::ModuleInclude<'_>>().map(|i| i.source()),
        SyntaxKind::StyleImport => node
            .cast::<ast::StyleImport<'_>>()
            .and_then(|i| i.path())
            .map(ast::Expr::Str),
        _ => None,
    };
    if let Some(ast::Expr::Str(path)) = source {
        // `@scope/name` is a package, which the lockfile resolves.
        let text = path.get();
        if !text.starts_with('@') {
            f(&text, path.span());
        }
        return;
    }
    for child in node.children() {
        for_each_import(child, f);
    }
}

/// The files of a build, found from its entry file and stylesheets (the
/// project's, then the variant's overlays), and every file they name
/// literally, in the order they were found. Reports the files that are named
/// but not in the project.
#[salsa::tracked(returns(ref))]
pub fn build_files(db: &dyn Db, build: Build<'_>) -> EcoVec<SourceFile> {
    let project = build.project(db);
    let manifest = project.manifest(db);
    let mut roots: Vec<&Spanned<ProjectPath>> = vec![&manifest.project.entry];
    roots.extend(&manifest.project.styles);
    if let Some(variant) = declared_variant(manifest, build.variant(db).as_ref()) {
        roots.extend(&variant.styles);
    }

    let mut out = EcoVec::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(ProjectPath, DiagSpan)> =
        roots.iter().map(|r| (r.v.clone(), r.span)).collect();
    while let Some((path, span)) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let kind = match path.extension() {
            Some("fsty") => FileKind::Stylesheet,
            _ => FileKind::Content,
        };
        report_read(db, path.clone(), kind);
        let Some(file) = source_file(db, project, path.clone()) else {
            diag::report(
                db,
                Diagnostic::error(span, eco_format!("file `{}` not found", path.as_str()))
                    .with_hint("Folio reads `.fol` and `.fsty` files inside the project"),
            );
            continue;
        };
        out.push(file);
        for dep in dependencies(db, file) {
            queue.push_back((dep.path.clone(), dep.span.into()));
        }
    }
    out
}

/// The components a project uses, with their templates parsed and names
/// resolved through the lockfile's bindings, or `None` after reporting why
/// they cannot be hosted. A declared component that is not registered is
/// reported as an error ([`crate::component::unhosted`]) while the others
/// are still hosted, so `Some` does not mean that nothing was reported.
///
/// Reads the registry (durable), the manifest and lockfile, and the element
/// declarations of the project's content files, which an edit outside a
/// declaration leaves equal, so such an edit does not re-run it.
#[salsa::tracked(returns(ref))]
pub fn component_host(db: &dyn Db, project: Project) -> Option<ComponentHost> {
    for error in
        crate::component::unhosted(project.components(db), &project.manifest(db).components)
    {
        diag::report(db, error);
    }
    let host = ComponentHost::new(
        project.components(db),
        &project.manifest(db).components,
        &project.lockfile(db).bindings,
    );
    match host {
        Ok(mut host) => {
            // The elements the project declares (SPEC §4.3).
            let elements = crate::eval::project_component(db, project);
            if let Some(error) = host.with_project(elements.as_ref()) {
                diag::report(db, error);
            }
            Some(host)
        }
        Err(errors) => {
            for error in errors {
                diag::report(db, error.into());
            }
            None
        }
    }
}

/// Checks what one variant for one target reads: that the manifest declares
/// the variant for the target, the build's files and their syntax, the
/// components, and the fonts its text is set in. Reports the problems it
/// finds.
#[salsa::tracked(returns(ref))]
pub fn build_output(db: &dyn Db, build: Build<'_>) -> BuildOutput {
    let project = build.project(db);
    for diagnostic in check_build(project.manifest(db), build.target(db), build.variant(db)) {
        diag::report(db, diagnostic);
    }
    for diagnostic in project
        .manifest(db)
        .not_implemented(build.variant(db).as_ref())
    {
        diag::report(db, diagnostic);
    }

    let files = build_files(db, build);
    for &file in files {
        check_source(db, file);
    }
    component_host(db, project);
    crate::fonts::check_fonts(db, build);
    crate::lint::check_lints(db, build);

    BuildOutput {
        sources: files.iter().map(|f| f.path(db).clone()).collect(),
    }
}

/// The variant of a build, if the manifest declares it.
fn declared_variant<'a>(
    manifest: &'a Manifest,
    variant: Option<&Name>,
) -> Option<&'a crate::project::Variant> {
    let variant = variant?;
    manifest.variants.iter().find(|v| &v.name.v == variant)
}

/// Checks that the manifest declares a build's variant, and that the variant
/// is built for the target.
fn check_build(manifest: &Manifest, target: Target, variant: &Option<Name>) -> EcoVec<Diagnostic> {
    let declared = || -> EcoString {
        let names: Vec<String> = manifest
            .variants
            .iter()
            .map(|v| format!("`{}`", v.name.v))
            .collect();
        eco_format!("the manifest declares {}", names.join(", "))
    };
    let Some(name) = variant else {
        if manifest.variants.is_empty() {
            return EcoVec::new();
        }
        return EcoVec::from([Diagnostic::error(
            DiagSpan::detached(),
            "no variant was chosen, but the manifest declares variants",
        )
        .with_hint(declared())]);
    };
    let Some(decl) = declared_variant(manifest, Some(name)) else {
        let mut diag = Diagnostic::error(
            DiagSpan::detached(),
            eco_format!("no variant `{name}` is declared"),
        );
        if !manifest.variants.is_empty() {
            diag = diag.with_hint(declared());
        }
        return EcoVec::from([diag]);
    };
    if decl.targets.iter().any(|t| t.v == target) {
        return EcoVec::new();
    }
    let span = decl.targets.first().map_or(decl.name.span, |t| t.span);
    EcoVec::from([Diagnostic::error(
        span,
        eco_format!("variant `{name}` is not built for {target}"),
    )
    .with_hint(eco_format!("add `\"{target}\"` to its `targets`"))])
}
