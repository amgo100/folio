// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/fenced.rs,
// and typst-library v0.15.1, crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Delimiters that grow with what they enclose.

use unicode_math_class::MathClass;

use super::font::Constant;
use super::fragment::{Axis, MathFragment, Stretch};
use super::{Ctx, DELIM_SHORT_FALL, Style};
use crate::Delimited;

/// Lays out a body between delimiters, which grow to enclose it as far
/// above and below the math axis as it reaches.
pub(crate) fn layout_delimited(
    ctx: &mut Ctx,
    delimited: &Delimited,
    style: Style,
    out: &mut Vec<MathFragment>,
) {
    let body = ctx.layout_fragments(delimited.body.nodes(), style, delimited.close.is_some());
    let target = body
        .iter()
        .map(|f| {
            let size = f.font_size().unwrap_or_else(|| ctx.font_size(style));
            let axis = ctx.font.constant(Constant::AxisHeight) * size;
            2.0 * (f.ascent() - axis).max(f.descent() + axis)
        })
        .fold(0.0, f64::max);
    push_delimiter(
        ctx,
        delimited.open,
        MathClass::Opening,
        target,
        style,
        delimited.span,
        out,
    );
    out.extend(body);
    push_delimiter(
        ctx,
        delimited.close,
        MathClass::Closing,
        target,
        style,
        delimited.span,
        out,
    );
}

/// Pushes a delimiter stretched to `target`, if there is one.
pub(crate) fn push_delimiter(
    ctx: &mut Ctx,
    delimiter: Option<char>,
    class: MathClass,
    target: f64,
    style: Style,
    span: folio_syntax::Span,
    out: &mut Vec<MathFragment>,
) {
    let Some(c) = delimiter else { return };
    let short_fall = DELIM_SHORT_FALL * ctx.font_size(style);
    let text = c.to_string();
    if let Some(glyph) = ctx.stretched(
        &text,
        span,
        class,
        style,
        Stretch {
            axis: Axis::Y,
            target,
            short_fall,
        },
    ) {
        out.push(glyph.into());
    }
}
