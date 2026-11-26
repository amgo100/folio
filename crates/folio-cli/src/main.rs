//! The `folio` command-line interface (SPEC §12.1).

#![forbid(unsafe_code)]

mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
