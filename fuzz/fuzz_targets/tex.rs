//! The `tex` fuzz target: [`folio_testkit::fuzz::tex`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::tex(data));
