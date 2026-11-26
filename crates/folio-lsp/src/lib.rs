//! The Folio language server (SPEC §12.3): `folio lsp`.
//!
//! The server speaks the Language Server Protocol over a
//! [`Connection`](lsp_server::Connection), synchronously, without an async
//! runtime. It loads the project with the library's loader
//! ([`folio::loader`]), applies the client's unsaved buffers on top of it as
//! source edits, and checks every build the manifest declares
//! ([`folio::check_builds`]) on a thread of its own after each edit. It
//! publishes their diagnostics, resolved against the snapshot taken with the
//! check ([`folio::World::snapshot`]), for every file they are in.
//!
//! An edit cancels the check still running for the previous revision
//! through the query database (SPEC §2.3), so diagnostics never wait for
//! stale work, and the result of a check that an edit overtook is dropped.
//! Edits that arrive together are applied together before the next check
//! starts. The world is long-lived, so a panic in a query is published as
//! an `internal-error` diagnostic and the server keeps running (SPEC §12.2).
//!
//! When the project pins a compiler version other than this one
//! ([`folio::VERSION`]), the server says so with a notice, and checks with
//! its own version all the same (SPEC §2.4).
//!
//! ```no_run
//! let (connection, io_threads) = lsp_server::Connection::stdio();
//! folio_lsp::run(&connection, folio_lsp::Options::default()).unwrap();
//! drop(connection);
//! io_threads.join().unwrap();
//! ```

#![forbid(unsafe_code)]

mod convert;
mod server;

use std::fmt::{self, Debug, Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub use self::server::run;

/// How the server runs.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Options {
    /// The project directory, which holds `folio.toml`. By default the
    /// nearest directory that holds one, from the client's workspace folder
    /// up, or else from the first file the client opens up.
    pub root: Option<PathBuf>,
    /// Whether to read the fonts installed on the system, as `folio build`
    /// does. On by default.
    pub system_fonts: Option<bool>,
    /// Called on every [`Event`], on the thread where it happens, for tests
    /// and profiling. The hook must not block the server's own thread.
    pub on_event: Option<Hook>,
}

impl Options {
    /// Sets the project directory.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Sets whether to read the system's fonts.
    pub fn system_fonts(mut self, read: bool) -> Self {
        self.system_fonts = Some(read);
        self
    }

    /// Sets the event hook.
    pub fn on_event(mut self, hook: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        self.on_event = Some(Arc::new(hook));
        self
    }
}

impl Debug for Options {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("root", &self.root)
            .field("system_fonts", &self.system_fonts)
            .field("on_event", &self.on_event.as_ref().map(|_| ".."))
            .finish()
    }
}

/// A hook that sees the server's [`Event`]s ([`Options::on_event`]).
pub type Hook = Arc<dyn Fn(&Event) + Send + Sync>;

/// What the server reports to the hook set with [`Options::on_event`].
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// An event of the project's world: a query runs, or an edit cancels
    /// the queries running on other handles.
    World(folio::Event),
    /// The server has changed the world, so the revision it checks next is
    /// `revision`.
    Edited {
        /// The new revision.
        revision: u64,
    },
    /// The check of a revision has started.
    Started {
        /// Its revision.
        revision: u64,
    },
    /// The check of a revision has ended.
    Checked {
        /// Its revision.
        revision: u64,
        /// What became of it.
        outcome: Outcome,
    },
}

/// What became of the check of a revision.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Outcome {
    /// Its diagnostics were published.
    Published,
    /// An edit cancelled it while it ran.
    Cancelled,
    /// It finished after an edit, so its diagnostics were stale and were
    /// dropped.
    Stale,
}

/// Why the server stopped other than at the client's request.
#[derive(Debug)]
pub struct Error(String);

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<lsp_server::ProtocolError> for Error {
    fn from(error: lsp_server::ProtocolError) -> Self {
        Self(error.to_string())
    }
}
