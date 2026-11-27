//! Pairing instances across the variants of a numbering group (SPEC §6.1).
//!
//! Each variant evaluates its own document, and the common tree pairs the
//! element instances of a group's variants by location: by span and the
//! loop iterations and calls that made them ([`crate::locate::Origin`]).
//! Iteration `i` of a loop is the same in every variant only if the loop
//! iterates over the same items in each. Evaluation records what each loop
//! iterated over ([`LoopRun`]), and [`check_loops`] reports a loop whose
//! items differ between the variants of a group when it makes an element
//! that a shared counter counts: its numbering cannot be shared, so the
//! build says so instead of numbering wrongly.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use ecow::EcoVec;
use folio_component::Name;
use folio_syntax::Span;

use crate::db::{Db, Project};
use crate::diag::{self, Diagnostic};
use crate::eval::{self, Value};

/// A run of a loop, or of a function that calls a closure for each item:
/// what it iterated over and which elements its output holds.
#[salsa::accumulator]
#[derive(Debug)]
pub struct LoopRun {
    /// The loop, or the call of the function.
    pub span: Span,
    /// A hash of the items, in order. It is compared within one process,
    /// never written out.
    pub items: u64,
    /// The names of the elements its output holds, at any depth.
    pub names: EcoVec<Name>,
}

/// Records a run of the loop at `span` over `items`, which gave `output`.
pub(crate) fn record<'a>(
    db: &dyn Db,
    span: Span,
    items: impl IntoIterator<Item = &'a Value>,
    output: &Value,
) {
    use salsa::Accumulator;
    let mut hasher = DefaultHasher::new();
    let mut count = 0u64;
    for item in items {
        item.hash(&mut hasher);
        count += 1;
    }
    count.hash(&mut hasher);
    let mut names = BTreeSet::new();
    element_names(output, &mut names, &mut HashSet::new());
    LoopRun {
        span,
        items: hasher.finish(),
        names: names.into_iter().collect(),
    }
    .accumulate(db);
}

/// Adds the names of the elements a value holds to `out`, looking at each
/// shared piece of content once.
fn element_names(value: &Value, out: &mut BTreeSet<Name>, seen: &mut HashSet<usize>) {
    match value {
        Value::Content(content) => {
            if !seen.insert(content.nodes().as_ptr() as usize) {
                return;
            }
            for node in content.nodes() {
                match node {
                    eval::Node::Element(element) => {
                        out.insert(element.name.name.clone());
                        for (_, field) in element.fields.iter() {
                            element_names(field, out, seen);
                        }
                    }
                    eval::Node::Slot(slot) => {
                        element_names(&Value::Content(slot.body.clone()), out, seen);
                    }
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter() {
                element_names(item, out, seen);
            }
        }
        Value::Dict(dict) => {
            for (_, item) in dict.iter() {
                element_names(item, out, seen);
            }
        }
        _ => {}
    }
}

/// Whether a shared counter counts the elements of a name: the counter
/// named after them, or `equation` for display maths (SPEC §4.5).
fn counts(counter: &Name, element: &Name) -> bool {
    counter == element || (counter.as_str() == "equation" && element.as_str() == "math")
}

/// The runs of each loop in the evaluation of a variant: for each loop,
/// the hashes of what its runs iterated over, sorted, and the names of the
/// elements they made.
type Runs = BTreeMap<u64, (Span, Vec<u64>, BTreeSet<Name>)>;

fn runs(db: &dyn Db, project: Project, variant: &Name) -> Runs {
    let mut out = Runs::new();
    for run in eval::evaluate::accumulated::<LoopRun>(db, project, Some(variant.clone())) {
        let entry = out
            .entry(run.span.into_raw().get())
            .or_insert_with(|| (run.span, Vec::new(), BTreeSet::new()));
        entry.1.push(run.items);
        entry.2.extend(run.names.iter().cloned());
    }
    for (_, items, _) in out.values_mut() {
        items.sort_unstable();
    }
    out
}

/// Reports, for a variant in a numbering group, each loop that iterates
/// over different items in another variant of the group while it makes an
/// element a shared counter counts (`variant-loop-differs`, SPEC §6.1).
#[salsa::tracked]
pub fn check_loops(db: &dyn Db, project: Project, variant: Option<Name>) {
    let Some(variant) = variant else {
        return;
    };
    let manifest = project.manifest(db);
    let Some(group) = manifest.numbering_group(variant.as_str()) else {
        return;
    };
    let shared: Vec<Name> = group
        .shared_counters
        .iter()
        .map(|c| c.v.name.clone())
        .collect();
    let own = runs(db, project, &variant);
    let mut reported = HashSet::new();
    for other in &group.variants {
        if other.v == variant {
            continue;
        }
        let theirs = runs(db, project, &other.v);
        for (key, (span, items, names)) in &own {
            let (their_items, their_names) = match theirs.get(key) {
                Some((_, items, names)) => (items.as_slice(), Some(names)),
                None => (&[][..], None),
            };
            if items.as_slice() == their_items {
                continue;
            }
            let counted = shared.iter().find(|counter| {
                names
                    .iter()
                    .chain(their_names.into_iter().flatten())
                    .any(|name| counts(counter, name))
            });
            let Some(counter) = counted else {
                continue;
            };
            if !reported.insert(*key) {
                continue;
            }
            diag::report(
                db,
                Diagnostic::error(
                    *span,
                    format!(
                        "this loop runs over different items in `{variant}` and `{}`, so the \
                         `{counter}` numbering of numbering group `{}` cannot be shared",
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
}
