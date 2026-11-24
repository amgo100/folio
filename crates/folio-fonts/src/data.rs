//! The bytes of a font file.

use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// The contents of a font file, cheap to clone: compiled into the binary,
/// read into memory, or mapped from the immutable font cache.
#[derive(Clone)]
pub struct FontData(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl FontData {
    /// Data compiled into the binary.
    pub fn from_static(data: &'static [u8]) -> Self {
        Self(Arc::new(data))
    }

    /// Data held by something else, such as a memory map.
    pub(crate) fn from_owner(owner: impl AsRef<[u8]> + Send + Sync + 'static) -> Self {
        Self(Arc::new(owner))
    }
}

impl Deref for FontData {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl AsRef<[u8]> for FontData {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl From<Vec<u8>> for FontData {
    fn from(data: Vec<u8>) -> Self {
        Self(Arc::new(data))
    }
}

impl From<&[u8]> for FontData {
    fn from(data: &[u8]) -> Self {
        Self::from(data.to_vec())
    }
}

impl PartialEq for FontData {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || **self == **other
    }
}

impl Eq for FontData {}

/// Hashes the length only: font files are large, and two different files
/// of one length are rare enough for the hash tables of the query database.
impl Hash for FontData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len().hash(state);
    }
}

impl Debug for FontData {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "FontData({} bytes)", self.len())
    }
}
