//! `folio watch` (SPEC §12.2): incremental rebuilds and the preview server.
//!
//! [`Preview::start`] loads a project with the library's loader
//! ([`folio::loader`]) into a long-lived [`World`](folio::World) in watch
//! mode, which may use the fonts installed on the system, watches the
//! project directory, and serves a preview on `127.0.0.1`. File-system
//! events are debounced (10 ms by default) and mapped to updates of the
//! world; each batch that changes it starts a check of the previewed build
//! ([`folio::check`]) on a thread of its own, and an edit that arrives
//! meanwhile cancels it through the query database (SPEC §2.3). The pages
//! of a finished check are compared with the last ones sent by their
//! content hashes (`folio::Pages::changed` and `remapped`), and only the
//! display lists of pages that changed, or the span maps of pages whose
//! sources moved, are encoded and sent.
//!
//! Compilation is partial: a construct that fails is an error marker on
//! its page and the rest is shown. Only when nothing can be laid out, as
//! when the manifest or a stylesheet does not parse, do the last good
//! pages stay, with a banner that says why. Output files are written once
//! the project has been quiet for a second ([`Options::idle`]), through
//! [`Options::on_outputs`], never on every keystroke.
//!
//! # Security
//!
//! The server listens on `127.0.0.1` only. Every request must carry the
//! session token, 32 random bytes drawn when the server starts, as the
//! `token` query parameter, and name the server itself in its `Host`
//! header, which stops pages on other sites from reaching it through a
//! name that resolves to `127.0.0.1` (DNS rebinding). A websocket request
//! that carries an `Origin` header must come from the preview's own origin
//! (`http://127.0.0.1:<port>` or `http://localhost:<port>`); one without
//! is a program, not a page in a browser, and is let through on its token
//! alone. A request that fails any of these is answered with `403` and the
//! connection closed. The token is never logged by the server; the URL
//! that [`Preview::url`] gives holds it, for the person who started the
//! preview.
//!
//! # Protocol
//!
//! Version [`PROTOCOL`] of the protocol, for the viewer (#51) and other
//! clients. Every path takes `?token=<token>`.
//!
//! | Path | What it serves |
//! | --- | --- |
//! | `/` | A minimal viewer, with `/viewer.js` and `/viewer.css`: the pages as PNG images and the diagnostics, refreshed over the websocket |
//! | `/ws` | The websocket |
//! | `/page/<n>.png` | Page `n` (from 0) of the current revision, rendered by `folio-render` at 96 dpi |
//! | `/font/<n>` | The bytes of the font file that glyph runs name as font `n`, with the font's index in it in the `X-Font-Index` header |
//!
//! On the websocket, the server sends, for every revision the client has
//! not seen:
//!
//! 1. a text message, a JSON object `{"type": "update", "protocol": 1,
//!    "revision": r, "variant": v | null, "target": "pdf", "pages": n,
//!    "lists": [..], "spans": [..], "banner": text | null, "diagnostics":
//!    [..]}`: `pages` is how many pages the document has now (pages the
//!    client had beyond it are gone), `lists` the indices of the pages
//!    whose display lists follow, `spans` those whose span maps follow,
//!    `banner` why the pages shown are not of this revision, if they are
//!    not, and `diagnostics` the diagnostics of the previewed build,
//!    resolved as `folio check --diagnostics json` writes them
//!    (`docs/diagnostics-json.md`);
//! 2. one binary message per page in `lists` and `spans`, in that order:
//!    the page's index as four bytes, little-endian, then a message of
//!    `folio-render`'s wire format ([`folio_render::wire`]), a whole
//!    display list or a span map that replaces the spans of the list the
//!    client has for that page;
//! 3. a text message `{"type": "done", "revision": r}`.
//!
//! A new connection receives every page. Pages that the client says are
//! visible come first: it sends `{"type": "visible", "pages": [..]}`
//! whenever that changes. Other messages from the client are ignored, so
//! that later versions can add some. Fonts reach a client that draws
//! display lists itself through `/font/<n>`.

#![forbid(unsafe_code)]

mod engine;
mod http;
mod server;
mod watch;

use std::fmt::{self, Debug, Display, Formatter};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use folio::diag::Files;
use folio::{Diagnostic, Output};

/// The version of the preview's protocol, which the `update` message
/// carries.
pub const PROTOCOL: u32 = 1;

/// How the preview runs.
#[derive(Clone)]
#[non_exhaustive]
pub struct Options {
    /// The project directory, which holds `folio.toml`.
    pub root: PathBuf,
    /// The variant to preview. By default the first that the manifest
    /// declares for PDF.
    pub variant: Option<String>,
    /// The port to listen on, on `127.0.0.1`; `0`, the default, takes any
    /// free one.
    pub port: u16,
    /// Whether to read the fonts installed on the system, which watch mode
    /// allows. On by default.
    pub system_fonts: bool,
    /// Whether to watch the project directory for changes. On by default;
    /// off, only [`Preview::changed`] tells the preview what changed.
    pub watch: bool,
    /// How long the file system must be quiet before the changes it
    /// reported are read: 10 ms by default.
    pub debounce: Duration,
    /// How long the project must be unchanged before outputs are written:
    /// one second by default.
    pub idle: Duration,
    /// The date that scripts see as `today`, in place of `project.today`.
    pub today: Option<folio::Date>,
    /// Called with every build the manifest declares, compiled, once the
    /// project has been unchanged for [`Self::idle`] after a revision, to
    /// write them. Without it, nothing is written.
    pub on_outputs: Option<OutputsHook>,
    /// Called on every [`Event`], on the thread where it happens. It must
    /// not block.
    pub on_event: Option<Hook>,
}

impl Options {
    /// The defaults for the project in `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            variant: None,
            port: 0,
            system_fonts: true,
            watch: true,
            debounce: Duration::from_millis(10),
            idle: Duration::from_secs(1),
            today: None,
            on_outputs: None,
            on_event: None,
        }
    }

    /// Sets the variant to preview.
    pub fn variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    /// Sets the port.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Sets whether to read the system's fonts.
    pub fn system_fonts(mut self, read: bool) -> Self {
        self.system_fonts = read;
        self
    }

    /// Sets whether to watch the project directory.
    pub fn watch(mut self, watch: bool) -> Self {
        self.watch = watch;
        self
    }

    /// Sets the debounce interval.
    pub fn debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Sets how long the project must be unchanged before outputs are
    /// written.
    pub fn idle(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Sets the date scripts see as `today`.
    pub fn today(mut self, today: Option<folio::Date>) -> Self {
        self.today = today;
        self
    }

    /// Sets the hook that writes outputs.
    pub fn on_outputs(mut self, hook: impl Fn(&Outputs) + Send + Sync + 'static) -> Self {
        self.on_outputs = Some(Arc::new(hook));
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
            .field("variant", &self.variant)
            .field("port", &self.port)
            .field("system_fonts", &self.system_fonts)
            .field("watch", &self.watch)
            .field("debounce", &self.debounce)
            .field("idle", &self.idle)
            .field("today", &self.today)
            .field("on_outputs", &self.on_outputs.as_ref().map(|_| ".."))
            .field("on_event", &self.on_event.as_ref().map(|_| ".."))
            .finish()
    }
}

/// A hook that sees the preview's [`Event`]s ([`Options::on_event`]).
pub type Hook = Arc<dyn Fn(&Event) + Send + Sync>;

/// A hook that writes outputs ([`Options::on_outputs`]).
pub type OutputsHook = Arc<dyn Fn(&Outputs) + Send + Sync>;

/// The builds of a revision, compiled, for [`Options::on_outputs`].
#[non_exhaustive]
pub struct Outputs {
    /// The revision they were compiled from.
    pub revision: u64,
    /// Each build the manifest declares, in its order, with the project
    /// path the manifest writes its output to, if any. When an edit
    /// cancels any of them, none is handed over: the next revision is
    /// compiled once the project is quiet again.
    pub outputs: Vec<(Output, Option<String>)>,
    /// The files their diagnostics point into.
    pub files: Arc<dyn Files + Send + Sync>,
}

impl Debug for Outputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outputs")
            .field("revision", &self.revision)
            .field("outputs", &self.outputs)
            .finish_non_exhaustive()
    }
}

/// What the preview reports to the hook set with [`Options::on_event`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The world changed, so the revision checked next is `revision`.
    Edited {
        /// The new revision.
        revision: u64,
    },
    /// A revision was checked and sent to the clients.
    Updated(Arc<Update>),
    /// The check of a revision ended after an edit had overtaken it, so
    /// nothing was sent.
    Stale {
        /// Its revision.
        revision: u64,
    },
    /// The outputs of a revision were handed to [`Options::on_outputs`].
    Written {
        /// Its revision.
        revision: u64,
    },
}

/// A revision as it was sent ([`Event::Updated`]).
#[non_exhaustive]
pub struct Update {
    /// The revision.
    pub revision: u64,
    /// How many pages the preview shows.
    pub pages: usize,
    /// The pages whose display lists were sent again.
    pub lists: Vec<usize>,
    /// The pages whose span maps were sent again.
    pub spans: Vec<usize>,
    /// Why the pages shown are those of an earlier revision, if they are.
    pub banner: Option<String>,
    /// The diagnostics of the previewed build, and of the manifest and
    /// lockfile when they have errors.
    pub diagnostics: Vec<Diagnostic>,
    /// The files the diagnostics point into.
    pub files: Arc<dyn Files + Send + Sync>,
}

impl Debug for Update {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Update")
            .field("revision", &self.revision)
            .field("pages", &self.pages)
            .field("lists", &self.lists)
            .field("spans", &self.spans)
            .field("banner", &self.banner)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

/// Why the preview could not start.
#[derive(Debug)]
pub struct Error(String);

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A running preview. Dropping it stops the preview.
pub struct Preview {
    addr: SocketAddr,
    token: Arc<str>,
    engine: engine::Handle,
    server: Option<server::Handle>,
    _watcher: Option<watch::Watcher>,
    threads: Vec<JoinHandle<()>>,
}

impl Preview {
    /// Loads the project, starts watching it and serving the preview, and
    /// starts the first check. Fails when the project directory cannot be
    /// read or watched, or the port cannot be bound; a project with
    /// errors starts all the same and shows them.
    pub fn start(options: Options) -> Result<Self, Error> {
        let root = std::path::absolute(&options.root).unwrap_or_else(|_| options.root.clone());
        let token: Arc<str> = http::token().map_err(|e| Error(e.to_string()))?.into();
        // The watcher starts before the project is read, so that a change
        // saved meanwhile queues for the engine. Nothing runs until every
        // part has started, so a failure leaves nothing behind.
        let pending = engine::prepare(&root, &options)?;
        let engine = pending.handle.clone();
        let watcher = match options.watch {
            true => Some(watch::Watcher::start(&root, engine.clone())?),
            false => None,
        };
        let (server, addr, server_thread) =
            server::start(options.port, token.clone(), pending.published.clone())?;
        let engine_thread = match pending.spawn() {
            Ok(thread) => thread,
            Err(e) => {
                server.stop();
                let _ = server_thread.join();
                return Err(e);
            }
        };
        Ok(Self {
            addr,
            token,
            engine,
            server: Some(server),
            _watcher: watcher,
            threads: vec![engine_thread, server_thread],
        })
    }

    /// The address the server listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The session token.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The address of the viewer, with the session token.
    pub fn url(&self) -> String {
        format!("http://{}/?token={}", self.addr, self.token)
    }

    /// Tells the preview that the files or directories at these paths were
    /// created, changed, removed or renamed, as a file-system event would.
    pub fn changed<P: AsRef<Path>>(&self, paths: impl IntoIterator<Item = P>) {
        self.engine.changed(
            paths
                .into_iter()
                .map(|p| p.as_ref().to_path_buf())
                .collect(),
        );
    }

    /// Blocks until the preview stops, which only [`Self::stop`] from
    /// another handle or the end of the process does.
    pub fn wait(mut self) {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }

    /// Stops the preview: the watcher, the server and its connections, and
    /// the checks still running.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        self.engine.stop();
        if let Some(server) = self.server.take() {
            server.stop();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl Debug for Preview {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Preview")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}
