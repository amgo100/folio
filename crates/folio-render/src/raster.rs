//! Rasterisation of display lists and glyphs with `tiny-skia`, and PNG
//! encoding.
//!
//! Glyph outlines come from the font bytes through `skrifa`, unhinted, so
//! a glyph has the same shape at every zoom and on every platform.
//! `tiny-skia` is built without its SIMD paths, so the same list gives the
//! same pixels on every platform (SPEC §2.4).

use std::collections::HashMap;

use folio_component::{Color, FillRule, FontId, LineCap, LineJoin};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia as sk;

use crate::list::{Command, DisplayList, FRAME_THICKNESS, Glyphs, PathShape, Segment};

/// The fonts that a display list's glyph runs name by [`FontId`].
pub trait Fonts {
    /// The bytes of the font file that holds the font with an id, and the
    /// font's index in it, or `None` when there is no such font.
    fn font(&self, id: FontId) -> Option<FontBytes<'_>>;
}

/// A font as bytes: a font file and the index of the font in it.
#[derive(Debug, Copy, Clone)]
pub struct FontBytes<'a> {
    /// The font file.
    pub data: &'a [u8],
    /// The font's index in a collection, `0` for a single font.
    pub index: u32,
}

impl Fonts for Vec<FontBytes<'_>> {
    fn font(&self, id: FontId) -> Option<FontBytes<'_>> {
        self.get(id.0 as usize).copied()
    }
}

/// The outlines of glyphs, in font units with `y` pointing up, read once
/// per glyph and kept for every later rendering.
///
/// The cache is keyed by font id, so it belongs to one font table: use a
/// new one when the ids name other fonts.
#[derive(Debug, Default)]
pub struct Outlines {
    fonts: HashMap<FontId, Option<f32>>,
    glyphs: HashMap<(FontId, u16), Option<sk::Path>>,
}

impl Outlines {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The units per em of a font, or `None` when it cannot be read.
    pub(crate) fn units_per_em(&mut self, fonts: &dyn Fonts, font: FontId) -> Option<f32> {
        *self.fonts.entry(font).or_insert_with(|| {
            let bytes = fonts.font(font)?;
            let font = FontRef::from_index(bytes.data, bytes.index).ok()?;
            let upem = font
                .metrics(Size::unscaled(), LocationRef::default())
                .units_per_em;
            (upem > 0).then_some(f32::from(upem))
        })
    }

    /// The outline of a glyph, or `None` when it has none (a space) or
    /// cannot be read.
    pub fn outline(&mut self, fonts: &dyn Fonts, font: FontId, glyph: u16) -> Option<&sk::Path> {
        self.glyphs
            .entry((font, glyph))
            .or_insert_with(|| {
                let bytes = fonts.font(font)?;
                let font = FontRef::from_index(bytes.data, bytes.index).ok()?;
                let outline = font.outline_glyphs().get(GlyphId::new(u32::from(glyph)))?;
                let mut pen = Pen(sk::PathBuilder::new());
                outline
                    .draw(
                        DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                        &mut pen,
                    )
                    .ok()?;
                pen.0.finish()
            })
            .as_ref()
    }

    /// The transform from a glyph's font units to a point at a font size
    /// and scale, with `y` flipped to point down.
    fn transform(
        &mut self,
        fonts: &dyn Fonts,
        font: FontId,
        size: f32,
        scale: f32,
        x: f32,
        y: f32,
    ) -> Option<sk::Transform> {
        let k = size / self.units_per_em(fonts, font)? * scale;
        Some(sk::Transform::from_row(
            k,
            0.0,
            0.0,
            -k,
            x * scale,
            y * scale,
        ))
    }
}

struct Pen(sk::PathBuilder);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

/// How a display list is rendered.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Options {
    /// Pixels per point: `1.0` renders at 72 dpi, `96.0 / 72.0` at the
    /// size a browser shows at 100 %.
    pub scale: f32,
    /// What the image is filled with first, or `None` for transparency.
    pub background: Option<Color>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale: 1.0,
            background: Some(Color::WHITE),
        }
    }
}

/// An image: rows of pixels, top to bottom, each four bytes of red, green,
/// blue and straight (not premultiplied) alpha.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Image {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// The pixels.
    pub rgba: Vec<u8>,
}

/// Why an image could not be made.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// The image would be empty or too large: its size in pixels, from
    /// the list's size and the scale.
    Size(f32, f32),
    /// PNG encoding or decoding failed.
    Png(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size(w, h) => write!(f, "an image of {w} × {h} pixels cannot be made"),
            Self::Png(message) => write!(f, "PNG: {message}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// The largest side of an image [`render`] makes, in pixels.
pub const MAX_SIDE: u32 = 16_384;

/// Renders a display list to an image at a scale.
///
/// Glyphs are filled from their outlines, anti-aliased; a glyph whose font
/// or outline cannot be read is left out. Paths are filled, then stroked.
///
/// # Errors
///
/// When the image would have no pixels, or a side longer than
/// [`MAX_SIDE`].
pub fn render(
    list: &DisplayList,
    fonts: &dyn Fonts,
    outlines: &mut Outlines,
    options: &Options,
) -> Result<Image, RenderError> {
    let width = (list.width * options.scale).ceil();
    let height = (list.height * options.scale).ceil();
    let side = |v: f32| (v >= 1.0 && v <= MAX_SIDE as f32).then_some(v as u32);
    let (Some(w), Some(h)) = (side(width), side(height)) else {
        return Err(RenderError::Size(width, height));
    };
    let mut pixmap = sk::Pixmap::new(w, h).ok_or(RenderError::Size(width, height))?;
    if let Some(color) = options.background {
        pixmap.fill(sk::Color::from_rgba8(color.r, color.g, color.b, color.a));
    }
    let scale = options.scale;
    for item in &list.items {
        match &item.command {
            Command::Glyphs(run) => glyphs(&mut pixmap, run, fonts, outlines, scale),
            Command::Path(shape) => path(&mut pixmap, shape, scale),
            Command::Frame(rect) => {
                // The frame of the PDF writer: a stroke inside the box.
                let half = FRAME_THICKNESS / 2.0;
                let inner = |side: f32| (side - FRAME_THICKNESS).max(0.0);
                let x = rect.x + half;
                let y = rect.y + half;
                let (w, h) = (inner(rect.width), inner(rect.height));
                let p = |x, y| crate::list::Point { x, y };
                let frame = PathShape {
                    segments: vec![
                        Segment::MoveTo(p(x, y)),
                        Segment::LineTo(p(x + w, y)),
                        Segment::LineTo(p(x + w, y + h)),
                        Segment::LineTo(p(x, y + h)),
                        Segment::Close,
                    ],
                    fill: None,
                    fill_rule: FillRule::NonZero,
                    stroke: Some(crate::list::Stroke {
                        color: Color::BLACK,
                        thickness: FRAME_THICKNESS,
                        cap: LineCap::Butt,
                        join: LineJoin::Miter,
                        miter_limit: 4.0,
                        dash: None,
                    }),
                };
                path(&mut pixmap, &frame, scale);
            }
            Command::Box(_) | Command::Unknown(_) => {}
        }
    }
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(Image {
        width: w,
        height: h,
        rgba,
    })
}

fn paint(color: Color) -> sk::Paint<'static> {
    let mut paint = sk::Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    paint
}

fn glyphs(
    pixmap: &mut sk::Pixmap,
    run: &Glyphs,
    fonts: &dyn Fonts,
    outlines: &mut Outlines,
    scale: f32,
) {
    let fill = paint(run.fill);
    for (at, glyph) in run.positioned() {
        let Some(transform) = outlines.transform(fonts, run.font, run.size, scale, at.x, at.y)
        else {
            return;
        };
        if let Some(outline) = outlines.outline(fonts, run.font, glyph.id) {
            pixmap.fill_path(outline, &fill, sk::FillRule::Winding, transform, None);
        }
    }
}

/// A path in tiny-skia's form, or `None` when it is empty.
fn sk_path(segments: &[Segment]) -> Option<sk::Path> {
    let mut builder = sk::PathBuilder::new();
    for segment in segments {
        match *segment {
            Segment::MoveTo(p) => builder.move_to(p.x, p.y),
            Segment::LineTo(p) => builder.line_to(p.x, p.y),
            Segment::CubicTo(a, b, c) => builder.cubic_to(a.x, a.y, b.x, b.y, c.x, c.y),
            Segment::Close => builder.close(),
        }
    }
    builder.finish()
}

fn path(pixmap: &mut sk::Pixmap, shape: &PathShape, scale: f32) {
    let Some(path) = sk_path(&shape.segments) else {
        return;
    };
    let transform = sk::Transform::from_scale(scale, scale);
    if let Some(color) = shape.fill {
        let rule = match shape.fill_rule {
            FillRule::NonZero => sk::FillRule::Winding,
            FillRule::EvenOdd => sk::FillRule::EvenOdd,
        };
        pixmap.fill_path(&path, &paint(color), rule, transform, None);
    }
    if let Some(stroke) = &shape.stroke {
        let sk_stroke = sk::Stroke {
            width: stroke.thickness,
            miter_limit: stroke.miter_limit,
            line_cap: match stroke.cap {
                LineCap::Butt => sk::LineCap::Butt,
                LineCap::Round => sk::LineCap::Round,
                LineCap::Square => sk::LineCap::Square,
            },
            line_join: match stroke.join {
                LineJoin::Miter => sk::LineJoin::Miter,
                LineJoin::Round => sk::LineJoin::Round,
                LineJoin::Bevel => sk::LineJoin::Bevel,
            },
            dash: stroke
                .dash
                .as_ref()
                .and_then(|(array, phase)| sk::StrokeDash::new(array.clone(), *phase)),
        };
        pixmap.stroke_path(&path, &paint(stroke.color), &sk_stroke, transform, None);
    }
}

/// A glyph rasterised to coverage: what a client that draws display lists
/// itself, such as a canvas, places at a glyph's position (SPEC §12.4).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GlyphBitmap {
    /// The horizontal distance from the glyph's origin to the bitmap's left
    /// edge, in pixels.
    pub left: i32,
    /// The vertical distance from the glyph's origin to the bitmap's top
    /// edge, in pixels, downwards: negative for a glyph above its baseline.
    pub top: i32,
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// The coverage of each pixel, rows top to bottom, `0` to `255`.
    pub coverage: Vec<u8>,
}

/// Rasterises a glyph at a size in pixels (the font size in points times
/// the zoom), anti-aliased, with its origin `subpixel` pixels (from `0.0`
/// up to `1.0`) to the right of a pixel's left edge.
///
/// `None` when the glyph has no outline, such as a space, or its font or
/// outline cannot be read, or the bitmap would be larger than
/// [`MAX_SIDE`] a side.
pub fn rasterize_glyph(
    fonts: &dyn Fonts,
    outlines: &mut Outlines,
    font: FontId,
    glyph: u16,
    size: f32,
    subpixel: f32,
) -> Option<GlyphBitmap> {
    let transform = outlines.transform(fonts, font, size, 1.0, subpixel, 0.0)?;
    let outline = outlines.outline(fonts, font, glyph)?;
    let bounds = outline.clone().transform(transform)?.bounds();
    let left = bounds.left().floor();
    let top = bounds.top().floor();
    let width = (bounds.right().ceil() - left).max(1.0);
    let height = (bounds.bottom().ceil() - top).max(1.0);
    if width > MAX_SIDE as f32 || height > MAX_SIDE as f32 {
        return None;
    }
    let size = sk::IntSize::from_wh(width as u32, height as u32)?;
    let mut mask = sk::Mask::new(size.width(), size.height())?;
    let transform = transform.post_translate(-left, -top);
    mask.fill_path(outline, sk::FillRule::Winding, true, transform);
    Some(GlyphBitmap {
        left: left as i32,
        top: top as i32,
        width: size.width(),
        height: size.height(),
        coverage: mask.data().to_vec(),
    })
}

impl Image {
    /// The image as a PNG file: eight bits per channel, RGBA, or RGB when
    /// every pixel is opaque.
    ///
    /// # Errors
    ///
    /// When the encoder fails, which it does only for an image whose
    /// pixels do not match its size.
    pub fn png(&self) -> Result<Vec<u8>, RenderError> {
        let opaque = self.rgba.chunks_exact(4).all(|p| p[3] == 255);
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let data: Vec<u8> = if opaque {
            encoder.set_color(png::ColorType::Rgb);
            self.rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect()
        } else {
            encoder.set_color(png::ColorType::Rgba);
            self.rgba.clone()
        };
        let err = |e: png::EncodingError| RenderError::Png(e.to_string());
        let mut writer = encoder.write_header().map_err(err)?;
        writer.write_image_data(&data).map_err(err)?;
        writer.finish().map_err(err)?;
        Ok(out)
    }

    /// Reads a PNG file of eight bits per channel, grey or colour, with or
    /// without alpha.
    ///
    /// # Errors
    ///
    /// When the file is not such a PNG.
    pub fn from_png(bytes: &[u8]) -> Result<Self, RenderError> {
        let err = |e: png::DecodingError| RenderError::Png(e.to_string());
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().map_err(err)?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| RenderError::Png("the image is too large".into()))?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(err)?;
        if info.bit_depth != png::BitDepth::Eight {
            return Err(RenderError::Png("not eight bits per channel".into()));
        }
        let buf = &buf[..info.buffer_size()];
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf.to_vec(),
            png::ColorType::Rgb => buf
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => buf
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => {
                return Err(RenderError::Png("indexed colour was not expanded".into()));
            }
        };
        Ok(Self {
            width: info.width,
            height: info.height,
            rgba,
        })
    }
}
