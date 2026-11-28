//! The query database behind `folio::World` and `folio::compile` (SPEC §2.3):
//! early cutoff, cancellation, durability, parallel builds, and the world's
//! file API.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use folio::{Cancelled, Event, FileKind, Name, Target, World, WorldError};
use folio_syntax::DiagSpanKind;

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/exam.fsty"]

[variants.exam]
targets = ["pdf", "docx"]

[variants.markscheme]
targets = ["pdf"]
styles = ["styles/markscheme.fsty"]
"#;

const MAIN: &str = "#include \"questions/q1.fol\"\n\n= Forces\n\nA ball is thrown.\n";

/// A world with a project of three files, and the names of the queries it
/// has executed since the log was last taken.
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
        world
            .set_source("questions/q1.fol", "A question.\n")
            .unwrap();
        world
            .set_source(
                "styles/exam.fsty",
                "@import \"base.fsty\";\nheading { font-size: 14pt; }\n",
            )
            .unwrap();
        world
            .set_source("styles/base.fsty", "paragraph { indent: 0; }\n")
            .unwrap();
        world
            .set_source("styles/markscheme.fsty", "answer { display: block; }\n")
            .unwrap();
        Self { world, log }
    }

    fn take(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
            .into_iter()
            .collect()
    }

    fn compile(&self, variant: &str) -> folio::Compiled {
        let variant = Name::new(variant).unwrap();
        folio::compile(&self.world, Target::Pdf, Some(&variant)).unwrap()
    }
}

/// The queries that run when a variant's entry file changed, written as one
/// name for [`set`]: the file's evaluation, the variant's, and its document
/// tree; the styling of the tree, which finds every node's match memoised
/// but records the spans of the nodes, which the reparse renumbered, so the
/// stages that pass the styled tree on run too (Select, Sequence, Resolve
/// and Step styling); the text stage, whose paragraphs record the spans of
/// their text, in its two queries, the collection of the paragraphs and
/// their shaping (memoised apart per paragraph); the check of the fonts the
/// collected text is set in; line and page breaking, which read the text;
/// and Emit, which reads the pages.
const EVAL: &str = "eval_file evaluate document styles select sequence resolve_counters resolve step_style \
                    check_fonts collect_text text lines pages pdf_input pdf_check emit";

/// A set of query names; an item may hold several, separated by spaces.
fn set(names: &[&str]) -> BTreeSet<String> {
    names
        .iter()
        .flat_map(|s| s.split(' '))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn compiles_a_project() {
    let logged = Logged::new();
    let exam = logged.compile("exam");
    assert!(!exam.has_errors(), "{:?}", exam.diagnostics);
    let sources: Vec<_> = exam.sources.iter().map(|p| p.as_str()).collect();
    assert_eq!(
        sources,
        [
            "main.fol",
            "styles/exam.fsty",
            "questions/q1.fol",
            "styles/base.fsty"
        ]
    );
    // Every file read, with how it was read.
    let files: Vec<_> = exam
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect();
    assert_eq!(
        files,
        [
            ("main.fol", FileKind::Content),
            ("styles/exam.fsty", FileKind::Stylesheet),
            ("questions/q1.fol", FileKind::Content),
            ("styles/base.fsty", FileKind::Stylesheet)
        ]
    );
    let markscheme = logged.compile("markscheme");
    assert_eq!(markscheme.sources.len(), 5);
    assert_eq!(markscheme.sources[2].as_str(), "styles/markscheme.fsty");
}

#[test]
fn early_cutoff_on_a_whitespace_edit() {
    let mut logged = Logged::new();
    let before = logged.compile("exam");
    assert!(logged.take().contains("build_output"));

    // Whitespace in a paragraph: the file is reparsed and its three
    // per-file queries re-run, but return what they returned before, so the
    // file set of the build and the build's checks are not re-run. The file
    // is evaluated again, because its text spans changed, and so are the
    // stages that carry those spans to the pages' glyphs (SPEC §12.2). The
    // PDF is the same, so the build query is cut off.
    let edited = MAIN.replace("ball is", "ball   is");
    logged.world.set_source("main.fol", &edited).unwrap();
    let after = logged.compile("exam");
    assert_eq!(after, before);
    assert_eq!(
        logged.take(),
        set(&[
            "dependencies",
            "check_source",
            "variant_conditions",
            "file_elements",
            EVAL
        ])
    );

    // An edit that changes the imports re-runs the file set. The file it
    // adds does not exist, so the set is the same and the build's checks
    // are cut off, but the missing file is reported, once, though both the
    // file set and evaluation find it. Evaluation turns the include into
    // an error marker (SPEC §12.2), a new node that is matched against the
    // rules; the rest of the document still compiles. The PDF shows the
    // marker, so the build query runs again.
    let edited = format!("{edited}#include \"questions/q2.fol\"\n");
    logged.world.set_source("main.fol", &edited).unwrap();
    let after = logged.compile("exam");
    assert_eq!(
        logged.take(),
        set(&[
            "build_files",
            "dependencies",
            "check_source",
            "variant_conditions",
            "file_elements",
            "project_elements",
            "source_file",
            "match_node",
            "compile_build",
            EVAL
        ])
    );
    assert!(after.output.is_some());
    assert_eq!(after.diagnostics.len(), 1);
    assert_eq!(
        after.diagnostics[0].message,
        "file `questions/q2.fol` not found"
    );

    // Adding the missing file re-runs the lookup of every file, which
    // returns the same file for the others.
    logged
        .world
        .set_source("questions/q2.fol", "Another question.\n")
        .unwrap();
    assert!(!logged.compile("exam").has_errors());
    let executed = logged.take();
    assert!(executed.contains("source_file"));
    assert!(executed.contains("build_output"));
    assert!(!executed.contains("component_host"));
}

#[test]
fn diagnostics_change_while_the_build_is_cut_off() {
    let mut logged = Logged::new();
    assert!(!logged.compile("exam").has_errors());
    logged.take();

    // An unclosed call in a paragraph: the file's check reports it, but its
    // imports are the same, so the build's checks are not re-run, and the
    // diagnostic is still collected. Evaluation turns the call into an
    // error marker without adding errors of its own, and the rest of the
    // document still compiles (SPEC §12.2).
    let broken = MAIN.replace("A ball", "#box( A ball");
    logged.world.set_source("main.fol", &broken).unwrap();
    let compiled = logged.compile("exam");
    let syntax = logged.world.source("main.fol").unwrap();
    let (syntax_errors, _) = syntax.root().errors_and_warnings();
    assert!(!syntax_errors.is_empty());
    assert_eq!(
        compiled.diagnostics.len(),
        syntax_errors.len(),
        "{:?}",
        compiled.diagnostics
    );
    assert!(compiled.output.is_some());
    // The marker is a new node, matched against the rules, and the PDF
    // shows it, so the build query runs again.
    assert_eq!(
        logged.take(),
        set(&[
            "dependencies",
            "check_source",
            "variant_conditions",
            "file_elements",
            "match_node",
            "compile_build",
            EVAL
        ])
    );

    // Fixing it removes the diagnostic, again without re-running the build's
    // checks; the PDF loses the marker.
    logged.world.set_source("main.fol", MAIN).unwrap();
    let compiled = logged.compile("exam");
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    assert_eq!(
        logged.take(),
        set(&[
            "dependencies",
            "check_source",
            "variant_conditions",
            "file_elements",
            "compile_build",
            EVAL
        ])
    );

    // A diagnostic of a file that two builds share is reported by one
    // query, and each build collects it once.
    logged
        .world
        .set_source("styles/base.fsty", "paragraph { indent: ; }\n")
        .unwrap();
    for variant in ["exam", "markscheme"] {
        let compiled = logged.compile(variant);
        let base = logged.world.id("styles/base.fsty");
        let count = compiled
            .diagnostics
            .iter()
            .filter(|d| d.span.id() == base)
            .count();
        assert_eq!(count, 1, "{variant}: {:?}", compiled.diagnostics);
    }
}

#[test]
fn variants_share_queries_that_do_not_read_them() {
    let logged = Logged::new();
    logged.compile("exam");
    logged.take();

    // The mark scheme reads the same files plus its overlay: only that
    // overlay's per-file queries and the per-build queries run, and the
    // component host is shared.
    logged.compile("markscheme");
    let log = std::mem::take(&mut *logged.log.lock().unwrap());
    let count = |query: &str| log.iter().filter(|q| *q == query).count();
    assert_eq!(count("build_output"), 1);
    assert_eq!(count("build_files"), 1);
    assert_eq!(count("dependencies"), 1);
    assert_eq!(count("check_source"), 1);
    assert_eq!(count("component_host"), 0);

    // Compiling a build again runs nothing.
    logged.compile("exam");
    logged.compile("markscheme");
    assert!(logged.take().is_empty());
}

#[test]
fn equal_inputs_start_no_revision() {
    let mut logged = Logged::new();
    logged.compile("exam");
    logged.take();
    logged.world.set_manifest(MANIFEST).unwrap();
    logged.world.set_source("main.fol", MAIN).unwrap();
    logged.world.set_lockfile(None).unwrap();
    logged.compile("exam");
    assert!(logged.take().is_empty());

    // A changed manifest re-runs the component host, which is cut off.
    let manifest = format!("{MANIFEST}\n[components]\ndisable = [\"exam\"]\n");
    logged.world.set_manifest(&manifest).unwrap();
    logged.compile("exam");
    let executed = logged.take();
    assert!(executed.contains("component_host"), "{executed:?}");
}

#[test]
fn an_edit_cancels_a_running_compilation() {
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let resume_tx = Mutex::new(resume_tx);
    let resume_rx = Mutex::new(resume_rx);
    let armed = Arc::new(AtomicBool::new(false));

    let hook_armed = armed.clone();
    let mut world = World::builder()
        .on_event(move |event| {
            if !hook_armed.load(Ordering::SeqCst) {
                return;
            }
            match event {
                // The build has started on the worker: tell the test, then
                // wait until the edit has asked running queries to stop.
                Event::Execute { query } if query == "build_output" => {
                    started_tx.send(()).unwrap();
                    resume_rx.lock().unwrap().recv().unwrap();
                }
                // The edit, on the test's thread: let the worker go on.
                Event::Cancel => resume_tx.lock().unwrap().send(()).unwrap(),
                _ => {}
            }
        })
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", "= Forces\n").unwrap();
    for path in ["styles/exam.fsty", "styles/markscheme.fsty"] {
        world.set_source(path, "").unwrap();
    }

    armed.store(true, Ordering::SeqCst);
    let handle = world.clone();
    let worker = thread::spawn(move || {
        let exam = Name::new("exam").unwrap();
        folio::compile(&handle, Target::Pdf, Some(&exam))
    });
    started_rx.recv().unwrap();

    // Blocks until the worker has unwound and dropped its handle.
    world.set_source("main.fol", "= Energy\n").unwrap();
    assert_eq!(worker.join().unwrap(), Err(Cancelled));

    armed.store(false, Ordering::SeqCst);
    let exam = Name::new("exam").unwrap();
    let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
}

/// A world with the project of [`Logged`] whose `query` panics, a bug
/// injected through the event hook, while the flag is set.
fn panicking(long_lived: bool, query: &'static str) -> (World, Arc<AtomicBool>) {
    let armed = Arc::new(AtomicBool::new(false));
    let hook_armed = armed.clone();
    let mut world = World::builder()
        .long_lived(long_lived)
        .on_event(move |event| {
            if let Event::Execute { query: q } = event
                && q == query
                && hook_armed.load(Ordering::SeqCst)
            {
                panic!("injected into `{query}`");
            }
        })
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", MAIN).unwrap();
    world
        .set_source("questions/q1.fol", "A question.\n")
        .unwrap();
    for path in ["styles/exam.fsty", "styles/markscheme.fsty"] {
        world.set_source(path, "").unwrap();
    }
    (world, armed)
}

#[test]
fn a_panic_in_a_query_is_a_diagnostic_in_a_long_lived_world() {
    let (mut world, armed) = panicking(true, "lines");
    assert!(world.is_long_lived());
    let exam = Name::new("exam").unwrap();
    let good = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert!(!good.has_errors(), "{:?}", good.diagnostics);
    assert!(good.output.is_some());

    // An edit whose compilation panics: an internal error, which keeps the
    // last good output and what the build read.
    world.set_source("main.fol", "= Energy\n").unwrap();
    armed.store(true, Ordering::SeqCst);
    let failed = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert_eq!(failed.diagnostics.len(), 1, "{:?}", failed.diagnostics);
    let error = &failed.diagnostics[0];
    assert!(error.is_error());
    assert_eq!(error.code.as_deref(), Some("internal-error"));
    assert_eq!(
        error.message,
        "internal error: the compiler panicked: injected into `lines`"
    );
    assert!(matches!(error.span.get(), DiagSpanKind::Detached));
    assert_eq!(error.hints.len(), 2, "{:?}", error.hints);
    assert_eq!(failed.output, good.output);
    // What the build last read, then the world's other files: the build
    // that panicked may have read more.
    assert!(failed.sources.starts_with(&good.sources));
    assert!(failed.files.starts_with(&good.files));
    assert!(
        failed
            .sources
            .iter()
            .any(|p| p.as_str() == "styles/markscheme.fsty")
    );
    assert!(
        !good
            .sources
            .iter()
            .any(|p| p.as_str() == "styles/markscheme.fsty")
    );

    // A check panics alike.
    let checked = folio::check(&world, Target::Pdf, Some(&exam)).unwrap();
    assert_eq!(checked.diagnostics.len(), 1, "{:?}", checked.diagnostics);
    assert_eq!(
        checked.diagnostics[0].code.as_deref(),
        Some("internal-error")
    );
    assert_eq!(checked.sources, failed.sources);
    assert!(checked.writes);

    // Once the bug is gone, the next compilation succeeds, in the same
    // process and on the same database.
    armed.store(false, Ordering::SeqCst);
    let fixed = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert!(!fixed.has_errors(), "{:?}", fixed.diagnostics);
    assert!(fixed.output.is_some());
    assert_ne!(fixed.output, good.output);

    // A build the manifest no longer declares is forgotten once another
    // compiles, so it keeps no output if it comes back.
    let markscheme = Name::new("markscheme").unwrap();
    let without_exam = MANIFEST.replace("[variants.exam]\ntargets = [\"pdf\", \"docx\"]\n", "");
    assert_ne!(without_exam, MANIFEST);
    world.set_manifest(&without_exam).unwrap();
    let compiled = folio::compile(&world, Target::Pdf, Some(&markscheme)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    world.set_manifest(MANIFEST).unwrap();
    world.set_source("main.fol", "= Momentum\n").unwrap();
    armed.store(true, Ordering::SeqCst);
    let failed = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert_eq!(
        failed.diagnostics[0].code.as_deref(),
        Some("internal-error")
    );
    assert_eq!(failed.output, None);
}

#[test]
fn a_panic_in_a_shared_query_is_a_diagnostic_in_every_parallel_build() {
    // Every build reads the files' dependencies, whichever thread runs
    // them first: the others wait for it, and see its panic.
    let (world, armed) = panicking(true, "dependencies");
    armed.store(true, Ordering::SeqCst);
    let outputs = folio::compile_all(&world);
    assert_eq!(outputs.len(), 3);
    for output in outputs {
        let compiled = output.result.unwrap();
        assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
        assert_eq!(
            compiled.diagnostics[0].code.as_deref(),
            Some("internal-error")
        );
        // Nothing compiled before, so there is no output to keep; a watcher
        // watches every file of the world, so the edit that works around
        // the bug compiles again.
        assert_eq!(compiled.output, None);
        assert_eq!(compiled.diagnostics[0].hints.len(), 1);
        let sources: Vec<_> = compiled.sources.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            sources,
            [
                "main.fol",
                "questions/q1.fol",
                "styles/exam.fsty",
                "styles/markscheme.fsty"
            ]
        );
        assert!(
            compiled
                .files
                .iter()
                .any(|f| f.path.as_str() == "styles/exam.fsty" && f.kind == FileKind::Stylesheet)
        );
    }

    armed.store(false, Ordering::SeqCst);
    for output in folio::compile_all(&world) {
        let compiled = output.result.unwrap();
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    }
}

#[test]
fn a_panic_in_a_query_unwinds_in_an_ordinary_world() {
    let (world, armed) = panicking(false, "lines");
    assert!(!world.is_long_lived());
    armed.store(true, Ordering::SeqCst);
    let exam = Name::new("exam").unwrap();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        folio::compile(&world, Target::Pdf, Some(&exam))
    }));
    let payload = panicked.unwrap_err();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert_eq!(message, "injected into `lines`");
}

/// A query that an edit stops gives up its claim as if it had panicked,
/// so a handle waiting for it on another thread sees a panic there: an
/// edit, not a bug, in either kind of world.
#[test]
fn an_edit_is_not_a_panic_for_a_handle_that_waits() {
    for long_lived in [false, true] {
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let resume_tx = Mutex::new(resume_tx);
        let resume_rx = Mutex::new(resume_rx);
        let started_tx = Mutex::new(started_tx);
        let armed = Arc::new(AtomicBool::new(false));
        let hook_armed = armed.clone();
        let waiting = AtomicBool::new(false);
        let mut world = World::builder()
            .long_lived(long_lived)
            .on_event(move |event| match event {
                // The first handle reaches `lines`: wait for the edit,
                // then go on, to stop at the next query it calls.
                Event::Execute { query }
                    if query == "lines" && hook_armed.swap(false, Ordering::SeqCst) =>
                {
                    waiting.store(true, Ordering::SeqCst);
                    started_tx.lock().unwrap().send(()).unwrap();
                    resume_rx.lock().unwrap().recv().unwrap();
                }
                Event::Cancel if waiting.swap(false, Ordering::SeqCst) => {
                    resume_tx.lock().unwrap().send(()).unwrap();
                }
                _ => {}
            })
            .build(MANIFEST, None)
            .unwrap();
        world.set_source("main.fol", MAIN).unwrap();
        world
            .set_source("questions/q1.fol", "A question.\n")
            .unwrap();
        for path in ["styles/exam.fsty", "styles/markscheme.fsty"] {
            world.set_source(path, "").unwrap();
        }

        armed.store(true, Ordering::SeqCst);
        let compile = |world: World| {
            thread::spawn(move || {
                let exam = Name::new("exam").unwrap();
                folio::compile(&world, Target::Pdf, Some(&exam))
            })
        };
        let first = compile(world.clone());
        started_rx.recv().unwrap();
        // The second handle waits for the first's queries.
        let second = compile(world.clone());
        thread::sleep(std::time::Duration::from_millis(200));

        world.set_source("main.fol", "= Energy\n").unwrap();
        assert_eq!(first.join().unwrap(), Err(Cancelled), "{long_lived}");
        assert_eq!(second.join().unwrap(), Err(Cancelled), "{long_lived}");
    }
}

#[test]
fn compiles_all_builds_in_parallel() {
    let logged = Logged::new();
    let outputs = folio::compile_all(&logged.world);
    let builds: Vec<_> = outputs
        .iter()
        .map(|o| (o.variant.as_ref().unwrap().as_str(), o.target))
        .collect();
    assert_eq!(
        builds,
        [
            ("exam", Target::Pdf),
            ("exam", Target::Docx),
            ("markscheme", Target::Pdf)
        ]
    );
    assert!(
        outputs
            .iter()
            .all(|o| o.result.as_ref().is_ok_and(|c| !c.has_errors()))
    );
    // Each file's queries ran once, whichever thread got there first.
    let log = logged.log.lock().unwrap();
    assert_eq!(log.iter().filter(|q| *q == "build_output").count(), 3);
    assert_eq!(log.iter().filter(|q| *q == "dependencies").count(), 5);

    let plain =
        World::new("[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n")
            .unwrap();
    assert_eq!(plain.builds(), [(None, Target::Pdf)]);
}

#[test]
fn diagnostics() {
    let mut logged = Logged::new();
    let world = &mut logged.world;

    // A missing entry file points at the manifest.
    assert!(world.remove_source("main.fol"));
    assert!(!world.remove_source("main.fol"));
    let exam = logged.compile("exam");
    assert_eq!(exam.diagnostics[0].message, "file `main.fol` not found");
    let id = exam.diagnostics[0].span.id().unwrap();
    assert_eq!(logged.world.path(id), Some("folio.toml"));

    // Syntax errors and imports that leave the project point at the source.
    let id = logged
        .world
        .set_source(
            "main.fol",
            "#import \"@school/house\": box\n#include \"../../secret.fol\"\n#(\n",
        )
        .unwrap();
    let exam = logged.compile("exam");
    let messages: Vec<_> = exam
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(messages.contains(&"`../../secret.fol` is not a path in the project"));
    // A package is not a project file.
    assert!(
        !messages.iter().any(|m| m.contains("house")),
        "{messages:?}"
    );
    assert!(exam.diagnostics.len() >= 2);
    for diag in &exam.diagnostics {
        assert!(
            matches!(diag.span.get(), DiagSpanKind::Number { .. }),
            "{diag}"
        );
        assert_eq!(diag.span.id(), Some(id));
    }

    // Builds the manifest does not declare.
    let world = &logged.world;
    let pptx = folio::compile(world, Target::Pptx, Name::new("exam").as_ref()).unwrap();
    assert_eq!(
        pptx.diagnostics[0].message,
        "variant `exam` is not built for pptx"
    );
    let unknown = folio::compile(world, Target::Pdf, Name::new("quiz").as_ref()).unwrap();
    assert_eq!(
        unknown.diagnostics[0].message,
        "no variant `quiz` is declared"
    );
    assert_eq!(
        unknown.diagnostics[0].hints[0],
        "the manifest declares `exam`, `markscheme`"
    );
    let none = folio::compile(world, Target::Pdf, None).unwrap();
    assert_eq!(
        none.diagnostics[0].message,
        "no variant was chosen, but the manifest declares variants"
    );
}

#[test]
fn files_and_ids() {
    let mut world = World::new(MANIFEST).unwrap();
    assert_eq!(world.path(World::MANIFEST), Some("folio.toml"));
    assert_eq!(world.path(World::LOCKFILE), Some("folio.lock"));

    let main = world.set_source("main.fol", "Hello").unwrap();
    let style = world.set_source("styles/../exam.fsty", "").unwrap();
    assert_ne!(main, style);
    assert_eq!(world.path(main), Some("main.fol"));
    assert_eq!(world.id("exam.fsty"), Some(style));
    assert_eq!(world.set_source("main.fol", "Hello, world").unwrap(), main);
    assert_eq!(world.source("main.fol").unwrap().text(), "Hello, world");

    // A removed source's id is freed; a source added soon after gets a new
    // one.
    world.remove_source("main.fol");
    assert!(world.source("main.fol").is_none());
    assert_eq!(world.path(main), None);
    assert_eq!(world.id("main.fol"), None);
    let main = world.set_source("main.fol", "Again").unwrap();
    assert_ne!(Some(main), world.id("exam.fsty"));

    // Edits reparse in place.
    let reparsed = world.edit_source("main.fol", 0..5, "Once more").unwrap();
    assert!(reparsed.end <= "Once more".len());
    assert_eq!(world.source("main.fol").unwrap().text(), "Once more");

    fn err<T: std::fmt::Debug>(result: Result<T, WorldError>) -> String {
        result.unwrap_err().to_string()
    }
    assert_eq!(
        err(world.set_source("../x.fol", "")),
        "`../x.fol` is not a path in the project"
    );
    assert_eq!(
        err(world.set_source("data.csv", "")),
        "`data.csv` is not a source: sources are `.fol` and `.fsty` files"
    );
    assert_eq!(
        err(world.edit_source("none.fol", 0..0, "")),
        "there is no source `none.fol`"
    );
    assert_eq!(
        err(world.edit_source("main.fol", 3..99, "")),
        "3..99 is not a range of the text"
    );

    assert_eq!(
        err(world.set_file("main.fol", b"x".to_vec())),
        "`main.fol` is a source: set it with `set_source`"
    );
    // An edit that changes nothing starts no revision.
    assert_eq!(world.edit_source("main.fol", 4..4, "").unwrap(), 4..4);

    world
        .set_file("data/points.csv", b"x,y\n".to_vec())
        .unwrap();
    world
        .set_file("data/points.csv", b"x,y\n".to_vec())
        .unwrap();
    assert!(world.remove_file("data/points.csv"));
    assert!(!world.remove_file("data/points.csv"));

    let errors = world.set_manifest("[project]\n").unwrap_err();
    assert!(!errors.is_empty());
    assert_eq!(world.manifest().variants.len(), 2);
}

/// File ids are recycled: a long session that sees any number of paths
/// does not run out of them.
#[test]
fn adds_and_removes_many_paths() {
    let mut world = World::new(MANIFEST).unwrap();
    let kept = world.set_source("main.fol", "Kept").unwrap();
    let mut seen = BTreeSet::new();
    for i in 0..100_000 {
        let path = format!("drafts/{i}.fol");
        let id = world.set_source(&path, "A draft.").unwrap();
        assert_eq!(world.path(id), Some(path.as_str()));
        seen.insert(id);
        assert!(world.remove_source(&path));
    }
    // Ids were new until every one had been given out, and freed ones were
    // given out again after that.
    assert!((60_000..100_000).contains(&seen.len()), "{}", seen.len());
    assert_eq!(world.path(kept), Some("main.fol"));

    // Freed ids are given out lowest first.
    let ids: Vec<_> = (0..4)
        .map(|i| world.set_source(&format!("{i}.fol"), "").unwrap())
        .collect();
    world.remove_source("1.fol");
    world.remove_source("2.fol");
    assert_eq!(world.set_source("new.fol", "").unwrap(), ids[1]);
    assert_eq!(world.path(ids[2]), None);
}

/// A snapshot resolves diagnostics after the world has changed, and holds
/// no handle on the database, so an edit does not wait for it.
#[test]
fn snapshots_resolve_diagnostics_after_edits() {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", "#let x = \n").unwrap();
    let exam = Name::new("exam").unwrap();
    let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    let snapshot = world.snapshot();
    let before = folio::diag::render(&snapshot, &compiled.diagnostics, false);

    // The edit goes ahead while the snapshot is kept.
    world.remove_source("main.fol");
    world.set_source("other.fol", "Something else\n").unwrap();
    assert_eq!(
        folio::diag::render(&snapshot, &compiled.diagnostics, false),
        before
    );
    assert!(before.contains("main.fol"), "{before}");
}

/// A file on disk is read the first time something asks for it, and then
/// kept until it is set again (SPEC §12.1).
#[test]
fn reads_a_file_on_disk_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let disk = dir.path().join("points.csv");
    let mut world = World::new(MANIFEST).unwrap();
    world.set_file_on_disk("data/points.csv", &disk).unwrap();
    // Not there yet: nothing has read it.
    std::fs::write(&disk, "x,y\n").unwrap();
    assert_eq!(&*world.file("data/points.csv").unwrap(), b"x,y\n");
    std::fs::write(&disk, "x,y\n1,2\n").unwrap();
    assert_eq!(&*world.file("data/points.csv").unwrap(), b"x,y\n");
    world.set_file_on_disk("data/points.csv", &disk).unwrap();
    assert_eq!(&*world.file("data/points.csv").unwrap(), b"x,y\n1,2\n");

    // A file that cannot be read is not there.
    world
        .set_file_on_disk("data/none.csv", dir.path().join("none.csv"))
        .unwrap();
    assert!(world.file_paths().any(|p| p.as_str() == "data/none.csv"));
    assert!(world.file("data/none.csv").is_none());
    assert!(world.set_file_on_disk("main.fol", &disk).is_err());
}

/// Adding many files one by one takes time linear in their number (#342).
#[test]
fn adds_many_files() {
    let mut world = World::new(MANIFEST).unwrap();
    for i in 0..20_000 {
        world
            .set_file(&format!("data/{i}.csv"), b"x".to_vec())
            .unwrap();
    }
    assert_eq!(world.file_paths().count(), 20_000);
    for i in 0..20_000 {
        assert!(world.remove_file(&format!("data/{i}.csv")));
    }
    assert_eq!(world.file_paths().count(), 0);
}

/// A text data file gets a file id that errors can point into; an image
/// does not, and removing the file frees its id.
#[test]
fn text_data_files_have_ids() {
    let mut world = World::new(MANIFEST).unwrap();
    world
        .set_file("data/bank.yaml", b"a: 1\n".to_vec())
        .unwrap();
    world.set_file("figure.png", b"png".to_vec()).unwrap();
    let id = world.id("data/bank.yaml").unwrap();
    assert_eq!(world.path(id), Some("data/bank.yaml"));
    assert!(world.id("figure.png").is_none());
    assert!(world.remove_file("data/bank.yaml"));
    assert!(world.id("data/bank.yaml").is_none());
    assert_eq!(world.path(id), None);
}

/// `compile_all` builds every variant in every version of a shuffled
/// paper, each as `compile_versioned` builds it (SPEC §6.4).
#[test]
fn compile_all_builds_every_version() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/versions");
    let mut loader = folio::loader::Loader::new(&root).unwrap();
    let mut world = loader.world(World::builder()).unwrap();
    loader.load(&mut world, &mut Vec::new()).unwrap();
    let outputs = folio::compile_all(&world);
    let builds: Vec<(Option<&str>, Option<&str>)> = outputs
        .iter()
        .map(|o| {
            (
                o.variant.as_ref().map(Name::as_str),
                o.version.as_ref().map(Name::as_str),
            )
        })
        .collect();
    assert_eq!(
        builds,
        [
            (Some("exam"), Some("A")),
            (Some("markscheme"), Some("A")),
            (Some("exam"), Some("B")),
            (Some("markscheme"), Some("B")),
        ]
    );
    for output in outputs {
        let compiled = output.result.unwrap();
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        let alone = folio::compile_versioned(
            &world,
            output.target,
            output.variant.as_ref(),
            output.version.as_ref(),
        )
        .unwrap();
        assert_eq!(compiled.output, alone.output);
    }
}
