// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/scripts.rs,
// and typst-library v0.15.1, crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Scripts and limits: a base with a subscript, a superscript and primes
//! beside it, or limits below and above it.

use unicode_math_class::MathClass;

use super::font::{Constant, Corner};
use super::fragment::{Axis, Frame, FrameFragment, MathFragment, Stretch};
use super::{Ctx, MathSize, Style};
use crate::{Attach, Nary, Node, Row};

/// Lays out a base with attachments. Limits go below and above an
/// operator with limits in display style, and a relation; else scripts go
/// beside the base, primes first in the superscript.
pub(crate) fn layout_attach(
    ctx: &mut Ctx,
    attach: &Attach,
    style: Style,
    class: MathClass,
    out: &mut Vec<MathFragment>,
) {
    let limits = match attach.base.nodes() {
        [Node::Operator(op)] => op.limits && style.size == MathSize::Display,
        [Node::Symbol(atom)] => atom.class == MathClass::Relation,
        _ => false,
    };
    let base = ctx.layout_row(&attach.base, style, class);

    // Primes, then the superscript, as one superscript.
    let primes = primes(ctx, attach.primes, attach.span, style.sup());
    let sup = attach
        .sup
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sup()));
    let sub = attach
        .sub
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sub()));

    let (t, tr, b, br) = if limits {
        (sup, primes, sub, None)
    } else {
        let tr = match (primes, sup) {
            (Some(primes), Some(sup)) => {
                Some(ctx.group(vec![primes, sup], style.sup(), MathClass::Normal))
            }
            (primes, sup) => primes.or(sup),
        };
        (None, tr, None, sub)
    };
    // Pre-scripts are scripts, never limits: ²³⁵₉₂U.
    let tl = attach
        .pre_sup
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sup()));
    let bl = attach
        .pre_sub
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sub()));
    out.push(layout_scripted(
        ctx,
        style,
        class,
        base,
        [t, tr, b, br],
        [tl, bl],
    ));
}

/// Lays out an n-ary operator with its limits: below and above it in
/// display style, unless it is an integral, and beside it otherwise.
/// In display style it is as tall as the font's display operators.
pub(crate) fn layout_nary(ctx: &mut Ctx, nary: &Nary, style: Style, out: &mut Vec<MathFragment>) {
    let text = nary.op.to_string();
    let display = style.size == MathSize::Display;
    let glyph = if display {
        let target = ctx.constant(Constant::DisplayOperatorMinHeight, style);
        ctx.stretched(
            &text,
            nary.span,
            MathClass::Large,
            style,
            Stretch {
                axis: Axis::Y,
                target,
                short_fall: 0.0,
            },
        )
    } else {
        ctx.glyph(&text, nary.span, MathClass::Large, style)
    };
    let Some(mut glyph) = glyph else { return };
    // Large operators are centred on the axis (TeXbook, page 155).
    glyph.center_on_axis(&ctx.font);
    let base = MathFragment::from(glyph);
    if nary.sub.is_none() && nary.sup.is_none() {
        out.push(base);
        return;
    }
    let sup = nary
        .sup
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sup()));
    let sub = nary
        .sub
        .as_ref()
        .map(|row| layout_script(ctx, row, style.sub()));
    let limits = display && !is_integral(nary.op);
    let parts = if limits {
        [sup, None, sub, None]
    } else {
        [None, sup, None, sub]
    };
    out.push(layout_attachments(
        ctx,
        style,
        MathClass::Large,
        base,
        parts,
    ));
}

/// Whether a large operator is an integral, whose limits go beside it.
fn is_integral(c: char) -> bool {
    matches!(c, '\u{222B}'..='\u{2233}' | '\u{2A0B}'..='\u{2A1C}')
}

fn layout_script(ctx: &mut Ctx, row: &Row, style: Style) -> MathFragment {
    ctx.layout_row(row, style, MathClass::Normal)
}

/// The primes after a base, set in its superscript's style.
fn primes(
    ctx: &mut Ctx,
    count: u8,
    span: folio_syntax::Span,
    style: Style,
) -> Option<MathFragment> {
    let text = match count {
        0 => return None,
        1 => "′",
        2 => "″",
        3 => "‴",
        4 => "⁗",
        _ => {
            // More primes than one character holds: overlapping primes.
            let prime = ctx.glyph("′", span, MathClass::Normal, style)?;
            let prime = MathFragment::from(prime).into_frame();
            let width = prime.width * (f64::from(count) + 1.0) / 2.0;
            let mut frame = Frame::new(width, prime.height);
            frame.baseline = prime.baseline;
            for i in 0..count {
                frame.push_frame(prime.width * (f64::from(i) / 2.0), 0.0, prime.clone());
            }
            let mut fragment =
                FrameFragment::new(frame, ctx.font_size(style), MathClass::Normal, style.size);
            fragment.text_like = true;
            return Some(fragment.into());
        }
    };
    ctx.glyph(text, span, MathClass::Normal, style)
        .map(Into::into)
}

/// Lays out a base with its upper limit, superscript, lower limit and
/// subscript.
pub(crate) fn layout_attachments(
    ctx: &Ctx,
    style: Style,
    class: MathClass,
    base: MathFragment,
    parts: [Option<MathFragment>; 4],
) -> MathFragment {
    layout_scripted(ctx, style, class, base, parts, [None, None])
}

/// Lays out a base with its upper limit, superscript, lower limit and
/// subscript, and its pre-superscript and pre-subscript, which are
/// right-aligned against the base.
fn layout_scripted(
    ctx: &Ctx,
    style: Style,
    class: MathClass,
    base: MathFragment,
    [t, tr, b, br]: [Option<MathFragment>; 4],
    [tl, bl]: [Option<MathFragment>; 2],
) -> MathFragment {
    let size = base.font_size().unwrap_or_else(|| ctx.font_size(style));
    let c = |constant: Constant| ctx.font.constant(constant) * size;

    // From the base's baseline to the scripts' and the limits' baselines.
    let (tr_shift, br_shift) = if tr.is_none() && br.is_none() {
        (0.0, 0.0)
    } else {
        script_shifts(ctx, size, style.cramped, &base, tr.as_ref(), br.as_ref())
    };
    let (tl_shift, bl_shift) = if tl.is_none() && bl.is_none() {
        (0.0, 0.0)
    } else {
        script_shifts(ctx, size, style.cramped, &base, tl.as_ref(), bl.as_ref())
    };
    let t_shift = t.as_ref().map_or(0.0, |t| {
        base.ascent()
            + c(Constant::UpperLimitBaselineRiseMin)
                .max(c(Constant::UpperLimitGapMin) + t.descent())
    });
    let b_shift = b.as_ref().map_or(0.0, |b| {
        base.descent()
            + c(Constant::LowerLimitBaselineDropMin).max(c(Constant::LowerLimitGapMin) + b.ascent())
    });

    let ascent = base
        .ascent()
        .max(tr.as_ref().map_or(0.0, |tr| tr_shift + tr.ascent()))
        .max(t.as_ref().map_or(0.0, |t| t_shift + t.ascent()))
        .max(tl.as_ref().map_or(0.0, |tl| tl_shift + tl.ascent()));
    let descent = base
        .descent()
        .max(br.as_ref().map_or(0.0, |br| br_shift + br.descent()))
        .max(b.as_ref().map_or(0.0, |b| b_shift + b.descent()))
        .max(bl.as_ref().map_or(0.0, |bl| bl_shift + bl.descent()));

    // How far the limits reach beyond the base, shifted right (above) and
    // left (below) by half its italic correction.
    let delta = base.italics_correction() / 2.0;
    let (t_pre, t_post) = t.as_ref().map_or((0.0, 0.0), |t| {
        let half = (t.width() - base.width()) / 2.0;
        (half - delta, half + delta)
    });
    let (b_pre, b_post) = b.as_ref().map_or((0.0, 0.0), |b| {
        let half = (b.width() - base.width()) / 2.0;
        (half + delta, half - delta)
    });

    // How far the scripts reach beyond the base, with their kerns.
    let space_after = c(Constant::SpaceAfterScript);
    let (tr_post, tr_kern) = tr.as_ref().map_or((0.0, 0.0), |tr| {
        let kern = math_kern(ctx, &base, tr, tr_shift, Corner::TopRight);
        (space_after + tr.width() + kern, kern)
    });
    // The base's box holds its italic correction, so the subscript moves
    // left by it.
    let (br_post, br_kern) = br.as_ref().map_or((0.0, 0.0), |br| {
        let kern =
            math_kern(ctx, &base, br, br_shift, Corner::BottomRight) - base.italics_correction();
        (space_after + br.width() + kern, kern)
    });

    // How far the pre-scripts reach before the base, with their kerns.
    let (tl_pre, tl_kern) = tl.as_ref().map_or((0.0, 0.0), |tl| {
        let kern = math_kern(ctx, &base, tl, tl_shift, Corner::TopLeft);
        (space_after + tl.width() + kern, kern)
    });
    let (bl_pre, bl_kern) = bl.as_ref().map_or((0.0, 0.0), |bl| {
        let kern = math_kern(ctx, &base, bl, bl_shift, Corner::BottomLeft);
        (space_after + bl.width() + kern, kern)
    });

    let pre = t_pre.max(b_pre).max(tl_pre).max(bl_pre).max(0.0);
    let base_width = base.width();
    let post = t_post.max(b_post).max(tr_post).max(br_post);
    let width = pre + base_width + post;

    let mut frame = Frame::new(width, ascent + descent);
    frame.baseline = ascent;
    let base_ascent = base.ascent();
    frame.push_frame(pre, ascent - base_ascent, base.into_frame());
    if let Some(tr) = tr {
        let y = ascent - tr_shift - tr.ascent();
        frame.push_frame(pre + base_width + tr_kern, y, tr.into_frame());
    }
    if let Some(br) = br {
        let y = ascent + br_shift - br.ascent();
        frame.push_frame(pre + base_width + br_kern, y, br.into_frame());
    }
    // Right-aligned against the base, as the numbers of ²³⁵₉₂U are.
    if let Some(tl) = tl {
        let y = ascent - tl_shift - tl.ascent();
        let x = pre - tl.width() - tl_kern;
        frame.push_frame(x, y, tl.into_frame());
    }
    if let Some(bl) = bl {
        let y = ascent + bl_shift - bl.ascent();
        let x = pre - bl.width() - bl_kern;
        frame.push_frame(x, y, bl.into_frame());
    }
    if let Some(t) = t {
        let y = ascent - t_shift - t.ascent();
        frame.push_frame(pre - t_pre, y, t.into_frame());
    }
    if let Some(b) = b {
        let y = ascent + b_shift - b.ascent();
        frame.push_frame(pre - b_pre, y, b.into_frame());
    }
    FrameFragment::new(frame, ctx.font_size(style), class, style.size).into()
}

/// The distances from the base's baseline up to the superscript's and down
/// to the subscript's (TeXbook, appendix G, rules 18a–f).
fn script_shifts(
    ctx: &Ctx,
    size: f64,
    cramped: bool,
    base: &MathFragment,
    sup: Option<&MathFragment>,
    sub: Option<&MathFragment>,
) -> (f64, f64) {
    let c = |constant: Constant| ctx.font.constant(constant) * size;
    let sup_shift_up = c(if cramped {
        Constant::SuperscriptShiftUpCramped
    } else {
        Constant::SuperscriptShiftUp
    });
    let sup_bottom_min = c(Constant::SuperscriptBottomMin);
    let sup_bottom_max_with_sub = c(Constant::SuperscriptBottomMaxWithSubscript);
    let sup_drop_max = c(Constant::SuperscriptBaselineDropMax);
    let gap_min = c(Constant::SubSuperscriptGapMin);
    let sub_shift_down = c(Constant::SubscriptShiftDown);
    let sub_top_max = c(Constant::SubscriptTopMax);
    let sub_drop_min = c(Constant::SubscriptBaselineDropMin);

    let text_like = base.is_text_like();
    let mut shift_up = 0.0f64;
    let mut shift_down = 0.0f64;
    if let Some(sup) = sup {
        let drop = if text_like {
            0.0
        } else {
            base.base_ascent() - sup_drop_max
        };
        shift_up = shift_up
            .max(sup_shift_up)
            .max(drop)
            .max(sup_bottom_min + sup.descent());
    }
    if let Some(sub) = sub {
        let drop = if text_like {
            0.0
        } else {
            base.base_descent() + sub_drop_min
        };
        shift_down = shift_down
            .max(sub_shift_down)
            .max(drop)
            .max(sub.ascent() - sub_top_max);
    }
    if let (Some(sup), Some(sub)) = (sup, sub) {
        let sup_bottom = shift_up - sup.descent();
        let sub_top = sub.ascent() - shift_down;
        let gap = sup_bottom - sub_top;
        if gap < gap_min {
            let increase = gap_min - gap;
            let sup_only = (sup_bottom_max_with_sub - sup_bottom).clamp(0.0, increase);
            let rest = (increase - sup_only) / 2.0;
            shift_up += sup_only + rest;
            shift_down += rest;
        }
    }
    (shift_up, shift_down)
}

/// The kern of a script against its base at a corner: positive moves it
/// away from the base (OpenType MATH, MathKernInfo). Of the kerns at the
/// two correction heights, the smaller kern wins, so that the script does
/// not collide with the base.
fn math_kern(
    ctx: &Ctx,
    base: &MathFragment,
    script: &MathFragment,
    shift: f64,
    corner: Corner,
) -> f64 {
    let (top, bottom) = match corner {
        Corner::TopLeft | Corner::TopRight => (base.ascent() - shift, shift - script.descent()),
        Corner::BottomLeft | Corner::BottomRight => {
            (script.ascent() - shift, shift - base.descent())
        }
    };
    let summed = |height: f64| {
        base.kern_at_height(&ctx.font, corner, height)
            + script.kern_at_height(&ctx.font, corner.inv(), height)
    };
    summed(top).max(summed(bottom))
}
