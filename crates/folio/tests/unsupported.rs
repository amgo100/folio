//! What Folio does not lay out or implement yet (SPEC §7.9): each is a
//! warning with its span, so that an author, or an agent working from the
//! diagnostics, can tell that the output leaves something out. Each
//! warning goes with the work that implements its feature.

use folio::{Compiled, Severity, Target, World, diag};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]
"#;

fn world(style: &str, text: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn compile(world: &World, target: Target) -> Compiled {
    folio::compile(world, target, None).unwrap()
}

/// Each diagnostic's severity, message and the source text it points at.
fn located(world: &World, compiled: &Compiled) -> Vec<(Severity, String, String)> {
    compiled
        .diagnostics
        .iter()
        .map(|d| {
            let at = diag::locate(world, d.span).map_or(String::new(), |l| {
                let source = world.source(&l.path).unwrap();
                source.text()[l.bytes].to_string()
            });
            (d.severity, d.message.to_string(), at)
        })
        .collect()
}

fn warning(message: &str, at: &str) -> (Severity, String, String) {
    (Severity::Warning, message.into(), at.into())
}

#[test]
fn a_reference_and_maths_are_reported() {
    let world = world("", "See @nowhere and $x^2$ here.\n");
    let compiled = compile(&world, Target::Pdf);
    assert_eq!(
        located(&world, &compiled),
        [
            warning(
                "no element has the label `<nowhere>`, so the reference has nothing to refer to",
                "@nowhere"
            ),
            warning(
                "references are not laid out yet, so the output leaves them out",
                "@nowhere"
            ),
            warning(
                "maths is not laid out yet, so the output leaves it out",
                "$x^2$"
            ),
        ]
    );
    assert!(compiled.output.is_some());
}

#[test]
fn lists_the_warnings_for_what_is_not_supported() {
    let style = "page:first { margin-top: 1cm; header: \"Ignored\"; }\n\
                 page:nth(2) { margin-top: 1cm; }\n\
                 page[size] { margin-top: 1cm; }\n\
                 page:has(heading) { margin-top: 1cm; }\n\
                 figure { side: outer; }\n\
                 heading { counter-reset: q; marker: none; }\n\
                 page:left { margin-top: 1cm; }\n\
                 page.landscape { margin-top: 1cm; }\n\
                 heading { numbering: \"1.\"; }\n\
                 emph { display: hidden; }\n\
                 paragraph { width: 50%; grid-snap: false; }\n\
                 page { baseline-grid: 12pt; header: \"Exam\"; footer: none; }\n\
                 figure { placement: top; }\n\
                 figure.here { placement: here; }\n\
                 strong { template: \"bold\"; }\n";
    let text = "= Forces <forces>\n\n\
                See @forces, $x$ and #image(\"ball.png\"), then $y$ and @forces.\n";
    let mut world = world(style, text);
    world
        .set_file("ball.png", b"not read yet".to_vec())
        .unwrap();
    let compiled = compile(&world, Target::Pdf);
    let not_implemented =
        |prop: &str| format!("`{prop}` is not implemented yet, so this declaration has no effect");
    let expected = [
        warning(
            "`page:first` cannot match yet: pages are not told apart by position, so the rule is ignored",
            ":first",
        ),
        warning(
            "`page:left` cannot match yet: pages are not told apart by position, so the rule is ignored",
            ":left",
        ),
        warning(
            "`page.landscape` cannot match yet: there are no named page masters, so the rule is ignored",
            "page.landscape",
        ),
        warning(
            "`page:nth(2)` cannot match yet: pages are not told apart by position, so the rule is ignored",
            ":nth(2)",
        ),
        warning(
            "this page selector cannot match yet: a page has no label or fields, so the rule is ignored",
            "page[size]",
        ),
        warning(
            "`page:has(..)` cannot match yet: a page has no children or steps, so the rule is ignored",
            ":has(heading)",
        ),
        warning(&not_implemented("side"), "side: outer;"),
        warning(&not_implemented("counter-reset"), "counter-reset: q;"),
        warning(&not_implemented("marker"), "marker: none;"),
        // The `header` of the ignored `page:first` rule is not reported.
        warning(&not_implemented("numbering"), "numbering: \"1.\";"),
        warning(&not_implemented("display"), "display: hidden;"),
        warning(&not_implemented("width"), "width: 50%;"),
        warning(&not_implemented("grid-snap"), "grid-snap: false;"),
        warning(&not_implemented("baseline-grid"), "baseline-grid: 12pt;"),
        warning(&not_implemented("header"), "header: \"Exam\";"),
        warning(&not_implemented("placement"), "placement: top;"),
        warning(&not_implemented("template"), "template: \"bold\";"),
        // Once per build and kind, at the first.
        warning(
            "references are not laid out yet, so the output leaves them out",
            "@forces",
        ),
        warning(
            "maths is not laid out yet, so the output leaves it out",
            "$x$",
        ),
        warning(
            "images are not laid out yet, so the output leaves them out",
            "image(\"ball.png\")",
        ),
    ];
    let mut got = located(&world, &compiled);
    got.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn a_build_that_is_not_laid_out_does_not_warn_about_layout() {
    let world = world("", "See $x$ here.\n");
    let messages: Vec<String> = compile(&world, Target::Docx)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("DOCX output is not implemented yet"));
}

#[test]
fn a_reference_to_a_missing_label_suggests_a_close_one() {
    let world = world("", "= Forces <forces>\n\nSee @force.\n");
    let compiled = compile(&world, Target::Pdf);
    let missing = compiled
        .diagnostics
        .iter()
        .find(|d| d.message.starts_with("no element has the label"))
        .unwrap();
    assert_eq!(missing.hints[0], "did you mean `<forces>`?");
}

#[test]
fn rules_of_other_targets_and_other_pseudo_classes() {
    // A rule for another target has no effect in this build either way, and
    // a page selector's other errors stay errors.
    let world = world(
        "@target docx { heading { numbering: \"1.\"; } }\n\
         page:foo { margin-top: 1cm; }\n\
         page:nth(0) { margin-top: 1cm; }\n\
         figure { wrap-gap: 0.6em; }\n",
        "Text.\n",
    );
    let messages: Vec<String> = compile(&world, Target::Pdf)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(
        messages,
        [
            // The parser's.
            "positions are counted from 1",
            "unknown pseudo-class `:foo`; the rule is ignored",
            // 6pt at a 10pt font size, as the initial value, but not at
            // every size.
            "`wrap-gap` is not implemented yet, so this declaration has no effect",
        ]
    );
}
