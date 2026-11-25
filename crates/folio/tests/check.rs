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
