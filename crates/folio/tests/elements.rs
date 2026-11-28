//! Elements a project declares in its own content files (SPEC §4.3,
//! decision D-58-1): declared with `#element` at the top level of a file,
//! imported where they are used, numbered and referenced through their
//! counter and reference key, and styled by name and by slot like a
//! component's elements.

use folio::style::{NodeName, Styled, StyledNode};
use folio::{Registry, Target, World};
use folio_component::{Abs, Value};

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nlang = \"en\"\nstyles = [\"styles/main.fsty\"]\n";

/// The `example` element of SPEC §4.3, with words for its reference key.
const BLOCKS: &str = "\
#element example(
  title: content = none,
  numbered: bool = true,
  difficulty: choice(\"core\", \"extension\") = \"core\",
) {
  kind: block                         // block | inline | float | note | slide
  body: slots(statement: blocks, solution: blocks)
  counter: within heading(level: 1)   // or scoped(question, part)
  ref-key: \"example\"                  // word looked up in locale files
  words: (en: \"Example\", pl: \"przykł.\")
}
";

const MAIN: &str = "\
#import \"lib/blocks.fol\": example

= Kinematics

#example[
  #statement[A ball is thrown.]
  #solution[Use the equations of motion.]
] <ex:ball>

= Slopes

#example(difficulty: \"extension\")[
  #statement[A particle slides.]
  #solution[Resolve along the slope.]
] <ex:slide>

#example(title: [Projectile on a slope], difficulty: \"extension\")[
  #statement[A particle is projected.]
  #solution[Resolve along the slope.]
] <ex:slope>

See @ex:slope and @ex:ball.
";

const STYLE: &str = "\
heading { numbering: \"1\"; }
example { numbering: \"1.1\"; }
example > solution { indent: 1pt; }
";

/// A world without the first-party components: these projects declare
/// elements of their own, such as `question`, which the exam component
/// would also provide.
fn world_with(files: &[(&str, &str)]) -> World {
    let mut world = World::builder()
        .components(Registry::new())
        .build(MANIFEST, None)
        .unwrap();
    for (path, text) in files {
        world.set_source(path, text).unwrap();
    }
    world
}

fn project(main: &str) -> World {
    world_with(&[
        ("main.fol", main),
        ("lib/blocks.fol", BLOCKS),
        ("styles/main.fsty", STYLE),
    ])
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

/// Every diagnostic's message and hints, joined.
fn diagnostics(world: &World) -> Vec<String> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .iter()
        .filter(|d| !d.message.contains("not laid out") && !d.message.contains("not implemented"))
        .map(|d| {
            let mut text = d.message.to_string();
            for hint in &d.hints {
                text.push_str(" | ");
                text.push_str(hint);
            }
            text
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

/// Each example's number, in document order.
fn numbers(world: &World) -> Vec<String> {
    folio::resolve(world, Target::Pdf, None)
        .unwrap()
        .unwrap()
        .numbers
        .iter()
        .filter(|n| n.counter.ends_with("example"))
        .map(|n| n.number.as_deref().unwrap_or("-").to_string())
        .collect()
}

fn styled(world: &World) -> Styled {
    folio::styles(world, Target::Pdf, None).unwrap().unwrap()
}

/// The nodes with a name, in document order.
fn all<'a>(styled: &'a Styled, element: &str) -> Vec<&'a StyledNode> {
    styled
        .nodes()
        .into_iter()
        .filter(|n| match &n.name {
            NodeName::Element(qualified) => qualified.name.as_str() == element,
            NodeName::Part(part) => part.as_str() == element,
            NodeName::Page => element == "page",
        })
        .collect()
}

fn indent(styled: &Styled, node: &StyledNode) -> Value {
    styled.value(node, "indent").cloned().unwrap_or(Value::None)
}

#[test]
fn the_example_element_end_to_end() {
    let world = project(MAIN);
    assert_eq!(messages(&world), Vec::<String>::new());
    assert_eq!(numbers(&world), ["1.1", "2.1", "2.2"]);
    assert_eq!(references(&world), ["Example~2.2", "Example~1.1"]);

    // Slots are parts, styled separately: `example > solution`.
    let styled = styled(&world);
    assert_eq!(all(&styled, "example").len(), 3);
    for solution in all(&styled, "solution") {
        assert_eq!(indent(&styled, solution), Value::Length(Abs::pt(1.0)));
    }
    for statement in all(&styled, "statement") {
        assert_ne!(indent(&styled, statement), Value::Length(Abs::pt(1.0)));
    }

    // The document builds to PDF.
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
}

#[test]
fn words_follow_the_language() {
    let polish = MANIFEST.replace("lang = \"en\"", "lang = \"pl\"");
    let mut world = World::builder().build(&polish, None).unwrap();
    world.set_source("main.fol", MAIN).unwrap();
    world.set_source("lib/blocks.fol", BLOCKS).unwrap();
    world.set_source("styles/main.fsty", STYLE).unwrap();
    assert_eq!(references(&world), ["przykł.~2.2", "przykł.~1.1"]);

    // A reference key the core knows needs no words; one it does not know
    // falls back to English in a language without words.
    let blocks = "\
#element worked(title: content = none) {
  counter: auto
  ref-key: \"figure\"
}
#element task() {
  counter: auto
  ref-key: \"task\"
  words: (en: \"Task\")
}
";
    let main = "#import \"lib/blocks.fol\": worked, task\n\n\
                #worked[A.] <w>\n\n#task[B.] <t>\n\nSee @w and @t.\n";
    let mut world = World::builder().build(&polish, None).unwrap();
    world.set_source("main.fol", main).unwrap();
    world.set_source("lib/blocks.fol", blocks).unwrap();
    world.set_source("styles/main.fsty", "").unwrap();
    assert_eq!(messages(&world), Vec::<String>::new());
    assert_eq!(references(&world), ["rys.~1", "Task~1"]);
}

#[test]
fn fields_are_typed_and_checked() {
    let blocks = "\
#element drawing(
  scale: layout-data(length) = 1cm,
  count: int = 3,
  tags: array(str) = (),
  caption: content = \"A drawing\",
) {
  kind: block
  body: none
}
";
    let world = world_with(&[
        (
            "main.fol",
            "#import \"lib/blocks.fol\": drawing\n\n#drawing(count: 4)\n",
        ),
        ("lib/blocks.fol", blocks),
        ("styles/main.fsty", ""),
    ]);
    assert_eq!(messages(&world), Vec::<String>::new());
    let content = folio::evaluate(&world, None).unwrap().unwrap();
    let debug = format!("{content:?}");
    assert!(debug.contains("drawing"), "{debug}");

    // Calls are checked against the declared schema.
    let world = project(
        "#import \"lib/blocks.fol\": example\n\n\
         #example(difficulty: \"hard\")[#statement[A.]]\n\n\
         #example(colour: \"red\")[#statement[A.]]\n",
    );
    let messages = messages(&world);
    assert!(
        messages.iter().any(|m| m.contains("difficulty")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("colour")),
        "{messages:?}"
    );
}

#[test]
fn declared_elements_are_imported_to_be_used() {
    // In the declaring file, the element is bound after its declaration.
    let world = world_with(&[
        (
            "main.fol",
            &format!("{BLOCKS}\n#example[#statement[A.]] <e>\n\n@e\n"),
        ),
        ("styles/main.fsty", STYLE),
    ]);
    assert_eq!(messages(&world), Vec::<String>::new());

    // Elsewhere it is imported.
    let world = project("#import \"lib/blocks.fol\"\n\n#example[#statement[A.]]\n");
    let found = diagnostics(&world);
    assert!(
        found.iter().any(|d| d.contains("unknown variable: example")
            && d.contains("#import \"/lib/blocks.fol\": example")),
        "{found:?}"
    );

    // Stylesheets see every declared element, imported or not.
    let world = world_with(&[
        ("main.fol", "#include \"lib/blocks.fol\"\n"),
        ("lib/blocks.fol", BLOCKS),
        ("styles/main.fsty", STYLE),
    ]);
    assert_eq!(messages(&world), Vec::<String>::new());
}

#[test]
fn declaration_errors() {
    let check = |blocks: &str, expected: &str| {
        let world = world_with(&[
            ("main.fol", "#import \"lib/blocks.fol\"\n"),
            ("lib/blocks.fol", blocks),
            ("styles/main.fsty", ""),
        ]);
        let found = diagnostics(&world);
        assert!(
            found.iter().any(|d| d.contains(expected)),
            "{blocks}: expected {expected:?} in {found:?}"
        );
    };
    check("#element figure() {}\n", "`figure` is a built-in element");
    check(
        "#element a() {}\n#element a() {}\n",
        "element `a` is already declared",
    );
    check("#element a(x: float) {}\n", "unknown type `float`");
    check(
        "#element a(x: length) {}\n",
        "a field of type `length` is formatting",
    );
    check(
        "#element a(x: layout-data(int)) {}\n",
        "only lengths and colours are marked `layout-data`",
    );
    check(
        "#element a(x: int = 1 + 1) {}\n",
        "expected a literal value",
    );
    check(
        "#element a(x: int = \"one\") {}\n",
        "the default has type `string`",
    );
    check(
        "#element a(class: str) {}\n",
        "every element already has the field `class`",
    );
    check(
        "#element a(body: str) {}\n",
        "a field cannot be named `body`",
    );
    check(
        "#element a(x: array(content)) {}\n",
        "a field cannot have type `array(content)`",
    );
    check("#element a() { kind: box }\n", "expected a kind");
    check("#element a() { colour: red }\n", "unknown entry `colour`");
    check(
        "#element a() { kind: block; kind: inline }\n",
        "duplicate entry `kind`",
    );
    check(
        "#element a() { ref-key: \"thing\" }\n",
        "the reference key `thing` has no words",
    );
    check(
        "#element a() { words: (en: \"A\") }\n",
        "`words` gives the words of a reference key",
    );
    check(
        "#element a() { ref-key: \"thing\"; words: (pl: \"rzecz\") }\n",
        "the words need English",
    );
    check(
        "#element a() { counter: within chapter }\n",
        "unknown element `chapter`",
    );
    check(
        "#element a(x: str) { body: slots(x: blocks) }\n",
        "slot `x` has the name of a field",
    );
    check(
        "#element a(x: lenght) {}\n#element a() {}\n",
        "element `a` is already declared",
    );
    check(
        "#element a() { counter: within heading(levle: 1) }\n",
        "`heading` has no field `levle`",
    );
    check(
        "#element a() { counter: within heading(level: \"one\") }\n",
        "field `level` of `heading` has type `int`, not `string`",
    );
    check(
        "#if true {\n  element a() {}\n}\n",
        "an element is declared at the top level of a file",
    );
}

#[test]
fn counters_scoped_to_declared_elements() {
    let blocks = "\
#element question() {
  counter: auto
  ref-key: \"question\"
  words: (en: \"Question\")
}
#element part() {
  counter: scoped(question, part)
  ref-key: \"part\"
  words: (en: \"part\")
}
";
    let main = "#import \"lib/blocks.fol\": question, part\n\n\
                #question[\n  #part[A. #part[B.] <b>]\n  #part[C.] <c>\n]\n\n\
                See @b and @c.\n";
    let style = "question { numbering: \"1\"; }\npart { numbering: \"(a)\"; }\n\
                 part part { numbering: \"(i)\"; }\n";
    let world = world_with(&[
        ("main.fol", main),
        ("lib/blocks.fol", blocks),
        ("styles/main.fsty", style),
    ]);
    assert_eq!(messages(&world), Vec::<String>::new());
    assert_eq!(references(&world), ["part~i", "part~b"]);
}

#[test]
fn shared_reference_keys_share_words() {
    // One element relies on the core's words for `figure`, another gives
    // its own: the given English word wins, without a conflict.
    let blocks = "\
#element diagram() {
  counter: auto
  ref-key: \"figure\"
}
#element photo() {
  counter: auto
  ref-key: \"figure\"
  words: (en: \"Fig.\")
}
";
    let main = "#import \"lib/blocks.fol\": diagram\n\n#diagram[A.] <d>\n\nSee @d.\n";
    let world = world_with(&[
        ("main.fol", main),
        ("lib/blocks.fol", blocks),
        ("styles/main.fsty", ""),
    ]);
    assert_eq!(messages(&world), Vec::<String>::new());
    assert_eq!(references(&world), ["Fig.~1"]);
}

#[test]
fn use_before_declaration_says_so() {
    let world = world_with(&[
        ("main.fol", &format!("#example[#statement[A.]]\n\n{BLOCKS}")),
        ("styles/main.fsty", ""),
    ]);
    let found = diagnostics(&world);
    assert!(
        found
            .iter()
            .any(|d| d.contains("declared later in this file")),
        "{found:?}"
    );
}

/// A component with a `question` element.
struct Exam;

impl folio_component::Component for Exam {
    fn manifest(&self) -> folio_component::ComponentManifest {
        folio_component::ComponentManifest::new(
            folio_component::ComponentName::new("exam").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> folio_component::Declarations {
        let mut question = folio_component::ElementSchema::new(
            folio::Name::new("question").unwrap(),
            folio_component::ElementKind::Block,
        );
        question.body = folio_component::Body::Content(folio_component::ContentKind::Blocks);
        let mut declarations = folio_component::Declarations::new();
        declarations.elements = [question].into_iter().collect();
        declarations
    }
}

#[test]
fn a_component_element_keeps_its_name() {
    let mut registry = Registry::new();
    registry.register(Exam).unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(MANIFEST, None)
        .unwrap();
    world
        .set_source("main.fol", "#element question() {}\n\n#question[A.]\n")
        .unwrap();
    world
        .set_source("styles/main.fsty", "question { indent: 1pt; }\n")
        .unwrap();
    let found = diagnostics(&world);
    assert_eq!(
        found,
        ["component `exam` already has an element `question` | give the element another name"]
    );
}
