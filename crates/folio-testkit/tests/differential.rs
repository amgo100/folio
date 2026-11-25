//! The differential test of incremental compilation (SPEC §2.3, §14.2):
//! random sequences of edits to the content and stylesheets of the corpus
//! projects, applied in place to one world, after each of which every
//! build must have the outputs and diagnostics of a world built afresh
//! from the same texts ([`folio_testkit::fuzz::Edits::check`]).
//!
//! It catches results that outlive their inputs: in the memoised queries,
//! and in the shaping, line-break and page caches, which live outside
//! salsa's dependency tracking. The `differential` fuzz target
//! (`scripts/fuzz.sh`) explores further from the same harness, and the
//! fuzz test replays its seed corpus.
//!
//! `FOLIO_DIFFERENTIAL_CASES` sets how many random sequences run (default
//! 12) and `FOLIO_DIFFERENTIAL_SEED` the first seed (default 0), so that a
//! longer run can explore further and a failing sequence can be rerun
//! alone.

use std::env;

use folio_testkit::fuzz;

/// A number from the environment, or a default.
fn var(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name} must be a number"))
        })
        .unwrap_or(default)
}

#[test]
fn random_edits_compile_as_a_fresh_world_does() {
    // The seed corpus of the fuzz target uses the first seeds; start past
    // them, so that this test checks other sequences.
    let first = var("FOLIO_DIFFERENTIAL_SEED", fuzz::DIFFERENTIAL_SEEDS);
    let cases = var("FOLIO_DIFFERENTIAL_CASES", 12);
    for seed in first..first + cases {
        let input = fuzz::differential_input(seed);
        if let Err(panic) = std::panic::catch_unwind(|| fuzz::differential(&input)) {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("");
            panic!(
                "seed {seed} fails (rerun with FOLIO_DIFFERENTIAL_SEED={seed} \
                 FOLIO_DIFFERENTIAL_CASES=1): {message}"
            );
        }
    }
}
