//! Maths in layout (SPEC §8.2): every formula of a build set by
//! `folio-math`, for the paragraph builder, which sizes the chunks of
//! inline formulas as objects of their lines, and for the page builder,
//! which places displays as the content of their blocks, with the numbers
//! of their lines.
//!
//! Each formula is laid out once per world for its tree and what it is set
//! with (font, size, paint, leading): an edit lays out only the formulas
//! it changed ([`super::Cache`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Abs, Color, FontId, FragmentItem, Paint, Point, Size, Value};
use folio_math::layout::{
    DisplayMath, InlineMath, MathFont, MathOptions, VectorStyle, layout_display, layout_inline,
    layout_text,
};
use folio_syntax::Span;

use super::cache::CachedMath;
use super::paragraph::{ObjectBreak, ObjectMetrics};
use super::{Cache, length};
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::fonts::{Fonts, fonts};
use crate::pipeline::resolution;
use crate::resolve::{Resolution, Target};
use crate::span_map::source_ref;
use crate::style::{ComputedStyle, Properties};
use crate::text::{MathSettings, TextMath, TextObject, collect_text};

/// The family formulas are set in, until a stylesheet property chooses it
/// (SPEC §8.2). It is bundled, so every build has it.
pub const MATH_FONT: &str = "New Computer Modern Math";

/// How far apart an equation and its number are at least, in ems.
const NUMBER_GUTTER: f64 = 0.5;

/// The formulas of a build laid out, by their index in
/// [`Text::maths`](crate::text::Text::maths).
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Maths {
    /// Each formula, or `None` where no math font could set it.
    pub formulas: EcoVec<Option<LaidMath>>,
}

/// A formula laid out.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum LaidMath {
    /// An inline formula, in chunks.
    Inline(Arc<InlineMath>),
    /// A display, with its numbers.
    Display(Arc<Display>),
}

/// A display and the numbers of its lines.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Display {
    /// The formula.
    pub math: Arc<DisplayMath>,
    /// The number of each line, where it has one.
    pub lines: Vec<Option<Number>>,
    /// The number of the whole equation, where it has one: beside its only
    /// line, or centred beside its lines.
    pub whole: Option<Number>,
}

/// An equation's number, set.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Number {
    /// How wide it is.
    pub width: Abs,
    /// How far it reaches above its baseline.
    pub ascent: Abs,
    /// How far it reaches below it.
    pub descent: Abs,
    /// What it draws, from the start of its baseline.
    pub items: EcoVec<(Point, FragmentItem)>,
}

impl Maths {
    /// The size of an object of a paragraph that is a chunk of an inline
    /// formula.
    pub fn object(
        &self,
        object: &TextObject,
    ) -> Option<(ObjectMetrics, &EcoVec<(Point, FragmentItem)>)> {
        let chunk = object.math?;
        let Some(Some(LaidMath::Inline(math))) = self.formulas.get(chunk.formula) else {
            return None;
        };
        let laid = math.chunks.get(chunk.chunk)?;
        let metrics = ObjectMetrics {
            width: laid.width,
            ascent: laid.ascent,
            descent: laid.descent,
            shift: Abs::zero(),
            binds: true,
            after: laid.after.map(|after| ObjectBreak {
                penalty: after.penalty,
                space: after.space,
            }),
        };
        Some((metrics, &laid.items))
    }

    /// The sizes of a paragraph's objects as the paragraph builder reads
    /// them, or none when no object of it is a formula.
    pub fn metrics(&self, objects: &[TextObject]) -> EcoVec<ObjectMetrics> {
        if !objects.iter().any(|o| o.math.is_some()) {
            return EcoVec::new();
        }
        objects
            .iter()
            .map(|o| self.object(o).map(|(m, _)| m).unwrap_or_default())
            .collect()
    }

    /// The display at an index.
    pub fn display(&self, index: usize) -> Option<&Arc<Display>> {
        match self.formulas.get(index)? {
            Some(LaidMath::Display(display)) => Some(display),
            _ => None,
        }
    }
}

/// The formulas of a build laid out, or `None` when the document could not
/// be styled. Reports characters the math font has no glyph for. The
/// [`lines`](super::lines) query calls it, and each formula is memoised in
/// the layout cache.
pub(super) fn maths(db: &dyn Db, build: Build<'_>) -> Option<Maths> {
    let collected = collect_text(db, build).as_ref()?;
    let resolution = resolution(db, build)?;
    let fonts = fonts(db, build.project(db));
    if collected.maths.is_empty() {
        return Some(Maths::default());
    }
    // The faces of the `math-font` chains, each chosen and checked once.
    let mut chosen: HashMap<EcoVec<EcoString>, Option<(MathFont, EcoString)>> = HashMap::new();
    let mut reported: HashSet<(Span, EcoString)> = HashSet::new();
    let formulas = collected
        .maths
        .iter()
        .map(|m| {
            db.unwind_if_revision_cancelled();
            let (font, name) = chosen
                .entry(m.settings.families.clone())
                .or_insert_with(|| choose_font(db, fonts, &m.settings.families, m.span))
                .clone()?;
            let options = MathOptions {
                font,
                size: m.settings.size,
                fill: m.settings.fill.clone(),
                leading: m.settings.leading,
                vector: m.settings.vector,
                number: m.settings.number,
            };
            let (laid, unset) = lay_out(db.layout_cache(), m, &options, resolution);
            for text in unset {
                diag::report(
                    db,
                    Diagnostic::warning(
                        m.span,
                        eco_format!(
                            "the math font {name} cannot set the equation number `{text}`, \
                             so it is left out"
                        ),
                    ),
                );
            }
            let missing = match &laid {
                LaidMath::Inline(math) => &math.missing,
                LaidMath::Display(display) => &display.math.missing,
            };
            for glyph in missing {
                if reported.insert((glyph.span, glyph.text.clone())) {
                    diag::report(
                        db,
                        Diagnostic::warning(
                            if glyph.span.is_detached() { m.span } else { glyph.span },
                            eco_format!(
                                "the math font {name} has no glyph for `{}`, so it is left out",
                                glyph.text
                            ),
                        ),
                    );
                }
            }
            Some(laid)
        })
        .collect();
    Some(Maths { formulas })
}

/// The face of a `math-font` chain (SPEC §8.2): the first family the
/// build has, or New Computer Modern Math, which is bundled, when it has
/// none; with its family's name. Warns when no family of the chain is
/// available, and when the face has no MATH table. `None`, with a warning,
/// when not even the fallback can be read: the formulas are left out.
fn choose_font(
    db: &dyn Db,
    fonts: &Fonts,
    families: &[EcoString],
    span: Span,
) -> Option<(MathFont, EcoString)> {
    let variant = folio_fonts::FontVariant::default();
    let found = chosen_family(fonts, families);
    let (face, name) = match found {
        Some(found) => found,
        None => {
            if !families.is_empty() {
                let names: Vec<String> = families.iter().map(|f| format!("`{f}`")).collect();
                diag::report(
                    db,
                    Diagnostic::warning(
                        span,
                        eco_format!(
                            "no family of `math-font` ({}) is available, so maths is set in {MATH_FONT}",
                            names.join(", ")
                        ),
                    )
                    .with_hint("lock the font in `folio.lock`, or put it in the project's `fonts` directory"),
                );
            }
            match fonts.select(MATH_FONT, variant) {
                Some(face) => (face, EcoString::from(MATH_FONT)),
                None => {
                    report_unreadable(db, span);
                    return None;
                }
            }
        }
    };
    let Some(font) = math_font(db, fonts, face) else {
        report_unreadable(db, span);
        return None;
    };
    if !font.has_math() {
        diag::report(
            db,
            Diagnostic::warning(
                span,
                eco_format!("{name} is not a math font: it has no MATH table, so maths set in it may look poor"),
            )
            .with_hint(eco_format!("set `math-font` to a font made for maths, such as \"{MATH_FONT}\"")),
        );
    }
    Some((font, name))
}

/// The face of the first family of a `math-font` chain that the build has,
/// with the family, as an index into [`Fonts::faces`].
pub(crate) fn chosen_family(fonts: &Fonts, families: &[EcoString]) -> Option<(usize, EcoString)> {
    let variant = folio_fonts::FontVariant::default();
    families
        .iter()
        .find_map(|family| Some((fonts.select(family, variant)?, family.clone())))
}

/// The face formulas set with a `math-font` chain are set in: that of its
/// first available family, else New Computer Modern Math.
pub(crate) fn math_face(fonts: &Fonts, families: &[EcoString]) -> Option<usize> {
    chosen_family(fonts, families)
        .map(|(face, _)| face)
        .or_else(|| fonts.select(MATH_FONT, folio_fonts::FontVariant::default()))
}

fn report_unreadable(db: &dyn Db, span: Span) {
    diag::report(
        db,
        Diagnostic::warning(
            span,
            eco_format!("the math font cannot be read, so maths is left out"),
        ),
    );
}

/// The math font of a face of a build, read once per world.
fn math_font(db: &dyn Db, fonts: &Fonts, face: usize) -> Option<MathFont> {
    let font = fonts.faces()[face].font()?;
    let text = db.text_cache().font(font.data(), font.index())?;
    db.layout_cache().math_font(face, &font, text)
}

/// What a formula is set with, from the computed style of its element.
/// `vector` is the style of its part `vector`, where its formula has
/// vectors, and `lang` the language of its text.
pub(crate) fn math_settings(
    style: &ComputedStyle,
    vector: Option<&ComputedStyle>,
    lang: &folio_text::Lang,
    props: &Properties,
) -> MathSettings {
    fn value<'a>(style: &'a ComputedStyle, props: &'a Properties, name: &str) -> Option<&'a Value> {
        style.value(props, props.builtin(name))
    }
    let value = |style: &'_ ComputedStyle, name: &str| value(style, props, name).cloned();
    let fill = match value(style, "color") {
        Some(Value::Color(color)) => Paint::Solid(color),
        _ => Paint::Solid(Color::BLACK),
    };
    let families = match value(style, "math-font") {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        Some(Value::Str(s)) => EcoVec::from([s]),
        _ => EcoVec::new(),
    };
    let vector = match vector.and_then(|v| value(v, "math-style")) {
        Some(Value::Keyword(k)) if k.as_str() == "bold-italic" => VectorStyle::BoldItalic,
        Some(Value::Keyword(k)) if k.as_str() == "arrow" => VectorStyle::Arrow,
        _ => VectorStyle::BoldUpright,
    };
    MathSettings {
        size: length(style, props, "font-size"),
        fill,
        leading: length(style, props, "leading"),
        families,
        vector,
        number: crate::locale::number_format(lang, value(style, "decimal-marker").as_ref()),
    }
}

/// Lays out a formula, through the cache, with the numbers of a display,
/// and the numbers the font could not set.
fn lay_out(
    cache: &Cache,
    m: &TextMath,
    options: &MathOptions,
    resolution: &Resolution,
) -> (LaidMath, Vec<EcoString>) {
    if !m.display {
        let math = cache.math(&m.formula, options, false, || {
            CachedMath::Inline(Arc::new(layout_inline(&m.formula, options, &source_ref)))
        });
        let laid = match math {
            CachedMath::Inline(math) => LaidMath::Inline(math),
            CachedMath::Display(_) => unreachable!("an inline formula"),
        };
        return (laid, Vec::new());
    }
    let math = match cache.math(&m.formula, options, true, || {
        CachedMath::Display(Arc::new(layout_display(&m.formula, options, &source_ref)))
    }) {
        CachedMath::Display(math) => math,
        CachedMath::Inline(_) => unreachable!("a display"),
    };
    let unset = std::cell::RefCell::new(Vec::new());
    let number = |target: Target| {
        let text = resolution.number(target)?.number.as_ref()?;
        let set = set_number(cache, text, options, m.span);
        if set.is_none() {
            unset.borrow_mut().push(text.clone());
        }
        set
    };
    let lines: Vec<Option<Number>> = (0..m.formula.lines().len())
        .map(|i| {
            number(Target {
                location: m.location,
                line: Some(u32::try_from(i).ok()?),
            })
        })
        .collect();
    let whole = lines
        .iter()
        .all(Option::is_none)
        .then(|| number(Target::element(m.location)))
        .flatten();
    let laid = LaidMath::Display(Arc::new(Display { math, lines, whole }));
    (laid, unset.into_inner())
}

/// An equation's number, set in the math font.
/// An equation's number, set in the math font, once per world for its
/// text, options and source.
fn set_number(
    cache: &Cache,
    text: &EcoString,
    options: &MathOptions,
    span: Span,
) -> Option<Number> {
    let set = cache.number(text, options, span, || {
        layout_text(text, options, source_ref(span))
    })?;
    Some(Number {
        width: set.width,
        ascent: set.ascent,
        descent: set.descent,
        items: set.items,
    })
}

/// The content of the block of a display, `width` wide: the formula
/// centred, and its numbers at the end of the lines they number, or below
/// it, one after another, where they do not fit beside it. Its items are
/// positioned from the block's content box, and its height is that of the
/// formula with its numbers. The baselines are those of its first and last
/// lines, from its top.
pub(super) fn display_content(display: &Display, width: Abs, size: Abs) -> DisplayContent {
    let math = &display.math;
    let gutter = size * NUMBER_GUTTER;
    // Numbers, each with the baseline it sits on.
    let mut numbers: Vec<(&Number, Abs)> = Vec::new();
    for (number, baseline) in display.lines.iter().zip(&math.baselines) {
        if let Some(number) = number {
            numbers.push((number, *baseline));
        }
    }
    if let Some(number) = &display.whole {
        let baseline = match math.baselines.as_slice() {
            [one] => *one,
            _ => (math.height - number.ascent - number.descent) / 2.0 + number.ascent,
        };
        numbers.push((number, baseline));
    }
    let widest = numbers
        .iter()
        .map(|(n, _)| n.width)
        .max()
        .unwrap_or_default();
    let centred = (width - math.width) / 2.0;
    // Beside the formula where it fits with the gutter, at the formula's
    // place moved as far towards the start as needed.
    let beside =
        numbers.is_empty() || centred.min(width - widest - gutter - math.width) >= Abs::zero();
    if !beside {
        // Each number on a line of its own below the formula.
        let mut bottom = math.height;
        for (number, baseline) in &mut numbers {
            *baseline = bottom + gutter / 2.0 + number.ascent;
            bottom = *baseline + number.descent;
        }
    }
    let x = if numbers.is_empty() || !beside {
        centred
    } else {
        centred.min(width - widest - gutter - math.width)
    }
    .max(Abs::zero());
    // Numbers that reach above or below the formula make room for it.
    let above = numbers
        .iter()
        .map(|(n, baseline)| n.ascent - *baseline)
        .max()
        .unwrap_or_default()
        .max(Abs::zero());
    let below = numbers
        .iter()
        .map(|(n, baseline)| *baseline + n.descent - math.height)
        .max()
        .unwrap_or_default()
        .max(Abs::zero());
    let mut items = EcoVec::with_capacity(math.items.len());
    for (pos, item) in &math.items {
        items.push((Point::new(pos.x + x, pos.y + above), item.clone()));
    }
    for (number, baseline) in numbers {
        let nx = width - number.width;
        for (pos, item) in &number.items {
            items.push((
                Point::new(pos.x + nx, pos.y + baseline + above),
                item.clone(),
            ));
        }
    }
    let first = math.baselines.first().copied().unwrap_or(math.height);
    let last = math.baselines.last().copied().unwrap_or(math.height);
    DisplayContent {
        size: Size::new(width, above + math.height + below),
        items,
        baselines: (above + first, above + last),
    }
}

/// The content of a display's block.
pub(super) struct DisplayContent {
    pub size: Size,
    pub items: EcoVec<(Point, FragmentItem)>,
    /// The baselines of its first and last lines, from its top.
    pub baselines: (Abs, Abs),
}

/// How far a display is wider than its block's content, or zero.
pub(super) fn display_overflow(display: &Display, width: Abs) -> Abs {
    (display.math.width - width).max(Abs::zero())
}

/// The id a face's glyph runs name it by: its index in the build's faces.
pub(super) fn font_id(face: usize) -> FontId {
    FontId(u32::try_from(face).unwrap_or(u32::MAX))
}
