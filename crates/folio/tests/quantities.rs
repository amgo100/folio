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
    // A carry moves a power of ten along: never `10 × 10³`.
    assert_eq!(
        formulas("$num(9.99e3, digits: 2)$ $num(-0)$"),
        ["[qty(1.0e4)]", "[qty(0)]"]
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

/// A float written as the argument of `qty` or `num` in text keeps its
/// digits and its power of ten, as it would in an equation (D-324-1); a
/// float computed or bound to a name is written in the shorter of its plain
/// and power-of-ten forms (D-324-2).
#[test]
fn floats_in_text_as_written() {
    assert_eq!(
        formulas(
            "#qty(3e8, \"m s^-1\") #qty(3.00e8, \"m s^-1\") #qty(300000000.0, \"m s^-1\") \
             #qty(2.0, \"m\") #qty(-1.6e-19, \"C\") #qty(1e8, \"m\", pm: 1e6) \
             #num(+1.5e3)"
        ),
        [
            "[qty(3e8, unit: \"m s^-1\")]",
            "[qty(3.00e8, unit: \"m s^-1\")]",
            "[qty(300000000.0, unit: \"m s^-1\")]",
            "[qty(2.0, unit: \"m\")]",
            "[qty(-1.6e-19, unit: \"C\")]",
            "[qty(1e8, pm: 1e6, unit: \"m\")]",
            "[qty(1.5e3)]",
        ]
    );
    // A script's function hands the spelling on, in text and in an
    // equation alike, and code after `#` in an equation keeps it too.
    assert_eq!(
        formulas(
            "#let speed(..a) = qty(..a, \"m s^-1\")\n\
             #speed(3.00e8) $c = speed(#3.00e8)$ $qty(#3.00e8, \"m\")$ \
             #qty(2.31, \"m\", pm: 0.020)"
        ),
        [
            "[qty(3.00e8, unit: \"m s^-1\")]",
            "[c, rel(=), qty(3.00e8, unit: \"m s^-1\")]",
            "[qty(3.00e8, unit: \"m\")]",
            "[qty(2.31, pm: 0.020, unit: \"m\")]",
        ]
    );
    // Text matches the same call in an equation.
    assert_eq!(
        formulas("$qty(3.00e8, \"m s^-1\")$ $qty(2.0, \"m\")$"),
        ["[qty(3.00e8, unit: \"m s^-1\")]", "[qty(2.0, unit: \"m\")]",]
    );
    assert_eq!(
        formulas(
            "#let c = 3.00e8\n\
             #qty(c, \"m s^-1\") #qty(2.0 * 1.5e8, \"m\") #qty(0.5 * 3000.0, \"J\") \
             #qty(1.2e5 * 1.0, \"Pa\") #num(-(2.0)) $qty(#(0.25 * 0.004), \"m\")$"
        ),
        [
            "[qty(3e8, unit: \"m s^-1\")]",
            "[qty(3e8, unit: \"m\")]",
            "[qty(1500, unit: \"J\")]",
            "[qty(1.2e5, unit: \"Pa\")]",
            "[qty(-2)]",
            "[qty(1e-3, unit: \"m\")]",
        ]
    );
}

/// An SI prefix before `ohm` names the prefixed unit; a symbol that ends in
/// a name but cannot be read is an error at that factor.
#[test]
fn prefixed_unit_names() {
    assert_eq!(
        formulas(
            "#qty(4.7, \"kohm\") #qty(2.2, \"Mohm\") $qty(15, \"mohm\")$ #unit(\"microohm m\")"
        ),
        [
            "[qty(4.7, unit: \"kΩ\")]",
            "[qty(2.2, unit: \"MΩ\")]",
            "[qty(15, unit: \"mΩ\")]",
            "[qty(unit: \"µΩ m\")]",
        ]
    );
    assert_eq!(
        errors("#qty(4.7, \"kiloohm\") $qty(20, \"m kdegC\")$"),
        [
            // In text the error points at the whole string, as for any unit.
            error("`kilo` is not an SI prefix of `ohm`", "\"kiloohm\""),
            error("`degC` takes no prefix, so `kdegC` is not a unit", "kdegC"),
        ]
    );
    // A literal that cannot be read says why, in text as in an equation.
    assert_eq!(
        errors(
            "#let len(..a) = qty(..a, \"m\")\n\
             #qty(1e10000, \"m\") $qty(1e10000, \"m\")$ $len(#1e10000)$ \
             #qty(2.0, \"m\", pm: -0.10, pm@markscheme: 0.2)"
        ),
        [
            error(
                "a power of ten can be at most 9999 in either direction",
                "1e10000"
            ),
            error(
                "a power of ten can be at most 9999 in either direction",
                "1e10000"
            ),
            error(
                "a power of ten can be at most 9999 in either direction",
                "1e10000"
            ),
            error("an uncertainty cannot be negative", "pm: -0.10"),
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
        errors("#let k = 3\n$qty(2k, \"m\")$ #qty(9.81, \"m\", pm: -0.02) #num(2, digits: 0.5)"),
        [
            error("expected a number", "2k"),
            error("an uncertainty cannot be negative", "pm: -0.02"),
            error("expected an integer, found a float", "digits: 0.5"),
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

/// The `math-style` of each formula's vectors, or `None` for a formula
/// without vectors.
fn vector_styles(main: &str, sheet: &str) -> Vec<Option<String>> {
    let world = world(main, sheet);
    assert!(errors_in(&world).is_empty());
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    math_nodes(&styled)
        .into_iter()
        .map(|node| {
            let vectors = node.vector_style()?;
            assert_eq!(vectors.location, node.location);
            Some(match styled.value(vectors, "math-style") {
                Some(value) => format!("{value:?}"),
                None => "none".into(),
            })
        })
        .collect()
}

#[test]
fn stylesheets_choose_bold_or_arrowed_vectors() {
    let main = "$vec(F) = m vec(a)$ $x^2$\n\n#emph[$hat(i)$ and $1/2 vec(v)^2$]";
    let auto = || Some("Auto".to_string());
    let kw = |word: &str| Some(format!("Keyword({word:?})"));
    assert_eq!(vector_styles(main, ""), [auto(), None, auto(), auto()]);
    assert_eq!(
        vector_styles(main, "math.vector { math-style: arrow; }"),
        [kw("arrow"), None, kw("arrow"), kw("arrow")]
    );
    // Per part of the document, and inherited from the formula.
    assert_eq!(
        vector_styles(
            main,
            "math.vector { math-style: arrow; } emph math.vector { math-style: bold-italic; }"
        ),
        [kw("arrow"), None, kw("bold-italic"), kw("bold-italic")]
    );
    assert_eq!(
        vector_styles(main, "emph { math-style: arrow; }"),
        [auto(), None, kw("arrow"), kw("arrow")]
    );
    // The class styles the vectors, not the formula.
    let world = world(main, "math.vector { math-style: arrow; }");
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    for node in math_nodes(&styled) {
        assert_eq!(
            styled.value(node, "math-style"),
            Some(&folio_component::Value::Auto)
        );
    }
}

/// A comma written between two integers without spaces is kept as one the
/// layout of the formula's text decides on (D-349-1).
#[test]
fn commas_between_numbers() {
    assert_eq!(
        formulas("$9,81$ $9, 81$ $1,2,3$ $1,5, 2,5$ $(0,5)$ $9,81^2$ $x,1$ $1.5,2$ $1,2^2,3$"),
        [
            "[num(9), num(,), num(81)]",
            "[num(9), punct(,), num(81)]",
            // Three or more integers joined so are a list.
            "[num(1), punct(,), num(2), punct(,), num(3)]",
            "[num(1), num(,), num(5), punct(,), num(2), num(,), num(5)]",
            "[delim('(', [num(0), num(,), num(5)], ')')]",
            "[num(9), num(,), attach([num(81)], sup: [num(2)])]",
            "[x, punct(,), num(1)]",
            "[num(1.5), punct(,), num(2)]",
            "[num(1), punct(,), attach([num(2)], sup: [num(2)]), punct(,), num(3)]",
        ]
    );
}

/// With an uncertainty, `digits` is the uncertainty's significant figures,
/// and the value is rounded to the place of its last one (D-349-2).
#[test]
fn digits_round_the_uncertainty_and_the_value_follows() {
    assert_eq!(
        formulas(
            "#qty(1.5, \"m\", pm: 0.25, digits: 1) #qty(12.345, \"m\", pm: 0.021, digits: 2) \
             $qty(9.8123, \"m s^-2\", pm: 0.0456, digits: 1)$ $num(1234.5, pm: 56, digits: 1)$ \
             $qty(3.0041e8, \"m s^-1\", pm: 2.6e5, digits: 2)$ $num(0.4, pm: 3, digits: 1)$ \
             $num(2.71828, pm: 0.096, digits: 1)$ $num(5, pm: 0.02, digits: 2)$"
        ),
        [
            "[qty(1.5, pm: 0.3, unit: \"m\")]",
            "[qty(12.345, pm: 0.021, unit: \"m\")]",
            "[qty(9.81, pm: 0.05, unit: \"m s^-2\")]",
            "[qty(1230, pm: 60)]",
            "[qty(3.0041e8, pm: 2.6e5, unit: \"m s^-1\")]",
            "[qty(0, pm: 3)]",
            // The carry makes `0.096` `0.1`: the value follows to tenths.
            "[qty(2.7, pm: 0.1)]",
            // Padded to the uncertainty's place.
            "[qty(5.000, pm: 0.020)]",
        ]
    );
    // A value that rounds to zero needs no power of ten.
    assert_eq!(
        formulas("$num(1.5e3, pm: 2e4, digits: 1)$"),
        ["[qty(0, pm: 2e4)]"]
    );
    // Without an uncertainty, `digits` rounds the value as before.
    assert_eq!(formulas("$num(1.25, digits: 1)$"), ["[qty(1)]"]);
}
