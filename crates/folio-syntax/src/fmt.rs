//! Canonical formatting of content (`.fol`) and stylesheets (`.fsty`), and
//! the text edits it is made of (SPEC §12.1).
//!
//! The formatter works on the lossless syntax tree and changes only the
//! whitespace between tokens. Every other byte stays as written: comments,
//! strings, raw text, equations, and the line breaks of prose. The style is
//! decision D-54-1 (SPEC §12.1): stylesheets get a uniform layout, one
//! declaration per line and no column alignment, while content keeps its
//! lines:
//!
//! - Line ends are `\n`, lines carry no trailing whitespace (line comments
//!   included; block comments, strings, raw text and equations keep their
//!   bytes, line ends included), and there is at most one blank line in a
//!   row. A stylesheet starts with its first token and ends with one
//!   newline, unless it ends inside a block comment left open. Content keeps
//!   the kind of whitespace it starts and ends with (none, a space, a
//!   paragraph break), since an included file puts it into the text around
//!   the include; a line break stands for a space, a blank line for a
//!   paragraph break.
//! - Lines of code and stylesheets are indented by two spaces more than the
//!   line that opened the innermost bracket (`{`, `(`, `[`) still open, so
//!   several brackets opened on one line count once. A line that starts by
//!   closing a bracket, in markup too, takes the opener's indentation. The
//!   later lines of an item that runs over several lines (a declaration, an
//!   expression, a selector) are indented two spaces more, except a line
//!   that starts with `else`.
//! - Other lines of markup keep their indentation, which is part of their
//!   meaning (list items nest by it).
//! - In a stylesheet, every declaration, token definition, import and rule
//!   is on a line of its own, a declaration block's `{` ends a line and its
//!   `}` starts one (an empty block is `{}`), and a blank line separates
//!   top-level rules from what is around them, above the comments on the
//!   lines just over a rule. A comment on the line of a token stays there.
//! - Within a line, a run of spaces becomes one space, before comments
//!   too: columns are not aligned. In markup, the spaces before the last
//!   list, enum or term marker of a line and those after the comments that
//!   start a line stay as written: they set a column that the lines below,
//!   whose indentation stays, nest by. In code and stylesheets a space follows
//!   `,` (in the step lists of selectors, `:step(1,3-4)`, it stays as
//!   written), `;` and the `:` of a named argument, declaration, field or
//!   import, surrounds `=` in a binding, `=>`, and the combinators `>` and
//!   `+` of selectors, and separates the braces of a one-line block from
//!   what they hold; none goes before `,`, `;` or the `:` of a named
//!   argument, after `(` or before `)`. Runs of whitespace that hold other
//!   characters than spaces, tabs and line breaks (a form feed, U+2028) are
//!   kept as written.
//! - Table rows (SPEC §7.4) are the exception to unaligned columns. In a
//!   row that is a line of its own, one space separates each pipe from the
//!   cell next to it, and a cell of only spaces is written `||`. The rows
//!   of consecutive lines with the same indentation and number of cells are
//!   aligned: each cell is padded after it to the width of the widest cell
//!   of its column, counting a wide character as two columns (`||` stays
//!   where the whole column is empty). A `|---|` line keeps the spacing the
//!   other rules give it, and does not divide the rows around it. A row
//!   that runs over several lines, through a cell's content block, keeps
//!   its first line as written, and the `]` that closes the block is
//!   indented two spaces more than the row.
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
/// break) and whitespace at the end of a line comment. Whitespace at the
/// start and end of content counts as in markup elsewhere: an included file
/// puts it into the text around the include.
pub fn equivalent(a: &SyntaxNode, b: &SyntaxNode) -> bool {
    let mut left = Vec::new();
    let mut right = Vec::new();
    shape(a, None, &mut left);
    shape(b, None, &mut right);
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
fn shape(node: &SyntaxNode, parent: Option<SyntaxKind>, out: &mut Vec<Step>) {
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
    // In markup, text and the spaces between words are one run: the lexer
    // joins words with a single space into one text, and more spaces into
    // texts with spaces between.
    let mut run: Option<EcoString> = None;
    for child in node.children() {
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
                shape(child, Some(kind), out);
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
    /// Whether the file is content, not a stylesheet.
    markup: bool,
    tokens: Vec<Token>,
    gaps: Vec<Gap>,
    /// The number of brackets open after the tokens so far.
    depth: usize,
    /// For each item being walked, the number of brackets open where it
    /// starts.
    items: Vec<usize>,
    /// For each token, [`Self::opens_markup`].
    opens_markup: Vec<bool>,
    /// For each gap, whether a list, enum or term marker follows it on its
    /// line.
    marker_ahead: Vec<bool>,
    /// The table rows, inner rows before the rows whose cells hold them
    /// (SPEC §7.4).
    rows: Vec<Row>,
    /// For each token, whether it is in a table row.
    row_tokens: Vec<bool>,
}

/// A table row, by the indices of its tokens: its pipes, and each cell's
/// tokens (`start..end`, empty for an empty cell, at the pipe after it).
#[derive(Debug, Clone)]
struct Row {
    tokens: Range<usize>,
    pipes: Vec<usize>,
    cells: Vec<Range<usize>>,
    separator: bool,
}

impl Walk {
    fn new(root: &SyntaxNode) -> Self {
        let mut this = Self {
            text: root.full_text(),
            markup: root.kind() == SyntaxKind::Markup,
            tokens: Vec::new(),
            gaps: vec![Gap::default()],
            depth: 0,
            items: Vec::new(),
            opens_markup: Vec::new(),
            marker_ahead: Vec::new(),
            rows: Vec::new(),
            row_tokens: Vec::new(),
        };
        this.walk(root, root.kind(), 0);
        this.row_tokens = vec![false; this.tokens.len()];
        for row in &this.rows {
            for token in &mut this.row_tokens[row.tokens.clone()] {
                *token = true;
            }
        }
        this.find_markup_lines();
        this.find_markers_ahead();
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
        let first = self.tokens.len();
        let mut row = (kind == SyntaxKind::TableRow).then(|| Row {
            tokens: first..first,
            pipes: Vec::new(),
            cells: Vec::new(),
            separator: node
                .cast::<crate::ast::TableRow<'_>>()
                .is_some_and(|row| row.is_separator()),
        });
        for child in node.children() {
            if items {
                self.items.push(self.depth);
            }
            let before = self.tokens.len();
            self.walk(child, kind, at);
            if let Some(row) = &mut row {
                match child.kind() {
                    SyntaxKind::TablePipe => row.pipes.push(before),
                    SyntaxKind::Markup => row.cells.push(before..self.tokens.len()),
                    _ => {}
                }
            }
            if items {
                self.items.pop();
            }
            at += child.len();
        }
        if let Some(mut row) = row {
            row.tokens = first..self.tokens.len();
            self.rows.push(row);
        }
    }

    /// The line breaks that the layout of a stylesheet asks for in each gap
    /// (SPEC §12.1, D-54-1): every declaration, rule and token definition on
    /// a line of its own, the braces of a declaration block on the lines of
    /// what they hold, and a blank line between top-level rules. Content
    /// asks for none.
    fn breaks(&self) -> Vec<u8> {
        let mut need = vec![0u8; self.gaps.len()];
        if self.markup {
            return need;
        }
        let tokens = &self.tokens;
        let block = |t: &Token, kind| t.kind == kind && is_rule_block(t.parent);
        for (j, token) in tokens.iter().enumerate() {
            let next = tokens.get(j + 1);
            if block(token, SyntaxKind::LeftBrace)
                && !next.is_some_and(|t| block(t, SyntaxKind::RightBrace))
            {
                need[j + 1] = 1;
            }
            if block(token, SyntaxKind::RightBrace) {
                if j > 0 && !block(&tokens[j - 1], SyntaxKind::LeftBrace) {
                    need[j] = 1;
                }
                if next
                    .is_some_and(|t| !matches!(t.kind, SyntaxKind::Semicolon | SyntaxKind::Comma))
                {
                    need[j + 1] = 1;
                }
            }
            if token.kind == SyntaxKind::Semicolon
                && matches!(
                    token.parent,
                    SyntaxKind::Declaration | SyntaxKind::TokenDecl | SyntaxKind::StyleImport
                )
            {
                need[j + 1] = 1;
            }
        }

        // The top-level items, each with whether it holds a block.
        let mut items: Vec<(usize, usize, bool)> = Vec::new();
        let mut current: Option<(usize, bool)> = None;
        let mut depth = 0usize;
        for (j, token) in tokens.iter().enumerate() {
            if depth == 0 && current.is_none() && !token.is_comment() {
                current = Some((j, false));
            }
            if token.opens() {
                if depth == 0
                    && block(token, SyntaxKind::LeftBrace)
                    && let Some((_, has_block)) = &mut current
                {
                    *has_block = true;
                }
                depth += 1;
            } else if token.closes() {
                depth = depth.saturating_sub(1);
            }
            let ends = token.kind == SyntaxKind::Semicolon || block(token, SyntaxKind::RightBrace);
            if depth == 0
                && ends
                && let Some((start, has_block)) = current.take()
            {
                items.push((start, j, has_block));
            }
        }
        if let Some((start, has_block)) = current {
            items.push((start, tokens.len() - 1, has_block));
        }
        // A blank line between two top-level items when either is a rule,
        // before the comments on the lines just above the second.
        for pair in items.windows(2) {
            let ((_, end, a), (start, _, b)) = (pair[0], pair[1]);
            if !(a || b) {
                continue;
            }
            let mut at = start;
            while at > end + 1 {
                let comment = at - 1;
                let gap = |k: usize| {
                    let range = self.gaps[k].range;
                    count_newlines(&self.text[range.0..range.1])
                };
                if tokens[comment].is_comment() && gap(comment) > 0 && gap(at) < 2 {
                    at = comment;
                } else {
                    break;
                }
            }
            need[at] = 2;
        }
        need
    }

    /// Whether the line that starts with token `i` opens a content block
    /// whose markup starts on that line and goes on below it. Moving
    /// anything on such a line would move its list markers against the
    /// markup lines below, whose indentation stays, so the line stays as
    /// written.
    fn opens_markup(&self, i: usize) -> bool {
        self.opens_markup.get(i).copied().unwrap_or(false)
    }

    /// [`Self::opens_markup`] for every token, in one pass from the end of
    /// each line: whether a content block whose markup starts on its line
    /// is opened at or after the token and still open where the line ends.
    fn find_markup_lines(&mut self) {
        let breaks = |gap: &Gap| count_newlines(&self.text[gap.range.0..gap.range.1]) > 0;
        let mut out = vec![false; self.tokens.len()];
        // Closes after the token on its line, not yet matched.
        let mut pending = 0usize;
        let mut found = false;
        for j in (0..self.tokens.len()).rev() {
            if breaks(&self.gaps[j + 1]) {
                pending = 0;
                found = false;
            }
            let token = self.tokens[j];
            if token.opens() {
                if pending > 0 {
                    pending -= 1;
                } else {
                    let inline = !breaks(&self.gaps[j + 1])
                        && self.tokens.get(j + 1).is_some_and(|t| !t.closes());
                    found |= token.parent == SyntaxKind::ContentBlock && inline;
                }
            } else if token.closes() {
                pending += 1;
            }
            out[j] = found;
        }
        self.opens_markup = out;
    }

    /// [`Self::marker_ahead`] for every gap, in one pass from the end.
    fn find_markers_ahead(&mut self) {
        let mut out = vec![false; self.gaps.len()];
        let mut ahead = false;
        for (j, token) in self.tokens.iter().enumerate().rev() {
            let gap = &self.gaps[j + 1];
            if count_newlines(&self.text[gap.range.0..gap.range.1]) > 0 {
                ahead = false;
            }
            ahead |= is_marker(token.kind);
            out[j] = ahead;
        }
        self.marker_ahead = out;
    }

    /// Decides every gap.
    fn run(self) -> Edits {
        let mut edits = Edits::new();
        // The whitespace each gap gets, where it changes.
        let mut decided: Vec<Option<EcoString>> = vec![None; self.gaps.len()];
        // The indentation of the current line, and of the line where its
        // item starts, which a declaration block's `{` indents from.
        let mut indent = EcoString::new();
        let mut item_indent = EcoString::new();
        // The open brackets, each with the indentation of the line that
        // opened it.
        let mut open: Vec<EcoString> = Vec::new();

        // Whether the current line opens markup that goes on below it, and
        // so stays as written.
        let mut frozen = false;
        // Whether every token so far on the current line is a comment.
        let mut comments_only = true;
        // The line breaks each gap must hold: 1 for a line break, 2 for a
        // blank line.
        let mut need = self.breaks();

        for (i, gap) in self.gaps.iter().enumerate() {
            let text = &self.text[gap.range.0..gap.range.1];
            let prev = i.checked_sub(1).map(|i| self.tokens[i]);
            let next = self.tokens.get(i).copied();
            let ascii = text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
            let written = count_newlines(text);
            let mut newlines = written;
            // In a stylesheet, a declaration block's braces hold no blank
            // line next to them, and an empty block is `{}`.
            let brace = |t: Option<Token>, kind| {
                t.is_some_and(|t| t.kind == kind && is_rule_block(t.parent))
            };
            let after_open = brace(prev, SyntaxKind::LeftBrace);
            let before_close = brace(next, SyntaxKind::RightBrace);
            if !self.markup && ascii && (after_open || before_close) {
                newlines = newlines.min(1);
                if after_open && before_close {
                    newlines = 0;
                }
            }
            // A break is not added where it would start a line that opens
            // markup going on below: moving its list markers would change
            // how the items below nest.
            if ascii
                && need[i] > 0
                && prev.is_some()
                && next.is_some()
                && !(newlines == 0 && self.opens_markup(i))
            {
                if newlines == 0 && next.is_some_and(|t| t.is_comment()) {
                    // A comment on the line stays there; the break goes
                    // after it.
                    need[i + 1] = need[i + 1].max(need[i]);
                } else {
                    newlines = newlines.max(usize::from(need[i]));
                }
            }
            if i == 0 || newlines > 0 {
                frozen = next.is_some() && self.opens_markup(i);
                comments_only = true;
            }

            let new: Option<EcoString> = match (prev, next) {
                // Other whitespace than spaces, tabs and line feeds, such as
                // a form feed, is left as it is.
                _ if !ascii => {
                    if newlines > 0 {
                        indent = trailing_indent(text).into();
                        item_indent = indent.clone();
                    }
                    None
                }
                // The end of the file, unless it is inside a block comment
                // left open, where a line break would be part of the comment.
                (Some(prev), None)
                    if prev.kind == SyntaxKind::BlockComment
                        && comment_is_open(&self.text[prev.range.0..prev.range.1]) =>
                {
                    None
                }
                // Content keeps the kind of whitespace it ends with (none,
                // a space, a paragraph break), which an include puts into
                // the text around it; a stylesheet ends with a line break.
                (Some(_), None) if self.markup => Some(edge(text, newlines)),
                (Some(_), None) => Some("\n".into()),
                // The start of the file: markup keeps the indentation of
                // its first line.
                (None, Some(_)) if gap.markup => {
                    indent = after_last_newline(text).unwrap_or(text).into();
                    item_indent = indent.clone();
                    let mut new = match newlines {
                        0 => EcoString::new(),
                        _ => edge(text, newlines),
                    };
                    new.push_str(&indent);
                    Some(new)
                }
                (None, Some(_)) => Some(EcoString::new()),
                (None, None) if self.markup => Some(edge(text, newlines)),
                (None, None) => Some(EcoString::new()),
                (Some(_), Some(next)) if newlines > 0 => {
                    let mut continued = false;
                    let new_indent: EcoString = if frozen || gap.markup && !next.closes() {
                        trailing_indent(text).into()
                    } else if next.closes() && !open.is_empty() {
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
                            // A declaration block's `{` is no continuation.
                            if gap.continues == Some(open.len())
                                && next.kind != SyntaxKind::Else
                                && !brace(Some(next), SyntaxKind::LeftBrace)
                            {
                                continued = true;
                                base.push_str("  ");
                            }
                        }
                        base
                    };
                    indent = new_indent.clone();
                    if !continued {
                        item_indent = new_indent.clone();
                    }
                    let mut new = EcoString::from(if newlines > 1 { "\n\n" } else { "\n" });
                    new.push_str(&new_indent);
                    Some(new)
                }
                // Tokens that touch in markup stay touching: a space there
                // would be a space in the text. The inside of a content
                // block is markup, even where code ends at its `]`.
                (Some(prev), Some(next))
                    if text.is_empty()
                        && (is_markup(prev.parent)
                            || is_markup(next.parent)
                            || prev.kind == SyntaxKind::LeftBracket
                                && prev.parent == SyntaxKind::ContentBlock
                            || next.kind == SyntaxKind::RightBracket
                                && next.parent == SyntaxKind::ContentBlock) =>
                {
                    None
                }
                // A line that opens markup going on below it keeps its
                // spacing: moving its list markers would change how the
                // items below nest.
                (Some(_), Some(_)) if frozen => None,
                // Spaces that set the column of a marker later on the line,
                // or of what follows a comment that starts a line, stay as
                // written: the lines below nest by that column, and their
                // indentation stays.
                (Some(_), Some(_)) if gap.markup && (comments_only || self.marker_ahead[i]) => None,
                (Some(_), Some(_)) if gap.markup => Some(inline_markup(text)),
                (Some(prev), Some(next)) => Some(inline_code(text, prev, next)),
            };

            decided[i] = new;

            // The brackets the next token opens or closes.
            if let Some(next) = next {
                comments_only &= next.is_comment();
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
                    let rule = next.kind == SyntaxKind::LeftBrace && is_rule_block(next.parent);
                    open.push(if rule {
                        item_indent.clone()
                    } else if next.parent == SyntaxKind::ContentBlock && self.in_row(i) {
                        // A cell's content block in a table row closes
                        // indented under the row.
                        let mut cell = indent.clone();
                        cell.push_str("  ");
                        cell
                    } else {
                        indent.clone()
                    });
                } else if next.closes() {
                    open.pop();
                }
                // A token over several lines, such as a string or a block
                // comment, ends on a line with the indentation it gives.
                let token = &self.text[next.range.0..next.range.1];
                if let Some(last) = after_last_newline(token) {
                    indent = last[..last.len() - last.trim_start_matches([' ', '\t']).len()].into();
                }
            }
        }
        self.align_rows(&mut decided);
        for (gap, new) in self.gaps.iter().zip(decided) {
            if let Some(new) = new
                && new != self.text[gap.range.0..gap.range.1]
            {
                edits.replace(gap.range.0..gap.range.1, new);
            }
        }
        edits
    }

    /// Whether token `i` is in a table row.
    fn in_row(&self, i: usize) -> bool {
        self.row_tokens.get(i).copied().unwrap_or(false)
    }

    /// The whitespace written or decided for gap `i`.
    fn gap_text<'a>(&'a self, decided: &'a [Option<EcoString>], i: usize) -> &'a str {
        match &decided[i] {
            Some(new) => new,
            None => &self.text[self.gaps[i].range.0..self.gaps[i].range.1],
        }
    }

    /// Lays out the table rows of content (SPEC §7.4, §12.1): in a row that
    /// is a line of its own, one space separates each pipe from the cell
    /// next to it, and a cell of only spaces is written `||`; the rows of
    /// consecutive lines with the same indentation and the same number of
    /// cells are aligned, each cell padded with spaces after it to the
    /// width of the widest cell of its column (`||` stays where the whole
    /// column is empty). A separator line (`|---|`) is left as the other
    /// rules lay it out, and does not divide the rows around it. A row that
    /// runs over several lines, through a cell's content block, stays as
    /// written: moving its first line would move the list markers on it
    /// against the lines below.
    fn align_rows(&self, decided: &mut [Option<EcoString>]) {
        let breaks = |text: &str| count_newlines(text);
        // The rows that are lines of their own, with the width of each
        // cell and the row's indentation.
        let mut lines: Vec<Option<(Vec<usize>, EcoString)>> = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            lines.push(self.row_line(row, decided));
        }
        // Groups of rows to align: consecutive lines, separators skipped.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut last: Option<usize> = None;
        for (r, row) in self.rows.iter().enumerate() {
            let consecutive = last.is_some_and(|l| {
                let prev = &self.rows[l];
                prev.tokens.end == row.tokens.start
                    && breaks(self.gap_text(decided, row.tokens.start)) == 1
            });
            if row.separator && lines[r].is_some() {
                // A separator does not divide the rows around it.
                if !consecutive {
                    groups.push(Vec::new());
                }
                last = Some(r);
                continue;
            }
            let Some((widths, indent)) = &lines[r] else {
                last = None;
                groups.push(Vec::new());
                continue;
            };
            let joins = consecutive
                && groups.last().and_then(|g| g.last()).is_some_and(|&p| {
                    let (w, i) = lines[p].as_ref().expect("grouped rows are lines");
                    w.len() == widths.len() && i == indent
                });
            if joins {
                groups.last_mut().expect("checked above").push(r);
            } else {
                groups.push(vec![r]);
            }
            last = Some(r);
        }
        for group in groups {
            let mut columns: Vec<usize> = Vec::new();
            for &r in &group {
                let (widths, _) = lines[r].as_ref().expect("grouped rows are lines");
                columns.resize(columns.len().max(widths.len()), 0);
                for (k, &w) in widths.iter().enumerate() {
                    columns[k] = columns[k].max(w);
                }
            }
            for &r in &group {
                let row = &self.rows[r];
                let (widths, _) = lines[r].as_ref().expect("grouped rows are lines");
                for (k, cell) in row.cells.iter().enumerate() {
                    let pad = columns[k] - widths[k];
                    if cell.is_empty() {
                        // An empty cell: the gap between its pipes.
                        decided[cell.start] = Some(if columns[k] == 0 {
                            EcoString::new()
                        } else {
                            " ".repeat(columns[k] + 2).into()
                        });
                        continue;
                    }
                    decided[cell.start] = Some(" ".into());
                    // The gap before the pipe after the cell, if one closes
                    // it.
                    if row.pipes.contains(&cell.end) {
                        let mut after = EcoString::from(" ");
                        after.push_str(&" ".repeat(pad));
                        decided[cell.end] = Some(after);
                    }
                }
            }
        }
    }

    /// The width of each cell of a row that is a line of its own, as the
    /// row is formatted, and the line's indentation; `None` for another
    /// row, or one with a comment.
    fn row_line(&self, row: &Row, decided: &[Option<EcoString>]) -> Option<(Vec<usize>, EcoString)> {
        let (first, end) = (row.tokens.start, row.tokens.end);
        if first == end {
            return None;
        }
        let before = self.gap_text(decided, first);
        let starts_line = first == 0 || count_newlines(before) > 0;
        let span = &self.text[self.tokens[first].range.0..self.tokens[end - 1].range.1];
        if !starts_line
            || count_newlines(span) > 0
            || self.tokens[first..end].iter().any(Token::is_comment)
        {
            return None;
        }
        let mut widths = Vec::with_capacity(row.cells.len());
        for cell in &row.cells {
            let mut width = 0;
            for t in cell.clone() {
                if t > cell.start {
                    width += text_width(self.gap_text(decided, t));
                }
                let token = &self.tokens[t];
                width += text_width(&self.text[token.range.0..token.range.1]);
            }
            widths.push(width);
        }
        Some((widths, trailing_indent(before).into()))
    }
}

/// How many columns a text takes in a monospaced editor: a column for each
/// grapheme cluster, two for a wide one (East Asian wide and full-width
/// characters, and emoji).
fn text_width(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true)
        .map(|g| {
            let c = g.chars().next().map_or(0, u32::from);
            let wide = matches!(c,
                0x1100..=0x115F
                | 0x2E80..=0x303E
                | 0x3041..=0x33FF
                | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xA000..=0xA4CF
                | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F
                | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6
                | 0x1F300..=0x1F64F
                | 0x1F900..=0x1F9FF
                | 0x20000..=0x3FFFD);
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// The markers of list, enum and term items.
fn is_marker(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::ListMarker | SyntaxKind::EnumMarker | SyntaxKind::TermMarker
    )
}

/// The whitespace between two tokens of markup on one line.
fn inline_markup(text: &str) -> EcoString {
    if text.is_empty() {
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

    // Around comments, brackets and braces space as around other tokens;
    // columns are not aligned (D-54-1).
    if next.is_comment() && prev.kind == K::LeftParen && prev.opens()
        || prev.is_comment() && next.kind == K::RightParen && next.closes()
    {
        return "".into();
    }
    if prev.is_comment() && next.kind == K::RightBrace && is_spaced_block(next.parent) {
        return " ".into();
    }
    if next.is_comment() {
        return " ".into();
    }

    let separated = matches!(prev.parent, K::StepSpec | K::PseudoArgs | K::StepRange);
    let named_colon = |t: Token| {
        t.kind == K::Colon
            && matches!(
                t.parent,
                K::Named
                    | K::Keyed
                    | K::ModuleImport
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

    if prev.is_comment() {
        // A space separates a block comment from what follows it.
        return if matches!(next.kind, K::Comma | K::Semicolon) || named_colon(next) {
            "".into()
        } else {
            " ".into()
        };
    }

    // No space.
    if prev.kind == K::LeftBrace
        && is_rule_block(prev.parent)
        && next.kind == K::RightBrace
        && is_rule_block(next.parent)
        || next.kind == K::Comma
        || next.kind == K::Semicolon && !separated
        || named_colon(next)
        || prev.kind == K::LeftParen && prev.opens()
        || next.kind == K::RightParen && next.closes()
    {
        return "".into();
    }

    // One space.
    if matches!(prev.kind, K::Comma | K::Semicolon) && !separated && !next.closes()
        || prev.kind == K::RightBrace && is_rule_block(prev.parent) && !next.closes()
        || matches!(prev.kind, K::RightBrace | K::RightBracket) && next.kind == K::Else
        || prev.kind == K::Else
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

/// Whitespace at the start or end of content, of the same kind as `text`
/// with `newlines` line breaks: none, a space (a line break, or one space
/// on the same line), or a paragraph break (a blank line).
fn edge(text: &str, newlines: usize) -> EcoString {
    match newlines {
        0 if text.is_empty() => EcoString::new(),
        0 => " ".into(),
        1 => "\n".into(),
        _ => "\n\n".into(),
    }
}

/// Whether a block comment is left open at the end of the file. Block
/// comments nest, as the lexer reads them.
fn comment_is_open(comment: &str) -> bool {
    let bytes = comment.as_bytes();
    let (mut depth, mut i) = (0usize, 0);
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'/', Some(b'*')) => {
                depth += 1;
                i += 2;
            }
            (b'*', Some(b'/')) => {
                depth = depth.saturating_sub(1);
                i += 2;
            }
            _ => i += 1,
        }
    }
    depth > 0
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
    after_last_newline(text).unwrap_or("")
}

/// The text after the last line break of `text`, if it has one.
fn after_last_newline(text: &str) -> Option<&str> {
    let at = text.rfind(crate::is_newline)?;
    let c = text[at..].chars().next().expect("found above");
    Some(&text[at + c.len_utf8()..])
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
        style("a{b:c;d :e}", "a {\n  b: c;\n  d: e\n}\n");
        style("a>b+c   d,e{x:1 ;}", "a > b + c d, e {\n  x: 1;\n}\n");
        style("$accent:#1f5fa8;", "$accent: #1f5fa8;\n");
        style("a { margin: 0 -1mm; }", "a {\n  margin: 0 -1mm;\n}\n");
        style("a:step(1,3-4) { b: c; }", "a:step(1,3-4) {\n  b: c;\n}\n");
        style("a:step( 1 , 2 ) { }", "a:step(1, 2) {}\n");
        style("a[level=1] { }", "a[level=1] {}\n");
        style("a {}", "a {}\n");
        style("a {\n}\nb {\n\n}", "a {}\n\nb {}\n");
        style("a {\n\n  b: c;\n\n\n}", "a {\n  b: c;\n}\n");
        style("a\n{ b: c; }", "a\n{\n  b: c;\n}\n");
        style("a,\nb { c: d; }", "a,\n  b {\n  c: d;\n}\n");
        // A line that opens markup going on below stays as written.
        style(
            "a { b: c; d: [- x\n  - y]; }",
            "a { b: c; d: [- x\n  - y];\n}\n",
        );
    }

    #[test]
    fn stylesheet_layout() {
        // One declaration or definition per line.
        style("$a: 1; $b: 2;", "$a: 1;\n$b: 2;\n");
        style(
            "@import \"a.fsty\"; $a: 1;",
            "@import \"a.fsty\";\n$a: 1;\n",
        );
        // A blank line between top-level rules, above their comments.
        style(
            "$a: 1;\na { b: c; }\nd { e: f; }",
            "$a: 1;\n\na {\n  b: c;\n}\n\nd {\n  e: f;\n}\n",
        );
        style("a {}\n// c\n// d\nb {}", "a {}\n\n// c\n// d\nb {}\n");
        style("a {}   // t\nb {}", "a {} // t\n\nb {}\n");
        // Nested rules: one per line, no blank line forced.
        style(
            "@target pdf { a { b: c; } d { e: f; } }",
            "@target pdf {\n  a {\n    b: c;\n  }\n  d {\n    e: f;\n  }\n}\n",
        );
        // No column alignment.
        style(
            "mark   { a: b; }     // m\nanswer { c: d; }",
            "mark {\n  a: b;\n} // m\n\nanswer {\n  c: d;\n}\n",
        );
        // A comment after `{` stays on its line.
        style(
            "render x {   // default\n  body: slot(s);\n}\n",
            "render x { // default\n  body: slot(s);\n}\n",
        );
        // Template blocks are values, not declaration blocks.
        style(
            "a { t: if x { \": \" y } else { }; }",
            "a {\n  t: if x { \": \" y } else { };\n}\n",
        );
    }

    #[test]
    fn stylesheet_lines() {
        style(
            "\n\npage {\npage-size: A4;   \n\n\n\n    margin: 1mm;  // m\n}\n\n\n",
            "page {\n  page-size: A4;\n\n  margin: 1mm; // m\n}\n",
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
        style("a{b:c}b{c:d}", "a {\n  b: c\n}\n\nb {\n  c: d\n}\n");
        style(
            "a {\n  x: 1\n    + f(\n2\n);\n}",
            "a {\n  x: 1\n    + f(\n      2\n    );\n}\n",
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
        markup("Text  // note\n", "Text // note\n");
        markup("Text // note  \t\n// more \n", "Text // note\n// more\n");
        markup("`a   b`  $x   y$\n", "`a   b` $x   y$\n");
        markup("Done", "Done");
        markup("Done \t  ", "Done ");
        markup("Done\n\n\n", "Done\n\n");
        markup("  - a\n  - b\n", "  - a\n  - b\n");
        markup("\n\n  - a\n", "\n\n  - a\n");
        markup("\n  \nText\n", "\n\nText\n");
        markup("\n  Text\n", "\n  Text\n");
        markup("", "");
        markup("  \n", "\n");
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
        markup("#import \"a.fol\"  :x\n", "#import \"a.fol\": x\n");
        // An import without items that ends at a content block's `]`: a
        // space after its colon would be text.
        for touching in ["#[#import\"\":][]", "#[#import\"a\":]\n"] {
            markup(touching, touching);
        }
        markup("#if a [b]else[c]\n", "#if a [b] else [c]\n");
        markup("= Hi  \n/* the rest", "= Hi\n/* the rest");
        style("a { b: c; } /* x", "a {\n  b: c;\n} /* x");
        style("a { } /* x /* y */", "a {} /* x /* y */");
        style("a { } /*/", "a {} /*/");
        style("a { } /* x /* y */ */", "a {} /* x /* y */ */\n");
        style("a { b/* c */: c; }", "a {\n  b /* c */: c;\n}\n");
        markup("#{\n[- x\n  - y]\n}\n", "#{\n[- x\n  - y]\n}\n");
        for frozen in [
            "#let x =  [- a\n           - b]\n",
            "#let x = ( [- a\n            - b])\n",
            "Text    #f[- a\n           - b]\n",
            "#{\nlet x = [- a\n    ] + [- b\n      - c]\n}\n",
        ] {
            markup(frozen, frozen);
        }
        markup("#f(\n[+ x\n  + y])\n", "#f(\n[+ x\n  + y])\n");
        markup(
            "#{\nif a {b}else{c}\n}\n",
            "#{\n  if a { b } else { c }\n}\n",
        );
        style("a {   // x\n}\n", "a { // x\n}\n");
        markup("#f(   // x\n  1)\n", "#f(// x\n  1)\n");
        markup("#f(/* c */ a)\n", "#f(/* c */ a)\n");
        markup("#f(// c\n  a)\n", "#f(// c\n  a)\n");
        style("a{/* c */b:c}", "a { /* c */\n  b: c\n}\n");
        style("a{b:c/* c */}", "a {\n  b: c /* c */\n}\n");
        markup("#let s = \"a\u{2028}b\"\n", "#let s = \"a\u{2028}b\"\n");
        markup("/* a\u{85}b */\n", "/* a\u{85}b */\n");
        style(
            "a { b: \"x\u{2029}y\"; }\n",
            "a {\n  b: \"x\u{2029}y\";\n}\n",
        );
        for touching in [
            "#x// c\n",
            "#f(x)// c\n",
            "#let x = 1// c\n",
            "Text/* c */\n",
            "a/* c */b\n",
            "#x;b\n",
            "#let x = 1;more\n",
        ] {
            markup(touching, touching);
        }
        markup(
            "#{\n  if a {\n    x\n  }\n  else {\n    y\n  }\n}\n",
            "#{\n  if a {\n    x\n  }\n  else {\n    y\n  }\n}\n",
        );
        markup("#f((\n1))\n", "#f((\n  1))\n");
        markup(
            "#{\n  let x = \"a\n        b\" + f(\n  1)\n}\n",
            "#{\n  let x = \"a\n        b\" + f(\n          1)\n}\n",
        );
        markup(
            "#{\nlet x = a\n.map(y => {\ny\n})\n}\n",
            "#{\n  let x = a\n    .map(y => {\n      y\n    })\n}\n",
        );
        markup(
            "#{\n[\n  - a\n  - b\n]\n}\n",
            "#{\n  [\n  - a\n  - b\n  ]\n}\n",
        );
    }

    #[test]
    fn markup_marker_columns() {
        // The spaces before a marker on its line set the marker's column,
        // which the lines below nest by.
        for minimal in [
            "-  -\n   a",
            "+  / :\n   e",
            "-  1.\n   b",
            "/**/  +\n/* */ __**",
            "#[ ][-\n/**/  d]",
        ] {
            markup(minimal, minimal);
        }
        markup(
            "1.  - Award one mark.\n    Accept any rearrangement.\n2.  Second point.\n",
            "1.  - Award one mark.\n    Accept any rearrangement.\n2. Second point.\n",
        );
        // A comment between markers, or after a term's colon.
        for minimal in [
            "-   /**/ - a\n        c\n",
            "/ T:  /**/ - a\n           b\n",
        ] {
            markup(minimal, minimal);
        }
        // Spaces after the last marker of a line still collapse.
        markup("-   a\n    b\n", "- a\n    b\n");
        markup("/ Term:   x\n", "/ Term: x\n");
    }

    #[test]
    fn table_rows() {
        // Consecutive rows are aligned; `||` stays an empty column.
        markup(
            "#grid[\n  |a|bb|\n  | ccc |d|\n  |e   ||\n]\n",
            "#grid[\n  | a   | bb |\n  | ccc | d  |\n  | e   |    |\n]\n",
        );
        markup("| a || b |\n", "| a || b |\n");
        markup("|   a   |  |b|\n", "| a || b |\n");
        // A separator does not divide the rows, and stays as written.
        markup(
            "| Head | X |\n|---|:-:|\n| 1 | 22 |\n",
            "| Head | X  |\n|---|:-:|\n| 1    | 22 |\n",
        );
        // Rows with other indentation or another number of cells, or after
        // a blank line, are aligned apart.
        markup(
            "| aaa | b |\n  | c | d |\n| e |\n\n| f | g |\n",
            "| aaa | b |\n  | c | d |\n| e |\n\n| f | g |\n",
        );
        // Wide characters take two columns, and markup and code in cells
        // count as formatted.
        markup(
            "| 表 | x |\n| ab | y |\n",
            "| 表 | x |\n| ab | y |\n",
        );
        markup(
            "| #f(a,b) | x |\n| $y$   z | w |\n",
            "| #f(a, b) | x |\n| $y$ z    | w |\n",
        );
        // A cell after the last pipe, and a row that is not a line of its
        // own.
        markup("| a |  b\n| cc | d\n", "| a  | b\n| cc | d\n");
        markup("#grid[| a |  b |]\n", "#grid[| a | b |]\n");
        // A row whose cell holds a block over several lines stays as
        // written, and divides the rows around it.
        // Its block closes indented under the row.
        markup(
            "#grid[\n  | aaa | b |\n  | c | #cell[\n      Two.\n] | 2 |\n  | d |  e |\n]\n",
            "#grid[\n  | aaa | b |\n  | c | #cell[\n      Two.\n    ] | 2 |\n  | d | e |\n]\n",
        );
        markup(
            "| a |  #cell[- x\n    - y] |\n",
            "| a |  #cell[- x\n    - y] |\n",
        );
    }

    #[test]
    fn errors_leave_the_file() {
        assert_eq!(format_markup("#f(\n"), Err(FormatError::Syntax));
        assert_eq!(format_stylesheet("a { b }"), Err(FormatError::Syntax));
    }

    #[test]
    fn equivalence() {
        assert!(equivalent(
            &parse("Hello  world\n"),
            &parse("Hello world\n")
        ));
        // An included file puts the whitespace at its ends into the text.
        assert!(!equivalent(&parse("Hello world\n"), &parse("Hello world")));
        assert!(!equivalent(&parse("x\n"), &parse("x\n\n")));
        assert!(!equivalent(&parse("Hello world"), &parse("Helloworld")));
        assert!(!equivalent(&parse("a\n\nb"), &parse("a\nb")));
        let a = parse_stylesheet("a { margin: 0 -1mm; }");
        let b = parse_stylesheet("a { margin: 0 - 1mm; }");
        assert!(!equivalent(&a, &b));
    }
}
