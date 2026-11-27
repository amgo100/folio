//! Component lints (SPEC §3.2): each component in use reads the instances
//! of its own elements through an [`InstanceTree`] made from the document
//! tree, and the diagnostics its lints return are reported like the core's,
//! at the instances they point at.

use std::collections::HashMap;

use ecow::{EcoVec, eco_format};
use folio_component::{
    Instance, InstanceTree, LintDiagnostic, Location, Name, Part, Severity, Value as Plain,
};
use folio_syntax::Span;

use crate::db::{Build, Db, Project, component_host};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::model::document;

/// Runs the lints of the components a build uses, over its variant's
/// document tree, and reports what they find. The document tree is the same
/// for every target of a variant, so the lints run once per variant.
pub fn check_lints(db: &dyn Db, build: Build<'_>) {
    lint_variant(db, build.project(db), build.variant(db).clone());
}

#[salsa::tracked]
fn lint_variant(db: &dyn Db, project: Project, variant: Option<Name>) {
    let Some(host) = component_host(db, project).as_ref() else {
        return;
    };
    let Some(document) = document(db, project, variant) else {
        return;
    };
    for component in host.components() {
        let namespace = component.manifest.namespace();
        let mut spans = HashMap::new();
        let view = InstanceTree::new(own_in_element(&namespace, &document.root, &mut spans));
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

/// The component's instances in an element: the element itself if it is
/// one, else those in its fields, in their order.
fn own_in_element(
    namespace: &Name,
    element: &Element,
    spans: &mut HashMap<Location, Span>,
) -> Vec<Instance> {
    if element.name.namespace.as_ref() == Some(namespace) {
        return vec![instance(namespace, element, spans)];
    }
    let mut out = Vec::new();
    for (_, value) in element.fields.iter() {
        own_in_value(namespace, value, spans, &mut out);
    }
    out
}

/// The component's instances in a field's value, at any depth that no
/// other instance of the component is between.
fn own_in_value(
    namespace: &Name,
    value: &Value,
    spans: &mut HashMap<Location, Span>,
    out: &mut Vec<Instance>,
) {
    match value {
        Value::Content(content) => out.extend(own_in_content(namespace, content, spans)),
        Value::Array(items) => {
            for item in items.as_slice() {
                own_in_value(namespace, item, spans, out);
            }
        }
        Value::Dict(dict) => {
            for (_, item) in dict.iter() {
                own_in_value(namespace, item, spans, out);
            }
        }
        _ => {}
    }
}

fn own_in_content(
    namespace: &Name,
    content: &Content,
    spans: &mut HashMap<Location, Span>,
) -> Vec<Instance> {
    let mut out = Vec::new();
    for node in content.nodes() {
        if let Node::Element(element) = node {
            out.extend(own_in_element(namespace, element, spans));
        }
    }
    out
}

/// The view of one of the component's instances: its fields that are not
/// content as plain values, and a part for each field that holds content.
fn instance(namespace: &Name, element: &Element, spans: &mut HashMap<Location, Span>) -> Instance {
    spans.insert(element.location, element.span);
    let mut out = Instance::new(element.name.clone(), element.location);
    out.label = element.label.clone();
    let mut fields = EcoVec::new();
    let mut parts = EcoVec::new();
    for (key, value) in element.fields.iter() {
        // Fields scoped to a variant or target (`override@markscheme`) are
        // not names; the view holds the fields that apply to every build.
        let Some(name) = Name::new(key) else {
            continue;
        };
        match value {
            Value::Content(content) => parts.push(
                Part::new(name, own_in_content(namespace, content, spans))
                    .with_empty(content.is_empty()),
            ),
            _ => {
                if let Some(plain) = plain(value) {
                    fields.push((name, plain));
                }
            }
        }
    }
    out.fields = fields;
    out.parts = parts;
    out
}

/// A value as plain data, if it has a plain form: content, functions and
/// the like have none.
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
