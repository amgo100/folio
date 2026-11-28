//! The Select stage (SPEC §2.1, §6.2, §6.3) through the pipeline: what
//! each variant shows by `display`, `show-only`, `#in-variant` and
//! `#except-variant`, placeholders, the references to what a variant does
//! not show (§4.5), and the lint `variant-condition`.

use folio::{Compiled, Name, Severity, Target, World};

fn manifest(extra: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n{extra}\n\
         [variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\nstyles = [\"styles/markscheme.fsty\"]\n"
    )
}

fn world(style: &str, markscheme: &str, text: &str) -> World {
    world_with("", style, markscheme, text)
}

fn world_with(extra: &str, style: &str, markscheme: &str, text: &str) -> World {
    let mut world = World::new(&manifest(extra)).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world
        .set_source("styles/markscheme.fsty", markscheme)
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn name(variant: &str) -> Name {
    Name::new(variant).unwrap()
}

/// The text of every paragraph a variant sets.
fn paragraphs(world: &World, variant: &str) -> Vec<String> {
    folio::text(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', "~"))
        .collect()
}

fn compile(world: &World, variant: &str) -> Compiled {
    folio::compile(world, Target::Pdf, Some(&name(variant))).unwrap()
}

/// Each diagnostic of a variant's build: its severity, code and message.
fn diagnostics(world: &World, variant: &str) -> Vec<(Severity, String, String)> {
    compile(world, variant)
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.severity,
                d.code.as_deref().unwrap_or_default().to_string(),
                d.message.to_string(),
            )
        })
        .collect()
}

/// The document tree a variant shows, as text.
fn selected(world: &World, variant: &str) -> String {
    folio::select(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
        .repr()
}

/// SPEC §6.2: one source, two variants, visibility decided by element in
/// the stylesheet. The exam leaves out the marks and the solution; the
/// mark scheme overlay shows them.
#[test]
fn the_overlay_decides_what_each_variant_shows() {
    let world = world(
        "figure.solution { display: none; }\nstrong.mark { display: none; }\n",
        "@variant markscheme {\n  figure.solution { display: block; }\n  \
         strong.mark { display: inline; }\n}\n",
        "A ball falls for 2 s. Find its speed. #strong(class: \"mark\")[[2]]\n\n\
         #figure(class: \"solution\")[SOLUTION: 19.6 m/s.]\n\n\
         Next question.\n",
    );
    assert_eq!(
        paragraphs(&world, "exam"),
        ["A ball falls for 2 s. Find its speed.", "Next question."]
    );
    assert_eq!(
        paragraphs(&world, "markscheme"),
        [
            "A ball falls for 2 s. Find its speed. [2]",
            "SOLUTION: 19.6 m/s.",
            "Next question."
        ]
    );
    assert!(!selected(&world, "exam").contains("SOLUTION"));
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
    assert!(diagnostics(&world, "markscheme").is_empty());
}

/// A variant that hides nothing passes the document tree on as it is.
#[test]
fn a_variant_that_hides_nothing_shows_the_document_tree() {
    let world = world("", "", "= Forces\n\nA paragraph.\n");
    let document = folio::document(&world, Some(&name("exam")))
        .unwrap()
        .unwrap();
    assert_eq!(selected(&world, "exam"), document.repr());
}

/// What is left out is not counted: the numbers follow what the variant
/// shows (shared numbering is numbering groups', SPEC §6.1).
#[test]
fn what_is_left_out_is_not_counted() {
    let world = world(
        "heading { numbering: \"1.\"; }\nheading.extra { display: none; }\n\
         @variant markscheme { heading.extra { display: block; } }\n",
        "",
        "= First\n\n#heading(class: \"extra\")[Extra]\n\n= Last\n",
    );
    assert_eq!(paragraphs(&world, "exam"), ["1.~First", "2.~Last"]);
    assert_eq!(
        paragraphs(&world, "markscheme"),
        ["1.~First", "2.~Extra", "3.~Last"]
    );
}

/// `#in-variant` and `#except-variant` (SPEC §6.2, mechanism 2), inline
/// in a sentence and on a line of their own: the paragraph that would hold
/// nothing goes, and the spaces around a left-out element collapse.
#[test]
fn in_variant_and_except_variant() {
    let world = world(
        "",
        "",
        "The range is #in-variant(\"markscheme\")[7.3 m] #except-variant(\"markscheme\")[to be found].\n\n\
         #in-variant(\"markscheme\")[Accept 7.2 to 7.4 m.]\n\n\
         Next.\n",
    );
    assert_eq!(
        paragraphs(&world, "exam"),
        ["The range is to be found.", "Next."]
    );
    assert_eq!(
        paragraphs(&world, "markscheme"),
        ["The range is 7.3 m.", "Accept 7.2 to 7.4 m.", "Next."]
    );
    assert!(diagnostics(&world, "exam").is_empty());
}

/// They are elements, so a stylesheet styles them, and a stylesheet may
/// hide them in the variant they are for too.
#[test]
fn variant_conditions_are_stylable() {
    let world = world(
        "in-variant { font-weight: 700; }\n",
        "in-variant { display: none; }\n",
        "Text #in-variant(\"exam\")[for the exam].\n",
    );
    let styles = folio::styles(&world, Target::Pdf, Some(&name("exam")))
        .unwrap()
        .unwrap();
    let node = styles
        .nodes()
        .into_iter()
        .find(|n| format!("{:?}", n.name).contains("in-variant"))
        .expect("the element is styled");
    let weight = styles.value(node, "font-weight").expect("a weight");
    assert_eq!(format!("{weight:?}"), "Int(700)");
    assert_eq!(paragraphs(&world, "exam"), ["Text for the exam."]);
    // The mark scheme's overlay hides nothing that is not for it.
    assert_eq!(paragraphs(&world, "markscheme"), ["Text."]);
}

/// A condition that names no declared variant is a warning.
#[test]
fn a_condition_naming_no_variant_warns() {
    let world = world(
        "",
        "",
        "#in-variant(\"slides\")[Only on slides.]\n\nText.\n",
    );
    let found = diagnostics(&world, "exam");
    assert!(
        found.iter().any(|(s, _, m)| *s == Severity::Warning
            && m.contains("no variant `slides` is declared, so this `in-variant` is never shown")),
        "{found:?}"
    );
    assert_eq!(paragraphs(&world, "exam"), ["Text."]);
}

/// `placeholder("…")` keeps the element, numbered, and replaces all of
/// its content with the text (SPEC §6.3).
#[test]
fn a_placeholder_replaces_the_content() {
    let world = world(
        "figure { numbering: \"1\"; }\n\
         figure.solution { display: placeholder(\"Solution in the mark scheme\"); }\n\
         @variant markscheme { figure.solution { display: block; } }\n",
        "",
        "#figure(class: \"solution\", caption: [CAPTION])[SECRET working.\n\nSECOND paragraph.]\n\n\
         #figure(caption: [Shown])[Plain.]\n",
    );
    let exam = paragraphs(&world, "exam");
    assert!(
        exam.iter().any(|p| p == "Solution in the mark scheme"),
        "{exam:?}"
    );
    assert!(
        exam.iter()
            .all(|p| !p.contains("SECRET") && !p.contains("SECOND") && !p.contains("CAPTION")),
        "{exam:?}"
    );
    // The figure is still counted: the next one is figure 2.
    assert!(exam.iter().any(|p| p.contains("Figure~2")), "{exam:?}");
    let markscheme = paragraphs(&world, "markscheme");
    assert!(markscheme.iter().any(|p| p.contains("SECRET working.")));
}

/// A part takes a placeholder or `none` too: `figure > caption`.
#[test]
fn a_part_is_selected_like_an_element() {
    let world = world(
        "figure > caption { display: none; }\n\
         @variant markscheme { figure > caption { display: placeholder(\"(caption)\"); } }\n",
        "",
        "#figure(caption: [CAPTION text])[Body.]\n",
    );
    assert_eq!(paragraphs(&world, "exam"), ["Body."]);
    assert_eq!(paragraphs(&world, "markscheme"), ["Body.", "(caption)"]);
}

/// `show-only` keeps the named children of an element, whole, and the
/// children that hold one, condensed (SPEC §6.2: "stem condensed, answers
/// prominent").
#[test]
fn show_only_condenses_an_element() {
    let world = world(
        "",
        "@variant markscheme { enum-item { show-only: figure strong; } }\n",
        "+ STEM of the first question. #strong[[3]]\n\n  #figure[Answer one.]\n\
         + STEM of the second question.\n\n  #figure[Answer two.]\n\n  More STEM.\n",
    );
    let exam = paragraphs(&world, "exam");
    assert!(exam.iter().any(|p| p.contains("STEM of the first")));
    let markscheme = paragraphs(&world, "markscheme");
    assert!(
        markscheme.iter().all(|p| !p.contains("STEM")),
        "{markscheme:?}"
    );
    assert!(
        markscheme.iter().any(|p| p.contains("[3]")),
        "{markscheme:?}"
    );
    assert!(markscheme.iter().any(|p| p.contains("Answer one.")));
    assert!(markscheme.iter().any(|p| p.contains("Answer two.")));
}

/// `hidden` and `answer-space` keep the block of an element and leave its
/// content out of the document the variant shows; layout sizes the block
/// (`answer_space.rs`). Neither warns of a block.
#[test]
fn spaces_leave_the_content_out() {
    let world = world(
        "figure.a { display: answer-space; }\nfigure.h { display: hidden; }\n",
        "",
        "#figure(class: \"a\")[ANSWER one.]\n\n#figure(class: \"a\")[ANSWER two.]\n\n\
         #figure(class: \"h\")[HIDDEN.]\n\nText.\n",
    );
    let text = folio::text(&world, Target::Pdf, Some(&name("exam")))
        .unwrap()
        .unwrap();
    // The paragraphs the document's blocks hold: the spaces hold none.
    fn shown(block: &folio::TextBlock, text: &folio::Text, out: &mut Vec<String>) {
        for child in &block.children {
            match child {
                folio::TextFlow::Paragraph(i) => {
                    out.push(text.paragraphs[*i].shaped.text().to_string())
                }
                folio::TextFlow::Block(b) => shown(b, text, out),
            }
        }
    }
    let mut paragraphs = Vec::new();
    shown(&text.root, &text, &mut paragraphs);
    assert_eq!(paragraphs, ["Text."]);
    assert_eq!(text.spaces.len(), 3);
    let found = diagnostics(&world, "exam");
    assert!(found.is_empty(), "{found:?}");
}

/// A placeholder takes one string.
#[test]
fn a_placeholder_takes_one_string() {
    let world = world(
        "figure { display: placeholder(3); }\nemph { display: placeholder(); }\n",
        "",
        "#figure[Kept.]\n",
    );
    let found = diagnostics(&world, "exam");
    assert!(
        found.iter().any(|(s, _, m)| *s == Severity::Error
            && m == "wrong value for `display`: `placeholder` takes a string"),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|(_, _, m)| m.contains("`placeholder` needs the text to show")),
        "{found:?}"
    );
    assert_eq!(paragraphs(&world, "exam"), ["Kept."]);
}

/// A reference to an element the variant does not show is a warning, and
/// an error in strict mode (SPEC §4.5).
#[test]
fn a_reference_to_hidden_content() {
    let style = "figure { numbering: \"1\"; }\nfigure.solution { display: none; }\n\
                 @variant markscheme { figure.solution { display: block; } }\n";
    let text = "See @fig:sol.\n\n#figure(class: \"solution\", caption: [Working])[W.] <fig:sol>\n";
    let world = world(style, "", text);
    let found = diagnostics(&world, "exam");
    let hidden: Vec<_> = found
        .iter()
        .filter(|(_, code, _)| code == "reference-hidden")
        .collect();
    assert_eq!(hidden.len(), 1, "{found:?}");
    assert_eq!(hidden[0].0, Severity::Warning);
    assert_eq!(
        hidden[0].2,
        "`<fig:sol>` names a `figure` that variant `exam` does not show, so the reference has \
         nothing to refer to"
    );
    assert!(
        found
            .iter()
            .all(|(_, _, m)| !m.contains("no element has the label")),
        "{found:?}"
    );
    assert!(diagnostics(&world, "markscheme").is_empty());

    let strict = world_with("\n[formatting]\nmode = \"strict\"\n", style, "", text);
    let found = diagnostics(&strict, "exam");
    assert!(
        found
            .iter()
            .any(|(s, code, _)| *s == Severity::Error && code == "reference-hidden"),
        "{found:?}"
    );
}

/// `#if variant == ..` choosing inline markup could say the same with
/// `#in-variant`; in a loop, it generates content, which is what scripts
/// are for, and blocks or code are left alone.
#[test]
fn the_lint_suggests_mechanisms_one_and_two() {
    let world = world(
        "",
        "",
        "#if variant == \"markscheme\" [Answer.]\n\n\
         #if (variant != \"exam\") [Other.]\n\n\
         #if \"exam\" == variant [Exam.] else [Not.]\n\n\
         #if variant in (\"exam\", \"markscheme\") [Both.]\n\n\
         #for i in (1, 2) { if variant == \"markscheme\" [Generated #i.] }\n\n\
         #let note(body) = if variant == \"markscheme\" { body }\n\n\
         #if variant == \"exam\" and true [Mixed.]\n\n\
         #if variant == \"markscheme\" [= Mark scheme\n\nBlocks.]\n\n\
         #if variant == \"markscheme\" { [Code.] }\n",
    );
    let found: Vec<_> = diagnostics(&world, "exam")
        .into_iter()
        .filter(|(_, code, _)| code == "variant-condition")
        .map(|(s, _, m)| (s, m))
        .collect();
    assert_eq!(
        found,
        [
            (
                Severity::Warning,
                "this `if` only tests whether the variant is \"markscheme\", which the content \
                 can say without a script"
                    .to_string()
            ),
            (
                Severity::Warning,
                "this `if` only tests whether the variant is \"exam\", which the content can \
                 say without a script"
                    .to_string()
            ),
            (
                Severity::Warning,
                "this `if` only tests whether the variant is \"exam\", which the content can \
                 say without a script"
                    .to_string()
            ),
            (
                Severity::Warning,
                "this `if` only tests whether the variant is \"exam\" or \"markscheme\", which \
                 the content can say without a script"
                    .to_string()
            ),
        ]
    );
    let hints: Vec<String> = compile(&world, "exam")
        .diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some("variant-condition"))
        .flat_map(|d| d.hints.iter().map(ToString::to_string))
        .collect();
    assert!(
        hints[0].contains("`#in-variant(\"markscheme\")[..]`"),
        "{hints:?}"
    );
    assert!(
        hints[1].contains("`#except-variant(\"exam\")[..]`"),
        "{hints:?}"
    );
    assert!(
        hints[3].contains("`@variant exam, markscheme`"),
        "{hints:?}"
    );
}

/// A placeholder on a formula replaces the formula: neither it nor its
/// source is set (SPEC §6.4).
#[test]
fn a_placeholder_replaces_a_formula() {
    let world = world(
        "math { display: placeholder(\"(answer)\"); }\n\
         @variant markscheme { math { display: auto; } }\n",
        "",
        "The speed is $v = 7.2$.\n",
    );
    let text = folio::text(&world, Target::Pdf, Some(&name("exam")))
        .unwrap()
        .unwrap();
    assert_eq!(paragraphs(&world, "exam"), ["The speed is (answer)."]);
    assert!(text.maths.is_empty(), "no formula is laid out");
    assert!(!selected(&world, "exam").contains("7.2"));
}

/// A variant that hides the whole document shows an empty one, not all
/// of it.
#[test]
fn hiding_the_document_leaves_it_empty() {
    let world = world(
        "@variant exam { doc { display: none; } }\n",
        "",
        "= Forces\n\nSECRET.\n",
    );
    assert!(paragraphs(&world, "exam").is_empty());
    assert!(!selected(&world, "exam").contains("SECRET"));
    assert_eq!(paragraphs(&world, "markscheme"), ["Forces", "SECRET."]);
}

/// `in` a string tests for a substring, which the lint does not read as
/// a list of variants.
#[test]
fn the_lint_reads_only_names_and_lists() {
    let world = world(
        "",
        "",
        "#if variant in \"markscheme\" [Sub.]\n\n#if variant == (\"exam\",) [Odd.]\n",
    );
    assert!(
        diagnostics(&world, "exam")
            .iter()
            .all(|(_, code, _)| code != "variant-condition")
    );
}
