//! Page masters (SPEC §7.1) and the block properties of the box model:
//! what the page builder reads of the `page` style and of a block's style.

use std::sync::Arc;

use folio_component::{Abs, Color, Dash, LineCap, Paint, Point, Rect, Size, Stroke, Value};
use folio_syntax::Span;

use crate::style::{ComputedStyle, Properties};

/// A page master: the size of its pages and its one region, the body
/// (SPEC §7.1). Header, footer and margin regions, named masters and
/// masters chosen by rules follow with the full flow state.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct PageMaster {
    /// The size of a page.
    pub size: Size,
    /// The body region, relative to the page's top-left corner: the page
    /// without its margins, at least a point in each direction.
    pub body: Rect,
}

/// The smallest width or height of a page: PDF's limit (SPEC §11.1).
pub(crate) const MIN_PAGE_SIDE: Abs = Abs::pt(3.0);

/// The largest width or height of a page: PDF's limit of 14,400 units of
/// 1pt, 200 inches (SPEC §11.1).
pub(crate) const MAX_PAGE_SIDE: Abs = Abs::pt(14_400.0);

impl PageMaster {
    /// The master of a `page { .. }` style: `page-size` (a paper size or
    /// a width and a height), and the margins. The inner margin is on the
    /// left of every page: documents are one-sided until masters mirror
    /// (§7.1). A side outside PDF's limits is clamped to them
    /// ([`PageMaster::checked`]).
    pub fn of(page: &ComputedStyle, props: &Properties) -> Self {
        Self::checked(page, props).0
    }

    /// The master of a `page { .. }` style, as [`PageMaster::of`] makes
    /// it, and the size `page-size` asked for when a side of it was outside
    /// PDF's limits of 3pt and 14,400pt, and so was clamped.
    pub fn checked(page: &ComputedStyle, props: &Properties) -> (Self, Option<Size>) {
        let a4 = Size::new(Abs::mm(210.0), Abs::mm(297.0));
        let size = match page.value(props, props.builtin("page-size")) {
            Some(Value::Keyword(k)) => match k.as_str() {
                "A3" => Size::new(Abs::mm(297.0), Abs::mm(420.0)),
                "A5" => Size::new(Abs::mm(148.0), Abs::mm(210.0)),
                "letter" => Size::new(Abs::inches(8.5), Abs::inches(11.0)),
                "legal" => Size::new(Abs::inches(8.5), Abs::inches(14.0)),
                _ => a4,
            },
            Some(Value::Array(items)) => {
                let width = match items.first() {
                    Some(Value::Length(width)) => *width,
                    _ => a4.width,
                };
                let height = match items.get(1) {
                    Some(Value::Length(height)) => *height,
                    _ => a4.height,
                };
                Size::new(width, height)
            }
            _ => a4,
        };
        let clamp = |side: Abs| side.max(MIN_PAGE_SIDE).min(MAX_PAGE_SIDE);
        let requested = size;
        let size = Size::new(clamp(size.width), clamp(size.height));
        let clamped = (size != requested).then_some(requested);
        // A margin as wide as the page leaves the body a point inside it,
        // where content is still drawn on the page.
        let margin = |name: &str, side: Abs| {
            length(page, props, name)
                .max(Abs::zero())
                .min(side - Abs::pt(1.0))
        };
        let (top, bottom) = (
            margin("margin-top", size.height),
            margin("margin-bottom", size.height),
        );
        let (inner, outer) = (
            margin("margin-inner", size.width),
            margin("margin-outer", size.width),
        );
        let body = Size::new(
            (size.width - inner - outer).max(Abs::pt(1.0)),
            (size.height - top - bottom).max(Abs::pt(1.0)),
        );
        let master = Self {
            size,
            body: Rect::new(Point::new(inner, top), body),
        };
        (master, clamped)
    }
}

/// Four values, one per side of a box.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Sides<T> {
    /// The top side.
    pub top: T,
    /// The right side.
    pub right: T,
    /// The bottom side.
    pub bottom: T,
    /// The left side.
    pub left: T,
}

/// A block's box as the page builder reads it from the block's style
/// (SPEC §5.2), placed in the body: where its border box sits
/// horizontally, its spaces, padding, borders and background, and its
/// keep and break constraints.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct BlockBox {
    /// The element the block is, or whose part it is.
    pub span: Span,
    /// The left edge of its border box, from the left of the body.
    pub x: Abs,
    /// The width of its border box.
    pub width: Abs,
    /// The space above it, which collapses with the spaces it meets and is
    /// dropped at a page break.
    pub space_before: Abs,
    /// The space below it.
    pub space_after: Abs,
    /// The padding inside its borders.
    pub padding: Sides<Abs>,
    /// Its borders: `border-top` and the other sides, where they are `auto`
    /// the value of `border`. Each takes up its thickness inside the box.
    pub border: Sides<Option<Stroke>>,
    /// The paint of its border box, if any.
    pub background: Option<Paint>,
    /// Whether a page break directly after it is avoided.
    pub keep_with_next: bool,
    /// Whether a page break inside it is avoided.
    pub keep_together: bool,
    /// Whether it may be broken across pages at all: `false` avoids a
    /// break inside it more strongly than `keep-together`.
    pub breakable: bool,
    /// Whether it starts a new page, and on which side.
    pub break_before: BreakBefore,
}

/// Whether a block starts a new page (`break-before`).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum BreakBefore {
    /// Where it falls: `auto`, and `column` in a body of one column.
    Auto,
    /// On a new page.
    Page,
    /// On a new odd page, after a blank even one if need be.
    OddPage,
    /// On a new even page, after a blank odd one if need be.
    EvenPage,
}

impl BlockBox {
    /// The box of a block of a style, whose border box starts `x` from the
    /// left of the body and is `width` wide.
    pub fn of(style: &ComputedStyle, props: &Properties, span: Span, x: Abs, width: Abs) -> Self {
        let value = |name: &str| style.value(props, props.builtin(name));
        let flag = |name: &str| matches!(value(name), Some(Value::Bool(true)));
        let all = value("border").and_then(stroke);
        // A side's `auto` takes `border`'s value, and its `none` removes it.
        let side = |name: &str| match value(name) {
            Some(Value::Auto) | None => all.clone(),
            Some(v) => stroke(v),
        };
        Self {
            span,
            x,
            width,
            space_before: length(style, props, "space-before").max(Abs::zero()),
            space_after: length(style, props, "space-after").max(Abs::zero()),
            padding: padding(value("padding")),
            border: Sides {
                top: side("border-top"),
                right: side("border-right"),
                bottom: side("border-bottom"),
                left: side("border-left"),
            },
            background: match value("background") {
                Some(Value::Color(color)) => Some(Paint::Solid(*color)),
                _ => None,
            },
            keep_with_next: flag("keep-with-next"),
            keep_together: flag("keep-together"),
            breakable: !matches!(value("breakable"), Some(Value::Bool(false))),
            break_before: match value("break-before") {
                Some(Value::Keyword(k)) => match k.as_str() {
                    "page" => BreakBefore::Page,
                    "odd-page" => BreakBefore::OddPage,
                    "even-page" => BreakBefore::EvenPage,
                    // A column break needs columns, which a column set
                    // makes (§13.2).
                    _ => BreakBefore::Auto,
                },
                _ => BreakBefore::Auto,
            },
        }
    }

    /// How far its content is inset from its border box: its border and
    /// padding on each side.
    pub fn inset(&self) -> Sides<Abs> {
        let thick = |s: &Option<Stroke>| s.as_ref().map_or(Abs::zero(), |s| s.thickness);
        Sides {
            top: self.padding.top + thick(&self.border.top),
            right: self.padding.right + thick(&self.border.right),
            bottom: self.padding.bottom + thick(&self.border.bottom),
            left: self.padding.left + thick(&self.border.left),
        }
    }

    /// The box of a block inside this one: its border box spans this
    /// one's content width.
    pub fn child(&self, style: &ComputedStyle, props: &Properties, span: Span) -> Arc<Self> {
        let inset = self.inset();
        let width = (self.width - inset.left - inset.right).max(Abs::pt(1.0));
        Arc::new(Self::of(style, props, span, self.x + inset.left, width))
    }

    /// Where its content starts, from the left of the body, and how wide
    /// it is: at least a point.
    pub fn content(&self) -> (Abs, Abs) {
        let inset = self.inset();
        (
            self.x + inset.left,
            (self.width - inset.left - inset.right).max(Abs::pt(1.0)),
        )
    }
}

/// The value of a length property, or zero.
pub(super) fn length(style: &ComputedStyle, props: &Properties, name: &str) -> Abs {
    match style.value(props, props.builtin(name)) {
        Some(Value::Length(abs)) => *abs,
        _ => Abs::zero(),
    }
}

/// `padding`: one length for every side, or two to four in CSS order
/// (vertical and horizontal; top, horizontal and bottom; top, right,
/// bottom and left). Negative lengths count as zero.
fn padding(value: Option<&Value>) -> Sides<Abs> {
    let lengths: Vec<Abs> = match value {
        Some(Value::Length(l)) => vec![*l],
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::Length(l) => *l,
                _ => Abs::zero(),
            })
            .collect(),
        _ => Vec::new(),
    };
    let l: Vec<Abs> = lengths.into_iter().map(|l| l.max(Abs::zero())).collect();
    let (top, right, bottom, left) = match l.as_slice() {
        [] => Default::default(),
        [a] => (*a, *a, *a, *a),
        [v, h] => (*v, *h, *v, *h),
        [t, h, b] => (*t, *h, *b, *h),
        [t, r, b, left, ..] => (*t, *r, *b, *left),
    };
    Sides {
        top,
        right,
        bottom,
        left,
    }
}

/// A border from its value: a thickness, a style keyword (`solid`,
/// `dashed`, `dotted`, `none`) and a colour, in any order and each
/// optional (`1.5pt solid $accent`). A thickness alone is solid black; a
/// colour or style alone is a point thick. `None` for no border.
fn stroke(value: &Value) -> Option<Stroke> {
    let items: &[Value] = match value {
        Value::Array(items) => items,
        Value::None | Value::Auto => return None,
        single => std::slice::from_ref(single),
    };
    let mut thickness = None;
    let mut color = None;
    let mut style = "solid";
    for item in items {
        match item {
            Value::Length(l) => thickness = Some(*l),
            Value::Color(c) => color = Some(*c),
            Value::Keyword(k) => match k.as_str() {
                "none" | "hidden" => return None,
                "dashed" => style = "dashed",
                "dotted" => style = "dotted",
                _ => style = "solid",
            },
            _ => {}
        }
    }
    let thickness = thickness.unwrap_or(Abs::pt(1.0));
    if thickness <= Abs::zero() {
        return None;
    }
    let mut stroke = Stroke::solid(color.unwrap_or(Color::BLACK), thickness);
    match style {
        "dashed" => {
            stroke.dash = Some(Dash {
                array: [thickness * 3.0, thickness * 2.0].into_iter().collect(),
                phase: Abs::zero(),
            });
        }
        "dotted" => {
            stroke.cap = LineCap::Round;
            stroke.dash = Some(Dash {
                array: [Abs::zero(), thickness * 2.0].into_iter().collect(),
                phase: Abs::zero(),
            });
        }
        _ => {}
    }
    Some(stroke)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecow::EcoVec;
    use folio_component::Name;

    #[test]
    fn padding_in_css_order() {
        let pt = |v: f64| Value::Length(Abs::pt(v));
        let arr = |v: &[f64]| Value::Array(v.iter().map(|&x| pt(x)).collect::<EcoVec<_>>());
        let p = padding(Some(&pt(2.0)));
        assert_eq!((p.top, p.left), (Abs::pt(2.0), Abs::pt(2.0)));
        let p = padding(Some(&arr(&[1.0, 2.0])));
        assert_eq!(
            (p.top, p.right, p.bottom, p.left),
            (Abs::pt(1.0), Abs::pt(2.0), Abs::pt(1.0), Abs::pt(2.0))
        );
        let p = padding(Some(&arr(&[1.0, 2.0, 3.0])));
        assert_eq!((p.bottom, p.left), (Abs::pt(3.0), Abs::pt(2.0)));
        let p = padding(Some(&arr(&[1.0, 2.0, 3.0, 4.0])));
        assert_eq!(p.left, Abs::pt(4.0));
        assert_eq!(padding(None), Sides::default());
    }

    #[test]
    fn borders_from_values() {
        let kw = |w: &str| Value::Keyword(Name::new(w).unwrap());
        let red = Color::rgb(255, 0, 0);
        let s = stroke(&Value::Array(EcoVec::from([
            Value::Length(Abs::pt(1.5)),
            kw("dashed"),
            Value::Color(red),
        ])))
        .unwrap();
        assert_eq!(s.thickness, Abs::pt(1.5));
        assert_eq!(s.paint, Paint::Solid(red));
        assert!(s.dash.is_some());
        let s = stroke(&Value::Length(Abs::pt(2.0))).unwrap();
        assert_eq!(s.paint, Paint::Solid(Color::BLACK));
        assert!(s.dash.is_none());
        assert_eq!(stroke(&kw("none")), None);
        assert_eq!(stroke(&Value::None), None);
        assert_eq!(stroke(&Value::Length(Abs::zero())), None);
    }
}
