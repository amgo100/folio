//! Locations: the identity of each element instance in a build (SPEC §1.5,
//! §2.3).
//!
//! An element carries the span of the markup or call that made it, and
//! spans are not unique: every iteration of a loop, and every call of a
//! closure, makes elements with the same span. The document model gives
//! every element of the document tree a [`Location`] instead: its span and
//! a key of its [`Origin`], the loop iterations and closure calls that made
//! it (SPEC §2.3, §6.1). Instances with the same span and origin, such as
//! content inserted twice, are told apart by a number counted in document
//! order. An instance keeps its location across an edit that leaves its
//! span and origin alone: inserting a paragraph or a call elsewhere changes
//! no other location, and the instance that one iteration of a loop makes
//! has the same location in every variant that runs the iteration.
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

use std::collections::{BTreeMap, HashMap};

use folio_component::Name;
use folio_syntax::Span;

use crate::eval::Element;
use crate::span_map::source_ref;
use crate::style::{NodeName, StyledNode};

pub use folio_component::Location;

/// What made an element: evaluation, or the document model, which makes
/// paragraphs, lists and sections with the span of their first node or
/// heading. Each is counted apart, so a paragraph's location does not
/// depend on the elements that share its span, nor a section's on how many
/// headings its body holds.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Made {
    /// An element of evaluated content.
    Evaluated = 0,
    /// A paragraph of the document model.
    Paragraph = 1,
    /// A list of the document model.
    List = 2,
    /// A section of the document model.
    Section = 3,
}

/// The origin of an element instance: the loop iterations and closure
/// calls that made it, from the outermost inward, as a key (SPEC §2.3,
/// §6.1). Content made outside any loop or call has the root origin.
///
/// An origin is a hash, mixed by a fixed function so that it is the same on
/// every platform and in every process; like the spans it is made from, it
/// is compared, never written to output.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct Origin(u64);

impl Origin {
    /// The origin of content made outside any loop or call.
    pub(crate) const ROOT: Self = Self(0);
}

/// Mixes a word into a key: a step of SplitMix64's finaliser, a bijection
/// of the word, so different words give different keys for one state.
fn mix(state: u64, word: u64) -> u64 {
    let mut z = (state ^ word).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Gives the element instances of one document tree their locations.
#[derive(Debug, Default)]
pub(crate) struct Locator {
    /// How many instances each span has made so far from each origin, by
    /// what made them. It is never iterated, so its order does not matter.
    made: BTreeMap<(u64, Made, Origin), u64>,
}

impl Locator {
    /// The most an instance number of the root origin counts to: below the
    /// bit that marks a key of another origin, and far beyond evaluation's
    /// step and memory limits.
    const COUNT: u64 = (1 << 60) - 1;

    /// The bit that marks the key of an origin other than the root, so that
    /// no such key equals a number counted from the root.
    const KEYED: u64 = 1 << 61;

    /// The location of the next instance that `made` makes from `span`, at
    /// `origin`.
    ///
    /// The instance is what made it in its top two bits and, below them,
    /// the number of instances that the span made before it from the same
    /// origin: as it is for the root origin, and mixed with the origin's
    /// key under [`Self::KEYED`] for any other. Elements without a span
    /// are counted from `1`, so that none of them is
    /// [`Location::DETACHED`]; no instance is
    /// [`Location::UNKNOWN_INSTANCE`], whose bits below [`Self::KEYED`]
    /// are all set.
    pub(crate) fn locate(&mut self, span: Span, made: Made, origin: Origin) -> Location {
        let first = u64::from(span.is_detached());
        let count = self
            .made
            .entry((span.into_raw().get(), made, origin))
            .or_insert(first);
        let n = *count;
        *count = n.saturating_add(1).min(Self::COUNT);
        let number = if origin == Origin::ROOT {
            n
        } else {
            Self::KEYED | (mix(origin.0, n) & Self::COUNT)
        };
        Location {
            source: source_ref(span),
            instance: ((made as u64) << 62) | number,
        }
    }
}

/// The styled nodes of a node's children, looked up by the elements and
/// parts of the document tree they style.
///
/// Elements are found by location and parts by name. A child that a later
/// stage removed has no node, and a node that a later stage added, such as
/// generated text, styles no element of the document tree, so neither
/// shifts the others. A lookup costs one step when the children are looked
/// up in order and a later stage changed nothing between them, and a hash
/// lookup otherwise.
#[derive(Debug, Clone)]
pub struct StyledChildren<'a> {
    nodes: &'a [StyledNode],
    /// Where the next lookup starts.
    at: usize,
    /// The index of each element's node, made at the first lookup that
    /// does not find its node next. Locations are unique in the document
    /// tree, so each names one node.
    index: Option<HashMap<Location, usize>>,
}

impl<'a> StyledChildren<'a> {
    /// The children of `node`.
    pub fn new(node: &'a StyledNode) -> Self {
        Self {
            nodes: &node.children,
            at: 0,
            index: None,
        }
    }

    /// The styled node of a child element, or `None` if it has none.
    pub fn element(&mut self, element: &Element) -> Option<&'a StyledNode> {
        // Parts may come between the elements of the body.
        while self
            .nodes
            .get(self.at)
            .is_some_and(|node| matches!(node.name, NodeName::Part(_)))
        {
            self.at += 1;
        }
        let next = self.nodes.get(self.at).is_some_and(|node| {
            node.location == element.location && matches!(node.name, NodeName::Element(_))
        });
        let i = if next {
            self.at
        } else {
            let nodes = self.nodes;
            let index = self.index.get_or_insert_with(|| {
                let mut index = HashMap::new();
                for (i, node) in nodes.iter().enumerate() {
                    if matches!(node.name, NodeName::Element(_)) {
                        index.entry(node.location).or_insert(i);
                    }
                }
                index
            });
            *index.get(&element.location)?
        };
        self.at = i + 1;
        Some(&self.nodes[i])
    }

    /// The styled node of the part `name` (a content field other than the
    /// body), or `None` if it has none.
    pub fn part(&mut self, name: &Name) -> Option<&'a StyledNode> {
        let is = |node: &StyledNode| matches!(&node.name, NodeName::Part(n) if n == name);
        // An element has few parts, so they are searched for.
        let start = self.at.min(self.nodes.len());
        let (before, after) = self.nodes.split_at(start);
        let i = after
            .iter()
            .position(is)
            .map(|i| start + i)
            .or_else(|| before.iter().position(is))?;
        self.at = i + 1;
        Some(&self.nodes[i])
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
        let root = Origin::ROOT;
        let mut locator = Locator::default();
        let first = locator.locate(a, Made::Evaluated, root);
        let other = locator.locate(b, Made::Evaluated, root);
        let paragraph = locator.locate(a, Made::Paragraph, root);
        let second = locator.locate(a, Made::Evaluated, root);
        assert_eq!(first.source, second.source);
        assert_eq!((first.instance, second.instance), (0, 1));
        assert_eq!(other.instance, 0);
        assert_ne!(first, second);
        // What the model makes is counted apart.
        assert_eq!(paragraph.instance, 1 << 62);
        assert_ne!(paragraph, first);
        // No element is detached.
        let detached = locator.locate(Span::detached(), Made::Evaluated, root);
        assert_ne!(detached, Location::DETACHED);
    }

    #[test]
    fn origins_are_counted_apart() {
        let span = Span::from_raw(NonZeroU64::new((1 << 48) | 2).unwrap());
        let (x, y) = (Origin(mix(0, 1)), Origin(mix(0, 2)));
        // Two orders of the same instances: an origin's instances do not
        // depend on another origin's.
        let mut one = Locator::default();
        let a = one.locate(span, Made::Evaluated, x);
        let b = one.locate(span, Made::Evaluated, y);
        let mut two = Locator::default();
        let root = two.locate(span, Made::Evaluated, Origin::ROOT);
        let b2 = two.locate(span, Made::Evaluated, y);
        let a2 = two.locate(span, Made::Evaluated, x);
        assert_eq!((a, b), (a2, b2));
        assert_ne!(a, b);
        assert_eq!(root.instance, 0);
        for location in [a, b] {
            assert_ne!(location.instance & Locator::KEYED, 0);
            assert_ne!(location.instance, Location::UNKNOWN_INSTANCE);
        }
        // A second instance of one origin is told apart by its number.
        let again = one.locate(span, Made::Evaluated, x);
        assert_ne!(again, a);
    }
}
