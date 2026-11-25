//! Incremental recompilation (SPEC §2.3), counted: which queries re-run
//! after an edit, and which pages the page cache gives back. The
//! differential test in `folio-testkit` (`tests/differential.rs`) checks
//! that what is reused is right; this one checks that it is reused.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use folio::{Event, FragmentItem, Page, Pages, Target, World};

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nlang = \"en\"\nstyles = [\"main.fsty\"]\n";

/// A5 pages in one font.
const SHEET: &str = "page { page-size: A5; margin-top: 15mm; margin-bottom: 15mm; \
                     margin-inner: 15mm; margin-outer: 15mm; }\n\
                     doc { font: \"Source Serif 4\"; font-size: 11pt; }\n\
                     heading { keep-with-next: true; }\n";

/// How many times each query ran, by name.
type Counts = Arc<Mutex<BTreeMap<String, usize>>>;

/// A world whose query executions are counted.
fn counted(main: &str) -> (World, Counts) {
    let counts = Counts::default();
    let sink = counts.clone();
    let mut world = World::builder()
        .on_event(move |event| {
            if let Event::Execute { query } = event {
                *sink.lock().unwrap().entry(query.to_string()).or_default() += 1;
            }
        })
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", main).unwrap();
    world.set_source("main.fsty", SHEET).unwrap();
    (world, counts)
}

fn pages(world: &World) -> Pages {
    folio::pages(world, Target::Pdf, None).unwrap().unwrap()
}

/// A document of sections, each a heading and paragraphs of distinct
/// sentences, long enough for several pages.
fn document(sections: usize) -> String {
    let mut out = String::new();
    for s in 0..sections {
        out.push_str(&format!("= Section {s}\n\n"));
        for p in 0..3 {
            for w in 0..12 {
                out.push_str(&format!(
                    "Sentence {w} of paragraph {p} in section {s} says little. "
                ));
            }
            out.push_str("\n\n");
        }
    }
    out
}

/// The byte range of the first occurrence of `needle` in `text`.
fn find(text: &str, needle: &str) -> std::ops::Range<usize> {
    let start = text.find(needle).unwrap_or_else(|| panic!("no {needle:?}"));
    start..start + needle.len()
}

#[test]
fn a_one_character_edit_in_a_paragraph_does_not_rerun_the_cascade() {
    let main = document(3);
    let (mut world, counts) = counted(&main);
    folio::compile(&world, Target::Pdf, None).unwrap();
    assert_eq!(counts.lock().unwrap().get("cascade"), Some(&1));
    counts.lock().unwrap().clear();

    let range = find(&main, "says little");
    world
        .edit_source("main.fol", range.start..range.start + 1, "S")
        .unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let counts = counts.lock().unwrap();
    // The edit reached the output.
    assert!(counts.contains_key("compile_build"), "{counts:?}");
    assert!(counts.contains_key("styles"), "{counts:?}");
    assert!(!counts.contains_key("cascade"), "{counts:?}");
    // Selector matching re-runs only for the nodes the edit touched: the
    // paragraph's text, not the whole document.
    let matched = counts.get("match_node").copied().unwrap_or(0);
    assert!(matched <= 3, "{matched} nodes matched again: {counts:?}");
}

#[test]
fn a_reflow_confined_to_one_page_reuses_the_other_pages() {
    let main = document(12);
    let (mut world, _) = counted(&main);
    let before = pages(&world);
    assert!(before.pages.len() >= 6, "{} pages", before.pages.len());

    // A capital made small in a paragraph in the middle of the document,
    // which lies on one page: its lines change, but it keeps its number of
    // lines, so no page break moves.
    let range = find(&main, "Sentence 5 of paragraph 0 in section 6");
    world
        .edit_source("main.fol", range.start..range.start + 1, "s")
        .unwrap();
    let after = pages(&world);
    assert_eq!(before.pages.len(), after.pages.len());

    // The page itself is dressed as a box of the document; its body's
    // content is what the cache shares.
    let body = |p: &Page| match &p.fragment.items[0].1 {
        FragmentItem::Group(body) => body.items.as_ptr(),
        _ => panic!("a page holds its body"),
    };
    let changed: Vec<usize> = before
        .pages
        .iter()
        .zip(&after.pages)
        .enumerate()
        .filter(|(_, (a, b))| body(a) != body(b))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(changed.len(), 1, "pages laid out again: {changed:?}");
    let edited = changed[0];
    assert!(
        edited > 0 && edited + 1 < before.pages.len(),
        "the edit is in the middle of the document, on page {edited}"
    );
}
