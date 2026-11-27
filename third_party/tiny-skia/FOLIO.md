# tiny-skia 0.12.0, patched for Folio

This is the crates.io release of `tiny-skia` 0.12.0
(https://crates.io/crates/tiny-skia/0.12.0), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: BSD-3-Clause (LICENSE).

Folio's changes make its floating-point code call the `libm` crate where it
called std's transcendental functions, whose results come from the
platform's maths library, so that SVG figures give the same PDF bytes on
every platform (Folio's SPEC §2.4, decision D-300-1). Each changed place
carries a comment that starts with `Folio:`.

- `src/path64/cubic64.rs`: the three-root case of the cubic solver with
  `libm::acos` and `cos`.
- `src/pipeline/highp.rs`: the `color-burn` and `color-dodge` blend modes
  divide exactly instead of calling `recip_fast`, whose SSE (`rcpps`) and
  NEON (`vrecpe`) approximations give different bits (and `rcpps` differs
  between x86 vendors).
- `Cargo.toml`: depends on `libm`.

`f32x8::powf` in `highp.rs` (the sRGB stages) is tiny-skia's own
polynomial approximation in IEEE operations, so it stays.

The whole change is `third_party/patches/tiny-skia.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
