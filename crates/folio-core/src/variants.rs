//! Pairing instances across the variants of a numbering group (SPEC §6.1).
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

use std::collections::{HashMap, HashSet};

use ecow::EcoVec;
use folio_component::{Location, Name};
use folio_syntax::Span;

use crate::db::{Db, Project};
use crate::diag::Diagnostic;
use crate::model::{document, visit_origins};

/// Whether a shared counter counts the elements of a name: the counter
/// named after them, or `equation` for display maths (SPEC §4.5).
fn counts(counter: &Name, element: &Name) -> bool {
    counter == element || (counter.as_str() == "equation" && element.as_str() == "math")
}

/// The instances of a variant's document that the shared counters count,
/// by location: their span, their name and the fingerprint of the items
/// that made them.
fn counted(
    db: &dyn Db,
    project: Project,
    variant: &Name,
    shared: &[Name],
) -> HashMap<Location, (Span, Name, u64)> {
    let mut out = HashMap::new();
    if let Some(doc) = document(db, project, Some(variant.clone())) {
        visit_origins(&doc.root, &mut |element, origin| {
            let name = &element.name.name;
            if element.name.namespace.is_none() && shared.iter().any(|c| counts(c, name)) {
                out.insert(
                    element.location,
                    (element.span, name.clone(), origin.items()),
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
    let mut out = EcoVec::new();
    let Some(variant) = variant else {
        return out;
    };
    let manifest = project.manifest(db);
    let Some(group) = manifest.numbering_group(variant.as_str()) else {
        return out;
    };
    let shared: Vec<Name> = group
        .shared_counters
        .iter()
        .filter(|c| c.v.namespace.is_none())
        .map(|c| c.v.name.clone())
        .collect();
    if shared.is_empty() {
        return out;
    }
    let own = counted(db, project, &variant, &shared);
    let mut reported = HashSet::new();
    for other in &group.variants {
        if other.v == variant {
            continue;
        }
        let theirs = counted(db, project, &other.v, &shared);
        // In the order of their spans, so that the diagnostics come in a
        // fixed order.
        let mut differ: Vec<_> = own
            .iter()
            .filter(|(location, (.., items))| {
                theirs
                    .get(location)
                    .is_some_and(|(.., their)| their != items)
            })
            .map(|(_, (span, name, _))| (*span, name.clone()))
            .collect();
        differ.sort_by_key(|(span, _)| span.into_raw().get());
        for (span, name) in differ {
            if !reported.insert(span) {
                continue;
            }
            out.push(
                Diagnostic::error(
                    span,
                    format!(
                        "this `{name}` is made by the same loop iteration in `{variant}` and \
                         `{}` from different items, so numbering group `{}` cannot share its \
                         number",
                        other.v, group.name.v
                    ),
                )
                .with_code("variant-loop-differs")
                .with_hint(
                    "loop over the same items in every variant and leave items out inside the \
                     loop with `if`, or hide them with `display` or `#in-variant`",
                ),
            );
        }
    }
    out
}
