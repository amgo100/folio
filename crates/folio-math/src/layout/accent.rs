// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/accent.rs and
// crates/typst-layout/src/math/line.rs, and typst-library v0.15.1,
// crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Accents over and under a base, lines over and under it, and braces and
//! brackets over and under it with their annotations.

use folio_syntax::Span;
use unicode_math_class::MathClass;

use super::font::Constant;
use super::fragment::{Axis, Frame, FrameFragment, GlyphFragment, MathFragment, Stretch};
use super::{ACCENT_SHORT_FALL, Ctx, Style, scripts};
use crate::{Decoration, DecorationKind, Row};

/// Whether a combining character is an accent set below its base.
fn is_bottom(c: char) -> bool {
    matches!(
        c,
        '\u{316}'..='\u{319}'
            | '\u{31C}'..='\u{333}'
            | '\u{339}'..='\u{33C}'
            | '\u{345}'
            | '\u{347}'..='\u{349}'
            | '\u{34D}'
            | '\u{34E}'
            | '\u{353}'..='\u{356}'
            | '\u{359}'
            | '\u{35A}'
            | '\u{20E8}'
            | '\u{20EC}'..='\u{20EF}'
    )
}

/// Lays out an accent over or under a base, stretched to the base's width
/// where the font can stretch it.
pub(crate) fn layout_accent(
    ctx: &mut Ctx,
    base: &Row,
    accent: char,
    span: Span,
    style: Style,
    out: &mut Vec<MathFragment>,
) {
    let top = !is_bottom(accent);
    // The base of an accent above is cramped, and its `i` and `j` lose
    // their dots.
    let base_style = if top {
        Style {
            dotless: true,
            ..style.cramp()
        }
    } else {
        style
    };
    let base = ctx.layout_row(base, base_style, MathClass::Normal);
    let size = base.font_size().unwrap_or_else(|| ctx.font_size(style));
    // A tall base takes the flattened form of the accent.
    let flat = ctx.font.constant(Constant::FlattenedAccentBaseHeight) * size;
    let mut features = style.features();
    features.flac = top && base.ascent() > flat;
    features.dtls = false;
    let stretch = Stretch {
        axis: Axis::X,
        target: base.width(),
        short_fall: ACCENT_SHORT_FALL * size,
    };
    let text = accent.to_string();
    let Some(glyph) = GlyphFragment::stretched(
        &ctx.font,
        &text,
        span,
        MathClass::Diacritic,
        style.size,
        size,
        features,
        stretch,
    ) else {
        out.push(base);
        return;
    };
    out.push(place_accent(ctx, base, glyph.into(), top, false, style));
}

/// Places an accent over or under a base. An exact frame is as wide as
/// both, as for braces; otherwise it is as wide as the base.
fn place_accent(
    ctx: &Ctx,
    base: MathFragment,
    accent: MathFragment,
    top: bool,
    exact: bool,
    style: Style,
) -> MathFragment {
    let size = base.font_size().unwrap_or_else(|| ctx.font_size(style));
    let base_attach = base.accent_attach();
    let accent_attach = accent.accent_attach().0;
    let accent = accent.into_frame();
    let attach = if top { base_attach.0 } else { base_attach.1 };
    let (width, base_x, accent_x) = if !exact {
        (base.width(), 0.0, attach - accent_attach)
    } else {
        let pre = accent_attach - attach;
        let post = (accent.width - accent_attach) - (base.width() - attach);
        let width = pre.max(0.0) + base.width() + post.max(0.0);
        if pre < 0.0 {
            (width, 0.0, -pre)
        } else {
            (width, pre, 0.0)
        }
    };

    let (gap, accent_y, base_y) = if top {
        // An accent's ink is above the baseline, so its descent is
        // negative; the gap brings it down to the base, unless the base is
        // so small that it would come too low.
        let accent_base = ctx.font.constant(Constant::AccentBaseHeight) * size;
        let gap = -accent.descent() - base.ascent().min(accent_base);
        (gap, 0.0, accent.height + gap)
    } else {
        let gap = -accent.ascent();
        (gap, base.height() + gap, 0.0)
    };

    let mut frame = Frame::new(width, accent.height + gap + base.height());
    frame.baseline = base_y + base.ascent();
    let text_like = !exact && base.is_text_like();
    let italics = base.italics_correction();
    let base_ascent = base.base_ascent();
    let base_descent = base.base_descent();
    let class = base.class();
    frame.push_frame(base_x, base_y, base.into_frame());
    frame.push_frame(accent_x, accent_y, accent);
    let mut fragment = FrameFragment::new(frame, size, class, style.size);
    fragment.base_ascent = base_ascent;
    fragment.base_descent = base_descent;
    fragment.italics = italics;
    fragment.text_like = text_like;
    fragment.accent_attach = base_attach;
    fragment.into()
}

/// Lays out a line, brace or bracket over or under a body, with the
/// annotation of a brace or bracket beyond it.
pub(crate) fn layout_decoration(
    ctx: &mut Ctx,
    decoration: &Decoration,
    style: Style,
    out: &mut Vec<MathFragment>,
) {
    let c = match decoration.kind {
        DecorationKind::Overline | DecorationKind::Underline => {
            return layout_line(ctx, decoration, style, out);
        }
        DecorationKind::Overbrace => '⏞',
        DecorationKind::Underbrace => '⏟',
        DecorationKind::Overbracket => '⎴',
        DecorationKind::Underbracket => '⎵',
    };
    let over = decoration.kind.is_over();
    let base = ctx.layout_row(&decoration.body, style, MathClass::Normal);
    let stretch = Stretch {
        axis: Axis::X,
        target: base.width(),
        short_fall: 0.0,
    };
    let glyph = ctx.stretched(
        &c.to_string(),
        decoration.span,
        MathClass::Diacritic,
        style,
        stretch,
    );
    let base = match glyph {
        Some(glyph) => place_accent(ctx, base, glyph.into(), over, true, style),
        None => base,
    };
    let Some(annotation) = &decoration.annotation else {
        out.push(base);
        return;
    };
    let parts = if over {
        let t = ctx.layout_row(annotation, style.sup(), MathClass::Normal);
        [Some(t), None, None, None]
    } else {
        let b = ctx.layout_row(annotation, style.sub(), MathClass::Normal);
        [None, None, Some(b), None]
    };
    out.push(scripts::layout_attachments(
        ctx,
        style,
        MathClass::Normal,
        base,
        parts,
    ));
}

/// Lays out a line over or under a body.
fn layout_line(ctx: &mut Ctx, decoration: &Decoration, style: Style, out: &mut Vec<MathFragment>) {
    let below = decoration.kind == DecorationKind::Underline;
    // The body under a line is cramped.
    let body_style = if below { style } else { style.cramp() };
    let content = ctx.layout_row(&decoration.body, body_style, MathClass::Normal);
    let size = content.font_size().unwrap_or_else(|| ctx.font_size(style));
    let c = |constant: Constant| ctx.font.constant(constant) * size;
    let (extra, line_y, content_y, baseline, thickness, adjust);
    if below {
        let sep = c(Constant::UnderbarExtraDescender);
        thickness = c(Constant::UnderbarRuleThickness);
        let gap = c(Constant::UnderbarVerticalGap);
        extra = sep + thickness + gap;
        line_y = content.height() + gap;
        content_y = 0.0;
        baseline = content.ascent();
        adjust = -content.italics_correction();
    } else {
        let sep = c(Constant::OverbarExtraAscender);
        thickness = c(Constant::OverbarRuleThickness);
        let gap = c(Constant::OverbarVerticalGap);
        extra = sep + thickness + gap;
        line_y = sep;
        content_y = extra;
        baseline = content.ascent() + extra;
        adjust = 0.0;
    }
    let width = content.width();
    let mut frame = Frame::new(width, content.height() + extra);
    frame.baseline = baseline;
    let text_like = content.is_text_like();
    let italics = content.italics_correction();
    frame.push_frame(0.0, content_y, content.into_frame());
    frame.push_rule(0.0, line_y, width + adjust, thickness, decoration.span);
    let mut fragment = FrameFragment::new(frame, size, MathClass::Normal, style.size);
    fragment.italics = italics;
    fragment.text_like = text_like;
    out.push(fragment.into());
}
