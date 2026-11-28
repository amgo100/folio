//! The stage functions (`folio::evaluate` … `folio::pages_through`) in a
//! long-lived world: a panic in a query is `Failed::Internal` with the
//! `internal-error` diagnostic, not an unwind (SPEC §2.3).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use folio::{Event, Failed, Target, World};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
"#;

/// A world whose `query` panics, a bug injected through the event hook,
/// while the flag is set.
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
    world
        .set_source("main.fol", "= Forces\n\nA ball is thrown.\n")
        .unwrap();
    (world, armed)
}

/// Runs a stage function and keeps only whether it gave a result.
type Stage = fn(&World) -> Result<(), Failed>;

/// Each stage function, with a query it runs.
fn stages() -> Vec<(&'static str, &'static str, Stage)> {
    vec![
        ("evaluate", "eval_file", |w| {
            folio::evaluate(w, None).map(drop)
        }),
        ("document", "document", |w| {
            folio::document(w, None).map(drop)
        }),
        ("styles", "styles", |w| {
            folio::styles(w, Target::Pdf, None).map(drop)
        }),
        ("fonts", "fonts", |w| folio::fonts(w).map(drop)),
        ("text", "text", |w| {
            folio::text(w, Target::Pdf, None).map(drop)
        }),
        ("lines", "lines", |w| {
            folio::lines(w, Target::Pdf, None).map(drop)
        }),
        ("pages", "pages", |w| {
            folio::pages(w, Target::Pdf, None).map(drop)
        }),
        ("pages_through", "lines", |w| {
            folio::pages_through(w, Target::Pdf, None, 0).map(drop)
        }),
    ]
}

#[test]
fn a_panic_in_a_stage_is_an_internal_error_in_a_long_lived_world() {
    for (stage, query, run) in stages() {
        let (world, armed) = panicking(true, query);
        armed.store(true, Ordering::SeqCst);
        let error = match run(&world) {
            Err(Failed::Internal(error)) => error,
            other => panic!("{stage}: {other:?}"),
        };
        assert!(error.is_error(), "{stage}");
        assert_eq!(error.code.as_deref(), Some("internal-error"), "{stage}");
        assert_eq!(
            error.message,
            format!("internal error: the compiler panicked: injected into `{query}`"),
            "{stage}"
        );
        // The world is still usable: asking again fails alike, and once the
        // bug is gone the stage gives a result.
        assert!(matches!(run(&world), Err(Failed::Internal(_))), "{stage}");
        armed.store(false, Ordering::SeqCst);
        assert_eq!(run(&world), Ok(()), "{stage}");
    }
}

#[test]
fn a_panic_in_a_stage_unwinds_in_an_ordinary_world() {
    for (stage, query, run) in stages() {
        let (world, armed) = panicking(false, query);
        armed.store(true, Ordering::SeqCst);
        let payload = catch_unwind(AssertUnwindSafe(|| run(&world))).unwrap_err();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert_eq!(message, format!("injected into `{query}`"), "{stage}");
    }
}

#[test]
fn failed_says_why() {
    assert_eq!(
        Failed::from(folio::Cancelled),
        Failed::Cancelled,
        "an edit cancelled it"
    );
    assert_eq!(Failed::Cancelled.to_string(), folio::Cancelled.to_string());
}
