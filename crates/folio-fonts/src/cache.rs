//! The immutable font cache: font files locked in `folio.lock`, stored
//! under their hash (SPEC §2.4, §2.5).
//!
//! A file in the cache is named by the SHA-256 hash of its contents. A
//! file that [`FontCache::insert`] wrote is written once, to a temporary
//! file that is renamed into place and made read-only, and never changed
//! afterwards: a different font has a different hash and so a different
//! file. That is what makes it safe to memory-map, which this module alone
//! does in the compiler (the audited exception of SPEC §14.2). A file put
//! in the cache by hand, and project and system fonts, can change while
//! they are in use, so they are read into memory instead.

use std::collections::HashMap;
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use crate::discover::read;
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

    /// The file with this hash, or `None` when the cache does not have it.
    /// The file has its own hash ([`FontFile::hash`]), which differs from
    /// `hash` when the cache was changed from outside or a file was copied
    /// in under the wrong name; the check of `folio.lock` then reports the
    /// file with both hashes (SPEC §2.5).
    ///
    /// A file that [`Self::insert`] wrote, which is read-only and carries
    /// the modification time `insert` gives it, is memory-mapped, once per
    /// process; so is a copy that keeps both. Any other file, such as one
    /// copied into the cache by hand, could change while it is in use, so
    /// it is read into memory, once per process while its length and
    /// modification time stay the same.
    ///
    /// Fails when the file cannot be read.
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
        // The metadata of the open file, not of the path, so that the file
        // checked is the file mapped.
        if written_by_insert(&file.metadata()?, hash) {
            let data = map::read_only(&file)?;
            if FontHash::of(&data) == hash {
                let font = FontFile::with_hash(FontOrigin::Cache, data, hash);
                let mut mapped = lock();
                let font = mapped
                    .get_or_insert_with(HashMap::new)
                    .entry(path)
                    .or_insert(font)
                    .clone();
                return Ok(Some(font));
            }
            // Not what `insert` wrote: read it as any other file.
        }
        drop(file);
        read(&path, FontOrigin::Cache).map(Some)
    }

    /// Stores a font file under its hash, unless the cache has it, and
    /// returns the hash. A file under that name that `insert` did not
    /// write, such as one copied in by hand, is replaced. The file is
    /// written to a temporary file of its own
    /// and given the modification time that marks it as written here
    /// ([`stamp`]), then renamed into place and made read-only, so a reader
    /// never sees it half-written and nothing changes it afterwards. Until
    /// it is read-only, [`Self::get`] reads it rather than mapping it.
    pub fn insert(&self, data: &[u8]) -> io::Result<FontHash> {
        let hash = FontHash::of(data);
        let path = self.path(hash);
        let stored = || fs::metadata(&path).is_ok_and(|meta| written_by_insert(&meta, hash));
        if stored() {
            return Ok(hash);
        }
        fs::create_dir_all(&self.dir)?;
        let (mut file, temp) = self.temporary(hash)?;
        let result = (|| {
            file.write_all(data)?;
            // Without the stamp the file is only read, never mapped.
            let _ = file.set_modified(stamp(hash));
            file.sync_all()?;
            // Windows renames nothing over a read-only file, such as a copy
            // that kept its mode but not the stamp.
            #[cfg(windows)]
            if let Ok(meta) = fs::metadata(&path)
                && meta.permissions().readonly()
            {
                let mut permissions = meta.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&path, permissions);
            }
            fs::rename(&temp, &path)?;
            // Read-only after the rename, so that a temporary file left by
            // a failed rename can still be removed on Windows.
            let mut permissions = file.metadata()?.permissions();
            permissions.set_readonly(true);
            file.set_permissions(permissions)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
            // Another process may have stored the same file meanwhile.
            if stored() {
                return Ok(hash);
            }
        }
        result.map(|()| hash)
    }

    /// A new temporary file in the cache for a file with this hash, with a
    /// name unique per call, so that threads of one process storing the
    /// same file at once do not write to one file. A name taken by a file
    /// that a crashed process left behind is passed over.
    fn temporary(&self, hash: FontHash) -> io::Result<(File, PathBuf)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let temp = self.dir.join(format!(
                ".{}.{}.{}.tmp",
                hash.hex(),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match File::create_new(&temp) {
                Ok(file) => return Ok((file, temp)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// The modification time [`FontCache::insert`] gives the file it stores
/// under a hash: a time taken from the hash, which a file copied into the
/// cache keeps only by a rare coincidence or when the copy keeps times
/// (`cp -a`). It is a whole, even number of seconds from 1980 to 2014, so
/// that file systems with 32-bit times, times from 1980 or 2-second
/// precision store it as given, and no tool sees a time in the future.
fn stamp(hash: FontHash) -> SystemTime {
    /// 1980-01-01T00:00:00Z.
    const START: u64 = 315_532_800;
    let [a, b, c, d, ..] = hash.0;
    let offset = u32::from_be_bytes([a, b, c, d]) & 0x3FFF_FFFE;
    SystemTime::UNIX_EPOCH + Duration::from_secs(START + u64::from(offset))
}

/// Whether a file of the cache shows that [`FontCache::insert`] wrote it,
/// and so may be mapped: a regular file that is read-only (on Unix, no one
/// has write permission) and has the modification time that `insert` gives
/// a file with its hash.
fn written_by_insert(meta: &Metadata, hash: FontHash) -> bool {
    meta.is_file()
        && meta.permissions().readonly()
        && meta.modified().is_ok_and(|time| time == stamp(hash))
}

/// Memory-mapping, the one use of unsafe code outside `folio-node`.
#[allow(unsafe_code)]
mod map {
    use std::fs::File;
    use std::io;

    use memmap2::Mmap;

    use crate::data::FontData;

    /// Maps a file of the font cache that [`FontCache::insert`] wrote,
    /// as [`written_by_insert`] showed for the open `file`.
    ///
    /// [`FontCache::insert`]: super::FontCache::insert
    /// [`written_by_insert`]: super::written_by_insert
    pub(super) fn read_only(file: &File) -> io::Result<FontData> {
        // SAFETY: a mapping is undefined behaviour if the file changes
        // while it is mapped. The caller maps only a file that shows, by
        // the metadata of the open handle, that `FontCache::insert` wrote
        // it: read-only for everyone, with the modification time `insert`
        // gives it. `insert` writes each file once to a temporary file of
        // its own, renames it into place under the hash of its contents and
        // makes it read-only, and never writes to a file that exists; a copy
        // that keeps both marks is read-only too. Any other file, such as
        // one copied into the cache by hand, is read into memory instead,
        // as is a mapped file whose contents do not have its hash. What
        // remains is a file changed from outside
        // Folio, against its read-only permission, while a compile runs:
        // the risk accepted by every program that maps fonts, and the
        // reason only these files are mapped (SPEC §2.5, §14.2).
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
        let meta = fs::metadata(&path).unwrap();
        assert!(meta.permissions().readonly());
        assert!(written_by_insert(&meta, hash));

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

    /// A file whose contents do not have its name's hash is returned with
    /// its own hash, for the lock check to report, and never mapped.
    #[test]
    fn a_changed_file_has_its_own_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path());
        let hash = FontHash::of(b"a font");
        let path = cache.path(hash);
        fs::write(&path, b"another font").unwrap();
        let got = cache.get(hash).unwrap().unwrap();
        assert_eq!(got.hash(), FontHash::of(b"another font"));

        // With both marks, it is still read rather than mapped.
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(stamp(hash)).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions).unwrap();
        assert!(written_by_insert(&fs::metadata(&path).unwrap(), hash));
        let got = cache.get(hash).unwrap().unwrap();
        assert_eq!(&**got.data(), b"another font");
        assert_eq!(got.hash(), FontHash::of(b"another font"));
    }

    /// A file copied into the cache by hand is read into memory, not
    /// mapped, so a change to it cannot reach a font in use.
    #[test]
    fn a_copied_file_is_read_not_mapped() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path());
        let font = &bundled()[0];
        let hash = FontHash::of(font.data());
        let path = cache.path(hash);
        fs::write(&path, font.data()).unwrap();
        assert!(!written_by_insert(&fs::metadata(&path).unwrap(), hash));

        let got = cache.get(hash).unwrap().unwrap();
        assert_eq!(got.origin(), &FontOrigin::Cache);
        assert_eq!(&**got.data(), &**font.data());

        // Truncated in place: the font in use keeps its bytes, and the
        // next lookup reads the file again, with its new hash.
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(16)
            .unwrap();
        assert_eq!(&**got.data(), &**font.data());
        let again = cache.get(hash).unwrap().unwrap();
        assert_eq!(again.data().len(), 16);
        assert_ne!(again.hash(), hash);

        // Storing the font replaces the file that `insert` did not write.
        assert_eq!(cache.insert(font.data()).unwrap(), hash);
        assert!(written_by_insert(&fs::metadata(&path).unwrap(), hash));
        assert_eq!(&**cache.get(hash).unwrap().unwrap().data(), &**font.data());
    }

    /// Read-only is not enough: a file is mapped only with the
    /// modification time `insert` gives it, and only when read-only.
    #[test]
    fn maps_only_what_insert_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path());
        let font = &bundled()[0];
        let hash = FontHash::of(font.data());
        let path = cache.path(hash);
        fs::write(&path, font.data()).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        let read_only = |yes: bool| {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(yes);
            fs::set_permissions(&path, permissions).unwrap();
        };

        read_only(true);
        assert!(!written_by_insert(&fs::metadata(&path).unwrap(), hash));
        read_only(false);
        file.set_modified(stamp(hash)).unwrap();
        assert!(!written_by_insert(&fs::metadata(&path).unwrap(), hash));
        read_only(true);
        assert!(written_by_insert(&fs::metadata(&path).unwrap(), hash));
        // Another hash's time does not count.
        assert!(!written_by_insert(
            &fs::metadata(&path).unwrap(),
            FontHash::of(b"another font")
        ));
        read_only(false);
    }

    /// Threads storing the same file at once each write a temporary file of
    /// their own, and none is left behind.
    #[test]
    fn concurrent_inserts() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FontCache::new(dir.path().join("fonts"));
        let font = &bundled()[0];
        let hash = FontHash::of(font.data());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| assert_eq!(cache.insert(font.data()).unwrap(), hash));
            }
        });
        let names: Vec<_> = fs::read_dir(cache.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(hash.hex())]);
        let meta = fs::metadata(cache.path(hash)).unwrap();
        assert!(written_by_insert(&meta, hash));
        assert_eq!(&**cache.get(hash).unwrap().unwrap().data(), &**font.data());
    }
}
