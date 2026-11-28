//! Checking without emitting (SPEC §12.1, §12.2): `folio::check` reports
//! the diagnostics that `folio::compile` reports, the PDF writer's
//! included, without writing a file; and the pages of a build carry
//! content hashes that tell which pages an edit changed (SPEC §2.3).

use std::fs;
use std::path::{Path, PathBuf};

use folio::{Name, Target, World};

const MANIFEST: &str =
    "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";

/// The corpus directory.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// A world with a corpus project: its manifest and every source file.
fn project(dir: &Path) -> World {
    let manifest = fs::read_to_string(dir.join("folio.toml")).unwrap();
    let mut world = World::new(&manifest).unwrap();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("fol" | "fsty")
            ) {
                let relative = path.strip_prefix(dir).unwrap().to_str().unwrap();
                let text = fs::read_to_string(&path).unwrap();
                world
                    .set_source(&relative.replace('\\', "/"), &text)
                    .unwrap();
            }
        }
    }
    world
}

/// Every build of every corpus project reports the same diagnostics when
/// checked as when compiled, and the same files read.
#[test]
fn checks_report_what_builds_report() {
    let mut projects: Vec<PathBuf> = fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("folio.toml").exists())
        .collect();
    projects.sort();
    assert!(!projects.is_empty());
    for dir in projects {
        let world = project(&dir);
        for (variant, target) in world.builds() {
            // Checked first in one world, compiled first in another, so
            // neither reuses what the other found.
            let checked = folio::check(&world, target, variant.as_ref()).unwrap();
            let compiled = folio::compile(&project(&dir), target, variant.as_ref()).unwrap();
            assert_eq!(
                checked.diagnostics,
                compiled.diagnostics,
                "{} {variant:?} {target}",
                dir.display()
            );
            assert_eq!(checked.sources, compiled.sources);
            // Compiling after checking emits what was checked.
            let after = folio::compile(&world, target, variant.as_ref()).unwrap();
            assert_eq!(after, compiled);
        }
    }
}

/// The PDF writer's warnings are diagnostics of the check: here why a
/// document with a title cannot claim PDF/UA-1.
#[test]
fn checks_report_the_pdf_writers_warnings() {
    let mut world = World::new(MANIFEST).unwrap();
    // A no-break space draws a glyph but gives the heading no text.
    let main = "#doc(title: [Forces])\n\n= ~\n\nA ball falls.\n";
    world.set_source("main.fol", main).unwrap();
    let checked = folio::check(&world, Target::Pdf, None).unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.message.contains("this heading has no text")),
        "{:?}",
        checked.diagnostics
    );
    assert_eq!(checked.diagnostics, compiled.diagnostics);
    let pdf = String::from_utf8_lossy(compiled.output.as_deref().unwrap()).into_owned();
    assert!(!pdf.contains("pdfuaid"));
}

/// Targets without a writer report so when checked, as when built.
#[test]
fn checks_report_targets_without_a_writer() {
    let manifest = format!("{MANIFEST}\n[variants.exam]\ntargets = [\"pdf\", \"docx\"]\n");
    let mut world = World::new(&manifest).unwrap();
    world.set_source("main.fol", "A ball falls.\n").unwrap();
    let exam = Name::new("exam").unwrap();
    let outputs = folio::check_builds(&world, world.builds());
    assert_eq!(outputs.len(), 2);
    let docx = outputs[1].result.as_ref().unwrap();
    assert_eq!(outputs[1].target, Target::Docx);
    assert_eq!(
        docx.diagnostics[0].message,
        "DOCX output is not implemented yet: this build checks the document but writes no file"
    );
    let compiled = folio::compile(&world, Target::Docx, Some(&exam)).unwrap();
    assert_eq!(docx.diagnostics, compiled.diagnostics);
}

/// A document of many pages: a short first paragraph, then paragraphs
/// long enough to fill several pages.
fn long_document(first: &str) -> String {
    let mut text = format!("#doc(title: [Motion])\n\n{first}\n\n");
    for i in 0..60 {
        text.push_str(&format!(
            "Paragraph {i}: a ball rolls down a slope and speeds up as it goes, \
             since the component of its weight along the slope is not balanced. \
             Its speed grows by the same amount every second.\n\n"
        ));
    }
    text
}

/// An edit on the first page that does not reflow the document changes
/// that page's hash and no other, wherever the edit moved the source
/// numbers of what follows.
#[test]
fn an_edit_that_does_not_reflow_changes_one_page_hash() {
    let mut world = World::new(MANIFEST).unwrap();
    world
        .set_source("main.fol", &long_document("A ball falls."))
        .unwrap();
    let before = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert!(before.pages.len() >= 3, "{} pages", before.pages.len());
    assert_eq!(before.hashes.len(), before.pages.len());
    // Pages that show different content hash differently.
    assert_ne!(before.hashes[1], before.hashes[2]);

    world
        .set_source("main.fol", &long_document("A bell falls."))
        .unwrap();
    let after = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(after.pages.len(), before.pages.len());
    for (b, a) in before.pages.iter().zip(&after.pages) {
        assert_eq!(b.lines, a.lines, "the edit reflowed the document");
    }
    assert_ne!(after.hashes[0], before.hashes[0]);
    assert_eq!(after.hashes[1..], before.hashes[1..]);
    assert_eq!(after.changed(&before), [0]);
    assert!(!after.remapped(&before).contains(&0));

    // Back as it was, the first page hashes as it did.
    world
        .set_source("main.fol", &long_document("A ball falls."))
        .unwrap();
    let again = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(again.hashes, before.hashes);
    assert!(again.changed(&before).is_empty());
}

/// Hashes leave out the sources that fragments and glyphs name: the same
/// page made from another file, whose nodes have other numbers, has the
/// same hash.
#[test]
fn page_hashes_leave_out_sources() {
    let text = long_document("A ball falls.");
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", &text).unwrap();
    let before = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();

    let mut moved = World::new(MANIFEST).unwrap();
    moved
        .set_source("main.fol", "#include \"chapter.fol\"\n")
        .unwrap();
    moved.set_source("chapter.fol", &text).unwrap();
    let after = folio::pages(&moved, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(after.pages.len(), before.pages.len());
    // The pages name other sources, but show the same: a watcher sends
    // their span maps and draws none of them again.
    assert_ne!(after.pages[1], before.pages[1]);
    assert_eq!(after.hashes, before.hashes);
    assert!(after.changed(&before).is_empty());
    let all: Vec<usize> = (0..after.pages.len()).collect();
    assert_eq!(after.remapped(&before), all);
    assert!(before.remapped(&before).is_empty());
}

/// The baselines of the glyph runs of a page, from the top.
fn baselines(fragment: &folio::Fragment, y: f64, out: &mut Vec<f64>) {
    for (pos, item) in &fragment.items {
        let y = y + pos.y.to_pt();
        match item {
            folio::FragmentItem::Group(group) => baselines(group, y, out),
            folio::FragmentItem::Text(_) => out.push(y),
            folio::FragmentItem::Shape { .. }
            | folio::FragmentItem::Object(_)
            | folio::FragmentItem::Image(_) => {}
        }
    }
}

/// Paragraphs that are equal in every way, as those a loop makes, are
/// still two paragraphs: they are set a paragraph apart, as two that
/// differ only in their source are, though the flow shares equal
/// paragraphs between revisions.
#[test]
fn equal_paragraphs_stay_apart() {
    let lines = |main: &str| {
        let mut world = World::new(MANIFEST).unwrap();
        world.set_source("main.fol", main).unwrap();
        let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
        let mut out = Vec::new();
        baselines(&pages.pages[0].fragment, 0.0, &mut out);
        out
    };
    let looped = lines("#for i in range(2) [\n\nSame text.\n\n]\n");
    let written = lines("Same text.\n\nSame text.\n");
    assert_eq!(looped.len(), 2, "{looped:?}");
    assert_eq!(written.len(), 2, "{written:?}");
    let gap = |b: &[f64]| b[1] - b[0];
    assert!(
        (gap(&looped) - gap(&written)).abs() < 1e-6,
        "{looped:?} {written:?}"
    );
}

/// Each kind of layout diagnostic (SPEC §7.9) is reported once on the
/// fixture in `tests/check/layout`, with its code, at the source it is
/// about: a line at the text it ends with, a paragraph at its text, a
/// section at its heading, what does not fit at its content.
#[test]
fn layout_diagnostics_have_codes_and_spans() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/check/layout");
    let world = project(&dir);
    let checked = folio::check(&world, Target::Pdf, None).unwrap();
    let source = world.source("main.fol").unwrap();
    let at = |code: &str| -> String {
        let found: Vec<_> = checked
            .diagnostics
            .iter()
            .filter(|d| d.code.as_deref() == Some(code))
            .collect();
        assert_eq!(found.len(), 1, "{code}: {:?}", checked.diagnostics);
        assert_eq!(found[0].severity, folio::Severity::Warning);
        let location = folio::diag::locate(&world, found[0].span).unwrap();
        source.text()[location.bytes].to_string()
    };
    assert_eq!(
        at("overfull-line"),
        "Pneumonoultramicroscopicsilicovolcanoconiosis"
    );
    // The final pass sets the fuller of the lines it cannot fill (#351).
    assert_eq!(at("underfull-line"), "Stretched spaces in");
    assert!(at("widow").starts_with("The widow paragraph"));
    assert!(at("orphan").starts_with("The orphan paragraph"));
    assert_eq!(at("broken-keep-together"), "heading(class: \"kept\")[Kept]");
    assert_eq!(at("broken-unbreakable"), "heading(class: \"whole\")[Whole]");
    assert_eq!(
        at("broken-keep-with-next"),
        "heading(class: \"next\")[Next]"
    );
    assert_eq!(at("page-overflow"), "Tall");
    // A reference to a label no element has is a warning at the
    // reference, without a code yet.
    assert!(checked.diagnostics.iter().any(|d| {
        d.message.contains("no element has the label")
            && folio::diag::locate(&world, d.span)
                .is_some_and(|l| &source.text()[l.bytes] == "@nowhere")
    }));
    // Compiling reports the same.
    let compiled = folio::compile(&project(&dir), Target::Pdf, None).unwrap();
    assert_eq!(checked.diagnostics, compiled.diagnostics);
}

/// The codes of the fixture's layout diagnostics that `layout` shows, in
/// order.
fn layout_codes(diagnostics: &[folio::Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter_map(|d| d.code.as_deref())
        .map(str::to_string)
        .collect()
}

/// Content that runs off its line or page is always shown; underfull
/// lines and relaxed widows, orphans, keeps and unbreakable blocks only
/// with every layout diagnostic (SPEC §7.9).
#[test]
fn only_serious_layout_diagnostics_are_shown_by_default() {
    use folio::LayoutDiagnostics;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/check/layout");
    let world = project(&dir);
    let all = folio::check_with(&world, Target::Pdf, None, LayoutDiagnostics::All).unwrap();
    assert_eq!(all, folio::check(&world, Target::Pdf, None).unwrap());
    let serious =
        folio::check_with(&world, Target::Pdf, None, LayoutDiagnostics::default()).unwrap();
    assert_eq!(
        layout_codes(&serious.diagnostics),
        ["page-overflow", "overfull-line"]
    );
    let mut codes = layout_codes(&all.diagnostics);
    codes.sort();
    let mut expected: Vec<&str> = folio::layout::DETAIL_CODES.to_vec();
    expected.extend(["overfull-line", "page-overflow"]);
    expected.sort();
    assert_eq!(codes, expected);
    // Every other diagnostic is kept, in its order.
    let others = |d: &[folio::Diagnostic]| -> Vec<folio::Diagnostic> {
        d.iter().filter(|d| d.code.is_none()).cloned().collect()
    };
    assert!(!others(&serious.diagnostics).is_empty());
    assert_eq!(others(&serious.diagnostics), others(&all.diagnostics));
}
