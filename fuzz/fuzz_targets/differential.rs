//! The `differential` fuzz target: [`folio_testkit::fuzz::differential`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::differential(data));
