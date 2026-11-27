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
/// instances it made: a key of the instance's origin, the loop iterations
/// and closure calls that made it, so that the instance made by the same
/// iteration pairs across the variants of a numbering group (SPEC §6.1).
/// It stays the same across edits that leave its node's span and origin
/// alone, such as an edit to another paragraph or an inserted call.
/// Locations are compared, never ordered or written to output: a reparse
/// may renumber spans (SPEC §2.3, §2.4).
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Location {
    /// The node that made the instance: its markup or its call.
    pub source: SourceRef,
    /// Which of the node's instances it is: a key that only the core
    /// interprets.
    pub instance: u64,
}

impl Location {
    /// The location of no instance, as a node that no instance names has.
    pub const DETACHED: Self = Self {
        source: SourceRef::DETACHED,
        instance: 0,
    };

    /// The instance of a box whose maker knew only its node's
    /// [`SourceRef`], not which instance of it the box is
    /// ([`crate::Fragment::with_source`]). The core issues no location
    /// with it.
    pub const UNKNOWN_INSTANCE: u64 = u64::MAX;
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
    /// Which element instance each glyph comes from, for provenance (SPEC
    /// §6.4): the index of the first glyph from an instance, with its
    /// location, in the order of the glyphs, wherever the instance changes.
    /// A glyph before the first entry, or of an entry with
    /// [`Location::DETACHED`], comes from the instance of the box around
    /// the run. A run in one instance, such as a run of a formula, has no
    /// entries.
    pub instances: EcoVec<(u32, Location)>,
}

impl GlyphRun {
    /// The element instance the glyph at `index` comes from, or
    /// [`Location::DETACHED`] for that of the box around the run.
    pub fn instance(&self, index: usize) -> Location {
        let i = self
            .instances
            .partition_point(|&(first, _)| first as usize <= index);
        i.checked_sub(1)
            .map_or(Location::DETACHED, |i| self.instances[i].1)
    }

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
