//! `folio watch` (SPEC §12.2): rebuilds incrementally and serves the
//! preview until the process is stopped.
//!
//! It prints the preview's address, with its session token, to standard
//! output, and the diagnostics of each revision to standard error when
//! they differ from the last ones shown. Once the project has been
//! unchanged for a second, it writes the outputs of every build without
//! errors, as `folio build` does.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Args;
use folio::{Date, Diagnostic, Manifest};
use folio_preview::{Event, Options, Outputs, Preview};

use super::build::{Written, error, print, write};
use super::{Color, FAILURE, parse_date, project};

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

    /// When to colour diagnostics
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    color: Color,
}

/// Runs `folio watch`.
pub(crate) fn watch(args: &WatchArgs) -> ExitCode {
    let color = args.color.enabled();
    let fail = |message: String| {
        print(&folio::diag::TextFiles::new(), &[error(message)], color);
        ExitCode::from(FAILURE)
    };
    let root = match project::find_root(args.root.as_deref()) {
        Ok(root) => root,
        Err(message) => return fail(message),
    };
    notice_pin(&root);

    let shown: Arc<Mutex<Option<Shown>>> = Arc::default();
    let mut options = Options::new(&root)
        .port(args.port)
        .today(args.today)
        .on_event(move |event| updated(event, &shown, color))
        .on_outputs({
            let root = root.clone();
            move |outputs| write_outputs(&root, outputs, color)
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

/// Warns when the project pins another compiler version: the preview
/// runs this one all the same, as the language server does.
fn notice_pin(root: &Path) {
    let Ok(text) = std::fs::read_to_string(root.join(project::MANIFEST)) else {
        return;
    };
    if let Some(pin) = Manifest::compiler_pin(&text, folio::World::MANIFEST)
        && pin.v.to_string() != folio::VERSION
    {
        eprintln!(
            "warning: this project pins Folio {}, but this is Folio {}; \
             the preview may differ from what `folio build` writes",
            pin.v,
            folio::VERSION
        );
    }
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
