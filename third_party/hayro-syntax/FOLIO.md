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

The whole change is `third_party/patches/hayro-syntax.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
