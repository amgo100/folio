//! File identifiers.

use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroU16;

/// Identifies a source file within a compilation.
///
/// Folio's file ids are opaque numbers: whoever owns the files (the
/// `folio::World` and its query database, SPEC §2.3) assigns them and maps
/// them back to paths. The syntax crate keeps no global interner, so the ids
/// a build assigns never depend on the order in which threads first touch a
/// file (SPEC §2.4).
///
/// The id `u16::MAX` is reserved for [detached](crate::Source::detached)
/// sources.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FileId(NonZeroU16);

impl FileId {
    /// The id given to detached sources, which belong to no project.
    pub const DETACHED: Self = Self(NonZeroU16::MAX);

    /// Create a file id from its raw number.
    pub const fn from_raw(v: NonZeroU16) -> Self {
        Self(v)
    }

    /// The raw number of this file id.
    pub const fn into_raw(self) -> NonZeroU16 {
        self.0
    }
}

impl Debug for FileId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "FileId({})", self.0)
    }
}
