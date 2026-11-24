//! The Folio compiler library: the memoised compile pipeline
//! (`folio::compile(world, target, variant)`) and the `folio::World` that
//! supplies sources, fonts, files and components (SPEC §2.2).

#![forbid(unsafe_code)]

/// The version of this compiler, which `folio.toml` pins for byte-identical
/// output (SPEC §2.4).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
