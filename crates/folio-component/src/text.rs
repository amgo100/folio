//! Shaped text as it appears in fragments: [`GlyphRun`]s of positioned
//! glyphs, each traced back to its source (SPEC §1.5 span map, §6.4).

use std::ops::Range;

use ecow::{EcoString, EcoVec};

use crate::draw::Paint;
use crate::geom::{Abs, Em};

/// A font, as an index into the core's font table.
///
/// Fonts are loaded once per process and shared between compiles (SPEC
/// §2.3), so fragments refer to them by number instead of holding them.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FontId(pub u32);

/// An opaque reference to a source node, issued by the core.
///
/// Every glyph and graphic carries the reference of the node it came from,
/// so the span map can lead from output back to source and the leak check
/// can tell which node produced what (SPEC §6.4). Components copy the
/// references they are given and never make them up.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SourceRef(pub u64);

impl SourceRef {
    /// A reference to no node, for output generated without a source, such
    /// as a rule drawn by a template.
    pub const DETACHED: Self = Self(0);

    /// Whether this refers to no node.
    pub fn is_detached(self) -> bool {
        self == Self::DETACHED
    }
}

/// A run of glyphs in one font, size and paint, on one baseline.
///
/// The run's origin is on its baseline, at the start of its first glyph.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct GlyphRun {
    /// The font.
    pub font: FontId,
    /// The font size.
    pub size: Abs,
    /// The paint the glyphs are filled with.
    pub fill: Paint,
    /// The text the glyphs were shaped from, for text extraction and
    /// accessibility.
    pub text: EcoString,
    /// The glyphs, in visual order.
    pub glyphs: EcoVec<Glyph>,
}

impl GlyphRun {
    /// The width of the run: the sum of the glyph advances.
    pub fn width(&self) -> Abs {
        self.glyphs.iter().fold(Abs::zero(), |width, glyph| {
            width + glyph.x_advance.at(self.size)
        })
    }
}

/// A shaped glyph.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Glyph {
    /// The glyph's id in its font.
    pub id: u16,
    /// How far the pen moves after the glyph.
    pub x_advance: Em,
    /// The horizontal offset from the pen position.
    pub x_offset: Em,
    /// The vertical offset from the baseline, upwards.
    pub y_offset: Em,
    /// The bytes of the run's text that this glyph shows.
    pub range: Range<u32>,
    /// The source node the glyph came from, and the byte offset of its text
    /// within that node.
    pub source: (SourceRef, u32),
}
