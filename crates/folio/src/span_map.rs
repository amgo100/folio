//! Span maps (SPEC §1.5, §12.2, §12.4): from the glyphs, shapes and boxes of
//! fragments back to where their source is written.
//!
//! Every glyph of a page names the source node its text came from and the
//! bytes of the node's text it shows; every shape and fragment names the
//! node it is drawn for, and every object the element it is the box of.
//! [`locate_glyph`], [`locate_fragment`], [`locate_object`] and
//! [`locate_node`] resolve those to a file, a byte range and lines and
//! columns, against the files of the same revision. [`find`] goes the other
//! way, from a place in a file to the parts of pages that show it, which is
//! how a preview follows an editor's cursor and marks diagnostics on the
//! page (SPEC §12.2). A ligature resolves to all the characters it
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

use std::collections::HashMap;
use std::ops::Range;

use folio_component::{Fragment, FragmentItem, Glyph, GlyphRun, ObjectBox, Point, Size, SourceRef};
use folio_syntax::{DiagSpan, FileId, Source};

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

/// Where the node that a glyph, shape, fragment or object names is
/// written, as a whole: an element's call or markup, or the text node of a
/// glyph.
pub fn locate_node(files: &dyn Files, source: SourceRef) -> Option<Location> {
    locate(files, span(source).into())
}

/// A part of a page that shows source text, from [`find`].
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Found {
    /// The index of the page, among those [`find`] was given.
    pub page: usize,
    /// The top-left corner of the area, from the page's top-left corner.
    pub pos: Point,
    /// The area's size.
    pub size: Size,
    /// Whether the area is that of glyphs that show the text; otherwise it
    /// is the box of the innermost fragment or object whose node holds it.
    pub glyphs: bool,
}

/// The parts of pages that show the bytes `bytes` of the file at `path`
/// (a project path, as [`Files::path`] gives it), in page order: what a
/// preview scrolls to when an editor's cursor moves, and marks when a
/// diagnostic points there (SPEC §12.2). Spans are resolved against
/// `files`, which must be the files the pages were made from.
///
/// The glyphs whose text overlaps the bytes are found, a run of
/// neighbouring glyphs as one area. An empty range, such as a cursor,
/// finds the glyph whose text holds the byte after it, or else the glyph
/// whose text ends there, at the end of a word. A glyph's area reaches from
/// its pen position to the next and from a font size above its baseline to
/// a quarter of it below, as a click finds it. Where no glyph shows the
/// bytes, as in the markup of an element's call, the boxes of the
/// fragments and objects whose nodes hold them, and hold the fewest bytes,
/// are found instead.
///
/// ```
/// use folio::{Target, World, span_map};
///
/// let mut world = World::new(
///     "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
/// )
/// .unwrap();
/// world.set_source("main.fol", "A *bold* claim.\n").unwrap();
/// let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
/// let fragments: Vec<_> = pages.pages.iter().map(|p| &p.fragment).collect();
///
/// let found = span_map::find(&world, &fragments, "main.fol", 3..7);
/// assert_eq!(found.len(), 1);
/// assert!(found[0].glyphs && found[0].page == 0);
/// ```
pub fn find(files: &dyn Files, pages: &[&Fragment], path: &str, bytes: Range<usize>) -> Vec<Found> {
    let mut sources: HashMap<FileId, Option<Source>> = HashMap::new();
    let mut source_of = |source: SourceRef| -> Option<(Source, folio_syntax::Span)> {
        let span = span(source);
        let id = span.id()?;
        let file = sources
            .entry(id)
            .or_insert_with(|| {
                (files.path(id).as_deref() == Some(path))
                    .then(|| files.source(id))
                    .flatten()
            })
            .clone()?;
        Some((file, span))
    };

    // Each glyph's bytes in the file, when it is of the file.
    let mut glyphs = Vec::new();
    for (page, fragment) in pages.iter().enumerate() {
        for (index, placed) in self::glyphs(fragment).into_iter().enumerate() {
            let (node, within) = &placed.glyph.source;
            let Some((file, span)) = source_of(*node) else {
                continue;
            };
            let within = within.start as usize..within.end as usize;
            if let Some(range) = text_range(&file, span, within) {
                glyphs.push((page, index, placed, range));
            }
        }
    }
    let overlaps = |r: &Range<usize>| {
        if bytes.is_empty() {
            (r.start <= bytes.start && bytes.start < r.end) || r.start == bytes.start
        } else if r.is_empty() {
            bytes.start <= r.start && r.start < bytes.end
        } else {
            r.start < bytes.end && bytes.start < r.end
        }
    };
    let mut hits: Vec<_> = glyphs.iter().filter(|g| overlaps(&g.3)).collect();
    if hits.is_empty() && bytes.is_empty() {
        hits = glyphs
            .iter()
            .filter(|g| !g.3.is_empty() && g.3.end == bytes.start)
            .collect();
    }
    if !hits.is_empty() {
        return merge(&hits);
    }

    // The innermost boxes whose nodes hold the bytes.
    let mut boxes: Vec<(usize, usize, Point, Size)> = Vec::new();
    for (page, fragment) in pages.iter().enumerate() {
        let mut found = Vec::new();
        boxes_in(fragment, Point::zero(), &mut found);
        for (source, pos, size) in found {
            let Some((file, span)) = source_of(source) else {
                continue;
            };
            let Some(node) = file.find(span) else {
                continue;
            };
            let range = node.range();
            let holds = range.start <= bytes.start
                && bytes.end <= range.end
                && (range.start < range.end || bytes.is_empty());
            if holds {
                boxes.push((range.len(), page, pos, size));
            }
        }
    }
    let Some(fewest) = boxes.iter().map(|b| b.0).min() else {
        return Vec::new();
    };
    boxes
        .into_iter()
        .filter(|b| b.0 == fewest)
        .map(|(_, page, pos, size)| Found {
            page,
            pos,
            size,
            glyphs: false,
        })
        .collect()
}

/// The areas of glyphs, one for each run of glyphs that follow each other
/// in a run of text.
fn merge(hits: &[&(usize, usize, Placed<'_>, Range<usize>)]) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let mut last: Option<(usize, usize, *const GlyphRun)> = None;
    for &&(page, index, placed, _) in hits {
        let size = placed.run.size;
        let advance = placed.glyph.x_advance.at(size);
        let x0 = placed.pos.x.min(placed.pos.x + advance);
        let area = Found {
            page,
            pos: Point::new(x0, placed.pos.y - size),
            size: Size::new(advance.max(-advance), size * 1.25),
            glyphs: true,
        };
        let run = std::ptr::from_ref(placed.run);
        let follows = last.is_some_and(|(p, i, r)| p == page && i + 1 == index && r == run);
        match out.last_mut() {
            Some(prev) if follows => {
                let left = prev.pos.x.min(area.pos.x);
                let right = (prev.pos.x + prev.size.width).max(area.pos.x + area.size.width);
                prev.pos.x = left;
                prev.size.width = right - left;
            }
            _ => out.push(area),
        }
        last = Some((page, index, run));
    }
    out
}

/// The boxes of the fragments nested in a fragment, itself included, and
/// of its objects, with the nodes they name.
fn boxes_in(fragment: &Fragment, at: Point, out: &mut Vec<(SourceRef, Point, Size)>) {
    out.push((fragment.source, at, fragment.size));
    for (pos, item) in &fragment.items {
        let origin = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => boxes_in(group, origin, out),
            FragmentItem::Object(object) => {
                out.push((object.location.source, origin, object.size));
            }
            FragmentItem::Text(_) | FragmentItem::Shape { .. } => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Target, World};

    fn world(text: &str) -> World {
        let mut world = World::new(
            "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
        )
        .unwrap();
        world.set_source("main.fol", text).unwrap();
        world
    }

    fn find_in(world: &World, bytes: Range<usize>) -> Vec<Found> {
        let pages = crate::pages(world, Target::Pdf, None).unwrap().unwrap();
        let fragments: Vec<_> = pages.pages.iter().map(|p| &p.fragment).collect();
        find(world, &fragments, "main.fol", bytes)
    }

    #[test]
    fn a_cursor_finds_its_glyph_and_a_range_its_glyphs() {
        let world = world("Alpha beta gamma.\n");
        let one = find_in(&world, 7..7);
        assert_eq!(one.len(), 1);
        assert!(one[0].glyphs);
        let word = find_in(&world, 6..10);
        assert_eq!(word.len(), 1, "neighbouring glyphs merge: {word:?}");
        assert!(word[0].size.width > one[0].size.width);
        assert!(word[0].pos.x <= one[0].pos.x);
    }

    #[test]
    fn a_cursor_after_the_last_letter_finds_it() {
        let world = world("Alpha beta gamma.\n");
        let end = find_in(&world, 17..17);
        assert_eq!(end.len(), 1);
        assert!(end[0].glyphs);
    }

    #[test]
    fn a_range_over_two_lines_finds_one_area_on_each() {
        let mut text = String::new();
        for _ in 0..40 {
            text.push_str("word ");
        }
        text.push('\n');
        let world = world(&text);
        let all = find_in(&world, 0..text.len());
        let mut lines: Vec<_> = all.iter().map(|f| f.pos.y).collect();
        lines.dedup();
        assert!(lines.len() >= 2, "{all:?}");
        // Spaces at a break are not drawn, so an area is a line or a word.
        assert!(all.iter().all(|f| f.glyphs));
    }

    #[test]
    fn markup_without_glyphs_finds_the_innermost_box() {
        let world = world("Text before.\n\n= A heading\n\nText after.\n");
        // The `=` of the heading draws nothing of its own.
        let found = find_in(&world, 14..14);
        assert!(!found.is_empty());
        assert!(found.iter().all(|f| !f.glyphs), "{found:?}");
        // A whole page is not the innermost box.
        let pages = crate::pages(&world, Target::Pdf, None).unwrap().unwrap();
        assert!(found[0].size.height < pages.pages[0].fragment.size.height);
    }

    #[test]
    fn another_file_finds_nothing() {
        let world = world("Alpha beta gamma.\n");
        let pages = crate::pages(&world, Target::Pdf, None).unwrap().unwrap();
        let fragments: Vec<_> = pages.pages.iter().map(|p| &p.fragment).collect();
        assert!(find(&world, &fragments, "other.fol", 0..3).is_empty());
    }
}
