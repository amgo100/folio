//! Answer space and hidden content (SPEC §6.3): `display: answer-space`
//! as ruled lines, a box of `answer-height`, or the answer's own height
//! times `answer-scale`; `display: hidden` keeping the height of what it
//! hides, so that a paper and its mark scheme break pages alike; the
//! heights layout gives them, for the Office writers; and nothing of what
//! they leave out reaching the output (SPEC §6.4).

use folio::{Abs, Compiled, Fragment, FragmentItem, Name, Pages, Point, Severity, Target, World};

/// A5 with 15mm margins: a body of 118mm by 180mm.
const PAGE: &str = "page { page-size: A5; margin-top: 15mm; margin-bottom: 15mm; \
                    margin-inner: 15mm; margin-outer: 15mm; }\n\
                    doc { font: \"Source Serif 4\"; font-size: 11pt; }\n";

fn world(style: &str, markscheme: &str, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n\n\
                    [variants.exam]\ntargets = [\"pdf\", \"docx\"]\n\n\
                    [variants.markscheme]\ntargets = [\"pdf\"]\n\
                    styles = [\"styles/markscheme.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("styles/main.fsty", &format!("{PAGE}{style}"))
        .unwrap();
    world
        .set_source("styles/markscheme.fsty", markscheme)
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn name(variant: &str) -> Name {
    Name::new(variant).unwrap()
}

fn pages(world: &World, variant: &str) -> Pages {
    folio::pages(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
}

fn compile(world: &World, variant: &str) -> Compiled {
    folio::compile(world, Target::Pdf, Some(&name(variant))).unwrap()
}

/// The warnings and errors of a variant's build.
fn diagnostics(world: &World, variant: &str) -> Vec<String> {
    compile(world, variant)
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error | Severity::Warning))
        .map(|d| d.message.to_string())
        .collect()
}

/// A paragraph of `n` words.
fn prose(n: usize) -> String {
    let words = ["force", "energy", "momentum", "field", "charge", "wave"];
    let mut out = String::from("Text");
    for i in 0..n {
        out.push(' ');
        out.push_str(words[i % words.len()]);
    }
    out.push('.');
    out
}

/// Every glyph run on the pages: its page, where its baseline starts on
/// the page, and its text.
fn runs(pages: &Pages) -> Vec<(usize, Point, String)> {
    fn walk(fragment: &Fragment, at: Point, page: usize, out: &mut Vec<(usize, Point, String)>) {
        for (pos, item) in &fragment.items {
            let here = Point::new(at.x + pos.x, at.y + pos.y);
            match item {
                FragmentItem::Group(group) => walk(group, here, page, out),
                FragmentItem::Text(run) => {
                    let text: String = run
                        .glyphs
                        .iter()
                        .map(|g| &run.text[g.range.start as usize..g.range.end as usize])
                        .collect();
                    out.push((page, here, text));
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (i, page) in pages.pages.iter().enumerate() {
        walk(&page.fragment, Point::zero(), i, &mut out);
    }
    out
}

/// All the text the pages show.
fn shown(pages: &Pages) -> String {
    runs(pages)
        .into_iter()
        .map(|(.., text)| text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where the first run that shows `word` is: its page and baseline.
fn find(pages: &Pages, word: &str) -> (usize, Abs) {
    runs(pages)
        .into_iter()
        .find(|(.., text)| text.contains(word))
        .map(|(page, at, _)| (page, at.y))
        .unwrap_or_else(|| panic!("no run shows {word:?}: {:?}", shown(pages)))
}

/// The number of horizontal rules drawn on the pages.
fn rules(pages: &Pages) -> usize {
    fn walk(fragment: &Fragment, out: &mut usize) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(group, out),
                FragmentItem::Shape { .. } => *out += 1,
                _ => {}
            }
        }
    }
    let mut out = 0;
    for page in &pages.pages {
        walk(&page.fragment, &mut out);
    }
    out
}

/// Asserts that two runs are at the same place, but for rounding.
#[track_caller]
fn assert_same(a: (usize, Abs), b: (usize, Abs), what: &str) {
    assert!(a.0 == b.0 && close(a.1, b.1), "{what}: {a:?} and {b:?}");
}

fn close(a: Abs, b: Abs) -> bool {
    (a.to_pt() - b.to_pt()).abs() < 0.01
}

const SHOWN: &str = "figure.answer { display: block; }\n";

#[test]
fn ruled_lines_take_the_place_of_the_answer() {
    let world = world(
        "figure.answer { display: answer-space; rule-lines: 4; rule-spacing: 10mm; }\n",
        SHOWN,
        "Question one.\n\n#figure(class: \"answer\")[SECRET answer.]\n\nAfter.\n",
    );
    let exam = pages(&world, "exam");
    assert!(!shown(&exam).contains("SECRET"), "{}", shown(&exam));
    assert_eq!(rules(&exam), 4);
    assert_eq!(exam.spaces.len(), 1);
    assert!(close(exam.spaces[0].height, Abs::mm(40.0)));
    let (_, question) = find(&exam, "Question");
    let (_, after) = find(&exam, "After");
    assert!(after - question > Abs::mm(40.0));
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
    // The mark scheme shows the answer.
    assert!(shown(&pages(&world, "markscheme")).contains("SECRET"));
}

#[test]
fn a_fixed_height_is_a_box_of_that_height() {
    let style = "figure.answer { display: answer-space; answer-height: 30mm; }\n";
    let answer = world(
        style,
        SHOWN,
        "Question one.\n\n#figure(class: \"answer\")[SECRET answer.]\n\nAfter.\n",
    );
    let exam = pages(&answer, "exam");
    assert!(!shown(&exam).contains("SECRET"));
    assert_eq!(rules(&exam), 0);
    assert!(close(exam.spaces[0].height, Abs::mm(30.0)));
    // As tall as an empty block of that `min-height`.
    let sized = world(
        "figure.answer { min-height: 30mm; }\n",
        "",
        "Question one.\n\n#figure(class: \"answer\")[]\n\nAfter.\n",
    );
    assert_same(
        find(&exam, "After"),
        find(&pages(&sized, "exam"), "After"),
        "After",
    );
}

#[test]
fn auto_answer_space_is_as_tall_as_the_answer_shown() {
    let text = format!(
        "Question one.\n\n#figure(class: \"answer\")[SECRET {}\n\n{}]\n\nAfter.\n",
        prose(40),
        prose(25)
    );
    let shown_world = world(
        "figure.answer { display: answer-space; border: 0.5pt; padding: 2mm; }\n",
        SHOWN,
        &text,
    );
    let exam = pages(&shown_world, "exam");
    let markscheme = pages(&shown_world, "markscheme");
    assert!(!shown(&exam).contains("SECRET"));
    // With `answer-scale: 1` the box is as tall as the answer set in it,
    // so what follows starts where it does in the mark scheme.
    assert_same(find(&exam, "After"), find(&markscheme, "After"), "After");
    assert!(exam.spaces[0].height > Abs::mm(20.0));

    // `answer-scale` multiplies the content's height, not the box's.
    let scaled = world(
        "figure.answer { display: answer-space; border: 0.5pt; padding: 2mm; \
         answer-scale: 1.5; }\n",
        SHOWN,
        &text,
    );
    let scaled = pages(&scaled, "exam");
    let ratio = scaled.spaces[0].height.to_pt() / exam.spaces[0].height.to_pt();
    assert!((ratio - 1.5).abs() < 1e-6, "{ratio}");
}

#[test]
fn hidden_content_keeps_its_height() {
    let text = format!(
        "Question one.\n\n#figure(class: \"note\")[HIDDEN {}]\n\nAfter.\n",
        prose(60)
    );
    let world = world(
        "figure.note { display: hidden; border: 1pt; padding: 3mm; space-before: 4mm; \
         background: #eeeeee; }\n",
        "figure.note { display: block; }\n",
        &text,
    );
    let exam = pages(&world, "exam");
    let markscheme = pages(&world, "markscheme");
    assert!(!shown(&exam).contains("HIDDEN"), "{}", shown(&exam));
    assert!(shown(&markscheme).contains("HIDDEN"));
    assert_same(find(&exam, "After"), find(&markscheme, "After"), "After");
    // Nothing is drawn: no border, no background.
    assert_eq!(rules(&exam), 0);
    assert_eq!(exam.spaces.len(), 1);
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
}

#[test]
fn hidden_content_keeps_paper_and_mark_scheme_paginated_alike() {
    // A question set over several pages, its working hidden in the paper.
    let mut text = String::new();
    for i in 1..=8 {
        text.push_str(&format!(
            "Question {i}. {}\n\n#figure(class: \"working\")[WORKING {}\n\n$ v = u + a t $]\n\n",
            prose(30),
            prose(50)
        ));
    }
    text.push_str("End of paper.\n");
    let world = world(
        "figure.working { display: hidden; }\n",
        "figure.working { display: block; }\n",
        &text,
    );
    let exam = pages(&world, "exam");
    let markscheme = pages(&world, "markscheme");
    assert!(exam.pages.len() > 2);
    assert_eq!(exam.pages.len(), markscheme.pages.len());
    for i in 1..=8 {
        let q = format!("Question {i}.");
        assert_same(find(&exam, &q), find(&markscheme, &q), &q);
    }
    assert_same(find(&exam, "End"), find(&markscheme, "End"), "End");
    assert!(!shown(&exam).contains("WORKING"));
    assert_eq!(exam.spaces.len(), 8);
}

#[test]
fn nested_answer_space_is_measured_as_space() {
    // A hidden question holding an answer space is as tall as the question
    // with its answer space, not with the answer.
    let text = format!(
        "#figure(class: \"q\")[Stem.\n\n#figure(class: \"answer\")[SECRET {}]]\n\nAfter.\n",
        prose(80)
    );
    let style = "figure.answer { display: answer-space; rule-lines: 2; }\n\
                 figure.q { display: hidden; }\n";
    let hidden = world(style, "figure.q { display: block; }\n", &text);
    let exam = pages(&hidden, "exam");
    let markscheme = pages(&hidden, "markscheme");
    assert!(!shown(&exam).contains("SECRET"));
    assert!(!shown(&markscheme).contains("SECRET"));
    assert_same(find(&exam, "After"), find(&markscheme, "After"), "After");
    // The outer space and the inner one measured with it.
    assert_eq!(exam.spaces.len(), 1);
}

#[test]
fn a_part_keeps_its_space() {
    let world = world(
        "figure > caption { display: hidden; }\n",
        "figure > caption { display: block; }\n",
        "#figure(caption: [SECRET caption that runs to a second line or more, \
         as captions of figures often do.])[Body.]\n\nAfter.\n",
    );
    let exam = pages(&world, "exam");
    let markscheme = pages(&world, "markscheme");
    assert!(!shown(&exam).contains("SECRET"), "{}", shown(&exam));
    assert!(shown(&exam).contains("Body"));
    assert_same(find(&exam, "After"), find(&markscheme, "After"), "After");
    assert_eq!(exam.spaces.len(), 1);
    assert_eq!(
        exam.spaces[0].part.as_ref().map(|p| p.as_str()),
        Some("caption")
    );
}

#[test]
fn inline_content_is_left_out_with_a_warning() {
    let world = world(
        "strong.mark { display: hidden; }\nemph.mark { display: answer-space; }\n",
        "",
        "One #strong(class: \"mark\")[SECRET] two #emph(class: \"mark\")[OTHER] three.\n",
    );
    let exam = pages(&world, "exam");
    assert!(!shown(&exam).contains("SECRET"));
    assert!(!shown(&exam).contains("OTHER"));
    let found = diagnostics(&world, "exam");
    let count = |what: &str| found.iter().filter(|m| m.contains(what)).count();
    assert_eq!(
        count("`display: hidden` keeps the space of blocks only"),
        1,
        "{found:?}"
    );
    assert_eq!(
        count("`display: answer-space` makes answer space of blocks only"),
        1
    );
}

#[test]
fn heights_are_measured_for_the_office_targets() {
    // SPEC §2.1: DOCX takes the heights Folio's layout measures.
    let world = world(
        "figure.note { display: hidden; }\n",
        "",
        &format!("#figure(class: \"note\")[{}]\n\nAfter.\n", prose(40)),
    );
    let pdf = pages(&world, "exam");
    let docx = folio::pages(&world, Target::Docx, Some(&name("exam")))
        .unwrap()
        .unwrap();
    assert_eq!(pdf.spaces, docx.spaces);
    assert!(pdf.spaces[0].height > Abs::zero());
}

#[test]
fn nothing_hidden_reaches_the_pdf() {
    // SPEC §6.4: no text, tag, bookmark or alt text of what a space
    // replaced, though layout measured it.
    let world = world(
        "figure.answer { display: answer-space; border: 0.5pt; }\n\
         figure.note { display: hidden; }\n",
        "figure.answer, figure.note { display: block; }\n",
        "#doc(title: [Paper])\n\n= Questions\n\nQuestion one.\n\n\
         #figure(class: \"answer\")[= Zebrafish\n\nQuokka #image(\"wave.svg\", alt: \"Narwhal\")]\n\n\
         #figure(class: \"note\")[= Axolotl\n\nPangolin.]\n\nAfter.\n",
    );
    let mut world = world;
    world
        .set_file(
            "wave.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\">\
              <rect width=\"20\" height=\"10\"/></svg>"
                .to_vec(),
        )
        .unwrap();
    let pdf = |variant: &str| {
        let compiled = compile(&world, variant);
        String::from_utf8_lossy(compiled.output.as_deref().unwrap()).into_owned()
    };
    let words = ["Zebrafish", "Quokka", "Narwhal", "Axolotl", "Pangolin"];
    let markscheme = pdf("markscheme");
    // The words are found where they are shown, so their absence means
    // something.
    assert!(markscheme.contains("Zebrafish") && markscheme.contains("Narwhal"));
    let exam = pdf("exam");
    for word in words {
        assert!(!exam.contains(word), "{word} reached the paper");
    }
    let exam = pages(&world, "exam");
    for word in words {
        assert!(!shown(&exam).contains(word), "{word} was drawn");
    }
}

#[test]
fn hidden_tables_and_cells_keep_their_height() {
    let rows: String = (1..=12)
        .map(|i| format!("#cell[{i}] #cell[SECRET row {i}, with a longer note that wraps]\n"))
        .collect();
    let text = format!(
        "Before.\n\n#grid(class: \"t\", column-tracks: (auto, 1fr), header-rows: 1)[\n\
         #cell[Run] #cell[Note]\n{rows}]\n\nAfter.\n"
    );
    for style in [
        "grid.t { display: hidden; }\n",
        "cell { display: hidden; }\n",
    ] {
        let world = world(style, "grid.t, cell { display: block; }\n", &text);
        let exam = pages(&world, "exam");
        let markscheme = pages(&world, "markscheme");
        assert!(!shown(&exam).contains("SECRET"), "{style}");
        assert!(shown(&markscheme).contains("SECRET"));
        assert_same(find(&exam, "After"), find(&markscheme, "After"), style);
    }
}

#[test]
fn hidden_footnotes_are_not_placed() {
    let world = world(
        "figure.note { display: hidden; }\n",
        "figure.note { display: block; }\n",
        "Before.\n\n#figure(class: \"note\")[Hidden text.#footnote[SECRET note.]]\n\n\
         After.#footnote[Shown note.]\n",
    );
    let exam = pages(&world, "exam");
    assert!(!shown(&exam).contains("SECRET"), "{}", shown(&exam));
    assert!(shown(&exam).contains("Shown"));
    assert_same(
        find(&exam, "After"),
        find(&pages(&world, "markscheme"), "After"),
        "After",
    );
}

#[test]
fn hidden_cells_size_their_columns() {
    // An `auto` column is as wide as the content of its cells as the
    // variant that shows them sets it.
    let text = "Before.\n\n#grid(column-tracks: (auto, 1fr))[\n#cell[A] #cell[Note]\n\
                #cell(class: \"h\")[SECRET a long cell that sets the column wide] \
                #cell[x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x x]\n\
                ]\n\nAfter.\n";
    let world = world(
        "cell.h { display: hidden; }\n",
        "cell.h { display: block; }\n",
        text,
    );
    let exam = pages(&world, "exam");
    assert!(!shown(&exam).contains("SECRET"));
    assert_same(
        find(&exam, "After"),
        find(&pages(&world, "markscheme"), "After"),
        "After",
    );
}

#[test]
fn hidden_content_breaks_where_shown_content_does() {
    // Hidden content that the showing variant breaks across pages, and
    // content taller than a page, keep both variants' pages alike.
    for (before, hidden) in [(14, 6), (2, 30)] {
        let mut text = String::new();
        for i in 0..before {
            text.push_str(&format!("Lead {i}. {}\n\n", prose(20)));
        }
        text.push_str("#figure(class: \"h\")[");
        for i in 0..hidden {
            text.push_str(&format!("SECRET {i}. {}\n\n", prose(30)));
        }
        text.push_str("]\n\nAfter.\n");
        let world = world(
            "figure.h { display: hidden; }\n",
            "figure.h { display: block; }\n",
            &text,
        );
        let exam = pages(&world, "exam");
        let markscheme = pages(&world, "markscheme");
        assert!(markscheme.pages.len() > 1);
        assert!(!shown(&exam).contains("SECRET"));
        assert_eq!(
            exam.pages.len(),
            markscheme.pages.len(),
            "{before} {hidden}"
        );
        assert_same(find(&exam, "After"), find(&markscheme, "After"), "After");
    }
}

#[test]
fn answer_scale_leaves_min_height_unscaled() {
    let world = world(
        "figure.answer { display: answer-space; answer-scale: 1.5; min-height: 30mm; }\n",
        SHOWN,
        "#figure(class: \"answer\")[Short.]\n\nAfter.\n",
    );
    assert!(close(pages(&world, "exam").spaces[0].height, Abs::mm(30.0)));
}

#[test]
fn deeply_nested_spaces_are_measured_once_each() {
    // Each level measures the one inside it: the work grows with the
    // depth, not with two to its power.
    let mut text = String::from("Deep.");
    for _ in 0..40 {
        text = format!("#figure(class: \"h\")[{text}]");
    }
    let world = world(
        "figure.h { display: hidden; }\n",
        "",
        &format!("{text}\n\nAfter.\n"),
    );
    let exam = pages(&world, "exam");
    assert!(!shown(&exam).contains("Deep"));
    assert!(shown(&exam).contains("After"));
}

#[test]
fn hidden_blocks_keep_together_as_shown_ones_do() {
    // A break inside a hidden block costs what it costs inside the shown
    // one, not twice that.
    for keep in ["keep-together: true;", "breakable: false;", ""] {
        for lead in [10, 12, 13, 14] {
            let mut text = String::new();
            for i in 0..lead {
                text.push_str(&format!("Lead {i}. {}\n\n", prose(20)));
            }
            text.push_str(&format!(
                "#figure(class: \"h\")[SECRET {}\n\n{}]\n\nAfter.\n",
                prose(40),
                prose(40)
            ));
            let world = world(
                &format!("figure.h {{ display: hidden; {keep} }}\n"),
                "figure.h { display: block; }\n",
                &text,
            );
            let exam = pages(&world, "exam");
            let markscheme = pages(&world, "markscheme");
            assert_same(
                find(&exam, "After"),
                find(&markscheme, "After"),
                &format!("{keep} {lead}"),
            );
        }
    }
}

#[test]
fn a_hidden_caption_is_measured_with_its_marker() {
    // Captions whose text nearly fills a line, so that the marker before
    // them decides whether they wrap.
    for n in 8..16 {
        let world = world(
            "figure > caption { display: hidden; }\n",
            "figure > caption { display: block; }\n",
            &format!("#figure(caption: [{}])[Body.]\n\nAfter.\n", prose(n)),
        );
        assert_same(
            find(&pages(&world, "exam"), "After"),
            find(&pages(&world, "markscheme"), "After"),
            &n.to_string(),
        );
    }
}

#[test]
fn answer_space_breaks_across_pages_only_when_asked() {
    // A 120mm box after about 85mm of text on a 180mm body: whole, it moves to
    // the next page; with `answer-breakable: true` it starts on this one.
    let text = format!(
        "{}\n\n#figure(class: \"answer\")[Answer.]\n\nAfter.\n",
        (0..4).map(|_| prose(25)).collect::<Vec<_>>().join("\n\n")
    );
    let style = "figure.answer { display: answer-space; answer-height: 120mm; \
                 border: 0.5pt; space-before: 0pt; }\n";
    let whole = pages(&world(style, SHOWN, &text), "exam");
    let broken = pages(
        &world(
            &format!("{style}figure.answer {{ answer-breakable: true; }}\n"),
            SHOWN,
            &text,
        ),
        "exam",
    );
    assert!(close(whole.spaces[0].height, Abs::mm(120.0)));
    assert!(close(broken.spaces[0].height, Abs::mm(120.0)));
    // Its border is drawn on both pages it is broken across.
    let borders = |pages: &Pages, page: usize| {
        let mut out = 0;
        fn walk(fragment: &Fragment, out: &mut usize) {
            for (_, item) in &fragment.items {
                match item {
                    FragmentItem::Group(group) => walk(group, out),
                    FragmentItem::Shape { .. } => *out += 1,
                    _ => {}
                }
            }
        }
        walk(&pages.pages[page].fragment, &mut out);
        out
    };
    assert_eq!(borders(&whole, 0), 0);
    assert!(borders(&whole, 1) > 0);
    assert!(borders(&broken, 0) > 0 && borders(&broken, 1) > 0);
    // What follows comes sooner when the space is broken.
    assert_eq!(find(&whole, "After").0, find(&broken, "After").0);
    assert!(find(&broken, "After").1 < find(&whole, "After").1);
}

/// The text of the paragraphs that the spaces of a variant measure.
fn measured_text(world: &World, variant: &str) -> Vec<String> {
    fn walk(block: &folio::TextBlock, text: &folio::Text, out: &mut Vec<String>) {
        for child in &block.children {
            match child {
                folio::TextFlow::Paragraph(i) => {
                    out.push(text.paragraphs[*i].shaped.text().replace('\u{a0}', " "))
                }
                folio::TextFlow::Block(b) => walk(b, text, out),
            }
        }
    }
    let text = folio::text(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap();
    let mut out = Vec::new();
    for space in &text.spaces {
        if let Some(block) = &space.measure {
            walk(block, &text, &mut out);
        }
    }
    out
}

#[test]
fn hidden_content_is_measured_with_its_numbers_and_references() {
    let world = world(
        "figure.h { display: hidden; }\nheading { numbering: \"1.\"; }\n",
        "figure.h { display: block; }\n",
        "= Forces <forces>\n\n#figure(caption: [A ball.])[Ball.] <ball>\n\n\
         #figure(class: \"h\")[See @ball and @forces, and @step.\n\n\
         + First step. <step>\n+ Second step.\n\n#figure(caption: [Inner.])[In.] <inner>\n\n\
         As @inner shows.]\n\n#figure(caption: [After.])[Last.] <last>\n\nClosing @last.\n",
    );
    let measured = measured_text(&world, "exam");
    let joined = measured.join(" | ");
    assert!(!joined.contains("??"), "{joined}");
    assert!(
        joined.contains("See Figure 1 and Section 1, and 1."),
        "{joined}"
    );
    // The hidden figure is still counted (2), and the one inside it is
    // numbered after it, as where it is shown.
    assert!(joined.contains("2. Second step"), "{joined}");
    assert!(joined.contains("As Figure 3 shows"), "{joined}");
    // What the paper shows after it is numbered without what it only
    // measures.
    let exam = pages(&world, "exam");
    let text = shown(&exam).replace('\u{a0}', " ");
    assert!(text.contains("Figure 3"), "{text}");
    assert!(!text.contains("Figure 4"), "{text}");
    // And nothing reports on what only measures.
    let found = diagnostics(&world, "exam");
    assert!(found.is_empty(), "{found:?}");
    // The paper and the mark scheme still break alike.
    assert_same(
        find(&exam, "Last"),
        find(&pages(&world, "markscheme"), "Last"),
        "Last",
    );
}
