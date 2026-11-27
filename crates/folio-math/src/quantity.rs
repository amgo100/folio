//! Quantities, units and numbers (SPEC §8.1): `qty(9.81, "m s^-2")`,
//! `unit("kg m^2 s^-1")`, `num(6.02e23)`, with uncertainties
//! (`qty(2.31, "m", pm: 0.02)`) and rounding to significant figures
//! (`digits: 3`).
//!
//! A [`Quantity`] is semantic: it holds its number as decimal digits,
//! exactly as written or rounded, and its unit as factors with powers. How
//! it is written out depends on the language of the text it stands in (the
//! decimal marker is a point in English and a comma in Polish) and on the
//! stylesheet, so the tree keeps it whole, and math layout and the Office
//! writers turn it into ordinary nodes with [`Quantity::lower`] once they
//! know the [`NumberFormat`].
//!
//! The written form follows the SI brochure and ISO 80000-1:
//!
//! - digits are grouped in threes, counted from the decimal marker, on each
//!   side that has at least [`NumberFormat::group_min_digits`] digits (five
//!   by default, so `1500` stays whole and `15 000` is grouped); the
//!   separator is a narrow no-break space (U+202F), so a number never
//!   breaks across lines;
//! - a power of ten is written `× 10ⁿ`;
//! - an uncertainty follows the value after `±`, and the two are put in
//!   parentheses when a unit or a shared power of ten follows:
//!   `(2.31 ± 0.02) m`;
//! - units are upright symbols, not words, and are separated from the
//!   number and from each other by a thin space (U+2009) and nothing
//!   more, except that the degree, minute and second of plane angle
//!   follow the number directly (`30°`); negative powers stay powers
//!   (`m s⁻²`), as written.

use std::fmt::{self, Debug, Display, Formatter, Write};
use std::sync::Arc;

use ecow::{EcoString, EcoVec};
use folio_syntax::Span;

use crate::{Atom, Attach, Delimited, MathClass, Node, Row};

/// The minus sign (U+2212) that negative numbers and powers are written
/// with.
pub const MINUS: char = '−';

/// The separator of digit groups: a narrow no-break space (U+202F).
pub const GROUP_SEPARATOR: char = '\u{202F}';

/// The space between a number and its unit, and between the factors of a
/// unit: a thin space (U+2009).
pub const THIN_SPACE: char = '\u{2009}';

/// The largest power of ten a number may be written with, in either
/// direction.
pub const MAX_EXPONENT: i32 = 9999;

/// The largest power a unit may be raised to, in either direction.
pub const MAX_UNIT_POWER: i32 = 99;

/// The most digits a number may have, before and after the marker together.
pub const MAX_DIGITS: usize = 1000;

/// A quantity, a unit alone or a number alone: what `qty`, `unit` and
/// `num` make.
///
/// `qty(9.81, "m s^-2")` has a value and a unit, `unit("kg")` a unit only,
/// and `num(6.02e23)` a value only; any with a value may have an
/// uncertainty. It is a leaf of the tree: [`Quantity::lower`] turns it into
/// the nodes it is set as.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Quantity {
    /// The value, as written or rounded: `9.81`.
    pub value: Option<Number>,
    /// The uncertainty that follows the value after `±`: `0.02`.
    pub pm: Option<Number>,
    /// The unit: `m s^-2`.
    pub unit: Option<Unit>,
    /// Where it was written.
    pub span: Span,
}

impl Quantity {
    /// The nodes the quantity is set as, in a format: numbers with their
    /// digits grouped and the format's decimal marker, a power of ten as
    /// `× 10ⁿ`, an uncertainty after `±`, and upright units after thin
    /// spaces (see the module documentation). Every node has the
    /// quantity's span.
    pub fn lower(&self, format: &NumberFormat) -> Row {
        let span = self.span;
        let mut out = Vec::new();
        if let Some(value) = &self.value {
            match &self.pm {
                None => value.lower(format, span, &mut out),
                Some(pm) => {
                    // A power of ten that both share is written once,
                    // after the parentheses.
                    let shared = value.exponent.filter(|_| value.exponent == pm.exponent);
                    let mut inner = Vec::new();
                    let strip = |n: &Number| Number {
                        exponent: if shared.is_some() { None } else { n.exponent },
                        ..n.clone()
                    };
                    strip(value).lower(format, span, &mut inner);
                    inner.push(Node::symbol('±', span));
                    strip(pm).lower(format, span, &mut inner);
                    if shared.is_some() || self.unit.is_some() {
                        out.push(Node::Delimited(Arc::new(Delimited {
                            open: Some('('),
                            body: Row::new(inner),
                            close: Some(')'),
                            span,
                        })));
                    } else {
                        out.extend(inner);
                    }
                    if let Some(exponent) = shared {
                        power_of_ten(exponent, span, &mut out);
                    }
                }
            }
        }
        if let Some(unit) = &self.unit {
            if self.value.is_some() && !unit.is_angle() {
                out.push(thin_space(span));
            }
            unit.lower(span, &mut out);
        }
        Row::new(out)
    }

    /// A compact description, as [`Formula::repr`](crate::Formula::repr)
    /// writes it: `qty(9.81, pm: 0.02, unit: "m s^-2")`.
    pub(crate) fn write_repr(&self, out: &mut String) {
        out.push_str("qty(");
        let mut first = true;
        let mut sep = |out: &mut String| {
            if !first {
                out.push_str(", ");
            }
            first = false;
        };
        if let Some(value) = &self.value {
            sep(out);
            write!(out, "{value}").unwrap();
        }
        if let Some(pm) = &self.pm {
            sep(out);
            write!(out, "pm: {pm}").unwrap();
        }
        if let Some(unit) = &self.unit {
            sep(out);
            write!(out, "unit: \"{unit}\"").unwrap();
        }
        out.push(')');
    }
}

/// How numbers are written: the decimal marker and the grouping of digits.
/// It depends on the language of the text a number stands in and on the
/// stylesheet; math layout builds it from the computed style.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct NumberFormat {
    /// The decimal marker: `.` in English, `,` in Polish.
    pub decimal_marker: char,
    /// The separator of digit groups.
    pub group_separator: char,
    /// The fewest digits on one side of the marker that are grouped: five,
    /// so that `1500` and `0.1234` stay whole and `15 000` is grouped, as
    /// the SI brochure and ISO 80000-1 do.
    pub group_min_digits: u8,
}

impl NumberFormat {
    /// The format with a decimal point.
    pub const POINT: Self = Self::with_marker('.');

    /// The format with a decimal comma.
    pub const COMMA: Self = Self::with_marker(',');

    /// The format with a decimal marker and the default grouping.
    pub const fn with_marker(decimal_marker: char) -> Self {
        Self {
            decimal_marker,
            group_separator: GROUP_SEPARATOR,
            group_min_digits: 5,
        }
    }
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self::POINT
    }
}

/// A decimal number: a sign, the digits before and after the decimal
/// marker, and a power of ten. It keeps the digits as written, so that
/// `9.80` keeps its last zero, a significant figure.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Number {
    /// Whether it is negative.
    pub negative: bool,
    /// The digits before the decimal marker, without leading zeros, or
    /// `0`.
    pub int: EcoString,
    /// The digits after the decimal marker, as written; empty for an
    /// integer.
    pub frac: EcoString,
    /// The power of ten it is multiplied by, if it was written with one:
    /// `23` of `6.02e23`.
    pub exponent: Option<i32>,
}

impl Number {
    /// Reads a decimal literal: an optional sign (`-`, `−` or `+`), digits
    /// with an optional decimal point, and an optional exponent after `e`
    /// or `E`: `9.81`, `-0.5`, `6.02e23`, `1E-3`.
    pub fn parse(text: &str) -> Result<Self, EcoString> {
        let invalid = || EcoString::from(format!("`{text}` is not a number"));
        let mut rest = text.trim();
        let mut negative = false;
        if let Some(r) = rest.strip_prefix(['-', MINUS]) {
            negative = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('+') {
            rest = r;
        }
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !all_digits(int) || !all_digits(frac) || (int.is_empty() && frac.is_empty()) {
            return Err(invalid());
        }
        if int.len() + frac.len() > MAX_DIGITS {
            return Err(format!("a number can have at most {MAX_DIGITS} digits").into());
        }
        let exponent = match exponent {
            None => None,
            Some(exp) => {
                let (negative, digits) = match exp.strip_prefix(['-', MINUS]) {
                    Some(d) => (true, d),
                    None => (false, exp.strip_prefix('+').unwrap_or(exp)),
                };
                if digits.is_empty() || !all_digits(digits) {
                    return Err(invalid());
                }
                let magnitude = digits
                    .trim_start_matches('0')
                    .parse::<i32>()
                    .ok()
                    .or_else(|| digits.bytes().all(|b| b == b'0').then_some(0))
                    .filter(|&m| m <= MAX_EXPONENT)
                    .ok_or_else(|| {
                        EcoString::from(format!(
                            "a power of ten can be at most {MAX_EXPONENT} in either direction"
                        ))
                    })?;
                Some(if negative { -magnitude } else { magnitude })
            }
        };
        let int = int.trim_start_matches('0');
        // Zero has no sign: `-0` is `0`.
        let zero = int.is_empty() && frac.bytes().all(|b| b == b'0');
        Ok(Self {
            negative: negative && !zero,
            int: if int.is_empty() {
                "0".into()
            } else {
                int.into()
            },
            frac: frac.into(),
            exponent,
        })
    }

    /// An integer.
    pub fn from_int(value: i64) -> Self {
        Self {
            negative: value < 0,
            int: value.unsigned_abs().to_string().into(),
            frac: EcoString::new(),
            exponent: None,
        }
    }

    /// A float that has no written form (computed by a script, or bound to
    /// a name), in the shortest digits that read back as the same float,
    /// written plain or with a power of ten, whichever literal is shorter,
    /// plain on a tie (D-324-2): `3e8` (not `300000000`), `1500` (not
    /// `1.5e3`), `1.2e5`, `0.05`, `1e-3`, `1e3`. A whole float has no
    /// decimals: the `.0` of Rust's form is a figure nobody wrote. `None`
    /// for NaN and the infinities.
    pub fn from_float(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        // Rust writes the shortest digits that read back as the same
        // float, one before the point: `-1.5e3`.
        let scientific = format!("{:e}", value.abs());
        let (mantissa, exponent) = scientific.split_once('e')?;
        let exponent: i64 = exponent.parse().ok()?;
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        // Where the decimal point falls in `digits` when written plain.
        let point = 1 + exponent;
        let plain = if value == 0.0 {
            "0".to_string()
        } else if point >= digits.len() as i64 {
            let zeros = usize::try_from(point).ok()? - digits.len();
            format!("{digits}{}", "0".repeat(zeros))
        } else if point <= 0 {
            format!("0.{}{digits}", "0".repeat(usize::try_from(-point).ok()?))
        } else {
            let (int, frac) = digits.split_at(usize::try_from(point).ok()?);
            format!("{int}.{frac}")
        };
        let text = if scientific.len() < plain.len() {
            &scientific
        } else {
            &plain
        };
        let mut number = Self::parse(text).ok()?;
        number.negative = value < 0.0;
        Some(number)
    }

    /// Whether every digit is zero.
    pub fn is_zero(&self) -> bool {
        self.int.bytes().chain(self.frac.bytes()).all(|b| b == b'0')
    }

    /// The number rounded to `figures` significant figures, half away from
    /// zero, on its decimal digits: `14.679` to 3 is `14.7`, `9.8` to 3 is
    /// `9.80`, `0.09996` to 2 is `0.10`. Digits before the marker that fall
    /// beyond the figures become zeros (`123456` to 3 is `123000`). A
    /// number with a power of ten stays in that form, and one whose
    /// mantissa a carry made one digit longer moves the marker and the
    /// power with it: `9.99e3` to 2 is `1.0e4`. Zero has no significant figures, so it
    /// keeps `figures - 1` decimals: `0` to 3 is `0.00`.
    pub fn round(&self, figures: u32) -> Self {
        let figures = (figures.max(1) as usize).min(MAX_DIGITS);
        let int_len = self.int.len();
        let mut all: Vec<u8> = self.int.bytes().chain(self.frac.bytes()).collect();
        let Some(first) = all.iter().position(|&b| b != b'0') else {
            return Self {
                negative: false,
                int: "0".into(),
                frac: "0".repeat(figures - 1).into(),
                exponent: self.exponent,
            };
        };
        let cut = first + figures;
        let (mut kept, mut int_len) = if cut >= all.len() {
            // Pad with zeros after the marker.
            all.resize(cut.max(int_len), b'0');
            (all, int_len)
        } else {
            let up = all[cut] >= b'5';
            let mut kept = all[..cut].to_vec();
            let mut int_len = int_len;
            if up {
                let mut i = kept.len();
                loop {
                    if i == 0 {
                        kept.insert(0, b'1');
                        int_len += 1;
                        break;
                    }
                    i -= 1;
                    if kept[i] == b'9' {
                        kept[i] = b'0';
                    } else {
                        kept[i] += 1;
                        break;
                    }
                }
                // A carry into a new leading digit adds a figure: drop the
                // last one if it is a decimal.
                let new_first = kept.iter().position(|&b| b != b'0').unwrap_or(0);
                let old_first = first + (int_len - self.int.len());
                if new_first < old_first && kept.len() > int_len {
                    kept.pop();
                }
            }
            // Digits before the marker that were cut off become zeros.
            if kept.len() < int_len {
                kept.resize(int_len, b'0');
            }
            (kept, int_len)
        };
        let frac = kept.split_off(int_len);
        // Leading zeros before the marker go, but one stays.
        let lead = kept.iter().take_while(|&&b| b == b'0').count();
        let lead = lead.min(kept.len().saturating_sub(1));
        kept.drain(..lead);
        if kept.is_empty() {
            kept.push(b'0');
        }
        int_len = kept.len();
        debug_assert!(int_len > 0);
        let text = |digits: Vec<u8>| EcoString::from(String::from_utf8(digits).expect("digits"));
        let mut frac = frac;
        let mut exponent = self.exponent;
        if let Some(power) = exponent
            && kept.len() > self.int.len()
            && power < MAX_EXPONENT
        {
            // A carry made the mantissa longer: `10e3` is `1.0e4`. A zero
            // that only holds a place is dropped, not made a decimal:
            // `999999e3` to one figure is `100000e4`.
            let last = kept.pop().expect("a longer mantissa has digits");
            if !(frac.is_empty() && last == b'0' && kept.len() >= figures) {
                frac.insert(0, last);
            }
            exponent = Some(power + 1);
        }
        let number = Self {
            negative: self.negative,
            int: text(kept),
            frac: text(frac),
            exponent,
        };
        Self {
            negative: number.negative && !number.is_zero(),
            ..number
        }
    }

    /// The mantissa as it is written in a format: `12 345.678`, `9,81`,
    /// without the sign and the power of ten.
    pub fn format_mantissa(&self, format: &NumberFormat) -> EcoString {
        let mut out = EcoString::new();
        let min = format.group_min_digits.max(1) as usize;
        let group = self.int.len() >= min;
        for (i, c) in self.int.chars().enumerate() {
            if group && i > 0 && (self.int.len() - i).is_multiple_of(3) {
                out.push(format.group_separator);
            }
            out.push(c);
        }
        if !self.frac.is_empty() {
            out.push(format.decimal_marker);
            let group = self.frac.len() >= min;
            for (i, c) in self.frac.chars().enumerate() {
                if group && i > 0 && i.is_multiple_of(3) {
                    out.push(format.group_separator);
                }
                out.push(c);
            }
        }
        out
    }

    /// The nodes of the number: a minus sign, the mantissa as one number,
    /// and its power of ten.
    fn lower(&self, format: &NumberFormat, span: Span, out: &mut Vec<Node>) {
        if self.negative {
            out.push(Node::symbol(MINUS, span));
        }
        out.push(Node::number(self.format_mantissa(format), span));
        if let Some(exponent) = self.exponent {
            power_of_ten(exponent, span, out);
        }
    }
}

impl Display for Number {
    /// The number as a literal that [`Number::parse`] reads back:
    /// `-6.02e23`.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_char('-')?;
        }
        f.write_str(&self.int)?;
        if !self.frac.is_empty() {
            write!(f, ".{}", self.frac)?;
        }
        if let Some(exponent) = self.exponent {
            write!(f, "e{exponent}")?;
        }
        Ok(())
    }
}

impl Debug for Number {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Number({self})")
    }
}

/// `× 10ⁿ`.
fn power_of_ten(exponent: i32, span: Span, out: &mut Vec<Node>) {
    out.push(Node::symbol('×', span));
    out.push(Node::Attach(Arc::new(Attach {
        base: Row::new([Node::number("10".into(), span)]),
        sub: None,
        sup: Some(signed(exponent, span)),
        primes: 0,
        span,
    })));
}

/// A row of a signed integer: `−2`.
fn signed(n: i32, span: Span) -> Row {
    let mut nodes = Vec::new();
    if n < 0 {
        nodes.push(Node::symbol(MINUS, span));
    }
    nodes.push(Node::number(n.unsigned_abs().to_string().into(), span));
    Row::new(nodes)
}

fn thin_space(span: Span) -> Node {
    Node::Symbol(Atom {
        text: THIN_SPACE.into(),
        class: MathClass::Space,
        span,
    })
}

/// A unit: factors with powers, `kg m^2 s^-1`.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Unit {
    /// The factors, in the order they were written.
    pub factors: EcoVec<UnitFactor>,
}

/// A factor of a unit: a symbol with a power, `s^-2`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct UnitFactor {
    /// The symbol, set upright: `m`, `kg`, `Ω`, `°C`.
    pub symbol: EcoString,
    /// The power it is raised to: `1` unless written, never `0`.
    pub power: i32,
}

/// Why a unit could not be read, and where in its text.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UnitError {
    /// What is wrong.
    pub message: EcoString,
    /// A hint at how to write it, if there is one.
    pub hint: Option<EcoString>,
    /// The bytes of the unit's text it is about.
    pub range: std::ops::Range<usize>,
}

/// Names for unit symbols that are hard to type, and the symbols they
/// stand for.
const UNIT_NAMES: &[(&str, &str)] = &[
    ("ohm", "Ω"),
    ("deg", "°"),
    ("degC", "°C"),
    ("arcmin", "′"),
    ("arcsec", "″"),
    ("angstrom", "Å"),
    ("micro", "µ"),
];

/// The names of [`UNIT_NAMES`] that an SI prefix may stand before:
/// `kohm` is `kΩ`. The others name units of angle and temperature, which
/// take no prefix, or `micro`, which is itself a prefix.
const PREFIXED_NAMES: &[&str] = &["ohm"];

/// The SI prefixes, as written before a named symbol (`kohm`, `Mohm`,
/// `mohm`), and the symbols they stand for. `micro` is spelt out, as `µ`
/// is hard to type; `da` is the one prefix of two letters.
const SI_PREFIXES: &[(&str, &str)] = &[
    ("Q", "Q"),
    ("R", "R"),
    ("Y", "Y"),
    ("Z", "Z"),
    ("E", "E"),
    ("P", "P"),
    ("T", "T"),
    ("G", "G"),
    ("M", "M"),
    ("k", "k"),
    ("h", "h"),
    ("da", "da"),
    ("d", "d"),
    ("c", "c"),
    ("m", "m"),
    ("micro", "µ"),
    ("µ", "µ"),
    // The Greek small letter mu (U+03BC), which looks the same as the
    // micro sign and is what many keyboards type.
    ("μ", "µ"),
    ("n", "n"),
    ("p", "p"),
    ("f", "f"),
    ("a", "a"),
    ("z", "z"),
    ("y", "y"),
    ("r", "r"),
    ("q", "q"),
];

impl Unit {
    /// Reads a unit: factors separated by spaces, each a symbol with an
    /// optional integer power after `^`: `kg m^2 s^-1`. A symbol is
    /// letters of any script and the signs `°`, `′`, `″` and `%`, and may
    /// be or end in a name for a hard-to-type symbol: `ohm` for `Ω`, `deg`
    /// for `°`, `degC` for `°C`, `micro` for `µ` (`microF`), and an SI
    /// prefix before `ohm` (`kohm`, `Mohm`, `mohm`, `microohm`). A symbol of
    /// several letters that ends in a name but is not one of these is an
    /// error, not letters set as they are: `xohm`, `kdegC`.
    pub fn parse(text: &str) -> Result<Self, UnitError> {
        let mut factors = EcoVec::new();
        for (start, word) in words(text) {
            let range = start..start + word.len();
            let error = |message: &str, hint: Option<&str>| UnitError {
                message: message.into(),
                hint: hint.map(Into::into),
                range: range.clone(),
            };
            let (symbol, power) = match word.split_once('^') {
                Some((symbol, power)) => (symbol, Some(power)),
                None => (word, None),
            };
            if let Some(bad) = symbol
                .chars()
                .find(|&c| !(c.is_alphabetic() || matches!(c, '°' | '′' | '″' | '%')))
            {
                let hint = match bad {
                    '/' => Some("write a quotient with negative powers: `m s^-1`"),
                    '*' | '.' | '·' | '⋅' => {
                        Some("separate the factors of a unit with spaces: `N m`")
                    }
                    c if c.is_ascii_digit() => Some("a power follows `^`: `m^2`"),
                    _ => None,
                };
                return Err(error(&format!("`{bad}` cannot be part of a unit"), hint));
            }
            if symbol.is_empty() {
                return Err(error("a power needs a unit before it", None));
            }
            let power = match power {
                None => 1,
                Some(power) => {
                    let p = power.strip_prefix('(').and_then(|p| p.strip_suffix(')'));
                    let p = p.unwrap_or(power);
                    let (negative, digits) = match p.strip_prefix(['-', MINUS]) {
                        Some(d) => (true, d),
                        None => (false, p.strip_prefix('+').unwrap_or(p)),
                    };
                    let magnitude = (!digits.is_empty()
                        && digits.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| digits.parse::<i32>().ok())
                    .flatten();
                    let Some(magnitude) = magnitude else {
                        return Err(error(
                            "the power of a unit is an integer",
                            Some("write `m^2` or `s^-1`"),
                        ));
                    };
                    if magnitude == 0 || magnitude > MAX_UNIT_POWER {
                        return Err(error(
                            &format!(
                                "the power of a unit is a whole number from 1 to {MAX_UNIT_POWER}, or its negative"
                            ),
                            None,
                        ));
                    }
                    if negative { -magnitude } else { magnitude }
                }
            };
            let Some(symbol) = expand(symbol) else {
                return Err(unknown_name(symbol, range.start));
            };
            factors.push(UnitFactor { symbol, power });
        }
        if factors.is_empty() {
            return Err(UnitError {
                message: "a unit cannot be empty".into(),
                hint: Some("write its symbols: `m s^-1`".into()),
                range: 0..text.len(),
            });
        }
        Ok(Self { factors })
    }

    /// Whether the unit is one of plane angle written right after its
    /// number: `°`, `′` or `″`.
    pub fn is_angle(&self) -> bool {
        matches!(self.factors.as_slice(), [f] if f.power == 1 && matches!(f.symbol.as_str(), "°" | "′" | "″"))
    }

    /// The nodes of the unit: each factor upright, with its power, and a
    /// thin space between factors.
    fn lower(&self, span: Span, out: &mut Vec<Node>) {
        for (i, factor) in self.factors.iter().enumerate() {
            if i > 0 {
                out.push(thin_space(span));
            }
            let symbol = unit_symbol(&factor.symbol, span);
            if factor.power == 1 {
                out.push(symbol);
            } else {
                out.push(Node::Attach(Arc::new(Attach {
                    base: Row::new([symbol]),
                    sub: None,
                    sup: Some(signed(factor.power, span)),
                    primes: 0,
                    span,
                })));
            }
        }
    }
}

/// The node of a unit's symbol: one symbol atom of the ordinary class,
/// shaped as one run (`kg`, `°C`). Layout sets it upright, or in the
/// alphabet chosen around the formula (bold, sans), as it sets numbers, and
/// spaces it as an ordinary atom. Quoted text would be spaced from its
/// neighbours like a word, which a unit is not: only the thin spaces
/// [`Quantity::lower`] and [`Unit::lower`] put in separate it from its
/// number and from the other factors.
fn unit_symbol(symbol: &EcoString, span: Span) -> Node {
    Node::Symbol(Atom {
        text: symbol.clone(),
        class: MathClass::Normal,
        span,
    })
}

/// The words of a text separated by whitespace, with the byte offset each
/// starts at.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = text;
    let mut offset = 0;
    std::iter::from_fn(move || {
        let trimmed = rest.trim_start();
        offset += rest.len() - trimmed.len();
        if trimmed.is_empty() {
            return None;
        }
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        let word = (offset, &trimmed[..end]);
        rest = &trimmed[end..];
        offset += end;
        Some(word)
    })
}

/// A symbol with a name for a hard-to-type symbol expanded: `ohm` is `Ω`,
/// `microF` is `µF`, `kohm` is `kΩ`. `None` for a symbol of several
/// letters that ends in a name but is neither the name, nor `micro` before
/// a symbol, nor an SI prefix before a name that takes one: `xohm`,
/// `kdeg`, which would otherwise be set as the letters they are.
fn expand(symbol: &str) -> Option<EcoString> {
    if let Some(&(_, expanded)) = UNIT_NAMES.iter().find(|(name, _)| *name == symbol) {
        return Some(expanded.into());
    }
    // An SI prefix before a name that takes one: `kohm`, `microohm`.
    for &name in PREFIXED_NAMES {
        if let Some(prefix) = symbol.strip_suffix(name)
            && let Some(&(_, prefix)) = SI_PREFIXES.iter().find(|(p, _)| *p == prefix)
        {
            let mut out = EcoString::from(prefix);
            out.push_str(&expand(name)?);
            return Some(out);
        }
    }
    // The Greek small letter mu (U+03BC), which many keyboards type, is
    // the micro sign (U+00B5) before any symbol, so that a document has one
    // micro: `μF` is `µF`.
    if let Some(rest) = symbol.strip_prefix('\u{3bc}')
        && !rest.is_empty()
    {
        let mut out = EcoString::from("µ");
        out.push_str(&expand(rest)?);
        return Some(out);
    }
    if let Some(rest) = symbol.strip_prefix("micro")
        && !rest.is_empty()
        // A name that takes no prefix takes no `micro` either
        // (`microdeg`), and one that takes a prefix takes only one
        // (`microkohm`).
        && !UNIT_NAMES.iter().any(|(name, _)| *name == rest)
        && !PREFIXED_NAMES.iter().any(|name| rest.ends_with(name))
    {
        let mut out = EcoString::from("µ");
        out.push_str(&expand(rest)?);
        return Some(out);
    }
    if ends_in_name(symbol) {
        return None;
    }
    Some(symbol.into())
}

/// Whether a symbol ends in one of the names of [`UNIT_NAMES`] after other
/// letters: `xohm`, `kdegC`.
fn ends_in_name(symbol: &str) -> bool {
    UNIT_NAMES
        .iter()
        .any(|(name, _)| symbol.len() > name.len() && symbol.ends_with(name))
}

/// The error for a symbol that ends in a name it cannot be read with:
/// `xohm`, `kdegC`. It points at the symbol, which starts at `start` in the
/// unit's text.
fn unknown_name(symbol: &str, start: usize) -> UnitError {
    let (name, _) = UNIT_NAMES
        .iter()
        .filter(|(name, _)| symbol.len() > name.len() && symbol.ends_with(name))
        .max_by_key(|(name, _)| name.len())
        .expect("the symbol ends in a name");
    let prefix = &symbol[..symbol.len() - name.len()];
    let (message, hint) = if PREFIXED_NAMES.contains(name) {
        (
            format!("`{prefix}` is not an SI prefix of `{name}`"),
            format!(
                "write an SI prefix before `{name}`: `k{name}`, `M{name}`, `m{name}`, `micro{name}`"
            ),
        )
    } else {
        (
            format!("`{name}` takes no prefix, so `{symbol}` is not a unit"),
            format!("write `{name}` alone, or separate the factors with spaces"),
        )
    };
    UnitError {
        message: message.into(),
        hint: Some(hint.into()),
        range: start..start + symbol.len(),
    }
}

impl Display for Unit {
    /// The unit as [`Unit::parse`] reads it back: `kg m^2 s^-1`.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, factor) in self.factors.iter().enumerate() {
            if i > 0 {
                f.write_char(' ')?;
            }
            f.write_str(&factor.symbol)?;
            if factor.power != 1 {
                write!(f, "^{}", factor.power)?;
            }
        }
        Ok(())
    }
}

impl Debug for Unit {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Unit({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(text: &str) -> Number {
        Number::parse(text).unwrap()
    }

    #[test]
    fn parses_literals() {
        assert_eq!(num("9.81").to_string(), "9.81");
        assert_eq!(num("-0.50").to_string(), "-0.50");
        assert_eq!(num("006.02e+23").to_string(), "6.02e23");
        assert_eq!(num("1E-3").to_string(), "1e-3");
        assert_eq!(num(".5").to_string(), "0.5");
        assert_eq!(num("−2").to_string(), "-2");
        assert_eq!(num("-0.0").to_string(), "0.0");
        for bad in ["", "-", "e5", "1.2.3", "1e", "1,5", "1e99999", "x"] {
            assert!(Number::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn floats_in_shortest_form() {
        assert_eq!(Number::from_float(0.1).unwrap().to_string(), "0.1");
        assert_eq!(Number::from_float(6.02e23).unwrap().to_string(), "6.02e23");
        let float = |value: f64| Number::from_float(value).unwrap().to_string();
        assert_eq!(float(-0.0), "0");
        assert_eq!(float(2.0), "2");
        assert_eq!(float(2.5), "2.5");
        assert_eq!(float(-1500.0), "-1500");
        assert_eq!(float(2.0 * 1.5e8), "3e8");
        assert_eq!(float(1.2e5), "1.2e5");
        assert_eq!(float(0.05), "0.05");
        assert_eq!(float(0.001), "1e-3");
        assert_eq!(float(100.0), "100");
        assert_eq!(float(1000.0), "1e3");
        assert_eq!(float(9.81), "9.81");
        assert_eq!(float(-1.602e-19), "-1.602e-19");
        assert_eq!(float(6.62607015e-34), "6.62607015e-34");
        assert_eq!(float(123456.789), "123456.789");
        assert_eq!(float(1e21), "1e21");
        assert_eq!(float(f64::MAX).len(), "1.7976931348623157e308".len());
        assert_eq!(float(f64::MIN_POSITIVE), "2.2250738585072014e-308");
        assert_eq!(Number::from_float(f64::NAN), None);
        assert_eq!(Number::from_int(-42).to_string(), "-42");
    }

    #[test]
    fn rounds_to_significant_figures() {
        let r = |text: &str, n| num(text).round(n).to_string();
        assert_eq!(r("14.679", 3), "14.7");
        assert_eq!(r("9.8", 3), "9.80");
        assert_eq!(r("9.996", 3), "10.0");
        assert_eq!(r("99.96", 3), "100");
        assert_eq!(r("0.09996", 2), "0.10");
        assert_eq!(r("0.0012345", 3), "0.00123");
        assert_eq!(r("123456", 3), "123000");
        assert_eq!(r("2.5", 1), "3");
        assert_eq!(r("-2.5", 1), "-3");
        assert_eq!(r("-0.0004", 1), "-0.0004");
        assert_eq!(r("-0.00004", 1), "-0.00004");
        assert_eq!(r("0", 3), "0.00");
        assert_eq!(r("6.0221e23", 3), "6.02e23");
        assert_eq!(r("9.99e3", 2), "1.0e4");
        assert_eq!(r("9.99e3", 3), "9.99e3");
        assert_eq!(r("9.996e-3", 3), "1.00e-2");
        assert_eq!(r("0.96e3", 1), "1e3");
        assert_eq!(r("999999e3", 1), "100000e4");
        assert_eq!(r("99.9e3", 2), "10e4");
        assert_eq!(r("950", 1), "1000");
    }

    #[test]
    fn groups_digits_from_five() {
        let f = |text: &str, format: &NumberFormat| {
            num(text)
                .format_mantissa(format)
                .to_string()
                .replace(GROUP_SEPARATOR, " ")
        };
        let en = NumberFormat::POINT;
        let pl = NumberFormat::COMMA;
        assert_eq!(f("1500", &en), "1500");
        assert_eq!(f("15000", &en), "15 000");
        assert_eq!(f("1234567.1234", &en), "1 234 567.1234");
        assert_eq!(f("0.123456", &en), "0.123 456");
        assert_eq!(f("9.81", &pl), "9,81");
        assert_eq!(f("12345.678", &pl), "12 345,678");
    }

    #[test]
    fn parses_units() {
        assert_eq!(
            Unit::parse("kg m^2 s^-1").unwrap().to_string(),
            "kg m^2 s^-1"
        );
        assert_eq!(Unit::parse(" m  s^(-2) ").unwrap().to_string(), "m s^-2");
        assert_eq!(Unit::parse("ohm").unwrap().to_string(), "Ω");
        assert_eq!(Unit::parse("degC").unwrap().to_string(), "°C");
        assert_eq!(Unit::parse("microF").unwrap().to_string(), "µF");
        assert!(Unit::parse("deg").unwrap().is_angle());
        let error = Unit::parse("m/s").unwrap_err();
        assert_eq!(error.range, 0..3);
        assert!(error.hint.unwrap().contains("s^-1"));
        let error = Unit::parse("m s^x").unwrap_err();
        assert_eq!(error.range, 2..5);
        assert!(Unit::parse("m^0").is_err());
        assert_eq!(
            Unit::parse("kg\tm^+2\u{a0}s").unwrap().to_string(),
            "kg m^2 s"
        );
        assert_eq!(Unit::parse("m  s^x").unwrap_err().range, 3..6);
        assert!(Unit::parse("^2").is_err());
        assert!(Unit::parse("  ").is_err());
    }

    #[test]
    fn expands_prefixed_names() {
        let unit = |text: &str| Unit::parse(text).unwrap().to_string();
        assert_eq!(unit("kohm"), "kΩ");
        assert_eq!(unit("Mohm"), "MΩ");
        assert_eq!(unit("mohm"), "mΩ");
        assert_eq!(unit("Gohm"), "GΩ");
        assert_eq!(unit("microohm"), "µΩ");
        assert_eq!(unit("µohm"), "µΩ");
        assert_eq!(unit("\u{3bc}ohm"), "µΩ");
        assert_eq!(unit("\u{3bc}F \u{3bc}m"), "µF µm");
        assert_eq!(unit("daohm"), "daΩ");
        assert_eq!(unit("kohm m"), "kΩ m");
        assert_eq!(unit("V kohm^-1"), "V kΩ^-1");
        assert_eq!(unit("microF"), "µF");
        assert_eq!(unit("degC"), "°C");
        assert_eq!(unit("angstrom"), "Å");
        // Symbols that merely contain a name stay as they are.
        assert_eq!(unit("kg"), "kg");
        assert_eq!(unit("ohmx"), "ohmx");
    }

    #[test]
    fn rejects_unknown_names() {
        let error = Unit::parse("m xohm").unwrap_err();
        assert_eq!(error.range, 2..6);
        assert!(
            error.message.contains("`x` is not an SI prefix"),
            "{error:?}"
        );
        assert!(error.hint.unwrap().contains("kohm"));
        let error = Unit::parse("kdegC").unwrap_err();
        assert_eq!(error.range, 0..5);
        assert!(
            error.message.contains("`degC` takes no prefix"),
            "{error:?}"
        );
        for bad in [
            "mdeg",
            "microdeg",
            "karcmin",
            "Mangstrom",
            "kiloohm",
            "microxohm",
            "microkohm",
            "micromohm",
        ] {
            assert!(Unit::parse(bad).is_err(), "{bad}");
        }
    }
}
