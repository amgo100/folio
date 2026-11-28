//! The test corpus in `tests/corpus`: one directory per project, each with
//! its `folio.toml`, its content and its stylesheets.
//!
//! Every build of every project is checked by the layout golden files
//! (`tests/golden.rs` of this crate), krilla's PDF/UA-1 validator
//! (`folio`'s `tests/pdf.rs`), the determinism check (`folio`'s
//! `tests/determinism.rs`) and the command line (`folio-cli`'s `tests/cli.rs`).
//! A new document that exercises a feature goes here, and its golden files
//! are written with `scripts/bless.sh`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use folio::{BuildMode, Name, Target, World};

/// The corpus directory.
pub fn dir() -> PathBuf {
    crate::root().join("tests/corpus")
}

/// A project: its manifest and its sources, without a directory of its own
/// when it was generated ([`crate::refdoc`]).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Project {
    /// The project's name: its directory's name.
    pub name: String,
    /// The text of `folio.toml`.
    pub manifest: String,
    /// The `.fol` and `.fsty` sources, by path relative to the project's
    /// directory, with `/` between components.
    pub sources: BTreeMap<String, String>,
    /// The data files, such as images, by path as the sources are.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Project {
    /// Reads a project directory: its manifest, every `.fol` and `.fsty`
    /// file under it, and its other files, such as images, as data files.
    /// The corpus has no fonts or lockfiles.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("a project directory needs a UTF-8 name"))?
            .to_string();
        let manifest = fs::read_to_string(dir.join("folio.toml"))?;
        let mut sources = BTreeMap::new();
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in fs::read_dir(&next)? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path
                        .strip_prefix(dir)
                        .map_err(io::Error::other)?
                        .to_str()
                        .ok_or_else(|| io::Error::other("a file needs a UTF-8 path"))?
                        .replace('\\', "/");
                    match path.extension().and_then(|e| e.to_str()) {
                        Some("fol" | "fsty") => {
                            sources.insert(relative, fs::read_to_string(&path)?);
                        }
                        // The manifest, and what a build writes.
                        _ if relative == "folio.toml" || relative.starts_with("out/") => {}
                        _ => {
                            files.insert(relative, fs::read(&path)?);
                        }
                    }
                }
            }
        }
        Ok(Self {
            name,
            manifest,
            sources,
            files,
        })
    }

    /// A world with the project's sources, in a build mode.
    ///
    /// # Panics
    ///
    /// When the manifest or a source path is invalid: corpus projects are
    /// meant to be valid.
    pub fn world(&self, mode: BuildMode) -> World {
        let mut world = World::builder()
            .mode(mode)
            .build(&self.manifest, None)
            .unwrap_or_else(|errors| panic!("{}: invalid manifest: {errors:?}", self.name));
        for (path, text) in &self.sources {
            world
                .set_source(path, text)
                .unwrap_or_else(|e| panic!("{}: cannot set {path}: {e}", self.name));
        }
        for (path, bytes) in &self.files {
            world
                .set_file(path, bytes.clone())
                .unwrap_or_else(|e| panic!("{}: cannot set {path}: {e}", self.name));
        }
        world
    }

    /// The name of a build's output as `folio build` writes it by default
    /// (SPEC §6.4) without its extension: `{project}-{variant}`, or the
    /// project's name for a project without variants.
    pub fn build_name(&self, variant: Option<&Name>) -> String {
        match variant {
            Some(variant) => format!("{}-{variant}", self.name),
            None => self.name.clone(),
        }
    }

    /// [`Project::build_name`] of a build of one version of a shuffled
    /// paper (SPEC §6.4): `{project}-{variant}-{version}`, each part left
    /// out when the project has none.
    pub fn versioned_build_name(&self, variant: Option<&Name>, version: Option<&Name>) -> String {
        let mut name = self.build_name(variant);
        if let Some(version) = version {
            name.push('-');
            name.push_str(version.as_str());
        }
        name
    }
}

/// The corpus projects, in order of name.
///
/// # Panics
///
/// When the corpus cannot be read.
pub fn projects() -> Vec<Project> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir())
        .expect("the corpus directory")
        .map(|entry| entry.expect("a corpus entry").path())
        .filter(|path| path.join("folio.toml").is_file())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            Project::read(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        })
        .collect()
}

/// The targets that go through Folio's layout (SPEC §2.1), whose pages the
/// layout golden files hold.
pub fn laid_out(target: Target) -> bool {
    matches!(target, Target::Pdf | Target::Live)
}
