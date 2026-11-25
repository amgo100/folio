//! The `stylesheet` fuzz target: [`folio_testkit::fuzz::stylesheet`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::stylesheet(data));
