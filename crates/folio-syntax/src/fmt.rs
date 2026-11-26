//! Canonical formatting of content (`.fol`) and stylesheets (`.fsty`), and
//! the text edits it is made of (SPEC §12.1).
//!
//! The formatter works on the lossless syntax tree and changes only the
//! whitespace between tokens. Every other byte stays as written: comments,
//! strings, raw text, equations, and the line breaks of prose. The rules are
//! conservative, so a file written as SPEC's own examples are is already
//! formatted:
//!
//! - Line ends are `\n`, lines carry no trailing whitespace (line comments
//!   included; block comments, strings, raw text and equations keep their
//!   bytes), there is at most one blank line in a row, and a file that is
//!   not empty starts with its first token, or in markup with its
//!   indentation, and ends with one newline.
//! - Lines of code and stylesheets are indented by two spaces more than the
//!   line that opened the innermost bracket (`{`, `(`, `[`) still open, so
//!   several brackets opened on one line count once. A line that starts by
//!   closing a bracket, in markup too, takes the opener's indentation. The
//!   later lines of an item that runs over several lines (a declaration, an
//!   expression, a selector) are indented two spaces more, except a line
//!   that starts with `else`; a bracket opened on such a line indents from
//!   the item's first line.
//! - Other lines of markup keep their indentation, which is part of their
//!   meaning (list items nest by it).
//! - Within a line, a run of spaces becomes one space, except before a
//!   comment and before the `{` of a rule, which keep the alignment they
//!   were given. In code and stylesheets a space follows `,` (except in the
//!   step lists of selectors, `:step(1,3-4)`), `;` and the `:` of a named
//!   argument, declaration or field, surrounds `=` in a binding, `=>`, and
//!   the combinators `>` and `+` of selectors, and separates the braces of
//!   a declaration block from what they hold; none goes before `,`, `;` or
//!   the `:` of a named argument, after `(` or before `)`.
//!
//! Formatting never changes what a file means: the result parses to the same
//! tree apart from whitespace ([`equivalent`]), which [`format()`] checks
//! before it answers. A file with syntax errors is left as it is.
//!
//! The formatter answers with [`Edits`], replacements of byte ranges of the
//! original text, so a caller can apply them to a buffer in place, keep the
//! cursor, or show them as a fix. Other rewriters of source text, such as
//! moving a local override into a stylesheet (SPEC §5.4) or writing an edit
//! from the visual editor back (SPEC §12.4), make their changes as the same
//! kind of edits.

use std::fmt::{self, Display};
use std::ops::Range;

use ecow::EcoString;

use crate::{SyntaxKind, SyntaxNode, parse, parse_stylesheet};

/// A replacement of a byte range of a text.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Edit {
    /// The bytes to replace, in the original text.
    pub range: Range<usize>,
    /// What replaces them.
    pub text: EcoString,
}

/// Edits of one text that do not overlap, in the order of their ranges.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Edits(Vec<Edit>);

impl Edits {
    /// No edits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces `range` with `text`.
    ///
    /// # Panics
    /// If the range is reversed or overlaps an edit already made. Edits may
    /// touch; an insertion (an empty range) goes after the insertions
    /// already made at the same place and before a replacement that starts
    /// there.
    pub fn replace(&mut self, range: Range<usize>, text: impl Into<EcoString>) {
        assert!(range.start <= range.end, "a reversed range: {range:?}");
        let at = self.0.partition_point(|e| e.range.end <= range.start);
        if let Some(next) = self.0.get(at) {
            assert!(
                range.end <= next.range.start,
                "edits overlap: {range:?} and {:?}",
                next.range,
            );
        }
        self.0.insert(
            at,
            Edit {
                range,
                text: text.into(),
            },
        );
    }

    /// Inserts `text` at byte `at`.
    pub fn insert(&mut self, at: usize, text: impl Into<EcoString>) {
        self.replace(at..at, text);
    }

    /// Deletes `range`.
    pub fn delete(&mut self, range: Range<usize>) {
        self.replace(range, "");
    }

    /// Whether there are no edits.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of edits.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The edits, in the order of their ranges.
    pub fn iter(&self) -> std::slice::Iter<'_, Edit> {
        self.0.iter()
    }

    /// The text with the edits made.
    ///
    /// # Panics
    /// If an edit reaches past the end of `text` or does not start and end
    /// at character boundaries.
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for edit in &self.0 {
            out.push_str(&text[cursor..edit.range.start]);
            out.push_str(&edit.text);
            cursor = edit.range.end;
        }
        out.push_str(&text[cursor..]);
        out
    }
}

impl<'a> IntoIterator for &'a Edits {
    type Item = &'a Edit;
    type IntoIter = std::slice::Iter<'a, Edit>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Why a text was not formatted.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FormatError {
    /// The text has syntax errors, so it is left as it is.
    Syntax,
    /// Formatting would have changed what the text means. This is a bug in
    /// the formatter; the text is left as it is.
    Unsound,
}

impl Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syntax => "the file has syntax errors",
            Self::Unsound => "formatting would change what the file means",
        })
    }
}

impl std::error::Error for FormatError {}

/// Formats content markup.
pub fn format_markup(text: &str) -> Result<String, FormatError> {
    format(&parse(text)).map(|edits| edits.apply(text))
}

/// Formats a stylesheet.
pub fn format_stylesheet(text: &str) -> Result<String, FormatError> {
    format(&parse_stylesheet(text)).map(|edits| edits.apply(text))
}

/// The edits that format a file, given its syntax tree: the root of content
/// ([`SyntaxKind::Markup`], from [`parse`]) or of a stylesheet
/// ([`SyntaxKind::Stylesheet`], from [`parse_stylesheet`]). The edits are
/// empty when the file is formatted already.
///
/// # Panics
/// If `root` is neither.
pub fn format(root: &SyntaxNode) -> Result<Edits, FormatError> {
    let reparse: fn(&str) -> SyntaxNode = match root.kind() {
        SyntaxKind::Markup => parse,
        SyntaxKind::Stylesheet => parse_stylesheet,
        kind => panic!("not the root of a file: {kind:?}"),
    };
    if root.diagnosis().errors {
        return Err(FormatError::Syntax);
    }
    let edits = Walk::new(root).run();
    if !edits.is_empty() {
        let text = root.full_text();
        let formatted = reparse(&edits.apply(&text));
        if formatted.diagnosis().errors || !equivalent(root, &formatted) {
            return Err(FormatError::Unsound);
        }
    }
    Ok(edits)
}

/// Whether two syntax trees of files are the same apart from whitespace that
/// carries no meaning: whitespace in code and stylesheets, the amount of
/// whitespace in markup (a space is a space, a paragraph break a paragraph
/// break), whitespace at the end of a line comment, and whitespace at the
/// start and end of a file.
pub fn equivalent(a: &SyntaxNode, b: &SyntaxNode) -> bool {
    let mut left = Vec::new();
    let mut right = Vec::new();
    shape(a, None, true, &mut left);
    shape(b, None, true, &mut right);
    left == right
}

/// A step of a walk over a tree, for [`equivalent`].
#[derive(Debug, Eq, PartialEq)]
enum Step {
    Enter(SyntaxKind),
    Leaf(SyntaxKind, EcoString),
    Exit,
}

/// Walks `node` into `out`, leaving out whitespace that carries no meaning.
fn shape(node: &SyntaxNode, parent: Option<SyntaxKind>, root: bool, out: &mut Vec<Step>) {
    let kind = node.kind();
    if is_space(kind) {
        if parent.is_some_and(is_markup) {
            out.push(Step::Leaf(kind, EcoString::new()));
        }
        return;
    }
    if kind == SyntaxKind::LineComment {
        // The spaces at the end of a comment's line mean nothing either.
        let text = node.leaf_text().trim_end_matches([' ', '\t']);
        out.push(Step::Leaf(kind, text.into()));
        return;
    }
    if node.is_empty() {
        // An inner node without children, such as the code of `{}`, or a
        // file without text.
        out.push(Step::Enter(kind));
        out.push(Step::Exit);
        return;
    }
    if node.children().len() == 0 || is_opaque(kind) {
        out.push(Step::Leaf(kind, node.full_text()));
        return;
    }
    out.push(Step::Enter(kind));
    let children = node.children().as_slice();
    let (mut start, mut end) = (0, children.len());
    if root {
        while start < end && is_space(children[start].kind()) {
            start += 1;
        }
        while end > start && is_space(children[end - 1].kind()) {
            end -= 1;
        }
    }
    // In markup, text and the spaces between words are one run: the lexer
    // joins words with a single space into one text, and more spaces into
    // texts with spaces between.
    let mut run: Option<EcoString> = None;
    for child in &children[start..end] {
        match child.kind() {
            SyntaxKind::Text | SyntaxKind::Space if is_markup(kind) => {
                let run = run.get_or_insert_default();
                if child.kind() == SyntaxKind::Text {
                    run.push_str(child.leaf_text());
                } else if !run.ends_with(' ') {
                    run.push(' ');
                }
            }
            _ => {
                if let Some(run) = run.take() {
                    out.push(Step::Leaf(SyntaxKind::Text, run));
                }
                shape(child, Some(kind), false, out);
            }
        }
    }
    if let Some(run) = run {
        out.push(Step::Leaf(SyntaxKind::Text, run));
    }
    out.push(Step::Exit);
}

/// Whitespace.
fn is_space(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::Space | SyntaxKind::Parbreak)
}

/// Nodes whose whitespace is markup, where it has meaning.
fn is_markup(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Markup
            | SyntaxKind::Heading
            | SyntaxKind::ListItem
            | SyntaxKind::EnumItem
            | SyntaxKind::TermItem
            | SyntaxKind::Strong
            | SyntaxKind::Emph
    )
}

/// Nodes kept exactly as written, whitespace included.
fn is_opaque(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Equation | SyntaxKind::TexEquation | SyntaxKind::Raw | SyntaxKind::Str
    )
}

/// Nodes that hold a sequence of items (statements, arguments, declarations,
/// rules), so that whitespace directly in them separates items, and
/// whitespace deeper down continues an item.
fn is_container(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Code
            | SyntaxKind::CodeBlock
            | SyntaxKind::Args
            | SyntaxKind::Array
            | SyntaxKind::Dict
            | SyntaxKind::Params
            | SyntaxKind::Destructuring
            | SyntaxKind::ImportItems
            | SyntaxKind::SchemaFields
            | SyntaxKind::SchemaBody
            | SyntaxKind::Stylesheet
            | SyntaxKind::RuleBlock
            | SyntaxKind::OverlayBody
            | SyntaxKind::MasterBlock
            | SyntaxKind::TemplateBlock
            | SyntaxKind::SelectorList
    )
}

/// Braces that hold declarations or template items, inside which a space
/// separates the braces from what they hold.
fn is_spaced_block(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::RuleBlock
            | SyntaxKind::OverlayBody
            | SyntaxKind::MasterBlock
            | SyntaxKind::SchemaBody
            | SyntaxKind::TemplateBlock
            | SyntaxKind::CodeBlock
    )
}

/// The blocks of declarations or rules that follow a selector or a name.
fn is_rule_block(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::RuleBlock | SyntaxKind::OverlayBody | SyntaxKind::MasterBlock
    )
}

/// A token: a leaf that is not whitespace, or a node kept as written.
#[derive(Debug, Copy, Clone)]
struct Token {
    kind: SyntaxKind,
    /// The kind of the node that holds it.
    parent: SyntaxKind,
    range: (usize, usize),
}

impl Token {
    /// Whether it opens a bracket of code or a stylesheet.
    fn opens(&self) -> bool {
        matches!(
            self.kind,
            SyntaxKind::LeftParen | SyntaxKind::LeftBrace | SyntaxKind::LeftBracket
        ) && !is_markup(self.parent)
    }

    /// Whether it closes a bracket of code or a stylesheet.
    fn closes(&self) -> bool {
        matches!(
            self.kind,
            SyntaxKind::RightParen | SyntaxKind::RightBrace | SyntaxKind::RightBracket
        ) && !is_markup(self.parent)
    }

    /// Whether it is a comment.
    fn is_comment(&self) -> bool {
        matches!(
            self.kind,
            SyntaxKind::LineComment | SyntaxKind::BlockComment
        )
    }
}

/// The whitespace between two tokens.
#[derive(Debug, Clone, Default)]
struct Gap {
    range: (usize, usize),
    /// Whether it is markup whitespace.
    markup: bool,
    /// When it lies inside an item of code or a stylesheet, rather than
    /// between items: the number of brackets open where the item starts.
    continues: Option<usize>,
}

/// What the formatter walks: tokens, each with the gap before it, and the
/// gap after the last.
struct Walk {
    text: EcoString,
    tokens: Vec<Token>,
    gaps: Vec<Gap>,
    /// The number of brackets open after the tokens so far.
    depth: usize,
    /// For each item being walked, the number of brackets open where it
    /// starts.
    items: Vec<usize>,
}

impl Walk {
    fn new(root: &SyntaxNode) -> Self {
        let mut this = Self {
            text: root.full_text(),
            tokens: Vec::new(),
            gaps: vec![Gap::default()],
            depth: 0,
            items: Vec::new(),
        };
        this.walk(root, root.kind(), 0);
        this
    }

    /// Collects the tokens and gaps of `node`, which starts at `offset`.
    fn walk(&mut self, node: &SyntaxNode, parent: SyntaxKind, offset: usize) {
        if node.is_empty() {
            // Nothing to separate, such as the empty code of `{}`.
            return;
        }
        let kind = node.kind();
        let end = offset + node.len();
        if is_space(kind) {
            let gap = self.gaps.last_mut().expect("there is always a gap");
            if gap.range.0 == gap.range.1 {
                gap.range = (offset, end);
                gap.markup = is_markup(parent);
                gap.continues = if is_container(parent) {
                    None
                } else {
                    self.items.last().copied()
                };
            } else {
                gap.range.1 = end;
            }
            return;
        }
        if node.children().len() == 0 || is_opaque(kind) {
            let token = Token {
                kind,
                parent,
                range: (offset, end),
            };
            if token.opens() {
                self.depth += 1;
            } else if token.closes() {
                self.depth = self.depth.saturating_sub(1);
            }
            self.tokens.push(token);
            self.gaps.push(Gap {
                range: (end, end),
                ..Gap::default()
            });
            return;
        }
        let mut at = offset;
        let items = is_container(kind) || is_markup(kind);
        for child in node.children() {
            if items {
                self.items.push(self.depth);
            }
            self.walk(child, kind, at);
            if items {
                self.items.pop();
            }
            at += child.len();
        }
    }

    /// Decides every gap.
    fn run(self) -> Edits {
        let mut edits = Edits::new();
        // The indentation of the current line, and that of the line that
        // started its item when it continues one, which brackets opened on
        // the line take.
        let mut indent = EcoString::new();
        let mut item_indent = EcoString::new();
        // The open brackets, each with the indentation of the line that
        // opened it, or of the line that started its item.
        let mut open: Vec<EcoString> = Vec::new();

        for (i, gap) in self.gaps.iter().enumerate() {
            let text = &self.text[gap.range.0..gap.range.1];
            let prev = i.checked_sub(1).map(|i| self.tokens[i]);
            let next = self.tokens.get(i).copied();
            let ascii = text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
            let newlines = count_newlines(text);

            let new: Option<EcoString> = match (prev, next) {
                // Whitespace other than spaces, tabs and line feeds is left
                // as it is.
                _ if !ascii => {
                    if newlines > 0 {
                        indent = trailing_indent(text).into();
                        item_indent = indent.clone();
                    }
                    None
                }
                // The start of the file: markup keeps the indentation of
                // its first line.
                (None, Some(_)) if gap.markup => {
                    let start = text.rfind(crate::is_newline).map_or(0, |at| at + 1);
                    indent = text[start..].into();
                    item_indent = indent.clone();
                    Some(indent.clone())
                }
                (None, _) => Some(EcoString::new()),
                // The end of the file.
                (_, None) => Some("\n".into()),
                (Some(_), Some(next)) if newlines > 0 => {
                    let mut continued = false;
                    let new_indent: EcoString = if next.closes() && !open.is_empty() {
                        open.last().cloned().unwrap_or_default()
                    } else if gap.markup {
                        trailing_indent(text).into()
                    } else {
                        let mut base = open.last().cloned().unwrap_or_default();
                        if !next.closes() {
                            if !open.is_empty() {
                                base.push_str("  ");
                            }
                            // An item over several lines: its later lines
                            // are indented once more, unless a bracket it
                            // opened still is, which indents them already.
                            // `else` belongs with its `if`.
                            if gap.continues == Some(open.len()) && next.kind != SyntaxKind::Else {
                                item_indent = base.clone();
                                continued = true;
                                base.push_str("  ");
                            }
                        }
                        base
                    };
                    indent = new_indent.clone();
                    if !continued {
                        item_indent = indent.clone();
                    }
                    let mut new = EcoString::from(if newlines > 1 { "\n\n" } else { "\n" });
                    new.push_str(&new_indent);
                    Some(new)
                }
                (Some(prev), Some(next)) => Some(if gap.markup {
                    inline_markup(text, next)
                } else {
                    inline_code(text, prev, next)
                }),
            };

            if let Some(new) = new
                && new != text
            {
                edits.replace(gap.range.0..gap.range.1, new);
            }

            // The brackets the next token opens or closes.
            if let Some(next) = next {
                // A line comment holds the spaces at the end of its line.
                if next.kind == SyntaxKind::LineComment {
                    let comment = &self.text[next.range.0..next.range.1];
                    let trimmed = comment.trim_end_matches([' ', '\t']).len();
                    if trimmed < comment.len() {
                        edits.delete(next.range.0 + trimmed..next.range.1);
                    }
                }
                // Each bracket remembers the indentation of the line that
                // opened it, so several opened on one line count once.
                if next.opens() {
                    open.push(item_indent.clone());
                } else if next.closes() {
                    open.pop();
                }
            }
        }
        edits
    }
}

/// The whitespace between two tokens of markup on one line.
fn inline_markup(text: &str, next: Token) -> EcoString {
    if next.is_comment() || text.is_empty() {
        text.into()
    } else {
        " ".into()
    }
}

/// The whitespace between two tokens of code or a stylesheet on one line.
fn inline_code(text: &str, prev: Token, next: Token) -> EcoString {
    use SyntaxKind as K;
    let keep = || -> EcoString {
        if text.is_empty() {
            "".into()
        } else {
            " ".into()
        }
    };

    if next.is_comment() {
        // Comments keep the alignment they were given.
        return if text.is_empty() {
            " ".into()
        } else {
            text.into()
        };
    }
    if prev.is_comment() {
        return keep();
    }
    if next.kind == K::LeftBrace && is_rule_block(next.parent) && !text.is_empty() {
        // Rules keep the alignment they were given (`mark   { .. }`).
        return text.into();
    }

    let separated = matches!(prev.parent, K::StepSpec | K::PseudoArgs | K::StepRange);
    let named_colon = |t: Token| {
        t.kind == K::Colon
            && matches!(
                t.parent,
                K::Named
                    | K::Keyed
                    | K::Declaration
                    | K::TokenDecl
                    | K::SchemaField
                    | K::SchemaEntry
            )
    };
    let binding_eq = |t: Token| {
        (t.kind == K::Eq
            && matches!(
                t.parent,
                K::LetBinding | K::Closure | K::SchemaField | K::DestructAssignment
            ))
            || t.kind == K::Arrow
    };
    let combinator = |t: Token| matches!(t.kind, K::Gt | K::Plus) && t.parent == K::Selector;

    // No space.
    if matches!(next.kind, K::Comma | K::Semicolon) && !separated
        || named_colon(next)
        || prev.kind == K::LeftParen && prev.opens()
        || next.kind == K::RightParen && next.closes()
    {
        return "".into();
    }

    // One space.
    if matches!(prev.kind, K::Comma | K::Semicolon) && !separated && !next.closes()
        || named_colon(prev)
        || binding_eq(prev)
        || binding_eq(next)
        || combinator(prev)
        || combinator(next)
        || next.kind == K::LeftBrace && is_spaced_block(next.parent) && prev.kind != K::Hash
        || prev.kind == K::LeftBrace && is_spaced_block(prev.parent) && next.kind != K::RightBrace
        || next.kind == K::RightBrace && is_spaced_block(next.parent) && prev.kind != K::LeftBrace
    {
        return " ".into();
    }

    keep()
}

/// The number of line breaks in whitespace, a `\r\n` counting once.
fn count_newlines(text: &str) -> usize {
    let mut count = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                count += 1;
            }
            c if crate::is_newline(c) => count += 1,
            _ => {}
        }
    }
    count
}

/// The whitespace after the last line break of `text`.
fn trailing_indent(text: &str) -> &str {
    match text.rfind(crate::is_newline) {
        Some(at) => {
            let c = text[at..].chars().next().expect("found above");
            &text[at + c.len_utf8()..]
        }
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn style(input: &str, expected: &str) {
        let output = format_stylesheet(input).unwrap();
        assert_eq!(output, expected, "formatting\n{input}");
        assert_eq!(format_stylesheet(&output).unwrap(), output, "idempotence");
    }

    #[track_caller]
    fn markup(input: &str, expected: &str) {
        let output = format_markup(input).unwrap();
        assert_eq!(output, expected, "formatting\n{input}");
        assert_eq!(format_markup(&output).unwrap(), output, "idempotence");
    }

    #[test]
    fn edits_apply_in_order() {
        let mut edits = Edits::new();
        edits.replace(4..5, "X");
        edits.insert(0, ">");
        edits.delete(1..2);
        edits.insert(4, "[");
        assert_eq!(edits.apply("abcdef"), ">acd[Xf");
        assert_eq!(edits.len(), 4);
    }

    #[test]
    #[should_panic(expected = "edits overlap")]
    fn overlapping_edits_panic() {
        let mut edits = Edits::new();
        edits.replace(1..3, "X");
        edits.replace(2..4, "Y");
    }

    #[test]
    fn stylesheet_spacing() {
        style("a{b:c;d :e}", "a { b: c; d: e }\n");
        style("a>b+c   d,e{x:1 ;}", "a > b + c d, e { x: 1; }\n");
        style("$accent:#1f5fa8;", "$accent: #1f5fa8;\n");
        style("a { margin: 0 -1mm; }", "a { margin: 0 -1mm; }\n");
        style("a:step(1,3-4) { b: c; }", "a:step(1,3-4) { b: c; }\n");
        style("a[level=1] { }", "a[level=1] { }\n");
        style("a {}", "a {}\n");
    }

    #[test]
    fn stylesheet_lines() {
        style(
            "\n\npage {\npage-size: A4;   \n\n\n\n    margin: 1mm;  // m\n}\n\n\n",
            "page {\n  page-size: A4;\n\n  margin: 1mm;  // m\n}\n",
        );
        style(
            "render x {\n header: row(\n a,\n  b(\nc),\n);\n}",
            "render x {\n  header: row(\n    a,\n    b(\n      c),\n  );\n}\n",
        );
        style(
            "@variant a {\nq {\nb: c;\n}\n}\r\n",
            "@variant a {\n  q {\n    b: c;\n  }\n}\n",
        );
        style(
            "question\nanswer {\nx: 1;\n}\n",
            "question\n  answer {\n  x: 1;\n}\n",
        );
        style(
            "a {\n  trailer:\n  x y;\n}\n",
            "a {\n  trailer:\n    x y;\n}\n",
        );
    }

    #[test]
    fn markup_whitespace() {
        markup(
            "=   Title  \n\n\n\nText   more.  \n",
            "= Title\n\nText more.\n",
        );
        markup("- a\n  - b\n    c\n", "- a\n  - b\n    c\n");
        markup("Text  // note\n", "Text  // note\n");
        markup("Text // note  \t\n// more \n", "Text // note\n// more\n");
        markup("`a   b`  $x   y$\n", "`a   b` $x   y$\n");
        markup("Done", "Done\n");
        markup("  - a\n  - b\n", "  - a\n  - b\n");
        markup("\n\n  - a\n", "  - a\n");
        markup("", "");
        markup("  \n", "");
        style("", "");
        style("\n", "");
    }

    #[test]
    fn markup_code() {
        markup(
            "#figure(placement:wrap,side : outer)[\n  Body.\n]\n",
            "#figure(placement: wrap, side: outer)[\n  Body.\n]\n",
        );
        markup(
            "#let f(x)=  {\nlet y = (x +\n1)\ny\n}\n",
            "#let f(x) = {\n  let y = (x +\n    1)\n  y\n}\n",
        );
        markup(
            "#let y = {\nlet z = x\n.at(0)\nz\n}\n",
            "#let y = {\n  let z = x\n    .at(0)\n  z\n}\n",
        );
        markup(
            "#question[\n    #answer(\n  lines: 4,\n    )\n  ]\n",
            "#question[\n    #answer(\n      lines: 4,\n    )\n]\n",
        );
        markup("#{x}\n", "#{ x }\n");
        markup("#{}\n#{ }\n#let f() = {}\n", "#{}\n#{ }\n#let f() = {}\n");
        markup("#{\n}\n", "#{\n}\n");
        markup(
            "#{\n  if a {\n    x\n  }\n  else {\n    y\n  }\n}\n",
            "#{\n  if a {\n    x\n  }\n  else {\n    y\n  }\n}\n",
        );
        markup("#f((\n1))\n", "#f((\n  1))\n");
        markup(
            "#{\n[\n  - a\n  - b\n]\n}\n",
            "#{\n  [\n  - a\n  - b\n  ]\n}\n",
        );
    }

    #[test]
    fn errors_leave_the_file() {
        assert_eq!(format_markup("#f(\n"), Err(FormatError::Syntax));
        assert_eq!(format_stylesheet("a { b }"), Err(FormatError::Syntax));
    }

    #[test]
    fn equivalence() {
        let a = parse("Hello  world\n");
        let b = parse("Hello world");
        assert!(equivalent(&a, &b));
        assert!(!equivalent(&parse("Hello world"), &parse("Helloworld")));
        assert!(!equivalent(&parse("a\n\nb"), &parse("a\nb")));
        let a = parse_stylesheet("a { margin: 0 -1mm; }");
        let b = parse_stylesheet("a { margin: 0 - 1mm; }");
        assert!(!equivalent(&a, &b));
    }
}
