//! The `document` fuzz target: [`folio_testkit::fuzz::document`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::document(data));
