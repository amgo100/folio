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
    assert!(find(&broken.pages[1], "Long").is_some());
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
        "cell { background: #eeeeee; padding: 4pt; }\n",
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
