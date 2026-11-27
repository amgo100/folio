//! Cascade origins of tokens and of stylesheets reached more than once
//! (SPEC §5.1, #329).

use folio::style::{NodeName, Styled};
use folio::{Name, Target, World};
use folio_component::{Abs, Value};

/// A world whose project lists `styles`, with the stylesheets `sheets`.
fn project(styles: &[&str], overlay: Option<&str>, sheets: &[(&str, &str)]) -> World {
    let list = |paths: &[&str]| {
        paths
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let overlay = overlay.map_or(String::new(), |o| format!("styles = [{}]\n", list(&[o])));
    let manifest = format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         styles = [{}]\n\n[variants.exam]\ntargets = [\"pdf\"]\n{overlay}",
        list(styles)
    );
    let mut world = World::builder().build(&manifest, None).unwrap();
    world.set_source("main.fol", "= Forces\n\nText.\n").unwrap();
    for (path, text) in sheets {
        world.set_source(path, text).unwrap();
    }
    world
}

/// The font size of the first heading of the exam build, which must have
/// no errors.
#[track_caller]
fn heading_size(world: &World) -> Value {
    let variant = Name::new("exam").unwrap();
    let compiled = folio::compile(world, Target::Pdf, Some(&variant)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let styled: Styled = folio::styles(world, Target::Pdf, Some(&variant))
        .unwrap()
        .unwrap();
    let heading = styled
        .nodes()
        .into_iter()
        .find(|n| matches!(&n.name, NodeName::Element(q) if q.to_string() == "heading"))
        .expect("a heading");
    styled.value(heading, "font-size").cloned().unwrap()
}

fn pt(value: f64) -> Value {
    Value::Length(Abs::pt(value))
}

const A: (&str, &str) = (
    "styles/a.fsty",
    "$size: 30pt;\nheading { font-size: $size; }\n",
);
const THEME: (&str, &str) = ("styles/theme.fsty", "$size: 8pt;\n");

#[test]
fn a_theme_token_loses_to_a_project_token_read_before_it() {
    // The theme is read after `a.fsty`, through `b.fsty`, but its origin
    // is lower.
    let world = project(
        &["styles/a.fsty", "styles/b.fsty"],
        None,
        &[A, ("styles/b.fsty", "@import \"theme.fsty\";\n"), THEME],
    );
    assert_eq!(heading_size(&world), pt(30.0));

    // Where the import is written makes no difference.
    let world = project(
        &["styles/a.fsty"],
        None,
        &[
            (
                "styles/a.fsty",
                "$size: 30pt;\nheading { font-size: $size; }\n@import \"theme.fsty\";\n",
            ),
            THEME,
        ],
    );
    assert_eq!(heading_size(&world), pt(30.0));
}

#[test]
fn a_token_in_an_overlay_wins_over_the_project() {
    let world = project(
        &["styles/a.fsty"],
        Some("styles/exam.fsty"),
        &[A, ("styles/exam.fsty", "$size: 12pt;\n")],
    );
    assert_eq!(heading_size(&world), pt(12.0));

    // So does one in an `@variant` block written before the project's.
    let world = project(
        &["styles/v.fsty", "styles/a.fsty"],
        None,
        &[A, ("styles/v.fsty", "@variant exam { $size: 14pt; }\n")],
    );
    assert_eq!(heading_size(&world), pt(14.0));
}

#[test]
fn a_file_reached_twice_has_the_highest_origin() {
    // `theme.fsty` is imported by `a.fsty` and listed in the project's
    // stylesheets: it is a project stylesheet, read where the list puts it,
    // so source order decides between it and `a.fsty`.
    let importing = (
        "styles/a.fsty",
        "@import \"theme.fsty\";\n$size: 30pt;\nheading { font-size: $size; }\n",
    );
    let world = project(
        &["styles/a.fsty", "styles/theme.fsty"],
        None,
        &[importing, THEME],
    );
    assert_eq!(heading_size(&world), pt(8.0));
    let world = project(
        &["styles/theme.fsty", "styles/a.fsty"],
        None,
        &[importing, THEME],
    );
    assert_eq!(heading_size(&world), pt(30.0));

    // Rules follow the same origin as tokens: the theme's rule, at the
    // project origin and later, wins over `a.fsty`'s.
    let world = project(
        &["styles/a.fsty", "styles/theme.fsty"],
        None,
        &[
            importing,
            ("styles/theme.fsty", "heading { font-size: 9pt; }\n"),
        ],
    );
    assert_eq!(heading_size(&world), pt(9.0));

    // A variant overlay that the project also imports is read once, as the
    // overlay, above the project.
    let world = project(
        &["styles/a.fsty"],
        Some("styles/exam.fsty"),
        &[
            (
                "styles/a.fsty",
                "@import \"exam.fsty\";\n$size: 30pt;\nheading { font-size: $size; }\n",
            ),
            ("styles/exam.fsty", "$size: 12pt;\n"),
        ],
    );
    assert_eq!(heading_size(&world), pt(12.0));
}
