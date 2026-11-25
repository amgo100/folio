//! Folio's test infrastructure (SPEC §14): the shared test corpus, layout
//! golden files, the fuzz harnesses, the reference document generator and
//! what the benchmarks build.
//!
//! - [`corpus`] reads the projects of `tests/corpus`, which the golden,
//!   PDF, determinism and command-line tests all build.
//! - [`golden`] compares output with the files in `tests/golden`, and
//!   writes them when `FOLIO_BLESS` is set. Layout goldens are fragment
//!   dumps ([`golden::dump_pages`]) until the rasteriser of M1 lets them be
//!   page images compared with a perceptual threshold.
//! - [`fuzz`] holds the bodies of the `cargo fuzz` targets in `fuzz/`, so
//!   that the inputs that once failed are replayed on stable Rust by
//!   `cargo test` (`tests/fuzz.rs`).
//! - [`refdoc`] generates the reference document of SPEC §14.1 and smaller
//!   documents of the same make-up.
//!
//! The benchmarks are in `benches/compile.rs` (instruction counts, the gate
//! of SPEC §14.1) and `src/bin/folio-wall.rs` (wall-clock times). The
//! scripts in `scripts/` run them; `tests/README.md` describes the whole
//! layout.

#![forbid(unsafe_code)]

pub mod corpus;
pub mod fuzz;
pub mod golden;
pub mod refdoc;

use std::path::{Path, PathBuf};

/// The repository's root directory.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
