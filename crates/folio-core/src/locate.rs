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
/// calls that made it, from the outermost inward (SPEC §2.3, §6.1).
/// Content made outside any loop or call has the root origin.
///
/// An origin has two parts. Its key names the iterations by their loops
/// and indices and the calls by their sites: it is what a location is
/// built from, so that iteration `i` of a loop pairs across the variants
/// of a numbering group. Its items fingerprint what those iterations
/// iterated over: two instances with one key whose items differ were made
/// from different items, so they cannot share numbering
/// ([`crate::variants`]).
///
/// Evaluation stamps the [`Frame`] of an iteration or a call on the
/// elements of the value it returns (`Content::stamp`), and the document
/// model gives an element inside another's content the origin of the
/// element around it as well ([`Origin::nested`]). Both parts are hashes,
/// mixed by a fixed function so that they are the same on every platform
/// and in every process; like the spans they are made from, they are
/// compared, never written to output.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Origin {
    key: u64,
    items: u64,
}

impl Origin {
    /// The origin of content made outside any loop or call.
    pub const ROOT: Self = Self { key: 0, items: 0 };

    /// The origin of an instance with this origin that `frame` returns:
    /// the frame comes before the frames already recorded.
    #[must_use]
    pub fn within(self, frame: Frame) -> Self {
        Self {
            key: pair(frame.key(), self.key).max(1),
            items: pair(frame.item(), self.items),
        }
    }

    /// The origin of an element whose own origin is `own`, inside the
    /// content of an element whose origin is `self`.
    #[must_use]
    pub fn nested(self, own: Self) -> Self {
        if own == Self::ROOT {
            self
        } else if self == Self::ROOT {
            own
        } else {
            Self {
                key: pair(self.key, own.key).max(1),
                items: pair(self.items, own.items),
            }
        }
    }

    /// The fingerprint of the items its iterations iterated over.
    pub fn items(self) -> u64 {
        self.items
    }
}

/// One step of an instance's [`Origin`]: an iteration of a loop or a call
/// of a closure.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Frame {
    /// An iteration of a loop, counted from 0: of a `for` or `while` loop,
    /// at its span, or of a function that calls a closure for each item,
    /// at the call.
    Iteration {
        /// The loop or the call.
        span: Span,
        /// Which iteration.
        index: u64,
        /// A hash of the item it iterates over ([`item_key`]), or its index
        /// for a loop over no items.
        item: u64,
    },
    /// A call of a closure, at its call site.
    Call {
        /// The call site.
        span: Span,
    },
}

impl Frame {
    /// The frame's key: its loop or call site, and its index.
    fn key(self) -> u64 {
        match self {
            Self::Iteration { span, index, .. } => pair(pair(1, span.into_raw().get()), index),
            Self::Call { span } => pair(2, span.into_raw().get()),
        }
    }

    /// The item it iterates over, or `0` for a call.
    fn item(self) -> u64 {
        match self {
            Self::Iteration { item, .. } => item,
            Self::Call { .. } => 0,
        }
    }
}

/// A hash of an item a loop iterates over, for [`Frame::Iteration`]. It
/// is compared within one process, never written out.
pub(crate) fn item_key(item: &impl std::hash::Hash) -> u64 {
    use std::hash::{DefaultHasher, Hasher};
    let mut hasher = DefaultHasher::new();
    item.hash(&mut hasher);
    hasher.finish()
}

/// Mixes a word into a key: SplitMix64's finaliser of the two, a
/// bijection of the word for a given state, so different words give
/// different keys for one state.
fn mix(state: u64, word: u64) -> u64 {
    let mut z = (state ^ word).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A key of an ordered pair: the first word mixed before the second, so
/// that a pair of equal words, or the same words in the other order, does
/// not cancel out as in a plain [`mix`] of the two.
fn pair(first: u64, second: u64) -> u64 {
    mix(mix(0x243f_6a88_85a3_08d3, first), second)
}

/// Gives the element instances of one document tree their locations.
#[derive(Debug, Default)]
pub(crate) struct Locator {
    /// How many instances each span has made so far from each origin, by
    /// what made them. It is never iterated, so its order does not matter.
    made: BTreeMap<(u64, Made, u64), u64>,
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
            .entry((span.into_raw().get(), made, origin.key))
            .or_insert(first);
        let n = *count;
        *count = n.saturating_add(1).min(Self::COUNT);
        let number = if origin.key == 0 {
            n
        } else {
            Self::KEYED | (mix(origin.key, n) & Self::COUNT)
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
        let call = |n: u64| Frame::Call {
            span: Span::from_raw(NonZeroU64::new((1 << 48) | n).unwrap()),
        };
        let (x, y) = (Origin::ROOT.within(call(7)), Origin::ROOT.within(call(8)));
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

    #[test]
    fn frames_compose() {
        let span = |n: u64| Span::from_raw(NonZeroU64::new((1 << 48) | n).unwrap());
        let iteration = |index| Frame::Iteration {
            span: span(4),
            index,
            item: index,
        };
        let root = Origin::ROOT;
        assert_ne!(root.within(iteration(0)), root);
        assert_ne!(root.within(iteration(0)), root.within(iteration(1)));
        // The order of frames counts.
        let call = Frame::Call { span: span(5) };
        assert_ne!(
            root.within(iteration(0)).within(call),
            root.within(call).within(iteration(0))
        );
        // Nesting under the root, or the root under anything, changes
        // nothing.
        let x = root.within(call);
        let y = root.within(iteration(0));
        assert_eq!(root.nested(x), x);
        assert_eq!(x.nested(root), x);
        // An origin nested in itself keeps what tells it apart.
        assert_ne!(x.nested(x), x);
        assert_ne!(x.nested(x), y.nested(y));
        assert_ne!(x.nested(y), y.nested(x));
    }
}
