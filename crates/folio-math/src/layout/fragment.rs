// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/fragment/mod.rs
// and crates/typst-layout/src/math/fragment/glyph.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Math fragments: glyphs, stretched and assembled as they must be, and
//! frames of laid-out parts, with what placing them next to each other
//! needs (class, italic correction, accent attachment, kerns).
//!
//! Lengths are in points, as `f64`, and frames are y-down from their top.

use ecow::EcoString;
use folio_syntax::Span;
use unicode_math_class::MathClass;

use super::MathSize;
use super::font::{Corner, Features, MathFont};

/// The most times an assembly repeats its extenders.
const MAX_REPEATS: usize = 1024;

/// A box of laid-out maths: its items, positioned from its top-left corner.
#[derive(Debug, Clone, Default)]
pub(crate) struct Frame {
    pub width: f64,
    pub height: f64,
    /// The baseline, from the top.
    pub baseline: f64,
    pub items: Vec<(f64, f64, FrameItem)>,
}

/// An item of a frame.
#[derive(Debug, Clone)]
pub(crate) enum FrameItem {
    /// A nested frame, by its top-left corner.
    Frame(Frame),
    /// A run of glyphs, by the start of its baseline.
    Run(Run),
    /// A filled rectangle, by its top-left corner.
    Rule { width: f64, height: f64, span: Span },
}

/// Glyphs in the math font at one size, on one baseline.
#[derive(Debug, Clone)]
pub(crate) struct Run {
    /// The font size, in points.
    pub size: f64,
    /// The text the glyphs show.
    pub text: EcoString,
    /// The node they come from.
    pub span: Span,
    pub glyphs: Vec<RunGlyph>,
}

/// A glyph of a run, in ems.
#[derive(Debug, Copy, Clone)]
pub(crate) struct RunGlyph {
    pub id: u16,
    /// The bytes of the run's text it shows.
    pub range: (u32, u32),
    pub advance: f64,
    pub x_offset: f64,
    pub y_offset: f64,
}

impl Frame {
    /// An empty frame of a size, with its baseline at its bottom.
    pub(crate) fn new(width: f64, height: f64) -> Self {
        Self {
            width,
            height,
            baseline: height,
            items: Vec::new(),
        }
    }

    pub(crate) fn ascent(&self) -> f64 {
        self.baseline
    }

    pub(crate) fn descent(&self) -> f64 {
        self.height - self.baseline
    }

    /// Places a frame with its top-left corner at a point.
    pub(crate) fn push_frame(&mut self, x: f64, y: f64, frame: Frame) {
        if !frame.items.is_empty() {
            self.items.push((x, y, FrameItem::Frame(frame)));
        }
    }

    /// Places a rule.
    pub(crate) fn push_rule(&mut self, x: f64, y: f64, width: f64, height: f64, span: Span) {
        self.items.push((
            x,
            y,
            FrameItem::Rule {
                width,
                height,
                span,
            },
        ));
    }
}

/// A laid-out part of a formula.
#[derive(Debug, Clone)]
pub(crate) enum MathFragment {
    Glyph(GlyphFragment),
    Frame(FrameFragment),
    /// Horizontal space.
    Space(f64),
}

impl MathFragment {
    pub(crate) fn width(&self) -> f64 {
        match self {
            Self::Glyph(glyph) => glyph.width,
            Self::Frame(fragment) => fragment.frame.width,
            Self::Space(amount) => *amount,
        }
    }

    pub(crate) fn height(&self) -> f64 {
        match self {
            Self::Glyph(glyph) => glyph.ascent + glyph.descent,
            Self::Frame(fragment) => fragment.frame.height,
            Self::Space(_) => 0.0,
        }
    }

    pub(crate) fn ascent(&self) -> f64 {
        match self {
            Self::Glyph(glyph) => glyph.ascent,
            Self::Frame(fragment) => fragment.frame.ascent(),
            Self::Space(_) => 0.0,
        }
    }

    pub(crate) fn descent(&self) -> f64 {
        match self {
            Self::Glyph(glyph) => glyph.descent,
            Self::Frame(fragment) => fragment.frame.descent(),
            Self::Space(_) => 0.0,
        }
    }

    /// The ascent of what scripts attach to: of the base of an accent, not
    /// the accent.
    pub(crate) fn base_ascent(&self) -> f64 {
        match self {
            Self::Frame(fragment) => fragment.base_ascent,
            _ => self.ascent(),
        }
    }

    pub(crate) fn base_descent(&self) -> f64 {
        match self {
            Self::Frame(fragment) => fragment.base_descent,
            _ => self.descent(),
        }
    }

    pub(crate) fn class(&self) -> MathClass {
        match self {
            Self::Glyph(glyph) => glyph.class,
            Self::Frame(fragment) => fragment.class,
            Self::Space(_) => MathClass::Space,
        }
    }

    pub(crate) fn math_size(&self) -> Option<MathSize> {
        match self {
            Self::Glyph(glyph) => Some(glyph.math_size),
            Self::Frame(fragment) => Some(fragment.math_size),
            Self::Space(_) => None,
        }
    }

    /// The font size it is set at, if it is set at one.
    pub(crate) fn font_size(&self) -> Option<f64> {
        match self {
            Self::Glyph(glyph) => Some(glyph.size),
            Self::Frame(fragment) => Some(fragment.font_size),
            Self::Space(_) => None,
        }
    }

    /// Whether it is like a letter, so that scripts are placed by the
    /// font's shifts alone rather than by its height.
    pub(crate) fn is_text_like(&self) -> bool {
        match self {
            Self::Glyph(glyph) => !glyph.extended,
            Self::Frame(fragment) => fragment.text_like,
            Self::Space(_) => false,
        }
    }

    pub(crate) fn italics_correction(&self) -> f64 {
        match self {
            Self::Glyph(glyph) => glyph.italics,
            Self::Frame(fragment) => fragment.italics,
            Self::Space(_) => 0.0,
        }
    }

    /// Where a top and a bottom accent attach, from the left.
    pub(crate) fn accent_attach(&self) -> (f64, f64) {
        match self {
            Self::Glyph(glyph) => glyph.accent_attach,
            Self::Frame(fragment) => fragment.accent_attach,
            Self::Space(_) => (self.width() / 2.0, self.width() / 2.0),
        }
    }

    /// The kern at a corner at a height, in points; zero where the font
    /// gives none.
    pub(crate) fn kern_at_height(&self, font: &MathFont, corner: Corner, height: f64) -> f64 {
        let Self::Glyph(glyph) = self else {
            return 0.0;
        };
        // An assembly kerns by its first or last part.
        let id = match &glyph.shape {
            Shape::Vertical(parts) => match corner {
                Corner::TopLeft | Corner::TopRight => parts.last().map(|p| p.0),
                _ => parts.first().map(|p| p.0),
            },
            Shape::Horizontal(glyphs) => match corner {
                Corner::TopRight | Corner::BottomRight => glyphs.last().map(|g| g.id),
                _ => glyphs.first().map(|g| g.id),
            },
        };
        let Some(id) = id else { return 0.0 };
        font.kern_at_height(id, corner, height / glyph.size) * glyph.size
    }

    pub(crate) fn into_frame(self) -> Frame {
        match self {
            Self::Glyph(glyph) => glyph.into_frame(),
            Self::Frame(fragment) => fragment.frame,
            Self::Space(amount) => Frame::new(amount, 0.0),
        }
    }
}

impl From<GlyphFragment> for MathFragment {
    fn from(glyph: GlyphFragment) -> Self {
        Self::Glyph(glyph)
    }
}

impl From<FrameFragment> for MathFragment {
    fn from(fragment: FrameFragment) -> Self {
        Self::Frame(fragment)
    }
}

/// A frame of laid-out parts, as one fragment.
#[derive(Debug, Clone)]
pub(crate) struct FrameFragment {
    pub frame: Frame,
    pub font_size: f64,
    pub class: MathClass,
    pub math_size: MathSize,
    pub base_ascent: f64,
    pub base_descent: f64,
    pub italics: f64,
    pub accent_attach: (f64, f64),
    pub text_like: bool,
}

impl FrameFragment {
    pub(crate) fn new(frame: Frame, font_size: f64, class: MathClass, math_size: MathSize) -> Self {
        let attach = frame.width / 2.0;
        Self {
            base_ascent: frame.ascent(),
            base_descent: frame.descent(),
            frame,
            font_size,
            class,
            math_size,
            italics: 0.0,
            accent_attach: (attach, attach),
            text_like: false,
        }
    }
}

/// The glyphs of a glyph fragment.
#[derive(Debug, Clone)]
pub(crate) enum Shape {
    /// Glyphs side by side on the baseline.
    Horizontal(Vec<RunGlyph>),
    /// The parts of a vertical assembly, each with its baseline from the
    /// fragment's top.
    Vertical(Vec<(u16, f64)>),
}

/// Text in the math font, usually one character, which may be stretched.
#[derive(Debug, Clone)]
pub(crate) struct GlyphFragment {
    pub text: EcoString,
    pub span: Span,
    /// The font size, in points.
    pub size: f64,
    pub shape: Shape,
    pub width: f64,
    pub ascent: f64,
    pub descent: f64,
    /// Where its glyphs' baseline is, from its top.
    pub draw_y: f64,
    pub italics: f64,
    pub accent_attach: (f64, f64),
    pub math_size: MathSize,
    pub class: MathClass,
    pub extended: bool,
}

/// How far a glyph must reach along an axis.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Stretch {
    pub axis: Axis,
    /// The length it should have, in points.
    pub target: f64,
    /// How much shorter it may be.
    pub short_fall: f64,
}

/// An axis to stretch along.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Axis {
    X,
    Y,
}

impl GlyphFragment {
    /// Text shaped in the font with math features, or `None` when the font
    /// has no glyph for one of its characters.
    pub(crate) fn new(
        font: &MathFont,
        text: &str,
        span: Span,
        class: MathClass,
        math_size: MathSize,
        size: f64,
        features: Features,
    ) -> Option<Self> {
        let shaped = font.shape(text, features)?;
        let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        let glyphs: Vec<RunGlyph> = shaped
            .iter()
            .enumerate()
            .map(|(i, g)| RunGlyph {
                id: g.id,
                range: (
                    g.cluster,
                    shaped[i + 1..]
                        .iter()
                        .map(|n| n.cluster)
                        .find(|&c| c > g.cluster)
                        .unwrap_or(len),
                ),
                advance: g.advance,
                x_offset: g.x_offset,
                y_offset: g.y_offset,
            })
            .collect();
        if glyphs.is_empty() {
            return None;
        }
        let mut fragment = Self {
            text: text.into(),
            span,
            size,
            shape: Shape::Horizontal(glyphs),
            width: 0.0,
            ascent: 0.0,
            descent: 0.0,
            draw_y: 0.0,
            italics: 0.0,
            accent_attach: (0.0, 0.0),
            math_size,
            class,
            extended: false,
        };
        fragment.update(font);
        Some(fragment)
    }

    /// Text shaped and stretched to reach as far as `stretch` asks: by a
    /// larger variant, by an assembly of parts, or by shaping it again
    /// without `flac` and `ssty` when only those forms cannot grow.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stretched(
        font: &MathFont,
        text: &str,
        span: Span,
        class: MathClass,
        math_size: MathSize,
        size: f64,
        features: Features,
        stretch: Stretch,
    ) -> Option<Self> {
        let mut glyph = Self::new(font, text, span, class, math_size, size, features)?;
        let mut action = glyph.decide(font, stretch);
        if action == Action::Fallback {
            MathFont::fallback_features(features, |features| {
                let Some(new) = Self::new(font, text, span, class, math_size, size, features)
                else {
                    return false;
                };
                match new.decide(font, stretch) {
                    Action::Fallback => false,
                    other => {
                        glyph = new;
                        action = other;
                        true
                    }
                }
            });
        }
        if action == Action::Stretch {
            glyph.stretch(font, stretch);
            if stretch.axis == Axis::Y {
                glyph.center_on_axis(font);
            }
        }
        Some(glyph)
    }

    /// The range of all of its text.
    fn whole(&self) -> (u32, u32) {
        (0, u32::try_from(self.text.len()).unwrap_or(u32::MAX))
    }

    fn first(&self) -> Option<u16> {
        match &self.shape {
            Shape::Horizontal(glyphs) => glyphs.first().map(|g| g.id),
            Shape::Vertical(parts) => parts.first().map(|p| p.0),
        }
    }

    /// Sets the metrics from its glyphs, as they are after shaping or after
    /// a variant replaced its glyph.
    fn update(&mut self, font: &MathFont) {
        let size = self.size;
        let Shape::Horizontal(glyphs) = &mut self.shape else {
            return;
        };
        let Some(first) = glyphs.first().map(|g| g.id) else {
            return;
        };
        let info = font.glyph(first);
        let extended = info.extended;
        // The italic correction of the last glyph, added to its advance
        // unless it is an extended shape.
        let last = glyphs.last().map_or(first, |g| g.id);
        let italics = font.glyph(last).italics.unwrap_or(0.0);
        let mut ascent = f64::NEG_INFINITY;
        let mut descent = f64::NEG_INFINITY;
        let mut x = 0.0;
        for g in glyphs.iter() {
            let gi = font.glyph(g.id);
            ascent = ascent.max(gi.ascent + g.y_offset);
            descent = descent.max(gi.descent - g.y_offset);
            x += g.advance;
        }
        let width = x;
        if !extended && let Some(g) = glyphs.last_mut() {
            g.advance += italics;
        }
        let width_em = width;
        let italics_pt = italics * size;
        let top = info
            .accent
            .map_or((width_em * size + italics_pt) / 2.0, |a| a * size);
        let bottom = (width_em * size - italics_pt) / 2.0;
        self.width = glyphs.iter().map(|g| g.advance).sum::<f64>() * size;
        self.ascent = if ascent.is_finite() {
            ascent * size
        } else {
            0.0
        };
        self.descent = if descent.is_finite() {
            descent * size
        } else {
            0.0
        };
        self.draw_y = self.ascent;
        self.italics = italics_pt;
        self.accent_attach = (top, bottom);
        self.extended = extended;
    }

    /// Centres it vertically on the math axis, as large operators and
    /// stretched delimiters are.
    pub(crate) fn center_on_axis(&mut self, font: &MathFont) {
        let height = self.ascent + self.descent;
        let axis = font.constant(super::font::Constant::AxisHeight) * self.size;
        let ascent = height / 2.0 + axis;
        self.ascent = ascent;
        self.descent = height - ascent;
    }

    pub(crate) fn into_frame(self) -> Frame {
        let mut frame = Frame::new(self.width, self.ascent + self.descent);
        frame.baseline = self.ascent;
        match self.shape {
            Shape::Horizontal(glyphs) => frame.items.push((
                0.0,
                self.draw_y,
                FrameItem::Run(Run {
                    size: self.size,
                    text: self.text,
                    span: self.span,
                    glyphs,
                }),
            )),
            Shape::Vertical(parts) => {
                for (id, y) in parts {
                    frame.items.push((
                        0.0,
                        y,
                        FrameItem::Run(Run {
                            size: self.size,
                            text: self.text.clone(),
                            span: self.span,
                            glyphs: vec![RunGlyph {
                                id,
                                range: (0, u32::try_from(self.text.len()).unwrap_or(u32::MAX)),
                                advance: 0.0,
                                x_offset: 0.0,
                                y_offset: 0.0,
                            }],
                        }),
                    ));
                }
            }
        }
        frame
    }

    /// How far it reaches along an axis, without the italic correction
    /// added to its width.
    fn stretch_advance(&self, axis: Axis) -> f64 {
        match axis {
            Axis::X if !self.extended => self.width - self.italics,
            Axis::X => self.width,
            Axis::Y => self.ascent + self.descent,
        }
    }

    /// Whether and how it must be stretched.
    fn decide(&self, font: &MathFont, stretch: Stretch) -> Action {
        let Some(id) = self.first() else {
            return Action::Keep;
        };
        let info = font.glyph(id);
        let constructible = match stretch.axis {
            Axis::X => info.horizontal.is_some(),
            Axis::Y => info.vertical.is_some(),
        };
        if constructible {
            return Action::Stretch;
        }
        // Combining marks have no advance, so their ink counts too.
        let ink = match stretch.axis {
            Axis::X => info.ink_width,
            Axis::Y => info.ink_height,
        } * self.size;
        let advance = self.stretch_advance(stretch.axis).max(ink);
        if stretch.target - stretch.short_fall <= advance {
            Action::Keep
        } else {
            Action::Fallback
        }
    }

    /// Stretches it to the target, as far as its variants and assembly
    /// reach.
    fn stretch(&mut self, font: &MathFont, stretch: Stretch) {
        let advance = self.stretch_advance(stretch.axis);
        let short_target = stretch.target - stretch.short_fall;
        if short_target <= advance {
            return;
        }
        let Some(id) = self.first() else { return };
        let info = font.glyph(id);
        let construction = match stretch.axis {
            Axis::X => info.horizontal.clone(),
            Axis::Y => info.vertical.clone(),
        };
        let Some(construction) = construction else {
            return;
        };

        // The first pre-made variant that reaches far enough.
        let mut best_id = id;
        let mut best_advance = advance;
        for &(variant, variant_advance) in &construction.variants {
            best_id = variant;
            best_advance = variant_advance * self.size;
            if short_target <= best_advance {
                break;
            }
        }

        let Some(assembly) = construction
            .assembly
            .filter(|_| short_target > best_advance)
        else {
            let advance = font.glyph(best_id).advance;
            self.shape = Shape::Horizontal(vec![RunGlyph {
                id: best_id,
                range: self.whole(),
                advance,
                x_offset: 0.0,
                y_offset: 0.0,
            }]);
            self.update(font);
            return;
        };
        self.assemble(font, &assembly, stretch.target, stretch.axis);
    }

    /// Assembles it from parts to reach `target`.
    fn assemble(
        &mut self,
        font: &MathFont,
        assembly: &super::font::Assembly,
        target: f64,
        axis: Axis,
    ) {
        let size = self.size;
        let min_overlap = font.min_overlap() * size;
        let parts = |repeat: usize| {
            assembly.parts.iter().flat_map(move |part| {
                let count = if part.extender { repeat } else { 1 };
                std::iter::repeat_n(*part, count)
            })
        };

        // How often the extenders repeat, and how far the parts are spread
        // apart: 0 is the most overlap, 1 the least.
        let mut full;
        let mut ratio;
        let mut repeat = 0;
        loop {
            full = 0.0;
            ratio = 0.0;
            let mut growable = 0.0;
            let mut iter = parts(repeat).peekable();
            while let Some(part) = iter.next() {
                let mut advance = part.full * size;
                if let Some(next) = iter.peek() {
                    let max_overlap = part.end.min(next.start) * size;
                    advance -= max_overlap;
                    growable += (max_overlap - min_overlap).max(0.0);
                }
                full += advance;
            }
            if full < target && growable > 0.0 {
                let delta = target - full;
                ratio = (delta / growable).min(1.0);
                full += ratio * growable;
            }
            if target <= full || repeat >= MAX_REPEATS {
                break;
            }
            repeat += 1;
        }

        let mut advances = Vec::new();
        let mut iter = parts(repeat).peekable();
        while let Some(part) = iter.next() {
            let mut advance = part.full * size;
            if let Some(next) = iter.peek() {
                let max_overlap = part.end.min(next.start) * size;
                advance -= max_overlap;
                advance += ratio * (max_overlap - min_overlap).max(0.0);
            }
            advances.push((part.glyph, advance));
        }

        match axis {
            Axis::X => {
                let whole = self.whole();
                let (mut ascent, mut descent) = (0.0f64, 0.0f64);
                let glyphs = advances
                    .iter()
                    .map(|&(id, advance)| {
                        let info = font.glyph(id);
                        ascent = ascent.max(info.ascent);
                        descent = descent.max(info.descent);
                        RunGlyph {
                            id,
                            range: whole,
                            advance: advance / size,
                            x_offset: 0.0,
                            y_offset: 0.0,
                        }
                    })
                    .collect();
                self.shape = Shape::Horizontal(glyphs);
                self.width = full;
                self.ascent = ascent * size;
                self.descent = descent * size;
                self.draw_y = self.ascent;
                self.accent_attach = (full / 2.0, full / 2.0);
            }
            Axis::Y => {
                // Parts are stacked from the bottom, each aligned by the
                // bottom of its ink.
                let mut y = full;
                let mut width = 0.0f64;
                let parts = advances
                    .iter()
                    .map(|&(id, advance)| {
                        let info = font.glyph(id);
                        width = width.max(info.advance);
                        let baseline = y - info.descent * size;
                        y -= advance;
                        (id, baseline)
                    })
                    .collect();
                self.shape = Shape::Vertical(parts);
                self.width = width * size;
                self.ascent = full;
                self.descent = 0.0;
                self.draw_y = 0.0;
            }
        }
        self.italics = assembly.italics * size;
        self.extended = true;
    }
}

/// What to do with a glyph that may have to be stretched.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Action {
    Keep,
    Stretch,
    Fallback,
}
