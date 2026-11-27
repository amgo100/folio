//! Images (SPEC §7.2, §11.1): the file an `image` element names, read as
//! an [`Image`] of its format, with the size it has of itself.
//!
//! - **Formats**: JPEG, PNG, WebP and GIF (its first frame), SVG, and PDF
//!   (its first page), told by the file's first bytes, not its name.
//! - **Natural size**: a raster image is as large as its pixels at 96 per
//!   inch (a CSS pixel, ¾ pt), an SVG drawing as its `width` and `height`
//!   (or its `viewBox`) in CSS pixels, a PDF page as its crop box,
//!   turned as the page is. Layout scales it from there (SPEC §7.2).
//!
//! Each file is read once per content: [`load_image`] is a query on the
//! file, so an edit elsewhere reads no image again.

use std::collections::HashMap;
use std::sync::Arc;

use ecow::{EcoString, eco_format};
use folio_component::{Abs, Image, ImageFormat, Size};

use crate::db::{DataFile, Db};

/// How many points a CSS pixel is: 96 pixels to the inch.
pub const PIXEL: f64 = 0.75;

/// An image file read: the image, and the size it has of itself.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LoadedImage {
    /// The image.
    pub image: Image,
    /// Its natural size (see the module's documentation).
    pub size: Size,
    /// Whether it is an SVG drawing with text, which is not drawn: no
    /// fonts are given to SVG drawings, so that output does not depend on
    /// the machine's fonts.
    pub text: bool,
}

/// The image in a file, or why it cannot be read: a format Folio does not
/// read, or a file of a format it reads that is broken.
#[salsa::tracked(returns(ref))]
pub fn load_image(db: &dyn Db, file: DataFile) -> Result<LoadedImage, EcoString> {
    let bytes = file.bytes(db);
    read_image(bytes.shared())
}

/// The most pixels an image may have: 8,192 × 8,192. A larger one is
/// refused before it is decoded, as decoding it could take more memory than
/// the machine has.
pub const MAX_PIXELS: u64 = 8_192 * 8_192;

/// The most memory the rasters of one image may take decoded: 256 MB,
/// 8,192 × 8,192 pixels at four bytes each (RGBA). A pixel takes four bytes
/// or more: a 16-bit RGBA PNG takes eight. The rasters an SVG or PDF figure
/// holds count together, so that a figure cannot bring in a raster larger
/// than a raster file may be. It is the limit the PDF reader applies to the
/// images it decodes (`hayro_syntax::MAX_IMAGE_BYTES`).
pub const MAX_IMAGE_BYTES: u64 = MAX_PIXELS * 4;

/// The longest side of an image's natural size, in points: the most any
/// length may be (SPEC §5.1), so that layout and the PDF stay in range.
pub const MAX_SIDE: f64 = 1_000_000.0;

/// The image in a file's bytes, as [`load_image`] reads it. A raster image
/// is decoded once here, so that a file the PDF writer and the renderer
/// could not decode is reported by every build, `folio check` included.
pub fn read_image(bytes: Arc<[u8]>) -> Result<LoadedImage, EcoString> {
    let Some(format) = ImageFormat::detect(&bytes) else {
        return Err(
            "this file is not an image Folio can read: JPEG, PNG, WebP, GIF, SVG or PDF".into(),
        );
    };
    let broken = |why: &dyn std::fmt::Display| -> EcoString {
        eco_format!("this {} file cannot be read: {why}", format.name())
    };
    let mut text = false;
    let (width, height) = match format {
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Webp | ImageFormat::Gif => {
            let size = imagesize::blob_size(&bytes).map_err(|e| broken(&e))?;
            let decoded = raster_bytes(format, &bytes).map_err(|e| broken(&e))?;
            let pixels = (size.width as u64).saturating_mul(size.height as u64);
            if pixels > MAX_PIXELS || decoded > MAX_IMAGE_BYTES {
                return Err(eco_format!(
                    "this {} image is too large: {} × {} pixels, {} decoded, more than the {} Folio reads",
                    format.name(),
                    size.width,
                    size.height,
                    megabytes(decoded),
                    megabytes(MAX_IMAGE_BYTES)
                ));
            }
            decodes(format, &bytes).map_err(|e| broken(&e))?;
            (size.width as f64 * PIXEL, size.height as f64 * PIXEL)
        }
        ImageFormat::Svg => {
            let tree = usvg::Tree::from_data(&bytes, &svg_options()).map_err(|e| broken(&e))?;
            svg_rasters_fit(&tree).map_err(|e| broken(&e))?;
            text = svg_has_text(&bytes);
            let size = tree.size();
            (
                f64::from(size.width()) * PIXEL,
                f64::from(size.height()) * PIXEL,
            )
        }
        ImageFormat::Pdf => {
            let pdf = hayro_syntax::Pdf::new(Arc::new(Shared(bytes.clone())))
                .map_err(|e| broken(&format_args!("{e:?}")))?;
            let Some(page) = pdf.pages().first() else {
                return Err(broken(&"it has no pages"));
            };
            pdf_page_fits(page).map_err(|e| broken(&e))?;
            let (width, height) = page.render_dimensions();
            (f64::from(width), f64::from(height))
        }
    };
    if !(width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite()) {
        return Err(broken(&"it has no size"));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(eco_format!(
            "this {} image is too large: {width:.0} × {height:.0} pt, more than the {MAX_SIDE:.0} pt Folio lays out",
            format.name()
        ));
    }
    Ok(LoadedImage {
        image: Image::new(bytes, format),
        size: Size::new(Abs::pt(width), Abs::pt(height)),
        text,
    })
}

/// How every stage parses SVG: without fonts, so that output does not
/// depend on the machine's, and with images only from `data:` URLs, so
/// that a drawing reads no file, inside the project or out of it.
pub fn svg_options() -> usvg::Options<'static> {
    usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    }
}

/// Megabytes of 2²⁰ bytes, rounded to the nearest, for messages.
fn megabytes(bytes: u64) -> EcoString {
    eco_format!("{} MB", bytes.saturating_add(1 << 19) >> 20)
}

/// The memory a raster file takes decoded: its pixels at four bytes each,
/// or more for a PNG of more channels or bits (a 16-bit RGBA PNG takes
/// eight), which the PDF writer keeps. Reads only the file's header.
fn raster_bytes(format: ImageFormat, bytes: &[u8]) -> Result<u64, String> {
    let size = imagesize::blob_size(bytes).map_err(|e| e.to_string())?;
    let mut per_pixel = 4;
    if format == ImageFormat::Png {
        let reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .map_err(|e| e.to_string())?;
        per_pixel = per_pixel.max(reader.info().bytes_per_pixel() as u64);
    }
    Ok((size.width as u64)
        .saturating_mul(size.height as u64)
        .saturating_mul(per_pixel))
}

/// Checks the rasters an SVG drawing holds (as `data:` URLs, in it or in
/// the SVG drawings it holds): together they take at most
/// [`MAX_IMAGE_BYTES`] decoded, by their headers, each counted once by its
/// bytes however often it is drawn; drawn as often as they are, at most
/// [`MAX_DRAWN_BYTES`], as a renderer decodes a raster where it is drawn.
/// Then each decodes, one at a time, so that a raster the PDF writer could
/// not decode is reported by every build, as a raster file is.
fn svg_rasters_fit(tree: &usvg::Tree) -> Result<(), String> {
    let rasters = svg_rasters(tree);
    let (mut total, mut drawn) = (0u64, 0u64);
    for raster in &rasters {
        let bytes = raster_bytes(raster.format, &raster.data)
            .map_err(|e| format!("a {} image in it cannot be read: {e}", raster.format.name()))?;
        total = total.saturating_add(bytes);
        drawn = drawn.saturating_add(bytes.saturating_mul(raster.uses));
    }
    too_large(total)?;
    if drawn > MAX_DRAWN_BYTES {
        return Err(format!(
            "its images are drawn so often that drawing them decodes {}, more than the {} Folio draws",
            megabytes(drawn),
            megabytes(MAX_DRAWN_BYTES)
        ));
    }
    for raster in &rasters {
        decodes(raster.format, &raster.data)
            .map_err(|e| format!("a {} image in it cannot be read: {e}", raster.format.name()))?;
    }
    Ok(())
}

/// The most bytes the rasters of an SVG drawing may take decoded, counted
/// as often as they are drawn: four times [`MAX_IMAGE_BYTES`].
pub const MAX_DRAWN_BYTES: u64 = MAX_IMAGE_BYTES * 4;

/// A distinct raster of an SVG drawing.
struct SvgRaster {
    format: ImageFormat,
    data: Arc<Vec<u8>>,
    /// How many images draw it.
    uses: u64,
}

/// The distinct rasters of an SVG drawing and of the SVG drawings it holds,
/// in document order. Images that share their data (as `use` elements
/// copy them) are told apart without reading it; others by their bytes.
fn svg_rasters(tree: &usvg::Tree) -> Vec<SvgRaster> {
    #[derive(Default)]
    struct Seen {
        by_pointer: HashMap<*const Vec<u8>, usize>,
        by_bytes: HashMap<Arc<Vec<u8>>, usize>,
        out: Vec<SvgRaster>,
    }
    fn walk(group: &usvg::Group, seen: &mut Seen) {
        for node in group.children() {
            if let usvg::Node::Image(image) = node {
                let (format, data) = match image.kind() {
                    usvg::ImageKind::JPEG(data) => (ImageFormat::Jpeg, data),
                    usvg::ImageKind::PNG(data) => (ImageFormat::Png, data),
                    usvg::ImageKind::GIF(data) => (ImageFormat::Gif, data),
                    usvg::ImageKind::WEBP(data) => (ImageFormat::Webp, data),
                    usvg::ImageKind::SVG(tree) => {
                        walk(tree.root(), seen);
                        continue;
                    }
                };
                let index = match seen.by_pointer.get(&Arc::as_ptr(data)) {
                    Some(&index) => index,
                    None => {
                        let next = seen.out.len();
                        let index = *seen.by_bytes.entry(data.clone()).or_insert(next);
                        if index == next {
                            seen.out.push(SvgRaster {
                                format,
                                data: data.clone(),
                                uses: 0,
                            });
                        }
                        seen.by_pointer.insert(Arc::as_ptr(data), index);
                        index
                    }
                };
                seen.out[index].uses += 1;
            }
            if let usvg::Node::Group(group) = node {
                walk(group, seen);
            }
            node.subroots(|root| walk(root, seen));
        }
    }
    let mut seen = Seen::default();
    walk(tree.root(), &mut seen);
    seen.out
}

/// The error for rasters in a figure that take more than
/// [`MAX_IMAGE_BYTES`] decoded.
fn too_large(total: u64) -> Result<(), String> {
    if total > MAX_IMAGE_BYTES {
        return Err(format!(
            "the images in it take {} decoded, more than the {} Folio reads",
            megabytes(total),
            megabytes(MAX_IMAGE_BYTES)
        ));
    }
    Ok(())
}

/// Checks that a PDF figure's page can be drawn within the PDF reader's
/// limits, by converting it to SVG as the PDF writer and the renderer do:
/// no form XObject that draws itself, no more work than the reader does,
/// and images that take at most [`MAX_IMAGE_BYTES`] decoded together.
fn pdf_page_fits(page: &hayro_syntax::page::Page<'_>) -> Result<(), String> {
    use hayro_svg::hayro_interpret::{InterpreterSettings, InterpreterWarning};
    use std::sync::Mutex;
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = warnings.clone();
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |warning| {
            if let Ok(mut warnings) = sink.lock() {
                warnings.push(warning);
            }
        }),
        ..InterpreterSettings::default()
    };
    // A panic in the reader, on a file it does not expect, is the file's
    // fault: it is reported as the file not read, as the writers would not
    // read it either.
    let svg = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hayro_svg::convert(
            page,
            &hayro_svg::RenderCache::new(),
            &settings,
            &hayro_svg::SvgRenderSettings::default(),
        )
    }))
    .map_err(|_| "the PDF reader failed on its page".to_string())?;
    let warnings = warnings.lock().map_err(|e| e.to_string())?;
    for warning in warnings.iter() {
        match warning {
            InterpreterWarning::FormCycle => {
                return Err("a form XObject in it draws itself".into());
            }
            InterpreterWarning::TooComplex => {
                return Err("its page takes more work to draw than Folio does".into());
            }
            InterpreterWarning::ImageTooLarge => {
                return Err(format!(
                    "the images in it take more than the {} decoded that Folio reads",
                    megabytes(MAX_IMAGE_BYTES)
                ));
            }
            _ => {}
        }
    }
    // The writers parse the page converted as an SVG drawing: one they
    // could not parse is reported here too.
    usvg::Tree::from_data(svg.as_bytes(), &svg_options())
        .map_err(|e| format!("its page converts to a drawing that cannot be read: {e}"))?;
    Ok(())
}

/// Whether an SVG drawing has a `text` element, which is not drawn.
fn svg_has_text(bytes: &[u8]) -> bool {
    const SVG: &str = "http://www.w3.org/2000/svg";
    let Ok(source) = std::str::from_utf8(bytes) else {
        return false;
    };
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let Ok(document) = roxmltree::Document::parse_with_options(source, options) else {
        return false;
    };
    document.descendants().any(|node| {
        let name = node.tag_name();
        node.is_element() && name.name() == "text" && name.namespace().is_none_or(|ns| ns == SVG)
    })
}

/// Decodes a raster image's first frame whole, or says why it cannot be.
fn decodes(format: ImageFormat, bytes: &[u8]) -> Result<(), String> {
    use std::io::Cursor;
    match format {
        ImageFormat::Png => {
            let mut reader = png::Decoder::new(Cursor::new(bytes))
                .read_info()
                .map_err(|e| e.to_string())?;
            let size = reader
                .output_buffer_size()
                .ok_or("its size does not fit in memory")?;
            let mut buf = vec![0; size];
            reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        }
        ImageFormat::Jpeg => {
            let cursor = zune_jpeg::zune_core::bytestream::ZCursor::new(bytes);
            zune_jpeg::JpegDecoder::new(cursor)
                .decode()
                .map_err(|e| format!("{e:?}"))?;
        }
        ImageFormat::Webp => {
            let mut decoder =
                image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            let size = decoder
                .output_buffer_size()
                .ok_or("its size does not fit in memory")?;
            let mut buf = vec![0; size];
            decoder.read_image(&mut buf).map_err(|e| e.to_string())?;
        }
        ImageFormat::Gif => {
            let mut options = gif::DecodeOptions::new();
            options.set_color_output(gif::ColorOutput::RGBA);
            options.set_memory_limit(gif::MemoryLimit::Bytes(
                std::num::NonZeroU64::new(MAX_IMAGE_BYTES).expect("not zero"),
            ));
            let mut decoder = options
                .read_info(Cursor::new(bytes))
                .map_err(|e| e.to_string())?;
            decoder
                .read_next_frame()
                .map_err(|e| e.to_string())?
                .ok_or("it has no frames")?;
        }
        ImageFormat::Svg | ImageFormat::Pdf => {}
    }
    Ok(())
}

/// Bytes shared with a reader that takes an `Arc` of something sized.
struct Shared(Arc<[u8]>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_size_in_points() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"/>"#;
        let loaded = read_image(Arc::from(&svg[..])).unwrap();
        assert_eq!(loaded.image.format(), ImageFormat::Svg);
        assert_eq!(loaded.size, Size::new(Abs::pt(150.0), Abs::pt(75.0)));
    }

    #[test]
    fn svg_reads_no_files() {
        // A PNG that the default resolver would read from the disk.
        let png = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/corpus/figures/assets/wave.png"
        );
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="{png}" width="10" height="10"/></svg>"#
        );
        let read = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).unwrap();
        assert!(read.root().has_children(), "the default reads the file");
        let tree = usvg::Tree::from_data(svg.as_bytes(), &svg_options()).unwrap();
        assert!(!tree.root().has_children());
    }

    #[test]
    fn text_in_svg() {
        let with = |body: &str| {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{body}</svg>"#
            );
            svg_has_text(svg.as_bytes())
        };
        assert!(with("<text>Hi</text>"));
        assert!(!with(
            "<!-- <text> was here --><rect width='1' height='1'/>"
        ));
        assert!(!with("<textArea/>"));
    }

    /// The core's limit is the one the PDF reader applies.
    #[test]
    fn limit_matches_the_pdf_reader() {
        assert_eq!(MAX_IMAGE_BYTES, hayro_syntax::MAX_IMAGE_BYTES);
    }

    /// A form XObject that draws itself is refused, by its first `Do`
    /// (#333).
    #[test]
    fn a_form_that_draws_itself() {
        let bytes = include_bytes!("../tests/untrusted/form-cycle-once.pdf");
        let error = read_image(Arc::from(&bytes[..])).unwrap_err();
        assert_eq!(
            error,
            "this PDF file cannot be read: a form XObject in it draws itself"
        );
    }

    /// A PNG of a size, 8-bit RGBA, with a header and no pixels.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff]);
        chunk(b"IEND", &[]);
        out
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn svg_of(pngs: &[Vec<u8>]) -> usvg::Tree {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let base64 = |bytes: &[u8]| {
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
                for i in 0..4 {
                    out.push(if i <= chunk.len() {
                        ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
                    } else {
                        '='
                    });
                }
            }
            out
        };
        let images: String = pngs
            .iter()
            .enumerate()
            .map(|(i, png)| {
                format!(
                    r#"<image x="{i}" width="1" height="1" href="data:image/png;base64,{}"/>"#,
                    base64(png)
                )
            })
            .collect();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{images}</svg>"#
        );
        usvg::Tree::from_data(svg.as_bytes(), &svg_options()).unwrap()
    }

    /// A raster an SVG drawing repeats counts once, by its bytes; distinct
    /// ones count together (#333).
    #[test]
    fn svg_rasters_count_once_each() {
        let png = png_header(6_000, 6_000);
        let repeated = svg_of(&vec![png.clone(); 10]);
        let rasters = svg_rasters(&repeated);
        assert_eq!(rasters.len(), 1);
        assert_eq!(rasters[0].uses, 10);
        let error = svg_rasters_fit(&repeated).unwrap_err();
        assert!(error.contains("drawn so often"), "{error}");
        let distinct = svg_of(&[png, png_header(6_000, 6_001)]);
        assert_eq!(svg_rasters(&distinct).len(), 2);
        let error = svg_rasters_fit(&distinct).unwrap_err();
        assert!(error.contains("more than the 256 MB"), "{error}");
    }

    /// A raster in an SVG drawing that does not decode is reported, as the
    /// PDF writer would fail on it.
    #[test]
    fn a_broken_raster_in_an_svg() {
        let error = svg_rasters_fit(&svg_of(&[png_header(10, 10)])).unwrap_err();
        assert!(
            error.starts_with("a PNG image in it cannot be read"),
            "{error}"
        );
    }

    #[test]
    fn megabytes_round() {
        assert_eq!(megabytes(MAX_IMAGE_BYTES), "256 MB");
        assert_eq!(megabytes(0), "0 MB");
        assert_eq!(megabytes((1 << 20) + (1 << 19)), "2 MB");
        assert_eq!(megabytes(u64::MAX), "17592186044415 MB");
    }

    #[test]
    fn unknown_and_broken_files() {
        assert!(read_image(Arc::from(&b"hello"[..])).is_err());
        let broken = read_image(Arc::from(&b"\x89PNG\r\n\x1a\nxx"[..])).unwrap_err();
        assert!(
            broken.starts_with("this PNG file cannot be read"),
            "{broken}"
        );
    }
}
