//! The grid primitive (SPEC §7.4): tables and layout grids.
//!
//! A `grid` element holds cells: each block of its body, a `cell` element
//! or any other block standing for a cell of its own. This module decides
//! where they stand and how wide their columns are; the flow sets each
//! row as a *band* (`super::flow`), and the page builder breaks between
//! bands and, with `row-breakable: true`, inside them (`super::page`).
//!
//! - **Tracks**: `column-tracks` and `row-tracks` take fixed lengths
//!   (`2cm`, `30%` of the grid's width), fractions (`1fr`), `auto`,
//!   `minmax(a, b)` and `repeat(n, track)`, as a list or one value; an
//!   integer `n` is `n` columns of `auto`. Without `column-tracks`, a grid
//!   has as many columns as its cells' explicit places and spans ask for,
//!   at least one. Cells placed past the tracks given add `auto` tracks.
//! - **Placement**: cells are placed in the order they are written, each
//!   at its `column` and `row` where it gives them, else by auto-flow:
//!   the first place, row by row, from after the cell placed before it,
//!   where it fits without covering a cell already placed. A cell spans
//!   `colspan` columns, at most as many as the grid has from its column,
//!   and `rowspan` rows.
//! - **Track sizing** ([`size_tracks`]): fixed tracks take their length;
//!   `auto` tracks start at the largest min-content of the cells that span
//!   only them, and share the width left, up to their largest
//!   max-content, in proportion to how far each is below it; one more
//!   pass gives a spanning cell's missing min-content to the `auto`
//!   tracks it spans, the cells that span fewest tracks first; what is
//!   left goes to the `fr` tracks in proportion. When the fixed tracks and
//!   the min-contents need more than the grid's width, the `fr` tracks
//!   are empty and the grid is overfull.
//! - **Measures** ([`Measurer`]): a cell's min-content and max-content
//!   widths are those of its paragraphs ([`super::Cache::measures`]),
//!   displays and nested grids, with the padding and borders of the cell
//!   and of the blocks between them.
//! - **Rows**: rows joined by a cell that spans them are one band, so
//!   that a page break never falls through a cell; `header-rows: n` makes
//!   the bands of the first `n` rows header bands.

use std::collections::HashMap;

use ecow::EcoVec;
use folio_component::{Abs, Location, Value};
use folio_syntax::Span;

use super::master::{BlockBox, length};
use super::paragraph::Measures;
use crate::style::Properties;
use crate::text::{CellSpec, MAX_TRACKS, TableRole, TextBlock, TextFlow};

/// Lengths closer than this, in points, are equal.
const EPSILON: f64 = 1e-6;

/// The most a track list may expand to with `repeat(..)`: more tracks are
/// left out.
const MAX_REPEAT: usize = MAX_TRACKS;

/// A track as `column-tracks` or `row-tracks` gives it: the least size it
/// takes and how far it may grow.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Track {
    /// Its least size: a length, or `None` for the largest min-content of
    /// the cells that span only it (`auto`).
    pub min: Option<Abs>,
    /// How far it grows.
    pub max: Grow,
}

/// How far a track grows.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Grow {
    /// Up to a length.
    Fixed(Abs),
    /// Up to the largest max-content of the cells that span only it.
    Auto,
    /// By its share, its fraction, of the width left after every other
    /// track.
    Fr(f64),
}

impl Track {
    /// A track of a fixed length.
    pub const fn fixed(length: Abs) -> Self {
        Self {
            min: Some(length),
            max: Grow::Fixed(length),
        }
    }

    /// An `auto` track.
    pub const AUTO: Self = Self {
        min: None,
        max: Grow::Auto,
    };

    /// A track of a fraction of the width left: empty when nothing is
    /// left.
    pub const fn fr(fraction: f64) -> Self {
        Self {
            min: Some(Abs::zero()),
            max: Grow::Fr(fraction),
        }
    }

    /// Whether it grows to the cells' content, as `auto` does.
    fn is_auto(&self) -> bool {
        self.min.is_none() || self.max == Grow::Auto
    }

    /// Its fraction, if it is one.
    fn fraction(&self) -> Option<f64> {
        match self.max {
            Grow::Fr(f) => Some(f),
            _ => None,
        }
    }
}

/// A track list from the value of `column-tracks` or `row-tracks`: `None`
/// for `auto` or `none`, which give no tracks. `font_size` resolves em
/// lengths, and ratios are of `width`. What is not a track is left out.
pub fn parse_tracks(value: &Value, font_size: Abs, width: Abs) -> Option<Vec<Track>> {
    let mut out = Vec::new();
    match value {
        Value::Auto | Value::None => return None,
        // `column-tracks: 3`: three `auto` columns.
        Value::Int(n) => {
            let n = usize::try_from(*n).unwrap_or(0).min(MAX_TRACKS);
            out.extend(std::iter::repeat_n(Track::AUTO, n));
        }
        Value::Array(items) => {
            for item in items {
                push_track(item, font_size, width, &mut out);
            }
        }
        other => push_track(other, font_size, width, &mut out),
    }
    (!out.is_empty()).then_some(out)
}

/// Adds the tracks of one item of a track list: a track, or `repeat(..)`.
fn push_track(value: &Value, font_size: Abs, width: Abs, out: &mut Vec<Track>) {
    if let Value::Call(name, args) = value
        && name.as_str() == "repeat"
    {
        let Some(Value::Int(n)) = args.first() else {
            return;
        };
        let tracks: Vec<Track> = args[1..]
            .iter()
            .filter_map(|v| track(v, font_size, width))
            .collect();
        let n = usize::try_from(*n).unwrap_or(0);
        for _ in 0..n {
            if out.len() + tracks.len() > MAX_REPEAT || tracks.is_empty() {
                break;
            }
            out.extend_from_slice(&tracks);
        }
        return;
    }
    if let Some(t) = track(value, font_size, width)
        && out.len() < MAX_TRACKS
    {
        out.push(t);
    }
}

/// One track: a length, a ratio, a fraction, `auto` or `minmax(a, b)`.
fn track(value: &Value, font_size: Abs, width: Abs) -> Option<Track> {
    Some(match value {
        Value::Call(name, args) if name.as_str() == "minmax" => {
            let [min, max] = args.as_slice() else {
                return None;
            };
            let min = match min {
                Value::Auto => None,
                other => Some(fixed(other, font_size, width)?),
            };
            let max = match max {
                Value::Auto => Grow::Auto,
                Value::Fraction(f) => Grow::Fr(f.get().max(0.0)),
                other => Grow::Fixed(fixed(other, font_size, width)?),
            };
            Track { min, max }
        }
        Value::Auto => Track::AUTO,
        Value::Keyword(k) if k.as_str() == "auto" => Track::AUTO,
        Value::Fraction(f) => Track::fr(f.get().max(0.0)),
        other => Track::fixed(fixed(other, font_size, width)?),
    })
}

/// A fixed size: a length, an em length at `font_size`, or a ratio of
/// `width`; at least zero.
fn fixed(value: &Value, font_size: Abs, width: Abs) -> Option<Abs> {
    let abs = match value {
        Value::Length(abs) => *abs,
        Value::Em(em) => font_size * em.get(),
        Value::Ratio(ratio) => width * ratio.get(),
        _ => return None,
    };
    abs.is_finite().then(|| abs.max(Abs::zero()))
}

/// Where a cell stands in its grid: its first column and row, from 0, and
/// how many it spans.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Place {
    /// Its first column.
    pub column: usize,
    /// Its first row.
    pub row: usize,
    /// The columns it spans, at least one.
    pub colspan: usize,
    /// The rows it spans, at least one.
    pub rowspan: usize,
}

/// Places cells in a grid of `columns` columns, in order: at their
/// explicit places where they give them, else by auto-flow (see the
/// module's documentation). Returns their places and the number of rows.
pub fn place(cells: &[CellSpec], columns: usize) -> (Vec<Place>, usize) {
    let columns = columns.max(1);
    let mut taken: Vec<Vec<bool>> = Vec::new();
    let free = |taken: &Vec<Vec<bool>>, row: usize, column: usize, place: &Place| {
        (row..row + place.rowspan).all(|r| {
            (column..column + place.colspan).all(|c| !taken.get(r).is_some_and(|cells| cells[c]))
        })
    };
    let mut cursor = (0usize, 0usize);
    let mut out = Vec::with_capacity(cells.len());
    for spec in cells {
        let column = spec.column.map(|c| c.saturating_sub(1).min(columns - 1));
        let colspan = spec.colspan.clamp(1, columns - column.unwrap_or(0));
        let mut place = Place {
            column: 0,
            row: 0,
            colspan,
            rowspan: spec.rowspan.max(1),
        };
        let row = spec.row.map(|r| r.saturating_sub(1).min(MAX_TRACKS));
        place.rowspan = place.rowspan.min(MAX_TRACKS);
        match (column, row) {
            (Some(c), Some(r)) => {
                place.column = c;
                place.row = r;
            }
            (None, Some(r)) => {
                place.row = r;
                place.column = (0..=columns - colspan)
                    .find(|&c| free(&taken, r, c, &place))
                    .unwrap_or(0);
            }
            (Some(c), None) => {
                let mut row = cursor.0 + usize::from(c < cursor.1);
                while !free(&taken, row, c, &place) {
                    row += 1;
                }
                place.column = c;
                place.row = row;
                cursor = (row, c + colspan);
            }
            (None, None) => {
                let (mut row, mut c) = cursor;
                loop {
                    if c + colspan > columns {
                        row += 1;
                        c = 0;
                        continue;
                    }
                    if free(&taken, row, c, &place) {
                        break;
                    }
                    c += 1;
                }
                place.column = c;
                place.row = row;
                cursor = (row, c + colspan);
            }
        }
        let rows = place.row + place.rowspan;
        if taken.len() < rows {
            taken.resize(rows, vec![false; columns]);
        }
        for cells in &mut taken[place.row..rows] {
            for cell in &mut cells[place.column..place.column + place.colspan] {
                *cell = true;
            }
        }
        out.push(place);
    }
    (out, taken.len())
}

/// The sizes of a grid's tracks in a grid `width` wide, from the tracks and
/// the cells that span them, each with its first track, the number of
/// tracks it spans and its measures (see the module's documentation).
/// Returns the sizes and how far they need more than `width`, or zero.
///
/// Every size is at least zero and finite. A fixed track takes its length,
/// an `auto` track at least the min-content of every cell that spans only
/// it, and a cell that spans tracks of which one is `auto` gets its
/// min-content from them. When the tracks need less than `width`, the
/// sizes add up to `width` if any track is a fraction.
pub fn size_tracks(
    tracks: &[Track],
    cells: &[(usize, usize, Measures)],
    width: Abs,
) -> (Vec<Abs>, Abs) {
    let n = tracks.len();
    let width = if width.is_finite() {
        width.max(Abs::zero())
    } else {
        Abs::zero()
    };
    // Fixed tracks take their length; `auto` ones start at the largest
    // min-content of the cells that span only them.
    let mut sizes: Vec<Abs> = tracks
        .iter()
        .map(|t| t.min.unwrap_or(Abs::zero()))
        .collect();
    let mut limits: Vec<Abs> = tracks
        .iter()
        .map(|t| match t.max {
            Grow::Fixed(max) => max,
            Grow::Auto | Grow::Fr(_) => Abs::zero(),
        })
        .collect();
    for &(first, span, m) in cells {
        if span != 1 || first >= n {
            continue;
        }
        let track = tracks[first];
        if track.min.is_none() {
            sizes[first] = sizes[first].max(m.min);
        }
        if track.max == Grow::Auto {
            limits[first] = limits[first].max(m.max);
        }
    }
    for (size, limit) in sizes.iter_mut().zip(&mut limits) {
        *limit = (*limit).max(*size);
    }

    // The width left goes to the tracks that grow to a limit, up to it,
    // in proportion to how far each is below it.
    let used: f64 = sizes.iter().map(|s| s.to_pt()).sum();
    let left = width.to_pt() - used;
    let grows = |t: &Track| t.fraction().is_none();
    let deficit: f64 = (0..n)
        .filter(|&i| grows(&tracks[i]))
        .map(|i| (limits[i] - sizes[i]).to_pt())
        .sum();
    if left > EPSILON && deficit > EPSILON {
        let share = (left / deficit).min(1.0);
        for i in (0..n).filter(|&i| grows(&tracks[i])) {
            let gap = (limits[i] - sizes[i]).to_pt();
            sizes[i] += Abs::pt(gap * share);
        }
    }

    // A spanning cell's missing min-content goes to the `auto` tracks it
    // spans, the cells that span fewest tracks first; where it spans none,
    // to its fractions.
    let mut spanning: Vec<&(usize, usize, Measures)> = cells
        .iter()
        .filter(|(first, span, _)| *span > 1 && *first < n)
        .collect();
    spanning.sort_by_key(|(_, span, _)| *span);
    for &&(first, span, m) in &spanning {
        let range = first..(first + span).min(n);
        let have: f64 = sizes[range.clone()].iter().map(|s| s.to_pt()).sum();
        let missing = m.min.to_pt() - have;
        if missing <= EPSILON {
            continue;
        }
        let mut targets: Vec<usize> = range.clone().filter(|&i| tracks[i].is_auto()).collect();
        if targets.is_empty() {
            targets = range.filter(|&i| tracks[i].fraction().is_some()).collect();
        }
        if targets.is_empty() {
            continue;
        }
        let each = missing / targets.len() as f64;
        for i in targets {
            sizes[i] += Abs::pt(each);
        }
    }

    // What is left goes to the fractions, in proportion.
    let used: f64 = sizes.iter().map(|s| s.to_pt()).sum();
    let left = width.to_pt() - used;
    let total: f64 = tracks.iter().filter_map(Track::fraction).sum();
    if left > EPSILON && total > EPSILON {
        for (size, track) in sizes.iter_mut().zip(tracks) {
            if let Some(f) = track.fraction() {
                *size += Abs::pt(left * f / total);
            }
        }
    }
    for size in &mut sizes {
        if !size.is_finite() || *size < Abs::zero() {
            *size = Abs::zero();
        }
    }
    let used: f64 = sizes.iter().map(|s| s.to_pt()).sum();
    let over = used - width.to_pt();
    let overflow = if over > 0.1 {
        Abs::pt(over)
    } else {
        Abs::zero()
    };
    (sizes, overflow)
}

/// A grid laid out in a width: where its cells stand and how wide its
/// columns are.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct GridLayout {
    /// The grid element instance.
    pub location: Location,
    /// The grid's element.
    pub span: Span,
    /// The width of each column.
    pub columns: EcoVec<Abs>,
    /// Where each column starts, from the left of the grid's content box.
    pub offsets: EcoVec<Abs>,
    /// Each cell's place, in the order of the grid's blocks.
    pub places: EcoVec<Place>,
    /// The number of rows.
    pub rows: usize,
    /// The least height of each row, from `row-tracks`.
    pub row_mins: EcoVec<Abs>,
    /// How many of the first rows are header rows: as many as
    /// `header-rows` asks for, and the rows that cells spanning from them
    /// join to them.
    pub header_rows: usize,
    /// How far the columns are wider than the grid's content box, or zero.
    pub overflow: Abs,
}

impl GridLayout {
    /// Where a cell's box starts, from the left of the body, and how wide
    /// it is, in a grid whose content box starts at `x`.
    pub fn cell_box(&self, place: &Place, x: Abs) -> (Abs, Abs) {
        let start = self.offsets.get(place.column).copied().unwrap_or_default();
        let end = place.column + place.colspan;
        let width =
            total(&self.columns[place.column.min(self.columns.len())..end.min(self.columns.len())]);
        (x + start, width.max(Abs::pt(1.0)))
    }
}

/// The spec of each cell of a grid, in order: a `cell` element's, or one
/// column and row for another block.
pub(super) fn cell_specs(grid: &TextBlock) -> Vec<CellSpec> {
    grid.children
        .iter()
        .map(|child| match child {
            TextFlow::Block(TextBlock {
                table: Some(TableRole::Cell(spec)),
                ..
            }) => *spec,
            _ => CellSpec {
                colspan: 1,
                rowspan: 1,
                column: None,
                row: None,
            },
        })
        .collect()
}

/// Measures the content of blocks (SPEC §7.4) and lays out the grids
/// among them.
pub(super) struct Measurer<'a> {
    /// The properties of the build.
    pub(super) props: &'a Properties,
    /// A paragraph's measures, by its index in the build's text.
    pub(super) paragraph: &'a dyn Fn(usize) -> Measures,
    /// The natural width of the display whose element is at a location,
    /// if it is one.
    pub(super) display: &'a dyn Fn(Location) -> Option<Abs>,
    /// The grids laid out, by location, as they are measured or laid
    /// out, so that each is laid out once per width.
    pub(super) grids: HashMap<(Location, Abs), GridLayout>,
}

impl Measurer<'_> {
    /// The grid of block `grid`, whose box is `boxed`, laid out at its
    /// content width.
    pub(super) fn grid(&mut self, grid: &TextBlock, boxed: &BlockBox) -> GridLayout {
        let (_, width) = boxed.content();
        if let Some(layout) = self.grids.get(&(grid.location, width)) {
            return layout.clone();
        }
        let layout = self.lay_out(grid, width);
        self.grids.insert((grid.location, width), layout.clone());
        layout
    }

    fn lay_out(&mut self, grid: &TextBlock, width: Abs) -> GridLayout {
        let props = self.props;
        let font_size = length(&grid.style, props, "font-size");
        let value = |name: &str| grid.style.value(props, props.builtin(name)).cloned();
        let given = value("column-tracks")
            .and_then(|v| parse_tracks(&v, font_size, width))
            .unwrap_or_default();
        let specs = cell_specs(grid);
        // As many columns as the tracks give, or the cells ask for.
        let asked = specs
            .iter()
            .map(|s| s.column.unwrap_or(1).saturating_sub(1) + s.colspan.min(MAX_TRACKS))
            .max()
            .unwrap_or(1);
        let count = given.len().max(asked).clamp(1, MAX_TRACKS);
        let mut tracks = given;
        tracks.resize(count, Track::AUTO);
        let (places, rows) = place(&specs, count);
        let measures: Vec<(usize, usize, Measures)> = grid
            .children
            .iter()
            .zip(&places)
            .map(|(child, place)| {
                (
                    place.column,
                    place.colspan,
                    self.flow_measures(child, width),
                )
            })
            .collect();
        let (columns, overflow) = size_tracks(&tracks, &measures, width);
        let mut offsets = EcoVec::with_capacity(columns.len());
        let mut x = Abs::zero();
        for c in &columns {
            offsets.push(x);
            x += *c;
        }
        let header_rows = match grid.table {
            Some(TableRole::Grid { header_rows }) => header_rows,
            _ => 0,
        };
        let row_tracks = value("row-tracks")
            .and_then(|v| parse_tracks(&v, font_size, width))
            .unwrap_or_default();
        let row_mins = (0..rows)
            .map(|r| row_tracks.get(r).and_then(|t| t.min).unwrap_or_default())
            .collect();
        let mut layout = GridLayout {
            location: grid.location,
            span: grid.span,
            columns: columns.into_iter().collect(),
            offsets,
            places: places.into_iter().collect(),
            rows,
            row_mins,
            header_rows: header_rows.min(rows),
            overflow,
        };
        // A cell that spans from the header rows into the rows after them
        // makes those header rows too, as they are set again with them.
        if layout.header_rows > 0 {
            layout.header_rows = bands(&layout)
                .into_iter()
                .filter(|&(start, _)| start < layout.header_rows)
                .map(|(_, end)| end + 1)
                .max()
                .unwrap_or(layout.header_rows);
        }
        layout
    }

    /// The measures of what a block holds, with the padding and borders of
    /// the block, in a grid `width` wide, which the ratios of nested grids'
    /// tracks are of.
    fn flow_measures(&mut self, child: &TextFlow, width: Abs) -> Measures {
        match child {
            TextFlow::Paragraph(i) => (self.paragraph)(*i),
            TextFlow::Block(block) => self.block_measures(block, width),
        }
    }

    fn block_measures(&mut self, block: &TextBlock, width: Abs) -> Measures {
        let boxed = BlockBox::of(&block.style, self.props, block.span, Abs::zero(), width);
        let inset = boxed.inset();
        let extra = inset.left + inset.right;
        let mut out = Measures::default();
        let mut grow = |m: Measures| {
            out.min = out.min.max(m.min);
            out.max = out.max.max(m.max);
        };
        if matches!(block.table, Some(TableRole::Grid { .. })) {
            // A nested grid at the width it would have alone: its tracks'
            // least sizes, and its sizes with every cell at max-content.
            let inner = boxed.content().1;
            let layout = self.grid(block, &boxed);
            let font_size = length(&block.style, self.props, "font-size");
            let tracks = block
                .style
                .value(self.props, self.props.builtin("column-tracks"))
                // A ratio of the cell's width needs no width of its own.
                .and_then(|v| parse_tracks(v, font_size, Abs::zero()))
                .unwrap_or_default();
            let mut min = vec![Abs::zero(); layout.columns.len()];
            let mut max = vec![Abs::zero(); layout.columns.len()];
            for (child, place) in block.children.iter().zip(&layout.places) {
                let m = self.flow_measures(child, inner);
                if place.colspan == 1 && place.column < min.len() {
                    min[place.column] = min[place.column].max(m.min);
                    max[place.column] = max[place.column].max(m.max);
                }
            }
            for (c, track) in tracks.iter().enumerate().take(min.len()) {
                if let Some(fixed) = track.min {
                    min[c] = min[c].max(fixed);
                    max[c] = max[c].max(fixed);
                }
            }
            grow(Measures {
                min: total(&min),
                max: total(&max),
            });
        } else {
            if let Some(natural) = (self.display)(block.location).filter(|_| block.part.is_none()) {
                grow(Measures {
                    min: natural,
                    max: natural,
                });
            }
            for child in &block.children {
                let m = self.flow_measures(child, width);
                grow(m);
            }
        }
        Measures {
            min: out.min + extra,
            max: out.max + extra,
        }
    }
}

/// The bands of a grid: the rows each joins, from the first to the last,
/// in order. Rows joined by a cell that spans them are one band.
pub(super) fn bands(layout: &GridLayout) -> Vec<(usize, usize)> {
    let mut reach: Vec<usize> = (0..layout.rows).collect();
    for place in &layout.places {
        let last = place.row + place.rowspan - 1;
        if let Some(r) = reach.get_mut(place.row) {
            *r = (*r).max(last.min(layout.rows.saturating_sub(1)));
        }
    }
    let mut out = Vec::new();
    let mut row = 0;
    while row < layout.rows {
        let mut end = reach[row];
        let mut r = row;
        while r <= end {
            end = end.max(reach[r]);
            r += 1;
        }
        out.push((row, end));
        row = end + 1;
    }
    out
}

/// The sum of lengths.
pub(super) fn total(lengths: &[Abs]) -> Abs {
    lengths.iter().fold(Abs::zero(), |sum, l| sum + *l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pt(v: f64) -> Abs {
        Abs::pt(v)
    }

    fn m(min: f64, max: f64) -> Measures {
        Measures {
            min: pt(min),
            max: pt(max),
        }
    }

    #[test]
    fn fixed_auto_and_fractions() {
        let tracks = [
            Track::fixed(pt(50.0)),
            Track::AUTO,
            Track::fr(1.0),
            Track::fr(3.0),
        ];
        let cells = [(1, 1, m(20.0, 60.0))];
        let (sizes, over) = size_tracks(&tracks, &cells, pt(300.0));
        assert_eq!(over, Abs::zero());
        assert_eq!(sizes[0], pt(50.0));
        // The `auto` track reaches its max-content, and the fractions
        // share the rest, 190pt, one to three.
        assert!((sizes[1] - pt(60.0)).to_pt().abs() < 1e-6);
        assert!((sizes[2] - pt(47.5)).to_pt().abs() < 1e-6);
        assert!((sizes[3] - pt(142.5)).to_pt().abs() < 1e-6);
    }

    #[test]
    fn auto_tracks_share_in_proportion_to_their_deficit() {
        let tracks = [Track::AUTO, Track::AUTO];
        let cells = [(0, 1, m(10.0, 110.0)), (1, 1, m(10.0, 210.0))];
        let (sizes, _) = size_tracks(&tracks, &cells, pt(170.0));
        // 150pt left, deficits 100 and 200.
        assert!((sizes[0] - pt(60.0)).to_pt().abs() < 1e-6);
        assert!((sizes[1] - pt(110.0)).to_pt().abs() < 1e-6);
    }

    #[test]
    fn spanning_cells_grow_auto_tracks() {
        let tracks = [Track::fixed(pt(20.0)), Track::AUTO, Track::AUTO];
        let cells = [(0, 3, m(100.0, 100.0))];
        let (sizes, over) = size_tracks(&tracks, &cells, pt(50.0));
        assert_eq!(sizes[0], pt(20.0));
        assert!((sizes[1] - pt(40.0)).to_pt().abs() < 1e-6);
        assert!((sizes[2] - pt(40.0)).to_pt().abs() < 1e-6);
        assert!((over - pt(50.0)).to_pt().abs() < 1e-6);
    }

    #[test]
    fn overfull_grids_have_empty_fractions() {
        let tracks = [Track::fixed(pt(80.0)), Track::fr(1.0)];
        let (sizes, over) = size_tracks(&tracks, &[], pt(50.0));
        assert_eq!(sizes, [pt(80.0), Abs::zero()]);
        assert!((over - pt(30.0)).to_pt().abs() < 1e-6);
    }

    #[test]
    fn auto_flow_skips_taken_cells() {
        let one = CellSpec {
            colspan: 1,
            rowspan: 1,
            column: None,
            row: None,
        };
        let tall = CellSpec { rowspan: 2, ..one };
        let wide = CellSpec { colspan: 2, ..one };
        let (places, rows) = place(&[tall, one, one, wide, one], 3);
        let at: Vec<(usize, usize)> = places.iter().map(|p| (p.row, p.column)).collect();
        assert_eq!(at, [(0, 0), (0, 1), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(rows, 3);
    }

    #[test]
    fn explicit_places_and_clamped_spans() {
        let spec = CellSpec {
            colspan: 5,
            rowspan: 1,
            column: Some(2),
            row: Some(3),
        };
        let (places, rows) = place(&[spec], 3);
        assert_eq!(
            places[0],
            Place {
                column: 1,
                row: 2,
                colspan: 2,
                rowspan: 1
            }
        );
        assert_eq!(rows, 3);
    }

    #[test]
    fn placement_is_total() {
        let spec = CellSpec {
            colspan: 0,
            rowspan: 0,
            column: Some(0),
            row: Some(0),
        };
        let (places, rows) = place(&[spec], 3);
        assert_eq!(
            places[0],
            Place {
                column: 0,
                row: 0,
                colspan: 1,
                rowspan: 1
            }
        );
        assert_eq!(rows, 1);
    }

    #[test]
    fn track_lists() {
        use folio_component::{Name, Ratio, Scalar};
        let name = |n: &str| Name::new(n).unwrap();
        let value = Value::Array(EcoVec::from([
            Value::Length(pt(10.0)),
            Value::Call(
                name("repeat"),
                EcoVec::from([Value::Int(2), Value::Fraction(Scalar::new(1.0))]),
            ),
            Value::Call(
                name("minmax"),
                EcoVec::from([Value::Auto, Value::Length(pt(40.0))]),
            ),
            Value::Ratio(Ratio::new(0.5)),
        ]));
        let tracks = parse_tracks(&value, pt(10.0), pt(200.0)).unwrap();
        assert_eq!(
            tracks,
            [
                Track::fixed(pt(10.0)),
                Track::fr(1.0),
                Track::fr(1.0),
                Track {
                    min: None,
                    max: Grow::Fixed(pt(40.0))
                },
                Track::fixed(pt(100.0)),
            ]
        );
        assert_eq!(
            parse_tracks(&Value::Int(3), pt(10.0), pt(1.0))
                .unwrap()
                .len(),
            3
        );
        assert!(parse_tracks(&Value::Auto, pt(10.0), pt(1.0)).is_none());
    }

    fn arb_track() -> impl Strategy<Value = Track> {
        prop_oneof![
            (0.0..200.0f64).prop_map(|v| Track::fixed(pt(v))),
            Just(Track::AUTO),
            (0.1..5.0f64).prop_map(Track::fr),
            ((0.0..100.0f64), (0.0..200.0f64)).prop_map(|(a, b)| Track {
                min: Some(pt(a)),
                max: Grow::Fixed(pt(a.max(b))),
            }),
            (0.0..100.0f64).prop_map(|a| Track {
                min: Some(pt(a)),
                max: Grow::Auto,
            }),
        ]
    }

    fn arb_case() -> impl Strategy<Value = (Vec<Track>, Vec<(usize, usize, Measures)>, f64)> {
        prop::collection::vec(arb_track(), 1..7).prop_flat_map(|tracks| {
            let n = tracks.len();
            let cell = (0..n, 1..=3usize, 0.0..150.0f64, 0.0..300.0f64).prop_map(
                move |(first, span, min, extra)| (first, span.min(n - first), m(min, min + extra)),
            );
            (
                Just(tracks),
                prop::collection::vec(cell, 0..10),
                0.0..800.0f64,
            )
        })
    }

    proptest! {
        /// Track sizing (SPEC §7.4, §14.2): sizes are finite and never
        /// negative; fixed tracks take their length; `auto` tracks hold
        /// the min-content of the cells that span only them; spanning
        /// cells get their min-content where they span an `auto` track;
        /// the overflow is what the sizes need beyond the width; and with
        /// a fraction and room to spare, the sizes fill the width.
        #[test]
        fn track_sizing((tracks, cells, width) in arb_case()) {
            let width = pt(width);
            let (sizes, overflow) = size_tracks(&tracks, &cells, width);
            prop_assert_eq!(sizes.len(), tracks.len());
            for s in &sizes {
                prop_assert!(s.is_finite() && *s >= Abs::zero());
            }
            for (track, size) in tracks.iter().zip(&sizes) {
                if let (Some(min), Grow::Fixed(max)) = (track.min, track.max)
                    && min == max
                {
                    prop_assert!((*size - min).to_pt().abs() < 1e-6);
                }
                if let Some(min) = track.min {
                    prop_assert!(size.to_pt() >= min.to_pt() - 1e-6);
                }
            }
            for &(first, span, m) in &cells {
                if span == 1 && tracks[first].min.is_none() {
                    prop_assert!(sizes[first].to_pt() >= m.min.to_pt() - 1e-6);
                }
                let range = first..first + span;
                if span > 1 && range.clone().any(|i| tracks[i].is_auto()) {
                    let have: f64 = sizes[range].iter().map(|s| s.to_pt()).sum();
                    prop_assert!(have >= m.min.to_pt() - 1e-6);
                }
            }
            let total: f64 = sizes.iter().map(|s| s.to_pt()).sum();
            let over = total - width.to_pt();
            if over > 0.1 {
                prop_assert!((overflow.to_pt() - over).abs() < 1e-6);
            } else {
                prop_assert_eq!(overflow, Abs::zero());
            }
            let fractions = tracks.iter().any(|t| t.fraction().is_some_and(|f| f > 0.0));
            if fractions && overflow == Abs::zero() {
                prop_assert!((total - width.to_pt()).abs() < 1e-6);
            }
        }

        /// Auto-flow places every cell inside the grid's columns, and no
        /// two cells it places cover the same place.
        #[test]
        fn placement(columns in 1..6usize, spans in prop::collection::vec((1..4usize, 1..3usize), 0..20)) {
            let specs: Vec<CellSpec> = spans
                .iter()
                .map(|&(colspan, rowspan)| CellSpec { colspan, rowspan, column: None, row: None })
                .collect();
            let (places, rows) = place(&specs, columns);
            let mut seen = std::collections::HashSet::new();
            for p in &places {
                prop_assert!(p.column + p.colspan <= columns);
                prop_assert!(p.row + p.rowspan <= rows);
                for r in p.row..p.row + p.rowspan {
                    for c in p.column..p.column + p.colspan {
                        prop_assert!(seen.insert((r, c)));
                    }
                }
            }
        }
    }
}
