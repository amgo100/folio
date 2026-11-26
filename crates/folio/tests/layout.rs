//! The paragraph builder (SPEC §7.2) through the whole pipeline: lines
//! that cover their paragraphs, paragraph properties, the line-break
//! cache across edits, overfull and underfull lines as diagnostics, and
//! the time an ordinary paragraph takes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use folio::layout::{LineSlot, layout_paragraph};
use folio::{Abs, Lines, ParagraphLayout, Severity, Target, Text, World, diag};

/// The page is 100mm wide with 10mm margins: lines are 80mm wide.
const PAGE: &str = "page { page-size: (100mm, 150mm); margin-inner: 10mm; margin-outer: 10mm; }\n";

fn world(lang: &str, style: &str, text: &str) -> World {
    let manifest = format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"{lang}\"\nstyles = [\"styles/main.fsty\"]\n"
    );
    let mut world = World::new(&manifest).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            &format!("{PAGE}doc {{ font: \"Source Serif 4\"; }}\n{style}"),
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

/// The text of each line, with `-` where a hyphen is drawn.
fn line_texts(world: &World, index: usize) -> Vec<String> {
    let text = text(world);
    let paragraph = &text.paragraphs[index];
    let layout = &lines(world).paragraphs[index];
    layout
        .lines
        .iter()
        .map(|line| {
            let mut s = String::new();
            if line.repeated_hyphen {
                s.push('-');
            }
            s.push_str(&paragraph.shaped.text()[line.range.start..line.content_end]);
            if line.hyphen {
                s.push('-');
            }
            s
        })
        .collect()
}

const PROSE: &str = "Physics is the natural science of matter, involving the study of matter, \
    its fundamental constituents, its motion and behaviour through space and time, and the \
    related entities of energy and force. It is one of the most fundamental scientific \
    disciplines. A scientist who specialises in the field of physics is called a physicist.";

/// Every line starts where the previous one ended, and the last ends at
/// the paragraph's end.
fn assert_covers(text: &str, layout: &ParagraphLayout) {
    let mut at = 0;
    for line in &layout.lines {
        assert_eq!(line.range.start, at);
        assert!(line.content_end <= line.range.end);
        at = line.range.end;
    }
    assert_eq!(at, text.len());
}

#[test]
fn justified_lines_fill_their_width() {
    let world = world("en", "", &format!("{PROSE}\n"));
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let lines = lines(&world);
    let width = lines.width;
    assert!((width - Abs::mm(80.0)).to_pt().abs() < 1e-9);
    let layout = &lines.paragraphs[0];
    assert!(layout.optimal);
    assert!(layout.lines.len() >= 4, "{:?}", line_texts(&world, 0));
    assert_covers(text(&world).paragraphs[0].shaped.text(), layout);
    let (last, rest) = layout.lines.split_last().unwrap();
    for line in rest {
        assert!((line.width - width).to_pt().abs() < 1e-6, "{line:?}");
        assert_eq!(line.x, Abs::zero());
        assert!(line.overflow.is_zero() && !line.underfull);
    }
    // The last line is not stretched.
    assert!(last.width < width);
    assert_eq!(last.glue.get(), 0.0);
    // Baselines are a line's descent, the leading and the next line's
    // ascent apart; `leading` is 0.65em of 11pt.
    for pair in layout.lines.windows(2) {
        let gap = pair[1].baseline - pair[0].baseline;
        let expected = pair[0].descent + Abs::pt(0.65 * 11.0) + pair[1].ascent;
        assert!((gap - expected).to_pt().abs() < 1e-9);
    }
    assert_eq!(
        layout.height,
        last.baseline + last.descent,
        "the paragraph ends at its last line's descent"
    );
}

#[test]
fn ragged_lines_are_aligned() {
    for (align, expect) in [("start", 0.0), ("center", 0.5), ("end", 1.0)] {
        let world = world(
            "en",
            &format!("paragraph {{ justify: false; align: {align}; }}\n"),
            &format!("{PROSE}\n"),
        );
        let lines = lines(&world);
        let width = lines.width;
        for line in &lines.paragraphs[0].lines {
            assert_eq!(line.glue.get(), 0.0);
            assert!(line.width <= width);
            let free = (width - line.width).to_pt();
            assert!(
                (line.x.to_pt() - free * expect).abs() < 1e-9,
                "{align}: {line:?}"
            );
        }
    }
}

#[test]
fn right_to_left_lines_start_on_the_right() {
    let world = world(
        "he",
        "doc { font: \"New Computer Modern\"; }\nparagraph { justify: false; }\n",
        "שלום עולם, זהו משפט ארוך מספיק כדי להתפרס על פני כמה שורות בעמוד הצר הזה, \
         ועוד כמה מילים כדי לוודא שיש יותר משורה אחת.\n",
    );
    let lines = lines(&world);
    let layout = &lines.paragraphs[0];
    assert!(layout.lines.len() > 1);
    for line in &layout.lines {
        // Ragged lines start at the right edge.
        assert!(((line.x + line.width) - lines.width).to_pt().abs() < 1e-9);
    }
}

#[test]
fn the_first_line_is_indented() {
    let plain = world("en", "paragraph { justify: false; }\n", "A short line.\n");
    let indented = world(
        "en",
        "paragraph { justify: false; indent: 12pt; }\n",
        "A short line.\n",
    );
    let a = &lines(&plain).paragraphs[0].lines[0];
    let b = &lines(&indented).paragraphs[0].lines[0];
    assert!(((b.width - a.width) - Abs::pt(12.0)).to_pt().abs() < 1e-9);
}

#[test]
fn hyphenation_breaks_words() {
    let text = "Incomprehensibilities notwithstanding, characteristically \
        unconventional internationalisation considerations overwhelmingly \
        predominate.\n";
    let without = world("en", "paragraph { hyphenate: false; }\n", text);
    let with = world("en", "paragraph { hyphenate: true; }\n", text);
    assert!(line_texts(&without, 0).iter().all(|l| !l.ends_with('-')));
    let hyphenated = line_texts(&with, 0);
    assert!(
        hyphenated.iter().any(|l| l.ends_with('-')),
        "{hyphenated:?}"
    );
    // The hyphenated paragraph has no underfull lines; the other does.
    let has_underfull = |world: &World| {
        folio::compile(world, Target::Pdf, None)
            .unwrap()
            .diagnostics
            .iter()
            .any(|d| d.message.contains("underfull"))
    };
    assert!(!has_underfull(&with));
    assert!(has_underfull(&without));
}

#[test]
fn polish_single_letters_never_end_a_line() {
    let text = "Kot i pies, a w domu z bratem o tym rozmawiali, u nas w lesie i w polu \
        z kolegą o świcie, a potem w mieście i w szkole z nauczycielem o fizyce.\n";
    for width in [30.0, 40.0, 50.0, 60.0] {
        let world = world(
            "pl",
            &format!(
                "page {{ page-size: ({}mm, 100mm); margin-inner: 0mm; margin-outer: 0mm; }}\n\
                 paragraph {{ justify: false; }}\n",
                width
            ),
            text,
        );
        let lines = line_texts(&world, 0);
        assert!(lines.len() > 2);
        for line in &lines {
            let last = line.rsplit(' ').next().unwrap();
            assert!(
                last.chars().count() > 1,
                "{width}mm: a line ends in `{last}`: {lines:?}"
            );
        }
    }
}

#[test]
fn polish_repeats_a_hyphen_at_the_break() {
    let plain = world(
        "pl",
        "page { page-size: (28mm, 100mm); margin-inner: 0mm; margin-outer: 0mm; }\n\
         paragraph { justify: false; hyphenate: false; }\n",
        "Flaga biało-czerwona powiewała.\n",
    );
    let lines = line_texts(&plain, 0);
    assert_eq!(lines, ["Flaga biało-", "-czerwona", "powiewała."]);

    // The hyphen may be U+2010, three bytes long.
    let unicode = world(
        "pl",
        "page { page-size: (28mm, 100mm); margin-inner: 0mm; margin-outer: 0mm; }\n\
         doc { font: \"New Computer Modern\"; }\n\
         paragraph { justify: false; hyphenate: false; }\n",
        "Flaga biało\u{2010}czerwona powiewała.\n",
    );
    let lines = line_texts(&unicode, 0);
    assert_eq!(lines, ["Flaga biało\u{2010}", "-czerwona", "powiewała."]);
}

#[test]
fn protrusion_hangs_punctuation_into_the_margin() {
    let text = "Matter, energy, force, motion, time, space, light, heat, sound, waves, fields, \
        charge, mass, momentum, inertia, gravity.\n";
    let plain = world("en", "paragraph { hyphenate: false; }\n", text);
    let hanging = world(
        "en",
        "paragraph { hyphenate: false; protrusion: true; }\n",
        text,
    );
    let hung_lines = lines(&hanging);
    let width = hung_lines.width;
    let texts = line_texts(&hanging, 0);
    let mut hung = 0;
    for (line, text) in hung_lines.paragraphs[0]
        .lines
        .iter()
        .zip(&texts)
        .rev()
        .skip(1)
    {
        if text.ends_with(',') {
            // The comma hangs past the right edge.
            assert!(line.x + line.width > width, "{text}: {line:?}");
            hung += 1;
        }
    }
    assert!(hung > 0, "{texts:?}");
    // Without protrusion, no line passes the right edge.
    for line in &lines(&plain).paragraphs[0].lines {
        assert!((line.x + line.width - width).to_pt() < 1e-9, "{line:?}");
    }
}

#[test]
fn overfull_lines_are_reported_at_their_text() {
    let world = world(
        "en",
        "page { page-size: (30mm, 100mm); margin-inner: 5mm; margin-outer: 5mm; }\n\
         paragraph { hyphenate: false; }\n",
        "A Pneumonoultramicroscopicsilicovolcanoconiosis case.\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let overfull: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("overfull"))
        .collect();
    assert_eq!(overfull.len(), 1, "{:?}", compiled.diagnostics);
    let d = overfull[0];
    assert_eq!(d.severity, Severity::Warning);
    let location = diag::locate(&world, d.span).unwrap();
    let source = world.source("main.fol").unwrap();
    assert_eq!(
        &source.text()[location.bytes],
        "Pneumonoultramicroscopicsilicovolcanoconiosis"
    );
    let layout = &lines(&world).paragraphs[0];
    assert_eq!(
        layout
            .lines
            .iter()
            .filter(|l| !l.overflow.is_zero())
            .count(),
        1
    );
}

#[test]
fn unchanged_paragraphs_keep_their_lines() {
    let mut world = world(
        "en",
        "",
        &format!("{PROSE}\n\nSecond paragraph.\n\nThird paragraph, {PROSE}\n"),
    );
    let before = lines(&world);
    assert_eq!(before.paragraphs.len(), 3);

    // An edit to the second paragraph re-breaks only it.
    let source = world.source("main.fol").unwrap();
    let at = source.text().find("Second").unwrap();
    world
        .edit_source("main.fol", at..at + 6, "Another")
        .unwrap();
    let after = lines(&world);
    assert!(Arc::ptr_eq(&before.paragraphs[0], &after.paragraphs[0]));
    assert!(!Arc::ptr_eq(&before.paragraphs[1], &after.paragraphs[1]));
    assert!(Arc::ptr_eq(&before.paragraphs[2], &after.paragraphs[2]));

    // A change of colour moves nothing, so every paragraph keeps its lines.
    world
        .set_source(
            "styles/main.fsty",
            &format!("{PAGE}doc {{ font: \"Source Serif 4\"; color: rgb(\"#204080\"); }}\n"),
        )
        .unwrap();
    let painted = lines(&world);
    for (a, b) in after.paragraphs.iter().zip(&painted.paragraphs) {
        assert!(Arc::ptr_eq(a, b));
    }

    // A narrower page breaks them again.
    world
        .set_source(
            "styles/main.fsty",
            "page { page-size: (90mm, 150mm); margin-inner: 10mm; margin-outer: 10mm; }\n\
             doc { font: \"Source Serif 4\"; }\n",
        )
        .unwrap();
    let narrower = lines(&world);
    assert!(narrower.width < after.width);
    assert!(!Arc::ptr_eq(&after.paragraphs[0], &narrower.paragraphs[0]));
}

/// SPEC §7.2: Knuth–Plass takes well under a millisecond for an ordinary
/// paragraph. Debug builds are several times slower than release builds,
/// so they get more room.
#[test]
fn an_ordinary_paragraph_breaks_quickly() {
    let world = world(
        "en",
        "paragraph { hyphenate: true; }\n",
        &format!("{PROSE}\n"),
    );
    let text = text(&world);
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    let fonts = folio::fonts(&world).unwrap();
    let paragraph = &text.paragraphs[0];
    let input = folio::layout::input(paragraph, &styled.properties, &fonts);
    let slots = [LineSlot {
        offset: Abs::zero(),
        width: Abs::mm(80.0),
    }];
    let mut best = Duration::MAX;
    for _ in 0..20 {
        let start = Instant::now();
        let layout = layout_paragraph(&input, &slots, None);
        best = best.min(start.elapsed());
        assert!(layout.optimal);
    }
    let limit = if cfg!(debug_assertions) {
        Duration::from_millis(5)
    } else {
        Duration::from_micros(500)
    };
    eprintln!("an ordinary paragraph breaks in {best:?}");
    assert!(best < limit, "{best:?}");
}

/// Paragraphs are broken in parallel, and each gets the lines that
/// breaking it alone gives, in its place (SPEC §2.4); equal paragraphs
/// are broken once and share their lines.
#[test]
fn paragraphs_broken_in_parallel_keep_their_order() {
    let mut source = String::new();
    for i in 0..40 {
        // Every fifth paragraph is the same.
        if i % 5 == 0 {
            source.push_str(&format!("{PROSE}\n\n"));
        } else {
            source.push_str(&format!("Paragraph {i}: {}\n\n", &PROSE[..8 * i]));
        }
    }
    let world = world("en", "paragraph { hyphenate: true; }\n", &source);
    let text = text(&world);
    let lines = lines(&world);
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    let fonts = folio::fonts(&world).unwrap();
    assert_eq!(lines.paragraphs.len(), 40);
    for (i, (paragraph, layout)) in text.paragraphs.iter().zip(&lines.paragraphs).enumerate() {
        let input = folio::layout::input(paragraph, &styled.properties, &fonts);
        let slots = [LineSlot {
            offset: Abs::zero(),
            width: lines.slots[i].width,
        }];
        assert_eq!(**layout, layout_paragraph(&input, &slots, None), "{i}");
    }
    for i in (5..40).step_by(5) {
        assert!(Arc::ptr_eq(&lines.paragraphs[0], &lines.paragraphs[i]));
    }
}
