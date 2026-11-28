//! Queries (SPEC §4.5, §6.4): what an element shows that the rest of the
//! document determines.
//!
//! - `#outline(heading)` and `#list-of(figure)` list the instances of an
//!   element that the variant shows, in document order, each with its
//!   marker (*2.1*, *Figure 3:*) and its title: its caption, or else its
//!   text. They read the variant's own tree after Select, so they never
//!   list what the variant hides.
//! - `#total(question, "marks")` sums a numeric field over the instances
//!   of an element. In a numbering group it reads the group's common tree
//!   ([`crate::variants::common`]), so a cover page and its mark scheme
//!   show the same total; elsewhere, the variant's own tree. A missing or
//!   `none` field counts as 0, and a field of another type is a warning.
//!
//! The machinery is two walks that components' aggregates (#72) and
//! `folio query` (#74) can reuse: [`instances`] finds the instances of an
//! element in a tree, and [`sum`] totals a field over them.

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, Location};
use folio_syntax::Span;

use crate::diag::Diagnostic;
use crate::eval::{Element, Node, Value};

/// What a query element resolved to.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Query {
    /// The query element instance.
    pub location: Location,
    /// Its result.
    pub result: QueryResult,
}

/// The result of a query.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum QueryResult {
    /// `#total`: the sum, as it is shown.
    Total(EcoString),
    /// `#outline` and `#list-of`: the entries, in document order.
    List(EcoVec<Entry>),
}

/// An entry of an outline or figure list.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Entry {
    /// The instance it lists.
    pub target: Location,
    /// Its level: a heading's, and 1 for every other element.
    pub level: u32,
    /// Its marker as the document shows it, with the space after it
    /// (`2.1 `, `Figure 3: `), or `None` when it shows none.
    pub marker: Option<EcoString>,
    /// Its title: its caption, or else the text of its body.
    pub title: EcoString,
}

/// What a query asks for, as the walk found it.
#[derive(Debug, Clone)]
pub(super) struct Pending {
    pub location: Location,
    pub span: Span,
    /// The element it reads, by qualified name.
    pub target: EcoString,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub(super) enum Kind {
    /// An outline or a figure list.
    List,
    /// A total of a field, shown with a decimal marker.
    Total { field: EcoString, marker: char },
}

/// Whether an element is the one a query names: by qualified name, or by
/// name alone when the query names it without a namespace.
fn is(element: &Element, target: &str) -> bool {
    let name = element.name.to_string();
    name == target || (!target.contains('.') && element.name.name.as_str() == target)
}

/// The instances of an element in a tree, in document order, with the
/// instances inside them.
pub fn instances<'a>(root: &'a Element, target: &str) -> Vec<&'a Element> {
    fn walk<'a>(element: &'a Element, target: &str, out: &mut Vec<&'a Element>) {
        if is(element, target) {
            out.push(element);
        }
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                for node in content.nodes() {
                    if let Node::Element(child) = node {
                        walk(child, target, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, target, &mut out);
    out
}

/// The title of an entry: its caption, or else the text of its body,
/// trimmed.
pub(super) fn title(element: &Element) -> EcoString {
    let text = |field: &str| match element.field(field) {
        Some(Value::Content(content)) => Some(content.plain_text()),
        _ => None,
    };
    text("caption")
        .or_else(|| text(BODY))
        .map(|t| t.trim().into())
        .unwrap_or_default()
}

/// The level of an entry: a heading's level, and 1 for every other
/// element.
pub(super) fn level(element: &Element) -> u32 {
    match element.field("level") {
        Some(Value::Int(n)) if element.name.namespace.is_none() => {
            u32::try_from(*n).unwrap_or(1).max(1)
        }
        _ => 1,
    }
}

/// The sum of a field over instances.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sum {
    /// Every value was an integer.
    Int(i64),
    /// Some value was a float.
    Float(f64),
}

/// Sums `field` over `instances`. A missing or `none` field counts as 0.
/// The second result is the first instance whose field holds a value
/// that is no number, which counts as 0 too.
pub fn sum<'a>(
    instances: impl IntoIterator<Item = &'a Element>,
    field: &str,
) -> (Sum, Option<&'a Element>) {
    let mut int: i64 = 0;
    let mut float: Option<f64> = None;
    let mut wrong = None;
    for element in instances {
        match element.field(field) {
            None | Some(Value::None) => {}
            Some(Value::Int(n)) => match float.as_mut() {
                Some(f) => *f += *n as f64,
                None => int = int.saturating_add(*n),
            },
            Some(Value::Float(x)) => {
                let f = float.get_or_insert(int as f64);
                *f += x.get();
            }
            Some(_) => {
                wrong.get_or_insert(element);
            }
        }
    }
    let sum = match float {
        Some(f) => Sum::Float(f),
        None => Sum::Int(int),
    };
    (sum, wrong)
}

/// A sum as a total shows it: an integer as it is, a float in its
/// shortest form with the decimal marker.
pub fn format(sum: Sum, marker: char) -> EcoString {
    match sum {
        Sum::Int(n) => eco_format!("{n}"),
        Sum::Float(f) if f.is_finite() && f == f.trunc() && f.abs() < 1e15 => {
            eco_format!("{}", f as i64)
        }
        Sum::Float(f) => {
            let text = eco_format!("{f}");
            text.replace(".", marker.encode_utf8(&mut [0; 4]))
        }
    }
}

/// The warning for a total whose field holds a value that is no number at
/// an instance.
pub(super) fn not_a_number(query: Span, field: &str, element: &Element) -> Diagnostic {
    Diagnostic::warning(
        query,
        eco_format!(
            "`{field}` of a `{}` is not a number, so this total counts it as 0",
            element.name
        ),
    )
    .with_related(element.span, "this instance's value")
    .with_hint("a total adds up a field of integers or numbers, such as `marks: 3`")
}
