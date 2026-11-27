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

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use ecow::EcoString;
use folio_component::{Fragment, FragmentItem, Glyph, GlyphRun, ObjectBox, Point, Size, SourceRef};
use folio_syntax::{DiagSpan, FileId, LinkedNode, Source, SyntaxKind};

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
/// a quarter of it below, as a click finds it. A glyph that stands for a
/// whole element, such as a list item's marker or a heading's number, is
/// left out for bytes that start in the element's body (from the start of
/// its markup to the end, or a call's arguments) where other glyphs show
/// them, unless they hold the whole element: a cursor in an item's text
/// marks that text, not the item's label, which a cursor at the item's `+`
/// marks. Where only such glyphs show the bytes, those of the innermost
/// element are found. Where no glyph shows the bytes, as in the markup of
/// an element's call, the boxes of the fragments and objects whose nodes
/// hold them, and hold the fewest bytes, are found instead; pages
/// themselves, and the root nodes of files, are not.
///
/// To look up many places in the same pages, build an [`Index`] once.
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
    Index::new(files, pages).find(path, bytes)
}

/// The places in files that pages show, for [`find`]ing many of them.
/// Building one walks each file's syntax tree once; a lookup resolves only
/// the glyphs of the nodes that hold the bytes it looks for. It holds no
/// reference to the pages; it keeps the files' sources, which are cheap to
/// clone.
#[derive(Clone, Default)]
pub struct Index {
    /// The id of each file the pages show, by project path.
    paths: HashMap<EcoString, FileId>,
    /// The sources of those files.
    sources: HashMap<FileId, Source>,
    /// Every glyph whose node is in a file, in painting order.
    glyphs: Vec<GlyphEntry>,
    /// Every box of a fragment or object whose node is in a file.
    boxes: Vec<BoxEntry>,
    /// The end of the bytes of its text that a node's glyphs show.
    extent: HashMap<SourceRef, u32>,
    /// The root node of each file, which holds every place and shows none.
    roots: HashSet<SourceRef>,
}

/// A node's file and its bytes there.
type NodePlace = (FileId, Range<usize>);

/// How the bytes of a node's text map to bytes of its file.
#[derive(Copy, Clone)]
enum Mapping {
    /// Every byte maps to the whole node.
    Whole,
    /// Byte `i` of the text is byte `at + i` of the file: text written as
    /// it is.
    Shift(usize),
    /// Some other way, glyph by glyph.
    Each,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("files", &self.paths.len())
            .field("glyphs", &self.glyphs.len())
            .field("boxes", &self.boxes.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct GlyphEntry {
    page: usize,
    /// The glyph's run, numbered in painting order across the pages.
    run: usize,
    node: SourceRef,
    /// The bytes of the node's text that the glyph shows.
    within: Range<u32>,
    /// The node's file and bytes.
    file: FileId,
    whole: Range<usize>,
    pos: Point,
    size: Size,
}

#[derive(Debug, Clone)]
struct BoxEntry {
    page: usize,
    node: SourceRef,
    /// The node's file and bytes.
    file: FileId,
    whole: Range<usize>,
    pos: Point,
    size: Size,
}

impl Index {
    /// Indexes what the pages show, against the files they were made from.
    pub fn new(files: &dyn Files, pages: &[&Fragment]) -> Self {
        let mut index = Self::default();
        let mut run = 0;
        for (page, fragment) in pages.iter().enumerate() {
            let mut last: Option<*const GlyphRun> = None;
            for placed in glyphs(fragment) {
                let this = std::ptr::from_ref(placed.run);
                if last != Some(this) {
                    run += 1;
                    last = Some(this);
                }
                let (node, within) = &placed.glyph.source;
                if node.is_detached() {
                    continue;
                }
                let size = placed.run.size;
                let advance = placed.glyph.x_advance.at(size);
                index.glyphs.push(GlyphEntry {
                    page,
                    run,
                    node: *node,
                    within: within.clone(),
                    file: FileId::from_raw(std::num::NonZeroU16::MIN),
                    whole: 0..0,
                    pos: Point::new(
                        placed.pos.x.min(placed.pos.x + advance),
                        placed.pos.y - size,
                    ),
                    size: Size::new(advance.max(-advance), size * 1.25),
                });
            }
            let mut found = Vec::new();
            boxes_in(fragment, Point::zero(), &mut found);
            for (node, pos, size) in found {
                if !node.is_detached() {
                    index.boxes.push(BoxEntry {
                        page,
                        node,
                        file: FileId::from_raw(std::num::NonZeroU16::MIN),
                        whole: 0..0,
                        pos,
                        size,
                    });
                }
            }
        }

        // The nodes named, by file, found in one walk of each file's tree.
        // Neighbouring glyphs mostly name the same node, which is looked
        // up once.
        let mut wanted: HashMap<FileId, HashSet<u64>> = HashMap::new();
        let mut last = SourceRef::DETACHED;
        let named = index.glyphs.iter().map(|g| g.node);
        for node in named.chain(index.boxes.iter().map(|b| b.node)) {
            if node != last
                && let Some(id) = span(node).id()
            {
                wanted.entry(id).or_default().insert(node.0);
            }
            last = node;
        }
        let mut nodes: HashMap<u64, NodePlace> = HashMap::new();
        for (id, named) in wanted {
            let (Some(source), Some(path)) = (files.source(id), files.path(id)) else {
                continue;
            };
            let mut stack = vec![(source.root(), 0)];
            while let Some((node, start)) = stack.pop() {
                let reference = source_ref(node.span()).0;
                if named.contains(&reference) {
                    nodes.insert(reference, (id, start..start + node.len()));
                }
                let mut at = start;
                for child in node.children() {
                    stack.push((child, at));
                    at += child.len();
                }
            }
            index.roots.insert(source_ref(source.root().span()));
            index.paths.insert(path, id);
            index.sources.insert(id, source);
        }
        let mut last: Option<(SourceRef, Option<&NodePlace>)> = None;
        index.glyphs.retain_mut(|g| {
            let found = match last {
                Some((node, found)) if node == g.node => found,
                _ => nodes.get(&g.node.0),
            };
            last = Some((g.node, found));
            let Some((file, whole)) = found else {
                return false;
            };
            g.file = *file;
            g.whole = whole.clone();
            true
        });
        let mut run: Option<(SourceRef, u32)> = None;
        for g in &index.glyphs {
            match &mut run {
                Some((node, end)) if *node == g.node => *end = (*end).max(g.within.end),
                _ => {
                    if let Some((node, end)) = run {
                        let max = index.extent.entry(node).or_default();
                        *max = (*max).max(end);
                    }
                    run = Some((g.node, g.within.end));
                }
            }
        }
        if let Some((node, end)) = run {
            let max = index.extent.entry(node).or_default();
            *max = (*max).max(end);
        }
        index.boxes.retain_mut(|b| {
            let Some((file, whole)) = nodes.get(&b.node.0) else {
                return false;
            };
            b.file = *file;
            b.whole = whole.clone();
            true
        });
        index
    }

    /// The parts of the pages that show the bytes `bytes` of the file at
    /// `path`, as [`find`] finds them.
    pub fn find(&self, path: &str, bytes: Range<usize>) -> Vec<Found> {
        let Some(&file) = self.paths.get(path) else {
            return Vec::new();
        };
        let Some(source) = self.sources.get(&file) else {
            return Vec::new();
        };
        // The glyphs whose nodes reach the bytes, with the bytes each
        // shows. A glyph of a node that lies inside a range shows bytes
        // of it, so only those of nodes at its edges are resolved.
        let inside = |whole: &Range<usize>| {
            !bytes.is_empty() && bytes.start <= whole.start && whole.end < bytes.end
        };
        // How each node maps, found from where the start and end of its
        // text map to, so that the glyphs of a long text are not resolved
        // one by one. A map that keeps the text's length between its ends
        // is a shift, since it is monotonic and maps characters to
        // characters.
        let mut mappings: HashMap<SourceRef, Mapping> = HashMap::new();
        let mut mapping = |g: &GlyphEntry| {
            *mappings.entry(g.node).or_insert_with(|| {
                let node = span(g.node);
                let end = self.extent.get(&g.node).copied().unwrap_or(0) as usize;
                let first = text_range(source, node, 0..0);
                if !g.whole.is_empty() && first.as_ref() == Some(&g.whole) {
                    return Mapping::Whole;
                }
                let last = text_range(source, node, end..end);
                match (first, last) {
                    (Some(a), Some(b))
                        if a.is_empty() && b.is_empty() && b.start - a.start == end =>
                    {
                        Mapping::Shift(a.start)
                    }
                    _ => Mapping::Each,
                }
            })
        };
        // Each glyph with the bytes it shows, and whether it maps to its
        // whole node, as the glyphs of an element's marker do.
        let resolved: Vec<(usize, &GlyphEntry, Range<usize>, bool)> = self
            .glyphs
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                g.file == file && g.whole.start <= bytes.end && bytes.start <= g.whole.end
            })
            .filter_map(|(i, g)| {
                if inside(&g.whole) {
                    return Some((i, g, g.whole.clone(), false));
                }
                let within = g.within.start as usize..g.within.end as usize;
                let (range, whole) = match mapping(g) {
                    Mapping::Whole => (g.whole.clone(), true),
                    Mapping::Shift(at) => (at + within.start..at + within.end, false),
                    Mapping::Each => (text_range(source, span(g.node), within)?, false),
                };
                Some((i, g, range, whole))
            })
            .collect();
        let overlaps = |r: &Range<usize>| {
            if bytes.is_empty() {
                (r.start <= bytes.start && bytes.start < r.end) || r.start == bytes.start
            } else if r.is_empty() {
                bytes.start <= r.start && r.start < bytes.end
            } else {
                r.start < bytes.end && bytes.start < r.end
            }
        };
        // A glyph that stands for a whole element, such as a list item's
        // marker or a heading's number, is not found for bytes that start
        // in the element's body where other glyphs show them: there the
        // reveal marks the body's glyphs alone (#417). It is found for
        // bytes that start in the element's own markup, as at an item's
        // `+`, for bytes that hold the whole element, and where no other
        // glyph shows the bytes, as at a label.
        let mut bodies: HashMap<SourceRef, Option<Range<usize>>> = HashMap::new();
        let mut shown = |&(_, g, ref r, whole): &(usize, &GlyphEntry, Range<usize>, bool)| {
            if !whole || (bytes.start <= r.start && r.end <= bytes.end) {
                return true;
            }
            let found = bodies
                .entry(g.node)
                .or_insert_with(|| source.find(span(g.node)).and_then(|node| body(&node)));
            found
                .as_ref()
                .is_none_or(|b| !(b.start <= bytes.start && bytes.start <= b.end))
        };
        // The glyphs that show the bytes, or for a cursor that no glyph's
        // text holds, those whose text ends there; without the glyphs of
        // whole elements whose bodies hold the place where any other glyph
        // shows it, and with them where none does.
        let mut pick = |hide: bool| {
            let mut found = |at: &dyn Fn(&Range<usize>) -> bool| -> Vec<(usize, &GlyphEntry)> {
                resolved
                    .iter()
                    .filter(|(_, _, r, _)| at(r))
                    .filter(|entry| !hide || shown(entry))
                    .map(|&(i, g, _, _)| (i, g))
                    .collect()
            };
            let hits = found(&overlaps);
            if hits.is_empty() && bytes.is_empty() {
                found(&|r| !r.is_empty() && r.end == bytes.start)
            } else {
                hits
            }
        };
        let mut hits = pick(true);
        if hits.is_empty() {
            // Only whole elements show the place: the innermost of them,
            // not a label around it.
            hits = pick(false);
            let wholes: Vec<Range<usize>> = hits.iter().map(|(_, g)| g.whole.clone()).collect();
            hits.retain(|(_, g)| {
                !wholes.iter().any(|inner| {
                    *inner != g.whole && g.whole.start <= inner.start && inner.end <= g.whole.end
                })
            });
        }
        if !hits.is_empty() {
            return merge(&hits);
        }
        let holding = || {
            self.boxes.iter().filter_map(|b| {
                let holds = b.file == file
                    && !self.roots.contains(&b.node)
                    && b.whole.start <= bytes.start
                    && bytes.end <= b.whole.end
                    && (!b.whole.is_empty() || bytes.is_empty());
                holds.then_some((b, b.whole.len()))
            })
        };
        let Some(fewest) = holding().map(|(_, len)| len).min() else {
            return Vec::new();
        };
        holding()
            .filter(|(_, len)| *len == fewest)
            .map(|(b, _)| Found {
                page: b.page,
                pos: b.pos,
                size: b.size,
                glyphs: false,
            })
            .collect()
    }
}

/// The bytes of an element's body: its markup, from the start of the first
/// part to the end of the last (an item's or heading's text, a term and its
/// description), or the arguments of a call. `None` for a node with
/// neither, or whose body is empty.
fn body(node: &LinkedNode<'_>) -> Option<Range<usize>> {
    let parts: Vec<Range<usize>> = node
        .children()
        .filter(|child| child.kind() == SyntaxKind::Markup)
        .map(|child| child.range())
        .collect();
    let body = match (parts.first(), parts.last()) {
        (Some(first), Some(last)) => first.start..last.end,
        _ if node.kind() == SyntaxKind::FuncCall => node
            .children()
            .find(|child| child.kind() == SyntaxKind::Args)?
            .range(),
        _ => return None,
    };
    (!body.is_empty()).then_some(body)
}

/// The areas of glyphs, one for each run of glyphs that follow each other
/// in a run of text.
fn merge(hits: &[(usize, &GlyphEntry)]) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let mut last: Option<(usize, usize)> = None;
    for &(index, glyph) in hits {
        let follows = last.is_some_and(|(i, run)| i + 1 == index && run == glyph.run);
        match out.last_mut() {
            Some(prev) if follows => {
                let left = prev.pos.x.min(glyph.pos.x);
                let right = (prev.pos.x + prev.size.width).max(glyph.pos.x + glyph.size.width);
                prev.pos.x = left;
                prev.size.width = right - left;
            }
            _ => out.push(Found {
                page: glyph.page,
                pos: glyph.pos,
                size: glyph.size,
                glyphs: true,
            }),
        }
        last = Some((index, glyph.run));
    }
    out
}

/// The boxes of the fragments nested in a fragment, not itself, and of its
/// objects, with the nodes they name.
fn boxes_in(fragment: &Fragment, at: Point, out: &mut Vec<(SourceRef, Point, Size)>) {
    for (pos, item) in &fragment.items {
        let origin = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => {
                out.push((group.source, origin, group.size));
                boxes_in(group, origin, out);
            }
            FragmentItem::Object(object) => {
                out.push((object.location.source, origin, object.size));
            }
            FragmentItem::Image(image) => {
                out.push((image.location.source, origin, image.size));
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
                FragmentItem::Shape { .. } | FragmentItem::Object(_) | FragmentItem::Image(_) => {}
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
    fn a_blank_line_finds_no_page() {
        let world = world("First paragraph.\n\nSecond paragraph.\n");
        // The blank line between the paragraphs.
        let found = find_in(&world, 17..17);
        let pages = crate::pages(&world, Target::Pdf, None).unwrap().unwrap();
        let page = pages.pages[0].fragment.size;
        assert!(
            found.iter().all(|f| f.size != page),
            "a page is no place: {found:?}"
        );
    }

    #[test]
    fn another_file_finds_nothing() {
        let world = world("Alpha beta gamma.\n");
        let pages = crate::pages(&world, Target::Pdf, None).unwrap().unwrap();
        let fragments: Vec<_> = pages.pages.iter().map(|p| &p.fragment).collect();
        assert!(find(&world, &fragments, "other.fol", 0..3).is_empty());
    }
}
