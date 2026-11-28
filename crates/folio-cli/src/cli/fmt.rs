//! `folio fmt` (SPEC §12.1): formats the `.fol` and `.fsty` files of a
//! project canonically ([`folio_syntax::fmt`]).
//!
//! It reads the project's files as `folio build` does, so it formats the
//! sources that a build reads, and rewrites each one that formatting
//! changes, through a temporary file that takes the file's permissions and
//! keeps its byte order mark; a read-only file is an error. Like `folio
//! build`, it runs the compiler version that `folio.toml` pins (SPEC §2.4).
//! A file with syntax errors is left as it is and its errors are reported;
//! one that is not UTF-8, cannot be read or is a link out of the project
//! is left as it is with a warning.
//! With `--check`, no file changes: each file that is not formatted is
//! reported as an error with the code `unformatted` and a fix whose edits
//! format it. The exit status is 1 when there are errors, and with
//! `--check` when a file is not formatted.

use std::collections::HashSet;
use std::fs;
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
    // A source the loader could not take is not formatted. A build that
    // names it reports why; this command runs none, so it says so itself.
    for (path, why) in world.unreadable_sources() {
        let mut warning = Diagnostic::warning(
            DiagSpan::detached(),
            format!("cannot read `{}`: {why}", path.as_str()),
        )
        .with_hint("`folio fmt` leaves it as it is");
        if let Some(hint) = why.hint() {
            warning = warning.with_hint(hint);
        }
        diagnostics.push(warning);
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
    // A file that several links inside the project lead to is a source
    // under each of their paths; it is formatted, or reported, once.
    let mut seen = HashSet::new();
    for path in paths {
        let Some(source) = world.source(&path) else {
            continue;
        };
        if let Ok(file) = root.join(&path).canonicalize() {
            if file.extension() != Path::new(&path).extension() {
                diagnostics.push(Diagnostic::warning(
                    DiagSpan::from_range(source.id(), 0..0),
                    format!("{path} is a link to a file of another kind, which is not formatted"),
                ));
                continue;
            }
            if !seen.insert(file) {
                continue;
            }
        }
        let id = source.id();
        let edits = match fmt::format(source.root()) {
            Ok(edits) => edits,
            Err(FormatError::Syntax) => {
                let (errors, _) = source.root().errors_and_warnings();
                // The first error says what becomes of the file.
                diagnostics.extend(errors.into_iter().enumerate().map(|(n, e)| {
                    let error = Diagnostic::from(e);
                    match n {
                        0 => error.with_hint(format!(
                            "`folio fmt` leaves {path} as it is until it parses"
                        )),
                        _ => error,
                    }
                }));
                continue;
            }
            Err(FormatError::Unsound) => {
                diagnostics.push(Diagnostic::error(
                    DiagSpan::from_range(id, 0..0),
                    format!(
                        "formatting {path} would change what it means, so it is left as it \
                         is; this is a bug in Folio"
                    ),
                ));
                continue;
            }
        };
        if edits.is_empty() {
            continue;
        }
        if args.check {
            let first = edits.iter().next().expect("not empty");
            let mut unformatted = Diagnostic::error(
                DiagSpan::from_range(id, first.range.clone()),
                format!("{path} is not formatted"),
            )
            .with_code(UNFORMATTED)
            .with_hint("run `folio fmt` to format it");
            // Byte ranges count in the file, past a byte order mark
            // (`diag::resolve_in_file`), so the fix applies to it as it is.
            let mut fix = Fix::new("Format the file");
            for edit in &edits {
                fix = fix.with_edit(
                    DiagSpan::from_range(id, edit.range.clone()),
                    edit.text.clone(),
                );
            }
            unformatted = unformatted.with_fix(fix);
            diagnostics.push(unformatted);
            continue;
        }
        let text = edits.apply(source.text());
        match write(&canonical, &root.join(&path), source.text(), &text) {
            Ok(()) => formatted.push(path),
            Err(message) => diagnostics.push(error(format!("cannot write {path}: {message}"))),
        }
    }

    report.diagnostics(&world, &diagnostics);
    if !report.is_json() {
        for path in &formatted {
            errln!("formatted {path}");
        }
    }
    diagnostics.iter().any(Diagnostic::is_error)
}

/// The byte order mark, which the loader leaves out of a source's text.
const BOM: &str = "\u{FEFF}";

/// Replaces the file at `path`, or the file a link there leads to, with
/// `text`, the formatted `old`, through a temporary file beside it that
/// takes the file's permissions. Keeps a byte order mark the file starts
/// with. Refuses a file that cannot be written to, and one that no longer
/// holds `old`, which another program changed since it was read.
fn write(root: &Path, path: &Path, old: &str, text: &str) -> Result<(), String> {
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
    // Whether this process may write it, which the permissions alone do not
    // tell.
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .map_err(|e| e.to_string())?;
    let now = fs::read(&file).map_err(|e| e.to_string())?;
    let (bom, now) = match now.strip_prefix(BOM.as_bytes()) {
        Some(rest) => (BOM, rest),
        None => ("", &now[..]),
    };
    if now != old.as_bytes() {
        return Err("it changed while it was being formatted".into());
    }
    let bytes = format!("{bom}{text}");
    replace(root, &file, bytes.as_bytes(), Some(permissions)).map_err(|e| e.to_string())
}
