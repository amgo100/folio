//! Footnotes (SPEC §7.8) through the whole pipeline: the marker at the
//! anchor, the note at the foot of the anchor's page, notes that continue
//! on the next page, numbering through the document, per section and per
//! page, symbols, the warning for a note that cannot be placed, the page
//! cache, and the PDF structure.

use folio::{Diagnostic, Fragment, FragmentItem, Page, Pages, Resolution, Target, Text, World};
use folio_component::Location;

/// A6-sized pages: a body of about 81mm by 124mm.
const PAGE: &str = "page { page-size: 105mm 148mm; margin-top: 12mm; margin-bottom: 12mm; \
                    margin-inner: 12mm; margin-outer: 12mm; }\n";

fn world(style: &str, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("styles/main.fsty", &format!("{PAGE}{style}"))
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn pages(world: &World) -> Pages {
    folio::pages(world, Target::Pdf, None).unwrap().unwrap()
}

fn text(world: &World) -> Text {
    folio::text(world, Target::Pdf, None).unwrap().unwrap()
}

fn resolution(world: &World) -> Resolution {
    folio::resolve(world, Target::Pdf, None).unwrap().unwrap()
}

fn diagnostics(world: &World) -> Vec<Diagnostic> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .to_vec()
}

const WORDS: &[&str] = &[
    "force", "energy", "momentum", "the", "of", "a", "field", "charge", "is", "and", "wave",
    "particle", "mass", "velocity", "in", "gravity", "with", "light", "to", "measured",
];

/// A sentence of `words` words, the same for the same seed.
fn prose(seed: usize, words: usize) -> String {
    let mut out = String::new();
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for i in 0..words {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let word = WORDS[(state >> 33) % WORDS.len()];
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.push('.');
    out
}

/// Paragraphs of prose, each with a footnote of `note` words after its
/// `at`th word.
fn noted(paragraphs: usize, words: usize, at: usize, note: usize) -> String {
    let mut out = String::new();
    for p in 0..paragraphs {
        let body = prose(p, words);
        let mut parts: Vec<&str> = body.split(' ').collect();
        let tail = parts.split_off(at.min(parts.len()));
        let footnote = format!("#footnote[{}]", prose(100 + p, note));
        out.push_str(&format!(
            "{}{footnote} {}\n\n",
            parts.join(" "),
            tail.join(" ")
        ));
    }
    out
}

/// Whether a fragment or a fragment inside it is a box of `location`.
fn shows(fragment: &Fragment, location: Location) -> bool {
    fragment.location() == location
        || fragment.items.iter().any(|(_, item)| match item {
            FragmentItem::Group(group) => shows(group, location),
            _ => false,
        })
}

/// The pages, by index, with a box of `location`.
fn pages_showing(pages: &Pages, location: Location) -> Vec<usize> {
    pages
        .pages
        .iter()
        .enumerate()
        .filter(|(_, page)| shows(&page.fragment, location))
        .map(|(i, _)| i)
        .collect()
}

/// The footnotes of a build: each note's location and the page its
/// anchor's line is on.
fn anchors(world: &World) -> Vec<(Location, usize)> {
    let text = text(world);
    let lines = folio::lines(world, Target::Pdf, None).unwrap().unwrap();
    let pages = pages(world);
    let mut out = Vec::new();
    for (p, paragraph) in text.paragraphs.iter().enumerate() {
        for anchor in &paragraph.notes {
            let line = lines.paragraphs[p]
                .lines
                .iter()
                .position(|l| l.range.contains(&anchor.range.start))
                .expect("the anchor is on a line");
            let page = pages
                .pages
                .iter()
                .position(|page: &Page| {
                    page.lines
                        .iter()
                        .any(|l| l.paragraph == p && l.line == line)
                })
                .expect("the anchor's line is on a page");
            out.push((text.notes[anchor.note].location, page));
        }
    }
    out
}

/// The numbers of the footnotes, in document order.
fn numbers(world: &World) -> Vec<String> {
    let resolution = resolution(world);
    resolution
        .footnotes
        .iter()
        .map(|f| {
            resolution
                .number(folio::ResolveTarget::element(f.location))
                .and_then(|n| n.number.clone())
                .map(|n| n.to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn a_note_is_set_at_the_foot_of_its_anchors_page() {
    let world = world(
        "",
        "A ball is thrown.#footnote[Air resistance is negligible.] It falls.\n",
    );
    let text = text(&world);
    // The marker stands at the anchor; the note's text is a paragraph of
    // its own, after its number.
    assert_eq!(
        text.paragraphs[0].shaped.text(),
        "A ball is thrown.1 It falls."
    );
    assert_eq!(
        text.paragraphs[1].shaped.text(),
        "1\u{a0}Air resistance is negligible."
    );
    assert_eq!(text.notes.len(), 1);
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let note = text.notes[0].location;
    assert_eq!(pages.pages_of(&[note]), [Some(0)]);
    // At the foot of the body: below the paragraph, near the bottom.
    let body = match &pages.pages[0].fragment.items[0].1 {
        FragmentItem::Group(body) => body,
        _ => panic!("a page holds its body"),
    };
    let mut tops = Vec::new();
    find(body, note, folio::Point::zero(), &mut tops);
    let top = tops.first().expect("the note has a box").y;
    assert!(top.to_pt() > body.size.height.to_pt() * 0.8, "{top:?}");
    assert!(diagnostics(&world).is_empty(), "{:?}", diagnostics(&world));
}

/// Where the boxes of `location` are, from the fragment's top-left.
fn find(fragment: &Fragment, location: Location, at: folio::Point, out: &mut Vec<folio::Point>) {
    for (pos, item) in &fragment.items {
        if let FragmentItem::Group(group) = item {
            let pos = folio::Point::new(at.x + pos.x, at.y + pos.y);
            if group.location() == location {
                out.push(pos);
            }
            find(group, location, pos, out);
        }
    }
}

#[test]
fn a_note_always_starts_on_the_page_of_its_anchor() {
    for (words, at, note) in [(40, 30, 20), (60, 50, 40), (30, 5, 60), (80, 70, 10)] {
        let world = world("", &noted(12, words, at, note));
        let pages = pages(&world);
        assert!(pages.pages.len() >= 3);
        for (location, page) in anchors(&world) {
            let shown = pages_showing(&pages, location);
            assert_eq!(
                shown.first(),
                Some(&page),
                "a note starts on its anchor's page ({words}, {at}, {note})"
            );
            // A note continues only onto the next pages.
            for pair in shown.windows(2) {
                assert_eq!(pair[1], pair[0] + 1);
            }
        }
        let diags = diagnostics(&world);
        assert!(
            !diags.iter().any(|d| d.message.contains("footnote")),
            "{diags:?}"
        );
    }
}

#[test]
fn a_long_note_continues_on_the_next_page() {
    let source = format!(
        "{}\n\n{}#footnote[{}] {}\n\n{}\n",
        prose(1, 120),
        prose(2, 20),
        prose(3, 220),
        prose(4, 10),
        prose(5, 60)
    );
    let world = world("", &source);
    let pages = pages(&world);
    let text = text(&world);
    let note = text.notes[0].location;
    let shown = pages_showing(&pages, note);
    assert_eq!(shown.len(), 2, "the note is split across two pages");
    let (location, page) = anchors(&world)[0];
    assert_eq!(location, note);
    assert_eq!(shown[0], page);
    assert_eq!(shown[1], page + 1);
    // Only the warnings shown on request, of the breaks the page took.
    let diags = diagnostics(&world);
    assert!(
        diags.iter().all(|d| d
            .code
            .as_deref()
            .is_some_and(|c| folio::layout::DETAIL_CODES.contains(&c))),
        "{diags:?}"
    );
}

#[test]
fn notes_are_numbered_through_the_document() {
    let world = world("", &noted(12, 40, 30, 20));
    let numbers = numbers(&world);
    let expected: Vec<String> = (1..=12).map(|n| n.to_string()).collect();
    assert_eq!(numbers, expected);
}

#[test]
fn page_counter_reset_numbers_notes_from_1_on_every_page() {
    let world = world(
        "page { counter-reset: footnote; }\n",
        &noted(12, 40, 30, 20),
    );
    let pages = pages(&world);
    let resolution = resolution(&world);
    assert!(resolution.footnotes_per_page);
    let numbers = numbers(&world);
    // Each note is numbered among those of its page.
    let mut expected = Vec::new();
    let mut previous = None;
    let mut n = 0;
    for footnote in &resolution.footnotes {
        let page = pages.pages_of(&[footnote.location])[0];
        n = if page == previous { n + 1 } else { 1 };
        previous = page;
        expected.push(n.to_string());
    }
    assert_eq!(numbers, expected);
    assert!(
        numbers.iter().filter(|n| *n == "1").count() >= 3,
        "{numbers:?}"
    );
    // The marker at the anchor shows the number too.
    let text = text(&world);
    for paragraph in &text.paragraphs {
        for anchor in &paragraph.notes {
            let shown = &paragraph.shaped.text()[anchor.range.clone()];
            assert!(numbers.contains(&shown.to_string()), "{shown}");
        }
    }
    let diags = diagnostics(&world);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_heading_restarts_the_notes_of_its_section() {
    let source = format!(
        "= One\n\n{}\n\n= Two\n\n{}",
        noted(2, 20, 10, 5),
        noted(3, 20, 10, 5)
    );
    let world = world("heading { counter-reset: footnote; }\n", &source);
    assert_eq!(numbers(&world), ["1", "2", "1", "2", "3"]);
}

#[test]
fn symbols_number_notes() {
    let world = world("footnote { numbering: \"*\"; }\n", &noted(3, 20, 10, 5));
    assert_eq!(numbers(&world), ["*", "†", "‡"]);
    let text = text(&world);
    assert!(text.paragraphs[0].shaped.text().contains('*'));
}

#[test]
fn page_counter_reset_of_another_counter_warns() {
    let world = world("page { counter-reset: figure; }\n", "Text.\n");
    let diags = diagnostics(&world);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("only footnotes are numbered per page")),
        "{diags:?}"
    );
}

#[test]
fn a_note_that_cannot_fit_warns_like_page_overflow() {
    // A body only a few lines high: the anchor's line and the separator
    // leave no room for the note's first line.
    let world = world(
        "page { page-size: 105mm 21mm; margin-top: 5mm; margin-bottom: 5mm; }\n",
        "Text#footnote[A note.] here.\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let diags = diagnostics(&world);
    let overflow: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("page-overflow"))
        .collect();
    assert_eq!(overflow.len(), 1, "{diags:?}");
    assert!(overflow[0].message.contains("footnote"));
}

#[test]
fn a_footnote_inside_a_footnote_warns() {
    let world = world("", "Text#footnote[Outer#footnote[Inner.].] here.\n");
    let diags = diagnostics(&world);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("footnote inside a footnote")),
        "{diags:?}"
    );
}

#[test]
fn pages_with_notes_come_from_the_cache() {
    let source = noted(16, 40, 30, 20);
    let mut world = world("", &source);
    let before = pages(&world);
    assert!(before.pages.len() >= 5);
    let body = |p: &Page| match &p.fragment.items[0].1 {
        FragmentItem::Group(body) => body.items.as_ptr(),
        _ => panic!("a page holds its body"),
    };
    // An edit to the last paragraph leaves the pages before it alone.
    let last = prose(15, 40);
    let tail = last.split(' ').next_back().unwrap();
    let edited = source.replacen(tail, "changed.", 1);
    let edited = if edited == source {
        format!("{source}More.\n")
    } else {
        edited
    };
    world.set_source("main.fol", &edited).unwrap();
    let after = pages(&world);
    let unchanged = before
        .pages
        .iter()
        .zip(&after.pages)
        .take_while(|(a, b)| body(a) == body(b))
        .count();
    assert!(
        unchanged >= before.pages.len() - 2,
        "{unchanged} of {} pages reused",
        before.pages.len()
    );
    // An edit to a note lays out its page again.
    let note = prose(100, 20);
    let first_word = note.split(' ').next().unwrap();
    let edited = source.replacen(&format!("#footnote[{first_word}"), "#footnote[Edited", 1);
    world.set_source("main.fol", &edited).unwrap();
    let again = pages(&world);
    assert_ne!(body(&before.pages[0]), body(&again.pages[0]));
    assert_eq!(again.hashes.len(), before.hashes.len());
    assert_ne!(again.hashes[0], before.hashes[0]);
}

#[test]
fn the_pdf_tags_each_note() {
    let world = world(
        "",
        "#doc(title: [Notes])\n\nA ball is thrown.#footnote[Air resistance is negligible.] It \
         falls.\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let pdf = compiled.output.clone().expect("a PDF");
    let bytes = String::from_utf8_lossy(&pdf);
    assert!(bytes.contains("/S/Note"), "the PDF has no Note");
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    assert!(
        bytes.contains("<pdfuaid:part>1</pdfuaid:part>"),
        "the PDF claims PDF/UA-1"
    );
}

/// The glyph runs of a fragment and those inside it, with where they
/// start from the fragment's top-left, and their text.
fn runs(fragment: &Fragment, at: folio::Point, out: &mut Vec<(folio::Point, String)>) {
    for (pos, item) in &fragment.items {
        let pos = folio::Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => runs(group, pos, out),
            FragmentItem::Text(run) => out.push((pos, run.text.to_string())),
            _ => {}
        }
    }
}

#[test]
fn baseline_shift_raises_text_and_makes_its_line_taller() {
    let plain = world("", "Up *here* and on.\n");
    let raised = world("strong { baseline-shift: 3pt; }\n", "Up *here* and on.\n");
    let line = |w: &World| {
        folio::lines(w, Target::Pdf, None)
            .unwrap()
            .unwrap()
            .paragraphs[0]
            .lines[0]
            .clone()
    };
    let (a, b) = (line(&plain), line(&raised));
    assert!(((b.ascent - a.ascent).to_pt() - 3.0).abs() < 1e-6);
    assert!(b.descent <= a.descent);
    let mut found = Vec::new();
    runs(
        &pages(&raised).pages[0].fragment,
        folio::Point::zero(),
        &mut found,
    );
    let y = |text: &str| {
        found
            .iter()
            .find(|(_, t)| t.contains(text))
            .unwrap_or_else(|| panic!("no run of {text:?} in {found:?}"))
            .0
            .y
    };
    assert!(((y("Up") - y("here")).to_pt() - 3.0).abs() < 1e-6);
    // A negative shift lowers it.
    let lowered = world("strong { baseline-shift: -2pt; }\n", "Up *here* and on.\n");
    let c = line(&lowered);
    assert!(((c.descent - a.descent).to_pt() - 2.0).abs() < 1e-6);
    assert!(diagnostics(&raised).is_empty());
}

#[test]
fn baseline_shift_is_a_length_within_the_limit() {
    let world = world("strong { baseline-shift: 2e6pt; }\n", "Up *here*.\n");
    let diags = diagnostics(&world);
    assert!(
        diags.iter().any(|d| d.severity == folio::Severity::Error
            && d.message.contains("`baseline-shift`")
            && d.message.contains("beyond ±1000000pt")),
        "{diags:?}"
    );
    let world2 = world_with("strong { baseline-shift: red; }\n");
    let diags = diagnostics(&world2);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("wrong value for `baseline-shift`")),
        "{diags:?}"
    );
}

fn world_with(style: &str) -> World {
    world(style, "Up *here*.\n")
}

#[test]
fn the_footnote_markers_are_raised_by_default() {
    let world = world("", "A ball.#footnote[A note.] It falls.\n");
    let mut found = Vec::new();
    runs(
        &pages(&world).pages[0].fragment,
        folio::Point::zero(),
        &mut found,
    );
    let at = |text: &str| {
        found
            .iter()
            .find(|(_, t)| t == text)
            .unwrap_or_else(|| panic!("no run {text:?} in {found:?}"))
            .0
    };
    let marker = at("1");
    let before = found
        .iter()
        .find(|(_, t)| t.starts_with("A ball"))
        .unwrap()
        .0;
    assert!(marker.y < before.y, "the marker is raised: {found:?}");
}

#[test]
fn a_continued_note_starts_with_its_mark() {
    for (lang, mark) in [("en", "(cont.)"), ("pl", "(cd.)")] {
        let source = format!(
            "{}\n\n{}#footnote[{}] {}\n\n{}\n",
            prose(1, 120),
            prose(2, 20),
            prose(3, 220),
            prose(4, 10),
            prose(5, 60)
        );
        let world = world(&format!("doc {{ lang: \"{lang}\"; }}\n"), &source);
        let pages = pages(&world);
        let note = text(&world).notes[0].location;
        let shown = pages_showing(&pages, note);
        assert_eq!(shown.len(), 2, "{lang}");
        let on = |page: usize| {
            let mut found = Vec::new();
            runs(
                &pages.pages[page].fragment,
                folio::Point::zero(),
                &mut found,
            );
            found.iter().filter(|(_, t)| t == mark).count()
        };
        assert_eq!(on(shown[0]), 0, "{lang}");
        assert_eq!(on(shown[1]), 1, "{lang}");
    }
}
