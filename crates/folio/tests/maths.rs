//! Maths through the whole pipeline (SPEC §8.2): inline formulas sized in
//! their lines and broken only after relations and binary operators,
//! displays as the content of their blocks with their numbers, and the
//! characters the math font cannot set as warnings.

use folio::{Abs, FragmentItem, Lines, Severity, Target, Text, World};

/// The page is 100mm wide with 10mm margins: lines are 80mm wide.
const PAGE: &str = "page { page-size: (100mm, 150mm); margin-inner: 10mm; margin-outer: 10mm; }\n";

fn world(style: &str, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            &format!("{PAGE}doc {{ font: \"Source Serif 4\"; font-size: 10pt; }}\n{style}"),
        )
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn lines(world: &World) -> Lines {
    folio::lines(world, Target::Pdf, None).unwrap().unwrap()
}

fn text(world: &World) -> Text {
    folio::text(world, Target::Pdf, None).unwrap().unwrap()
}

/// For each line of a paragraph, the chunks of formulas it holds, as
/// (formula, chunk).
fn chunks_by_line(world: &World, index: usize) -> Vec<Vec<(usize, usize)>> {
    let text = text(world);
    let paragraph = &text.paragraphs[index];
    let layout = &lines(world).paragraphs[index];
    layout
        .lines
        .iter()
        .map(|line| {
            paragraph
                .objects
                .iter()
                .filter(|o| line.range.contains(&o.at))
                .filter_map(|o| o.math.map(|m| (m.formula, m.chunk)))
                .collect()
        })
        .collect()
}

#[test]
fn an_inline_formula_breaks_only_after_relations_and_binary_operators() {
    let world = world(
        "",
        "The sum is $a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p = q r s t u v w x$ here.\n",
    );
    let maths = lines(&world).maths;
    let by_line = chunks_by_line(&world, 0);
    assert!(by_line.len() > 1, "the formula is longer than a line");
    // Every line that ends inside the formula ends after a chunk that may
    // break, and the formula's chunks are in order across lines.
    let laid = match &maths.formulas[0] {
        Some(folio::layout::LaidMath::Inline(math)) => math.clone(),
        other => panic!("an inline formula, not {other:?}"),
    };
    let mut seen = Vec::new();
    for line in &by_line[..by_line.len() - 1] {
        if let Some(&(_, chunk)) = line.last() {
            seen.extend(line.iter().map(|&(_, c)| c));
            if chunk + 1 < laid.chunks.len() {
                assert!(laid.chunks[chunk].after.is_some());
            }
        }
    }
    seen.extend(by_line.last().unwrap().iter().map(|&(_, c)| c));
    assert_eq!(seen, (0..laid.chunks.len()).collect::<Vec<_>>());
    // `q r s t u v w x` is one chunk: a product does not break.
    assert_eq!(laid.chunks.len(), 17);
    let last = laid.chunks.last().unwrap();
    assert!(last.after.is_none());
    // Each break after `+` costs TeX's \binoppenalty, after `=` its
    // \relpenalty.
    assert_eq!(laid.chunks[0].after.unwrap().penalty, 700);
    assert_eq!(laid.chunks[15].after.unwrap().penalty, 500);
}

#[test]
fn a_formula_binds_to_the_text_it_touches() {
    // At this width the formula and its hyphenated word would end a line;
    // they stay together.
    let world = world(
        "",
        "The quantities measured along the direction of motion are on the $x$-axis and the rest on $y$, \
         the vertical.\n",
    );
    let text = text(&world);
    let paragraph = &text.paragraphs[0];
    let s = paragraph.shaped.text();
    for line in &lines(&world).paragraphs[0].lines {
        let start = &s[line.range.start..];
        assert!(
            !start.starts_with('-'),
            "a line starts with the hyphen after a formula"
        );
        assert!(
            !start.starts_with(','),
            "a line starts with the comma after a formula"
        );
    }
}

#[test]
fn a_display_is_its_block_with_its_numbers() {
    let world = world(
        "math { numbering: \"(1)\"; }\n",
        "Text.\n\n$ E = m c^2 $\n\n$ v &= u + a t <eq:v> \\\n s &= u t <eq:s> $\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:?}",
        compiled.diagnostics
    );
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    let mut texts = Vec::new();
    collect_texts(&pages.pages[0].fragment, &mut texts);
    for number in ["(1)", "(2)", "(3)"] {
        assert!(texts.iter().any(|t| t == number), "{number} in {texts:?}");
    }
    // The formulas' letters are set in their math alphabet.
    assert!(texts.iter().any(|t| t == "𝐸"), "{texts:?}");
}

fn collect_texts(fragment: &folio::Fragment, out: &mut Vec<String>) {
    for (_, item) in fragment.items.iter() {
        match item {
            FragmentItem::Group(group) => collect_texts(group, out),
            FragmentItem::Text(run) => out.push(run.text.to_string()),
            _ => {}
        }
    }
}

#[test]
fn a_character_the_math_font_cannot_set_is_a_warning() {
    let world = world("", "A symbol $x ☃ y$ here.\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let warning = compiled
        .diagnostics
        .iter()
        .find(|d| d.message.contains("has no glyph"))
        .expect("a warning");
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(
        warning.message,
        "the math font New Computer Modern Math has no glyph for `☃`, so it is left out"
    );
}

#[test]
fn an_inline_formula_is_as_tall_as_its_line_needs() {
    let world = world("", "A fraction $1/2$ and a letter $x$.\n");
    let maths = lines(&world).maths;
    let size = |i: usize| match &maths.formulas[i] {
        Some(folio::layout::LaidMath::Inline(math)) => math.chunks[0].ascent,
        _ => Abs::zero(),
    };
    assert!(size(0) > size(1));
}

#[test]
fn a_formula_s_glyphs_lead_back_to_their_nodes() {
    let world = world("", "Speed $v = alpha t$ here.\n\n$ sqrt(x) $\n");
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    let mut found = Vec::new();
    for placed in folio::span_map::glyphs(&pages.pages[0].fragment) {
        let text = placed.text();
        if ["𝑣", "𝛼", "√", "𝑥"].contains(&text) {
            let location = folio::span_map::locate_glyph(&world, placed.glyph).unwrap();
            let source = world.source(&location.path).unwrap();
            found.push((text.to_string(), source.text()[location.bytes].to_string()));
        }
    }
    assert_eq!(
        found,
        [
            ("𝑣".into(), "v".into()),
            ("𝛼".into(), "alpha".into()),
            ("√".into(), "sqrt(x)".into()),
            ("𝑥".into(), "x".into()),
        ]
    );
}

#[test]
fn a_display_wider_than_its_block_is_overfull_and_numbered_below() {
    let world = world(
        "math { numbering: \"(1)\"; }\n",
        "Text.\n\n$ a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r + s + t + u $\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let overfull = compiled
        .diagnostics
        .iter()
        .find(|d| d.message.starts_with("this display is overfull"))
        .expect("an overfull warning");
    assert_eq!(overfull.code.as_deref(), Some("overfull-line"));
    // The number is below the formula, not over it.
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    let mut runs = Vec::new();
    collect_runs(&pages.pages[0].fragment, folio::Point::zero(), &mut runs);
    let number = runs.iter().find(|(t, _)| t == "(1)").expect("the number").1;
    let formula = runs
        .iter()
        .find(|(t, _)| t == "𝑢")
        .expect("the last letter")
        .1;
    assert!(number.y > formula.y, "{number:?} below {formula:?}");
}

fn collect_runs(
    fragment: &folio::Fragment,
    at: folio::Point,
    out: &mut Vec<(String, folio::Point)>,
) {
    for (pos, item) in fragment.items.iter() {
        let p = folio::Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => collect_runs(group, p, out),
            FragmentItem::Text(run) => out.push((run.text.to_string(), p)),
            _ => {}
        }
    }
}

#[test]
fn an_empty_string_in_a_formula_sets_nothing_and_warns_of_nothing() {
    let world = world("", "A formula $x #\"\" y$ here.\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn a_number_the_math_font_cannot_set_is_a_warning() {
    let world = world("math { numbering: \"☃1\"; }\n", "Text.\n\n$ x $\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let messages: Vec<&str> = compiled
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "the math font New Computer Modern Math cannot set the equation number `☃1`, so it is left out"
        ]
    );
}

/// The texts of the glyph runs on a build's first page, and its
/// diagnostics' messages.
fn drawn(world: &World) -> (Vec<String>, Vec<String>) {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    let pages = folio::pages(world, Target::Pdf, None).unwrap().unwrap();
    let mut texts = Vec::new();
    collect_texts(&pages.pages[0].fragment, &mut texts);
    let messages = compiled
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    (texts, messages)
}

#[test]
fn every_formula_is_a_pdf_formula_with_its_source_as_alt_text() {
    let world = world(
        "",
        "Speed $v = u + a t$ and $:tex \\frac{1}{2}$ here.\n\n$ s = u t $\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let pdf = compiled.output.expect("a PDF");
    let pdf = String::from_utf8_lossy(pdf.as_ref());
    assert_eq!(pdf.matches("/S/Formula").count(), 3, "three formulas");
    for alt in ["/Alt(v = u + a t)", "/Alt(\\\\frac{1}{2})", "/Alt(s = u t)"] {
        assert!(pdf.contains(alt), "{alt}");
    }
}

#[test]
fn math_font_chooses_the_family_and_warns_of_what_it_cannot_use() {
    let width = |style: &str| {
        let world = world(style, "A formula $x + y = z$ here.\n");
        let (_, messages) = drawn(&world);
        let lines = lines(&world);
        let line = &lines.paragraphs[0].lines[0];
        (line.width, messages)
    };
    let (newcm, messages) = width("");
    assert!(messages.is_empty(), "{messages:?}");
    let (stix, messages) = width("math { math-font: \"STIX Two Math\"; }\n");
    assert!(messages.is_empty(), "{messages:?}");
    assert_ne!(newcm, stix, "STIX Two Math sets the formula");
    let (fallback, messages) = width("math { math-font: \"Nope Math\"; }\n");
    assert_eq!(fallback, newcm, "New Computer Modern Math sets it instead");
    assert!(
        messages.iter().any(|m| m.contains("no family of `math-font`")),
        "{messages:?}"
    );
    let (_, messages) = width("math { math-font: \"Source Serif 4\"; }\n");
    assert!(
        messages.iter().any(|m| m.contains("is not a math font")),
        "{messages:?}"
    );
}

#[test]
fn vectors_are_set_in_the_style_of_their_part() {
    let text = |style: &str| drawn(&world(style, "A field $vec(E)$ here.\n")).0.concat();
    assert!(text("").contains('𝐄'), "bold upright by default");
    assert!(
        text("math.vector { math-style: bold-italic; }\n").contains('𝑬'),
        "bold italic"
    );
    let arrow = text("math.vector { math-style: arrow; }\n");
    assert!(arrow.contains('𝐸') && arrow.contains('\u{20D7}'), "{arrow}");
}

#[test]
fn quantities_are_written_with_the_decimal_marker() {
    let text = |style: &str| {
        drawn(&world(style, "Gravity $g = qty(9.81, \"m s^-2\")$ here.\n"))
            .0
            .concat()
    };
    assert!(text("").contains("9.81"), "{}", text(""));
    let comma = text("math { decimal-marker: comma; }\n");
    assert!(comma.contains("9,81"), "{comma}");
}
