//! The manifest and lockfile as salsa inputs (SPEC §2.3).

// salsa generates the constructors, builders and setters of inputs.
#![allow(missing_docs)]

use super::{Lockfile, Manifest};

/// The project manifest as a salsa input (SPEC §2.3).
///
/// The host reads `folio.toml`, parses it with [`Manifest::parse`] and sets
/// the result; a manifest with errors stops the build before any query runs.
#[salsa::input(debug)]
pub struct ManifestInput {
    /// The parsed manifest.
    #[returns(ref)]
    pub manifest: Manifest,
}

/// The lockfile as a salsa input (SPEC §2.3, §2.4).
#[salsa::input(debug)]
pub struct LockfileInput {
    /// The parsed lockfile; [`Lockfile::default`] when the project has none.
    #[returns(ref)]
    pub lockfile: Lockfile,
}
