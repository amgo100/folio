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

use folio::diag::locate;
use folio::{BuildKey, LayoutDiagnostics, Override, OverrideForm, World};
use folio_syntax::DiagSpan;

use super::ProjectArgs;
use super::build::{Mode, Report, run_command_then};

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

/// Lists the local overrides of the checked builds on standard output, one
/// per line, by file and position:
/// `main.fol:3:2: override @markscheme figure <fig:ball>: space-before`.
fn report_overrides(world: &World, builds: &[BuildKey], _: &mut Report) -> bool {
    let Ok(list) = folio::overrides(world, builds.iter().cloned()) else {
        // Nothing edits the world of a command.
        return true;
    };
    for line in overrides_lines(world, &list) {
        outln!("{line}");
    }
    false
}

/// The lines of the overrides report, sorted by file and position.
fn overrides_lines(world: &World, list: &[Override]) -> Vec<String> {
    let mut rows: Vec<((String, usize), String)> = list
        .iter()
        .map(|item| {
            let (place, key) = match locate(world, DiagSpan::from(item.span)) {
                Some(at) => (
                    format!("{}:{}:{}", at.path, at.start.line, at.start.column),
                    (at.path.to_string(), at.bytes.start),
                ),
                None => ("<unknown>".to_owned(), (String::new(), 0)),
            };
            let mut line = format!("{place}: {}", item.form.name());
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
            (key, line)
        })
        .collect();
    rows.sort();
    rows.into_iter().map(|(_, line)| line).collect()
}
