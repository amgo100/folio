//! The base stylesheet (SPEC §5.1): the lowest origin of every build's
//! cascade, which gives the built-in elements a readable look in a project
//! without stylesheets, and which every project stylesheet wins over.

use folio::style::{BASE_STYLESHEET, NodeName, Styled, StyledNode};
use folio::{Name, Source, Target, World};
use folio_component::{Abs, Value};

/// A document with the built-in elements that the base stylesheet styles.
const MAIN: &str = "\
#doc(title: [Forces], subject: \"Notes\")

= Newton's laws

A force is a *push* or a _pull_. Code such as `F = m a` is set apart.

== The first law

- an object at rest stays at rest;
- an object in motion stays in motion.

+ measure the mass;
+ measure the acceleration.

/ Mass: how much matter.

=== Inertia

#figure[A trolley on a track.][A trolley]

Two paragraphs.
";

fn world(manifest_styles: Option<&str>, sheet: &str) -> World {
    let mut manifest =
        String::from("[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n");
    if let Some(path) = manifest_styles {
        manifest.push_str(&format!("styles = [\"{path}\"]\n"));
    }
    let mut world = World::builder().build(&manifest, None).unwrap();
    world.set_source("main.fol", MAIN).unwrap();
    if let Some(path) = manifest_styles {
        world.set_source(path, sheet).unwrap();
    }
    world
}

fn styled(world: &World) -> Styled {
    folio::styles(world, Target::Pdf, None).unwrap().unwrap()
}

fn all<'a>(styled: &'a Styled, element: &str) -> Vec<&'a StyledNode> {
    styled
        .nodes()
        .into_iter()
        .filter(|n| match &n.name {
            NodeName::Element(qualified) => qualified.to_string() == element,
            NodeName::Part(part) => part.as_str() == element,
            NodeName::Page => element == "page",
        })
        .collect()
}

#[track_caller]
fn first<'a>(styled: &'a Styled, element: &str) -> &'a StyledNode {
    all(styled, element)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no `{element}`"))
}

fn get(styled: &Styled, node: &StyledNode, property: &str) -> Value {
    styled.value(node, property).cloned().unwrap_or(Value::None)
}

fn pt(value: f64) -> Value {
    Value::Length(Abs::pt(value))
}

fn kw(word: &str) -> Value {
    Value::Keyword(Name::new(word).unwrap())
}

#[test]
fn the_base_stylesheet_parses_cleanly() {
    let source = Source::detached_stylesheet(BASE_STYLESHEET);
    let (errors, warnings) = source.root().errors_and_warnings();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_project_without_stylesheets_builds_without_diagnostics() {
    let world = world(None, "");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn built_in_elements_have_a_readable_look() {
    let world = world(None, "");
    let styled = styled(&world);

    // `strong` and `emph` stay semantic; the base stylesheet sets them.
    assert_eq!(
        get(&styled, first(&styled, "strong"), "font-weight"),
        kw("bold")
    );
    assert_eq!(
        get(&styled, first(&styled, "emph"), "font-style"),
        kw("italic")
    );

    // Headings are bold, sized by level (of the 11pt body), and kept with
    // what follows.
    let headings = all(&styled, "heading");
    assert_eq!(headings.len(), 3);
    for heading in &headings {
        assert_eq!(get(&styled, heading, "font-weight"), kw("bold"));
        assert_eq!(get(&styled, heading, "keep-with-next"), Value::Bool(true));
    }
    let size = |node| match get(&styled, node, "font-size") {
        Value::Length(size) => (size.to_pt() * 100.0).round() / 100.0,
        other => panic!("{other:?}"),
    };
    assert_eq!(size(headings[0]), 15.4);
    assert_eq!(size(headings[1]), 13.2);
    assert_eq!(size(headings[2]), 11.0);
    assert_eq!(size(first(&styled, "title")), 17.6);
    assert_eq!(
        get(&styled, first(&styled, "title"), "font-weight"),
        kw("bold")
    );

    // Paragraphs are spaced, list items closer together, lists indented.
    let paragraph = first(&styled, "paragraph");
    assert_eq!(get(&styled, paragraph, "space-before"), pt(5.5));
    assert_eq!(get(&styled, paragraph, "space-after"), pt(5.5));
    let item = first(&styled, "list-item");
    let in_item = item
        .children
        .iter()
        .find(|c| matches!(&c.name, NodeName::Element(q) if q.to_string() == "paragraph"))
        .expect("a list item holds a paragraph");
    assert_eq!(get(&styled, in_item, "space-before"), pt(2.2));
    assert_eq!(get(&styled, in_item, "space-after"), pt(2.2));
    for list in ["list", "enum", "terms"] {
        assert_eq!(
            get(&styled, first(&styled, list), "padding"),
            Value::Array([pt(0.0), pt(0.0), pt(0.0), pt(16.5)].into_iter().collect()),
            "{list}"
        );
    }
    assert_eq!(
        get(&styled, first(&styled, "term"), "font-weight"),
        kw("bold")
    );
    assert_eq!(
        get(&styled, first(&styled, "figure"), "space-before"),
        pt(11.0)
    );

    // Code is monospaced.
    let raw = first(&styled, "raw");
    assert_eq!(
        get(&styled, raw, "font"),
        Value::Array(
            [
                Value::Str("Source Code Pro".into()),
                Value::Str("New Computer Modern".into())
            ]
            .into_iter()
            .collect()
        )
    );
}

#[test]
fn project_stylesheets_win_whatever_their_specificity() {
    // A plain `paragraph` rule outranks the base sheet's `list-item >
    // paragraph`, and `strong` can be made regular again.
    let world = world(
        Some("styles/main.fsty"),
        "paragraph { space-before: 1pt; }\nstrong { font-weight: normal; }\n\
         heading { keep-with-next: false; }\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let styled = styled(&world);
    for paragraph in all(&styled, "paragraph") {
        assert_eq!(get(&styled, paragraph, "space-before"), pt(1.0));
        // What the project does not set still comes from the base sheet.
        assert_ne!(get(&styled, paragraph, "space-after"), pt(0.0));
    }
    assert_eq!(
        get(&styled, first(&styled, "strong"), "font-weight"),
        kw("normal")
    );
    assert_eq!(
        get(&styled, first(&styled, "heading"), "keep-with-next"),
        Value::Bool(false)
    );
}
