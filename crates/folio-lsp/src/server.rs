//! The server's state and main loop.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, select};
use folio::diag::Files;
use folio::loader::{self, LoadError, Loader, ProjectErrors};
use folio::{Diagnostic, FontCache, Manifest, World};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{self as note, Notification as _};
use lsp_types::request::{self as req, Request as _};
use lsp_types::{self as lsp, Uri};

use crate::convert::{self, Encoding, LineIndex, Uris};
use crate::{Error, Event, Hook, Options, Outcome};

/// Messages to log, with their kinds.
type Messages = Vec<(lsp::MessageType, String)>;

/// Runs the server on a connection until the client shuts it down or goes
/// away: answers `initialize`, then handles messages. The caller joins the
/// connection's I/O threads after.
pub fn run(connection: &Connection, options: Options) -> Result<(), Error> {
    let (id, params) = connection.initialize_start()?;
    let params: lsp::InitializeParams = serde_json::from_value(params)
        .map_err(|e| Error(format!("the `initialize` request is not valid: {e}")))?;
    let encoding = Encoding::choose(
        params
            .capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_deref()),
    );
    let capabilities = lsp::ServerCapabilities {
        position_encoding: Some(encoding.kind()),
        text_document_sync: Some(lsp::TextDocumentSyncCapability::Options(
            lsp::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(lsp::TextDocumentSyncKind::INCREMENTAL),
                save: Some(lsp::TextDocumentSyncSaveOptions::Supported(true)),
                ..lsp::TextDocumentSyncOptions::default()
            },
        )),
        ..lsp::ServerCapabilities::default()
    };
    let result = lsp::InitializeResult {
        capabilities,
        server_info: Some(lsp::ServerInfo {
            name: "folio".into(),
            version: Some(folio::VERSION.into()),
        }),
    };
    let result = serde_json::to_value(result).expect("the result serialises");
    connection.initialize_finish(id, result)?;

    let (results_tx, results_rx) = crossbeam_channel::unbounded();
    let mut server = Server {
        connection,
        encoding,
        options,
        project: None,
        docs: HashMap::new(),
        revision: 0,
        dirty: false,
        results_tx,
        published: BTreeMap::new(),
        noticed: None,
        request_id: 0,
    };
    server.watch_files(&params);
    if let Some(root) = server.initial_root(&params) {
        server.open_project(root);
    }
    server.main_loop(&results_rx)
}

/// A file the client has open: its text is the editor's buffer, which may
/// not be saved.
struct Doc {
    uri: Uri,
    version: i32,
    text: String,
}

/// The project the server checks.
struct Project {
    root: PathBuf,
    /// The project directory with links resolved, which paths from the
    /// client may be under instead.
    canonical: PathBuf,
    loader: Loader,
    /// `None` while `folio.toml` or `folio.lock` has errors that keep the
    /// project from loading at all.
    world: Option<World>,
    /// The errors of the manifest or lockfile on disk, which the world did
    /// not take, and the texts they point into.
    errors: Option<ProjectErrors>,
}

impl Project {
    /// The project path of a path on this machine, if it is inside the
    /// project.
    fn project_path(&self, path: &Path) -> Option<String> {
        let rel = match path
            .strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.canonical))
        {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => path
                .canonicalize()
                .ok()?
                .strip_prefix(&self.canonical)
                .ok()?
                .to_path_buf(),
        };
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        let joined = parts?.join("/");
        (!joined.is_empty()).then_some(joined)
    }
}

/// What a check thread hands back.
struct Done {
    revision: u64,
    /// The diagnostics by project path, or `None` when the check was
    /// cancelled.
    files: Option<BTreeMap<String, Vec<lsp::Diagnostic>>>,
    /// The versions of the open files the check saw, by project path.
    versions: HashMap<String, i32>,
}

struct Server<'a> {
    connection: &'a Connection,
    encoding: Encoding,
    options: Options,
    project: Option<Project>,
    /// The open sources of the project, by project path.
    docs: HashMap<String, Doc>,
    /// The revision of the world: one more for every batch of edits.
    revision: u64,
    /// Whether the world changed since the last check started.
    dirty: bool,
    results_tx: Sender<Done>,
    /// The files that have diagnostics shown, by project path, with the URI
    /// they were published under.
    published: BTreeMap<String, Uri>,
    /// The compiler pin the server last gave notice of.
    noticed: Option<String>,
    request_id: i32,
}

impl Server<'_> {
    fn main_loop(&mut self, results: &Receiver<Done>) -> Result<(), Error> {
        loop {
            // Edits that arrived together are applied before checking.
            if self.dirty && self.connection.receiver.is_empty() {
                self.check();
            }
            select! {
                recv(self.connection.receiver) -> message => {
                    let Ok(message) = message else { return Ok(()) };
                    match message {
                        Message::Request(request) => {
                            if self.connection.handle_shutdown(&request)? {
                                return Ok(());
                            }
                            self.request(request);
                        }
                        Message::Notification(notification) => {
                            if notification.method == note::Exit::METHOD {
                                return Ok(());
                            }
                            self.notification(notification);
                        }
                        Message::Response(_) => {}
                    }
                }
                recv(results) -> done => {
                    if let Ok(done) = done {
                        self.done(done);
                    }
                }
            }
        }
    }

    fn emit(&self, event: Event) {
        if let Some(hook) = &self.options.on_event {
            hook(&event);
        }
    }

    fn send(&self, message: impl Into<Message>) {
        // A client that went away is noticed by the main loop.
        let _ = self.connection.sender.send(message.into());
    }

    fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        self.send(Notification::new(N::METHOD.into(), params));
    }

    fn show(&self, typ: lsp::MessageType, message: String) {
        self.notify::<note::ShowMessage>(lsp::ShowMessageParams { typ, message });
    }

    fn log(&self, typ: lsp::MessageType, message: String) {
        self.notify::<note::LogMessage>(lsp::LogMessageParams { typ, message });
    }

    fn request(&self, request: Request) {
        self.send(Response::new_err(
            request.id,
            ErrorCode::MethodNotFound as i32,
            format!("`{}` is not supported", request.method),
        ));
    }

    /// Asks the client to report changes to every file, when it can be
    /// asked: the loader reads the files it names again.
    fn watch_files(&mut self, params: &lsp::InitializeParams) {
        let dynamic = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.did_change_watched_files.as_ref())
            .and_then(|w| w.dynamic_registration)
            .unwrap_or(false);
        if !dynamic {
            return;
        }
        let options = lsp::DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![lsp::FileSystemWatcher {
                glob_pattern: lsp::GlobPattern::String("**/*".into()),
                kind: None,
            }],
        };
        let registration = lsp::Registration {
            id: "folio-watch".into(),
            method: note::DidChangeWatchedFiles::METHOD.into(),
            register_options: Some(serde_json::to_value(options).expect("options serialise")),
        };
        self.request_id += 1;
        self.send(Request::new(
            self.request_id.into(),
            req::RegisterCapability::METHOD.into(),
            lsp::RegistrationParams {
                registrations: vec![registration],
            },
        ));
    }

    /// The project directory to start with: the one given, or the nearest
    /// one from a workspace folder up.
    fn initial_root(&self, params: &lsp::InitializeParams) -> Option<PathBuf> {
        if let Some(root) = &self.options.root {
            return Some(root.clone());
        }
        #[allow(deprecated)]
        let folders = params
            .workspace_folders
            .iter()
            .flatten()
            .map(|f| &f.uri)
            .chain(&params.root_uri);
        folders
            .filter_map(convert::uri_to_path)
            .find_map(|dir| find_root(&dir))
    }

    /// Loads the project in `root`.
    fn open_project(&mut self, root: PathBuf) {
        let cache = FontCache::default_dir().map(FontCache::new);
        let loader = match Loader::new(&root) {
            Ok(loader) => loader.with_font_cache(cache),
            Err(e) => {
                self.show(lsp::MessageType::ERROR, format!("Folio: {e}"));
                return;
            }
        };
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        self.project = Some(Project {
            root,
            canonical,
            loader,
            world: None,
            errors: None,
        });
        self.load_world();
    }

    /// Builds the project's world from the disk, with the open buffers on
    /// top.
    fn load_world(&mut self) {
        let Some(mut project) = self.project.take() else {
            return;
        };
        let loaded = self.build_world(&mut project);
        self.project = Some(project);
        match loaded {
            Ok((pin, messages)) => {
                for (typ, message) in messages {
                    self.log(typ, message);
                }
                if let Some(pin) = pin {
                    self.notice_pin(&pin);
                }
            }
            Err(message) => self.show(lsp::MessageType::ERROR, message),
        }
        self.edited();
    }

    /// [`Self::load_world`] for a project taken out of the server: the
    /// version the manifest pins, if it can be read, and what to log; or
    /// why the project cannot be loaded at all.
    fn build_world(&self, project: &mut Project) -> Result<(Option<String>, Messages), String> {
        project.world = None;
        project.errors = None;
        let mut builder = World::builder().long_lived(true);
        if let Some(hook) = self.options.on_event.clone() {
            builder = builder.on_event(move |event| hook(&Event::World(event.clone())));
        }
        let mut world = match project.loader.world(builder) {
            Ok(world) => world,
            Err(LoadError::Project(errors)) => {
                // A manifest that only the pinned version understands is
                // the likeliest cause: say which version it pins.
                let pin = project
                    .loader
                    .read_manifest()
                    .ok()
                    .and_then(|text| Manifest::compiler_pin(&text, World::MANIFEST))
                    .map(|pin| pin.v.to_string());
                project.errors = Some(errors);
                return Ok((pin, Vec::new()));
            }
            Err(e) => return Err(format!("Folio: {e}")),
        };
        let mut warnings = Vec::new();
        project
            .loader
            .load(&mut world, &mut warnings)
            .map_err(|e| format!("Folio: {e}"))?;
        let mut messages = messages(&warnings);
        let cache = FontCache::default_dir().map(FontCache::new);
        if let Err(e) = world.load_fonts(&project.root, cache.as_ref()) {
            messages.push((
                lsp::MessageType::ERROR,
                format!("Folio: cannot read the project's fonts: {e}"),
            ));
        }
        if self.options.system_fonts.unwrap_or(true) {
            world.set_system_fonts(folio::system_fonts());
        }
        for (path, doc) in &self.docs {
            let _ = world.set_source(path, &doc.text);
        }
        let pin = world.manifest().project.compiler.v.to_string();
        project.world = Some(world);
        Ok((Some(pin), messages))
    }

    /// Gives notice once when the project pins a compiler version other
    /// than this one.
    fn notice_pin(&mut self, pin: &str) {
        if pin == folio::VERSION || self.noticed.as_deref() == Some(pin) {
            return;
        }
        self.noticed = Some(pin.to_owned());
        self.show(
            lsp::MessageType::WARNING,
            format!(
                "This project pins Folio {pin}, but the language server is Folio {}: \
                 its diagnostics may differ from those of `folio build`.",
                folio::VERSION
            ),
        );
    }

    /// Logs what loading found that is not in any file.
    fn warn(&self, warnings: &[Diagnostic]) {
        for (typ, message) in messages(warnings) {
            self.log(typ, message);
        }
    }

    /// Marks the world as changed.
    fn edited(&mut self) {
        self.revision += 1;
        self.dirty = true;
        self.emit(Event::Edited {
            revision: self.revision,
        });
    }

    fn notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            note::DidOpenTextDocument::METHOD => {
                if let Ok(params) = notification.extract(note::DidOpenTextDocument::METHOD) {
                    self.did_open(params);
                }
            }
            note::DidChangeTextDocument::METHOD => {
                if let Ok(params) = notification.extract(note::DidChangeTextDocument::METHOD) {
                    self.did_change(params);
                }
            }
            note::DidCloseTextDocument::METHOD => {
                if let Ok(params) = notification
                    .extract::<lsp::DidCloseTextDocumentParams>(note::DidCloseTextDocument::METHOD)
                {
                    self.did_close(&params.text_document.uri);
                }
            }
            note::DidSaveTextDocument::METHOD => {
                if let Ok(params) = notification
                    .extract::<lsp::DidSaveTextDocumentParams>(note::DidSaveTextDocument::METHOD)
                    && let Some(path) = convert::uri_to_path(&params.text_document.uri)
                {
                    self.disk_changed(vec![path]);
                }
            }
            note::DidChangeWatchedFiles::METHOD => {
                if let Ok(params) = notification.extract::<lsp::DidChangeWatchedFilesParams>(
                    note::DidChangeWatchedFiles::METHOD,
                ) {
                    let paths = params
                        .changes
                        .iter()
                        .filter_map(|change| convert::uri_to_path(&change.uri))
                        .collect();
                    self.disk_changed(paths);
                }
            }
            _ => {}
        }
    }

    /// The project path of a source the client names, finding the project
    /// from the file up when there is none yet.
    fn source_path(&mut self, uri: &Uri) -> Option<String> {
        let path = convert::uri_to_path(uri)?;
        if self.project.is_none()
            && let Some(root) = path.parent().and_then(find_root)
        {
            self.open_project(root);
        }
        let rel = self.project.as_ref()?.project_path(&path)?;
        matches!(Path::new(&rel).extension()?.to_str()?, "fol" | "fsty").then_some(rel)
    }

    fn did_open(&mut self, params: lsp::DidOpenTextDocumentParams) {
        let document = params.text_document;
        let Some(path) = self.source_path(&document.uri) else {
            return;
        };
        if let Some(world) = self.world_mut() {
            let _ = world.set_source(&path, &document.text);
        }
        self.docs.insert(
            path,
            Doc {
                uri: document.uri,
                version: document.version,
                text: document.text,
            },
        );
        self.edited();
    }

    fn did_change(&mut self, params: lsp::DidChangeTextDocumentParams) {
        let Some(path) = self.path_of_open(&params.text_document.uri) else {
            return;
        };
        let encoding = self.encoding;
        let Some(doc) = self.docs.get_mut(&path) else {
            return;
        };
        doc.version = params.text_document.version;
        // The edits to make to the world, in order: a byte range and its
        // new text, or the whole text.
        let mut edits = Vec::new();
        for change in params.content_changes {
            match change.range {
                Some(range) => {
                    let index = LineIndex::new(&doc.text);
                    let start = index.offset(range.start, encoding);
                    let end = index.offset(range.end, encoding).max(start);
                    doc.text.replace_range(start..end, &change.text);
                    edits.push((Some(start..end), change.text));
                }
                None => {
                    doc.text.clone_from(&change.text);
                    edits.push((None, change.text));
                }
            }
        }
        let text = doc.text.clone();
        if let Some(world) = self.world_mut() {
            for (range, with) in edits {
                let applied = match range {
                    Some(range) => world.edit_source(&path, range, &with).is_ok(),
                    None => world.set_source(&path, &with).is_ok(),
                };
                if !applied {
                    // The world and the buffer disagree: take the buffer.
                    let _ = world.set_source(&path, &text);
                    break;
                }
            }
        }
        self.edited();
    }

    fn did_close(&mut self, uri: &Uri) {
        let Some(path) = self.path_of_open(uri) else {
            return;
        };
        self.docs.remove(&path);
        // The file is what the disk has again.
        if let Some(project) = &self.project {
            let file = project.root.join(&path);
            self.disk_changed(vec![file]);
        }
    }

    /// The project path of an open file.
    fn path_of_open(&self, uri: &Uri) -> Option<String> {
        let path = convert::uri_to_path(uri)?;
        let rel = self.project.as_ref()?.project_path(&path)?;
        self.docs.contains_key(&rel).then_some(rel)
    }

    fn world_mut(&mut self) -> Option<&mut World> {
        self.project.as_mut()?.world.as_mut()
    }

    /// Brings the world in step with the disk at paths that changed there.
    fn disk_changed(&mut self, paths: Vec<PathBuf>) {
        let Some(project) = &mut self.project else {
            return;
        };
        let names_project_file = paths.iter().any(|path| {
            project
                .project_path(path)
                .is_some_and(|rel| rel == loader::MANIFEST || rel == loader::LOCKFILE)
        });
        let Some(world) = &mut project.world else {
            // The manifest or lockfile kept the project from loading.
            if names_project_file {
                self.load_world();
            }
            return;
        };
        let mut warnings = Vec::new();
        let update = match project.loader.update(world, &paths, &mut warnings) {
            Ok(update) => update,
            Err(e) => {
                self.show(lsp::MessageType::ERROR, format!("Folio: {e}"));
                return;
            }
        };
        if names_project_file {
            project.errors = update.errors;
        }
        // The open buffers stand over the disk.
        for (path, doc) in &self.docs {
            if update.changed.contains(path) {
                let _ = world.set_source(path, &doc.text);
            }
        }
        let pin = world.manifest().project.compiler.v.to_string();
        self.warn(&warnings);
        if update.changed.contains(loader::MANIFEST) {
            self.notice_pin(&pin);
        }
        if !update.changed.is_empty() || names_project_file {
            self.edited();
        }
    }

    /// Starts checking the current revision on a thread of its own. A check
    /// still running for an earlier revision was cancelled by the edits
    /// since.
    fn check(&mut self) {
        self.dirty = false;
        let revision = self.revision;
        let Some(project) = &self.project else {
            return;
        };
        let versions: HashMap<String, i32> = self
            .docs
            .iter()
            .map(|(path, doc)| (path.clone(), doc.version))
            .collect();
        let tx = self.results_tx.clone();
        let Some(world) = &project.world else {
            // Only the manifest's or lockfile's errors are left to show.
            self.emit(Event::Started { revision });
            let _ = tx.send(Done {
                revision,
                files: Some(BTreeMap::new()),
                versions,
            });
            return;
        };
        let handle = world.clone();
        let snapshot = world.snapshot();
        let builds = world.builds();
        let root = project.root.clone();
        let open: HashMap<String, Uri> = self
            .docs
            .iter()
            .map(|(path, doc)| (path.clone(), doc.uri.clone()))
            .collect();
        let encoding = self.encoding;
        let hook: Option<Hook> = self.options.on_event.clone();
        if let Some(hook) = &hook {
            hook(&Event::Started { revision });
        }
        let spawned = thread::Builder::new()
            .name("folio-lsp-check".into())
            .spawn(move || {
                let outputs = folio::check_builds(&handle, builds);
                // The next edit waits for every handle to go.
                drop(handle);
                let mut diagnostics = Vec::new();
                for output in outputs {
                    match output.result {
                        Ok(checked) => diagnostics.extend(checked.diagnostics),
                        Err(folio::Cancelled) => {
                            let _ = tx.send(Done {
                                revision,
                                files: None,
                                versions,
                            });
                            return;
                        }
                    }
                }
                let uris = Uris {
                    root: &root,
                    open: &open,
                };
                let files = convert::diagnostics(&snapshot, &diagnostics, &uris, encoding);
                let _ = tx.send(Done {
                    revision,
                    files: Some(files),
                    versions,
                });
            });
        if let Err(e) = spawned {
            self.log(
                lsp::MessageType::ERROR,
                format!("Folio: cannot start a check: {e}"),
            );
        }
    }

    /// Publishes what a check found, unless an edit has overtaken it.
    fn done(&mut self, done: Done) {
        let revision = done.revision;
        let outcome = match done.files {
            None => Outcome::Cancelled,
            Some(_) if revision != self.revision => Outcome::Stale,
            Some(files) => {
                self.publish(files, &done.versions);
                Outcome::Published
            }
        };
        self.emit(Event::Checked { revision, outcome });
    }

    /// Publishes diagnostics by project path, with the errors of the
    /// manifest and lockfile, and clears those of files that have none
    /// now.
    fn publish(
        &mut self,
        mut files: BTreeMap<String, Vec<lsp::Diagnostic>>,
        versions: &HashMap<String, i32>,
    ) {
        let Some(project) = &self.project else {
            return;
        };
        let open: HashMap<String, Uri> = self
            .docs
            .iter()
            .map(|(path, doc)| (path.clone(), doc.uri.clone()))
            .collect();
        let uris = Uris {
            root: &project.root,
            open: &open,
        };
        if let Some(errors) = &project.errors {
            let found = convert::diagnostics(
                &errors.files as &dyn Files,
                &errors.diagnostics,
                &uris,
                self.encoding,
            );
            for (path, diagnostics) in found {
                files.entry(path).or_default().extend(diagnostics);
            }
        }
        let mut published = BTreeMap::new();
        for (path, diagnostics) in files {
            let Some(uri) = uris.get(&path) else {
                continue;
            };
            self.notify::<note::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: versions.get(&path).copied(),
            });
            published.insert(path, uri);
        }
        for (path, uri) in std::mem::take(&mut self.published) {
            if !published.contains_key(&path) {
                self.notify::<note::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
                    uri,
                    diagnostics: Vec::new(),
                    version: versions.get(&path).copied(),
                });
            }
        }
        self.published = published;
    }
}

/// The nearest directory from `dir` up that holds `folio.toml`.
fn find_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|dir| dir.join(loader::MANIFEST).is_file())
        .map(Path::to_path_buf)
}

/// Messages to log for what loading found that is not in any file.
fn messages(warnings: &[Diagnostic]) -> Messages {
    warnings
        .iter()
        .map(|warning| {
            let typ = if warning.is_error() {
                lsp::MessageType::ERROR
            } else {
                lsp::MessageType::WARNING
            };
            (typ, format!("Folio: {}", warning.message))
        })
        .collect()
}
