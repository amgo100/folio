//! The page builder (SPEC §7.1, §2.3): a build's flow broken into pages of
//! one master, strictly forward, and each page assembled into a fragment.
//!
//! **Breaking.** Each page is filled from where the previous one ended,
//! which may be inside a paragraph. Its builder breaks the paragraphs that
//! start on it into lines at the width of its region, through the
//! line-break cache; a paragraph it starts inside keeps the breaks of the
//! page it started on. It considers every boundary at which the page could
//! end, from its first line until the content would no longer fit in the
//! space left for it, the body less what the lines placed reserve at its
//! foot ([`Regions::reserve`]), and takes
//! the cheapest: the cost of a break is how empty it leaves the page
//! ([`FILL`] times the square of the empty fraction) plus the penalties of
//! the constraints it goes against ([`super::flow`]). Ties go to the later
//! break, a forced break ends the page where it stands, and the end of the
//! flow ends the last. No page is reconsidered once it is broken, which is
//! what makes the page cache and, later, the convergence short-circuit
//! sound (§2.3). A leaf that does not fit on an empty page is placed anyway,
//! overflowing it, with a warning.
//!
//! **Fragments.** A page is a fragment of the master's size holding one
//! fragment for its body, which holds a fragment for each block on the
//! page, nested as the blocks are, with its background and borders, its
//! lines as glyph runs, its baselines and its keep data.
//!
//! **The cache** ([`PageCache`]): each page, keyed on the master, the width
//! of its region, the state coming into it (whether it is the first, and
//! where in the flow it starts: a leaf, or a line of a paragraph broken at
//! some width), the first slice of the layout part of the flow state
//! (§2.3), and the leaves and boundaries its builder read, from its first
//! leaf to the boundary after the last line it considered. A page whose
//! content is unchanged, wherever an edit has moved it, is found there and
//! not broken or assembled again, and keeps its fragment: the page as
//! dressed for the document is kept with it, so an unchanged page shares
//! its fragment's content with the page it was before. The full two-part
//! flow state (§2.3) comes with page furniture and floats.
//!
//! **Changed pages** ([`Pages::hashes`]): each page has a hash of what it
//! shows, computed once when it is assembled. It leaves out the sources
//! that fragments and glyphs name, whose span numbers a reparse may
//! renumber, so a page that looks the same after an edit has the same hash
//! wherever its source moved, and a watcher draws again only the pages
//! whose hash changed ([`Pages::changed`]); of the others, those whose
//! sources changed need only their span maps sent ([`Pages::remapped`])
//! (§2.3).

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ecow::{EcoVec, eco_format};
use folio_component::{
    Abs, Fragment, FragmentItem, Location, Name, ObjectBox, Path, Point, Shape, Size, Stroke,
};
use folio_syntax::Span;

use super::cache::Cache;
use super::codes;
use super::flow::{
    Band, BlockObject, Boundary, Caches, CellOrigins, Constraint, Flow, Fx, Item, KEEP, Leaf,
    Origin, ParagraphInfo, Part, Shared, Sizes, UNBREAKABLE, Walk,
};
use super::footnote::{self, Carried, NOTE_SPLIT, NoteSet, Placement};
use super::master::{BlockBox, PageMaster};
use super::paragraph::{ObjectMetrics, ParagraphLayout};
use super::set::set_line;
use super::{Lines, lines, report_lines};
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::fonts::{Fonts, fonts};
use crate::pipeline::{resolution, step_styled};
use crate::resolve::PageNumbers;
use crate::span_map::source_ref;
use crate::style::{DeclValue, NodeName, NodeView, Position, Properties, Styled, Subject};
use crate::text::{SpaceKind, Text, TextBlock, TextObject, text};

/// How much a page's emptiness costs: an empty page would cost this much.
pub const FILL: f64 = 1_000.0;

/// How many calls of [`pages()`] a page is kept without being asked for.
const MAX_AGE: u64 = 16;

/// Lengths closer than this, in points, are equal.
const EPSILON: f64 = 1e-6;

/// A build's pages.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Pages {
    /// The master every page is made from.
    pub master: PageMaster,
    /// The pages, at least one.
    pub pages: EcoVec<Page>,
    /// The content hash of each page, in the order of [`Pages::pages`]:
    /// a hash of what the page shows, its geometry, glyphs and paint, and
    /// the faces its glyph runs name by index, that leaves out the sources
    /// its fragments and glyphs name. Two pages that look the same have
    /// the same hash, whatever their sources' span numbers. A face counts
    /// by where its file is, its names and its file's length, not its
    /// bytes: a font file replaced in place by one of the same length and
    /// names leaves the hashes as they were, so a watcher draws every page
    /// again when a font file changes.
    pub hashes: EcoVec<u64>,
    /// The page numbers the build's page-number references were set with
    /// (SPEC §2.3): those its layout pass was keyed by, or those the
    /// fixpoint settled on for the build itself. Empty when the build has
    /// none. The fixpoint keeps its own record of them; these tell a
    /// reader of the pages which numbers they show.
    pub page_refs: PageNumbers,
    /// The blocks whose content `hidden` or `answer-space` left out, with
    /// the heights layout gave them (SPEC §6.3): what the Office writers
    /// measure with Folio's layout (§2.1). Each is found by its location
    /// and part; they are in the order layout built them, a space inside
    /// a cell or note with its cell's or note's flow, not document order.
    pub spaces: EcoVec<SpaceHeight>,
}

/// The most `answer-scale` multiplies an answer's height by: more is
/// taken as this, and the space is no taller than the body anyway.
pub const MAX_ANSWER_SCALE: f64 = 100.0;

/// The height layout gave a block whose content `hidden` or `answer-space`
/// left out (SPEC §6.3).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SpaceHeight {
    /// The element instance of the block.
    pub location: Location,
    /// For a part's block, the part.
    pub part: Option<Name>,
    /// What keeps the space.
    pub kind: SpaceKind,
    /// How tall its content box is: for `hidden`, the border box of the
    /// content it held, measured in a discarded layout; for answer space,
    /// its ruled lines, its `answer-height`, or its content's height times
    /// `answer-scale`. At most the body's height, but for hidden content,
    /// ruled lines and answer space with `answer-breakable: true`, which
    /// may be broken across pages.
    pub height: Abs,
}

impl Pages {
    /// The page each element instance of `targets` starts on, by index,
    /// in the order given: the first page with a box of it, or `None`
    /// where no page has one, as for an inline element, whose text is set
    /// in its paragraph's lines without a box of its own. This is the map
    /// from locations to pages that page-number references read.
    pub fn pages_of(&self, targets: &[Location]) -> Vec<Option<usize>> {
        // Each target's index, looked up but never iterated.
        let mut index_of: HashMap<Location, Vec<usize>> = HashMap::new();
        for (i, target) in targets.iter().enumerate() {
            index_of.entry(*target).or_default().push(i);
        }
        let mut found: Vec<Option<usize>> = vec![None; targets.len()];
        let mut left = targets.len();
        for (index, page) in self.pages.iter().enumerate() {
            if left == 0 {
                break;
            }
            visit(&page.fragment, &mut |location| {
                if let Some(indices) = index_of.remove(&location) {
                    for i in indices {
                        found[i] = Some(index);
                        left -= 1;
                    }
                }
            });
        }
        found
    }

    /// The indices of the pages that differ from those of `before` by
    /// their content hash: pages whose hash changed, and pages that
    /// `before` did not have. These are the pages a watcher draws again.
    /// Pages that `before` had beyond the last of these are gone, which
    /// their numbers tell.
    pub fn changed(&self, before: &Pages) -> Vec<usize> {
        self.hashes
            .iter()
            .enumerate()
            .filter(|&(i, hash)| before.hashes.get(i) != Some(hash))
            .map(|(i, _)| i)
            .collect()
    }

    /// The indices of the pages that show what the same page of `before`
    /// showed, by their content hash, but name other sources: an edit
    /// renumbered the spans of what they show, or moved it to another
    /// file. A watcher need not draw them again, but must send their new
    /// span maps, or clicks on them would lead to the old sources.
    pub fn remapped(&self, before: &Pages) -> Vec<usize> {
        self.pages
            .iter()
            .zip(&before.pages)
            .enumerate()
            .filter(|&(i, (page, old))| {
                self.hashes.get(i) == before.hashes.get(i) && !same(&page.fragment, &old.fragment)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// Calls `f` with the location of a fragment and of every fragment and
/// object box inside it, in painting order.
fn visit(fragment: &Fragment, f: &mut dyn FnMut(Location)) {
    let location = fragment.location();
    if location != Location::DETACHED {
        f(location);
    }
    for (_, item) in &fragment.items {
        match item {
            // A copy shows content whose page is the first that shows it.
            FragmentItem::Group(group) if group.artifact => {}
            FragmentItem::Group(group) => visit(group, f),
            FragmentItem::Object(object) => f(object.location),
            _ => {}
        }
    }
}

/// Whether two fragments are equal: at once where they share their items,
/// as a page taken from the cache shares them with the page it was, or
/// else by comparing their content.
fn same(a: &Fragment, b: &Fragment) -> bool {
    let shared = a.items.as_ptr() == b.items.as_ptr() && a.items.len() == b.items.len();
    if shared {
        a.size == b.size
            // Span maps name sources, not instances.
            && a.source == b.source
            && a.first_baseline == b.first_baseline
            && a.last_baseline == b.last_baseline
            && a.keep_with_next == b.keep_with_next
            && a.break_penalty_after == b.break_penalty_after
            && a.artifact == b.artifact
    } else {
        a == b
    }
}

/// One page.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Page {
    /// The page: a fragment of the master's size, holding the body's
    /// fragment at the body's position.
    pub fragment: Fragment,
    /// The lines on the page, in order.
    pub lines: EcoVec<LineRef>,
}

/// A line of a build's text.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct LineRef {
    /// The paragraph, as an index into the build's text.
    pub paragraph: usize,
    /// The line of the paragraph.
    pub line: usize,
}

/// Where a page starts in the flow, or a note on a page in its own flow:
/// the first slice of the layout part of the flow state (SPEC §2.3), which
/// the queue of notes carried over completes ([`Carried`]). It is small,
/// `Eq` and hashes stably, so that it can key the page cache.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct State {
    /// The leaf of the flow the page starts in.
    item: usize,
    /// The line of that leaf's paragraph the page starts at: zero at the
    /// start of a leaf.
    line: usize,
    /// The width of the region the paragraph was broken in when the page
    /// starts inside it, else zero: a paragraph keeps the breaks of the
    /// region it starts in.
    width: Abs,
    /// When the page starts inside a band of a grid (SPEC §7.4), where
    /// each of its cells continues; else empty.
    band: EcoVec<CellAt>,
}

/// Where a cell of a band continues on the next page: where in its flow,
/// and how far below the top of the band's part there it starts, for a
/// cell of a later row that has not started yet.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct CellAt {
    state: State,
    wait: Abs,
}

impl CellAt {
    /// How tall a cell is through each leaf of `run`, its flow read from
    /// where it stands, from the top of the read: below what it waits,
    /// from the top of its box or where it continues, to the bottom of the
    /// leaf with what ends there. A break inside the cell drops the
    /// spaces and leading there; its own box's padding and border stay at
    /// its ends.
    fn wait_heights(&self, run: &mut Run<'_>) -> Vec<Abs> {
        let mut n = 0;
        while run.get(n).is_some() {
            n += 1;
        }
        if n == 0 {
            return Vec::new();
        }
        let start = if self.state == State::START {
            Walk::Start
        } else {
            Walk::Top
        };
        let mut y = self.wait + run.leaves[0].before.walk(start, |_, _| {});
        let mut out = Vec::with_capacity(n);
        for j in 0..n {
            y += run.leaves[j].height();
            let after = run.boundary(j + 1);
            let how = if j + 1 == n { Walk::End } else { Walk::Bottom };
            out.push(y + after.walk(how, |_, _| {}));
            if j + 1 < n {
                y += after.gap();
            }
        }
        out
    }
}

impl State {
    /// The start of the flow.
    pub(super) const START: Self = Self {
        item: 0,
        line: 0,
        width: Abs::zero(),
        band: EcoVec::new(),
    };

    /// The end of a flow of `len` leaves.
    fn end(len: usize) -> Self {
        Self {
            item: len,
            ..Self::START
        }
    }
}

/// A line on a page as its builder records it: its leaf counted from the
/// page's first, the line of its paragraph, and for a line of a cell of a
/// band, the path to it ([`CellLine::path`]).
type PageLine = (usize, usize, Option<EcoVec<(usize, usize)>>);

/// A page as its builder made it: it does not depend on where in the flow
/// the page is.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct PageOut {
    fragment: Fragment,
    /// The content hash of the fragment ([`content_hash`]).
    hash: u64,
    /// Where the next page starts: its leaf counted from the page's first,
    /// and its line and width as in [`State`].
    next: State,
    /// The lines on the page: each a leaf counted from the page's first,
    /// the line of its paragraph, and for a line of a cell of a band
    /// (SPEC §7.4), the cell and the leaf of the cell's flow.
    lines: EcoVec<PageLine>,
    /// The paragraphs that start on the page and were broken at a width
    /// other than the one the flow was built for, or with sized objects,
    /// each a leaf counted from the page's first: their lines' diagnostics
    /// are the page's to report.
    rebroken: EcoVec<(usize, Arc<ParagraphLayout>)>,
    /// The constraints the break after the page goes against.
    relaxed: EcoVec<Constraint>,
    /// The source of the leaf that overflows the page, and by how much.
    overflow: Option<(Span, Abs)>,
    /// The notes the page carries over to the next, in order (SPEC §7.8).
    queue: EcoVec<Carried>,
    /// The source of a footnote that overflows the page, and by how much.
    note_overflow: Option<(Span, Abs)>,
}

/// Test hooks for the regions the page builder fills, standing in for
/// what will make them vary: masters per page, floats and footnotes
/// (SPEC §7.1, §7.8). Without hooks, every page's body is the master's.
#[derive(Copy, Clone, Default)]
pub struct Regions<'a> {
    /// The width of the body of each page, by the page's index, where it
    /// is not the master's. Blocks are as much narrower or wider as the
    /// body is, and a paragraph that starts on the page is broken at its
    /// width.
    pub width: Option<&'a dyn Fn(usize) -> Abs>,
    /// The space a line reserves at the foot of the page it is placed on,
    /// as a footnote will: the body's height left for the flow shrinks by
    /// it as the page is filled, and a line that does not fit with what it
    /// reserves moves to the next page.
    pub reserve: Option<&'a dyn Fn(LineRef) -> Abs>,
    /// The size of an inline object, by the element it stands for, as
    /// maths and images will have once they are laid out (SPEC §7.2,
    /// §8.2). A paragraph with sized objects is broken again with them,
    /// and its lines are as tall as its objects reach; an object without a
    /// size is as wide as nothing and drawn as nothing.
    pub object: Option<&'a SizeObject<'a>>,
    /// The size of the content of a block that holds no paragraphs, as a
    /// display will have: the block's content is then a box of that size,
    /// at the start of its content box.
    pub block: Option<&'a SizeBlock<'a>>,
}

/// A hook that sizes an inline object by the element it stands for
/// ([`Regions::object`]).
pub type SizeObject<'a> = dyn Fn(&TextObject) -> Option<ObjectMetrics> + 'a;

/// A hook that sizes the content of a block that holds no paragraphs
/// ([`Regions::block`]).
pub type SizeBlock<'a> = dyn Fn(&TextBlock) -> Option<Size> + 'a;

impl Debug for Regions<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Regions")
            .field("width", &self.width.is_some())
            .field("reserve", &self.reserve.is_some())
            .field("object", &self.object.is_some())
            .field("block", &self.block.is_some())
            .finish()
    }
}

/// The pages of a build, or `None` when the document could not be styled.
/// Reports constraints that a page break had to relax, and content that
/// does not fit on a page. Every region is the master's body, so every
/// paragraph is broken as [`lines`] broke it, which reports its lines;
/// the page builder reports the lines of paragraphs that a region breaks
/// at another width, which only the test hooks of [`paginate`] make, and
/// [`lines`] is to leave them to it once masters vary.
///
/// The pages carry the page numbers their page-number references were set
/// with ([`Pages::page_refs`]); the fixpoint finds where they put each
/// target ([`Pages::pages_of`], [`crate::pipeline::settle`]). A layout
/// pass of the fixpoint reports nothing here: the build reports what its
/// own layout finds.
#[salsa::tracked(returns(ref))]
pub fn pages(db: &dyn Db, build: Build<'_>) -> Option<Pages> {
    // The lines at the master's widths, broken in parallel: what the page
    // builder reads wherever a region has those widths.
    let lines = lines(db, build).as_ref()?;
    let text = text(db, build).as_ref()?;
    let styled = step_styled(db, build)?;
    let props = &styled.properties;
    let fonts = fonts(db, build.project(db));
    let (master, requested) = PageMaster::checked(&styled.page, props);
    let quiet = build.is_pass(db);
    if let Some(requested) = requested
        && !quiet
    {
        report_page_size(db, build, master, requested, text.root.span);
    }
    let project = build.project(db);
    let input = Input {
        text,
        lines,
        props,
        fonts,
        master,
    };
    let page_refs = resolution(db, build)
        .map(|r| r.pages.clone())
        .unwrap_or_default();
    let (pages, _) = build_pages(
        &input,
        db.layout_cache(),
        db.page_cache(),
        Regions::default(),
        None,
        // An edit stops the query between pages (SPEC §2.3).
        &mut || db.unwind_if_revision_cancelled(),
        &mut |out, rebroken| {
            if quiet {
                return;
            }
            report(db, out);
            for (paragraph, layout) in rebroken {
                report_lines(db, project, &text.paragraphs[*paragraph], layout);
            }
        },
    );
    Some(Pages { page_refs, ..pages })
}

/// The first pages of a build ([`pages_through`]).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FirstPages {
    /// The pages laid out: the first pages of what [`pages()`] gives, the
    /// same in every respect, through the page asked for or to the end.
    pub pages: Pages,
    /// Whether they are all the build's pages.
    pub complete: bool,
}

/// The pages of a build through the page with index `last`, or all of
/// them when it has no more; or `None` when the document could not be
/// styled. The preview lays out the pages a reader sees before the rest
/// (SPEC §12.2): page breaking is strictly forward, so the pages before
/// the last visible one are needed anyway, and those after it are not.
///
/// Each page is what [`pages()`] makes of it, through the same page
/// cache, so that [`pages()`] finds these pages there afterwards and only
/// lays out the rest. A blank page that `break-before` pads with counts
/// as a page, so the result may hold a page more than asked for. This is
/// not a query: it reports nothing, and [`pages()`] reports what the
/// pages relax or cannot fit. An edit stops it between pages, as it does
/// [`pages()`]. A build whose page-number references need the bounded
/// fixpoint (SPEC §2.3) runs its layout passes first, as the build's
/// styled tree depends on them.
pub fn pages_through(db: &dyn Db, build: Build<'_>, last: usize) -> Option<FirstPages> {
    let lines = lines(db, build).as_ref()?;
    let text = text(db, build).as_ref()?;
    let styled = step_styled(db, build)?;
    let props = &styled.properties;
    let fonts = fonts(db, build.project(db));
    let (master, _) = PageMaster::checked(&styled.page, props);
    let input = Input {
        text,
        lines,
        props,
        fonts,
        master,
    };
    let page_refs = resolution(db, build)
        .map(|r| r.pages.clone())
        .unwrap_or_default();
    let (pages, complete) = build_pages(
        &input,
        db.layout_cache(),
        db.page_cache(),
        Regions::default(),
        Some(last),
        &mut || db.unwind_if_revision_cancelled(),
        &mut |_, _| {},
    );
    Some(FirstPages {
        pages: Pages { page_refs, ..pages },
        complete,
    })
}

/// The pages of a build as [`pages()`] makes them, from the stages before
/// it, in regions that the test hooks `regions` change; with `cache` and
/// `pages` for the line-break and page caches. Reports nothing.
pub fn paginate(
    text: &Text,
    lines: &Lines,
    styled: &Styled,
    fonts: &Fonts,
    cache: &Cache,
    pages: &PageCache,
    regions: Regions<'_>,
) -> Pages {
    let props = &styled.properties;
    let (master, _) = PageMaster::checked(&styled.page, props);
    let input = Input {
        text,
        lines,
        props,
        fonts,
        master,
    };
    build_pages(
        &input,
        cache,
        pages,
        regions,
        None,
        &mut || {},
        &mut |_, _| {},
    )
    .0
}

/// What the page builder reads of the stages before it.
struct Input<'a> {
    text: &'a Text,
    lines: &'a Lines,
    props: &'a Properties,
    fonts: &'a Fonts,
    master: PageMaster,
}

/// What a page reports: the page, and the paragraphs, by index into the
/// build's text, whose lines it broke.
type Notes<'a> = dyn FnMut(&PageOut, &[(usize, &ParagraphLayout)]) + 'a;

/// Builds the pages, through the page with index `last` when it is given,
/// calling `check` before each, which may unwind, and `notes` with each
/// page and the paragraphs, by index, whose lines it reports; with whether
/// the pages are all the build's.
fn build_pages(
    input: &Input<'_>,
    layout_cache: &Cache,
    cache: &PageCache,
    regions: Regions<'_>,
    last: Option<usize>,
    check: &mut dyn FnMut(),
    notes: &mut Notes<'_>,
) -> (Pages, bool) {
    let Input {
        text,
        lines,
        props,
        fonts,
        master,
    } = *input;
    // The first pages are laid out on the clock of the `pages` call before
    // them, and forget nothing, so that they do not age the pages that only
    // the `pages` calls ask for.
    let now = match last {
        None => cache.tick(),
        Some(_) => cache.now(),
    };
    let sweep = |now| {
        if last.is_none() {
            cache.sweep(now);
        }
    };
    let caches = Caches {
        cache: layout_cache,
        shared: &cache.shared,
        now,
    };
    let body = master.body.size.width;
    let flow = Flow::new(
        text,
        caches,
        &lines.paragraphs,
        &lines.slots,
        props,
        fonts,
        body,
        master.body.size.height,
        Sizes {
            inline: regions.object,
            block: regions.block,
            maths: &lines.maths,
        },
        &lines.grids,
    );
    // The paragraph at the end of `path` in the band at leaf `item`.
    let cell_paragraph = |item: usize, path: &[(usize, usize)]| {
        cell_origin(&flow.cells, item, path).unwrap_or(usize::MAX)
    };
    let paragraph = |item: usize| match flow.origins[item] {
        Origin::Paragraph(p) => p,
        Origin::Empty => usize::MAX,
    };
    let reserve = regions.reserve.map(|f| {
        move |item: usize, line: usize| {
            f(LineRef {
                paragraph: paragraph(item),
                line,
            })
        }
    });
    let reserve = reserve.as_ref().map(|f| f as &dyn Fn(usize, usize) -> Abs);

    // The page and its body are boxes of the document.
    let root = text.root.location;
    // Glyph runs name their fonts by index into the faces, so the faces
    // are part of what every page shows.
    let mut faces = Fx::default();
    fonts.faces().hash(&mut faces);
    let faces = faces.finish();
    let keyed = |hash: u64| {
        let mut keyed = Fx::default();
        keyed.write_u64(hash);
        keyed.write_u64(faces);
        keyed.finish()
    };
    let mut pages = EcoVec::new();
    let mut hashes = EcoVec::new();
    let mut state = State::START;
    let mut queue: EcoVec<Carried> = EcoVec::new();
    let mut complete = true;
    while state.item < flow.items.len() || !queue.is_empty() {
        if last.is_some_and(|last| pages.len() > last) {
            complete = false;
            break;
        }
        // The cache still forgets what it would have when `check` unwinds.
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut *check));
        if let Err(cancelled) = checked {
            sweep(now);
            std::panic::resume_unwind(cancelled);
        }
        // `break-before: odd-page` and `even-page` pad with a blank page.
        if state != State::START
            && state.line == 0
            && state.band.is_empty()
            && let Some(odd) = flow.boundary(state.item).parity
            && (pages.len() % 2 == 0) != odd
        {
            let fragment = blank(&master);
            hashes.push(keyed(content_hash(&fragment)));
            pages.push(Page {
                fragment: dress(&fragment, root),
                lines: EcoVec::new(),
            });
        }
        let width = regions.width.map_or(body, |f| f(pages.len()));
        let page = PageRequest {
            flow: &flow,
            caches,
            master,
            state: state.clone(),
            queue: &queue,
            width,
            reserve,
        };
        let (out, fragment) = cache.page(&page, root);
        let rebroken: Vec<(usize, &ParagraphLayout)> = out
            .rebroken
            .iter()
            .map(|(k, layout)| (paragraph(state.item + k), &**layout))
            .collect();
        notes(&out, &rebroken);
        hashes.push(keyed(out.hash));
        let lines = out
            .lines
            .iter()
            .map(|(k, line, cell)| LineRef {
                paragraph: match cell {
                    None => paragraph(state.item + k),
                    Some(path) => cell_paragraph(state.item + k, path),
                },
                line: *line,
            })
            .collect();
        pages.push(Page { fragment, lines });
        state = State {
            item: state.item + out.next.item,
            ..out.next.clone()
        };
        queue = out.queue.clone();
    }
    sweep(now);
    let pages = Pages {
        master,
        pages,
        hashes,
        page_refs: PageNumbers::default(),
        spaces: flow.spaces.clone(),
    };
    (pages, complete)
}

/// The paragraph that leaf `path` of the band at leaf `item` comes from,
/// through the bands nested in its cells ([`CellLine::path`]).
fn cell_origin(cells: &CellOrigins, item: usize, path: &[(usize, usize)]) -> Option<usize> {
    let ((c, j), rest) = path.split_first()?;
    let at = cells.partition_point(|(i, _)| *i < item);
    let (i, sources) = cells.get(at)?;
    if *i != item {
        return None;
    }
    let source = sources.get(*c)?;
    if rest.is_empty() {
        return match source.origins.get(*j)? {
            Origin::Paragraph(p) => Some(*p),
            Origin::Empty => None,
        };
    }
    cell_origin(&source.cells, *j, rest)
}

/// A hash of what a page shows: its fragments' sizes, positions,
/// baselines and break data, its glyph runs and its shapes, without the
/// sources that fragments, glyphs and shapes name.
fn content_hash(fragment: &Fragment) -> u64 {
    fn walk(fragment: &Fragment, h: &mut Fx) {
        fragment.size.hash(h);
        fragment.first_baseline.hash(h);
        fragment.last_baseline.hash(h);
        fragment.keep_with_next.hash(h);
        fragment.break_penalty_after.hash(h);
        fragment.artifact.hash(h);
        fragment.items.len().hash(h);
        for (pos, item) in &fragment.items {
            pos.hash(h);
            match item {
                FragmentItem::Group(group) => {
                    0u8.hash(h);
                    walk(group, h);
                }
                FragmentItem::Text(run) => {
                    1u8.hash(h);
                    run.font.hash(h);
                    run.size.hash(h);
                    run.fill.hash(h);
                    run.text.hash(h);
                    run.glyphs.len().hash(h);
                    for glyph in &run.glyphs {
                        glyph.id.hash(h);
                        glyph.x_advance.hash(h);
                        glyph.x_offset.hash(h);
                        glyph.y_offset.hash(h);
                        glyph.range.hash(h);
                    }
                }
                FragmentItem::Shape { shape, source: _ } => {
                    2u8.hash(h);
                    shape.hash(h);
                }
                FragmentItem::Object(object) => {
                    3u8.hash(h);
                    object.size.hash(h);
                    object.baseline.hash(h);
                }
                FragmentItem::Image(image) => {
                    4u8.hash(h);
                    image.size.hash(h);
                    image.image.hash(h);
                }
            }
        }
    }
    let mut hasher = Fx::default();
    walk(fragment, &mut hasher);
    hasher.finish()
}

/// A page and its body as boxes of the document's root, `root`. Pages come
/// from the cache whatever document they were made for, so they are
/// dressed afterwards; the cache keeps a page as it was last dressed.
fn dress(page: &Fragment, root: Location) -> Fragment {
    let mut page = page.clone().with_location(root);
    for (_, item) in page.items.make_mut() {
        if let FragmentItem::Group(body) = item {
            body.source = root.source;
            body.instance = root.instance;
        }
    }
    page
}

/// A page without content.
fn blank(master: &PageMaster) -> Fragment {
    let mut page = Fragment::new(master.size);
    page.push_fragment(master.body.origin, Fragment::new(master.body.size));
    page
}

/// Reports a page size outside PDF's limits (SPEC §11.1), at the
/// `page-size` declaration that set it: the last valid one of the rules
/// that match the page, as the style stage matches them; or at the
/// document when the size is the default.
fn report_page_size(
    db: &dyn Db,
    build: Build<'_>,
    master: PageMaster,
    requested: Size,
    root: Span,
) {
    let cascade = crate::style::cascade(db, build.base(db));
    let page_size = cascade.properties.builtin("page-size");
    let page = NodeView {
        subject: Subject {
            name: NodeName::Page,
            classes: EcoVec::new(),
            label: None,
            fields: EcoVec::new(),
        },
        pos: Position::default(),
        prev: EcoVec::new(),
        children: EcoVec::new(),
    };
    let span = cascade
        .rules
        .iter()
        .filter(|rule| rule.selector.matches(&[&page]))
        .flat_map(|rule| &rule.declarations)
        .rfind(|decl| {
            decl.prop == page_size
                && matches!(&decl.value, DeclValue::Value(value)
                    if crate::style::compute(value, Abs::pt(10.0)).is_ok())
        })
        .map_or(root, |decl| decl.span);
    let side = |a: Abs| {
        if a.to_pt().abs() < 1e6 {
            eco_format!("{:.1}pt", a.to_pt())
        } else {
            eco_format!("{:e}pt", a.to_pt())
        }
    };
    diag::report(
        db,
        Diagnostic::warning(
            span,
            eco_format!(
                "this page size, {} by {}, is outside what PDF allows; the pages are {} by {}",
                side(requested.width),
                side(requested.height),
                side(master.size.width),
                side(master.size.height),
            ),
        )
        .with_hint("a side of a PDF page is 3pt to 14,400pt (200in)"),
    );
}

/// Reports what a page relaxed or could not fit.
fn report(db: &dyn Db, out: &PageOut) {
    for constraint in &out.relaxed {
        let (span, code, message, hint) = match *constraint {
            Constraint::Widow(span) => (
                span,
                codes::WIDOW,
                "a page break leaves fewer lines of this paragraph at the top of a page \
                 than `widows` asks for",
                "no better break was found; reword the text or allow fewer lines with `widows`",
            ),
            Constraint::Orphan(span) => (
                span,
                codes::ORPHAN,
                "a page break leaves fewer lines of this paragraph at the foot of a page \
                 than `orphans` asks for",
                "no better break was found; reword the text or allow fewer lines with `orphans`",
            ),
            Constraint::KeepWithNext(span) => (
                span,
                codes::BROKEN_KEEP_WITH_NEXT,
                "a page break separates this block from the next despite `keep-with-next`",
                "the blocks kept together do not fit on one page",
            ),
            Constraint::KeepTogether(span) => (
                span,
                codes::BROKEN_KEEP_TOGETHER,
                "a page break falls inside this block despite `keep-together`",
                "the block does not fit on one page",
            ),
            Constraint::Unbreakable(span) => (
                span,
                codes::BROKEN_UNBREAKABLE,
                "a page break falls inside this block despite `breakable: false`",
                "the block does not fit on one page",
            ),
            Constraint::UnbreakableRules(span) => (
                span,
                codes::BROKEN_UNBREAKABLE,
                "a page break falls between the ruled lines of this block, which do not fit \
                 on one page",
                "set fewer `rule-lines`, or let them break with `rule-breakable: true`",
            ),
            Constraint::RulesWidow(span) => (
                span,
                codes::WIDOW,
                "a page break leaves fewer than two of this block's ruled lines at the top of \
                 a page",
                "no better break was found; set more or fewer `rule-lines`",
            ),
            Constraint::RulesOrphan(span) => (
                span,
                codes::ORPHAN,
                "a page break leaves fewer than two of this block's ruled lines at the foot of \
                 a page",
                "no better break was found; set more or fewer `rule-lines`",
            ),
            Constraint::UnbreakableRow(span) => (
                span,
                codes::BROKEN_UNBREAKABLE,
                "a page break falls inside a row of this grid, which does not fit on one page",
                "let its rows break with `row-breakable: true`, or shorten the row",
            ),
            Constraint::HeaderRows(span) => (
                span,
                codes::BROKEN_KEEP_WITH_NEXT,
                "a page break falls inside this grid's header rows or directly after them",
                "the header rows and the row after them do not fit on one page",
            ),
        };
        diag::report(
            db,
            Diagnostic::warning(span, message)
                .with_code(code)
                .with_hint(hint),
        );
    }
    if let Some((span, by)) = out.overflow {
        diag::report(
            db,
            Diagnostic::warning(
                span,
                eco_format!(
                    "this does not fit on a page: it is {:.1}pt taller than the space left \
                     for it",
                    by.to_pt()
                ),
            )
            .with_code(codes::PAGE_OVERFLOW)
            .with_hint("make it smaller, or the page's margins"),
        );
    }
    if let Some((span, by)) = out.note_overflow {
        diag::report(
            db,
            Diagnostic::warning(
                span,
                eco_format!(
                    "this footnote does not fit on the page of its anchor: it is {:.1}pt taller \
                     than the space left for it",
                    by.to_pt()
                ),
            )
            .with_code(codes::PAGE_OVERFLOW)
            .with_hint("shorten the footnote, or move its anchor"),
        );
    }
}

/// A page to build: where it starts in a flow, and its region.
pub(super) struct PageRequest<'a> {
    flow: &'a Flow,
    caches: Caches<'a>,
    master: PageMaster,
    /// Where the page starts.
    state: State,
    /// The notes carried over to it.
    pub(super) queue: &'a [Carried],
    /// The width of its body.
    width: Abs,
    /// The space a line reserves at the page's foot, by its leaf and line.
    reserve: Option<&'a dyn Fn(usize, usize) -> Abs>,
}

/// A leaf of a page: a line of a paragraph, or an empty block.
pub(super) struct Placed {
    /// What lies before it.
    pub(super) before: Boundary,
    /// The leaf of the flow it is or is a line of.
    pub(super) item: usize,
    /// The paragraph broken into lines, and the line.
    pub(super) line: Option<(Arc<ParagraphInfo>, usize)>,
    /// The sized content of a block, for a block object.
    object: Option<Arc<BlockObject>>,
    /// Whether the paragraph was broken at a width other than the flow's,
    /// or with sized objects: other breaks than [`lines`] reported.
    rebroken: bool,
    /// For a part of a band of a grid, the part (SPEC §7.4).
    pub(super) piece: Option<Piece>,
}

/// A part of a band of a grid, between two heights at which a page may
/// break it: from the top of the band, or from where the page before broke
/// it, each cell of the band breaking at the last boundary of its flow
/// that fits (SPEC §7.4). The parts of a band that a page reads one after
/// another are a *read* of it.
#[derive(Debug, Clone)]
pub(super) struct Piece {
    /// The band.
    pub(super) band: Arc<Band>,
    /// Where each cell starts in the read: its state, and how far below
    /// the read's top.
    starts: EcoVec<CellAt>,
    /// Whether the read starts at the band's top.
    fresh: bool,
    /// Whether the part is the read's last, which ends the band.
    ends: bool,
    /// Which read of the run it is part of: the index of its first part.
    read: usize,
    /// Where the part starts and ends, from the top of the read.
    from: Abs,
    to: Abs,
    /// How many leaves of each cell's flow the read has placed by the end
    /// of the part, and how many it has in all.
    kept: EcoVec<usize>,
    total: EcoVec<usize>,
    /// Where each cell stands at the start of the part.
    at: EcoVec<CellAt>,
    /// Whether it is a header band set again at the top of a page, which
    /// is drawn as a copy and anchors no footnotes.
    pub(super) repeat: bool,
    /// The lines of the cells that the part places, so that their
    /// footnotes are placed at the page's foot (SPEC §7.4, §7.8).
    pub(super) lines: EcoVec<CellLine>,
}

/// A line of a cell of a band: the cells and leaves that lead to it, the
/// first of the band, the others of grids nested in it, the last leaf a
/// paragraph's; and the line.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct CellLine {
    pub(super) path: EcoVec<(usize, usize)>,
    pub(super) line: usize,
}

impl Placed {
    /// How tall it is: a line from its ascent to its descent.
    pub(super) fn height(&self) -> Abs {
        if let Some(piece) = &self.piece {
            return piece.to - piece.from;
        }
        match &self.line {
            Some((paragraph, line)) => {
                let line = &paragraph.layout.lines[*line];
                line.ascent + line.descent
            }
            None => self.object.as_ref().map_or(Abs::zero(), |o| o.size.height),
        }
    }
}

/// The leaves of a page and those after it, from where it starts in the
/// flow: each paragraph broken into lines at the width of the page's
/// region, or, for the one the page starts inside, at the width it was
/// broken at before. Leaves are made as the page builder reads them. A
/// note on the page is a run of its own flow.
pub(super) struct Run<'a> {
    page: &'a PageRequest<'a>,
    /// The flow: the body's, or a note's.
    pub(super) flow: &'a Flow,
    /// Whether it is the body's flow, whose lines the test hook of
    /// [`Regions::reserve`] reserves space for.
    main: bool,
    pub(super) leaves: Vec<Placed>,
    /// The next leaf of the flow to read.
    next: usize,
    /// How many of the first leaves are the header bands of a grid set
    /// again (SPEC §7.4): a page does not end among them, nor directly
    /// after them, so it takes at least one leaf more.
    pub(super) fixed: usize,
}

impl<'a> Run<'a> {
    /// The leaves of `flow` from `state` on, in the region of `page`. A
    /// run that starts at a band after a grid's header bands starts with
    /// those bands, set again (SPEC §7.4).
    pub(super) fn new(page: &'a PageRequest<'a>, flow: &'a Flow, state: State, main: bool) -> Self {
        let mut run = Self {
            page,
            flow,
            main,
            leaves: Vec::new(),
            next: state.item,
            fixed: 0,
        };
        if let Some(item) = flow.items.get(state.item)
            && let Leaf::Band(band) = &item.leaf
        {
            let starts = (!state.band.is_empty()).then(|| state.band.clone());
            let mut before = match starts {
                None => item.before.clone(),
                Some(_) => Boundary::default(),
            };
            for header in &band.headers {
                run.read_band(state.item, header, None, before, true);
                before = Boundary::default();
            }
            if !band.headers.is_empty() {
                before.penalty += KEEP;
                before.constraints.push(Constraint::HeaderRows(band.span));
            }
            run.fixed = run.leaves.len();
            run.read_band(state.item, band, starts, before, false);
            run.next += 1;
        } else if state.line > 0 {
            run.read(state.item, state.width, state.line);
            run.next += 1;
        }
        run
    }

    /// The region cells are laid out in: the master's body, whatever the
    /// page's width, as their tracks were sized in it.
    fn cell_page(&self) -> PageRequest<'a> {
        let page = self.page;
        PageRequest {
            flow: page.flow,
            caches: page.caches,
            master: page.master,
            state: State::START,
            queue: &[],
            width: page.master.body.size.width,
            reserve: None,
        }
    }

    /// Adds the parts of band `band`, leaf `k` of the flow, from where its
    /// cells stand, `starts`, or from its top: the first part after
    /// `before`, the others after a break inside the band, which costs
    /// what each cell's break costs, plus [`UNBREAKABLE`] without
    /// `row-breakable: true`. A header band set again, `repeat`, is one
    /// part.
    fn read_band(
        &mut self,
        k: usize,
        band: &Arc<Band>,
        starts: Option<EcoVec<CellAt>>,
        before: Boundary,
        repeat: bool,
    ) {
        let fresh = starts.is_none();
        let starts = starts.unwrap_or_else(|| {
            band.cells
                .iter()
                .map(|c| CellAt {
                    state: State::START,
                    wait: c.top,
                })
                .collect()
        });
        let page = self.cell_page();
        let mut runs: Vec<Run<'_>> = band
            .cells
            .iter()
            .zip(&starts)
            .map(|(cell, at)| Run::new(&page, &cell.flow, at.state.clone(), false))
            .collect();
        // How tall each cell is through each of its leaves, from the top
        // of the read.
        let heights: Vec<Vec<Abs>> = runs
            .iter_mut()
            .zip(&starts)
            .map(|(run, at)| at.wait_heights(run))
            .collect();
        let floor = if fresh { band.height } else { Abs::zero() };
        let full = heights
            .iter()
            .filter_map(|h| h.last().copied())
            .fold(floor, Abs::max);
        let mut cuts: Vec<Abs> = if repeat {
            Vec::new()
        } else {
            heights
                .iter()
                .flatten()
                .copied()
                .filter(|&c| c.to_pt() > EPSILON && c.to_pt() < full.to_pt() - EPSILON)
                .collect()
        };
        cuts.push(full);
        cuts.sort_by(|a, b| a.to_pt().total_cmp(&b.to_pt()));
        cuts.dedup_by(|a, b| (a.to_pt() - b.to_pt()).abs() <= EPSILON);

        let item = &self.flow.items[k];
        let read = self.leaves.len();
        let total: EcoVec<usize> = heights.iter().map(Vec::len).collect();
        let mut at: Vec<CellAt> = starts.iter().cloned().collect();
        let mut kept_before: Vec<usize> = vec![0; heights.len()];
        let mut from = Abs::zero();
        let mut before = Some(before);
        for (index, &cut) in cuts.iter().enumerate() {
            let last = index + 1 == cuts.len();
            let kept: EcoVec<usize> = heights
                .iter()
                .map(|h| {
                    if last {
                        h.len()
                    } else {
                        h.partition_point(|&y| y.to_pt() <= cut.to_pt() + EPSILON)
                    }
                })
                .collect();
            let mut lines = EcoVec::new();
            if !repeat {
                for (c, run) in runs.iter().enumerate() {
                    for leaf in &run.leaves[kept_before[c]..kept[c]] {
                        if let Some((_, line)) = &leaf.line {
                            lines.push(CellLine {
                                path: EcoVec::from([(c, leaf.item)]),
                                line: *line,
                            });
                        } else if let Some(inner) = leaf.piece.as_ref().filter(|p| !p.repeat) {
                            for nested in &inner.lines {
                                let mut path = EcoVec::from([(c, leaf.item)]);
                                path.extend(nested.path.iter().copied());
                                lines.push(CellLine {
                                    path,
                                    line: nested.line,
                                });
                            }
                        }
                    }
                }
            }
            let boundary = before.take().unwrap_or_default();
            self.leaves.push(Placed {
                before: boundary,
                item: k,
                line: None,
                object: None,
                rebroken: false,
                piece: Some(Piece {
                    band: band.clone(),
                    starts: starts.clone(),
                    fresh,
                    ends: last,
                    read,
                    from,
                    to: cut,
                    kept: kept.clone(),
                    total: total.clone(),
                    at: at.iter().cloned().collect(),
                    repeat,
                    lines,
                }),
            });
            if last {
                break;
            }
            // What a break after this part costs, and where each cell
            // continues.
            let mut next = Boundary::default();
            if !band.breakable {
                next.penalty += UNBREAKABLE;
                next.constraints.push(Constraint::UnbreakableRow(band.span));
            }
            next.penalty += item.inside.penalty;
            next.constraints
                .extend(item.inside.constraints.iter().copied());
            for (c, run) in runs.iter().enumerate() {
                let start = &starts[c];
                let m = kept[c];
                at[c] = if m == 0 {
                    CellAt {
                        state: start.state.clone(),
                        wait: (start.wait - cut).max(Abs::zero()),
                    }
                } else {
                    if m < total[c] {
                        let b = run.boundary(m);
                        next.penalty += b.penalty;
                        next.constraints.extend(b.constraints.iter().copied());
                    }
                    CellAt {
                        state: run.state_at(m, &start.state),
                        wait: Abs::zero(),
                    }
                };
            }
            before = Some(next);
            kept_before = kept.into_iter().collect();
            from = cut;
        }
    }

    /// Leaf `i`, if the flow has it.
    pub(super) fn get(&mut self, i: usize) -> Option<&Placed> {
        let flow = self.flow;
        while self.leaves.len() <= i && self.next < flow.items.len() {
            self.read(self.next, self.page.width, 0);
            self.next += 1;
        }
        self.leaves.get(i)
    }

    /// The boundary before leaf `i`, or after the last for the leaf past
    /// it, once [`Run::get`] has read it.
    pub(super) fn boundary(&self, i: usize) -> &Boundary {
        self.leaves
            .get(i)
            .map_or(&self.flow.end, |leaf| &leaf.before)
    }

    /// Where the flow continues after its first `end` leaves read, for a
    /// run that started at `from`: the leaf of the flow and the line of its
    /// paragraph, with the width the paragraph was broken at when that is
    /// inside it; or the end of the flow.
    pub(super) fn state_at(&self, end: usize, from: &State) -> State {
        match self.leaves.get(end) {
            Some(Placed {
                item,
                piece: Some(piece),
                ..
            }) => State {
                item: *item,
                band: if piece.fresh && piece.from == Abs::zero() {
                    EcoVec::new()
                } else {
                    piece.at.clone()
                },
                ..State::START
            },
            Some(leaf) => {
                let line = leaf.line.as_ref().map_or(0, |(_, line)| *line);
                let width = if line == 0 {
                    Abs::zero()
                } else if leaf.item == from.item && from.line > 0 {
                    from.width
                } else {
                    self.page.width
                };
                State {
                    item: leaf.item,
                    line,
                    width,
                    band: EcoVec::new(),
                }
            }
            None => State::end(self.flow.items.len()),
        }
    }

    /// Starts the leaves read with the line of `mark`, the mark of a
    /// continued footnote (SPEC §7.8): inside the blocks the first leaf
    /// starts in, a leading above it.
    pub(super) fn prepend(&mut self, mark: &Arc<ParagraphInfo>) {
        let Some(first) = self.leaves.first_mut() else {
            return;
        };
        let before = std::mem::take(&mut first.before);
        first.before.parts.push(Part::Lead(mark.leading));
        let item = first.item;
        self.leaves.insert(
            0,
            Placed {
                before,
                item,
                line: Some((mark.clone(), 0)),
                object: None,
                rebroken: false,
                piece: None,
            },
        );
    }

    /// Adds the leaves of leaf `k` of the flow, from line `from` of its
    /// paragraph broken in a region `width` wide.
    fn read(&mut self, k: usize, width: Abs, from: usize) {
        let page = self.page;
        let item = &self.flow.items[k];
        let empty = |before: &Boundary| Placed {
            before: before.clone(),
            item: k,
            line: None,
            object: None,
            rebroken: false,
            piece: None,
        };
        match &item.leaf {
            Leaf::Empty => self.leaves.push(empty(&item.before)),
            Leaf::Band(band) => {
                let before = item.before.clone();
                self.read_band(k, band, None, before, false);
            }
            Leaf::Box(object) => {
                let body = page.master.body.size.width;
                self.leaves.push(Placed {
                    object: Some(BlockObject::at_width(object, body, width)),
                    ..empty(&item.before)
                });
            }
            Leaf::Paragraph(info) => {
                let body = page.master.body.size.width;
                let (paragraph, rebroken) = info.at_width(body, width, page.caches);
                // A paragraph whose objects the test hook sized was broken
                // again with them, so its lines are not those that `lines`
                // reported.
                let rebroken = rebroken || paragraph.hooked;
                if paragraph.layout.lines.is_empty() {
                    self.leaves.push(empty(&item.before));
                    return;
                }
                for line in from..paragraph.layout.lines.len() {
                    let before = if line == 0 {
                        item.before.clone()
                    } else {
                        item.between(&paragraph, line)
                    };
                    self.leaves.push(Placed {
                        before,
                        item: k,
                        line: Some((paragraph.clone(), line)),
                        object: None,
                        rebroken,
                        piece: None,
                    });
                }
            }
        }
    }

    /// The space leaf `i` reserves at the foot of the page.
    fn reserve(&self, i: usize) -> Abs {
        let leaf = &self.leaves[i];
        match (self.page.reserve, &leaf.line) {
            (Some(f), Some((_, line))) if self.main => f(leaf.item, *line),
            _ => Abs::zero(),
        }
    }
}

/// Where a page ends.
struct Break {
    /// The leaf of the run after the page's last.
    end: usize,
    /// The leaf after the last whose boundary after it the builder read.
    read: usize,
    /// Whether the break is a forced one or the end of the flow.
    forced: bool,
    /// How far the page's content is taller than the space left for it.
    overflow: Abs,
    /// How much of each of the page's notes it places (SPEC §7.8).
    notes: Placement,
}

/// Finds where the page that starts at the first leaf of `run` ends: the
/// body's height less what the lines on it reserve at its foot, and less
/// the notes anchored above the break, is the space its content may fill.
/// The notes of the lines read join `notes` as the lines are read.
fn break_page<'a>(run: &mut Run<'a>, notes: &mut NoteSet<'a>, height: Abs, first: bool) -> Break {
    run.get(0).expect("a page starts at a leaf");
    let before = &run.leaves[0].before;
    let mut y = if first {
        before.gap()
    } else {
        before.walk(Walk::Top, |_, _| {})
    };
    let mut reserved = Abs::zero();
    let mut best: Option<(usize, f64, Placement)> = None;
    // A page takes its first leaf, and the leaf after a grid's header
    // bands set again.
    let must = run.fixed;
    let mut k = 0;
    loop {
        y += run.leaves[k].height();
        reserved += run.reserve(k);
        notes.anchor(run, k);
        let space = height - reserved;
        let last = run.get(k + 1).is_none();
        let boundary = run.boundary(k + 1);
        let bottom = y + boundary.walk(Walk::Bottom, |_, _| {});
        // The notes take what the body's content leaves, and a page must
        // take its first leaf.
        let placed = notes.place(space - bottom, k <= must);
        let fits = bottom.to_pt() <= space.to_pt() + EPSILON && placed.ok;
        if !fits && k > must {
            let (end, _, notes) = best.expect("a break before the leaf that overflows");
            return Break {
                end,
                read: k + 1,
                forced: false,
                overflow: Abs::zero(),
                notes,
            };
        }
        let overflow = (bottom - space).max(Abs::zero());
        if last || boundary.forced {
            return Break {
                end: k + 1,
                read: k + 1,
                forced: true,
                overflow,
                notes: placed,
            };
        }
        let used = bottom + placed.height;
        let empty = ((space - used).to_pt() / height.to_pt()).max(0.0);
        let split = if placed.split { NOTE_SPLIT } else { 0.0 };
        let cost = FILL * empty * empty + boundary.penalty + split;
        if k >= must && best.as_ref().is_none_or(|(_, c, _)| cost <= *c) {
            best = Some((k + 1, cost, placed.clone()));
        }
        if !fits && k < must {
            // The header bands set again overflow the page: the leaf after
            // them still goes on it.
            y += boundary.gap();
            k += 1;
            continue;
        }
        if !fits {
            // The first leaf alone overflows the page.
            return Break {
                end: k + 1,
                read: k + 1,
                forced: false,
                overflow,
                notes: placed,
            };
        }
        if placed.split
            && let Some((end, _, notes)) = best.clone()
        {
            // The notes fill the page: a later break would only split
            // them further (SPEC §7.8).
            return Break {
                end,
                read: k + 1,
                forced: false,
                overflow: Abs::zero(),
                notes,
            };
        }
        y += boundary.gap();
        k += 1;
    }
}

/// Breaks and assembles a page; returns it with the run it read and the
/// leaf of the run after the last whose boundary after it was read.
fn layout_page<'a>(page: &'a PageRequest<'a>) -> (PageOut, Run<'a>, usize) {
    let mut run = Run::new(page, page.flow, page.state.clone(), true);
    let mut notes = NoteSet::new(page);
    let first = page.state == State::START && page.queue.is_empty();
    let height = page.master.body.size.height;
    let found = if run.get(0).is_some() {
        break_page(&mut run, &mut notes, height, first)
    } else {
        // Only notes carried over are left: the page holds what fits of
        // them.
        let placed = notes.place(height, true);
        let placed = if placed.leaves.iter().any(|&m| m > 0) {
            placed
        } else {
            notes.place_first(height)
        };
        Break {
            end: 0,
            read: 0,
            forced: true,
            overflow: Abs::zero(),
            notes: placed,
        }
    };
    let fragment = assemble(&run, &notes, &found, first);
    let relaxed = if found.forced {
        EcoVec::new()
    } else {
        run.boundary(found.end).constraints.clone()
    };
    let start = page.state.item;
    let overflow = (found.overflow > Abs::zero()).then(|| {
        let leaf = &run.leaves[0];
        let span = match &leaf.line {
            Some((paragraph, _)) => paragraph.span,
            None => page.flow.items[leaf.item]
                .stack
                .last()
                .map_or(Span::detached(), |(b, _)| b.span),
        };
        (span, found.overflow)
    });
    let note_overflow = (found.notes.overflow > Abs::zero()).then(|| {
        let i = found
            .notes
            .leaves
            .iter()
            .zip(&notes.runs)
            .position(|(&m, run)| m > 0 && run.anchor.is_some())
            .unwrap_or(0);
        let span = notes.runs.get(i).map_or(Span::detached(), |r| r.note.span);
        (span, found.notes.overflow)
    });
    let placed = &run.leaves[..found.end];
    let mut lines = EcoVec::new();
    for leaf in placed {
        if let Some((_, line)) = &leaf.line {
            lines.push((leaf.item - start, *line, None));
        } else if let Some(piece) = &leaf.piece {
            for cell in &piece.lines {
                lines.push((leaf.item - start, cell.line, Some(cell.path.clone())));
            }
        }
    }
    let rebroken = placed
        .iter()
        .filter_map(|leaf| match &leaf.line {
            Some((paragraph, 0)) if leaf.rebroken => {
                Some((leaf.item - start, paragraph.layout.clone()))
            }
            _ => None,
        })
        .collect();
    let next = run.state_at(found.end, &page.state);
    let next = State {
        item: next.item - start,
        ..next
    };
    let queue = notes.carried(&found.notes).into_iter().collect();
    let out = PageOut {
        hash: content_hash(&fragment),
        fragment,
        next,
        lines,
        rebroken,
        relaxed,
        overflow,
        queue,
        note_overflow,
    };
    (out, run, found.read)
}

/// Items positioned from the top-left of the body.
type Items = Vec<(Point, FragmentItem)>;

/// A block open on the page being assembled.
struct OpenBox {
    block: Arc<BlockBox>,
    /// The location of the element instance it is the box of.
    location: Location,
    /// The top of its box on the page, from the top of the body.
    top: Abs,
    /// Whether it started on an earlier page.
    continued: bool,
    /// Its content, positioned from the top-left of the body.
    items: Vec<(Point, FragmentItem)>,
    /// Its first and last baselines, from the top of the body.
    baselines: Option<(Abs, Abs)>,
}

/// A page being assembled.
struct Assembler {
    open: Vec<OpenBox>,
    /// The body's content.
    items: Vec<(Point, FragmentItem)>,
    baselines: Option<(Abs, Abs)>,
    /// How much wider the page's body is than the master's, and so every
    /// block on it.
    widen: Abs,
    /// Where the outermost box ends at the least, from the top of the
    /// body: the foot of a cell's row (SPEC §7.4); else zero.
    floor: Abs,
}

impl Assembler {
    /// Handles a block that ends or starts at `y`.
    fn event(&mut self, part: &Part, y: Abs) {
        match part {
            Part::Open(block, location) => self.open.push(OpenBox {
                block: block.clone(),
                location: *location,
                top: y,
                continued: false,
                items: Vec::new(),
                baselines: None,
            }),
            Part::Close => self.close(y, false),
            _ => {}
        }
    }

    /// Ends the innermost block at `y`: where its box ends, or the foot of
    /// the page's content when it continues on the next page.
    fn close(&mut self, y: Abs, continues: bool) {
        let Some(open) = self.open.pop() else {
            return;
        };
        let y = if self.open.is_empty() {
            y.max(self.floor)
        } else {
            y
        };
        let block = &open.block;
        let top = open.top;
        let height = (y - top).max(Abs::zero());
        let size = Size::new((block.width + self.widen).max(Abs::zero()), height);
        let mut fragment = Fragment::new(size).with_location(open.location);
        decorate(&mut fragment, block, !open.continued, !continues);
        let origin = Point::new(block.x, top);
        for (pos, item) in open.items {
            fragment.push(Point::new(pos.x - origin.x, pos.y - origin.y), item);
        }
        if let Some((first, last)) = open.baselines {
            fragment.first_baseline = first - top;
            fragment.last_baseline = last - top;
        }
        fragment.keep_with_next = block.keep_with_next && !continues;
        if fragment.keep_with_next {
            fragment.break_penalty_after = KEEP as i32;
        }
        self.add(origin, FragmentItem::Group(fragment));
    }

    /// Adds an item to the innermost open block, or to the body.
    fn add(&mut self, pos: Point, item: FragmentItem) {
        match self.open.last_mut() {
            Some(open) => open.items.push((pos, item)),
            None => self.items.push((pos, item)),
        }
    }

    /// Records a baseline in every open block and the body.
    fn baseline(&mut self, y: Abs) {
        let mark = |b: &mut Option<(Abs, Abs)>| {
            *b = Some(b.map_or((y, y), |(first, _)| (first, y)));
        };
        for open in &mut self.open {
            mark(&mut open.baselines);
        }
        mark(&mut self.baselines);
    }
}

/// Assembles the page `found` breaks `run` at, with its notes, `notes`
/// placed as it says.
fn assemble(run: &Run<'_>, notes: &NoteSet<'_>, found: &Break, first: bool) -> Fragment {
    let page = run.page;
    let master = &page.master;
    let widen = page.width - master.body.size.width;
    let new_asm = || Assembler {
        open: Vec::new(),
        items: Vec::new(),
        baselines: None,
        widen,
        floor: Abs::zero(),
    };

    // The notes, set from the top of their area at the foot of the body:
    // each the items of its boxes, positioned from the top-left of the
    // body.
    let height = master.body.size.height;
    let placed = &found.notes;
    let top = height - placed.height;
    let mut y = top + footnote::SKIP;
    let mut last: Option<usize> = None;
    // Each note's items, with the leaf whose line anchors it.
    let mut sets: Vec<(Option<usize>, Items)> = Vec::new();
    for (i, (note, &m)) in notes.runs.iter().zip(&placed.leaves).enumerate() {
        if m == 0 {
            continue;
        }
        if let Some(prev) = last {
            y += notes.gap(prev, placed.leaves[prev], i);
        }
        last = Some(i);
        let mut asm = new_asm();
        y = lay(
            &note.run,
            m,
            Walk::Top,
            Walk::Bottom,
            &mut asm,
            y,
            &mut |_, _| {},
        );
        while !asm.open.is_empty() {
            asm.close(y, true);
        }
        sets.push((note.anchor, asm.items));
    }

    let mut asm = new_asm();
    if found.end > 0 {
        let mut attach = |k: usize, asm: &mut Assembler| {
            for (anchor, items) in &mut sets {
                if *anchor == Some(k) {
                    for (pos, item) in items.drain(..) {
                        asm.add(pos, item);
                    }
                }
            }
        };
        let start = if first { Walk::Through } else { Walk::Top };
        let y = lay(
            run,
            found.end,
            start,
            Walk::Bottom,
            &mut asm,
            Abs::zero(),
            &mut attach,
        );
        while !asm.open.is_empty() {
            asm.close(y, true);
        }
    }
    // The separator, and the notes carried over, which no line on the page
    // anchors.
    if last.is_some() {
        let (pos, rule) = footnote::separator(top, page.width);
        asm.items.push((pos, rule));
    }
    for (_, items) in sets {
        asm.items.extend(items);
    }

    let body = master.body;
    let mut content = Fragment::new(Size::new(page.width, body.size.height));
    for (pos, item) in asm.items {
        content.push(pos, item);
    }
    if let Some((first, last)) = asm.baselines {
        content.first_baseline = first;
        content.last_baseline = last;
    }
    let mut fragment = Fragment::new(master.size);
    fragment.push_fragment(body.origin, content);
    fragment
}

/// Lays the first `end` leaves of `run` into `asm` from `y`, the top of
/// the first, with the blocks around them, and returns where the last
/// block that ends below them ends. `first` keeps the spaces before the
/// first leaf, as at the start of a document; else the page starts with
/// the padding and borders of the blocks that start there, and the blocks
/// the first leaf is in that started before it continue. `after` is called
/// with each leaf placed.
fn lay(
    run: &Run<'_>,
    end: usize,
    start: Walk,
    end_walk: Walk,
    asm: &mut Assembler,
    mut y: Abs,
    after: &mut dyn FnMut(usize, &mut Assembler),
) -> Abs {
    let head = &run.leaves[0];
    if matches!(start, Walk::Through | Walk::Start) {
        let base = y;
        y += head.before.walk(start, |p, at| asm.event(p, base + at));
    } else {
        let stack = &run.flow.items[head.item].stack;
        let continued = stack.len().saturating_sub(head.before.opens());
        for (block, location) in &stack[..continued] {
            asm.open.push(OpenBox {
                block: block.clone(),
                location: *location,
                top: y,
                continued: true,
                items: Vec::new(),
                baselines: None,
            });
        }
        let base = y;
        y += head.before.walk(Walk::Top, |p, at| asm.event(p, base + at));
    }
    // The top of the read of a band whose parts are being laid, and the
    // parts laid but not yet drawn.
    let mut band_top: Option<Abs> = None;
    let mut waiting: Vec<usize> = Vec::new();
    for k in 0..end {
        let leaf = &run.leaves[k];
        if let Some(piece) = &leaf.piece {
            // A read of a band is drawn once its last part on the page is
            // reached, and what its parts anchor follows it.
            let top = *band_top.get_or_insert(y);
            let ends = k + 1 == end
                || run.leaves[k + 1]
                    .piece
                    .as_ref()
                    .is_none_or(|next| next.read != piece.read);
            if ends {
                draw_band(run, piece, top, asm);
                draw_rule(piece, top, asm);
                band_top = None;
                for waited in waiting.drain(..) {
                    after(waited, asm);
                }
            } else {
                waiting.push(k);
                y += leaf.height();
                if k + 1 < end {
                    let base = y;
                    y += run
                        .boundary(k + 1)
                        .walk(Walk::Through, |p, at| asm.event(p, base + at));
                }
                continue;
            }
        } else if let Some((paragraph, line)) = &leaf.line {
            let baseline = y + paragraph.layout.lines[*line].ascent;
            let mut runs = Vec::new();
            set_line(paragraph, *line, baseline, &mut runs);
            for (pos, run) in runs {
                asm.add(pos, run);
            }
            asm.baseline(baseline);
        } else if let Some(object) = &leaf.object {
            let stack = &run.flow.items[leaf.item].stack;
            let x = stack
                .last()
                .map_or(Abs::zero(), |(block, _)| block.content().0);
            // A ruled line draws its rule at its foot, across its block's
            // content (SPEC §6.3).
            if let (Some(stroke), Some((block, _))) = (&object.rule, stack.last()) {
                let width = (block.content().1 + asm.widen).max(Abs::zero());
                let at = y + object.size.height - stroke.thickness / 2.0;
                let mut path = Path::new();
                path.move_to(Point::new(Abs::zero(), Abs::zero()));
                path.line_to(Point::new(width, Abs::zero()));
                asm.add(
                    Point::new(x, at),
                    FragmentItem::Shape {
                        shape: Shape::stroked(path, stroke.clone()),
                        source: source_ref(block.span),
                    },
                );
            }
            match &object.content {
                // A display draws its glyphs and rules.
                Some(items) => {
                    for (pos, item) in items {
                        asm.add(Point::new(x + pos.x, y + pos.y), item.clone());
                    }
                }
                // A ruled line draws its rule alone.
                None if object.rule.is_some() => {}
                None => asm.add(
                    Point::new(x, y),
                    FragmentItem::Object(ObjectBox {
                        size: object.size,
                        baseline: object.size.height,
                        location: object.location,
                    }),
                ),
            }
            match object.baselines {
                Some((first, last)) => {
                    asm.baseline(y + first);
                    asm.baseline(y + last);
                }
                // An empty box, or blank lines, set no baseline.
                None if object.content.as_ref().is_some_and(|c| c.is_empty()) => {}
                None => asm.baseline(y + object.size.height),
            }
        }
        after(k, asm);
        y += leaf.height();
        if k + 1 < end {
            let base = y;
            y += run
                .boundary(k + 1)
                .walk(Walk::Through, |p, at| asm.event(p, base + at));
        }
    }
    let base = y;
    y += run
        .boundary(end)
        .walk(end_walk, |p, at| asm.event(p, base + at));
    y
}

/// Draws the cells of a read of a band through `piece`, its last part on
/// the page, from `top` (SPEC §7.4): each cell's leaves placed, in its box,
/// which reaches the foot of the band's part where the cell continues or
/// its last row is the band's last, and the bottom of its last row where
/// the read is the band's whole. A header band set again is drawn as a
/// copy, whose boxes name no element instance.
fn draw_band(run: &Run<'_>, piece: &Piece, top: Abs, asm: &mut Assembler) {
    let page = run.cell_page();
    let height = piece.to;
    for (c, cell) in piece.band.cells.iter().enumerate() {
        let kept = piece.kept[c];
        let start = &piece.starts[c];
        if kept == 0 {
            // A cell with nothing on this part of a band that it spans
            // still shows its box: one that finished on an earlier page in
            // the band's last row, or one whose next leaf does not fit.
            let finished = piece.total[c] == 0;
            let started = start.state != State::START;
            if !piece.fresh
                && started
                && (cell.last || !finished)
                && let Some((block, location)) =
                    cell.flow.items.first().and_then(|i| i.stack.first())
            {
                let mut sub = Assembler {
                    open: vec![OpenBox {
                        block: block.clone(),
                        location: *location,
                        top: top + start.wait,
                        continued: true,
                        items: Vec::new(),
                        baselines: None,
                    }],
                    items: Vec::new(),
                    baselines: None,
                    widen: Abs::zero(),
                    floor: top + height,
                };
                sub.close(top + height, !(finished && piece.ends));
                for (pos, item) in sub.items {
                    asm.add(pos, item);
                }
            }
            continue;
        }
        let mut cell_run = Run::new(&page, &cell.flow, start.state.clone(), false);
        cell_run.get(kept - 1);
        let complete = kept == piece.total[c];
        let floor = if !complete || (cell.last && !piece.fresh) {
            top + height
        } else if piece.fresh {
            top + cell.bottom.min(height)
        } else {
            Abs::zero()
        };
        let mut sub = Assembler {
            open: Vec::new(),
            items: Vec::new(),
            baselines: None,
            // Cells are laid out in the master's body, whatever the page's
            // width.
            widen: Abs::zero(),
            floor,
        };
        let begin = if start.state == State::START {
            Walk::Start
        } else {
            Walk::Top
        };
        let finish = if complete { Walk::End } else { Walk::Bottom };
        let y = lay(
            &cell_run,
            kept,
            begin,
            finish,
            &mut sub,
            top + start.wait,
            &mut |_, _| {},
        );
        while !sub.open.is_empty() {
            sub.close(y, true);
        }
        let mut items = sub.items;
        if piece.repeat {
            detach(&mut items);
        }
        for (pos, item) in items {
            asm.add(pos, item);
        }
        if let Some((first, last)) = sub.baselines {
            asm.baseline(first);
            asm.baseline(last);
        }
    }
}

/// Draws the rule under a grid's header rows at the foot of `piece`'s band
/// when the read ends there (D-62-2).
fn draw_rule(piece: &Piece, top: Abs, asm: &mut Assembler) {
    let Some((stroke, x, width)) = &piece.band.rule else {
        return;
    };
    if !piece.ends {
        return;
    }
    let at = top + piece.to - stroke.thickness / 2.0;
    let mut path = Path::new();
    path.move_to(Point::new(Abs::zero(), Abs::zero()));
    path.line_to(Point::new(*width, Abs::zero()));
    asm.add(
        Point::new(*x, at),
        FragmentItem::Shape {
            shape: Shape::stroked(path, stroke.clone()),
            source: source_ref(piece.band.span),
        },
    );
}

/// Marks the boxes among `items` as a copy, such as a header band set
/// again (SPEC §7.4): they keep naming their element instances, for the
/// span map, but are artifacts, and no page-number reference finds them.
fn detach(items: &mut [(Point, FragmentItem)]) {
    for (_, item) in items {
        if let FragmentItem::Group(group) = item {
            group.artifact = true;
        }
    }
}

/// Draws a block's background and borders into its fragment: the top
/// border only where the block starts, the bottom one only where it ends.
fn decorate(fragment: &mut Fragment, block: &BlockBox, starts: bool, ends: bool) {
    let source = source_ref(block.span);
    let size = fragment.size;
    if let Some(paint) = &block.background {
        fragment.push(
            Point::zero(),
            FragmentItem::Shape {
                shape: Shape::filled(Path::rect(size.width, size.height), paint.clone()),
                source,
            },
        );
    }
    let (w, h) = (size.width, size.height);
    let mut line = |stroke: &Stroke, from: Point, to: Point| {
        let mut path = Path::new();
        path.move_to(from);
        path.line_to(to);
        fragment.push(
            Point::zero(),
            FragmentItem::Shape {
                shape: Shape::stroked(path, stroke.clone()),
                source,
            },
        );
    };
    let half = |s: &Stroke| s.thickness / 2.0;
    if starts && let Some(s) = &block.border.top {
        line(s, Point::new(Abs::zero(), half(s)), Point::new(w, half(s)));
    }
    if ends && let Some(s) = &block.border.bottom {
        line(
            s,
            Point::new(Abs::zero(), h - half(s)),
            Point::new(w, h - half(s)),
        );
    }
    if let Some(s) = &block.border.left {
        line(s, Point::new(half(s), Abs::zero()), Point::new(half(s), h));
    }
    if let Some(s) = &block.border.right {
        line(
            s,
            Point::new(w - half(s), Abs::zero()),
            Point::new(w - half(s), h),
        );
    }
}

/// Pages broken and assembled, memoised.
///
/// The key is what decides a page: the master, the width of the page's
/// region, the state coming into the page (whether it is the first page,
/// whose leading spaces are kept, and where in the flow it starts: at a
/// leaf, or at a line of a paragraph broken at a width), and the leaves
/// and boundaries its builder read, from its first leaf to the boundary
/// after the last line it considered, with what those lines reserve at the
/// page's foot. Pages are found by their master, region width, first-page
/// flag, first leaf and line, then checked against the rest of the key.
/// Shared by every handle of the database.
#[derive(Default)]
pub struct PageCache {
    entries: Mutex<HashMap<PageKey, Vec<Entry>>>,
    /// The paragraphs and blocks of recent flows, shared so that the same
    /// one in the next flow is the same `Arc`.
    shared: Shared,
    /// Counts the calls of `pages`.
    clock: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// What a page is found by: its master, its region's width, whether it is
/// the first page, and a fingerprint of the leaf and line it starts at.
type PageKey = (PageMaster, Abs, bool, u64);

/// What each line a page's builder read reserves at the page's foot, by
/// its leaf counted from the page's first and its line.
type Reserves = Arc<[(usize, usize, Abs)]>;

struct Entry {
    /// The state the page started at, but for its leaf.
    state: State,
    /// The notes carried over to the page (SPEC §7.8).
    queue: EcoVec<Carried>,
    /// The leaves of the flow the builder read.
    items: Vec<Item>,
    /// The boundary after the last leaf, when the builder read it.
    end: Option<Boundary>,
    /// What each line the builder read reserves at the page's foot, by its
    /// leaf counted from the page's first and its line, when a test hook
    /// reserved space.
    reserves: Option<Reserves>,
    out: Arc<PageOut>,
    /// The page as last dressed, and the root it was dressed for.
    dressed: (Location, Fragment),
    /// The clock when it was last asked for.
    used: u64,
}

/// How often the cache had a page.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct PageStats {
    /// Pages that were in the cache.
    pub hits: u64,
    /// Pages that were broken and assembled.
    pub misses: u64,
}

impl PageCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a call of `pages`.
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The clock of the last call of `pages`.
    fn now(&self) -> u64 {
        self.clock.load(Ordering::Relaxed)
    }

    /// Forgets the pages that no call has asked for in a while.
    fn sweep(&self, now: u64) {
        if now.is_multiple_of(4) {
            let mut entries = lock(&self.entries);
            for bucket in entries.values_mut() {
                bucket.retain(|e| e.used + MAX_AGE >= now);
            }
            entries.retain(|_, bucket| !bucket.is_empty());
            drop(entries);
            self.shared.sweep(now, MAX_AGE);
        }
    }

    /// A page: memoised, or made now; with its fragment dressed as a box
    /// of `root`.
    fn page(&self, page: &PageRequest<'_>, root: Location) -> (Arc<PageOut>, Fragment) {
        let (flow, state, now) = (page.flow, &page.state, page.caches.now);
        let mut start = Fx::default();
        // A page of notes carried over past the end of the flow starts at
        // no leaf.
        start.write_u64(
            flow.items
                .get(state.item)
                .map_or(0, |i| i.leaf.fingerprint()),
        );
        start.write_usize(state.line);
        state.band.hash(&mut start);
        start.write_usize(page.queue.len());
        let key = (
            page.master,
            page.width,
            *state == State::START,
            start.finish(),
        );
        if let Some(hit) = self.find(&key, page, root) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return hit;
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let (out, run, read) = layout_page(page);
        let out = Arc::new(out);
        let dressed = dress(&out.fragment, root);
        let n = flow.items.len();
        let stop = run.leaves.get(read).map_or(n, |leaf| leaf.item + 1);
        let reserves = page.reserve.map(|f| {
            run.leaves[..read]
                .iter()
                .filter_map(|leaf| {
                    let line = leaf.line.as_ref()?.1;
                    Some((leaf.item - state.item, line, f(leaf.item, line)))
                })
                .collect()
        });
        let entry = Entry {
            state: State {
                item: 0,
                ..state.clone()
            },
            queue: page.queue.iter().cloned().collect(),
            items: flow.items[state.item..stop].to_vec(),
            end: run.leaves.get(read).is_none().then(|| flow.end.clone()),
            reserves,
            out: out.clone(),
            dressed: (root, dressed.clone()),
            used: now,
        };
        lock(&self.entries).entry(key).or_default().push(entry);
        (out, dressed)
    }

    /// The page asked for, if the cache has it, dressed as a box of
    /// `root`. The reserve hook is called without the cache locked, so
    /// that it may build pages with the same cache.
    fn find(
        &self,
        key: &PageKey,
        page: &PageRequest<'_>,
        root: Location,
    ) -> Option<(Arc<PageOut>, Fragment)> {
        let now = page.caches.now;
        let take = |entry: &mut Entry| {
            // The first pages are laid out on the clock of the last call of
            // `pages`, which a call since may have passed.
            entry.used = entry.used.max(now);
            if entry.dressed.0 != root {
                entry.dressed = (root, dress(&entry.out.fragment, root));
            }
            (entry.out.clone(), entry.dressed.1.clone())
        };
        let Some(f) = page.reserve else {
            let mut entries = lock(&self.entries);
            let entry = entries
                .get_mut(key)?
                .iter_mut()
                .find(|e| e.reserves.is_none() && e.matches(page))?;
            return Some(take(entry));
        };
        let candidates: Vec<(Arc<PageOut>, Reserves)> = lock(&self.entries)
            .get(key)?
            .iter()
            .filter(|e| e.matches(page))
            .filter_map(|e| Some((e.out.clone(), e.reserves.clone()?)))
            .collect();
        let item = page.state.item;
        let (out, _) = candidates.into_iter().find(|(_, reserves)| {
            reserves
                .iter()
                .all(|&(k, line, reserve)| f(item + k, line) == reserve)
        })?;
        let mut entries = lock(&self.entries);
        let entry = entries
            .get_mut(key)?
            .iter_mut()
            .find(|e| Arc::ptr_eq(&e.out, &out))?;
        Some(take(entry))
    }

    /// How often the cache had a page since it was made.
    pub fn stats(&self) -> PageStats {
        PageStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// The number of pages held.
    pub fn len(&self) -> usize {
        lock(&self.entries).values().map(Vec::len).sum()
    }

    /// Whether no page is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Entry {
    /// Whether the page asked for starts where this one did, and its flow
    /// has, from there on, what this page's builder read, but for what its
    /// lines reserve.
    fn matches(&self, page: &PageRequest<'_>) -> bool {
        let (flow, state) = (page.flow, &page.state);
        let same = self.state.line == state.line
            && self.state.width == state.width
            && self.state.band == state.band;
        if !same || self.queue[..] != *page.queue {
            return false;
        }
        let stop = state.item + self.items.len();
        if stop > flow.items.len() || flow.items[state.item..stop] != self.items[..] {
            return false;
        }
        self.end
            .as_ref()
            .is_none_or(|end| stop == flow.items.len() && flow.end == *end)
    }
}

impl Debug for PageCache {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageCache")
            .field("pages", &self.len())
            .field("stats", &self.stats())
            .finish()
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary(parts: Vec<Part>) -> Boundary {
        Boundary {
            parts: parts.into_iter().collect(),
            ..Boundary::default()
        }
    }

    #[test]
    fn spaces_collapse_and_drop_at_breaks() {
        let pt = Abs::pt;
        let block = Arc::new(BlockBox {
            span: Span::detached(),
            x: Abs::zero(),
            width: pt(100.0),
            space_before: Abs::zero(),
            space_after: Abs::zero(),
            padding: Default::default(),
            border: Default::default(),
            background: None,
            keep_with_next: false,
            keep_together: false,
            breakable: true,
            break_before: crate::layout::BreakBefore::Auto,
            min_height: Abs::zero(),
            rules: None,
        });
        // A paragraph ends (space after 6pt), a section with padding 2pt
        // starts (space before 10pt), then its heading (space before 4pt).
        let b = boundary(vec![
            Part::Lead(pt(1.0)),
            Part::Solid(Abs::zero()),
            Part::Close,
            Part::Space(pt(6.0)),
            Part::Space(pt(10.0)),
            Part::Open(block.clone(), Location::DETACHED),
            Part::Solid(pt(2.0)),
            Part::Space(pt(4.0)),
            Part::Open(block, Location::DETACHED),
            Part::Solid(Abs::zero()),
            Part::Lead(pt(1.0)),
        ]);
        let mut events = Vec::new();
        let gap = b.walk(Walk::Through, |p, y| {
            events.push((matches!(p, Part::Open(..)), y.to_pt()))
        });
        // 1 + max(6, 10) + 2 + 4 + 1.
        assert_eq!(gap, pt(18.0));
        assert_eq!(events, [(false, 1.0), (true, 11.0), (true, 17.0)]);
        assert_eq!(b.walk(Walk::Bottom, |_, _| {}), Abs::zero());
        let mut tops = Vec::new();
        assert_eq!(b.walk(Walk::Top, |_, y| tops.push(y.to_pt())), pt(2.0));
        assert_eq!(tops, [0.0, 2.0]);
    }
}
