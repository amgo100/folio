// Derived from typst-syntax v0.15.1, crates/typst-syntax/src/reparser.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

use std::ops::Range;

use crate::{
    Span, SyntaxKind, SyntaxNode, is_newline, parse, parse_stylesheet, reparse_block,
    reparse_markup, reparse_style_block, reparse_stylesheet_items,
};

/// Refresh the given syntax node with as little parsing as possible.
///
/// Takes the new text, the range in the old text that was replaced and the
/// length of the replacement and returns the range in the new text that was
/// ultimately reparsed.
///
/// The high-level API for this function is
/// [`Source::edit`](crate::Source::edit).
pub(crate) fn reparse(
    root: &mut SyntaxNode,
    text: &str,
    replaced: Range<usize>,
    replacement_len: usize,
) -> Range<usize> {
    let style = root.kind() == SyntaxKind::Stylesheet;
    // The guard (#334): an incremental reparse is kept only when neither the
    // old tree nor the new one has a syntax error, and the reparsed range is
    // not deeply nested. See `reusable`.
    let failure = if root.diagnosis().errors {
        Failure::NotHere
    } else {
        match try_reparse(text, replaced, replacement_len, None, root, 0, style) {
            Ok(range) if reusable(root, &range) => return range,
            // The tree is whole and numbered: the full parse is spliced into
            // it as into the old tree.
            Ok(_) => Failure::NotHere,
            Err(failure) => failure,
        }
    };
    let mut new = if style {
        parse_stylesheet(text)
    } else {
        parse(text)
    };
    match (failure, root.span().id()) {
        (Failure::NotHere, Some(_)) => replace_root(root, new),
        // The tree may be half changed: it cannot be matched with the new one.
        (Failure::Unnumberable, Some(id)) => {
            new.numberize(id, Span::FULL).unwrap();
            *root = new;
        }
        (_, None) => *root = new,
    }
    0..text.len()
}

/// How deep in the tree a node of a reparsed range may lie for the reparse to
/// be kept: a fraction of the parser's `MAX_DEPTH` (256), with a margin.
///
/// A reparse starts the parser's depth counter at 0, so a range whose
/// ancestors hold the parser past its limit would parse without the error a
/// full parse reports. Every parser frame open at a point adds at least half
/// a level of nodes around it (two frames per level for blocks in
/// stylesheet values, one or fewer elsewhere; `test_depth_guard_margin`
/// measures each construct), so a point no deeper than this in the tree is
/// within 128 frames of the root, well below the limit.
pub(crate) const DEPTH_GUARD: usize = 64;

/// Whether an incremental reparse that produced `root`, reparsing `range` of
/// the new text, may be kept rather than parsed again in full.
///
/// The parse of a node can depend on text outside it: whether a later quote
/// closes a string, whether a later brace closes a block, how deep the parser
/// is. The reparser checks the delimiters and the start-of-line state at the
/// edges of the range; those checks are exact for text without syntax errors,
/// but a broken item next to the range can take in or give up text in ways
/// they do not see (#273, #274, #334). So an edit to a tree with an error, or
/// one that makes an error, is parsed in full, which keeps the span numbers
/// of the top-level items it leaves alone (`replace_root`); and so is a
/// reparse deep in the tree, where the depth limit may differ (`DEPTH_GUARD`).
///
/// So `try_reparse` only ever sees an error-free old tree, and its checks for
/// erroneous items at the edges of a range no longer decide anything. They
/// are kept: a narrower guard, one that looks only at the items near an
/// edit, would need them again.
fn reusable(root: &SyntaxNode, range: &Range<usize>) -> bool {
    !root.diagnosis().errors && !reaches_depth(root, range, 0, 0)
}

/// Whether a node overlapping `range` (with `node` starting at `offset`) lies
/// [`DEPTH_GUARD`] levels or more below the root, `depth` being `node`'s own.
/// Visits only the nodes that overlap the range.
fn reaches_depth(node: &SyntaxNode, range: &Range<usize>, offset: usize, depth: usize) -> bool {
    if depth >= DEPTH_GUARD {
        return true;
    }
    let mut start = offset;
    for child in node.children() {
        let end = start + child.len();
        if start > range.end {
            break;
        }
        if end >= range.start && reaches_depth(child, range, start, depth + 1) {
            return true;
        }
        start = end;
    }
    false
}

/// Why an attempt at an incremental reparse failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The edit cannot be reparsed at this level; the tree is unchanged.
    NotHere,
    /// Children were reparsed and put in, but there were no span numbers
    /// left to give them. The tree is left half changed, its ancestors'
    /// lengths not updated, so no level above may try again, and a full
    /// reparse may not keep anything of it.
    Unnumberable,
}

/// Put a freshly parsed tree in the place of `root`, keeping what it can of
/// the old tree.
///
/// The new root's children are spliced into the old root, which keeps the
/// leading and trailing top-level items that are equal apart from spans, with
/// their span numbers and shared subtrees, and numbers only the items between
/// them in the gap their predecessors leave. A full reparse is forced by an
/// edit such as an unbalanced `]`, which changes one item: without this, every
/// span in the file would be renumbered, and every memoised result keyed on
/// one, such as a call of a closure defined in the file, would be computed
/// again (#241). When the gap is too narrow, the neighbouring kept items are
/// numbered again too, a widening number of them on each side, as after any
/// incremental reparse; when even all of them do not fit, the whole new tree
/// is numbered. `root` must be whole and numbered: the old tree, or the
/// tree an incremental reparse made that the guard then rejected (only
/// items equal to the full parse apart from spans are kept, so a wrong
/// incremental result never survives), never one half changed by a failed
/// attempt.
fn replace_root(root: &mut SyntaxNode, mut new: SyntaxNode) {
    let id = root.span().id().expect("a numbered root");
    if root.kind() == new.kind() && root.is_inner() && new.is_inner() {
        let old_len = root.children().len();
        let children: Vec<SyntaxNode> = new.children().cloned().collect();
        let mut spliced = root.clone();
        if spliced.replace_children(0..old_len, children).is_ok() {
            *root = spliced;
            return;
        }
    }
    new.numberize(id, Span::FULL).unwrap();
    *root = new;
}

/// Try to reparse inside the given node, returning the range that was
/// ultimately reparsed.
///
/// We start by doing a depth-first search for the innermost node or nodes which
/// fully surround the replaced range. This can be a single node that is a
/// code/content block or one or more nodes that are markup expressions and are
/// directly inside a markup block or the top-level markup. In either case, we
/// call the parser and succeed only if the parsed text has balanced delimiters
/// with the same delimiter nesting level as before. Otherwise, we expand the
/// set of markup expressions outwards or return upwards until we either get a
/// parse that does succeed or we parse the entire text.
///
/// Note that we currently only reparse markup expressions at the top-level or
/// directly inside a markup block. E.g. we don't reparse markup expressions
/// inside lists or headings, etc. In the past we did reparse those, but the
/// implementation was very buggy due to edge cases surrounding indentation and
/// newlines, and was eventually removed without much performance impact. It's
/// still potentially desireable to handle some of those cases (individual list
/// items can get quite long in practice), but only if the implementation can be
/// easily reasoned as correct and shows a measured performance improvement.
///
/// We also do not currently reparse math in any capacity, but it would not be
/// too difficult to include equations as another kind of block, or reparse math
/// expressions similarly to markup expressions.
///
/// Folio adds stylesheets: the innermost declaration block, slide master body
/// or overlay body around an edit is reparsed, or else a range of top-level
/// items. Declaration blocks of scoped rules in content are reparsed the same
/// way. `style` tells whether the contents of `node` are lexed as a stylesheet:
/// true below a stylesheet's root, except inside code and content blocks.
fn try_reparse(
    text: &str,
    replaced: Range<usize>,
    replacement_len: usize,
    parent_kind: Option<SyntaxKind>,
    node: &mut SyntaxNode,
    offset: usize,
    style: bool,
) -> Result<Range<usize>, Failure> {
    let (overlap, start_offset) =
        overlapping_children(node, replaced.clone(), offset).ok_or(Failure::NotHere)?;

    let node_kind = node.kind();
    let children = node.children_mut();

    if let [child] = &mut children[overlap.clone()]
        && start_offset < replaced.start
        && replaced.end < start_offset + child.len()
        && child.is_inner()
    {
        // A single child fully surrounds the edit. We either reparse within the
        // child, or reparse the child itself (if the child is a block).
        let prev_len = child.len();
        let prev_desc = child.descendants();
        let new_len = prev_len + replacement_len - replaced.len();
        let new_range = start_offset..start_offset + new_len;

        // Recursively descend and try to reparse at a lower level.
        match try_reparse(
            text,
            replaced.clone(),
            replacement_len,
            Some(node_kind),
            child,
            start_offset,
            style && !child.kind().is_block(),
        ) {
            Ok(range) => {
                // A lower level reparse succeeded! Update this node and return
                // the reparsed range.
                assert_eq!(child.len(), new_len);
                let new_desc = child.descendants();
                node.update_parent(prev_len, new_len, prev_desc, new_desc);
                return Ok(range);
            }
            Err(Failure::Unnumberable) => return Err(Failure::Unnumberable),
            Err(Failure::NotHere) => {}
        }

        // This is the innermost block which fully surrounds the text (and
        // hasn't failed at reparsing yet), reparse!
        let reparsed = match child.kind() {
            kind if kind.is_block() => reparse_block(text, new_range.clone()),
            kind @ (SyntaxKind::RuleBlock | SyntaxKind::MasterBlock | SyntaxKind::OverlayBody) => {
                reparse_style_block(text, new_range.clone(), kind, style)
            }
            _ => None,
        };
        if let Some(reparsed) = reparsed {
            // Reparsing succeeded, but we can still fail if we're out of span
            // numbers to assign to nodes (this is rare).
            return node
                .replace_children(overlap, vec![reparsed])
                .map(|()| new_range)
                .map_err(|_| Failure::Unnumberable);
        }
    }

    if node_kind == SyntaxKind::Stylesheet && parent_kind.is_none() {
        expand_and_reparse_stylesheet(text, replaced, replacement_len, node, overlap, offset)
    } else if node_kind == SyntaxKind::Markup
        && matches!(parent_kind, None | Some(SyntaxKind::ContentBlock))
    {
        expand_and_reparse_markup(
            text,
            replaced,
            replacement_len,
            node,
            overlap,
            offset,
            parent_kind.is_none(),
        )
    } else {
        Err(Failure::NotHere)
    }
}

/// Reparse a range of markup expressions, expanding the range exponentially on
/// each iteration.
fn expand_and_reparse_markup(
    text: &str,
    replaced: Range<usize>,
    replacement_len: usize,
    node: &mut SyntaxNode,
    overlap: Range<usize>,
    offset: usize,
    top_level: bool,
) -> Result<Range<usize>, Failure> {
    let children = node.children().as_slice();

    // Reparse a segment. Retries until it works, taking exponentially more
    // children into account.
    let mut expansion = 1;
    loop {
        // Add slack in both directions.
        let mut start = overlap.start.saturating_sub(expansion.max(2));
        let mut end = (overlap.end + expansion).min(children.len());

        // Expand to the left.
        while start > 0 && expand(&children[start]) {
            start -= 1;
        }

        // Expand to the right.
        while end < children.len() && expand(&children[end]) {
            end += 1;
        }

        // Also take hash.
        if start > 0 && children[start - 1].kind() == SyntaxKind::Hash {
            start -= 1;
        }

        // Never end right after a hash: the expression that follows it was
        // parsed in code mode, so it cannot be reused on its own. (Not in
        // typst-syntax v0.15.1, where an edit such as inserting `#` before
        // `  #f()` leaves a stale `f()` call in the tree.)
        if end > 0 && end < children.len() && children[end - 1].kind() == SyntaxKind::Hash {
            end += 1;
            // Nor before the empty errors that expression ended with, such
            // as a missing semicolon: the reparse makes them again.
            while end < children.len()
                && children[end].kind().is_error()
                && children[end].is_empty()
            {
                end += 1;
            }
        }

        // Synthesize what `at_start` and `nesting` would be at the start of the
        // reparse.
        let mut prefix_len = 0;
        let mut nesting = 0;
        let mut at_start = true;
        for child in &children[..start] {
            prefix_len += child.len();
            next_at_start(child, &mut at_start);
            next_nesting(child, &mut nesting);
        }

        // Determine what `at_start` will have to be at the end of the reparse.
        let mut prev_len = 0;
        let mut prev_at_start_after = at_start;
        let mut prev_nesting_after = nesting;
        for child in &children[start..end] {
            prev_len += child.len();
            next_at_start(child, &mut prev_at_start_after);
            next_nesting(child, &mut prev_nesting_after);
        }

        // Determine the range in the new text that we want to reparse.
        let shifted = offset + prefix_len;
        let new_len = prev_len + replacement_len - replaced.len();
        let new_range = shifted..shifted + new_len;
        let at_end = end == children.len();

        // Reparse!
        let reparsed = reparse_markup(
            text,
            new_range.clone(),
            &mut at_start,
            &mut nesting,
            top_level,
        );

        if let Some(newborns) = reparsed {
            // If more children follow, at_start must match its previous value.
            // Similarly, if we children follow or we not top-level the nesting
            // must match its previous value.
            if (at_end || at_start == prev_at_start_after)
                && ((at_end && top_level) || nesting == prev_nesting_after)
            {
                // Reparsing succeeded, but we can still fail if we're out of
                // span numbers to assign to nodes (this is rare).
                return node
                    .replace_children(start..end, newborns)
                    .map(|()| new_range)
                    .map_err(|_| Failure::Unnumberable);
            }
        }

        // If it didn't even work with all children, we give up.
        if start == 0 && at_end {
            break;
        }

        // Exponential expansion to both sides.
        expansion *= 2;
    }

    Err(Failure::NotHere)
}

/// Reparse a range of top-level stylesheet items, expanding the range
/// exponentially on each iteration. Items are delimited by their own braces
/// and semicolons, so no state carries over from one item to the next.
fn expand_and_reparse_stylesheet(
    text: &str,
    replaced: Range<usize>,
    replacement_len: usize,
    node: &mut SyntaxNode,
    overlap: Range<usize>,
    offset: usize,
) -> Result<Range<usize>, Failure> {
    let children = node.children().as_slice();
    let mut expansion = 1;
    loop {
        let mut start = overlap.start.saturating_sub(expansion);
        let mut end = (overlap.end + expansion).min(children.len());

        // The reparse must start after an item that is closed by its own `}`
        // or `;`. An unclosed one, such as a token definition without its
        // semicolon, could have taken in the edited text.
        while start > 0 {
            let prev = &children[start - 1];
            if prev.kind().is_trivia()
                || prev.diagnosis().errors
                || !matches!(last_leaf_text(prev), "}" | ";")
            {
                start -= 1;
            } else {
                break;
            }
        }

        // The reparse must stop at the start of an item, after the trivia
        // and errors that precede it.
        while end < children.len()
            && (children[end].kind().is_trivia() || children[end].kind().is_error())
        {
            end += 1;
        }

        let prefix_len: usize = children[..start].iter().map(SyntaxNode::len).sum();
        let prev_len: usize = children[start..end].iter().map(SyntaxNode::len).sum();
        let shifted = offset + prefix_len;
        let new_len = prev_len + replacement_len - replaced.len();
        let new_range = shifted..shifted + new_len;
        let at_end = end == children.len();

        if let Some(newborns) = reparse_stylesheet_items(text, new_range.clone()) {
            return node
                .replace_children(start..end, newborns)
                .map(|()| new_range)
                .map_err(|_| Failure::Unnumberable);
        }

        if start == 0 && at_end {
            break;
        }
        expansion *= 2;
    }
    Err(Failure::NotHere)
}

/// The text of the last non-empty leaf of a node.
fn last_leaf_text(node: &SyntaxNode) -> &str {
    match node.children().rev().find(|child| !child.is_empty()) {
        Some(child) => last_leaf_text(child),
        None => node.leaf_text(),
    }
}

/// The indices and start offset of the children which overlap a replaced range.
/// Returns `None` if the children don't fully cover the range. Will never
/// return an empty range if it returns `Some`.
///
/// Note that the returned `Range<usize>` is the range of children in the
/// parent, but the returned `usize` is a byte offset in the source text.
fn overlapping_children(
    node: &SyntaxNode,
    range: Range<usize>,
    mut offset: usize,
) -> Option<(Range<usize>, usize)> {
    if !node.is_inner() || offset > range.start || range.end > offset + node.len() {
        // No set of children will fully cover the range, no need to search.
        return None;
    }
    let mut index = 0;
    let mut start = 0;
    let mut start_offset = offset;
    for child in node.children() {
        if offset < range.start {
            start = index;
            start_offset = offset;
        }
        offset += child.len();
        index += 1;
        if range.end < offset {
            break;
        }
    }
    debug_assert!(start_offset <= range.start);
    debug_assert!(range.end <= offset);
    Some((start..index, start_offset))
}

/// Whether the selection should be expanded beyond a node of this kind.
fn expand(node: &SyntaxNode) -> bool {
    let kind = node.kind();
    kind.is_trivia()
        || kind.is_error()
        || kind == SyntaxKind::Semicolon
        || node.leaf_text() == "/"
        || node.leaf_text() == ":"
        || node.leaf_text() == "|"
}

/// Whether `at_start` would still be true after this node given the
/// previous value of the property.
///
/// The parser sets `at_start` from the trivia just before the next token,
/// wherever that trivia ended up in the tree: an unclosed `*` or `_` stops
/// at a paragraph break and keeps it inside its own markup. So a node that
/// is not trivia itself still counts the newlines in the trivia it ends
/// with. (Not in typst-syntax v0.15.1, which looks only at the node's own
/// kind, so that the `=` in `*.\n\n=\nsei` is reparsed as text after an
/// edit behind it.)
fn next_at_start(node: &SyntaxNode, at_start: &mut bool) {
    match trailing_newline(node) {
        Trailing::Transparent(newline) => *at_start |= newline,
        Trailing::Token(newline) => *at_start = newline,
    }
}

/// What the trivia a node ends with does to `at_start`.
enum Trailing {
    /// The node holds only trivia and empty nodes: it keeps the previous
    /// value, set to true if this holds a newline.
    Transparent(bool),
    /// The node holds a token: the value is whether a newline follows the
    /// last one.
    Token(bool),
}

/// Reads a node's trailing trivia from its end, stopping at the last token.
fn trailing_newline(node: &SyntaxNode) -> Trailing {
    let kind = node.kind();
    if kind.is_trivia() {
        return Trailing::Transparent(
            kind == SyntaxKind::Parbreak
                || (kind == SyntaxKind::Space && node.leaf_text().chars().any(is_newline)),
        );
    }
    if node.is_empty() {
        return Trailing::Transparent(false);
    }
    if !node.is_inner() {
        return Trailing::Token(false);
    }
    let mut newline = false;
    for child in node.children().rev() {
        match trailing_newline(child) {
            Trailing::Transparent(nl) => newline |= nl,
            Trailing::Token(nl) => return Trailing::Token(newline || nl),
        }
    }
    Trailing::Transparent(newline)
}

/// Update `nesting` based on the node.
fn next_nesting(node: &SyntaxNode, nesting: &mut usize) {
    if node.kind() == SyntaxKind::Text {
        match node.leaf_text().as_str() {
            "[" => *nesting += 1,
            "]" if *nesting > 0 => *nesting -= 1,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::DEPTH_GUARD;
    use crate::{LinkedNode, Source, Span, parse, parse_stylesheet};

    /// How to replace text in the test string.
    enum Edit {
        /// Insert at the end.
        End,
        /// Insert at an index.
        At(usize),
        /// Replace the given range.
        Range(Range<usize>),
        /// Replace the first match in the original.
        Match(&'static str),
        /// Replace at the index after the first match of this string.
        After(&'static str),
    }

    impl Edit {
        #[track_caller]
        fn into_range(self, text: &str) -> Range<usize> {
            match self {
                Self::End => text.len()..text.len(),
                Self::At(index) => {
                    assert!(text.len() >= index, "index is out of bounds");
                    index..index
                }
                Self::Range(range) => {
                    assert!(text.len() >= range.end, "range is out of bounds");
                    range
                }
                Self::Match(pat) => {
                    let start = text.find(pat).expect("pattern must exist in original");
                    start..start + pat.len()
                }
                Self::After(pat) => {
                    let start = text.find(pat).expect("pattern must exist in original");
                    let end = start + pat.len();
                    end..end
                }
            }
        }
    }

    /// What kind of reparsing happened.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Reparse<'a> {
        /// The whole text was reparsed.
        All,
        /// The text was parsed incrementally matching this string.
        Incr(&'a str),
    }

    #[track_caller]
    fn test(text: &str, edit: Edit, with: &str, expected: Reparse<'_>) {
        let mut source = Source::detached(text);
        let orig_tree = source.root().clone();
        // `Source::edit()` is the public interface for reparsing.
        let replaced_range = source.edit(edit.into_range(text), with);
        let mut reparsed_tree = source.root().clone();
        let mut normal_parse = parse(source.text());
        reparsed_tree.synthesize(Span::detached());
        normal_parse.synthesize(Span::detached());
        if reparsed_tree != normal_parse {
            eprintln!("Original source text: {text:?}");
            eprintln!("Original tree:\n{orig_tree:#?}");
            eprintln!("New source text: {:?}", source.text());
            eprintln!("Reparsed tree:\n{reparsed_tree:#?}");
            eprintln!("Normal parse tree:\n{normal_parse:#?}");
            panic!("Reparsed tree did not match normal parse");
        }
        let actual = if replaced_range == (0..source.text().len()) {
            Reparse::All
        } else {
            Reparse::Incr(&source.text()[replaced_range])
        };
        assert_eq!(actual, expected);
    }

    /// Basic tests for the reparsing algorithm and the testing framework.
    #[test]
    fn test_reparse_basic() {
        use Reparse::*;
        // Replace everything with something else:
        test("some content", Edit::Match("some content"), "do it", All);
        test("some content", Edit::Range(0..12), "", All);
        test("", Edit::At(0), "do it", All);
        test("", Edit::End, "do it", All);
        // Add something at the end:
        test("some content", Edit::After("some content"), " do it", All);
    }

    /// Test incremental reparsing of markup expressions.
    #[test]
    fn test_reparse_markup() {
        use Reparse::*;
        // Tilde is useful because it always creates a distinct token, whereas
        // spaces may join with adjacent text as one token.
        test("abc~def~gh~", Edit::Range(5..6), "+", Incr("abc~d+f~"));
        test("~~~~~~~", Edit::Range(3..4), "A", Incr("~~~A~~"));
        test("abc~~", Edit::Match("b"), "", Incr("ac~"));
        // Newlines behave slightly differently to tildes:
        test("~b~c~d~e", Edit::At(0), "a", Incr("a~b"));
        test("a~b~c~d~", Edit::End, "e", Incr("~d~e"));
        test("\nb\nc\nd\ne", Edit::At(0), "a", Incr("a\nb\n"));
        test("a\nb\nc\nd\n", Edit::End, "e", Incr("c\nd\ne"));
        test("#var. hello", Edit::Match(" "), " ", All);
        test("#var;hello", Edit::Range(9..10), "a", All);
        test("https:/world", Edit::After("/"), "/", All);
        test("hello  world", Edit::Match("world"), "walkers", All);
        test("a d e", Edit::Match(" d"), " b c d", All);
        test("~*~*~", Edit::At(2), "*", All);
        test("::1\n2. a\n3", Edit::After(" "), "4", Incr("1\n2. 4a\n"));
        test("* #{1+2} *", Edit::Match("2"), "3", Incr("{1+3}"));
        test(
            "#{(0, 1, 2)}",
            Edit::Match("1"),
            "11pt",
            Incr("{(0, 11pt, 2)}"),
        );
        test("\n= A heading", Edit::After("A"), "n evocative", All);
        test(
            "#call() abc~d",
            Edit::After("()"),
            "[]",
            Incr("#call()[] abc"),
        );
        test("a your thing a", Edit::Range(6..7), "a", All);
        test(
            "#grid(columns: (auto, 1fr, 40%))",
            Edit::Match("auto"),
            "4pt",
            All,
        );
        test(
            "abc\n= a head\njoke",
            Edit::Match("\n"),
            "\nmore\n\n",
            Incr("abc\nmore\n\n= a head\n"),
        );
        test("#rule f { a: b.. }", Edit::End, "c", All);
        test("#for", Edit::End, "//", All);
        test("a\n#let \nb", Edit::At(7), "i", All);
        test("#{{let x = z}; a = 1} b", Edit::At(7), "//", All);
        test("a ```typst hello```", Edit::Range(16..17), "", All);
        test("a{b}c", Edit::At(1), "#", All);
        test("a#{b}c", Edit::Match("#"), "", All);
        // An unclosed strong or emph keeps the paragraph break, so the heading
        // marker after it is still at the start of a line. Texts with a syntax
        // error, before or after the edit, are parsed in full (#334).
        test("*.\n\n=\nsei", Edit::At(7), "", All);
        test("_a\n\n= b\nc~d~e", Edit::At(9), "x", All);
        // The empty error an embedded expression ends with is made once.
        test("\n#element a}", Edit::At(0), "", All);
    }

    /// Edits that make, unmake or change table rows parse as a full parse
    /// does (SPEC §7.4).
    #[test]
    fn test_reparse_table_rows() {
        #[track_caller]
        fn same(text: &str, edit: Edit, with: &str) {
            let mut source = Source::detached(text);
            source.edit(edit.into_range(text), with);
            let mut reparsed = source.root().clone();
            let mut fresh = parse(source.text());
            reparsed.synthesize(Span::detached());
            fresh.synthesize(Span::detached());
            assert_eq!(reparsed, fresh, "after an edit of {text:?}");
        }
        let grid = "a\n#grid[\n  | a | b |\n  | c | d |\n]\nz";
        same(grid, Edit::Match("b"), "x | y");
        same(grid, Edit::Match("| c"), "c");
        same(grid, Edit::Match("c |"), "|");
        same(grid, Edit::Match("  | c"), "x | c");
        same(grid, Edit::Match("d |"), "$|x|$ |");
        same(grid, Edit::Match("b |"), "[b |");
        same("x | a | b |\n| c |\n", Edit::Match("x "), "");
        same("| a | b |\n| c |\n", Edit::At(0), "x ");
        same("~~\n| a |~~\n~~", Edit::Match("a"), "#f[\n|]");
    }

    /// Test incremental reparsing of code and content blocks.
    #[test]
    fn test_reparse_block() {
        use Reparse::*;
        test(
            "Hello #{ x + 1 }!",
            Edit::Match("x"),
            "abc",
            Incr("{ abc + 1 }"),
        );
        test("A#{}!", Edit::After("{"), "\"", All);
        test("#{ [= x] }!", Edit::After("="), "=", Incr("== x"));
        test("#[[]]", Edit::At(3), "\\", All);
        test("#[[ab]]", Edit::Match("b"), "\\", All);
        test("#{}}", Edit::After("{"), "{", All);
        test("A: #[BC]", Edit::After("B"), "{", Incr("B{C"));
        test("A: #[BC]", Edit::After("B"), "#{", All);
        test("A: #[BC]", Edit::After("B"), "#{}", Incr("B#{}C"));
        test("#{\"ab\"}A", Edit::At(5), "c", Incr("{\"abc\"}"));
        test("#{\"ab\"}A", Edit::Range(5..6), "c", All);
        test("a#[]b", Edit::After("["), "#{", All);
        test(
            "a#{call(); abc}b",
            Edit::At(8),
            "[]",
            Incr("{call([]); abc}"),
        );
        test(
            "a #while x {\n g(x) \n}  b",
            Edit::After("{"),
            "//",
            Incr("{//\n g(x) \n}"),
        );
        test("a#[]b", Edit::After("["), "[hey]", Incr("[[hey]]"));
    }

    /// Test unclosed strings/blocks in embedded code.
    #[test]
    fn test_reparse_unclosed_embedded() {
        use Reparse::*;
        test("#\"a\nb\nc", Edit::End, "\"", All);
        test("#return \"a\nb\nc", Edit::End, "\"", All);
        test("#import \"@preview/pkg", Edit::End, "\"", All);
        test("#import [a\nb\nc", Edit::End, "]", All);
        test("#import $a\nb\nc", Edit::End, "$", All);
        test("#rule thing\"a\n", Edit::End, "\"", All);
        test("#let x = \"a\nb\nc", Edit::End, "\"", All);
        test("#let x = {\"a\nb\nc}", Edit::After("c"), "\"", All);
        test("#if x {\"a\nb\nc", Edit::End, "\"", All);
        test("#if x \"a\nb\nc", Edit::End, "\"", All);
        test("#if x \"a\nb\nc", Edit::After("x "), "{}", All);
        test("#if x $a\nb\nc", Edit::End, "$", All);
        test("#for x in \"a\nb\nc", Edit::End, "\"", All);
        test("#for x \"a\nb\nc", Edit::After("x "), "in ", All);
    }

    /// Every node's span leads back to that node: the numbers are unique and
    /// ordered as the span machinery requires.
    #[track_caller]
    fn assert_spans_resolve(source: &Source) {
        fn walk(source: &Source, node: LinkedNode<'_>) {
            let found = source.find(node.span()).expect("the span resolves");
            assert_eq!(found.range(), node.range(), "{:?}", node.kind());
            assert_eq!(found.kind(), node.kind());
            for child in node.children() {
                walk(source, child);
            }
        }
        walk(source, LinkedNode::new(source.root()));
    }

    /// The spans of the top-level items, in order.
    fn item_spans(source: &Source) -> Vec<Span> {
        source.root().children().map(|c| c.span()).collect()
    }

    /// A full reparse keeps the spans of the top-level items before and after
    /// the one the edit changed (#241).
    #[test]
    fn test_full_reparse_keeps_spans_of_unchanged_items() {
        let text = "#let g(x) = x + 1\n\nA #g(2) b.\n\nMore text here.\n";
        for (at, with) in [
            (text.find("A ").unwrap(), "]"),
            (text.find("More").unwrap(), "]"),
            (text.find(" b.").unwrap(), "]]"),
        ] {
            let mut source = Source::detached(text);
            let before = item_spans(&source);
            let first = source.root().children().next().unwrap().clone();
            let last = source.root().children().last().unwrap().clone();
            let range = source.edit(at..at, with);
            assert_eq!(range, 0..source.text().len(), "a full reparse");

            let mut fresh = parse(source.text());
            let mut reparsed = source.root().clone();
            fresh.synthesize(Span::detached());
            reparsed.synthesize(Span::detached());
            assert_eq!(reparsed, fresh);
            assert_spans_resolve(&source);

            // The `#let` and the trailing text keep their spans and their
            // shared subtrees.
            let after = item_spans(&source);
            assert_eq!(after[..2], before[..2]);
            assert_eq!(after.last(), before.last());
            assert!(source.root().children().next().unwrap().is_same(&first));
            assert!(source.root().children().last().unwrap().is_same(&last));
        }
    }

    /// Spans still resolve after many forced full reparses in a row, each of
    /// which narrows the gap before the item after the edit until the items
    /// kept before the edit are numbered again too.
    #[test]
    fn test_repeated_full_reparses_keep_spans_ordered() {
        let mut source = Source::detached("a\n\n#let f(x) = x\n\nb c d\n");
        let mut renumbered = 0;
        for i in 0..200 {
            let at = source.text().find("b c").unwrap();
            // The spans of the items wholly before `at` and from `from` on:
            // those a full reparse keeps.
            let kept = |source: &Source, from: usize| {
                let mut offset = 0;
                let mut spans = vec![];
                for child in source.root().children() {
                    if offset + child.len() < at || offset >= from {
                        spans.push(child.span());
                    }
                    offset += child.len();
                }
                spans
            };
            let before = kept(&source, at);
            // A paragraph of its own, so the gap before `b c d` halves each
            // time.
            let with = if i % 2 == 0 { "]\n\n" } else { "x ]\n\n" };
            assert_eq!(source.edit(at..at, with), 0..source.text().len());
            assert_spans_resolve(&source);
            renumbered += usize::from(kept(&source, at + with.len()) != before);
        }
        // The gap ran out, but only now and then.
        assert!(
            (1..50).contains(&renumbered),
            "renumbered {renumbered} times"
        );
        let mut fresh = parse(source.text());
        let mut reparsed = source.root().clone();
        fresh.synthesize(Span::detached());
        reparsed.synthesize(Span::detached());
        assert_eq!(reparsed, fresh);
    }

    /// Edits whose reparse depends on text outside the reparsed range are
    /// parsed in full (#334, #273, #274): the old or the new text has a
    /// syntax error, or the range is nested deeper than the parser's depth
    /// limit allows a reparse that starts at depth 0 to see.
    #[test]
    fn test_reparse_guard() {
        use Reparse::*;
        // A stale `Semicolon` after an embedded expression.
        test(">#f--;", Edit::At(0), "#let x = ", All);
        // An error token lexed as code.
        test(" #let x\"", Edit::At(0), "#if a ", All);
        // #274: a later quote changes an earlier broken binding.
        test(
            "#let g(x) = x + 1\n\nA #g(2) b.\n\n a - ement a\n#let f(x)\"= [#x]\n\n\
             - item\n- two #f(3)\n=#x\n\n#{ let y = a ;\"y }\n= Head\nMore *text$e.\n",
            Edit::Range(99..102),
            ")",
            All,
        );
        // Error-free text is still reparsed incrementally.
        test("a #f(x) b~c~d~e", Edit::Match("x"), "y", Incr("a #f(y) "));
    }

    #[track_caller]
    fn test_style(text: &str, range: Range<usize>, with: &str) -> Range<usize> {
        let mut source = Source::detached_stylesheet(text);
        let reparsed = source.edit(range, with);
        let mut incremental = source.root().clone();
        let mut fresh = parse_stylesheet(source.text());
        incremental.synthesize(Span::detached());
        fresh.synthesize(Span::detached());
        assert_eq!(incremental, fresh, "{:?}", source.text());
        reparsed
    }

    /// The guard in stylesheets: a block reparse inside a broken value, the
    /// unclosed attribute selector of #273, and parentheses nested to the
    /// parser's depth limit.
    #[test]
    fn test_stylesheet_reparse_guard() {
        let text = "a { b: 4pt{; c: d; }\nheading[level=1] { e: f; }\n";
        assert_eq!(test_style(text, 36..36, "}"), 0..text.len() + 1);
        let text = "doc { font-size: 11pt_\n\nheadi[g { font-size: 14pt; }\n\
                    @variant x { p { a: 1 } }\n--t: 3;\n";
        test_style(text, 30..33, "x");
        let text = format!("a {{ b: {}x{}; }}", "(".repeat(253), ")".repeat(253));
        let x = text.find('x').unwrap();
        let new_len = text.len() + 2;
        assert_eq!(test_style(&text, x..x + 1, "(x)"), 0..new_len);
        // Shallow and error-free: incremental.
        let text = "a { b: (1); }\nc { d: 2; }\n";
        assert_ne!(test_style(text, 8..9, "3"), 0..text.len());
    }

    /// `DEPTH_GUARD` keeps a margin of two: for each construct that nests,
    /// the tree is at least twice the guard deep at the last nesting a full
    /// parse takes without the depth error. A construct that opened parser
    /// frames with fewer nodes around them would fail here.
    #[test]
    fn test_depth_guard_margin() {
        use crate::{SyntaxNode, parse, parse_stylesheet};
        fn height(node: &SyntaxNode) -> usize {
            1 + node.children().map(height).max().unwrap_or(0)
        }
        fn too_deep(node: &SyntaxNode) -> bool {
            node.errors_and_warnings()
                .0
                .iter()
                .any(|e| e.message.contains("maximum parsing depth"))
        }
        // (stylesheet, before, opening, middle, closing, after)
        let shapes: &[(bool, &str, &str, &str, &str, &str)] = &[
            (false, "", "#[", "x", "]", ""),
            (false, "", "#[#[", "x", "]]", ""),
            (false, "", "#strong[", "x", "]", ""),
            (false, "", "_*", "x", "*_", ""),
            (false, "", "#[#", "x", "]", ""),
            (false, "#", "(", "x", ")", ""),
            (false, "#", "{", "x", "}", ""),
            (false, "#", "f(", "x", ")", ""),
            (false, "#", "if x {", "y", "}", ""),
            (false, "#", "f.with(", "x", ")", ""),
            (false, "#(", "-", "x", "", ")"),
            (false, "#(", "not ", "x", "", ")"),
            (false, "#(", "(1,", "2", ")", ")"),
            (false, "#(", "(a:", "2", ")", ")"),
            (false, "#let g = ", "(x) => ", "x", "", ""),
            (false, "#let (", "(", "a", ")", ") = x"),
            (false, "#{", "let x = ", "1", "", "}"),
            (false, "$", "(", "x", ")", "$"),
            (false, "$", "sqrt(", "x", ")", "$"),
            (false, "$", "a^(", "x", ")", "$"),
            (false, "$", "1/(", "x", ")", "$"),
            (false, "$", "#[$", "x", "$]", "$"),
            (true, "", "@variant m {", "a { b: c; }", "}", ""),
            (true, "a { b: ", "(", "1", ")", "; }"),
            (true, "a { b: ", "f(", "1", ")", "; }"),
            (true, "a { b: ", "-", "1", "", "; }"),
            (true, "a { b: ", "{", "1", "}", "; }"),
        ];
        for &(style, before, open, middle, close, after) in shapes {
            let tree = |n: usize| {
                let text = format!(
                    "{before}{}{middle}{}{after}",
                    open.repeat(n),
                    close.repeat(n)
                );
                if style {
                    parse_stylesheet(&text)
                } else {
                    parse(&text)
                }
            };
            let n = (1..600)
                .find(|&n| too_deep(&tree(n)))
                .unwrap_or_else(|| panic!("{open:?} nests without the depth error"));
            let h = height(&tree(n - 1));
            assert!(
                h >= 2 * DEPTH_GUARD,
                "{open:?}: {h} levels at the last nesting a parse takes"
            );
        }
    }

    /// A stylesheet that is parsed again in full keeps the spans of its
    /// unchanged rules too.
    #[test]
    fn test_full_stylesheet_reparse_keeps_spans() {
        let text = "doc { font-size: 11pt; }\n\nheading { font-size: 14pt; }\n";
        let mut source = Source::detached_stylesheet(text);
        let first = source.root().children().next().unwrap().clone();
        let at = text.find("heading").unwrap();
        let range = source.edit(at..at, "}");
        assert_eq!(range, 0..source.text().len(), "a full reparse");
        assert!(source.root().children().next().unwrap().is_same(&first));
        assert_spans_resolve(&source);
    }
}
