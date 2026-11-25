//! The `incremental` fuzz target: [`folio_testkit::fuzz::incremental`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::incremental(data));
