//! The page builder (SPEC §7.1, §7.2) through the whole pipeline: a
//! document of many pages laid out deterministically, keeps, widows and
//! orphans as costs, forced breaks, impossible constraints, the box model
//! of blocks, and the page cache across edits.

use folio::layout::{PageCache, PageStats, Regions};
use folio::{
    Abs, Diagnostic, Fragment, FragmentItem, LineRef, Page, Pages, Point, Severity, SourceRef,
    Target, Text, World,
};

/// A5 with 15mm margins: a body of 118mm by 180mm.
const PAGE: &str = "page { page-size: A5; margin-top: 15mm; margin-bottom: 15mm; \
                    margin-inner: 15mm; margin-outer: 15mm; }\n\
                    doc { font: \"Source Serif 4\"; }\n";

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

fn diagnostics(world: &World) -> Vec<Diagnostic> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .to_vec()
}

const WORDS: &[&str] = &[
    "force",
    "energy",
    "momentum",
    "the",
    "of",
    "a",
    "field",
    "charge",
    "is",
    "and",
    "wave",
    "particle",
    "mass",
    "velocity",
    "in",
    "acceleration",
    "gravity",
    "with",
    "light",
    "to",
    "measured",
    "constant",
    "frequency",
    "an",
    "electron",
    "current",
    "potential",
    "work",
];

/// A paragraph of `words` words, the same for the same seed.
fn prose(seed: usize, words: usize) -> String {
    let mut out = String::new();
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for i in 0..words {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let word = WORDS[(state >> 33) % WORDS.len()];
        if i == 0 {
            let mut chars = word.chars();
            let first = chars.next().unwrap().to_uppercase();
            out.extend(first);
            out.extend(chars);
        } else {
            out.push(' ');
            out.push_str(word);
        }
    }
    out.push('.');
    out
}

/// A document of sections with headings and paragraphs of varied lengths.
fn document(sections: usize) -> String {
    let mut out = String::new();
    for s in 0..sections {
        out.push_str(&format!("= Heading {s}\n\n"));
        for p in 0..4 {
            out.push_str(&prose(s * 10 + p, 30 + (s * 7 + p * 13) % 60));
            out.push_str("\n\n");
        }
    }
    out
}

/// Pages without the references of their glyphs, shapes and boxes to
/// source nodes.
fn strip(pages: &Pages) -> Pages {
    fn fragment(f: &Fragment) -> Fragment {
        let mut out = f.clone();
        out.source = SourceRef::DETACHED;
        out.items = f
            .items
            .iter()
            .map(|(pos, item)| {
                let item = match item {
                    FragmentItem::Group(g) => FragmentItem::Group(fragment(g)),
                    FragmentItem::Text(run) => {
                        let mut run = run.clone();
                        run.glyphs = run
                            .glyphs
                            .iter()
                            .map(|g| {
                                let mut g = g.clone();
                                g.source = (SourceRef::DETACHED, 0..0);
                                g
                            })
                            .collect();
                        FragmentItem::Text(run)
                    }
                    FragmentItem::Shape { shape, .. } => FragmentItem::Shape {
                        shape: shape.clone(),
                        source: SourceRef::DETACHED,
                    },
                    FragmentItem::Object(object) => FragmentItem::Object(object.clone()),
                    FragmentItem::Image(image) => FragmentItem::Image(image.clone()),
                };
                (*pos, item)
            })
            .collect();
        out
    }
    let mut out = pages.clone();
    out.pages = pages
        .pages
        .iter()
        .map(|p| Page {
            fragment: fragment(&p.fragment),
            lines: p.lines.clone(),
        })
        .collect();
    out
}

/// The glyph runs of a fragment and its descendants, with their origins on
/// the page.
fn runs(fragment: &Fragment, at: Point, out: &mut Vec<(Point, String)>) {
    for (pos, item) in &fragment.items {
        let p = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => runs(group, p, out),
            FragmentItem::Text(run) => out.push((p, run.text.to_string())),
            FragmentItem::Shape { .. } | FragmentItem::Object(_) | FragmentItem::Image(_) => {}
        }
    }
}

fn page_runs(page: &Page) -> Vec<(Point, String)> {
    let mut out = Vec::new();
    runs(&page.fragment, Point::zero(), &mut out);
    out
}

/// The paragraphs whose text starts with `prefix`.
fn paragraphs_starting(text: &Text, prefix: &str) -> Vec<usize> {
    text.paragraphs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.shaped.text().starts_with(prefix))
        .map(|(i, _)| i)
        .collect()
}

fn warnings_containing(diags: &[Diagnostic], needle: &str) -> usize {
    diags
        .iter()
        .filter(|d| d.severity == Severity::Warning && d.message.contains(needle))
        .count()
}

/// For each paragraph split across pages, how many of its lines end one
/// page and how many start the next.
fn splits(pages: &Pages) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for pair in pages.pages.windows(2) {
        let (Some(last), Some(first)) = (pair[0].lines.last(), pair[1].lines.first()) else {
            continue;
        };
        if last.paragraph == first.paragraph {
            let p = last.paragraph;
            let count = |page: &Page| page.lines.iter().filter(|l| l.paragraph == p).count();
            out.push((p, count(&pair[0]), count(&pair[1])));
        }
    }
    out
}

#[test]
fn ten_pages_lay_out_deterministically() {
    let source = document(14);
    let first = world(
        "heading { keep-with-next: true; space-before: 6pt; }\n",
        &source,
    );
    let diags = diagnostics(&first);
    assert!(
        diags.iter().all(|d| d.severity != Severity::Error),
        "{diags:?}"
    );
    let a = pages(&first);
    assert!(a.pages.len() >= 10, "{} pages", a.pages.len());

    // The same input gives the same pages in a new world, and the same
    // layout after an edit is undone; the reparsed source numbers its nodes
    // anew, so only the glyphs' references to them differ.
    let second = world(
        "heading { keep-with-next: true; space-before: 6pt; }\n",
        &source,
    );
    assert_eq!(pages(&second), a);
    let mut third = world(
        "heading { keep-with-next: true; space-before: 6pt; }\n",
        &source,
    );
    third
        .set_source("main.fol", &source.replace("Heading 3", "Heading three"))
        .unwrap();
    assert_ne!(strip(&pages(&third)), strip(&a));
    third.set_source("main.fol", &source).unwrap();
    assert_eq!(strip(&pages(&third)), strip(&a));

    // Every line is on exactly one page, in order.
    let text = text(&first);
    let all: Vec<LineRef> = a
        .pages
        .iter()
        .flat_map(|p| p.lines.iter().copied())
        .collect();
    let lines = folio::lines(&first, Target::Pdf, None).unwrap().unwrap();
    let expected: Vec<LineRef> = (0..text.paragraphs.len())
        .flat_map(|p| {
            (0..lines.paragraphs[p].lines.len()).map(move |line| LineRef { paragraph: p, line })
        })
        .collect();
    assert_eq!(all, expected);

    // Every page is the master's size, and its text lies in the body.
    let master = a.master;
    assert!((master.size.width - Abs::mm(148.0)).to_pt().abs() < 1e-9);
    let body = master.body;
    for page in &a.pages {
        assert_eq!(page.fragment.size, master.size);
        let runs = page_runs(page);
        assert!(!runs.is_empty());
        for (pos, text) in &runs {
            assert!(pos.x >= body.origin.x - Abs::pt(0.01), "{text}: {pos:?}");
            assert!(pos.x <= body.origin.x + body.size.width, "{text}: {pos:?}");
            assert!(pos.y > body.origin.y, "{text}: {pos:?}");
            assert!(pos.y <= body.origin.y + body.size.height, "{text}: {pos:?}");
        }
        // Baselines go down the page.
        for pair in runs.windows(2) {
            assert!(pair[1].0.y >= pair[0].0.y);
        }
    }
    // Pages are full but for the last few lines' worth: no page ends
    // early without a reason.
    for page in &a.pages[..a.pages.len() - 1] {
        let body_fragment = match &page.fragment.items[0].1 {
            FragmentItem::Group(body) => body,
            _ => panic!("a page holds its body"),
        };
        assert!(
            body_fragment.last_baseline > body.size.height * 0.75,
            "{:?}",
            body_fragment.last_baseline
        );
    }
}

#[test]
fn headings_are_kept_with_what_follows() {
    // Vary the text before a heading so that the heading falls at every
    // height of the page, the foot among them.
    let mut at_foot_without_keep = 0;
    for filler in 0..24 {
        let mut source = String::new();
        for i in 0..filler {
            source.push_str(&format!("Filler line {i}.\n\n"));
        }
        source.push_str(&document(3));
        for keep in [true, false] {
            let style = if keep {
                "heading { keep-with-next: true; }\nparagraph { space-after: 3pt; }\n"
            } else {
                // The base stylesheet keeps headings with what follows.
                "heading { keep-with-next: false; }\nparagraph { space-after: 3pt; }\n"
            };
            let world = world(style, &source);
            let text = text(&world);
            let headings = paragraphs_starting(&text, "Heading");
            let pages = pages(&world);
            for page in &pages.pages[..pages.pages.len() - 1] {
                let last = page.lines.last().unwrap();
                let at_foot = headings.contains(&last.paragraph);
                if keep {
                    assert!(!at_foot, "filler {filler}: a heading ends a page");
                } else if at_foot {
                    at_foot_without_keep += 1;
                }
            }
            if keep {
                let diags = diagnostics(&world);
                assert_eq!(
                    warnings_containing(&diags, "keep-with-next"),
                    0,
                    "{diags:?}"
                );
            }
        }
    }
    assert!(
        at_foot_without_keep > 0,
        "the test never puts a heading at a foot"
    );
}

#[test]
fn widows_and_orphans_are_avoided() {
    let mut split_lone = 0;
    let mut split = 0;
    for filler in 0..20 {
        let mut source = String::new();
        for i in 0..filler {
            source.push_str(&format!("Filler line {i}.\n\n"));
        }
        for p in 0..12 {
            source.push_str(&prose(p, 45));
            source.push_str("\n\n");
        }
        for (widows, orphans) in [(2, 2), (1, 1), (3, 3)] {
            let style = format!("paragraph {{ widows: {widows}; orphans: {orphans}; }}\n");
            let world = world(&style, &source);
            let pages = pages(&world);
            for (_, foot, head) in splits(&pages) {
                split += 1;
                if (widows, orphans) == (1, 1) {
                    if foot == 1 || head == 1 {
                        split_lone += 1;
                    }
                } else {
                    assert!(foot >= orphans, "filler {filler}: {foot} lines at the foot");
                    assert!(head >= widows, "filler {filler}: {head} lines at the head");
                }
            }
            if widows > 1 {
                let diags = diagnostics(&world);
                assert_eq!(warnings_containing(&diags, "widows"), 0, "{diags:?}");
                assert_eq!(warnings_containing(&diags, "orphans"), 0, "{diags:?}");
            }
        }
    }
    assert!(split > 0, "no paragraph is split across pages");
    assert!(
        split_lone > 0,
        "without control, some split leaves a lone line"
    );
}

#[test]
fn a_block_kept_together_moves_to_the_next_page() {
    // The section starts in the lower half of the first page and would
    // run onto the second.
    let mut source = String::new();
    for p in 0..3 {
        source.push_str(&prose(p, 50));
        source.push_str("\n\n");
    }
    source.push_str("= Kept\n\n");
    for p in 0..3 {
        source.push_str(&prose(100 + p, 40));
        source.push_str("\n\n");
    }
    for keep in [false, true] {
        let style = format!("section {{ keep-together: {keep}; }}\n");
        let world = world(&style, &source);
        let text = text(&world);
        let heading = paragraphs_starting(&text, "Kept")[0];
        let pages = pages(&world);
        let in_section = |l: &&LineRef| l.paragraph >= heading;
        let holding: Vec<usize> = pages
            .pages
            .iter()
            .map(|p| p.lines.iter().filter(in_section).count())
            .filter(|&n| n > 0)
            .collect();
        if keep {
            // All of it on one page, which starts with it.
            assert_eq!(holding.len(), 1, "{holding:?}");
            let page = pages
                .pages
                .iter()
                .find(|p| p.lines.iter().any(|l| l.paragraph == heading))
                .unwrap();
            assert_eq!(page.lines[0].paragraph, heading);
            let diags = diagnostics(&world);
            assert_eq!(warnings_containing(&diags, "keep-together"), 0, "{diags:?}");
        } else {
            assert_eq!(
                holding.len(),
                2,
                "without the keep it is split: {holding:?}"
            );
        }
    }
}

#[test]
fn an_impossible_keep_gives_the_least_bad_page_and_a_warning() {
    let mut source = String::from("Before.\n\n= Too long\n\n");
    for p in 0..14 {
        source.push_str(&prose(p, 50));
        source.push_str("\n\n");
    }
    let world = world("section { keep-together: true; }\n", &source);
    let pages = pages(&world);
    assert!(pages.pages.len() >= 3);
    // The section starts a page, then breaks where it must: every page is
    // full and nothing overflows.
    let text = text(&world);
    let heading = paragraphs_starting(&text, "Too long")[0];
    assert_eq!(pages.pages[1].lines[0].paragraph, heading);
    let body = pages.master.body;
    for page in &pages.pages {
        for (pos, _) in page_runs(page) {
            assert!(pos.y <= body.origin.y + body.size.height);
        }
    }
    let diags = diagnostics(&world);
    assert!(
        warnings_containing(&diags, "despite `keep-together`") >= 1,
        "{diags:?}"
    );
    assert_eq!(warnings_containing(&diags, "does not fit"), 0, "{diags:?}");
}

#[test]
fn break_before_starts_a_page() {
    let world = world(
        "heading { break-before: page; }\n",
        "Intro.\n\n= One\n\nFirst.\n\n= Two\n\nSecond.\n",
    );
    let text = text(&world);
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 3);
    let heads: Vec<String> = pages
        .pages
        .iter()
        .map(|p| {
            text.paragraphs[p.lines[0].paragraph]
                .shaped
                .text()
                .to_string()
        })
        .collect();
    assert_eq!(heads, ["Intro.", "One", "Two"]);
}

#[test]
fn spaces_collapse_and_are_dropped_at_page_tops() {
    let world = world(
        "heading { space-before: 20pt; space-after: 4pt; }\n\
         paragraph { space-before: 10pt; }\n\
         heading.big { break-before: page; }\n",
        "Intro.\n\n= Heading\n\nText.\n\n#heading(class: \"big\")[Big]\n\nMore.\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 2);
    let runs = page_runs(&pages.pages[0]);
    let ys: Vec<f64> = runs.iter().map(|(p, _)| p.y.to_pt()).collect();
    assert_eq!(runs.len(), 3, "{runs:?}");
    // Intro to the heading: max(20, 10) plus a leading; the heading to its
    // text: max(4, 10) plus a leading.
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let leading = 0.65 * 11.0;
    let line = |i: usize| &lines.paragraphs[i].lines[0];
    let gap = |a: usize, b: usize| (line(a).descent + line(b).ascent).to_pt() + leading;
    assert!((ys[1] - ys[0] - gap(0, 1) - 20.0).abs() < 1e-6, "{ys:?}");
    assert!((ys[2] - ys[1] - gap(1, 2) - 10.0).abs() < 1e-6, "{ys:?}");
    // On the next page the heading's space before is dropped: its first
    // baseline is its ascent below the body's top.
    let next = page_runs(&pages.pages[1]);
    let top = pages.master.body.origin.y;
    let big = lines.paragraphs[3].lines[0].ascent;
    assert!((next[0].0.y - top - big).to_pt().abs() < 1e-6, "{next:?}");
}

/// Shapes in a fragment and its descendants, with the size of the
/// fragment holding them.
fn shapes(fragment: &Fragment, out: &mut Vec<(Abs, Abs)>) {
    for (_, item) in &fragment.items {
        match item {
            FragmentItem::Group(group) => shapes(group, out),
            FragmentItem::Shape { .. } => out.push((fragment.size.width, fragment.size.height)),
            FragmentItem::Text(_) | FragmentItem::Object(_) | FragmentItem::Image(_) => {}
        }
    }
}

#[test]
fn padding_borders_and_backgrounds_box_a_block() {
    let source = format!("Plain {}\n\n#figure[{}]\n", prose(1, 40), prose(2, 40));
    let world = world(
        "figure { padding: 6pt 10pt; border: 2pt solid #000000; background: #eeeeee; }\n",
        &source,
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let body = lines.width;
    // The figure's lines are narrower by its padding and borders, and
    // start inside them.
    assert_eq!(lines.slots[0].width, body);
    let inset = Abs::pt(10.0 + 2.0);
    assert!((lines.slots[1].width - (body - inset * 2.0)).to_pt().abs() < 1e-9);
    assert!((lines.slots[1].offset - inset).to_pt().abs() < 1e-9);
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let mut found = Vec::new();
    shapes(&pages.pages[0].fragment, &mut found);
    // A background and four borders, on a box of the body's width.
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(found.iter().all(|(w, _)| *w == body));
    let runs = page_runs(&pages.pages[0]);
    let x0 = pages.master.body.origin.x;
    let figure_runs: Vec<_> = runs
        .iter()
        .filter(|(p, _)| p.x >= x0 + inset - Abs::pt(0.01))
        .collect();
    assert!(!figure_runs.is_empty());
}

#[test]
fn a_block_broken_across_pages_is_sliced() {
    let mut body = String::new();
    for p in 0..12 {
        body.push_str(&prose(p, 50));
        body.push_str("\n\n");
    }
    let world = world("figure { border: 1pt; }\n", &format!("#figure[\n{body}]\n"));
    let pages = pages(&world);
    assert!(pages.pages.len() >= 2);
    let mut first = Vec::new();
    shapes(&pages.pages[0].fragment, &mut first);
    let mut last = Vec::new();
    shapes(&pages.pages[pages.pages.len() - 1].fragment, &mut last);
    // The first part has no bottom border, the last no top one.
    assert_eq!(first.len(), 3, "{first:?}");
    assert_eq!(last.len(), 3, "{last:?}");
}

#[test]
fn unchanged_pages_come_from_the_cache() {
    let source = document(14);
    let mut world = world("", &source);
    let before = pages(&world);
    assert!(before.pages.len() >= 10);
    // An edit to the last section leaves the pages before it unchanged:
    // they are taken from the cache, not assembled again.
    let edited = source.replace("Heading 13", "Heading thirteen");
    world.set_source("main.fol", &edited).unwrap();
    let after = pages(&world);
    // The page itself is dressed as a box of the document; its body's
    // content is what the cache shares.
    let body = |p: &Page| match &p.fragment.items[0].1 {
        FragmentItem::Group(body) => body.items.as_ptr(),
        _ => panic!("a page holds its body"),
    };
    let shared = |a: &Page, b: &Page| body(a) == body(b);
    let unchanged = before
        .pages
        .iter()
        .zip(&after.pages)
        .take_while(|(a, b)| shared(a, b))
        .count();
    assert!(
        unchanged >= before.pages.len() - 3,
        "{unchanged} pages reused"
    );
    assert!(!shared(
        before.pages.last().unwrap(),
        after.pages.last().unwrap()
    ));
}

#[test]
fn an_empty_document_has_one_page() {
    let world = world("", "");
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    assert!(pages.pages[0].lines.is_empty());
}

#[test]
fn content_taller_than_the_page_overflows_with_a_warning() {
    let world = world(
        "figure { padding: 300mm 0pt 0pt 0pt; }\n",
        "Before.\n\n#figure[Tall]\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 2);
    let diags = diagnostics(&world);
    assert_eq!(
        warnings_containing(&diags, "does not fit on a page"),
        1,
        "{diags:?}"
    );
}

/// The text each line draws, in order: its runs' texts joined, for
/// left-to-right text.
fn drawn_lines(page: &Page) -> Vec<String> {
    let mut lines: Vec<(Abs, String)> = Vec::new();
    for (pos, text) in page_runs(page) {
        match lines.last_mut() {
            Some((y, line)) if *y == pos.y => line.push_str(&text),
            _ => lines.push((pos.y, text)),
        }
    }
    lines.into_iter().map(|(_, t)| t).collect()
}

#[test]
fn every_line_draws_its_own_text() {
    // Narrow lines full of ligatures, hyphenated inside them.
    let words = "difficult office affluent baffling efficient sufficiently offline \
                 afflicted scaffolding traffic officials effortlessly ";
    let world = world(
        "page { margin-inner: 45mm; margin-outer: 45mm; }\n",
        &format!("{}\n", words.repeat(6)),
    );
    assert_lines_drawn(&world, false);
}

#[test]
fn polish_lines_repeat_their_hyphen() {
    let words = "biało-czerwona flaga i polsko-niemiecka granica oraz naukowo-techniczny \
                 postęp w badawczo-rozwojowym ośrodku ";
    let world = world(
        "page { margin-inner: 50mm; margin-outer: 50mm; }\ndoc { lang: \"pl\"; }\n",
        &format!("{}\n", words.repeat(8)),
    );
    assert_lines_drawn(&world, true);
}

/// Every line draws its text, the hyphen a break inserts after it, and the
/// one a Polish break repeats before it; some line is hyphenated, and with
/// `repeats`, some line repeats a hyphen.
fn assert_lines_drawn(world: &World, repeats: bool) {
    let text = text(world);
    let lines = folio::lines(world, Target::Pdf, None).unwrap().unwrap();
    let pages = pages(world);
    let mut hyphenated = 0;
    let mut repeated = 0;
    let mut drawn = Vec::new();
    for page in &pages.pages {
        drawn.extend(drawn_lines(page));
    }
    let paragraph = &text.paragraphs[0];
    let layout = &lines.paragraphs[0];
    assert_eq!(drawn.len(), layout.lines.len());
    for (line, drawn) in layout.lines.iter().zip(&drawn) {
        let mut expected = String::new();
        if line.repeated_hyphen {
            expected.push('-');
            repeated += 1;
        }
        expected.push_str(&paragraph.shaped.text()[line.range.start..line.content_end]);
        if line.hyphen {
            expected.push('-');
        }
        if line.hyphen || line.repeated_hyphen {
            hyphenated += 1;
        }
        assert_eq!(drawn, &expected);
    }
    assert!(hyphenated > 0, "no line is hyphenated");
    assert!(!repeats || repeated > 0, "no line repeats a hyphen");
}

#[test]
fn a_side_of_a_border_can_be_removed() {
    let world = world(
        "figure { border: 1pt; border-top: none; border-bottom: 2pt dashed; }\n",
        "#figure[Boxed.]\n",
    );
    let mut found = Vec::new();
    shapes(&pages(&world).pages[0].fragment, &mut found);
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn odd_and_even_page_breaks_pad_with_blank_pages() {
    let world = world(
        "heading { break-before: odd-page; }\nheading.even { break-before: even-page; }\n\
         heading.col { break-before: column; }\n",
        "Intro.\n\n= One\n\nFirst.\n\n#heading(class: \"col\")[Same page]\n\n\
         = Three\n\nThird.\n\n#heading(class: \"even\")[Four]\n",
    );
    let text = text(&world);
    let pages = pages(&world);
    let heads: Vec<Option<String>> = pages
        .pages
        .iter()
        .map(|p| {
            p.lines
                .first()
                .map(|l| text.paragraphs[l.paragraph].shaped.text().to_string())
        })
        .collect();
    let s = |t: &str| Some(t.to_string());
    // One starts page 3 after a blank page 2; a column break does nothing in
    // one column; Three is on page 5, and Four on page 6, which is even.
    assert_eq!(
        heads,
        [s("Intro."), None, s("One"), None, s("Three"), s("Four")]
    );
}

/// The pages of a world built by [`folio::layout::paginate`] in regions
/// that test hooks change, with caches of their own.
fn paginate(world: &World, regions: Regions<'_>, cache: &PageCache) -> Pages {
    let styled = folio::styles(world, Target::Pdf, None).unwrap().unwrap();
    let lines = folio::lines(world, Target::Pdf, None).unwrap().unwrap();
    let fonts = folio::fonts(world).unwrap();
    folio::layout::paginate(
        &text(world),
        &lines,
        &styled,
        &fonts,
        &folio::layout::Cache::new(),
        cache,
        regions,
    )
}

/// The body of a page: its one group.
fn body(page: &Page) -> &Fragment {
    match &page.fragment.items[0].1 {
        FragmentItem::Group(body) => body,
        _ => panic!("a page holds its body"),
    }
}

/// Every line of a build, in order, as its pages hold them.
fn all_lines(pages: &Pages) -> Vec<LineRef> {
    pages
        .pages
        .iter()
        .flat_map(|p| p.lines.iter().copied())
        .collect()
}

#[test]
fn pages_without_hooks_are_those_of_the_query() {
    let world = world("", &document(6));
    let cache = PageCache::new();
    let built = paginate(&world, Regions::default(), &cache);
    assert_eq!(built, pages(&world));
    // The same flow again is the same pages, every one from the cache.
    let misses = cache.stats().misses;
    assert_eq!(misses, built.pages.len() as u64);
    assert_eq!(paginate(&world, Regions::default(), &cache), built);
    assert_eq!(cache.stats().misses, misses);
}

#[test]
fn space_reserved_at_a_page_foot_moves_lines_to_the_next_page() {
    let world = world("", &document(6));
    let before = pages(&world);
    let height = before.master.body.size.height;
    let first = &before.pages[0].lines;
    assert!(first.len() > 10, "{first:?}");

    // A line in the middle of the first page reserves a third of the body,
    // as a footnote would: it stays, and the lines that no longer fit
    // above the reserved space move to the next page.
    let anchor = first[first.len() / 2];
    let third = height / 3.0;
    let reserve = |line: LineRef| {
        if line == anchor { third } else { Abs::zero() }
    };
    let regions = Regions {
        reserve: Some(&reserve),
        ..Regions::default()
    };
    let after = paginate(&world, regions, &PageCache::new());
    let page = &after.pages[0];
    assert!(page.lines.contains(&anchor));
    assert!(page.lines.len() < first.len());
    assert_eq!(page.lines[..], first[..page.lines.len()]);
    assert!(body(page).last_baseline <= height - third);
    // The next line would not have fitted: the old page held it lower.
    let moved = first[page.lines.len()];
    assert_eq!(after.pages[1].lines[0], moved);
    // No line is lost or repeated.
    assert_eq!(all_lines(&after), all_lines(&before));

    // A line that would not fit above its own reserve moves with it.
    let last = *first.last().unwrap();
    let reserve = |line: LineRef| {
        if line == last { third } else { Abs::zero() }
    };
    let regions = Regions {
        reserve: Some(&reserve),
        ..Regions::default()
    };
    let after = paginate(&world, regions, &PageCache::new());
    assert!(!after.pages[0].lines.contains(&last));
    assert!(after.pages[1].lines.contains(&last));
    assert!(body(&after.pages[1]).last_baseline <= height - third);
    assert_eq!(all_lines(&after), all_lines(&before));
}

#[test]
fn a_paragraph_is_broken_at_the_width_of_the_region_it_starts_in() {
    let world = world("", &document(6));
    let before = pages(&world);
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let full = before.master.body.size.width;
    let narrow = full - Abs::pt(120.0);
    let width = |page: usize| if page == 0 { narrow } else { full };
    let regions = Regions {
        width: Some(&width),
        ..Regions::default()
    };
    let after = paginate(&world, regions, &PageCache::new());

    let page = &after.pages[0];
    assert_eq!(body(page).size.width, narrow);
    // Every line of the first page ends within the narrower body.
    fn right(fragment: &Fragment, x: Abs, out: &mut Abs) {
        for (pos, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => right(group, x + pos.x, out),
                FragmentItem::Text(run) => *out = (*out).max(x + pos.x + run.width()),
                FragmentItem::Shape { .. } | FragmentItem::Object(_) | FragmentItem::Image(_) => {}
            }
        }
    }
    let mut edge = Abs::zero();
    right(body(page), Abs::zero(), &mut edge);
    assert!(edge.to_pt() <= narrow.to_pt() + 0.01, "{edge:?}");
    let count =
        |pages: &Pages, p: usize| all_lines(pages).iter().filter(|l| l.paragraph == p).count();
    // The paragraphs that start on the first page have more lines than in
    // the full width; one that continues onto the second page keeps them.
    let starting: Vec<usize> = page
        .lines
        .iter()
        .filter(|l| l.line == 0)
        .map(|l| l.paragraph)
        .collect();
    let prose = starting
        .iter()
        .copied()
        .filter(|&p| lines.paragraphs[p].lines.len() > 2)
        .collect::<Vec<_>>();
    assert!(!prose.is_empty());
    for p in prose {
        assert!(count(&after, p) > lines.paragraphs[p].lines.len(), "{p}");
    }
    // Paragraphs that start on later pages have their full-width lines.
    let later = after.pages[2].lines[0].paragraph + 1;
    for p in later..lines.paragraphs.len() {
        assert_eq!(count(&after, p), lines.paragraphs[p].lines.len(), "{p}");
    }
}

#[test]
fn pages_are_cached_by_the_state_coming_into_them() {
    let world = world("", &document(6));
    let cache = PageCache::new();
    let plain = paginate(&world, Regions::default(), &cache);
    let n = plain.pages.len() as u64;
    assert_eq!(cache.stats(), PageStats { hits: 0, misses: n });

    // A reserve on the second page changes where the third starts. What
    // lines reserve is part of a page's key, so with a hook every page is
    // broken again, the first too.
    let anchor = plain.pages[1].lines[3];
    let reserve = |line: LineRef| {
        if line == anchor {
            Abs::pt(100.0)
        } else {
            Abs::zero()
        }
    };
    let regions = Regions {
        reserve: Some(&reserve),
        ..Regions::default()
    };
    let reserved = paginate(&world, regions, &cache);
    assert_ne!(reserved.pages[2].lines[0], plain.pages[2].lines[0]);
    // The same hook again finds them all.
    let stats = cache.stats();
    assert_eq!(stats.hits, 0);
    let again = paginate(&world, regions, &cache);
    assert_eq!(again, reserved);
    assert_eq!(cache.stats().misses, stats.misses);

    // Pages that start at other lines of the same paragraphs did not
    // replace the ones without a reserve: those are all still found.
    let hits = cache.stats().hits;
    assert_eq!(paginate(&world, Regions::default(), &cache), plain);
    assert_eq!(cache.stats().hits, hits + n);
}

/// The first pages of a build, through the page with index `last`.
fn first_pages(world: &World, last: usize) -> folio::layout::FirstPages {
    folio::pages_through(world, Target::Pdf, None, last)
        .unwrap()
        .unwrap()
}

/// Asserts that the first pages laid out through each page, in a world
/// that has laid out nothing yet, are the first pages of the build, and
/// that the build's pages laid out after them are those of a world that
/// laid out all at once.
fn assert_first_pages(style: &str, text: &str) {
    let all = pages(&world(style, text));
    let n = all.pages.len();
    assert!(n >= 3, "{n} pages");
    for last in [0, 1, n / 2, n - 2, n - 1, n, n + 5] {
        let world = world(style, text);
        let first = first_pages(&world, last);
        let len = first.pages.pages.len();
        assert_eq!(first.complete, len == n, "through page {last}");
        assert!(
            first.complete || len > last,
            "through page {last}: {len} pages"
        );
        // A blank page padding before a page counts as one.
        assert!(len <= last + 2, "through page {last}: {len} pages");
        assert_eq!(first.pages.master, all.master);
        assert_eq!(first.pages.page_refs, all.page_refs);
        assert_eq!(
            &first.pages.pages[..],
            &all.pages[..len],
            "through page {last}"
        );
        assert_eq!(
            &first.pages.hashes[..],
            &all.hashes[..len],
            "through page {last}"
        );
        // The build's pages after them are the same as at once.
        assert_eq!(pages(&world), all, "after the pages through {last}");
    }
}

#[test]
fn the_first_pages_are_the_pages_of_the_build() {
    assert_first_pages("", &document(8));
}

#[test]
fn the_first_pages_carry_footnotes_over() {
    // Long notes, which continue on the next page: the queue of notes
    // carried over is part of the state the next page starts from.
    let mut text = String::new();
    for p in 0..30 {
        text.push_str(&format!(
            "{} #footnote[{}] {}\n\n",
            prose(p, 20),
            prose(100 + p, 60),
            prose(200 + p, 40)
        ));
    }
    let all = pages(&world("", &text));
    assert!(all.pages.len() >= 4, "{} pages", all.pages.len());
    assert_first_pages("", &text);
}

#[test]
fn the_first_pages_count_blank_pages() {
    let style = "heading { break-before: odd-page; }\n";
    let mut text = String::new();
    for s in 0..4 {
        text.push_str(&format!("= Heading {s}\n\n{}\n\n", prose(s, 40)));
    }
    assert_first_pages(style, &text);
}

/// A document whose page-number references need a layout pass.
const REFS: &str = "= One <one>\n\nSee #ref(<three>, form: \"page\") and #ref(<one>, form: \"page\").\n\n\
                    = Two\n\nText.\n\n= Three <three>\n\nMore.\n";

#[test]
fn the_first_pages_show_settled_page_numbers() {
    let style = "heading { break-before: page; }\n";
    let world = world(style, REFS);
    let first = first_pages(&world, 0);
    assert!(!first.complete);
    assert_eq!(first.pages.pages.len(), 1);
    // The page-number fixpoint laid every page out first, so the first
    // page shows where the third heading is.
    assert_eq!(first.pages.page_refs.pass, 1);
    let runs: String = page_runs(&first.pages.pages[0])
        .into_iter()
        .map(|(_, run)| run)
        .collect();
    assert!(runs.contains("page\u{a0}3"), "{runs:?}");
    assert_first_pages(style, REFS);
}
