//! `folio build` (SPEC §12.1): builds the variants and targets of a project
//! and writes their outputs where `[output] path` says (SPEC §6.4).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use folio::diag::{Files, TextFiles};
use folio::{
    Compiled, Diagnostic, FontCache, Manifest, Name, ProjectPath, Target, World, compile_builds,
};
use folio_syntax::DiagSpan;

use super::project::{self, LOCKFILE, MANIFEST};
use super::{BuildArgs, FAILURE, toolchain};

/// Runs `folio build`.
pub(crate) fn build(args: &BuildArgs) -> ExitCode {
    let color = args.color.enabled();
    let fail = |message: String| {
        print(&TextFiles::new(), &[error(message)], color);
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
            print(&files, &errors, color);
            return ExitCode::from(FAILURE);
        }
    };

    // Problems found before compiling, which the builds' follow.
    let mut diagnostics = Vec::new();
    let builds = match select(world.manifest(), &world.builds(), args) {
        Ok(builds) => builds,
        Err(error) => {
            print(&world, &[error], color);
            return ExitCode::from(FAILURE);
        }
    };
    diagnostics.extend(toolchain::check(world.manifest(), current));

    // The outputs of every build, which are not inputs of the next.
    let outputs: Vec<_> = world
        .builds()
        .iter()
        .filter_map(|(variant, target)| world.manifest().output_path(variant.as_ref(), *target))
        .filter_map(|path| ProjectPath::new(&path))
        .collect();
    let outputs: Vec<&str> = outputs.iter().map(ProjectPath::as_str).collect();
    let loaded = project::read_files(&root, &outputs, &mut diagnostics)
        .map_err(|e| format!("cannot read the project directory: {e}"))
        .and_then(|files| {
            project::load(&mut world, files, &mut diagnostics).map_err(|e| e.to_string())
        });
    if let Err(message) = loaded {
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
    let mut written = Vec::new();
    for output in outputs {
        let name = build_name(output.variant.as_ref(), output.target);
        let compiled = match output.result {
            Ok(compiled) => compiled,
            Err(cancelled) => {
                diagnostics.push(error(format!("the build of {name} failed: {cancelled}")));
                continue;
            }
        };
        for diagnostic in &compiled.diagnostics {
            if seen.insert(diagnostic.clone()) {
                diagnostics.push(diagnostic.clone());
            }
        }
        let path = world
            .manifest()
            .output_path(output.variant.as_ref(), output.target);
        written.push((name, path, compiled));
    }
    print(&world, &diagnostics, color);

    let mut failed = diagnostics.iter().any(Diagnostic::is_error);
    for (name, path, compiled) in written {
        match write(&root, path.as_deref(), &compiled) {
            Ok(Written::File(path)) => eprintln!("wrote {path}"),
            Ok(Written::Nothing) => {}
            Ok(Written::Skipped(path)) => {
                eprintln!("did not write {path}: the build of {name} has errors");
            }
            Err(message) => {
                print(&world, &[error(message)], color);
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
/// then renamed over the output. Symbolic links are followed only inside
/// the project (SPEC §12.1); an output whose directory or file lies outside
/// it is refused.
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
    let file = resolve(root, &path).map_err(|e| format!("cannot write `{path}`: {e}"))?;
    replace(&file, bytes).map_err(|e| format!("cannot write `{path}`: {e}"))?;
    Ok(Written::File(path))
}

/// The file on disk that the output at `path`, a project path, is written
/// to: the directories on the way are created if they are missing, and
/// symbolic links among them, or at the file, are followed only to
/// directories and files inside the project.
fn resolve(root: &Path, path: &str) -> io::Result<PathBuf> {
    let root = root.canonicalize()?;
    let outside = |what: &str| {
        io::Error::other(format!(
            "{what} is a symbolic link to a place outside the project"
        ))
    };
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    let mut dir = root.clone();
    let mut walked = String::new();
    for component in dirs.split('/').filter(|c| !c.is_empty()) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(component);
        let next = dir.join(component);
        match fs::symlink_metadata(&next) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&next) {
                    Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                    _ => {}
                }
                // Another build may have created it, or put a link there,
                // in the meantime: check what is there now.
                if !fs::symlink_metadata(&next)?.is_dir() {
                    return Err(io::Error::other(format!("`{walked}` is not a directory")));
                }
                dir = next;
            }
            Err(e) => return Err(e),
            Ok(meta) if meta.is_symlink() => {
                let target = next.canonicalize().map_err(|_| {
                    io::Error::other(format!("`{walked}` is a broken symbolic link"))
                })?;
                if !target.starts_with(&root) {
                    return Err(outside(&format!("`{walked}`")));
                }
                if !target.is_dir() {
                    return Err(io::Error::other(format!("`{walked}` is not a directory")));
                }
                dir = target;
            }
            Ok(meta) if meta.is_dir() => dir = next,
            Ok(_) => return Err(io::Error::other(format!("`{walked}` is not a directory"))),
        }
    }
    let file = dir.join(name);
    match fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_symlink() => {
            let target = file
                .canonicalize()
                .map_err(|_| io::Error::other("it is a broken symbolic link"))?;
            if !target.starts_with(&root) {
                return Err(outside("it"));
            }
            Ok(target)
        }
        _ => Ok(file),
    }
}

/// Replaces `file` whole with `bytes`, through a temporary file beside it
/// whose name no other write uses: it holds this process's id and a count,
/// and is created only if nothing, not even a symbolic link, has that name.
fn replace(file: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let (temporary, mut out) = loop {
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
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
    out.write_all(bytes)
        .and_then(|()| {
            drop(out);
            fs::rename(&temporary, file)
        })
        .inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })
}

/// An error that points at no source.
pub(super) fn error(message: String) -> Diagnostic {
    Diagnostic::error(DiagSpan::detached(), message)
}

/// Prints diagnostics to standard error.
pub(super) fn print(files: &dyn Files, diagnostics: &[Diagnostic], color: bool) {
    eprint!("{}", folio::diag::render(files, diagnostics, color));
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

        // An output file that links out of the project.
        fs::remove_file(root.join("out")).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        fs::write(outside.path().join("victim"), "keep").unwrap();
        symlink(outside.path().join("victim"), root.join("out/main.pdf")).unwrap();
        assert_eq!(
            message("out/main.pdf"),
            "cannot write `out/main.pdf`: it is a symbolic link to a place outside the project"
        );
        assert_eq!(fs::read(outside.path().join("victim")).unwrap(), b"keep");

        // A link that points nowhere, which writing through would create.
        symlink(
            outside.path().join("nothing"),
            root.join("out/dangling.pdf"),
        )
        .unwrap();
        assert_eq!(
            message("out/dangling.pdf"),
            "cannot write `out/dangling.pdf`: it is a broken symbolic link"
        );
        assert!(!outside.path().join("nothing").exists());

        // A temporary name taken by a link is passed over, not written
        // through. Other tests in this process advance the count, so the
        // first names are all taken.
        for n in 0..1024 {
            symlink(
                outside.path().join("victim"),
                root.join(format!("out/.x.pdf.{}-{n}.folio-tmp", std::process::id())),
            )
            .unwrap();
        }
        assert!(write(root, Some("out/x.pdf"), &ok).is_ok());
        assert_eq!(fs::read(outside.path().join("victim")).unwrap(), b"keep");

        // Links inside the project are followed.
        fs::create_dir(root.join("build")).unwrap();
        symlink(root.join("build"), root.join("pdfs")).unwrap();
        assert_eq!(
            write(root, Some("pdfs/main.pdf"), &ok),
            Ok(Written::File("pdfs/main.pdf".into()))
        );
        assert_eq!(fs::read(root.join("build/main.pdf")).unwrap(), b"%PDF-");
        symlink(root.join("build/main.pdf"), root.join("out/alias.pdf")).unwrap();
        assert!(write(root, Some("out/alias.pdf"), &ok).is_ok());
        assert!(
            fs::symlink_metadata(root.join("out/alias.pdf"))
                .unwrap()
                .is_symlink()
        );

        // A file where a directory is needed.
        fs::write(root.join("file"), "").unwrap();
        assert_eq!(
            message("file/main.pdf"),
            "cannot write `file/main.pdf`: `file` is not a directory"
        );
    }
}
