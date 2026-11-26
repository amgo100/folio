//! The engine: the project's world, kept in step with the disk, checked
//! after every batch of changes, with what the clients are sent.
//!
//! The engine's thread owns the world and applies changes to it. Each
//! revision is checked on a thread of its own with a handle of the world;
//! a change cancels the check still running, since setting an input of the
//! query database waits for every other handle to be dropped, and the
//! cancelled check drops its handle at once (SPEC §2.3).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use ecow::EcoString;
use folio::diag::{Files, TextFiles};
use folio::loader::{self, LoadError, Loader, ProjectErrors};
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
    /// A check finished.
    Done(Box<Done>),
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
#[derive(Debug, Default)]
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
}

/// A page as the clients are sent it.
#[derive(Debug, Clone)]
pub(crate) struct PageEntry {
    /// The page's content hash.
    pub(crate) hash: u64,
    /// One more each time the page's spans change while its content hash
    /// stays: a client that has an older one is sent the span map.
    pub(crate) generation: u64,
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

/// Starts the engine for the project in `root` and its first check.
pub(crate) fn start(root: &Path, options: &Options) -> Result<Started, Error> {
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
    let (publish, published) = watch::channel(Arc::new(Published {
        target: Target::Pdf.as_str(),
        ..Published::default()
    }));
    let mut engine = Engine {
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
    };
    engine.load();
    let thread = thread::Builder::new()
        .name("folio-preview".into())
        .spawn(move || engine.run(&rx))
        .map_err(|e| Error(format!("cannot start the preview: {e}")))?;
    Ok(Started {
        handle: Handle(tx),
        published,
        thread,
    })
}

/// A started engine.
pub(crate) struct Started {
    /// The handle that tells it what changed.
    pub(crate) handle: Handle,
    /// What it publishes for the clients.
    pub(crate) published: watch::Receiver<Arc<Published>>,
    /// Its thread.
    pub(crate) thread: JoinHandle<()>,
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
            Input::Done(done) => self.done(*done),
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
                    "folio.toml or folio.lock has errors, so the project cannot be built".into(),
                ),
                diagnostics,
                files,
                fonts: self.publish.borrow().fonts.clone(),
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
        let spawned = thread::Builder::new()
            .name("folio-preview-check".into())
            .spawn(move || {
                let project_errors = project.is_some();
                let (files, mut diagnostics) = project_files(project, Some(Arc::new(snapshot)));
                diagnostics.extend(warnings);
                let frame = check(
                    &handle,
                    build,
                    before.as_ref(),
                    &published,
                    files,
                    diagnostics,
                    project_errors,
                );
                // The next edit waits for every handle to go.
                drop(handle);
                let _ = tx.send(Input::Done(Box::new(Done { revision, frame })));
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
            for (index, entry, spans_only) in frame.lists {
                if spans_only {
                    spans.push(index);
                } else {
                    lists.push(index);
                }
                if index < pages.len() {
                    pages[index] = entry;
                } else {
                    pages.push(entry);
                }
            }
            self.last = frame.pages.clone();
            self.redraw = false;
        }
        let resolved: Vec<_> = frame
            .diagnostics
            .iter()
            .map(|d| folio::diag::resolve(&*frame.files, d))
            .collect();
        let count = pages.len();
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

/// The build to preview: the chosen variant, or the first declared, for
/// PDF, or else for the live target, which is laid out too.
fn choose(world: &World, variant: Option<&str>) -> Result<(Option<Name>, Target), String> {
    let builds = world.builds();
    let of_variant: Vec<_> = builds
        .iter()
        .filter(|(v, _)| variant.is_none_or(|want| v.as_ref().is_some_and(|v| v.as_str() == want)))
        .collect();
    if of_variant.is_empty() {
        let variant = variant.unwrap_or_default();
        return Err(format!("the manifest declares no variant `{variant}`"));
    }
    for target in [Target::Pdf, Target::Live] {
        if let Some((v, t)) = of_variant.iter().find(|(_, t)| *t == target) {
            return Ok((v.clone(), *t));
        }
    }
    Err(match variant {
        Some(v) => format!("the variant `{v}` is built for no target that is laid out (pdf)"),
        None => "no variant is built for a target that is laid out (pdf)".into(),
    })
}

/// Checks a build on a handle of the world and prepares what changed
/// since `before` to send; `None` when an edit cancelled it.
fn check(
    world: &World,
    build: Result<(Option<Name>, Target), String>,
    before: Option<&Pages>,
    published: &Published,
    files: Arc<dyn Files + Send + Sync>,
    mut diagnostics: Vec<Diagnostic>,
    project_errors: bool,
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
            });
        }
    };
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
    let name = variant.as_ref().map(|v| v.as_str().to_owned());
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
            fonts: published.fonts.clone(),
        });
    };
    let fonts = Arc::new(FontTable::new(&folio::fonts(world).ok()?));
    let (changed, remapped) = match before {
        Some(before) => (pages.changed(before), pages.remapped(before)),
        None => ((0..pages.pages.len()).collect(), Vec::new()),
    };
    let mut lists = Vec::with_capacity(changed.len() + remapped.len());
    for index in changed {
        lists.push((index, entry(&pages, index, 0), false));
    }
    for index in remapped {
        let generation = published
            .pages
            .get(index)
            .map_or(0, |entry| entry.generation + 1);
        lists.push((index, entry(&pages, index, generation), true));
    }
    Some(Frame {
        variant: name,
        target: target.as_str(),
        pages: Some(pages),
        lists,
        banner: project_errors.then(|| {
            "folio.toml or folio.lock has errors: the pages follow their last valid version".into()
        }),
        diagnostics,
        files,
        fonts,
    })
}

/// A page's display list, encoded.
fn entry(pages: &Pages, index: usize, generation: u64) -> PageEntry {
    let list = DisplayList::from_fragment(&pages.pages[index].fragment);
    let bytes = wire::encode(&list).into();
    let spans = wire::encode_spans(&list.spans()).into();
    PageEntry {
        hash: pages.hashes[index],
        generation,
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
