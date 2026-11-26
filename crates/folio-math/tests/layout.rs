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
