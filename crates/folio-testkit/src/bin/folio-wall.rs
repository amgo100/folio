//! Wall-clock benchmarks (SPEC §14.1): times compiles of generated
//! documents and compares their 95th percentile with the targets and hard
//! limits of SPEC §14.1 and the exit checks of SPEC §15.
//!
//! ```text
//! folio-wall [--iterations N] [--keystrokes N] [--check]
//! ```
//!
//! It reports three tables:
//!
//! - the scenarios: each runs once to warm up, then `N` times (20 by
//!   default), and its 95th percentile is compared with its bounds. The
//!   M1 exit check, the preview updated within 80 ms of an edit, is
//!   `watch-edit`: the acceptance exam of the test corpus served by `folio watch`'s
//!   preview ([`folio_preview`]), an edit written to disk, and the time
//!   until a websocket client has received the display lists of the pages
//!   it changed. `watch-paint` times the same edit to the completed canvas:
//!   until the preview's viewer, in Chromium, shows the edited page with
//!   every glyph (SPEC §12.2). It runs when Node.js can load Playwright and
//!   its Chromium, and is left out with a note otherwise. Glyphs drawn
//!   from another quarter-pixel offset while the right one is fetched
//!   count as drawn;
//! - the stages of the reference document's builds: how long each query
//!   from evaluation to emit takes in a full compile, after a one-word
//!   edit and after a stylesheet edit, measured by running the public
//!   queries in pipeline order, so that each finds the stages before it
//!   memoised;
//! - typing: one character every 100 ms into a paragraph in the middle of
//!   the reference document (`N` keystrokes, 40 by default), each timed
//!   from the edit to the pages (the preview path), against the preview
//!   limit.
//!
//! `--check` exits with an error when a scenario that is gated exceeds its
//! hard limit; the others are release criteria (M1, M8, 1.0) and are
//! reported only. Timings mean something only on the reference machine of
//! SPEC §14.1 and in an optimised build: `scripts/bench.sh --wall` runs
//! this with `--release --check`.

#![forbid(unsafe_code)]

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use folio::{BuildMode, Name, Target, World};
use folio_preview::{Event, Preview};
use folio_testkit::corpus;
use folio_testkit::corpus::Project;
use folio_testkit::refdoc::{self, Edit, Spec};
use tokio_tungstenite::tungstenite::{self, Message, WebSocket};

/// What a scenario is measured against.
#[derive(Debug, Copy, Clone)]
struct Bounds {
    /// Where the limit comes from.
    source: &'static str,
    /// The target for the 95th percentile, if the source sets one apart
    /// from the limit.
    target: Option<Duration>,
    /// The hard limit for the 95th percentile.
    limit: Duration,
    /// Whether `--check` fails when the limit is exceeded.
    gated: bool,
}

/// A scenario: how to set up a run, and what to time.
struct Scenario {
    name: &'static str,
    what: &'static str,
    /// What the scenario is measured against; `None` for one that is only
    /// reported, such as a part of a ratio.
    bounds: Option<Bounds>,
    /// Prepares a run, outside the timing; returns what `run` takes.
    setup: Setup,
    /// The timed part.
    run: fn(Prepared) -> usize,
}

/// Prepares a run of a scenario, outside the timing.
type Setup = fn() -> Prepared;

/// Applies an edit to a world.
fn apply(edit: &Edit, world: &mut World) {
    world
        .edit_source(&edit.path, edit.range.clone(), edit.with)
        .expect("a valid edit");
}

/// What a scenario's setup prepares.
enum Prepared {
    /// A project to compile in a fresh world.
    Project(Project),
    /// A compiled world and an edit to make to it.
    Compiled(World, Edit),
    /// A project served by the preview, with a client that has every page.
    Watching(Box<Watching>),
    /// A project served by the preview, shown by the viewer in Chromium.
    Painting(Box<Painting>),
}

/// A copy of a corpus project served by the preview and shown by its
/// viewer in Chromium, driven by `paint.cjs`, with an edit to write.
struct Painting {
    /// `paint.cjs`: dropped first, so that its browser closes before the
    /// preview stops.
    painter: Painter,
    _preview: Preview,
    _dir: tempfile::TempDir,
    /// The revisions the preview starts, which tie a paint to the edit.
    edited_revisions: std::sync::mpsc::Receiver<u64>,
    /// The file to edit, and its text after the edit.
    file: PathBuf,
    edited: String,
}

/// The directory of `paint.cjs`, the script that watches what the
/// preview's viewer paints, which Node.js runs from there.
fn paint_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../folio-preview/tests")
}

/// Whether Node.js can load Playwright and Playwright has its Chromium,
/// which `watch-paint` needs.
fn playwright() -> bool {
    let probe = "process.exit(require('fs').existsSync(\
                 require('playwright').chromium.executablePath()) ? 0 : 1)";
    std::process::Command::new("node")
        .current_dir(paint_dir())
        .args(["-e", probe])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `paint.cjs` showing a page of a preview: its lines arrive on a channel,
/// and dropping it closes its standard input, which stops it.
struct Painter {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
}

impl Painter {
    /// Starts `paint.cjs` showing page `page` (from 0) of the preview at
    /// `url`, and waits until the viewer shows it; returns the revision.
    fn start(url: &str, page: usize) -> (Self, u64) {
        let mut child = std::process::Command::new("node")
            .current_dir(paint_dir())
            .args(["paint.cjs", url, &page.to_string()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("Node.js runs paint.cjs");
        let stdout = child.stdout.take().expect("its output");
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut painter = Self { child, lines };
        let revision = painter.until("ready", 0).0;
        (painter, revision)
    }

    /// Waits for a line `<word> <revision>` with a revision after `after`;
    /// returns the revision and how many frames with holes came before.
    fn until(&mut self, word: &str, after: u64) -> (u64, usize) {
        let mut holes = 0;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no `{word}` from paint.cjs in time"));
            assert!(!line.starts_with("error "), "the viewer failed: {line}");
            let (w, r) = line.split_once(' ').expect("a word and a revision");
            let r: u64 = r.parse().expect("a revision");
            holes += usize::from(w == "holes" && r > after);
            if w == word && r > after {
                return (r, holes);
            }
        }
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        // A browser that hangs as it closes is killed.
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A copy of a corpus project served by the preview, with a websocket
/// client that has received every page, and an edit to write to disk.
struct Watching {
    /// Fields drop in order: the preview stops before its directory goes.
    _preview: Preview,
    /// Removed, with the copy, when the run ends.
    _dir: tempfile::TempDir,
    client: WebSocket<TcpStream>,
    /// The revision the client has.
    revision: u64,
    /// The file to edit, and its text after the edit.
    file: PathBuf,
    edited: String,
}

/// The corpus project that `watch-edit` previews: the acceptance exam of
/// SPEC §15, whose preview the M1 exit check times.
const WATCHED: &str = "acceptance";

/// The edit `watch-edit` writes: a quantity in a question on the second
/// page, under the figure of the sledge.
const WATCH_EDIT: (&str, &str) = ("sledge moves #qty(25,", "sledge moves #qty(26,");

/// Copies a directory into another, recursively.
fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).expect("a readable corpus") {
        let entry = entry.expect("a readable corpus");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            std::fs::create_dir(&target).expect("a temporary directory");
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a temporary copy");
        }
    }
}

/// Serves a copy of [`WATCHED`] with `folio watch`'s preview, watching the
/// disk, and connects a client that receives every page.
fn watching() -> Prepared {
    let dir = tempfile::tempdir().expect("a temporary directory");
    copy_dir(&corpus::dir().join(WATCHED), dir.path());
    let (tx, rx) = std::sync::mpsc::channel();
    let options = folio_preview::Options::new(dir.path())
        .system_fonts(false)
        .on_event(move |event| {
            if let Event::Updated(update) = event {
                let _ = tx.send(update.revision);
            }
        });
    let preview = Preview::start(options).expect("the preview starts");
    let revision = rx.recv().expect("the first revision");
    let url = format!("ws://{}/ws?token={}", preview.addr(), preview.token());
    let stream = TcpStream::connect(preview.addr()).expect("the server");
    let (mut client, _) = tungstenite::client(url, stream).expect("the websocket opens");
    while received(&mut client) < revision {}
    let file = dir.path().join("main.fol");
    let text = std::fs::read_to_string(&file).expect("the entry file");
    assert!(
        text.contains(WATCH_EDIT.0),
        "the edit's text is in the acceptance exam"
    );
    let edited = text.replacen(WATCH_EDIT.0, WATCH_EDIT.1, 1);
    Prepared::Watching(Box::new(Watching {
        _preview: preview,
        _dir: dir,
        client,
        revision,
        file,
        edited,
    }))
}

/// Serves a copy of [`WATCHED`] with `folio watch`'s preview, watching the
/// disk, and shows the page [`WATCH_EDIT`] changes in the viewer.
fn painting() -> Prepared {
    let dir = tempfile::tempdir().expect("a temporary directory");
    copy_dir(&corpus::dir().join(WATCHED), dir.path());
    let (tx, rx) = std::sync::mpsc::channel();
    let (edits, edited_revisions) = std::sync::mpsc::channel();
    let options = folio_preview::Options::new(dir.path())
        .system_fonts(false)
        .on_event(move |event| match event {
            Event::Updated(_) => {
                let _ = tx.send(());
            }
            Event::Edited { revision } => {
                let _ = edits.send(*revision);
            }
            _ => {}
        });
    let preview = Preview::start(options).expect("the preview starts");
    rx.recv().expect("the first revision");
    // The question under the figure of the sledge, on the second page.
    let (painter, _) = Painter::start(&preview.url(), 1);
    // The revisions so far are not the edit's.
    while edited_revisions.try_recv().is_ok() {}
    let file = dir.path().join("main.fol");
    let text = std::fs::read_to_string(&file).expect("the entry file");
    assert!(
        text.contains(WATCH_EDIT.0),
        "the edit's text is in the acceptance exam"
    );
    let edited = text.replacen(WATCH_EDIT.0, WATCH_EDIT.1, 1);
    Prepared::Painting(Box::new(Painting {
        painter,
        _preview: preview,
        _dir: dir,
        edited_revisions,
        file,
        edited,
    }))
}

/// Writes the edit to disk and waits until the viewer shows the edited
/// page with every glyph: the preview path of `folio watch` to the
/// completed canvas. Panics when a frame with holes was shown. The
/// preview is stopped after the time is taken.
fn paint_after_edit(prepared: Prepared) -> usize {
    let Prepared::Painting(mut painting) = prepared else {
        unreachable!("set up as a painted project")
    };
    std::fs::write(&painting.file, &painting.edited).expect("the edit is written");
    let edit = painting
        .edited_revisions
        .recv_timeout(Duration::from_secs(60))
        .expect("the edit starts a revision");
    let (_, holes) = painting.painter.until("painted", edit - 1);
    assert_eq!(holes, 0, "the viewer showed text with holes");
    defer_drop(painting);
    1
}

/// Reads one revision from the preview's websocket, to its `done`
/// message; returns the revision and how many pages' display lists came.
fn received_lists(client: &mut WebSocket<TcpStream>) -> (u64, usize) {
    let mut lists = 0;
    loop {
        match client.read().expect("the preview's messages") {
            Message::Binary(_) => lists += 1,
            Message::Text(text) if text.starts_with(r#"{"type":"done""#) => {
                let revision = text
                    .trim_end_matches('}')
                    .rsplit(':')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("a revision");
                return (revision, lists);
            }
            _ => {}
        }
    }
}

/// [`received_lists`]: the revision only.
fn received(client: &mut WebSocket<TcpStream>) -> u64 {
    received_lists(client).0
}

thread_local! {
    /// What a timed run leaves to be dropped after its time is taken, such
    /// as a preview whose threads take time to stop ([`bury`]).
    static GRAVEYARD: std::cell::RefCell<Vec<Box<dyn std::any::Any>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Keeps a value until [`bury`] drops it, outside the timing.
fn defer_drop(value: Box<dyn std::any::Any>) {
    GRAVEYARD.with(|graveyard| graveyard.borrow_mut().push(value));
}

/// Drops what timed runs left ([`defer_drop`]).
fn bury() {
    GRAVEYARD.with(|graveyard| graveyard.borrow_mut().clear());
}

/// Writes the edit to disk and waits until the client has the display
/// lists of the pages it changed: the preview path of `folio watch`.
/// Returns how many pages were sent, and panics when none were. The
/// preview is stopped after the time is taken.
fn watch_after_edit(prepared: Prepared) -> usize {
    let Prepared::Watching(mut watching) = prepared else {
        unreachable!("set up as a watched project")
    };
    std::fs::write(&watching.file, &watching.edited).expect("the edit is written");
    loop {
        let (revision, lists) = received_lists(&mut watching.client);
        if revision > watching.revision {
            assert!(lists > 0, "the edit changed a page");
            defer_drop(watching);
            return lists;
        }
    }
}

/// A world of a project that has compiled its PDF, and an edit to make.
fn compiled(project: Project, edit: fn(&Project) -> Edit) -> Prepared {
    let world = project.world(BuildMode::Build);
    folio::compile(&world, Target::Pdf, None).expect("not cancelled");
    let edit = edit(&project);
    Prepared::Compiled(world, edit)
}

/// The preview path: the pages, without emit, which is what a preview
/// repaints from; returns the number of pages, and panics when the build
/// cannot be laid out, so that a broken edit is not timed as a fast one.
/// The check path, which also runs the PDF writer's check and gathers the
/// diagnostics, is [`check_after_edit`].
fn preview(world: &World) -> usize {
    folio::pages(world, Target::Pdf, None)
        .expect("not cancelled")
        .expect("the build is laid out")
        .pages
        .len()
}

/// Compiles a project in a fresh world, from its sources to the PDF's
/// bytes; returns the PDF's size.
fn compile_fresh(prepared: Prepared) -> usize {
    let Prepared::Project(project) = prepared else {
        unreachable!("set up as a project")
    };
    let world = project.world(BuildMode::Build);
    let compiled = folio::compile(&world, Target::Pdf, None).expect("not cancelled");
    compiled.output.map_or(0, |bytes| bytes.len())
}

/// Edits a compiled world and compiles it again to PDF.
fn recompile_after_edit(prepared: Prepared) -> usize {
    let Prepared::Compiled(mut world, edit) = prepared else {
        unreachable!("set up as a compiled world")
    };
    apply(&edit, &mut world);
    let compiled = folio::compile(&world, Target::Pdf, None).expect("not cancelled");
    compiled.output.map_or(0, |bytes| bytes.len())
}

/// Edits a compiled world and lays it out again: the preview path.
fn preview_after_edit(prepared: Prepared) -> usize {
    let Prepared::Compiled(mut world, edit) = prepared else {
        unreachable!("set up as a compiled world")
    };
    apply(&edit, &mut world);
    preview(&world)
}

/// Edits a world and checks it again ([`folio::check`]): every stage
/// before Emit and the PDF writer's check, as `folio check` runs them;
/// returns the number of diagnostics.
fn check_after_edit(prepared: Prepared) -> usize {
    let Prepared::Compiled(mut world, edit) = prepared else {
        unreachable!("set up as a compiled world")
    };
    apply(&edit, &mut world);
    let checked = folio::check(&world, Target::Pdf, None).expect("not cancelled");
    checked.diagnostics.len()
}

/// The variant that [`compile_one_variant`] compiles: the teacher's, which
/// shows everything and shares its styles with the exam.
const ONE_VARIANT: &str = refdoc::VARIANTS[2];

/// Compiles [`ONE_VARIANT`] of a project with variants in a fresh world.
fn compile_one_variant(prepared: Prepared) -> usize {
    let Prepared::Project(project) = prepared else {
        unreachable!("set up as a project")
    };
    let world = project.world(BuildMode::Build);
    let variant = Name::new(ONE_VARIANT).expect("a name");
    let compiled = folio::compile(&world, Target::Pdf, Some(&variant)).expect("not cancelled");
    compiled.output.map_or(0, |bytes| bytes.len())
}

/// Compiles every variant of a project in a fresh world, in parallel.
fn compile_variants(prepared: Prepared) -> usize {
    let Prepared::Project(project) = prepared else {
        unreachable!("set up as a project")
    };
    let world = project.world(BuildMode::Build);
    folio::compile_all(&world)
        .into_iter()
        .map(|output| {
            output
                .result
                .expect("not cancelled")
                .output
                .map_or(0, |bytes| bytes.len())
        })
        .sum()
}

/// The reference document, to compile in a fresh world.
fn reference_project() -> Prepared {
    Prepared::Project(refdoc::generate(&Spec::REFERENCE))
}

/// The reference document, compiled, with a one-word edit to make.
fn reference_word_edit() -> Prepared {
    compiled(refdoc::generate(&Spec::REFERENCE), Edit::word)
}

/// The reference document, compiled, with a stylesheet edit to make.
fn reference_style_edit() -> Prepared {
    compiled(refdoc::generate(&Spec::REFERENCE), Edit::style)
}

/// The bounds of the preview after an edit: the M1 exit check of SPEC §15,
/// the preview updated within 80 ms of an edit. The edits here may reflow
/// the pages after them, so the 30 ms target of §14.1 for an edit inside
/// one paragraph does not apply.
const PREVIEW: Bounds = Bounds {
    source: "preview within 80 ms of an edit: M1 exit (§15)",
    target: None,
    limit: Duration::from_millis(80),
    gated: false,
};

/// The bounds of a warm full compile (SPEC §14.1).
const WARM_FULL: Bounds = Bounds {
    source: "warm full compile (§14.1), the world's caches empty",
    target: Some(Duration::from_millis(400)),
    limit: Duration::from_secs(1),
    gated: false,
};

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "m0-exit",
            what: "10 pages, full compile in a fresh world",
            bounds: Some(Bounds {
                source: "M0 exit (§15)",
                target: None,
                limit: Duration::from_millis(200),
                gated: true,
            }),
            setup: || Prepared::Project(refdoc::generate(&Spec::M0_EXIT)),
            run: compile_fresh,
        },
        Scenario {
            name: "reference-full",
            what: "100 pages, full compile in a fresh world in a warm process",
            bounds: Some(WARM_FULL),
            setup: reference_project,
            run: compile_fresh,
        },
        Scenario {
            name: "reference-edit",
            what: "100 pages, a one-word edit in the middle, to PDF",
            bounds: Some(Bounds {
                source: "edit that reflows (§14.1)",
                target: None,
                limit: Duration::from_millis(600),
                gated: false,
            }),
            setup: reference_word_edit,
            run: recompile_after_edit,
        },
        Scenario {
            name: "preview-edit",
            what: "100 pages, a one-word edit in the middle, to the pages",
            bounds: Some(PREVIEW),
            setup: reference_word_edit,
            run: preview_after_edit,
        },
        Scenario {
            name: "watch-edit",
            what: "the acceptance exam in `folio watch`, an edit on disk to changed display lists",
            bounds: Some(Bounds {
                gated: true,
                ..PREVIEW
            }),
            setup: watching,
            run: watch_after_edit,
        },
        Scenario {
            name: "watch-paint",
            what: "the acceptance exam in `folio watch`, an edit on disk to the completed canvas",
            bounds: Some(Bounds {
                gated: true,
                ..PREVIEW
            }),
            setup: painting,
            run: paint_after_edit,
        },
        Scenario {
            name: "check-edit",
            what: "100 pages, a one-word edit in the middle, checked without emit",
            bounds: None,
            setup: reference_word_edit,
            run: check_after_edit,
        },
        Scenario {
            name: "stylesheet-edit",
            what: "100 pages, the body's font size changed, to PDF",
            bounds: Some(Bounds {
                source: "stylesheet edit (§14.1)",
                target: Some(Duration::from_millis(400)),
                limit: Duration::from_secs(1),
                gated: false,
            }),
            setup: reference_style_edit,
            run: recompile_after_edit,
        },
        Scenario {
            name: "variants-one",
            what: "100 pages and answers, the teacher variant of three, in a fresh world",
            bounds: None,
            setup: || Prepared::Project(refdoc::generate_variants(&Spec::REFERENCE)),
            run: compile_one_variant,
        },
        Scenario {
            name: "variants-three",
            what: "100 pages and answers, three variants in parallel, in a fresh world",
            bounds: None,
            setup: || Prepared::Project(refdoc::generate_variants(&Spec::REFERENCE)),
            run: compile_variants,
        },
    ]
}

/// The bounds of the ratio of three variants to one (SPEC §14.1).
const VARIANTS_TARGET: f64 = 1.3;
const VARIANTS_LIMIT: f64 = 2.0;

/// The stages of a build, in pipeline order, that [`stage_times`] times.
const STAGES: [&str; 8] = [
    "edit", "evaluate", "document", "style", "text", "lines", "pages", "emit",
];

/// Runs a build's queries one after another, each finding the ones before
/// it memoised, and returns how long each took, in the order of
/// [`STAGES`]: applying the edit (parsing the file again and matching its
/// node identities; nothing for a fresh world, where parsing is part of
/// evaluation), then `evaluate`, `document`, `styles`, `text` (with the
/// fonts), `lines`, `pages`, and the rest of `compile`: the PDF and the
/// build's checks.
fn stage_times(prepared: Prepared) -> [Duration; STAGES.len()] {
    fn time<T>(f: impl FnOnce() -> T) -> Duration {
        let start = Instant::now();
        std::hint::black_box(f());
        start.elapsed()
    }
    let (world, edit) = match prepared {
        Prepared::Project(project) => (project.world(BuildMode::Build), Duration::ZERO),
        Prepared::Compiled(mut world, edit) => {
            let took = time(|| apply(&edit, &mut world));
            (world, took)
        }
        Prepared::Watching(_) | Prepared::Painting(_) => unreachable!("stages are timed on worlds"),
    };
    let target = Target::Pdf;
    [
        edit,
        time(|| folio::evaluate(&world, None).expect("not cancelled")),
        time(|| folio::document(&world, None).expect("not cancelled")),
        time(|| folio::styles(&world, target, None).expect("not cancelled")),
        time(|| folio::text(&world, target, None).expect("not cancelled")),
        time(|| folio::lines(&world, target, None).expect("not cancelled")),
        time(|| folio::pages(&world, target, None).expect("not cancelled")),
        time(|| folio::compile(&world, target, None).expect("not cancelled")),
    ]
}

/// The interval between keystrokes of the typing scenario.
const KEYSTROKE: Duration = Duration::from_millis(100);

/// What the typing scenario types.
const TYPED: &str = "The pendulum swings with a constant period. ";

/// Types `keystrokes` characters of [`TYPED`] into a compiled reference
/// document, one every [`KEYSTROKE`], at the start of a paragraph in its
/// middle; returns how long each took from the edit to the pages.
fn typing(keystrokes: usize) -> Vec<Duration> {
    let Prepared::Compiled(mut world, edit) = reference_word_edit() else {
        unreachable!("set up as a compiled world")
    };
    preview(&world);
    let mut at = edit.range.start;
    let mut next = Instant::now();
    TYPED
        .chars()
        .cycle()
        .take(keystrokes)
        .map(|c| {
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            next = Instant::now() + KEYSTROKE;
            let start = Instant::now();
            let mut buf = [0; 4];
            world
                .edit_source(&edit.path, at..at, c.encode_utf8(&mut buf))
                .expect("a valid edit");
            std::hint::black_box(preview(&world));
            let took = start.elapsed();
            at += c.len_utf8();
            took
        })
        .collect()
}

/// The `p`-th percentile of sorted durations, by the nearest rank.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// Whether the 95th percentile is within its bounds, as a status.
fn status(p95: Duration, bounds: Bounds) -> &'static str {
    match (p95 > bounds.limit, bounds.gated) {
        (false, _) if bounds.target.is_some_and(|t| p95 > t) => "over target",
        (false, _) => "ok",
        (true, true) => "FAILED",
        (true, false) => "over limit (release criterion)",
    }
}

fn main() -> ExitCode {
    let mut iterations = 20;
    let mut keystrokes = 40;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let count = match arg.as_str() {
            "--check" => {
                check = true;
                continue;
            }
            "--iterations" => &mut iterations,
            "--keystrokes" => &mut keystrokes,
            _ => {
                eprintln!("usage: folio-wall [--iterations N] [--keystrokes N] [--check]");
                return ExitCode::from(2);
            }
        };
        match args.next().and_then(|n| n.parse().ok()) {
            Some(n) if n > 0 => *count = n,
            _ => {
                eprintln!("{arg} needs a positive number");
                return ExitCode::from(2);
            }
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("warning: an unoptimised build; run with `cargo run --release`");
    }
    let reference = refdoc::generate(&Spec::REFERENCE);
    let world = reference.world(BuildMode::Build);
    println!(
        "reference document: {} pages (meant for {}), {} threads",
        // Tolerant here, so that the M0 exit check still runs when the
        // reference document cannot be laid out.
        folio::pages(&world, Target::Pdf, None)
            .expect("not cancelled")
            .map_or(0, |pages| pages.pages.len()),
        Spec::REFERENCE.pages,
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );

    println!(
        "\n{:<16} {:>10} {:>10} {:>10} {:>10}  status",
        "scenario", "median", "p95", "target", "limit"
    );
    let mut failed = false;
    let mut p95s = Vec::new();
    let painted = playwright();
    for scenario in scenarios() {
        if scenario.name == "watch-paint" && !painted {
            println!(
                "{:<16} not run: needs Node.js with Playwright and its Chromium",
                scenario.name
            );
            continue;
        }
        (scenario.run)((scenario.setup)());
        bury();
        let mut times: Vec<Duration> = (0..iterations)
            .map(|_| {
                let prepared = (scenario.setup)();
                let start = Instant::now();
                std::hint::black_box((scenario.run)(prepared));
                let took = start.elapsed();
                bury();
                took
            })
            .collect();
        times.sort();
        let p95 = percentile(&times, 95);
        p95s.push((scenario.name, p95));
        let (target, limit, status, source) = match scenario.bounds {
            Some(bounds) => {
                failed |= p95 > bounds.limit && bounds.gated;
                (
                    bounds.target.map_or_else(|| "-".into(), ms),
                    ms(bounds.limit),
                    status(p95, bounds),
                    bounds.source,
                )
            }
            None => ("-".into(), "-".into(), "reported", "a part of a ratio"),
        };
        println!(
            "{:<16} {:>10} {:>10} {target:>10} {limit:>10}  {status}: {}, {source}",
            scenario.name,
            ms(percentile(&times, 50)),
            ms(p95),
            scenario.what,
        );
    }
    let p95_of = |name| {
        p95s.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, d)| d.as_secs_f64())
            .expect("a scenario of that name")
    };
    let ratio = p95_of("variants-three") / p95_of("variants-one");
    let ratio_status = if ratio > VARIANTS_LIMIT {
        "over limit (release criterion)"
    } else if ratio > VARIANTS_TARGET {
        "over target"
    } else {
        "ok"
    };
    println!(
        "three variants against the teacher's (p95): {ratio:.2}×, target {VARIANTS_TARGET}×, \
         limit {VARIANTS_LIMIT}×: {ratio_status}, three variants in parallel (§14.1)"
    );

    let columns: [(&str, Setup); 3] = [
        ("full", reference_project),
        ("word edit", reference_word_edit),
        ("style edit", reference_style_edit),
    ];
    println!("\nstages of the reference document's PDF build, median / p95:");
    print!("{:<10}", "stage");
    for (name, _) in &columns {
        print!(" {name:>23}");
    }
    println!();
    let timed: Vec<Vec<[Duration; STAGES.len()]>> = columns
        .iter()
        .map(|(_, setup)| {
            stage_times(setup());
            (0..iterations).map(|_| stage_times(setup())).collect()
        })
        .collect();
    let cell = |runs: &[[Duration; STAGES.len()]], stage: Option<usize>| {
        let mut times: Vec<Duration> = runs
            .iter()
            .map(|run| stage.map_or_else(|| run.iter().sum(), |i| run[i]))
            .collect();
        times.sort();
        format!(
            "{} / {}",
            ms(percentile(&times, 50)),
            ms(percentile(&times, 95))
        )
    };
    for (i, stage) in STAGES.iter().enumerate() {
        print!("{stage:<10}");
        for runs in &timed {
            print!(" {:>23}", cell(runs, Some(i)));
        }
        println!();
    }
    print!("{:<10}", "total");
    for runs in &timed {
        print!(" {:>23}", cell(runs, None));
    }
    println!();
    println!("(the preview path is every stage but emit; emit includes the build's checks)");

    let mut times = typing(keystrokes);
    times.sort();
    let bounds = PREVIEW;
    let p95 = percentile(&times, 95);
    println!(
        "\ntyping: {keystrokes} characters, one every {} ms, edit to pages: median {}, p95 {}, \
         max {}, limit {}: {}, {}",
        KEYSTROKE.as_millis(),
        ms(percentile(&times, 50)),
        ms(p95),
        ms(*times.last().expect("a keystroke")),
        ms(bounds.limit),
        status(p95, bounds),
        bounds.source,
    );

    if check && failed {
        eprintln!("a gated scenario exceeded its limit");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
