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
use folio_syntax::Span;

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::{Content, Element, Node, Value};
use crate::locate::StyledChildren;
use crate::model::document;
use crate::pipeline::{select, selected_root};
use crate::resolve::{Target, count_common};
use crate::style::{NodeName, PropId, Properties, StyledNode};
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
/// common tree. `None` when the build's variant is in no group.
pub fn group_build<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<Build<'db>> {
    let build = build.base(db);
    let project = build.project(db);
    let variant = build.variant(db).as_ref()?;
    let group = project.manifest(db).numbering_group(variant.as_str())?;
    let first = group.variants.first()?.v.clone();
    Some(Build::versioned(
        db,
        project,
        build.target(db),
        Some(first),
        build.version(db).clone(),
    ))
}

/// The common tree of a numbering group: the `doc` element and its styled
/// node, merged from what each variant shows, and the instances whose
/// versions restart counters differently.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CommonTree {
    /// The `doc` element.
    pub root: Element,
    /// Its styled node.
    pub styled: StyledNode,
    /// The instances whose versions differ in `counter-reset`.
    differ: Vec<Span>,
}

/// The common tree of the numbering group that `build` stands for
/// ([`group_build`]), or `None` when a variant of the group cannot be
/// styled.
#[salsa::tracked(returns(ref))]
pub fn common_tree(db: &dyn Db, build: Build<'_>) -> Option<CommonTree> {
    let project = build.project(db);
    let variant = build.variant(db).as_ref()?;
    let group = project.manifest(db).numbering_group(variant.as_str())?;
    let mut trees = Vec::new();
    let mut props = None;
    for name in &group.variants {
        let each = Build::versioned(
            db,
            project,
            build.target(db),
            Some(name.v.clone()),
            build.version(db).clone(),
        );
        let root = selected_root(db, each)?;
        let selected = select(db, each).as_ref()?;
        props.get_or_insert(&selected.styled.properties);
        trees.push((root, &selected.styled.root));
    }
    let props = props?;
    // Each variant's document before Select, in document order: it holds
    // what the variant's stylesheets hide too, so it orders instances that
    // different variants show (#448).
    let mut orders = Vec::new();
    for name in &group.variants {
        let doc = document(db, project, Some(name.v.clone()), build.version(db).clone());
        if let Some(doc) = doc.as_ref() {
            orders.push(document_order(&doc.root));
        }
    }
    let mut merger = Merger {
        props,
        counter_reset: props.builtin("counter-reset"),
        differ: Vec::new(),
        orders,
    };
    let (root, styled) = merger.merge(&trees);
    Some(CommonTree {
        root,
        styled,
        differ: merger.differ,
    })
}

/// Counts the common tree of the numbering group that `build` stands for
/// ([`group_build`]): the values of every counter, of which its variants
/// keep those they share; `None` when a variant of the group cannot be
/// styled, so that each variant counts its own content. Warns at an
/// instance whose `counter-reset` the variants that show it style
/// differently: the common tree counts it with one of their styles.
#[salsa::tracked(returns(ref))]
pub fn common_numbers(db: &dyn Db, build: Build<'_>) -> Option<CommonNumbers> {
    let project = build.project(db);
    let variant = build.variant(db).as_ref()?;
    let group = project.manifest(db).numbering_group(variant.as_str())?;
    let tree = common_tree(db, build).as_ref()?;
    for &span in &tree.differ {
        diag::report(
            db,
            Diagnostic::warning(
                span,
                eco_format!(
                    "the variants of numbering group `{}` restart counters differently here, so \
                     their shared numbers follow one variant's `counter-reset`",
                    group.name.v
                ),
            )
            .with_hint(
                "restart shared counters alike in every variant of the group, outside \
                 `@variant` blocks",
            ),
        );
    }
    let numbers = count_common(db, build, &tree.root, &tree.styled);
    Some(CommonNumbers {
        numbers: numbers
            .into_iter()
            .map(|n| (n.target, n.counter, n.values))
            .collect(),
    })
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
        let each = Build::versioned(
            db,
            project,
            build.target(db),
            Some(name.v.clone()),
            build.version(db).clone(),
        );
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

/// Merges the selected trees of a group's variants.
struct Merger<'a> {
    props: &'a Properties,
    counter_reset: PropId,
    /// The instances whose versions differ in `counter-reset`.
    differ: Vec<Span>,
    /// The position of each instance in each variant's document before
    /// Select, in document order.
    orders: Vec<HashMap<Location, usize>>,
}

impl Merger<'_> {
    /// Whether an instance comes before another in the document: in the
    /// first variant's document before Select that holds both, which
    /// orders the instances of loops and calls as they were made. `false`
    /// when no document holds both, as when each variant makes one by
    /// script.
    fn precedes(&self, a: Location, b: Location) -> bool {
        self.orders
            .iter()
            .find_map(|order| Some(*order.get(&a)? < *order.get(&b)?))
            .unwrap_or(false)
    }

    /// Merges the versions of one instance into one element and styled
    /// node: the fullest version's, the one with the most fields that
    /// are not content (a placeholder has fewer), holding in each field of
    /// content the union of the elements each version holds there. Only
    /// elements are kept: text is not counted.
    fn merge(&mut self, versions: &[(&Element, &StyledNode)]) -> (Element, StyledNode) {
        let plain = |element: &Element| {
            element
                .fields
                .iter()
                .filter(|(_, v)| !matches!(v, Value::Content(_)))
                .count()
        };
        let mut base = 0;
        for (i, (element, _)) in versions.iter().enumerate() {
            if plain(element) > plain(versions[base].0) {
                base = i;
            }
        }
        let (first, first_node) = versions[base];
        let reset = |node: &StyledNode| node.style.value(self.props, self.counter_reset).cloned();
        if versions
            .iter()
            .any(|(_, node)| reset(node) != reset(first_node))
        {
            self.differ.push(first.span);
        }
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
                let (content, nodes) = self.merge_lists(&lists);
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
                let (content, nodes) = self.merge_lists(&lists);
                element.fields.insert(key.clone(), Value::Content(content));
                part_nodes.push(StyledNode {
                    children: nodes.into_iter().collect(),
                    ..part_node.clone()
                });
            }
        }
        // Parts that are not content, such as the style of a formula's
        // vectors, stay as the base version has them.
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

    /// Merges lists of instances by location: the first list's order, with
    /// each instance of a later list that the merged list lacks put after
    /// the instance that list has before it, and after the instances only
    /// earlier lists have there that come before it in a variant's document
    /// before Select. Each instance is merged from
    /// the versions of every list that has it. Linear in the lengths of
    /// the lists.
    fn merge_lists(&mut self, lists: &[Versions<'_>]) -> (Content, Vec<StyledNode>) {
        let mut order: Vec<Location> = Vec::new();
        let mut versions: HashMap<Location, Versions<'_>> = HashMap::new();
        for list in lists {
            let at: HashMap<Location, usize> =
                order.iter().enumerate().map(|(i, l)| (*l, i)).collect();
            let here: HashSet<Location> = list.iter().map(|(e, _)| e.location).collect();
            let mut merged = Vec::with_capacity(order.len() + list.len());
            // The first instance of `order` not yet in `merged`.
            let mut next = 0;
            for &(element, node) in list {
                let location = element.location;
                match at.get(&location) {
                    Some(&i) if i >= next => {
                        merged.extend_from_slice(&order[next..=i]);
                        next = i + 1;
                    }
                    Some(_) => {}
                    None if !versions.contains_key(&location) => {
                        // Instances that only the lists before show, at
                        // the same place, stand before it if they come
                        // before it in the document (#448): ties go by
                        // document order, not by which variant shows them.
                        while let Some(&waiting) = order.get(next)
                            && !here.contains(&waiting)
                            && self.precedes(waiting, location)
                        {
                            merged.push(waiting);
                            next += 1;
                        }
                        merged.push(location);
                    }
                    None => {}
                }
                versions.entry(location).or_default().push((element, node));
            }
            merged.extend_from_slice(&order[next..]);
            order = merged;
        }
        let mut content = Vec::with_capacity(order.len());
        let mut nodes = Vec::with_capacity(order.len());
        for location in order {
            let (element, node) = self.merge(&versions[&location]);
            content.push(Node::Element(element));
            nodes.push(node);
        }
        (content.into_iter().collect(), nodes)
    }
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

/// The position of every element instance of a tree in document order: an
/// instance before the instances in its fields, in their order.
fn document_order(root: &Element) -> HashMap<Location, usize> {
    fn walk(element: &Element, out: &mut HashMap<Location, usize>) {
        let next = out.len();
        out.entry(element.location).or_insert(next);
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                for node in content.nodes() {
                    if let Node::Element(child) = node {
                        walk(child, out);
                    }
                }
            }
        }
    }
    let mut out = HashMap::new();
    walk(root, &mut out);
    out
}
