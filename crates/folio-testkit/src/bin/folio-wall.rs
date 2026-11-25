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
//!   default), and its 95th percentile is compared with its bounds;
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

use std::process::ExitCode;
use std::time::{Duration, Instant};

use folio::{BuildMode, Name, Target, World};
use folio_testkit::corpus::Project;
use folio_testkit::refdoc::{self, Edit, Spec};

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
/// Point it at a query that stops at the pages and runs the build's checks
/// once there is one.
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
    for scenario in scenarios() {
        (scenario.run)((scenario.setup)());
        let mut times: Vec<Duration> = (0..iterations)
            .map(|_| {
                let prepared = (scenario.setup)();
                let start = Instant::now();
                std::hint::black_box((scenario.run)(prepared));
                start.elapsed()
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
