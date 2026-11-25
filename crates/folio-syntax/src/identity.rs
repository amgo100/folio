//! Node identity across reparses (SPEC §1.4, §12.4).
//!
//! A span names a node in one revision of a file; an edit may renumber the
//! nodes it reparses, and replacing a file's text renumbers whatever the
//! new text reparses. A [`NodeId`] names a node for as long as the node
//! lives: [`NodeIds`] carries it from one revision to the next through a
//! diff of the two syntax trees, so a selection, the inspector or a dynamic
//! element's runtime state stays with its node while the text around it is
//! edited.
//!
//! Identity belongs to a session, not to a document: ids depend on the
//! order of edits, so they are used for selections, the inspector and
//! runtime state only, never for undo, saved data or anything a compile
//! reads (output must not depend on edit history, SPEC §2.4).
//!
//! **The diff.** The roots of the two trees are one node. For two nodes
//! that are one node, the new node takes the old one's id, and their
//! children are matched: first the children that an edit left alone or
//! that are equal apart from spans, from the start and from the end; then
//! the rest by the longest common subsequence of their kinds, preferring
//! equal nodes. Matched children are one node, and their children are
//! matched in turn. An old node without a match has gone; a new node
//! without one is new and gets a fresh id. A subtree that the edit left
//! alone ([`SyntaxNode::is_same`]) keeps its spans and its ids without
//! being visited, so an edit costs time in proportion to what it reparsed,
//! not to the file.

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroU64;

use crate::{FileId, Span, SyntaxNode};

/// The identity of a source node while it lives, across edits and
/// reparses. Unique among the nodes of all files of a [`NodeIds`].
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct NodeId(NonZeroU64);

impl NodeId {
    /// The id as a number, for a host that passes it on, as Folio Studio
    /// does to its interface.
    pub fn into_raw(self) -> NonZeroU64 {
        self.0
    }

    /// The id of a number from [`Self::into_raw`].
    pub fn from_raw(raw: NonZeroU64) -> Self {
        Self(raw)
    }
}

impl Debug for NodeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

/// The identities of the nodes of a set of files: a [`NodeId`] for every
/// node of each file's current syntax tree.
#[derive(Clone, Default)]
pub struct NodeIds {
    by_span: HashMap<Span, NodeId>,
    by_id: HashMap<NodeId, Span>,
    /// The last id given.
    last: u64,
}

impl NodeIds {
    /// No files.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file with its tree, giving every node a fresh id. A file
    /// that is there already has its ids replaced.
    pub fn insert(&mut self, root: &SyntaxNode) {
        if let Some(id) = root.span().id() {
            self.remove(id);
        }
        let mut added = Vec::new();
        added_all(root, &mut added);
        for span in added {
            let id = self.fresh();
            self.bind(span, id);
        }
    }

    /// Carries the ids of a file's nodes from its tree `old` to its tree
    /// `new`, after an edit or a replacement of its text. Nodes that the
    /// diff matches keep their ids; new ones get fresh ids. `old` must be
    /// the tree the ids are of.
    pub fn update(&mut self, old: &SyntaxNode, new: &SyntaxNode) {
        let mut diff = Diff::default();
        diff.pair(old, new);
        // Every span that goes is unbound before any that comes is bound,
        // since a reparse may give a new node the number of one it removed.
        let carried: Vec<(Span, Option<NodeId>)> = diff
            .added
            .iter()
            .map(|&(span, from)| (span, from.and_then(|from| self.by_span.get(&from).copied())))
            .collect();
        for span in diff.removed {
            if let Some(id) = self.by_span.remove(&span) {
                self.by_id.remove(&id);
            }
        }
        for (span, id) in carried {
            let id = id.unwrap_or_else(|| self.fresh());
            self.bind(span, id);
        }
    }

    /// Removes a file's ids.
    pub fn remove(&mut self, file: FileId) {
        self.by_span.retain(|span, _| span.id() != Some(file));
        let by_span = &self.by_span;
        self.by_id.retain(|_, span| by_span.contains_key(span));
    }

    /// The id of the node with a span in a current tree.
    pub fn id(&self, span: Span) -> Option<NodeId> {
        self.by_span.get(&span).copied()
    }

    /// The span of a node in its file's current tree, if it still lives.
    pub fn span(&self, id: NodeId) -> Option<Span> {
        self.by_id.get(&id).copied()
    }

    /// How many nodes have ids.
    pub fn len(&self) -> usize {
        self.by_span.len()
    }

    /// Whether no node has an id.
    pub fn is_empty(&self) -> bool {
        self.by_span.is_empty()
    }

    fn fresh(&mut self) -> NodeId {
        self.last += 1;
        NodeId(NonZeroU64::new(self.last).expect("ids count up from one"))
    }

    fn bind(&mut self, span: Span, id: NodeId) {
        if span.is_detached() {
            return;
        }
        self.by_span.insert(span, id);
        self.by_id.insert(id, span);
    }
}

impl Debug for NodeIds {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIds")
            .field("nodes", &self.by_span.len())
            .finish()
    }
}

/// The spans of every node of a subtree.
fn added_all(node: &SyntaxNode, out: &mut Vec<Span>) {
    out.push(node.span());
    for child in node.children() {
        added_all(child, out);
    }
}

/// The changes a diff finds.
#[derive(Default)]
struct Diff {
    /// Spans of old nodes that have gone or been renumbered.
    removed: Vec<Span>,
    /// Spans of new nodes, each with the span of the old node it is, if
    /// any.
    added: Vec<(Span, Option<Span>)>,
}

/// Above this many pairs of unmatched children, the middle of two child
/// lists is matched in order instead of by their longest common
/// subsequence.
const LCS_LIMIT: usize = 1 << 14;

impl Diff {
    /// Records that `old` and `new`, of one kind, are one node, and matches
    /// their children.
    fn pair(&mut self, old: &SyntaxNode, new: &SyntaxNode) {
        if old.is_same(new) {
            return;
        }
        self.removed.push(old.span());
        self.added.push((new.span(), Some(old.span())));

        let a: Vec<&SyntaxNode> = old.children().collect();
        let b: Vec<&SyntaxNode> = new.children().collect();
        let equal = |x: &SyntaxNode, y: &SyntaxNode| x.is_same(y) || x.spanless_eq(y);
        let mut start = 0;
        while start < a.len() && start < b.len() && equal(a[start], b[start]) {
            self.pair(a[start], b[start]);
            start += 1;
        }
        let (mut end_a, mut end_b) = (a.len(), b.len());
        while end_a > start && end_b > start && equal(a[end_a - 1], b[end_b - 1]) {
            self.pair(a[end_a - 1], b[end_b - 1]);
            end_a -= 1;
            end_b -= 1;
        }
        self.middle(&a[start..end_a], &b[start..end_b]);
    }

    /// Matches the children between the matched ends of two child lists.
    fn middle(&mut self, a: &[&SyntaxNode], b: &[&SyntaxNode]) {
        let pairs = if a.len().saturating_mul(b.len()) <= LCS_LIMIT {
            lcs(a, b)
        } else {
            in_order(a, b)
        };
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in pairs.into_iter().chain([(a.len(), b.len())]) {
            for old in &a[i..pi] {
                self.gone(old);
            }
            for new in &b[j..pj] {
                self.born(new);
            }
            if pi < a.len() && pj < b.len() {
                self.pair(a[pi], b[pj]);
            }
            (i, j) = (pi + 1, pj + 1);
        }
    }

    fn gone(&mut self, old: &SyntaxNode) {
        added_all(old, &mut self.removed);
    }

    fn born(&mut self, new: &SyntaxNode) {
        self.added.push((new.span(), None));
        for child in new.children() {
            self.born(child);
        }
    }
}

/// The pairs of the longest common subsequence of two child lists by kind,
/// where a pair of equal nodes counts more than a pair of one kind.
fn lcs(a: &[&SyntaxNode], b: &[&SyntaxNode]) -> Vec<(usize, usize)> {
    let score = |x: &SyntaxNode, y: &SyntaxNode| {
        if x.kind() != y.kind() {
            0
        } else if x.spanless_eq(y) {
            2
        } else {
            1
        }
    };
    let (n, m) = (a.len(), b.len());
    // best[i][j]: the best score of a[i..] and b[j..].
    let mut best = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let s = score(a[i], b[j]);
            let take = if s > 0 { best[at(i + 1, j + 1)] + s } else { 0 };
            best[at(i, j)] = take.max(best[at(i + 1, j)]).max(best[at(i, j + 1)]);
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        let s = score(a[i], b[j]);
        if s > 0 && best[at(i, j)] == best[at(i + 1, j + 1)] + s {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if best[at(i + 1, j)] >= best[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Pairs of two child lists matched in order by kind, dropping from the
/// longer list where kinds differ.
fn in_order(a: &[&SyntaxNode], b: &[&SyntaxNode]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].kind() == b[j].kind() {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if a.len() - i >= b.len() - j {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;
    use crate::{LinkedNode, Source, SyntaxKind};

    fn source(text: &str) -> Source {
        Source::new(FileId::from_raw(NonZeroU16::new(3).unwrap()), text.into())
    }

    fn ids(text: &str) -> (Source, NodeIds) {
        let source = source(text);
        let mut ids = NodeIds::new();
        ids.insert(source.root());
        (source, ids)
    }

    fn edit(source: &mut Source, ids: &mut NodeIds, range: std::ops::Range<usize>, with: &str) {
        let old = source.clone();
        source.edit(range, with);
        ids.update(old.root(), source.root());
        check(source, ids);
    }

    fn replace(source: &mut Source, ids: &mut NodeIds, text: &str) {
        let old = source.clone();
        source.replace(text);
        ids.update(old.root(), source.root());
        check(source, ids);
    }

    /// Every node of the tree has an id, no other node has one, and no two
    /// share one.
    fn check(source: &Source, ids: &NodeIds) {
        let mut spans = Vec::new();
        added_all(source.root(), &mut spans);
        assert_eq!(ids.len(), spans.len());
        let mut seen = std::collections::HashSet::new();
        for span in spans {
            let id = ids.id(span).unwrap();
            assert!(seen.insert(id));
            assert_eq!(ids.span(id), Some(span));
        }
    }

    /// The id of the node that starts at the first occurrence of `text`,
    /// of a kind.
    fn id_at(source: &Source, ids: &NodeIds, text: &str, kind: SyntaxKind) -> NodeId {
        let at = source.text().find(text).unwrap();
        fn walk(node: &LinkedNode<'_>, at: usize, kind: SyntaxKind) -> Option<Span> {
            if node.kind() == kind && node.offset() == at {
                return Some(node.span());
            }
            node.children()
                .filter(|c| c.range().start <= at && at <= c.range().end)
                .find_map(|c| walk(&c, at, kind))
        }
        ids.id(walk(&LinkedNode::new(source.root()), at, kind).unwrap())
            .unwrap()
    }

    const TEXT: &str =
        "= Forces\n\nA ball is *thrown* up.\n\n#countdown(duration: 5)\n\nThe end.\n";

    #[test]
    fn every_node_has_an_id() {
        let (source, ids) = ids(TEXT);
        check(&source, &ids);
    }

    #[test]
    fn ids_survive_edits_elsewhere() {
        let (mut source, mut ids) = ids(TEXT);
        let heading = id_at(&source, &ids, "= Forces", SyntaxKind::Heading);
        let strong = id_at(&source, &ids, "*thrown*", SyntaxKind::Strong);
        let call = id_at(&source, &ids, "countdown(", SyntaxKind::FuncCall);
        let end = id_at(&source, &ids, "The end", SyntaxKind::Text);

        // Typing in the paragraph.
        let at = source.text().find("ball").unwrap();
        edit(&mut source, &mut ids, at..at + 4, "stone");
        // A new paragraph before the call.
        let at = source.text().find("#countdown").unwrap();
        edit(&mut source, &mut ids, at..at, "Another paragraph.\n\n");
        // Text replaced wholesale, as from a file watcher, which changes
        // the heading's text.
        let text = source.text().replace("Forces", "Forces and motion");
        replace(&mut source, &mut ids, &text);

        assert_eq!(
            id_at(&source, &ids, "= Forces", SyntaxKind::Heading),
            heading
        );
        assert_eq!(id_at(&source, &ids, "*thrown*", SyntaxKind::Strong), strong);
        assert_eq!(
            id_at(&source, &ids, "countdown(", SyntaxKind::FuncCall),
            call
        );
        assert_eq!(id_at(&source, &ids, "The end", SyntaxKind::Text), end);
    }

    #[test]
    fn renumbered_nodes_keep_their_ids() {
        // An edit in a content block reparses the block, and numbers its
        // nodes anew.
        let (mut source, mut ids) = ids("#figure[A *b* c]\n\nAfter.\n");
        let span = |source: &Source| {
            let at = source.text().find('*').unwrap();
            LinkedNode::new(source.root())
                .leaf_at(at + 1, crate::Side::After)
                .and_then(|leaf| leaf.parent().map(|p| p.span()))
                .unwrap()
        };
        let before = span(&source);
        let strong = ids.id(before).unwrap();
        let after = id_at(&source, &ids, "After", SyntaxKind::Text);
        edit(&mut source, &mut ids, 8..9, "An");
        assert_ne!(span(&source), before);
        assert_eq!(ids.id(span(&source)), Some(strong));
        assert_eq!(id_at(&source, &ids, "After", SyntaxKind::Text), after);
    }

    #[test]
    fn an_edited_node_keeps_its_id() {
        let (mut source, mut ids) = ids(TEXT);
        let call = id_at(&source, &ids, "countdown(", SyntaxKind::FuncCall);
        let at = source.text().find('5').unwrap();
        edit(&mut source, &mut ids, at..at + 1, "10");
        assert_eq!(
            id_at(&source, &ids, "countdown(", SyntaxKind::FuncCall),
            call
        );
    }

    #[test]
    fn removed_nodes_lose_their_ids_and_new_ones_get_fresh_ids() {
        let (mut source, mut ids) = ids(TEXT);
        let strong = id_at(&source, &ids, "*thrown*", SyntaxKind::Strong);
        let before = ids.clone();
        let at = source.text().find("*thrown*").unwrap();
        edit(&mut source, &mut ids, at..at + 8, "thrown");
        assert_eq!(ids.span(strong), None);
        let at = source.text().find("The end").unwrap();
        edit(&mut source, &mut ids, at..at, "_New_ ");
        let emph = id_at(&source, &ids, "_New_", SyntaxKind::Emph);
        assert_eq!(before.span(emph), None);
        assert_ne!(emph, strong);
    }

    #[test]
    fn removing_a_file_removes_its_ids() {
        let (source, mut ids) = ids(TEXT);
        let other = Source::new(FileId::from_raw(NonZeroU16::new(4).unwrap()), "Hi".into());
        ids.insert(other.root());
        ids.remove(source.id());
        let mut spans = Vec::new();
        added_all(other.root(), &mut spans);
        assert_eq!(ids.len(), spans.len());
        assert!(ids.id(source.root().span()).is_none());
    }
}
