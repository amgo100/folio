//! The common tree of a numbering group (SPEC §6.1): the union of what
//! the group's variants show, on which their shared counters are counted.
//!
//! Each variant of a group is selected on its own ([`super::select`]), and
//! the common tree merges the selected trees by location: an instance that
//! several variants show is one node, holding the union of what each shows
//! inside it, and an instance that only some show stands where they show
//! it, after the instance they show before it. The common tree is counted
//! once per group and target, like a variant's own tree but without
//! reporting anything ([`crate::resolve::count_common`]), and each variant
//! of the group takes the values of its shared counters from it, so that
//! exam and mark scheme give a question the same number whatever each
//! hides.
//!
//! Each variant's tree is the one its builds for the target select, so the
//! common tree reuses their Style and Select results. SPEC §6.1 styles the
//! common tree without target overlays, so that format-specific rules
//! cannot change shared numbering; until a build can be styled without
//! them, a target overlay that changes what a group counts is a warning
//! ([`check_target_overlays`]).

use std::collections::{HashMap, HashSet};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, Location, Name};

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::locate::StyledChildren;
use crate::pipeline::{select, selected_root};
use crate::resolve::{Target, count_common};
use crate::style::{NodeName, StyledNode};
use crate::style::{Origin, cascade};

/// What the common tree of a numbering group counts: each counted target
/// (an element, or a line of an equation), the counter that counts it and
/// its values, in document order.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct CommonNumbers {
    /// Each counted target, its counter and its values.
    pub numbers: EcoVec<(Target, EcoString, EcoVec<u64>)>,
}

/// The build that stands for a numbering group and a target: the build of
/// its first variant, so that every variant of the group asks for one
/// common count. `None` when the build's variant is in no group or its
/// group shares no counters.
pub fn group_build<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<Build<'db>> {
    let build = build.base(db);
    let project = build.project(db);
    let variant = build.variant(db).as_ref()?;
    let group = project.manifest(db).numbering_group(variant.as_str())?;
    if group.shared_counters.is_empty() {
        return None;
    }
    let first = group.variants.first()?.v.clone();
    Some(Build::new(db, project, build.target(db), Some(first)))
}

/// Counts the common tree of the numbering group that `build` stands for
/// ([`group_build`]): the values of every counter, of which its variants
/// keep those they share. Empty when a variant of the group cannot be
/// styled.
#[salsa::tracked(returns(ref))]
pub fn common_numbers(db: &dyn Db, build: Build<'_>) -> CommonNumbers {
    let project = build.project(db);
    let Some(variant) = build.variant(db).as_ref() else {
        return CommonNumbers::default();
    };
    let Some(group) = project.manifest(db).numbering_group(variant.as_str()) else {
        return CommonNumbers::default();
    };
    let mut trees = Vec::new();
    for name in &group.variants {
        let each = Build::new(db, project, build.target(db), Some(name.v.clone()));
        let (Some(root), Some(selected)) = (selected_root(db, each), select(db, each).as_ref())
        else {
            return CommonNumbers::default();
        };
        trees.push((root, &selected.styled.root));
    }
    let (root, styled) = merge(&trees);
    let numbers = count_common(db, build, &root, &styled);
    CommonNumbers {
        numbers: numbers
            .into_iter()
            .map(|n| (n.target, n.counter, n.values))
            .collect(),
    }
}

/// Warns at each declaration in a target overlay (`@target` or a target's
/// stylesheet) that changes what a variant of numbering group `group`
/// shows or how its counters restart (`display`, `show-only`,
/// `counter-reset`), for the variants' builds of `build`'s target: the
/// group's common tree is counted with it, so the shared numbers may
/// differ between targets.
pub fn check_target_overlays(db: &dyn Db, build: Build<'_>, group: &Name) {
    let build = build.base(db);
    let project = build.project(db);
    let Some(variant) = build.variant(db).as_ref() else {
        return;
    };
    let Some(decl) = project.manifest(db).numbering_group(variant.as_str()) else {
        return;
    };
    let mut seen = HashSet::new();
    for name in &decl.variants {
        let each = Build::new(db, project, build.target(db), Some(name.v.clone()));
        let cascade = cascade(db, each);
        let props = &cascade.properties;
        let counted = [
            props.builtin("display"),
            props.builtin("show-only"),
            props.builtin("counter-reset"),
        ];
        for rule in &cascade.rules {
            if rule.priority.origin != Origin::Target {
                continue;
            }
            for d in &rule.declarations {
                if counted.contains(&d.prop) && seen.insert(d.span) {
                    diag::report(
                        db,
                        Diagnostic::warning(
                            d.span,
                            eco_format!(
                                "this target overlay changes what numbering group `{group}` \
                                 counts, so its shared numbers may differ between targets"
                            ),
                        )
                        .with_hint(
                            "Folio does not count a group's common tree without target \
                             overlays yet; set visibility and counter restarts in variant \
                             overlays instead",
                        ),
                    );
                }
            }
        }
    }
}

/// One instance as each variant that shows it has it: its element and
/// styled node.
type Versions<'a> = Vec<(&'a Element, &'a StyledNode)>;

/// Merges the versions of one instance into one element and styled node:
/// the first version's, holding in each field of content the union of the
/// elements each version holds there. Only elements are kept: text is not
/// counted.
fn merge(versions: &[(&Element, &StyledNode)]) -> (Element, StyledNode) {
    let (first, first_node) = versions[0];
    // The fields of content, in the order the versions give them.
    let mut keys: Vec<&EcoString> = Vec::new();
    for (element, _) in versions {
        for (key, value) in element.fields.iter() {
            if matches!(value, Value::Content(_)) && !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    let mut element = first.clone();
    let mut body_nodes = Vec::new();
    let mut part_nodes = Vec::new();
    for key in keys {
        if key == BODY {
            let lists: Vec<Versions<'_>> = versions
                .iter()
                .filter_map(|(element, node)| {
                    let Some(Value::Content(content)) = element.field(BODY) else {
                        return None;
                    };
                    Some(paired(content, &mut StyledChildren::new(node)))
                })
                .collect();
            let (content, nodes) = merge_lists(&lists);
            element.fields.insert(key.clone(), Value::Content(content));
            body_nodes = nodes;
        } else {
            let Some(name) = Name::new(key) else {
                continue;
            };
            let mut part_node: Option<&StyledNode> = None;
            let lists: Vec<Versions<'_>> = versions
                .iter()
                .filter_map(|(element, node)| {
                    let Some(Value::Content(content)) = element.field(key) else {
                        return None;
                    };
                    let part = StyledChildren::new(node).part(&name)?;
                    part_node.get_or_insert(part);
                    Some(paired(content, &mut StyledChildren::new(part)))
                })
                .collect();
            let Some(part_node) = part_node else {
                continue;
            };
            let (content, nodes) = merge_lists(&lists);
            element.fields.insert(key.clone(), Value::Content(content));
            part_nodes.push(StyledNode {
                children: nodes.into_iter().collect(),
                ..part_node.clone()
            });
        }
    }
    // Parts that are not content, such as the style of a formula's
    // vectors, stay as the first version has them.
    let others: Vec<StyledNode> = first_node
        .children
        .iter()
        .filter(|child| match &child.name {
            NodeName::Part(name) => first.field(name.as_str()).is_none(),
            _ => false,
        })
        .cloned()
        .collect();
    let children = body_nodes
        .into_iter()
        .chain(part_nodes)
        .chain(others)
        .collect();
    let node = StyledNode {
        children,
        ..first_node.clone()
    };
    (element, node)
}

/// The elements of content with their styled nodes; an element without a
/// node is left out, as no stage after Select reads it.
fn paired<'a>(content: &'a Content, kids: &mut StyledChildren<'a>) -> Versions<'a> {
    content
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Element(element) => Some((element, kids.element(element)?)),
            _ => None,
        })
        .collect()
}

/// Merges lists of instances by location: the first list's order, with
/// each instance of a later list that the merged list lacks put after the
/// instance that list has before it. Each instance is merged from the
/// versions of every list that has it.
fn merge_lists(lists: &[Versions<'_>]) -> (Content, Vec<StyledNode>) {
    let mut order: Vec<Location> = Vec::new();
    let mut versions: HashMap<Location, Versions<'_>> = HashMap::new();
    for list in lists {
        let mut at = 0;
        let placed: HashSet<Location> = order.iter().copied().collect();
        for &(element, node) in list {
            let location = element.location;
            if placed.contains(&location) {
                at = order
                    .iter()
                    .position(|l| *l == location)
                    .map_or(at, |i| i + 1);
            } else if !versions.contains_key(&location) {
                order.insert(at, location);
                at += 1;
            }
            versions.entry(location).or_default().push((element, node));
        }
    }
    let mut content = Vec::with_capacity(order.len());
    let mut nodes = Vec::with_capacity(order.len());
    for location in order {
        let (element, node) = merge(&versions[&location]);
        content.push(Node::Element(element));
        nodes.push(node);
    }
    (content.into_iter().collect(), nodes)
}
