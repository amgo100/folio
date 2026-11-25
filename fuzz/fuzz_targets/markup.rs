//! The `markup` fuzz target: [`folio_testkit::fuzz::markup`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::markup(data));
