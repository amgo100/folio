//! The document model (SPEC §1.5, §4.2, §4.3): the built-in elements and
//! their schemas, and the typed document tree that the later stages read.
//!
//! Evaluation makes element instances and checks each against its schema
//! as it makes it, so fields have their declared types and defaults, and
//! bodies fit their content models. The [`document`] query then gives the
//! evaluated content its structure: the `doc` element at the root, inline
//! content grouped into paragraphs, list items into lists, and the blocks
//! after each heading into nested sections.
//!
//! This is the stage that becomes `folio-model` (SPEC §2.2) once its
//! interface settles.

mod builtins;
mod tree;

use folio_component::{ElementSchema, QualifiedName};

pub use self::builtins::{Builtin, builtin, builtins, is_block};
pub use self::tree::{Document, document};

use crate::db::{Db, Project, component_host};

/// The schema of an element: a built-in element for an unqualified name, a
/// component's element for a qualified one. Calls of component elements are
/// always qualified once evaluated, whether or not the source qualified
/// them, because the global scope resolves unqualified names (SPEC §3.6).
pub fn schema<'a>(
    db: &'a dyn Db,
    project: Project,
    name: &QualifiedName,
) -> Option<&'a ElementSchema> {
    match &name.namespace {
        None => builtin(name.name.as_str()).map(|b| &b.schema),
        Some(_) => component_host(db, project)
            .as_ref()?
            .element(name)
            .ok()
            .map(|resolved| resolved.item),
    }
}
