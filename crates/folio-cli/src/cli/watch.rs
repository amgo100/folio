//! `folio watch` (SPEC §12.2): rebuilds incrementally and serves the
//! preview until the process is stopped.
//!
//! It prints the preview's address, with its session token, to standard
//! output, and the diagnostics of each revision to standard error when
//! they differ from the last ones shown. Once the project has been
//! unchanged for a second, it writes the outputs of every build without
//! errors, as `folio build` does.
//!
//! When the project pins another compiler version, `folio` hands the
//! command to it before this runs (SPEC §2.4, `toolchain::dispatch`), so
//! the pinned version previews and writes the outputs. This version runs
//! the command when it is the pinned one, or with `--compiler current`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap::Args;
use folio::diag::TextFiles;
use folio::{Date, Diagnostic, Manifest, World};
use folio_preview::{Event, Options, Outputs, Preview};
use semver::Version;

use super::build::{Written, error, print, write};
use super::{Color, FAILURE, parse_date, project, toolchain};

/// The arguments of `folio watch`.
#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// The variant to preview [default: the first the manifest builds for
    /// PDF]
    #[arg(long, value_name = "VARIANT")]
    variant: Option<String>,

    /// The project directory, which holds folio.toml [default: the current
    /// directory or the nearest one above it that holds folio.toml]
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// The port to serve the preview on, on 127.0.0.1 [default: any free
    /// one]
    #[arg(
        long,
        value_name = "PORT",
        default_value_t = 0,
        hide_default_value = true
    )]
    port: u16,

    /// The date that scripts see as `today`, such as 2026-11-24; it
    /// replaces `project.today` of folio.toml
    #[arg(long, value_name = "DATE", value_parser = parse_date)]
    today: Option<Date>,

    /// Use this compiler instead of the version folio.toml pins; `current`
    /// is the running one
    #[arg(long, value_name = "VERSION", value_parser = ["current"])]
    compiler: Option<String>,

    /// When to colour diagnostics
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    color: Color,
}

/// Runs `folio watch`.
pub(crate) fn watch(args: &WatchArgs) -> ExitCode {
    let color = args.color.enabled();
    let fail = |message: String| {
        print(&TextFiles::new(), &[error(message)], color);
        ExitCode::from(FAILURE)
    };
    let root = match project::find_root(args.root.as_deref()) {
        Ok(root) => root,
        Err(message) => return fail(message),
    };
    let current = args.compiler.is_some();
    let Ok(pin) = check_pin(&root, current, color) else {
        return ExitCode::from(FAILURE);
    };
    let stopped = AtomicBool::new(false);

    let shown: Arc<Mutex<Option<Shown>>> = Arc::default();
    let mut options = Options::new(&root)
        .port(args.port)
        .today(args.today)
        .on_event(move |event| updated(event, &shown, color))
        .on_outputs({
            let root = root.clone();
            move |outputs| {
                // Once the pin changes, this version no longer writes what
                // `folio build` would.
                if !current && !stopped.load(Ordering::Relaxed) {
                    if pin_changed(&root, pin.as_ref(), color) {
                        stopped.store(true, Ordering::Relaxed);
                    } else {
                        write_outputs(&root, outputs, color);
                    }
                }
            }
        });
    if let Some(variant) = &args.variant {
        options = options.variant(variant);
    }
    let preview = match Preview::start(options) {
        Ok(preview) => preview,
        Err(e) => return fail(e.to_string()),
    };
    println!("Preview at {}", preview.url());
    println!("Press Ctrl-C to stop.");
    match preview.wait() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e.to_string()),
    }
}

/// Checks the compiler version that the manifest pins, as `folio build`
/// does when it runs the build itself (`toolchain::check`), and prints the
/// result. It is an error, `Err`, for another pin without `--compiler
/// current`, which `folio` hands to the pinned version when it is
/// installed. Otherwise returns the pin, or `None` for a manifest without a
/// valid one, which the preview reports.
fn check_pin(root: &Path, current: bool, color: bool) -> Result<Option<Version>, ()> {
    let Ok(text) = std::fs::read_to_string(root.join(project::MANIFEST)) else {
        return Ok(None);
    };
    let Some(pin) = Manifest::compiler_pin(&text, World::MANIFEST) else {
        return Ok(None);
    };
    if let Some(diagnostic) = toolchain::check_version(&pin, current) {
        let failed = diagnostic.is_error();
        let mut files = TextFiles::new();
        files.insert(World::MANIFEST, project::MANIFEST, text);
        print(&files, &[diagnostic], color);
        if failed {
            return Err(());
        }
    }
    Ok(Some(pin.v))
}

/// Whether the manifest on disk pins another compiler version than it did
/// when the command started (or, without a valid pin then, another than this
/// version); if so, prints an error that the outputs are no longer written.
/// A manifest without a valid pin has not changed it: the preview keeps the
/// last valid manifest and reports the new one's errors.
fn pin_changed(root: &Path, started: Option<&Version>, color: bool) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join(project::MANIFEST)) else {
        return false;
    };
    let Some(pin) = Manifest::compiler_pin(&text, World::MANIFEST) else {
        return false;
    };
    let unchanged = match started {
        Some(started) => *started == pin.v,
        None => pin.v.to_string() == folio::VERSION,
    };
    if unchanged {
        return false;
    }
    let error = Diagnostic::error(
        pin.span,
        format!(
            "the project now pins compiler {}, but this is Folio {}: the outputs are no \
         longer written",
            pin.v,
            folio::VERSION
        ),
    )
    .with_hint("restart `folio watch` to preview and write them with the pinned version");
    let mut files = TextFiles::new();
    files.insert(World::MANIFEST, project::MANIFEST, text);
    print(&files, &[error], color);
    true
}

/// What the terminal last showed of a revision: its diagnostics and
/// banner.
type Shown = (Vec<Diagnostic>, Option<String>);

/// Prints a revision's diagnostics and banner when they differ from the
/// last shown.
fn updated(event: &Event, shown: &Mutex<Option<Shown>>, color: bool) {
    let Event::Updated(update) = event else {
        return;
    };
    let mut shown = shown.lock().unwrap_or_else(|e| e.into_inner());
    if shown
        .as_ref()
        .is_some_and(|(d, b)| *d == update.diagnostics && *b == update.banner)
    {
        return;
    }
    print(&*update.files, &update.diagnostics, color);
    let errors = update.diagnostics.iter().filter(|d| d.is_error()).count();
    let warnings = update.diagnostics.len() - errors;
    match &update.banner {
        Some(banner) => eprintln!("{banner}"),
        None => eprintln!(
            "{} page(s), {errors} error(s), {warnings} warning(s)",
            update.pages
        ),
    }
    *shown = Some((update.diagnostics.clone(), update.banner.clone()));
}

/// Writes the outputs of a quiet revision, as `folio build` does: only
/// those of builds without errors.
fn write_outputs(root: &Path, outputs: &Outputs, color: bool) {
    for (output, path) in &outputs.outputs {
        let Ok(compiled) = &output.result else {
            continue;
        };
        match write(root, path.as_deref(), compiled) {
            Ok(Written::File(path)) => eprintln!("wrote {path}"),
            Ok(Written::Nothing | Written::Skipped(_)) => {}
            Err(message) => print(&*outputs.files, &[error(message)], color),
        }
    }
}
