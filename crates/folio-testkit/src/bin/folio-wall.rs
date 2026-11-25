//! Wall-clock benchmarks (SPEC §14.1): times compiles of generated
//! documents and compares their 95th percentile with the targets and hard
//! limits of SPEC §14.1 and the M0 exit check of SPEC §15.
//!
//! ```text
//! folio-wall [--iterations N] [--check]
//! ```
//!
//! Each scenario runs once to warm up, then `N` times (20 by default).
//! `--check` exits with an error when a scenario that is gated exceeds its
//! hard limit; the others are release criteria (M8, 1.0) and are reported
//! only. Timings mean something only on the reference machine of SPEC
//! §14.1 and in an optimised build: `scripts/bench.sh --wall` runs this
//! with `--release --check`.

#![forbid(unsafe_code)]

use std::ops::Range;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use folio::{BuildMode, Target, World};
use folio_testkit::corpus::Project;
use folio_testkit::refdoc::{self, Spec};

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
    bounds: Bounds,
    /// Prepares a run, outside the timing; returns what `run` takes.
    setup: Box<dyn Fn() -> Prepared>,
    /// The timed part.
    run: fn(Prepared) -> usize,
}

/// What a scenario's setup prepares.
enum Prepared {
    /// A project to compile in a fresh world.
    Project(Project),
    /// A compiled world, the entry file and the range of a word to edit.
    Compiled(World, String, Range<usize>),
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

/// Edits a word of a compiled world and compiles it again.
fn recompile_after_edit(prepared: Prepared) -> usize {
    let Prepared::Compiled(mut world, entry, range) = prepared else {
        unreachable!("set up as a compiled world")
    };
    world
        .edit_source(&entry, range, "This")
        .expect("a valid edit");
    let compiled = folio::compile(&world, Target::Pdf, None).expect("not cancelled");
    compiled.output.map_or(0, |bytes| bytes.len())
}

/// A compiled world of a generated document, and the first word of the
/// first paragraph after its middle.
fn compiled(spec: Spec) -> Prepared {
    let project = refdoc::generate(&spec);
    let world = project.world(BuildMode::Build);
    folio::compile(&world, Target::Pdf, None).expect("not cancelled");
    let text = &project.sources["main.fol"];
    let start = text[text.len() / 2..]
        .find("\n\nThe ")
        .map(|i| text.len() / 2 + i + 2)
        .expect("a paragraph after the middle");
    Prepared::Compiled(world, "main.fol".into(), start..start + 3)
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "m0-exit",
            what: "10 pages, full compile in a fresh world",
            bounds: Bounds {
                source: "M0 exit (§15)",
                target: None,
                limit: Duration::from_millis(200),
                gated: true,
            },
            setup: Box::new(|| Prepared::Project(refdoc::generate(&Spec::M0_EXIT))),
            run: compile_fresh,
        },
        Scenario {
            name: "reference-full",
            what: "100 pages, full compile in a fresh world in a warm process",
            bounds: Bounds {
                source: "warm full compile (§14.1), the world's caches empty",
                target: Some(Duration::from_millis(400)),
                limit: Duration::from_secs(1),
                gated: false,
            },
            setup: Box::new(|| Prepared::Project(refdoc::generate(&Spec::REFERENCE))),
            run: compile_fresh,
        },
        Scenario {
            name: "reference-edit",
            what: "100 pages, recompile after an edit in the middle",
            bounds: Bounds {
                source: "edit that reflows (§14.1)",
                target: None,
                limit: Duration::from_millis(600),
                gated: false,
            },
            setup: Box::new(|| compiled(Spec::REFERENCE)),
            run: recompile_after_edit,
        },
    ]
}

/// The `p`-th percentile of sorted durations, by the nearest rank.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn main() -> ExitCode {
    let mut iterations = 20;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--iterations" => match args.next().and_then(|n| n.parse().ok()) {
                Some(n) if n > 0 => iterations = n,
                _ => {
                    eprintln!("--iterations needs a positive number");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("usage: folio-wall [--iterations N] [--check]");
                return ExitCode::from(2);
            }
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("warning: an unoptimised build; run with `cargo run --release`");
    }
    let reference = refdoc::generate(&Spec::REFERENCE);
    let world = reference.world(BuildMode::Build);
    let pages = folio::pages(&world, Target::Pdf, None)
        .expect("not cancelled")
        .map_or(0, |pages| pages.pages.len());
    println!(
        "reference document: {pages} pages (meant for {}), {} threads",
        Spec::REFERENCE.pages,
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    println!(
        "{:<16} {:>10} {:>10} {:>10} {:>10}  status",
        "scenario", "median", "p95", "target", "limit"
    );
    let mut failed = false;
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
        let bounds = scenario.bounds;
        let over = p95 > bounds.limit;
        let status = match (over, bounds.gated) {
            (false, _) if bounds.target.is_some_and(|t| p95 > t) => "over target",
            (false, _) => "ok",
            (true, true) => "FAILED",
            (true, false) => "over limit (release criterion)",
        };
        failed |= over && bounds.gated;
        println!(
            "{:<16} {:>10} {:>10} {:>10} {:>10}  {status}: {}, {}",
            scenario.name,
            ms(percentile(&times, 50)),
            ms(p95),
            bounds.target.map_or_else(|| "-".into(), ms),
            ms(bounds.limit),
            scenario.what,
            bounds.source
        );
    }
    if check && failed {
        eprintln!("a gated scenario exceeded its limit");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
