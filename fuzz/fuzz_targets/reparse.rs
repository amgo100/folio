//! The `reparse` fuzz target: [`folio_testkit::fuzz::reparse`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::reparse(data));
