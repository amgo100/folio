# tiny-skia-path 0.12.0, patched for Folio

This is the crates.io release of `tiny-skia-path` 0.12.0
(https://crates.io/crates/tiny-skia-path/0.12.0), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: BSD-3-Clause (LICENSE).

Folio's changes make its floating-point code call the `libm` crate where it
called std's transcendental functions, whose results come from the
platform's maths library, so that SVG figures give the same PDF bytes on
every platform (Folio's SPEC §2.4, decision D-300-1). Each changed place
carries a comment that starts with `Folio:`.

- `src/transform.rs`: `Transform::from_rotate` (and so `pre_rotate` and
  `post_rotate`) with `libm::cosf` and `sinf`.
- `src/path_geometry.rs`: the three-root case of the cubic solver (used by
  the stroker and stroke bounds) with `libm::acosf` and `cosf`, and the
  cube root with `libm::powf`.
- `Cargo.toml`: `libm` is no longer optional, and the `no-std-float`
  feature no longer names it.

The whole change is `third_party/patches/tiny-skia-path.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
