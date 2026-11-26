//! Locations: the identity of each element instance in a build (SPEC §1.5,
//! §2.3).
//!
//! An element carries the span of the markup or call that made it, and
//! spans are not unique: every iteration of a loop, and every call of a
//! closure, makes elements with the same span. The document model gives
//! every element of the document tree a [`Location`] instead: its span and
//! a number that tells apart the instances made from that span, counted in
//! the order the model builds the tree, as Typst's locator
//! disambiguates repeated spans. An instance keeps its location across an
//! edit that leaves its span and the instances made before it from the same
//! span alone: inserting a paragraph elsewhere changes no other location.
//!
//! The styled tree, the text stage's blocks and paragraphs, and the boxes of
//! layout carry the location of the element they come from. Stages that
//! read the document tree beside a later tree pair the two by location
//! ([`StyledChildren`]), not by position, so a stage that drops nodes
//! (Select) or adds them (Resolve's generated text) does not shift the
//! styles of the nodes after them.
//!
//! Locations are built from span numbers, which a reparse may change, so
//! they are for comparing within one revision and across edits, never for
//! ordering or for output (SPEC §2.4). Node identity across reparses, which
//! the editor keeps, is another thing: [`folio_syntax::NodeIds`].

use std::collections::HashMap;

use folio_component::Name;
use folio_syntax::Span;

use crate::eval::Element;
use crate::span_map::source_ref;
use crate::style::{NodeName, StyledNode};

pub use folio_component::Location;

/// Gives the element instances of one document tree their locations.
#[derive(Debug, Default)]
pub(crate) struct Locator {
    /// How many instances each span has made so far.
    made: HashMap<Span, u32>,
}

impl Locator {
    /// The location of the next instance made from `span`.
    pub(crate) fn locate(&mut self, span: Span) -> Location {
        let count = self.made.entry(span).or_insert(0);
        let instance = *count;
        *count = count.saturating_add(1);
        Location {
            source: source_ref(span),
            instance,
        }
    }
}

/// The styled nodes of a node's children, looked up by the elements and
/// parts of the document tree they style.
///
/// Elements are found by location and parts by name. A child that a later
/// stage removed has no node, and a node that a later stage added, such as
/// generated text, styles no element of the document tree, so neither
/// shifts the others. Lookups in the order of the children cost one step
/// each.
#[derive(Debug, Clone)]
pub struct StyledChildren<'a> {
    nodes: &'a [StyledNode],
    /// Where the next lookup starts.
    at: usize,
}

impl<'a> StyledChildren<'a> {
    /// The children of `node`.
    pub fn new(node: &'a StyledNode) -> Self {
        Self {
            nodes: &node.children,
            at: 0,
        }
    }

    /// The styled node of a child element, or `None` if it has none.
    pub fn element(&mut self, element: &Element) -> Option<&'a StyledNode> {
        let i = self.find(|node| {
            node.location == element.location && matches!(node.name, NodeName::Element(_))
        })?;
        self.at = i + 1;
        Some(&self.nodes[i])
    }

    /// The styled node of the part `name` (a content field other than the
    /// body), or `None` if it has none.
    pub fn part(&mut self, name: &Name) -> Option<&'a StyledNode> {
        let i = self.find(|node| matches!(&node.name, NodeName::Part(n) if n == name))?;
        self.at = i + 1;
        Some(&self.nodes[i])
    }

    /// The first node from the cursor on that passes `test`, or before the
    /// cursor if none after it does.
    fn find(&self, test: impl Fn(&StyledNode) -> bool) -> Option<usize> {
        let start = self.at.min(self.nodes.len());
        let (before, after) = self.nodes.split_at(start);
        after
            .iter()
            .position(&test)
            .map(|i| start + i)
            .or_else(|| before.iter().position(&test))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    #[test]
    fn repeated_spans_get_distinct_instances() {
        let span = |n: u64| Span::from_raw(NonZeroU64::new((1 << 48) | n).unwrap());
        let (a, b) = (span(2), span(3));
        let mut locator = Locator::default();
        let first = locator.locate(a);
        let other = locator.locate(b);
        let second = locator.locate(a);
        assert_eq!(first.source, second.source);
        assert_eq!((first.instance, second.instance), (0, 1));
        assert_eq!(other.instance, 0);
        assert_ne!(first, second);
    }
}
