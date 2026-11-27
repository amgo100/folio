//! Component default stylesheets in the cascade (SPEC §3.2, §5.1, #337).

use ecow::EcoVec;
use folio::style::{NodeName, Styled, StyledNode};
use folio::{Name, Registry, Target, World};
use folio_component::{
    Abs, Body, Component, ComponentManifest, ComponentName, ContentKind, Declarations, Effect,
    ElementKind, ElementSchema, INTERFACE_VERSION, PropertyDecl, PropertyType, Value,
};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf"]

[variants.markscheme]
targets = ["pdf"]
"#;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// A component with an element `item` and a property `gap`, which another
/// component also has, and a default stylesheet.
struct Sheet {
    namespace: &'static str,
    stylesheet: &'static str,
}

impl Component for Sheet {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest {
            name: ComponentName::new(self.namespace).unwrap(),
            version: semver::Version::new(0, 0, 0),
            interface: INTERFACE_VERSION,
            targets: EcoVec::from([Target::Pdf]),
        }
    }

    fn declarations(&self) -> Declarations {
        let mut item = ElementSchema::new(name("item"), ElementKind::Block);
        item.body = Body::Content(ContentKind::Blocks);
        Declarations {
            elements: EcoVec::from([item]),
            properties: EcoVec::from([PropertyDecl {
                name: name("gap"),
                ty: PropertyType::Length,
                initial: Value::Length(Abs::zero()),
                inherited: false,
                effect: Effect::Layout,
            }]),
            stylesheet: Some(self.stylesheet.into()),
            ..Declarations::default()
        }
    }
}

/// A world with the components `exam` and `plot`, whose default sheets
/// are given, and a project stylesheet.
fn project(exam: &'static str, plot: &'static str, project: &str) -> World {
    let mut registry = Registry::new();
    registry
        .register(&Sheet {
            namespace: "exam",
            stylesheet: exam,
        })
        .unwrap();
    registry
        .register(&Sheet {
            namespace: "plot",
            stylesheet: plot,
        })
        .unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(MANIFEST, None)
        .unwrap();
    world
        .set_source("main.fol", "#exam.item[A]\n\n#plot.item[B]\n\nText.\n")
        .unwrap();
    world.set_source("styles/main.fsty", project).unwrap();
    world
}

/// The styles of a build, which must have no errors.
#[track_caller]
fn styles(world: &World, variant: &str) -> Styled {
    let variant = name(variant);
    let compiled = folio::compile(world, Target::Pdf, Some(&variant)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    folio::styles(world, Target::Pdf, Some(&variant))
        .unwrap()
        .unwrap()
}

#[track_caller]
fn first<'a>(styled: &'a Styled, element: &str) -> &'a StyledNode {
    styled
        .nodes()
        .into_iter()
        .find(|n| match &n.name {
            NodeName::Element(qualified) => qualified.to_string() == element,
            _ => false,
        })
        .unwrap_or_else(|| panic!("no `{element}`"))
}

fn get(styled: &Styled, element: &str, property: &str) -> Value {
    let node = first(styled, element);
    styled.value(node, property).cloned().unwrap_or(Value::None)
}

fn pt(value: f64) -> Value {
    Value::Length(Abs::pt(value))
}

#[test]
fn a_component_sheet_styles_its_own_elements() {
    // Both components have `item` and `gap`, and no lockfile binds them, so
    // the project could not write them unqualified; each sheet resolves
    // them to its own.
    let world = project(
        "item { gap: 3pt; indent: 3pt; }\n",
        "item { gap: 4pt; }\n",
        "",
    );
    let styled = styles(&world, "exam");
    assert_eq!(get(&styled, "exam.item", "exam.gap"), pt(3.0));
    assert_eq!(get(&styled, "exam.item", "indent"), pt(3.0));
    assert_eq!(get(&styled, "plot.item", "plot.gap"), pt(4.0));
    assert_ne!(get(&styled, "plot.item", "indent"), pt(3.0));
}

#[test]
fn a_component_sheet_ranks_above_the_base_and_below_the_project() {
    // Above the base sheet: the component restyles a built-in element.
    let world = project("paragraph { indent: 4pt; }\n", "", "");
    let styled = styles(&world, "exam");
    assert_eq!(get(&styled, "paragraph", "indent"), pt(4.0));

    // Below the project, whatever the specificity.
    let world = project(
        "item:first { space-after: 8pt; indent: 3pt; }\n",
        "",
        "exam.item { space-after: 2pt; }\n",
    );
    let styled = styles(&world, "exam");
    assert_eq!(get(&styled, "exam.item", "space-after"), pt(2.0));
    assert_eq!(get(&styled, "exam.item", "indent"), pt(3.0));
}

#[test]
fn variant_blocks_in_a_component_sheet_keep_its_origin() {
    const EXAM: &str = "@variant markscheme { item { space-before: 6pt; } }\n";
    let world = project(EXAM, "", "");
    assert_eq!(
        get(&styles(&world, "markscheme"), "exam.item", "space-before"),
        pt(6.0)
    );
    assert_ne!(
        get(&styles(&world, "exam"), "exam.item", "space-before"),
        pt(6.0)
    );

    // The project's plain rule still wins over the component's block.
    let world = project(EXAM, "", "exam.item { space-before: 1pt; }\n");
    assert_eq!(
        get(&styles(&world, "markscheme"), "exam.item", "space-before"),
        pt(1.0)
    );
}

#[test]
fn a_project_token_rethemes_a_component_sheet() {
    const EXAM: &str = "$gap: 5pt;\nitem { exam.gap: $gap; }\n";
    let world = project(EXAM, "", "");
    assert_eq!(
        get(&styles(&world, "exam"), "exam.item", "exam.gap"),
        pt(5.0)
    );
    let world = project(EXAM, "", "$gap: 6pt;\n");
    assert_eq!(
        get(&styles(&world, "exam"), "exam.item", "exam.gap"),
        pt(6.0)
    );
}

#[test]
fn a_component_sheet_cannot_import() {
    let world = project("@import \"x.fsty\";\n", "", "");
    let compiled = folio::compile(&world, Target::Pdf, Some(&name("exam"))).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cannot import")),
        "{:?}",
        compiled.diagnostics
    );
}
