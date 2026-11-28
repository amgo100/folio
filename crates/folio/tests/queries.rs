//! Queries (SPEC §4.5, §6.4): `#total` over a numbering group's common
//! tree, and `#outline` and `#list-of` over the variant's own tree, which
//! never list what the variant hides.

use folio::{Name, QueryResult, Resolution, Severity, Target, World};

const STYLE: &str = "heading { numbering: \"1.\"; }\n\
                     figure { numbering: \"1\"; }\n\
                     .scheme-only { display: none; }\n\
                     @variant markscheme { .scheme-only { display: auto; } }\n";

fn manifest(group: bool) -> String {
    let group = if group {
        "\n[numbering-groups.paper]\nvariants = [\"exam\", \"markscheme\"]\nshared-counters = []\n"
    } else {
        ""
    };
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n\n\
         [variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\n{group}"
    )
}

fn world(group: bool, text: &str) -> World {
    let mut world = World::new(&manifest(group)).unwrap();
    world.set_source("styles/main.fsty", STYLE).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn name(variant: &str) -> Name {
    Name::new(variant).unwrap()
}

fn resolution(world: &World, variant: &str) -> Resolution {
    folio::resolve(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
}

/// The results of a variant's queries: a total as its text, a list as
/// `level marker|title` per entry, with no-break spaces written `~`.
fn results(world: &World, variant: &str) -> Vec<Vec<String>> {
    resolution(world, variant)
        .queries
        .iter()
        .map(|q| match &q.result {
            QueryResult::Total(text) => vec![text.to_string()],
            QueryResult::List(entries) => entries
                .iter()
                .map(|e| {
                    format!(
                        "{} {}|{}",
                        e.level,
                        e.marker.as_deref().unwrap_or("").replace('\u{a0}', "~"),
                        e.title
                    )
                })
                .collect(),
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

const QUESTIONS: &str = "\
#element question(marks: int = 1) { kind: block }

Total: #total(question, \"marks\") marks.

#question(marks: 3)[A ball is thrown.]

#question(marks: 4, class: \"scheme-only\")[A bonus question.]

#question[A short one.]
";

/// SPEC §6.4: in a numbering group, the total is the same in every
/// variant, though one hides a question.
#[test]
fn a_total_is_the_same_in_every_variant_of_a_group() {
    let world = world(true, QUESTIONS);
    assert_eq!(results(&world, "exam"), [["8"]]);
    assert_eq!(results(&world, "markscheme"), [["8"]]);
    // Until the text stage sets queries, it warns that it leaves them out.
    let found: Vec<_> = diagnostics(&world, "exam")
        .into_iter()
        .filter(|(_, m)| !m.contains("is not laid out yet"))
        .collect();
    assert!(found.is_empty(), "{found:?}");
}

/// Outside a group, a variant totals what it shows.
#[test]
fn outside_a_group_a_total_counts_what_the_variant_shows() {
    let world = world(false, QUESTIONS);
    assert_eq!(results(&world, "exam"), [["4"]]);
    assert_eq!(results(&world, "markscheme"), [["8"]]);
}

/// Floats sum to a float, shown with the language's decimal marker; a
/// field that is no number counts as 0, with a warning.
#[test]
fn totals_of_numbers_and_of_values_that_are_not() {
    let world = world_pl(
        "#element part(marks: number = none, note: str = none) { kind: block }\n\
         #total(part, \"marks\") i #total(part, \"note\")\n\n\
         #part(marks: 1.5)[A.]\n\n#part(marks: 2)[B.]\n\n#part(note: \"x\")[C.]\n",
    );
    assert_eq!(results(&world, "exam"), [["3,5"], ["0"]]);
    let found = diagnostics(&world, "exam");
    assert!(
        found.iter().any(|(s, m)| *s == Severity::Warning
            && m == "`note` of a `project.part` is not a number, so this total counts it as 0"),
        "{found:?}"
    );
}

fn world_pl(text: &str) -> World {
    let manifest = manifest(false).replace("lang = \"en\"", "lang = \"pl\"");
    let mut world = World::new(&manifest).unwrap();
    world.set_source("styles/main.fsty", STYLE).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

const DOCUMENT: &str = "\
#outline(heading)

#list-of(figure)

= Forces

#figure(caption: [Free-body diagram.])[A.]

== Friction

#figure(class: \"scheme-only\", caption: [Worked answer.])[B.]

#heading(class: \"scheme-only\")[Answers]

= Energy
";

/// SPEC §6.4: an outline and a figure list never mention what the
/// variant hides; each entry has its marker as the document shows it.
#[test]
fn outlines_and_figure_lists_list_what_the_variant_shows() {
    let world = world(false, DOCUMENT);
    assert_eq!(
        results(&world, "exam"),
        [
            vec!["1 1.~|Forces", "2 1.1.~|Friction", "1 2.~|Energy"],
            vec!["1 Figure~1: |Free-body diagram."],
        ]
    );
    assert_eq!(
        results(&world, "markscheme"),
        [
            vec![
                "1 1.~|Forces",
                "2 1.1.~|Friction",
                "1 2.~|Answers",
                "1 3.~|Energy"
            ],
            vec![
                "1 Figure~1: |Free-body diagram.",
                "1 Figure~2: |Worked answer."
            ],
        ]
    );
}

/// An element is named bare, as the spec writes it; a string is an
/// error that says so.
#[test]
fn a_query_names_its_element_bare() {
    let world = world(false, "#outline(\"heading\")\n\n= A\n");
    let found = diagnostics(&world, "exam");
    assert!(
        found.iter().any(
            |(s, m)| *s == Severity::Error && m.contains("expected an element, found a string")
        ),
        "{found:?}"
    );
}
