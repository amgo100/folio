//! Which thread shapes the paragraphs of [`Cache::shape_all`].
//!
//! The instruction-count benchmarks (SPEC §14.1) run with one `rayon`
//! thread and count only the calling thread, so with one thread the cache
//! must shape on the caller. The shaping pool is built once per process from
//! `RAYON_NUM_THREADS`, so each case runs this test binary again in a child
//! process with the variable set.
//!
//! A child shapes two paragraphs, the second of which does not cover its
//! text, and reports the name of the thread that panicked while shaping it.

use std::process::Command;
use std::sync::{Arc, Mutex};

use ecow::EcoVec;
use folio_text::{Cache, Lang, Paragraph, Segment, SegmentKind};

/// The variable that tells this binary it runs as a child.
const CHILD: &str = "FOLIO_SHAPING_THREAD_CHILD";
/// What a child prints before the name of the thread that shaped.
const MARK: &str = "shaped on thread: ";
/// What a child prints before the name of the thread that called the cache.
const CALLER: &str = "called on thread: ";

/// A paragraph whose one segment covers the text up to `end`, without a
/// font.
fn paragraph(text: &str, end: usize) -> Paragraph {
    Paragraph {
        text: text.into(),
        segments: EcoVec::from([Segment {
            range: 0..end,
            kind: SegmentKind::Text {
                font: None,
                features: EcoVec::new(),
            },
            lang: Lang::new("en"),
            hyphenate: false,
        }]),
    }
}

/// The name of the current thread.
fn this_thread() -> String {
    std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_owned()
}

/// The child's work: shape a whole and a broken paragraph, and print the
/// name of the thread that panicked on the broken one and of its own.
fn child() {
    let panicked_on: Arc<Mutex<Option<String>>> = Arc::default();
    let record = panicked_on.clone();
    std::panic::set_hook(Box::new(move |_| {
        *record.lock().unwrap() = Some(this_thread());
    }));
    let cache = Cache::new();
    let paragraphs = [paragraph("Whole.", 6), paragraph("Broken.", 3)];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cache.shape_all(&paragraphs)
    }));
    let _ = std::panic::take_hook();
    assert!(result.is_err(), "the broken paragraph panics");
    let thread = panicked_on.lock().unwrap().clone().expect("a panic");
    println!("{MARK}{thread}");
    println!("{CALLER}{}", this_thread());
}

/// The thread a child with `threads` shaping threads shaped on, and the
/// name of the thread that called the cache in it.
fn shaped_on(threads: &str) -> (String, String) {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new(exe)
        .args([
            "--exact",
            "child_process",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("RAYON_NUM_THREADS", threads)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let find = |mark: &str| {
        stdout
            .lines()
            .find_map(|l| l.split_once(mark).map(|(_, t)| t.to_owned()))
            .unwrap_or_else(|| panic!("no {mark:?} in the child's output:\n{stdout}"))
    };
    (find(MARK), find(CALLER))
}

/// Runs only in a child, where [`CHILD`] is set.
#[test]
fn child_process() {
    if std::env::var_os(CHILD).is_some() {
        child();
    }
}

#[test]
fn one_thread_shapes_on_the_caller() {
    let (thread, caller) = shaped_on("1");
    assert_eq!(thread, caller);
}

#[test]
fn several_threads_shape_on_the_pool() {
    let (thread, caller) = shaped_on("2");
    assert_ne!(thread, caller);
    assert!(thread.starts_with("folio-shape-"), "shaped on {thread}");
}
