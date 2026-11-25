//! The `lockfile` fuzz target: [`folio_testkit::fuzz::lockfile`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::lockfile(data));
