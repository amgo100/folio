//! The engine: the project's world, kept in step with the disk, checked
//! after every batch of changes, with what the clients are sent.
//!
//! The engine's thread owns the world and applies changes to it. Each
//! revision is checked on a thread of its own with a handle of the world;
//! a change cancels the check still running, since setting an input of the
//! query database waits for every other handle to be dropped, and the
//! cancelled check drops its handle at once (SPEC §2.3).
//!
//! The visible pages are laid out first (SPEC §12.2): when a client has
//! said which pages it shows, the check lays out the pages through the
//! last of them and sends those that changed, then checks the revision,
//! which lays out the rest, and sends the rest with the diagnostics.
//!
//! Two guards keep a stream of saves from showing what no save meant. A
//! file saved in place is truncated and then written: when it is read
//! while it is still open for writing, or a source reads empty, it is held until the
//! writer closes it ([`MAX_HOLD`] at most), so that the preview never shows
//! the file half saved. And once the pages have lagged behind the world for
//! [`MAX_LAG`], changes wait for the running check to send its first pages,
//! or to finish, [`MAX_DEFER`] at first, so that saves faster than a check
//! still update the preview.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use ecow::EcoString;
use folio::diag::{Files, TextFiles};
use folio::loader::{self, LoadError, Loader, ProjectErrors};
use folio::span_map;
use folio::{
    BuildMode, Diagnostic, FileId, FontCache, Name, PROJECT_FONT_DIR, Pages, Source, Target, World,
};
use folio_component::FontId;
use folio_render::{DisplayList, wire};
use folio_syntax::{DiagSpan, Lines};
use tokio::sync::watch;

use crate::{Error, Event, Hook, Options, Outputs, OutputsHook, Update};

/// The longest a stream of file-system events postpones reading them.
const MAX_DEBOUNCE: Duration = Duration::from_millis(100);

/// The longest a file held as perhaps read in the middle of a save in
/// place ([`torn`]) waits for its writer to close it.
const MAX_HOLD: Duration = Duration::from_millis(500);

/// How long the pages may lag behind the world before changes wait for
/// the running check's first pages.
const MAX_LAG: Duration = Duration::from_millis(250);

/// The longest changes wait for the running check ([`MAX_LAG`]) before
/// they are applied, and cancel it, all the same: a check whose first
/// pages are slow to come, as one that evaluates a long loop, does not
/// hold back the edit that fixes it.
const MAX_DEFER: Duration = Duration::from_secs(1);

/// The longest the wait of [`MAX_DEFER`] grows to.
const MAX_DEFER_LIMIT: Duration = Duration::from_secs(8);

/// The code of the diagnostic that stands for a panic in a query.
const INTERNAL_ERROR: &str = "internal-error";

/// What the engine's thread is told.
enum Input {
    /// Files or directories changed.
    Paths(Vec<PathBuf>, Change),
    /// A check laid out the first pages of its revision.
    First(Box<First>),
    /// A check finished.
    Done(Box<Done>),
    /// The pages of a check that finished were indexed.
    Indexed(Box<Indexed>),
    /// Outputs were written.
    Written(u64),
    /// Stop.
    Stop,
}

/// What a file-system event says about the paths it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// A file was truncated or written, and may still be open for writing.
    Written,
    /// A file open for writing was closed.
    Closed,
    /// A file created, removed or renamed.
    Moved,
    /// Anything else, such as a change of permissions, or a change
    /// reported without saying what it was.
    Other,
}

/// A handle on the engine's thread.
#[derive(Clone)]
pub(crate) struct Handle(Sender<Input>);

impl Handle {
    /// Tells the engine that the paths changed.
    pub(crate) fn changed(&self, paths: Vec<PathBuf>) {
        self.event(paths, Change::Other);
    }

    /// Tells the engine what an event says about the paths.
    pub(crate) fn event(&self, paths: Vec<PathBuf>, change: Change) {
        let _ = self.0.send(Input::Paths(paths, change));
    }

    /// Stops the engine.
    pub(crate) fn stop(&self) {
        let _ = self.0.send(Input::Stop);
    }
}

/// What the clients are sent: the pages of the last revision that could be
/// laid out, with the diagnostics of the last revision checked.
#[derive(Debug, Default, Clone)]
pub(crate) struct Published {
    /// The revision.
    pub(crate) revision: u64,
    /// The previewed variant.
    pub(crate) variant: Option<String>,
    /// The previewed target.
    pub(crate) target: &'static str,
    /// The pages.
    pub(crate) pages: Vec<PageEntry>,
    /// Why the pages are not those of this revision, if they are not.
    pub(crate) banner: Option<String>,
    /// The diagnostics, resolved.
    pub(crate) diagnostics: Arc<Vec<folio::diag::Resolved>>,
    /// The fonts that the pages' glyph runs name, by id.
    pub(crate) fonts: Arc<FontTable>,
    /// A number that changes whenever the font ids may name other faces
    /// or other bytes, so that clients drop the glyphs they have.
    pub(crate) font_generation: u64,
    /// Where the pages' sources are, for source sync, when any pages are
    /// those of this revision.
    pub(crate) layout: Option<Arc<Layout>>,
    /// The areas of the pages that the diagnostics point at, once the
    /// pages are indexed.
    pub(crate) overlays: Arc<Vec<Overlay>>,
    /// Whether only the first pages are of this revision, sent before the
    /// rest (SPEC §12.2).
    pub(crate) partial: bool,
}

/// The sources of the pages a client is shown: the files they were made
/// from and where on the pages each place in them is.
pub(crate) struct Layout {
    /// The revision the pages were laid out in.
    pub(crate) revision: u64,
    /// How many of the first pages are of that revision: all of them, or
    /// those laid out first while the rest are kept from the revision
    /// before.
    pub(crate) laid: usize,
    /// The files their spans resolve against.
    pub(crate) files: Arc<dyn Files + Send + Sync>,
    /// Where on the pages each place in the files is: indexed after the
    /// pages are sent, so that they never wait for it.
    pub(crate) index: Option<span_map::Index>,
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout")
            .field("revision", &self.revision)
            .field("laid", &self.laid)
            .finish_non_exhaustive()
    }
}

/// The areas on one page that a diagnostic points at.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Overlay {
    /// The diagnostic's index in the update's `diagnostics`.
    pub(crate) diagnostic: usize,
    /// The page.
    pub(crate) page: usize,
    /// The areas, as `[x, y, width, height]` in points from the page's
    /// top-left corner.
    pub(crate) rects: Vec<[f32; 4]>,
}

/// The most diagnostics that overlays are drawn for, in their order.
const MAX_OVERLAYS: usize = 100;

/// The areas a diagnostic's location covers, by page.
pub(crate) fn rects(found: &[span_map::Found]) -> Vec<(usize, [f32; 4])> {
    found
        .iter()
        .map(|f| {
            let pt = |abs: folio_component::Abs| abs.to_pt() as f32;
            (
                f.page,
                [
                    pt(f.pos.x),
                    pt(f.pos.y),
                    pt(f.size.width),
                    pt(f.size.height),
                ],
            )
        })
        .collect()
}

/// A page as the clients are sent it.
#[derive(Debug, Clone)]
pub(crate) struct PageEntry {
    /// Names what the page shows: a new number whenever a client must be
    /// sent the whole display list, because the page's content hash
    /// changed or every page is drawn again (a font changed). Never
    /// repeats, so a client never mistakes an older page for this one.
    pub(crate) content: u64,
    /// Names this entry: a new number whenever the page's display list or
    /// its spans change. A client whose page has the same `content` but
    /// another `id` is sent only the span map.
    pub(crate) id: u64,
    /// The display list.
    pub(crate) list: Arc<DisplayList>,
    /// The display list in the wire format, without the bytes of its
    /// images: for a client that has them ([`Self::images`]).
    pub(crate) bytes: Arc<[u8]>,
    /// The content hashes and formats of its images, if it has any.
    pub(crate) images: Arc<[(u64, folio_component::ImageFormat)]>,
    /// The display list in the wire format with every image's bytes, made
    /// when a client first needs it, so that an edit to a page with a
    /// large image does not copy the image ([`Self::whole`]).
    pub(crate) whole: Arc<OnceLock<Arc<[u8]>>>,
    /// The span map in the wire format.
    pub(crate) spans: Arc<[u8]>,
}

/// The fonts of a build, by the ids glyph runs name them by. A face is
/// read only when a page is rendered or a client asks for it, since
/// reading a system face reads its file.
#[derive(Debug, Default)]
pub(crate) struct FontTable {
    faces: Vec<(folio::Face, OnceLock<Option<folio_fonts::Font>>)>,
}

impl FontTable {
    /// The table of a build's fonts.
    fn new(fonts: &folio::Fonts) -> Self {
        Self {
            faces: fonts
                .faces()
                .iter()
                .map(|face| (face.clone(), OnceLock::new()))
                .collect(),
        }
    }

    /// Whether two tables name the same faces by the same ids.
    fn same_faces(&self, other: &Self) -> bool {
        self.faces.len() == other.faces.len()
            && self.faces.iter().zip(&other.faces).all(|(a, b)| a.0 == b.0)
    }

    /// The font with an id, read on first use.
    pub(crate) fn get(&self, id: usize) -> Option<&folio_fonts::Font> {
        let (face, read) = self.faces.get(id)?;
        read.get_or_init(|| face.font()).as_ref()
    }
}

impl folio_render::Fonts for FontTable {
    fn font(&self, id: FontId) -> Option<folio_render::FontBytes<'_>> {
        let font = self.get(usize::try_from(id.0).ok()?)?;
        Some(folio_render::FontBytes {
            data: font.data(),
            index: font.index(),
        })
    }
}

/// The pages a check laid out first, through the last page a client
/// shows, ready to send.
struct First {
    revision: u64,
    variant: Option<String>,
    target: &'static str,
    /// The pages laid out, followed by the pages sent before beyond them:
    /// what the clients have once these are sent.
    pages: Pages,
    /// How many pages were laid out.
    laid: usize,
    /// The display lists of the pages laid out that changed or were
    /// remapped, with whether only their spans changed.
    lists: Vec<(usize, PageEntry, bool)>,
    banner: Option<String>,
    files: Arc<dyn Files + Send + Sync>,
    fonts: Arc<FontTable>,
    /// Whether the font ids may name other faces or bytes than before.
    fonts_changed: bool,
}

/// The pages each client shows, which the check of a revision lays out
/// first (SPEC §12.2).
#[derive(Debug, Default)]
pub(crate) struct Visible {
    /// The last page each client shows, by the client's number.
    clients: Mutex<Vec<(u64, usize)>>,
    /// The last number given to a client.
    next: AtomicU64,
}

impl Visible {
    /// A number for a client that has connected.
    pub(crate) fn join(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Records the pages a client shows.
    pub(crate) fn show(&self, client: u64, pages: &[usize]) {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        clients.retain(|&(c, _)| c != client);
        if let Some(&last) = pages.iter().max() {
            clients.push((client, last));
        }
    }

    /// Forgets a client that has gone.
    pub(crate) fn leave(&self, client: u64) {
        self.show(client, &[]);
    }

    /// The last page that any client shows, or `None` when no client has
    /// said what it shows.
    pub(crate) fn last(&self) -> Option<usize> {
        let clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        clients.iter().map(|&(_, last)| last).max()
    }
}

/// What a check found.
struct Done {
    revision: u64,
    /// `None` when an edit cancelled it.
    frame: Option<Frame>,
}

/// A checked revision, ready to send.
struct Frame {
    variant: Option<String>,
    target: &'static str,
    /// The pages, or `None` when the build could not be laid out.
    pages: Option<Pages>,
    /// The display lists of the pages that changed or were remapped, with
    /// whether only their spans changed.
    lists: Vec<(usize, PageEntry, bool)>,
    banner: Option<String>,
    diagnostics: Vec<Diagnostic>,
    files: Arc<dyn Files + Send + Sync>,
    fonts: Arc<FontTable>,
    /// Whether the font ids may name other faces or bytes than before.
    fonts_changed: bool,
}

/// The index of a checked revision's pages ([`Layout::index`]), with the
/// areas its diagnostics point at.
struct Indexed {
    revision: u64,
    index: span_map::Index,
    overlays: Vec<Overlay>,
}

/// The builds to preview, and how.
#[derive(Clone)]
struct Settings {
    variant: Option<String>,
    system_fonts: bool,
    debounce: Duration,
    idle: Duration,
    today: Option<folio::Date>,
    on_outputs: Option<OutputsHook>,
    on_event: Option<Hook>,
}

impl Settings {
    fn emit(&self, event: Event) {
        if let Some(hook) = &self.on_event {
            hook(&event);
        }
    }
}

/// Prepares the engine for the project in `root`, without reading the
/// project yet: what changes from now on queues for it, so that the
/// watcher can start before the first read and miss nothing.
pub(crate) fn prepare(root: &Path, options: &Options) -> Result<Pending, Error> {
    let settings = Settings {
        variant: options.variant.clone(),
        system_fonts: options.system_fonts,
        debounce: options.debounce,
        idle: options.idle,
        today: options.today,
        on_outputs: options.on_outputs.clone(),
        on_event: options.on_event.clone(),
    };
    let cache = FontCache::default_dir().map(FontCache::new);
    let loader = Loader::new(root)
        .map_err(|e| Error(e.to_string()))?
        .with_font_cache(cache);
    let (tx, rx) = crossbeam_channel::unbounded();
    let visible = Arc::new(Visible::default());
    let (publish, published) = watch::channel(Arc::new(Published {
        target: Target::Pdf.as_str(),
        ..Published::default()
    }));
    let engine = Engine {
        settings,
        root: root.to_path_buf(),
        canonical: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        loader,
        world: None,
        errors: None,
        unreadable: false,
        warnings: Vec::new(),
        revision: 0,
        dirty: false,
        redraw: true,
        last: None,
        tx: tx.clone(),
        publish,
        idle_at: None,
        written: 0,
        ids: 0,
        current: Arc::new(AtomicU64::new(0)),
        visible: visible.clone(),
        // inotify reports closes; other systems' watchers say so by
        // reporting one. Without a watcher, nothing does.
        closes: options.watch && cfg!(any(target_os = "linux", target_os = "android")),
        open: BTreeSet::new(),
        held: BTreeMap::new(),
        defer_limit: MAX_DEFER,
        running: None,
        lagging: None,
        deferred: BTreeSet::new(),
        deferred_since: None,
        released: BTreeSet::new(),
        rx,
        queue: VecDeque::new(),
    };
    Ok(Pending {
        handle: Handle(tx),
        published,
        visible,
        engine,
    })
}

/// An engine that has not read the project yet ([`prepare`]).
pub(crate) struct Pending {
    /// The handle that tells it what changed.
    pub(crate) handle: Handle,
    /// What it publishes for the clients.
    pub(crate) published: watch::Receiver<Arc<Published>>,
    /// The pages the clients show, which the server records.
    pub(crate) visible: Arc<Visible>,
    engine: Engine,
}

impl Pending {
    /// Starts the engine's thread, which reads the project, checks it,
    /// and then applies the changes that queued meanwhile.
    pub(crate) fn spawn(self) -> Result<JoinHandle<()>, Error> {
        let Self { mut engine, .. } = self;
        thread::Builder::new()
            .name("folio-preview".into())
            .spawn(move || {
                engine.load();
                engine.run();
            })
            .map_err(|e| Error(format!("cannot start the preview: {e}")))
    }
}

struct Engine {
    settings: Settings,
    root: PathBuf,
    /// The project directory with links resolved, as some systems name
    /// the paths of events.
    canonical: PathBuf,
    loader: Loader,
    /// `None` while `folio.toml` or `folio.lock` keeps the project from
    /// loading.
    world: Option<World>,
    /// The errors of the manifest or lockfile on disk, which the world did
    /// not take.
    errors: Option<ProjectErrors>,
    /// Whether `folio.toml` or `folio.lock` could not be read when it last
    /// changed: the world keeps the last valid one, as with `errors`, and
    /// nothing is written.
    unreadable: bool,
    /// Problems reading the project, shown with the next revision.
    warnings: Vec<Diagnostic>,
    revision: u64,
    /// Whether the world changed since the last check started.
    dirty: bool,
    /// Whether every page must be sent again, as after a font changed.
    redraw: bool,
    /// The pages last sent.
    last: Option<Pages>,
    tx: Sender<Input>,
    publish: watch::Sender<Arc<Published>>,
    /// When outputs are written, if the project stays unchanged.
    idle_at: Option<Instant>,
    /// The last revision whose outputs were written.
    written: u64,
    /// The last number given to a page entry ([`PageEntry::id`]).
    ids: u64,
    /// The revision checked next, which check threads read to leave out
    /// work an edit has made stale.
    current: Arc<AtomicU64>,
    /// The pages the clients show.
    visible: Arc<Visible>,
    /// Whether a watcher runs that reports when a file open for writing
    /// is closed, which files are held for ([`torn`]).
    closes: bool,
    /// The paths truncated or written and not closed since.
    open: BTreeSet<PathBuf>,
    /// The files held as perhaps read in the middle of a save in place
    /// ([`torn`]), and when they are read anyway if their writer has not
    /// closed them.
    held: BTreeMap<PathBuf, Instant>,
    /// How long changes may wait for the running check: [`MAX_DEFER`],
    /// doubled each time a check is cancelled for taking longer, up to
    /// [`MAX_DEFER_LIMIT`], until pages are sent.
    defer_limit: Duration,
    /// The revision of the check running, if one is.
    running: Option<u64>,
    /// Since when the pages have lagged behind the world: since the first
    /// revision after the last one whose pages were sent.
    lagging: Option<Instant>,
    /// The changes that wait for the running check ([`MAX_LAG`]).
    deferred: BTreeSet<PathBuf>,
    /// Since when changes have waited ([`MAX_DEFER`]).
    deferred_since: Option<Instant>,
    /// The held sources whose time ran out, read as they are when next
    /// applied.
    released: BTreeSet<PathBuf>,
    /// What the engine is told.
    rx: Receiver<Input>,
    /// Inputs taken from `rx` to see whether events wait, handled before
    /// the inputs after them.
    queue: VecDeque<Input>,
}

impl Engine {
    fn run(mut self) {
        loop {
            // Due before any input already queued is handled.
            if self.held.values().any(|at| *at <= Instant::now()) {
                self.release();
            }
            if self.deferred_since.is_some() && !self.lagging_behind() {
                self.apply_with_deferred(Vec::new());
            }
            if self.dirty {
                self.check();
            }
            let release = self.held.values().min().copied();
            let defer = self.deferred_since.map(|since| since + self.defer_limit);
            let deadline = [self.idle_at, release, defer].into_iter().flatten().min();
            let input = match self.recv(deadline) {
                Ok(input) => input,
                Err(RecvTimeoutError::Timeout) => {
                    if release.is_some() && release == deadline {
                        self.release();
                    } else if defer.is_some() && defer == deadline {
                        // Applied at the top of the loop.
                    } else {
                        self.idle_at = None;
                        self.write_outputs();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            match input {
                Input::Paths(paths, change) => {
                    self.note(&paths, change);
                    let Some(paths) = self.debounce(paths) else {
                        return;
                    };
                    if self.lagging_behind() {
                        // Applied once the running check sends its first
                        // pages, which applying them now would cancel.
                        self.defer(paths);
                        continue;
                    }
                    self.apply_with_deferred(paths);
                }
                other => {
                    if !self.handle(other) {
                        return;
                    }
                }
            }
        }
    }

    /// Handles an input other than changed paths; `false` to stop.
    fn handle(&mut self, input: Input) -> bool {
        match input {
            Input::Paths(..) => unreachable!("handled by the caller"),
            Input::First(first) => self.first(*first),
            Input::Done(done) => self.done(*done),
            Input::Indexed(indexed) => self.indexed(*indexed),
            Input::Written(revision) => {
                self.settings.emit(Event::Written { revision });
            }
            Input::Stop => return false,
        }
        true
    }

    /// Collects the paths of events that follow within the debounce
    /// interval of each other; `None` to stop.
    fn debounce(&mut self, mut paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
        let until = Instant::now() + MAX_DEBOUNCE;
        loop {
            let deadline = (Instant::now() + self.settings.debounce).min(until);
            match self.recv(Some(deadline)) {
                Ok(Input::Paths(more, change)) => {
                    self.note(&more, change);
                    paths.extend(more);
                }
                Ok(other) => {
                    if !self.handle(other) {
                        return None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Some(paths),
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// The next input: those queued first, then those from the channel,
    /// waiting until `deadline` if there is one.
    fn recv(&mut self, deadline: Option<Instant>) -> Result<Input, RecvTimeoutError> {
        if let Some(input) = self.queue.pop_front() {
            return Ok(input);
        }
        match deadline {
            Some(at) => self.rx.recv_deadline(at),
            None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        }
    }

    /// Records which paths are open for writing after an event.
    fn note(&mut self, paths: &[PathBuf], change: Change) {
        match change {
            // Without closes, nothing would take them out again.
            Change::Written if self.closes => {
                for path in paths {
                    // A new save: guarded again.
                    self.released.remove(path);
                    self.open.insert(path.clone());
                }
                // Under every path that reaches the same file.
                if !self.released.is_empty() {
                    let written: BTreeSet<PathBuf> =
                        paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
                    self.released.retain(|released| {
                        released
                            .canonicalize()
                            .map_or(true, |r| !written.contains(&r))
                    });
                }
            }
            Change::Written => {}
            Change::Closed => {
                self.closes = true;
                for path in paths {
                    // Read with the rest of the event's batch, which takes
                    // it out of `held` unless it still reads torn.
                    self.open.remove(path);
                }
                // Held under another path that reaches the same file, as
                // through a link: the loader reads it under every such path
                // with the batch.
                if !self.held.is_empty() || !self.open.is_empty() {
                    let closed: BTreeSet<PathBuf> =
                        paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
                    let other = |path: &PathBuf| {
                        !paths.contains(path)
                            && path.canonicalize().is_ok_and(|r| closed.contains(&r))
                    };
                    self.held.retain(|held, _| !other(held));
                    self.open.retain(|open| !other(open));
                }
            }
            // Removed, renamed or created: whatever writes it now has
            // opened it again.
            Change::Moved => {
                // With everything under a directory moved.
                for path in paths {
                    self.open.retain(|open| !open.starts_with(path));
                    self.released.retain(|released| !released.starts_with(path));
                }
            }
            Change::Other => {}
        }
    }

    /// Whether the pages have lagged behind the world for so long that
    /// changes wait for the running check ([`MAX_LAG`]), and they have not
    /// waited too long ([`MAX_DEFER`]).
    fn lagging_behind(&self) -> bool {
        self.running.is_some()
            && self.lagging.is_some_and(|since| since.elapsed() >= MAX_LAG)
            && self
                .deferred_since
                .is_none_or(|since| since.elapsed() < self.defer_limit)
    }

    /// Applies `paths` with the changes that waited for the running check.
    /// When their time ran out while the pages still lagged, and they make
    /// a new revision, they cancel a check that took too long: the next
    /// one gets the same grace before changes wait for it, and twice as
    /// long to send pages.
    fn apply_with_deferred(&mut self, mut paths: Vec<PathBuf>) {
        let timed_out = self.running.is_some()
            && self.lagging.is_some()
            && self
                .deferred_since
                .is_some_and(|since| since.elapsed() >= self.defer_limit);
        self.deferred_since = None;
        paths.extend(std::mem::take(&mut self.deferred));
        let before = self.revision;
        self.apply(&paths);
        if timed_out && self.revision != before {
            self.lagging = Some(Instant::now());
            self.defer_limit = (self.defer_limit * 2).min(MAX_DEFER_LIMIT);
        }
    }

    /// Lets changes wait for the running check.
    fn defer(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.deferred_since.get_or_insert_with(Instant::now);
        self.deferred.extend(paths);
    }

    /// Reads the held sources whose writers have not closed them in time.
    fn release(&mut self) {
        let now = Instant::now();
        let due: Vec<PathBuf> = self
            .held
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &due {
            self.held.remove(path);
            // Read as it is from now on, even if it is not closed.
            self.open.remove(path);
            self.released.insert(path.clone());
        }
        // Under every path that reaches the same file.
        if !due.is_empty() && !self.open.is_empty() {
            let resolved: BTreeSet<PathBuf> =
                due.iter().filter_map(|p| p.canonicalize().ok()).collect();
            self.open
                .retain(|open| open.canonicalize().map_or(true, |r| !resolved.contains(&r)));
        }
        if self.lagging_behind() {
            self.defer(due);
        } else {
            self.apply_with_deferred(due);
        }
    }

    /// Takes out of `paths` the files that may be read between the
    /// truncate and the end of the write of a save in place: any file
    /// still open for writing, and the sources that read [`torn`]; and
    /// holds them until their writers close them, [`MAX_HOLD`] at most.
    /// Returns the world's sources as they are, by project path, which
    /// [`Self::recheck`] checks the loader's reads against.
    fn hold(&mut self, paths: &mut Vec<PathBuf>) -> Option<Before> {
        // Released paths are read as they are, once.
        let released: BTreeSet<PathBuf> = paths
            .iter()
            .filter(|path| self.released.remove(*path))
            .cloned()
            .collect();
        if !self.closes {
            return None;
        }
        let world = self.world.as_ref()?;
        let mut held = Vec::new();
        let mut read = Vec::new();
        paths.retain(|path| {
            if released.contains(path) {
                return true;
            }
            // Any file still open for writing, new ones and data files
            // among them, waits for its close.
            let input = self
                .loader_path(path)
                .is_some_and(|rel| !rel.split('/').any(|part| part.starts_with('.')));
            if input && self.open.contains(path) && path.is_file() {
                held.push(path.clone());
                return false;
            }
            // A file that is gone is removed at once; a new one is checked
            // once read.
            let Some((source, meta)) = self
                .loader_path(path)
                .and_then(|rel| world.source(&rel))
                .zip(std::fs::metadata(path).ok())
            else {
                read.push(path.clone());
                return true;
            };
            let hold = torn(
                source.text().len() as u64,
                meta.len(),
                self.open.contains(path),
            );
            if hold {
                held.push(path.clone());
            } else {
                read.push(path.clone());
            }
            !hold
        });
        // The sources as they are, which the loader's reads may replace,
        // under every path that reaches a file: through links too, and
        // under directories read again.
        let sources = world
            .source_paths()
            .filter_map(|path| {
                let path = path.as_str();
                Some((path.to_owned(), world.source(path)?))
            })
            .collect();
        let before = Before {
            sources,
            released: released
                .iter()
                .filter_map(|path| path.canonicalize().ok())
                .chain(released.iter().cloned())
                .collect(),
        };
        // Read whole, or gone: no longer held, under any path that reaches
        // the same file.
        if !self.held.is_empty() && !read.is_empty() {
            let resolved: BTreeSet<PathBuf> = read
                .iter()
                .filter_map(|p| p.canonicalize().ok())
                .chain(read.iter().cloned())
                .collect();
            self.held.retain(|held, _| {
                !resolved.contains(held)
                    && held.canonicalize().map_or(true, |r| !resolved.contains(&r))
            });
        }
        let until = Instant::now() + MAX_HOLD;
        for path in held {
            self.held.entry(path).or_insert(until);
        }
        Some(before)
    }

    /// Puts back the world's text of the sources that read [`torn`] after
    /// the loader read them, or takes out a new one, as their truncate or
    /// write may have come between the check of [`Self::hold`] and the
    /// read, and holds them; takes them out of `changed`. Every source the
    /// loader changed is checked, by its project path: a file may be read
    /// under several, through links inside the project, or under a
    /// directory read again, and whether it is being written is told by
    /// its path or the path it resolves to. Returns whether it put any
    /// back: the world changed twice, which cancelled the check running,
    /// so its revision must be checked again.
    fn recheck(
        &mut self,
        before: Option<Before>,
        changed: &mut BTreeSet<String>,
        warned: usize,
    ) -> bool {
        let Some(before) = before else {
            return false;
        };
        // The files being written: open, or with a write waiting to be
        // handled, by their paths and the paths they resolve to.
        self.queue.extend(self.rx.try_iter());
        let queued = self.queue.iter().filter_map(|input| match input {
            Input::Paths(paths, Change::Written) => Some(paths),
            _ => None,
        });
        let written: BTreeSet<PathBuf> = self
            .open
            .iter()
            .chain(queued.flatten())
            .flat_map(|path| [Some(path.clone()), path.canonicalize().ok()])
            .flatten()
            .collect();
        let mut held = Vec::new();
        let sources: Vec<String> = changed
            .iter()
            .filter(|rel| rel.ends_with(".fol") || rel.ends_with(".fsty"))
            .cloned()
            .collect();
        for rel in sources {
            let path = self.root.join(&rel);
            let resolved = path.canonicalize().ok();
            // Read as it is, its hold over.
            if before.released.contains(&path)
                || resolved
                    .as_ref()
                    .is_some_and(|r| before.released.contains(r))
            {
                continue;
            }
            let writing =
                written.contains(&path) || resolved.as_ref().is_some_and(|r| written.contains(r));
            let old = before.sources.get(&rel);
            let old_len = old.map_or(0, |old| old.text().len() as u64);
            // Removed by the loader: gone, or not UTF-8, as when the read
            // ends inside a character that a write in chunks splits.
            let new = self.world.as_ref().and_then(|world| world.source(&rel));
            let torn = match &new {
                Some(new) => {
                    let new_len = new.text().len() as u64;
                    torn(old_len, new_len, writing)
                }
                None => old.is_some() && path.exists() && writing,
            };
            if !torn {
                continue;
            }
            // Read again on its close either way. A new file is taken out
            // again.
            let put_back = self.world.as_mut().is_some_and(|world| match old {
                Some(old) => world.set_source(&rel, old.text()).is_ok(),
                None => world.remove_source(&rel),
            });
            if put_back {
                changed.remove(&rel);
                held.push((path, rel));
            } else {
                self.held.entry(path).or_insert(Instant::now() + MAX_HOLD);
            }
        }
        let restored = !held.is_empty();
        let until = Instant::now() + MAX_HOLD;
        for (path, rel) in held {
            // The loader's warning that the file is not UTF-8 is about a
            // read that is put back.
            let message = format!("cannot read `{rel}`: it is not UTF-8 text");
            let mut index = 0;
            self.warnings.retain(|warning| {
                index += 1;
                index <= warned || warning.message != message
            });
            self.held.entry(path).or_insert(until);
        }
        restored
    }

    /// Builds the project's world from the disk.
    fn load(&mut self) {
        self.world = None;
        self.errors = None;
        self.unreadable = false;
        let builder = World::builder().long_lived(true).mode(BuildMode::Watch);
        let mut world = match self.loader.world(builder) {
            Ok(world) => world,
            Err(LoadError::Project(errors)) => {
                self.errors = Some(errors);
                self.edited();
                return;
            }
            Err(e) => {
                self.warnings.push(error(e.to_string()));
                self.edited();
                return;
            }
        };
        if let Err(e) = self.loader.load(&mut world, &mut self.warnings) {
            self.warnings.push(error(e.to_string()));
        }
        let cache = FontCache::default_dir().map(FontCache::new);
        if let Err(e) = world.load_fonts(&self.root, cache.as_ref()) {
            self.warnings
                .push(error(format!("cannot read the project's fonts: {e}")));
        }
        if self.settings.system_fonts {
            world.set_system_fonts(folio::system_fonts());
        }
        world.set_today(self.settings.today);
        self.world = Some(world);
        self.redraw = true;
        self.edited();
    }

    /// Brings the world in step with the disk at the paths that changed.
    fn apply(&mut self, paths: &[PathBuf]) {
        let mut paths = paths.to_vec();
        let before = self.hold(&mut paths);
        if paths.is_empty() {
            return;
        }
        let paths = &paths[..];
        let names = |name: &str| {
            paths
                .iter()
                .any(|path| self.loader_path(path).is_some_and(|rel| rel == name))
        };
        // The project directory itself, as after the watcher lost events,
        // is read again whole, the manifest and lockfile among it.
        let project_file = names(loader::MANIFEST) || names(loader::LOCKFILE) || names("");
        let Some(world) = &mut self.world else {
            // The project could not be loaded: try again when a file it
            // reads changes.
            let read = paths.iter().any(|path| {
                self.loader_path(path)
                    .is_some_and(|rel| !rel.split('/').any(|part| part.starts_with('.')))
            });
            if read {
                self.load();
            }
            return;
        };
        let mut paths = paths.to_vec();
        if project_file {
            // Both are read again, so that the errors are those of both.
            paths.push(self.root.join(loader::MANIFEST));
            paths.push(self.root.join(loader::LOCKFILE));
        }
        let warned = self.warnings.len();
        let mut update = match self.loader.update(world, &paths, &mut self.warnings) {
            Ok(update) => update,
            Err(e) => {
                self.warnings.push(error(e.to_string()));
                self.edited();
                return;
            }
        };
        let restored = self.recheck(before, &mut update.changed, warned);
        if project_file {
            // Cleared only here, where the change is a new revision
            // (`edited` below), whose check schedules the writes that the
            // errors held back.
            self.errors = update.errors;
            // The loader warns about a file it cannot read and keeps the
            // last valid one, which `folio build` would not.
            self.unreadable =
                self.loader.read_manifest().is_err() || self.loader.read_lockfile().is_err();
        }
        let fonts = update.changed.iter().any(|path| {
            path == loader::LOCKFILE
                || path == PROJECT_FONT_DIR
                || path.starts_with(&format!("{PROJECT_FONT_DIR}/"))
        });
        if fonts {
            // A font file replaced by one of the same names and length
            // leaves the pages' hashes as they were (`Pages::hashes`).
            self.redraw = true;
        }
        if !update.changed.is_empty() || project_file || !self.warnings.is_empty() || restored {
            self.edited();
        }
    }

    /// The project path of a path from an event, if it is in the project.
    fn loader_path(&self, path: &Path) -> Option<String> {
        let rel = path
            .strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.canonical))
            .ok()?;
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        Some(parts?.join("/"))
    }

    /// Marks the world as changed: a new revision, checked next.
    fn edited(&mut self) {
        self.lagging.get_or_insert_with(Instant::now);
        self.revision += 1;
        self.current.store(self.revision, Ordering::Relaxed);
        self.dirty = true;
        self.idle_at = None;
        self.settings.emit(Event::Edited {
            revision: self.revision,
        });
    }

    /// Starts checking the current revision on a thread of its own.
    fn check(&mut self) {
        self.dirty = false;
        let revision = self.revision;
        self.running = Some(revision);
        let warnings = std::mem::take(&mut self.warnings);
        let project = self.errors.clone();
        let tx = self.tx.clone();
        let Some(world) = &self.world else {
            // Nothing can be laid out: the last pages stay.
            let (files, mut diagnostics) = project_files(project, None);
            diagnostics.extend(warnings);
            let frame = Frame {
                variant: self.settings.variant.clone(),
                target: Target::Pdf.as_str(),
                pages: None,
                lists: Vec::new(),
                banner: Some(
                    match self.errors {
                        Some(_) => {
                            "folio.toml or folio.lock has errors, so the project cannot be built"
                        }
                        None => "the project cannot be read, so it cannot be built",
                    }
                    .into(),
                ),
                diagnostics,
                files,
                fonts: self.publish.borrow().fonts.clone(),
                fonts_changed: false,
            };
            let _ = tx.send(Input::Done(Box::new(Done {
                revision,
                frame: Some(frame),
            })));
            return;
        };
        let handle = world.clone();
        let snapshot = world.snapshot();
        let build = choose(world, self.settings.variant.as_deref());
        let before = if self.redraw { None } else { self.last.clone() };
        let published = self.publish.borrow().clone();
        let current = self.current.clone();
        let visible = self.visible.last();
        let spawned = thread::Builder::new()
            .name("folio-preview-check".into())
            .spawn(move || {
                // Tells the engine the check ended even if it panics.
                let mut ended = Ended {
                    tx: Some(tx.clone()),
                    revision,
                };
                let project_errors = project.is_some();
                let (files, mut diagnostics) = project_files(project, Some(Arc::new(snapshot)));
                diagnostics.extend(warnings);
                let frame = check(
                    &handle,
                    build,
                    before,
                    &published,
                    files,
                    diagnostics,
                    project_errors,
                    visible,
                    &mut |first| {
                        let _ = tx.send(Input::First(Box::new(First { revision, ..first })));
                    },
                );
                // The next edit waits for every handle to go.
                drop(handle);
                let later = frame.as_ref().and_then(|frame| {
                    let pages = frame.pages.clone()?;
                    Some((pages, frame.files.clone(), frame.diagnostics.clone()))
                });
                ended.tx = None;
                let _ = tx.send(Input::Done(Box::new(Done { revision, frame })));
                // The pages go first; source sync and overlays follow,
                // unless an edit has made them stale.
                if let Some((pages, files, diagnostics)) = later
                    && current.load(Ordering::Relaxed) == revision
                {
                    let indexed = index(revision, &pages, &*files, &diagnostics);
                    let _ = tx.send(Input::Indexed(Box::new(indexed)));
                }
            });
        if spawned.is_err() {
            // Checked again with the next change.
            self.running = None;
            self.dirty = true;
        }
    }

    /// Sends what a check found, unless an edit has overtaken it.
    fn done(&mut self, done: Done) {
        let revision = done.revision;
        if self.running == Some(revision) {
            self.running = None;
        }
        let Some(frame) = done.frame else {
            // Cancelled by an edit, whose revision is checked next; or, if
            // no edit overtook it, the check panicked, and the pages that
            // stay are no longer counted as lagging behind a check.
            if revision == self.revision {
                self.lagging = None;
                self.defer_limit = MAX_DEFER;
            }
            return;
        };
        if revision != self.revision {
            self.settings.emit(Event::Stale { revision });
            return;
        }
        // The pages are those of the world again.
        self.lagging = None;
        self.defer_limit = MAX_DEFER;
        let old = self.publish.borrow().clone();
        let mut pages = old.pages.clone();
        let mut lists = Vec::new();
        let mut spans = Vec::new();
        if let Some(new) = &frame.pages {
            pages.truncate(new.pages.len());
            (lists, spans) = self.place(&mut pages, frame.lists);
            self.last = frame.pages.clone();
            self.redraw = false;
        }
        let resolved: Vec<_> = frame
            .diagnostics
            .iter()
            .map(|d| folio::diag::resolve(&*frame.files, d))
            .collect();
        let count = pages.len();
        // Pages kept from an earlier revision are not synced with the
        // source, which has changed since; those laid out first in this
        // revision are.
        let layout = match &frame.pages {
            Some(_) => Some(Arc::new(Layout {
                revision,
                laid: count,
                files: frame.files.clone(),
                index: None,
            })),
            None => old
                .layout
                .clone()
                .filter(|layout| layout.revision == revision),
        };
        let font_generation = old.font_generation + u64::from(frame.fonts_changed);
        // The marks stay, rather than blink, while the pages are indexed,
        // when neither the pages nor the diagnostics changed.
        let unchanged = frame.pages.is_some()
            && count == old.pages.len()
            && lists.is_empty()
            && spans.is_empty()
            && *old.diagnostics == resolved;
        let overlays = match unchanged {
            true => old.overlays.clone(),
            false => Arc::new(Vec::new()),
        };
        self.publish.send_replace(Arc::new(Published {
            revision,
            variant: frame.variant,
            target: frame.target,
            pages,
            banner: frame.banner.clone(),
            diagnostics: Arc::new(resolved),
            fonts: if frame.pages.is_some() {
                frame.fonts
            } else {
                old.fonts.clone()
            },
            font_generation,
            layout,
            overlays,
            // The first pages of this revision, sent before a check that
            // laid out nothing, are still all that is of it.
            partial: frame.pages.is_none() && old.partial && old.revision == revision,
        }));
        self.settings.emit(Event::Updated(Arc::new(Update {
            revision,
            pages: count,
            lists,
            spans,
            banner: frame.banner,
            diagnostics: frame.diagnostics,
            files: frame.files,
        })));
        // Outputs follow the manifest on disk, as `folio build` does: while
        // it has errors, the pages follow its last valid version, but
        // nothing is written under it.
        if self.world.is_some()
            && self.writable()
            && self.written < revision
            && self.settings.on_outputs.is_some()
        {
            self.idle_at = Some(Instant::now() + self.settings.idle);
        }
    }

    /// Gives the display lists of a check numbers ([`PageEntry`]) and puts
    /// them in `pages`; returns the pages whose lists and whose span maps
    /// are sent.
    fn place(
        &mut self,
        pages: &mut Vec<PageEntry>,
        new: Vec<(usize, PageEntry, bool)>,
    ) -> (Vec<usize>, Vec<usize>) {
        let mut lists = Vec::new();
        let mut spans = Vec::new();
        for (index, mut entry, spans_only) in new {
            self.ids += 1;
            entry.id = self.ids;
            if spans_only {
                // Remapped pages are pages `before` had, which were sent.
                entry.content = pages.get(index).map_or(entry.id, |old| old.content);
                spans.push(index);
            } else {
                entry.content = entry.id;
                lists.push(index);
            }
            if index < pages.len() {
                pages[index] = entry;
            } else {
                pages.push(entry);
            }
        }
        (lists, spans)
    }

    /// Sends the pages a check laid out first, unless an edit has
    /// overtaken it. The pages after them stay as they were sent, and so
    /// do the diagnostics, until the check finishes.
    fn first(&mut self, first: First) {
        let revision = first.revision;
        if revision != self.revision {
            // The check's end is stale too, and says so.
            return;
        }
        self.lagging = None;
        self.defer_limit = MAX_DEFER;
        let old = self.publish.borrow().clone();
        let mut pages = old.pages.clone();
        let (lists, spans) = self.place(&mut pages, first.lists);
        // The marks on the pages drawn again marked them as they were.
        let overlays: Vec<Overlay> = old
            .overlays
            .iter()
            .filter(|o| !lists.contains(&o.page))
            .cloned()
            .collect();
        self.last = Some(first.pages);
        self.redraw = false;
        let count = pages.len();
        self.publish.send_replace(Arc::new(Published {
            revision,
            variant: first.variant,
            target: first.target,
            pages,
            banner: first.banner,
            diagnostics: old.diagnostics.clone(),
            fonts: first.fonts,
            font_generation: old.font_generation + u64::from(first.fonts_changed),
            layout: Some(Arc::new(Layout {
                revision,
                laid: first.laid,
                files: first.files,
                index: None,
            })),
            overlays: Arc::new(overlays),
            partial: true,
        }));
        self.settings.emit(Event::FirstPages {
            revision,
            laid: first.laid,
            pages: count,
            lists,
            spans,
        });
    }

    /// Adds the index of the pages and the overlays to the revision sent,
    /// if it is still the current one.
    fn indexed(&mut self, indexed: Indexed) {
        let published = self.publish.borrow().clone();
        let current = indexed.revision == self.revision
            && published.revision == indexed.revision
            && published
                .layout
                .as_ref()
                .is_some_and(|layout| layout.revision == indexed.revision);
        let Some(layout) = published.layout.as_ref().filter(|_| current) else {
            return;
        };
        let mut next = (*published).clone();
        next.layout = Some(Arc::new(Layout {
            revision: layout.revision,
            laid: layout.laid,
            files: layout.files.clone(),
            index: Some(indexed.index),
        }));
        next.overlays = Arc::new(indexed.overlays);
        self.publish.send_replace(Arc::new(next));
        self.settings.emit(Event::Indexed {
            revision: indexed.revision,
        });
    }

    /// Whether outputs may be written: not while `folio.toml` or
    /// `folio.lock` has errors or cannot be read, as `folio build` refuses
    /// the project then.
    fn writable(&self) -> bool {
        self.errors.is_none() && !self.unreadable
    }

    /// Compiles every build on a thread of its own and hands them to the
    /// outputs hook, unless the project is not [`Self::writable`]. An edit
    /// cancels the compilation, and the outputs of a revision an edit has
    /// overtaken are not handed on.
    fn write_outputs(&mut self) {
        let (Some(world), Some(hook)) = (&self.world, self.settings.on_outputs.clone()) else {
            return;
        };
        if !self.writable() {
            // `folio build` refuses the project; so does the writer.
            return;
        }
        let revision = self.revision;
        self.written = revision;
        let handle = world.clone();
        let files: Arc<dyn Files + Send + Sync> = Arc::new(world.snapshot());
        let tx = self.tx.clone();
        let current = self.current.clone();
        let _ = thread::Builder::new()
            .name("folio-preview-write".into())
            .spawn(move || {
                let builds = handle.builds();
                let manifest = handle.manifest().clone();
                let outputs = folio::compile_builds(&handle, builds);
                drop(handle);
                // An edit overtook it, and its revision is written instead
                // if it can be. A manifest that does not parse leaves the
                // world as it was, so it cancels nothing, but it is a new
                // revision too.
                if outputs.iter().any(|o| o.result.is_err())
                    || current.load(Ordering::Relaxed) != revision
                {
                    return;
                }
                let outputs = outputs
                    .into_iter()
                    .map(|o| {
                        let path = manifest
                            .output_path(o.variant.as_ref(), o.target)
                            .map(String::from);
                        (o, path)
                    })
                    .collect();
                hook(&Outputs {
                    revision,
                    outputs,
                    files,
                });
                let _ = tx.send(Input::Written(revision));
            });
    }
}

/// The build to preview (SPEC §12.2): the chosen variant, or else the
/// first the manifest declares, for PDF.
fn choose(world: &World, variant: Option<&str>) -> Result<(Option<Name>, Target), String> {
    let builds = world.builds();
    if let Some(want) = variant
        && !builds
            .iter()
            .any(|(v, _)| v.as_ref().is_some_and(|v| v.as_str() == want))
    {
        return Err(format!("the manifest declares no variant `{want}`"));
    }
    builds
        .into_iter()
        .find(|(v, target)| {
            *target == Target::Pdf
                && variant.is_none_or(|want| v.as_ref().is_some_and(|v| v.as_str() == want))
        })
        .ok_or_else(|| match variant {
            Some(v) => format!("the variant `{v}` is not built for PDF, which the preview shows"),
            None => "no variant is built for PDF, which the preview shows".into(),
        })
}

/// Checks a build on a handle of the world and prepares what changed
/// since `before` to send; `None` when an edit cancelled it.
///
/// When a client shows pages (`visible`, the last of them), the pages sent
/// before go past it, and the build still has pages after it, those
/// through it are laid out first, and what
/// changed of them is handed to `first` to send before the check goes on
/// (SPEC §12.2). Strictly forward page breaking needs the pages before
/// the last visible one anyway; the check then finds them in the page
/// cache and lays out only the rest.
#[allow(clippy::too_many_arguments)]
fn check(
    world: &World,
    build: Result<(Option<Name>, Target), String>,
    mut before: Option<Pages>,
    published: &Published,
    files: Arc<dyn Files + Send + Sync>,
    mut diagnostics: Vec<Diagnostic>,
    project_errors: bool,
    visible: Option<usize>,
    first: &mut dyn FnMut(First),
) -> Option<Frame> {
    let (variant, target) = match build {
        Ok(build) => build,
        Err(message) => {
            diagnostics.push(error(message.clone()));
            return Some(Frame {
                variant: None,
                target: Target::Pdf.as_str(),
                pages: None,
                lists: Vec::new(),
                banner: Some(message),
                diagnostics,
                files,
                fonts: published.fonts.clone(),
                fonts_changed: false,
            });
        }
    };
    let name = variant.as_ref().map(|v| v.as_str().to_owned());
    let banner = project_errors.then(|| {
        "folio.toml or folio.lock has errors: the pages follow their last valid version".into()
    });
    // The fonts the clients have, and whether they must drop their glyphs.
    let mut fonts_before = published.fonts.clone();
    let mut redraw = before.is_none();
    // When the pages sent before end by the last visible one, so will
    // these most likely: laying them out first would only add work.
    if let Some(last) = visible
        && last < published.pages.len().saturating_sub(1)
        && let Some(laid) = folio::pages_through(world, target, variant.as_ref(), last).ok()?
        && !laid.complete
    {
        let pages = laid.pages;
        let fonts = Arc::new(FontTable::new(&folio::fonts(world).ok()?));
        let fonts_changed = redraw || !fonts.same_faces(&fonts_before);
        let count = pages.pages.len();
        // Pages kept beyond the first may name fonts by ids that now name
        // others: then every page waits for the check.
        let keeps_pages = published.pages.len() > count;
        if !(fonts_changed && keeps_pages) {
            let (changed, remapped) = match &before {
                Some(before) => (pages.changed(before), pages.remapped(before)),
                None => ((0..count).collect(), Vec::new()),
            };
            let mut lists = Vec::with_capacity(changed.len() + remapped.len());
            for index in changed {
                lists.push((index, entry(&pages, index), false));
            }
            for index in remapped {
                lists.push((index, entry(&pages, index), true));
            }
            // Nothing to show before the check: it sends what it finds.
            let shows = !lists.is_empty() || fonts_changed;
            let merged = merge(&pages, before.as_ref());
            if shows {
                first(First {
                    revision: 0,
                    variant: name.clone(),
                    target: target.as_str(),
                    pages: merged.clone(),
                    laid: count,
                    lists,
                    banner: banner.clone(),
                    files: files.clone(),
                    fonts: fonts.clone(),
                    fonts_changed,
                });
                before = Some(merged);
                fonts_before = fonts;
                redraw = false;
            }
        }
    }
    let checked = folio::check(world, target, variant.as_ref()).ok()?;
    let panicked = checked
        .diagnostics
        .iter()
        .any(|d| d.code.as_deref() == Some(INTERNAL_ERROR));
    diagnostics.extend(checked.diagnostics.iter().cloned());
    // A check that panicked did not lay the pages out, and laying them
    // out now would panic again.
    let pages = match panicked {
        true => None,
        false => folio::pages(world, target, variant.as_ref()).ok()?,
    };
    let Some(pages) = pages else {
        return Some(Frame {
            variant: name,
            target: target.as_str(),
            pages: None,
            lists: Vec::new(),
            banner: Some(match panicked {
                true => "the compiler failed on this revision; the last good pages stay".into(),
                false => "nothing could be laid out; the last good pages stay".into(),
            }),
            diagnostics,
            files,
            fonts: fonts_before,
            fonts_changed: false,
        });
    };
    let fonts = Arc::new(FontTable::new(&folio::fonts(world).ok()?));
    let (changed, remapped) = match &before {
        Some(before) => (pages.changed(before), pages.remapped(before)),
        None => ((0..pages.pages.len()).collect(), Vec::new()),
    };
    let mut lists = Vec::with_capacity(changed.len() + remapped.len());
    for index in changed {
        lists.push((index, entry(&pages, index), false));
    }
    for index in remapped {
        lists.push((index, entry(&pages, index), true));
    }
    let fonts_changed = redraw || !fonts.same_faces(&fonts_before);
    Some(Frame {
        variant: name,
        target: target.as_str(),
        pages: Some(pages),
        lists,
        banner,
        diagnostics,
        files,
        fonts,
        fonts_changed,
    })
}

/// The pages the clients have once the first pages of a revision are sent:
/// those pages, followed by the pages of `before` beyond them.
fn merge(first: &Pages, before: Option<&Pages>) -> Pages {
    let mut merged = first.clone();
    if let Some(before) = before {
        for i in first.pages.len()..before.pages.len() {
            merged.pages.push(before.pages[i].clone());
            merged.hashes.push(before.hashes[i]);
        }
    }
    merged
}

/// Indexes the pages of a check, and finds where its diagnostics point.
fn index(revision: u64, pages: &Pages, files: &dyn Files, diagnostics: &[Diagnostic]) -> Indexed {
    let fragments: Vec<_> = pages.pages.iter().map(|page| &page.fragment).collect();
    let index = span_map::Index::new(files, &fragments);
    let overlays = overlays(&index, files, diagnostics);
    Indexed {
        revision,
        index,
        overlays,
    }
}

/// The areas of the pages that diagnostics point at.
fn overlays(
    index: &span_map::Index,
    files: &dyn Files,
    diagnostics: &[Diagnostic],
) -> Vec<Overlay> {
    let mut out = Vec::new();
    for (i, diagnostic) in diagnostics.iter().enumerate().take(MAX_OVERLAYS) {
        let Some(location) = folio::diag::locate(files, diagnostic.span) else {
            continue;
        };
        let mut by_page: Vec<Overlay> = Vec::new();
        for (page, rect) in rects(&index.find(&location.path, location.bytes)) {
            match by_page.last_mut() {
                Some(last) if last.page == page => last.rects.push(rect),
                _ => by_page.push(Overlay {
                    diagnostic: i,
                    page,
                    rects: vec![rect],
                }),
            }
        }
        out.extend(by_page);
    }
    out
}

/// A page's display list, encoded, without its numbers, which the engine
/// gives it when it is sent ([`Engine::done`]).
fn entry(pages: &Pages, index: usize) -> PageEntry {
    let list = DisplayList::from_fragment(&pages.pages[index].fragment);
    let bytes: Arc<[u8]> = wire::encode_with(&list, |_| true).into();
    let spans = wire::encode_spans(&list.spans()).into();
    let mut images: Vec<(u64, folio_component::ImageFormat)> = list
        .items
        .iter()
        .filter_map(|item| match &item.command {
            folio_render::Command::Image(image) => {
                Some((image.image.content_hash(), image.image.format()))
            }
            _ => None,
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    images.retain(|image| seen.insert(*image));
    // Without images, the two encodings are the same.
    let whole = OnceLock::new();
    if images.is_empty() {
        let _ = whole.set(bytes.clone());
    }
    PageEntry {
        content: 0,
        id: 0,
        list: Arc::new(list),
        bytes,
        images: images.into(),
        whole: Arc::new(whole),
        spans,
    }
}

impl PageEntry {
    /// The display list in the wire format with every image's bytes.
    pub(crate) fn whole(&self) -> &Arc<[u8]> {
        self.whole.get_or_init(|| wire::encode(&self.list).into())
    }
}

/// The files diagnostics point into, with the manifest and lockfile when
/// they have errors, and those errors.
fn project_files(
    project: Option<ProjectErrors>,
    files: Option<Arc<dyn Files + Send + Sync>>,
) -> (Arc<dyn Files + Send + Sync>, Vec<Diagnostic>) {
    match project {
        None => (
            files.unwrap_or_else(|| Arc::new(TextFiles::new())),
            Vec::new(),
        ),
        Some(errors) => {
            let diagnostics = errors.diagnostics.clone();
            let files = Arc::new(Layered {
                project: errors.files,
                world: files,
            });
            (files, diagnostics)
        }
    }
}

/// The manifest's and lockfile's texts over the files of a world.
struct Layered {
    project: TextFiles,
    world: Option<Arc<dyn Files + Send + Sync>>,
}

impl Files for Layered {
    fn path(&self, id: FileId) -> Option<EcoString> {
        self.project
            .path(id)
            .or_else(|| self.world.as_ref()?.path(id))
    }

    fn source(&self, id: FileId) -> Option<Source> {
        self.project
            .source(id)
            .or_else(|| self.world.as_ref()?.source(id))
    }

    fn lines(&self, id: FileId) -> Option<Lines<String>> {
        self.project
            .lines(id)
            .or_else(|| self.world.as_ref()?.lines(id))
    }
}

/// The world's sources before the loader reads a batch, by project path,
/// and the paths whose hold ran out, by their paths and the paths they
/// resolve to, which are read as they are ([`Engine::hold`]).
struct Before {
    sources: HashMap<String, Source>,
    released: BTreeSet<PathBuf>,
}

/// Sends a check's end as cancelled when it is dropped before the check
/// sends its own, as when the check panics.
struct Ended {
    tx: Option<Sender<Input>>,
    revision: u64,
}

impl Drop for Ended {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Input::Done(Box::new(Done {
                revision: self.revision,
                frame: None,
            })));
        }
    }
}

/// Whether a source whose world's text is `old` bytes long and which
/// reads `new` bytes long may have been read between the truncate and the
/// end of the write of a save in place: it is read while it is `writing`
/// (open for writing, or with an event about it waiting), whatever its
/// length, as a write in chunks may already be longer; or it reads empty
/// while the world's text is not, since its truncate may come after the
/// close that the read follows. A file emptied on purpose is therefore
/// read [`MAX_HOLD`] late.
fn torn(old: u64, new: u64, writing: bool) -> bool {
    writing || (new == 0 && old > 0)
}

/// An error that points at no source.
fn error(message: String) -> Diagnostic {
    Diagnostic::error(DiagSpan::detached(), message)
}

#[cfg(test)]
mod tests {
    use super::torn;

    #[test]
    fn reads_that_may_fall_inside_a_save_are_torn() {
        // Empty while the world's text is not: torn, open or not.
        assert!(torn(100, 0, false));
        // Any length while it is written, as a write in chunks may be
        // longer already; else never.
        assert!(torn(100, 40, true));
        assert!(torn(100, 140, true));
        assert!(!torn(100, 40, false));
        assert!(!torn(100, 140, false));
        assert!(!torn(0, 0, false));
    }
}
