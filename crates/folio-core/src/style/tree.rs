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
use folio_component::{BODY, LAYOUT_HINTS, Name, QualifiedName, Target};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{Span, SyntaxNode};

use super::computed::{ComputedStyle, Styler};
use super::props::Properties;
use super::rules::{Cascade, Compiler, Decl, DeclValue, Origin, Rule, StepRule, cascade};
use super::selector::{Attr, Need, Needs, NodeName, NodeView, Position, Subject};
use super::value::{Declared, Values, check};
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
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
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct StyledNode {
    /// The element's name, or the part's.
    pub name: NodeName,
    /// The element, or the element whose part it is.
    pub span: Span,
    /// Its style.
    pub style: ComputedStyle,
    /// Its children.
    pub children: EcoVec<StyledNode>,
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

/// Calls `f` with every run of text of a document and the computed style it
/// is set in, in document order: the style of the element whose body holds
/// it, or of the part (a content field other than the body) that does.
/// `styled` is the styled tree of `root` ([`Styled::root`]).
pub(crate) fn for_each_text(
    root: &Element,
    styled: &StyledNode,
    f: &mut impl FnMut(&ComputedStyle, &str, Span),
) {
    fn texts(
        content: &Content,
        style: &ComputedStyle,
        f: &mut impl FnMut(&ComputedStyle, &str, Span),
    ) {
        for node in content.nodes() {
            match node {
                Node::Text { text, span } => f(style, text, *span),
                Node::Slot(slot) => texts(&slot.body, style, f),
                _ => {}
            }
        }
    }

    fn walk(element: &Element, node: &StyledNode, f: &mut impl FnMut(&ComputedStyle, &str, Span)) {
        if let Some(Value::Content(body)) = element.fields.get(BODY) {
            texts(body, &node.style, f);
        }
        for (child, styled) in element_children(element).into_iter().zip(&node.children) {
            match child {
                Child::Element(e) => walk(e, styled, f),
                Child::Part(_, content) => {
                    texts(content, &styled.style, f);
                    for (e, styled) in content_elements(content).zip(&styled.children) {
                        walk(e, styled, f);
                    }
                }
            }
        }
    }

    walk(root, styled, f);
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
/// (`node` is the element's node), in the styled tree's order.
pub(crate) fn styled_children<'a>(
    element: &'a Element,
    node: &'a StyledNode,
) -> Vec<(Child<'a>, &'a StyledNode)> {
    element_children(element)
        .into_iter()
        .zip(&node.children)
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
    variant: Option<Name>,
    target: Target,
}

impl<'a> Walker<'a> {
    fn root(&mut self, root: &Element) -> StyledNode {
        let siblings = [Child::Element(root)];
        self.node(&siblings, 0, None, None, root.span)
    }

    /// Styles the child `i` of `siblings`, with the context of its parent;
    /// `owner` is the element whose children they are.
    fn node(
        &mut self,
        siblings: &[Child<'_>],
        i: usize,
        context: Option<ContextKey<'a>>,
        parent: Option<&ComputedStyle>,
        owner: Span,
    ) -> StyledNode {
        let needs = &self.cascade.needs;
        let child = siblings[i];
        let span = match child {
            Child::Element(element) => element.span,
            Child::Part(..) => owner,
        };
        let view = self.view(siblings, i, &needs.subject, needs);
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
            let view = self.view(siblings, i, &self.scoped_needs.subject, &self.scoped_needs);
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
        if let Child::Element(element) = child {
            local.extend(self.inline(element));
        }

        let style = self.compute(parent, &matched, &local);

        // The children, with this node as their context.
        let context_view = self.view(siblings, i, &needs.context, needs);
        let child_context = Some(ContextKey::new(self.db, context_view, context));
        let scoped_view = self
            .has_scoped
            .then(|| self.view(siblings, i, &self.scoped_needs.context, &self.scoped_needs));
        if let Some(view) = &scoped_view {
            self.scoped_chain.push(view.clone());
        }
        let grandchildren = child.children();
        let children = (0..grandchildren.len())
            .map(|j| self.node(&grandchildren, j, child_context, Some(&style), span))
            .collect();
        if scoped_view.is_some() {
            self.scoped_chain.pop();
        }
        self.scoped.truncate(mark);

        StyledNode {
            name: name_of(child),
            span,
            style,
            children,
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
    /// reported whether or not it applies to the build, and the rules that
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
        let compiled = self.compiler.rule(
            &values,
            rule.selectors(),
            rule.declarations(),
            Origin::Local,
            self.order,
            rule.span(),
        );
        if !applies {
            return;
        }
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

    /// A declaration of the field `key`: `override` or a hint. The span is
    /// the element's, since instances do not keep the spans of their
    /// arguments.
    fn inline_decl(&self, key: &str, name: &str, value: &Value, span: Span) -> Option<Decl> {
        let prop = self.compiler.property_named(name, span)?;
        let property = self.cascade.properties.get(prop);
        let declared = Declared::Value(value.clone());
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
        Some(Decl {
            prop,
            value: DeclValue::Value(declared),
            span,
        })
    }

    /// The view of child `i` of `siblings` with a need of a set of rules:
    /// its previous siblings are seen as the rules' context, and its fields
    /// as their attribute tests name them.
    ///
    /// Positions and previous siblings count the elements of the body
    /// only (SPEC §5.1): a part has no position and no previous siblings,
    /// and the elements around it skip it.
    fn view(&self, siblings: &[Child<'_>], i: usize, need: &Need, needs: &Needs) -> NodeView {
        let fields = &needs.fields;
        let body = body_range(siblings);
        let (full, prev) = if body.contains(&i) {
            let full = Position {
                first: i == body.start,
                last: i + 1 == body.end,
                index: u32::try_from(i - body.start + 1).unwrap_or(u32::MAX),
                column: 0,
            };
            let context = Need {
                prev: 0,
                ..needs.context
            };
            let prev = (1..=need.prev)
                .filter_map(|k| i.checked_sub(k).filter(|&j| j >= body.start))
                .map(|j| self.view(siblings, j, &context, needs))
                .collect();
            (full, prev)
        } else {
            (Position::default(), EcoVec::new())
        };
        let children = if need.children {
            siblings[i]
                .children()
                .iter()
                .map(|c| subject(*c, fields))
                .collect()
        } else {
            EcoVec::new()
        };
        NodeView {
            subject: subject(siblings[i], fields),
            pos: need.position(full),
            prev,
            children,
        }
    }
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

/// What selectors see of a node itself.
fn subject(child: Child<'_>, fields: &std::collections::BTreeSet<EcoString>) -> Subject {
    let Child::Element(element) = child else {
        return Subject {
            name: name_of(child),
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
        name: name_of(child),
        classes,
        label: element.label.clone(),
        fields: fields
            .iter()
            .filter_map(|f| Some((f.clone(), Attr::of(element.field(f)?)?)))
            .collect(),
    }
}
