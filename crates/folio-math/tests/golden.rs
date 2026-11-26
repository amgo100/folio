//! Math layout golden files (SPEC §8.2, §14.2): a corpus of formulas laid
//! out in each of the four math fonts the spec names, as displays and as
//! inline formulas, dumped as text to `tests/golden/<font>.txt` and drawn
//! by `folio-render` to `tests/golden/<font>.png`.
//!
//! New Computer Modern Math and STIX Two Math are bundled with Folio;
//! Libertinus Math and Fira Math are test fonts, in `tests/fonts`.
//!
//! After a change meant to alter math layout, run the test with
//! `FOLIO_BLESS=1` and review the diff, images included, before committing
//! it.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ecow::EcoVec;
use folio_component::{Abs, Color, FontId, Fragment, FragmentItem, Paint, Point, Size, SourceRef};
use folio_math::layout::{MathFont, MathOptions, layout_display, layout_inline};
use folio_math::tex;
use folio_render::{DisplayList, FontBytes, Options, Outlines};
use folio_syntax::Span;

/// The formulas, as LaTeX: what each exercises of the MATH table.
const DISPLAYS: &[&str] = &[
    // Fractions, nested: numerator and denominator shifts and gaps.
    r"\frac{1}{2} m v^2 = \frac{\frac{a}{b} + 1}{c}",
    // Scripts: shifts, cramped subscripts, kerning against the base, primes.
    r"x^2 + x_i + x_i^2 + f'(x) + f''^2 + P_{\text{max}}",
    // Large operators with limits in display style, and an integral.
    r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2} \quad \prod_{k=1}^{n} k \quad \int_0^\infty e^{-x^2} \, \mathrm{d}x",
    // Limits of a named operator, and named operators.
    r"\lim_{x \to 0} \frac{\sin x}{x} = 1 \quad \log_2 8 = 3",
    // Roots: the radical stretched, with and without an index.
    r"\sqrt{2} + \sqrt{\frac{a}{b}} + \sqrt[3]{x^3 + y^3}",
    // Delimiters grown by variants and assembled from parts.
    r"\left( \frac{a}{b} \right) \left[ \sum_{i=1}^n \frac{x_i}{2} \right] \left\{ \frac{\frac{1}{2}}{3} \right\} \left| x \right|",
    // Matrices and their delimiters.
    r"\begin{pmatrix} a & b \\ c & d \end{pmatrix} \begin{bmatrix} 1 & 0 & 0 \\ 0 & 1 & 0 \\ 0 & 0 & 1 \end{bmatrix} \begin{vmatrix} x & y \\ z & w \end{vmatrix}",
    // Cases, aligned at their alignment points.
    r"|x| = \begin{cases} x & x \ge 0 \\ -x & x < 0 \end{cases}",
    // Accents stretched to their bases, dotless i, vectors.
    r"\hat{x} + \hat{i} + \hat{j} + \bar{x} + \dot{x} + \ddot{x} + \tilde{y} + \vec{F} + \widehat{xyz} + \widetilde{AB}",
    // Lines and braces with annotations.
    r"\overline{AB} + \underline{x} + \overbrace{a + b + c}^{3} + \underbrace{x + y}_{2}",
    // Alphabets.
    r"\mathbb{R} \mathcal{L} \mathfrak{g} \mathbf{F} \mathsf{A} \mathtt{x} \alpha \beta \Gamma \Delta \theta",
    // Relations and binary operators spaced, and a unary minus.
    r"-a \le b \ne c \cdot d \times e \pm f \to g \Rightarrow h",
];

/// A formula of several lines, aligned at its alignment points.
const ALIGNED: &str = r"\begin{aligned} v &= u + a t \\ s &= u t + \frac{1}{2} a t^2 \\ v^2 &= u^2 + 2 a s \end{aligned}";

/// Inline formulas, which break into chunks after relations and binary
/// operators.
const INLINES: &[&str] = &[
    r"v^2 = u^2 + 2 a s",
    r"F = \frac{m v - m u}{t}",
    r"a \le b \le c",
    r"(a + b)",
];

/// The fonts: a file and the name of its golden files.
const FONTS: &[(&str, &str)] = &[
    ("../folio-fonts/bundled/NewCMMath-Regular.otf", "newcm"),
    ("../folio-fonts/bundled/STIXTwoMath-Regular.ttf", "stix-two"),
    ("tests/fonts/LibertinusMath-Regular.otf", "libertinus"),
    ("tests/fonts/FiraMath-Regular.otf", "fira"),
];

/// The largest difference of a channel of a pixel from the golden image.
const TOLERANCE: u8 = 2;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn blessing() -> bool {
    std::env::var_os("FOLIO_BLESS").is_some_and(|v| v == "1")
}

fn formula(src: &str) -> folio_math::Formula {
    tex::parse(src, Span::detached(), 64).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

#[test]
fn every_font_sets_the_corpus() {
    let mut failures = Vec::new();
    for &(file, name) in FONTS {
        let data = std::fs::read(manifest().join(file)).expect("a font file");
        let bytes: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(data.clone());
        let text = folio_text::TextFont::new(bytes.clone(), 0).expect("a font");
        let font = MathFont::new(bytes, 0, text, FontId(0)).expect("a font");
        assert!(font.has_math(), "{name} has a MATH table");
        let options = MathOptions {
            font,
            size: Abs::pt(12.0),
            fill: Paint::Solid(Color::BLACK),
            leading: Abs::pt(6.0),
        };
        let (dump, page) = lay_out(&options);
        let dir = manifest().join("tests/golden");
        if let Err(e) = check_text(&dir.join(format!("{name}.txt")), &dump) {
            failures.push(e);
        }
        let image = render(&data, &page);
        if let Err(e) = check_image(&dir.join(format!("{name}.png")), &image) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Lays out the corpus: its dump, and a page with every formula on it.
fn lay_out(options: &MathOptions) -> (String, Fragment) {
    let source = |_: Span| SourceRef::DETACHED;
    let mut dump = String::new();
    let margin = Abs::pt(12.0);
    let gap = Abs::pt(10.0);
    let mut y = margin;
    let mut width = Abs::zero();
    let mut items: EcoVec<(Point, FragmentItem)> = EcoVec::new();
    for src in DISPLAYS.iter().chain([&ALIGNED]) {
        let laid = layout_display(&formula(src), options, &source);
        writeln!(
            dump,
            "display {src}\n  size {} x {}, baselines {}",
            pt(laid.width),
            pt(laid.height),
            laid.baselines
                .iter()
                .map(|b| pt(*b))
                .collect::<Vec<_>>()
                .join(" ")
        )
        .unwrap();
        dump_missing(&mut dump, &laid.missing);
        dump_items(&mut dump, &laid.items);
        for (pos, item) in &laid.items {
            items.push((Point::new(margin + pos.x, y + pos.y), item.clone()));
        }
        width = width.max(laid.width);
        y += laid.height + gap;
    }
    for src in INLINES {
        let laid = layout_inline(&formula(src), options, &source);
        writeln!(dump, "inline {src}").unwrap();
        dump_missing(&mut dump, &laid.missing);
        let ascent = laid
            .chunks
            .iter()
            .map(|c| c.ascent)
            .max()
            .unwrap_or_default();
        let descent = laid
            .chunks
            .iter()
            .map(|c| c.descent)
            .max()
            .unwrap_or_default();
        let mut x = margin;
        for chunk in &laid.chunks {
            writeln!(
                dump,
                "  chunk {} wide, {} up, {} down, break after {:?}",
                pt(chunk.width),
                pt(chunk.ascent),
                pt(chunk.descent),
                chunk.after.map(|a| (a.penalty, pt(a.space)))
            )
            .unwrap();
            dump_items(&mut dump, &chunk.items);
            for (pos, item) in &chunk.items {
                items.push((Point::new(x + pos.x, y + ascent + pos.y), item.clone()));
            }
            // Chunks drawn apart, to show where lines may break.
            x += chunk.width + gap;
        }
        width = width.max(x - margin);
        y += ascent + descent + gap;
    }
    let mut page = Fragment::new(Size::new(width + margin * 2.0, y + margin));
    page.items = items;
    (dump, page)
}

/// What a font could not set: Fira Math has no script, fraktur or
/// sans-serif letters.
fn dump_missing(out: &mut String, missing: &[folio_math::layout::MissingGlyph]) {
    for glyph in missing {
        writeln!(out, "  missing {:?}", glyph.text.as_str()).unwrap();
    }
}

fn dump_items(out: &mut String, items: &[(Point, FragmentItem)]) {
    for (pos, item) in items {
        match item {
            FragmentItem::Text(run) => {
                let glyphs: Vec<String> = run
                    .glyphs
                    .iter()
                    .map(|g| format!("{}+{}", g.id, em(g.x_advance.get())))
                    .collect();
                writeln!(
                    out,
                    "    text {} {} at {}pt: {:?} {}",
                    pt(pos.x),
                    pt(pos.y),
                    pt(run.size),
                    run.text.as_str(),
                    glyphs.join(" ")
                )
                .unwrap();
            }
            FragmentItem::Shape { .. } => {
                writeln!(out, "    rule {} {}", pt(pos.x), pt(pos.y)).unwrap();
            }
            other => writeln!(out, "    {other:?}").unwrap(),
        }
    }
}

/// A length, rounded so that the dump does not change with the last bits
/// of a float.
fn pt(abs: Abs) -> String {
    format!("{:.2}", abs.to_pt())
}

fn em(em: f64) -> String {
    format!("{em:.3}")
}

fn render(data: &[u8], page: &Fragment) -> folio_render::Image {
    let fonts = vec![FontBytes { data, index: 0 }];
    let list = DisplayList::from_fragment(page);
    let options = Options {
        scale: 2.0,
        background: Some(Color::WHITE),
    };
    folio_render::render(&list, &fonts, &mut Outlines::new(), &options).expect("a page renders")
}

fn check_text(path: &Path, actual: &str) -> Result<(), String> {
    let expected = std::fs::read_to_string(path).ok();
    if blessing() {
        if expected.as_deref() != Some(actual) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, actual).unwrap();
        }
        return Ok(());
    }
    match expected {
        None => Err(format!(
            "{} is missing: run with FOLIO_BLESS=1",
            path.display()
        )),
        Some(expected) if expected != actual => {
            let line = expected
                .lines()
                .zip(actual.lines())
                .position(|(a, b)| a != b)
                .map_or(expected.lines().count().min(actual.lines().count()), |i| i);
            Err(format!(
                "{} differs from line {}: expected {:?}, got {:?}",
                path.display(),
                line + 1,
                expected.lines().nth(line),
                actual.lines().nth(line)
            ))
        }
        Some(_) => Ok(()),
    }
}

fn check_image(path: &Path, actual: &folio_render::Image) -> Result<(), String> {
    let expected = std::fs::read(path)
        .ok()
        .and_then(|bytes| folio_render::Image::from_png(&bytes).ok());
    if blessing() {
        if expected.as_ref() != Some(actual) {
            std::fs::write(path, actual.png().unwrap()).unwrap();
        }
        return Ok(());
    }
    let Some(expected) = expected else {
        return Err(format!(
            "{} is missing: run with FOLIO_BLESS=1",
            path.display()
        ));
    };
    if (expected.width, expected.height) != (actual.width, actual.height) {
        return Err(format!("{}: the image has another size", path.display()));
    }
    let differ = actual
        .rgba
        .iter()
        .zip(&expected.rgba)
        .filter(|(a, b)| a.abs_diff(**b) > TOLERANCE)
        .count();
    if differ > 0 {
        return Err(format!("{}: {differ} channels differ", path.display()));
    }
    Ok(())
}
