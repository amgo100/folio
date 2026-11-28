//! Render templates (SPEC §5.3): default and named templates, the closed
//! primitive set, conditions on fields, expansion in the Style stage and
//! the Office export classification.

use std::sync::Arc;

use ecow::EcoVec;
use folio::style::{NodeName, OfficeClass, Styled, StyledNode};
use folio::{Name, Registry, Severity, Target, Text, TextBlock, TextFlow, World, diag};
use folio_component::{
    Body, Component, ComponentManifest, ComponentName, ContentKind, Declarations, ElementKind,
    ElementSchema, TemplateDecl,
};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
lang = "en"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf"]

[variants.markscheme]
targets = ["pdf"]
"#;

/// The element of SPEC §4.3, with two instances: one with a title and an
/// extension difficulty, one of the class `short`.
const CONTENT: &str = r#"#element example(
  title: content = none,
  numbered: bool = true,
  difficulty: choice("core", "extension") = "core",
) {
  kind: block
  body: slots(statement: blocks, solution: blocks)
  counter: auto
  ref-key: "example"
  words: (en: "Example", pl: "przykł.")
}

#example(title: [Projectile on a slope], difficulty: "extension")[
  #statement[A particle is projected up a slope.]
  #solution[Resolve along the slope.]
]

#example(class: "short")[
  #statement[A ball is dropped.]
  #solution[Use the equations of motion.]
]
"#;

/// The templates of SPEC §5.3.
const TEMPLATES: &str = r#"
$accent: #1f5fa8;
$accent-tint: #e8eef7;

example {
  padding: 3mm 4mm; border-left: 1.5pt solid $accent; background: $accent-tint;
  space-before: 8pt; space-after: 8pt; breakable: true;
}

render example {
  header: row(
    strong(term() " " counter() if field(title) { ": " field(title) }),
    align-end(badge(field(difficulty))) if field(difficulty) == "extension",
  );
  body: slot(statement) rule(0.4pt) slot(solution);
}

render example as compact { body: slot(statement); }
example.short { template: compact; }
"#;

fn world(style: &str, content: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", content).unwrap();
    world
}

fn exam() -> Option<Name> {
    Name::new("exam")
}

/// Each diagnostic's severity, message and the source text it points at.
fn located(world: &World) -> Vec<(Severity, String, String)> {
    folio::compile(world, Target::Pdf, exam().as_ref())
        .unwrap()
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

fn text(world: &World) -> Text {
    folio::text(world, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap()
}

fn styles(world: &World) -> Styled {
    folio::styles(world, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap()
}

/// The blocks of an element, by its name, in document order.
fn blocks<'a>(block: &'a TextBlock, element: &str, out: &mut Vec<&'a TextBlock>) {
    if block.element.to_string() == element && block.part.is_none() {
        out.push(block);
    }
    for child in &block.children {
        if let TextFlow::Block(child) = child {
            blocks(child, element, out);
        }
    }
}

/// A block's structure as text: each block as `part[..]` (or `-[..]` for
/// an element's own block) and each paragraph as its text in quotes.
fn outline(block: &TextBlock, text: &Text) -> String {
    let inner: Vec<String> = block
        .children
        .iter()
        .map(|child| match child {
            TextFlow::Paragraph(i) => {
                let shown = text.paragraphs[*i].shaped.text().replace('\u{a0}', " ");
                format!("{shown:?}")
            }
            TextFlow::Block(child) => outline(child, text),
        })
        .collect();
    let name = block.part.as_ref().map_or("-", |p| p.as_str());
    format!("{name}[{}]", inner.join(" "))
}

fn example_outlines(world: &World) -> Vec<String> {
    let text = text(world);
    let mut found = Vec::new();
    blocks(&text.root, "project.example", &mut found);
    found.iter().map(|b| outline(b, &text)).collect()
}

#[track_caller]
fn first<'a>(styled: &'a Styled, element: &str) -> &'a StyledNode {
    styled
        .nodes()
        .into_iter()
        .find(|n| matches!(&n.name, NodeName::Element(e) if e.to_string() == element))
        .unwrap()
}

#[test]
fn the_example_templates_render_as_specified() {
    let world = world(TEMPLATES, CONTENT);
    assert_eq!(located(&world), []);
    assert_eq!(
        example_outlines(&world),
        [
            // The header: a row of the bold term, number and title, and
            // the badge set at its end; the body: the statement, a rule
            // and the solution.
            "-[header[row[cell[\"Example 1: Projectile on a slope\"] cell[\"extension\"]]] \
             statement[-[\"A particle is projected up a slope.\"]] rule[] \
             solution[-[\"Resolve along the slope.\"]]]",
            // `compact`: the statement alone.
            "-[statement[-[\"A ball is dropped.\"]]]",
        ]
    );
    let compiled = folio::compile(&world, Target::Pdf, exam().as_ref()).unwrap();
    assert!(compiled.output.is_some());
}

#[test]
fn conditions_read_the_fields_of_each_element() {
    // Without a title, the header has no colon; with a core difficulty,
    // no badge, and the row has one cell.
    let content = CONTENT.replace(r#"#example(class: "short")"#, "#example");
    let untitled = world(TEMPLATES, &content);
    let outlines = example_outlines(&untitled);
    assert_eq!(
        outlines[1],
        "-[header[row[cell[\"Example 2\"]]] statement[-[\"A ball is dropped.\"]] rule[] \
         solution[-[\"Use the equations of motion.\"]]]"
    );
    // `else`, `not` and comparisons.
    let style = TEMPLATES.replace(
        "render example as compact { body: slot(statement); }",
        "render example as compact {\n  \
           body: if not field(title) { \"Untitled\" } else { field(title) } \
           \" (\" text(\"core\") \")\" if field(difficulty) != \"extension\";\n}",
    );
    let world = world(&style, CONTENT);
    assert_eq!(example_outlines(&world)[1], "-[\"Untitled (core)\"]");
}

#[test]
fn the_number_follows_numbering_and_the_word_the_language() {
    let style = format!(
        "{TEMPLATES}\nexample {{ numbering: \"(a)\"; }}\n\
         example.short {{ template: auto; lang: \"pl\"; }}\n"
    );
    let world = world(&style, CONTENT);
    let outlines = example_outlines(&world);
    assert!(
        outlines[0].contains("\"Example (a): Projectile on a slope\""),
        "{outlines:?}"
    );
    assert!(outlines[1].contains("\"przykł. (b)\""), "{outlines:?}");
    // The element's marker is not set: the template places its number.
    assert!(!outlines[0].contains("(a) Example"), "{outlines:?}");
}

#[test]
fn templates_record_their_office_class() {
    let style = format!(
        "{TEMPLATES}\n\
         render example as table {{ body: grid(slot(statement), slot(solution), columns: 2); }}\n\
         render example as drawn {{ body: slot(statement) shape(); }}\n"
    );
    let content = format!(
        "{CONTENT}\n#example(class: \"table\")[#statement[A]]\n\n\
         #example(class: \"drawn\")[#statement[B]]\n"
    );
    let style = format!(
        "{style}\nexample.table {{ template: table; }}\nexample.drawn {{ template: drawn; }}\n"
    );
    let world = world(&style, &content);
    let styled = styles(&world);
    let classes: Vec<OfficeClass> = styled
        .nodes()
        .into_iter()
        .filter(|n| matches!(&n.name, NodeName::Element(e) if e.to_string() == "project.example"))
        .map(|n| n.template.as_ref().unwrap().office)
        .collect();
    assert_eq!(
        classes,
        [
            OfficeClass::HeaderBody,
            OfficeClass::Block,
            OfficeClass::Grid,
            OfficeClass::Graphic
        ]
    );
    // Graphic primitives draw nothing yet, which is reported.
    assert!(
        located(&world)
            .iter()
            .any(|(severity, message, at)| *severity == Severity::Warning
                && message.starts_with("graphic primitives are not drawn yet")
                && at.starts_with("render example as drawn")),
        "{:?}",
        located(&world)
    );
    // A template's grid is a grid of cells.
    let outlines = example_outlines(&world);
    // The cell of the missing solution is empty.
    assert_eq!(outlines[2], "-[grid[cell[statement[-[\"A\"]]] cell[]]]");
}

#[test]
fn errors_in_templates_point_at_the_primitive() {
    let style = "render example {\n  \
                   footer: \"x\";\n  \
                   body: slot(answer) field(colour) frobnicate() counter;\n}\n";
    let world = world(style, CONTENT);
    let errors: Vec<(String, String)> = located(&world)
        .into_iter()
        .filter(|(severity, ..)| *severity == Severity::Error)
        .map(|(_, message, at)| (message, at))
        .collect();
    assert_eq!(
        errors,
        [
            ("a template has no part `footer`".into(), "footer".into()),
            (
                "`example` has no slot or content field `answer`".into(),
                "slot(answer)".into()
            ),
            ("`example` has no field `colour`".into(), "colour".into()),
            (
                "`frobnicate` is not a template primitive".into(),
                "frobnicate".into()
            ),
            ("`counter` is not a template item".into(), "counter".into()),
        ]
    );
    // A template with an error is left out: the elements show their
    // content.
    assert_eq!(
        example_outlines(&world)[1],
        "-[statement[-[\"A ball is dropped.\"]] solution[-[\"Use the equations of motion.\"]]]"
    );
}

#[test]
fn a_template_name_without_a_template_is_reported() {
    let world = world("example { template: wide; }\n", CONTENT);
    let warnings: Vec<String> = located(&world).into_iter().map(|(_, m, _)| m).collect();
    assert_eq!(
        warnings,
        ["no template `wide` renders `project.example`, so it is shown without a template"]
    );
}

#[test]
fn later_origins_and_later_templates_win() {
    // The markscheme overlay's template wins over the project's in its
    // variant; of two templates in one sheet, the later one.
    let mut world = world(
        "render example { body: \"first\"; }\nrender example { body: \"second\"; }\n\
         @variant markscheme { render example { body: \"marked\"; } }\n",
        "#element example() { body: blocks }\n\n#example[]\n",
    );
    let shown = |world: &World, variant: &str| {
        let variant = Name::new(variant);
        let text = folio::text(world, Target::Pdf, variant.as_ref())
            .unwrap()
            .unwrap();
        text.paragraphs
            .iter()
            .map(|p| p.shaped.text().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(&world, "exam"), ["second"]);
    assert_eq!(shown(&world, "markscheme"), ["marked"]);
    // `none` shows the element without its template.
    world
        .set_source(
            "styles/main.fsty",
            "render example { body: \"first\"; }\nexample { template: none; }\n",
        )
        .unwrap();
    assert_eq!(shown(&world, "exam"), Vec::<String>::new());
}

#[test]
fn template_parts_are_styled_by_selectors() {
    let style = format!(
        "{TEMPLATES}\nexample > header {{ font-size: 14pt; }}\nexample badge {{ color: #d62728; }}\n"
    );
    let world = world(&style, CONTENT);
    let styled = styles(&world);
    let example = first(&styled, "project.example");
    let expansion = example.template.as_ref().unwrap();
    let header = expansion.header.as_ref().unwrap();
    let size = styled
        .properties
        .find(&folio_component::QualifiedName::new("font-size").unwrap())
        .unwrap();
    assert_eq!(
        header.style.value(&styled.properties, size),
        Some(&folio_component::Value::Length(folio_component::Abs::pt(
            14.0
        )))
    );
}

#[test]
fn an_inline_element_sets_its_template_in_its_line() {
    let world = world(
        "render strong { body: \"[\" slot() \"]\"; }\n",
        "Some *bold* text.\n",
    );
    let text = text(&world);
    let shown: Vec<String> = text
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().to_string())
        .collect();
    assert_eq!(shown, ["Some [bold] text."]);
}

#[test]
fn equal_elements_share_one_expansion() {
    let world = world(
        "render example { body: \"x\" slot(); }\n",
        "#element example() { body: blocks }\n\n#example[A]\n\n#example[A]\n",
    );
    let styled = styles(&world);
    let expansions: Vec<_> = styled
        .nodes()
        .into_iter()
        .filter_map(|n| n.template.clone())
        .collect();
    assert_eq!(expansions.len(), 2);
    assert!(Arc::ptr_eq(&expansions[0], &expansions[1]));
}

/// A component with a block element `aside` and templates for it, under
/// the namespace of a first-party component, which is enabled by default.
struct Notes;

impl Component for Notes {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new("timer").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let name = |t: &str| Name::new(t).unwrap();
        let mut note = ElementSchema::new(name("aside"), ElementKind::Block);
        note.body = Body::Content(ContentKind::Blocks);
        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([note]);
        declarations.templates = EcoVec::from([
            TemplateDecl::new(name("aside"), "header: strong(\"Note\"); body: slot();"),
            TemplateDecl::named(name("aside"), name("plain"), "body: slot();"),
        ]);
        declarations
    }
}

#[test]
fn components_provide_default_templates() {
    let mut registry = Registry::new();
    registry.register(Notes).unwrap();
    let make = |style: &str| {
        let mut world = World::builder()
            .components(registry.clone())
            .build(MANIFEST, None)
            .unwrap();
        world
            .set_source("main.fol", "#timer.aside[Read this.]\n")
            .unwrap();
        world.set_source("styles/main.fsty", style).unwrap();
        world
    };
    let shown = |world: &World| {
        let text = text(world);
        let mut found = Vec::new();
        blocks(&text.root, "timer.aside", &mut found);
        outline(found[0], &text)
    };
    let world = make("");
    assert_eq!(located(&world), []);
    assert_eq!(shown(&world), "-[header[\"Note\"] -[\"Read this.\"]]");
    // The component's named template, and the project's own default,
    // which wins over the component's.
    assert_eq!(
        shown(&make("aside { template: plain; }\n")),
        "-[-[\"Read this.\"]]"
    );
    assert_eq!(
        shown(&make("render aside { header: \"Aside\"; }\n")),
        "-[header[\"Aside\"] -[\"Read this.\"]]"
    );
}

#[test]
fn a_placeholder_replaces_the_template_too() {
    // In the mark scheme the examples are shown; in the exam, a
    // placeholder takes the place of their content and their template.
    let style = format!(
        "{TEMPLATES}\n@variant exam {{ example {{ display: placeholder(\"See the mark scheme\"); }} }}\n"
    );
    let world = world(&style, CONTENT);
    assert_eq!(
        example_outlines(&world),
        ["-[\"See the mark scheme\"]", "-[\"See the mark scheme\"]"]
    );
    let variant = Name::new("markscheme");
    let text = folio::text(&world, Target::Pdf, variant.as_ref())
        .unwrap()
        .unwrap();
    let mut found = Vec::new();
    blocks(&text.root, "project.example", &mut found);
    assert!(outline(found[0], &text).starts_with("-[header["));
}

#[test]
fn a_part_the_variant_hides_is_unset_for_the_template() {
    // In the exam the solution is hidden, so the header that tells there
    // is one is not shown either (SPEC §6.4).
    let style = "render example { header: \"Has a solution\" if field(solution); body: slot(statement); }\n\
                 @variant exam { example > solution { display: none; } }\n";
    let world = world(style, CONTENT);
    assert_eq!(
        example_outlines(&world)[0],
        "-[statement[-[\"A particle is projected up a slope.\"]]]"
    );
    let variant = Name::new("markscheme");
    let text = folio::text(&world, Target::Pdf, variant.as_ref())
        .unwrap()
        .unwrap();
    let mut found = Vec::new();
    blocks(&text.root, "project.example", &mut found);
    assert!(outline(found[0], &text).starts_with("-[header[\"Has a solution\"]"));
}

#[test]
fn a_space_in_a_row_is_a_width() {
    let style = "render example { header: row(\"A\", space(3cm), align-end(\"B\")); body: slot(statement); }\n";
    let world = world(style, CONTENT);
    let styled = styles(&world);
    let example = first(&styled, "project.example");
    let header = example.template.as_ref().unwrap().header.as_ref().unwrap();
    let folio::style::Piece::Block(row) = &header.items[0] else {
        panic!("a row");
    };
    let folio::style::Piece::Block(space) = &row.children[1] else {
        panic!("a cell");
    };
    assert!(space.children.is_empty());
    let min_height = styled
        .properties
        .find(&folio_component::QualifiedName::new("min-height").unwrap())
        .unwrap();
    assert_eq!(
        space.style.value(&styled.properties, min_height),
        Some(&folio_component::Value::Length(folio_component::Abs::zero()))
    );
}

#[test]
fn content_placed_twice_is_shown_once_and_empty_rows_vanish() {
    // The title in the header and again in the body: shown in the header
    // only. A row whose cells' conditions all fail is no block, so the
    // second example has no header.
    let style = "render example {\n  \
                   header: row(strong(field(title)) if field(title));\n  \
                   body: field(title) slot(statement);\n}\n";
    let world = world(style, CONTENT);
    let outlines = example_outlines(&world);
    assert_eq!(
        outlines,
        [
            "-[header[row[cell[\"Projectile on a slope\"]]] \
             statement[-[\"A particle is projected up a slope.\"]]]",
            "-[statement[-[\"A ball is dropped.\"]]]",
        ]
    );
}

#[test]
fn a_slot_shown_as_answer_space_keeps_its_space_where_the_template_places_it() {
    let style = format!(
        "{TEMPLATES}\n@variant exam {{ example > solution {{ display: answer-space; }} }}\n"
    );
    let world = world(&style, CONTENT);
    let text = text(&world);
    assert!(
        text.spaces
            .iter()
            .any(|s| s.part.as_ref().is_some_and(|p| p.as_str() == "solution")),
        "{:?}",
        text.spaces.iter().map(|s| &s.part).collect::<Vec<_>>()
    );
}

#[test]
fn content_a_template_leaves_out_is_not_counted() {
    // `compact` shows the statement only: the equation in the solution it
    // leaves out is neither numbered nor referable, so the next one is
    // number 1 (#446).
    let content = r#"#element example() {
  kind: block
  body: slots(statement: blocks, solution: blocks)
}

#example(class: "short")[
  #statement[A ball is dropped.]
  #solution[$ s = 1/2 g t^2 $ <eq:dropped>]
]

$ v = u + a t $ <eq:speed>

See @eq:speed.
"#;
    let style = "math { numbering: \"(1)\"; }\n\
                 render example as compact { body: slot(statement); }\n\
                 example.short { template: compact; }\n";
    let compact = world(style, content);
    let resolution = folio::resolve(&compact, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap();
    let numbers: Vec<String> = resolution
        .numbers
        .iter()
        .filter(|n| n.counter == "equation")
        .filter_map(|n| n.number.as_ref().map(|s| s.to_string()))
        .collect();
    assert_eq!(numbers, ["(1)"]);
    assert!(resolution.label("eq:dropped").is_none());
    assert!(resolution.label("eq:speed").is_some());
    // Without the template the solution is shown, and counted.
    let world = world("math { numbering: \"(1)\"; }\n", content);
    let resolution = folio::resolve(&world, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(
        resolution
            .numbers
            .iter()
            .filter(|n| n.counter == "equation")
            .count(),
        2
    );
}

#[test]
fn a_body_the_template_leaves_out_and_a_slot_its_header_places() {
    // The body (with a numbered equation) is not placed: it is not
    // counted. The note is placed by the header: it is kept.
    let content = r#"#element card(note: content = none) {
  kind: block
  body: blocks
}

#card(note: [Read first.])[
  $ x = 1 $ <eq:hidden>
]

$ y = 2 $
"#;
    let style = "math { numbering: \"(1)\"; }\n\
                 render card { header: field(note); body: \"Card\"; }\n";
    let world = world(style, content);
    let resolution = folio::resolve(&world, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap();
    let equations = resolution
        .numbers
        .iter()
        .filter(|n| n.counter == "equation")
        .count();
    assert_eq!(equations, 1);
    assert!(resolution.label("eq:hidden").is_none());
    let text = text(&world);
    let mut found = Vec::new();
    blocks(&text.root, "project.card", &mut found);
    assert_eq!(
        outline(found[0], &text),
        "-[header[\"Read first.\"] \"Card\"]"
    );
}

#[test]
fn a_part_shown_as_a_space_is_kept_but_conditions_read_it_unset() {
    // `if field(solution) { "Worked solution" slot(solution) }`: in the
    // exam the solution is a space, so the condition fails and the heading
    // does not tell that a solution exists (SPEC §6.4). Select still keeps
    // the part's block, so the variant counts what it counts without a
    // template, and its space stands after the body.
    let content = r#"#element example() {
  kind: block
  body: slots(statement: blocks, solution: blocks)
}

#example[
  #statement[A ball is dropped.]
  #solution[$ s = 1/2 g t^2 $]
]
"#;
    let count = |world: &World, variant: &str| {
        let variant = Name::new(variant);
        folio::resolve(world, Target::Pdf, variant.as_ref())
            .unwrap()
            .unwrap()
            .numbers
            .iter()
            .filter(|n| n.counter == "equation")
            .count()
    };
    for display in ["hidden", "answer-space"] {
        let overlay = format!("@variant exam {{ example > solution {{ display: {display}; }} }}\n");
        let templated = world(
            &format!(
                "math {{ numbering: \"(1)\"; }}\n\
                 render example {{ body: slot(statement) \
                 if field(solution) {{ \"Worked solution\" slot(solution) }}; }}\n{overlay}"
            ),
            content,
        );
        let plain = world(
            &format!("math {{ numbering: \"(1)\"; }}\n{overlay}"),
            content,
        );
        for variant in ["exam", "markscheme"] {
            assert_eq!(
                count(&templated, variant),
                count(&plain, variant),
                "{display} {variant}"
            );
        }
        let outline = &example_outlines(&templated)[0];
        assert!(!outline.contains("Worked solution"), "{display}: {outline}");
        let spaced = text(&templated)
            .spaces
            .iter()
            .any(|s| s.part.as_ref().is_some_and(|p| p.as_str() == "solution"));
        assert!(spaced, "{display}: {outline}");
    }
}

#[test]
fn blocks_an_inline_template_cannot_set_are_not_counted() {
    let content = r#"#element hint() {
  kind: inline
  body: slots(note: blocks)
}

Text #hint[#note[$ x = 1 $]] more.

$ y = 2 $
"#;
    let style = "math { numbering: \"(1)\"; }\nrender hint { body: \"(hint)\" slot(note); }\n";
    let world = world(style, content);
    let resolution = folio::resolve(&world, Target::Pdf, exam().as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(
        resolution
            .numbers
            .iter()
            .filter(|n| n.counter == "equation")
            .count(),
        1
    );
}

#[test]
fn a_field_in_a_line_shows_nothing_of_a_space() {
    // In the exam the solution is answer space: the header's line shows
    // its text and nothing of the space, which stands where `slot` places
    // it.
    let style = "render example {\n  \
                   header: strong(\"Answer: \" field(solution));\n  \
                   body: slot(statement) slot(solution);\n}\n\
                 @variant exam { example > solution { display: answer-space; } }\n";
    let world = world(style, CONTENT);
    assert_eq!(
        example_outlines(&world)[0],
        "-[header[\"Answer: \"] statement[-[\"A particle is projected up a slope.\"]] \
         solution[]]"
    );
}

#[test]
fn a_space_a_template_shows_only_in_a_line_keeps_its_space() {
    // The solution is placed by `field` alone: in the exam its answer
    // space follows the body, so the answer lines are not lost.
    let style = "render example {\n  \
                   header: strong(\"Answer: \" field(solution));\n  \
                   body: slot(statement);\n}\n\
                 @variant exam { example > solution { display: answer-space; } }\n";
    let world = world(style, CONTENT);
    assert_eq!(
        example_outlines(&world)[0],
        "-[header[\"Answer: \"] statement[-[\"A particle is projected up a slope.\"]] \
         solution[]]"
    );
    assert!(
        text(&world)
            .spaces
            .iter()
            .any(|s| s.part.as_ref().is_some_and(|p| p.as_str() == "solution"))
    );
}

#[test]
fn what_select_keeps_of_content_a_template_leaves_out() {
    // A list whose template does not place its body keeps the template's
    // text; what an inline template places in its line is kept as it is
    // without a template.
    let lists = world(
        "render list { body: \"(list omitted)\"; }\n",
        "Before.\n\n- one\n- two\n\nAfter.\n",
    );
    let shown: Vec<String> = text(&lists)
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().to_string())
        .collect();
    assert_eq!(shown, ["Before.", "(list omitted)", "After."]);

    let content = r#"#element hint() {
  kind: inline
  body: slots(note: inline)
}

Text #hint[#note[see $ x = 1 $ <eq:a>]] more.

$ y = 2 $
"#;
    let count = |style: &str| {
        let world = world(style, content);
        let resolution = folio::resolve(&world, Target::Pdf, exam().as_ref())
            .unwrap()
            .unwrap();
        (
            resolution.label("eq:a").is_some(),
            resolution
                .numbers
                .iter()
                .filter(|n| n.counter == "equation")
                .count(),
        )
    };
    // The template keeps what the line holds: the same as without it.
    assert_eq!(
        count("math { numbering: \"(1)\"; }\nrender hint { body: \"(\" slot(note) \")\"; }\n"),
        count("math { numbering: \"(1)\"; }\n")
    );
}
