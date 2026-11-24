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
use super::rules::{Cascade, Compiler, Decl, DeclValue, Origin, Rule, cascade};
use super::selector::{Attr, Need, Needs, NodeName, NodeView, Position, Subject};
use super::value::{Declared, Values, check};
use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::model::document;

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
}

/// A node of the document tree with its computed style.
///
/// The tree has the shape of the document tree: an element's children are
/// the elements of its body and a part for each slot and content field
/// other than the body, in the order of its fields. A part holds the
/// elements of its content.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct StyledNode {
    /// The element's name, or the part's.
    pub name: NodeName,
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
    let doc = document(db, project, build.variant(db).clone()).as_ref()?;
    let cascade = cascade(db, build);
    let tokens = cascade.tokens();
    let compiler = Compiler::for_cascade(db, project, cascade, &tokens);
    let entry = project.manifest(db).project.entry.v.clone();
    let values = Values::new(compiler.values, entry);

    let mut walker = Walker {
        db,
        build,
        cascade,
        compiler: &compiler,
        values: &values,
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
    for node in &found {
        if let Some(rule) = node.cast::<ast::ScopedRule<'_>>() {
            for (_, compiled) in walker.compile_scoped(rule) {
                walker.scoped_needs.add(&compiled.selector);
            }
        }
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
    })
}

/// A child of a node: an element of its body, or a part.
#[derive(Copy, Clone)]
enum Child<'a> {
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

/// The children of an element: the elements of its body, and a part for
/// each other content field, in field order.
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

fn content_elements(content: &Content) -> impl Iterator<Item = &Element> {
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
    values: &'a Values<'a>,
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
        self.node(&siblings, 0, None, None)
    }

    /// Styles the child `i` of `siblings`, with the context of its parent.
    fn node(
        &mut self,
        siblings: &[Child<'_>],
        i: usize,
        context: Option<ContextKey<'a>>,
        parent: Option<&ComputedStyle>,
    ) -> StyledNode {
        let needs = &self.cascade.needs;
        let child = siblings[i];
        let view = self.view(siblings, i, &needs.subject, needs);
        let key = MatchKey::new(self.db, self.build, view, context);
        let matched = match_node(self.db, key).clone();

        // Scoped rules of this element apply to it and its descendants.
        let mark = self.scoped.len();
        if let Child::Element(element) = child {
            for node in own_rules(element) {
                if let Some(rule) = node.cast::<ast::ScopedRule<'_>>() {
                    let rules: Vec<Rule> = self
                        .compile_scoped(rule)
                        .into_iter()
                        .filter(|(applies, _)| *applies)
                        .map(|(_, r)| r)
                        .collect();
                    self.scoped.extend(rules);
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
            .map(|j| self.node(&grandchildren, j, child_context, Some(&style)))
            .collect();
        if scoped_view.is_some() {
            self.scoped_chain.pop();
        }
        self.scoped.truncate(mark);

        StyledNode {
            name: name_of(child),
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

    /// Compiles a scoped rule, with whether it applies to the build.
    fn compile_scoped(&mut self, rule: ast::ScopedRule<'_>) -> Vec<(bool, Rule)> {
        let applies = rule.scopes().all(|scope| {
            let mut names = scope.names().map(|n| n.get().clone());
            match scope.kind() {
                "variant" => names.any(|n| self.variant.as_ref().is_some_and(|v| v.as_str() == n)),
                "target" => names.any(|n| Target::from_name(&n) == Some(self.target)),
                _ => false,
            }
        });
        self.order += 1;
        let compiled = self.compiler.rule(
            self.values,
            rule.selectors(),
            rule.declarations(),
            Origin::Local,
            self.order,
            rule.span(),
        );
        compiled.rules.into_iter().map(|r| (applies, r)).collect()
    }

    /// The declarations an element makes for itself: its `override`
    /// fields and layout hints (SPEC §5.4), checked against their
    /// properties.
    fn inline(&self, element: &Element) -> Vec<Decl> {
        let mut out = Vec::new();
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
            if field == "override" {
                if let Value::Dict(dict) = value {
                    for (name, v) in dict.iter() {
                        if let Some(decl) = self.inline_decl(name, v, element.span) {
                            out.push(decl);
                        }
                    }
                }
            } else if LAYOUT_HINTS.contains(&field)
                && let Some(decl) = self.inline_decl(field, value, element.span)
            {
                out.push(decl);
            }
        }
        out
    }

    fn inline_decl(&self, name: &str, value: &Value, span: Span) -> Option<Decl> {
        let prop = self.compiler.property_named(name, span)?;
        let property = self.cascade.properties.get(prop);
        let declared = Declared::Value(value.clone());
        if let Err(message) = check(&declared, &property.ty) {
            diag::report(
                self.db,
                Diagnostic::error(
                    span,
                    eco_format!("wrong value for `{}`: {message}", property.name),
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
    fn view(&self, siblings: &[Child<'_>], i: usize, need: &Need, needs: &Needs) -> NodeView {
        let fields = &needs.fields;
        let full = Position {
            first: i == 0,
            last: i + 1 == siblings.len(),
            index: u32::try_from(i + 1).unwrap_or(u32::MAX),
            column: 0,
        };
        let context = Need {
            prev: 0,
            ..needs.context
        };
        let prev = (1..=need.prev)
            .filter_map(|k| i.checked_sub(k))
            .map(|j| self.view(siblings, j, &context, needs))
            .collect();
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
