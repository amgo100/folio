//! Cancellation inside long queries (SPEC §2.3): an edit made while
//! another handle compiles a long document stops the compilation between
//! paragraphs and pages, not only between queries.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use folio::{Cancelled, Event, Target, World};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
lang = \"en-GB\"
";

/// A document of about the reference document's length (SPEC §14.1): a
/// hundred A4 pages of headings and paragraphs, generated
/// deterministically.
fn document() -> String {
    const WORDS: &str = "force mass the of acceleration a velocity is energy and momentum to \
        displacement in conserved field charge potential which gives resultant friction surface \
        particle system";
    let words: Vec<&str> = WORDS.split_whitespace().collect();
    let mut out = String::new();
    let mut state: u32 = 0x2545_f491;
    let mut next = || {
        // A xorshift, so that the document is the same on every run.
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as usize
    };
    for section in 0..100 {
        writeln!(out, "= Section {section}\n").unwrap();
        for _ in 0..5 {
            let count = 90 + next() % 40;
            for w in 0..count {
                if w > 0 {
                    out.push(' ');
                }
                out.push_str(words[next() % words.len()]);
            }
            out.push_str(".\n\n");
        }
    }
    out
}

/// Starts compiling `source` in a fresh world on another thread; once
/// `query` has run for `delay`, edits the world and returns how long the
/// edit waited for the compilation to stop.
fn wait(source: &str, query: &str, delay: Duration) -> Duration {
    let (started_tx, started_rx) = mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let armed = Arc::new(AtomicBool::new(true));
    let hook_armed = armed.clone();
    let name = query.to_owned();
    let mut world = World::builder()
        .on_event(move |event| {
            if let Event::Execute { query: q } = event
                && *q == name
                && hook_armed.swap(false, Ordering::SeqCst)
            {
                started_tx.lock().unwrap().send(Instant::now()).unwrap();
            }
        })
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", source).unwrap();

    let handle = world.clone();
    let worker = thread::spawn(move || folio::compile(&handle, Target::Pdf, None));
    let started = started_rx.recv().unwrap();
    thread::sleep(delay.saturating_sub(started.elapsed()));

    let edit = Instant::now();
    // Blocks until the worker has unwound and dropped its handle.
    world.set_source("main.fol", "= Energy\n").unwrap();
    let waited = edit.elapsed();
    assert_eq!(worker.join().unwrap(), Err(Cancelled), "{query}");
    waited
}

/// How long each long query takes in a fresh world, without the queries
/// it calls, and when to edit to land in the middle of it: queries start
/// before those they call, so `pages` starts before `lines` and `text` run.
fn stages(source: &str) -> Vec<(&'static str, Duration, Duration)> {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", source).unwrap();
    let time = |f: &dyn Fn()| {
        let start = Instant::now();
        f();
        start.elapsed()
    };
    // Evaluation and styling run inside `text`, before its own work.
    let styles = time(&|| drop(folio::styles(&world, Target::Pdf, None).unwrap().unwrap()));
    let text = time(&|| drop(folio::text(&world, Target::Pdf, None).unwrap().unwrap()));
    let lines = time(&|| drop(folio::lines(&world, Target::Pdf, None).unwrap().unwrap()));
    let pages = time(&|| drop(folio::pages(&world, Target::Pdf, None).unwrap().unwrap()));
    // The checks run before `emit` does.
    time(&|| drop(folio::check(&world, Target::Pdf, None).unwrap()));
    let emit = time(&|| {
        let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    });
    vec![
        ("text", text, styles + text / 2),
        ("lines", lines, styles + text + lines / 2),
        ("pages", pages, styles + text + lines + pages / 2),
        ("emit", emit, emit / 2),
    ]
}

/// An edit made in the middle of each long query of a compilation of a
/// hundred-page document stops it within a few paragraphs or a page: within a
/// few milliseconds in an optimised build, and within a small part of the
/// query's time in any build.
#[test]
fn an_edit_stops_a_long_query_within_milliseconds() {
    let source = document();
    for (query, took, delay) in stages(&source) {
        let waited = wait(&source, query, delay);
        eprintln!("{query}: took {took:?}, an edit in the middle waited {waited:?}");
        // A floor, so that a loaded machine does not fail a short query.
        assert!(
            waited < (took / 4).max(Duration::from_millis(25)),
            "an edit in the middle of `{query}`, which takes {took:?}, waited {waited:?}"
        );
        if !cfg!(debug_assertions) {
            assert!(
                waited < Duration::from_millis(10),
                "an edit in the middle of `{query}` waited {waited:?}"
            );
        }
    }
}
