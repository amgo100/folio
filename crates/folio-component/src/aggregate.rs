//! Aggregates (SPEC §3.2): query functions over a component's own element
//! instances, such as an exam's total marks.

use ecow::EcoVec;

use crate::names::Name;

/// An aggregate a component declares: its name and what it reads.
///
/// The core calls [`crate::Component::aggregate`] with the aggregate's
/// name over a view of the component's instances. The view holds only
/// the fields the aggregate declares it reads, so an edit to another field
/// does not run it again (SPEC §3.5). In a numbering group the view is
/// made from the group's common tree, so every variant of the group gets
/// the same value (SPEC §6.1, §6.4).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct AggregateDecl {
    /// The aggregate's name within the component's namespace:
    /// `total-marks`.
    pub name: Name,
    /// The fields of the component's elements it reads; the view it
    /// receives holds no others.
    pub reads: EcoVec<Name>,
}

impl AggregateDecl {
    /// An aggregate that reads no fields; set [`AggregateDecl::reads`] for
    /// those it does.
    pub fn new(name: Name) -> Self {
        Self {
            name,
            reads: EcoVec::new(),
        }
    }

    /// The aggregate, reading these fields.
    pub fn reading(mut self, fields: impl IntoIterator<Item = Name>) -> Self {
        self.reads = fields.into_iter().collect();
        self
    }
}

/// The fields a component's lints read (SPEC §3.5): the view the core
/// hands [`crate::Component::lint`] holds only these, so an edit to
/// another field does not run the lints again. `All`, the default, hands
/// them every field.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum LintReads {
    /// Every field.
    #[default]
    All,
    /// These fields only.
    Fields(EcoVec<Name>),
}

impl LintReads {
    /// Whether the lints read a field.
    pub fn reads(&self, field: &str) -> bool {
        match self {
            Self::All => true,
            Self::Fields(fields) => fields.iter().any(|f| f.as_str() == field),
        }
    }
}
