//! Display lists (SPEC §1.5, §12.2): the drawing commands for one fragment,
//! each with the source node it is drawn for.

use std::ops::Range;

use ecow::EcoString;
use folio_component::{
    Abs, Color, FillRule, FontId, Fragment, FragmentItem, GlyphRun, LineCap, LineJoin, Location,
    Paint, PathItem, Shape, SourceRef,
};

/// The thickness of the frame drawn for an object whose content no stage
/// lays out yet, in points: the frame the PDF writer draws.
pub const FRAME_THICKNESS: f32 = 0.5;

/// The drawing commands for one fragment, such as a page, in painting
/// order, with the source node each is drawn for.
///
/// Lengths are in points, as `f32`, with the origin at the fragment's
/// top-left corner and `y` pointing down. Every item names its source node
/// ([`Span`]) and every glyph the bytes of its node's text that it shows,
/// so a list is also its fragment's span map: [`DisplayList::spans`] takes
/// that map out, and [`DisplayList::set_spans`] replaces it, for a page
/// that looks the same after an edit but whose sources moved (SPEC §2.3).
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayList {
    /// The fragment's width.
    pub width: f32,
    /// The fragment's height.
    pub height: f32,
    /// The element instance the fragment is the box of.
    pub span: Span,
    /// The items, in painting order.
    pub items: Vec<Item>,
}

/// One item of a display list.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// What to draw.
    pub command: Command,
    /// The source node it is drawn for.
    pub span: Span,
}

/// The source node an item is drawn for, as a [`Location`] of the element
/// instance when it is known. Opaque references that the core issued and
/// resolves (`folio::span_map`); a client sends them back, never reads
/// them.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Span {
    /// The node.
    pub source: SourceRef,
    /// Which instance of the element made from `source`: a number only
    /// the core interprets, [`Location::UNKNOWN_INSTANCE`] when the item
    /// names a node but not an instance, `0` for a detached item.
    pub instance: u32,
}

impl Span {
    /// The span of no node.
    pub const DETACHED: Self = Self {
        source: SourceRef::DETACHED,
        instance: 0,
    };
}

impl From<Location> for Span {
    fn from(location: Location) -> Self {
        Self {
            source: location.source,
            instance: location.instance,
        }
    }
}

/// A drawing command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A run of glyphs.
    Glyphs(Glyphs),
    /// A filled or stroked path.
    Path(PathShape),
    /// The box of an object whose content is not laid out yet (SPEC §7.2),
    /// drawn as an empty frame [`FRAME_THICKNESS`] thick inside the box,
    /// as the PDF writer draws it.
    Frame(Rect),
    /// The box of a nested fragment. Nothing is drawn: it is there for the
    /// span map, so hit-testing finds the element whose box a point is in.
    Box(Rect),
    /// An item of a kind that a later version of the wire format added,
    /// which this version decodes without knowing what it draws. Nothing
    /// is drawn; it keeps its place and its span, so that the indices of
    /// the items match those of a span map the newer encoder sends.
    Unknown(Unknown),
}

/// An item of a kind this version of the wire format does not know
/// ([`Command::Unknown`]). Only the decoder makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct Unknown {
    pub(crate) tag: u64,
}

impl Unknown {
    /// The item's tag in the wire format.
    pub fn tag(&self) -> u64 {
        self.tag
    }
}

/// A rectangle.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct Rect {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

impl Rect {
    /// Whether a point is inside the rectangle or on its edge.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// A point.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct Point {
    /// The horizontal coordinate.
    pub x: f32,
    /// The vertical coordinate, downwards.
    pub y: f32,
}

/// A run of glyphs in one font, size and colour, on one baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyphs {
    /// The start of the baseline: the pen position of the first glyph.
    pub origin: Point,
    /// The font, by the id that the font table knows it by.
    pub font: FontId,
    /// The font size.
    pub size: f32,
    /// The colour the glyphs are filled with.
    pub fill: Color,
    /// The text the glyphs were shaped from, for selection and copying.
    pub text: EcoString,
    /// The glyphs, in visual order.
    pub glyphs: Vec<GlyphItem>,
}

impl Glyphs {
    /// Each glyph with where it is drawn: the point its outline's origin
    /// is placed at. The pen starts at the origin and moves by each
    /// glyph's advance, summed in `f32` in order, so that every renderer
    /// places glyphs at the same points.
    pub fn positioned(&self) -> impl Iterator<Item = (Point, &GlyphItem)> + '_ {
        let mut pen = self.origin.x;
        self.glyphs.iter().map(move |glyph| {
            let at = Point {
                x: pen + glyph.dx,
                y: self.origin.y + glyph.dy,
            };
            pen += glyph.advance;
            (at, glyph)
        })
    }
}

/// A glyph of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphItem {
    /// The glyph's id in its font.
    pub id: u16,
    /// How far the pen moves after the glyph.
    pub advance: f32,
    /// The horizontal offset of the glyph from the pen position.
    pub dx: f32,
    /// The vertical offset of the glyph from the baseline, downwards.
    pub dy: f32,
    /// The bytes of the run's text that the glyph shows: more than one
    /// character for a ligature.
    pub text: Range<u32>,
    /// The span-map entry of the glyph: its source node and the bytes of
    /// the node's text that it shows, as [`folio_component::Glyph::source`].
    pub source: GlyphSpan,
}

/// The span-map entry of a glyph: its source node and the bytes of the
/// node's text that it shows.
pub type GlyphSpan = (SourceRef, Range<u32>);

/// A path with a fill, a stroke or both.
#[derive(Debug, Clone, PartialEq)]
pub struct PathShape {
    /// The segments, in the list's coordinates.
    pub segments: Vec<Segment>,
    /// The fill colour, if any.
    pub fill: Option<Color>,
    /// How the fill treats self-intersections.
    pub fill_rule: FillRule,
    /// The outline, if any.
    pub stroke: Option<Stroke>,
}

/// One segment of a path.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Segment {
    /// Starts a new subpath.
    MoveTo(Point),
    /// A straight line.
    LineTo(Point),
    /// A cubic Bézier curve through two control points.
    CubicTo(Point, Point, Point),
    /// Closes the current subpath.
    Close,
}

/// How a path's outline is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    /// The colour.
    pub color: Color,
    /// The line width.
    pub thickness: f32,
    /// The shape of open ends.
    pub cap: LineCap,
    /// The shape of corners.
    pub join: LineJoin,
    /// The miter limit, as a ratio of the line width.
    pub miter_limit: f32,
    /// The dash pattern, if any: dash and gap lengths, and the phase.
    pub dash: Option<(Vec<f32>, f32)>,
}

/// The span map of a display list: the span of each item, and of each
/// glyph of a glyph run, in the list's order. It is what an edit that
/// leaves a page's look alone changes (SPEC §2.3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanMap {
    /// The span of the fragment the list is for.
    pub span: Span,
    /// Per item, its span and the span-map entries of its glyphs (empty
    /// for an item that is not a glyph run).
    pub items: Vec<(Span, Vec<GlyphSpan>)>,
}

/// Why a span map does not fit a display list.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct SpanMismatch;

impl std::fmt::Display for SpanMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the span map has another number of items or glyphs than the display list")
    }
}

impl std::error::Error for SpanMismatch {}

impl DisplayList {
    /// The display list of a fragment: its items and those of the
    /// fragments nested in it, flattened in painting order into the
    /// fragment's coordinates.
    ///
    /// A nested fragment becomes a [`Command::Box`] before its items, a
    /// glyph run is drawn for the fragment it is in, and an object is
    /// drawn as an empty frame until its stage lays out its content.
    pub fn from_fragment(fragment: &Fragment) -> Self {
        let mut list = Self {
            width: len(fragment.size.width),
            height: len(fragment.size.height),
            span: fragment.location().into(),
            items: Vec::new(),
        };
        list.walk(fragment, Abs::zero(), Abs::zero());
        list
    }

    fn walk(&mut self, fragment: &Fragment, x: Abs, y: Abs) {
        let box_span: Span = fragment.location().into();
        for (pos, item) in &fragment.items {
            let (x, y) = (x + pos.x, y + pos.y);
            match item {
                FragmentItem::Group(inner) => {
                    self.items.push(Item {
                        command: Command::Box(rect(x, y, inner.size.width, inner.size.height)),
                        span: inner.location().into(),
                    });
                    self.walk(inner, x, y);
                }
                FragmentItem::Text(run) => self.items.push(Item {
                    command: Command::Glyphs(glyphs(run, x, y)),
                    span: box_span,
                }),
                FragmentItem::Shape { shape, source } => {
                    let instance = if source.is_detached() {
                        0
                    } else if *source == fragment.source {
                        fragment.instance
                    } else {
                        Location::UNKNOWN_INSTANCE
                    };
                    self.items.push(Item {
                        command: Command::Path(path(shape, x, y)),
                        span: Span {
                            source: *source,
                            instance,
                        },
                    });
                }
                FragmentItem::Object(object) => self.items.push(Item {
                    command: Command::Frame(rect(x, y, object.size.width, object.size.height)),
                    span: object.location.into(),
                }),
            }
        }
    }

    /// The span map: the spans of the list and of its items and glyphs.
    pub fn spans(&self) -> SpanMap {
        SpanMap {
            span: self.span,
            items: self
                .items
                .iter()
                .map(|item| {
                    let glyphs = match &item.command {
                        Command::Glyphs(run) => {
                            run.glyphs.iter().map(|g| g.source.clone()).collect()
                        }
                        _ => Vec::new(),
                    };
                    (item.span, glyphs)
                })
                .collect(),
        }
    }

    /// Replaces the span map with another of a list that draws the same,
    /// such as the same page after an edit elsewhere.
    ///
    /// # Errors
    ///
    /// When the map has another number of items, or of glyphs in a run;
    /// the list is left as it was.
    pub fn set_spans(&mut self, spans: SpanMap) -> Result<(), SpanMismatch> {
        if spans.items.len() != self.items.len() {
            return Err(SpanMismatch);
        }
        for (item, (_, glyphs)) in self.items.iter().zip(&spans.items) {
            let count = match &item.command {
                Command::Glyphs(run) => run.glyphs.len(),
                _ => 0,
            };
            if glyphs.len() != count {
                return Err(SpanMismatch);
            }
        }
        self.span = spans.span;
        for (item, (span, glyphs)) in self.items.iter_mut().zip(spans.items) {
            item.span = span;
            if let Command::Glyphs(run) = &mut item.command {
                for (glyph, source) in run.glyphs.iter_mut().zip(glyphs) {
                    glyph.source = source;
                }
            }
        }
        Ok(())
    }

    /// The items whose box contains a point, topmost first: glyphs by
    /// the box from their pen position to the next, a font size high
    /// above the baseline and a quarter of it below; paths and frames by
    /// their bounds; nested fragments by their box. What a click in a
    /// preview leads to (SPEC §12.2).
    pub fn hit(&self, x: f32, y: f32) -> Vec<Hit> {
        let mut hits = Vec::new();
        for (index, item) in self.items.iter().enumerate().rev() {
            match &item.command {
                Command::Glyphs(run) => {
                    let placed: Vec<_> = run.positioned().collect();
                    for (glyph, (at, item)) in placed.into_iter().enumerate().rev() {
                        let pen = at.x - item.dx;
                        let area = Rect {
                            x: pen.min(pen + item.advance),
                            y: run.origin.y - run.size,
                            width: item.advance.abs(),
                            height: run.size * 1.25,
                        };
                        if area.contains(x, y) {
                            hits.push(Hit {
                                item: index,
                                glyph: Some(glyph),
                            });
                        }
                    }
                }
                Command::Path(shape) => {
                    if bounds(&shape.segments).is_some_and(|b| b.contains(x, y)) {
                        hits.push(Hit {
                            item: index,
                            glyph: None,
                        });
                    }
                }
                Command::Frame(area) | Command::Box(area) => {
                    if area.contains(x, y) {
                        hits.push(Hit {
                            item: index,
                            glyph: None,
                        });
                    }
                }
                Command::Unknown(_) => {}
            }
        }
        hits
    }
}

/// An item that a point is in, from [`DisplayList::hit`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Hit {
    /// The item's index in the list.
    pub item: usize,
    /// The glyph's index in its run, for a glyph run.
    pub glyph: Option<usize>,
}

/// The bounds of a path's points, control points included.
fn bounds(segments: &[Segment]) -> Option<Rect> {
    let mut points = segments.iter().flat_map(|segment| match *segment {
        Segment::MoveTo(p) | Segment::LineTo(p) => vec![p],
        Segment::CubicTo(a, b, c) => vec![a, b, c],
        Segment::Close => vec![],
    });
    let first = points.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in points {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Some(Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// A length in points, as `f32`.
fn len(abs: Abs) -> f32 {
    abs.to_pt() as f32
}

fn rect(x: Abs, y: Abs, width: Abs, height: Abs) -> Rect {
    Rect {
        x: len(x),
        y: len(y),
        width: len(width),
        height: len(height),
    }
}

fn color(paint: &Paint) -> Color {
    let Paint::Solid(color) = paint;
    *color
}

fn glyphs(run: &GlyphRun, x: Abs, y: Abs) -> Glyphs {
    Glyphs {
        origin: Point {
            x: len(x),
            y: len(y),
        },
        font: run.font,
        size: len(run.size),
        fill: color(&run.fill),
        text: run.text.clone(),
        glyphs: run
            .glyphs
            .iter()
            .map(|glyph| GlyphItem {
                id: glyph.id,
                advance: len(glyph.x_advance.at(run.size)),
                dx: len(glyph.x_offset.at(run.size)),
                // `0.0 - y`, not `-y`, so that no offset is `-0.0`.
                dy: 0.0 - len(glyph.y_offset.at(run.size)),
                text: glyph.range.clone(),
                source: glyph.source.clone(),
            })
            .collect(),
    }
}

fn path(shape: &Shape, x: Abs, y: Abs) -> PathShape {
    let at = |p: folio_component::Point| Point {
        x: len(x + p.x),
        y: len(y + p.y),
    };
    PathShape {
        segments: shape
            .path
            .0
            .iter()
            .map(|item| match *item {
                PathItem::MoveTo(p) => Segment::MoveTo(at(p)),
                PathItem::LineTo(p) => Segment::LineTo(at(p)),
                PathItem::CubicTo(a, b, c) => Segment::CubicTo(at(a), at(b), at(c)),
                PathItem::ClosePath => Segment::Close,
            })
            .collect(),
        fill: shape.fill.as_ref().map(color),
        fill_rule: shape.fill_rule,
        stroke: shape.stroke.as_ref().map(|stroke| Stroke {
            color: color(&stroke.paint),
            thickness: len(stroke.thickness),
            cap: stroke.cap,
            join: stroke.join,
            miter_limit: stroke.miter_limit.get() as f32,
            dash: stroke.dash.as_ref().map(|dash| {
                (
                    dash.array.iter().map(|&d| len(d)).collect(),
                    len(dash.phase),
                )
            }),
        }),
    }
}
