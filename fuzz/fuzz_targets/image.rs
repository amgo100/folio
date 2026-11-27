//! The `image` fuzz target: [`folio_testkit::fuzz::image`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::image(data));
