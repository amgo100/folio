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
  the default range of indexed colour with `libm::powf`; the cubes of the
  L* decode in CalRGB's black-point compensation multiplied out instead of
  `powi`. Lab and ICC-based colour, and DeviceCMYK, go through `moxcms`,
  which Folio patches on its own (#312).
- `src/shading.rs`: the Bernstein polynomials of patch meshes multiplied
  out, and the largest coordinate and component values of mesh data with
  `libm::ldexpf` (exact powers of two) instead of `powi`.
- `src/x_object.rs`: the largest component value of image data with
  `libm::ldexpf` instead of `powi`.
- `Cargo.toml`: depends on `libm`, and on `moxcms` without its default
  features, which build the SIMD paths moxcms picks by CPU at run time
  (#312; Folio's own copy of moxcms refuses to build them).

Folio also reads PDF figures from untrusted files (Folio's SPEC §14.3,
#333). These changes limit the work, nesting and image memory one
interpretation of a page may take (decompressed streams are not limited
yet, Folio #376), and report each limit reached through the settings'
warning sink, so that Folio can refuse the figure:

- `src/cache.rs`: the object cache, which every nested context of one
  interpretation shares, carries a `Budget`: the work done (an operator
  is one unit, a form XObject, pattern, soft mask, Type 3 glyph or image
  drawing 50) up
  to `MAX_WORK` (10,000,000), the bytes of the images decoded up to
  `hayro_syntax::MAX_IMAGE_BYTES` (an image counts once, however often it
  is drawn: an image object by its id, an inline image by its dictionary
  and data; one refused takes nothing; every decoding also spends a unit
  of work a kilobyte).
- `src/interpret/mod.rs`: three new warnings, `FormCycle`, `TooComplex`
  and `ImageTooLarge`; `interpret` spends a unit of work for each
  operator and stops when the work is used up.
- `src/context.rs`: a nested context and a form XObject spend work; each
  context keeps the form XObjects it is drawing (`forms`).
- `src/x_object.rs`: a form XObject that its context is already drawing,
  one that draws itself, is not drawn again (`FormCycle`; a pattern or
  soft mask starts a context of its own, whose depth hayro limits); before an image is
  decoded, its size at four bytes a pixel or more (its components and bits
  per component) is taken from what is left of `MAX_IMAGE_BYTES`, and an
  image that does not fit is not drawn (`ImageTooLarge`, also sent when
  `hayro-syntax` refuses an image by its own header); an image whose data
  gives it more pixels than its dictionary takes the rest after decoding.

The whole change is `third_party/patches/hayro-interpret.patch` in Folio's repository;
`scripts/patch-third-party.sh` rebuilds this directory from the release and
the patch, and `third_party/README.md` says how to move to a new release.
