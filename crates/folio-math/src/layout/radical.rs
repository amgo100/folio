// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/radical.rs,
// and typst-library v0.15.1, crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Roots: a radical sign stretched to its radicand, with a rule over the
//! radicand and an index before it.

use unicode_math_class::MathClass;

use super::font::Constant;
use super::fragment::{Axis, Frame, FrameFragment, MathFragment, Stretch};
use super::{Ctx, MathSize, Style};
use crate::Root;

/// Lays out a root (TeXbook, page 443, and appendix G, rule 11; MathML
/// Core, §3.3.3).
pub(crate) fn layout_root(ctx: &mut Ctx, root: &Root, style: Style, out: &mut Vec<MathFragment>) {
    let radicand = ctx
        .layout_row(&root.radicand, style.cramp(), MathClass::Normal)
        .into_frame();
    let index_style = Style {
        size: MathSize::ScriptScript,
        ..style.cramp()
    };
    let index = root.index.as_ref().map(|index| {
        ctx.layout_row(index, index_style, MathClass::Normal)
            .into_frame()
    });

    let c = |constant| ctx.constant(constant, style);
    let thickness = c(Constant::RadicalRuleThickness);
    let extra_ascender = c(Constant::RadicalExtraAscender);
    let kern_before = c(Constant::RadicalKernBeforeDegree);
    let kern_after = c(Constant::RadicalKernAfterDegree);
    let raise = ctx.font.constant(Constant::RadicalDegreeBottomRaisePercent);
    let gap = c(if style.size == MathSize::Display {
        Constant::RadicalDisplayStyleVerticalGap
    } else {
        Constant::RadicalVerticalGap
    });

    // The radical sign, as tall as the radicand with the gap and rule.
    let target = radicand.height + thickness + gap;
    let Some(sqrt) = ctx.stretched(
        "√",
        root.span,
        MathClass::Normal,
        style,
        Stretch {
            axis: Axis::Y,
            target,
            short_fall: 0.0,
        },
    ) else {
        out.push(
            FrameFragment::new(
                radicand,
                ctx.font_size(style),
                MathClass::Normal,
                style.size,
            )
            .into(),
        );
        return;
    };
    let sqrt = MathFragment::from(sqrt).into_frame();

    // Keep the gap, and share what space is left above and below.
    let gap = gap.max((sqrt.height - thickness - radicand.height + gap) / 2.0);
    let sqrt_ascent = radicand.ascent() + gap + thickness;
    let descent = sqrt.height - sqrt_ascent;
    let inner_ascent = sqrt_ascent + extra_ascender;

    let mut sqrt_offset = 0.0;
    let mut shift_up = 0.0;
    let mut ascent = inner_ascent;
    if let Some(index) = &index {
        sqrt_offset = kern_before + index.width + kern_after;
        // TeXbook, page 360, with the index's descent kept clear of the
        // sign.
        shift_up = raise * (inner_ascent - descent) + index.descent();
        ascent = ascent.max(shift_up + index.ascent());
    }

    let sqrt_x = sqrt_offset.max(0.0);
    let radicand_x = sqrt_x + sqrt.width;
    let radicand_y = ascent - radicand.ascent();
    let width = radicand_x + radicand.width;
    let mut frame = Frame::new(width, ascent + descent);
    frame.baseline = ascent;

    if let Some(index) = index {
        let index_x = -sqrt_offset.min(0.0) + kern_before;
        let index_y = ascent - index.ascent() - shift_up;
        frame.push_frame(index_x, index_y, index);
    }
    frame.push_frame(sqrt_x, radicand_y - gap - thickness, sqrt);
    frame.push_rule(
        radicand_x,
        radicand_y - gap - thickness,
        radicand.width,
        thickness,
        root.span,
    );
    frame.push_frame(radicand_x, radicand_y, radicand);
    out.push(FrameFragment::new(frame, ctx.font_size(style), MathClass::Normal, style.size).into());
}
