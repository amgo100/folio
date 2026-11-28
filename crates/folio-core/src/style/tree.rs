//! Styling the document tree (SPEC §2.1, §5.1): matching every node and
//! computing its style.
//!
//! Matching a node is a query keyed by what its rules can see of it: its
//! view, and the views of its ancestors ([`NodeView`]). The views hold a
//! node's position and previous siblings only when a rule of the build
//! tests them, as `+`, `:first`, `:last`, `:nth` and `:has` rules do. An
//! insertion therefore re-matches the inserted node and only the siblings
//! whose views it changes; every other node finds its match memoised, even
//! though the document tree it belongs to is new (SPEC §5.1).

use std::collections::HashMap;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, LAYOUT_HINTS, Location, Name, QualifiedName, Target};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{Span, SyntaxNode};
use folio_text::Lang;

use super::computed::{ComputedStyle, Styler};
use super::props::Properties;
use super::rules::{Cascade, Compiler, Decl, DeclValue, Origin, Rule, StepRule, cascade};
use super::selector::{Attr, Needs, NodeName, NodeView, Position, Role, Subject};
use super::value::{Declared, Values, check};
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::locate::StyledChildren;
use crate::model::document;
use crate::project::ProjectPath;

/// The styled document of a build: a computed style for every node of its
/// document tree, and for the page.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Styled {
    /// The properties the styles hold values for.
    pub properties: Arc<Properties>,
    /// The style of the default page master: `page { .. }`.
    pub page: ComputedStyle,
    /// The `doc` element.
    pub root: StyledNode,
    /// The `:step` rules of scoped rules in content, which apply in the
    /// variant and target, collected for step styling like those of the
    /// cascade ([`Cascade::step_rules`]).
    pub step_rules: EcoVec<StepRule>,
}

/// A node of the document tree with its computed style.
///
/// The tree has the shape of the document tree: an element's children are
/// the elements of its body and a part for each slot and content field
/// other than the body, in the canonical order of its fields (SPEC §4.3),
/// which does not depend on how a call was written. A part holds the
/// elements of its content.
///
/// A node names the element it styles by its location, so later stages
/// that drop or add nodes keep every node with its element
/// ([`crate::locate::StyledChildren`]).
///
/// A `math` element whose formula holds a vector has one more child, the
/// part [`VECTOR_PART`]: the style its vectors take, matched as the `math`
/// element with the class `vector` and inheriting from it, so that
/// `math.vector { math-style: arrow; }` styles them (SPEC §8.1). Math
/// layout finds it with [`StyledNode::vector_style`].
///
/// A `footnote` element has one more child too, the part [`NOTE_PART`]:
/// the style of its note, matched as the footnote and inheriting from the
/// `doc` element rather than from the anchor, as the footnote's body does
/// (SPEC §7.8). Only `lang` follows the anchor, and `font` too where the
/// anchor's language, by language and script, is another than the
/// document's. The text stage finds it with [`StyledNode::note_style`].
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct StyledNode {
    /// The element's name, or the part's.
    pub name: NodeName,
    /// The element, or the element whose part it is.
    pub span: Span,
    /// The location of the element, or of the element whose part it is.
    pub location: Location,
    /// Its style.
    pub style: ComputedStyle,
    /// Its children.
    pub children: EcoVec<StyledNode>,
}

/// The name of the part of a `math` element that holds the style of its
/// vectors (see [`StyledNode`]).
pub const VECTOR_PART: &str = "vector";

/// The name of the part of a `footnote` element that holds the style of
/// its note (see [`StyledNode`]).
pub const NOTE_PART: &str = "note";

impl StyledNode {
    /// The style of the vectors of a `math` element's formula: its part
    /// [`VECTOR_PART`], present when the formula holds a vector. `None` for
    /// any other node.
    pub fn vector_style(&self) -> Option<&StyledNode> {
        self.children.iter().find(
            |child| matches!(&child.name, NodeName::Part(name) if name.as_str() == VECTOR_PART),
        )
    }

    /// The style of a `footnote` element's note: its part [`NOTE_PART`].
    /// `None` for any other node.
    pub fn note_style(&self) -> Option<&StyledNode> {
        self.children
            .iter()
            .find(|child| matches!(&child.name, NodeName::Part(name) if name.as_str() == NOTE_PART))
    }
}

impl Styled {
    /// The computed value of a property of a node, by the property's name,
    /// for tests and debugging.
    pub fn value<'a>(
        &'a self,
        node: &'a StyledNode,
        property: &str,
    ) -> Option<&'a folio_component::Value> {
        let name = QualifiedName::new(property)?;
        let id = self.properties.find(&name)?;
        node.style.value(&self.properties, id)
    }

    /// Every node, depth first, parents before children.
    pub fn nodes(&self) -> Vec<&StyledNode> {
        fn walk<'a>(node: &'a StyledNode, out: &mut Vec<&'a StyledNode>) {
            out.push(node);
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

/// The view of an ancestor, interned with the views of its own ancestors.
#[salsa::interned(debug)]
struct ContextKey<'db> {
    #[returns(ref)]
    view: NodeView,
    #[returns(copy)]
    parent: Option<ContextKey<'db>>,
}

/// What matching sees of a node in a build.
#[salsa::interned(debug)]
struct MatchKey<'db> {
    #[returns(copy)]
    build: Build<'db>,
    #[returns(ref)]
    view: NodeView,
    #[returns(copy)]
    context: Option<ContextKey<'db>>,
}

/// The rules of the build's cascade that match a node, as indices into
/// its rules, lowest priority first.
#[salsa::tracked(returns(ref))]
fn match_node<'db>(db: &'db dyn Db, key: MatchKey<'db>) -> EcoVec<u32> {
    let cascade = cascade(db, key.build(db));
    let mut chain = vec![key.view(db)];
    let mut context = key.context(db);
    while let Some(ancestor) = context {
        chain.push(ancestor.view(db));
        context = ancestor.parent(db);
    }
    let subject = &chain[0].subject;
    cascade
        .candidates(&subject.name, &subject.classes, subject.label.as_ref())
        .into_iter()
        .filter(|&i| cascade.rules[i as usize].selector.matches(&chain))
        .collect()
}

/// Styles a build's document: matches every node against the build's
/// rules and computes its style. `None` when the document could not be
/// built.
#[salsa::tracked(returns(ref))]
pub fn styles(db: &dyn Db, build: Build<'_>) -> Option<Styled> {
    let project = build.project(db);
    // The rules first, so that their errors are reported even when the
    // document cannot be built.
    let cascade = cascade(db, build);
    let doc = document(db, project, build.variant(db).clone()).as_ref()?;
    let tokens = cascade.tokens();
    let compiler = Compiler::for_cascade(db, project, cascade, &tokens);

    let mut walker = Walker {
        db,
        build,
        cascade,
        compiler: &compiler,
        compiled: HashMap::new(),
        step_rules: EcoVec::new(),
        styler: Styler::new(&cascade.properties),
        memo: HashMap::new(),
        scoped: Vec::new(),
        scoped_needs: Needs::default(),
        scoped_chain: Vec::new(),
        has_scoped: false,
        order: 0,
        root_style: None,
        default_lang: {
            let manifest = project.manifest(db);
            Lang::new(manifest.project.lang.v.as_str()).unwrap_or_else(Lang::english)
        },
        variant: build.variant(db).clone(),
        target: build.target(db),
    };

    // Scoped rules in content are rare: when there are none, matching them
    // costs nothing.
    let mut found = Vec::new();
    collect_rules(&doc.root, &mut found);
    walker.has_scoped = !found.is_empty();
    for node in found {
        walker.compile_scoped(node);
    }

    let page = {
        let view = NodeView {
            subject: Subject {
                name: NodeName::Page,
                classes: EcoVec::new(),
                label: None,
                fields: EcoVec::new(),
            },
            pos: Position::default(),
            prev: EcoVec::new(),
            children: EcoVec::new(),
        };
        let key = MatchKey::new(db, build, view, None);
        let matched = match_node(db, key).clone();
        walker.compute(None, &matched, &EcoVec::new())
    };
    let root = walker.root(&doc.root);
    Some(Styled {
        properties: cascade.properties.clone(),
        page,
        root,
        step_rules: walker.step_rules,
    })
}

/// A child of a node: an element of its body, or a part.
#[derive(Copy, Clone)]
pub(crate) enum Child<'a> {
    Element(&'a Element),
    Part(&'a str, &'a Content),
}

impl<'a> Child<'a> {
    /// The children of this child.
    fn children(self) -> Vec<Child<'a>> {
        match self {
            Self::Element(element) => element_children(element),
            Self::Part(_, content) => content_elements(content).map(Child::Element).collect(),
        }
    }
}

/// The children of an element with their nodes in the styled tree
/// (`node` is the element's node), in the order of the element's fields.
/// Each is paired with its node by location, or by name for a part
/// ([`StyledChildren`]); a child without a node is left out.
pub(crate) fn styled_children<'a>(
    element: &'a Element,
    node: &'a StyledNode,
) -> Vec<(Child<'a>, &'a StyledNode)> {
    let mut styled = StyledChildren::new(node);
    element_children(element)
        .into_iter()
        .filter_map(|child| {
            let node = match child {
                Child::Element(element) => styled.element(element),
                Child::Part(name, _) => styled.part(&Name::new(name)?),
            }?;
            Some((child, node))
        })
        .collect()
}

/// The children of an element: the elements of its body, and a part for
/// each other content field, in the canonical order of its fields.
fn element_children(element: &Element) -> Vec<Child<'_>> {
    let mut out = Vec::new();
    for (key, value) in element.fields.iter() {
        let Value::Content(content) = value else {
            continue;
        };
        if key == BODY {
            out.extend(content_elements(content).map(Child::Element));
        } else if Name::new(key).is_some() {
            out.push(Child::Part(key, content));
        }
    }
    out
}

pub(crate) fn content_elements(content: &Content) -> impl Iterator<Item = &Element> {
    content.nodes().iter().filter_map(|node| match node {
        Node::Element(element) => Some(element),
        _ => None,
    })
}

/// The scoped rules in an element's content, which apply to it.
fn own_rules(element: &Element) -> Vec<&SyntaxNode> {
    let mut out = Vec::new();
    for (_, value) in element.fields.iter() {
        if let Value::Content(content) = value {
            for node in content.nodes() {
                if let Node::Rule(rule) = node {
                    out.push(rule);
                }
            }
        }
    }
    out
}

/// Every scoped rule in the document.
fn collect_rules<'a>(element: &'a Element, out: &mut Vec<&'a SyntaxNode>) {
    out.extend(own_rules(element));
    for child in element_children(element) {
        collect_child_rules(child, out);
    }
}

fn collect_child_rules<'a>(child: Child<'a>, out: &mut Vec<&'a SyntaxNode>) {
    match child {
        Child::Element(element) => collect_rules(element, out),
        Child::Part(_, content) => {
            for element in content_elements(content) {
                collect_rules(element, out);
            }
        }
    }
}

/// What a computed style depends on: the parent's style, the matched rules
/// and the local declarations.
type MemoKey = (Option<ComputedStyle>, EcoVec<u32>, EcoVec<Decl>);

struct Walker<'a> {
    db: &'a dyn Db,
    build: Build<'a>,
    cascade: &'a Cascade,
    compiler: &'a Compiler<'a>,
    /// The scoped rules of the document that apply to the build, by the
    /// span of the scoped rule they were compiled from.
    compiled: HashMap<Span, Vec<Rule>>,
    /// The `:step` rules among them.
    step_rules: EcoVec<StepRule>,
    styler: Styler<'a>,
    /// Computed styles by parent style and declarations.
    memo: HashMap<MemoKey, ComputedStyle>,
    /// The scoped rules in effect: those of the elements being walked.
    scoped: Vec<Rule>,
    /// What the scoped rules of the document need to see.
    scoped_needs: Needs,
    /// The views of the ancestors for scoped rules, nearest last.
    scoped_chain: Vec<NodeView>,
    /// Whether the document has scoped rules.
    has_scoped: bool,
    /// The document order of scoped rules.
    order: u32,
    /// The computed style of the `doc` element, which the notes of
    /// footnotes inherit from, but for `lang`, and `font` in another
    /// language ([`NOTE_PART`]).
    root_style: Option<ComputedStyle>,
    /// The manifest's language, which `lang: auto` stands for.
    default_lang: Lang,
    variant: Option<Name>,
    target: Target,
}

impl<'a> Walker<'a> {
    fn root(&mut self, root: &Element) -> StyledNode {
        let siblings = [Child::Element(root)];
        self.node(&siblings, 0, None, None, (root.span, root.location))
    }

    /// Styles the child `i` of `siblings`, with the context of its parent;
    /// `owner` is the span and location of the element whose children they
    /// are.
    fn node(
        &mut self,
        siblings: &[Child<'_>],
        i: usize,
        context: Option<ContextKey<'a>>,
        parent: Option<&ComputedStyle>,
        owner: (Span, Location),
    ) -> StyledNode {
        let needs = &self.cascade.needs;
        let child = siblings[i];
        let (span, location) = match child {
            Child::Element(element) => (element.span, element.location),
            Child::Part(..) => owner,
        };
        let view = self.view(siblings, i, Role::Subject, needs);
        let key = MatchKey::new(self.db, self.build, view, context);
        let matched = match_node(self.db, key).clone();

        // Scoped rules of this element apply to it and its descendants.
        let mark = self.scoped.len();
        if let Child::Element(element) = child {
            for node in own_rules(element) {
                if let Some(rules) = self.compiled.get(&node.span()) {
                    self.scoped.extend(rules.iter().cloned());
                }
            }
        }

        let mut local = EcoVec::new();
        if !self.scoped.is_empty() {
            let view = self.view(siblings, i, Role::Subject, &self.scoped_needs);
            let mut chain: Vec<&NodeView> = vec![&view];
            chain.extend(self.scoped_chain.iter().rev());
            let mut hits: Vec<&Rule> = self
                .scoped
                .iter()
                .filter(|r| r.selector.matches(&chain))
                .collect();
            hits.sort_by_key(|r| r.priority);
            for rule in hits {
                local.extend(rule.declarations.iter().cloned());
            }
        }
        let own = match child {
            Child::Element(element) => self.inline(element),
            Child::Part(..) => Vec::new(),
        };
        local.extend(own.iter().cloned());

        let style = self.compute(parent, &matched, &local);
        if context.is_none() {
            self.root_style = Some(style.clone());
        }
        // A footnote's note inherits from the document, not from its
        // anchor, so that every note on a page looks alike, but keeps the
        // language of the passage it is anchored in, and the passage's font
        // where that is another language than the document's; the footnote
        // itself, which styles the marker at its anchor, inherits from the
        // anchor (SPEC §7.8, D-327-1, D-327-3, D-327-4, D-327-5).
        let note = match child {
            Child::Element(element) if is_footnote(element) => {
                let base = match (self.root_style.clone(), parent) {
                    (Some(root), Some(anchor)) => {
                        let props = &self.cascade.properties;
                        let mut kept = vec![props.builtin("lang")];
                        if !same_language(&self.language(anchor), &self.language(&root)) {
                            kept.push(props.builtin("font"));
                        }
                        Some(self.styler.adopt(&root, anchor, &kept))
                    }
                    (root, _) => root,
                };
                Some(self.compute(base.as_ref(), &matched, &local))
            }
            _ => None,
        };
        let inherited = note.as_ref().unwrap_or(&style);

        // The children, with this node as their context. A leaf has none,
        // so its context is neither seen nor interned.
        let grandchildren = child.children();
        let has_children = !grandchildren.is_empty();
        let child_context = has_children.then(|| {
            let view = self.view(siblings, i, Role::Context, needs);
            ContextKey::new(self.db, view, context)
        });
        let scoped_view = (self.has_scoped && has_children)
            .then(|| self.view(siblings, i, Role::Context, &self.scoped_needs));
        if let Some(view) = &scoped_view {
            self.scoped_chain.push(view.clone());
        }
        let mut children: EcoVec<StyledNode> = (0..grandchildren.len())
            .map(|j| {
                let owner = (span, location);
                self.node(&grandchildren, j, child_context, Some(inherited), owner)
            })
            .collect();
        if scoped_view.is_some() {
            self.scoped_chain.pop();
        }
        self.scoped.truncate(mark);
        if let Child::Element(element) = child
            && holds_vector(element)
        {
            let owner = (span, location);
            children.push(self.vector_node(siblings, i, context, &style, &own, owner));
        }
        if let Some(note) = note {
            children.push(StyledNode {
                name: NodeName::Part(Name::new(NOTE_PART).expect("an identifier")),
                span,
                location,
                style: note,
                children: EcoVec::new(),
            });
        }

        StyledNode {
            name: name_of(child),
            span,
            location,
            style,
            children,
        }
    }

    /// The part that styles the vectors of the `math` element at child `i`
    /// of `siblings` (see [`StyledNode`]): the element seen with the class
    /// `vector` added, in the element's own context, so that it matches
    /// every rule the element matches and the `math.vector` rules, and
    /// computed as a child of the element's `style`. The element's own
    /// declarations `own` ([`inline`](Self::inline)) apply to it as they do
    /// to the element, so a local override is not undone by a rule for
    /// `math`.
    fn vector_node(
        &mut self,
        siblings: &[Child<'_>],
        i: usize,
        context: Option<ContextKey<'a>>,
        style: &ComputedStyle,
        own: &[Decl],
        (span, location): (Span, Location),
    ) -> StyledNode {
        let vector = |mut view: NodeView| {
            view.subject.classes.push(VECTOR_PART.into());
            view
        };
        let needs = &self.cascade.needs;
        let view = vector(self.view(siblings, i, Role::Subject, needs));
        let key = MatchKey::new(self.db, self.build, view, context);
        let matched = match_node(self.db, key).clone();
        let mut local = EcoVec::new();
        if !self.scoped.is_empty() {
            let view = vector(self.view(siblings, i, Role::Subject, &self.scoped_needs));
            let mut chain: Vec<&NodeView> = vec![&view];
            chain.extend(self.scoped_chain.iter().rev());
            let mut hits: Vec<&Rule> = self
                .scoped
                .iter()
                .filter(|r| r.selector.matches(&chain))
                .collect();
            hits.sort_by_key(|r| r.priority);
            for rule in hits {
                local.extend(rule.declarations.iter().cloned());
            }
        }
        local.extend(own.iter().cloned());
        StyledNode {
            name: NodeName::Part(Name::new(VECTOR_PART).expect("an identifier")),
            span,
            location,
            style: self.compute(Some(style), &matched, &local),
            children: EcoVec::new(),
        }
    }

    fn compute(
        &mut self,
        parent: Option<&ComputedStyle>,
        matched: &EcoVec<u32>,
        local: &EcoVec<Decl>,
    ) -> ComputedStyle {
        let key = (parent.cloned(), matched.clone(), local.clone());
        if let Some(style) = self.memo.get(&key) {
            return style.clone();
        }
        let cascade = self.cascade;
        let decls = matched
            .iter()
            .flat_map(|&r| &cascade.rules[r as usize].declarations)
            .chain(local)
            .map(|d| (d.prop, &d.value));
        let style = self.styler.compute(parent, decls);
        self.memo.insert(key, style.clone());
        style
    }

    /// Compiles a scoped rule, in document order, once: its errors are
    /// reported whether or not it applies to the build, its warnings that
    /// something has no effect yet only when it does, and the rules that
    /// apply are kept for the walk.
    fn compile_scoped(&mut self, node: &SyntaxNode) {
        let Some(rule) = node.cast::<ast::ScopedRule<'_>>() else {
            return;
        };
        let applies = rule.scopes().all(|scope| {
            let mut names = scope.names().map(|n| n.get().clone());
            match scope.kind() {
                "variant" => names.any(|n| self.variant.as_ref().is_some_and(|v| v.as_str() == n)),
                "target" => names.any(|n| Target::from_name(&n) == Some(self.target)),
                _ => false,
            }
        });
        // Values are evaluated in the file that holds the rule, which
        // function calls see as theirs.
        let file = self.file_of(node.span());
        let values = Values::new(self.compiler.values, file);
        self.order += 1;
        self.compiler.applies.set(applies);
        let compiled = self.compiler.compile_rule(
            &values,
            rule.selectors(),
            rule.declarations(),
            Origin::Local,
            self.order,
            rule.span(),
        );
        self.compiler.applies.set(true);
        if !applies {
            return;
        }
        self.compiler.lint_rule(&compiled);
        for rule in &compiled.rules {
            self.scoped_needs.add(&rule.selector);
        }
        self.step_rules.extend(compiled.steps);
        self.compiled.insert(node.span(), compiled.rules);
    }

    /// The path of the file a span is in, or the entry file's.
    fn file_of(&self, span: Span) -> ProjectPath {
        let project = self.build.project(self.db);
        span.id()
            .and_then(|id| {
                project
                    .sources(self.db)
                    .iter()
                    .find(|(_, file)| file.source(self.db).id() == id)
                    .map(|(path, _)| path.clone())
            })
            .unwrap_or_else(|| project.manifest(self.db).project.entry.v.clone())
    }

    /// The language a style sets text in: its `lang`, or the manifest's
    /// language where that is `auto` or not a language tag, as the text
    /// stage reads it.
    fn language(&self, style: &ComputedStyle) -> Lang {
        let props = &self.cascade.properties;
        let tag = match style.value(props, props.builtin("lang")) {
            Some(folio_component::Value::Str(tag)) => Lang::new(tag),
            _ => None,
        };
        tag.unwrap_or_else(|| self.default_lang.clone())
    }

    /// The declarations an element makes for itself: its `override`
    /// fields and layout hints (SPEC §5.4), checked against their
    /// properties, lowest priority first. They rank by form, not by the
    /// order the fields are written in: hints, then scoped hints, then
    /// `override`, then scoped `override@…`.
    fn inline(&self, element: &Element) -> Vec<Decl> {
        // One list per form, in rank order: [hint, scoped hint, override,
        // scoped override].
        let mut ranked: [Vec<Decl>; 4] = Default::default();
        for (key, value) in element.fields.iter() {
            let (field, scope) = match key.split_once('@') {
                Some((field, scope)) => (field, Some(scope)),
                None => (key.as_str(), None),
            };
            if let Some(scope) = scope {
                let applies = self.variant.as_ref().is_some_and(|v| v.as_str() == scope)
                    || Target::from_name(scope) == Some(self.target);
                if !applies {
                    continue;
                }
            }
            let scoped = usize::from(scope.is_some());
            if field == "override" {
                if let Value::Dict(dict) = value {
                    for (name, v) in dict.iter() {
                        if let Some(decl) = self.inline_decl(key, name, v, element.span) {
                            ranked[2 + scoped].push(decl);
                        }
                    }
                }
            } else if LAYOUT_HINTS.contains(&field)
                && let Some(decl) = self.inline_decl(key, field, value, element.span)
            {
                ranked[scoped].push(decl);
            }
        }
        ranked.into_iter().flatten().collect()
    }

    /// A declaration of the field `key`: `override` or a hint, with the
    /// warning a stylesheet's declaration would give if no stage reads its
    /// property yet. The span is the element's, since instances do not keep
    /// the spans of their arguments.
    fn inline_decl(&self, key: &str, name: &str, value: &Value, span: Span) -> Option<Decl> {
        let prop = self.compiler.property_named(name, span)?;
        let property = self.cascade.properties.get(prop);
        let declared = self.resolve_tokens(value, span)?;
        if let Err(message) = check(&declared, &property.ty, true) {
            diag::report(
                self.db,
                Diagnostic::error(
                    span,
                    eco_format!("wrong value for `{}` in `{key}`: {message}", property.name),
                ),
            );
            return None;
        }
        self.compiler
            .lint_not_implemented(property, Some(&declared), true, span);
        Some(Decl {
            prop,
            value: DeclValue::Value(declared),
            span,
        })
    }

    /// The declared value of a field's value, with its token references
    /// (`tok.warn`, SPEC §5.4) resolved against the build's tokens: as the
    /// whole value, or as items of a list. `None` after an error, such as
    /// an unknown token, which is reported at `span`.
    fn resolve_tokens(&self, value: &Value, span: Span) -> Option<Declared> {
        if !holds_token(value) {
            return Some(Declared::Value(value.clone()));
        }
        let values = Values::new(self.compiler.values, self.file_of(span));
        resolve_tokens(&values, value, span)
    }

    /// The view of child `i` of `siblings` in a role, as a set of rules
    /// needs to see it: its previous siblings are seen in the rules'
    /// context, and each node's fields as the attribute tests for its name
    /// name them.
    ///
    /// Positions and previous siblings count the elements of the body
    /// only (SPEC §5.1): a part has no position and no previous siblings,
    /// and the elements around it skip it.
    fn view(&self, siblings: &[Child<'_>], i: usize, role: Role, needs: &Needs) -> NodeView {
        self.view_with(siblings, i, role, needs, true)
    }

    /// [`view`](Self::view), with previous siblings only if `prev`: those
    /// of a previous sibling are not seen.
    fn view_with(
        &self,
        siblings: &[Child<'_>],
        i: usize,
        role: Role,
        needs: &Needs,
        prev: bool,
    ) -> NodeView {
        let name = name_of(siblings[i]);
        let own = needs.of(&name);
        let need = own.need(role);
        let body = body_range(siblings);
        let (full, prev) = if body.contains(&i) {
            let full = Position {
                first: i == body.start,
                last: i + 1 == body.end,
                index: u32::try_from(i - body.start + 1).unwrap_or(u32::MAX),
                column: 0,
            };
            let count = if prev { need.prev } else { 0 };
            let prev = (1..=count)
                .filter_map(|k| i.checked_sub(k).filter(|&j| j >= body.start))
                .map(|j| self.view_with(siblings, j, Role::Context, needs, false))
                .collect();
            (full, prev)
        } else {
            (Position::default(), EcoVec::new())
        };
        let children = if need.children {
            siblings[i]
                .children()
                .iter()
                .map(|&c| {
                    let name = name_of(c);
                    let fields = &needs.of(&name).fields;
                    subject(c, name, fields)
                })
                .collect()
        } else {
            EcoVec::new()
        };
        NodeView {
            pos: need.position(full),
            prev,
            children,
            subject: subject(siblings[i], name, &own.fields),
        }
    }
}

/// Whether a value is a token reference or a list with one as an item. It
/// looks one level deep only: values can share parts, and a walk of all of
/// them could take exponential time.
fn holds_token(value: &Value) -> bool {
    match value {
        Value::Token(_) => true,
        Value::Array(items) => items.iter().any(|v| matches!(v, Value::Token(_))),
        _ => false,
    }
}

/// A value with its token references resolved; see
/// [`Walker::resolve_tokens`].
fn resolve_tokens(values: &Values<'_>, value: &Value, span: Span) -> Option<Declared> {
    match value {
        Value::Token(name) => values.token(name, span),
        Value::Array(items) if holds_token(value) => items
            .iter()
            .map(|item| match item {
                Value::Token(name) => values.token(name, span),
                _ => Some(Declared::Value(item.clone())),
            })
            .collect::<Option<EcoVec<_>>>()
            .map(Declared::List),
        _ => Some(Declared::Value(value.clone())),
    }
}

/// Whether two language tags name one language: the same primary subtag,
/// and the same script where both write one, whatever their regions. So
/// `en-GB` and `en` are one, `sr` and `sr-Cyrl` too, but `sr-Latn` and
/// `sr-Cyrl` two (SPEC §7.8).
fn same_language(a: &Lang, b: &Lang) -> bool {
    // The script is the subtag of four letters after the primary one,
    // before any singleton (`x-…` and extensions), whose subtags are not
    // scripts. `Lang::new` checked the form.
    fn script(lang: &Lang) -> Option<&str> {
        lang.as_str()
            .split('-')
            .skip(1)
            .take_while(|subtag| subtag.len() > 1)
            .find(|subtag| subtag.len() == 4 && subtag.bytes().all(|b| b.is_ascii_alphabetic()))
    }
    a.primary() == b.primary()
        && match (script(a), script(b)) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

/// Whether an element is a built-in `footnote`.
fn is_footnote(element: &Element) -> bool {
    element.name.namespace.is_none() && element.name.name.as_str() == "footnote"
}

/// Whether an element is a `math` element whose formula holds a vector.
fn holds_vector(element: &Element) -> bool {
    element.name.namespace.is_none()
        && element.name.name.as_str() == "math"
        && matches!(element.field("tree"), Some(Value::Math(formula)) if formula.has_vector())
}

/// The range of `siblings` that the body's elements take. They are
/// contiguous, since the body is one field; the parts before and after
/// them are few, so this costs as many steps as there are parts.
fn body_range(siblings: &[Child<'_>]) -> std::ops::Range<usize> {
    let is_element = |c: &Child<'_>| matches!(c, Child::Element(_));
    match siblings.iter().position(is_element) {
        Some(start) => {
            start
                ..siblings
                    .iter()
                    .rposition(is_element)
                    .map_or(start, |e| e + 1)
        }
        None => 0..0,
    }
}

fn name_of(child: Child<'_>) -> NodeName {
    match child {
        Child::Element(element) => NodeName::Element(element.name.clone()),
        Child::Part(name, _) => {
            NodeName::Part(Name::new(name).expect("parts are named by identifiers"))
        }
    }
}

/// What selectors see of a node itself, whose name is `name`.
fn subject(
    child: Child<'_>,
    name: NodeName,
    fields: &std::collections::BTreeSet<EcoString>,
) -> Subject {
    let Child::Element(element) = child else {
        return Subject {
            name,
            classes: EcoVec::new(),
            label: None,
            fields: EcoVec::new(),
        };
    };
    let classes = match element.field("class") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => EcoVec::new(),
    };
    Subject {
        name,
        classes,
        label: element.label.clone(),
        fields: fields
            .iter()
            .filter_map(|f| Some((f.clone(), Attr::of(element.field(f)?)?)))
            .collect(),
    }
}
