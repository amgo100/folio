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
//!   registry. Each field is tracked on its own, with a durability that says
//!   how often it changes: bundled fonts and the components compiled into the
//!   binary (the first-party ones, SPEC §2.2) are [`Durability::HIGH`], so
//!   after an edit the queries that depend only on them are validated without
//!   walking their dependencies; the manifest, lockfile and project fonts are
//!   [`Durability::MEDIUM`]; sources and data files are [`Durability::LOW`].
//! - A [`SourceFile`] holds one `.fol` or `.fsty` file as a parsed
//!   [`Source`]. The host edits it in place ([`Source::edit`]), which reparses
//!   only the smallest enclosing block, and sets the result.
//! - A [`DataFile`] holds the bytes of an image or data file.
//!
//! The variant and the target are not settable inputs but keys: a [`Build`]
//! interns a project with a target and an optional variant, and the
//! per-variant queries take it as their argument. A query that does not read
//! the variant therefore runs once for all variants, and every variant of a
//! revision can be compiled at the same time on its own database handle
//! (SPEC §2.1).

// salsa generates the constructors, builders and setters of inputs and
// interned structs.
#![allow(missing_docs)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::{self, Debug, Formatter};
use std::ops::Deref;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Registry, Target};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{DiagSpan, Source, Span, SyntaxKind, SyntaxNode};
pub use salsa::Durability;

use crate::component::ComponentHost;
use crate::diag::Diagnostic;
use crate::project::{Lockfile, Manifest, Name, ProjectPath, Spanned};

/// The database that the compile queries run in. The `folio` crate provides
/// the implementation; each stage extends it when it moves into its own
/// crate.
#[salsa::db]
pub trait Db: salsa::Database {}

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
    /// Project, locked and system fonts. [`Durability::MEDIUM`].
    #[returns(ref)]
    pub fonts: EcoVec<FontFile>,
    /// The components the compiler hosts. [`Durability::HIGH`].
    #[returns(ref)]
    pub components: Registry,
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
    /// The file's contents. [`Durability::LOW`].
    #[returns(ref)]
    pub bytes: Bytes,
}

/// One output of a project: its target, and its variant, or none when the
/// manifest declares no variants.
#[salsa::interned(debug)]
pub struct Build<'db> {
    #[returns(copy)]
    pub project: Project,
    #[returns(copy)]
    pub target: Target,
    #[returns(ref)]
    pub variant: Option<Name>,
}

/// The contents of a file, cheap to clone.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Bytes(Arc<[u8]>);

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

/// A font file the host supplies. The font stage parses and indexes it
/// (SPEC §2.5); the database tracks it as an input.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontFile {
    /// Where it was found, for diagnostics: a project path, a system path,
    /// or the name of a bundled family.
    pub origin: EcoString,
    /// The file's contents.
    pub data: Bytes,
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

/// The files of one build: the entry file, its stylesheets, and every file
/// they name literally, in the order they were found.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct BuildFiles {
    /// The files found.
    pub files: EcoVec<SourceFile>,
    /// Files that are named but not in the project.
    pub missing: EcoVec<Diagnostic>,
}

/// What compiling a build produces.
///
/// The stages after parsing (SPEC §2.1) are not wired in yet, so a build
/// collects its files and their diagnostics, checks its variant and target
/// against the manifest, and hosts its components.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Compiled {
    /// The source files the build read, in the order they were found. A
    /// watcher recompiles when one of them changes.
    pub sources: EcoVec<ProjectPath>,
    /// Every problem found, errors and warnings.
    pub diagnostics: EcoVec<Diagnostic>,
}

impl Compiled {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// The source file at a path, if the project has one.
///
/// Re-runs when a file is added or removed, but returns the same file for
/// paths that still exist, so queries that look files up are cut off.
#[salsa::tracked(returns(copy))]
pub fn source_file(db: &dyn Db, project: Project, path: ProjectPath) -> Option<SourceFile> {
    project.sources(db).get(&path).copied()
}

/// The data file at a path, if the project has one.
#[salsa::tracked(returns(copy))]
pub fn data_file(db: &dyn Db, project: Project, path: ProjectPath) -> Option<DataFile> {
    project.files(db).get(&path).copied()
}

/// The files a source names literally: string paths of `#import` and
/// `#include` in content, and of `@import` in stylesheets. Paths computed by
/// scripts are found when the file is evaluated.
///
/// Spans are numbers in the syntax tree, not offsets, so an edit that does
/// not change the imports (such as one in whitespace or text) gives an equal
/// result, and the queries that depend on it are not re-run.
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

/// The syntax errors and warnings of a source, and the literal import paths
/// in it that leave the project.
#[salsa::tracked(returns(ref))]
pub fn file_diagnostics(db: &dyn Db, file: SourceFile) -> EcoVec<Diagnostic> {
    let path = file.path(db);
    let root = file.source(db).root();
    let (errors, warnings) = root.errors_and_warnings();
    let mut diags: EcoVec<Diagnostic> =
        errors.into_iter().chain(warnings).map(Into::into).collect();
    for_each_import(root, &mut |text, span| {
        if path.join(text).is_none() {
            diags.push(
                Diagnostic::error(span, eco_format!("`{text}` is not a path in the project"))
                    .with_hint("Folio reads files only inside the project directory"),
            );
        }
    });
    diags
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
        f(&path.get(), path.span());
        return;
    }
    for child in node.children() {
        for_each_import(child, f);
    }
}

/// The files of a build, found from its entry file and stylesheets (the
/// project's, then the variant's overlays).
#[salsa::tracked(returns(ref))]
pub fn build_files(db: &dyn Db, build: Build<'_>) -> BuildFiles {
    let project = build.project(db);
    let manifest = project.manifest(db);
    let mut roots: Vec<&Spanned<ProjectPath>> = vec![&manifest.project.entry];
    roots.extend(&manifest.project.styles);
    if let Some(variant) = declared_variant(manifest, build.variant(db).as_ref()) {
        roots.extend(&variant.styles);
    }

    let mut out = BuildFiles::default();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(ProjectPath, DiagSpan)> =
        roots.iter().map(|r| (r.v.clone(), r.span)).collect();
    while let Some((path, span)) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(file) = source_file(db, project, path.clone()) else {
            out.missing.push(
                Diagnostic::error(span, eco_format!("file `{}` not found", path.as_str()))
                    .with_hint("Folio reads `.fol` and `.fsty` files inside the project"),
            );
            continue;
        };
        out.files.push(file);
        for dep in dependencies(db, file) {
            queue.push_back((dep.path.clone(), dep.span.into()));
        }
    }
    out
}

/// The components a project uses, with their templates parsed and names
/// resolved through the lockfile's bindings.
///
/// Reads only the registry (durable) and the manifest and lockfile, so an
/// edit to a source does not re-run or even re-validate it.
#[salsa::tracked(returns(ref))]
pub fn component_host(db: &dyn Db, project: Project) -> Result<ComponentHost, EcoVec<Diagnostic>> {
    ComponentHost::new(
        project.components(db),
        &project.manifest(db).components,
        &project.lockfile(db).bindings,
    )
    .map_err(|errors| errors.into_iter().map(Into::into).collect())
}

/// Compiles one build.
#[salsa::tracked(returns(ref))]
pub fn compile(db: &dyn Db, build: Build<'_>) -> Compiled {
    let project = build.project(db);
    let mut diagnostics = check_build(project.manifest(db), build.target(db), build.variant(db));

    let files = build_files(db, build);
    diagnostics.extend(files.missing.iter().cloned());
    for &file in &files.files {
        diagnostics.extend(file_diagnostics(db, file).iter().cloned());
    }
    if let Err(errors) = component_host(db, project) {
        diagnostics.extend(errors.iter().cloned());
    }

    Compiled {
        sources: files.files.iter().map(|f| f.path(db).clone()).collect(),
        diagnostics,
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
