//! Shaped text as it appears in fragments: [`GlyphRun`]s of positioned
//! glyphs, each traced back to its source (SPEC §1.5 span map, §6.4).
//!
//! A glyph's [`Glyph::source`] names the source node its text came from and
//! the bytes of the node's text that it shows. The core resolves that pair
//! to a file and a byte range of the file; components copy what they are
//! given.

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

/// The location of an element instance (SPEC §1.5): which instance of
/// the document tree an output or a later stage concerns, issued by the
/// core.
///
/// A span is not enough to say that: every iteration of a loop, and every
/// call of a closure, makes its elements from the same source node. A
/// location is that node's reference with a number that tells apart the
/// instances it made, counted in the order of the document tree. It stays
/// the same across edits that leave its node's span and the instances made
/// before it from the same node alone, such as an edit to another
/// paragraph. Locations are compared, never ordered or written to output:
/// a reparse may renumber spans (SPEC §2.3, §2.4).
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Location {
    /// The node that made the instance: its markup or its call.
    pub source: SourceRef,
    /// Which of the node's instances it is: a number that only the core
    /// interprets.
    pub instance: u32,
}

impl Location {
    /// The location of no instance, as a node that no instance names has.
    pub const DETACHED: Self = Self {
        source: SourceRef::DETACHED,
        instance: 0,
    };
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
    /// The bytes of the run's text that this glyph shows: more than one
    /// character for a ligature.
    pub range: Range<u32>,
    /// The source node the glyph came from, and the bytes of the node's
    /// text that it shows. A glyph whose text runs across two nodes names
    /// the first, with the part of its text that the first gives; a hyphen
    /// that a line break inserts shows no text of its node, and names the
    /// empty range where the break is. A range that runs past the node's
    /// text stands for the whole node.
    pub source: (SourceRef, Range<u32>),
}
