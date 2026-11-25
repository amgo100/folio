//! The compile pipeline of SPEC §2.1 as memoised queries (SPEC §2.3): a
//! build runs end to end to a PDF, an edit to one paragraph re-runs only
//! the queries that depend on it, the pipeline forks by target after Step
//! styling, and compilation is partial (SPEC §12.2).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use folio::style::NodeName;
use folio::{Event, Name, Severity, Target, World};
use folio_component::{Color, Value};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf", "docx", "live"]

[variants.markscheme]
targets = ["pdf"]
"#;

const MAIN: &str = "\
#doc(title: [Mechanics])

= Forces

#include \"questions/q1.fol\"

#include \"questions/q2.fol\"
";

const Q1: &str = "\
A ball is thrown upwards at 12 m/s. Find its greatest height.

The ball lands on a slope inclined at 30 degrees.
";

const Q2: &str = "\
A trolley of mass 2 kg is pushed along a smooth track.

Its speed doubles in 4 s. Find the force on it.
";

const STYLE: &str = "\
page { page-size: A5; }
doc { font: \"Source Serif 4\"; }
heading { font-size: 14pt; }
";

/// A world with the project above, and the names of the queries it has
/// executed since the log was last taken.
struct Logged {
    world: World,
    log: Arc<Mutex<Vec<String>>>,
}

impl Logged {
    fn new() -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let mut world = World::builder()
            .on_event(move |event| {
                if let Event::Execute { query } = event {
                    sink.lock().unwrap().push(query.to_string());
                }
            })
            .build(MANIFEST, None)
            .unwrap();
        world.set_source("main.fol", MAIN).unwrap();
        world.set_source("questions/q1.fol", Q1).unwrap();
        world.set_source("questions/q2.fol", Q2).unwrap();
        world.set_source("styles/main.fsty", STYLE).unwrap();
        Self { world, log }
    }

    fn compile(&self, variant: &str, target: Target) -> folio::Compiled {
        let variant = Name::new(variant).unwrap();
        folio::compile(&self.world, target, Some(&variant)).unwrap()
    }

    /// How often each query ran since the log was last taken.
    fn take(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for query in std::mem::take(&mut *self.log.lock().unwrap()) {
            *counts.entry(query).or_insert(0) += 1;
        }
        counts
    }
}

/// Query names with how often each ran; an item may hold several names,
/// separated by spaces, that ran once each.
fn counts(items: &[(&str, usize)]) -> BTreeMap<String, usize> {
    items
        .iter()
        .flat_map(|&(names, n)| names.split_whitespace().map(move |q| (q.to_owned(), n)))
        .collect()
}

/// The stages a build runs from Style to Emit (SPEC §2.1), and the build
/// queries around them.
const STAGES: &str = "styles select sequence resolve step_style text lines pages emit";

#[test]
fn compiles_end_to_end_to_pdf() {
    let logged = Logged::new();
    let compiled = logged.compile("exam", Target::Pdf);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let pdf = compiled.output.expect("a PDF");
    assert!(pdf.starts_with(b"%PDF-1.7\n"));
    assert!(pdf.ends_with(b"%%EOF\n"));

    // One page in the PDF per page laid out.
    let exam = Name::new("exam").unwrap();
    let pages = folio::pages(&logged.world, Target::Pdf, Some(&exam))
        .unwrap()
        .unwrap();
    let count = pdf.windows(12).filter(|w| w == b"/Type /Page ").count();
    assert_eq!(count, pages.pages.len());

    // Every stage ran once, and so did the build's queries.
    let executed = logged.take();
    for stage in STAGES
        .split_whitespace()
        .chain(["compile_build", "build_output"])
    {
        assert_eq!(executed.get(stage), Some(&1), "{stage}: {executed:?}");
    }

    // The same inputs give the same bytes in a new world (SPEC §2.4).
    let again = Logged::new().compile("exam", Target::Pdf);
    assert_eq!(again.output.as_deref(), Some(&*pdf));
}

#[test]
fn an_edit_to_one_paragraph_reruns_only_what_depends_on_it() {
    let mut logged = Logged::new();
    let before = logged.compile("exam", Target::Pdf);
    assert!(!before.has_errors(), "{:?}", before.diagnostics);
    logged.take();

    // A word of the second question's first paragraph. Its file is
    // reparsed and checked, and evaluated again, and so is the entry file
    // that includes it, but not the first question's file. The reparse
    // renumbered the spans of the file's nodes, which the styled tree
    // records, so every stage from Style to Emit runs once; matching finds
    // every node's rules memoised, and the cascade, the fonts and the
    // component host are not touched. The PDF is the same (its pages are
    // blank until the PDF writer draws them), so the build query is cut
    // off.
    let edited = Q2.replace("pushed", "pulled");
    logged
        .world
        .set_source("questions/q2.fol", &edited)
        .unwrap();
    let after = logged.compile("exam", Target::Pdf);
    assert!(!after.has_errors(), "{:?}", after.diagnostics);
    assert_eq!(
        logged.take(),
        counts(&[
            ("dependencies check_source", 1),
            // `questions/q2.fol` and `main.fol`.
            ("eval_file", 2),
            ("evaluate document check_fonts", 1),
            (STAGES, 1),
        ])
    );

    // A paragraph split in two adds a node to the document. Matching is
    // keyed by what rules can see of a node, and no rule tells the new
    // paragraph from the others, so no node is matched again.
    let split = edited.replace("smooth track.", "smooth\n\ntrack.");
    logged.world.set_source("questions/q2.fol", &split).unwrap();
    logged.compile("exam", Target::Pdf);
    let executed = logged.take();
    for stage in STAGES.split_whitespace() {
        assert_eq!(executed.get(stage), Some(&1), "{stage}: {executed:?}");
    }
    for query in [
        "match_node",
        "cascade",
        "fonts",
        "component_host",
        "build_files",
    ] {
        assert!(!executed.contains_key(query), "{query}: {executed:?}");
    }

    // The other variant shares the evaluation of the files, which do not
    // read `variant`: it runs its own stages from the document model on.
    logged.compile("markscheme", Target::Pdf);
    let executed = logged.take();
    assert!(!executed.contains_key("eval_file"), "{executed:?}");
    for stage in STAGES.split_whitespace() {
        assert_eq!(executed.get(stage), Some(&1), "{stage}: {executed:?}");
    }

    // Compiling again runs nothing.
    logged.compile("exam", Target::Pdf);
    logged.compile("markscheme", Target::Pdf);
    assert!(logged.take().is_empty());
}

#[test]
fn the_pipeline_forks_after_step_styling() {
    let logged = Logged::new();

    // DOCX takes the step-styled tree, without laying it out, and has no
    // writer yet.
    let docx = logged.compile("exam", Target::Docx);
    let executed = logged.take();
    assert!(executed.contains_key("step_style"), "{executed:?}");
    for stage in ["text", "lines", "pages"] {
        assert!(!executed.contains_key(stage), "{stage}: {executed:?}");
    }
    assert_eq!(docx.output, None);
    assert_eq!(docx.diagnostics.len(), 1, "{:?}", docx.diagnostics);
    assert_eq!(docx.diagnostics[0].severity, Severity::Warning);
    assert!(
        docx.diagnostics[0]
            .message
            .starts_with("DOCX output is not")
    );

    // The live target is laid out like PDF, and writes no file.
    let live = logged.compile("exam", Target::Live);
    assert!(logged.take().contains_key("pages"));
    assert_eq!(live.output, None);
    assert_eq!(live.diagnostics.len(), 1, "{:?}", live.diagnostics);
    assert_eq!(live.diagnostics[0].severity, Severity::Warning);
}

#[test]
fn failing_constructs_become_error_markers_and_the_rest_compiles() {
    let mut logged = Logged::new();
    let edited = Q1.replace("Find its", "Find #(1 / 0) its");
    logged
        .world
        .set_source("questions/q1.fol", &edited)
        .unwrap();
    let compiled = logged.compile("exam", Target::Pdf);
    assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
    assert_eq!(compiled.diagnostics[0].message, "cannot divide by zero");
    assert!(compiled.output.is_some());

    // The marker is set in its paragraph, which keeps the rest of its
    // text, and the other paragraphs are all there.
    let exam = Name::new("exam").unwrap();
    let text = folio::text(&logged.world, Target::Pdf, Some(&exam))
        .unwrap()
        .unwrap();
    let paragraphs: Vec<&str> = text.paragraphs.iter().map(|p| p.shaped.text()).collect();
    assert_eq!(paragraphs.len(), 6, "{paragraphs:?}");
    assert_eq!(
        paragraphs[2],
        "A ball is thrown upwards at 12 m/s. Find [error] its greatest height."
    );

    // A stylesheet styles markers like any element.
    logged
        .world
        .set_source(
            "styles/main.fsty",
            &format!("{STYLE}error {{ color: #c00000; }}\n"),
        )
        .unwrap();
    let styled = folio::styles(&logged.world, Target::Pdf, Some(&exam))
        .unwrap()
        .unwrap();
    let mut found = false;
    let mut stack = vec![&styled.root];
    while let Some(node) = stack.pop() {
        if matches!(&node.name, NodeName::Element(name) if name.to_string() == "error") {
            assert_eq!(
                styled.value(node, "color"),
                Some(&Value::Color(Color::rgb(0xc0, 0, 0)))
            );
            found = true;
        }
        stack.extend(&node.children);
    }
    assert!(found);
}
