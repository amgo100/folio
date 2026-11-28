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
fn a_reference_is_reported_and_maths_is_laid_out() {
    let world = world("", "See @nowhere and $x^2$ here.\n");
    let compiled = compile(&world, Target::Pdf);
    assert_eq!(
        located(&world, &compiled),
        [warning(
            "no element has the label `<nowhere>`, so the reference has nothing to refer to",
            "@nowhere"
        )]
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
                 enum-item { marker-position: hanging 8mm; }\n\
                 page:left { margin-top: 1cm; }\n\
                 page.landscape { margin-top: 1cm; }\n\
                 paragraph { width: 50%; grid-snap: false; }\n\
                 page { baseline-grid: 12pt; header: \"Exam\"; footer: none; }\n\
                 figure { placement: top; }\n\
                 figure.here { placement: here; }\n\
                 strong { template: \"bold\"; }\n";
    let text = "= Forces <forces>\n\n\
                See @forces, $x$ and #image(\"ball.png\"), then $y$ and @forces.\n\n\
                + Item\n";
    let mut world = world(style, text);
    world
        .set_file(
            "ball.png",
            include_bytes!("../../../tests/corpus/figures/assets/wave.png").to_vec(),
        )
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
        warning(
            &not_implemented("marker-position"),
            "marker-position: hanging 8mm;",
        ),
        // The `header` of the ignored `page:first` rule is not reported.
        // Only images read `width`: at the element, once per kind.
        warning(
            "`width` is not implemented yet for `paragraph`, so it has no effect",
            "See",
        ),
        warning(&not_implemented("grid-snap"), "grid-snap: false;"),
        warning(&not_implemented("baseline-grid"), "baseline-grid: 12pt;"),
        warning(&not_implemented("header"), "header: \"Exam\";"),
        warning(&not_implemented("placement"), "placement: top;"),
        warning(&not_implemented("template"), "template: \"bold\";"),
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
        "@target docx { heading { marker-position: hanging; } }\n\
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

#[test]
fn a_render_block_is_reported_in_a_laid_out_build() {
    let world = world("render strong {\n  body: slot(body);\n}\n", "Text.\n");
    let message = "render templates are not implemented yet, so this `render` block has no effect";
    let pdf: Vec<String> = compile(&world, Target::Pdf)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(pdf, [message]);
    let docx = compile(&world, Target::Docx);
    assert!(docx.diagnostics.iter().all(|d| d.message != message));
}

#[test]
fn display_maths_and_page_rules_of_other_targets() {
    // Display and inline maths are laid out, and a rule for another target
    // does not warn.
    let world = world(
        "@target docx { page:first { margin-top: 1cm; } }\n",
        "Text.\n\n$ x^2 $\n\nMore $y$.\n",
    );
    let messages: Vec<String> = compile(&world, Target::Pdf)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn hints_and_overrides_in_content_warn_as_declarations_do() {
    // The same warnings as a stylesheet's declarations, at the element,
    // and none for an initial value or another target's field.
    let world = world(
        "",
        "#figure(placement: top)[]\n\n\
         #figure(placement: here, width@docx: 50%)[]\n\n\
         #heading(override: (marker-position: \"hanging\", space-before: 2pt))[Forces]\n",
    );
    let compiled = compile(&world, Target::Pdf);
    let not_implemented =
        |prop: &str| format!("`{prop}` is not implemented yet, so this declaration has no effect");
    assert_eq!(
        located(&world, &compiled),
        [
            warning(&not_implemented("placement"), "figure(placement: top)[]"),
            warning(
                &not_implemented("marker-position"),
                "heading(override: (marker-position: \"hanging\", space-before: 2pt))[Forces]"
            ),
        ]
    );
}

#[test]
fn scoped_rules_warn_only_when_they_apply() {
    let world = world(
        "",
        "#rule @target docx heading { marker-position: hanging; }\n\
         #rule @variant markscheme emph { side: outer; }\n\
         #rule strong { side: outer; }\n\
         Text.\n",
    );
    let compiled = compile(&world, Target::Pdf);
    assert_eq!(
        located(&world, &compiled),
        [warning(
            "`side` is not implemented yet, so this declaration has no effect",
            "side: outer;"
        )]
    );
    // The same rules in a DOCX build: the first applies there.
    let docx: Vec<String> = compile(&world, Target::Docx)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .filter(|m| m.contains("is not implemented yet, so this declaration"))
        .collect();
    assert_eq!(
        docx,
        [
            "`marker-position` is not implemented yet, so this declaration has no effect",
            "`side` is not implemented yet, so this declaration has no effect",
        ]
    );
}
