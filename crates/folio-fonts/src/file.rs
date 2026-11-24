//! A font file and where it came from.

use std::fmt::{self, Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use ecow::{EcoString, EcoVec};

use crate::data::FontData;
use crate::font::Font;
use crate::hash::FontHash;

/// Where a font file came from, which decides whether it pins a build
/// (SPEC §2.4).
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum FontOrigin {
    /// Bundled with the compiler; the compiler version pins it. Holds the
    /// file's name.
    Bundled(EcoString),
    /// In the project's font directory; the project pins it. Holds its path
    /// in the project, such as `fonts/Serif-Regular.otf`.
    Project(EcoString),
    /// From the immutable font cache, where fonts locked in `folio.lock`
    /// are kept under their hash.
    Cache,
    /// Installed on this machine; it pins nothing unless `folio.lock` locks
    /// it. Holds its path.
    System(EcoString),
}

impl Display for FontOrigin {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bundled(name) => write!(f, "the bundled font `{name}`"),
            Self::Project(path) => write!(f, "the project font `{path}`"),
            Self::Cache => f.write_str("the font cache"),
            Self::System(path) => write!(f, "the system font `{path}`"),
        }
    }
}

/// Why a font file has no faces.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontError(pub EcoString);

impl Display for FontError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FontError {}

/// The contents of a font file and where it came from. Cheap to clone: a
/// clone shares the faces, which are parsed once, on first use, however
/// many compiles use the file.
#[derive(Clone)]
pub struct FontFile(Arc<Inner>);

struct Inner {
    origin: FontOrigin,
    data: FontData,
    hash: OnceLock<FontHash>,
    faces: OnceLock<Result<EcoVec<Font>, FontError>>,
}

impl FontFile {
    /// A font file with its contents.
    pub fn new(origin: FontOrigin, data: impl Into<FontData>) -> Self {
        Self(Arc::new(Inner {
            origin,
            data: data.into(),
            hash: OnceLock::new(),
            faces: OnceLock::new(),
        }))
    }

    /// A font file whose hash is already known, as for one from the font
    /// cache, which checked it.
    pub(crate) fn with_hash(origin: FontOrigin, data: FontData, hash: FontHash) -> Self {
        let file = Self::new(origin, data);
        let _ = file.0.hash.set(hash);
        file
    }

    /// Where it came from.
    pub fn origin(&self) -> &FontOrigin {
        &self.0.origin
    }

    /// Its contents.
    pub fn data(&self) -> &FontData {
        &self.0.data
    }

    /// The SHA-256 hash of its contents, computed once.
    pub fn hash(&self) -> FontHash {
        *self.0.hash.get_or_init(|| FontHash::of(&self.0.data))
    }

    /// Its faces: one, or several for a collection (`.ttc`, `.otc`).
    /// Parsed once.
    pub fn faces(&self) -> Result<&[Font], &FontError> {
        self.0
            .faces
            .get_or_init(|| {
                let data = &self.0.data;
                let count = match skrifa::raw::FileRef::new(data) {
                    Ok(skrifa::raw::FileRef::Collection(collection)) => collection.len(),
                    _ => 1,
                };
                let faces: EcoVec<Font> = (0..count)
                    .filter_map(|i| Font::new(data.clone(), i))
                    .collect();
                if faces.is_empty() {
                    Err(FontError("it is not a font file Folio can read".into()))
                } else {
                    Ok(faces)
                }
            })
            .as_ref()
            .map(EcoVec::as_slice)
    }
}

impl PartialEq for FontFile {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.origin == other.0.origin && self.0.data == other.0.data)
    }
}

impl Eq for FontFile {}

impl Hash for FontFile {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.origin.hash(state);
        self.0.data.hash(state);
    }
}

impl Debug for FontFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontFile")
            .field("origin", &self.0.origin)
            .field("len", &self.0.data.len())
            .finish()
    }
}
