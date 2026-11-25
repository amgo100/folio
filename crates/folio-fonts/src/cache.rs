//! The immutable font cache: font files locked in `folio.lock`, stored
//! under their hash (SPEC §2.3, §2.4).
//!
//! A file in the cache is named by the SHA-256 hash of its contents. It is
//! written once, to a temporary file that is renamed into place and made
//! read-only, and never changed afterwards: a different font has a
//! different hash and so a different file. That is what makes it safe to
//! memory-map, which this module alone does in the compiler (the audited
//! exception of SPEC §14.2). Project and system fonts can change while
//! they are in use, so they are read into memory instead.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::file::{FontFile, FontOrigin};
use crate::hash::FontHash;

/// Folio's cache directory, which holds the font cache and the toolchain
/// cache (SPEC §2.4): `$FOLIO_CACHE_DIR` when the variable is set,
/// otherwise `folio` in the user's cache directory (`$XDG_CACHE_HOME` or
/// `~/.cache` on Linux, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on
/// Windows). `None` when there is no such directory.
pub fn cache_root() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(dir) = var("FOLIO_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = if cfg!(windows) {
        var("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| PathBuf::from(home).join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    Some(base?.join("folio"))
}

/// The immutable font cache in a directory.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontCache {
    dir: PathBuf,
}

impl FontCache {
    /// The cache in a directory, which need not exist yet.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The cache's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The default directory: `fonts` in Folio's cache directory
    /// ([`cache_root`]). `None` when there is no such directory.
    pub fn default_dir() -> Option<PathBuf> {
        cache_root().map(|root| root.join("fonts"))
    }

    /// The path a file with this hash is stored at.
    pub fn path(&self, hash: FontHash) -> PathBuf {
        self.dir.join(hash.hex())
    }

    /// The file with this hash, memory-mapped, or `None` when the cache does
    /// not have it. Each file is mapped once per process, and its contents
    /// are checked against its name when it is mapped.
    ///
    /// Fails when the file cannot be read, or when its contents do not have
    /// the hash it is stored under: the cache was changed from outside.
    pub fn get(&self, hash: FontHash) -> io::Result<Option<FontFile>> {
        static MAPPED: Mutex<Option<HashMap<PathBuf, FontFile>>> = Mutex::new(None);

        let path = self.path(hash);
        let lock = || MAPPED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = lock().as_ref().and_then(|m| m.get(&path)) {
            return Ok(Some(file.clone()));
        }
        // Mapped and checked without the lock, so that lookups of other
        // files need not wait for the hashing; a file mapped twice at once
        // is kept once.
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let data = map::read_only(&file)?;
        let found = FontHash::of(&data);
        if found != hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the font cache file {} has hash {found}, not the hash it is named by; \
                     delete it so that it is fetched again",
                    path.display()
                ),
            ));
        }
        let font = FontFile::with_hash(FontOrigin::Cache, data, hash);
        let mut mapped = lock();
        let font = mapped
            .get_or_insert_with(HashMap::new)
            .entry(path)
            .or_insert(font)
            .clone();
        Ok(Some(font))
    }

    /// Stores a font file under its hash, unless the cache has it, and
    /// returns the hash. The file is written to a temporary file that is
    /// renamed into place and made read-only, so a reader never sees it
    /// half-written and nothing changes it afterwards.
    pub fn insert(&self, data: &[u8]) -> io::Result<FontHash> {
        let hash = FontHash::of(data);
        let path = self.path(hash);
        if path.exists() {
            return Ok(hash);
        }
        fs::create_dir_all(&self.dir)?;
        let temp = self
            .dir
            .join(format!(".{}.{}.tmp", hash.hex(), std::process::id()));
        let result = (|| {
            let mut file = File::create(&temp)?;
            file.write_all(data)?;
            file.sync_all()?;
            let mut permissions = file.metadata()?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&temp, permissions)?;
            fs::rename(&temp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
            // Another process may have stored the same file meanwhile.
            if path.exists() {
                return Ok(hash);
            }
        }
        result.map(|()| hash)
    }
}

/// Memory-mapping, the one use of unsafe code outside `folio-node`.
#[allow(unsafe_code)]
mod map {
    use std::fs::File;
    use std::io;

    use memmap2::Mmap;

    use crate::data::FontData;

    /// Maps a file of the font cache.
    pub(super) fn read_only(file: &File) -> io::Result<FontData> {
        // SAFETY: a mapping is undefined behaviour if the file changes
        // while it is mapped. Files of the font cache never change: each is
        // written once to a temporary file, made read-only and renamed into
        // place under the hash of its contents, and the cache never writes
        // to a file that exists (`FontCache::insert`). The caller checks the
        // contents against the hash after mapping. What remains is a file
        // changed from outside Folio, against its read-only permission,
        // while a compile runs: the risk accepted by every program that maps
        // fonts, and the reason the cache alone is mapped (SPEC §2.3).
        let map = unsafe { Mmap::map(file)? };
        Ok(FontData::from_owner(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled::bundled;

    #[test]
    fn stores_maps_and_checks() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path().join("fonts"));
        let font = &bundled()[0];
        let hash = FontHash::of(font.data());
        assert!(cache.get(hash).unwrap().is_none());

        assert_eq!(cache.insert(font.data()).unwrap(), hash);
        assert_eq!(cache.insert(font.data()).unwrap(), hash);
        let path = cache.path(hash);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());

        let got = cache.get(hash).unwrap().unwrap();
        assert_eq!(got.origin(), &FontOrigin::Cache);
        assert_eq!(got.hash(), hash);
        assert_eq!(&**got.data(), &**font.data());
        assert_eq!(
            got.faces().unwrap()[0].info(),
            font.faces().unwrap()[0].info()
        );
        // Mapped once per process.
        let again = cache.get(hash).unwrap().unwrap();
        assert!(std::ptr::eq(got.data().as_ptr(), again.data().as_ptr()));
    }

    #[test]
    fn a_changed_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path());
        let hash = FontHash::of(b"a font");
        fs::write(cache.path(hash), b"another font").unwrap();
        let error = cache.get(hash).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
