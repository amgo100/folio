//! Layout golden files (SPEC §14.2): the pages of every build of the test
//! corpus that goes through Folio's layout, dumped as text
//! ([`folio_testkit::golden::dump_pages`]) and compared with
//! `tests/golden/layout/<project>-<variant>.<target>.txt`, and drawn by
//! `folio-render` ([`folio_testkit::golden::page_images`]) and compared
//! with `tests/golden/layout/<project>-<variant>.<target>-<page>.png`; and
//! the same for a document with fixed-size test boxes for its objects,
//! inline and as a block ([`folio_testkit::golden::boxes`]), in
//! `boxes.pdf.txt` and `boxes.pdf-<page>.png`; and for a document with
//! footnotes ([`folio_testkit::golden::footnotes`]), in `footnotes.pdf.txt`
//! and `footnotes.pdf-<page>.png`. Each page's display list
//! also goes through the wire format and back unchanged.
//!
//! After a change meant to alter layout or drawing, run `scripts/bless.sh`
//! and review the diff, images included, before committing it.

use std::path::PathBuf;

use folio::{BuildMode, Pages, World};
use folio_render::wire;
use folio_testkit::{corpus, golden};

#[test]
fn layout() {
    let dir = golden::dir().join("layout");
    let mut expected: Vec<PathBuf> = Vec::new();
    let mut failures = Vec::new();
    for project in corpus::projects() {
        let world = project.world(BuildMode::Ci);
        for (variant, target) in world.builds() {
            if !corpus::laid_out(target) {
                continue;
            }
            let name = format!(
                "{}.{}.txt",
                project.build_name(variant.as_ref()),
                target.as_str()
            );
            let pages = folio::pages(&world, target, variant.as_ref())
                .expect("nothing cancels the build")
                .unwrap_or_else(|| panic!("{name}: the document could not be laid out"));
            check_build(&dir, &name, &world, &pages, &mut expected, &mut failures);
        }
    }
    assert!(!expected.is_empty(), "the corpus has no laid-out builds");
    let (world, pages) = golden::boxes();
    check_build(
        &dir,
        "boxes.pdf.txt",
        &world,
        &pages,
        &mut expected,
        &mut failures,
    );
    let (world, pages) = golden::footnotes();
    check_build(
        &dir,
        "footnotes.pdf.txt",
        &world,
        &pages,
        &mut expected,
        &mut failures,
    );
    for ext in ["txt", "png"] {
        if let Err(message) = golden::prune(&dir, ext, &expected) {
            failures.push(message);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Checks a build's dump against `name` in `dir`, and its page images
/// against the PNG files named after it, one per page; and that each
/// page's display list survives the wire format.
fn check_build(
    dir: &std::path::Path,
    name: &str,
    world: &World,
    pages: &Pages,
    expected: &mut Vec<PathBuf>,
    failures: &mut Vec<String>,
) {
    let path = dir.join(name);
    if let Err(message) = golden::check(&path, &golden::dump_pages(world, pages)) {
        failures.push(message);
    }
    expected.push(path);
    for (i, list) in golden::display_lists(pages).into_iter().enumerate() {
        if wire::decode(&wire::encode(&list)) != Ok(wire::Message::List(list)) {
            failures.push(format!(
                "{name}: page {} does not survive the wire format",
                i + 1
            ));
        }
    }
    let stem = name.strip_suffix(".txt").unwrap_or(name);
    for (i, image) in golden::page_images(world, pages).iter().enumerate() {
        let path = dir.join(format!("{stem}-{}.png", i + 1));
        if let Err(message) = golden::check_image(&path, image) {
            failures.push(message);
        }
        expected.push(path);
    }
}
