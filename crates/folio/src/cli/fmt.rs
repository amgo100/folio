//! `folio fmt` (SPEC §12.1): formats the `.fol` and `.fsty` files of a
//! project canonically ([`folio_syntax::fmt`]).
//!
//! It reads the project's files as `folio build` does, so it formats the
//! sources that a build reads, and rewrites each one that formatting
//! changes, through a temporary file that takes the file's permissions and
//! keeps its byte order mark; a read-only file is an error. Like `folio
//! build`, it runs the compiler version that `folio.toml` pins (SPEC §2.4).
//! A file with syntax errors is left as it is and its errors are reported. With `--check`, no file changes:
//! each file that is not formatted is reported as an error with the code
//! `unformatted` and a fix whose edits format it. The exit status is 1 when
//! there are errors, and with `--check` when a file is not formatted.

use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::process::ExitCode;

use folio::loader::{LoadError, Loader};
use folio::{Diagnostic, Fix, World};
use folio_syntax::DiagSpan;
use folio_syntax::fmt::{self, FormatError};

use super::FmtArgs;
use super::build::{Report, error, replace};
use super::project;
use super::{FAILURE, toolchain};

/// The code of the error that `--check` reports for a file that is not
/// formatted.
const UNFORMATTED: &str = "unformatted";

/// Runs `folio fmt`.
pub(crate) fn fmt(args: &FmtArgs) -> ExitCode {
    let mut report = Report::with(args.color, args.diagnostics);
    let failed = run(args, &mut report);
    report.finish();
    if failed {
        ExitCode::from(FAILURE)
    } else {
        ExitCode::SUCCESS
    }
}

/// Formats or checks the project's files; whether there were errors.
fn run(args: &FmtArgs, report: &mut Report) -> bool {
    let mut fail = |message: String| {
        report.diagnostics(&folio::diag::TextFiles::new(), &[error(message)]);
        true
    };
    let root = match project::find_root(args.root.as_deref()) {
        Ok(root) => root,
        Err(message) => return fail(message),
    };
    let mut loader = match Loader::new(&root) {
        Ok(loader) => loader,
        Err(e) => return fail(e.to_string()),
    };
    let mut world = match loader.world(World::builder()) {
        Ok(world) => world,
        Err(LoadError::Project(errors)) => {
            report.diagnostics(&errors.files, &errors.diagnostics);
            return true;
        }
        Err(e) => return fail(e.to_string()),
    };
    // The pinned compiler formats, as it builds (SPEC §2.4); a project
    // that pins another version was handed to it before the arguments were
    // parsed (`toolchain::dispatch`).
    let mut diagnostics: Vec<Diagnostic> =
        toolchain::check(world.manifest(), args.compiler.is_some())
            .into_iter()
            .collect();
    if diagnostics.iter().any(Diagnostic::is_error) {
        report.diagnostics(&world, &diagnostics);
        return true;
    }
    if let Err(e) = loader.load(&mut world, &mut diagnostics) {
        return fail(e.to_string());
    }
    let canonical = match root.canonicalize() {
        Ok(path) => path,
        Err(e) => return fail(format!("cannot read the project directory: {e}")),
    };

    let mut paths: Vec<String> = world
        .source_paths()
        .map(|p| p.as_str().to_owned())
        .collect();
    paths.sort();
    let mut formatted = Vec::new();
    for path in paths {
        let Some(source) = world.source(&path) else {
            continue;
        };
        let id = source.id();
        let edits = match fmt::format(source.root()) {
            Ok(edits) => edits,
            Err(FormatError::Syntax) => {
                let (errors, _) = source.root().errors_and_warnings();
                diagnostics.extend(errors.into_iter().map(|e| {
                    Diagnostic::from(e).with_hint(format!(
                        "`folio fmt` leaves {path} as it is until it parses"
                    ))
                }));
                continue;
            }
            Err(FormatError::Unsound) => {
                diagnostics.push(error(format!(
                    "formatting {path} would change what it means, so it is left as it is; \
                     this is a bug in Folio"
                )));
                continue;
            }
        };
        if edits.is_empty() {
            continue;
        }
        if args.check {
            let first = edits.iter().next().expect("not empty");
            let mut fix = Fix::new("Format the file");
            for edit in &edits {
                fix = fix.with_edit(
                    DiagSpan::from_range(id, edit.range.clone()),
                    edit.text.clone(),
                );
            }
            diagnostics.push(
                Diagnostic::error(
                    DiagSpan::from_range(id, first.range.clone()),
                    format!("{path} is not formatted"),
                )
                .with_code(UNFORMATTED)
                .with_hint("run `folio fmt` to format it")
                .with_fix(fix),
            );
            continue;
        }
        let text = edits.apply(source.text());
        match write(&canonical, &root.join(&path), &text) {
            Ok(()) => formatted.push(path),
            Err(message) => diagnostics.push(error(format!("cannot write {path}: {message}"))),
        }
    }

    report.diagnostics(&world, &diagnostics);
    if !report.is_json() {
        for path in &formatted {
            eprintln!("formatted {path}");
        }
    }
    diagnostics.iter().any(Diagnostic::is_error)
}

/// The byte order mark, which the loader leaves out of a source's text.
const BOM: &str = "\u{FEFF}";

/// Replaces the file at `path`, or the file a link there leads to, with
/// `text`, through a temporary file beside it that takes the file's
/// permissions. Keeps a byte order mark the file starts with; refuses a
/// read-only file.
fn write(root: &Path, path: &Path, text: &str) -> Result<(), String> {
    let file = path.canonicalize().map_err(|e| e.to_string())?;
    if !file.starts_with(root) {
        return Err("it leads out of the project".into());
    }
    let permissions = fs::metadata(&file)
        .map_err(|e| e.to_string())?
        .permissions();
    if permissions.readonly() {
        return Err("it is read-only".into());
    }
    let mut start = [0; BOM.len()];
    let bom = fs::File::open(&file)
        .and_then(|mut f| f.read_exact(&mut start))
        .is_ok_and(|()| start == BOM.as_bytes());
    let bytes = match bom {
        true => format!("{BOM}{text}"),
        false => text.to_owned(),
    };
    replace(root, &file, bytes.as_bytes(), Some(permissions)).map_err(|e| e.to_string())
}
