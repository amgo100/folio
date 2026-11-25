//! The `compile` fuzz target: [`folio_testkit::fuzz::compile`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::compile(data));
