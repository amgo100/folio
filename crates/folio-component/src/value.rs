//! Plain-data values: the defaults of element fields and the initial values
//! of properties (SPEC §3.2, §4.3).

use std::fmt::{self, Display, Formatter};

use ecow::{EcoString, EcoVec};

use crate::draw::Color;
use crate::geom::{Abs, Em, Ratio, Scalar};
use crate::names::Name;

/// A constant value, as a component declares it.
///
/// It is plain data so that the interface can cross a WebAssembly boundary
/// unchanged (SPEC §3.4).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Value {
    /// `none`: no value.
    None,
    /// `auto`: a value the core decides.
    Auto,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Number(Scalar),
    /// A string.
    Str(EcoString),
    /// A keyword, as stylesheets write them unquoted: `balance`.
    Keyword(Name),
    /// An absolute length.
    Length(Abs),
    /// A font-relative length.
    Em(Em),
    /// A ratio or percentage.
    Ratio(Ratio),
    /// A colour.
    Color(Color),
    /// A duration in seconds: `300ms`.
    Duration(Scalar),
    /// An angle in radians: `90deg`.
    Angle(Scalar),
    /// A fraction of the remaining space: `1fr`.
    Fraction(Scalar),
    /// A keyword with arguments, as stylesheets write effects and areas:
    /// `wipe(from-left)`, `area(5%, 5%, 90%, 15%)`.
    Call(Name, EcoVec<Value>),
    /// An array.
    Array(EcoVec<Value>),
    /// A dictionary with keys in declaration order.
    Dict(EcoVec<(Name, Value)>),
}

impl Value {
    /// A short description of the value's type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Number(_) => "number",
            Self::Str(_) => "string",
            Self::Keyword(_) => "keyword",
            Self::Length(_) => "length",
            Self::Em(_) => "em length",
            Self::Ratio(_) => "ratio",
            Self::Color(_) => "colour",
            Self::Duration(_) => "duration",
            Self::Angle(_) => "angle",
            Self::Fraction(_) => "fraction",
            Self::Call(..) => "call",
            Self::Array(_) => "array",
            Self::Dict(_) => "dictionary",
        }
    }
}

/// The type of an element field (SPEC §4.3).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum FieldType {
    /// Markup content.
    Content,
    /// A string.
    Str,
    /// An integer.
    Int,
    /// A number: an integer or a float.
    Number,
    /// A boolean.
    Bool,
    /// One of a fixed set of strings: `choice("core", "extension")`.
    Choice(EcoVec<EcoString>),
    /// A reference to a labelled element.
    Ref,
    /// An array of values of one type.
    Array(Box<FieldType>),
    /// A dictionary with values of one type.
    Dict(Box<FieldType>),
    /// A length. Allowed only on fields marked `layout-data` (SPEC §5.5).
    Length,
    /// A colour. Allowed only on fields marked `layout-data` (SPEC §5.5).
    Color,
}

impl FieldType {
    /// Whether a default value fits the type. `none` fits every type: a
    /// field that defaults to `none` is optional.
    pub fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (_, Value::None) => true,
            (Self::Content | Self::Str, Value::Str(_)) => true,
            (Self::Int, Value::Int(_)) => true,
            (Self::Number, Value::Int(_) | Value::Number(_)) => true,
            (Self::Bool, Value::Bool(_)) => true,
            (Self::Choice(options), Value::Str(s)) => options.contains(s),
            (Self::Array(item), Value::Array(items)) => items.iter().all(|v| item.accepts(v)),
            (Self::Dict(item), Value::Dict(pairs)) => pairs.iter().all(|(_, v)| item.accepts(v)),
            (Self::Length, Value::Length(_) | Value::Em(_)) => true,
            (Self::Color, Value::Color(_)) => true,
            _ => false,
        }
    }

    /// Whether the type holds lengths or colours, which are formatting
    /// unless the field is marked `layout-data`.
    pub fn is_presentational(&self) -> bool {
        match self {
            Self::Length | Self::Color => true,
            Self::Array(item) | Self::Dict(item) => item.is_presentational(),
            _ => false,
        }
    }
}

impl Display for FieldType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content => f.write_str("content"),
            Self::Str => f.write_str("str"),
            Self::Int => f.write_str("int"),
            Self::Number => f.write_str("number"),
            Self::Bool => f.write_str("bool"),
            Self::Choice(options) => {
                f.write_str("choice(")?;
                for (i, option) in options.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{option:?}")?;
                }
                f.write_str(")")
            }
            Self::Ref => f.write_str("ref"),
            Self::Array(item) => write!(f, "array({item})"),
            Self::Dict(item) => write!(f, "dict({item})"),
            Self::Length => f.write_str("length"),
            Self::Color => f.write_str("color"),
        }
    }
}

/// The type of a stylesheet property's value (SPEC §3.2, §5.2).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum PropertyType {
    /// `none`.
    None,
    /// `auto`.
    Auto,
    /// A boolean.
    Bool,
    /// An integer.
    Int,
    /// A number: an integer or a float.
    Number,
    /// A length, absolute or relative to the font size.
    Length,
    /// A ratio or percentage.
    Ratio,
    /// A colour.
    Color,
    /// A duration.
    Duration,
    /// A fraction of the remaining space: `1fr`.
    Fraction,
    /// A string.
    Str,
    /// One of a fixed set of keywords: `balance | auto`.
    Keyword(EcoVec<Name>),
    /// A list of values of one type.
    List(Box<PropertyType>),
    /// A value of any of several types: `auto | int`.
    AnyOf(EcoVec<PropertyType>),
    /// Any value. For compound values whose grammar the stage that reads
    /// them checks, such as `margin-column: outer, width 32mm`.
    Any,
    /// A render template (SPEC §5.3), such as a `trailer` or a `header`,
    /// expanded per element. Its initial value is `none` or `auto`; the
    /// style stage keeps a declared template as syntax.
    Template,
}

impl PropertyType {
    /// Whether a value fits the type.
    pub fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::None, Value::None) => true,
            (Self::Auto, Value::Auto) => true,
            (Self::Bool, Value::Bool(_)) => true,
            (Self::Int, Value::Int(_)) => true,
            (Self::Number, Value::Int(_) | Value::Number(_)) => true,
            (Self::Length, Value::Length(_) | Value::Em(_)) => true,
            (Self::Ratio, Value::Ratio(_)) => true,
            (Self::Color, Value::Color(_)) => true,
            (Self::Duration, Value::Duration(_)) => true,
            (Self::Fraction, Value::Fraction(_)) => true,
            (Self::Str, Value::Str(_)) => true,
            (Self::Keyword(words), Value::Keyword(word)) => words.contains(word),
            (Self::Any, _) => true,
            (Self::Template, Value::None | Value::Auto) => true,
            (Self::List(item), Value::Array(items)) => items.iter().all(|v| item.accepts(v)),
            (Self::AnyOf(types), value) => types.iter().any(|ty| ty.accepts(value)),
            _ => false,
        }
    }
}

impl Display for PropertyType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Auto => f.write_str("auto"),
            Self::Bool => f.write_str("bool"),
            Self::Int => f.write_str("int"),
            Self::Number => f.write_str("number"),
            Self::Length => f.write_str("length"),
            Self::Ratio => f.write_str("ratio"),
            Self::Color => f.write_str("color"),
            Self::Duration => f.write_str("duration"),
            Self::Fraction => f.write_str("fraction"),
            Self::Str => f.write_str("str"),
            Self::Keyword(words) => join(f, words.iter()),
            Self::List(item) => write!(f, "list({item})"),
            Self::AnyOf(types) => join(f, types.iter()),
            Self::Any => f.write_str("any"),
            Self::Template => f.write_str("template"),
        }
    }
}

/// Writes items separated by ` | `.
fn join<T: Display>(f: &mut Formatter<'_>, items: impl Iterator<Item = T>) -> fmt::Result {
    for (i, item) in items.enumerate() {
        if i > 0 {
            f.write_str(" | ")?;
        }
        write!(f, "{item}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Name {
        Name::new(s).unwrap()
    }

    #[test]
    fn field_types() {
        let choice = FieldType::Choice(EcoVec::from(["core".into(), "extension".into()]));
        assert!(choice.accepts(&Value::Str("core".into())));
        assert!(!choice.accepts(&Value::Str("hard".into())));
        assert!(choice.accepts(&Value::None));
        assert!(FieldType::Number.accepts(&Value::Int(3)));
        assert!(!FieldType::Int.accepts(&Value::Number(Scalar::new(3.5))));
        assert!(FieldType::Array(Box::new(FieldType::Length)).is_presentational());
        assert_eq!(choice.to_string(), r#"choice("core", "extension")"#);
    }

    #[test]
    fn property_types() {
        let columns = PropertyType::AnyOf(EcoVec::from([PropertyType::Auto, PropertyType::Int]));
        assert!(columns.accepts(&Value::Auto));
        assert!(columns.accepts(&Value::Int(2)));
        assert!(!columns.accepts(&Value::None));
        assert_eq!(columns.to_string(), "auto | int");
        let fill = PropertyType::Keyword(EcoVec::from([name("balance"), name("fill")]));
        assert!(fill.accepts(&Value::Keyword(name("balance"))));
        assert!(!fill.accepts(&Value::Keyword(name("even"))));
    }
}
