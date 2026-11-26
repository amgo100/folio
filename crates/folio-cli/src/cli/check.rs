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

use folio::LayoutDiagnostics;

use super::ProjectArgs;
use super::build::{Mode, run_command};

/// Runs `folio check`, with every layout diagnostic when `layout` is set.
pub(crate) fn check(args: &ProjectArgs, layout: bool) -> ExitCode {
    let layout = if layout {
        LayoutDiagnostics::All
    } else {
        LayoutDiagnostics::Serious
    };
    run_command(args, Mode::Check, layout)
}
