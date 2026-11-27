//! Counters of components' elements (SPEC §4.5, §13.1): a counter scoped to
//! elements opens a level inside each of their instances and gets its value
//! back on leaving it, so exam parts nest as 1, (a), (i), (ii), (b); and
//! `counter-reset` names a component's counter unqualified.

use ecow::EcoVec;
use folio::{Name, Registry, Target, World};
use folio_component::{
    Body, Component, ComponentManifest, ComponentName, ContentKind, CounterDecl, Declarations,
    ElementKind, ElementSchema, LocaleString, QualifiedName, Within,
};

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// A stand-in for the exam component: questions count through the paper,
/// and parts are scoped to questions and to parts, so they nest.
struct Exam;

impl Component for Exam {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new("exam").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let mut question = ElementSchema::new(name("question"), ElementKind::Block);
        question.body = Body::Content(ContentKind::Blocks);
        question.counter = Some(CounterDecl::default());
        question.ref_key = Some(name("question"));

        let mut part = ElementSchema::new(name("part"), ElementKind::Block);
        part.body = Body::Content(ContentKind::Blocks);
        let mut counter = CounterDecl::new();
        // The component's own elements, unqualified.
        counter.scope = EcoVec::from([
            QualifiedName::new("question").unwrap(),
            QualifiedName::new("part").unwrap(),
        ]);
        part.counter = Some(counter);
        part.ref_key = Some(name("part"));

        // Restarted at each top-level heading, and scoped to questions.
        let mut note = ElementSchema::new(name("note"), ElementKind::Block);
        note.body = Body::Content(ContentKind::Blocks);
        let mut counter = CounterDecl::new();
        counter.within = Some(Within::new(
            QualifiedName::new("heading").unwrap(),
            [(name("level"), folio_component::Value::Int(1))],
        ));
        counter.scope = EcoVec::from([QualifiedName::new("question").unwrap()]);
        note.counter = Some(counter);

        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([question, part, note]);
        declarations.locale_strings = EcoVec::from([
            LocaleString::new("en", name("question"), "Question"),
            LocaleString::new("en", name("part"), "part"),
        ]);
        declarations
    }
}

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nlang = \"en\"\nstyles = [\"styles/main.fsty\"]\n";

/// Questions, parts and sub-parts numbered as an exam paper numbers them.
const STYLE: &str = "question { numbering: \"1\"; }\n\
                     part { numbering: \"(a)\"; }\n\
                     part part { numbering: \"(i)\"; }\n";

fn world(style: &str, text: &str) -> World {
    let mut registry = Registry::new();
    registry.register(Exam).unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

/// Each numbered instance's counter and number, in document order.
fn numbers(world: &World) -> Vec<(String, String)> {
    folio::resolve(world, Target::Pdf, None)
        .unwrap()
        .unwrap()
        .numbers
        .iter()
        .filter(|n| n.counter.starts_with("exam."))
        .map(|n| {
            let number = n.number.as_deref().unwrap_or("-");
            (n.counter.to_string(), number.to_string())
        })
        .collect()
}

/// The text of each reference, with no-break spaces written `~`.
fn references(world: &World) -> Vec<String> {
    folio::resolve(world, Target::Pdf, None)
        .unwrap()
        .unwrap()
        .references
        .iter()
        .map(|r| r.text.as_str().replace('\u{a0}', "~"))
        .collect()
}

/// Every diagnostic's message, except those for what Folio does not lay
/// out yet.
fn messages(world: &World) -> Vec<String> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .filter(|m| !m.contains("not laid out") && !m.contains("not implemented"))
        .collect()
}

const PAPER: &str = "\
#question[
  A ball is thrown.

  #part[
    Find its speed.

    #part[At the top.] <top>
    #part[At the ground.] <ground>
  ]
  #part[Find its range.] <range>
]

#question[
  #part[Explain.] <explain>
]

See @ground[], @range and @explain[].
";

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(c, n)| (c.to_string(), n.to_string()))
        .collect()
}

#[test]
fn parts_nest_and_get_their_value_back_on_leaving_a_part() {
    let world = world(STYLE, PAPER);
    assert!(messages(&world).is_empty(), "{:?}", messages(&world));
    assert_eq!(
        numbers(&world),
        pairs(&[
            ("exam.question", "1"),
            ("exam.part", "(a)"),
            ("exam.part", "(i)"),
            ("exam.part", "(ii)"),
            // The sub-parts' level closed with part (a): the next part is
            // (b), not (iii).
            ("exam.part", "(b)"),
            ("exam.question", "2"),
            // A new question opens a new level.
            ("exam.part", "(a)"),
        ])
    );
    // A reference shows the part's own level.
    assert_eq!(references(&world), ["ii", "part~b", "a"]);
    assert!(messages(&world).is_empty(), "{:?}", messages(&world));
}

#[test]
fn counter_reset_names_a_components_counter_unqualified() {
    // `question` resolves to `exam.question`, the only element of that
    // name, as element names do (SPEC §3.6).
    let unqualified = world(
        &format!("{STYLE}heading {{ counter-reset: question; }}\n"),
        "#question[One.]\n\n#question[Two.]\n\n= Section B\n\n#question[Three.]\n",
    );
    assert_eq!(
        numbers(&unqualified),
        pairs(&[
            ("exam.question", "1"),
            ("exam.question", "2"),
            ("exam.question", "1"),
        ])
    );
    let messages = messages(&unqualified);
    assert!(messages.is_empty(), "{messages:?}");

    // Qualified, the same.
    let qualified = world(
        &format!("{STYLE}heading {{ counter-reset: \"exam.question\"; }}\n"),
        "#question[One.]\n\n= Section B\n\n#question[Two.]\n",
    );
    assert_eq!(
        numbers(&qualified),
        pairs(&[("exam.question", "1"), ("exam.question", "1")])
    );
}

#[test]
fn counter_reset_of_no_counter_warns() {
    let world = world(
        &format!("{STYLE}heading {{ counter-reset: answer; }}\n"),
        "= Section A\n\n#question[One.]\n",
    );
    assert_eq!(
        messages(&world),
        ["there is no counter `answer`, so `counter-reset` does not restart it"]
    );
}

#[test]
fn a_restart_within_an_element_inside_a_scope_outlasts_the_scope() {
    // Restarts go in document order: the heading inside the question
    // restarts `note`, and the note after the question counts under it.
    let world = world(
        &format!("{STYLE}heading {{ numbering: \"1\"; }}\nnote {{ numbering: \"1.1\"; }}\n"),
        "= A\n\n#note[One.]\n\n#question[\n  #note[Two.]\n\n  = B\n\n  #note[Three.]\n]\n\n#note[Four.]\n",
    );
    let notes: Vec<String> = numbers(&world)
        .into_iter()
        .filter(|(counter, _)| counter == "exam.note")
        .map(|(_, number)| number)
        .collect();
    assert_eq!(notes, ["1.1", "1.1", "2.1", "2.1"]);
}
