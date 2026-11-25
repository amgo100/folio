//! The document tree: the evaluated content with its structure (SPEC §4.2).

use std::collections::BTreeMap;
use std::fmt::Write;

use ecow::{EcoString, eco_format};
use folio_component::{BODY, Body, ContentKind, ElementKind, FieldType, Name, QualifiedName};
use folio_syntax::Span;

use super::{is_block, schema};
use crate::db::{Db, Project, source_file};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Dict, Element, Node, Value, canonical_fields, evaluate};

/// The typed document tree of one variant (SPEC §2.1): what every stage
/// after evaluation reads.
///
/// Its root is a `doc` element, with the fields of the document's `#doc`
/// call and the rest of the document as its body. Every element is an
/// instance checked against its schema. Bodies and slots that hold blocks
/// hold only block elements and scoped rules: inline content between blocks
/// is grouped into `paragraph`s (a float written in a paragraph's text stays
/// in it as an anchor), consecutive list items into a `list`,
/// `enum` or `terms`, and each heading with the blocks after it, up to the
/// next heading of the same or a higher level, into a `section`. Inline
/// content has no spaces at its ends and no two spaces in a row.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Document {
    /// The `doc` element.
    pub root: Element,
}

impl Document {
    /// The tree as indented text, one block per line, for snapshot tests
    /// and debugging. Inline content is written as
    /// [`Content::repr`] writes it; content of blocks is written below its
    /// element, indented, after a line with its field's name unless it is
    /// the body.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        write_element(&mut out, &self.root, 0);
        out
    }
}

/// The document tree of a variant (`None` for a project without variants),
/// or `None` when the document could not be evaluated. Reports misplaced
/// `#doc` calls, labels used twice and references to labels that no
/// element has.
#[salsa::tracked(returns(ref))]
pub fn document(db: &dyn Db, project: Project, variant: Option<Name>) -> Option<Document> {
    let content = evaluate(db, project, variant).as_ref()?;
    let entry = project.manifest(db).project.entry.v.clone();
    let span = source_file(db, project, entry)
        .map_or(Span::detached(), |file| file.source(db).root().span());
    let mut model = Model { db, project };
    let root = model.root(content, span);
    check_labels(db, &root);
    Some(Document { root })
}

struct Model<'a> {
    db: &'a dyn Db,
    project: Project,
}

impl Model<'_> {
    /// Makes the `doc` element from the entry file's content.
    fn root(&mut self, content: &Content, span: Span) -> Element {
        let mut doc: Option<Element> = None;
        let mut rest = Content::new();
        for node in content.nodes() {
            match node {
                Node::Element(element) if is_doc(&element.name) => {
                    if let Some(first) = &doc {
                        diag::report(
                            self.db,
                            Diagnostic::error(element.span, "the document has `#doc` twice")
                                .with_related(first.span, "it is first set here")
                                .with_hint("one `#doc(..)` call sets the document's fields"),
                        );
                    } else if element.field(BODY).is_some() {
                        diag::report(
                            self.db,
                            Diagnostic::error(element.span, "`#doc` takes no body")
                                .with_hint("the document's content is what follows `#doc(..)`"),
                        );
                    }
                    doc.get_or_insert_with(|| element.clone());
                }
                _ => rest.extend(Content::node(node.clone())),
            }
        }
        let mut root = doc.unwrap_or_else(|| Element {
            name: doc_name(),
            kind: ElementKind::Block,
            fields: Dict::new(),
            label: None,
            span,
        });
        root.fields.remove(BODY);
        let body = self.blocks(&rest);
        root.fields.insert(BODY.into(), Value::Content(body));
        if let Some(schema) = schema(self.db, self.project, &root.name) {
            root.fields = canonical_fields(schema, root.fields);
        }
        self.fields(root)
    }

    /// Gives an element's content its structure: the content of the body,
    /// the slots and the content fields. The root's body is done already.
    fn element(&mut self, element: &Element) -> Element {
        if is_doc(&element.name) {
            diag::report(
                self.db,
                Diagnostic::error(
                    element.span,
                    "`#doc` belongs at the top level of the document",
                )
                .with_hint("it sets the fields of the whole document"),
            );
        }
        let mut element = element.clone();
        let Some(schema) = schema(self.db, self.project, &element.name) else {
            return element;
        };
        match &schema.body {
            Body::None => {}
            Body::Content(kind) => {
                if let Some(Value::Content(body)) = element.fields.get(BODY) {
                    let body = self.content(body, *kind);
                    element.fields.insert(BODY.into(), Value::Content(body));
                }
            }
            Body::Slots(slots) => {
                for slot in slots {
                    if let Some(Value::Content(body)) = element.fields.get(slot.name.as_str()) {
                        let body = self.content(body, slot.content);
                        element
                            .fields
                            .insert(slot.name.as_str().into(), Value::Content(body));
                    }
                }
            }
        }
        self.fields(element)
    }

    /// Gives the content fields of an element their structure: they hold
    /// inline content.
    fn fields(&mut self, mut element: Element) -> Element {
        let Some(schema) = schema(self.db, self.project, &element.name) else {
            return element;
        };
        for decl in &schema.fields {
            if let Some(value) = element.fields.get(decl.name.as_str()) {
                let value = self.value(&decl.ty, value);
                element.fields.insert(decl.name.as_str().into(), value);
            }
        }
        element
    }

    fn value(&mut self, ty: &FieldType, value: &Value) -> Value {
        match (ty, value) {
            (FieldType::Content, Value::Content(content)) => Value::Content(self.inline(content)),
            (FieldType::Array(item), Value::Array(items)) => {
                Value::Array(items.iter().map(|v| self.value(item, v)).collect())
            }
            (FieldType::Dict(item), Value::Dict(dict)) => {
                let mut out = Dict::new();
                for (key, v) in dict.iter() {
                    out.insert(key.clone(), self.value(item, v));
                }
                Value::Dict(out)
            }
            _ => value.clone(),
        }
    }

    fn content(&mut self, content: &Content, kind: ContentKind) -> Content {
        match kind {
            ContentKind::Inline => self.inline(content),
            ContentKind::Blocks => self.blocks(content),
        }
    }

    /// Inline content: spaces trimmed at the ends and collapsed between
    /// words, and the content of its elements structured.
    fn inline(&mut self, content: &Content) -> Content {
        let mut out: Vec<Node> = Vec::new();
        for node in content.nodes() {
            match node {
                Node::Space(_) if matches!(out.last(), None | Some(Node::Space(_))) => {}
                Node::Element(element) => out.push(Node::Element(self.element(element))),
                // Checked when the element was made; reached only by
                // content that no element holds.
                Node::Slot(slot) => self.misplaced(slot),
                other => out.push(other.clone()),
            }
        }
        while matches!(out.last(), Some(Node::Space(_))) {
            out.pop();
        }
        out.into_iter().collect()
    }

    /// Blocks: paragraphs, lists and sections made, and the content of
    /// every element structured.
    fn blocks(&mut self, content: &Content) -> Content {
        let mut blocks = Blocks::default();
        for node in content.nodes() {
            match node {
                Node::Parbreak(_) => blocks.end_paragraph(),
                Node::Space(_) if blocks.run.is_empty() => {}
                Node::Rule(_) => blocks.rules.push(node.clone()),
                Node::Slot(slot) => self.misplaced(slot),
                Node::Element(element) => {
                    let element = self.element(element);
                    // A float written in a paragraph's text stays there as
                    // its anchor (SPEC §7.5, §7.7).
                    let anchor = element.kind == ElementKind::Float && !blocks.run.is_empty();
                    if let Some(list) = list_of(&element.name) {
                        blocks.item(list, element);
                    } else if is_block(element.kind) && !anchor {
                        blocks.block(Node::Element(element));
                    } else {
                        blocks.inline(Node::Element(element));
                    }
                }
                other => blocks.inline(other.clone()),
            }
        }
        blocks.finish()
    }

    fn misplaced(&mut self, slot: &crate::eval::Slot) {
        diag::report(
            self.db,
            Diagnostic::error(
                slot.span,
                eco_format!(
                    "`#{}` fills a slot of `{}`, so it belongs directly in its body",
                    slot.name,
                    slot.parent
                ),
            ),
        );
    }
}

/// Blocks being built from a flow of content.
#[derive(Default)]
struct Blocks {
    /// The blocks so far, with sections not yet made.
    out: Vec<Node>,
    /// The scoped rules. They apply to the enclosing element, so they stay
    /// in its content, ahead of its blocks, rather than in a section.
    rules: Vec<Node>,
    /// The inline content of the paragraph being built.
    run: Vec<Node>,
    /// The list being built: its element name, items and span.
    list: Option<(&'static str, Vec<Node>, Span)>,
}

impl Blocks {
    fn inline(&mut self, node: Node) {
        self.end_list();
        if matches!(node, Node::Space(_)) && matches!(self.run.last(), Some(Node::Space(_))) {
            return;
        }
        self.run.push(node);
    }

    fn block(&mut self, node: Node) {
        self.end_paragraph();
        self.end_list();
        self.out.push(node);
    }

    /// Adds a list item, to the list being built if it is of the same kind:
    /// spaces and paragraph breaks between items do not end a list.
    fn item(&mut self, list: &'static str, item: Element) {
        self.end_paragraph();
        match &mut self.list {
            Some((name, items, _)) if *name == list => items.push(Node::Element(item)),
            _ => {
                self.end_list();
                let span = item.span;
                self.list = Some((list, vec![Node::Element(item)], span));
            }
        }
    }

    fn end_paragraph(&mut self) {
        while matches!(self.run.last(), Some(Node::Space(_))) {
            self.run.pop();
        }
        if self.run.is_empty() {
            return;
        }
        let run = std::mem::take(&mut self.run);
        let span = run.iter().find_map(node_span).unwrap_or(Span::detached());
        self.out.push(Node::Element(made(
            "paragraph",
            [(BODY, Value::Content(run.into_iter().collect()))],
            span,
        )));
    }

    fn end_list(&mut self) {
        if let Some((name, items, span)) = self.list.take() {
            self.out.push(Node::Element(made(
                name,
                [(BODY, Value::Content(items.into_iter().collect()))],
                span,
            )));
        }
    }

    fn finish(mut self) -> Content {
        self.end_paragraph();
        self.end_list();
        let mut out = self.rules;
        out.extend(sections(self.out));
        out.into_iter().collect()
    }
}

/// Opens a section at each heading, holding it and the blocks after it up
/// to the next heading of the same or a higher level.
fn sections(blocks: Vec<Node>) -> Vec<Node> {
    let mut top = Vec::new();
    // The open sections, outermost first: level, content, span.
    let mut open: Vec<(i64, Vec<Node>, Span)> = Vec::new();
    let close = |open: &mut Vec<(i64, Vec<Node>, Span)>, top: &mut Vec<Node>| {
        let (level, content, span) = open.pop().expect("a section is open");
        let section = Node::Element(made(
            "section",
            [
                ("level", Value::Int(level)),
                (BODY, Value::Content(content.into_iter().collect())),
            ],
            span,
        ));
        match open.last_mut() {
            Some((_, parent, _)) => parent.push(section),
            None => top.push(section),
        }
    };
    for node in blocks {
        if let Node::Element(element) = &node
            && element.name.namespace.is_none()
            && element.name.name.as_str() == "heading"
        {
            let level = match element.field("level") {
                Some(Value::Int(level)) => *level,
                _ => 1,
            };
            while open.last().is_some_and(|(l, _, _)| *l >= level) {
                close(&mut open, &mut top);
            }
            let span = element.span;
            open.push((level, vec![node], span));
            continue;
        }
        match open.last_mut() {
            Some((_, content, _)) => content.push(node),
            None => top.push(node),
        }
    }
    while !open.is_empty() {
        close(&mut open, &mut top);
    }
    top
}

/// An element that the document model makes: `paragraph`, `section` and
/// the lists.
fn made<'f>(name: &str, fields: impl IntoIterator<Item = (&'f str, Value)>, span: Span) -> Element {
    Element {
        name: QualifiedName::unqualified(Name::new(name).expect("built-in names are identifiers")),
        kind: ElementKind::Block,
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        label: None,
        span,
    }
}

/// The list that holds an item of a built-in kind.
fn list_of(name: &QualifiedName) -> Option<&'static str> {
    if name.namespace.is_some() {
        return None;
    }
    match name.name.as_str() {
        "list-item" => Some("list"),
        "enum-item" => Some("enum"),
        "terms-item" => Some("terms"),
        _ => None,
    }
}

fn is_doc(name: &QualifiedName) -> bool {
    name.namespace.is_none() && name.name.as_str() == "doc"
}

fn doc_name() -> QualifiedName {
    QualifiedName::unqualified(Name::new("doc").expect("built-in names are identifiers"))
}

fn node_span(node: &Node) -> Option<Span> {
    match node {
        Node::Text { span, .. } => Some(*span),
        Node::Element(element) => Some(element.span),
        Node::Slot(slot) => Some(slot.span),
        Node::Rule(rule) => Some(rule.span()),
        Node::Space(_) | Node::Linebreak(_) | Node::Parbreak(_) | Node::Quote { .. } => None,
    }
}

/// Reports labels that more than one element has: a reference to one could
/// not say which it means; and references to a label that no element has.
fn check_labels(db: &dyn Db, root: &Element) {
    struct Walk<'a> {
        db: &'a dyn Db,
        seen: BTreeMap<EcoString, Span>,
        /// The references, with the label each names, in document order.
        refs: Vec<(&'a EcoString, Span)>,
    }
    impl<'a> Walk<'a> {
        fn element(&mut self, element: &'a Element) {
            if let Some(label) = &element.label {
                if let Some(first) = self.seen.get(label) {
                    diag::report(
                        self.db,
                        Diagnostic::error(
                            element.span,
                            eco_format!("label `<{label}>` is used twice"),
                        )
                        .with_related(*first, "it is first used here")
                        .with_hint("a label names one element, so references can find it"),
                    );
                } else {
                    self.seen.insert(label.clone(), element.span);
                }
            }
            if element.name.namespace.is_none()
                && element.name.name.as_str() == "ref"
                && let Some(Value::Label(target)) = element.field("target")
            {
                self.refs.push((target, element.span));
            }
            for (_, value) in element.fields.iter() {
                self.value(value);
            }
        }
        fn value(&mut self, value: &'a Value) {
            match value {
                Value::Content(content) => {
                    for node in content.nodes() {
                        if let Node::Element(element) = node {
                            self.element(element);
                        }
                    }
                }
                Value::Array(items) => items.iter().for_each(|v| self.value(v)),
                Value::Dict(dict) => dict.iter().for_each(|(_, v)| self.value(v)),
                _ => {}
            }
        }
    }
    let mut walk = Walk {
        db,
        seen: BTreeMap::new(),
        refs: Vec::new(),
    };
    walk.element(root);
    let labels: Vec<&str> = walk.seen.keys().map(EcoString::as_str).collect();
    for (target, span) in walk.refs {
        if walk.seen.contains_key(target) {
            continue;
        }
        let diagnostic = Diagnostic::warning(
            span,
            eco_format!(
                "no element has the label `<{target}>`, so the reference has nothing to refer to"
            ),
        );
        let diagnostic = match crate::project::suggest(target, &labels) {
            Some(close) => diagnostic.with_hint(eco_format!("did you mean `<{close}>`?")),
            None => diagnostic
                .with_hint("label the element the reference means, such as `= Forces <forces>`"),
        };
        diag::report(db, diagnostic);
    }
}

/// Writes an element and the blocks it holds, indented by `depth`.
fn write_element(out: &mut String, element: &Element, depth: usize) {
    let indent = "  ".repeat(depth);
    write!(out, "{indent}{}", element.name).unwrap();
    let mut nested = Vec::new();
    let mut inline = Vec::new();
    for (key, value) in element.fields.iter() {
        match value {
            Value::Content(content) if is_blocks(content) => nested.push((key, content)),
            _ => inline.push((key, value)),
        }
    }
    if !inline.is_empty() {
        out.push('(');
        for (i, (key, value)) in inline.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write!(out, "{key}: {}", value.repr()).unwrap();
        }
        out.push(')');
    }
    if let Some(label) = &element.label {
        write!(out, " <{label}>").unwrap();
    }
    out.push('\n');
    for (key, content) in nested {
        let mut depth = depth + 1;
        if key != BODY {
            writeln!(out, "{}{key}:", "  ".repeat(depth)).unwrap();
            depth += 1;
        }
        for node in content.nodes() {
            match node {
                Node::Element(element) => write_element(out, element, depth),
                _ => writeln!(out, "{}rule", "  ".repeat(depth)).unwrap(),
            }
        }
    }
}

/// Whether content is a non-empty sequence of blocks.
fn is_blocks(content: &Content) -> bool {
    !content.is_empty()
        && content.nodes().iter().all(|node| match node {
            Node::Element(element) => is_block(element.kind),
            Node::Rule(_) => true,
            _ => false,
        })
}
