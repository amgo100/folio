//! The generated documents (SPEC §14.1): they compile without errors, have
//! their make-up, and fill the pages they are meant to.

use folio::{BuildMode, Target};
use folio_testkit::refdoc::{self, Spec};

/// Compiles a generated document and returns its number of pages.
fn pages(spec: &Spec) -> usize {
    let project = refdoc::generate(spec);
    let world = project.world(BuildMode::Ci);
    assert_eq!(world.builds(), [(None, Target::Pdf)]);
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    folio::pages(&world, Target::Pdf, None)
        .unwrap()
        .unwrap()
        .pages
        .len()
}

#[test]
fn make_up() {
    let spec = Spec::M0_EXIT;
    assert_eq!(
        spec,
        Spec {
            pages: 10,
            figures: 3,
            equations: 40,
            tables: 2
        }
    );
    let project = refdoc::generate(&spec);
    assert_eq!(
        project,
        refdoc::generate(&spec),
        "generation is deterministic"
    );
    let main = &project.sources["main.fol"];
    assert_eq!(main.matches("#figure[").count(), spec.figures);
    assert_eq!(main.matches("\nEquation ").count(), spec.equations);
    assert_eq!(main.matches("\nTable ").count(), spec.tables);
    assert_eq!(refdoc::generate(&Spec::REFERENCE).name, "reference");
}

#[test]
fn fills_its_pages() {
    // Within a tenth either way, so that small changes to layout do not
    // need the generator re-tuned. The reference document is checked by
    // `folio-wall`, which builds it optimised.
    for target in [2, 10] {
        let got = pages(&Spec::pages(target));
        let slack = target.div_ceil(10);
        assert!(
            got.abs_diff(target) <= slack,
            "a document meant for {target} pages has {got}: re-tune refdoc::CHARS_PER_PAGE"
        );
    }
}
