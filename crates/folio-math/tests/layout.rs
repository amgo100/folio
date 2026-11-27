//! Math layout with the bundled math fonts.

use std::sync::Arc;

use folio_component::{Abs, Color, FontId, FragmentItem, Paint};
use folio_math::layout::{MathFont, MathOptions, inline_chunks, layout_display, layout_inline};
use folio_math::tex;
use folio_syntax::Span;

fn font(file: &str) -> MathFont {
    let path = format!(
        "{}/../folio-fonts/bundled/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(std::fs::read(path).unwrap());
    let text = folio_text::TextFont::new(data.clone(), 0).unwrap();
    MathFont::new(data, 0, text, FontId(0)).unwrap()
}

fn options(font: MathFont) -> MathOptions {
    MathOptions {
        font,
        size: Abs::pt(10.0),
        fill: Paint::Solid(Color::BLACK),
        leading: Abs::pt(5.0),
        vector: folio_math::layout::VectorStyle::default(),
        number: folio_math::NumberFormat::POINT,
    }
}

fn formula(src: &str) -> folio_math::Formula {
    tex::parse(src, Span::detached(), 64).unwrap()
}

#[test]
fn inline_formulas_break_after_relations_and_operators() {
    let opts = options(font("NewCMMath-Regular.otf"));
    let f = formula(r"v^2 = u^2 + 2 a s");
    assert_eq!(inline_chunks(&f), 3);
    let laid = layout_inline(&f, &opts, &|_| folio_component::SourceRef::DETACHED);
    assert!(laid.missing.is_empty(), "{:?}", laid.missing);
    assert_eq!(laid.chunks.len(), 3);
    for chunk in &laid.chunks {
        eprintln!(
            "{:?} {:?} {:?} {:?}",
            chunk.width, chunk.ascent, chunk.descent, chunk.after
        );
        assert!(chunk.width > Abs::zero());
    }
}

#[test]
fn display_formulas_lay_out_in_every_bundled_math_font() {
    for file in ["NewCMMath-Regular.otf", "STIXTwoMath-Regular.ttf"] {
        let opts = options(font(file));
        for src in [
            r"\frac{1}{2} m v^2",
            r"\sum_{i=1}^n x_i",
            r"\int_0^T P(t) \, \mathrm{d}t",
            r"\sqrt[3]{\frac{a}{b}}",
            r"\left( \frac{a}{b} \right)",
            r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}",
            r"\hat{x} + \vec{F} + \overline{AB}",
            r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & x \le 0 \end{cases}",
        ] {
            let laid = layout_display(&formula(src), &opts, &|_| {
                folio_component::SourceRef::DETACHED
            });
            assert!(laid.missing.is_empty(), "{file} {src}: {:?}", laid.missing);
            let glyphs = laid
                .items
                .iter()
                .filter(|(_, item)| matches!(item, FragmentItem::Text(_)))
                .count();
            eprintln!(
                "{file} {src}: {:?} x {:?}, {glyphs} runs",
                laid.width, laid.height
            );
            assert!(glyphs > 0);
        }
    }
}

#[test]
fn no_break_between_a_binary_operator_and_a_relation() {
    // The relation's space would start the next line.
    assert_eq!(inline_chunks(&formula(r"a + = b")), 2);
    // Nor before a closing delimiter, which the formula's own delimiters
    // keep together anyway.
    assert_eq!(inline_chunks(&formula(r"a \le b \le c")), 3);
}

/// The glyph runs of a laid-out formula, left to right: their text, where
/// they start and where they end, in points.
fn runs(items: &[(folio_component::Point, FragmentItem)]) -> Vec<(String, f64, f64)> {
    fn walk(
        items: &[(folio_component::Point, FragmentItem)],
        dx: f64,
        out: &mut Vec<(String, f64, f64)>,
    ) {
        for (point, item) in items {
            let x = dx + point.x.to_pt();
            match item {
                FragmentItem::Text(run) => {
                    out.push((run.text.to_string(), x, x + run.width().to_pt()))
                }
                FragmentItem::Group(fragment) => walk(&fragment.items, x, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(items, 0.0, &mut out);
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// The gaps between consecutive runs of a quantity set at 10pt, in points.
fn quantity_gaps(value: &str, unit: &str) -> Vec<(String, f64)> {
    use folio_math::{Formula, Node, Number, Quantity, Row, Unit};
    let quantity = Quantity {
        value: Some(Number::parse(value).unwrap()),
        pm: None,
        unit: Some(Unit::parse(unit).unwrap()),
        span: Span::detached(),
    };
    let formula = Formula::row(
        Row::new([Node::Quantity(Arc::new(quantity))]),
        Span::detached(),
    );
    let opts = options(font("NewCMMath-Regular.otf"));
    let laid = layout_display(&formula, &opts, &|_| folio_component::SourceRef::DETACHED);
    assert!(laid.missing.is_empty(), "{:?}", laid.missing);
    let runs = runs(&laid.items);
    runs.windows(2)
        .map(|pair| {
            let gap = pair[1].1 - pair[0].2;
            (pair[1].0.clone(), (gap * 1000.0).round() / 1000.0)
        })
        .collect()
}

#[test]
fn units_are_spaced_by_thin_spaces_only() {
    // SPEC §8.1: a thin space, ⅙ em (1.667pt at 10pt), between the number
    // and its unit and between the factors of the unit.
    let thin = 1.667;
    let gaps = quantity_gaps("4.0", "m s^-1");
    assert_eq!(gaps[0], ("m".into(), thin), "{gaps:?}");
    assert_eq!(gaps[1], ("s".into(), thin), "{gaps:?}");
    let gaps = quantity_gaps("0.20", "mm^2");
    assert_eq!(gaps[0], ("mm".into(), thin), "{gaps:?}");
    let gaps = quantity_gaps("9.81", "kg m^2 s^-2");
    assert_eq!(gaps[0], ("kg".into(), thin), "{gaps:?}");
    assert_eq!(gaps[1], ("m".into(), thin), "{gaps:?}");
    // The degree, minute and second of plane angle follow the number.
    for unit in ["deg", "arcmin", "arcsec"] {
        let gaps = quantity_gaps("35", unit);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].1, 0.0, "{unit}: {gaps:?}");
    }
    // Degrees Celsius take the thin space.
    assert_eq!(quantity_gaps("20", "degC")[0].1, thin);
}
