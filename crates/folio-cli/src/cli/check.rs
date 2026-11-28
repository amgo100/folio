//! `folio check` (SPEC §12.1): checks the variants and targets of a project
//! and reports their diagnostics without writing any output.
//!
//! It reads the project as `folio build` does and takes the same choice of
//! builds and the same diagnostics flags, but runs each build only up to
//! Emit ([`folio::check`]): the document is laid out and, for PDF, the
//! writer's check runs, so the diagnostics are those of `folio build`, but
//! no file is written. An output path that leaves the project, or whose
//! directories a link places outside it, is an error, as for `folio
//! build`, but no directory is created; what only writing finds, such as
//! a directory that cannot be written to, is not checked. With
//! `--diagnostics json`, each build of the document has the status
//! `checked` and no path.
//!
//! Like `folio build`, it leaves out the softer layout diagnostics
//! ([`folio::layout::DETAIL_CODES`]: underfull lines, relaxed widows,
//! orphans, keeps and unbreakable blocks) unless `--layout` asks for them
//! (SPEC §7.9).

use std::process::ExitCode;

use folio::diag::{Files, Location, locate};
use folio::{BuildKey, LayoutDiagnostics, Override, OverrideForm, World};
use folio_syntax::DiagSpan;

use super::ProjectArgs;
use super::build::{Mode, OverrideEntry, Report, run_command_then};

/// Runs `folio check`, with every layout diagnostic when `layout` is set,
/// and the list of local overrides when `overrides` is.
pub(crate) fn check(args: &ProjectArgs, layout: bool, overrides: bool) -> ExitCode {
    let layout = if layout {
        LayoutDiagnostics::All
    } else {
        LayoutDiagnostics::Serious
    };
    let report: super::build::Then<'_> = &report_overrides;
    run_command_then(args, Mode::Check, layout, overrides.then_some(report))
}

// ---- `--report overrides` (SPEC §5.4) ----

/// Lists the local overrides of the checked builds, by file and position:
/// in the JSON document's `overrides` with `--diagnostics json` (D-69-1),
/// and otherwise on standard output, one per line:
/// `main.fol:3:2: override @markscheme figure <fig:ball>: space-before`.
fn report_overrides(world: &World, builds: &[BuildKey], report: &mut Report) -> bool {
    let Ok(list) = folio::overrides(world, builds.iter().cloned()) else {
        // Nothing edits the world of a command.
        return true;
    };
    let mut rows: Vec<(Option<Location>, &Override)> = list
        .iter()
        .map(|item| (locate(world, DiagSpan::from(item.span)), item))
        .collect();
    // By file and position, then weakest form first.
    rows.sort_by(|(a, x), (b, y)| {
        let key = |l: &Option<Location>| l.as_ref().map(|l| (l.path.clone(), l.bytes.start));
        key(a).cmp(&key(b)).then(x.form.cmp(&y.form))
    });
    let entries = rows
        .iter()
        .map(|(location, item)| OverrideEntry {
            form: item.form.name(),
            // Byte ranges count from the file's first byte, as those of
            // diagnostics do.
            location: location.clone().map(|mut location| {
                let start = world.text_start(location.id);
                location.bytes = location.bytes.start + start..location.bytes.end + start;
                location
            }),
            element: item.subject.to_string(),
            label: item.label.as_ref().map(ToString::to_string),
            properties: item.properties.iter().map(ToString::to_string).collect(),
            scope: item.scope.as_ref().map(ToString::to_string),
        })
        .collect();
    if report.overrides(entries) {
        return false;
    }
    for (location, item) in rows {
        outln!("{}", overrides_line(location.as_ref(), item));
    }
    false
}

/// An override as a line of the report.
fn overrides_line(location: Option<&Location>, item: &Override) -> String {
    let mut line = match location {
        Some(at) => format!("{}:{}:{}", at.path, at.start.line, at.start.column),
        None => "<unknown>".to_owned(),
    };
    line.push_str(&format!(": {}", item.form.name()));
    if let Some(scope) = &item.scope {
        line.push_str(&format!(" @{scope}"));
    }
    // A styled span names itself.
    if item.form != OverrideForm::Styled {
        line.push_str(&format!(" {}", item.subject));
    }
    if let Some(label) = item.label.as_ref().filter(|_| item.form.in_content()) {
        line.push_str(&format!(" <{label}>"));
    }
    let properties: Vec<&str> = item.properties.iter().map(|p| p.as_str()).collect();
    line.push_str(&format!(": {}", properties.join(", ")));
    line
}
