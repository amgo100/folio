//! The `wire` fuzz target: [`folio_testkit::fuzz::wire`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::wire(data));
