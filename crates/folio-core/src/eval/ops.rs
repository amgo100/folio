//! Operators: arithmetic, comparison, containment and joining.

use std::cmp::Ordering;

use ecow::eco_format;
use folio_component::Scalar;

use super::content::{Content, Node};
use super::error::StrResult;
use super::value::{Angle, Duration, Fraction, Length, Value as V};

fn mismatch(op: &str, lhs: &V, rhs: &V) -> ecow::EcoString {
    eco_format!(
        "cannot {op} {} and {}",
        a_type(lhs.type_name()),
        a_type(rhs.type_name())
    )
}

/// A type name with its article: "an int", "a string".
pub(crate) fn a_type(name: &str) -> String {
    let article = match name.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    };
    format!("{article} {name}")
}

/// A float result, or an error for one that is not a number, such as
/// `inf - inf`: it would otherwise become zero silently.
fn float(v: f64) -> StrResult<V> {
    if v.is_nan() {
        return Err("the result is not a number".into());
    }
    Ok(V::Float(Scalar::new(v)))
}

/// `+x`.
pub(crate) fn pos(value: V) -> StrResult<V> {
    match value {
        V::Int(_)
        | V::Float(_)
        | V::Length(_)
        | V::Angle(_)
        | V::Ratio(_)
        | V::Fraction(_)
        | V::Duration(_) => Ok(value),
        v => Err(eco_format!(
            "cannot apply unary `+` to {}",
            a_type(v.type_name())
        )),
    }
}

/// `-x`.
pub(crate) fn neg(value: V) -> StrResult<V> {
    Ok(match value {
        V::Int(v) => V::Int(
            v.checked_neg()
                .ok_or_else(|| eco_format!("value is too large"))?,
        ),
        V::Float(v) => float(-v.get())?,
        V::Length(v) => V::Length(Length {
            abs: -v.abs,
            em: -v.em,
        }),
        V::Angle(v) => V::Angle(Angle(Scalar::new(-v.0.get()))),
        V::Ratio(v) => V::Ratio(-v),
        V::Fraction(v) => V::Fraction(Fraction(Scalar::new(-v.get()))),
        V::Duration(v) => V::Duration(Duration(Scalar::new(-v.seconds()))),
        v => bail_str!("cannot apply unary `-` to {}", a_type(v.type_name())),
    })
}

/// `not x`.
pub(crate) fn not(value: V) -> StrResult<V> {
    match value {
        V::Bool(b) => Ok(V::Bool(!b)),
        v => Err(eco_format!(
            "cannot apply `not` to {}",
            a_type(v.type_name())
        )),
    }
}

macro_rules! bail_str {
    ($($fmt:tt)*) => { return Err(eco_format!($($fmt)*)) };
}
use bail_str;

/// `a + b`. Strings, arrays and content concatenate, dictionaries merge.
/// The left operand is extended in place when nothing else holds it.
pub(crate) fn add(lhs: V, rhs: V) -> StrResult<V> {
    Ok(match (lhs, rhs) {
        (V::Int(a), V::Int(b)) => V::Int(a.checked_add(b).ok_or("value is too large")?),
        (V::Int(a), V::Float(b)) => float(a as f64 + b.get())?,
        (V::Float(a), V::Int(b)) => float(a.get() + b as f64)?,
        (V::Float(a), V::Float(b)) => float(a.get() + b.get())?,
        (V::Length(a), V::Length(b)) => V::Length(Length {
            abs: a.abs + b.abs,
            em: a.em + b.em,
        }),
        (V::Angle(a), V::Angle(b)) => V::Angle(Angle(Scalar::new(a.0.get() + b.0.get()))),
        (V::Ratio(a), V::Ratio(b)) => V::Ratio(a + b),
        (V::Fraction(a), V::Fraction(b)) => V::Fraction(Fraction(Scalar::new(a.get() + b.get()))),
        (V::Duration(a), V::Duration(b)) => {
            V::Duration(Duration(Scalar::new(a.seconds() + b.seconds())))
        }
        (V::Str(mut a), V::Str(b)) => {
            a.push_str(&b);
            V::Str(a)
        }
        (V::Array(mut a), V::Array(b)) => {
            a.extend(b);
            V::Array(a)
        }
        (V::Dict(mut a), V::Dict(b)) => {
            for (key, value) in b.iter() {
                a.insert(key.clone(), value.clone());
            }
            V::Dict(a)
        }
        (a @ (V::Content(_) | V::Str(_)), b @ (V::Content(_) | V::Str(_))) => {
            let mut out = into_content(a);
            out.extend(into_content(b));
            V::Content(out)
        }
        (a, b) => return Err(mismatch("add", &a, &b)),
    })
}

/// A string or content as content.
fn into_content(value: V) -> Content {
    match value {
        V::Content(c) => c,
        V::Str(s) => Content::node(Node::Text {
            text: s,
            span: folio_syntax::Span::detached(),
        }),
        _ => Content::new(),
    }
}

/// `a - b`.
pub(crate) fn sub(lhs: V, rhs: V) -> StrResult<V> {
    Ok(match (lhs, rhs) {
        (V::Int(a), V::Int(b)) => V::Int(a.checked_sub(b).ok_or("value is too large")?),
        (V::Int(a), V::Float(b)) => float(a as f64 - b.get())?,
        (V::Float(a), V::Int(b)) => float(a.get() - b as f64)?,
        (V::Float(a), V::Float(b)) => float(a.get() - b.get())?,
        (V::Length(a), V::Length(b)) => V::Length(Length {
            abs: a.abs - b.abs,
            em: a.em - b.em,
        }),
        (V::Angle(a), V::Angle(b)) => V::Angle(Angle(Scalar::new(a.0.get() - b.0.get()))),
        (V::Ratio(a), V::Ratio(b)) => V::Ratio(a - b),
        (V::Fraction(a), V::Fraction(b)) => V::Fraction(Fraction(Scalar::new(a.get() - b.get()))),
        (V::Duration(a), V::Duration(b)) => {
            V::Duration(Duration(Scalar::new(a.seconds() - b.seconds())))
        }
        (a, b) => return Err(mismatch("subtract", &a, &b)),
    })
}

/// A number as a float, if the value is one.
fn number(value: &V) -> Option<f64> {
    match value {
        V::Int(v) => Some(*v as f64),
        V::Float(v) => Some(v.get()),
        _ => None,
    }
}

/// A quantity scaled by a factor.
fn scale(value: &V, by: f64) -> Option<V> {
    Some(match value {
        V::Length(v) => V::Length(Length {
            abs: v.abs * by,
            em: v.em * by,
        }),
        V::Angle(v) => V::Angle(Angle(Scalar::new(v.0.get() * by))),
        V::Ratio(v) => V::Ratio(*v * by),
        V::Fraction(v) => V::Fraction(Fraction(Scalar::new(v.get() * by))),
        V::Duration(v) => V::Duration(Duration(Scalar::new(v.seconds() * by))),
        _ => return None,
    })
}

/// How many times a string, array or content is repeated: `"ab" * 3`. The
/// caller has checked the result's size against the memory limit.
pub(crate) fn repetitions(value: &V) -> StrResult<usize> {
    match value {
        V::Int(n) => {
            usize::try_from(*n).map_err(|_| eco_format!("cannot repeat a negative number of times"))
        }
        _ => Err("expected an integer".into()),
    }
}

/// `a * b`.
pub(crate) fn mul(lhs: V, rhs: V) -> StrResult<V> {
    Ok(match (lhs, rhs) {
        (V::Int(a), V::Int(b)) => V::Int(a.checked_mul(b).ok_or("value is too large")?),
        (a @ (V::Int(_) | V::Float(_)), b @ (V::Int(_) | V::Float(_))) => {
            float(number(&a).unwrap_or(0.0) * number(&b).unwrap_or(0.0))?
        }
        // An empty value repeats to itself without looping: the memory
        // limit, which bounds the other repetitions, does not count it.
        (V::Str(s), n @ V::Int(_)) | (n @ V::Int(_), V::Str(s)) if s.is_empty() => {
            repetitions(&n)?;
            V::Str(s)
        }
        (V::Array(a), n @ V::Int(_)) | (n @ V::Int(_), V::Array(a)) if a.is_empty() => {
            repetitions(&n)?;
            V::Array(a)
        }
        (V::Content(c), n @ V::Int(_)) | (n @ V::Int(_), V::Content(c)) if c.is_empty() => {
            repetitions(&n)?;
            V::Content(c)
        }
        (V::Str(s), n @ V::Int(_)) | (n @ V::Int(_), V::Str(s)) => {
            V::Str(s.repeat(repetitions(&n)?))
        }
        (V::Array(a), n @ V::Int(_)) | (n @ V::Int(_), V::Array(a)) => {
            let n = repetitions(&n)?;
            let mut out = super::value::Array::with_capacity(a.len() * n);
            for _ in 0..n {
                out.extend(a.iter().cloned());
            }
            V::Array(out)
        }
        (V::Content(c), n @ V::Int(_)) | (n @ V::Int(_), V::Content(c)) => {
            let n = repetitions(&n)?;
            (0..n)
                .flat_map(|_| c.nodes().iter().cloned())
                .collect::<Content>()
                .into()
        }
        (a, b) => {
            if let Some(by) = number(&b)
                && let Some(v) = scale(&a, by)
            {
                v
            } else if let Some(by) = number(&a)
                && let Some(v) = scale(&b, by)
            {
                v
            } else {
                return Err(mismatch("multiply", &a, &b));
            }
        }
    })
}

/// `a / b`.
pub(crate) fn div(lhs: V, rhs: V) -> StrResult<V> {
    if is_zero(&rhs) {
        return Err("cannot divide by zero".into());
    }
    Ok(match (lhs, rhs) {
        (a @ (V::Int(_) | V::Float(_)), b @ (V::Int(_) | V::Float(_))) => {
            float(number(&a).unwrap_or(0.0) / number(&b).unwrap_or(1.0))?
        }
        (V::Length(a), V::Length(b)) if a.em.is_zero() && b.em.is_zero() => {
            float(a.abs.to_pt() / b.abs.to_pt())?
        }
        (V::Length(a), V::Length(b)) if a.abs.is_zero() && b.abs.is_zero() => {
            float(a.em.get() / b.em.get())?
        }
        (V::Angle(a), V::Angle(b)) => float(a.0.get() / b.0.get())?,
        (V::Ratio(a), V::Ratio(b)) => float(a.get() / b.get())?,
        (V::Fraction(a), V::Fraction(b)) => float(a.get() / b.get())?,
        (V::Duration(a), V::Duration(b)) => float(a.seconds() / b.seconds())?,
        (a, b) => match number(&b).and_then(|by| scale(&a, 1.0 / by)) {
            Some(v) => v,
            None => return Err(mismatch("divide", &a, &b)),
        },
    })
}

fn is_zero(value: &V) -> bool {
    match value {
        V::Int(0) => true,
        V::Float(v) => v.get() == 0.0,
        V::Length(v) => v.abs.is_zero() && v.em.is_zero(),
        V::Angle(v) => v.0.get() == 0.0,
        V::Ratio(v) => v.is_zero(),
        V::Fraction(v) => v.get() == 0.0,
        V::Duration(v) => v.seconds() == 0.0,
        _ => false,
    }
}

/// `a == b`: structural, except that integers and floats compare by value.
pub(crate) fn equal(lhs: &V, rhs: &V) -> bool {
    match (lhs, rhs) {
        (V::Int(a), V::Float(b)) | (V::Float(b), V::Int(a)) => *a as f64 == b.get(),
        (V::Array(a), V::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (V::Dict(a), V::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| equal(value, other)))
        }
        (a, b) => a == b,
    }
}

/// The order of two values for `<`, `<=`, `>`, `>=` and sorting.
pub(crate) fn compare(lhs: &V, rhs: &V) -> StrResult<Ordering> {
    Ok(match (lhs, rhs) {
        (V::Bool(a), V::Bool(b)) => a.cmp(b),
        (V::Int(a), V::Int(b)) => a.cmp(b),
        (a @ (V::Int(_) | V::Float(_)), b @ (V::Int(_) | V::Float(_))) => {
            Scalar::new(number(a).unwrap_or(0.0)).cmp(&Scalar::new(number(b).unwrap_or(0.0)))
        }
        (V::Length(a), V::Length(b)) if a.em == b.em => a.abs.cmp(&b.abs),
        (V::Length(a), V::Length(b)) if a.abs == b.abs => a.em.cmp(&b.em),
        (V::Angle(a), V::Angle(b)) => a.cmp(b),
        (V::Ratio(a), V::Ratio(b)) => a.cmp(b),
        (V::Fraction(a), V::Fraction(b)) => a.cmp(b),
        (V::Duration(a), V::Duration(b)) => a.cmp(b),
        (V::Str(a), V::Str(b)) => a.cmp(b),
        (V::Date(a), V::Date(b)) => a.cmp(b),
        (V::Array(a), V::Array(b)) => {
            for (a, b) in a.iter().zip(b) {
                match compare(a, b)? {
                    Ordering::Equal => {}
                    order => return Ok(order),
                }
            }
            a.len().cmp(&b.len())
        }
        (a, b) => {
            return Err(eco_format!("cannot compare {} and {}", a.repr(), b.repr()));
        }
    })
}

/// `a in b`.
pub(crate) fn contains(item: &V, collection: &V) -> StrResult<bool> {
    match (item, collection) {
        (V::Str(a), V::Str(b)) => Ok(b.contains(a.as_str())),
        (V::Str(key), V::Dict(d)) => Ok(d.get(key).is_some()),
        (a, V::Array(items)) => Ok(items.iter().any(|b| equal(a, b))),
        (a, b) => Err(eco_format!(
            "cannot apply `in` to {} and {}",
            a_type(a.type_name()),
            a_type(b.type_name())
        )),
    }
}

/// Joins the values of consecutive expressions in a code block or loop:
/// `none` disappears, strings and content concatenate, arrays concatenate,
/// dictionaries merge.
pub(crate) fn join(lhs: V, rhs: V) -> StrResult<V> {
    Ok(match (lhs, rhs) {
        (a, V::None) => a,
        (V::None, b) => b,
        (a @ (V::Str(_) | V::Content(_) | V::Array(_) | V::Dict(_)), b) => match add(a, b) {
            Ok(v) => v,
            Err(_) => return Err("cannot join these values".into()),
        },
        (a, b) => {
            return Err(eco_format!(
                "cannot join {} with {}",
                a_type(a.type_name()),
                a_type(b.type_name())
            ));
        }
    })
}

impl From<Content> for V {
    fn from(content: Content) -> Self {
        V::Content(content)
    }
}
