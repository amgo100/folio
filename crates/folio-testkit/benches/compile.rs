//! Instruction-count benchmarks (SPEC §14.1): the regression gate.
//!
//! Each benchmark runs once under Callgrind (Gungraun, formerly
//! `iai-callgrind`), which counts the instructions it executes; counts are
//! stable on shared hardware, where wall-clock times are not. The gate
//! fails when a benchmark executes more than 3% more instructions than the
//! baseline it is compared with. `scripts/bench.sh` records the baseline
//! from the commit a branch starts from and compares the branch with it.
//!
//! Run them with `cargo bench -p folio-testkit --bench compile`; they need
//! Valgrind and `gungraun-runner` of the same version as `gungraun`
//! (`cargo install gungraun-runner --version <version> --locked`).
//!
//! `compile_cold::m0_exit` is the M0 exit check (SPEC §15) counted in
//! instructions: the ten-page document compiled from source in a fresh
//! world. `folio-wall` times it against its 200 ms limit.

// Gungraun's macros expand to paths the first lint calls redundant, and to
// public items without documentation.
#![allow(unused_qualifications, missing_docs)]

use std::hint::black_box;
use std::ops::Range;

use folio::{BuildMode, Name, Target, World};
use folio_testkit::corpus::{self, Project};
use folio_testkit::refdoc::{self, Spec};
use gungraun::prelude::*;
use gungraun::{Callgrind, EventKind};

/// The regression limit on instruction counts, in per cent.
const LIMIT: f64 = 3.0;

/// The ten-page document of the M0 exit check.
fn m0_exit() -> Project {
    refdoc::generate(&Spec::M0_EXIT)
}

/// A corpus project, by name.
fn corpus_project(name: &str) -> Project {
    corpus::projects()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no corpus project {name}"))
}

/// A project's first build.
fn first_build(world: &World) -> (Option<Name>, Target) {
    world.builds().into_iter().next().expect("a build")
}

// Compiles a project's first build in a fresh world: everything from
// parsing to the PDF's bytes.
#[library_benchmark]
#[bench::m0_exit(setup = m0_exit)]
#[bench::exam(args = ("exam"), setup = corpus_project)]
#[bench::notes(args = ("notes"), setup = corpus_project)]
fn compile_cold(project: Project) -> usize {
    let world = project.world(BuildMode::Build);
    let (variant, target) = first_build(&world);
    let compiled = folio::compile(&world, target, variant.as_ref()).expect("not cancelled");
    black_box(compiled.output.map_or(0, |bytes| bytes.len()))
}

/// A world that has compiled a project's first build, and the range of a
/// word in the middle of its entry file, for an edit.
fn compiled(project: Project) -> (World, String, Range<usize>) {
    let world = project.world(BuildMode::Build);
    let (variant, target) = first_build(&world);
    folio::compile(&world, target, variant.as_ref()).expect("not cancelled");
    let entry = world.manifest().project.entry.v.to_string();
    let text = &project.sources[&entry];
    // The first word of the paragraph that starts after the middle.
    let start = text[text.len() / 2..]
        .find("\n\nThe ")
        .map(|i| text.len() / 2 + i + 2)
        .expect("a paragraph after the middle");
    (world, entry, start..start + 3)
}

/// The M0 exit document, compiled, with a word to edit.
fn compiled_m0_exit() -> (World, String, Range<usize>) {
    compiled(m0_exit())
}

// Recompiles after an edit that adds a word to a paragraph in the middle
// of the document, so that the following pages may reflow: the memoised
// queries and the shaping, line and page caches are warm.
#[library_benchmark]
#[bench::m0_exit(setup = compiled_m0_exit)]
fn recompile_after_edit((mut world, entry, range): (World, String, Range<usize>)) -> usize {
    world
        .edit_source(&entry, range, "This")
        .expect("a valid edit");
    let (variant, target) = first_build(&world);
    let compiled = folio::compile(&world, target, variant.as_ref()).expect("not cancelled");
    black_box(compiled.output.map_or(0, |bytes| bytes.len()))
}

/// The reference document's markup.
fn reference_markup() -> String {
    refdoc::generate(&Spec::REFERENCE).sources["main.fol"].clone()
}

// Parses the reference document's 100 pages of markup.
#[library_benchmark]
#[bench::reference(setup = reference_markup)]
fn parse(text: String) -> usize {
    black_box(folio_syntax::parse(&text).len())
}

library_benchmark_group!(
    name = compile,
    benchmarks = [compile_cold, recompile_after_edit, parse]
);

main!(
    config = LibraryBenchmarkConfig::default()
        // One thread, so that counts do not depend on how work is shared
        // out.
        .env("RAYON_NUM_THREADS", "1")
        .tool(Callgrind::default().soft_limits([(EventKind::Ir, LIMIT)])),
    library_benchmark_groups = compile
);
