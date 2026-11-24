//! The query database behind `folio::World` and `folio::compile` (SPEC §2.3):
//! early cutoff, cancellation, durability, parallel builds, and the world's
//! file API.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use folio::{Cancelled, Event, Name, Target, World, WorldError};
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

/// The queries that evaluate a variant whose entry file changed, written as
/// one name for [`set`]: the file's evaluation, the variant's, its
/// document tree, and the build's check of it, which returns the same and so
/// cuts the build off; and the styling of the tree, which finds every node's
/// match memoised and returns the same, so the build's check of the styles
/// is cut off too.
const EVAL: &str = "eval_file evaluate document check_document styles";

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
    let markscheme = logged.compile("markscheme");
    assert_eq!(markscheme.sources.len(), 5);
    assert_eq!(markscheme.sources[2].as_str(), "styles/markscheme.fsty");
}

#[test]
fn early_cutoff_on_a_whitespace_edit() {
    let mut logged = Logged::new();
    let before = logged.compile("exam");
    assert!(logged.take().contains("build_output"));

    // Whitespace in a paragraph: the file is reparsed and its two
    // per-file queries re-run, but return what they returned before, so the
    // file set of the build and the build itself are not re-run. The file is
    // evaluated again, because its text spans changed.
    let edited = MAIN.replace("ball is", "ball   is");
    logged.world.set_source("main.fol", &edited).unwrap();
    let after = logged.compile("exam");
    assert_eq!(after, before);
    assert_eq!(logged.take(), set(&["dependencies", "check_source", EVAL]));

    // An edit that changes the imports re-runs the file set. The file it
    // adds does not exist, so the set is the same and the build is cut off,
    // but the missing file is reported, once, though both the file set and
    // evaluation find it.
    let edited = format!("{edited}#include \"questions/q2.fol\"\n");
    logged.world.set_source("main.fol", &edited).unwrap();
    let after = logged.compile("exam");
    assert_eq!(
        logged.take(),
        set(&[
            "build_files",
            "dependencies",
            "check_source",
            "source_file",
            // Evaluation fails, so there is no document to style.
            "check_styles",
            EVAL
        ])
    );
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
    // imports are the same, so the build query is not re-run, and the
    // diagnostic is still collected. Evaluation stops at the syntax error
    // without adding errors of its own.
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
    // The document is gone, so the build's check of the styles re-runs.
    assert_eq!(
        logged.take(),
        set(&["dependencies", "check_source", "check_styles", EVAL])
    );

    // Fixing it removes the diagnostic, again without re-running the build.
    logged.world.set_source("main.fol", MAIN).unwrap();
    let compiled = logged.compile("exam");
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    assert_eq!(
        logged.take(),
        set(&["dependencies", "check_source", "check_styles", EVAL])
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

    // Ids are never reused, even after a file is removed.
    world.remove_source("main.fol");
    assert!(world.source("main.fol").is_none());
    assert_eq!(world.set_source("main.fol", "Again").unwrap(), main);

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
