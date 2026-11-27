//! Sized blocks (SPEC §6.3): a block of a `min-height`, and ruled lines as
//! answer space has them, laid out by the page builder; ruled lines stay on
//! one page unless `rule-breakable: true` or they fit on none.

use folio::layout::{PageCache, Regions};
use folio::{Abs, Diagnostic, Fragment, FragmentItem, Page, Pages, Point, Target, Text, World};

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

/// Where the shapes of a fragment and its descendants are on the page,
/// with each one's width as the extent of its path.
fn shapes(fragment: &Fragment, at: Point, out: &mut Vec<(Point, Abs)>) {
    for (pos, item) in &fragment.items {
        let here = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => shapes(group, here, out),
            FragmentItem::Shape { shape, .. } => {
                // The extent of the path's points across: the crate's
                // path types are not re-exported, so read them from their
                // debug form (`x: 12.5pt`).
                let debug = format!("{:?}", shape.path);
                let xs: Vec<f64> = debug
                    .split("x: ")
                    .skip(1)
                    .map(|rest| rest[..rest.find("pt").unwrap()].parse().unwrap())
                    .collect();
                let max = xs.iter().copied().fold(f64::MIN, f64::max);
                let min = xs.iter().copied().fold(f64::MAX, f64::min);
                let width = Abs::pt(max - min);
                out.push((here, width));
            }
            FragmentItem::Object(_) => panic!("a sized block draws no object frame"),
            FragmentItem::Text(_) | FragmentItem::Image(_) => {}
        }
    }
}

fn page_shapes(page: &Page) -> Vec<(Point, Abs)> {
    let mut out = Vec::new();
    shapes(&page.fragment, Point::zero(), &mut out);
    out
}

/// The baselines of the text runs on a page, from its top.
fn baselines(fragment: &Fragment, at: Abs, out: &mut Vec<Abs>) {
    for (pos, item) in &fragment.items {
        match item {
            FragmentItem::Group(group) => baselines(group, at + pos.y, out),
            FragmentItem::Text(_) => out.push(at + pos.y),
            _ => {}
        }
    }
}

fn page_baselines(page: &Page) -> Vec<Abs> {
    let mut out = Vec::new();
    baselines(&page.fragment, Abs::zero(), &mut out);
    out.dedup();
    out
}

fn close(a: Abs, b: Abs) -> bool {
    (a - b).to_pt().abs() < 1e-6
}

#[test]
fn ruled_lines_are_drawn_a_spacing_apart() {
    let world = world(
        "figure.answer { rule-lines: 4; rule-spacing: 10mm; space-before: 0pt; \
         space-after: 0pt; }\n",
        "Question.\n\n#figure(class: \"answer\")[]\n\nAfter.\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let rules = page_shapes(&pages.pages[0]);
    assert_eq!(rules.len(), 4, "{rules:?}");
    let body = pages.master.body;
    for (i, (pos, width)) in rules.iter().enumerate() {
        // Across the body, from its left edge.
        assert!(close(pos.x, body.origin.x), "{rules:?}");
        assert!(close(*width, body.size.width), "{rules:?}");
        if i > 0 {
            assert!(close(pos.y - rules[i - 1].0.y, Abs::mm(10.0)), "{rules:?}");
        }
    }
    // The rules are under the question, and the text after them below the
    // last.
    let lines = page_baselines(&pages.pages[0]);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0] < rules[0].0.y && rules[3].0.y < lines[1],
        "{lines:?} {rules:?}"
    );
    // The first rule is a spacing below the top of the block, which
    // starts half a leading below the question's descent.
    let space = rules[1].0.y - rules[0].0.y;
    assert!(close(space, Abs::mm(10.0)));
}

#[test]
fn ruled_lines_default_to_eight_millimetres_and_can_be_blank() {
    let world = world(
        "figure.answer { rule-lines: 3; }\nfigure.blank { rule-lines: 2; rule-stroke: none; }\n",
        "#figure(class: \"answer\")[]\n\n#figure(class: \"blank\")[]\n\nAfter.\n",
    );
    let pages = pages(&world);
    let rules = page_shapes(&pages.pages[0]);
    // The blank lines draw nothing but take their space.
    assert_eq!(rules.len(), 3, "{rules:?}");
    assert!(close(rules[1].0.y - rules[0].0.y, Abs::mm(8.0)));
    let after = page_baselines(&pages.pages[0]);
    assert!(
        after[0] - rules[2].0.y > Abs::mm(16.0),
        "{after:?} {rules:?}"
    );
}

/// The baseline of the paragraph after a block, with the block's style.
fn baseline_after(style: &str, block: &str) -> Abs {
    let world = world(
        &format!("figure {{ space-before: 0pt; space-after: 0pt; }}\n{style}"),
        &format!("Before.\n\n{block}\n\nAfter.\n"),
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    *page_baselines(&pages.pages[0]).last().unwrap()
}

#[test]
fn min_height_sizes_an_empty_block() {
    let plain = baseline_after("", "#figure[]");
    let sized = baseline_after("figure { min-height: 30mm; }", "#figure[]");
    assert!(close(sized - plain, Abs::mm(30.0)), "{plain:?} {sized:?}");
    // Padding and borders lie outside it.
    let padded = baseline_after("figure { min-height: 30mm; padding: 5pt; }", "#figure[]");
    assert!(close(padded - plain, Abs::mm(30.0) + Abs::pt(10.0)));
}

#[test]
fn min_height_grows_a_block_whose_content_is_shorter() {
    let block = "#figure[Short.]";
    let plain = baseline_after("", block);
    let sized = baseline_after("figure { min-height: 40mm; }", block);
    // The content box is 40mm tall: the line and the half leadings around
    // it, then the space that fills it.
    let world = world("", "Line.\n");
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let line = &lines.paragraphs[0].lines[0];
    let content = line.ascent + line.descent + Abs::pt(0.65 * 11.0);
    assert!(
        close(sized - plain, Abs::mm(40.0) - content),
        "{plain:?} {sized:?} {content:?}"
    );
    // A block taller than its `min-height` keeps its height.
    let tall = baseline_after("figure { min-height: 2pt; }", block);
    assert!(close(tall, plain));
}

/// A document whose first page has about a third of its body left
/// after its prose, then a block of `rules` ruled lines.
fn ruled_after_prose(style: &str, rules: usize) -> World {
    let mut source = String::new();
    for _ in 0..5 {
        source.push_str(&prose(60));
        source.push_str("\n\n");
    }
    source.push_str("#figure(class: \"answer\")[]\n\nAfter.\n");
    world(
        &format!("figure.answer {{ rule-lines: {rules}; rule-spacing: 10mm; }}\n{style}"),
        &source,
    )
}

/// How many rules each page draws.
fn rules_per_page(pages: &Pages) -> Vec<usize> {
    pages.pages.iter().map(|p| page_shapes(p).len()).collect()
}

#[test]
fn ruled_lines_stay_on_one_page() {
    let world = ruled_after_prose("", 12);
    let pages = pages(&world);
    let counts = rules_per_page(&pages);
    // They would fit in part on the first page, but move whole.
    assert_eq!(counts[0], 0, "{counts:?}");
    assert_eq!(counts.iter().sum::<usize>(), 12);
    assert!(counts.contains(&12), "{counts:?}");
    let diags = diagnostics(&world);
    assert!(
        diags.iter().all(|d| !d.message.contains("ruled")),
        "{diags:?}"
    );
}

#[test]
fn ruled_lines_break_when_asked_to() {
    let world = ruled_after_prose("figure.answer { rule-breakable: true; }", 12);
    let pages = pages(&world);
    let counts = rules_per_page(&pages);
    assert_eq!(counts.iter().sum::<usize>(), 12, "{counts:?}");
    let split: Vec<usize> = counts.iter().copied().filter(|&n| n > 0).collect();
    assert_eq!(split.len(), 2, "{counts:?}");
    // At least two on either side of the break.
    assert!(split.iter().all(|&n| n >= 2), "{counts:?}");
}

#[test]
fn ruled_lines_taller_than_a_page_break_with_a_warning() {
    // 25 lines of 10mm do not fit in a body 180mm tall.
    let world = world(
        "figure.answer { rule-lines: 25; rule-spacing: 10mm; }\n",
        "Question.\n\n#figure(class: \"answer\")[]\n",
    );
    let pages = pages(&world);
    let counts = rules_per_page(&pages);
    assert_eq!(counts.iter().sum::<usize>(), 25, "{counts:?}");
    assert!(counts.len() >= 2, "{counts:?}");
    let diags = diagnostics(&world);
    let broken: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("broken-unbreakable"))
        .collect();
    assert_eq!(broken.len(), 1, "{diags:?}");
    assert!(broken[0].message.contains("ruled lines"), "{broken:?}");
    // Nothing runs off a page.
    let body = pages.master.body;
    for page in &pages.pages {
        for (pos, _) in page_shapes(page) {
            assert!(pos.y <= body.origin.y + body.size.height + Abs::pt(0.01));
        }
    }
}

#[test]
fn rules_span_the_region_they_are_set_in() {
    let world = world(
        "figure.answer { rule-lines: 2; padding: 0pt 6pt; }\n",
        "#figure(class: \"answer\")[]\n",
    );
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let fonts = folio::fonts(&world).unwrap();
    let wider = |_: usize| lines.width + Abs::pt(20.0);
    let pages = folio::layout::paginate(
        &text(&world),
        &lines,
        &styled,
        &fonts,
        &folio::layout::Cache::new(),
        &PageCache::new(),
        Regions {
            width: Some(&wider),
            ..Regions::default()
        },
    );
    let rules = page_shapes(&pages.pages[0]);
    assert_eq!(rules.len(), 2);
    let content = lines.width + Abs::pt(20.0) - Abs::pt(12.0);
    for (pos, width) in &rules {
        assert!(close(*width, content), "{rules:?}");
        assert!(close(pos.x, pages.master.body.origin.x + Abs::pt(6.0)));
    }
}
