//! The local overrides of a build (SPEC §5.4), listed for
//! `folio check --report overrides` and the language server: the id rules
//! of the overrides stylesheet, and the scoped rules, layout hints,
//! `override` fields and `styled` spans of the content the build
//! evaluates.
//!
//! Each override is listed once, at the span it was written at, however
//! many instances a loop or a function call made from it.

use std::collections::HashSet;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, LAYOUT_HINTS, QualifiedName, UNIVERSAL_FIELDS};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{Span, SyntaxKind, SyntaxNode};

use super::rules::{Origin, cascade};
use crate::db::{Build, Db};
use crate::eval::{Content, Element, Node, Value};
use crate::model::document;

/// The name of the element whose fields are style properties: the styled
/// span or block of SPEC §5.4 (D-341-3).
pub const STYLED: &str = "styled";

/// The code of the lint that warns about a colour written as a literal
/// in a local override, where a token would keep it in step with the
/// stylesheets (SPEC §5.4).
pub const OVERRIDE_LITERAL: &str = "override-literal";

/// The colours a scoped rule's declarations write as literals: `#d62728`
/// and calls of `rgb`, for the lint `override-literal`.
pub(crate) fn literal_colours<'a>(rule: ast::ScopedRule<'a>) -> Vec<&'a SyntaxNode> {
    fn walk<'a>(node: &'a SyntaxNode, out: &mut Vec<&'a SyntaxNode>) {
        match node.kind() {
            SyntaxKind::Color => out.push(node),
            SyntaxKind::FuncCall
                if node.cast::<ast::FuncCall<'_>>().is_some_and(
                    |call| matches!(call.callee(), ast::Expr::Ident(id) if id.get() == "rgb"),
                ) =>
            {
                out.push(node);
            }
            _ => node.children().for_each(|child| walk(child, out)),
        }
    }
    let mut out = Vec::new();
    for decl in rule.declarations() {
        walk(decl.to_untyped(), &mut out);
    }
    out
}

/// The form of a local override (SPEC §5.4), weakest first.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum OverrideForm {
    /// An id rule in the overrides stylesheet: `#{fig:ball} { width: 40%; }`.
    IdRule,
    /// A scoped rule in content: `#rule question { numbering: "(a)"; }`.
    ScopedRule,
    /// Layout hints of an element: `#figure(placement: wrap, width: 1/3)`.
    Hint,
    /// The override attribute: `override: (space-before: 24pt)`.
    Attribute,
    /// A styled span or block: `#styled(color: tok.warn)[..]`.
    Styled,
}

impl OverrideForm {
    /// Every form, weakest first.
    pub const ALL: [Self; 5] = [
        Self::IdRule,
        Self::ScopedRule,
        Self::Hint,
        Self::Attribute,
        Self::Styled,
    ];

    /// The form's name in reports: `id-rule`, `scoped-rule`, `hint`,
    /// `override` or `styled`.
    pub fn name(self) -> &'static str {
        match self {
            Self::IdRule => "id-rule",
            Self::ScopedRule => "scoped-rule",
            Self::Hint => "hint",
            Self::Attribute => "override",
            Self::Styled => "styled",
        }
    }

    /// Whether the override is written in content, rather than in the
    /// overrides stylesheet.
    pub fn in_content(self) -> bool {
        self != Self::IdRule
    }
}

/// A local override as it is written.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Override {
    /// Its form.
    pub form: OverrideForm,
    /// Where it is written: the rule, or the element whose fields hold it.
    pub span: Span,
    /// What it styles: an element's name (`figure`), or a rule's
    /// selectors as written (`#{fig:ball}`, `question > answer`).
    pub subject: EcoString,
    /// The label of the element it styles, if it has one.
    pub label: Option<EcoString>,
    /// The properties it declares, as written (`space-before`,
    /// `multicol.columns`), in the order they are written.
    pub properties: EcoVec<EcoString>,
    /// The variant or target it is limited to, as written after `@`
    /// (`markscheme`, `target pptx`), or `None`.
    pub scope: Option<EcoString>,
}

/// The local overrides that a build reads (SPEC §5.4): the id rules of the
/// overrides stylesheet that apply to it, and every override written in
/// the content its variant evaluates, whether or not a scope limits it to
/// another variant or target. In document order, the id rules first.
#[salsa::tracked(returns(ref))]
pub fn local_overrides(db: &dyn Db, build: Build<'_>) -> EcoVec<Override> {
    let mut out = Collector::default();
    let cascade = cascade(db, build);
    for rule in &cascade.rules {
        if rule.priority.origin != Origin::Overrides {
            continue;
        }
        let Some(label) = rule.selector.subject().label.clone() else {
            continue;
        };
        let properties = rule
            .declarations
            .iter()
            .map(|decl| cascade.properties.get(decl.prop).name.to_string().into())
            .collect();
        out.push(Override {
            form: OverrideForm::IdRule,
            span: rule.span,
            subject: eco_format!("#{{{label}}}"),
            label: Some(label),
            properties,
            scope: None,
        });
    }
    let project = build.project(db);
    if let Some(doc) = document(
        db,
        project,
        build.variant(db).clone(),
        build.version(db).clone(),
    )
    .as_ref()
    {
        out.element(&doc.root);
    }
    out.list
}

/// Collects overrides, each once.
#[derive(Default)]
struct Collector {
    list: EcoVec<Override>,
    seen: HashSet<(OverrideForm, Span, Option<EcoString>)>,
}

impl Collector {
    fn push(&mut self, item: Override) {
        if self.seen.insert((item.form, item.span, item.scope.clone())) {
            self.list.push(item);
        }
    }

    fn element(&mut self, element: &Element) {
        let subject: EcoString = element.name.to_string().into();
        let styled = is_styled(&element.name);
        // One entry per form and scope, in the order of the fields.
        let mut hints: Vec<(Option<EcoString>, EcoVec<EcoString>)> = Vec::new();
        let mut styles: Vec<(Option<EcoString>, EcoVec<EcoString>)> = Vec::new();
        for (key, value) in element.fields.iter() {
            let (field, scope) = match key.split_once('@') {
                Some((field, scope)) => (field, Some(EcoString::from(scope))),
                None => (key.as_str(), None),
            };
            if field == "override" {
                if let Value::Dict(dict) = value {
                    self.push(Override {
                        form: OverrideForm::Attribute,
                        span: element.span,
                        subject: subject.clone(),
                        label: element.label.clone(),
                        properties: dict.iter().map(|(k, _)| k.clone()).collect(),
                        scope,
                    });
                }
            } else if LAYOUT_HINTS.contains(&field) {
                add(&mut hints, scope, field);
            } else if styled && is_style_field(field) {
                add(&mut styles, scope, field);
            }
        }
        for (form, groups) in [(OverrideForm::Hint, hints), (OverrideForm::Styled, styles)] {
            for (scope, properties) in groups {
                self.push(Override {
                    form,
                    span: element.span,
                    subject: subject.clone(),
                    label: element.label.clone(),
                    properties,
                    scope,
                });
            }
        }
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                self.content(content);
            }
        }
    }

    fn content(&mut self, content: &Content) {
        for node in content.nodes() {
            match node {
                Node::Element(element) => self.element(element),
                Node::Slot(slot) => self.content(&slot.body),
                Node::Rule(syntax) => {
                    if let Some(rule) = syntax.cast::<ast::ScopedRule<'_>>() {
                        self.rule(rule);
                    }
                }
                _ => {}
            }
        }
    }

    fn rule(&mut self, rule: ast::ScopedRule<'_>) {
        let scopes: Vec<EcoString> = rule
            .scopes()
            .map(|scope| {
                let names: Vec<&str> = scope.names().map(|n| n.get().as_str()).collect();
                eco_format!("{} {}", scope.kind(), names.join(", "))
            })
            .collect();
        let properties = rule
            .declarations()
            .map(|decl| match decl.namespace() {
                Some(namespace) => eco_format!("{}.{}", namespace.get(), decl.property().get()),
                None => decl.property().get().clone(),
            })
            .collect();
        let subject = squeeze(&rule.selectors().to_untyped().full_text());
        self.push(Override {
            form: OverrideForm::ScopedRule,
            span: rule.span(),
            subject,
            label: None,
            properties,
            scope: (!scopes.is_empty()).then(|| scopes.join(" ").into()),
        });
    }
}

/// Adds a field to the group of its scope.
fn add(
    groups: &mut Vec<(Option<EcoString>, EcoVec<EcoString>)>,
    scope: Option<EcoString>,
    field: &str,
) {
    match groups.iter_mut().find(|(s, _)| *s == scope) {
        Some((_, fields)) => fields.push(field.into()),
        None => groups.push((scope, EcoVec::from([EcoString::from(field)]))),
    }
}

/// Text with each run of whitespace as one space.
fn squeeze(text: &str) -> EcoString {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ").into()
}

/// Whether an element is the built-in `styled`.
pub(crate) fn is_styled(name: &QualifiedName) -> bool {
    name.namespace.is_none() && name.name.as_str() == STYLED
}

/// Whether a field of a `styled` element names a style property: any but
/// its body and the universal fields.
pub(crate) fn is_style_field(field: &str) -> bool {
    field != BODY && !UNIVERSAL_FIELDS.contains(&field)
}
