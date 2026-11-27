//! The instance view: a component's own element instances as plain data,
//! which aggregates and lints read (SPEC §3.2).

use ecow::{EcoString, EcoVec};

use crate::names::{Name, QualifiedName};
use crate::text::Location;
use crate::value::Value;

/// A component's element instances in a build, as the core hands them to
/// its aggregates and lints: a forest of the instances of the component's
/// own elements, in document order (SPEC §3.2).
///
/// It is plain data, made by the core once per build, so that the same
/// interface can cross a WebAssembly boundary (SPEC §3.4). Content of
/// elements that are not the component's is left out: an instance holds
/// the component's instances inside each of its parts, wherever in the
/// part they are.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct InstanceTree {
    /// The instances that no other instance of the component holds, in
    /// document order.
    pub roots: EcoVec<Instance>,
}

impl InstanceTree {
    /// A tree with these top-level instances.
    pub fn new(roots: impl IntoIterator<Item = Instance>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    /// Every instance, in document order: an instance before the instances
    /// in its parts.
    pub fn iter(&self) -> impl Iterator<Item = &Instance> {
        let mut stack: Vec<&Instance> = self.roots.iter().rev().collect();
        std::iter::from_fn(move || {
            let next = stack.pop()?;
            for part in next.parts.iter().rev() {
                stack.extend(part.children.iter().rev());
            }
            Some(next)
        })
    }
}

/// An instance of one of the component's elements.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct Instance {
    /// The element, qualified with the component's namespace.
    pub name: QualifiedName,
    /// Which instance it is (SPEC §1.5): diagnostics point at it.
    pub location: Location,
    /// Its label, if it has one.
    pub label: Option<EcoString>,
    /// Its fields that do not hold content, with the values they have after
    /// defaults are applied, in declaration order.
    pub fields: EcoVec<(Name, Value)>,
    /// Its body, slots and content fields, in the order the element stores
    /// its fields; the body is named [`crate::BODY`].
    pub parts: EcoVec<Part>,
}

impl Instance {
    /// An instance with no label, fields or parts.
    pub fn new(name: QualifiedName, location: Location) -> Self {
        Self {
            name,
            location,
            label: None,
            fields: EcoVec::new(),
            parts: EcoVec::new(),
        }
    }

    /// The value of a field, if it has one.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, v)| v)
    }

    /// A part by name: the body ([`crate::BODY`]), a slot or a content
    /// field.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name.as_str() == name)
    }

    /// The component's instances directly inside this one, in document
    /// order across its parts.
    pub fn children(&self) -> impl Iterator<Item = &Instance> {
        self.parts.iter().flat_map(|p| &p.children)
    }
}

/// The body, a slot or a content field of an instance.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct Part {
    /// Its name: [`crate::BODY`], or the slot's or field's.
    pub name: Name,
    /// Whether it holds no content at all, the component's or other.
    pub empty: bool,
    /// The component's instances inside it, at any depth that no other
    /// instance of the component is between, in document order.
    pub children: EcoVec<Instance>,
}

impl Part {
    /// A part holding these instances; it is empty if they are none.
    pub fn new(name: Name, children: impl IntoIterator<Item = Instance>) -> Self {
        let children: EcoVec<Instance> = children.into_iter().collect();
        Self {
            name,
            empty: children.is_empty(),
            children,
        }
    }

    /// The part, with whether it holds no content at all set: a part with
    /// none of the component's instances may still hold other content.
    pub fn with_empty(mut self, empty: bool) -> Self {
        self.empty = empty;
        self
    }
}
