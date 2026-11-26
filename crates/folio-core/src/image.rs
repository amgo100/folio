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

/// The image in a file's bytes, as [`load_image`] reads it.
pub fn read_image(bytes: Arc<[u8]>) -> Result<LoadedImage, EcoString> {
    let Some(format) = ImageFormat::detect(&bytes) else {
        return Err(
            "this file is not an image Folio can read: JPEG, PNG, WebP, GIF, SVG or PDF".into(),
        );
    };
    let broken = |why: &dyn std::fmt::Display| -> EcoString {
        eco_format!("this {} file cannot be read: {why}", format.name())
    };
    let (width, height) = match format {
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Webp | ImageFormat::Gif => {
            let size = imagesize::blob_size(&bytes).map_err(|e| broken(&e))?;
            (size.width as f64 * PIXEL, size.height as f64 * PIXEL)
        }
        ImageFormat::Svg => {
            let tree =
                usvg::Tree::from_data(&bytes, &usvg::Options::default()).map_err(|e| broken(&e))?;
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
    let text = format == ImageFormat::Svg && bytes.windows(5).any(|w| w == b"<text");
    Ok(LoadedImage {
        image: Image::new(bytes, format),
        size: Size::new(Abs::pt(width), Abs::pt(height)),
        text,
    })
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
    fn unknown_and_broken_files() {
        assert!(read_image(Arc::from(&b"hello"[..])).is_err());
        let broken = read_image(Arc::from(&b"\x89PNG\r\n\x1a\nxx"[..])).unwrap_err();
        assert!(
            broken.starts_with("this PNG file cannot be read"),
            "{broken}"
        );
    }
}
