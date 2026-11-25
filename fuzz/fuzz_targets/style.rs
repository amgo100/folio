//! The `style` fuzz target: [`folio_testkit::fuzz::style`].

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| folio_testkit::fuzz::style(data));
