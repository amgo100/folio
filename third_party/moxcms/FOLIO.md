# moxcms 0.8.1, patched for Folio

This is the crates.io release of `moxcms` 0.8.1
(https://crates.io/crates/moxcms/0.8.1), unpacked as published, with Folio's
patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: BSD-3-Clause OR Apache-2.0
(LICENSE.md, LICENSE-APACHE.md).

Folio converts a PDF figure to SVG through `hayro-svg` where PDF/A or
PDF/UA forbids embedding the figure or its PDF version is newer than the
output's (Folio's SPEC §11.1), and `hayro-interpret` converts its
ICC-based and Lab colour, and DeviceCMYK, with this crate. The release
computes in ways that depend on the machine, which SPEC §2.4 forbids for
output: an aarch64 build, an x86-64 build on a CPU with SSE 4.1 or AVX2,
and one on a CPU without converted the same figure to different colours
(#312). Folio's changes make it compute the same way everywhere. Each
changed file carries a comment that starts with `Folio:`.

- `src/mlaf.rs`: `mlaf` is always `acc + a * b`, a rounded product and a
  rounded sum. The release fused the two on aarch64 and on x86-64 built
  with FMA, which rounds once.
- `src/lib.rs`, `Cargo.toml`: the default features are only `lut`, and the
  crate refuses to build with `avx`, `sse`, `avx512` or `neon`, which build
  the SIMD paths it picks by CPU at run time and which round otherwise
  than the scalar code. Folio builds only the scalar code.
- `src/folio_libm.rs` (new): the float functions the crate took from
  `pxfm` at run time, under pxfm's names, computed with the `libm` crate.
  Several of pxfm's functions pick an AVX+FMA or a generic path by CPU at
  run time and by target at compile time, and its `dirty_powf` is an
  approximation whose result depends on fused multiply-adds. `dirty_powf`
  becomes `libm::powf` of the magnitude with the release's sign rule, and
  `f_hypot3f` a square root in `f64`. `src/defaults.rs` still uses
  pxfm's `const fn`s, which the compiler evaluates the same on every host.
- `src/rgb.rs`, `src/lab.rs`, `src/srlab2.rs`, `src/gamma.rs`,
  `src/trc.rs`, `src/jzazbz.rs`, `src/jzczhz.rs`, `src/dt_ucs.rs`,
  `src/oklab.rs`, `src/oklch.rs`, `src/luv.rs`, `src/yrg.rs`: take those
  functions from `folio_libm` instead of `pxfm`.
- `src/rgb.rs`: `Rgb::cbrt` and `Rgb`'s `Pow` with `libm`, through a new
  public trait `LibmFloat` (for `f32` and `f64`), instead of std's through
  `num_traits::Float`.
- `Cargo.toml`: depends on `libm`.

The whole change is `third_party/patches/moxcms.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
