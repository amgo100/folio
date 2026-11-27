# Third-party code

This directory holds what Folio takes from other projects.

- `typst/`: the licence and notice of Typst, from which `folio-syntax` and
  `folio-math` derive (see `NOTICE` at the repository root).
- `kurbo/`, `resvg/`, `svgtypes/`, `tiny-skia/`, `tiny-skia-path/`, `usvg/`:
  patched copies of crates.io releases, which the workspace uses in place
  of the releases through `[patch.crates-io]` in the root `Cargo.toml` (and
  in `fuzz/Cargo.toml`).

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

Each copy is the release exactly as published on crates.io, with its
licence files, plus Folio's patch. Its `FOLIO.md` says what the patch
changes, and every changed file carries a comment that starts with
`Folio:`.

| Crate | Version | Licence | Patch |
|---|---|---|---|
| kurbo | 0.13.1 | Apache-2.0 OR MIT | `patches/kurbo.patch` |
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
is not the one `libm` gives.

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
   that the digests still match x86-64.

When upstream gives these crates a way to use `libm` with std, the copies
can go.
