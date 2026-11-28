# Third-party code

This directory holds what Folio takes from other projects.

- `typst/`: the licence and notice of Typst, from which `folio-syntax` and
  `folio-math` derive (see `NOTICE` at the repository root).
- `hayro-interpret/`, `hayro-jpeg2000/`, `hayro-syntax/`, `kurbo/`,
  `moxcms/`, `resvg/`, `svgtypes/`, `tiny-skia/`, `tiny-skia-path/`,
  `usvg/`: patched copies of crates.io
  releases, which the workspace uses in place of the releases through
  `[patch.crates-io]` in the root `Cargo.toml` (and in `fuzz/Cargo.toml`).

## The patched crates

SVG figures reach PDF through `usvg` (which parses them with `svgtypes`
and draws arcs with `kurbo`), `tiny-skia-path`, and, for filters, `resvg`
and `tiny-skia` (SPEC §11.1). Their floating-point code called std's
transcendental functions (`sin`, `atan2`, `powf`, …), whose results come
from the platform's maths library: an arrowhead drawn by an SVG marker got
a rotation one unit in the last place apart on aarch64 and x86-64, and so
other PDF bytes (#300). SPEC §2.4 forbids that, and decision D-300-1 chose
to patch the crates: each copy calls the `libm` crate instead, as Folio's
own code does (`clippy.toml`). Nothing changes for a document beyond the
last bits of these numbers.

PDF figures that Folio converts to SVG (under PDF/A or PDF/UA, or when
their PDF version is newer than the output's; SPEC §11.1) are read by
`hayro-interpret` and `hayro-syntax`, which `hayro-svg` turns into SVG.
Their calibrated colour (CalGray, CalRGB), shadings and PostScript
calculator functions called the same functions, and are patched the same
way (#304). `tests/corpus/figures/assets/spectrum.pdf`, written by
`spectrum.py` beside it, exercises them.

`hayro-interpret` converts ICC-based and Lab colour, and DeviceCMYK through
its CMYK profile, with `moxcms`, and `hayro-syntax` decodes JPEG 2000
images with `hayro-jpeg2000`. Both fused multiply-adds on some targets only
(aarch64 always, x86-64 when built with FMA) and picked SIMD paths by CPU
at run time, and moxcms took powers and cube roots from `pxfm`, which also
picks a path by CPU: the same figure converted to three different SVGs on
aarch64, on x86-64 with SSE 4.1 or later, and on baseline x86-64 (#312).
Their copies always round a product and a sum apart, as baseline x86-64
did, refuse to build their SIMD features (moxcms's `avx`, `sse`, `avx512`
and `neon`, hayro-jpeg2000's `simd`; the patched `hayro-interpret` and
`hayro-syntax` manifests no longer ask for them), and moxcms computes
pxfm's functions with `libm`. `crates/folio-pdf/tests/data/colour.pdf`,
written by `colour.py` beside it, runs every 8-bit level through these
paths, and `cargo test -p folio-pdf --test colour` compares a digest of
its conversion with the one recorded on x86-64. It passes unchanged under
QEMU on aarch64 and on x86-64 CPUs without AVX2 or SSE 4.1 (`qemu64`,
`Nehalem`); before the patch, each of the three gave its own digest.
`pxfm` itself is not patched: moxcms keeps only its `const fn`s, which the
compiler evaluates the same on every host.

PDF figures come from files Folio does not trust (SPEC §14.3), and the two
hayro crates are patched for that too (#333): `hayro-syntax` reads arrays
and dictionaries at most 256 deep and decodes no JPEG 2000, JBIG2 or CCITT image larger than
`hayro_syntax::MAX_IMAGE_BYTES` (256 MiB), and `hayro-interpret` does not
draw a form XObject inside itself, stops a page after a fixed amount of
work, and decodes images only up to `MAX_IMAGE_BYTES` for a page, sending a
warning for each limit reached. Folio's image reader
(`folio_core::image::read_image`) converts every PDF figure's page once
and refuses the figure on those warnings, so that `folio check` and
`folio build` give the same error.

Each copy is the release exactly as published on crates.io, with its
licence files, plus Folio's patch. Its `FOLIO.md` says what the patch
changes, and every changed file carries a comment that starts with
`Folio:`.

| Crate | Version | Licence | Patch |
|---|---|---|---|
| hayro-interpret | 0.7.0 | Apache-2.0 OR MIT | `patches/hayro-interpret.patch` |
| hayro-jpeg2000 | 0.3.5 | Apache-2.0 OR MIT | `patches/hayro-jpeg2000.patch` |
| hayro-syntax | 0.7.2 | Apache-2.0 OR MIT | `patches/hayro-syntax.patch` |
| kurbo | 0.13.1 | Apache-2.0 OR MIT | `patches/kurbo.patch` |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | `patches/moxcms.patch` |
| resvg | 0.47.0 | Apache-2.0 OR MIT | `patches/resvg.patch` |
| svgtypes | 0.16.2 | Apache-2.0 OR MIT | `patches/svgtypes.patch` |
| tiny-skia | 0.12.0 | BSD-3-Clause | `patches/tiny-skia.patch` |
| tiny-skia-path | 0.12.0 | BSD-3-Clause | `patches/tiny-skia-path.patch` |
| usvg | 0.47.0 | Apache-2.0 OR MIT | `patches/usvg.patch` |

`crates.txt` lists them with the SHA-256 of each release's `.crate` file,
the checksum `Cargo.lock` recorded for it before the patch.
`cargo test -p folio-pdf --test third_party` fails when a copy calls one of
the float functions `clippy.toml` disallows outside comments and tests,
when either workspace stops using a copy, and when an arrowhead's rotation
or a calculator function's colour in `spectrum.pdf` is not the one `libm`
gives; `cargo test -p folio-pdf --test colour` fails when moxcms or
hayro-jpeg2000 converts `colour.pdf` otherwise than recorded.

`polycool`, which kurbo uses to find polynomial roots, is not patched: its
only such calls outside its tests are `2.0f64.powi(-515)`, a power of two,
which every way of computing `powi` gives exactly.

Still unpatched, and outside what the corpus uses: krilla's sweep
gradients (one `atan2`; Folio draws none). `zune-jpeg`, which decodes
JPEG images in PDF figures, and `hayro-jbig2` keep their SIMD paths, which
compute in integers only.

The copies are not workspace members (`exclude` in the root `Cargo.toml`):
they keep their own lints, and `cargo test --workspace` does not run their
tests.

### Checking and rebuilding the copies

```bash
scripts/patch-third-party.sh --check   # every copy is its release plus its patch
scripts/patch-third-party.sh           # rebuild every copy from crates.io
```

### Moving to a new release

1. Change the crate's version and checksum in `crates.txt`, and its
   version in the root `Cargo.toml` (and in the crates that depend on it).
   The checksum is the one `Cargo.lock` records for the release; take it
   from a lockfile without the patch, or from
   `curl -sL https://static.crates.io/crates/<name>/<name>-<version>.crate | sha256sum`.
2. Run `scripts/patch-third-party.sh`. Where a hunk no longer applies,
   `patch` stops; make the change in `third_party/<crate>/` by hand, and
   look for new calls of the disallowed functions.
3. Run `scripts/patch-third-party.sh --diff <crate>` to write the patch
   again, update the crate's `FOLIO.md` and the table above, and run
   `cargo test -p folio-pdf --test third_party` and `scripts/check.sh`.
4. Rebuild the corpus on aarch64 as `tests/README.md` describes, to check
   that the digests still match x86-64, and run
   `cargo test -p folio-pdf --test colour` there and on x86-64 without
   AVX2 (below).

### Checking on other CPUs

`cargo test -p folio-pdf --test colour` must pass unchanged on aarch64 and
on x86-64 CPUs without AVX2. On x86-64 Linux, QEMU's user-mode emulation
runs it on both (the packages and target of `tests/README.md`'s aarch64
check):

```bash
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
cargo test -p folio-pdf --test colour --no-run --target aarch64-unknown-linux-gnu
QEMU_LD_PREFIX=/usr/aarch64-linux-gnu qemu-aarch64 \
    target/aarch64-unknown-linux-gnu/debug/deps/colour-<hash>
cargo test -p folio-pdf --test colour --no-run
for cpu in qemu64 Nehalem Haswell; do
    qemu-x86_64 -cpu $cpu target/debug/deps/colour-<hash>
done
```

`cargo test --no-run` prints the path of each test binary.

When upstream gives these crates a way to use `libm` with std, the copies
can go.
