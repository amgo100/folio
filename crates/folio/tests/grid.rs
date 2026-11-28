//! Tables and the grid primitive (SPEC §7.4): cells placed by auto-flow or
//! explicitly, tracks sized from the cells' measures, rows set side by side
//! as bands, broken between pages, inside only with `row-breakable: true`
//! or where a row fits on no page, and header rows set again on every page
//! that continues a grid.

use folio::{Abs, Diagnostic, Fragment, FragmentItem, Page, Pages, Point, Target, World};

/// A5 with 15mm margins: a body of 118mm by 180mm.
const PAGE: &str = "page { page-size: A5; margin-top: 15mm; margin-bottom: 15mm; \
                    margin-inner: 15mm; margin-outer: 15mm; }\n\
                    doc { font: \"Source Serif 4\"; font-size: 10pt; justify: false; }\n";

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

fn diagnostics(world: &World) -> Vec<Diagnostic> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .to_vec()
}

fn codes(world: &World) -> Vec<String> {
    diagnostics(world)
        .iter()
        .filter_map(|d| d.code.as_deref().map(str::to_owned))
        .collect()
}

/// The text runs of a fragment and its descendants, each with where its
/// baseline starts on the page, in painting order.
fn runs(fragment: &Fragment, at: Point, out: &mut Vec<(Point, String)>) {
    for (pos, item) in &fragment.items {
        let here = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Group(group) => runs(group, here, out),
            FragmentItem::Text(run) => out.push((here, run.text.to_string())),
            _ => {}
        }
    }
}

fn page_runs(page: &Page) -> Vec<(Point, String)> {
    let mut out = Vec::new();
    runs(&page.fragment, Point::zero(), &mut out);
    out
}

/// The run whose text starts with `text` on a page.
fn find(page: &Page, text: &str) -> Option<Point> {
    page_runs(page)
        .into_iter()
        .find(|(_, t)| t.starts_with(text))
        .map(|(p, _)| p)
}

fn close(a: Abs, b: Abs) -> bool {
    (a - b).to_pt().abs() < 1e-3
}

#[test]
fn cells_flow_into_rows_side_by_side() {
    let world = world(
        "",
        "#grid(column-tracks: 3)[\n  #cell[Alpha] #cell[Beta] #cell[Gamma]\n  #cell[Delta]\n]\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let page = &pages.pages[0];
    let (a, b, c, d) = (
        find(page, "Alpha").unwrap(),
        find(page, "Beta").unwrap(),
        find(page, "Gamma").unwrap(),
        find(page, "Delta").unwrap(),
    );
    // The first three on one baseline, left to right; the fourth starts
    // the next row, in the first column.
    assert!(close(a.y, b.y) && close(b.y, c.y), "{a:?} {b:?} {c:?}");
    assert!(a.x < b.x && b.x < c.x);
    assert!(d.y > a.y);
    assert!(close(d.x, a.x));
    assert!(diagnostics(&world).is_empty(), "{:?}", diagnostics(&world));
}

#[test]
fn tracks_are_sized_from_the_cells() {
    let world = world(
        "",
        "#grid(column-tracks: (20mm, auto, 1fr, 3fr))[\n  #cell[A] #cell[A longer cell] \
         #cell[x] #cell[y]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let grid = &lines.grids[0];
    let body = Abs::mm(118.0);
    assert_eq!(grid.columns.len(), 4);
    assert!(close(grid.columns[0], Abs::mm(20.0)));
    // The `auto` track takes its cell's max-content; the fractions share
    // the rest one to three.
    let rest = body - grid.columns[0] - grid.columns[1];
    assert!(grid.columns[1] > Abs::zero() && grid.columns[1] < Abs::mm(40.0));
    assert!(close(grid.columns[2], rest / 4.0), "{:?}", grid.columns);
    assert!(close(grid.columns[3], rest * 0.75));
    assert!(close(grid.offsets[2], grid.columns[0] + grid.columns[1]));
}

#[test]
fn stylesheet_tracks_and_track_functions() {
    let world = world(
        "grid.t { column-tracks: repeat(2, 30mm) minmax(10mm, 1fr); }\n",
        "#grid(class: \"t\")[\n  #cell[a] #cell[b] #cell[c]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let columns = &lines.grids[0].columns;
    assert_eq!(columns.len(), 3);
    assert!(close(columns[0], Abs::mm(30.0)) && close(columns[1], Abs::mm(30.0)));
    assert!(close(columns[2], Abs::mm(58.0)), "{columns:?}");
}

#[test]
fn spans_and_explicit_places() {
    let world = world(
        "",
        "#grid(column-tracks: (1fr, 1fr, 1fr))[\n  #cell(rowspan: 2)[Tall] #cell(colspan: 2)[Wide]\n  \
         #cell[Under] #cell(column: 1, row: 3)[Placed]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let places: Vec<(usize, usize, usize, usize)> = lines.grids[0]
        .places
        .iter()
        .map(|p| (p.row, p.column, p.rowspan, p.colspan))
        .collect();
    assert_eq!(
        places,
        [(0, 0, 2, 1), (0, 1, 1, 2), (1, 1, 1, 1), (2, 0, 1, 1)]
    );
    let page = &pages(&world).pages[0];
    let (tall, wide, under) = (
        find(page, "Tall").unwrap(),
        find(page, "Wide").unwrap(),
        find(page, "Under").unwrap(),
    );
    assert!(close(tall.y, wide.y));
    assert!(under.y > wide.y && under.x > tall.x);
}

#[test]
fn an_overfull_grid_is_reported() {
    let world = world(
        "",
        "#grid(column-tracks: (100mm, 50mm, 1fr))[\n  #cell[a] #cell[b] #cell[c]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let grid = &lines.grids[0];
    assert_eq!(grid.columns[2], Abs::zero());
    assert!(close(grid.overflow, Abs::mm(32.0)), "{:?}", grid.overflow);
    assert!(codes(&world).contains(&"overfull-line".to_owned()));
}

/// A grid of a header row and `n` rows.
fn long_table(n: usize, extra: &str) -> String {
    let mut out = format!("#grid(column-tracks: (auto, 1fr), header-rows: 1{extra})[\n");
    out.push_str("  #cell[Head] #cell[Value]\n");
    for i in 1..=n {
        out.push_str(&format!("  #cell[Row {i}] #cell[Value {i}]\n"));
    }
    out.push_str("]\n");
    out
}

#[test]
fn header_rows_repeat_on_every_page_the_grid_continues() {
    let world = world("cell { padding: 3pt; }\n", &long_table(80, ""));
    let pages = pages(&world);
    assert!(pages.pages.len() >= 2, "{}", pages.pages.len());
    let mut next = 1;
    for page in &pages.pages {
        let runs = page_runs(page);
        // The header comes first on every page.
        assert!(runs[0].1.starts_with("Head"), "{:?}", &runs[..2]);
        let heads = runs.iter().filter(|(_, t)| t.starts_with("Head")).count();
        assert_eq!(heads, 1);
        // The rows continue in order, none lost or set twice.
        for (_, text) in &runs {
            if let Some(n) = text.strip_prefix("Row ") {
                assert_eq!(n.parse::<usize>().unwrap(), next);
                next += 1;
            }
        }
    }
    assert_eq!(next, 81);
    // The copies of the header are artifacts; the first is content.
    fn artifacts(fragment: &Fragment) -> usize {
        fragment
            .items
            .iter()
            .map(|(_, item)| match item {
                FragmentItem::Group(g) if g.artifact => 1,
                FragmentItem::Group(g) => artifacts(g),
                _ => 0,
            })
            .sum()
    }
    assert_eq!(artifacts(&pages.pages[0].fragment), 0);
    for page in &pages.pages[1..] {
        assert_eq!(artifacts(&page.fragment), 2);
    }
    // Rows are not broken: no row warns.
    assert!(!codes(&world).contains(&"broken-unbreakable".to_owned()));
}

#[test]
fn a_page_does_not_end_right_after_the_header() {
    // A tall paragraph leaves room for the header but not the first row.
    let world = world(
        "cell { padding: 2pt; }\nparagraph { space-after: 0pt; }\n",
        &format!(
            "{}\n\n#grid(header-rows: 1)[\n  #cell[Head]\n  #cell[{}]\n]\n",
            "Lead text. ".repeat(160),
            "Body. ".repeat(10)
        ),
    );
    let pages = pages(&world);
    for page in &pages.pages {
        let runs = page_runs(page);
        if let Some(i) = runs.iter().position(|(_, t)| t.starts_with("Head")) {
            assert!(
                runs[i + 1..].iter().any(|(_, t)| t.starts_with("Body")),
                "the header ends its page"
            );
        }
    }
}

#[test]
fn a_row_breaks_only_with_row_breakable() {
    let tall = |breakable: &str| {
        world(
            &format!("grid {{ {breakable} }}\n"),
            &format!(
                "{}\n\n#grid(column-tracks: (1fr, 1fr))[\n  #cell[{}] #cell[Short]\n]\n",
                "Lead text. ".repeat(100),
                "Long words in a tall cell. ".repeat(60)
            ),
        )
    };
    // Without it, the row moves whole to the next page.
    let whole = pages(&tall(""));
    let first = page_runs(&whole.pages[0]);
    assert!(!first.iter().any(|(_, t)| t.starts_with("Short")));
    assert!(find(&whole.pages[1], "Short").is_some());
    // With it, the row starts on the first page and continues.
    let broken = pages(&tall("row-breakable: true;"));
    assert!(find(&broken.pages[0], "Short").is_some());
    assert!(
        page_runs(&broken.pages[1])
            .iter()
            .any(|(_, t)| t.contains("tall"))
    );
}

#[test]
fn a_row_taller_than_a_page_splits_with_a_warning() {
    let world = world(
        "",
        &format!(
            "#grid(column-tracks: (1fr, 1fr))[\n  #cell[{}] #cell[Short]\n]\n",
            "Long words in a very tall cell. ".repeat(200)
        ),
    );
    let pages = pages(&world);
    assert!(pages.pages.len() >= 2);
    // No content is lost: every word of the cell is set.
    let words: usize = pages
        .pages
        .iter()
        .flat_map(page_runs)
        .map(|(_, t)| t.matches("tall").count())
        .sum();
    assert_eq!(words, 200);
    let warned = diagnostics(&world)
        .iter()
        .filter(|d| d.code.as_deref() == Some("broken-unbreakable"))
        .count();
    assert!(warned >= 1);
    assert!(!codes(&world).contains(&"page-overflow".to_owned()));
}

#[test]
fn footnotes_in_cells_go_to_the_page_foot() {
    let world = world(
        "",
        "#grid(column-tracks: (1fr, 1fr))[\n  #cell[One#footnote[First note.]] \
         #cell[Two#footnote[Second note.]]\n]\n",
    );
    let pages = pages(&world);
    assert_eq!(pages.pages.len(), 1);
    let page = &pages.pages[0];
    let first = find(page, "First").unwrap();
    let second = find(page, "Second").unwrap();
    let one = find(page, "One").unwrap();
    // Below the table, in the order of the cells.
    assert!(first.y > one.y + Abs::mm(50.0));
    assert!(second.y > first.y);
}

#[test]
fn cells_carry_their_boxes() {
    let world = world(
        "grid { border-top: none; border-bottom: none; }\ncell { background: #eeeeee; padding: 4pt; }\n",
        "#grid(column-tracks: (1fr, 1fr))[\n  #cell[Short] #cell[A cell with much more text \
         in it than the other, so that it runs onto several lines.]\n]\n",
    );
    let page = &pages(&world).pages[0];
    // Both cells' boxes are as tall as the row.
    fn boxes(fragment: &Fragment, out: &mut Vec<Abs>) {
        for (_, item) in &fragment.items {
            if let FragmentItem::Group(group) = item {
                if group
                    .items
                    .iter()
                    .any(|(_, i)| matches!(i, FragmentItem::Shape { .. }))
                {
                    out.push(group.size.height);
                }
                boxes(group, out);
            }
        }
    }
    let mut heights = Vec::new();
    boxes(&page.fragment, &mut heights);
    assert_eq!(heights.len(), 2, "{heights:?}");
    assert!(close(heights[0], heights[1]), "{heights:?}");
}

#[test]
fn an_edit_after_a_long_table_reuses_its_pages() {
    let mut world = world("cell { padding: 3pt; }\n", &long_table(80, ""));
    let before = pages(&world);
    let text = format!("{}\nAfter the table.\n", long_table(80, ""));
    world.set_source("main.fol", &text).unwrap();
    let after = pages(&world);
    // The table's pages look as they did.
    for i in 0..before.pages.len() - 1 {
        assert_eq!(before.hashes[i], after.hashes[i], "page {i}");
    }
}

#[test]
fn footnotes_in_nested_grids_go_to_the_page_foot() {
    let world = world(
        "",
        "#grid(column-tracks: (1fr, 1fr))[\n  #cell[Outer] #cell[\n    #grid(column-tracks: 2)[\n      \
         #cell[Inner#footnote[Nested note.]] #cell[x]\n    ]\n  ]\n]\n",
    );
    let pages = pages(&world);
    let page = &pages.pages[0];
    let note = find(page, "Nested").expect("the note is placed");
    assert!(note.y > find(page, "Inner").unwrap().y + Abs::mm(50.0));
    // The nested cell's lines are among the page's lines.
    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let inner = text
        .paragraphs
        .iter()
        .position(|p| p.shaped.text().starts_with("Inner"))
        .unwrap();
    assert!(page.lines.iter().any(|l| l.paragraph == inner));
}

#[test]
fn a_header_cell_spanning_rows_makes_them_header_rows() {
    let world = world(
        "",
        "#grid(column-tracks: 2, header-rows: 1)[\n  #cell(rowspan: 2)[Head] #cell[a]\n  \
         #cell[b] #cell[c] #cell[d]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(lines.grids[0].header_rows, 2);
}

#[test]
fn a_row_continued_on_the_next_page_is_not_padded_to_parity() {
    let world = world(
        "grid { break-before: odd-page; row-breakable: true; }\n",
        &format!(
            "Intro.\n\n#grid(column-tracks: (1fr, 1fr))[\n  #cell[{}] #cell[Short]\n]\n",
            "Long words in a very tall cell. ".repeat(150)
        ),
    );
    let pages = pages(&world);
    // The intro, a blank page before the grid's odd page, then the row
    // on every page after it.
    assert!(pages.pages.len() >= 4, "{}", pages.pages.len());
    assert!(page_runs(&pages.pages[1]).is_empty());
    for page in &pages.pages[2..] {
        assert!(!page_runs(page).is_empty());
    }
}

#[test]
fn blocks_stand_for_cells() {
    let world = world(
        "",
        "#grid(column-tracks: 2)[\n  First cell.\n\n  Second cell.\n\n  - an item\n  - another\n\n  \
         #cell[Last]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(lines.grids[0].places.len(), 4);
    let page = &pages(&world).pages[0];
    let (a, b) = (find(page, "First").unwrap(), find(page, "Second").unwrap());
    assert!(close(a.y, b.y) && a.x < b.x);
    let pdf = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(pdf.diagnostics.is_empty(), "{:?}", pdf.diagnostics);
}

#[test]
fn a_finished_cell_keeps_its_box_where_its_row_continues() {
    let world = world(
        "grid { row-breakable: true; border-top: none; border-bottom: none; }\ncell { background: #eeeeee; }\n",
        &format!(
            "#grid(column-tracks: (1fr, 1fr))[\n  #cell[Short] #cell[{}]\n]\n",
            "Long words in a very tall cell. ".repeat(150)
        ),
    );
    let pages = pages(&world);
    assert!(pages.pages.len() >= 2);
    // Two cell boxes, each with its background, on every page.
    fn filled(fragment: &Fragment, out: &mut usize) {
        for (_, item) in &fragment.items {
            if let FragmentItem::Group(group) = item {
                if group
                    .items
                    .iter()
                    .any(|(_, i)| matches!(i, FragmentItem::Shape { .. }))
                {
                    *out += 1;
                }
                filled(group, out);
            }
        }
    }
    for page in &pages.pages {
        let mut n = 0;
        filled(&page.fragment, &mut n);
        assert_eq!(n, 2);
    }
}

#[test]
fn ratio_tracks_of_a_nested_grid_need_no_width() {
    let world = world(
        "",
        "#grid(column-tracks: (auto, auto))[\n  #cell[#grid(column-tracks: (50%, 50%))[#cell[a] \
         #cell[b]]] #cell[Some words in the second column]\n]\n",
    );
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let outer = lines
        .grids
        .iter()
        .find(|g| g.columns.len() == 2 && g.places.len() == 2);
    let outer = outer.unwrap();
    assert_eq!(outer.overflow, Abs::zero());
    assert!(outer.columns[0] < Abs::mm(60.0), "{:?}", outer.columns);
}

#[test]
fn the_base_look_rules_the_header_on_every_page() {
    let world = world("", &long_table(80, ""));
    let pages = pages(&world);
    assert!(pages.pages.len() >= 2);
    // Each page has the rule under its header: a line as wide as the grid.
    fn rules(fragment: &Fragment, out: &mut usize) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(g) => rules(g, out),
                FragmentItem::Shape { .. } => *out += 1,
                _ => {}
            }
        }
    }
    let mut first = 0;
    rules(&pages.pages[0].fragment, &mut first);
    // The top border and the header rule.
    assert_eq!(first, 2);
    for page in &pages.pages[1..pages.pages.len() - 1] {
        let mut n = 0;
        rules(&page.fragment, &mut n);
        assert_eq!(n, 1, "the header rule alone");
    }
}

// ---- Pipe rows and data rows (#440) ----

/// Asserts that two documents lay out to the same pages, and builds both
/// without diagnostics.
#[track_caller]
fn same_pages(a: &str, b: &str, files: &[(&str, &str)]) {
    let build = |text: &str| {
        let mut world = world("", text);
        for (path, data) in files {
            world.set_file(path, data.as_bytes().to_vec()).unwrap();
        }
        let messages: Vec<String> = diagnostics(&world)
            .iter()
            .map(|d| d.message.to_string())
            .collect();
        assert!(
            messages.is_empty(),
            "diagnostics for {text:?}: {messages:?}"
        );
        pages(&world).hashes.to_vec()
    };
    assert_eq!(build(a), build(b), "{a}\n---\n{b}");
}

/// The five tables of `docs/design/table-syntax.md`, written with cells
/// (option A) and with pipe rows and data rows (option B).
#[test]
fn the_design_examples_lay_out_alike_in_both_forms() {
    // (a) A data table with a header row and units.
    same_pages(
        "#grid(column-tracks: (auto, 1fr, 1fr), header-rows: 1)[\n\
         \x20 #cell[Length $l$ / m] #cell[Period $T$ / s] #cell[$T^2$ / s²]\n\
         \x20 #cell[0.200] #cell[0.897] #cell[0.805]\n\
         \x20 #cell[0.250] #cell[1.003] #cell[1.006]\n]\n",
        "#grid(column-tracks: (auto, 1fr, 1fr), header-rows: 1)[\n\
         \x20 | Length $l$ / m | Period $T$ / s | $T^2$ / s² |\n\
         \x20 | 0.200          | 0.897          | 0.805      |\n\
         \x20 | 0.250          | 1.003          | 1.006      |\n]\n",
        &[],
    );
    // (b) A mark scheme whose cells hold paragraphs, a list and formulas.
    same_pages(
        "#grid(class: \"markscheme\", column-tracks: (auto, 1fr, auto), header-rows: 1)[\n\
         \x20 #cell[Question] #cell[Answer] #cell[Marks]\n\
         \x20 #cell[3(a)] #cell[\n    Use of $v = u + a t$.\n\n    $a = 18 / 6.0$\n  ] #cell[2]\n\
         \x20 #cell[3(b)] #cell[\n    Any two of:\n    - air resistance is ignored;\n    - $g$ is uniform.\n  ] #cell[2]\n]\n",
        "#grid(class: \"markscheme\", column-tracks: (auto, 1fr, auto))[\n\
         \x20 | Question | Answer | Marks |\n\
         \x20 |----------|--------|-------|\n\
         \x20 | 3(a) | #cell[\n      Use of $v = u + a t$.\n\n      $a = 18 / 6.0$\n    ] | 2 |\n\
         \x20 | 3(b) | #cell[\n      Any two of:\n      - air resistance is ignored;\n      - $g$ is uniform.\n    ] | 2 |\n]\n",
        &[],
    );
    // (c) Spans across columns and rows.
    same_pages(
        "#grid(column-tracks: 3, header-rows: 1)[\n\
         \x20 #cell[Kind] #cell[Source] #cell[Remedy]\n\
         \x20 #cell(rowspan: 2)[Random] #cell[Reaction time] #cell[Time ten swings]\n\
         \x20 #cell[Amplitude varies] #cell[Release from a mark]\n\
         \x20 #cell(colspan: 3)[Systematic errors are discussed below.]\n]\n",
        "#grid(column-tracks: 3, header-rows: 1)[\n\
         \x20 | Kind                      | Source           | Remedy              |\n\
         \x20 | #cell(rowspan: 2)[Random] | Reaction time    | Time ten swings     |\n\
         \x20                             | Amplitude varies | Release from a mark |\n\
         \x20 | #cell(colspan: 3)[Systematic errors are discussed below.] |\n]\n",
        &[],
    );
    // (d) A sparse table.
    same_pages(
        "#grid(column-tracks: 6, header-rows: 1)[\n\
         \x20 #cell[] #cell[Mon] #cell[Tue] #cell[Wed] #cell[Thu] #cell[Fri]\n\
         \x20 #cell[9:00] #cell[] #cell[Physics] #cell[] #cell[] #cell[Lab]\n\
         \x20 #cell[10:00] #cell[] #cell[] #cell[Maths] #cell[] #cell[]\n]\n",
        "#grid(column-tracks: 6, header-rows: 1)[\n\
         \x20 |       | Mon | Tue     | Wed   | Thu | Fri |\n\
         \x20 | 9:00  ||       Physics ||| Lab |\n\
         \x20 | 10:00 |     |         | Maths |     |     |\n]\n",
        &[],
    );
    // (e) A table from a data file, header included.
    let csv = [(
        "results.csv",
        "l,T,T2\n0.200,0.897,0.805\n0.250,1.003,1.006\n",
    )];
    same_pages(
        "#let data = csv(\"results.csv\")\n\
         #grid(column-tracks: data.first().len(), header-rows: 1)[\n\
         \x20 #for row in data { for value in row [#cell[#value]] }\n]\n",
        "#grid(rows: csv(\"results.csv\"), header: auto)\n",
        &csv,
    );
}

#[test]
fn data_rows_take_a_header_and_come_before_the_body() {
    same_pages(
        "#grid(column-tracks: 2, header-rows: 1)[\n\
         \x20 #cell[a] #cell[b]\n  #cell[1] #cell[2]\n  #cell[3] #cell[]\n  #cell[x] #cell[y]\n]\n",
        "#grid(header: ([a], \"b\"), rows: ((1, 2), (3,)))[\n  | x | y |\n]\n",
        &[],
    );
    // A row under a cell that spans rows leaves out its place.
    same_pages(
        "#grid(column-tracks: 2)[\n\
         \x20 #cell(rowspan: 2)[a] #cell[b]\n  #cell[c]\n]\n",
        "#grid(rows: ((cell(rowspan: 2)[a], [b]), ([c],)))\n",
        &[],
    );
}

#[test]
fn a_separator_line_marks_header_rows() {
    let long = |separator: bool, header: &str| {
        let mut text = format!("#grid(column-tracks: 2{header})[\n  | Head | Er |\n");
        if separator {
            text.push_str("  |------|----|\n");
        }
        for i in 0..80 {
            text.push_str(&format!("  | Row {i} | x |\n"));
        }
        text.push_str("]\n");
        text
    };
    same_pages(&long(true, ""), &long(false, ", header-rows: 1"), &[]);
    // It agrees with `header-rows`, or is an error.
    same_pages(
        &long(true, ", header-rows: 1"),
        &long(false, ", header-rows: 1"),
        &[],
    );
    let world = world("", &long(true, ", header-rows: 2"));
    let messages: Vec<String> = diagnostics(&world)
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("marks 1 header row, but `header-rows` is 2")),
        "{messages:?}"
    );
}

#[test]
fn pipe_row_errors() {
    let errors = |text: &str| -> Vec<String> {
        diagnostics(&world("", text))
            .iter()
            .filter(|d| d.severity == folio::Severity::Error)
            .map(|d| d.message.to_string())
            .collect()
    };
    // Outside a grid's body, and in a cell inside one.
    assert_eq!(
        errors("| a | b |\n"),
        ["a pipe row makes cells only in a grid's body"]
    );
    assert_eq!(
        errors("#grid[#cell[\n| a |\n]]\n"),
        ["a pipe row makes cells only in a grid's body"]
    );
    // An escaped pipe is text.
    assert!(errors("\\| a | b |\n").is_empty());
    assert!(errors("#grid[\n  | a \\| b |\n]\n").is_empty());
    // Separator lines.
    assert_eq!(
        errors("#grid[\n  |---|\n  | a |\n]\n"),
        ["a `|---|` line marks the rows above it as header rows, but there are none"]
    );
    assert_eq!(
        errors("#grid[\n  | a |\n  |---|\n  | b |\n  |---|\n]\n"),
        ["a grid's body has one `|---|` line at most"]
    );
    assert_eq!(
        errors("#grid[\n  #cell[a]\n  |---|\n]\n"),
        ["the rows above a `|---|` line must all be pipe rows"]
    );
    assert_eq!(
        errors("#grid(header: ([A], [B]))[\n  | H | I |\n  |---|---|\n  | 1 | 2 |\n]\n"),
        [
            "a `|---|` line cannot mark header rows in a grid with `rows` or `header`, whose rows \
             come before the body's"
        ]
    );
    assert_eq!(
        errors("#let t = grid.with(rows: ((1,),))\n#t[\n  | H |\n  |---|\n  | x |\n]\n"),
        [
            "a `|---|` line cannot mark header rows in a grid with `rows` or `header`, whose rows \
             come before the body's"
        ]
    );
    // Pipes after a block's `[` or a list marker are text.
    assert!(errors("#let n(v) = [|#v|]\n#n[x] #strong[|x|]\n- |v| is the speed\n").is_empty());
    // Data rows.
    assert_eq!(
        errors("#grid(rows: ((cell(column: 2)[x],),))\n"),
        ["a cell of a row gives its place by both `row` and `column`, or by neither"]
    );
    assert_eq!(
        errors("#grid(rows: (1, 2))\n"),
        ["row 1 of `rows`: a row is an array of cells, not an int"]
    );
    assert_eq!(
        errors("#grid(header: auto)\n"),
        ["`header: auto` makes the first row of `rows` the header row, but there is none"]
    );
    assert_eq!(
        errors("#grid(rows: ((1,),), header: auto, header-rows: 2)\n"),
        ["`header` makes one header row, but `header-rows` is 2"]
    );
}

#[test]
fn rows_in_a_named_body_and_through_with() {
    same_pages(
        "#grid(column-tracks: 2, body: [\n  | a | b |\n])\n",
        "#grid(column-tracks: 2)[\n  | a | b |\n]\n",
        &[],
    );
    // A `|---|` line agrees with `header-rows` applied through `.with`.
    same_pages(
        "#let t = grid.with(column-tracks: 2, header-rows: 1)\n#t[\n  | H | I |\n  |---|---|\n  | a | b |\n]\n",
        "#grid(column-tracks: 2, header-rows: 1)[\n  | H | I |\n  | a | b |\n]\n",
        &[],
    );
}

#[test]
fn a_data_cell_at_its_own_place_is_not_covered() {
    same_pages(
        "#grid(column-tracks: 2)[\n  #cell[a] #cell[x]\n  #cell[b] #cell[c]\n]\n",
        "#grid(rows: ((cell(row: 1, column: 2)[x], [a]), ([b], [c])))\n",
        &[],
    );
}

#[test]
fn a_body_of_pipe_rows_gives_the_grid_its_columns() {
    // Without `column-tracks`, each line is a row, as wide as the widest;
    // a short row ends with empty cells, and `|---|` marks the lines above.
    same_pages(
        "#grid(column-tracks: 3, header-rows: 1)[\n\
         \x20 #cell[A] #cell[B] #cell[]\n  #cell[1] #cell[2] #cell[3]\n]\n",
        "#grid[\n  | A | B |\n  |---|---|\n  | 1 | 2 | 3 |\n]\n",
        &[],
    );
    // Data rows and then the body's rows, with wider tracks.
    same_pages(
        "#grid(column-tracks: 3)[\n\
         \x20 #cell[1] #cell[2] #cell[]\n  #cell[3] #cell[4] #cell[]\n  #cell[a] #cell[b] #cell[c]\n]\n",
        "#grid(column-tracks: 3, rows: ((1, 2), (3, 4)))[\n  | a | b | c |\n]\n",
        &[],
    );
}

#[test]
fn more_pipe_row_errors() {
    let errors = |text: &str| -> Vec<String> {
        diagnostics(&world("", text))
            .iter()
            .filter(|d| d.severity == folio::Severity::Error)
            .map(|d| d.message.to_string())
            .collect()
    };
    assert_eq!(
        errors("#grid(rows: ((1,),))[\n  #cell[a]\n]\n"),
        ["the body of a grid with `rows` or `header` holds pipe rows only"]
    );
    assert_eq!(
        errors("#grid(column-tracks: 2)[\n  | #cell[a] #cell[b] |\n]\n"),
        ["a cell of a pipe row holds one `#cell(..)[..]` at most"]
    );
    // Strong text in a cell keeps a pipe inside brackets.
    assert!(errors("#grid(column-tracks: 2)[\n  | *a [x | y]* | b |\n]\n").is_empty());
    // A `break` ends the row and the loop.
    assert!(
        errors("#for i in (1, 2) {\n  grid[\n| #if i == 1 { break } | #undefined |\n]\n}\n")
            .is_empty()
    );
}
