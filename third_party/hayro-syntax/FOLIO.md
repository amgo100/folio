# hayro-syntax 0.7.2, patched for Folio

This is the crates.io release of `hayro-syntax` 0.7.2
(https://crates.io/crates/hayro-syntax/0.7.2), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: Apache-2.0 OR MIT (LICENSE-APACHE,
LICENSE-MIT).

Folio converts a PDF figure to SVG through `hayro-svg`, which reads it with
this crate, where PDF/A or PDF/UA forbids embedding the figure or its PDF
version is newer than the output's (Folio's SPEC §11.1). Folio's changes
make the crate's floating-point code call the `libm` crate where it called
std's transcendental functions, whose results come from the platform's
maths library, so that such figures give the same PDF bytes on every
platform (SPEC §2.4, decisions D-300-1 and #304). Each changed file carries
a comment that starts with `Folio:`.

- `src/math.rs`: `powi_f64` always multiplies by squaring, as it already
  did without the `std` feature, instead of calling `powi`, whose result
  the language leaves unspecified.
- `Cargo.toml`: the `unsafe` feature no longer turns on `hayro-jpeg2000`'s
  `simd` feature, whose paths fearless_simd picks by CPU at run time and
  which fuse multiply-adds on some CPUs only (#312; Folio's own copy of
  hayro-jpeg2000 refuses to build them).

Folio also reads PDF figures from untrusted files (Folio's SPEC §14.3,
#333). These changes bound what reading one may take:

- `src/lib.rs`: `MAX_NESTING` (256), the deepest arrays and dictionaries
  are read, and `MAX_IMAGE_BYTES` (256 MiB, 8,192 × 8,192 pixels at four
  bytes), the most memory the images of one interpretation may take
  decoded, which `hayro-interpret` and Folio's image reader apply.
- `src/byte_reader.rs`: the reader counts how many arrays and dictionaries
  it is inside, and `Reader::nest` refuses one more past `MAX_NESTING`, so
  that 100,000 nested `[` no longer overflow the stack.
- `src/object/array.rs`, `src/object/dict.rs`: skipping an array or a
  dictionary, and reading a dictionary whole, go through `Reader::nest`.
- `src/object/array.rs`: an array item that cannot be read ends the
  iteration instead of panicking (an input found by Folio's `image` fuzz
  target, through `hayro-write`).
- `src/filter/jpx.rs`, `src/filter/jbig2.rs`, `src/filter/ccitt.rs`,
  `src/filter/mod.rs`, `src/object/stream.rs`: an image whose own header
  (JPEG 2000, JBIG2) or decode parameters (CCITT) say it would take more
  than `MAX_IMAGE_BYTES` decoded, at four bytes a pixel or more, is not
  decoded, and fails with a new `DecodeFailure::ImageTooLarge`; its size
  there need not match the image dictionary's, which `hayro-interpret`
  checks.
- `src/object/dict.rs`: `Dict::entries` leaves out an entry that cannot be
  read instead of panicking, as the array iterator does.

The whole change is `third_party/patches/hayro-syntax.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
