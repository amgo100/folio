//! `folio build` (SPEC §12.1): builds the variants, versions and targets of
//! a project and writes their outputs where `[output] path` says (SPEC §6.4). `folio
//! check` runs the same command without emitting or writing anything
//! ([`Mode::Check`]).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use ecow::EcoVec;
use folio::diag::{self, Files, Resolved, TextFiles};
use folio::{
    BuildKey, Cancelled, Compiled, Diagnostic, FileKind, FileRead, FontCache, LayoutDiagnostics,
    Manifest, Name, ProjectPath, World, check_versioned_builds, compile_versioned_builds,
};
use folio_syntax::DiagSpan;
use serde::Serialize;

use super::project::{self, LOCKFILE, MANIFEST};
use super::{Color, DiagnosticsFormat, FAILURE, ProjectArgs, toolchain};

/// What a command does with the builds it chooses.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(super) enum Mode {
    /// `folio build`: compile them and write their outputs.
    Build,
    /// `folio check`: check them ([`folio::check`]) and write nothing.
    Check,
}

/// Runs `folio build`.
pub(crate) fn build(args: &ProjectArgs) -> ExitCode {
    run_command(args, Mode::Build, LayoutDiagnostics::Serious)
}

/// Runs `folio build` or `folio check`, showing the layout diagnostics
/// that `layout` shows.
pub(super) fn run_command(args: &ProjectArgs, mode: Mode, layout: LayoutDiagnostics) -> ExitCode {
    let mut report = Report::new(args);
    let status = run(args, mode, layout, &mut report);
    report.finish();
    status
}

/// A build's diagnostics, and what compiling or checking it gave; or how it
/// failed.
type Outcome = Result<(EcoVec<Diagnostic>, Done), Cancelled>;

/// What a build gave.
enum Done {
    /// It was compiled.
    Compiled(Compiled),
    /// It was checked: whether it has errors, whether compiling it would
    /// give a file to write, and the files it read.
    Checked {
        errors: bool,
        writes: bool,
        files: EcoVec<FileRead>,
    },
}

/// Builds or checks, reporting what it finds to `report`.
fn run(args: &ProjectArgs, mode: Mode, layout: LayoutDiagnostics, report: &mut Report) -> ExitCode {
    let mut fail = |message: String| {
        report.diagnostics(&TextFiles::new(), &[error(message)]);
        ExitCode::from(FAILURE)
    };

    let root = match project::find_root(args.root.as_deref()) {
        Ok(root) => root,
        Err(message) => return fail(message),
    };
    let manifest = match fs::read_to_string(root.join(MANIFEST)) {
        Ok(text) => text,
        Err(e) => return fail(format!("cannot read {MANIFEST}: {e}")),
    };
    // A project that pins another installed version was handed to it
    // before the arguments were parsed (`toolchain::dispatch`).
    let current = args.compiler.is_some();
    let lockfile = match project::read_lockfile(&root) {
        Ok(text) => text,
        Err(e) => return fail(format!("cannot read {LOCKFILE}: {e}")),
    };
    let mut world = match World::builder().build(&manifest, lockfile.as_deref()) {
        Ok(world) => world,
        Err(errors) => {
            let mut files = TextFiles::new();
            files.insert(World::MANIFEST, MANIFEST, manifest);
            if let Some(lockfile) = lockfile {
                files.insert(World::LOCKFILE, LOCKFILE, lockfile);
            }
            let errors: Vec<Diagnostic> = errors.into_iter().map(Diagnostic::from).collect();
            report.diagnostics(&files, &errors);
            return ExitCode::from(FAILURE);
        }
    };

    // Problems found before compiling, which the builds' follow.
    let mut diagnostics = Vec::new();
    let builds = match select(world.manifest(), &world.versioned_builds(), args) {
        Ok(builds) => builds,
        Err(error) => {
            report.diagnostics(&world, &[error]);
            return ExitCode::from(FAILURE);
        }
    };
    diagnostics.extend(toolchain::check(world.manifest(), current));

    // The outputs of every build are not inputs of the next.
    if let Err(message) = project::load(&root, &mut world, &mut diagnostics) {
        return fail(message);
    }
    let cache = FontCache::default_dir().map(FontCache::new);
    if let Err(e) = world.load_fonts(&root, cache.as_ref()) {
        return fail(format!("cannot read the project's fonts: {e}"));
    }
    world.set_system_fonts(folio::system_fonts());
    if let Some(today) = args.today {
        world.set_today(Some(today));
    }

    let outputs: Vec<(BuildKey, Outcome)> = if diagnostics.iter().any(Diagnostic::is_error) {
        Vec::new()
    } else {
        match mode {
            Mode::Build => compile_versioned_builds(&world, builds)
                .into_iter()
                .map(|o| {
                    let result = o
                        .result
                        .map(|c| (layout.filter(c.diagnostics.clone()), Done::Compiled(c)));
                    (o.build, result)
                })
                .collect(),
            Mode::Check => check_versioned_builds(&world, builds)
                .into_iter()
                .map(|o| {
                    let result = o.result.map(|c| {
                        let errors = c.has_errors();
                        let writes = c.writes;
                        (
                            layout.filter(c.diagnostics),
                            Done::Checked {
                                errors,
                                writes,
                                files: c.files,
                            },
                        )
                    });
                    (o.build, result)
                })
                .collect(),
        }
    };

    // A diagnostic of a file that several builds read is shown once.
    let mut seen: HashSet<Diagnostic> = diagnostics.iter().cloned().collect();
    // Each build, in the manifest's order, with what it compiled to, or
    // `None` when it failed.
    let mut written = Vec::new();
    for (key, result) in outputs {
        let name = build_name(&key);
        let path = world.manifest().versioned_output_path(
            key.variant.as_ref(),
            key.version.as_ref(),
            key.target,
        );
        let (found, compiled) = match result {
            Ok(outcome) => outcome,
            Err(cancelled) => {
                let what = match mode {
                    Mode::Build => "build",
                    Mode::Check => "check",
                };
                diagnostics.push(error(format!("the {what} of {name} failed: {cancelled}")));
                written.push((key, path, None));
                continue;
            }
        };
        for diagnostic in &found {
            if seen.insert(diagnostic.clone()) {
                diagnostics.push(diagnostic.clone());
            }
        }
        written.push((key, path, Some(compiled)));
    }
    report.diagnostics(&world, &diagnostics);

    // No output replaces a file that any build of the run reads.
    let read: Vec<FileRead> = written
        .iter()
        .flat_map(|(.., done)| match done {
            Some(Done::Compiled(compiled)) => compiled.files.iter(),
            Some(Done::Checked { files, .. }) => files.iter(),
            None => [].iter(),
        })
        .cloned()
        .collect();
    let mut failed = diagnostics.iter().any(Diagnostic::is_error);
    for (key, path, compiled) in written {
        let build = |path: Option<&str>, status| Build::new(&key, path, status);
        let compiled = match compiled {
            None => {
                let path = (mode == Mode::Build).then_some(path).flatten();
                report.build(build(path.as_deref(), Status::Failed));
                continue;
            }
            // A check writes nothing, but finds what would stop the write.
            Some(Done::Checked { errors, writes, .. }) => {
                let path = path.as_deref().filter(|_| writes);
                let status = match check_write(&root, path, errors, &read) {
                    Ok(()) => Status::Checked,
                    Err(message) => {
                        report.diagnostics(&world, &[error(message)]);
                        failed = true;
                        Status::Failed
                    }
                };
                report.build(build(None, status));
                continue;
            }
            Some(Done::Compiled(compiled)) => compiled,
        };
        match write(&root, path.as_deref(), &compiled, &read) {
            Ok(Written::File(path)) => report.build(build(Some(&path), Status::Written)),
            Ok(Written::Nothing) => report.build(build(path.as_deref(), Status::None)),
            Ok(Written::Skipped(path)) => report.build(build(Some(&path), Status::Skipped)),
            Err(message) => {
                report.diagnostics(&world, &[error(message)]);
                report.build(build(path.as_deref(), Status::Failed));
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(FAILURE)
    } else {
        ExitCode::SUCCESS
    }
}

/// The builds that the arguments choose among those the manifest declares,
/// in the manifest's order: all of them unless `--variant`, `--versions` or
/// `--target` narrows them.
fn select(
    manifest: &Manifest,
    declared: &[BuildKey],
    args: &ProjectArgs,
) -> Result<Vec<BuildKey>, Diagnostic> {
    let names = || -> String {
        let names: Vec<_> = manifest
            .variants
            .iter()
            .map(|v| format!("`{}`", v.name.v))
            .collect();
        names.join(", ")
    };
    for variant in &args.variants {
        if !manifest
            .variants
            .iter()
            .any(|v| v.name.v.as_str() == variant)
        {
            let error = error(format!("the manifest declares no variant `{variant}`"));
            return Err(if manifest.variants.is_empty() {
                error.with_hint("a project without variants is built once, for PDF")
            } else {
                error.with_hint(format!("it declares {}", names()))
            });
        }
    }
    let versions = &manifest.project.versions;
    for version in &args.versions {
        if !versions.iter().any(|v| v.v.as_str() == version) {
            let error = error(format!("the manifest declares no version `{version}`"));
            let names: Vec<_> = versions.iter().map(|v| format!("`{}`", v.v)).collect();
            return Err(if versions.is_empty() {
                error.with_hint("declare versions in `[project]`, as `versions = [\"A\", \"B\"]`")
            } else {
                error.with_hint(format!("it declares {}", names.join(", ")))
            });
        }
    }
    let builds: Vec<_> = declared
        .iter()
        .filter(|build| {
            let variant = build.variant.as_ref().map_or("", Name::as_str);
            let version = build.version.as_ref().map_or("", Name::as_str);
            (args.variants.is_empty() || args.variants.iter().any(|v| v == variant))
                && (args.versions.is_empty() || args.versions.iter().any(|v| v == version))
                && (args.targets.is_empty() || args.targets.contains(&build.target))
        })
        .cloned()
        .collect();
    if builds.is_empty() {
        let chosen: Vec<_> = match args.targets.is_empty() {
            true => args.variants.iter().map(|v| format!("`{v}`")).collect(),
            false => args.targets.iter().map(|t| format!("`{t}`")).collect(),
        };
        let error = error(format!("nothing to build for {}", chosen.join(", ")));
        return Err(if manifest.variants.is_empty() {
            error.with_hint("a project without variants is built once, for PDF")
        } else {
            error.with_hint("a variant's `targets` in folio.toml declare what it is built for")
        });
    }
    Ok(builds)
}

/// A build as the messages of `folio build` name it: `exam (pdf)`, or
/// `exam, version B (pdf)`.
fn build_name(build: &BuildKey) -> String {
    build_name_text(
        build.variant.as_ref().map(Name::as_str),
        build.version.as_ref().map(Name::as_str),
        build.target.as_str(),
    )
}

/// [`build_name`] of a variant, version and target given by name.
fn build_name_text(variant: Option<&str>, version: Option<&str>, target: &str) -> String {
    let variant = variant.unwrap_or("the document");
    match version {
        Some(version) => format!("{variant}, version {version} ({target})"),
        None => format!("{variant} ({target})"),
    }
}

/// What [`write`] did with a build's output.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Written {
    /// It wrote the file at this path.
    File(String),
    /// The build has errors, so it did not write the file at this path.
    Skipped(String),
    /// The target writes no file, or has no writer yet.
    Nothing,
}

/// Writes a build's output to `path` in the project, unless the build has
/// errors. The file is replaced whole, so a reader never sees half of it:
/// the bytes go to a temporary file of a name no other build uses, which is
/// then renamed over the output. Symbolic links to directories are followed
/// only inside the project (SPEC §12.1), and an output whose directory lies
/// outside it is refused; a link at the output itself is replaced. An
/// output that is one of the files `read` lists is refused, so it never
/// replaces a source, a data file or an image: `read` holds what the
/// builds written together read ([`Compiled::files`]), this one's among
/// them.
pub(super) fn write(
    root: &Path,
    path: Option<&str>,
    compiled: &Compiled,
    read: &[FileRead],
) -> Result<Written, String> {
    let (Some(path), Some(bytes)) = (path, &compiled.output) else {
        return Ok(Written::Nothing);
    };
    let Some(path) = ProjectPath::new(path) else {
        return Err(format!(
            "the output path `{path}` is not a path in the project"
        ));
    };
    let message = |e: io::Error| format!("cannot write `{path}`: {e}");
    refuse_read(root, &path, read).map_err(message)?;
    let path = path.as_str().to_owned();
    if compiled.has_errors() {
        return Ok(Written::Skipped(path));
    }
    let (root, file) = resolve(root, &path, true).map_err(message)?;
    // Directories created on the way hold no file a build read.
    replace(&root, &file, bytes, None).map_err(message)?;
    Ok(Written::File(path))
}

/// Finds what would stop [`write`] from placing a build's output at `path`
/// in the project, for `folio check`, without writing or creating
/// anything: a path that is not in the project, and for a build without
/// errors (`errors`), a directory on the way that a link places outside
/// the project or that is not a directory. A file the builds checked
/// together read (`files`) is refused as [`write`] refuses it, whether the
/// build has errors or not. What only writing finds, such as a directory
/// where the file would go or one that cannot be written to, is not
/// checked. `path` is `None` when the build would write no file.
fn check_write(
    root: &Path,
    path: Option<&str>,
    errors: bool,
    files: &[FileRead],
) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(path) = ProjectPath::new(path) else {
        return Err(format!(
            "the output path `{path}` is not a path in the project"
        ));
    };
    let message = |e: io::Error| format!("cannot write `{path}`: {e}");
    refuse_read(root, &path, files).map_err(message)?;
    if errors {
        return Ok(());
    }
    resolve(root, path.as_str(), false)
        .map(|_| ())
        .map_err(message)
}

/// Refuses an output at the project path `path` in the project at `root`
/// when it is a file a build read, or tried to read (`files`): by its path
/// ([`read_by_build`]) or on disk ([`read_on_disk`]), as far as the
/// directories on the way exist.
fn refuse_read(root: &Path, path: &ProjectPath, files: &[FileRead]) -> io::Result<()> {
    read_by_build(path, files)?;
    match resolve(root, path.as_str(), false) {
        Ok((root, file)) => read_on_disk(&root, &file, files),
        // What stops the write is reported where it is written.
        Err(_) => Ok(()),
    }
}

/// Refuses an output at the project path `path` when a build read, or
/// tried to read, a file of that path (`files`): writing it would replace
/// one of the project's inputs. Paths are compared without regard to ASCII
/// case, as file systems that ignore case would replace the file all the
/// same.
fn read_by_build(path: &ProjectPath, files: &[FileRead]) -> io::Result<()> {
    match files
        .iter()
        .find(|f| f.path.as_str().eq_ignore_ascii_case(path.as_str()))
    {
        Some(read) => Err(read_error(read)),
        None => Ok(()),
    }
}

/// Refuses an output at `file`, on disk in the project at `root` (both as
/// [`resolve`] gives them), when it is a file a build read (`files`),
/// reached another way: through a symbolic link to a directory, or as the
/// target of a symbolic link that the build read. The file names are
/// compared without regard to ASCII case, as by [`read_by_build`].
fn read_on_disk(root: &Path, file: &Path, files: &[FileRead]) -> io::Result<()> {
    let (Some(out_dir), Some(out_name)) = (file.parent(), file.file_name()) else {
        return Ok(());
    };
    let is_output = |path: &Path| {
        path.parent() == Some(out_dir)
            && path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(out_name))
    };
    for read in files {
        let path = root.join(read.path.as_str());
        // The file the build reads, through any link.
        let target = path.canonicalize().ok();
        // The path it reads, with only its directory resolved: the link
        // itself, or a file that is missing.
        let named = path
            .parent()
            .zip(path.file_name())
            .and_then(|(dir, name)| Some(dir.canonicalize().ok()?.join(name)));
        if target.iter().chain(&named).any(|p| is_output(p)) {
            return Err(read_error(read));
        }
    }
    Ok(())
}

/// The error for an output that would replace a file the build read.
fn read_error(read: &FileRead) -> io::Error {
    let what = match read.kind {
        FileKind::Content => "a content file",
        FileKind::Stylesheet => "a stylesheet",
        FileKind::Data => "a data file",
        FileKind::Image => "an image",
    };
    io::Error::other(format!("a build reads `{}` as {what}", read.path.as_str()))
}

/// The count in the names of temporary files, which tells apart the writes
/// of one process.
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// The file on disk that the output at `path`, a project path, is written
/// to, with the project directory: the directories on the way are created
/// if they are missing (unless `create` is false, when they are left
/// missing), and symbolic links among them are followed only to
/// directories inside the project. A link at the file itself is replaced,
/// not followed.
fn resolve(root: &Path, path: &str, create: bool) -> io::Result<(PathBuf, PathBuf)> {
    let root = root.canonicalize()?;
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut dir = root.clone();
    let mut walked = String::new();
    for component in dirs.split('/').filter(|c| !c.is_empty()) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(component);
        dir = enter(&root, &dir.join(component), &walked, create)?;
    }
    Ok((root, dir.join(name)))
}

/// The directory to go on from at `next`, which the project path `walked`
/// names: `next` itself, created if it is missing (or left missing, unless
/// `create`), or the target of a link there when that is a directory in
/// the project at `root`.
fn enter(root: &Path, next: &Path, walked: &str, create: bool) -> io::Result<PathBuf> {
    let not_dir = || io::Error::other(format!("`{walked}` is not a directory"));
    let mut created = false;
    loop {
        match fs::symlink_metadata(next) {
            Err(e) if e.kind() == io::ErrorKind::NotFound && !create => {
                return Ok(next.to_owned());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && !created => {
                // Another build may create it, or put a link there, in the
                // meantime: then what is there now is checked.
                match fs::create_dir(next) {
                    Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                    _ => created = true,
                }
            }
            Err(e) => return Err(e),
            Ok(meta) if meta.is_symlink() => {
                let target = next.canonicalize().map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        io::Error::other(format!("`{walked}` is a broken symbolic link"))
                    } else {
                        e
                    }
                })?;
                if !target.starts_with(root) {
                    return Err(outside(walked));
                }
                if !target.is_dir() {
                    return Err(not_dir());
                }
                return Ok(target);
            }
            Ok(meta) if meta.is_dir() => return Ok(next.to_owned()),
            Ok(_) => return Err(not_dir()),
        }
    }
}

/// The error for a link at the project path `walked` that leads out of the
/// project.
fn outside(walked: &str) -> io::Error {
    io::Error::other(format!(
        "`{walked}` is a symbolic link to a place outside the project"
    ))
}

/// Replaces `file` whole with `bytes`, through a temporary file beside it
/// whose name no other write uses: it holds this process's id and a count,
/// and is created only if nothing, not even a symbolic link, has that name.
/// The temporary file is synced before the rename, which goes through the
/// file's canonical directory once that is checked to be in the project at
/// `root`: this narrows, but does not close, the window in which another
/// process could swap a directory on the way for a link out of the project.
/// The new file takes `permissions`, if given, and else the defaults of a
/// new file.
pub(super) fn replace(
    root: &Path,
    file: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let (temporary, mut out) = loop {
        let n = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = file.with_file_name(format!(".{name}.{pid}-{n}.folio-tmp"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // A file that takes permissions is private until it has them.
        #[cfg(unix)]
        if permissions.is_some() {
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        }
        match options.open(&temporary) {
            Ok(out) => break (temporary, out),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    // Where the temporary file really is, which the rename and the clean-up
    // go through, so a directory swapped after the check is not followed.
    let mut placed = temporary.clone();
    // The permissions come first, so the text is never readable by more
    // than the file's were.
    let result = match permissions {
        Some(permissions) => out.set_permissions(permissions),
        None => Ok(()),
    }
    .and_then(|()| out.write_all(bytes))
    .and_then(|()| out.sync_all())
    .and_then(|()| {
        drop(out);
        placed = temporary.canonicalize()?;
        match placed.parent() {
            Some(dir) if dir.starts_with(root) => fs::rename(&placed, dir.join(&*name)),
            _ => Err(io::Error::other(
                "a directory on the way was replaced by a link out of the project",
            )),
        }
    });
    if result.is_err() {
        let _ = fs::remove_file(&placed);
    }
    result
}

/// Where `folio build` and `folio check` report what they found and did:
/// to standard error, as it goes, with source excerpts; or, with
/// `--diagnostics json`, as one JSON document written when the command
/// ends (`docs/diagnostics-json.md`).
pub(super) struct Report {
    color: bool,
    /// The document being collected, with `--diagnostics json`.
    json: Option<Document>,
}

/// The JSON document of `--diagnostics json`, which
/// `docs/diagnostics-json.md` describes.
#[derive(Debug, Default, Serialize)]
struct Document {
    /// The version of the document's shape.
    version: u32,
    /// The diagnostics, in the order the terminal shows them.
    diagnostics: Vec<Resolved>,
    /// The builds that were compiled, and what became of their outputs.
    builds: Vec<Build>,
}

/// A build in the JSON document, and what became of its output.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
struct Build {
    /// The variant, or `None` for a project without variants.
    variant: Option<String>,
    /// The version, or `None` for a project without versions.
    version: Option<String>,
    /// The target, as the manifest names it.
    target: &'static str,
    /// The output's path in the project, if the build has one.
    path: Option<String>,
    /// What became of the output.
    status: Status,
}

impl Build {
    fn new(build: &BuildKey, path: Option<&str>, status: Status) -> Self {
        Self {
            variant: build.variant.as_ref().map(|v| v.as_str().to_owned()),
            version: build.version.as_ref().map(|v| v.as_str().to_owned()),
            target: build.target.as_str(),
            path: path.map(str::to_owned),
            status,
        }
    }
}

/// What became of a build's output.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    /// It was written.
    Written,
    /// It was not written, because the build has errors.
    Skipped,
    /// The target writes no file, or has no writer yet.
    None,
    /// The build or the write failed; a diagnostic says why.
    Failed,
    /// The build was checked (`folio check`), which writes no output.
    Checked,
}

impl Report {
    /// The version of [`Document`]'s shape.
    const VERSION: u32 = 1;

    fn new(args: &ProjectArgs) -> Self {
        Self::with(args.color, args.diagnostics)
    }

    /// A report coloured as `color` says, in `format`.
    pub(super) fn with(color: Color, format: DiagnosticsFormat) -> Self {
        Self {
            color: color.enabled(),
            json: (format == DiagnosticsFormat::Json).then(|| Document {
                version: Self::VERSION,
                ..Document::default()
            }),
        }
    }

    /// Whether the report is the JSON document.
    pub(super) fn is_json(&self) -> bool {
        self.json.is_some()
    }

    /// Reports diagnostics, with their spans resolved against `files`.
    pub(super) fn diagnostics(&mut self, files: &dyn Files, diagnostics: &[Diagnostic]) {
        match &mut self.json {
            Some(document) => document
                .diagnostics
                .extend(diagnostics.iter().map(|d| diag::resolve(files, d))),
            None => print(files, diagnostics, self.color),
        }
    }

    /// Reports what became of a build's output.
    fn build(&mut self, build: Build) {
        let Some(document) = &mut self.json else {
            let name = build_name_text(
                build.variant.as_deref(),
                build.version.as_deref(),
                build.target,
            );
            match (build.status, &build.path) {
                (Status::Written, Some(path)) => eprintln!("wrote {path}"),
                (Status::Skipped, Some(path)) => {
                    eprintln!("did not write {path}: the build of {name} has errors");
                }
                _ => {}
            }
            return;
        };
        document.builds.push(build);
    }

    /// Ends the report: writes the JSON document, if there is one.
    pub(super) fn finish(self) {
        if let Some(document) = self.json {
            let json = serde_json::to_string_pretty(&document).expect("the report serialises");
            eprintln!("{json}");
        }
    }
}

/// An error that points at no source.
pub(super) fn error(message: String) -> Diagnostic {
    Diagnostic::error(DiagSpan::detached(), message)
}

/// Prints diagnostics to standard error.
pub(super) fn print(files: &dyn Files, diagnostics: &[Diagnostic], color: bool) {
    eprint!("{}", diag::render(files, diagnostics, color));
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
        compiler = \"0.0.0\"\n\n[variants.exam]\ntargets = [\"pdf\", \"docx\"]\n\n\
        [variants.markscheme]\ntargets = [\"pdf\"]\n";

    fn args(flags: &[&str]) -> ProjectArgs {
        let mut all = vec!["folio", "build"];
        all.extend(flags);
        match Cli::parse_from(all).command {
            Command::Build(args) => args.project,
            other => panic!("{other:?}"),
        }
    }

    fn selected(manifest: &str, flags: &[&str]) -> Result<Vec<String>, String> {
        let world = World::new(manifest).unwrap();
        select(world.manifest(), &world.versioned_builds(), &args(flags))
            .map(|builds| builds.iter().map(build_name).collect())
            .map_err(|e| e.message.to_string())
    }

    /// SPEC §6.4, §12.1: every variant once per version, narrowed by
    /// `--versions`.
    #[test]
    fn selects_versions() {
        let versioned = MANIFEST.replace(
            "compiler = \"0.0.0\"\n",
            "compiler = \"0.0.0\"\nversions = [\"A\", \"B\"]\n",
        );
        assert_eq!(
            selected(&versioned, &["--target", "pdf"]).unwrap(),
            [
                "exam, version A (pdf)",
                "markscheme, version A (pdf)",
                "exam, version B (pdf)",
                "markscheme, version B (pdf)",
            ]
        );
        assert_eq!(
            selected(&versioned, &["--versions", "B", "--variant", "exam"]).unwrap(),
            ["exam, version B (pdf)", "exam, version B (docx)"]
        );
        assert_eq!(
            selected(&versioned, &["--versions", "C"]).unwrap_err(),
            "the manifest declares no version `C`"
        );
        assert_eq!(
            selected(MANIFEST, &["--versions", "A"]).unwrap_err(),
            "the manifest declares no version `A`"
        );
        assert_eq!(
            selected(MANIFEST, &["--variant", "exam"]).unwrap(),
            ["exam (pdf)", "exam (docx)"]
        );
    }

    #[test]
    fn selects_builds() {
        assert_eq!(
            selected(MANIFEST, &[]).unwrap(),
            ["exam (pdf)", "exam (docx)", "markscheme (pdf)"]
        );
        assert_eq!(
            selected(MANIFEST, &["--variant", "markscheme"]).unwrap(),
            ["markscheme (pdf)"]
        );
        assert_eq!(
            selected(MANIFEST, &["--target", "pdf"]).unwrap(),
            ["exam (pdf)", "markscheme (pdf)"]
        );
        assert_eq!(
            selected(MANIFEST, &["--variant", "markscheme", "--target", "docx"]).unwrap_err(),
            "nothing to build for `docx`"
        );
        assert_eq!(
            selected(MANIFEST, &["--variant", "slides"]).unwrap_err(),
            "the manifest declares no variant `slides`"
        );

        let plain = MANIFEST.split("\n\n").next().unwrap();
        assert_eq!(selected(plain, &[]).unwrap(), ["the document (pdf)"]);
        assert!(selected(plain, &["--target", "docx"]).is_err());
        assert!(selected(plain, &["--variant", "exam"]).is_err());
    }

    #[test]
    fn writes_only_builds_without_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ok = Compiled {
            output: Some(b"%PDF-".to_vec().into()),
            ..Compiled::default()
        };
        assert_eq!(
            write(dir.path(), Some("out/main-exam.pdf"), &ok, &[]),
            Ok(Written::File("out/main-exam.pdf".into()))
        );
        assert_eq!(
            fs::read(dir.path().join("out/main-exam.pdf")).unwrap(),
            b"%PDF-"
        );
        assert_eq!(fs::read_dir(dir.path().join("out")).unwrap().count(), 1);

        let broken = Compiled {
            diagnostics: [error("broken".into())].into_iter().collect(),
            ..ok.clone()
        };
        assert_eq!(
            write(dir.path(), Some("out/x.pdf"), &broken, &[]),
            Ok(Written::Skipped("out/x.pdf".into()))
        );
        assert!(!dir.path().join("out/x.pdf").exists());
        assert_eq!(write(dir.path(), None, &ok, &[]), Ok(Written::Nothing));
        assert_eq!(
            write(dir.path(), Some("x.docx"), &Compiled::default(), &[]),
            Ok(Written::Nothing)
        );
    }

    #[test]
    fn never_replaces_a_file_the_build_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("data/marks.csv"), "keep").unwrap();
        let read = |path: &str, kind| FileRead {
            path: ProjectPath::new(path).unwrap(),
            kind,
        };
        let ok = Compiled {
            output: Some(b"%PDF-".to_vec().into()),
            files: [
                read("main.fol", FileKind::Content),
                read("data/marks.csv", FileKind::Data),
                read("missing.png", FileKind::Image),
            ]
            .into_iter()
            .collect(),
            ..Compiled::default()
        };
        for (path, message) in [
            (
                "data/marks.csv",
                "cannot write `data/marks.csv`: a build reads `data/marks.csv` as a data file",
            ),
            (
                "Data/Marks.CSV",
                "cannot write `Data/Marks.CSV`: a build reads `data/marks.csv` as a data file",
            ),
            // A file named but missing is not replaced either.
            (
                "missing.png",
                "cannot write `missing.png`: a build reads `missing.png` as an image",
            ),
        ] {
            assert_eq!(
                write(root, Some(path), &ok, &ok.files).unwrap_err(),
                message
            );
            assert_eq!(
                check_write(root, Some(path), false, &ok.files).unwrap_err(),
                message
            );
        }
        // A build with errors would write nothing, but is still told.
        assert!(check_write(root, Some("data/marks.csv"), true, &ok.files).is_err());
        assert_eq!(fs::read(root.join("data/marks.csv")).unwrap(), b"keep");
        assert!(!root.join("missing.png").exists());
        assert!(check_write(root, Some("out/main.pdf"), false, &ok.files).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn never_replaces_a_file_the_build_read_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("data/marks.csv"), "keep").unwrap();
        std::os::unix::fs::symlink(root.join("data"), root.join("out")).unwrap();
        let ok = Compiled {
            output: Some(b"%PDF-".to_vec().into()),
            files: [FileRead {
                path: ProjectPath::new("data/marks.csv").unwrap(),
                kind: FileKind::Data,
            }]
            .into_iter()
            .collect(),
            ..Compiled::default()
        };
        let message = "cannot write `out/marks.csv`: a build reads `data/marks.csv` as a data file";
        assert_eq!(
            write(root, Some("out/marks.csv"), &ok, &ok.files).unwrap_err(),
            message
        );
        assert_eq!(
            check_write(root, Some("out/marks.csv"), false, &ok.files).unwrap_err(),
            message
        );
        // A build with errors is told too.
        assert_eq!(
            check_write(root, Some("out/marks.csv"), true, &ok.files).unwrap_err(),
            message
        );
        assert_eq!(fs::read(root.join("data/marks.csv")).unwrap(), b"keep");

        // A file the build reads through a link to it.
        std::os::unix::fs::symlink(root.join("data/marks.csv"), root.join("alias.csv")).unwrap();
        let through = Compiled {
            files: [FileRead {
                path: ProjectPath::new("alias.csv").unwrap(),
                kind: FileKind::Data,
            }]
            .into_iter()
            .collect(),
            ..ok.clone()
        };
        assert_eq!(
            write(root, Some("data/marks.csv"), &through, &through.files).unwrap_err(),
            "cannot write `data/marks.csv`: a build reads `alias.csv` as a data file"
        );
        assert_eq!(fs::read(root.join("data/marks.csv")).unwrap(), b"keep");
    }

    // Windows may refuse a rename over a file that another rename is
    // replacing at that moment; that is not verified here.
    #[cfg(unix)]
    #[test]
    fn concurrent_writes_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let contents: Vec<Vec<u8>> = (0..8u8).map(|i| vec![i; 64 * 1024]).collect();
        std::thread::scope(|scope| {
            for bytes in &contents {
                let root = dir.path();
                scope.spawn(move || {
                    let compiled = Compiled {
                        output: Some(bytes.clone().into()),
                        ..Compiled::default()
                    };
                    for _ in 0..20 {
                        assert_eq!(
                            write(root, Some("out/main.pdf"), &compiled, &[]),
                            Ok(Written::File("out/main.pdf".into()))
                        );
                    }
                });
            }
        });
        let written = fs::read(dir.path().join("out/main.pdf")).unwrap();
        assert!(contents.contains(&written));
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(dir.path().join("out")).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn writes_follow_symbolic_links_only_inside_the_project() {
        use std::os::unix::fs::symlink;

        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = project.path();
        let ok = Compiled {
            output: Some(b"%PDF-".to_vec().into()),
            ..Compiled::default()
        };
        let message = |path: &str| write(root, Some(path), &ok, &[]).unwrap_err();

        // A directory that links out of the project.
        symlink(outside.path(), root.join("out")).unwrap();
        assert_eq!(
            message("out/main.pdf"),
            "cannot write `out/main.pdf`: `out` is a symbolic link to a place outside the project"
        );
        assert_eq!(
            message("out/sub/main.pdf"),
            "cannot write `out/sub/main.pdf`: `out` is a symbolic link to a place outside \
             the project"
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        symlink(outside.path().join("nothing"), root.join("gone")).unwrap();
        assert_eq!(
            message("gone/main.pdf"),
            "cannot write `gone/main.pdf`: `gone` is a broken symbolic link"
        );

        // An output that is a link, out of the project, inside it, or to
        // nothing, is replaced; what it points at is left alone.
        fs::remove_file(root.join("out")).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        fs::write(outside.path().join("victim"), "keep").unwrap();
        fs::write(root.join("main.fol"), "keep").unwrap();
        symlink(outside.path().join("victim"), root.join("out/main.pdf")).unwrap();
        symlink(root.join("main.fol"), root.join("out/alias.pdf")).unwrap();
        symlink(
            outside.path().join("nothing"),
            root.join("out/dangling.pdf"),
        )
        .unwrap();
        for path in ["out/main.pdf", "out/alias.pdf", "out/dangling.pdf"] {
            assert_eq!(
                write(root, Some(path), &ok, &[]),
                Ok(Written::File(path.into()))
            );
            let meta = fs::symlink_metadata(root.join(path)).unwrap();
            assert!(meta.is_file(), "{path}");
            assert_eq!(fs::read(root.join(path)).unwrap(), b"%PDF-");
        }
        assert_eq!(fs::read(outside.path().join("victim")).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("main.fol")).unwrap(), b"keep");
        assert!(!outside.path().join("nothing").exists());

        // A temporary name taken by a link is passed over, not written
        // through. Other tests in this process may take counts meanwhile,
        // so many names ahead are taken.
        let next = TEMPORARY.load(Ordering::Relaxed);
        for n in next..next + 1024 {
            symlink(
                outside.path().join("victim"),
                root.join(format!("out/.x.pdf.{}-{n}.folio-tmp", std::process::id())),
            )
            .unwrap();
        }
        assert!(write(root, Some("out/x.pdf"), &ok, &[]).is_ok());
        assert!(TEMPORARY.load(Ordering::Relaxed) > next + 1024);
        assert_eq!(fs::read(outside.path().join("victim")).unwrap(), b"keep");

        // Links to directories inside the project are followed.
        fs::create_dir(root.join("build")).unwrap();
        symlink(root.join("build"), root.join("pdfs")).unwrap();
        assert_eq!(
            write(root, Some("pdfs/main.pdf"), &ok, &[]),
            Ok(Written::File("pdfs/main.pdf".into()))
        );
        assert_eq!(fs::read(root.join("build/main.pdf")).unwrap(), b"%PDF-");

        // A file where a directory is needed.
        fs::write(root.join("file"), "").unwrap();
        assert_eq!(
            message("file/main.pdf"),
            "cannot write `file/main.pdf`: `file` is not a directory"
        );
    }
}
