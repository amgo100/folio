//! The `build` fuzz target: [`folio_testkit::fuzz::build`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::build(data));
