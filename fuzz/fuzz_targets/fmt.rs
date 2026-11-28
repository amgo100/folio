//! The `fmt` fuzz target: [`folio_testkit::fuzz::fmt`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::fmt(data));
