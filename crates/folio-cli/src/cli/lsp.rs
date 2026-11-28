//! `folio lsp` (SPEC §12.3): runs the language server over standard input
//! and output until the client shuts it down.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use lsp_server::Connection;

use super::FAILURE;

/// The arguments of `folio lsp`.
#[derive(Debug, Args)]
pub(crate) struct LspArgs {
    /// The project directory, which holds folio.toml [default: the nearest
    /// one from the editor's workspace folder up, or from the first file it
    /// opens]
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Talk to the editor over standard input and output, the only way
    /// there is; editors' clients pass it
    #[arg(long)]
    stdio: bool,
}

/// Runs `folio lsp`.
pub(crate) fn lsp(args: &LspArgs) -> ExitCode {
    let _ = args.stdio;
    let mut options = folio_lsp::Options::default();
    if let Some(root) = &args.root {
        options = options.root(root);
    }
    let (connection, io_threads) = Connection::stdio();
    let result = folio_lsp::run(&connection, options);
    // Standard output closes once the connection is gone.
    drop(connection);
    let joined = io_threads.join();
    match (result, joined) {
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
        (Err(e), _) => {
            errln!("error: the language server stopped: {e}");
            ExitCode::from(FAILURE)
        }
        (_, Err(e)) => {
            errln!("error: the language server lost its connection: {e}");
            ExitCode::from(FAILURE)
        }
    }
}
