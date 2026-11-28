//! Variants (SPEC §6): what each shows ([`select`]), the common tree of a
//! numbering group on which its shared counters are counted ([`common`]),
//! and pairing instances across the variants of a group (§6.1).
//!
//! Each variant evaluates its own document, and the common tree pairs the
//! element instances of a group's variants by location: by span and the
//! loop iterations and calls that made them ([`crate::locate::Origin`]).
//! Iteration `i` of a loop is the same in every variant only if the loop
//! iterates over the same items in each, which the origin's fingerprint of
//! those items tells. [`check_pairing`] reports an instance that a shared
//! counter counts, paired with an instance of another variant of its group
//! that was made from other items: its numbering cannot be shared, so the
//! build says so instead of numbering wrongly.

pub mod common;
pub mod select;

use std::collections::{HashMap, HashSet};

use ecow::EcoVec;
use folio_component::{ElementKind, Location, Name, QualifiedName};
use folio_syntax::Span;

use crate::db::{Db, Project};
use crate::diag::Diagnostic;
use crate::eval::Element;
use crate::model::{document, visit_origins};

/// Whether a shared counter counts an element: the counter named after
/// it, or `equation` for display maths (SPEC §4.5). A counter named
/// without a namespace counts the elements of that name in any, as an
/// unqualified name may name a component's element (`question` for
/// `exam.question`).
fn counts(counter: &QualifiedName, element: &Element) -> bool {
    let name = &element.name;
    if counter.namespace.is_some() && counter.namespace != name.namespace {
        return false;
    }
    counter.name == name.name
        || (counter.namespace.is_none()
            && name.namespace.is_none()
            && counter.name.as_str() == "equation"
            && name.name.as_str() == "math"
            && element.kind == ElementKind::Block)
}

/// The instances of a variant's document that the shared counters count,
/// by location: their span, their name and the fingerprint of the items
/// that made them.
fn counted(
    db: &dyn Db,
    project: Project,
    variant: &Name,
    version: Option<&Name>,
    shared: &[QualifiedName],
) -> HashMap<Location, (Span, QualifiedName, u64)> {
    let mut out = HashMap::new();
    if let Some(doc) = document(db, project, Some(variant.clone()), version.cloned()) {
        visit_origins(&doc.root, &mut |element, origin| {
            if shared.iter().any(|c| counts(c, element)) {
                out.insert(
                    element.location,
                    (element.span, element.name.clone(), origin.items()),
                );
            }
        });
    }
    out
}

/// The problems of pairing a variant's instances with those of the other
/// variants of its numbering group (SPEC §6.1): each instance that a
/// shared counter counts and that pairs with an instance of another
/// variant made from other items is a `variant-loop-differs` error.
///
/// It reads the other variants' documents, so a build reports these
/// diagnostics beside its own rather than through its queries: what those
/// documents report belongs to their own builds.
#[salsa::tracked(returns(ref))]
pub fn check_pairing(db: &dyn Db, project: Project, variant: Option<Name>) -> EcoVec<Diagnostic> {
    check_version_pairing(db, project, variant, None).clone()
}

/// [`check_pairing`] for one version of a shuffled paper: each version's
/// variants pair with each other, as each version and its mark scheme form
/// their own numbering group (SPEC §6.4).
#[salsa::tracked(returns(ref))]
pub fn check_version_pairing(
    db: &dyn Db,
    project: Project,
    variant: Option<Name>,
    version: Option<Name>,
) -> EcoVec<Diagnostic> {
    let mut out = EcoVec::new();
    let Some(variant) = variant else {
        return out;
    };
    let manifest = project.manifest(db);
    let Some(group) = manifest.numbering_group(variant.as_str()) else {
        return out;
    };
    let shared: Vec<QualifiedName> = group.shared_counters.iter().map(|c| c.v.clone()).collect();
    if shared.is_empty() {
        return out;
    }
    // Every variant's instances, in the order the group names the
    // variants, so that each variant's build reports the same error.
    let all: Vec<(&Name, _)> = group
        .variants
        .iter()
        .map(|v| (&v.v, counted(db, project, &v.v, version.as_ref(), &shared)))
        .collect();
    let Some((_, own)) = all.iter().find(|(v, _)| **v == variant) else {
        return out;
    };
    let mut differ: Vec<(Span, &QualifiedName, Vec<&Name>)> = Vec::new();
    for (location, (span, name, items)) in own {
        let having: Vec<(&Name, u64)> = all
            .iter()
            .filter_map(|(v, counted)| counted.get(location).map(|(.., i)| (*v, *i)))
            .collect();
        if having.iter().any(|(_, i)| i != items) {
            differ.push((*span, name, having.into_iter().map(|(v, _)| v).collect()));
        }
    }
    // In the order of their spans, so that the diagnostics come in a fixed
    // order.
    differ.sort_by_key(|(span, ..)| span.into_raw().get());
    let mut reported = HashSet::new();
    for (span, name, variants) in differ {
        if !reported.insert(span) {
            continue;
        }
        let names: Vec<String> = variants.iter().map(|v| format!("`{v}`")).collect();
        let listed = match names.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        };
        out.push(
            Diagnostic::error(
                span,
                format!(
                    "this `{name}` is made by the same loop iteration in {listed} from \
                     different items, so numbering group `{}` cannot share its number",
                    group.name.v
                ),
            )
            .with_code("variant-loop-differs")
            .with_hint(
                "loop over the same items in every variant and leave items out inside the \
                 loop with `if`, or hide them with `display` or `#in-variant`",
            ),
        );
    }
    out
}
