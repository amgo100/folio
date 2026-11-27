//! The document model (SPEC §4.2, §4.3): built-in elements, schemas, typed
//! fields, labels, and the structure of the document tree.

use ecow::EcoVec;
use folio::diag;
use folio::eval::Value;
use folio::model::Document;
use folio::{Name, Registry, Target, World};
use folio_component::{
    Body, Component, ComponentManifest, ComponentName, ContentKind, CounterDecl, Declarations,
    ElementKind, ElementSchema, FieldDecl, FieldType, LocaleString, SlotDecl,
};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"

[variants.exam]
targets = [\"pdf\"]

[variants.markscheme]
targets = [\"pdf\"]
";

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// A test stand-in for the exam component (SPEC §13.1), and an element with
/// slots (SPEC §4.3).
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
        question.fields = EcoVec::from([FieldDecl::required(name("marks"), FieldType::Int)]);
        question.body = Body::Content(ContentKind::Blocks);
        question.counter = Some(CounterDecl::default());
        question.ref_key = Some(name("question"));

        let mut answer = ElementSchema::new(name("answer"), ElementKind::Block);
        answer.fields = EcoVec::from([FieldDecl::with_default(
            name("lines"),
            FieldType::Int,
            folio_component::Value::None,
        )]);
        answer.body = Body::Content(ContentKind::Blocks);

        let mut mark = ElementSchema::new(name("mark"), ElementKind::Inline);
        mark.fields = EcoVec::from([FieldDecl::required(name("code"), FieldType::Str)]);
        mark.positional = EcoVec::from([name("code")]);

        let mut example = ElementSchema::new(name("example"), ElementKind::Block);
        example.fields = EcoVec::from([
            FieldDecl::with_default(
                name("title"),
                FieldType::Content,
                folio_component::Value::None,
            ),
            FieldDecl::with_default(
                name("label-text"),
                FieldType::Content,
                folio_component::Value::Str("Example".into()),
            ),
            FieldDecl::with_default(
                name("numbered"),
                FieldType::Bool,
                folio_component::Value::Bool(true),
            ),
            FieldDecl::with_default(
                name("difficulty"),
                FieldType::Choice(EcoVec::from(["core".into(), "extension".into()])),
                folio_component::Value::Str("core".into()),
            ),
            FieldDecl::with_default(
                name("tags"),
                FieldType::Array(Box::new(FieldType::Str)),
                folio_component::Value::None,
            ),
            FieldDecl::with_default(name("see"), FieldType::Ref, folio_component::Value::None),
        ]);
        example.body = Body::Slots(EcoVec::from([
            SlotDecl {
                name: name("statement"),
                content: ContentKind::Blocks,
            },
            SlotDecl {
                name: name("solution"),
                content: ContentKind::Blocks,
            },
            SlotDecl {
                name: name("hint"),
                content: ContentKind::Inline,
            },
        ]));

        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([question, answer, mark, example]);
        declarations.locale_strings = EcoVec::from([LocaleString {
            lang: "en".into(),
            key: name("question"),
            text: "Question".into(),
        }]);
        declarations
    }
}

/// A world with the exam component and the given files.
fn world(files: &[(&str, &str)]) -> World {
    let mut registry = Registry::new();
    registry.register(&Exam).unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(MANIFEST, None)
        .unwrap();
    for (path, text) in files {
        if path.ends_with(".fol") {
            world.set_source(path, text).unwrap();
        } else {
            world.set_file(path, text.as_bytes()).unwrap();
        }
    }
    world
}

/// Whether a diagnostic says only that a feature is not laid out or
/// implemented yet, or why the PDF does not claim PDF/UA-1 (the spec's
/// example has an image without alt text), which the document model does
/// not depend on.
fn not_yet(diagnostic: &folio::Diagnostic) -> bool {
    let message = diagnostic.message.as_str();
    diagnostic.severity == folio::Severity::Warning
        && (message.ends_with("is not laid out yet, so the output leaves it out")
            || message.ends_with("are not laid out yet, so the output leaves them out")
            || message.ends_with("is not laid out yet, so the output shows at most its box")
            || message.ends_with("is not implemented yet, so this declaration has no effect")
            || message.ends_with("so it has no effect")
            || message.starts_with("the PDF does not claim PDF/UA-1"))
}

/// The document tree of the exam variant; panics with the diagnostics if
/// there are any other than [`not_yet`] warnings.
fn document(files: &[(&str, &str)]) -> Document {
    let world = world(files);
    let exam = name("exam");
    let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    let diagnostics: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| !not_yet(d))
        .cloned()
        .collect();
    assert!(
        diagnostics.is_empty(),
        "{}",
        diag::render(&world, &diagnostics, false)
    );
    folio::document(&world, Some(&exam))
        .unwrap()
        .expect("the document evaluates")
}

fn tree(main: &str) -> String {
    document(&[("main.fol", main)]).repr()
}

/// The rendered diagnostics of the exam variant.
fn errors(main: &str) -> String {
    let world = world(&[("main.fol", main), ("ball.svg", "<svg/>")]);
    let compiled = folio::compile(&world, Target::Pdf, Some(&name("exam"))).unwrap();
    assert!(compiled.has_errors(), "expected errors");
    diag::render(&world, &compiled.diagnostics, false)
}

/// The first line of each diagnostic: `error: ...`.
fn messages(main: &str) -> Vec<String> {
    errors(main)
        .lines()
        .filter(|l| l.starts_with("error") || l.starts_with("warning"))
        .map(str::to_owned)
        .collect()
}

/// The example of SPEC §4.2.
const SPEC_EXAMPLE: &str = r#"#doc(title: "Mechanics: Test 3", subject: "WPH11")

= Kinematics <sec:kin>

A ball is thrown vertically upwards with speed $u = qty(12, "m s^-1")$.
Air resistance is negligible.#sidenote[Take $g = qty(9.81, "m s^-2")$.]

#figure(placement: wrap, side: outer, width: 1/3)[
  #image("assets/ball.svg")
][Path of the ball.] <fig:ball>

#question(marks: 3)[
  Calculate the maximum height reached. See @fig:ball.
  #answer(lines: 4)[
    $h = u^2 / (2g) = qty(7.3, "m")$ #mark("M1") #mark("A1")
  ]
]
"#;

#[test]
fn the_spec_example_evaluates_to_its_tree() {
    let document = document(&[("main.fol", SPEC_EXAMPLE), ("assets/ball.svg", "<svg/>")]);
    assert_eq!(
        document.repr(),
        r#"doc(title: ["Mechanics: Test 3"], subject: "WPH11")
  section(level: 1)
    heading(level: 1, body: ["Kinematics"]) <sec:kin>
    paragraph(body: ["A ball is thrown vertically upwards with speed", space, math(block: false, tree: formula([u, rel(=), qty(12, unit: "m s^-1")])), ".", space, "Air resistance is negligible.", sidenote(body: [paragraph(body: ["Take", space, math(block: false, tree: formula([g, rel(=), qty(9.81, unit: "m s^-2")])), "."])])])
    figure(caption: ["Path of the ball."], placement: "wrap", side: "outer", width: 0.3333333333333333) <fig:ball>
      paragraph(body: [image(path: "assets/ball.svg")])
    exam.question(marks: 3)
      paragraph(body: ["Calculate the maximum height reached. See", space, ref(target: <fig:ball>), "."])
      exam.answer(lines: 4)
        paragraph(body: [math(block: false, tree: formula([h, rel(=), frac([attach([u], sup: [num(2)])], [num(2), g]), rel(=), qty(7.3, unit: "m")])), space, exam.mark(code: "M1"), space, exam.mark(code: "A1")])
"#
    );

    // The root has the fields of `#doc`; its fields are typed.
    let root = &document.root;
    assert_eq!(root.name.to_string(), "doc");
    assert_eq!(root.field("subject"), Some(&Value::Str("WPH11".into())));
}

// ---- Structure ----

#[test]
fn a_document_without_doc_has_a_doc_root() {
    assert_eq!(tree("Hello"), "doc\n  paragraph(body: [\"Hello\"])\n");
    assert_eq!(tree(""), "doc(body: [])\n");
}

#[test]
fn inline_content_becomes_paragraphs() {
    assert_eq!(
        tree("  A *b*  \nc\n\n\n_d_ #raw(\"x\", block: true) e\n"),
        "\
doc
  paragraph(body: [\"A\", space, strong(body: [\"b\"]), space, \"c\"])
  paragraph(body: [emph(body: [\"d\"])])
  raw(text: \"x\", block: true)
  paragraph(body: [\"e\"])
"
    );
}

#[test]
fn list_items_become_lists() {
    assert_eq!(
        tree("- a\n- b\n\n- c\n  - nested\n+ one\n/ T: d\nText\n- e\n"),
        "\
doc
  list
    list-item
      paragraph(body: [\"a\"])
    list-item
      paragraph(body: [\"b\"])
    list-item
      paragraph(body: [\"c\"])
      list
        list-item
          paragraph(body: [\"nested\"])
  enum
    enum-item
      paragraph(body: [\"one\"])
  terms
    terms-item(term: [\"T\"])
      paragraph(body: [\"d\"])
  paragraph(body: [\"Text\"])
  list
    list-item
      paragraph(body: [\"e\"])
"
    );
}

#[test]
fn headings_open_nested_sections() {
    assert_eq!(
        tree("Intro\n= A\na\n== B\nb\n=== C\n== D\n= E\n#heading(level: 3)[F]\n"),
        "\
doc
  paragraph(body: [\"Intro\"])
  section(level: 1)
    heading(level: 1, body: [\"A\"])
    paragraph(body: [\"a\"])
    section(level: 2)
      heading(level: 2, body: [\"B\"])
      paragraph(body: [\"b\"])
      section(level: 3)
        heading(level: 3, body: [\"C\"])
    section(level: 2)
      heading(level: 2, body: [\"D\"])
  section(level: 1)
    heading(level: 1, body: [\"E\"])
    section(level: 3)
      heading(level: 3, body: [\"F\"])
"
    );
    // Sections close at the end of the element that holds them.
    assert_eq!(
        tree("#question(marks: 1)[= Part\nText]\nAfter\n"),
        "\
doc
  exam.question(marks: 1)
    section(level: 1)
      heading(level: 1, body: [\"Part\"])
      paragraph(body: [\"Text\"])
  paragraph(body: [\"After\"])
"
    );
}

#[test]
fn scoped_rules_stay_with_their_element() {
    // A rule applies to the enclosing element, so a heading does not take
    // it into its section.
    assert_eq!(
        tree("= A\nText #rule heading { numbering: \"1.\"; }\nmore\n"),
        "\
doc
  rule
  section(level: 1)
    heading(level: 1, body: [\"A\"])
    paragraph(body: [\"Text\", space, \"more\"])
"
    );
}

// ---- Fields ----

#[test]
fn fields_are_typed_and_take_defaults() {
    let main = "\
#let ex = example(title: \"Slope\", tags: (\"a\", \"b\"), see: <x>)[#statement[S]]
#ex.title #ex.numbered #ex.difficulty #ex.tags #ex.see
#repr((ex.class, ex.solution, ex.id, figure[].caption, figure[].placement))
";
    let document = document(&[("main.fol", main)]);
    assert_eq!(
        document.repr(),
        "\
doc
  paragraph(body: [\"Slope\", space, \"true\", space, \"core\", space, \"(\\\"a\\\", \\\"b\\\")\", space, \"<x>\", space, \"(none, none, none, none, none)\"])
"
    );
    assert_eq!(
        tree("#example(title: [A *b*], difficulty: \"extension\")[#statement[S]] <ex>\n"),
        "\
doc
  exam.example(title: [\"A\", space, strong(body: [\"b\"])], label-text: [\"Example\"], numbered: true, difficulty: \"extension\") <ex>
    statement:
      paragraph(body: [\"S\"])
"
    );
}

#[test]
fn universal_fields() {
    assert_eq!(
        document(&[("b.svg", "<svg/>"), ("main.fol",
            "#figure(class: \"wide hard\", id: \"fig:a\", steps: \"2-\", handout: keep, \
             override@markscheme: (space-before: 24pt), override@pptx: (:), \
             break-before: page, keep-with-next: true)[]\n\
             #image(\"b.svg\", class: (\"icon\",), steps: 3)\n"
        )])
        .repr(),
        "\
doc
  figure(body: [], class: (\"wide\", \"hard\"), steps: \"2-\", handout: \"keep\", override@markscheme: (space-before: 24.0pt), override@pptx: (:), break-before: \"page\", keep-with-next: true) <fig:a>
  paragraph(body: [image(path: \"b.svg\", class: (\"icon\",), steps: \"3\")])
"
    );
}

/// Slots are stored in their declared order, whatever order they are
/// filled in (SPEC §4.3).
#[test]
fn slots_are_filled_by_sub_blocks() {
    assert_eq!(
        tree(
            "#example[\n  #solution[Resolve.\n\nThen solve.]\n  #hint[Use *axes*]\n  #statement[A particle.]\n] <ex:slope>\n"
        ),
        "\
doc
  exam.example(label-text: [\"Example\"], numbered: true, difficulty: \"core\", hint: [\"Use\", space, strong(body: [\"axes\"])]) <ex:slope>
    statement:
      paragraph(body: [\"A particle.\"])
    solution:
      paragraph(body: [\"Resolve.\"])
      paragraph(body: [\"Then solve.\"])
"
    );
    // Slot names are defined only in the element's arguments.
    assert_eq!(
        messages("#statement[A]"),
        ["error: unknown variable: statement"]
    );
}

#[test]
fn elements_with_arguments_applied_keep_slots_and_keywords() {
    assert_eq!(
        tree(
            "#let ex = example.with(difficulty: \"extension\")\n\
             #ex[#statement[S]]\n\
             #let fig = figure.with(placement: top)\n\
             #fig[]\n"
        ),
        "\
doc
  exam.example(label-text: [\"Example\"], numbered: true, difficulty: \"extension\")
    statement:
      paragraph(body: [\"S\"])
  figure(body: [], placement: \"top\")
"
    );
    // A variable is its value, not a keyword.
    assert_eq!(
        tree("#let p = \"bottom\"\n#figure(placement: p)[]\n"),
        "doc\n  figure(body: [], placement: \"bottom\")\n"
    );
}

#[test]
fn floats_in_running_text_stay_as_anchors() {
    assert_eq!(
        tree("Text #figure(placement: wrap)[] more.\n\n#figure[]\nAfter\n"),
        "\
doc
  paragraph(body: [\"Text\", space, figure(body: [], placement: \"wrap\"), space, \"more.\"])
  figure(body: [])
  paragraph(body: [\"After\"])
"
    );
}

#[test]
fn built_in_elements_can_be_called() {
    assert_eq!(
        tree("#heading(level: 2)[A] #strong[b] #emph(\"c\") #raw(\"x\", lang: \"rs\")\n"),
        "\
doc
  section(level: 2)
    heading(level: 2, body: [\"A\"])
    paragraph(body: [strong(body: [\"b\"]), space, emph(body: [\"c\"]), space, raw(text: \"x\", block: false, lang: \"rs\")])
"
    );
    // Structural elements are made by markup and the model, not by calls.
    assert_eq!(
        messages("#paragraph[A]"),
        ["error: unknown variable: paragraph"]
    );
}

// ---- Errors ----

#[test]
fn schema_violations_are_spanned_errors() {
    assert_eq!(
        errors("#question(marks: \"3\")[A]"),
        "\
error: field `marks` of `exam.question`: expected an integer, found a str
  ┌─ main.fol:1:11
  │
1 │ #question(marks: \"3\")[A]
  │           ^^^^^^^^^^

"
    );
    assert_eq!(
        errors("#question(marks: 1, font: \"Arial\")[A]"),
        "\
error: `exam.question` has no field `font`
  ┌─ main.fol:1:21
  │
1 │ #question(marks: 1, font: \"Arial\")[A]
  │                     ^^^^^^^^^^^^^
  │
  = hint: formatting belongs in a stylesheet; give the element a `class`, or use `override` for this one instance

"
    );
    assert_eq!(
        errors("#question[A]"),
        "\
error: `exam.question` needs the field `marks`
  ┌─ main.fol:1:2
  │
1 │ #question[A]
  │  ^^^^^^^^^^^
  │
  = hint: give it as `marks: ..`

"
    );
}

#[test]
fn schema_violations() {
    let cases: &[(&str, &str)] = &[
        (
            "#question(marks: 1, level: 2)[A]",
            "error: `exam.question` has no field `level`",
        ),
        (
            "#example(difficulty: \"hard\")[]",
            "error: field `difficulty` of `exam.example`: expected one of \"core\", \"extension\", found \"hard\"",
        ),
        (
            "#example(see: \"x\")[]",
            "error: field `see` of `exam.example`: a reference is a label; write `<x>`",
        ),
        (
            "#example(tags: (\"a\", 1))[]",
            "error: field `tags` of `exam.example`: item 1: expected a string, found an int",
        ),
        (
            "#example(title: [A\n\nB])[]",
            "error: field `title` of `exam.example`: expected inline content, found a paragraph break",
        ),
        (
            "#example(statement: [A])[]",
            "error: `exam.example` has no field `statement`",
        ),
        ("#mark(\"M1\", \"A1\")", "error: unexpected argument"),
        ("#mark(\"M1\")[x]", "error: unexpected argument"),
        (
            "#mark(code: \"M1\", body: [x])",
            "error: `exam.mark` takes no body",
        ),
        ("#image(\"ball.svg\")[x]", "error: unexpected argument"),
        (
            "#question(marks: 1, ..(marks: 2))[A]",
            "error: duplicate field `marks`",
        ),
        (
            "#question(marks: 1, body: [A])[B]",
            "error: duplicate field `body`: it is given by name and as the body",
        ),
        (
            "#question(marks@markscheme: 1)[A]",
            "error: field `marks` of `exam.question` cannot be limited to a variant or target",
        ),
        (
            "#question(marks: 1, override@teacher: (:))[A]",
            "error: `teacher` is neither a variant nor a target",
        ),
        (
            "= A #figure[]",
            "error: the body of `heading` takes inline content, but `figure` is a block",
        ),
        (
            "#strong[A\n\nB]",
            "error: the body of `strong` takes inline content, but has a paragraph break",
        ),
        (
            "#question(marks: 1)[#statement[A]]",
            "error: unknown variable: statement",
        ),
        (
            "#example[#question(marks: 1)[#statement[A]]]",
            "error: `#statement` fills a slot of `exam.example`, so it belongs directly in its body",
        ),
        (
            "#example[#statement[A] Text]",
            "error: the body of `exam.example` holds only its slots",
        ),
        (
            "#example[#statement[A] #statement[B]]",
            "error: slot `statement` of `exam.example` is filled twice",
        ),
        (
            "#example[#hint[A #figure[]]]",
            "error: slot `hint` of `exam.example` takes inline content, but `figure` is a block",
        ),
        (
            "#figure(class: \"\")[]",
            "error: `class` needs at least one class name",
        ),
        (
            "#figure(class: \"a.b\")[]",
            "error: `a.b` is not a class name",
        ),
        ("#figure(id: \"a:\")[]", "error: `a:` is not a valid label"),
        (
            "#figure(id: <a>)[] <b>",
            "error: this element already has a label",
        ),
        (
            "#figure(steps: \"2--\")[]",
            "error: `2--` is not a step specification; write steps such as `\"3\"`, `\"2-4\"`, `\"2-\"` or `\"1,3-4\"`",
        ),
        ("#figure(handout: drop)[]", "error: `handout` takes `keep`"),
        (
            "#figure(override: 1)[]",
            "error: `override` takes a dictionary of properties, such as `(space-before: 24pt)`",
        ),
        (
            "#image(\"missing.svg\")",
            "error: file `missing.svg` not found",
        ),
        (
            "#image(\"../x.svg\")",
            "error: `../x.svg` is not a path in the project",
        ),
        (
            "#raw(1)",
            "error: field `text` of `raw`: expected a string, found an int",
        ),
        (
            "#question(marks: none)[A]",
            "error: `exam.question` needs a value for `marks`, not `none`",
        ),
        (
            "#heading(level: 0)[A]",
            "error: a heading's level is 1 or more",
        ),
        (
            "#figure(steps: \"4-2\")[]",
            "error: `4-2` is not a step specification; write steps such as `\"3\"`, `\"2-4\"`, `\"2-\"` or `\"1,3-4\"`",
        ),
        (
            "#figure(steps: \"+3\")[]",
            "error: `+3` is not a step specification; write steps such as `\"3\"`, `\"2-4\"`, `\"2-\"` or `\"1,3-4\"`",
        ),
    ];
    for (main, expected) in cases {
        assert_eq!(messages(main), [*expected], "{main}");
    }
}

#[test]
fn element_errors_inside_functions_point_at_the_call() {
    assert_eq!(
        errors("#let q(m) = question(marks: m)[A]\n#q(\"x\")"),
        "\
error: field `marks` of `exam.question`: expected an integer, found a str
  ┌─ main.fol:1:22
  │
1 │ #let q(m) = question(marks: m)[A]
  │                      ^^^^^^^^
2 │ #q(\"x\")
  │  ------ in this call of `q`

"
    );
}

#[test]
fn doc_is_set_once_at_the_top_level() {
    assert_eq!(
        messages("#doc(title: \"A\")\n#doc(title: \"B\")\n"),
        ["error: the document has `#doc` twice"]
    );
    assert_eq!(messages("#doc[Body]"), ["error: `#doc` takes no body"]);
    assert_eq!(
        messages("#question(marks: 1)[#doc()]"),
        ["error: `#doc` belongs at the top level of the document"]
    );
}

#[test]
fn labels_name_one_element() {
    assert_eq!(
        errors("= A <a>\n#figure[] <a>\n"),
        "\
error: label `<a>` is used twice
  ┌─ main.fol:2:2
  │
1 │ = A <a>
  │ --- it is first used here
2 │ #figure[] <a>
  │  ^^^^^^^^
  │
  = hint: a label names one element, so references can find it

"
    );
    // Labels are per variant: content of other variants does not clash.
    let main = "#if variant == \"exam\" [= A <a>] else [= B <a>]\n";
    assert_eq!(
        tree(main),
        "doc\n  section(level: 1)\n    heading(level: 1, body: [\"A\"]) <a>\n"
    );
}
