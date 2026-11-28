//! The Select stage (SPEC §2.1, §6.2, §6.3): what a variant shows of the
//! document.
//!
//! Select reads the computed `display` and `show-only` of every node of the
//! styled tree, and the conditions of `#in-variant` and `#except-variant`,
//! and makes the document tree and the styled tree that the later stages
//! read:
//!
//! - **Removed**: an element or part whose `display` is `none`, an
//!   `#in-variant` of another variant and an `#except-variant` of this one
//!   are left out of both trees, with everything they hold, so no later
//!   stage counts, sets or emits them (SPEC §6.4). A paragraph, list or
//!   section that the document model made and that holds nothing once they
//!   are gone goes too, and the spaces around a removed inline element
//!   collapse as they would had it never been written.
//! - **Replaced**: `placeholder("…")` keeps the element, which is still
//!   counted and styled, and replaces all of its content with the text.
//! - **Condensed**: `show-only: answer mark` keeps, of an element's
//!   content, the children (elements of the body, or parts) with those
//!   names, whole, and the children that hold one, condensed in the same
//!   way; text and every other child are left out.
//! - **Shown**: `block`, `inline` and `auto` show the node as it is.
//!
//! `hidden` is to keep the space of the content without drawing it, and
//! `answer-space` to put answer space in its place; until layout measures
//! content it does not draw, both leave the content out, with a warning,
//! so that it never reaches the output ([`Select::answer_space`] is where
//! answer space plugs in).
//!
//! Select copies only what changes: an element whose subtree it leaves as
//! it is keeps its content shared with the document tree, and a document
//! that nothing hides is passed on unchanged ([`Selection::root`]).

use std::collections::{HashMap, HashSet};

use ecow::{EcoString, eco_format};
use folio_component::{BODY, Location, Name, QualifiedName, Value as Style};
use folio_syntax::Span;

use crate::db::Db;
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::locate::StyledChildren;
use crate::project::Manifest;
use crate::style::{NOTE_PART, NodeName, PropId, Properties, StyledNode, VECTOR_PART};

/// What Select made of a document.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Selection {
    /// The `doc` element with what the variant does not show left out, or
    /// `None` when it shows everything, so the document tree is the
    /// selected one.
    pub root: Option<Element>,
    /// The `doc` element's node of the styled tree, with the same nodes
    /// left out.
    pub styled: StyledNode,
}

/// Selects what a variant shows of a document (SPEC §6.2, §6.3): `root` is
/// the `doc` element of the document tree, `node` its styled node.
/// Reports content that is left out where it should keep its space or
/// become answer space, which no stage lays out yet, and variant
/// conditions that name no declared variant.
pub fn select(
    db: &dyn Db,
    manifest: &Manifest,
    variant: Option<&Name>,
    props: &Properties,
    root: &Element,
    node: &StyledNode,
) -> Selection {
    let mut select = Select {
        db,
        manifest,
        variant,
        props,
        display: props.builtin("display"),
        show_only: props.builtin("show-only"),
        warned: HashSet::new(),
    };
    match select.element(root, node, None) {
        Out::Same | Out::Removed => Selection {
            root: None,
            styled: node.clone(),
        },
        Out::Changed(changed) => {
            let (root, styled) = *changed;
            Selection {
                root: Some(root),
                styled,
            }
        }
    }
}

/// What Select does with an element or part.
enum Shown {
    /// Left out.
    Removed,
    /// Its content replaced by a text.
    Placeholder(EcoString),
    /// Shown, condensed if it has `show-only`.
    Kept,
}

/// What Select made of an element.
enum Out {
    /// The element as it was.
    Same,
    /// Nothing: the element is left out.
    Removed,
    /// The element with some of its content left out or replaced, and its
    /// styled node with the same nodes left out.
    Changed(Box<(Element, StyledNode)>),
}

/// The names that `show-only` keeps.
struct Only(Vec<EcoString>);

impl Only {
    /// Whether an element is one of the names: by its name, or qualified
    /// with its component's namespace (`exam.answer`).
    fn element(&self, name: &QualifiedName) -> bool {
        self.0
            .iter()
            .any(|n| n.as_str() == name.name.as_str() || *n == name.to_string())
    }

    /// Whether a part is one of the names.
    fn part(&self, name: &str) -> bool {
        self.0.iter().any(|n| n.as_str() == name)
    }

    /// Whether content holds an element of one of the names, or an element
    /// with a part of one of them, at any depth.
    fn within(&self, content: &Content) -> bool {
        content.nodes().iter().any(|node| match node {
            Node::Element(element) => self.element(&element.name) || self.holds(element),
            _ => false,
        })
    }

    /// Whether an element holds one of the names, at any depth.
    fn holds(&self, element: &Element) -> bool {
        element.fields.iter().any(|(key, value)| match value {
            Value::Content(content) => (key != BODY && self.part(key)) || self.within(content),
            _ => false,
        })
    }
}

/// The nodes that content left out or replaced, for rebuilding the styled
/// node that holds them.
#[derive(Default)]
struct Edits {
    /// The elements left out, by location.
    removed: HashSet<Location>,
    /// The elements replaced, by location, with their new nodes.
    replaced: HashMap<Location, StyledNode>,
    /// The parts left out (`None`) or replaced, by name.
    parts: HashMap<EcoString, Option<StyledNode>>,
}

impl Edits {
    /// `node` with the edits made to its children.
    fn apply(&self, node: &StyledNode) -> StyledNode {
        let children = node
            .children
            .iter()
            .filter_map(|child| match &child.name {
                NodeName::Element(_) if self.removed.contains(&child.location) => None,
                NodeName::Element(_) => {
                    Some(self.replaced.get(&child.location).unwrap_or(child).clone())
                }
                NodeName::Part(name) => match self.parts.get(name.as_str()) {
                    Some(None) => None,
                    Some(Some(new)) => Some(new.clone()),
                    None => Some(child.clone()),
                },
                NodeName::Page => Some(child.clone()),
            })
            .collect();
        StyledNode {
            children,
            ..node.clone()
        }
    }
}

struct Select<'a> {
    db: &'a dyn Db,
    manifest: &'a Manifest,
    variant: Option<&'a Name>,
    props: &'a Properties,
    display: PropId,
    show_only: PropId,
    /// The display modes already warned of in this build.
    warned: HashSet<&'static str>,
}

impl Select<'_> {
    /// Selects an element. `only` is the `show-only` of an element around
    /// it that condenses it, when the element is not one of its names.
    fn element(&mut self, element: &Element, node: &StyledNode, only: Option<&Only>) -> Out {
        match self.shown(Some(element), node) {
            Shown::Removed => return Out::Removed,
            Shown::Placeholder(text) => {
                return Out::Changed(Box::new((
                    placeholder(element, text),
                    placeholder_node(node),
                )));
            }
            Shown::Kept => {}
        }
        let own = self.show_only(node);
        let only = own.as_ref().or(only);
        let mut fields = None;
        let mut edits = Edits::default();
        let mut body_kids = StyledChildren::new(node);
        let mut part_kids = StyledChildren::new(node);
        for (key, value) in element.fields.iter() {
            let Value::Content(content) = value else {
                continue;
            };
            let new = if key == BODY {
                self.content(content, &mut body_kids, only, &mut edits)
                    .map(Some)
            } else {
                let Some(name) = Name::new(key) else {
                    continue;
                };
                let Some(part) = part_kids.part(&name) else {
                    continue;
                };
                self.part(key, content, part, only, &mut edits)
            };
            if let Some(new) = new {
                let fields = fields.get_or_insert_with(|| element.fields.clone());
                match new {
                    Some(content) => fields.insert(key.clone(), Value::Content(content)),
                    None => {
                        fields.remove(key);
                    }
                }
            }
        }
        let Some(fields) = fields else {
            return Out::Same;
        };
        // What the document model made to hold content goes when nothing
        // is left in it.
        if made(element) && is_empty(&fields) {
            return Out::Removed;
        }
        let element = Element {
            fields,
            ..element.clone()
        };
        Out::Changed(Box::new((element, edits.apply(node))))
    }

    /// Selects a part: `None` if it is as it was, `Some(None)` if it is
    /// left out, and `Some(Some(content))` if its content changed.
    fn part(
        &mut self,
        key: &EcoString,
        content: &Content,
        node: &StyledNode,
        only: Option<&Only>,
        edits: &mut Edits,
    ) -> Option<Option<Content>> {
        let only = match only {
            Some(only) if only.part(key) => None,
            Some(only) if only.within(content) => Some(only),
            Some(_) => {
                edits.parts.insert(key.clone(), None);
                return Some(None);
            }
            None => None,
        };
        match self.shown(None, node) {
            Shown::Removed => {
                edits.parts.insert(key.clone(), None);
                Some(None)
            }
            Shown::Placeholder(text) => {
                edits
                    .parts
                    .insert(key.clone(), Some(placeholder_node(node)));
                Some(Some(text_content(text)))
            }
            Shown::Kept => {
                let own = self.show_only(node);
                let only = own.as_ref().or(only);
                let mut kids = StyledChildren::new(node);
                let mut inner = Edits::default();
                let new = self.content(content, &mut kids, only, &mut inner)?;
                edits.parts.insert(key.clone(), Some(inner.apply(node)));
                Some(Some(new))
            }
        }
    }

    /// Selects the elements of content, whose styled nodes `kids` finds,
    /// and records what it changes in `edits`: `None` if nothing changed.
    /// Under `show-only`, text is left out.
    fn content(
        &mut self,
        content: &Content,
        kids: &mut StyledChildren<'_>,
        only: Option<&Only>,
        edits: &mut Edits,
    ) -> Option<Content> {
        let mut out: Vec<Node> = Vec::with_capacity(content.nodes().len());
        let mut changed = false;
        // Whether the last node was left out: the space before it goes too
        // when what follows is not a space, so that `Text #x[..].` without
        // `x` reads `Text.`.
        let mut after_removed = false;
        for node in content.nodes() {
            let kept = match node {
                Node::Element(child) => match kids.element(child) {
                    // An element that the style stage gave no node is left
                    // as it is: no later stage reads it either.
                    None => Some((node.clone(), false)),
                    Some(child_node) => self.child(child, child_node, only, edits),
                },
                Node::Rule(_) => Some((node.clone(), false)),
                // Under `show-only`, text is left out.
                _ if only.is_some() => None,
                _ => Some((node.clone(), false)),
            };
            let Some((kept, edited)) = kept else {
                changed = true;
                after_removed = true;
                continue;
            };
            changed |= edited;
            if after_removed
                && !matches!(kept, Node::Space(_))
                && matches!(out.last(), Some(Node::Space(_)))
            {
                out.pop();
            }
            after_removed = false;
            out.push(kept);
        }
        changed.then(|| tidy(out))
    }

    /// Selects an element of content: `None` if it is left out, else the
    /// node and whether it changed. Records what it changes in `edits`.
    fn child(
        &mut self,
        child: &Element,
        node: &StyledNode,
        only: Option<&Only>,
        edits: &mut Edits,
    ) -> Option<(Node, bool)> {
        let child_only = match only {
            Some(only) if only.element(&child.name) => None,
            Some(only) if only.holds(child) => Some(only),
            Some(_) => {
                edits.removed.insert(child.location);
                return None;
            }
            None => None,
        };
        match self.element(child, node, child_only) {
            Out::Same => Some((Node::Element(child.clone()), false)),
            Out::Removed => {
                edits.removed.insert(child.location);
                None
            }
            Out::Changed(changed) => {
                let (element, styled) = *changed;
                edits.replaced.insert(child.location, styled);
                Some((Node::Element(element), true))
            }
        }
    }

    /// What Select does with an element (`None` for a part) whose styled
    /// node is `node`.
    fn shown(&mut self, element: Option<&Element>, node: &StyledNode) -> Shown {
        if let Some(element) = element
            && !self.condition(element)
        {
            return Shown::Removed;
        }
        match node.style.value(self.props, self.display) {
            Some(Style::None) => Shown::Removed,
            Some(Style::Keyword(mode)) => match mode.as_str() {
                "hidden" => {
                    self.warn_left_out(
                        "hidden",
                        node.span,
                        "`display: hidden` does not keep the space of its content yet, so the \
                         content is left out and takes no space",
                        "until it does, pages after it may break differently from the variants \
                         that show it",
                    );
                    Shown::Removed
                }
                "answer-space" => self.answer_space(node),
                _ => Shown::Kept,
            },
            Some(Style::Call(name, args)) if name.as_str() == "placeholder" => match &args[..] {
                [Style::Str(text)] => Shown::Placeholder(text.clone()),
                // The declaration was reported; nothing is safer than
                // showing what it was to replace.
                _ => Shown::Removed,
            },
            _ => Shown::Kept,
        }
    }

    /// What `display: answer-space` makes of a node (SPEC §6.3). Until
    /// answer space is laid out, the content is left out, with a warning,
    /// so that the answer never reaches the output.
    fn answer_space(&mut self, node: &StyledNode) -> Shown {
        self.warn_left_out(
            "answer-space",
            node.span,
            "`display: answer-space` does not draw answer space yet, so the content is left out \
             and nothing takes its place",
            "until it does, give the space with a sized block, such as `min-height` or \
             `rule-lines`, on an element the variant shows",
        );
        Shown::Removed
    }

    /// Warns, once per build and mode, that content is left out where a
    /// display mode asks for more.
    fn warn_left_out(&mut self, mode: &'static str, span: Span, message: &str, hint: &str) {
        if self.warned.insert(mode) {
            diag::report(
                self.db,
                Diagnostic::warning(span, message.to_owned()).with_hint(hint.to_owned()),
            );
        }
    }

    /// Whether the condition of an `#in-variant` or `#except-variant`
    /// shows it in the variant; `true` for every other element. Reports a
    /// condition that names no declared variant.
    fn condition(&mut self, element: &Element) -> bool {
        if element.name.namespace.is_some() {
            return true;
        }
        let inside = match element.name.name.as_str() {
            "in-variant" => true,
            "except-variant" => false,
            _ => return true,
        };
        let Some(Value::Str(name)) = element.field("variant") else {
            return true;
        };
        if self.manifest.variant(name).is_none() {
            let declared: Vec<String> = self
                .manifest
                .variants
                .iter()
                .map(|v| format!("`{}`", v.name.v))
                .collect();
            let diagnostic = Diagnostic::warning(
                element.span,
                eco_format!(
                    "no variant `{name}` is declared, so this `{}` is {}",
                    element.name,
                    if inside {
                        "never shown"
                    } else {
                        "always shown"
                    }
                ),
            );
            diag::report(
                self.db,
                if declared.is_empty() {
                    diagnostic
                        .with_hint("variants are declared as `[variants.<name>]` in folio.toml")
                } else {
                    diagnostic
                        .with_hint(eco_format!("the manifest declares {}", declared.join(", ")))
                },
            );
        }
        let here = self.variant.is_some_and(|v| v.as_str() == name.as_str());
        here == inside
    }

    /// The names a node's `show-only` keeps, or `None` if it is `none`.
    fn show_only(&self, node: &StyledNode) -> Option<Only> {
        fn name(value: &Style) -> Option<EcoString> {
            match value {
                Style::Keyword(name) => Some(name.as_str().into()),
                Style::Str(name) => Some(name.clone()),
                _ => None,
            }
        }
        match node.style.value(self.props, self.show_only)? {
            Style::None => None,
            Style::Array(items) => Some(Only(items.iter().filter_map(name).collect())),
            value => Some(Only(name(value).into_iter().collect())),
        }
    }
}

/// Whether the document model made an element to hold content: a
/// paragraph, a list or a section (SPEC §4.2).
fn made(element: &Element) -> bool {
    element.name.namespace.is_none()
        && matches!(
            element.name.name.as_str(),
            "paragraph" | "list" | "enum" | "terms" | "section"
        )
}

/// Whether fields hold no content but spaces, breaks and scoped rules.
fn is_empty(fields: &crate::eval::Dict) -> bool {
    fields.iter().all(|(_, value)| match value {
        Value::Content(content) => content.nodes().iter().all(|node| {
            matches!(
                node,
                Node::Space(_) | Node::Parbreak(_) | Node::Linebreak(_) | Node::Rule(_)
            )
        }),
        _ => true,
    })
}

/// Content whose elements were left out, with the spaces around them
/// collapsed: none at the ends, and no two in a row, as the document model
/// leaves content it makes ([`crate::model::Document`]).
fn tidy(nodes: Vec<Node>) -> Content {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let space = matches!(node, Node::Space(_));
        if space && matches!(out.last(), None | Some(Node::Space(_))) {
            continue;
        }
        out.push(node);
    }
    while matches!(out.last(), Some(Node::Space(_))) {
        out.pop();
    }
    out.into_iter().collect()
}

/// Content of a placeholder's text. The text records no node, so it stands
/// for the whole element that holds it (SPEC §6.3).
fn text_content(text: EcoString) -> Content {
    Content::node(Node::Text {
        text,
        span: Span::detached(),
    })
}

/// An element with its content replaced by a text: every field of content
/// left out, and a code block's text, and the text as its body.
fn placeholder(element: &Element, text: EcoString) -> Element {
    let mut element = element.clone();
    let content: Vec<EcoString> = element
        .fields
        .iter()
        .filter(|(_, value)| matches!(value, Value::Content(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in content {
        element.fields.remove(&key);
    }
    if element.name.namespace.is_none() && element.name.name.as_str() == "raw" {
        element.fields.remove("text");
    }
    element
        .fields
        .insert(BODY.into(), Value::Content(text_content(text)));
    element
}

/// The styled node of an element or part whose content a placeholder
/// replaced: its own style, and none of the nodes of the content it held.
/// The parts that hold the style of a formula's vectors and of a
/// footnote's note stay, as they are not content.
fn placeholder_node(node: &StyledNode) -> StyledNode {
    let children = node
        .children
        .iter()
        .filter(|child| {
            matches!(&child.name, NodeName::Part(name)
                if name.as_str() == NOTE_PART || name.as_str() == VECTOR_PART)
        })
        .cloned()
        .collect();
    StyledNode {
        children,
        ..node.clone()
    }
}
