//! Component lints (SPEC §3.2): each component in use reads the instances
//! of its own elements through an [`InstanceTree`] made from the document
//! tree, and the diagnostics its lints return are reported like the core's,
//! at the instances they point at.
//!
//! The core's lint `variant-condition` (SPEC §6.2) reads the syntax of the
//! content files a build reads: an `#if` that tests only which variant is
//! built, where content is written rather than generated, would be clearer
//! as an element the variant's stylesheet hides or as `#in-variant`.

use std::collections::HashMap;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{
    BODY, Body, ElementSchema, FieldType, Instance, InstanceTree, LintDiagnostic, Location, Name,
    Part, Registered, Severity, Value as Plain,
};
use folio_syntax::ast::{self, AstNode, BinOp, Expr};
use folio_syntax::{Span, SyntaxKind, SyntaxNode};

use crate::db::{Build, Db, Project, SourceFile, build_files, component_host};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::model::document;

/// Runs the lints of the components a build uses, over its variant's
/// document tree, and reports what they find. The document tree is the same
/// for every target of a variant, so the lints run once per variant. A
/// build whose components declare no elements has nothing to lint, and
/// does not read the document tree here.
pub fn check_lints(db: &dyn Db, build: Build<'_>) {
    for &file in build_files(db, build) {
        if file.path(db).extension() == Some("fol") {
            for diagnostic in variant_conditions(db, file) {
                diag::report(db, diagnostic.clone());
            }
        }
    }
    let project = build.project(db);
    let linted = component_host(db, project)
        .as_ref()
        .is_some_and(|host| host.components().any(has_elements));
    if linted {
        lint_variant(db, project, build.variant(db).clone());
    }
}

/// The lint `variant-condition` over a content file (SPEC §6.2): a warning
/// at each `if` whose condition only compares `variant` with names and
/// whose branches are inline markup, which `#in-variant` and
/// `#except-variant` could hold, written in markup or in code that a loop
/// or closure does not repeat. Content that a loop or a function generates
/// is what scripts are for, so `if` there is left alone.
#[salsa::tracked(returns(ref))]
fn variant_conditions(db: &dyn Db, file: SourceFile) -> EcoVec<Diagnostic> {
    let mut out = EcoVec::new();
    walk_conditions(file.source(db).root(), &mut out);
    out
}

fn walk_conditions(node: &SyntaxNode, out: &mut EcoVec<Diagnostic>) {
    match node.kind() {
        // Generated content: a loop or a closure repeats its body.
        SyntaxKind::ForLoop | SyntaxKind::WhileLoop | SyntaxKind::Closure => return,
        SyntaxKind::Conditional => {
            if let Some(conditional) = node.cast::<ast::Conditional<'_>>()
                && let Some(diagnostic) = variant_condition(conditional)
            {
                out.push(diagnostic);
            }
        }
        _ => {}
    }
    for child in node.children() {
        walk_conditions(child, out);
    }
}

/// The warning for an `if` that tests only the variant: `variant == "a"`,
/// `variant != "a"`, `variant in ("a", "b")` or `variant not in (..)`,
/// either way round.
fn variant_condition(conditional: ast::Conditional<'_>) -> Option<Diagnostic> {
    // `#in-variant` holds inline content, so only an `if` that chooses
    // inline markup could be written with it.
    if !is_inline_markup(conditional.if_body())
        || conditional
            .else_body()
            .is_some_and(|body| !is_inline_markup(body))
    {
        return None;
    }
    let mut condition = conditional.condition();
    while let Expr::Parenthesized(inner) = condition {
        condition = inner.expr();
    }
    let Expr::Binary(binary) = condition else {
        return None;
    };
    let is_variant =
        |expr: Expr<'_>| matches!(expr, Expr::Ident(ident) if ident.as_str() == "variant");
    let name = |expr: Expr<'_>| match expr {
        Expr::Str(text) => Some(vec![text.get()]),
        _ => None,
    };
    let names = |expr: Expr<'_>| -> Option<Vec<EcoString>> {
        match expr {
            Expr::Array(array) => array
                .items()
                .map(|item| match item {
                    ast::ArrayItem::Pos(Expr::Str(text)) => Some(text.get()),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    };
    let (lhs, rhs) = (binary.lhs(), binary.rhs());
    let op = binary.op();
    let listed = match op {
        // `in` a string tests for a substring, so only a list names
        // variants.
        BinOp::Eq | BinOp::Neq if is_variant(lhs) => name(rhs)?,
        BinOp::Eq | BinOp::Neq if is_variant(rhs) => name(lhs)?,
        BinOp::In | BinOp::NotIn if is_variant(lhs) => names(rhs)?,
        _ => return None,
    };
    let quoted: Vec<String> = listed.iter().map(|n| format!("{n:?}")).collect();
    let hint = match (op, &listed[..]) {
        (BinOp::Eq | BinOp::In, [name]) => eco_format!(
            "write the content as `#in-variant({name:?})[..]`, or give it an element that \
             `@variant` rules in the stylesheet show or hide"
        ),
        (BinOp::Neq | BinOp::NotIn, [name]) => eco_format!(
            "write the content as `#except-variant({name:?})[..]`, or give it an element that \
             `@variant` rules in the stylesheet show or hide"
        ),
        _ => eco_format!(
            "give the content an element that `@variant {}` rules in the stylesheet show or \
             hide",
            listed.join(", ")
        ),
    };
    Some(
        Diagnostic::warning(
            binary.to_untyped().span(),
            eco_format!(
                "this `if` only tests whether the variant is {}, which the content can say \
                 without a script",
                quoted.join(" or ")
            ),
        )
        .with_code("variant-condition")
        .with_hint(hint),
    )
}

/// Whether an expression is a content block of inline markup: no
/// paragraph break, heading or list item.
fn is_inline_markup(expr: Expr<'_>) -> bool {
    fn inline(node: &SyntaxNode) -> bool {
        node.children().all(|child| {
            !matches!(
                child.kind(),
                SyntaxKind::Parbreak
                    | SyntaxKind::Heading
                    | SyntaxKind::ListItem
                    | SyntaxKind::EnumItem
                    | SyntaxKind::TermItem
            ) && (child.kind() != SyntaxKind::Markup || inline(child))
        })
    }
    matches!(expr, Expr::ContentBlock(_)) && inline(expr.to_untyped())
}

/// Whether a component has elements whose lints can run: the project's
/// own elements have none (SPEC §4.3), so a project that declares
/// elements does not walk its document tree for them.
fn has_elements(component: &Registered) -> bool {
    !crate::eval::is_project(component) && !component.declarations.elements.is_empty()
}

#[salsa::tracked]
fn lint_variant(db: &dyn Db, project: Project, variant: Option<Name>) {
    let Some(host) = component_host(db, project).as_ref() else {
        return;
    };
    let Some(document) = document(db, project, variant) else {
        return;
    };
    for component in host.components().filter(|c| has_elements(c)) {
        let namespace = component.manifest.namespace();
        let mut spans = HashMap::new();
        let mut cx = Cx {
            component,
            namespace: &namespace,
            spans: &mut spans,
        };
        let view = InstanceTree::new(cx.in_element(&document.root));
        for found in run_lint(db, project, namespace.clone(), view).iter() {
            diag::report(db, diagnostic(&namespace, found, &spans));
        }
    }
}

/// A component's lints over a view: a query of its own, so that an edit
/// that leaves the component's instances as they were reuses its result.
#[salsa::tracked(returns(ref))]
fn run_lint(
    db: &dyn Db,
    project: Project,
    namespace: Name,
    view: InstanceTree,
) -> EcoVec<LintDiagnostic> {
    component_host(db, project)
        .as_ref()
        .and_then(|host| {
            host.components()
                .find(|c| c.manifest.namespace() == namespace)
        })
        .map(|component| component.lint(&view))
        .unwrap_or_default()
}

/// A lint diagnostic as the core reports it: at its instance's span, with
/// its code qualified by the component's namespace.
fn diagnostic(
    namespace: &Name,
    found: &LintDiagnostic,
    spans: &HashMap<Location, Span>,
) -> Diagnostic {
    let span = |at: &Location| spans.get(at).copied().unwrap_or_else(Span::detached);
    let message = match &found.field {
        Some(field) => eco_format!("{} (field `{field}`)", found.message),
        None => found.message.clone(),
    };
    let mut out = match found.severity {
        Severity::Error => Diagnostic::error(span(&found.at), message),
        _ => Diagnostic::warning(span(&found.at), message),
    }
    .with_code(eco_format!("{namespace}.{}", found.code));
    for related in &found.related {
        out = out.with_related(span(&related.at), related.message.clone());
    }
    for hint in &found.hints {
        out = out.with_hint(hint.clone());
    }
    out
}

/// What walking the document tree for one component needs.
struct Cx<'a> {
    component: &'a Registered,
    namespace: &'a Name,
    /// The span of each instance, to report diagnostics at.
    spans: &'a mut HashMap<Location, Span>,
}

impl Cx<'_> {
    /// The component's instances in an element: the element itself if it
    /// is one, else those in its fields, in their order.
    fn in_element(&mut self, element: &Element) -> Vec<Instance> {
        if element.name.namespace.as_ref() == Some(self.namespace) {
            return vec![self.instance(element)];
        }
        let mut out = Vec::new();
        for (_, value) in element.fields.iter() {
            self.in_value(value, &mut out);
        }
        out
    }

    /// The component's instances in a field's value, at any depth that no
    /// other instance of the component is between.
    fn in_value(&mut self, value: &Value, out: &mut Vec<Instance>) {
        match value {
            Value::Content(content) => out.extend(self.in_content(content)),
            Value::Array(items) => {
                for item in items.as_slice() {
                    self.in_value(item, out);
                }
            }
            Value::Dict(dict) => {
                for (_, item) in dict.iter() {
                    self.in_value(item, out);
                }
            }
            _ => {}
        }
    }

    fn in_content(&mut self, content: &Content) -> Vec<Instance> {
        let mut out = Vec::new();
        for node in content.nodes() {
            if let Node::Element(element) = node {
                out.extend(self.in_element(element));
            }
        }
        out
    }

    /// The view of one of the component's instances: its fields that are
    /// not content as plain values, and a part for each field that holds
    /// content. Every slot and content field its schema declares has a
    /// part, empty when the call left it out.
    fn instance(&mut self, element: &Element) -> Instance {
        self.spans.insert(element.location, element.span);
        let mut out = Instance::new(element.name.clone(), element.location);
        out.label = element.label.clone();
        let mut fields = EcoVec::new();
        let mut parts: EcoVec<Part> = EcoVec::new();
        for (key, value) in element.fields.iter() {
            // Fields scoped to a variant or target (`override@markscheme`)
            // are not names; the view holds the fields that apply to every
            // build.
            let Some(name) = Name::new(key) else {
                continue;
            };
            match value {
                Value::Content(content) => {
                    let children = self.in_content(content);
                    parts.push(Part::new(name, children).with_empty(content.is_empty()));
                }
                _ => {
                    if let Some(plain) = plain(value) {
                        fields.push((name, plain));
                    }
                }
            }
        }
        if let Some(schema) = self.component.element(element.name.name.as_str()) {
            for declared in content_parts(schema) {
                if !parts.iter().any(|p| p.name == declared) {
                    parts.push(Part::new(declared, []));
                }
            }
        }
        out.fields = fields;
        out.parts = parts;
        out
    }
}

/// The names of the parts a schema declares: its body, its slots and its
/// content fields.
fn content_parts(schema: &ElementSchema) -> Vec<Name> {
    let mut out = Vec::new();
    match &schema.body {
        Body::None => {}
        Body::Slots(slots) => out.extend(slots.iter().map(|s| s.name.clone())),
        Body::Content(_) => out.extend(Name::new(BODY)),
    }
    out.extend(
        schema
            .fields
            .iter()
            .filter(|f| f.ty == FieldType::Content)
            .map(|f| f.name.clone()),
    );
    out
}

/// A value as plain data, if it has a plain form. Content, functions and
/// the like have none, nor have a length that mixes `em` and absolute
/// units and a dictionary whose keys are not names: the view leaves such
/// fields out, as it leaves out fields that are not given and default to
/// `none`. A label becomes a string.
fn plain(value: &Value) -> Option<Plain> {
    Some(match value {
        Value::None => Plain::None,
        Value::Auto => Plain::Auto,
        Value::Bool(b) => Plain::Bool(*b),
        Value::Int(i) => Plain::Int(*i),
        Value::Float(f) => Plain::Number(*f),
        Value::Str(s) | Value::Label(s) => Plain::Str(s.clone()),
        Value::Length(length) if length.em.is_zero() => Plain::Length(length.abs),
        Value::Length(length) if length.abs.is_zero() => Plain::Em(length.em),
        Value::Ratio(ratio) => Plain::Ratio(*ratio),
        Value::Color(color) => Plain::Color(*color),
        Value::Duration(duration) => Plain::Duration(duration.0),
        Value::Angle(angle) => Plain::Angle(angle.0),
        Value::Fraction(fraction) => Plain::Fraction(fraction.0),
        Value::Array(items) => {
            Plain::Array(items.as_slice().iter().map(plain).collect::<Option<_>>()?)
        }
        Value::Dict(dict) => Plain::Dict(
            dict.iter()
                .map(|(k, v)| Some((Name::new(k)?, plain(v)?)))
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}
