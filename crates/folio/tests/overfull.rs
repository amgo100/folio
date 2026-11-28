//! Lines that cannot be filled or that overflow (SPEC §7.2, §7.9, #351):
//! a paragraph of wide inline quantities is broken without a lone
//! fragment, and an overfull line reports the object that does not fit,
//! with hints that suggest only settings that are not on.

use folio::{Diagnostic, Target, World};

fn world(style: &str, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    // The maths corpus's page and text.
    let page = "page { page-size: A5; margin-top: 18mm; margin-bottom: 18mm; \
                margin-inner: 16mm; margin-outer: 16mm; }\n\
                doc { font: \"Source Serif 4\"; font-size: 10.5pt; justify: true; \
                hyphenate: true; }\n";
    world
        .set_source("styles/main.fsty", &format!("{page}{style}"))
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

/// The layout diagnostics of a code, each with the source text it points
/// at.
fn found(world: &World, code: &str) -> Vec<(Diagnostic, String)> {
    let source = world.source("main.fol").unwrap();
    folio::check(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some(code))
        .map(|d| {
            let at = folio::diag::locate(world, d.span)
                .map(|l| source.text()[l.bytes].to_string())
                .unwrap_or_default();
            (d.clone(), at)
        })
        .collect()
}

const QUANTITIES: &str = "Quantities $g = qty(9.81, \"m s^-2\")$, $qty(2.31, \"m\", pm: 0.02)$, \
                          $qty(1.60e-19, \"C\")$, $qty(3.0e8, \"m s^-1\")$, $qty(25, \"deg\")$, \
                          $qty(20, \"degC\")$, $num(6.02e23)$, $unit(\"kg m^2 s^-1\")$, \
                          $qty(15000, \"Pa\")$.\n";

#[test]
fn wide_quantities_leave_no_lone_fragment() {
    let world = world("", QUANTITIES);
    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let layout = &lines.paragraphs[0];
    let shown = text.paragraphs[0].shaped.text();
    let first = &shown[layout.lines[0].range.clone()];
    assert!(first.contains('\u{fffc}'), "the first line is {first:?}");
    // No line but the last is less than half the width.
    for line in &layout.lines[..layout.lines.len() - 1] {
        assert!(line.width > lines.width * 0.5, "{line:?}");
    }
}

#[test]
fn an_overfull_formula_is_reported_at_the_formula() {
    let world = world(
        "",
        "A long formula $(a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r \
         + s + t + u + v + w + x + y + z)$ in parentheses.\n",
    );
    let overfull = found(&world, "overfull-line");
    assert_eq!(overfull.len(), 1, "{overfull:?}");
    let (diagnostic, at) = &overfull[0];
    assert!(at.starts_with("$(a + b"), "{at:?}");
    let hints = diagnostic.hints.join(" ");
    assert!(hints.contains("formula"), "{hints}");
    assert!(!hints.contains("hyphenate: true"), "{hints}");
}

#[test]
fn hints_suggest_only_settings_that_are_off() {
    // Digits, which no hyphenation pattern breaks.
    let word = "1234567890".repeat(8);
    // Hyphenation on: an overfull word's hint does not ask for it.
    let on = world("", &format!("{word}\n"));
    let hints = |world: &World, code: &str| -> String {
        found(world, code)
            .iter()
            .flat_map(|(d, _)| d.hints.iter().map(|h| h.to_string()))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let overfull = hints(&on, "overfull-line");
    assert!(
        !overfull.is_empty() && !overfull.contains("hyphenate: true"),
        "{overfull}"
    );
    // Hyphenation off: it does.
    let off = world("doc { hyphenate: false; }\n", &format!("{word}\n"));
    assert!(hints(&off, "overfull-line").contains("hyphenate: true"));
    // An underfull justified line with hyphenation on: no hint to turn it
    // on, one to stop justifying.
    let underfull = hints(&world("", QUANTITIES), "underfull-line");
    assert!(!underfull.contains("hyphenate: true"), "{underfull}");
    assert!(underfull.contains("justify: false"), "{underfull}");
}
