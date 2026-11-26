//! Reading a project from disk, through the library's loader
//! ([`folio::loader`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use folio::loader::{self, Loader};
use folio::{Diagnostic, World};

pub(crate) use folio::loader::{LOCKFILE, MANIFEST};

/// The project directory: `root` if given, which must hold `folio.toml`,
/// or else the current directory or the nearest directory above it that
/// holds one.
pub(crate) fn find_root(root: Option<&Path>) -> Result<PathBuf, String> {
    loader::find_root(root).map_err(|e| e.to_string())
}

/// Reads the lockfile, if the project has one.
pub(crate) fn read_lockfile(root: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(root.join(LOCKFILE)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Reads the files of the project in `root` into a world, leaving out the
/// files that the builds of its manifest write. Warns of files that cannot
/// be read.
pub(crate) fn load(
    root: &Path,
    world: &mut World,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(), String> {
    let mut loader = Loader::new(root).map_err(|e| e.to_string())?;
    loader.set_outputs(world);
    loader
        .load(world, warnings)
        .map(drop)
        .map_err(|e| e.to_string())
}
