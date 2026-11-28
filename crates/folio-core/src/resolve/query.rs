//! Queries (SPEC §4.5, §6.4): what an element shows that the rest of the
//! document determines.
//!
//! - `#outline(heading)` and `#list-of(figure)` list the instances of an
//!   element that the variant shows, in document order, each with its
//!   marker (*2.1*, *Figure 3:*), its title (its caption, or else its
//!   text) and, after a comma, the page it starts on, which the
//!   page-number fixpoint sets (D-65-1: `2.1 Forces, 4`). They read the
//!   variant's own tree after Select, so they never list what the variant
//!   hides.
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
use folio_component::{BODY, ElementKind, Location, Name, QualifiedName, Value as Plain};
use folio_syntax::Span;

use crate::diag::Diagnostic;
use crate::eval::{Content, Element, Node, Value};

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
    /// The page it starts on, as the entry shows it (D-65-1), set by the
    /// page-number fixpoint like a page-number reference's
    /// ([`super::Resolution::set_pages`]); [`super::UNRESOLVED`] until a
    /// layout pass has measured it.
    pub page: EcoString,
}

impl Entry {
    /// The entry's text: its marker, its title, and its page after a
    /// separator (`2.1 Forces, 4`, D-65-1).
    pub fn text(&self) -> EcoString {
        eco_format!(
            "{}{}, {}",
            self.marker.as_deref().unwrap_or(""),
            self.title,
            self.page
        )
    }
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
    /// An outline or a figure list, and whether its titles drop a final
    /// full stop (`title-stop`, D-65-2).
    List { drop_stop: bool },
    /// A total of a field, shown with a decimal marker.
    Total { field: EcoString, marker: char },
    /// A component's aggregate (D-72-1), shown with a decimal marker.
    Aggregate {
        namespace: Name,
        name: Name,
        marker: char,
    },
}

/// Whether an element is the one a query names. A query stores the
/// element's qualified name, so a built-in `figure` never matches a
/// component's `plot.figure`.
fn is(element: &Element, target: &QualifiedName) -> bool {
    element.name == *target
}

/// The instances of an element in a tree, in document order, with the
/// instances inside them. `target` is the element's qualified name, as a
/// query stores it; a name that names no element finds none.
pub fn instances<'a>(root: &'a Element, target: &str) -> Vec<&'a Element> {
    fn walk<'a>(element: &'a Element, target: &QualifiedName, out: &mut Vec<&'a Element>) {
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
    if let Some(target) = QualifiedName::new(target) {
        walk(root, &target, &mut out);
    }
    out
}

/// The title of an entry: its caption, or else its body, as the
/// document shows the text: `shown` gives the text of an element that
/// shows text of its own (a reference's), notes and floats are left out,
/// and every other element gives the text of its fields of content, as
/// [`Content::plain_text`] does. Trimmed.
pub(super) fn title(element: &Element, shown: &dyn Fn(&Element) -> Option<EcoString>) -> EcoString {
    fn write(
        content: &Content,
        shown: &dyn Fn(&Element) -> Option<EcoString>,
        out: &mut EcoString,
    ) {
        for node in content.nodes() {
            match node {
                Node::Text { text, .. } => out.push_str(text),
                Node::Space(_) | Node::Parbreak(_) | Node::Linebreak(_) => out.push(' '),
                Node::Quote { double, .. } => out.push(if *double { '"' } else { '\'' }),
                Node::Element(child) => {
                    if matches!(child.kind, ElementKind::Note | ElementKind::Float) {
                        continue;
                    }
                    if let Some(text) = shown(child) {
                        out.push_str(&text);
                        continue;
                    }
                    // As `Content::plain_text` writes it: the text of every
                    // field of content. A formula holds none, so it shows
                    // nothing in a title yet.
                    for (_, value) in child.fields.iter() {
                        if let Value::Content(inner) = value {
                            write(inner, shown, out);
                        }
                    }
                }
                Node::Slot(_) | Node::Rule(_) => {}
            }
        }
    }
    let field = match element.field("caption") {
        Some(Value::Content(content)) => Some(content),
        _ => match element.field(BODY) {
            Some(Value::Content(content)) => Some(content),
            _ => None,
        },
    };
    let mut out = EcoString::new();
    if let Some(content) = field {
        write(content, shown, &mut out);
    }
    out.trim().into()
}

/// A title without the single full stop that ends it, as `title-stop:
/// drop` shows it (D-65-2): an ellipsis keeps its stops.
pub(super) fn drop_stop(title: EcoString) -> EcoString {
    match title.strip_suffix('.') {
        Some(rest) if !rest.ends_with('.') => rest.trim_end().into(),
        _ => title,
    }
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
                // A sum too large for an integer goes on as a float.
                None => match int.checked_add(*n) {
                    Some(sum) => int = sum,
                    None => float = Some(int as f64 + *n as f64),
                },
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

/// A sum as a total shows it: an integer as it is; a float rounded to
/// nine decimal places, so that the binary sum of decimals such as 0.1 and
/// 0.2 shows as 0.3, without trailing zeros, with the decimal marker.
pub fn format(sum: Sum, marker: char) -> EcoString {
    match sum {
        Sum::Int(n) => eco_format!("{n}"),
        Sum::Float(f) => {
            let text = eco_format!("{f:.9}");
            let text = if text.contains('.') {
                text.trim_end_matches('0').trim_end_matches('.')
            } else {
                text.as_str()
            };
            let text = if text == "-0" { "0" } else { text };
            text.replace('.', marker.encode_utf8(&mut [0; 4])).into()
        }
    }
}

/// An aggregate's value as the text shows it: a number as a total shows
/// it, a string as it is; `None` for a value that has no text in every
/// language, such as a boolean or a list.
pub fn show(value: &Plain, marker: char) -> Option<EcoString> {
    Some(match value {
        Plain::Int(n) => format(Sum::Int(*n), marker),
        Plain::Number(x) => format(Sum::Float(x.get()), marker),
        Plain::Str(text) => text.clone(),
        _ => return None,
    })
}

/// The warning for an aggregate that gives no value the text can show.
pub(super) fn no_value(query: Span, name: &QualifiedName) -> Diagnostic {
    Diagnostic::warning(
        query,
        eco_format!("aggregate `{name}` gives no value that can be shown, so it shows ??"),
    )
    .with_hint("its component gives no number or string for this document")
}

/// The warning for a total in a numbering group whose common tree could
/// not be made, as a variant of the group cannot be styled: it counts only
/// what its own variant shows.
pub(super) fn group_unavailable(query: Span, group: &str) -> Diagnostic {
    Diagnostic::warning(
        query,
        eco_format!(
            "a variant of numbering group `{group}` could not be styled, so this total counts \
             only what this variant shows"
        ),
    )
    .with_hint("fix the errors of the group's other variants")
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
