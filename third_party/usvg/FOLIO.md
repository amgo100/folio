# usvg 0.47.0, patched for Folio

This is the crates.io release of `usvg` 0.47.0
(https://crates.io/crates/usvg/0.47.0), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: Apache-2.0 OR MIT (LICENSE-APACHE, LICENSE-MIT).

Folio's changes make its floating-point code call the `libm` crate where it
called std's transcendental functions, whose results come from the
platform's maths library, so that SVG figures give the same PDF bytes on
every platform (Folio's SPEC §2.4, decision D-300-1). Each changed place
carries a comment that starts with `Folio:`.

- `src/parser/marker.rs`: the angle of markers with `orient="auto"` (and
  `auto-start-reverse`) with `libm::atan2f`. This is the call that gave
  arrowheads other rotations on aarch64 than on x86-64 (#300).
- `src/parser/units.rs`: the diagonal for percentages and the named font
  sizes multiply out instead of calling `powi`.
- `Cargo.toml`: depends on `libm`.

The whole change is `third_party/patches/usvg.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
