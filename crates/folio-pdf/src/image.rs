//! Images in PDF (SPEC §11.1): JPEG passed through, PNG, WebP and GIF
//! decoded and compressed by krilla, SVG drawn as vector paths through
//! `usvg`, and PDF figures placed as form XObjects; where a PDF/UA level
//! forbids embedded pages, a PDF figure is converted to SVG first (through
//! `hayro-svg`), so that it stays vector and its text becomes outlines.

use std::sync::Arc;

use ecow::{EcoString, eco_format};
use folio_component::{Image, ImageFormat};
use krilla::Data;
use krilla::geom::{Size, Transform};
use krilla::image::Image as KImage;
use krilla::pdf::{Pdf, PdfDocument};
use krilla::surface::Surface;
use krilla_svg::{SurfaceExt, SvgSettings};

/// An image as krilla draws it.
#[derive(Clone)]
pub(crate) enum Drawable {
    /// Pixels, encoded once per image.
    Raster(KImage),
    /// A drawing, as vector paths.
    Svg(Arc<usvg::Tree>),
    /// The first page of a PDF document, as a form XObject.
    Pdf(PdfDocument),
}

/// An image prepared, with the warning drawing it gives, or why it cannot
/// be drawn.
pub(crate) type Prepared = Result<(Drawable, Option<EcoString>), EcoString>;

/// Bytes shared with krilla and hayro, which take an `Arc` of something
/// sized.
struct Shared(Arc<[u8]>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

fn data(image: &Image) -> Data {
    let shared: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(Shared(image.bytes().clone()));
    Data::from(shared)
}

/// The image ready to draw, or why it cannot be: a file broken in a way
/// that reading its size did not show. `embed` says whether a PDF figure
/// may be embedded as it is; else it is converted to vector paths. A PDF
/// figure of a version newer than the output's (PDF 1.7) is converted too,
/// with a warning, the second value (SPEC §11.1).
pub(crate) fn prepare(image: &Image, embed: bool) -> Prepared {
    let broken = |why: &dyn std::fmt::Display| -> EcoString {
        eco_format!(
            "this {} image cannot be drawn: {why}",
            image.format().name()
        )
    };
    // Pixels are not interpolated: PDF/A forbids it, and viewers smooth
    // scaled images themselves.
    let raster = match image.format() {
        ImageFormat::Jpeg => Some(KImage::from_jpeg(data(image), false)),
        ImageFormat::Png => Some(KImage::from_png(data(image), false)),
        ImageFormat::Webp => Some(KImage::from_webp(data(image), false)),
        ImageFormat::Gif => Some(KImage::from_gif(data(image), false)),
        ImageFormat::Svg | ImageFormat::Pdf => None,
    };
    if let Some(raster) = raster {
        return raster
            .map(|r| (Drawable::Raster(r), None))
            .map_err(|e| broken(&e));
    }
    if image.format() == ImageFormat::Svg {
        return svg(image.bytes())
            .map(|d| (d, None))
            .map_err(|e| broken(&e));
    }
    let pdf = Pdf::new(Arc::new(Shared(image.bytes().clone())))
        .map_err(|e| broken(&format_args!("{e:?}")))?;
    let pdf = Arc::new(pdf);
    let newer = pdf.version() > hayro_svg::hayro_syntax::PdfVersion::Pdf17;
    if embed && !newer {
        return Ok((Drawable::Pdf(PdfDocument::new(pdf)), None));
    }
    let note = (embed && newer).then(|| {
        EcoString::from(
            "this PDF image is of a newer PDF version than the output (1.7), \
             so it is converted to vector paths and its text to outlines",
        )
    });
    let Some(page) = pdf.pages().first() else {
        return Err(broken(&"it has no pages"));
    };
    let converted = hayro_svg::convert(
        page,
        &hayro_svg::RenderCache::new(),
        &hayro_svg::hayro_interpret::InterpreterSettings::default(),
        &hayro_svg::SvgRenderSettings::default(),
    );
    svg(converted.as_bytes())
        .map(|d| (d, note))
        .map_err(|e| broken(&e))
}

/// An SVG drawing parsed. Text in it is not set: no fonts are given to
/// `usvg`, so that output does not depend on the fonts of the machine.
fn svg(bytes: &[u8]) -> Result<Drawable, usvg::Error> {
    let tree = usvg::Tree::from_data(bytes, &svg_options())?;
    Ok(Drawable::Svg(Arc::new(tree)))
}

/// Draws an image to fill `size` (in points), with its top-left corner at
/// `(x, y)`, where the surface's y axis points down.
pub(crate) fn draw(surface: &mut Surface<'_>, drawable: &Drawable, x: f32, y: f32, size: Size) {
    surface.push_transform(&Transform::from_translate(x, y));
    match drawable {
        Drawable::Raster(image) => surface.draw_image(image.clone(), size),
        Drawable::Svg(tree) => {
            let settings = SvgSettings {
                embed_text: false,
                ..SvgSettings::default()
            };
            let _ = surface.draw_svg(tree, size, settings);
        }
        Drawable::Pdf(pdf) => surface.draw_pdf_page(pdf, size, 0),
    }
    surface.pop();
}

/// How SVG is parsed, as the core parses it
/// (`folio_core::image::svg_options`): without fonts, and with images only
/// from `data:` URLs, so that a drawing reads no file.
fn svg_options() -> usvg::Options<'static> {
    usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    }
}
