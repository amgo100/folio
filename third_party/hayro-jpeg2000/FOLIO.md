# hayro-jpeg2000 0.3.5, patched for Folio

This is the crates.io release of `hayro-jpeg2000` 0.3.5
(https://crates.io/crates/hayro-jpeg2000/0.3.5), unpacked as published, with
Folio's patch applied. Folio uses it through `[patch.crates-io]` in its root
`Cargo.toml`. Its licence is unchanged: Apache-2.0 OR MIT (LICENSE-APACHE,
LICENSE-MIT).

`hayro-syntax` decodes JPEG 2000 images (`JPXDecode`) with this crate when
Folio reads a PDF figure, and converts it to SVG where PDF/A or PDF/UA
forbids embedding it (Folio's SPEC §11.1). The release's irreversible
wavelet and colour transforms computed in ways that depend on the machine,
which SPEC §2.4 forbids for output (#312). Folio's changes make them compute
the same way everywhere. Each changed file carries a comment that starts
with `Folio:`.

- `src/math.rs`: `mul_add` is always `a * b + c`, a rounded product and a
  rounded sum. The release fused the two on aarch64 and on x86-64 built
  with FMA, which rounds once.
- `src/lib.rs`, `Cargo.toml`: `simd` is no longer a default feature, and
  the crate refuses to build with it. Its paths, which `fearless_simd`
  picks by CPU at run time, fuse multiply-adds on some CPUs only. Folio
  builds only the scalar code.

The whole change is `third_party/patches/hayro-jpeg2000.patch` in Folio's
repository; `scripts/patch-third-party.sh` rebuilds this directory from the
release and the patch, and `third_party/README.md` says how to move to a new
release.
