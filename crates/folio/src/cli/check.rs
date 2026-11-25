//! `folio check` (SPEC §12.1): checks the variants and targets of a project
//! and reports their diagnostics without writing any output.
//!
//! It reads the project as `folio build` does and takes the same choice of
//! builds and the same diagnostics flags, but runs each build only up to
//! Emit ([`folio::check`]): the document is laid out and, for PDF, the
//! writer's check runs, so the diagnostics are those of `folio build`, but
//! no file is written. What would stop an output from being written where
//! `[output] path` puts it is an error, as for `folio build`, but no
//! directory is created. With `--diagnostics json`, each build of the
//! document has the status `checked` and no path.

use std::process::ExitCode;

use super::ProjectArgs;
use super::build::{Mode, run_command};

/// Runs `folio check`.
pub(crate) fn check(args: &ProjectArgs) -> ExitCode {
    run_command(args, Mode::Check)
}
