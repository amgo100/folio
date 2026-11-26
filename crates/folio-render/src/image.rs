//! Images in display lists (SPEC §11.1, §12.2): decoded once into pixels
//! or a vector drawing, and drawn scaled to their boxes. JPEG, PNG, WebP
//! and GIF (its first frame) are decoded to pixels; SVG is drawn by
//! `resvg`, and a PDF figure's first page is converted to SVG first, as
//! the PDF writer converts it under PDF/UA. Text in SVG is not set: no
//! fonts are given to `usvg`, as in PDF output.

use std::fmt::Write;
use std::sync::Arc;

use folio_component::{Image, ImageFormat};
use tiny_skia as sk;

/// An image ready to draw.
pub(crate) enum Decoded {
    /// Pixels, premultiplied.
    Pixels(sk::Pixmap),
    /// A drawing.
    Vector(Arc<usvg::Tree>),
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pixels(p) => write!(f, "Pixels({}x{})", p.width(), p.height()),
            Self::Vector(tree) => write!(f, "Vector({:?})", tree.size()),
        }
    }
}

/// Decodes an image, or `None` when its file is broken.
pub(crate) fn decode(image: &Image) -> Option<Decoded> {
    let bytes: &[u8] = image.bytes();
    match image.format() {
        ImageFormat::Png => pixels(png_rgba(bytes)?),
        ImageFormat::Jpeg => pixels(jpeg_rgba(bytes)?),
        ImageFormat::Webp => pixels(webp_rgba(bytes)?),
        ImageFormat::Gif => pixels(gif_rgba(bytes)?),
        ImageFormat::Svg => svg_tree(bytes).map(Decoded::Vector),
        ImageFormat::Pdf => svg_tree(pdf_svg(image)?.as_bytes()).map(Decoded::Vector),
    }
}

/// The most pixels an image is decoded with, as the core reads images
/// (`folio_core::image::MAX_PIXELS`): a larger one is not drawn, as
/// decoding it could take more memory than the machine has.
const MAX_PIXELS: u64 = 8_192 * 8_192;

/// Whether an image of a size may be decoded.
fn fits(width: u32, height: u32) -> bool {
    u64::from(width) * u64::from(height) <= MAX_PIXELS
}

/// Straight RGBA pixels and their size.
struct Rgba {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn pixels(rgba: Rgba) -> Option<Decoded> {
    let mut pixmap = sk::Pixmap::new(rgba.width, rgba.height)?;
    if rgba.data.len() != pixmap.data().len() {
        return None;
    }
    for (pixel, c) in pixmap
        .pixels_mut()
        .iter_mut()
        .zip(rgba.data.chunks_exact(4))
    {
        *pixel = sk::ColorU8::from_rgba(c[0], c[1], c[2], c[3]).premultiply();
    }
    Some(Decoded::Pixels(pixmap))
}

fn png_rgba(bytes: &[u8]) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if !fits(info.width, info.height) {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let buf = &buf[..info.buffer_size()];
    let data = match info.color_type {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some(Rgba {
        width: info.width,
        height: info.height,
        data,
    })
}

fn jpeg_rgba(bytes: &[u8]) -> Option<Rgba> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().ok()?;
    let (width, height) = decoder.dimensions()?;
    if !fits(width as u32, height as u32) {
        return None;
    }
    let data = decoder.decode().ok()?;
    let info = decoder.info()?;
    Some(Rgba {
        width: u32::from(info.width),
        height: u32::from(info.height),
        data,
    })
}

fn webp_rgba(bytes: &[u8]) -> Option<Rgba> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions();
    if !fits(width, height) {
        return None;
    }
    let mut buf = vec![0; decoder.output_buffer_size()?];
    decoder.read_image(&mut buf).ok()?;
    let data = if decoder.has_alpha() {
        buf
    } else {
        buf.chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect()
    };
    Some(Rgba {
        width,
        height,
        data,
    })
}

fn gif_rgba(bytes: &[u8]) -> Option<Rgba> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(std::io::Cursor::new(bytes)).ok()?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    let frame = decoder.read_next_frame().ok()??;
    // The first frame, on a transparent canvas of the image's size.
    let mut data = vec![0; width as usize * height as usize * 4];
    let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
    let (left, top) = (usize::from(frame.left), usize::from(frame.top));
    for row in 0..fh {
        let y = top + row;
        if y >= height as usize {
            break;
        }
        for col in 0..fw {
            let x = left + col;
            if x >= width as usize {
                break;
            }
            let from = (row * fw + col) * 4;
            let to = (y * width as usize + x) * 4;
            if let (Some(src), Some(dst)) =
                (frame.buffer.get(from..from + 4), data.get_mut(to..to + 4))
            {
                dst.copy_from_slice(src);
            }
        }
    }
    Some(Rgba {
        width,
        height,
        data,
    })
}

fn svg_tree(bytes: &[u8]) -> Option<Arc<usvg::Tree>> {
    usvg::Tree::from_data(bytes, &svg_options())
        .ok()
        .map(Arc::new)
}

/// The first page of a PDF figure as SVG, or `None` when it cannot be
/// read.
pub(crate) fn pdf_svg(image: &Image) -> Option<String> {
    struct Shared(Arc<[u8]>);
    impl AsRef<[u8]> for Shared {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    let pdf = hayro_svg::hayro_syntax::Pdf::new(Arc::new(Shared(image.bytes().clone()))).ok()?;
    let page = pdf.pages().first()?;
    Some(hayro_svg::convert(
        page,
        &hayro_svg::RenderCache::new(),
        &hayro_svg::hayro_interpret::InterpreterSettings::default(),
        &hayro_svg::SvgRenderSettings::default(),
    ))
}

/// Draws an image to fill a box at `(x, y)`, `width` by `height`, in the
/// pixmap's pixels.
pub(crate) fn draw(
    pixmap: &mut sk::Pixmap,
    decoded: &Decoded,
    (x, y, width, height): (f32, f32, f32, f32),
) {
    match decoded {
        Decoded::Pixels(image) => {
            let sx = width / image.width() as f32;
            let sy = height / image.height() as f32;
            let paint = sk::PixmapPaint {
                quality: sk::FilterQuality::Bilinear,
                ..sk::PixmapPaint::default()
            };
            let transform = sk::Transform::from_row(sx, 0.0, 0.0, sy, x, y);
            pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
        }
        Decoded::Vector(tree) => {
            let size = tree.size();
            let sx = width / size.width();
            let sy = height / size.height();
            let transform = sk::Transform::from_row(sx, 0.0, 0.0, sy, x, y);
            resvg::render(tree, transform, &mut pixmap.as_mut());
        }
    }
}

/// The media type of an image in SVG output and its bytes: a PDF figure
/// as the SVG it converts to.
pub(crate) fn data_uri(image: &Image) -> Option<String> {
    let (mime, converted);
    let bytes: &[u8] = match image.format() {
        ImageFormat::Jpeg => {
            mime = "image/jpeg";
            image.bytes()
        }
        ImageFormat::Png => {
            mime = "image/png";
            image.bytes()
        }
        ImageFormat::Webp => {
            mime = "image/webp";
            image.bytes()
        }
        ImageFormat::Gif => {
            mime = "image/gif";
            image.bytes()
        }
        ImageFormat::Svg => {
            mime = "image/svg+xml";
            image.bytes()
        }
        ImageFormat::Pdf => {
            mime = "image/svg+xml";
            converted = pdf_svg(image)?;
            converted.as_bytes()
        }
    };
    let mut out = String::with_capacity(bytes.len() / 3 * 4 + 40);
    let _ = write!(out, "data:{mime};base64,");
    base64(bytes, &mut out);
    Some(out)
}

/// Standard base64 with padding.
fn base64(bytes: &[u8], out: &mut String) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
}

/// How SVG is parsed, as the core parses it
/// (`folio_core::image::svg_options`): without fonts, and with images only
/// from `data:` URLs, so that a drawing reads no file.
fn svg_options() -> usvg::Options<'static> {
    let mut options = usvg::Options::default();
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(|_, _| None),
    };
    options
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_pads() {
        let encode = |b: &[u8]| {
            let mut s = String::new();
            base64(b, &mut s);
            s
        };
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }
}
