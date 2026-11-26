//! Quantities, units and numbers (SPEC §8.1): `qty`, `unit` and `num` in
//! equations and in text evaluate to quantity nodes of the math tree, and
//! the style stage gives each formula the format its numbers are written
//! in: the decimal marker of its language, or of `decimal-marker`.
//!
//! How the nodes are set in that format is tested in
//! `crates/folio-math/tests/quantity.rs`.

use folio::eval::{Content, Node, Value};
use folio::style::{NodeName, Styled, StyledNode};
use folio::{Lang, Severity, Target, World, diag};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
styles = [\"main.fsty\"]
";

fn world(main: &str, sheet: &str) -> World {
    let mut world = World::builder().build(MANIFEST, None).unwrap();
    world.set_source("main.fol", main).unwrap();
    world.set_source("main.fsty", sheet).unwrap();
    world
}

/// The formulas of the equations in `main`, in order, as
/// `folio_math::Formula::repr` writes them.
fn formulas(main: &str) -> Vec<String> {
    fn walk(content: &Content, out: &mut Vec<String>) {
        for node in content.nodes() {
            let Node::Element(element) = node else {
                continue;
            };
            if element.name.to_string() == "math"
                && let Some(Value::Math(formula)) = element.field("tree")
            {
                out.push(formula.repr());
            }
            for (_, value) in element.fields.iter() {
                if let Value::Content(inner) = value {
                    walk(inner, out);
                }
            }
        }
    }
    let world = world(main, "");
    let errors = errors_in(&world);
    assert!(errors.is_empty(), "{main}: {errors:?}");
    let content = folio::evaluate(&world, None).unwrap().unwrap();
    let mut out = Vec::new();
    walk(&content, &mut out);
    out
}

/// The errors of a build, each with the source text its span points at.
fn errors_in(world: &World) -> Vec<(String, String)> {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    let main = world.source("main.fol").unwrap();
    compiled
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| {
            let at = diag::locate(world, d.span)
                .map(|location| main.text()[location.bytes].to_string())
                .unwrap_or_default();
            (d.message.to_string(), at)
        })
        .collect()
}

fn errors(main: &str) -> Vec<(String, String)> {
    errors_in(&world(main, ""))
}

fn error(message: &str, at: &str) -> (String, String) {
    (message.to_string(), at.to_string())
}

#[test]
fn quantities_in_equations() {
    assert_eq!(
        formulas(
            "$g = qty(9.81, \"m s^-2\")$ $qty(2.31, \"m\", pm: 0.02)$ $num(6.02e23)$ \
             $unit(\"kg m^2 s^-1\")$ $qty(-1.60e-19, \"C\")$ $qty(9.80, \"N kg^-1\")$"
        ),
        [
            "[g, rel(=), qty(9.81, unit: \"m s^-2\")]",
            "[qty(2.31, pm: 0.02, unit: \"m\")]",
            "[qty(6.02e23)]",
            "[qty(unit: \"kg m^2 s^-1\")]",
            // The digits are kept as written: `9.80` has three figures.
            "[qty(-1.60e-19, unit: \"C\")]",
            "[qty(9.80, unit: \"N kg^-1\")]",
        ]
    );
}

#[test]
fn values_from_scripts_and_rounding() {
    assert_eq!(
        formulas(
            "#let r = 14.679\n\
             #let double(x) = 2 * x\n\
             $qty(#r, \"m\", digits: 3)$ $qty(double(7.3395), \"m\", digits: 3)$ \
             $qty(#(1/3), \"s\", digits: 2)$ $num(\"0.50\")$ $num(9.996, digits: 3)$"
        ),
        [
            "[qty(14.7, unit: \"m\")]",
            "[qty(14.7, unit: \"m\")]",
            "[qty(0.33, unit: \"s\")]",
            "[qty(0.50)]",
            "[qty(10.0)]",
        ]
    );
}

#[test]
fn quantities_in_text() {
    assert_eq!(
        formulas(
            "Take #qty(9.81, \"m s^-2\") and #qty(\"9.80\", \"m\", pm: 0.05). \
             #num(6.02e23) #unit(\"ohm\") #num(2/3, digits: 2)"
        ),
        [
            "[qty(9.81, unit: \"m s^-2\")]",
            "[qty(9.80, pm: 0.05, unit: \"m\")]",
            "[qty(6.02e23)]",
            "[qty(unit: \"Ω\")]",
            "[qty(0.67)]",
        ]
    );
}

#[test]
fn quantity_errors() {
    assert_eq!(
        errors("$qty(9.81)$ $unit()$ $num(1, 2)$"),
        [
            error("`qty` takes a value and a unit", "qty(9.81)"),
            error("`unit` takes a unit", "unit()"),
            error("`num` takes a value", "num(1, 2)"),
        ]
    );
    assert_eq!(
        errors("$qty(x, \"m\")$ $qty(1e99999, \"m\")$"),
        [
            error("expected a number", "x"),
            error(
                "a power of ten can be at most 9999 in either direction",
                "1e99999"
            ),
        ]
    );
    // A unit's error points at the factor it is about.
    assert_eq!(
        errors("$qty(3, \"m/s\")$ $qty(3, \"kg m^x\")$ $qty(3, 5)$"),
        [
            error("`/` cannot be part of a unit", "m/s"),
            error("the power of a unit is an integer", "m^x"),
            error("expected a unit, found an int", "5"),
        ]
    );
    assert_eq!(
        errors("$qty(2, \"m\", pm: -0.1)$ $num(2, digits: 0)$ $num(2, place: 1)$"),
        [
            error("an uncertainty cannot be negative", "-0.1"),
            error("0 is not a number of significant figures", "0"),
            error("`num` has no argument `place`", "place: 1"),
        ]
    );
    assert_eq!(
        errors("#qty(\"x\", \"m\") #unit(3) #num(true)"),
        [
            error("`x` is not a number", "\"x\""),
            error("expected a unit, found an int", "3"),
            error("expected a number, found a bool", "true"),
        ]
    );
}

/// The math nodes of a build's styled tree.
fn math_nodes(styled: &Styled) -> Vec<&StyledNode> {
    styled
        .nodes()
        .into_iter()
        .filter(|n| matches!(&n.name, NodeName::Element(name) if name.to_string() == "math"))
        .collect()
}

/// The decimal marker each formula's numbers are written with.
fn markers(main: &str, sheet: &str) -> Vec<char> {
    let world = world(main, sheet);
    assert!(errors_in(&world).is_empty());
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    math_nodes(&styled)
        .into_iter()
        .map(|node| {
            let lang = match styled.value(node, "lang") {
                Some(folio_component::Value::Str(tag)) => Lang::new(tag).unwrap(),
                _ => Lang::english(),
            };
            let format =
                folio_core::locale::number_format(&lang, styled.value(node, "decimal-marker"));
            format.decimal_marker
        })
        .collect()
}

#[test]
fn decimal_marker_by_language_and_stylesheet() {
    let main = "$qty(9.81, \"m\")$\n\n#emph[#qty(9.81, \"m\")]";
    assert_eq!(markers(main, ""), ['.', '.']);
    assert_eq!(markers(main, "doc { lang: \"pl\"; }"), [',', ',']);
    assert_eq!(markers(main, "emph { lang: \"pl\"; }"), ['.', ',']);
    assert_eq!(
        markers(
            main,
            "doc { lang: \"pl\"; } emph { decimal-marker: point; }"
        ),
        [',', '.']
    );
    assert_eq!(markers(main, "math { decimal-marker: comma; }"), [',', ',']);
}
