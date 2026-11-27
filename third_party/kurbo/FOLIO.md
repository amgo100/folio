# kurbo 0.13.1, patched for Folio

This is the crates.io release of `kurbo` 0.13.1
(https://crates.io/crates/kurbo/0.13.1), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: Apache-2.0 OR MIT (LICENSE-APACHE, LICENSE-MIT).

Folio's changes make its floating-point code call the `libm` crate where it
called std's transcendental functions, whose results come from the
platform's maths library, so that SVG figures give the same PDF bytes on
every platform (Folio's SPEC §2.4, decision D-300-1). Each changed file
carries a comment that starts with `Folio:`.

- `src/common.rs`: a crate-private trait, `LibmFloat`, whose `lm_` methods
  (`lm_sin`, `lm_atan2`, `lm_powf`, …) compute with `libm`; `lm_powi`
  multiplies by squaring.
- Every call of a transcendental `f32`/`f64` method outside tests and
  comments (`sin`, `cos`, `tan`, `sin_cos`, `acos`, `atan2`, `hypot`, `ln`,
  `log2`, `cbrt`, `powf`, `powi`; 126 calls in 17 files, `common.rs` among
  them) calls the `lm_` method instead, and each of those files imports the
  trait.
- `Cargo.toml`: `libm` is no longer optional; the `libm` feature only
  forwards to `polycool` and `euclid`. `src/lib.rs` drops the import that
  silenced the unused-dependency lint when both `std` and `libm` were on.

The `std` feature still links std. Whether a crate loads std, even through
a dependency, decides whether std's float methods are in scope, so kurbo's
own `libm` mode (`FloatFuncs`, used without `std`) cannot be forced; the
calls are renamed instead.

The whole change is `third_party/patches/kurbo.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
