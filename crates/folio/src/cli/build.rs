//! `folio build` (SPEC §12.1): builds the variants and targets of a project
//! and writes their outputs where `[output] path` says (SPEC §6.4).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use folio::diag::{self, Files, Resolved, TextFiles};
use folio::{
    Compiled, Diagnostic, FontCache, Manifest, Name, ProjectPath, Target, World, compile_builds,
};
use folio_syntax::DiagSpan;
use serde::Serialize;

use super::project::{self, LOCKFILE, MANIFEST};
use super::{BuildArgs, DiagnosticsFormat, FAILURE, toolchain};

/// Runs `folio build`.
pub(crate) fn build(args: &BuildArgs) -> ExitCode {
    let mut report = Report::new(args);
    let status = run(args, &mut report);
    report.finish();
    status
}

/// Builds, reporting what it finds to `report`.
fn run(args: &BuildArgs, report: &mut Report) -> ExitCode {
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
    let builds = match select(world.manifest(), &world.builds(), args) {
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

    let outputs = if diagnostics.iter().any(Diagnostic::is_error) {
        Vec::new()
    } else {
        compile_builds(&world, builds)
    };

    // A diagnostic of a file that several builds read is shown once.
    let mut seen: HashSet<Diagnostic> = diagnostics.iter().cloned().collect();
    // Each build, in the manifest's order, with what it compiled to, or
    // `None` when it failed.
    let mut written = Vec::new();
    for output in outputs {
        let name = build_name(output.variant.as_ref(), output.target);
        let path = world
            .manifest()
            .output_path(output.variant.as_ref(), output.target);
        let compiled = match output.result {
            Ok(compiled) => compiled,
            Err(cancelled) => {
                diagnostics.push(error(format!("the build of {name} failed: {cancelled}")));
                written.push((output.variant, output.target, path, None));
                continue;
            }
        };
        for diagnostic in &compiled.diagnostics {
            if seen.insert(diagnostic.clone()) {
                diagnostics.push(diagnostic.clone());
            }
        }
        written.push((output.variant, output.target, path, Some(compiled)));
    }
    report.diagnostics(&world, &diagnostics);

    let mut failed = diagnostics.iter().any(Diagnostic::is_error);
    for (variant, target, path, compiled) in written {
        let build = |path: Option<&str>, status| Build::new(&variant, target, path, status);
        let Some(compiled) = compiled else {
            report.build(build(path.as_deref(), Status::Failed));
            continue;
        };
        match write(&root, path.as_deref(), &compiled) {
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
/// in the manifest's order: all of them unless `--variant` or `--target`
/// narrows them.
fn select(
    manifest: &Manifest,
    declared: &[(Option<Name>, Target)],
    args: &BuildArgs,
) -> Result<Vec<(Option<Name>, Target)>, Diagnostic> {
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
    let builds: Vec<_> = declared
        .iter()
        .filter(|(variant, target)| {
            let variant = variant.as_ref().map_or("", Name::as_str);
            (args.variants.is_empty() || args.variants.iter().any(|v| v == variant))
                && (args.targets.is_empty() || args.targets.contains(target))
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

/// A build as the messages of `folio build` name it: `exam (pdf)`.
fn build_name(variant: Option<&Name>, target: Target) -> String {
    build_name_text(variant.map(Name::as_str), target.as_str())
}

/// [`build_name`] of a variant and target given by name.
fn build_name_text(variant: Option<&str>, target: &str) -> String {
    match variant {
        Some(variant) => format!("{variant} ({target})"),
        None => format!("the document ({target})"),
    }
}

/// What [`write`] did with a build's output.
#[derive(Debug, Eq, PartialEq)]
enum Written {
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
/// outside it is refused; a link at the output itself is replaced.
fn write(root: &Path, path: Option<&str>, compiled: &Compiled) -> Result<Written, String> {
    let (Some(path), Some(bytes)) = (path, &compiled.output) else {
        return Ok(Written::Nothing);
    };
    let Some(path) = ProjectPath::new(path) else {
        return Err(format!(
            "the output path `{path}` is not a path in the project"
        ));
    };
    let path = path.as_str().to_owned();
    if compiled.has_errors() {
        return Ok(Written::Skipped(path));
    }
    let message = |e: io::Error| format!("cannot write `{path}`: {e}");
    let (root, file) = resolve(root, &path).map_err(message)?;
    replace(&root, &file, bytes).map_err(message)?;
    Ok(Written::File(path))
}

/// The count in the names of temporary files, which tells apart the writes
/// of one process.
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// The file on disk that the output at `path`, a project path, is written
/// to, with the project directory: the directories on the way are created
/// if they are missing, and symbolic links among them are followed only to
/// directories inside the project. A link at the file itself is replaced,
/// not followed.
fn resolve(root: &Path, path: &str) -> io::Result<(PathBuf, PathBuf)> {
    let root = root.canonicalize()?;
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut dir = root.clone();
    let mut walked = String::new();
    for component in dirs.split('/').filter(|c| !c.is_empty()) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(component);
        dir = enter(&root, &dir.join(component), &walked)?;
    }
    Ok((root, dir.join(name)))
}

/// The directory to go on from at `next`, which the project path `walked`
/// names: `next` itself, created if it is missing, or the target of a link
/// there when that is a directory in the project at `root`.
fn enter(root: &Path, next: &Path, walked: &str) -> io::Result<PathBuf> {
    let not_dir = || io::Error::other(format!("`{walked}` is not a directory"));
    let mut created = false;
    loop {
        match fs::symlink_metadata(next) {
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
fn replace(root: &Path, file: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let (temporary, mut out) = loop {
        let n = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = file.with_file_name(format!(".{name}.{pid}-{n}.folio-tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(out) => break (temporary, out),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    // Where the temporary file really is, which the rename and the clean-up
    // go through, so a directory swapped after the check is not followed.
    let mut placed = temporary.clone();
    let result = out
        .write_all(bytes)
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

/// Where `folio build` reports what it found and did: to standard error,
/// as it goes, with source excerpts; or, with `--diagnostics json`, as one
/// JSON document written when the command ends (`docs/diagnostics-json.md`).
struct Report {
    color: bool,
    /// The document being collected, with `--diagnostics json`.
    json: Option<Document>,
}

/// The JSON document of `folio build --diagnostics json`, which
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
    /// The target, as the manifest names it.
    target: &'static str,
    /// The output's path in the project, if the build has one.
    path: Option<String>,
    /// What became of the output.
    status: Status,
}

impl Build {
    fn new(variant: &Option<Name>, target: Target, path: Option<&str>, status: Status) -> Self {
        Self {
            variant: variant.as_ref().map(|v| v.as_str().to_owned()),
            target: target.as_str(),
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
}

impl Report {
    /// The version of [`Document`]'s shape.
    const VERSION: u32 = 1;

    fn new(args: &BuildArgs) -> Self {
        Self {
            color: args.color.enabled(),
            json: (args.diagnostics == DiagnosticsFormat::Json).then(|| Document {
                version: Self::VERSION,
                ..Document::default()
            }),
        }
    }

    /// Reports diagnostics, with their spans resolved against `files`.
    fn diagnostics(&mut self, files: &dyn Files, diagnostics: &[Diagnostic]) {
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
            let name = build_name_text(build.variant.as_deref(), build.target);
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
    fn finish(self) {
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

    fn args(flags: &[&str]) -> BuildArgs {
        let mut all = vec!["folio", "build"];
        all.extend(flags);
        match Cli::parse_from(all).command {
            Command::Build(args) => args,
            other => panic!("{other:?}"),
        }
    }

    fn selected(manifest: &str, flags: &[&str]) -> Result<Vec<String>, String> {
        let world = World::new(manifest).unwrap();
        select(world.manifest(), &world.builds(), &args(flags))
            .map(|builds| {
                builds
                    .iter()
                    .map(|(v, t)| build_name(v.as_ref(), *t))
                    .collect()
            })
            .map_err(|e| e.message.to_string())
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
            write(dir.path(), Some("out/main-exam.pdf"), &ok),
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
            write(dir.path(), Some("out/x.pdf"), &broken),
            Ok(Written::Skipped("out/x.pdf".into()))
        );
        assert!(!dir.path().join("out/x.pdf").exists());
        assert_eq!(write(dir.path(), None, &ok), Ok(Written::Nothing));
        assert_eq!(
            write(dir.path(), Some("x.docx"), &Compiled::default()),
            Ok(Written::Nothing)
        );
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
                            write(root, Some("out/main.pdf"), &compiled),
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
        let message = |path: &str| write(root, Some(path), &ok).unwrap_err();

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
            assert_eq!(write(root, Some(path), &ok), Ok(Written::File(path.into())));
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
        assert!(write(root, Some("out/x.pdf"), &ok).is_ok());
        assert!(TEMPORARY.load(Ordering::Relaxed) > next + 1024);
        assert_eq!(fs::read(outside.path().join("victim")).unwrap(), b"keep");

        // Links to directories inside the project are followed.
        fs::create_dir(root.join("build")).unwrap();
        symlink(root.join("build"), root.join("pdfs")).unwrap();
        assert_eq!(
            write(root, Some("pdfs/main.pdf"), &ok),
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
