//! Span maps (SPEC §1.5, §12.2, §12.4): from what layout produces back to
//! where its source is written.
//!
//! Every glyph, shape and fragment names the source node it came from as a
//! [`SourceRef`], which is the node's span: the span numbers that the
//! evaluator gives content, the style stage gives styled nodes and the
//! text stage gives paragraphs are the ones that reach fragments. A glyph
//! also names the bytes of the node's text that it shows
//! ([`folio_component::Glyph::source`]). [`text_range`] turns those into
//! bytes of the file, by the kind of node:
//!
//! - text of markup and links are their text as written, so a glyph maps
//!   to exactly its characters, a ligature to all of them;
//! - a string literal without escapes is its text between the quotes;
//! - a `raw` element's text is its lines joined by newlines, so a glyph
//!   maps into its line and a line break to what separates two lines;
//! - any other node (a space, an escape, a shorthand, a quotation mark, an
//!   element call, an expression whose value was shown) maps as a whole.
//!
//! A span is valid for the revision of the file it was made from: an edit
//! may renumber the nodes it reparses. Node identity across revisions is
//! [`folio_syntax::NodeIds`].

use std::num::NonZeroU64;
use std::ops::Range;

use folio_component::SourceRef;
use folio_syntax::{LinkedNode, Source, Span, SyntaxKind};

/// The reference of a source node in fragments.
pub fn source_ref(span: Span) -> SourceRef {
    if span.is_detached() {
        SourceRef::DETACHED
    } else {
        SourceRef(span.into_raw().get())
    }
}

/// The span of a source node that a fragment refers to, detached for
/// [`SourceRef::DETACHED`].
pub fn span(source: SourceRef) -> Span {
    NonZeroU64::new(source.0).map_or(Span::detached(), Span::from_raw)
}

/// The bytes of a source file that show the bytes `within` of the text that
/// node `span` gave, by the rules of the [module](self): exactly those
/// bytes where the node's text is written as it is, else the whole node.
/// `None` when the span is not of a node of this file (it is detached, of
/// another file, or from another revision).
pub fn text_range(source: &Source, span: Span, within: Range<usize>) -> Option<Range<usize>> {
    let node = source.find(span)?;
    let whole = node.range();
    let text = source.text();
    let exact = match node.kind() {
        SyntaxKind::Text | SyntaxKind::Link => verbatim(whole.clone(), within),
        SyntaxKind::Str => {
            let written = text.get(whole.clone())?;
            if written.len() >= 2 && !written.contains('\\') {
                verbatim(whole.start + 1..whole.end - 1, within)
            } else {
                None
            }
        }
        SyntaxKind::Raw => raw(&node, within),
        _ => None,
    };
    Some(
        exact
            .filter(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
            .unwrap_or(whole),
    )
}

/// The part `within` of text written as it is at `base`, if it lies in it.
fn verbatim(base: Range<usize>, within: Range<usize>) -> Option<Range<usize>> {
    (within.start <= within.end && within.end <= base.len())
        .then(|| base.start + within.start..base.start + within.end)
}

/// The part `within` of a `raw` element's text: its lines, the text nodes
/// among its children, joined by newlines (as the evaluator joins them).
/// A part that starts at a newline starts where its line ends; one that
/// ends after a newline ends where the next line starts.
fn raw(node: &LinkedNode<'_>, within: Range<usize>) -> Option<Range<usize>> {
    let lines: Vec<Range<usize>> = node
        .children()
        .filter(|child| child.kind() == SyntaxKind::Text)
        .map(|child| child.range())
        .collect();
    let map = |p: usize, end: bool| {
        let mut f = 0;
        for line in &lines {
            let e = f + line.len();
            if (!end && f <= p && p < e) || (end && f < p && p <= e) {
                return Some(line.start + (p - f));
            }
            if !end && p == e {
                return Some(line.end);
            }
            if end && p == f {
                return Some(line.start);
            }
            f = e + 1;
        }
        None
    };
    let start = map(within.start, false)?;
    let end = if within.is_empty() {
        start
    } else {
        map(within.end, true)?
    };
    (start <= end).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use folio_syntax::FileId;

    use super::*;

    fn source(text: &str) -> Source {
        Source::new(
            FileId::from_raw(std::num::NonZeroU16::new(7).unwrap()),
            text.into(),
        )
    }

    /// The span of the first node of a kind.
    fn find(source: &Source, kind: SyntaxKind) -> Span {
        fn walk(node: &LinkedNode<'_>, kind: SyntaxKind) -> Option<Span> {
            if node.kind() == kind {
                return Some(node.span());
            }
            node.children().find_map(|child| walk(&child, kind))
        }
        walk(&LinkedNode::new(source.root()), kind).unwrap()
    }

    fn resolve(source: &Source, kind: SyntaxKind, within: Range<usize>) -> &str {
        let span = find(source, kind);
        &source.text()[text_range(source, span, within).unwrap()]
    }

    #[test]
    fn source_refs_round_trip() {
        let source = source("Hello");
        let span = find(&source, SyntaxKind::Text);
        assert_eq!(super::span(source_ref(span)), span);
        assert!(super::span(SourceRef::DETACHED).is_detached());
        assert!(source_ref(Span::detached()).is_detached());
    }

    #[test]
    fn text_maps_exactly() {
        let source = source("Say *office* hours");
        assert_eq!(resolve(&source, SyntaxKind::Text, 0..3), "Say");
        assert_eq!(resolve(&source, SyntaxKind::Text, 1..2), "a");
        // Past the node, the whole node.
        assert_eq!(resolve(&source, SyntaxKind::Text, 0..9), "Say");
        let empty = text_range(&source, find(&source, SyntaxKind::Text), 2..2).unwrap();
        assert_eq!(empty, 2..2);
    }

    #[test]
    fn strings_map_between_their_quotes() {
        let plain = source("#\"abc\" and more");
        assert_eq!(resolve(&plain, SyntaxKind::Str, 1..3), "bc");
        let escaped = source("#\"a\\nb\" and more");
        assert_eq!(resolve(&escaped, SyntaxKind::Str, 0..1), "\"a\\nb\"");
    }

    #[test]
    fn other_nodes_map_whole() {
        let source = source("A -- b \\# c");
        assert_eq!(resolve(&source, SyntaxKind::Shorthand, 0..1), "--");
        assert_eq!(resolve(&source, SyntaxKind::Escape, 0..1), "\\#");
        assert_eq!(resolve(&source, SyntaxKind::Space, 0..1), " ");
    }

    #[test]
    fn raw_maps_by_lines() {
        let source = source("```rust\n  let x;\n  x += 1;\n  ```\n");
        // The evaluator's text: "let x;\nx += 1;"
        assert_eq!(resolve(&source, SyntaxKind::Raw, 0..3), "let");
        assert_eq!(resolve(&source, SyntaxKind::Raw, 7..13), "x += 1");
        // The newline between the lines stands for what separates them.
        assert_eq!(resolve(&source, SyntaxKind::Raw, 6..7), "\n  ");
        assert_eq!(resolve(&source, SyntaxKind::Raw, 4..9), "x;\n  x ");
        // Outside its text, the whole element.
        assert_eq!(
            resolve(&source, SyntaxKind::Raw, 0..40),
            "```rust\n  let x;\n  x += 1;\n  ```"
        );
    }

    #[test]
    fn spans_of_other_files_do_not_resolve() {
        let a = source("Hello");
        let b = Source::detached("Hello");
        let span = find(&a, SyntaxKind::Text);
        assert_eq!(text_range(&b, span, 0..1), None);
    }
}
