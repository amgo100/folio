//! The world: everything a compilation reads, held in the query database.

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::num::NonZeroU16;
use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, EcoVec};
use folio_component::{Name, Registry, Target};
use folio_core::db::{Bytes, DataFile, Durability, FontFile, Project, SourceFile};
use folio_core::project::{Lockfile, Manifest, ProjectError, ProjectPath};
use folio_syntax::{FileId, Source};
use salsa::Setter;

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
/// File ids are assigned when a path is first set and never reused, so
/// diagnostics stay valid for the life of the world; [`World::MANIFEST`] and
/// [`World::LOCKFILE`] are reserved.
#[derive(Clone)]
pub struct World {
    db: Database,
    project: Project,
    ids: Arc<FileIds>,
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
/// compiler, and an event hook.
#[derive(Default)]
pub struct WorldBuilder {
    components: Registry,
    bundled_fonts: EcoVec<FontFile>,
    hook: Option<Hook>,
}

impl WorldBuilder {
    /// The components the compiler hosts. Marked as rarely changing.
    pub fn components(mut self, registry: Registry) -> Self {
        self.components = registry;
        self
    }

    /// The fonts bundled with the compiler. Marked as rarely changing.
    pub fn bundled_fonts(mut self, fonts: impl IntoIterator<Item = FontFile>) -> Self {
        self.bundled_fonts = fonts.into_iter().collect();
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
    pub fn build(self, manifest: &str, lockfile: Option<&str>) -> Result<World, Vec<ProjectError>> {
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
            self.components,
        )
        .manifest_durability(Durability::MEDIUM)
        .lockfile_durability(Durability::MEDIUM)
        .sources_durability(Durability::LOW)
        .files_durability(Durability::LOW)
        .bundled_fonts_durability(Durability::HIGH)
        .fonts_durability(Durability::MEDIUM)
        .components_durability(Durability::HIGH)
        .new(&db);
        Ok(World {
            db,
            project,
            ids: Arc::default(),
        })
    }
}

impl Debug for WorldBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldBuilder")
            .field("components", &self.components)
            .field("bundled_fonts", &self.bundled_fonts)
            .field("hook", &self.hook.is_some())
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

    /// A world with no components, no bundled fonts and no lockfile, from
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
    pub fn set_manifest(&mut self, text: &str) -> Result<(), Vec<ProjectError>> {
        let manifest = Manifest::parse(text, Self::MANIFEST)?;
        if &manifest != self.manifest() {
            self.project
                .set_manifest(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(manifest);
        }
        Ok(())
    }

    /// Replaces the lockfile with the text of `folio.lock`, or removes it.
    /// Fails with every error in it and keeps the old lockfile.
    pub fn set_lockfile(&mut self, text: Option<&str>) -> Result<(), Vec<ProjectError>> {
        let lockfile = parse_lockfile(text)?;
        if &lockfile != self.lockfile() {
            self.project
                .set_lockfile(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(lockfile);
        }
        Ok(())
    }

    /// Replaces the project, locked and system fonts.
    pub fn set_fonts(&mut self, fonts: impl IntoIterator<Item = FontFile>) {
        let fonts: EcoVec<FontFile> = fonts.into_iter().collect();
        if &fonts != self.project.fonts(&self.db) {
            self.project
                .set_fonts(&mut self.db)
                .with_durability(Durability::MEDIUM)
                .to(fonts);
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
            let mut source = file.source(&self.db).clone();
            if source.text() != text {
                source.replace(text);
                set_source(&mut self.db, file, source);
            }
            return Ok(file.source(&self.db).id());
        }

        let id = self.assign_id(&path)?;
        let text = text.to_owned();
        let source = if stylesheet {
            Source::stylesheet(id, text)
        } else {
            Source::new(id, text)
        };
        let file = SourceFile::builder(path.clone(), source)
            .durability(Durability::LOW)
            .new(&self.db);
        let mut sources = self.project.sources(&self.db).clone();
        sources.insert(path, file);
        self.project
            .set_sources(&mut self.db)
            .with_durability(Durability::LOW)
            .to(sources);
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
        let reparsed = source.edit(range, with);
        set_source(&mut self.db, file, source);
        Ok(reparsed)
    }

    /// Removes a source. Its file id stays reserved for its path. Returns
    /// whether there was one.
    pub fn remove_source(&mut self, path: &str) -> bool {
        let Some(path) = ProjectPath::new(path) else {
            return false;
        };
        let mut sources = self.project.sources(&self.db).clone();
        if sources.remove(&path).is_none() {
            return false;
        }
        self.project
            .set_sources(&mut self.db)
            .with_durability(Durability::LOW)
            .to(sources);
        true
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
        let path = project_path(path)?;
        if matches!(path.extension(), Some("fol" | "fsty")) {
            return Err(WorldError::IsSource(path));
        }
        let bytes = bytes.into();
        if let Some(file) = self.project.files(&self.db).get(&path).copied() {
            if file.bytes(&self.db) != &bytes {
                file.set_bytes(&mut self.db)
                    .with_durability(Durability::LOW)
                    .to(bytes);
            }
            return Ok(());
        }
        let file = DataFile::builder(path.clone(), bytes)
            .durability(Durability::LOW)
            .new(&self.db);
        let mut files = self.project.files(&self.db).clone();
        files.insert(path, file);
        self.project
            .set_files(&mut self.db)
            .with_durability(Durability::LOW)
            .to(files);
        Ok(())
    }

    /// Removes an image or data file. Returns whether there was one.
    pub fn remove_file(&mut self, path: &str) -> bool {
        let Some(path) = ProjectPath::new(path) else {
            return false;
        };
        let mut files = self.project.files(&self.db).clone();
        if files.remove(&path).is_none() {
            return false;
        }
        self.project
            .set_files(&mut self.db)
            .with_durability(Durability::LOW)
            .to(files);
        true
    }

    /// The path of a file id: a source, `folio.toml` or `folio.lock`.
    pub fn path(&self, id: FileId) -> Option<&str> {
        match id {
            Self::MANIFEST => Some("folio.toml"),
            Self::LOCKFILE => Some("folio.lock"),
            _ => {
                let index = usize::from(id.into_raw().get()).checked_sub(FIRST_SOURCE_ID)?;
                self.ids.paths.get(index).map(ProjectPath::as_str)
            }
        }
    }

    /// The file id of a source path, if one was ever set.
    pub fn id(&self, path: &str) -> Option<FileId> {
        self.ids.by_path.get(&ProjectPath::new(path)?).copied()
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

    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    pub(crate) fn project(&self) -> Project {
        self.project
    }

    fn assign_id(&mut self, path: &ProjectPath) -> Result<FileId, WorldError> {
        if let Some(&id) = self.ids.by_path.get(path) {
            return Ok(id);
        }
        let raw = u16::try_from(FIRST_SOURCE_ID + self.ids.paths.len())
            .ok()
            .and_then(NonZeroU16::new)
            .filter(|&raw| FileId::from_raw(raw) != FileId::DETACHED)
            .ok_or(WorldError::TooManyFiles)?;
        let id = FileId::from_raw(raw);
        let ids = Arc::make_mut(&mut self.ids);
        ids.by_path.insert(path.clone(), id);
        ids.paths.push(path.clone());
        Ok(id)
    }
}

impl Debug for World {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("World")
            .field("manifest", self.manifest())
            .field("sources", &self.ids.paths)
            .finish_non_exhaustive()
    }
}

/// The raw id of the first source; 1 and 2 are the manifest and lockfile.
const FIRST_SOURCE_ID: usize = 3;

/// The file ids given out so far.
#[derive(Debug, Clone, Default)]
struct FileIds {
    by_path: BTreeMap<ProjectPath, FileId>,
    /// The path of each id, from [`FIRST_SOURCE_ID`] on.
    paths: Vec<ProjectPath>,
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
        }
    }
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl folio_core::db::Db for Database {}
