//! The content tree: what evaluating markup produces (SPEC §2.1).
//!
//! Content is a sequence of [`Node`]s: text, spaces and breaks, and
//! [`Element`] instances with typed fields. Markup produces built-in
//! elements with the names of SPEC §4.2 (`heading`, `strong`, `raw`,
//! `list-item`…); element calls produce instances of built-in or component
//! elements. Either way the instance is checked against its schema when it
//! is made, so its fields have their declared types and defaults. Grouping
//! inline content into paragraphs and list items into lists, and opening
//! sections at headings, belong to the document model
//! ([`crate::model`]), which reads this tree.

use std::fmt::Write;

use ecow::{EcoString, EcoVec};
use folio_component::{ElementKind, Name, QualifiedName};
use folio_syntax::{Span, SyntaxNode};

use super::value::{Dict, Value};

/// A sequence of content nodes.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Content(EcoVec<Node>);

impl Content {
    /// No content.
    pub fn new() -> Self {
        Self::default()
    }

    /// Content of one node.
    pub fn node(node: Node) -> Self {
        Self(EcoVec::from([node]))
    }

    /// The nodes, in order.
    pub fn nodes(&self) -> &[Node] {
        &self.0
    }

    /// The number of nodes at the top level.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no nodes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Appends the nodes of other content.
    pub(crate) fn extend(&mut self, other: Content) {
        if self.0.is_empty() {
            *self = other;
        } else {
            self.0.extend(other.0);
        }
    }

    /// The element this content is, if it is one element.
    pub fn as_element(&self) -> Option<&Element> {
        match self.0.as_slice() {
            [Node::Element(element)] => Some(element),
            _ => None,
        }
    }

    /// Removes spaces at the end.
    pub(crate) fn trim_end_spaces(&mut self) {
        while matches!(self.0.last(), Some(Node::Space)) {
            self.0.pop();
        }
    }

    /// The last element, skipping spaces, to attach a label to.
    pub(crate) fn last_element_mut(&mut self) -> Option<&mut Element> {
        let index = self
            .0
            .iter()
            .rposition(|node| !matches!(node, Node::Space))?;
        match &mut self.0.make_mut()[index] {
            Node::Element(element) => Some(element),
            _ => None,
        }
    }

    /// The text of the content without markup, as a test or a lint reads
    /// it: text and the bodies of elements, with spaces and breaks as one
    /// space.
    pub fn plain_text(&self) -> EcoString {
        let mut out = EcoString::new();
        self.write_plain(&mut out);
        out
    }

    fn write_plain(&self, out: &mut EcoString) {
        for node in &self.0 {
            match node {
                Node::Text { text, .. } => out.push_str(text),
                Node::Space | Node::Linebreak | Node::Parbreak => out.push(' '),
                Node::Quote { double } => out.push(if *double { '"' } else { '\'' }),
                Node::Element(element) => {
                    for (_, value) in element.fields.iter() {
                        if let Value::Content(body) = value {
                            body.write_plain(out);
                        }
                    }
                }
                Node::Slot(slot) => slot.body.write_plain(out),
                Node::Rule(_) => {}
            }
        }
    }

    /// A compact description of the tree, for snapshot tests and debugging:
    /// `[heading(level: 2, body: [Forces]) <sec>, parbreak, "A ball"]`.
    pub fn repr(&self) -> EcoString {
        let mut out = EcoString::from("[");
        for (i, node) in self.0.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match node {
                Node::Text { text, .. } => write!(out, "{text:?}").unwrap(),
                Node::Space => out.push_str("space"),
                Node::Linebreak => out.push_str("linebreak"),
                Node::Parbreak => out.push_str("parbreak"),
                Node::Quote { double: true } => out.push_str("quote(double)"),
                Node::Quote { double: false } => out.push_str("quote(single)"),
                Node::Element(element) => {
                    write!(out, "{}(", element.name).unwrap();
                    for (j, (key, value)) in element.fields.iter().enumerate() {
                        if j > 0 {
                            out.push_str(", ");
                        }
                        write!(out, "{key}: {}", value.repr()).unwrap();
                    }
                    out.push(')');
                    if let Some(label) = &element.label {
                        write!(out, " <{label}>").unwrap();
                    }
                }
                Node::Slot(slot) => {
                    write!(out, "slot {}({})", slot.name, slot.body.repr()).unwrap()
                }
                Node::Rule(_) => out.push_str("rule"),
            }
        }
        out.push(']');
        out
    }
}

impl FromIterator<Node> for Content {
    fn from_iter<T: IntoIterator<Item = Node>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// A node of the content tree.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Node {
    /// A run of text.
    Text {
        /// The text.
        text: EcoString,
        /// Where it came from.
        span: Span,
    },
    /// A space between words.
    Space,
    /// A forced line break: `\`.
    Linebreak,
    /// A paragraph break: a blank line.
    Parbreak,
    /// A quotation mark that the target language's quotes replace.
    Quote {
        /// Whether it is a double quotation mark.
        double: bool,
    },
    /// An element instance.
    Element(Element),
    /// A slot's sub-block, `#statement[..]`, on its way into the body of
    /// the element that declares the slot (SPEC §4.3). That element stores
    /// it as a field, so it occurs in no finished element.
    Slot(Slot),
    /// A scoped rule (`#rule question { .. }`, SPEC §5.4), kept as syntax
    /// for the style stage. It applies to the enclosing element.
    Rule(SyntaxNode),
}

/// A slot's sub-block: `#solution[..]` in the body of `#example[..]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Slot {
    /// The element whose slot it fills.
    pub parent: QualifiedName,
    /// The slot.
    pub name: Name,
    /// Its content.
    pub body: Content,
    /// Where it was made.
    pub span: Span,
}

/// An element instance: a name, a kind, typed fields and an optional
/// label.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Element {
    /// The element: a built-in name such as `heading`, or a component
    /// element, qualified with its component's namespace (`exam.question`).
    pub name: QualifiedName,
    /// How layout treats this instance: its schema's kind, except that
    /// `raw` and `math` are blocks when their `block` field is true.
    pub kind: ElementKind,
    /// The fields given, in the order they were given, then the defaults of
    /// the declared fields that were not given and do not default to
    /// `none`. The body is the field `body`, and the content of each slot a
    /// field of the slot's name. A field limited to a variant or target is
    /// stored under its name and scope: `override@markscheme`. Universal
    /// fields are stored only when given; `class` as an array of names, and
    /// `id` as the label.
    pub fields: Dict,
    /// The label written after it: `fig:ball` for `<fig:ball>`.
    pub label: Option<EcoString>,
    /// Where it was made: the markup or the call.
    pub span: Span,
}

impl Element {
    /// The value of a field.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.get(name)
    }
}
