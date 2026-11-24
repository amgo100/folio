//! The `folio` command-line interface (SPEC §12.1).

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("folio {}", folio::VERSION);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("folio {}: no commands are implemented yet", folio::VERSION);
            ExitCode::FAILURE
        }
    }
}
