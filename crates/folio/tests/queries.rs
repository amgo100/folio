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
                        e.title.replace("\u{a0}", "~")
                    )
                })
                .collect(),
        })
        .collect()
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
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
    assert_eq!(paragraphs(&world, "exam")[0], "Total: 8 marks.");
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

/// D-65-1: each entry is set as its own paragraph, its marker, title and
/// page: `1. Forces, 1`.
#[test]
fn entries_show_their_page() {
    let world = world(
        false,
        "#outline(heading)\n\n= Forces\n\n#figure(caption: [Ball.])[A.]\n\n\
         #heading(level: 2)[Friction]\n\n= Energy\n",
    );
    let text = paragraphs(&world, "exam");
    assert_eq!(
        &text[..3],
        ["1.~Forces, 1", "1.1.~Friction, 1", "2.~Energy, 1"],
        "{text:?}"
    );
    assert!(
        diagnostics(&world, "exam").is_empty(),
        "{:?}",
        diagnostics(&world, "exam")
    );
}

/// Entries find the page their target starts on across a page break.
#[test]
fn entries_follow_their_targets_onto_later_pages() {
    let mut text = String::from("#outline(heading)\n\n= First\n\n");
    for _ in 0..60 {
        text.push_str("A paragraph long enough to fill a good part of a line of the page.\n\n");
    }
    text.push_str("= Second\n");
    let world = world(false, &text);
    let set = paragraphs(&world, "exam");
    assert_eq!(set[0], "1.~First, 1");
    let second = &set[1];
    assert!(
        second.starts_with("2.~Second, ") && !second.ends_with(", 1") && !second.ends_with("??"),
        "{second}"
    );
}

/// Each entry links to what it lists: by its label, or by a name made for
/// it; the PDF still claims PDF/UA-1.
#[test]
fn entries_are_links() {
    let world = world(
        false,
        "#doc(title: [Forces])\n\n#outline(heading)\n\n= Forces <sec:forces>\n\nText.\n\n= Energy\n\nMore.\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, Some(&name("exam"))).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let pdf = compiled.output.clone().expect("a PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"), "PDF/UA-1");
    assert_eq!(text.matches("/Subtype/Link").count(), 2);
    assert_eq!(text.matches("/S/Link").count(), 2);
    assert_eq!(text.matches("/Dest(sec:forces)").count(), 1);
    assert_eq!(text.matches("/Dest(entry 1)").count(), 1);
}

/// A total of decimals shows the decimal the marks add up to, not the
/// binary sum's noise.
#[test]
fn a_total_of_decimals_is_rounded() {
    let world = world(
        false,
        "#element part(marks: number = none) { kind: block }\n\
         #total(part, \"marks\")\n\n#part(marks: 0.1)[A.]\n\n#part(marks: 0.2)[B.]\n\n#part(marks: 1)[C.]\n",
    );
    assert_eq!(results(&world, "exam"), [["1.3"]]);
}

/// A title reads as the document shows it: a reference's text, no
/// footnote; an instance with neither a marker nor a title is not listed.
#[test]
fn titles_read_as_shown_and_empty_entries_are_left_out() {
    let world = world(
        false,
        "#list-of(figure)\n\n\
         #figure(caption: [Path#footnote[Air ignored.] of @fig:b])[A.] <fig:a>\n\n\
         #figure(caption: [Second])[B.] <fig:b>\n\n\
         #figure[]\n",
    );
    // The figure without a caption has no marker either.
    assert_eq!(
        results(&world, "exam"),
        [vec!["1 Figure~1: |Path of Figure~2", "1 Figure~2: |Second"]]
    );
}
