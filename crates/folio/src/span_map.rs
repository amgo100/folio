//! Span maps (SPEC §1.5, §12.2, §12.4): from the glyphs, shapes and boxes of
//! fragments back to where their source is written.
//!
//! Every glyph of a page names the source node its text came from and the
//! bytes of the node's text it shows; every shape and fragment names the
//! node it is drawn for, and every object the element it is the box of.
//! [`locate_glyph`], [`locate_fragment`] and [`locate_object`] resolve
//! those to a file, a byte range and lines and columns, against the files
//! of the same revision. A ligature resolves to all the characters it
//! shows, a glyph of right-to-left text to its own characters wherever the
//! line places it, and a hyphen that a break inserts to the empty range
//! where the break is. How each kind of node maps is described in
//! [`folio_core::span_map`].
//!
//! ```
//! use folio::{Target, World, span_map};
//!
//! let mut world = World::new(
//!     "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
//! )
//! .unwrap();
//! world.set_source("main.fol", "A *bold* claim.\n").unwrap();
//! let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
//!
//! let glyphs = span_map::glyphs(&pages.pages[0].fragment);
//! let b = glyphs.iter().find(|g| g.text() == "b").unwrap();
//! let location = span_map::locate_glyph(&world, b.glyph).unwrap();
//! assert_eq!(location.bytes, 3..4);
//! assert_eq!((location.start.line, location.start.column), (1, 4));
//! ```
//!
//! A span names a node in one revision of its file: resolve a fragment
//! against the world it was compiled from, before the next edit. What lasts
//! across edits is the node's identity, [`World::node_id`] of
//! [`span`].
//!
//! [`World::node_id`]: crate::World::node_id

use std::ops::Range;

use folio_component::{Fragment, FragmentItem, Glyph, GlyphRun, ObjectBox, Point, SourceRef};
use folio_syntax::DiagSpan;

pub use folio_core::span_map::{source_ref, span, text_range};

use crate::diag::{Files, Location, locate};

/// Resolves the bytes `within` of the text that a source node gave (as a
/// glyph names them) to the part of its file that shows them, or `None`
/// when the reference is detached or not of a source among `files` in its
/// current revision.
pub fn locate_source(files: &dyn Files, source: SourceRef, within: Range<u32>) -> Option<Location> {
    let span = span(source);
    let file = files.source(span.id()?)?;
    let bytes = text_range(&file, span, within.start as usize..within.end as usize)?;
    locate(files, DiagSpan::from_range(file.id(), bytes))
}

/// Where the text a glyph shows is written.
pub fn locate_glyph(files: &dyn Files, glyph: &Glyph) -> Option<Location> {
    let (source, within) = &glyph.source;
    locate_source(files, *source, within.clone())
}

/// Where the node that a fragment is the box of is written: the whole
/// node, such as an element's call or markup.
pub fn locate_fragment(files: &dyn Files, fragment: &Fragment) -> Option<Location> {
    locate(files, span(fragment.source).into())
}

/// Where the element that an object is the box of is written, as for a
/// fragment: the whole element's call or markup.
pub fn locate_object(files: &dyn Files, object: &ObjectBox) -> Option<Location> {
    locate(files, span(object.location.source).into())
}

/// A glyph of a fragment, with where it is drawn.
#[derive(Debug, Copy, Clone)]
pub struct Placed<'a> {
    /// The pen position of the glyph on its baseline, before the glyph's
    /// own offsets, from the top-left corner of the outermost fragment.
    pub pos: Point,
    /// The run it belongs to.
    pub run: &'a GlyphRun,
    /// The glyph.
    pub glyph: &'a Glyph,
}

impl<'a> Placed<'a> {
    /// The text the glyph shows, from its run's text: more than one
    /// character for a ligature.
    pub fn text(&self) -> &'a str {
        let range = &self.glyph.range;
        self.run
            .text
            .get(range.start as usize..range.end as usize)
            .unwrap_or_default()
    }
}

/// Every glyph of a fragment and of the fragments it holds, in painting
/// order, each in visual order within its run.
pub fn glyphs(fragment: &Fragment) -> Vec<Placed<'_>> {
    fn walk<'a>(fragment: &'a Fragment, at: Point, out: &mut Vec<Placed<'a>>) {
        for (pos, item) in &fragment.items {
            let origin = Point::new(at.x + pos.x, at.y + pos.y);
            match item {
                FragmentItem::Group(group) => walk(group, origin, out),
                FragmentItem::Text(run) => {
                    let mut x = origin.x;
                    for glyph in &run.glyphs {
                        out.push(Placed {
                            pos: Point::new(x, origin.y),
                            run,
                            glyph,
                        });
                        x += glyph.x_advance.at(run.size);
                    }
                }
                FragmentItem::Shape { .. } | FragmentItem::Object(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(fragment, Point::zero(), &mut out);
    out
}
