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
//! [`Options::on_outputs`], never on every keystroke, and never while
//! `folio.toml` or `folio.lock` has errors, as `folio build` refuses them.
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
//! The viewer may be shown in a frame. Editors show the preview beside the
//! source in an `<iframe>` (VS Code's Simple Browser, which the extension
//! opens with `"folio.preview.openIn": "editor"`), so the viewer's
//! `Content-Security-Policy` sets no `frame-ancestors` and the server sends
//! no `X-Frame-Options`. Any page that knows the URL with the token may
//! frame the viewer; one that does not cannot load it at all, as every
//! request needs the token. A page on another origin that has the token
//! still cannot open the websocket, which the `Origin` check refuses, but
//! it can frame the viewer under a decoy and turn the clicks it catches
//! into reveals of the source in the editor. That risk is accepted: it
//! needs the token, and a reveal only shows a place in the project. The viewer inside a frame is a page of the
//! preview's own origin, so its websocket passes the `Origin` check. The
//! policy otherwise loads only the server's own scripts, styles, images and
//! connections (decision D-332-1).
//!
//! # Protocol
//!
//! Version [`PROTOCOL`] of the protocol, for the viewer and other
//! clients. Every path takes `?token=<token>`.
//!
//! | Path | What it serves |
//! | --- | --- |
//! | `/` | The viewer, with `/viewer.js` and `/viewer.css`: it draws the display lists on canvases, with the diagnostics, and syncs with the source |
//! | `/ws` | The websocket |
//! | `/page/<n>.png` | Page `n` (from 0) of the current revision, rendered by `folio-render` at 96 dpi |
//! | `/font/<n>` | The bytes of the font file that glyph runs name as font `n`, with the font's index in it in the `X-Font-Index` header |
//! | `/glyphs/<n>?size=<px>&g=<id>.<sub>,..` | Glyphs of font `n` rasterised at `px` pixels to the em (at most 2048), for a client that draws display lists itself (SPEC §12.4); see below |
//! | `/figure/<hash>.svg` | An SVG or PDF figure of the current revision as SVG, found by its content hash; see below |
//!
//! On the websocket, the server sends, for every revision the client has
//! not seen, or the rest of one whose first pages it was sent:
//!
//! 1. a text message, a JSON object `{"type": "update", "protocol": 1,
//!    "revision": r, "variant": v | null, "target": "pdf", "pages": n,
//!    "lists": [..], "spans": [..], "banner": text | null, "diagnostics":
//!    [..], "overlays": [..], "fonts": g, "partial": p}`: `pages` is how many pages the document has now (pages the
//!    client had beyond it are gone), `lists` the indices of the pages
//!    whose display lists follow, `spans` those whose span maps follow,
//!    `banner` why the pages shown are not of this revision, if they are
//!    not, and `diagnostics` the diagnostics of the previewed build,
//!    resolved as `folio check --diagnostics json` writes them
//!    (`docs/diagnostics-json.md`) except that byte ranges count in the
//!    source's text, without the byte order mark that a `.fol` or `.fsty`
//!    file may start with (`folio::diag::resolve`), as an editor counts
//!    them, and `overlays` where on the pages the
//!    diagnostics point, as `{"diagnostic": i, "page": n, "rects": [[x, y,
//!    w, h], ..]}` with `i` an index into `diagnostics` and the rectangles
//!    in points from the page's top-left corner, for the first hundred
//!    diagnostics, and `fonts` a number that changes whenever the font ids
//!    of glyph runs may name other fonts, when a client drops the glyphs it
//!    has, and `partial` whether only the first pages are of this
//!    revision (see below). The pages are sent before they are indexed: the overlays follow
//!    in a second `update` of the same revision, which sends no pages, and
//!    until then reveals find nothing (clicks work at once). While neither
//!    the pages nor the diagnostics change, the first `update` keeps the
//!    overlays of the revision before;
//! 2. one binary message per page in `lists` and `spans`, in that order:
//!    the page's index as four bytes, little-endian, then a message of
//!    `folio-render`'s wire format ([`folio_render::wire`]), a whole
//!    display list or a span map that replaces the spans of the list the
//!    client has for that page;
//! 3. a text message `{"type": "done", "revision": r}`.
//!
//! A new connection receives every page. Pages that the client says are
//! visible come first: it sends `{"type": "visible", "pages": [..]}`
//! whenever that changes. They are also laid out first (SPEC §12.2): once
//! any client has said what it shows, a revision whose document has pages
//! after the last visible one, as the revision before had, may come in two
//! updates: the first, with `partial` true, sends the pages through it
//! that changed, while the pages after them, the diagnostics and the marks
//! on other pages stay those of the revision before; the second sends the
//! rest. A client applies each update as it comes; a click on a page after
//! those laid out first finds no source until the second.
//!
//! Source sync (SPEC §12.2): a client sends `{"type": "click", "revision":
//! r, "page": n, "x": x, "y": y}` for a click at a point of a page, in
//! points from its top-left corner, and the server answers that client
//! with `{"type": "source", "revision": r, "location": l | null}`, where
//! `l` is a location as in `docs/diagnostics-json.md` (`path`, `bytes`,
//! `start`, `end`), its bytes counted in the text, without a byte order
//! mark, as those of the diagnostics are: the characters of the topmost glyph at the point, or
//! the whole node of the topmost other item, such as an element's box.
//! The click is also an [`Event::Clicked`], which the language server
//! turns into a jump in the editor. `null` answers a point that shows no
//! source, a revision that is not the current one, and pages kept from an
//! earlier revision under a banner, whose source has changed since. The other way,
//! [`Preview::reveal`] sends every client `{"type": "reveal", "revision":
//! r, "areas": [{"page": n, "rect": [x, y, w, h]}, ..]}`, the areas of the
//! pages that show a place in a file, for the client to scroll to and
//! mark.
//!
//! Messages from the client of a type the server does not know are
//! ignored, and a client ignores messages and fields it does not know:
//! later versions may add both without a new protocol version.
//!
//! Fonts reach a client that draws display lists itself as bitmaps through
//! `/glyphs/<n>`, or as files through `/font/<n>`. `/glyphs/` takes the
//! glyph ids, each with the quarter pixel (`0` to `3`) its origin is placed
//! at right of a pixel's edge, at most 4096 of them, and answers with
//! their count as four bytes and each glyph as its id (two bytes), its
//! quarter pixel and a zero byte, the bitmap's left and top offsets from
//! the glyph's origin in pixels (four bytes each, signed, `y` down), its
//! width and height (four bytes each), and its coverage, a byte per pixel
//! row by row; all little-endian. A glyph with no outline has a width and
//! height of zero. An answer holds at most 32 MiB of coverage: the glyphs
//! past it are left out, and the count says how many it holds.
//!
//! Images arrive in the display lists with their files' bytes. A client
//! decodes JPEG, PNG, WebP and GIF itself (a JPEG as stored, without
//! turning it by its Exif orientation, which PDF output ignores), and asks for SVG and PDF
//! figures as SVG through `/figure/<hash>.svg`: `<hash>` is the content
//! hash of the file's bytes (`Image::content_hash` of `folio-component`)
//! as 16 lowercase hexadecimal digits. The answer is the SVG that
//! `folio-render` writes of the figure alone, at the size of the first box
//! that holds it and with `preserveAspectRatio="none"` on its root, so that
//! it stretches to any box as the figure does, with the figure embedded as
//! a `data:` URI (a PDF figure as the SVG it converts to), under a policy
//! that allows nothing else. A hash no SVG or PDF figure of the current
//! revision has is `404`. The viewer hashes an image when it first draws
//! it and keeps decoded images by their hash while the pages it has drawn
//! show them, so a page sent again does not decode them again.

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
    /// write them; not while `folio.toml` or `folio.lock` has errors or
    /// cannot be read. Without it, nothing is written.
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
    /// The pages of a revision through the last that a client shows were
    /// laid out and sent, before the rest (SPEC §12.2). The pages after
    /// them, and the diagnostics, are those of the revision before until
    /// the revision is checked ([`Event::Updated`]). Only when a client
    /// has said which pages it shows, and the document has pages after
    /// them, as the revision before had.
    FirstPages {
        /// The revision.
        revision: u64,
        /// How many pages were laid out, from the first.
        laid: usize,
        /// How many pages the preview shows.
        pages: usize,
        /// The pages whose display lists were sent again.
        lists: Vec<usize>,
        /// The pages whose span maps were sent again.
        spans: Vec<usize>,
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
    /// The pages of a revision were indexed after they were sent: from now
    /// on [`Preview::reveal`] finds places on them, and the diagnostics are
    /// marked on them. Clicks find their sources from the start.
    Indexed {
        /// The revision.
        revision: u64,
    },
    /// A client clicked on a page, which shows this source: what an editor
    /// jumps to (SPEC §12.2). Called on the server's thread.
    Clicked(Source),
}

/// A place in the project's sources that a page shows ([`Event::Clicked`]).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Source {
    /// The revision the client was shown.
    pub revision: u64,
    /// The place: its project path, bytes, lines and columns.
    pub location: folio::diag::Location,
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
    published: tokio::sync::watch::Receiver<Arc<engine::Published>>,
    broadcast: tokio::sync::broadcast::Sender<Arc<str>>,
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
        // saved meanwhile queues for the engine, which starts last. When a
        // part fails to start, those started before it are stopped: the
        // watcher and server by being dropped, the server's thread joined.
        let pending = engine::prepare(&root, &options)?;
        let engine = pending.handle.clone();
        let watcher = match options.watch {
            true => Some(watch::Watcher::start(&root, engine.clone())?),
            false => None,
        };
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let published = pending.published.clone();
        let (server, addr, server_thread) = server::start(
            options.port,
            token.clone(),
            published.clone(),
            pending.visible.clone(),
            broadcast.clone(),
            options.on_event.clone(),
        )?;
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
            published,
            broadcast,
            server: Some(server),
            _watcher: watcher,
            // The engine's first: `wait` joins it before the server's.
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

    /// Shows clients where the pages show the bytes `bytes` of the file at
    /// `path` (a project path, such as `main.fol`), counted in its text,
    /// without the byte order mark a source may start with: they scroll to the
    /// first area and mark them all, as when an editor's cursor moves
    /// (SPEC §12.2). An empty range is a cursor. Returns how many areas
    /// were found; with none, clients are not told. Nothing is found while
    /// the pages shown are of an earlier revision, or until the current
    /// one is indexed ([`Event::Indexed`]).
    ///
    /// The places are those of the files the pages were laid out from,
    /// which are the files on disk: in an editor buffer with unsaved
    /// changes, the same bytes may be elsewhere.
    pub fn reveal(&self, path: &str, bytes: std::ops::Range<usize>) -> usize {
        let published = self.published.borrow().clone();
        let Some(index) = published
            .layout
            .as_ref()
            .filter(|layout| layout.revision == published.revision)
            .and_then(|layout| layout.index.as_ref())
        else {
            return 0;
        };
        let found = engine::rects(&index.find(path, bytes));
        if found.is_empty() {
            return 0;
        }
        let areas: Vec<_> = found
            .iter()
            .map(|(page, rect)| serde_json::json!({ "page": page, "rect": rect }))
            .collect();
        let message = serde_json::json!({
            "type": "reveal",
            "revision": published.revision,
            "areas": areas,
        });
        // No client is no error.
        let _ = self.broadcast.send(message.to_string().into());
        found.len()
    }

    /// Serves until the process ends. Returns only when the engine fails,
    /// which is a bug: then it stops the server, and says so.
    pub fn wait(mut self) -> Result<(), Error> {
        let mut threads = self.threads.drain(..);
        let engine = threads.next().map(JoinHandle::join);
        let rest: Vec<_> = threads.collect();
        if let Some(server) = self.server.take() {
            server.stop();
        }
        for thread in rest {
            let _ = thread.join();
        }
        match engine {
            Some(Err(_)) => Err(Error(
                "the preview stopped: the compiler failed while reading the project;                  this is a bug in Folio"
                    .into(),
            )),
            _ => Ok(()),
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
