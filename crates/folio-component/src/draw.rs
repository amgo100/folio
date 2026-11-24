//! Graphic primitives as they appear in fragments: colours, paints, strokes,
//! paths and shapes (SPEC §9.1, §9.4).
//!
//! These are the resolved, positioned forms that layout and the renderers
//! exchange. Source-level graphic elements (`#arrow`, `#shape`) and their
//! stylesheet properties are lowered to them before they reach a fragment.

use ecow::EcoVec;

use crate::geom::{Abs, Point, Ratio};

/// An sRGB colour with straight alpha, eight bits per channel.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Opacity: 0 is transparent, 255 opaque.
    pub a: u8,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour with alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// How a shape or glyph is filled or stroked.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Paint {
    /// A single colour.
    Solid(Color),
}

impl From<Color> for Paint {
    fn from(color: Color) -> Self {
        Self::Solid(color)
    }
}

/// The shape at the open ends of a stroked path.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum LineCap {
    /// Square, ending at the end point.
    #[default]
    Butt,
    /// Rounded, extending past the end point by half the width.
    Round,
    /// Square, extending past the end point by half the width.
    Square,
}

/// The shape at the corners of a stroked path.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum LineJoin {
    /// Pointed, cut off beyond the miter limit.
    #[default]
    Miter,
    /// Rounded.
    Round,
    /// Cut off.
    Bevel,
}

/// A dash pattern: alternating dash and gap lengths, and the offset into the
/// pattern at which the stroke starts.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Dash {
    /// Dash and gap lengths, alternating, starting with a dash.
    pub array: EcoVec<Abs>,
    /// How far into the pattern the stroke starts.
    pub phase: Abs,
}

/// How a path's outline is drawn.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Stroke {
    /// The paint.
    pub paint: Paint,
    /// The line width.
    pub thickness: Abs,
    /// The shape of open ends.
    pub cap: LineCap,
    /// The shape of corners.
    pub join: LineJoin,
    /// The ratio of miter length to line width beyond which a miter join is
    /// bevelled.
    pub miter_limit: Ratio,
    /// The dash pattern; `None` draws a solid line.
    pub dash: Option<Dash>,
}

impl Stroke {
    /// A solid stroke with butt caps and miter joins.
    pub fn solid(paint: impl Into<Paint>, thickness: Abs) -> Self {
        Self {
            paint: paint.into(),
            thickness,
            cap: LineCap::default(),
            join: LineJoin::default(),
            miter_limit: Ratio::new(4.0),
            dash: None,
        }
    }
}

/// One segment of a path.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum PathItem {
    /// Starts a new subpath at a point.
    MoveTo(Point),
    /// A straight line to a point.
    LineTo(Point),
    /// A cubic Bézier curve through two control points to an end point.
    CubicTo(Point, Point, Point),
    /// Closes the current subpath with a line to its start.
    ClosePath,
}

/// A path made of straight and cubic segments.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Path(pub EcoVec<PathItem>);

impl Path {
    /// An empty path.
    pub fn new() -> Self {
        Self::default()
    }

    /// A rectangle with its top-left corner at the origin.
    pub fn rect(width: Abs, height: Abs) -> Self {
        let mut path = Self::new();
        path.move_to(Point::zero());
        path.line_to(Point::new(width, Abs::zero()));
        path.line_to(Point::new(width, height));
        path.line_to(Point::new(Abs::zero(), height));
        path.close_path();
        path
    }

    /// Starts a new subpath.
    pub fn move_to(&mut self, p: Point) {
        self.0.push(PathItem::MoveTo(p));
    }

    /// Adds a straight segment.
    pub fn line_to(&mut self, p: Point) {
        self.0.push(PathItem::LineTo(p));
    }

    /// Adds a cubic segment.
    pub fn cubic_to(&mut self, c1: Point, c2: Point, end: Point) {
        self.0.push(PathItem::CubicTo(c1, c2, end));
    }

    /// Closes the current subpath.
    pub fn close_path(&mut self) {
        self.0.push(PathItem::ClosePath);
    }
}

/// Which areas a self-intersecting path fills.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum FillRule {
    /// Areas with a non-zero winding number.
    #[default]
    NonZero,
    /// Areas crossed an odd number of times.
    EvenOdd,
}

/// A path with a fill, a stroke or both.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Shape {
    /// The geometry.
    pub path: Path,
    /// The fill, if any.
    pub fill: Option<Paint>,
    /// How the fill treats self-intersections.
    pub fill_rule: FillRule,
    /// The outline, if any.
    pub stroke: Option<Stroke>,
}

impl Shape {
    /// A filled path without an outline.
    pub fn filled(path: Path, fill: impl Into<Paint>) -> Self {
        Self {
            path,
            fill: Some(fill.into()),
            fill_rule: FillRule::default(),
            stroke: None,
        }
    }

    /// A stroked path without a fill.
    pub fn stroked(path: Path, stroke: Stroke) -> Self {
        Self {
            path,
            fill: None,
            fill_rule: FillRule::default(),
            stroke: Some(stroke),
        }
    }
}
