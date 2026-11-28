//! Footnotes in the page builder (SPEC §7.8): the queue of notes a page
//! places at its foot, and how much of each it places.
//!
//! A footnote's note is a flow of its own ([`Note`]). As the page builder
//! considers where a page could end, the notes anchored in the lines above
//! that break join the page's notes, after the notes that earlier pages
//! carried over, and the space they take at the page's foot comes off the
//! space the body leaves for the flow (§7.1). For each break considered,
//! the notes are placed in order, each whole while it fits ([`NoteSet::place`]):
//!
//! - A note that does not fit whole is split between two of its lines,
//!   after as many as fit, and continues on the next page, where the notes
//!   carried over come first. Only the last note on a page is split, as in
//!   TeX, so the page ends at the first break whose notes are split.
//! - A note always starts on the page of its anchor: a break whose notes
//!   include one that cannot start, as none of its lines fits, is not
//!   taken, and the anchor's line moves to the next page. A note carried
//!   over that does not fit at all waits for the next page.
//! - A page must take its first line. If the notes anchored in it cannot
//!   start even when the notes carried over wait, the first line of each is
//!   placed anyway, overflowing the page, with a warning.
//!
//! The queue a page leaves for the next ([`Carried`]) is part of the layout
//! part of the flow state (§2.3): it decides the next page's breaks, so it
//! keys the page cache with where the page starts. A note is the same
//! wherever it is, so an unchanged page with notes is found in the cache
//! after an edit before it.
//!
//! The notes are set at the foot of the body, below a separator rule, each
//! as its block's boxes; the boxes of a note that starts on the page are
//! drawn inside the box of the paragraph it is anchored in, so that its
//! structure follows its anchor (§11.1), and those of a continued note in
//! the body.
//!
//! The queue holds positions and small data only, never fragments, so a
//! layout container can negotiate the space of the notes anchored in it
//! (`cx.place_footnotes`, §3.3), and a column's own footnote area is another
//! set of notes placed against the column's height.

use std::sync::Arc;

use folio_component::{Abs, Color, FragmentItem, Paint, Path, Point, Shape, SourceRef, Stroke};

use super::flow::{Boundary, Note, Walk};
use super::page::{PageRequest, Run, State};

/// The space between the body's content and the first note: the separator
/// rule stands in it.
pub(super) const SKIP: Abs = Abs::pt(9.0);

/// How far above the top of the first note the separator rule's middle is.
const RULE_ABOVE: Abs = Abs::pt(3.0);

/// How thick the separator rule is.
const RULE: Abs = Abs::pt(0.5);

/// The cost of a page break whose notes are split: a note is easier to
/// read on one page (TeX's `\interfootnotelinepenalty`).
pub(super) const NOTE_SPLIT: f64 = 100.0;

/// Lengths closer than this, in points, are equal.
const EPSILON: f64 = 1e-6;

/// A note that a page carries over to the next: what is left of it starts
/// there, before the notes anchored on that page.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct Carried {
    /// The note.
    pub note: Arc<Note>,
    /// Where in its flow it continues.
    pub from: State,
}

/// The notes of a page: those carried over, then those anchored in the
/// lines the page builder has read, in order.
pub(super) struct NoteSet<'a> {
    page: &'a PageRequest<'a>,
    pub(super) runs: Vec<NoteRun<'a>>,
    /// How many of the runs were carried over.
    carried: usize,
}

/// A note of a page, from where it starts on the page.
pub(super) struct NoteRun<'a> {
    /// The note.
    pub note: &'a Arc<Note>,
    /// The leaf of the body's run whose line anchors it, or `None` for a
    /// note carried over.
    pub anchor: Option<usize>,
    /// Where in its flow it starts on the page.
    pub from: State,
    /// Its leaves from there, all read.
    pub run: Run<'a>,
    /// How tall its first `j + 1` leaves are, by `j`: from the padding and
    /// borders of the blocks that start before the first to those of the
    /// blocks that end after the last.
    heights: Vec<Abs>,
    /// The space between it and the note before it on the page.
    gap: Abs,
    /// Whether its first leaf is the mark of a continued note, which is
    /// never placed alone.
    marked: bool,
}

/// How much of each note of a set a page places.
#[derive(Debug, Clone, Default)]
pub(super) struct Placement {
    /// How many leaves of each note it places, in the order of the set's
    /// runs; the runs past the end place none.
    pub leaves: Vec<usize>,
    /// How tall the notes are with the separator: zero when none is placed.
    pub height: Abs,
    /// Whether every note anchored on the page starts on it.
    pub ok: bool,
    /// Whether a note is split, or a note carried over waits: the page can
    /// take no more lines.
    pub split: bool,
    /// How far the notes reach below the page's foot.
    pub overflow: Abs,
}

impl<'a> NoteSet<'a> {
    /// The notes that `page` starts with: those carried over.
    pub(super) fn new(page: &'a PageRequest<'a>) -> Self {
        let mut set = Self {
            page,
            runs: Vec::new(),
            carried: 0,
        };
        for carried in page.queue {
            set.push(&carried.note, carried.from.clone(), None);
        }
        set.carried = set.runs.len();
        set
    }

    /// Adds the notes anchored in leaf `k` of the body's run `body`: in the
    /// line it is.
    pub(super) fn anchor(&mut self, body: &Run<'a>, k: usize) {
        let leaf = &body.leaves[k];
        // A part of a band of a grid anchors the notes of the lines of its
        // cells it places, in the order of the cells (SPEC §7.4).
        if let Some(piece) = &leaf.piece {
            let flow: &'a super::flow::Flow = body.flow;
            let super::flow::Leaf::Band(band) = &flow.items[leaf.item].leaf else {
                return;
            };
            if piece.repeat {
                return;
            }
            for cell_line in &piece.lines {
                // Through the bands of grids nested in the cells.
                let mut band: &'a super::flow::Band = band;
                let mut found = None;
                for (n, &(cell, item)) in cell_line.path.iter().enumerate() {
                    let Some(leaf) = band.cells.get(cell).and_then(|c| c.flow.items.get(item))
                    else {
                        break;
                    };
                    match &leaf.leaf {
                        super::flow::Leaf::Band(inner) if n + 1 < cell_line.path.len() => {
                            band = inner;
                        }
                        super::flow::Leaf::Paragraph(info) if n + 1 == cell_line.path.len() => {
                            found = Some(info);
                        }
                        _ => break,
                    }
                }
                let Some(info) = found else {
                    continue;
                };
                let Some(range) = info
                    .layout
                    .lines
                    .get(cell_line.line)
                    .map(|l| l.range.clone())
                else {
                    continue;
                };
                for anchor in &info.notes {
                    let at = anchor.range.start;
                    if range.start <= at && (at < range.end || at == range.start) {
                        self.push(&anchor.note, State::START, Some(k));
                    }
                }
            }
            return;
        }
        let Some((paragraph, line)) = &leaf.line else {
            return;
        };
        let range = &paragraph.layout.lines[*line].range;
        // The notes of the flow's paragraph, which outlives the run: a
        // paragraph broken again at another width has the same.
        let super::flow::Leaf::Paragraph(info) = &body.flow.items[leaf.item].leaf else {
            return;
        };
        for anchor in &info.notes {
            let at = anchor.range.start;
            if range.start <= at && (at < range.end || at == range.start) {
                self.push(&anchor.note, State::START, Some(k));
            }
        }
    }

    fn push(&mut self, note: &'a Arc<Note>, from: State, anchor: Option<usize>) {
        let mut run = Run::new(self.page, &note.flow, from.clone(), false);
        let mut n = 0;
        while run.get(n).is_some() {
            n += 1;
        }
        // What is left of a note carried over starts with its mark.
        let marked = anchor.is_none() && n > 0 && note.mark.is_some();
        if let Some(mark) = note.mark.as_ref().filter(|_| marked) {
            run.prepend(mark);
            n += 1;
        }
        let mut heights = Vec::with_capacity(n);
        let mut y = match run.leaves.first() {
            Some(first) => first.before.walk(Walk::Top, |_, _| {}),
            None => Abs::zero(),
        };
        for j in 0..n {
            y += run.leaves[j].height();
            heights.push(y + run.boundary(j + 1).walk(Walk::Bottom, |_, _| {}));
            if j + 1 < n {
                y += run.boundary(j + 1).gap();
            }
        }
        let gap = match (self.runs.last(), run.leaves.first()) {
            (Some(prev), Some(first)) => between(&prev.note.flow.end, &first.before),
            _ => Abs::zero(),
        };
        self.runs.push(NoteRun {
            note,
            anchor,
            from,
            run,
            heights,
            gap,
            marked,
        });
    }

    /// Places the notes in `space`, the height the body leaves below its
    /// content. With `force`, for a page's first leaf, which it must take,
    /// the notes anchored in it start even where they do not fit: the
    /// notes carried over wait for them, or else they overflow the page.
    pub(super) fn place(&self, space: Abs, force: bool) -> Placement {
        if self.runs.is_empty() {
            return Placement {
                ok: true,
                ..Placement::default()
            };
        }
        let placed = self.try_place(space, false, false);
        if placed.ok || !force {
            return placed;
        }
        let placed = self.try_place(space, true, false);
        if placed.ok {
            return placed;
        }
        self.try_place(space, true, true)
    }

    /// Places the notes in order in `space`: whole while they fit, then the
    /// next split after as many leaves as fit. With `defer`, the notes
    /// carried over wait for the next page; with `overflow`, a note
    /// anchored on the page places its first leaf even where it does not
    /// fit.
    fn try_place(&self, space: Abs, defer: bool, overflow: bool) -> Placement {
        let avail = space - SKIP;
        let mut out = Placement {
            ok: true,
            ..Placement::default()
        };
        let mut used = Abs::zero();
        // The last note placed, which the next is spaced from.
        let mut last: Option<usize> = None;
        for (i, run) in self.runs.iter().enumerate() {
            let carried = i < self.carried;
            if defer && carried {
                // It waits for the next page; the notes anchored here start.
                out.leaves.push(0);
                continue;
            }
            if out.split && (carried || !overflow) {
                out.leaves.push(0);
                out.ok &= carried;
                continue;
            }
            let gap = last.map_or(Abs::zero(), |prev| self.gap(prev, out.leaves[prev], i));
            let n = run.heights.len();
            let fits = |h: Abs| (used + gap + h).to_pt() <= avail.to_pt() + EPSILON;
            if n == 0 {
                out.leaves.push(0);
                continue;
            }
            if fits(run.heights[n - 1]) {
                used += gap + run.heights[n - 1];
                out.leaves.push(n);
                last = Some(i);
                continue;
            }
            let mut m = 0;
            while m + 1 < n && fits(run.heights[m]) {
                m += 1;
            }
            if run.marked && m == 1 {
                m = 0;
            }
            if m == 0 && !carried {
                if overflow {
                    m = 1;
                    out.overflow = out.overflow.max(used + gap + run.heights[0] - avail);
                } else {
                    out.ok = false;
                }
            }
            if m > 0 {
                used += gap + run.heights[m - 1];
                last = Some(i);
            }
            out.leaves.push(m);
            out.split = true;
        }
        if defer && self.carried > 0 {
            // The notes that wait leave the page no room for more lines.
            out.split = true;
        }
        if last.is_some() {
            out.height = SKIP + used;
        }
        out
    }

    /// The space between note `prev`, of which the page places `placed`
    /// leaves, and note `next`, both on the page: the gap computed against
    /// the note before `next` when that is `prev` and whole, else the
    /// boundary after `prev`'s last leaf placed joined with `next`'s first.
    pub(super) fn gap(&self, prev: usize, placed: usize, next: usize) -> Abs {
        let before = &self.runs[prev];
        let whole = placed == before.heights.len();
        if prev + 1 == next && whole {
            return self.runs[next].gap;
        }
        match self.runs[next].run.leaves.first() {
            Some(first) => between(before.run.boundary(placed), &first.before),
            None => Abs::zero(),
        }
    }

    /// The first leaf of the first note, which a page that holds nothing
    /// else must take even where it overflows the page, `space` high.
    pub(super) fn place_first(&self, space: Abs) -> Placement {
        let mut leaves = vec![0; self.runs.len()];
        let Some(run) = self.runs.first().filter(|r| !r.heights.is_empty()) else {
            return Placement {
                ok: true,
                leaves,
                ..Placement::default()
            };
        };
        // A mark goes with the line after it.
        let take = if run.marked { 2 } else { 1 }.min(run.heights.len());
        leaves[0] = take;
        let height = SKIP + run.heights[take - 1];
        Placement {
            leaves,
            height,
            ok: true,
            split: true,
            overflow: (height - space).max(Abs::zero()),
        }
    }

    /// The notes that a page placed as `placed` carries over to the next:
    /// those it split, and those carried over that waited, in order.
    pub(super) fn carried(&self, placed: &Placement) -> Vec<Carried> {
        let mut out = Vec::new();
        for (i, run) in self.runs.iter().enumerate() {
            let m = placed.leaves.get(i).copied().unwrap_or(0);
            if m == run.heights.len() && m > 0 {
                continue;
            }
            if m == 0 && run.anchor.is_some() && !run.heights.is_empty() {
                // A note that cannot start is left to its anchor's page.
                continue;
            }
            if run.heights.is_empty() {
                continue;
            }
            out.push(Carried {
                note: run.note.clone(),
                from: if m == 0 {
                    run.from.clone()
                } else {
                    run.run.state_at(m, &run.from)
                },
            });
        }
        out
    }
}

/// The space between the end of one note and the start of the next on a
/// page, besides the padding and borders that each note's height counts:
/// the spaces of their blocks, collapsed, and the leading around them.
fn between(end: &Boundary, start: &Boundary) -> Abs {
    let mut joined = end.clone();
    joined.parts.extend(start.parts.iter().cloned());
    joined.gap() - end.walk(Walk::Bottom, |_, _| {}) - start.walk(Walk::Top, |_, _| {})
}

/// The separator rule above the notes whose top is at `top`, for a body
/// `width` wide: a third of it, from its start.
pub(super) fn separator(top: Abs, width: Abs) -> (Point, FragmentItem) {
    let y = top + SKIP - RULE_ABOVE;
    let mut path = Path::new();
    path.move_to(Point::new(Abs::zero(), Abs::zero()));
    path.line_to(Point::new(width / 3.0, Abs::zero()));
    let stroke = Stroke::solid(Paint::Solid(Color::BLACK), RULE);
    (
        Point::new(Abs::zero(), y),
        FragmentItem::Shape {
            shape: Shape::stroked(path, stroke),
            source: SourceRef::DETACHED,
        },
    )
}
