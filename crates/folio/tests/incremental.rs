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
    let range = find(&main, "Sentence 5 of paragraph 1 in section 6");
    world
        .edit_source("main.fol", range.start..range.start + 1, "s")
        .unwrap();
    let after = pages(&world);
    // What the test assumes of the layout: every page keeps its number of
    // lines, so no break moved.
    let lines = |p: &Pages| p.pages.iter().map(|p| p.lines.len()).collect::<Vec<_>>();
    assert_eq!(lines(&before), lines(&after), "the edit moved a page break");

    // The page itself is dressed as a box of the document; its body's
    // content is what the cache shares.
    let body = |p: &Page| {
        let body = p
            .fragment
            .items
            .iter()
            .find_map(|(_, item)| match item {
                FragmentItem::Group(body) => Some(body),
                _ => None,
            })
            .expect("a page holds its body");
        // Empty vectors share one pointer.
        assert!(!body.items.is_empty(), "an empty page");
        body.items.as_ptr()
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

#[test]
fn an_edit_that_forces_a_full_reparse_reuses_calls_of_unchanged_closures() {
    // An unbalanced `]` at the top level cannot be reparsed incrementally,
    // so the whole file is parsed again (#241). The items it leaves alone
    // keep their spans, so the calls of `g`, defined in the edited file,
    // and of `f`, from another file, are reused.
    let main = "#import \"lib.fol\": f\n\n\
                #let g(x) = x + 1\n\n\
                #g(2) and #f(3).\n\n\
                Some text here.\n\n\
                More text after.\n";
    let (mut world, counts) = counted(main);
    world.set_source("lib.fol", "#let f(x) = x * 2\n").unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    assert!(counts.lock().unwrap().contains_key("call_closure"));

    for (needle, with) in [("Some", "]"), ("#let", "] "), ("More", "]] ")] {
        counts.lock().unwrap().clear();
        let at = world
            .source("main.fol")
            .unwrap()
            .text()
            .find(needle)
            .unwrap();
        world.edit_source("main.fol", at..at, with).unwrap();
        folio::compile(&world, Target::Pdf, None).unwrap();
        let counts = counts.lock().unwrap();
        // The edit reached the output.
        assert!(counts.contains_key("compile_build"), "{counts:?}");
        assert!(!counts.contains_key("call_closure"), "{with:?}: {counts:?}");
    }

    // A change to the closure itself still runs its call again.
    counts.lock().unwrap().clear();
    let at = world
        .source("main.fol")
        .unwrap()
        .text()
        .find("x + 1")
        .unwrap();
    world.edit_source("main.fol", at..at + 5, "x + 2]").unwrap();
    folio::compile(&world, Target::Pdf, None).unwrap();
    assert_eq!(counts.lock().unwrap().get("call_closure"), Some(&1));
}
