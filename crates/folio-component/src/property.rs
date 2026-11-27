//! The Properties extension point: stylesheet properties a component adds
//! (SPEC §3.2, §5.2).

use crate::names::Name;
use crate::value::{PropertyType, Value};

/// A stylesheet property a component provides, such as `column-gap`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct PropertyDecl {
    /// The property's name within its component's namespace.
    pub name: Name,
    /// The type of its values.
    pub ty: PropertyType,
    /// The value an element has when no rule sets the property and it does
    /// not inherit one.
    pub initial: Value,
    /// Whether an element without a value takes its parent's (SPEC §5.1).
    pub inherited: bool,
    /// Whether a change to the property can move anything, or only repaints.
    pub effect: Effect,
}

impl PropertyDecl {
    /// A property that does not inherit and affects layout.
    pub fn new(name: Name, ty: PropertyType, initial: Value) -> Self {
        Self {
            name,
            ty,
            initial,
            inherited: false,
            effect: Effect::Layout,
        }
    }

    /// The property, inheriting or not.
    pub fn with_inherited(mut self, inherited: bool) -> Self {
        self.inherited = inherited;
        self
    }

    /// The property, with what a change to its value can change.
    pub fn with_effect(mut self, effect: Effect) -> Self {
        self.effect = effect;
        self
    }
}

/// What a change to a property's value can change (SPEC §3.5, §10.2).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Effect {
    /// Sizes or positions: a change re-runs layout.
    Layout,
    /// Appearance only: a change repaints without re-layout. Only such
    /// properties may be set on component states and animation steps.
    Paint,
}
