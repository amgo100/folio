// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/fraction.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Fractions: a numerator over a denominator, with a rule between them.

use unicode_math_class::MathClass;

use super::font::Constant;
use super::fragment::{Frame, FrameFragment, MathFragment};
use super::{Ctx, MathSize, Style};
use crate::Frac;

/// The space on each side of a fraction's rule, in ems.
const FRAC_PADDING: f64 = 0.1;

/// Lays out a fraction (TeXbook, appendix G, rule 15).
pub(crate) fn layout_frac(ctx: &mut Ctx, frac: &Frac, style: Style, out: &mut Vec<MathFragment>) {
    let num = ctx
        .layout_row(&frac.num, style.num(), MathClass::Normal)
        .into_frame();
    let den = ctx
        .layout_row(&frac.den, style.den(), MathClass::Normal)
        .into_frame();

    let display = style.size == MathSize::Display;
    let c = |constant| ctx.constant(constant, style);
    let axis = c(Constant::AxisHeight);
    let thickness = c(Constant::FractionRuleThickness);
    let shift_up = c(if display {
        Constant::FractionNumeratorDisplayStyleShiftUp
    } else {
        Constant::FractionNumeratorShiftUp
    });
    let shift_down = c(if display {
        Constant::FractionDenominatorDisplayStyleShiftDown
    } else {
        Constant::FractionDenominatorShiftDown
    });
    let num_min = c(if display {
        Constant::FractionNumDisplayStyleGapMin
    } else {
        Constant::FractionNumeratorGapMin
    });
    let den_min = c(if display {
        Constant::FractionDenomDisplayStyleGapMin
    } else {
        Constant::FractionDenominatorGapMin
    });
    let size = ctx.font_size(style);

    let num_gap = (shift_up - (axis + thickness / 2.0) - num.descent()).max(num_min);
    let den_gap = (shift_down + (axis - thickness / 2.0) - den.ascent()).max(den_min);

    let line_width = num.width.max(den.width);
    let width = line_width + 2.0 * FRAC_PADDING * size;
    let height = num.height + num_gap + thickness + den_gap + den.height;
    let line_y = num.height + num_gap;
    let num_x = (width - num.width) / 2.0;
    let den_x = (width - den.width) / 2.0;

    let mut frame = Frame::new(width, height);
    frame.baseline = line_y + thickness / 2.0 + axis;
    let den_height = den.height;
    frame.push_frame(num_x, 0.0, num);
    frame.push_frame(den_x, height - den_height, den);
    frame.push_rule(
        (width - line_width) / 2.0,
        line_y,
        line_width,
        thickness,
        frac.span,
    );
    out.push(FrameFragment::new(frame, size, MathClass::Normal, style.size).into());
}
