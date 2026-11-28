//! ICC-based, Lab and CMYK colour and JPEG 2000 images in PDF figures
//! converted to SVG, the same on every machine (SPEC §2.4, §11.1, #312).
//!
//! `hayro-interpret` converts ICC-based and Lab colour, and DeviceCMYK
//! through its CMYK profile, with `moxcms`, and `hayro-syntax` decodes JPEG
//! 2000 with `hayro-jpeg2000`. Upstream, both fuse multiply-adds on some
//! targets only and pick SIMD paths by CPU at run time, and moxcms took
//! powers and cube roots from `pxfm`, which also dispatches by CPU, so the
//! same figure could convert to other colours on another machine. Folio
//! builds patched copies (`third_party/README.md`) that compute the same
//! way everywhere. This test converts `data/colour.pdf` (written by
//! `data/colour.py`), which runs every 8-bit level through those paths, and
//! compares a digest of the SVG with the one recorded here, on x86-64 with
//! AVX2 and FMA. It must pass unchanged on x86-64 without them and on
//! aarch64: `third_party/README.md` says how to run it there under QEMU.

use std::fs;
use std::path::Path;
use std::sync::Arc;

/// The FNV-1a digest of the SVG that `data/colour.pdf` converts to.
const DIGEST: u64 = 0xa466_ede9_fa0d_8a1c;

/// The 64-bit FNV-1a hash: enough to tell two conversions apart, and the
/// test needs no hashing crate.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn convert() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/colour.pdf");
    let pdf = hayro_svg::hayro_syntax::Pdf::new(Arc::new(fs::read(path).unwrap())).unwrap();
    let page = &pdf.pages()[0];
    hayro_svg::convert(
        page,
        &hayro_svg::RenderCache::new(),
        &hayro_svg::hayro_interpret::InterpreterSettings::default(),
        &hayro_svg::SvgRenderSettings::default(),
    )
}

#[test]
fn colour_converts_the_same_everywhere() {
    let svg = convert();
    // Every swatch and image made it: the digest below is of a full page.
    let images = svg.matches("<image").count();
    assert_eq!(images, 6, "the six images of colour.pdf:\n{svg}");
    let digest = fnv1a(svg.as_bytes());
    assert_eq!(
        digest, DIGEST,
        "colour.pdf converts to an SVG with digest {digest:#018x}, not \
         {DIGEST:#018x}: moxcms or hayro-jpeg2000 computes otherwise than on \
         the machine that recorded it (SPEC §2.4, #312). If the change was \
         meant, record the new digest and check it on aarch64 and on x86-64 \
         without AVX2 (third_party/README.md)"
    );
}
