//! Quantities as they are set in English and in Polish (SPEC §8.1): the
//! nodes [`Quantity::lower`] makes in each language's [`NumberFormat`].
//! Thin spaces are written `_` and the narrow no-break spaces that group
//! digits `~`, so that the snapshots show them.

use folio_math::quantity::{GROUP_SEPARATOR, THIN_SPACE};
use folio_math::{Number, NumberFormat, Quantity, Unit};
use folio_syntax::Span;

fn quantity(value: Option<&str>, pm: Option<&str>, unit: Option<&str>) -> Quantity {
    Quantity {
        value: value.map(|v| Number::parse(v).unwrap()),
        pm: pm.map(|v| Number::parse(v).unwrap()),
        unit: unit.map(|u| Unit::parse(u).unwrap()),
        span: Span::detached(),
    }
}

fn set(q: &Quantity, format: &NumberFormat) -> String {
    q.lower(format)
        .repr()
        .replace(THIN_SPACE, "_")
        .replace(GROUP_SEPARATOR, "~")
}

fn en(q: &Quantity) -> String {
    set(q, &NumberFormat::POINT)
}

fn pl(q: &Quantity) -> String {
    set(q, &NumberFormat::COMMA)
}

#[test]
fn quantity_with_a_unit() {
    let g = quantity(Some("9.81"), None, Some("m s^-2"));
    assert_eq!(
        en(&g),
        "[num(9.81), space(_), sym(m), space(_), attach([sym(s)], sup: [vary(−), num(2)])]"
    );
    assert_eq!(
        pl(&g),
        "[num(9,81), space(_), sym(m), space(_), attach([sym(s)], sup: [vary(−), num(2)])]"
    );
}

#[test]
fn units_alone() {
    let unit = quantity(None, None, Some("kg m^2 s^-1"));
    assert_eq!(
        en(&unit),
        "[sym(kg), space(_), attach([sym(m)], sup: [num(2)]), space(_), \
         attach([sym(s)], sup: [vary(−), num(1)])]"
    );
    assert_eq!(en(&unit), pl(&unit));
}

#[test]
fn numbers_with_powers_of_ten() {
    let avogadro = quantity(Some("6.02e23"), None, None);
    assert_eq!(
        en(&avogadro),
        "[num(6.02), bin(×), attach([num(10)], sup: [num(23)])]"
    );
    assert_eq!(
        pl(&avogadro),
        "[num(6,02), bin(×), attach([num(10)], sup: [num(23)])]"
    );
    let small = quantity(Some("-1.6e-19"), None, Some("C"));
    assert_eq!(
        pl(&small),
        "[vary(−), num(1,6), bin(×), attach([num(10)], sup: [vary(−), num(19)]), space(_), \
         sym(C)]"
    );
}

#[test]
fn uncertainties() {
    let length = quantity(Some("2.31"), Some("0.02"), Some("m"));
    assert_eq!(
        en(&length),
        "[delim('(', [num(2.31), vary(±), num(0.02)], ')'), space(_), sym(m)]"
    );
    assert_eq!(
        pl(&length),
        "[delim('(', [num(2,31), vary(±), num(0,02)], ')'), space(_), sym(m)]"
    );
    // Without a unit, no parentheses.
    assert_eq!(
        pl(&quantity(Some("2.31"), Some("0.02"), None)),
        "[num(2,31), vary(±), num(0,02)]"
    );
    // A shared power of ten is written once.
    assert_eq!(
        en(&quantity(Some("2.31e3"), Some("0.02e3"), Some("m"))),
        "[delim('(', [num(2.31), vary(±), num(0.02)], ')'), bin(×), \
         attach([num(10)], sup: [num(3)]), space(_), sym(m)]"
    );
}

#[test]
fn digit_groups_from_five_digits() {
    assert_eq!(
        en(&quantity(Some("1500"), None, Some("J"))),
        "[num(1500), space(_), sym(J)]"
    );
    assert_eq!(
        en(&quantity(Some("299792458"), None, Some("m s^-1"))),
        "[num(299~792~458), space(_), sym(m), space(_), \
         attach([sym(s)], sup: [vary(−), num(1)])]"
    );
    assert_eq!(
        pl(&quantity(Some("12345.67891"), None, None)),
        "[num(12~345,678~91)]"
    );
    assert_eq!(pl(&quantity(Some("0.1234"), None, None)), "[num(0,1234)]");
}

#[test]
fn angles_follow_their_number() {
    assert_eq!(
        en(&quantity(Some("30"), None, Some("deg"))),
        "[num(30), sym(°)]"
    );
    assert_eq!(
        pl(&quantity(Some("20.5"), None, Some("degC"))),
        "[num(20,5), space(_), sym(°C)]"
    );
}

#[test]
fn rounding_to_a_decimal_place() {
    let round = |n: &str, place: i64| Number::parse(n).unwrap().round_to_place(place).to_string();
    assert_eq!(round("12.3456", -3), "12.346");
    assert_eq!(round("1.5", -1), "1.5");
    assert_eq!(round("0.2", -3), "0.200");
    assert_eq!(round("1234", 2), "1200");
    assert_eq!(round("999", 1), "1000");
    assert_eq!(round("3.004e8", 6), "3.00e8");
    assert_eq!(round("9.996e3", 1), "1.000e4");
    assert_eq!(round("99e3", 4), "10e4");
    assert_eq!(round("0.4", 0), "0");
    assert_eq!(round("0.6", 0), "1");
    assert_eq!(round("-0.04", -1), "0.0");
    assert_eq!(round("-1.25", -1), "-1.3");
    assert_eq!(round("12", 5), "0");
    // However far the place, the number keeps within its digits.
    assert!(round("1", -1_000_000).len() <= folio_math::quantity::MAX_DIGITS + 1);
    assert_eq!(round("5", 1_000_000), "0");
}

#[test]
fn places_of_significant_figures() {
    let place = |n: &str, figure: u32| Number::parse(n).unwrap().figure_place(figure);
    assert_eq!(place("9.81", 3), -2);
    assert_eq!(place("60", 1), 1);
    assert_eq!(place("2.6e5", 2), 4);
    assert_eq!(place("0.020", 2), -3);
    assert_eq!(place("0.00", 3), -2);
}

/// Numbers written in a formula, in each format: `9.81` and the comma of
/// `9,81` written without spaces, and how the formula reads.
#[test]
fn numbers_in_formulas_read_in_their_format() {
    use folio_math::{Formula, Node, Row};
    let span = Span::detached();
    let formula = Formula::row(
        Row::new([
            Node::number("9".into(), span),
            Node::decimal_comma(span),
            Node::number("81".into(), span),
            Node::symbol('+', span),
            Node::number("12345.678901".into(), span),
        ]),
        span,
    );
    assert_eq!(
        formula.reading(&NumberFormat::POINT),
        "9, 81 + 12345.678901"
    );
    assert_eq!(formula.reading(&NumberFormat::COMMA), "9,81 + 12345,678901");
}
