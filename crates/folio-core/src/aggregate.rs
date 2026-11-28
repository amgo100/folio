//! Aggregates (SPEC §3.2, §3.5): the values of components' query functions
//! over their own element instances, such as an exam's total marks.
//!
//! An aggregate reads a view of its component's instances
//! ([`folio_component::InstanceTree`]) that holds only the fields it
//! declares it reads ([`folio_component::AggregateDecl::reads`]). The view
//! is made from what the build shows: the numbering group's common tree
//! when the build's variant is in a group (SPEC §6.1), so every variant of
//! the group gets the same value, and the variant's own tree after Select
//! otherwise.
//!
//! Two queries keep the work incremental. [`aggregate`] makes the view for
//! a build and runs again whenever the tree changes; [`run_aggregate`]
//! calls the component, keyed by the view, so the component runs again
//! only when the instances or the fields it reads change.

use std::collections::HashMap;

use folio_component::{InstanceTree, Name, Value};

use crate::db::{Build, Db, Project, component_host};
use crate::lint::view;
use crate::pipeline::selected_root;
use crate::variants::common::{common_tree, group_build};

/// The value of aggregate `name` of the component with namespace
/// `namespace` in a build, or `None` when the component or aggregate is
/// unknown, the document could not be styled, or the component gives no
/// value.
#[salsa::tracked(returns(ref))]
pub fn aggregate(db: &dyn Db, build: Build<'_>, namespace: Name, name: Name) -> Option<Value> {
    let build = build.base(db);
    let project = build.project(db);
    let host = component_host(db, project).as_ref()?;
    let component = host
        .components()
        .find(|c| c.manifest.namespace() == namespace)?;
    let decl = component.aggregate_decl(name.as_str())?;
    let common = group_build(db, build).and_then(|group| common_tree(db, group).as_ref());
    let root = match common {
        Some(tree) => &tree.root,
        None => selected_root(db, build)?,
    };
    let reads = |field: &str| decl.reads.iter().any(|f| f.as_str() == field);
    let tree = view(component, root, &reads, &mut HashMap::new());
    run_aggregate(db, project, namespace, name, tree).clone()
}

/// Runs a component's aggregate over a view: a query of its own, keyed by
/// the view, so an edit that leaves the component's instances and the
/// fields the aggregate reads as they were reuses its value.
#[salsa::tracked(returns(ref))]
pub fn run_aggregate(
    db: &dyn Db,
    project: Project,
    namespace: Name,
    name: Name,
    view: InstanceTree,
) -> Option<Value> {
    let host = component_host(db, project).as_ref()?;
    let component = host
        .components()
        .find(|c| c.manifest.namespace() == namespace)?;
    component.aggregate(name.as_str(), &view)
}
