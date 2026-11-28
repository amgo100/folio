//! Numbering groups (SPEC §6.1, §6.4): the variants of a group count their
//! shared counters on the group's common tree, the union of what each
//! shows, so that each instance has one number in all of them.

use folio::{Name, Severity, Target, World};

const STYLE: &str = "heading { numbering: \"1.\"; }\n\
                     figure { numbering: \"1\"; }\n\
                     math { numbering: \"(1)\"; }\n\
                     .exam-only { display: none; }\n\
                     .scheme-only { display: none; }\n\
                     @variant exam { .exam-only { display: auto; } }\n\
                     @variant markscheme { .scheme-only { display: auto; } }\n";

fn manifest(shared: &str, extra: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n\n\
         [variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\n\n\
         [variants.slides]\ntargets = [\"pdf\"]\n\n\
         [numbering-groups.paper]\nvariants = [\"exam\", \"markscheme\"]\n\
         shared-counters = [{shared}]\n{extra}"
    )
}

fn world(shared: &str, text: &str) -> World {
    let mut world = World::new(&manifest(shared, "")).unwrap();
    world.set_source("styles/main.fsty", STYLE).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn name(variant: &str) -> Name {
    Name::new(variant).unwrap()
}

/// The text of every paragraph a variant sets, with no-break spaces
/// written `~`.
fn paragraphs(world: &World, variant: &str) -> Vec<String> {
    folio::text(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', "~"))
        .collect()
}

/// Each number a variant's Resolve gives, as `counter: number`.
fn numbers(world: &World, variant: &str) -> Vec<String> {
    folio::resolve(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
        .numbers
        .iter()
        .map(|n| {
            let values: Vec<String> = n.values.iter().map(ToString::to_string).collect();
            format!("{}: {}", n.counter, values.join("."))
        })
        .collect()
}

fn diagnostics(world: &World, variant: &str) -> Vec<(Severity, String)> {
    folio::compile(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .diagnostics
        .iter()
        .map(|d| (d.severity, d.message.to_string()))
        .collect()
}

/// A paper in which the exam and the mark scheme each show headings,
/// figures and equations the other hides.
const PAPER: &str = "= Mechanics\n\n\
    #figure(caption: [Both])[A.]\n\n\
    #figure(class: \"scheme-only\", caption: [Scheme])[B.]\n\n\
    #figure(class: \"exam-only\", caption: [Exam])[C.]\n\n\
    #heading(class: \"scheme-only\")[Worked answers]\n\n\
    $ a = b $\n\n\
    #figure(class: \"scheme-only\")[$ c = d $]\n\n\
    = Waves\n\n\
    #figure(caption: [Last])[D.] <fig:last>\n\n\
    $ e = f $ <eq:last>\n\n\
    See @fig:last and @eq:last.\n";

/// SPEC §6.1: the exam and the mark scheme share heading, figure and
/// equation numbers although each hides what the other shows.
#[test]
fn a_group_shares_its_counters() {
    let world = world("\"heading\", \"figure\", \"equation\"", PAPER);
    assert_eq!(
        numbers(&world, "exam"),
        [
            "heading: 1",
            "figure: 1",
            "figure: 3",
            "equation: 1",
            "heading: 3",
            "figure: 5",
            "equation: 3",
        ]
    );
    assert_eq!(
        numbers(&world, "markscheme"),
        [
            "heading: 1",
            "figure: 1",
            "figure: 2",
            "heading: 2",
            "equation: 1",
            "figure: 4",
            "equation: 2",
            "heading: 3",
            "figure: 5",
            "equation: 3",
        ]
    );
    // References show the shared numbers too.
    for variant in ["exam", "markscheme"] {
        let text = paragraphs(&world, variant);
        assert!(
            text.iter().any(|p| p == "See Figure~5 and Equation~3."),
            "{variant}: {text:?}"
        );
        assert!(text.iter().any(|p| p == "3.~Waves"), "{variant}: {text:?}");
    }
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
}

/// Counters a group does not share, and variants outside any group, count
/// only what they show.
#[test]
fn other_counters_and_other_variants_count_their_own() {
    let world = world("\"figure\"", PAPER);
    let exam = numbers(&world, "exam");
    assert!(exam.contains(&"heading: 2".to_string()), "{exam:?}");
    assert!(exam.contains(&"figure: 5".to_string()), "{exam:?}");
    assert!(exam.contains(&"equation: 2".to_string()), "{exam:?}");
    // Slides is in no group: it shows neither variant's own content.
    assert_eq!(
        numbers(&world, "slides"),
        [
            "heading: 1",
            "figure: 1",
            "equation: 1",
            "heading: 2",
            "figure: 2",
            "equation: 2",
        ]
    );
}

/// Instances made by a loop pair by iteration, so an item one variant
/// leaves out inside the loop keeps the others' numbers.
#[test]
fn loop_iterations_pair_across_the_group() {
    let world = world(
        "\"figure\"",
        "#let bank = ((t: \"one\", hidden: false), (t: \"two\", hidden: true), (t: \"three\", hidden: false))\n\
         #for q in bank { if variant == \"markscheme\" or not q.hidden { figure(caption: [#q.t])[Q.] } }\n",
    );
    assert_eq!(numbers(&world, "exam"), ["figure: 1", "figure: 3"]);
    assert_eq!(
        numbers(&world, "markscheme"),
        ["figure: 1", "figure: 2", "figure: 3"]
    );
}

/// A shared counter that names no counter is a warning at its name.
#[test]
fn an_unknown_shared_counter_warns() {
    let world = world("\"figure\", \"question\"", "#figure[A.]\n");
    let found = diagnostics(&world, "exam");
    assert!(
        found.iter().any(|(s, m)| *s == Severity::Warning
            && m == "there is no counter `question`, so numbering group `paper` cannot share it"),
        "{found:?}"
    );
    assert!(
        found.iter().all(|(_, m)| !m.contains("not implemented")),
        "{found:?}"
    );
}

/// A target overlay that changes what a group counts is a warning: the
/// common tree is counted with it, so numbers could differ by target.
#[test]
fn a_target_overlay_that_changes_counting_warns() {
    let mut world = World::new(&manifest("\"figure\"", "")).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            "figure { numbering: \"1\"; }\n@target pdf { figure.print { display: none; } }\n\
             @target pdf { figure { font-size: 9pt; } }\n",
        )
        .unwrap();
    world
        .set_source("main.fol", "#figure(class: \"print\")[A.]\n\n#figure[B.]\n")
        .unwrap();
    let found = diagnostics(&world, "exam");
    let warned: Vec<_> = found
        .iter()
        .filter(|(_, m)| m.contains("changes what numbering group `paper` counts"))
        .collect();
    assert_eq!(warned.len(), 1, "{found:?}");
    // Slides is in no group, so nothing is shared and nothing is said.
    assert!(
        diagnostics(&world, "slides")
            .iter()
            .all(|(_, m)| !m.contains("numbering group"))
    );
}

/// What only another variant of the group reports belongs to that
/// variant's build, though the common tree selects it.
#[test]
fn a_build_reports_only_its_own_problems() {
    let world = world(
        "\"figure\"",
        "#figure[A.]\n\n#if variant == \"markscheme\" { undefined_function() }\n",
    );
    let exam = diagnostics(&world, "exam");
    assert!(
        exam.iter().all(|(_, m)| !m.contains("undefined_function")),
        "{exam:?}"
    );
    let markscheme = diagnostics(&world, "markscheme");
    assert!(
        markscheme
            .iter()
            .any(|(s, m)| *s == Severity::Error && m.contains("undefined_function")),
        "{markscheme:?}"
    );
}

/// An equation one variant replaces by a placeholder is counted as the
/// variant that shows it numbers it: by its labelled lines.
#[test]
fn a_placeholder_does_not_change_what_is_counted() {
    let mut world = World::new(&manifest("\"equation\"", "")).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            "math { numbering: \"(1)\"; }\n\
             @variant exam { figure math { display: placeholder(\"(working)\"); } }\n",
        )
        .unwrap();
    world
        .set_source(
            "main.fol",
            "#figure[$ a = b <eq:x> \\ c = d <eq:y> $]\n\n$ e = f $\n",
        )
        .unwrap();
    assert_eq!(
        numbers(&world, "markscheme"),
        ["figure: 1", "equation: 1", "equation: 2", "equation: 3"]
    );
    // The placeholder is not numbered as a formula; the next one is 3.
    let exam = numbers(&world, "exam");
    assert_eq!(
        exam.last().map(String::as_str),
        Some("equation: 3"),
        "{exam:?}"
    );
}

/// Variants that restart a shared counter differently at one instance
/// are warned about.
#[test]
fn variants_restarting_a_shared_counter_differently_warn() {
    let mut world = World::new(&manifest("\"figure\"", "")).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            "figure { numbering: \"1\"; }\n\
             @variant markscheme { heading { counter-reset: figure; } }\n",
        )
        .unwrap();
    world
        .set_source("main.fol", "= One\n\n#figure[A.]\n\n= Two\n\n#figure[B.]\n")
        .unwrap();
    let found = diagnostics(&world, "exam");
    assert!(
        found
            .iter()
            .any(|(_, m)| m.contains("restart counters differently here")),
        "{found:?}"
    );
}
