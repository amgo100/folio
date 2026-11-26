// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/table.rs, and
// typst-library v0.15.1, crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Matrices and cases: cells in rows and columns, between delimiters.

use folio_syntax::Span;
use unicode_math_class::MathClass;

use super::fenced::push_delimiter;
use super::font::Constant;
use super::fragment::{Frame, FrameFragment, MathFragment};
use super::run::{Alternator, RowLayout, measure_row, stack_rows};
use super::{Ctx, Style};
use crate::{Cases, Matrix, Row};

/// The space between the rows of a matrix and between cases, in ems.
const ROW_GAP: f64 = 0.2;
/// The space between the columns of a matrix, in ems.
const COL_GAP: f64 = 0.5;
/// How much taller than what they enclose the delimiters of a matrix and
/// of cases are.
const DELIM_SCALE: f64 = 1.1;

/// Lays out a matrix between its delimiters, its cells centred in their
/// columns.
pub(crate) fn layout_matrix(
    ctx: &mut Ctx,
    matrix: &Matrix,
    style: Style,
    out: &mut Vec<MathFragment>,
) {
    let table = layout_table(ctx, &matrix.rows, style, (COL_GAP, ROW_GAP), Kind::Matrix);
    fence(
        ctx,
        table,
        matrix.open,
        matrix.close,
        style,
        matrix.span,
        out,
    );
}

/// Lays out cases behind an opening brace, each case at the start of its
/// line and aligned with the others at its alignment points.
pub(crate) fn layout_cases(
    ctx: &mut Ctx,
    cases: &Cases,
    style: Style,
    out: &mut Vec<MathFragment>,
) {
    let rows: Vec<Vec<Row>> = cases.cases.iter().map(|case| vec![case.clone()]).collect();
    let table = layout_table(ctx, &rows, style, (0.0, ROW_GAP), Kind::Cases);
    fence(ctx, table, Some('{'), None, style, cases.span, out);
}

/// Puts delimiters around a table, a little taller than it.
fn fence(
    ctx: &mut Ctx,
    table: MathFragment,
    open: Option<char>,
    close: Option<char>,
    style: Style,
    span: Span,
    out: &mut Vec<MathFragment>,
) {
    let target = DELIM_SCALE * table.height();
    push_delimiter(ctx, open, MathClass::Opening, target, style, span, out);
    out.push(table);
    push_delimiter(ctx, close, MathClass::Closing, target, style, span, out);
}

/// A cell, split at its alignment points.
struct Cell {
    columns: Vec<Vec<MathFragment>>,
    height: (f64, f64),
}

/// What a table is.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Kind {
    Matrix,
    Cases,
}

/// The space between the parts of a case, in ems: a quad, as TeX's `cases`
/// sets its columns.
const CASES_GAP: f64 = 1.0;

/// Lays out rows of cells in the style of a denominator, `gap` apart
/// (columns, rows, in ems). A matrix centres the rows of each column, and
/// alternates its cells' alignment at their alignment points; cases start
/// every part at its point, a quad after the part before.
fn layout_table(
    ctx: &mut Ctx,
    rows: &[Vec<Row>],
    style: Style,
    gap: (f64, f64),
    kind: Kind,
) -> MathFragment {
    let matrix = kind == Kind::Matrix;
    let size = ctx.font_size(style);
    let nrows = rows.len();
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if nrows == 0 || ncols == 0 {
        return FrameFragment::new(Frame::new(0.0, 0.0), size, MathClass::Normal, style.size)
            .into();
    }
    let (col_gap, row_gap) = (gap.0 * size, gap.1 * size);
    let cell_style = style.den();

    // Every row is at least as tall as a parenthesis, so that matrices of
    // short cells line up with others.
    let (paren_ascent, paren_descent) = ctx
        .glyph("(", Span::detached(), MathClass::Opening, cell_style)
        .map_or((0.0, 0.0), |g| (g.ascent, g.descent));

    let mut cols: Vec<Vec<Cell>> = (0..ncols).map(|_| Vec::with_capacity(nrows)).collect();
    let mut heights = vec![(0.0f64, 0.0f64); nrows];
    for (r, row) in rows.iter().enumerate() {
        for c in 0..ncols {
            let mut columns = match row.get(c) {
                Some(cell) => ctx.layout_aligned(cell.nodes(), cell_style),
                None => vec![Vec::new()],
            };
            if kind == Kind::Cases {
                let quad = CASES_GAP * size;
                let parts = columns.len();
                for part in columns.iter_mut().take(parts.saturating_sub(1)) {
                    part.push(MathFragment::Space(quad));
                }
            }
            let height = measure_row(&columns);
            heights[r].0 = heights[r].0.max(height.0.max(paren_ascent));
            heights[r].1 = heights[r].1.max(height.1.max(paren_descent));
            cols[c].push(Cell { columns, height });
        }
    }

    let total_height =
        heights.iter().map(|&(a, d)| a + d).sum::<f64>() + row_gap * (nrows - 1) as f64;
    let mut frame = Frame::new(0.0, total_height);
    let mut x = 0.0;
    let alternator = if matrix {
        Alternator::Right
    } else {
        Alternator::None
    };
    for col in cols {
        let parts = col.iter().map(|cell| cell.columns.len()).max().unwrap_or(1);
        let mut widths = vec![0.0f64; parts];
        for cell in &col {
            for (i, part) in cell.columns.iter().enumerate() {
                widths[i] = widths[i].max(part.iter().map(MathFragment::width).sum());
            }
        }
        let rows = col.into_iter().enumerate().map(|(r, cell)| RowLayout {
            cells: cell.columns,
            frame_height: cell.height,
            row_height: Some(heights[r]),
        });
        let stacked = stack_rows(rows, &widths, alternator, matrix, row_gap, 0.0);
        for (sub, sx, sy) in stacked.frames {
            frame.push_frame(x + sx, sy, sub);
        }
        x += stacked.width + col_gap;
    }
    frame.width = (x - col_gap).max(0.0);
    let axis = ctx.font.constant(Constant::AxisHeight) * size;
    frame.baseline = frame.height / 2.0 + axis;
    FrameFragment::new(frame, size, MathClass::Normal, style.size).into()
}
