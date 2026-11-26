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

/// The most pixels an image may have: 8,192 × 8,192, 256 MB as RGBA. A
/// larger one is refused before it is decoded, as decoding it could take
/// more memory than the machine has.
pub const MAX_PIXELS: u64 = 8_192 * 8_192;

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
            let pixels = (size.width as u64).saturating_mul(size.height as u64);
            if pixels > MAX_PIXELS {
                return Err(eco_format!(
                    "this {} image is too large: {} × {} pixels, more than Folio reads ({} million)",
                    format.name(),
                    size.width,
                    size.height,
                    MAX_PIXELS / 1_000_000
                ));
            }
            decodes(format, &bytes).map_err(|e| broken(&e))?;
            (size.width as f64 * PIXEL, size.height as f64 * PIXEL)
        }
        ImageFormat::Svg => {
            let tree = usvg::Tree::from_data(&bytes, &svg_options()).map_err(|e| broken(&e))?;
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
            let (width, height) = page.render_dimensions();
            (f64::from(width), f64::from(height))
        }
    };
    if !(width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite()) {
        return Err(broken(&"it has no size"));
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
    let mut options = usvg::Options::default();
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(|_, _| None),
    };
    options
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
