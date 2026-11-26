// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/run.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Runs of fragments set side by side on a baseline, and lines of runs
//! stacked and aligned at their alignment points.

use super::Lines;
use super::fragment::{Frame, MathFragment};

/// Fragments side by side on one baseline.
pub(crate) fn into_line_frame(fragments: Vec<MathFragment>) -> Frame {
    row_into_line_frame(vec![fragments], &[], None)
}

/// How a cell sits at its alignment point: cells alternate between ending
/// at their point and starting at the one before.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Alternator {
    /// Cells alternate, the first ending at its point.
    Right,
    /// Every cell starts at the point before it.
    None,
}

/// The ascent and descent of a row of cells.
pub(crate) fn measure_row(cells: &[Vec<MathFragment>]) -> (f64, f64) {
    cells
        .iter()
        .flatten()
        .filter(|f| !matches!(f, MathFragment::Space(_)))
        .map(|f| (f.ascent(), f.descent()))
        .fold((0.0, 0.0), |(a, d), (fa, fd)| (a.max(fa), d.max(fd)))
}

/// A row to stack.
pub(crate) struct RowLayout {
    /// Its cells, split at alignment points.
    pub cells: Vec<Vec<MathFragment>>,
    /// The ascent and descent its frame has.
    pub frame_height: (f64, f64),
    /// The ascent and descent it takes when rows are stacked, where it is
    /// not its frame's: a row of a matrix column is as tall as the matrix's
    /// row.
    pub row_height: Option<(f64, f64)>,
}

/// Rows stacked, `leading` apart, starting `start_y` from the top: each a
/// frame at its position, and the size they take.
pub(crate) struct Stacked {
    pub width: f64,
    pub height: f64,
    pub frames: Vec<(Frame, f64, f64)>,
}

/// Lays out the lines of a formula, each a list of cells, aligned at their
/// alignment points; lines without them are centred.
pub(crate) fn layout_multiline(
    rows: Vec<Vec<Vec<MathFragment>>>,
    leading: f64,
    center: bool,
) -> Lines {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0.0f64; ncols];
    for row in &rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(cell.iter().map(MathFragment::width).sum());
        }
    }
    let rows = rows.into_iter().map(|cells| {
        let height = measure_row(&cells);
        RowLayout {
            cells,
            frame_height: height,
            row_height: None,
        }
    });
    let stacked = stack_rows(rows, &widths, Alternator::Right, center, leading, 0.0);
    let mut frame = Frame::new(stacked.width, stacked.height);
    let mut baselines = Vec::with_capacity(stacked.frames.len());
    for (sub, x, y) in stacked.frames {
        baselines.push(y + sub.baseline);
        frame.push_frame(x, y, sub);
    }
    frame.baseline = baselines.first().copied().unwrap_or(0.0);
    Lines { frame, baselines }
}

/// Stacks rows of cells, aligned at the cumulative widths of their
/// columns. Rows without alignment points are centred when `center` is
/// set, and start at the left otherwise.
pub(crate) fn stack_rows(
    rows: impl IntoIterator<Item = RowLayout>,
    widths: &[f64],
    alternator: Alternator,
    center: bool,
    leading: f64,
    start_y: f64,
) -> Stacked {
    let (points, total_width) = cumulative_points(widths);
    let aligned = !points.is_empty();
    let mut frames = Vec::new();
    let mut width = 0.0f64;
    let mut y = start_y;
    for (i, row) in rows.into_iter().enumerate() {
        let (row_ascent, row_descent) = row.row_height.unwrap_or(row.frame_height);
        let sub = row_into_line_frame(row.cells, &points, Some((alternator, row.frame_height)));
        if i > 0 {
            y += leading;
        }
        let top = y + row_ascent - sub.ascent();
        let x = if !aligned && center {
            (total_width - sub.width) / 2.0
        } else {
            0.0
        };
        width = width.max(sub.width).max(total_width);
        y += row_ascent + row_descent;
        frames.push((sub, x, top));
    }
    Stacked {
        width,
        height: y,
        frames,
    }
}

/// A row of cells as a frame, each cell at its alignment point.
fn row_into_line_frame(
    cells: Vec<Vec<MathFragment>>,
    points: &[f64],
    alignment: Option<(Alternator, (f64, f64))>,
) -> Frame {
    let (ascent, descent) = alignment.map_or_else(|| measure_row(&cells), |(_, h)| h);
    let alternator = alignment.map_or(Alternator::Right, |(a, _)| a);
    let mut frame = Frame::new(0.0, ascent + descent);
    frame.baseline = ascent;
    let mut prev_point = 0.0;
    let mut x_end = 0.0;
    for (c, cell) in cells.into_iter().enumerate() {
        let width: f64 = cell.iter().map(MathFragment::width).sum();
        let mut x = match points.get(c) {
            Some(&point) => {
                let right = alternator == Alternator::Right && c % 2 == 0;
                let x = if right { point - width } else { prev_point };
                prev_point = point;
                x
            }
            None => prev_point,
        };
        for fragment in cell {
            let y = ascent - fragment.ascent();
            let w = fragment.width();
            frame.push_frame(x, y, fragment.into_frame());
            x += w;
        }
        x_end = x;
    }
    frame.width = x_end;
    frame
}

/// The alignment points of columns of these widths, and their total width.
/// A single column has none.
fn cumulative_points(widths: &[f64]) -> (Vec<f64>, f64) {
    if widths.len() <= 1 {
        return (Vec::new(), widths.first().copied().unwrap_or(0.0));
    }
    let mut points = Vec::with_capacity(widths.len());
    let mut total = 0.0;
    for &w in widths {
        total += w;
        points.push(total);
    }
    (points, total)
}
