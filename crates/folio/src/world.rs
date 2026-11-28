//! The world: everything a compilation reads, held in the query database.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Display, Formatter};
use std::io;
use std::num::NonZeroU16;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use ecow::{EcoString, EcoVec};
use folio_component::{Name, Registry, Target};
use folio_core::db::{
    BuildMode, Bytes, DataFile, Durability, FileContent, FileRead, Project, SourceFile,
};
use folio_core::eval::Limits;
use folio_core::project::{Date, Lockfile, Manifest, ProjectError, ProjectPath};
use folio_fonts::{FontCache, FontFile, FontHash, SystemFont};
use folio_syntax::{FileId, Lines, NodeId, NodeIds, Source, Span};
use salsa::Setter;

use crate::diag::Files;

/// Everything a compilation reads: the manifest and lockfile, sources, data
/// files, fonts and components (SPEC §2.2). It owns the query database that
/// memoises the compile stages, so compiling again after an edit re-runs only
/// what the edit affects (SPEC §2.3).
///
/// A clone is a handle on the same database, cheap to make, which can compile
/// on another thread while this one waits for edits. An edit (any method
/// that takes `&mut self`) first cancels the compilations still running on
/// other handles, which then return [`Cancelled`](crate::Cancelled), and
/// waits until every other handle is dropped. A host that keeps a handle
/// must drop it once its compilation returns, or the next edit waits for
/// ever.
///
/// A source's file id is assigned when its path is set, and freed when the
/// source is removed. A text data file (CSV, JSON, TOML, YAML, BibTeX or
/// plain text) gets one too when it is added, so that errors can point into
/// it, while data files hold fewer than half the ids. Ids are new until about 65,000 have been given out,
/// and then freed ones are given out again, so a world that sees any number
/// of paths over its life never runs out of them. The
/// diagnostics of a compilation therefore stay valid until a source is
/// removed: resolve them first, or against a [`Snapshot`], which keeps the
/// files as they were and holds no handle on the database.
/// [`World::MANIFEST`] and [`World::LOCKFILE`] are reserved.
///
/// The world also gives every node of its sources a [`NodeId`] that lasts
/// across edits ([`World::node_id`], SPEC §12.4). Node ids are kept beside
/// the query database, not in it, so no compilation reads them.
#[derive(Clone)]
pub struct World {
    db: Database,
    project: Project,
    ids: Arc<FileIds>,
    nodes: Arc<NodeIds>,
    /// The text of `folio.toml` and of `folio.lock`, which diagnostics
    /// point into.
    manifest_text: Lines<String>,
    lockfile_text: Option<Lines<String>>,
    /// In a long-lived world, what the last compilation of each build that
    /// did not panic gave, shared by every handle.
    kept: Option<Arc<Kept>>,
}

/// What a long-lived world keeps of each build ([`WorldBuilder::long_lived`]),
/// to answer with when compiling it panics.
#[derive(Debug, Default)]
pub(crate) struct Kept(Mutex<HashMap<(Option<Name>, Target), Last>>);

/// What the last compilation or check of a build that did not panic gave.
#[derive(Debug, Clone, Default)]
pub(crate) struct Last {
    /// Its sources, as [`Compiled::sources`](crate::Compiled::sources).
    pub sources: EcoVec<ProjectPath>,
    /// Its files, as [`Compiled::files`](crate::Compiled::files).
    pub files: EcoVec<FileRead>,
    /// The last output it compiled to: kept across compilations without
    /// output.
    pub output: Option<Bytes>,
}

impl Kept {
    /// What the last compilation or check of a build that did not panic
    /// gave.
    pub(crate) fn get(&self, build: &(Option<Name>, Target)) -> Last {
        self.lock().get(build).cloned().unwrap_or_default()
    }

    /// Updates what is kept of a build, and forgets the builds of variants
    /// that the manifest no longer declares, given the builds it declares
    /// now. A build without a variant is kept, as is a target the manifest
    /// does not declare, since a host may compile it all the same; there
    /// are few of them.
    pub(crate) fn update(
        &self,
        build: (Option<Name>, Target),
        builds: &[(Option<Name>, Target)],
        f: impl FnOnce(&mut Last),
    ) {
        let mut kept = self.lock();
        let last = kept.remove(&build).unwrap_or_default();
        retain(&mut kept, builds);
        f(kept.entry(build).or_insert(last));
    }

    /// Forgets the builds of variants that the manifest no longer
    /// declares, given the builds it declares now.
    pub(crate) fn forget(&self, builds: &[(Option<Name>, Target)]) {
        retain(&mut self.lock(), builds);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(Option<Name>, Target), Last>> {
        // Each update is one call that does not panic.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Keeps the builds without a variant and those of the variants among
/// `builds`.
fn retain(kept: &mut HashMap<(Option<Name>, Target), Last>, builds: &[(Option<Name>, Target)]) {
    kept.retain(|(variant, _), _| {
        variant.is_none() || builds.iter().any(|(declared, _)| declared == variant)
    });
}

/// What a world reports to the hook set with [`WorldBuilder::on_event`], for
/// tests and profiling.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Event {
    /// A query is about to run, because it has no memoised result or an
    /// input it read has changed. `query` is its name, such as `compile`.
    Execute {
        /// The query's name.
        query: EcoString,
    },
    /// An edit has asked the queries still running on other handles to
    /// stop.
    Cancel,
}

type Hook = Arc<dyn Fn(&Event) + Send + Sync>;

/// Sets up a [`World`]: the components and fonts that come with the
/// compiler, how it builds, and an event hook.
pub struct WorldBuilder {
    components: Registry,
    bundled_fonts: EcoVec<FontFile>,
    mode: BuildMode,
    limits: Limits,
    hook: Option<Hook>,
    long_lived: bool,
}

/// No components, the bundled fonts ([`folio_fonts::bundled`]), a build
/// that writes output, and the default limits.
impl Default for WorldBuilder {
    fn default() -> Self {
        Self {
            components: Registry::default(),
            bundled_fonts: folio_fonts::bundled(),
            mode: BuildMode::default(),
            limits: Limits::default(),
            hook: None,
            long_lived: false,
        }
    }
}

impl WorldBuilder {
    /// The components the compiler hosts. Marked as rarely changing.
    pub fn components(mut self, registry: Registry) -> Self {
        self.components = registry;
        self
    }

    /// The fonts bundled with the compiler, in place of the families that
    /// come with it ([`folio_fonts::bundled`]). Marked as rarely changing.
    pub fn bundled_fonts(mut self, fonts: impl IntoIterator<Item = FontFile>) -> Self {
        self.bundled_fonts = fonts.into_iter().collect();
        self
    }

    /// How the world builds: in watch mode, a build that writes output (the
    /// default), or a CI build. It decides whether an unpinned system font
    /// is allowed, a warning or an error (SPEC §2.4).
    pub fn mode(mut self, mode: BuildMode) -> Self {
        self.mode = mode;
        self
    }

    /// The limits of evaluation: steps, memory and call depth (SPEC §4.4).
    /// The defaults suit any document; a host that runs scripts it does not
    /// trust as far, or a test, can lower them.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Whether the world serves a long-lived process, such as `folio
    /// watch` or the language server (SPEC §12.2, §12.3); by default it
    /// serves one build and exits.
    ///
    /// A panic in a query is a bug in the compiler. In a long-lived world,
    /// [`compile`](crate::compile) and [`check`](crate::check), and so
    /// [`compile_builds`](crate::compile_builds) and
    /// [`check_builds`](crate::check_builds), turn it into an error with
    /// the code `internal-error` and answer with the last output the build
    /// was compiled to, and with every source and file to watch. The
    /// process lives on, and the next edit compiles again. Otherwise the
    /// panic unwinds out of them, as it does out of the other functions in
    /// either kind of world.
    pub fn long_lived(mut self, long_lived: bool) -> Self {
        self.long_lived = long_lived;
        self
    }

    /// Calls `hook` on every [`Event`], on the thread where it happens.
    ///
    /// The hook runs inside the database, during a query or an edit, so it
    /// must not compile or edit the world itself: that deadlocks.
    pub fn on_event(mut self, hook: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        self.hook = Some(Arc::new(hook));
        self
    }

    /// Builds the world from the text of `folio.toml` and of `folio.lock`,
    /// if the project has one. Fails with every error in either file.
    ///
    /// The errors point into the two files by byte range; to show them,
    /// resolve them against a [`TextFiles`](crate::diag::TextFiles) that
    /// holds both texts under [`World::MANIFEST`] and [`World::LOCKFILE`].
    pub fn build(self, manifest: &str, lockfile: Option<&str>) -> Result<World, Vec<ProjectError>> {
        let (manifest_text, lockfile_text) = (manifest, lockfile);
        let manifest = Manifest::parse(manifest, World::MANIFEST);
        let lockfile = parse_lockfile(lockfile);
        let (manifest, lockfile) = match (manifest, lockfile) {
            (Ok(m), Ok(l)) => (m, l),
            (m, l) => {
                return Err(m
                    .err()
                    .into_iter()
                    .flatten()
                    .chain(l.err().into_iter().flatten())
                    .collect());
            }
        };

        let db = Database::new(self.hook);
        let project = Project::builder(
            manifest,
            lockfile,
            BTreeMap::new(),
            BTreeMap::new(),
            self.bundled_fonts,
            EcoVec::new(),
            EcoVec::new(),
            self.mode,
            self.components,
            None,
            self.limits,
        )
        .manifest_durability(Durability::MEDIUM)
        .lockfile_durability(Durability::MEDIUM)
        .sources_durability(Durability::LOW)
        .files_durability(Durability::LOW)
        .bundled_fonts_durability(Durability::HIGH)
        .fonts_durability(Durability::MEDIUM)
        .system_fonts_durability(Durability::MEDIUM)
        .mode_durability(Durability::MEDIUM)
        .components_durability(Durability::HIGH)
        .today_durability(Durability::MEDIUM)
        .limits_durability(Durability::HIGH)
        .new(&db);
        Ok(World {
            db,
            project,
            ids: Arc::default(),
            nodes: Arc::default(),
            manifest_text: Lines::new(manifest_text.to_owned()),
            lockfile_text: lockfile_text.map(|text| Lines::new(text.to_owned())),
            kept: self.long_lived.then(Arc::default),
        })
    }
}

impl Debug for WorldBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldBuilder")
            .field("components", &self.components)
            .field("bundled_fonts", &self.bundled_fonts)
            .field("mode", &self.mode)
            .field("limits", &self.limits)
            .field("hook", &self.hook.is_some())
            .field("long_lived", &self.long_lived)
            .finish()
    }
}

impl World {
    /// The file id of `folio.toml`, which manifest errors point into.
    pub const MANIFEST: FileId = FileId::from_raw(NonZeroU16::MIN);

    /// The file id of `folio.lock`, which lockfile errors point into.
    pub const LOCKFILE: FileId = FileId::from_raw(NonZeroU16::MIN.saturating_add(1));

    /// Sets up a world with components, fonts or an event hook.
    pub fn builder() -> WorldBuilder {
        WorldBuilder::default()
    }

    /// A world with no components, the bundled fonts and no lockfile, from
    /// the text of `folio.toml`.
    pub fn new(manifest: &str) -> Result<Self, Vec<ProjectError>> {
        Self::builder().build(manifest, None)
    }

    /// The manifest.
    pub fn manifest(&self) -> &Manifest {
        self.project.manifest(&self.db)
    }

    /// The lockfile; empty when the project has none.
    pub fn lockfile(&self) -> &Lockfile {
        self.project.lockfile(&self.db)
    }

    /// Replaces the manifest with the text of `folio.toml`. Fails with every
    /// error in it and keeps the old manifest. A manifest equal to the
    /// current one changes nothing.
    ///
    /// The errors point into the new text by byte range, which the world
    /// does not keep: resolve them against a
    /// [`TextFiles`](crate::diag::TextFiles) that holds it under
    /// [`World::MANIFEST`], not against the world.
    pub fn set_manifest(&mut self, text: &str) -> Result<(), Vec<ProjectError>> {
        let manifest = Manifest::parse(text, Self::MANIFEST)?;
        if self.manifest_text.text() != text {
            self.manifest_text = Lines::new(text.to_owned());
        }
        if &manifest != self.manifest() {
            self.project
                .set_manifest(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(manifest);
            if let Some(kept) = &self.kept {
                kept.forget(&self.builds());
            }
        }
        Ok(())
    }

    /// Replaces the lockfile with the text of `folio.lock`, or removes it.
    /// Fails with every error in it and keeps the old lockfile.
    ///
    /// The errors point into the new text by byte range, which the world
    /// does not keep: resolve them against a
    /// [`TextFiles`](crate::diag::TextFiles) that holds it under
    /// [`World::LOCKFILE`], not against the world.
    pub fn set_lockfile(&mut self, text: Option<&str>) -> Result<(), Vec<ProjectError>> {
        let lockfile = parse_lockfile(text)?;
        self.lockfile_text = text.map(|text| Lines::new(text.to_owned()));
        if &lockfile != self.lockfile() {
            self.project
                .set_lockfile(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(lockfile);
        }
        Ok(())
    }

    /// Sets the date scripts see as `today`, as `folio build --today` does,
    /// or removes it, so that the manifest's `project.today` applies. With
    /// neither, reading `today` is an error: there is no clock (SPEC §4.4).
    pub fn set_today(&mut self, today: Option<Date>) {
        if *self.project.today(&self.db) != today {
            self.project
                .set_today(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(today);
        }
    }

    /// Replaces the font files besides the bundled ones: those of the
    /// project's font directory, and those the font cache has for
    /// `folio.lock`. [`Self::load_fonts`] finds both.
    pub fn set_fonts(&mut self, fonts: impl IntoIterator<Item = FontFile>) {
        let fonts: EcoVec<FontFile> = fonts.into_iter().collect();
        if &fonts != self.project.fonts(&self.db) {
            self.project
                .set_fonts(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(fonts);
        }
    }

    /// The font files besides the bundled ones, as [`Self::set_fonts`] set
    /// them.
    pub fn font_files(&self) -> &EcoVec<FontFile> {
        self.project.fonts(&self.db)
    }

    /// Reads the fonts of the project's font directory (`fonts/` next to
    /// `folio.toml`, which is in `root`) and takes the fonts that
    /// `folio.lock` locks from the font cache, if one is given, and sets
    /// them as [`Self::set_fonts`] does. A locked font that neither has is
    /// reported when compiling.
    pub fn load_fonts(&mut self, root: &Path, cache: Option<&FontCache>) -> io::Result<()> {
        let mut fonts = folio_fonts::project_fonts(root)?;
        if let Some(cache) = cache {
            for locked in &self.lockfile().fonts {
                let hash = FontHash(locked.hash.v.0);
                if fonts.iter().any(|f| f.hash() == hash) {
                    continue;
                }
                if let Some(file) = cache.get(hash)? {
                    fonts.push(file);
                }
            }
        }
        self.set_fonts(fonts);
        Ok(())
    }

    /// Replaces the faces installed on the machine, as
    /// [`folio_fonts::system_fonts`] finds them. Their files are read only
    /// when a document uses them, and each at most once until the system
    /// fonts change, so a file that changes on disk meanwhile is not seen.
    pub fn set_system_fonts(&mut self, fonts: impl IntoIterator<Item = SystemFont>) {
        let fonts: EcoVec<SystemFont> = fonts.into_iter().collect();
        if &fonts != self.project.system_fonts(&self.db) {
            self.project
                .set_system_fonts(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(fonts);
            // No other handle is left, and every query that read a face
            // re-runs.
            self.db.system_faces = Arc::default();
        }
    }

    /// Sets how the world builds (SPEC §2.4); see [`WorldBuilder::mode`].
    pub fn set_mode(&mut self, mode: BuildMode) {
        if *self.project.mode(&self.db) != mode {
            self.project
                .set_mode(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(mode);
        }
    }

    /// Sets the text of a `.fol` or `.fsty` file, adding the file if it is
    /// new. A file that exists is reparsed only where its text changed.
    pub fn set_source(&mut self, path: &str, text: &str) -> Result<FileId, WorldError> {
        let path = project_path(path)?;
        let stylesheet = match path.extension() {
            Some("fol") => false,
            Some("fsty") => true,
            _ => return Err(WorldError::NotSource(path)),
        };
        if let Some(file) = self.project.sources(&self.db).get(&path).copied() {
            let old = file.source(&self.db).clone();
            if old.text() != text {
                let mut source = old.clone();
                source.replace(text);
                set_source(&mut self.db, file, source.clone());
                Arc::make_mut(&mut self.nodes).update(old.root(), source.root());
            }
            return Ok(old.id());
        }

        let id = self.assign_id(&path)?;
        let text = text.to_owned();
        let source = if stylesheet {
            Source::stylesheet(id, text)
        } else {
            Source::new(id, text)
        };
        Arc::make_mut(&mut self.nodes).insert(source.root());
        let file = SourceFile::builder(path.clone(), source)
            .durability(Durability::LOW)
            .new(&self.db);
        self.edit_sources(|sources| sources.insert(path, file));
        Ok(id)
    }

    /// Replaces a byte range of a source's text, reparsing only the smallest
    /// enclosing block. Returns the range of the new text that was reparsed.
    pub fn edit_source(
        &mut self,
        path: &str,
        range: Range<usize>,
        with: &str,
    ) -> Result<Range<usize>, WorldError> {
        let path = project_path(path)?;
        let file = self
            .project
            .sources(&self.db)
            .get(&path)
            .copied()
            .ok_or(WorldError::NotFound(path))?;
        let mut source = file.source(&self.db).clone();
        let text = source.text();
        if range.start > range.end
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            return Err(WorldError::InvalidRange(range));
        }
        if &text[range.clone()] == with {
            return Ok(range);
        }
        let old = source.clone();
        let reparsed = source.edit(range, with);
        set_source(&mut self.db, file, source.clone());
        Arc::make_mut(&mut self.nodes).update(old.root(), source.root());
        Ok(reparsed)
    }

    /// Removes a source and frees its file id, which a source added much
    /// later may take. Returns whether there was one.
    pub fn remove_source(&mut self, path: &str) -> bool {
        let Some(path) = ProjectPath::new(path) else {
            return false;
        };
        if !self.project.sources(&self.db).contains_key(&path) {
            return false;
        }
        let file = self
            .edit_sources(|sources| sources.remove(&path))
            .expect("the source is there");
        let id = file.source(&self.db).id();
        Arc::make_mut(&mut self.nodes).remove(id);
        Arc::make_mut(&mut self.ids).release(&path);
        true
    }

    /// The paths of the world's sources, in path order.
    pub fn source_paths(&self) -> impl Iterator<Item = &ProjectPath> {
        self.project.sources(&self.db).keys()
    }

    /// The paths of the world's image and data files, in path order.
    pub fn file_paths(&self) -> impl Iterator<Item = &ProjectPath> {
        self.project.files(&self.db).keys()
    }

    /// The paths of the world's sources and image and data files that are
    /// `dir` or under it, every one when it is empty, without walking the
    /// others.
    pub(crate) fn paths_under(&self, dir: &str) -> Vec<String> {
        fn under<V>(map: &BTreeMap<ProjectPath, V>, dir: &str, out: &mut Vec<String>) {
            let paths: Box<dyn Iterator<Item = &ProjectPath>> = match ProjectPath::new(dir) {
                // The paths that start with `dir` follow it in path order.
                Some(start) => Box::new(
                    map.range(start..)
                        .map(|(path, _)| path)
                        .take_while(|path| path.as_str().starts_with(dir)),
                ),
                None => Box::new(map.keys()),
            };
            out.extend(
                paths
                    .map(ProjectPath::as_str)
                    .filter(|path| {
                        dir.is_empty()
                            || path
                                .strip_prefix(dir)
                                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
                    })
                    .map(str::to_owned),
            );
        }
        let mut out = Vec::new();
        under(self.project.sources(&self.db), dir, &mut out);
        under(self.project.files(&self.db), dir, &mut out);
        out
    }

    /// The contents of an image or data file, if the world has one at the
    /// path and it can be read. A file on disk is read now if no build has
    /// read it yet ([`Self::set_file_on_disk`]).
    pub fn file(&self, path: &str) -> Option<Bytes> {
        self.file_content(path)?.load().cloned()
    }

    /// The contents the world has for an image or data file, read or not.
    pub(crate) fn file_content(&self, path: &str) -> Option<&FileContent> {
        let path = ProjectPath::new(path)?;
        let file = self.project.files(&self.db).get(&path)?;
        Some(file.content(&self.db))
    }

    /// The files that diagnostics point into as they are now: the sources,
    /// `folio.toml` and `folio.lock`. A snapshot is cheap to make, holds no
    /// handle on the database, so it does not hold up edits, and stays valid
    /// after them: a language server resolves a compilation's diagnostics
    /// against the snapshot taken with it while the next edit goes ahead.
    pub fn snapshot(&self) -> Snapshot {
        let sources = self
            .project
            .sources(&self.db)
            .values()
            .map(|file| {
                let source = file.source(&self.db);
                (source.id(), source.clone())
            })
            .collect();
        let data = self
            .project
            .files(&self.db)
            .values()
            .filter_map(|file| Some((file.id(&self.db)?, file.content(&self.db).clone())))
            .collect();
        Snapshot {
            ids: self.ids.clone(),
            sources: Arc::new(sources),
            data: Arc::new(data),
            manifest_text: self.manifest_text.clone(),
            lockfile_text: self.lockfile_text.clone(),
        }
    }

    /// A source, if the world has one at the path.
    pub fn source(&self, path: &str) -> Option<Source> {
        let path = ProjectPath::new(path)?;
        let file = self.project.sources(&self.db).get(&path)?;
        Some(file.source(&self.db).clone())
    }

    /// Sets the contents of an image or data file, adding it if it is new.
    /// Sources (`.fol` and `.fsty`) go through [`Self::set_source`].
    pub fn set_file(&mut self, path: &str, bytes: impl Into<Bytes>) -> Result<(), WorldError> {
        let path = file_path(path)?;
        let bytes = bytes.into();
        if let Some(file) = self.project.files(&self.db).get(&path).copied()
            && file.content(&self.db).loaded() == Some(Ok(&bytes))
        {
            return Ok(());
        }
        self.set_contents([(path, FileContent::new(bytes))]);
        Ok(())
    }

    /// Adds an image or data file whose contents are those of `disk`, a
    /// file on disk, or points the world's file at it again. The file is
    /// read only when a build names it, and then once, so the world sees a
    /// later change on disk only when this is called again (SPEC §12.1).
    /// A file that cannot be read is reported by the builds that name it.
    pub fn set_file_on_disk(
        &mut self,
        path: &str,
        disk: impl Into<PathBuf>,
    ) -> Result<(), WorldError> {
        let path = file_path(path)?;
        self.set_contents([(path, FileContent::on_disk(disk))]);
        Ok(())
    }

    /// Sets the contents of image and data files: the files the world has
    /// one by one, and the new ones all at once, so that adding n files
    /// takes time linear in n.
    pub(crate) fn set_contents(
        &mut self,
        contents: impl IntoIterator<Item = (ProjectPath, FileContent)>,
    ) {
        let mut new = Vec::new();
        for (path, content) in contents {
            match self.project.files(&self.db).get(&path).copied() {
                Some(file) => {
                    file.set_content(&mut self.db)
                        .with_durability(Durability::LOW)
                        .to(content);
                }
                None => new.push((path, content)),
            }
        }
        if new.is_empty() {
            return;
        }
        let new: Vec<_> = new
            .into_iter()
            .map(|(path, content)| {
                let id = self.data_id(&path);
                let file = DataFile::builder(path.clone(), id, content)
                    .durability(Durability::LOW)
                    .new(&self.db);
                (path, file)
            })
            .collect();
        self.edit_files(|files| files.extend(new));
    }

    /// Removes an image or data file. Returns whether there was one.
    pub fn remove_file(&mut self, path: &str) -> bool {
        let Some(path) = ProjectPath::new(path) else {
            return false;
        };
        if !self.project.files(&self.db).contains_key(&path) {
            return false;
        }
        let file = self
            .edit_files(|files| files.remove(&path))
            .expect("the file is there");
        if file.id(&self.db).is_some() {
            Arc::make_mut(&mut self.ids).release(&path);
        }
        true
    }

    /// The path of a file id: a source, a text data file, `folio.toml` or
    /// `folio.lock`.
    pub fn path(&self, id: FileId) -> Option<&str> {
        match id {
            Self::MANIFEST => Some("folio.toml"),
            Self::LOCKFILE => Some("folio.lock"),
            _ => self.ids.path(id).map(ProjectPath::as_str),
        }
    }

    /// The file id of a path, if the world has a source there, or a text
    /// data file with an id.
    pub fn id(&self, path: &str) -> Option<FileId> {
        self.ids.by_path.get(&ProjectPath::new(path)?).copied()
    }

    /// The identity of the node of a source that has a span in the current
    /// revision: an id that the node keeps across edits and reparses for
    /// as long as it lives (SPEC §12.4). A span from a fragment is
    /// [`span_map::span`](crate::span_map::span) of its reference.
    ///
    /// Node ids depend on the order of edits. They serve selections, the
    /// inspector and runtime state, never undo or anything saved.
    pub fn node_id(&self, span: Span) -> Option<NodeId> {
        self.nodes.id(span)
    }

    /// The span of a node in the current revision of its source, if it
    /// still lives.
    pub fn node_span(&self, id: NodeId) -> Option<Span> {
        self.nodes.span(id)
    }

    /// The variants and targets the manifest declares, in declaration order:
    /// every target of every variant. A project without variants is built
    /// once, for PDF.
    pub fn builds(&self) -> Vec<(Option<Name>, Target)> {
        let variants = &self.manifest().variants;
        if variants.is_empty() {
            return vec![(None, Target::Pdf)];
        }
        variants
            .iter()
            .flat_map(|v| v.targets.iter().map(|t| (Some(v.name.v.clone()), t.v)))
            .collect()
    }

    /// Whether the world serves a long-lived process
    /// ([`WorldBuilder::long_lived`]).
    pub fn is_long_lived(&self) -> bool {
        self.kept.is_some()
    }

    /// In a long-lived world, what it keeps of each build.
    pub(crate) fn kept(&self) -> Option<&Kept> {
        self.kept.as_deref()
    }

    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    pub(crate) fn project(&self) -> Project {
        self.project
    }

    /// Changes the map of sources in place: it is taken out of the
    /// database and put back, rather than cloned, so that adding or removing
    /// a source takes time logarithmic in the number of sources, not linear.
    fn edit_sources<R>(
        &mut self,
        f: impl FnOnce(&mut BTreeMap<ProjectPath, SourceFile>) -> R,
    ) -> R {
        let mut sources = self.project.set_sources(&mut self.db).to(BTreeMap::new());
        let result = f(&mut sources);
        self.project
            .set_sources(&mut self.db)
            .with_durability(Durability::LOW)
            .to(sources);
        result
    }

    /// Changes the map of image and data files in place, as
    /// [`Self::edit_sources`] does the sources.
    fn edit_files<R>(&mut self, f: impl FnOnce(&mut BTreeMap<ProjectPath, DataFile>) -> R) -> R {
        let mut files = self.project.set_files(&mut self.db).to(BTreeMap::new());
        let result = f(&mut files);
        self.project
            .set_files(&mut self.db)
            .with_durability(Durability::LOW)
            .to(files);
        result
    }

    /// A file id for a new data file, if it is a text data file and the
    /// world has ids to spare: data files take at most half of them, so
    /// that sources always get one.
    fn data_id(&mut self, path: &ProjectPath) -> Option<FileId> {
        let text = path.extension().is_some_and(|ext| {
            ["csv", "tsv", "json", "toml", "yaml", "yml", "txt", "bib"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        });
        if !text || self.ids.by_path.len() >= MAX_DATA_IDS {
            return None;
        }
        Arc::make_mut(&mut self.ids).assign(path).ok()
    }

    /// The text of a data file with an id, as a build read it.
    fn data_lines(&self, id: FileId) -> Option<Lines<String>> {
        let file = self.project.files(&self.db).get(self.ids.path(id)?)?;
        content_lines(file.content(&self.db))
    }

    fn assign_id(&mut self, path: &ProjectPath) -> Result<FileId, WorldError> {
        if let Some(&id) = self.ids.by_path.get(path) {
            return Ok(id);
        }
        Arc::make_mut(&mut self.ids).assign(path)
    }
}

/// The sources the world has now, `folio.toml` and `folio.lock`. Resolve
/// the diagnostics of a compilation before editing the world: an edit can
/// renumber the spans of the text it changes.
impl Files for World {
    fn path(&self, id: FileId) -> Option<EcoString> {
        World::path(self, id).map(EcoString::from)
    }

    fn source(&self, id: FileId) -> Option<Source> {
        World::source(self, World::path(self, id)?)
    }

    fn lines(&self, id: FileId) -> Option<Lines<String>> {
        match id {
            Self::MANIFEST => Some(self.manifest_text.clone()),
            Self::LOCKFILE => self.lockfile_text.clone(),
            _ => Files::source(self, id)
                .map(|source| source.lines().clone())
                .or_else(|| self.data_lines(id)),
        }
    }
}

impl Debug for World {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("World")
            .field("manifest", self.manifest())
            .field("sources", &self.ids.by_path)
            .finish_non_exhaustive()
    }
}

/// The most file ids that may be taken when a data file is given one.
const MAX_DATA_IDS: usize = 32_768;

/// The text of a data file's contents as a build read them, if it read them
/// and they are UTF-8.
fn content_lines(content: &FileContent) -> Option<Lines<String>> {
    let bytes = content.loaded()?.ok()?;
    Some(Lines::new(String::from_utf8(bytes.to_vec()).ok()?))
}

/// The raw id of the first source; 1 and 2 are the manifest and lockfile.
const FIRST_SOURCE_ID: usize = 3;

/// The file ids of the world's sources.
#[derive(Debug, Clone, Default)]
struct FileIds {
    by_path: BTreeMap<ProjectPath, FileId>,
    /// The path of each id, from [`FIRST_SOURCE_ID`] on; `None` for an id
    /// that was freed.
    paths: Vec<Option<ProjectPath>>,
    /// The freed ids, by index into `paths`, which are given out again,
    /// lowest first, once every id has been given out once.
    free: BTreeSet<usize>,
}

impl FileIds {
    /// The path of an id.
    fn path(&self, id: FileId) -> Option<&ProjectPath> {
        let index = usize::from(id.into_raw().get()).checked_sub(FIRST_SOURCE_ID)?;
        self.paths.get(index)?.as_ref()
    }

    /// Gives a path an id: a new one while there are any, so that an id
    /// is not soon given to another path, and after that the lowest freed
    /// one.
    fn assign(&mut self, path: &ProjectPath) -> Result<FileId, WorldError> {
        let raw = |index: usize| {
            u16::try_from(FIRST_SOURCE_ID + index)
                .ok()
                .and_then(NonZeroU16::new)
                .filter(|&raw| FileId::from_raw(raw) != FileId::DETACHED)
        };
        let (index, raw) = match raw(self.paths.len()) {
            Some(raw) => (self.paths.len(), raw),
            None => {
                let index = *self.free.first().ok_or(WorldError::TooManyFiles)?;
                (index, raw(index).expect("a freed id was given out"))
            }
        };
        let id = FileId::from_raw(raw);
        if index == self.paths.len() {
            self.paths.push(Some(path.clone()));
        } else {
            self.free.remove(&index);
            self.paths[index] = Some(path.clone());
        }
        self.by_path.insert(path.clone(), id);
        Ok(id)
    }

    /// Frees the id of a path.
    fn release(&mut self, path: &ProjectPath) {
        if let Some(id) = self.by_path.remove(path)
            && let Some(index) = usize::from(id.into_raw().get()).checked_sub(FIRST_SOURCE_ID)
            && let Some(slot) = self.paths.get_mut(index)
        {
            *slot = None;
            self.free.insert(index);
        }
    }
}

/// The files that diagnostics point into, as they were when
/// [`World::snapshot`] took them: the sources, `folio.toml` and
/// `folio.lock`. It holds no handle on the database, so a host can keep it
/// while the world is edited.
#[derive(Clone)]
pub struct Snapshot {
    ids: Arc<FileIds>,
    sources: Arc<BTreeMap<FileId, Source>>,
    /// The data files with ids, which diagnostics can point into.
    data: Arc<BTreeMap<FileId, FileContent>>,
    manifest_text: Lines<String>,
    lockfile_text: Option<Lines<String>>,
}

impl Snapshot {
    /// The path of a file id: a source, a text data file, `folio.toml` or
    /// `folio.lock`.
    pub fn path(&self, id: FileId) -> Option<&str> {
        match id {
            World::MANIFEST => Some("folio.toml"),
            World::LOCKFILE => Some("folio.lock"),
            _ => self.ids.path(id).map(ProjectPath::as_str),
        }
    }

    /// The source with a file id.
    pub fn source(&self, id: FileId) -> Option<&Source> {
        self.sources.get(&id)
    }
}

impl Files for Snapshot {
    fn path(&self, id: FileId) -> Option<EcoString> {
        Snapshot::path(self, id).map(EcoString::from)
    }

    fn source(&self, id: FileId) -> Option<Source> {
        Snapshot::source(self, id).cloned()
    }

    fn lines(&self, id: FileId) -> Option<Lines<String>> {
        match id {
            World::MANIFEST => Some(self.manifest_text.clone()),
            World::LOCKFILE => self.lockfile_text.clone(),
            _ => Files::source(self, id)
                .map(|source| source.lines().clone())
                .or_else(|| content_lines(self.data.get(&id)?)),
        }
    }
}

impl Debug for Snapshot {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("sources", &self.ids.by_path)
            .finish_non_exhaustive()
    }
}

/// Why a world could not take a file or an edit.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum WorldError {
    /// The path is not a path inside the project.
    InvalidPath(EcoString),
    /// A source must be a `.fol` or `.fsty` file.
    NotSource(ProjectPath),
    /// There is no source at the path.
    NotFound(ProjectPath),
    /// A source was given as a data file.
    IsSource(ProjectPath),
    /// The range is reversed, outside the text, or splits a character.
    InvalidRange(Range<usize>),
    /// Every file id has been given out.
    TooManyFiles,
}

impl Display for WorldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "`{path}` is not a path in the project"),
            Self::NotSource(path) => {
                write!(
                    f,
                    "`{path}` is not a source: sources are `.fol` and `.fsty` files"
                )
            }
            Self::NotFound(path) => write!(f, "there is no source `{path}`"),
            Self::IsSource(path) => {
                write!(f, "`{path}` is a source: set it with `set_source`")
            }
            Self::InvalidRange(range) => {
                write!(
                    f,
                    "{}..{} is not a range of the text",
                    range.start, range.end
                )
            }
            Self::TooManyFiles => f.write_str("too many files"),
        }
    }
}

impl std::error::Error for WorldError {}

fn project_path(path: &str) -> Result<ProjectPath, WorldError> {
    ProjectPath::new(path).ok_or_else(|| WorldError::InvalidPath(path.into()))
}

/// The path of an image or data file: a project path that is not a
/// source's.
fn file_path(path: &str) -> Result<ProjectPath, WorldError> {
    let path = project_path(path)?;
    if matches!(path.extension(), Some("fol" | "fsty")) {
        return Err(WorldError::IsSource(path));
    }
    Ok(path)
}

fn parse_lockfile(text: Option<&str>) -> Result<Lockfile, Vec<ProjectError>> {
    text.map_or(Ok(Lockfile::default()), |text| {
        Lockfile::parse(text, World::LOCKFILE)
    })
}

fn set_source(db: &mut Database, file: SourceFile, source: Source) {
    file.set_source(db)
        .with_durability(Durability::LOW)
        .to(source);
}

/// The query database.
#[salsa::db]
#[derive(Clone)]
pub(crate) struct Database {
    storage: salsa::Storage<Self>,
    /// Shared by every handle of the world.
    text: Arc<folio_core::text::Cache>,
    /// Shared by every handle of the world.
    layout: Arc<folio_core::layout::Cache>,
    /// Shared by every handle of the world.
    pages: Arc<folio_core::layout::PageCache>,
    /// Shared by every handle of the world, and replaced when the system
    /// fonts are.
    system_faces: Arc<folio_core::fonts::SystemFaces>,
}

impl Database {
    fn new(hook: Option<Hook>) -> Self {
        let callback = hook.map(|hook| {
            Box::new(move |event: salsa::Event| {
                let event = match event.kind {
                    salsa::EventKind::WillExecute { database_key } => {
                        // The key prints as `query(Id(..))` while a database
                        // is attached, which it is during a query.
                        let key = format!("{database_key:?}");
                        let query = key.split_once('(').map_or(&*key, |(name, _)| name);
                        Event::Execute {
                            query: query.into(),
                        }
                    }
                    salsa::EventKind::DidSetCancellationFlag => Event::Cancel,
                    _ => return,
                };
                hook(&event);
            }) as Box<dyn Fn(salsa::Event) + Send + Sync>
        });
        Self {
            storage: salsa::Storage::new(callback),
            text: Arc::default(),
            layout: Arc::default(),
            pages: Arc::default(),
            system_faces: Arc::default(),
        }
    }
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl folio_core::db::Db for Database {
    fn text_cache(&self) -> &folio_core::text::Cache {
        &self.text
    }

    fn layout_cache(&self) -> &folio_core::layout::Cache {
        &self.layout
    }

    fn page_cache(&self) -> &folio_core::layout::PageCache {
        &self.pages
    }

    fn system_faces(&self) -> &folio_core::fonts::SystemFaces {
        &self.system_faces
    }
}
