//! Leak tests on every variant of the test corpus (SPEC §6.4, §14.2): no
//! build of any corpus project, every variant, version and target, lets
//! what its variant does not show reach its output, by provenance or by
//! the text of the written file.

use folio::BuildMode;
use folio_testkit::corpus;

#[test]
fn no_corpus_build_leaks() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for project in corpus::projects() {
        let world = project.world(BuildMode::Ci);
        for build in world.versioned_builds() {
            let (variant, version, target) = (build.variant, build.version, build.target);
            let leaks = folio::check_leaks(&world, target, variant.as_ref(), version.as_ref())
                .expect("nothing cancels the check");
            checked += 1;
            for leak in leaks {
                failures.push(format!(
                    "{} ({}): {}",
                    project.versioned_build_name(variant.as_ref(), version.as_ref()),
                    target.as_str(),
                    leak.message
                ));
            }
        }
    }
    assert!(checked > 0, "the corpus has builds");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
