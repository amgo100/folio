//! Layout golden files (SPEC §14.2): the pages of every build of the test
//! corpus that goes through Folio's layout, dumped as text
//! ([`folio_testkit::golden::dump_pages`]) and compared with
//! `tests/golden/layout/<project>-<variant>.<target>.txt`; and the pages of
//! a document with fixed-size test boxes for its objects, inline and as a
//! block ([`folio_testkit::golden::boxes`]), in `boxes.pdf.txt`.
//!
//! After a change meant to alter layout, run `scripts/bless.sh` and review
//! the diff before committing it.

use std::path::PathBuf;

use folio::BuildMode;
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
            let path = dir.join(&name);
            if let Err(message) = golden::check(&path, &golden::dump_pages(&world, &pages)) {
                failures.push(message);
            }
            expected.push(path);
        }
    }
    assert!(!expected.is_empty(), "the corpus has no laid-out builds");
    let (world, pages) = golden::boxes();
    let path = dir.join("boxes.pdf.txt");
    if let Err(message) = golden::check(&path, &golden::dump_pages(&world, &pages)) {
        failures.push(message);
    }
    expected.push(path);
    if let Err(message) = golden::prune(&dir, "txt", &expected) {
        failures.push(message);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
