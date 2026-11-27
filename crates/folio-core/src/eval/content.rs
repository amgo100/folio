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

use std::fmt::{self, Debug, Formatter, Write};
use std::hash::{Hash, Hasher};

use ecow::{EcoString, EcoVec};
use folio_component::{ElementKind, Location, Name, QualifiedName};
use folio_syntax::{Span, SyntaxNode};

use super::value::{Dict, Equality, LazyHash, TooLong, Value, compare_once, write_str_repr};
use crate::locate::{Frame, Origin};

/// A sequence of content nodes.
///
/// It caches its depth ([`Content::depth`]), and its hash when it holds
/// content that holds other content. Equality, hashing and debug output are
/// those of its nodes.
#[derive(Clone, Default)]
pub struct Content {
    nodes: EcoVec<Node>,
    depth: u32,
    hash: LazyHash,
}

impl Content {
    /// No content.
    pub fn new() -> Self {
        Self::default()
    }

    /// Content of one node.
    pub fn node(node: Node) -> Self {
        let depth = node.depth();
        Self {
            depth,
            nodes: EcoVec::from([node]),
            hash: LazyHash::for_depth(depth),
        }
    }

    /// The nodes, in order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// How deeply the content nests: as deeply as its deepest node. Text,
    /// spaces and breaks are `0` deep, and an element or a slot one more
    /// than its deepest field or its body ([`Value::depth`]). Nested
    /// elements are therefore as deep as they nest: `strong[*x*]` is 2
    /// deep.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The number of nodes at the top level.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether there are no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Appends the nodes of other content.
    pub(crate) fn extend(&mut self, other: Content) {
        if self.nodes.is_empty() {
            *self = other;
        } else {
            self.depth = self.depth.max(other.depth);
            self.nodes.extend(other.nodes);
            self.changed();
        }
    }

    /// Forgets the hash after a change.
    fn changed(&mut self) {
        self.hash.reset(self.depth >= 2);
    }

    /// The element this content is, if it is one element.
    pub fn as_element(&self) -> Option<&Element> {
        match self.nodes.as_slice() {
            [Node::Element(element)] => Some(element),
            _ => None,
        }
    }

    /// Removes spaces at the end.
    pub(crate) fn trim_end_spaces(&mut self) {
        while matches!(self.nodes.last(), Some(Node::Space(_))) {
            self.nodes.pop();
            self.changed();
        }
    }

    /// The last element, skipping spaces, to attach a label to. Only its
    /// label may be changed, since the content caches its depth.
    pub(crate) fn last_element_mut(&mut self) -> Option<&mut Element> {
        let index = self
            .nodes
            .iter()
            .rposition(|node| !matches!(node, Node::Space(_)))?;
        if !matches!(self.nodes[index], Node::Element(_)) {
            return None;
        }
        self.changed();
        match &mut self.nodes.make_mut()[index] {
            Node::Element(element) => Some(element),
            _ => None,
        }
    }

    /// Its elements at the top level, to change. Only their locations may
    /// be changed, since the content caches its depth.
    pub(crate) fn elements_mut(&mut self) -> impl Iterator<Item = &mut Element> {
        self.changed();
        self.nodes
            .make_mut()
            .iter_mut()
            .filter_map(|node| match node {
                Node::Element(element) => Some(element),
                _ => None,
            })
    }

    /// Whether two contents are equal but for the origins of their
    /// elements, as `==` compares content: the loop iteration or call that
    /// made content does not make it other content, though it gives its
    /// elements other locations.
    pub(crate) fn same_but_origins(&self, other: &Self) -> bool {
        if self.nodes.as_ptr() == other.nodes.as_ptr() && self.len() == other.len() {
            return true;
        }
        if self.len() != other.len() {
            return false;
        }
        compare_once(
            self.nodes.as_ptr() as usize,
            other.nodes.as_ptr() as usize,
            Equality::ButOrigins,
            || {
                self.nodes
                    .iter()
                    .zip(other.nodes.iter())
                    .all(|(a, b)| match (a, b) {
                        (Node::Element(a), Node::Element(b)) => {
                            a.name == b.name
                                && a.kind == b.kind
                                && a.label == b.label
                                && a.span == b.span
                                && a.location == b.location
                                && dicts_same_but_origins(&a.fields, &b.fields)
                        }
                        (Node::Slot(a), Node::Slot(b)) => {
                            a.parent == b.parent
                                && a.name == b.name
                                && a.span == b.span
                                && a.body.same_but_origins(&b.body)
                        }
                        (a, b) => a == b,
                    })
            },
        )
    }

    /// Records that `frame`, an iteration or a call, made this content:
    /// on its elements at the top level, and those of its slots' bodies.
    /// Elements inside them take the frame from the element around them
    /// when the document model locates them ([`Origin::nested`]), so
    /// stamping costs one step per node at the top level, however deeply
    /// the content nests.
    pub(crate) fn stamp(&mut self, frame: Frame) {
        if !self
            .nodes
            .iter()
            .any(|node| matches!(node, Node::Element(_) | Node::Slot(_)))
        {
            return;
        }
        self.changed();
        for node in self.nodes.make_mut() {
            match node {
                Node::Element(element) => element.origin = element.origin.within(frame),
                Node::Slot(slot) => slot.body.stamp(frame),
                _ => {}
            }
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
        for node in &self.nodes {
            match node {
                Node::Text { text, .. } => out.push_str(text),
                Node::Space(_) | Node::Linebreak(_) | Node::Parbreak(_) => out.push(' '),
                Node::Quote { double, .. } => out.push(if *double { '"' } else { '\'' }),
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
        let mut out = EcoString::new();
        let _ = self.write_repr(&mut out, usize::MAX);
        out
    }

    /// Writes [`Content::repr`] to `out`, or fails as soon as `out` is
    /// longer than `limit` bytes ([`Value::repr_within`]).
    pub(crate) fn write_repr(&self, out: &mut EcoString, limit: usize) -> Result<(), TooLong> {
        out.push('[');
        for (i, node) in self.nodes.iter().enumerate() {
            if out.len() > limit {
                return Err(TooLong);
            }
            if i > 0 {
                out.push_str(", ");
            }
            match node {
                Node::Text { text, .. } => write_str_repr(out, text, limit)?,
                Node::Space(_) => out.push_str("space"),
                Node::Linebreak(_) => out.push_str("linebreak"),
                Node::Parbreak(_) => out.push_str("parbreak"),
                Node::Quote { double: true, .. } => out.push_str("quote(double)"),
                Node::Quote { double: false, .. } => out.push_str("quote(single)"),
                Node::Element(element) => {
                    write!(out, "{}(", element.name).unwrap();
                    for (j, (key, value)) in element.fields.iter().enumerate() {
                        if j > 0 {
                            out.push_str(", ");
                        }
                        write!(out, "{key}: ").unwrap();
                        value.write_repr(out, limit)?;
                    }
                    out.push(')');
                    if let Some(label) = &element.label {
                        write!(out, " <{label}>").unwrap();
                    }
                }
                Node::Slot(slot) => {
                    write!(out, "slot {}(", slot.name).unwrap();
                    slot.body.write_repr(out, limit)?;
                    out.push(')');
                }
                Node::Rule(_) => out.push_str("rule"),
            }
        }
        out.push(']');
        if out.len() > limit {
            return Err(TooLong);
        }
        Ok(())
    }
}

impl FromIterator<Node> for Content {
    fn from_iter<T: IntoIterator<Item = Node>>(iter: T) -> Self {
        let nodes: EcoVec<Node> = iter.into_iter().collect();
        let depth = nodes.iter().map(Node::depth).max().unwrap_or(0);
        Self {
            nodes,
            depth,
            hash: LazyHash::for_depth(depth),
        }
    }
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        let same = self.nodes.as_ptr() == other.nodes.as_ptr() && self.len() == other.len();
        if same || self.len() != other.len() || self.hash.differ(&other.hash) {
            return same;
        }
        if self.depth < 2 {
            return self.nodes == other.nodes;
        }
        compare_once(
            self.nodes.as_ptr() as usize,
            other.nodes.as_ptr() as usize,
            Equality::Strict,
            || self.nodes == other.nodes,
        )
    }
}

impl Eq for Content {}

impl Hash for Content {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash
            .write(state, |mut state| self.nodes.hash(&mut state));
    }
}

impl Debug for Content {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Content").field(&self.nodes).finish()
    }
}

/// Whether two values are equal but for the origins of the elements of the
/// content they hold ([`Content::same_but_origins`]).
fn same_but_origins(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Content(a), Value::Content(b)) => a.same_but_origins(b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len()
                && compare_once(a.storage(), b.storage(), Equality::ButOrigins, || {
                    a.iter().zip(b.iter()).all(|(a, b)| same_but_origins(a, b))
                })
        }
        (Value::Dict(a), Value::Dict(b)) => dicts_same_but_origins(a, b),
        (a, b) => a == b,
    }
}

/// Whether two dictionaries are equal but for the origins of the elements
/// of the content they hold.
fn dicts_same_but_origins(a: &Dict, b: &Dict) -> bool {
    a.len() == b.len()
        && compare_once(a.storage(), b.storage(), Equality::ButOrigins, || {
            a.iter()
                .zip(b.iter())
                .all(|((ka, va), (kb, vb))| ka == kb && same_but_origins(va, vb))
        })
}

/// Records that `frame`, an iteration or a call, made `value`: on the
/// content it is, or that an array or dictionary holds as an item. Content
/// nested deeper in data is not stamped, so that a call returning a large
/// table pays nothing for it; its instances are told apart by number.
pub(crate) fn stamp(value: &mut Value, frame: Frame) {
    match value {
        Value::Content(content) => content.stamp(frame),
        Value::Array(array) => {
            for i in 0..array.len() {
                if matches!(array[i], Value::Content(_)) {
                    array.update(i, |item| stamp(item, frame));
                }
            }
        }
        Value::Dict(dict) => {
            let keys: Vec<EcoString> = dict
                .iter()
                .filter(|(_, v)| matches!(v, Value::Content(_)))
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                dict.update(&key, false, |v| stamp(v, frame));
            }
        }
        _ => {}
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
    /// A space between words, with the whitespace it was written as.
    Space(Span),
    /// A forced line break: `\`.
    Linebreak(Span),
    /// A paragraph break: a blank line.
    Parbreak(Span),
    /// A quotation mark that the target language's quotes replace.
    Quote {
        /// Whether it is a double quotation mark.
        double: bool,
        /// The mark as written.
        span: Span,
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

impl Node {
    /// How deeply the node nests: one more than an element's deepest field
    /// or a slot's body, and `0` for any other node.
    pub fn depth(&self) -> u32 {
        match self {
            Self::Element(element) => element.fields.depth(),
            Self::Slot(slot) => slot.body.depth().saturating_add(1),
            _ => 0,
        }
    }
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
    /// The fields given, and the defaults of the declared fields that were
    /// not given and do not default to `none`, in the schema's canonical
    /// order whatever order a call gave them in (SPEC §4.3): the
    /// positional fields, with the body where they place it, the other
    /// declared fields, the body if they do not place it, then the
    /// universal fields. The body is the field `body`, and the content of
    /// each slot a field of the slot's name, in the slots' order. A field
    /// limited to a variant or target is stored under its name and scope,
    /// `override@markscheme`, after the field without one. Universal fields
    /// are stored only when given; `class` as an array of names, and `id`
    /// as the label.
    pub fields: Dict,
    /// The label written after it: `fig:ball` for `<fig:ball>`.
    pub label: Option<EcoString>,
    /// Where it was made: the markup or the call.
    pub span: Span,
    /// Which instance of the document tree it is (SPEC §1.5): the document
    /// model gives every element of the tree its location
    /// ([`crate::locate`]); evaluated content, which one call may place
    /// several times, has none yet ([`Location::DETACHED`]).
    pub location: Location,
    /// The loop iterations and closure calls that made it, as evaluation
    /// recorded them (`Content::stamp`); the document model builds its
    /// location from it.
    pub origin: Origin,
}

impl Element {
    /// The value of a field.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content holding `inner` twice, each time in a `strong` element.
    fn doubled(inner: &Content) -> Content {
        let strong = |body: &Content| {
            let mut fields = Dict::new();
            fields.insert("body".into(), Value::Content(body.clone()));
            Node::Element(Element {
                name: QualifiedName::unqualified(Name::new("strong").unwrap()),
                kind: ElementKind::Inline,
                fields,
                label: None,
                span: Span::detached(),
                location: Location::DETACHED,
                origin: Origin::ROOT,
            })
        };
        [strong(inner), strong(inner)].into_iter().collect()
    }

    #[test]
    fn stamps_reach_top_level_elements_only() {
        let leaf = Content::node(Node::Text {
            text: "x".into(),
            span: Span::detached(),
        });
        let content = doubled(&doubled(&leaf));
        let frame = Frame::Call {
            span: Span::detached(),
        };
        let mut value = Value::Array([Value::Content(content.clone()), Value::Int(1)].into());
        stamp(&mut value, frame);
        let Value::Array(array) = &value else {
            unreachable!()
        };
        let Value::Content(stamped) = &array[0] else {
            unreachable!()
        };
        let origins = |content: &Content| -> Vec<Origin> {
            content
                .nodes()
                .iter()
                .filter_map(|node| match node {
                    Node::Element(element) => Some(element.origin),
                    _ => None,
                })
                .collect()
        };
        let within = Origin::ROOT.within(frame);
        assert_eq!(origins(stamped), [within, within]);
        assert_ne!(stamped, &content);
        // The elements inside take the frame from the element around
        // them when they are located: they keep their own origin.
        let Node::Element(first) = &stamped.nodes()[0] else {
            unreachable!()
        };
        let Some(Value::Content(inner)) = first.field("body") else {
            unreachable!()
        };
        assert_eq!(origins(inner), [Origin::ROOT, Origin::ROOT]);
    }

    #[test]
    fn content_built_alike_compares_once_per_part() {
        // Two contents built apart, each a tree of 2^60 elements.
        let text = |t: &str| {
            Content::node(Node::Text {
                text: t.into(),
                span: Span::detached(),
            })
        };
        let (mut a, mut b, mut c) = (text("x"), text("x"), text("y"));
        for _ in 0..60 {
            a = doubled(&a);
            b = doubled(&b);
            c = doubled(&c);
        }
        assert_eq!(a, b);
        assert_ne!(a, c);
        // A label changes the content, and its hash with it.
        let hash = |content: &Content| {
            let mut state = std::hash::DefaultHasher::new();
            content.hash(&mut state);
            state.finish()
        };
        let before = hash(&b);
        b.last_element_mut().unwrap().label = Some("l".into());
        assert_ne!(hash(&b), before);
        assert_ne!(a, b);
    }
}
