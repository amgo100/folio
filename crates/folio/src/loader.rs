//! Loading a project from disk into a [`World`], and keeping the world in
//! step with the disk (SPEC §12.1, §12.2).
//!
//! A [`Loader`] reads a project directory: `folio.toml` and `folio.lock`
//! into a world ([`Loader::world`]), and every other file of the project as
//! a source or a data file ([`Loader::load`]). Sources (`.fol` and `.fsty`)
//! are read at once; any other file is read the first time a build names
//! it, as an image, a data file or a bibliography, so a large file or a
//! folder of files that no build names costs only its directory entry
//! (SPEC §12.1). A host that watches the
//! project, such as `folio watch` or the language server, hands the paths
//! that file-system events name to [`Loader::update`], which reads them
//! again and changes the world to match; it does not watch itself. A file
//! that no build has read yet is not read on an event either, since no
//! build depends on what it holds.
//!
//! The files of a project are every file under its directory except
//! `folio.toml` and `folio.lock`, hidden files and directories (whose names
//! start with `.`), the font directory (read by [`World::load_fonts`]) and
//! the files its builds write. A symbolic link is followed only to a file or
//! directory inside the project, and not to a directory it is in; an output
//! is left out by the path it is written to, so a link that leads to it
//! does not make it an input.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use folio_core::db::{FileContent, Unreadable};
use folio_fonts::{FontCache, PROJECT_FONT_DIR};
use folio_syntax::DiagSpan;

use crate::diag::TextFiles;
use crate::{Diagnostic, ProjectError, ProjectPath, World, WorldBuilder, WorldError};

/// The manifest's file name.
pub const MANIFEST: &str = "folio.toml";
/// The lockfile's file name.
pub const LOCKFILE: &str = "folio.lock";

/// The project directory: `root` if given, which must hold `folio.toml`,
/// or else the current directory or the nearest directory above it that
/// holds one.
pub fn find_root(root: Option<&Path>) -> Result<PathBuf, LoadError> {
    if let Some(root) = root {
        return if root.join(MANIFEST).is_file() {
            Ok(root.to_path_buf())
        } else {
            Err(LoadError::NoManifest {
                dir: root.to_path_buf(),
                above: false,
            })
        };
    }
    let cwd = std::env::current_dir().map_err(|error| LoadError::Io {
        what: "the current directory".into(),
        error,
    })?;
    cwd.ancestors()
        .find(|dir| dir.join(MANIFEST).is_file())
        .map(Path::to_path_buf)
        .ok_or(LoadError::NoManifest {
            dir: cwd,
            above: true,
        })
}

/// Why a project could not be loaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    /// The directory, or with `above` any directory above it, holds no
    /// `folio.toml`.
    NoManifest {
        /// The directory.
        dir: PathBuf,
        /// Whether the directories above it were searched too.
        above: bool,
    },
    /// A file or directory could not be read.
    Io {
        /// What could not be read, as a message names it.
        what: String,
        /// Why.
        error: io::Error,
    },
    /// `folio.toml` or `folio.lock` has errors.
    Project(ProjectErrors),
    /// The world could not take a file.
    World(WorldError),
}

impl Display for LoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoManifest { dir, above: false } => {
                write!(f, "`{}` holds no {MANIFEST}", dir.display())
            }
            Self::NoManifest { dir, above: true } => write!(
                f,
                "no {MANIFEST} in `{}` or any directory above it",
                dir.display()
            ),
            Self::Io { what, error } => write!(f, "cannot read {what}: {error}"),
            Self::Project(errors) => {
                let n = errors.diagnostics.len();
                write!(
                    f,
                    "{n} error{} in the project files",
                    if n == 1 { "" } else { "s" }
                )
            }
            Self::World(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<WorldError> for LoadError {
    fn from(error: WorldError) -> Self {
        Self::World(error)
    }
}

/// The errors of `folio.toml` or `folio.lock`, with the texts they point
/// into: the world keeps the last valid manifest and lockfile, not the
/// texts that failed, so resolve them against `files`.
#[derive(Debug, Clone)]
pub struct ProjectErrors {
    /// The texts of `folio.toml` and `folio.lock`, under [`World::MANIFEST`]
    /// and [`World::LOCKFILE`].
    pub files: TextFiles,
    /// The errors.
    pub diagnostics: Vec<Diagnostic>,
}

impl ProjectErrors {
    fn new(manifest: Option<&str>, lockfile: Option<&str>, errors: Vec<ProjectError>) -> Self {
        let mut files = TextFiles::new();
        if let Some(text) = manifest {
            files.insert(World::MANIFEST, MANIFEST, text);
        }
        if let Some(text) = lockfile {
            files.insert(World::LOCKFILE, LOCKFILE, text);
        }
        Self {
            files,
            diagnostics: errors.into_iter().map(Diagnostic::from).collect(),
        }
    }
}

/// A file of the project, by its path relative to the project directory.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProjectFile {
    /// The path, with `/` as separator.
    pub path: String,
    /// Where the file is on disk: the file a link leads to, for a link.
    pub disk: PathBuf,
    /// The contents of a source (`.fol` or `.fsty`), read with the
    /// project; `None` for any other file, which is read when a build
    /// names it, and for a source that was not taken.
    pub bytes: Option<Vec<u8>>,
    /// Why a source was not taken: it could not be read, or it is a link
    /// that leads out of the project, whose target `disk` then names. A
    /// world records it ([`World::unreadable_sources`]), and a build that
    /// names it says why. Whether its bytes are UTF-8 is checked when it is
    /// set in a world.
    pub refused: Option<Unreadable>,
}

/// What [`Loader::update`] changed.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Update {
    /// The paths whose contents the world now has differently: sources and
    /// data files set or removed, `folio.toml`, `folio.lock`, and the font
    /// directory, or the path in it that an event named, when the fonts
    /// changed. Empty when nothing changed, so there is nothing
    /// to rebuild.
    pub changed: BTreeSet<String>,
    /// The errors of a new `folio.toml` or `folio.lock`, which the world did
    /// not take.
    pub errors: Option<ProjectErrors>,
}

/// Reads a project directory into a [`World`] and keeps it in step with the
/// disk. See the [module documentation](self) for which files are read.
#[derive(Debug, Clone)]
pub struct Loader {
    root: PathBuf,
    canonical: PathBuf,
    /// The project paths of the files the builds write.
    outputs: BTreeSet<String>,
    /// The links found by the last reads, by project path, with the
    /// canonical paths they lead to.
    links: BTreeMap<String, PathBuf>,
    font_cache: Option<FontCache>,
}

impl Loader {
    /// A loader for the project in `root`, the directory that holds
    /// `folio.toml`. Fails when the directory cannot be read.
    pub fn new(root: &Path) -> Result<Self, LoadError> {
        let canonical = root.canonicalize().map_err(|error| LoadError::Io {
            what: "the project directory".into(),
            error,
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            canonical,
            outputs: BTreeSet::new(),
            links: BTreeMap::new(),
            font_cache: None,
        })
    }

    /// Takes the fonts that `folio.lock` locks from a font cache when it
    /// reads the project's fonts again.
    pub fn with_font_cache(mut self, cache: Option<FontCache>) -> Self {
        self.font_cache = cache;
        self
    }

    /// The project directory, as given.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads `folio.toml`.
    pub fn read_manifest(&self) -> Result<String, LoadError> {
        fs::read_to_string(self.root.join(MANIFEST)).map_err(|error| LoadError::Io {
            what: MANIFEST.into(),
            error,
        })
    }

    /// Reads `folio.lock`, if the project has one.
    pub fn read_lockfile(&self) -> Result<Option<String>, LoadError> {
        match fs::read_to_string(self.root.join(LOCKFILE)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LoadError::Io {
                what: LOCKFILE.into(),
                error,
            }),
        }
    }

    /// Builds a world from the project's `folio.toml` and `folio.lock`, and
    /// leaves out the files its builds write from what it reads from then
    /// on.
    pub fn world(&mut self, builder: WorldBuilder) -> Result<World, LoadError> {
        let manifest = self.read_manifest()?;
        let lockfile = self.read_lockfile()?;
        let world = builder
            .build(&manifest, lockfile.as_deref())
            .map_err(|errors| {
                LoadError::Project(ProjectErrors::new(
                    Some(&manifest),
                    lockfile.as_deref(),
                    errors,
                ))
            })?;
        self.set_outputs(&world);
        Ok(world)
    }

    /// Leaves out the files that the builds of the world's manifest write,
    /// each version of a shuffled paper among them
    /// ([`World::versioned_builds`]). [`Self::world`] and [`Self::update`]
    /// do this for their worlds' manifests.
    pub fn set_outputs(&mut self, world: &World) {
        self.outputs = world
            .versioned_builds()
            .iter()
            .filter_map(|build| {
                world.manifest().versioned_output_path(
                    build.variant.as_ref(),
                    build.version.as_ref(),
                    build.target,
                )
            })
            .filter_map(|path| ProjectPath::new(&path))
            .map(|path| path.as_str().to_owned())
            .collect();
    }

    /// Finds the files of the project, in path order, and reads its
    /// sources. A source that cannot be read, or is a link that leads out
    /// of the project, comes with why ([`ProjectFile::refused`]). Warns of
    /// directories that cannot be read; fails only when the project
    /// directory cannot.
    pub fn read_files(
        &mut self,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<Vec<ProjectFile>, LoadError> {
        let mut files = Vec::new();
        let mut links = BTreeMap::new();
        self.scan(
            "",
            vec![self.canonical.clone()],
            &mut files,
            &mut links,
            warnings,
        )
        .map_err(|error| LoadError::Io {
            what: "the project directory".into(),
            error,
        })?;
        self.links = links;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// Reads the files of the project into a world: `.fol` and `.fsty` files
    /// as sources, the others as data files, which are read when a build
    /// names them, and removes the sources and
    /// data files the project no longer has, including those a host set
    /// itself, such as unsaved editor buffers, which it sets again after.
    /// Records sources that cannot be read, are not UTF-8 or are links out
    /// of the project, so that a build that names one says why; leaves out
    /// files whose paths a document cannot name. Returns the paths it
    /// changed.
    pub fn load(
        &mut self,
        world: &mut World,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<BTreeSet<String>, LoadError> {
        let files = self.read_files(warnings)?;
        let mut changed = BTreeSet::new();
        let found: BTreeSet<String> = files.iter().map(|f| f.path.clone()).collect();
        for path in world.paths_under("") {
            if !found.contains(&path) && remove(world, &path) {
                changed.insert(path);
            }
        }
        set_all(world, files, &mut changed, &self.canonical)?;
        Ok(changed)
    }

    /// Brings the world in step with the disk at the given paths: those
    /// that file-system events name, whether a file or directory was
    /// created, changed, removed or renamed. A path that no longer exists
    /// is removed from the world with everything under it; a directory is
    /// read again with everything under it; `folio.toml` and `folio.lock`
    /// are parsed again, and a valid one replaces the world's; a change in
    /// the font directory, or of the lockfile, reads the project's fonts
    /// again. Paths outside the project, hidden ones and outputs are
    /// ignored. A path that a link inside the project leads to is updated
    /// under the link's path too. A host that keeps sources of its own on
    /// top of the disk, such as unsaved editor buffers, sets them again
    /// after an update that changed their paths.
    pub fn update<P: AsRef<Path>>(
        &mut self,
        world: &mut World,
        paths: impl IntoIterator<Item = P>,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<Update, LoadError> {
        let mut update = Update::default();
        let mut relative = BTreeSet::new();
        for path in paths {
            relative.extend(self.project_paths(path.as_ref()));
        }
        // The project directory stands for every path in it.
        if relative.contains("") {
            relative = [MANIFEST, LOCKFILE, PROJECT_FONT_DIR, ""]
                .map(String::from)
                .into();
        }
        // The manifest first, which decides the outputs and which the
        // lockfile is checked against; then the lockfile; then the fonts,
        // which the lockfile locks and a read of the files leaves out.
        for special in [MANIFEST, LOCKFILE] {
            if relative.remove(special) {
                self.refresh(world, special, &mut update, warnings)?;
            }
        }
        // The fonts are read as a whole, once, and not again if the new
        // lockfile had them read.
        let fonts = relative
            .iter()
            .any(|rel| is_under(rel, PROJECT_FONT_DIR) && !is_hidden(rel));
        relative.retain(|rel| !is_under(rel, PROJECT_FONT_DIR));
        if fonts && !update.changed.contains(LOCKFILE) {
            self.refresh(world, PROJECT_FONT_DIR, &mut update, warnings)?;
        }
        // A directory read again covers the paths under it.
        let mut done: Vec<String> = Vec::new();
        for rel in relative {
            if done.iter().any(|d| d.is_empty() || is_under(&rel, d)) {
                continue;
            }
            self.refresh(world, &rel, &mut update, warnings)?;
            done.push(rel);
        }
        Ok(update)
    }

    /// The project paths that an event's path stands for: its own, and
    /// those of the links that lead to it.
    fn project_paths(&self, path: &Path) -> Vec<String> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let mut out = Vec::new();
        for base in [&self.root, &self.canonical] {
            if let Some(rel) = path.strip_prefix(base).ok().and_then(relative) {
                out.push(rel);
            }
        }
        // The path resolved, and, since it may be gone or now be a link to
        // somewhere else, the path in its resolved directory.
        let in_dir = path
            .parent()
            .and_then(|dir| dir.canonicalize().ok())
            .zip(path.file_name())
            .map(|(dir, name)| dir.join(name));
        let resolved = path.canonicalize().ok();
        for resolved in resolved.iter().chain(&in_dir) {
            if let Some(rel) = resolved
                .strip_prefix(&self.canonical)
                .ok()
                .and_then(relative)
            {
                out.push(rel);
            }
            for (link, target) in &self.links {
                if let Ok(rest) = resolved.strip_prefix(target)
                    && let Some(rest) = relative(rest)
                {
                    out.push(join(link, &rest));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Brings the world in step with the disk at one project path.
    fn refresh(
        &mut self,
        world: &mut World,
        rel: &str,
        update: &mut Update,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<(), LoadError> {
        if is_hidden(rel) {
            return Ok(());
        }
        if rel.is_empty() {
            update.changed.extend(self.load(world, warnings)?);
            return Ok(());
        }
        if rel == MANIFEST {
            return self.refresh_manifest(world, update, warnings);
        }
        if rel == LOCKFILE {
            return self.refresh_lockfile(world, update, warnings);
        }
        if is_under(rel, PROJECT_FONT_DIR) {
            let before = world.font_files().clone();
            self.load_fonts(world, warnings);
            if *world.font_files() != before {
                update.changed.insert(rel.to_owned());
            }
            return Ok(());
        }
        let full = self.root.join(rel);
        self.links.retain(|link, _| !is_under(link, rel));
        // A source that a link outside the project stands for, in a
        // directory a read of the project follows, is recorded as a read
        // would record it.
        let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        if is_source(rel)
            && !self.outputs.contains(rel)
            && self.is_link(&full)
            && let Ok(target) = full.canonicalize()
            && !target.starts_with(&self.canonical)
            && target.is_file()
            && self.chain(dir).is_some()
        {
            for path in world.paths_under(rel) {
                if remove(world, &path) {
                    update.changed.insert(path);
                }
            }
            let file = ProjectFile::not_taken(rel.to_owned(), target, Unreadable::LinkOutOfProject);
            set(world, file, &mut update.changed)?;
            return Ok(());
        }
        // Reads are limited to the project, and leave out outputs.
        let canonical = full
            .canonicalize()
            .ok()
            .filter(|c| c.starts_with(&self.canonical) && !self.canonical_outputs().contains(c));
        let metadata = canonical
            .as_ref()
            .filter(|_| !self.outputs.contains(rel))
            .and_then(|c| fs::metadata(c).ok());
        // Gone, or an output: the world no longer has it.
        let Some(metadata) = metadata else {
            for path in world.paths_under(rel) {
                if remove(world, &path) {
                    update.changed.insert(path);
                }
            }
            return Ok(());
        };
        let canonical = canonical.expect("there is metadata");
        // The directories it is in must be ones a read of the project
        // follows, and so must it if it is a directory.
        let chain = if metadata.is_dir() {
            self.chain(rel)
        } else {
            self.chain(dir)
        };
        // A read of the project would not reach it: the world no longer
        // has what is under it.
        let Some(chain) = chain else {
            for path in world.paths_under(rel) {
                if remove(world, &path) {
                    update.changed.insert(path);
                }
            }
            return Ok(());
        };
        if self.is_link(&full) {
            self.links.insert(rel.to_owned(), canonical.clone());
        }

        if metadata.is_dir() {
            let mut files = Vec::new();
            let mut links = BTreeMap::new();
            if let Err(e) = self.scan(rel, chain, &mut files, &mut links, warnings) {
                // As a read of the project leaves it out.
                warnings.push(unreadable(rel, &e.to_string()));
                self.links.remove(rel);
                for path in world.paths_under(rel) {
                    if remove(world, &path) {
                        update.changed.insert(path);
                    }
                }
                return Ok(());
            }
            self.links.extend(links);
            let found: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
            for path in world.paths_under(rel) {
                if !found.contains(path.as_str()) && remove(world, &path) {
                    update.changed.insert(path);
                }
            }
            files.sort_by(|a, b| a.path.cmp(&b.path));
            set_all(world, files, &mut update.changed, &self.canonical)?;
        } else if metadata.is_file() {
            // A directory that is now a file.
            for path in world.paths_under(rel) {
                if path != rel && remove(world, &path) {
                    update.changed.insert(path);
                }
            }
            if !is_source(rel) {
                let file = ProjectFile {
                    path: rel.to_owned(),
                    disk: canonical,
                    bytes: None,
                    refused: None,
                };
                set_data(world, vec![file], &mut update.changed, &self.canonical);
                return Ok(());
            }
            let file = match fs::read(&canonical) {
                Ok(bytes) => ProjectFile {
                    path: rel.to_owned(),
                    disk: canonical,
                    bytes: Some(bytes),
                    refused: None,
                },
                Err(e) => ProjectFile::not_taken(
                    rel.to_owned(),
                    canonical,
                    Unreadable::Io(e.to_string().into()),
                ),
            };
            set(world, file, &mut update.changed)?;
        } else {
            // Neither a file nor a directory, which a read leaves out.
            for path in world.paths_under(rel) {
                if remove(world, &path) {
                    update.changed.insert(path);
                }
            }
        }
        Ok(())
    }

    fn refresh_manifest(
        &mut self,
        world: &mut World,
        update: &mut Update,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<(), LoadError> {
        let text = match self.read_manifest() {
            Ok(text) => text,
            Err(e) => {
                // The world keeps the manifest it has.
                warnings.push(warning(e.to_string()));
                return Ok(());
            }
        };
        let before = world.manifest().clone();
        match world.set_manifest(&text) {
            Ok(()) => {
                if *world.manifest() != before {
                    update.changed.insert(MANIFEST.into());
                    let outputs = self.outputs.clone();
                    self.set_outputs(world);
                    // A file that was an output may be an input now, and
                    // the other way round.
                    for path in outputs.symmetric_difference(&self.outputs.clone()) {
                        self.refresh(world, path, update, warnings)?;
                    }
                }
            }
            Err(errors) => {
                let errors = ProjectErrors::new(Some(&text), None, errors);
                merge(&mut update.errors, errors);
            }
        }
        Ok(())
    }

    fn refresh_lockfile(
        &mut self,
        world: &mut World,
        update: &mut Update,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<(), LoadError> {
        let text = match self.read_lockfile() {
            Ok(text) => text,
            Err(e) => {
                warnings.push(warning(e.to_string()));
                return Ok(());
            }
        };
        let before = world.lockfile().clone();
        match world.set_lockfile(text.as_deref()) {
            Ok(()) => {
                if *world.lockfile() != before {
                    update.changed.insert(LOCKFILE.into());
                    // The locked fonts come from the font cache.
                    self.load_fonts(world, warnings);
                }
            }
            Err(errors) => {
                let errors = ProjectErrors::new(None, text.as_deref(), errors);
                merge(&mut update.errors, errors);
            }
        }
        Ok(())
    }

    /// Reads the project's fonts again, warning when they cannot be read.
    fn load_fonts(&self, world: &mut World, warnings: &mut Vec<Diagnostic>) {
        if let Err(e) = world.load_fonts(&self.root, self.font_cache.as_ref()) {
            warnings.push(warning(format!("cannot read the project's fonts: {e}")));
        }
    }

    /// Whether a path is a symbolic link.
    fn is_link(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    }

    /// The canonical paths of the directories a project directory is in,
    /// itself included, which a link under it must not lead back to; `None`
    /// when a read of the project would not reach it: one of them is the
    /// font directory, leaves the project, is a link to a directory it is
    /// in, or cannot be resolved.
    fn chain(&self, rel: &str) -> Option<Vec<PathBuf>> {
        let mut chain = vec![self.canonical.clone()];
        let mut prefix = String::new();
        for name in rel.split('/').filter(|name| !name.is_empty()) {
            prefix = join(&prefix, name);
            if prefix == PROJECT_FONT_DIR {
                return None;
            }
            let canonical = self.root.join(&prefix).canonicalize().ok()?;
            if !canonical.starts_with(&self.canonical) || chain.contains(&canonical) {
                return None;
            }
            chain.push(canonical);
        }
        Some(chain)
    }

    /// The canonical paths the builds write to, where their directories
    /// exist.
    fn canonical_outputs(&self) -> BTreeSet<PathBuf> {
        self.outputs
            .iter()
            .filter_map(|output| {
                let path = self.root.join(output);
                let dir = path.parent()?.canonicalize().ok()?;
                Some(dir.join(path.file_name()?))
            })
            .collect()
    }

    /// Reads the files under the project directory `start`, whose own
    /// directories, from the project's on, have the canonical paths in
    /// `chain`. Fails only when `start` cannot be read.
    fn scan(
        &self,
        start: &str,
        chain: Vec<PathBuf>,
        files: &mut Vec<ProjectFile>,
        links: &mut BTreeMap<String, PathBuf>,
        warnings: &mut Vec<Diagnostic>,
    ) -> io::Result<()> {
        let outputs = self.canonical_outputs();
        let mut dirs = vec![(start.to_owned(), chain)];
        while let Some((dir, chain)) = dirs.pop() {
            let entries = fs::read_dir(self.root.join(&dir)).and_then(|entries| {
                entries
                    .map(|entry| entry.and_then(|e| Ok((e.file_type()?, e))))
                    .collect::<io::Result<Vec<_>>>()
            });
            let mut entries = match entries {
                Ok(entries) => entries,
                Err(e) if dir == start => return Err(e),
                Err(e) => {
                    warnings.push(unreadable(&dir, &e.to_string()));
                    continue;
                }
            };
            entries.sort_by_key(|(_, entry)| entry.file_name());
            let here = chain.last().expect("a chain holds the project").clone();
            for (file_type, entry) in entries {
                // A name that is not Unicode cannot be named in a document.
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let path = join(&dir, &name);
                let target = if file_type.is_symlink() {
                    // Reads are limited to the project. A source that a
                    // link outside stands for is recorded, so that a build
                    // that names it says why it is not read.
                    match entry.path().canonicalize() {
                        Ok(target) if target.starts_with(&self.canonical) => Some(target),
                        Ok(target) if is_source(&path) && target.is_file() => {
                            files.push(ProjectFile::not_taken(
                                path,
                                target,
                                Unreadable::LinkOutOfProject,
                            ));
                            continue;
                        }
                        _ => continue,
                    }
                } else {
                    None
                };
                let is_dir = match &target {
                    Some(target) => target.is_dir(),
                    None => file_type.is_dir(),
                };
                if is_dir {
                    if path == PROJECT_FONT_DIR {
                        continue;
                    }
                    let link = target.is_some();
                    let target = match target {
                        Some(target) => target,
                        None => here.join(&name),
                    };
                    // A link to a directory it is in would never end.
                    if chain.contains(&target) {
                        continue;
                    }
                    if link {
                        links.insert(path.clone(), target.clone());
                    }
                    let mut chain = chain.clone();
                    chain.push(target);
                    dirs.push((path, chain));
                    continue;
                }
                let is_file = match &target {
                    Some(target) => target.is_file(),
                    None => file_type.is_file(),
                };
                if !is_file || path == MANIFEST || path == LOCKFILE || self.outputs.contains(&path)
                {
                    continue;
                }
                // An output written through a link is still an output.
                let canonical = target.clone().unwrap_or_else(|| here.join(&name));
                if outputs.contains(&canonical) {
                    continue;
                }
                if let Some(target) = target {
                    links.insert(path.clone(), target);
                }
                // Only sources are read now; any other file when a build
                // names it.
                if !is_source(&path) {
                    files.push(ProjectFile {
                        path,
                        disk: canonical,
                        bytes: None,
                        refused: None,
                    });
                    continue;
                }
                files.push(match fs::read(entry.path()) {
                    Ok(bytes) => ProjectFile {
                        path,
                        disk: canonical,
                        bytes: Some(bytes),
                        refused: None,
                    },
                    Err(e) => ProjectFile::not_taken(
                        path,
                        canonical,
                        Unreadable::Io(e.to_string().into()),
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The byte order mark, which the loader leaves out of a source's text.
const BOM: &str = "\u{FEFF}";

/// Whether a project path names a source: a `.fol` or `.fsty` file.
fn is_source(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|e| e.to_str()),
        Some("fol" | "fsty")
    )
}

/// Sets files in a world, as [`set`] does sources and [`set_data`] the
/// other files, which it sets together.
fn set_all(
    world: &mut World,
    files: Vec<ProjectFile>,
    changed: &mut BTreeSet<String>,
    root: &Path,
) -> Result<(), WorldError> {
    let mut data = Vec::new();
    for file in files {
        if is_source(&file.path) {
            set(world, file, changed)?;
        } else {
            data.push(file);
        }
    }
    set_data(world, data, changed, root);
    Ok(())
}

/// Sets a source in a world. Records its path in `changed` if the world
/// had it differently. A source that was not taken, or is not UTF-8, is
/// recorded as unreadable with why, which a build that names it reports;
/// leaves out files whose paths a document cannot name.
fn set(
    world: &mut World,
    file: ProjectFile,
    changed: &mut BTreeSet<String>,
) -> Result<(), WorldError> {
    let text = match (file.bytes, file.refused) {
        (Some(bytes), None) => String::from_utf8(bytes).map_err(|_| Unreadable::NotUtf8),
        (_, Some(why)) => Err(why),
        (None, None) => Err(Unreadable::Io("it was not read".into())),
    };
    let text = match text {
        Ok(text) => text,
        Err(why) => {
            let removed = world.remove_source(&file.path);
            if world.set_unreadable(&file.path, Some(why)) || removed {
                changed.insert(file.path);
            }
            return Ok(());
        }
    };
    // Some editors start UTF-8 files with a byte-order mark, which is not
    // part of the text. Diagnostics count bytes in the file, so the world
    // records its length.
    let (text, start) = match text.strip_prefix(BOM) {
        Some(rest) => (rest, BOM.len()),
        None => (&text[..], 0),
    };
    if let Some(source) = world.source(&file.path)
        && source.text() == text
    {
        let id = source.id();
        world.set_text_start(id, start);
        return Ok(());
    }
    match world.set_source(&file.path, text) {
        Ok(id) => {
            world.set_text_start(id, start);
            changed.insert(file.path);
            Ok(())
        }
        Err(WorldError::InvalidPath(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Sets image and data files in a world, all new ones at once, as files
/// read when a build names them, and only while they are inside the
/// project, the canonical path `root`. A file the world has and a build has
/// read is read again now, and recorded in `changed` if it reads
/// differently; a file that no build has read is set afresh, unread, and
/// not recorded, since no build depends on it. Leaves out files whose
/// paths a document cannot name.
fn set_data(
    world: &mut World,
    files: Vec<ProjectFile>,
    changed: &mut BTreeSet<String>,
    root: &Path,
) {
    let mut contents = Vec::new();
    for file in files {
        let Some(path) = ProjectPath::new(&file.path) else {
            continue;
        };
        let content = FileContent::in_project(file.disk, root);
        let Some(old) = world.file_content(&file.path) else {
            contents.push((path, content));
            changed.insert(file.path);
            continue;
        };
        match old.loaded() {
            // Nothing read it: set afresh, unread.
            None => contents.push((path, content)),
            Some(before) => {
                let _ = content.load();
                if content.loaded() != Some(before) {
                    contents.push((path, content));
                    changed.insert(file.path);
                }
            }
        }
    }
    world.set_contents(contents);
}

/// Removes a source, data file or unreadable source from a world. Returns
/// whether there was one.
fn remove(world: &mut World, path: &str) -> bool {
    let source = world.remove_source(path);
    let file = world.remove_file(path);
    let unreadable = world.set_unreadable(path, None);
    source || file || unreadable
}

/// Whether a project path names a hidden file or directory, or one in a
/// hidden directory.
fn is_hidden(path: &str) -> bool {
    path.split('/').any(|name| name.starts_with('.'))
}

/// Whether a project path is `dir` or under it.
fn is_under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Joins two project paths, either of which may be empty.
fn join(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (true, _) => name.to_owned(),
        (_, true) => dir.to_owned(),
        _ => format!("{dir}/{name}"),
    }
}

/// A path relative to the project as a project path, with `/` as
/// separator; `None` if it leaves the project or a name is not Unicode.
fn relative(path: &Path) -> Option<String> {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => out = join(&out, name.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Adds the errors of one project file to those of the other.
fn merge(errors: &mut Option<ProjectErrors>, new: ProjectErrors) {
    match errors {
        None => *errors = Some(new),
        Some(errors) => {
            for id in [World::MANIFEST, World::LOCKFILE] {
                use crate::diag::Files;
                if let (Some(path), Some(lines)) = (new.files.path(id), new.files.lines(id)) {
                    errors.files.insert(id, path, lines.text());
                }
            }
            errors.diagnostics.extend(new.diagnostics);
        }
    }
}

/// A warning about the project without a place in a file.
fn warning(message: String) -> Diagnostic {
    Diagnostic::warning(DiagSpan::detached(), message)
}

/// The warning that a directory cannot be read.
fn unreadable(path: &str, why: &str) -> Diagnostic {
    warning(format!("cannot read `{path}`: {why}"))
        .with_hint("a document that uses a file in it reports it as not found")
}

impl ProjectFile {
    /// A source that was not taken, and why.
    fn not_taken(path: String, disk: PathBuf, why: Unreadable) -> Self {
        Self {
            path,
            disk,
            bytes: None,
            refused: Some(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(root: &Path, outputs: &[&str]) -> (Vec<String>, Vec<Diagnostic>) {
        let mut loader = Loader::new(root).unwrap();
        loader.outputs = outputs.iter().map(|&o| o.to_owned()).collect();
        let mut warnings = Vec::new();
        let files = loader.read_files(&mut warnings).unwrap();
        (files.into_iter().map(|f| f.path).collect(), warnings)
    }

    #[test]
    fn reads_the_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: &[u8]| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        write(MANIFEST, b"");
        write(LOCKFILE, b"");
        write("main.fol", b"= Forces");
        write("styles/exam.fsty", b"");
        write("data/points.csv", b"x,y\n");
        write(".git/config", b"");
        write("styles/.hidden.fsty", b"");
        write("fonts/serif.ttf", b"");
        write("out/main-exam.pdf", b"");
        write("out/figure.png", b"");
        write("outline/notes.fol", b"");

        let (paths, warnings) = read(root, &["out/main-exam.pdf"]);
        assert_eq!(
            paths,
            [
                "data/points.csv",
                "main.fol",
                "out/figure.png",
                "outline/notes.fol",
                "styles/exam.fsty"
            ]
        );
        assert!(warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_only_inside_the_project() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.csv"), b"x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        fs::create_dir_all(root.join("shared/questions")).unwrap();
        fs::write(root.join("shared/questions/q1.fol"), b"").unwrap();
        symlink(root.join("main.fol"), root.join("alias.fol")).unwrap();
        symlink(outside.path().join("secret.csv"), root.join("secret.csv")).unwrap();
        symlink(outside.path(), root.join("elsewhere")).unwrap();
        symlink(root.join("shared/questions"), root.join("questions")).unwrap();
        symlink(root, root.join("loop")).unwrap();
        symlink(root.join("shared"), root.join("shared/questions/up")).unwrap();

        let (paths, _) = read(root, &[]);
        assert_eq!(
            paths,
            [
                "alias.fol",
                "main.fol",
                "questions/q1.fol",
                "shared/questions/q1.fol",
            ]
        );
    }

    /// An output written through a link to a directory inside the project
    /// is not read as an input (#230).
    #[cfg(unix)]
    #[test]
    fn leaves_out_outputs_written_through_links() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        fs::create_dir(root.join("build")).unwrap();
        symlink(root.join("build"), root.join("pdfs")).unwrap();
        // What a build with the output `pdfs/main.pdf` wrote.
        fs::write(root.join("build/main.pdf"), b"%PDF-").unwrap();
        fs::write(root.join("build/notes.csv"), b"").unwrap();

        let (paths, _) = read(root, &["pdfs/main.pdf"]);
        assert_eq!(paths, ["build/notes.csv", "main.fol", "pdfs/notes.csv"]);
    }

    #[cfg(unix)]
    #[test]
    fn warns_of_unreadable_directories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/x.csv"), b"").unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        // Permissions do not stop a privileged user.
        let readable = fs::read_dir(root.join("locked")).is_ok();

        let mut loader = Loader::new(root).unwrap();
        let mut warnings = Vec::new();
        let files = loader.read_files(&mut warnings);
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        let files = files.unwrap();
        if !readable {
            assert_eq!(files.len(), 1);
            assert!(warnings[0].message.contains("`locked`"));
        }
    }

    /// A source that is not UTF-8, or a link out of the project, is
    /// recorded as unreadable on an update too, and forgotten once it is
    /// readable or gone (#343).
    #[cfg(unix)]
    #[test]
    fn records_unreadable_sources_on_updates() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("q.fol"), b"= Q\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join(MANIFEST),
            "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
        )
        .unwrap();
        fs::write(root.join("main.fol"), b"= A\n").unwrap();
        let mut loader = Loader::new(root).unwrap();
        let mut world = loader.world(World::builder()).unwrap();
        let mut warnings = Vec::new();
        loader.load(&mut world, &mut warnings).unwrap();
        let unreadable = |world: &World| {
            world.source("main.fol").is_none() && world.paths_under("main.fol") == ["main.fol"]
        };
        assert!(!unreadable(&world));

        fs::write(root.join("main.fol"), b"= \xb3\n").unwrap();
        let update = loader
            .update(&mut world, [root.join("main.fol")], &mut warnings)
            .unwrap();
        assert!(update.changed.contains("main.fol"));
        assert!(unreadable(&world));

        fs::write(root.join("main.fol"), b"= B\n").unwrap();
        loader
            .update(&mut world, [root.join("main.fol")], &mut warnings)
            .unwrap();
        assert_eq!(world.source("main.fol").unwrap().text(), "= B\n");
        assert!(!unreadable(&world));

        fs::remove_file(root.join("main.fol")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("q.fol"), root.join("main.fol")).unwrap();
        let update = loader
            .update(&mut world, [root.join("main.fol")], &mut warnings)
            .unwrap();
        assert!(update.changed.contains("main.fol"));
        assert!(unreadable(&world));

        fs::remove_file(root.join("main.fol")).unwrap();
        let update = loader
            .update(&mut world, [root.join("main.fol")], &mut warnings)
            .unwrap();
        assert!(update.changed.contains("main.fol"));
        assert!(world.paths_under("main.fol").is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn sets_sources_and_data() {
        let mut world = World::new(
            "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = |path: &str, bytes: &[u8]| {
            let disk = root.join(path.replace(['/', '\\'], "_"));
            fs::write(&disk, bytes).unwrap();
            ProjectFile {
                path: path.into(),
                disk,
                bytes: is_source(path).then(|| bytes.to_vec()),
                refused: None,
            }
        };
        let mut changed = BTreeSet::new();
        let files = vec![
            file("bad.fol", &[0xff, 0xfe]),
            file("data/points.csv", b"x,y\n"),
            file("main.fol", b"= Forces\n"),
            file("bom.fol", "\u{FEFF}= Forces\n".as_bytes()),
            file("weird\\name.csv", b""),
        ];
        set_all(&mut world, files, &mut changed, &root).unwrap();
        assert_eq!(world.source("main.fol").unwrap().text(), "= Forces\n");
        assert_eq!(world.source("bom.fol").unwrap().text(), "= Forces\n");
        assert!(world.source("bad.fol").is_none());
        assert_eq!(world.paths_under("bad.fol"), ["bad.fol"]);
        assert_eq!(
            changed.iter().map(String::as_str).collect::<Vec<_>>(),
            ["bad.fol", "bom.fol", "data/points.csv", "main.fol"]
        );

        // The data file is not read until something asks for it.
        let content = world.file_content("data/points.csv").unwrap();
        assert!(content.loaded().is_none());

        // Setting what the world has changes nothing, and a data file no
        // build has read is not read for it.
        let mut again = BTreeSet::new();
        let files = vec![
            file("main.fol", b"= Forces\n"),
            file("data/points.csv", b"x,y\n"),
        ];
        set_all(&mut world, files, &mut again, &root).unwrap();
        assert!(again.is_empty());
        assert!(
            world
                .file_content("data/points.csv")
                .unwrap()
                .loaded()
                .is_none()
        );

        // Once read, a data file is compared with the disk.
        assert_eq!(&*world.file("data/points.csv").unwrap(), b"x,y\n");
        let files = vec![file("data/points.csv", b"x,y\n")];
        set_all(&mut world, files, &mut again, &root).unwrap();
        assert!(again.is_empty());
        let files = vec![file("data/points.csv", b"x,y\n1,2\n")];
        set_all(&mut world, files, &mut again, &root).unwrap();
        assert_eq!(again.iter().collect::<Vec<_>>(), ["data/points.csv"]);
        assert_eq!(&*world.file("data/points.csv").unwrap(), b"x,y\n1,2\n");
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("a/./b")).as_deref(), Some("a/b"));
        assert_eq!(relative(Path::new("../a")), None);
        assert_eq!(relative(Path::new("")).as_deref(), Some(""));
        assert!(is_under("a/b", "a"));
        assert!(is_under("a", "a"));
        assert!(!is_under("ab", "a"));
    }
}
