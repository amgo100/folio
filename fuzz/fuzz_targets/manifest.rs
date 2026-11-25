//! The `manifest` fuzz target: [`folio_testkit::fuzz::manifest`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::manifest(data));
