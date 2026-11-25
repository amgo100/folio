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
//!
//! `preview_after_edit` and `restyle` count the preview path (SPEC §14.1,
//! the M1 exit check of §15): the reference document laid out again after
//! an edit, up to its pages and without emit. Both go through [`preview`].
//! `check_after_edit` counts the check path of the same one-word edit
//! (SPEC §12.1, `folio check`): every stage before Emit and the PDF
//! writer's check, which give a build's diagnostics without its file. `compile_variants` counts the ten-page
//! document with three variants: the teacher's variant alone, and all
//! three.
//!
//! Callgrind counts only the benchmark function's thread. Paragraphs that
//! `folio-text` shapes several at a time run on its shaping thread, so
//! their shaping is not counted: a full compile's or a stylesheet edit's
//! counts leave most of the shaping out, and a one-word edit's, which
//! shapes one paragraph on the calling thread, keep it. Gungraun also turns
//! counting on and off on entering and leaving any function of a
//! benchmark's wrapper module, a closure that the body defines and does
//! not inline included, so the bodies here do their work in loops rather
//! than closures (the small closures that only read a length are inlined). Issue #234 tracks counting the shaping thread.

// Gungraun's macros expand to paths the first lint calls redundant, and to
// public items without documentation.
#![allow(unused_qualifications, missing_docs)]

use std::hint::black_box;
use std::ops::Range;

use folio::{BuildMode, Name, Target, World};
use folio_testkit::corpus::{self, Project};
use folio_testkit::refdoc::{self, Edit, Spec};
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
    let edit = Edit::word(&project);
    assert_eq!(edit.path, entry);
    (world, entry, edit.range)
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

/// The preview path: the pages of a project without variants, which is
/// what a preview repaints from; returns their number, and panics when the
/// build cannot be laid out, so that a broken edit is not counted as a
/// cheap one.
fn preview(world: &World) -> usize {
    folio::pages(world, Target::Pdf, None)
        .expect("not cancelled")
        .expect("the build is laid out")
        .pages
        .len()
}

/// The reference document laid out to its pages, and an edit to make.
fn previewed(edit: fn(&Project) -> Edit) -> (World, Edit) {
    let project = refdoc::generate(&Spec::REFERENCE);
    let world = project.world(BuildMode::Build);
    preview(&world);
    let edit = edit(&project);
    (world, edit)
}

/// The reference document laid out, with a one-word edit in its middle.
fn previewed_word_edit() -> (World, Edit) {
    previewed(Edit::word)
}

/// The reference document laid out, with the body's font size changed.
fn previewed_style_edit() -> (World, Edit) {
    previewed(Edit::style)
}

// Lays the reference document out again after an edit: the preview path,
// with the memoised queries and the shaping, line and page caches warm.
#[library_benchmark]
#[bench::reference(setup = previewed_word_edit)]
fn preview_after_edit((mut world, edit): (World, Edit)) -> usize {
    world
        .edit_source(&edit.path, edit.range, edit.with)
        .expect("a valid edit");
    black_box(preview(&world))
}

/// The reference document checked ([`folio::check`]), with a one-word
/// edit in its middle.
fn checked_word_edit() -> (World, Edit) {
    let project = refdoc::generate(&Spec::REFERENCE);
    let world = project.world(BuildMode::Build);
    folio::check(&world, Target::Pdf, None).expect("not cancelled");
    let edit = Edit::word(&project);
    (world, edit)
}

// Checks the reference document again after a one-word edit, as `folio
// check`, the language server and the preview do: every stage before
// Emit and the PDF writer's check, with the memoised queries and the
// shaping, line and page caches warm; returns the number of diagnostics.
#[library_benchmark]
#[bench::reference(setup = checked_word_edit)]
fn check_after_edit((mut world, edit): (World, Edit)) -> usize {
    world
        .edit_source(&edit.path, edit.range, edit.with)
        .expect("a valid edit");
    let checked = folio::check(&world, Target::Pdf, None).expect("not cancelled");
    black_box(checked.diagnostics.len())
}

// Lays the reference document out again after its body's font size
// changes in the stylesheet: every paragraph is set at the new size and
// broken again, and every page reflows.
#[library_benchmark]
#[bench::reference(setup = previewed_style_edit)]
fn restyle((mut world, edit): (World, Edit)) -> usize {
    world
        .edit_source(&edit.path, edit.range, edit.with)
        .expect("a valid edit");
    black_box(preview(&world))
}

/// The variant that `compile_variants::one` compiles: the teacher's, which
/// shows everything and shares its styles with the exam.
const ONE_VARIANT: &[&str] = &["teacher"];

/// The ten-page document with three variants (and answers, which make two
/// of them a little longer), and the variants to compile.
fn m0_exit_variants(variants: &'static [&'static str]) -> (Project, &'static [&'static str]) {
    (refdoc::generate_variants(&Spec::M0_EXIT), variants)
}

// Compiles variants of the ten-page document with three variants in a
// fresh world, to PDF: the teacher's alone, and all three, which share
// what does not read the variant (SPEC §6). The builds run one after
// another on the benchmark's thread rather than through
// `folio::compile_builds`, whose worker threads Callgrind would not count:
// the counts compare the work, not how it is shared out.
#[library_benchmark]
#[bench::one(args = (ONE_VARIANT), setup = m0_exit_variants)]
#[bench::three(args = (&refdoc::VARIANTS), setup = m0_exit_variants)]
fn compile_variants((project, variants): (Project, &[&str])) -> usize {
    // Loops and matches rather than closures: Gungraun toggles counting on
    // entering and leaving any function of the benchmark's wrapper module,
    // a closure in its body included, which would stop counting inside it.
    let world = project.world(BuildMode::Build);
    let mut size = 0;
    for &variant in variants {
        let variant = Name::new(variant).expect("a name");
        let compiled = folio::compile(&world, Target::Pdf, Some(&variant)).expect("not cancelled");
        if let Some(bytes) = compiled.output {
            size += bytes.len();
        }
    }
    black_box(size)
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
    benchmarks = [
        compile_cold,
        recompile_after_edit,
        preview_after_edit,
        check_after_edit,
        restyle,
        compile_variants,
        parse
    ]
);

main!(
    config = LibraryBenchmarkConfig::default()
        // One thread, so that counts do not depend on how work is shared
        // out.
        .env("RAYON_NUM_THREADS", "1")
        .tool(Callgrind::default().soft_limits([(EventKind::Ir, LIMIT)])),
    library_benchmark_groups = compile
);
