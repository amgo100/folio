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

/// The code of the diagnostic that stands for a panic in a query.
const INTERNAL_ERROR: &str = "internal-error";

/// What the engine's thread is told.
enum Input {
    /// Files or directories changed.
    Paths(Vec<PathBuf>),
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

/// A handle on the engine's thread.
#[derive(Clone)]
pub(crate) struct Handle(Sender<Input>);

impl Handle {
    /// Tells the engine that the paths changed.
    pub(crate) fn changed(&self, paths: Vec<PathBuf>) {
        let _ = self.0.send(Input::Paths(paths));
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
    /// The display list in the wire format.
    pub(crate) bytes: Arc<[u8]>,
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
    };
    Ok(Pending {
        handle: Handle(tx),
        published,
        visible,
        engine,
        rx,
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
    rx: Receiver<Input>,
}

impl Pending {
    /// Starts the engine's thread, which reads the project, checks it,
    /// and then applies the changes that queued meanwhile.
    pub(crate) fn spawn(self) -> Result<JoinHandle<()>, Error> {
        let Self { mut engine, rx, .. } = self;
        thread::Builder::new()
            .name("folio-preview".into())
            .spawn(move || {
                engine.load();
                engine.run(&rx);
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
}

impl Engine {
    fn run(mut self, rx: &Receiver<Input>) {
        loop {
            if self.dirty {
                self.check();
            }
            let input = match self.idle_at {
                Some(at) => match rx.recv_deadline(at) {
                    Ok(input) => input,
                    Err(RecvTimeoutError::Timeout) => {
                        self.idle_at = None;
                        self.write_outputs();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(input) => input,
                    Err(_) => return,
                },
            };
            match input {
                Input::Paths(paths) => {
                    let Some(paths) = self.debounce(rx, paths) else {
                        return;
                    };
                    self.apply(&paths);
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
            Input::Paths(_) => unreachable!("handled by the caller"),
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
    fn debounce(&mut self, rx: &Receiver<Input>, mut paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
        let until = Instant::now() + MAX_DEBOUNCE;
        loop {
            let deadline = (Instant::now() + self.settings.debounce).min(until);
            match rx.recv_deadline(deadline) {
                Ok(Input::Paths(more)) => paths.extend(more),
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

    /// Builds the project's world from the disk.
    fn load(&mut self) {
        self.world = None;
        self.errors = None;
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
        let names = |name: &str| {
            paths
                .iter()
                .any(|path| self.loader_path(path).is_some_and(|rel| rel == name))
        };
        let project_file = names(loader::MANIFEST) || names(loader::LOCKFILE);
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
        let update = match self.loader.update(world, &paths, &mut self.warnings) {
            Ok(update) => update,
            Err(e) => {
                self.warnings.push(error(e.to_string()));
                self.edited();
                return;
            }
        };
        if project_file {
            self.errors = update.errors;
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
        if !update.changed.is_empty() || project_file || !self.warnings.is_empty() {
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
            self.dirty = true;
        }
    }

    /// Sends what a check found, unless an edit has overtaken it.
    fn done(&mut self, done: Done) {
        let revision = done.revision;
        let Some(frame) = done.frame else {
            // Cancelled by an edit, whose revision is checked next.
            return;
        };
        if revision != self.revision {
            self.settings.emit(Event::Stale { revision });
            return;
        }
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
        // source, which has changed since.
        let layout = frame.pages.as_ref().map(|_| {
            Arc::new(Layout {
                revision,
                laid: count,
                files: frame.files.clone(),
                index: None,
            })
        });
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
        if self.world.is_some() && self.written < revision && self.settings.on_outputs.is_some() {
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
        let old = self.publish.borrow().clone();
        let mut pages = old.pages.clone();
        let (lists, spans) = self.place(&mut pages, first.lists);
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
            // They marked the pages as they were.
            overlays: Arc::new(Vec::new()),
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

    /// Compiles every build on a thread of its own and hands them to the
    /// outputs hook. An edit cancels the compilation.
    fn write_outputs(&mut self) {
        let (Some(world), Some(hook)) = (&self.world, self.settings.on_outputs.clone()) else {
            return;
        };
        let revision = self.revision;
        self.written = revision;
        let handle = world.clone();
        let files: Arc<dyn Files + Send + Sync> = Arc::new(world.snapshot());
        let tx = self.tx.clone();
        let _ = thread::Builder::new()
            .name("folio-preview-write".into())
            .spawn(move || {
                let builds = handle.builds();
                let manifest = handle.manifest().clone();
                let outputs = folio::compile_builds(&handle, builds);
                drop(handle);
                if outputs.iter().any(|o| o.result.is_err()) {
                    // An edit overtook it; its revision is written instead.
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
/// When a client shows pages (`visible`, the last of them) and the build
/// has pages after them, those through it are laid out first, and what
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
    if let Some(last) = visible
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
            let merged = merge(&pages, before.as_ref());
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
    let bytes = wire::encode(&list).into();
    let spans = wire::encode_spans(&list.spans()).into();
    PageEntry {
        content: 0,
        id: 0,
        list: Arc::new(list),
        bytes,
        spans,
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

/// An error that points at no source.
fn error(message: String) -> Diagnostic {
    Diagnostic::error(DiagSpan::detached(), message)
}
