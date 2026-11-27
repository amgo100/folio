//! Fonts for Folio (SPEC §2.5): the bundled families, discovery and
//! loading, metrics, coverage, embedding permissions, the immutable font
//! cache, and family lookup.
//!
//! - **Files and faces**: a [`FontFile`] is the contents of one font file
//!   and where it came from ([`FontOrigin`]); it parses into its [`Font`]
//!   faces once, on first use, however many compiles share it. A face knows
//!   its names, style ([`FontVariant`]), [`Metrics`], [`Coverage`] and
//!   [`Embedding`] permissions.
//! - **Bundled families** ([`bundled`]): New Computer Modern (text and
//!   math), Source Serif 4, Source Sans 3, Source Code Pro and STIX Two
//!   (text and math), compiled into the binary and shared by every compile
//!   of the process.
//! - **Project and system fonts** ([`project_fonts`], [`system_fonts`]):
//!   files that can change are read into memory, never mapped, and each
//!   file is read once per process while it is unchanged. System fonts are
//!   discovered with `fontdb` and only read when a document uses them.
//! - **The font cache** ([`FontCache`]): fonts locked by hash in
//!   `folio.lock` are stored under their hash. The files the cache wrote
//!   itself are written once and never changed, so they are the only files
//!   that are memory-mapped (SPEC §2.5, §14.2); a file copied into the
//!   cache by hand is read into memory.
//! - **Lookup** ([`FontBook`]): faces by family name and the variant
//!   closest to a requested weight, style and stretch.
//!
//! This is the `folio-fonts` crate of SPEC §2.2.

#![deny(unsafe_code)]

mod book;
mod bundled;
mod cache;
mod data;
mod discover;
mod file;
mod font;
mod hash;

pub use self::book::{FaceKey, FontBook};
pub use self::bundled::{BUNDLED_FAMILIES, bundled};
pub use self::cache::{FontCache, cache_root};
pub use self::data::FontData;
pub use self::discover::{
    FONT_EXTENSIONS, PROJECT_FONT_DIR, SystemFont, is_font_path, project_fonts, system_fonts,
};
pub use self::file::{FontError, FontFile, FontOrigin};
pub use self::font::{
    Coverage, Embedding, Font, FontInfo, FontStyle, FontVariant, Metrics, Permission,
};
pub use self::hash::FontHash;
