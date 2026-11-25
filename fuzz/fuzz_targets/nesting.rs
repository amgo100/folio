//! The `nesting` fuzz target: [`folio_testkit::fuzz::nesting`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::nesting(data));
