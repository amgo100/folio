# hayro-interpret 0.7.0, patched for Folio

This is the crates.io release of `hayro-interpret` 0.7.0
(https://crates.io/crates/hayro-interpret/0.7.0), unpacked as published, with Folio's
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

- `src/function/type4.rs`: the PostScript calculator operators `atan`,
  `cos`, `sin`, `exp`, `ln` and `log` with `libm` (`atan2f`, `cosf`, `sinf`,
  `powf`, `logf`, `log10f`).
- `src/function/type2.rs`: exponential functions with `libm::powf`.
- `src/color.rs`: CalGray and CalRGB gamma, the sRGB transfer function and
  the default range of indexed colour with `libm::powf`; the Lab decode's
  cubes multiplied out instead of `powi`.
- `src/shading.rs`: the Bernstein polynomials of patch meshes multiplied
  out, and the largest coordinate and component values of mesh data with
  `libm::ldexpf` (exact powers of two) instead of `powi`.
- `src/x_object.rs`: the largest component value of image data with
  `libm::ldexpf` instead of `powi`.
- `Cargo.toml`: depends on `libm`.

The whole change is `third_party/patches/hayro-interpret.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
