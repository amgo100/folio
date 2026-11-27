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
