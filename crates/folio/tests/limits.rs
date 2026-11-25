//! Limits on property values that would otherwise reach layout and the PDF
//! writer out of range, found by fuzzing past the style stage (SPEC §11.1,
//! §14.2): lengths, numbers and ratios that are not finite, and page sizes
//! outside PDF's limits.

use folio::{Severity, Target, World, diag};

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nstyles = [\"main.fsty\"]\n";

fn build(sheet: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", "= A\n\nSome text.\n").unwrap();
    world.set_source("main.fsty", sheet).unwrap();
    world
}

/// The diagnostics of a PDF build: severity, message and the text they
/// point at.
fn diagnostics(world: &World) -> Vec<(Severity, String, String)> {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    assert!(compiled.output.is_some(), "a PDF is written");
    compiled
        .diagnostics
        .iter()
        .map(|d| {
            let resolved = diag::resolve(world, d);
            let text = resolved.location.map_or(String::new(), |l| {
                world.source(&l.path).unwrap().text()[l.bytes].to_string()
            });
            (d.severity, d.message.to_string(), text)
        })
        .collect()
}

fn page_size(world: &World) -> (f64, f64) {
    let pages = folio::pages(world, Target::Pdf, None).unwrap().unwrap();
    (
        pages.master.size.width.to_pt(),
        pages.master.size.height.to_pt(),
    )
}

#[test]
fn values_that_are_not_finite_are_errors() {
    for (sheet, property, what) in [
        ("doc { font-size: 1e308pt * 10; }", "font-size", "length"),
        ("paragraph { indent: -1e308pt * 10; }", "indent", "length"),
        (
            "page { page-size: 1e308pt * 10 100mm; }",
            "page-size",
            "length",
        ),
        ("paragraph { widows: 1e308 * 10; }", "widows", "number"),
    ] {
        let world = build(sheet);
        let diags = diagnostics(&world);
        let error = diags
            .iter()
            .find(|(severity, ..)| *severity == Severity::Error)
            .unwrap_or_else(|| panic!("{sheet}: no error in {diags:?}"));
        assert_eq!(
            error.1,
            format!("wrong value for `{property}`: this {what} is not finite"),
            "{sheet}"
        );
        assert!(sheet.contains(&error.2), "{sheet}: points at {:?}", error.2);
    }
}

#[test]
fn an_em_length_that_overflows_at_a_font_size_is_left_out() {
    // Finite where it is declared, at a nominal font size, but not at the
    // element's: the value is left out, and the initial one used.
    let world = build("doc { font-size: 1e300pt; }\nheading { space-before: 1e300em; }\n");
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    for page in &pages.pages {
        let mut stack = vec![&page.fragment];
        while let Some(fragment) = stack.pop() {
            assert!(fragment.size.width.is_finite() && fragment.size.height.is_finite());
            for (pos, item) in &fragment.items {
                assert!(pos.x.is_finite() && pos.y.is_finite());
                if let folio::FragmentItem::Group(group) = item {
                    stack.push(group);
                }
            }
        }
    }
}

#[test]
fn page_sizes_outside_pdf_limits_are_clamped_with_a_warning() {
    let world = build("page { page-size: 20000pt 2pt; }\n");
    assert_eq!(page_size(&world), (14_400.0, 3.0));
    let diags = diagnostics(&world);
    let warning = diags
        .iter()
        .find(|(_, message, _)| message.starts_with("this page size"))
        .unwrap_or_else(|| panic!("no warning in {diags:?}"));
    assert_eq!(warning.0, Severity::Warning);
    assert_eq!(
        warning.1,
        "this page size, 20000.0pt by 2.0pt, is outside what PDF allows; \
         the pages are 14400.0pt by 3.0pt"
    );
    assert_eq!(warning.2, "page-size: 20000pt 2pt;");

    // Huge sizes are written in scientific notation.
    let huge = build("page { page-size: 1e300pt 100mm; }\n");
    assert!(
        diagnostics(&huge)
            .iter()
            .any(|(_, message, _)| message.starts_with("this page size, 1e300pt by"))
    );
}

#[test]
fn page_sizes_within_pdf_limits_are_kept_without_a_warning() {
    for (sheet, size) in [
        ("", (595.2755905511812, 841.8897637795276)),
        (
            "page { page-size: A3; }",
            (841.8897637795276, 1190.5511811023623),
        ),
        ("page { page-size: 14400pt 3pt; }", (14_400.0, 3.0)),
    ] {
        let world = build(sheet);
        assert_eq!(page_size(&world), size, "{sheet}");
        let diags = diagnostics(&world);
        assert!(
            !diags
                .iter()
                .any(|(_, m, _)| m.starts_with("this page size")),
            "{sheet}: {diags:?}"
        );
    }
}
