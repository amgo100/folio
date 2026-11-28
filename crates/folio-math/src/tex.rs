//! LaTeX input (SPEC §8.1): reading the math of `$:tex …$` into the same
//! tree that native math evaluates to.
//!
//! The reader covers the amsmath core that teaching material uses:
//! fractions, roots, scripts and primes, matrices, `cases`, the alignment
//! environments, `\left … \right` and the sizing commands, accents,
//! named operators, math alphabets, `\text` and the symbols of
//! [`SYMBOLS`]. It follows TeX where TeX and native math differ in
//! meaning: spaces separate nothing, `x^10` raises only the `1`, a group
//! `{a + b}` is one base for a script, and `/` is a slash, not a fraction.
//! Where they agree, it builds what native math builds, so that a pasted
//! formula and its native equivalent have the same tree:
//!
//! - Brackets pair as in native math: an opening one (`(`, `[`, `\{`,
//!   `\langle`, `\lfloor`, …) and the first closing one after it at the
//!   same level make a [`Node::Delimited`] whose delimiters grow with their
//!   body; one without a partner is a plain symbol. `\left( … \right)`
//!   pairs explicitly, and `\left.` leaves a side open.
//! - `-` is the minus sign `−` and `*` the asterisk operator `∗`, as in
//!   native math; the tie `~` is a space, as `\ ` is.
//! - An upright d (`\mathrm{d}`, `\operatorname{d}`, `\dd`) is the
//!   differential, as `dif` is, and the thin space `\,` before it is part
//!   of it.
//! - `\vec` is a vector, as `vec` is; the accents (`\hat`, `\bar`, `\dot`,
//!   `\tilde`, …) are accents.
//! - The top level of the formula, or of an alignment environment that
//!   holds the whole formula (`align`, `aligned`, `gather`, `equation`,
//!   `split`, …), is split into lines at `\\`; `&` marks alignment points
//!   there and in the cases of `cases`; and `\label{name}` names its line,
//!   as `<name>` does in native math.
//!
//! Some commands lay out what the tree leaves to layout or the stylesheet,
//! and are read without a node: `\displaystyle` and the other style
//! switches, `\limits` and `\nolimits`, the negative spaces (`\!`), and
//! the sizes of `\big(` and its kin, which delimit as their delimiter
//! alone does. Anything else that is not in the tables is an error at the
//! command, so a formula pasted from an older document says which of its
//! commands to rewrite.

use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, eco_format};
use folio_syntax::Span;

use crate::{
    Accent, Attach, Cases, ColumnAlign, Decoration, DecorationKind, Delimited, Formula, Frac,
    Label, Line, MathClass, MathVariant, Matrix, Node, Operator, Root, Row, Styled, Vector,
};

/// An error in LaTeX input.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TexError {
    /// The bytes of the input it is about.
    pub range: Range<usize>,
    /// What is wrong.
    pub message: EcoString,
    /// How to fix it.
    pub hints: Vec<EcoString>,
    /// Whether the formula nests more deeply than the reader allows, rather
    /// than being wrong.
    pub too_deep: bool,
}

impl TexError {
    fn new(range: Range<usize>, message: impl Into<EcoString>) -> Self {
        Self {
            range,
            message: message.into(),
            hints: Vec::new(),
            too_deep: false,
        }
    }

    fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }
}

type TexResult<T> = Result<T, TexError>;

/// Reads LaTeX math into a formula. Every node gets `span`, the span of the
/// whole input; errors point into `src` by byte range. Groups, arguments
/// and environments may nest at most `max_depth` deep.
pub fn parse(src: &str, span: Span, max_depth: u32) -> TexResult<Formula> {
    let mut parser = Parser {
        src,
        pos: 0,
        span,
        depth: 0,
        max_depth,
        label: None,
    };
    let lines = parser.formula()?;
    Ok(Formula::new(lines))
}

/// A token of LaTeX input.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Tok<'s> {
    /// The end of the input.
    Eof,
    /// `{`.
    Open,
    /// `}`.
    Close,
    /// `^`.
    Sup,
    /// `_`.
    Sub,
    /// `'`, `''`, …: a number of primes.
    Primes(usize),
    /// `&`.
    Amp,
    /// `#`, which is special in TeX.
    Hash,
    /// A control sequence, without its backslash: a word of letters, or one
    /// other character (`\,`, `\{`, `\\`).
    Cmd(&'s str),
    /// Any other character.
    Char(char),
}

/// Where a row stands, which decides what it may hold and what ends it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Level {
    /// The top level of a line of the formula: may hold alignment points
    /// and a label; ends at `\\`.
    Line,
    /// The top level of a case of `cases`: may hold alignment points; ends
    /// at `\\`.
    Case,
    /// A cell of a matrix: ends at `&` and `\\`.
    Cell,
    /// Anywhere else.
    Nested,
}

/// What ends a row besides the end of the input, `}`, `\right` and `\end`.
#[derive(Debug, Copy, Clone)]
struct Ctx {
    level: Level,
    /// Whether a closing bracket ends it: the body of an opening bracket.
    closer: bool,
    /// Whether `]` ends it: the optional argument of `\sqrt`.
    bracket: bool,
}

impl Ctx {
    fn new(level: Level) -> Self {
        Self {
            level,
            closer: false,
            bracket: false,
        }
    }
}

/// A part of a row under construction: a node, or a group `{..}` that is
/// one base for a script and is spliced into the row otherwise.
enum Item {
    Node(Node),
    Group(Row),
}

/// A row under construction, with the first alignment point at its top
/// level, which a bracket pair around it makes an error.
#[derive(Default)]
struct Items {
    items: Vec<Item>,
    align: Option<Range<usize>>,
}

impl Items {
    fn push(&mut self, node: Node) {
        // Scripts on an empty base before a nucleus are its pre-scripts:
        // `{}^{14}_{6}\mathrm{C}`.
        if matches!(
            node,
            Node::Ident(_) | Node::Number(_) | Node::Text(_) | Node::Styled(_)
        ) && let Some(Item::Node(Node::Attach(attach))) = self.items.last()
            && attach.base.nodes().is_empty()
            && attach.primes == 0
            && attach.pre_sub.is_none()
            && attach.pre_sup.is_none()
        {
            let attach = Attach {
                base: Row::new([node]),
                sub: None,
                sup: None,
                primes: 0,
                pre_sub: attach.sub.clone(),
                pre_sup: attach.sup.clone(),
                span: attach.span,
            };
            *self.items.last_mut().expect("an item") = Item::Node(Node::Attach(Arc::new(attach)));
            return;
        }
        self.items.push(Item::Node(node));
    }

    fn into_nodes(self) -> Vec<Node> {
        let mut nodes = Vec::with_capacity(self.items.len());
        for item in self.items {
            match item {
                Item::Node(node) => nodes.push(node),
                Item::Group(row) => nodes.extend(row.nodes().iter().cloned()),
            }
        }
        nodes
    }
}

struct Parser<'s> {
    src: &'s str,
    pos: usize,
    span: Span,
    depth: u32,
    max_depth: u32,
    /// The label of the line being read, with where it was written.
    label: Option<(EcoString, Range<usize>)>,
}

/// Lexing.
impl<'s> Parser<'s> {
    /// Skips spaces and comments from `at`, returning where the next token
    /// starts.
    fn skip_trivia(&self, mut at: usize) -> usize {
        let bytes = self.src.as_bytes();
        while at < bytes.len() {
            let c = self.src[at..].chars().next().expect("not at the end");
            if c.is_whitespace() {
                at += c.len_utf8();
            } else if c == '%' {
                at = self.src[at..]
                    .find(['\n', '\r'])
                    .map_or(self.src.len(), |end| at + end);
            } else {
                break;
            }
        }
        at
    }

    /// The token at `at`, after trivia, and its range.
    fn token_at(&self, at: usize) -> (Tok<'s>, Range<usize>) {
        let start = self.skip_trivia(at);
        let rest = &self.src[start..];
        let Some(c) = rest.chars().next() else {
            return (Tok::Eof, start..start);
        };
        let one = start..start + c.len_utf8();
        let tok = match c {
            '{' => Tok::Open,
            '}' => Tok::Close,
            '^' => Tok::Sup,
            '_' => Tok::Sub,
            '&' => Tok::Amp,
            '#' => Tok::Hash,
            '\'' => {
                let count = rest.chars().take_while(|&c| c == '\'').count();
                return (Tok::Primes(count), start..start + count);
            }
            '\\' => {
                let after = &rest[1..];
                let letters = after
                    .char_indices()
                    .find(|(_, c)| !c.is_ascii_alphabetic())
                    .map_or(after.len(), |(i, _)| i);
                let len = match after.chars().next() {
                    _ if letters > 0 => letters,
                    // A backslash before any space, a line break or a tab
                    // among them, is a control space.
                    Some(c) if c.is_whitespace() => {
                        return (Tok::Cmd(" "), start..start + 1 + c.len_utf8());
                    }
                    c => c.map_or(0, char::len_utf8),
                };
                return (Tok::Cmd(&after[..len]), start..start + 1 + len);
            }
            c => Tok::Char(c),
        };
        (tok, one)
    }

    fn peek(&self) -> (Tok<'s>, Range<usize>) {
        self.token_at(self.pos)
    }

    fn next(&mut self) -> (Tok<'s>, Range<usize>) {
        let (tok, range) = self.peek();
        self.pos = range.end;
        (tok, range)
    }

    /// Eats `c` if it is the next character after trivia.
    fn eat_char(&mut self, c: char) -> Option<Range<usize>> {
        let (tok, range) = self.peek();
        (tok == Tok::Char(c)).then(|| {
            self.pos = range.end;
            range
        })
    }

    /// Eats `c` if it directly follows, without trivia between.
    fn eat_directly(&mut self, c: char) -> bool {
        if self.src[self.pos..].starts_with(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// Reads a braced argument as raw text, with escaped characters
    /// unescaped: what `\text`, `\label` and `\begin` take. `what` names
    /// the argument for errors.
    fn raw_arg(&mut self, cmd: Range<usize>, what: &str) -> TexResult<(EcoString, Range<usize>)> {
        let (tok, open) = self.peek();
        if tok != Tok::Open {
            return Err(TexError::new(
                cmd.clone(),
                eco_format!("`{}` takes {what} in braces", &self.src[cmd]),
            ));
        }
        self.pos = open.end;
        let start = self.pos;
        let mut text = EcoString::new();
        let mut depth = 0usize;
        let mut chars = self.src[start..].char_indices();
        while let Some((i, c)) = chars.next() {
            let at = start + i;
            match c {
                '{' => {
                    depth += 1;
                    continue;
                }
                '}' if depth == 0 => {
                    self.pos = at + 1;
                    return Ok((text, start..at));
                }
                '}' => {
                    depth -= 1;
                    continue;
                }
                '\\' => {
                    let Some((_, next)) = chars.next() else { break };
                    match next {
                        '{' | '}' | '%' | '&' | '_' | '#' | '$' | ' ' => {
                            text.push(next);
                            continue;
                        }
                        ',' => {
                            text.push('\u{2009}');
                            continue;
                        }
                        _ => {
                            let end = self.src[at + 1..]
                                .char_indices()
                                .find(|(_, c)| !c.is_ascii_alphabetic())
                                .map_or(self.src.len(), |(i, _)| at + 1 + i)
                                .max(at + 1 + next.len_utf8());
                            return Err(TexError::new(
                                at..end,
                                eco_format!(
                                    "`{}` cannot stand in {what}",
                                    &self.src[at..end]
                                ),
                            )
                            .with_hint("it holds plain text, with `\\{`, `\\}`, `\\%`, `\\&`, `\\_`, `\\#` and `\\$` for those characters"));
                        }
                    }
                }
                '~' => {
                    text.push('\u{a0}');
                    continue;
                }
                _ => {}
            }
            text.push(c);
        }
        Err(TexError::new(open, "unclosed brace"))
    }
}

/// Lines and environments.
impl<'s> Parser<'s> {
    /// The whole formula: its lines, or the lines of the alignment
    /// environments that hold all of it.
    fn formula(&mut self) -> TexResult<Vec<Line>> {
        let mut envs: Vec<(EcoString, Range<usize>)> = Vec::new();
        loop {
            let (tok, range) = self.peek();
            if tok != Tok::Cmd("begin") {
                break;
            }
            let saved = self.pos;
            self.pos = range.end;
            let (name, name_range) = self.raw_arg(range.clone(), "an environment's name")?;
            if !is_lines_env(&name) {
                self.pos = saved;
                break;
            }
            self.skip_env_args(&name)?;
            envs.push((name, range.start..name_range.end + 1));
        }
        let lines = self.lines(
            envs.last()
                .map(|(name, begin)| (name.as_str(), begin.clone())),
        )?;
        for pair in envs.windows(2).rev() {
            let [(outer, begin), (inner, inner_begin)] = pair else {
                unreachable!("windows of two")
            };
            if self.peek().0 != Tok::Cmd("end") {
                return Err(TexError::new(
                    inner_begin.clone(),
                    eco_format!("`{inner}` can only hold the whole of `{outer}`"),
                )
                .with_hint(
                    "Folio reads one alignment environment, or one inside another that it fills",
                ));
            }
            self.expect_end(outer, begin.clone())?;
        }
        let (tok, range) = self.peek();
        match tok {
            Tok::Eof => Ok(lines),
            _ if !envs.is_empty() => Err(TexError::new(
                range,
                eco_format!("nothing can follow `\\end{{{}}}` in an equation", envs[0].0),
            )
            .with_hint("an alignment environment holds the whole formula")),
            _ => Err(self.stray(tok, range)),
        }
    }

    /// The arguments of an environment that are not its body: the number
    /// of columns of `alignat`.
    fn skip_env_args(&mut self, name: &str) -> TexResult<()> {
        if name.starts_with("alignat") || name.starts_with("alignedat") {
            let (tok, range) = self.peek();
            if tok == Tok::Open {
                let begin = range.clone();
                self.raw_arg(begin, "a number of columns")?;
            }
        }
        Ok(())
    }

    /// Lines up to the end of the input or to `\end{env}`. A line break
    /// at the end makes no empty line, as in native math.
    fn lines(&mut self, env: Option<(&str, Range<usize>)>) -> TexResult<Vec<Line>> {
        let mut lines = Vec::new();
        loop {
            self.label = None;
            let nodes = self.row_items(Ctx::new(Level::Line))?.into_nodes();
            let label = self.label.take().map(|(name, _)| Label {
                name,
                span: self.span,
            });
            let span = self.span;
            let line = |nodes, label| Line {
                row: Row::new(nodes),
                label,
                span,
            };
            let (tok, range) = self.peek();
            match tok {
                Tok::Cmd("\\") => {
                    self.pos = range.end;
                    self.line_break_args();
                    lines.push(line(nodes, label));
                    let (tok, range) = self.peek();
                    match (tok, &env) {
                        (Tok::Eof, None) => return Ok(lines),
                        (Tok::Cmd("end"), Some((env, _))) => {
                            self.expect_end(env, range)?;
                            return Ok(lines);
                        }
                        _ => {}
                    }
                }
                Tok::Eof => {
                    if let Some((env, begin)) = env {
                        return Err(TexError::new(
                            begin,
                            eco_format!("missing `\\end{{{env}}}`"),
                        ));
                    }
                    if !nodes.is_empty() || label.is_some() {
                        lines.push(line(nodes, label));
                    }
                    return Ok(lines);
                }
                Tok::Cmd("end") if let Some((env, _)) = &env => {
                    self.expect_end(env, range)?;
                    if !nodes.is_empty() || label.is_some() {
                        lines.push(line(nodes, label));
                    }
                    return Ok(lines);
                }
                _ => return Err(self.stray(tok, range)),
            }
        }
    }

    /// Skips what may follow `\\`: a star and a space in brackets.
    fn line_break_args(&mut self) {
        self.eat_directly('*');
        if self.src[self.pos..].starts_with('[')
            && let Some(end) = self.src[self.pos..].find(']')
        {
            self.pos += end + 1;
        }
    }

    /// Expects `\end{name}`; `begin` is where the environment began.
    fn expect_end(&mut self, name: &str, begin: Range<usize>) -> TexResult<()> {
        let (tok, range) = self.peek();
        if tok != Tok::Cmd("end") {
            return Err(TexError::new(
                begin,
                eco_format!("missing `\\end{{{name}}}`"),
            ));
        }
        self.pos = range.end;
        let (found, found_range) = self.raw_arg(range.clone(), "an environment's name")?;
        if found != name {
            return Err(TexError::new(
                range.start..found_range.end + 1,
                eco_format!("`\\end{{{found}}}` ends `\\begin{{{name}}}`"),
            ));
        }
        Ok(())
    }

    /// An environment inside a row: a matrix, `cases` or `array`.
    fn environment(&mut self, begin: Range<usize>) -> TexResult<Node> {
        let (name, name_range) = self.raw_arg(begin.clone(), "an environment's name")?;
        let whole = begin.start..name_range.end + 1;
        let delims = match name.as_str() {
            "matrix" | "smallmatrix" | "array" => Some((None, None)),
            "pmatrix" => Some((Some('('), Some(')'))),
            "bmatrix" => Some((Some('['), Some(']'))),
            "Bmatrix" => Some((Some('{'), Some('}'))),
            "vmatrix" => Some((Some('|'), Some('|'))),
            "Vmatrix" => Some((Some('‖'), Some('‖'))),
            _ => None,
        };
        if let Some((open, close)) = delims {
            let align = match name.as_str() {
                "array" => self.array_columns(whole.clone())?,
                _ => Vec::new(),
            };
            return self.matrix(&name, open, close, align, whole);
        }
        match name.as_str() {
            "cases" | "dcases" => self.cases(&name, whole),
            name if is_lines_env(name) => Err(TexError::new(
                whole,
                eco_format!("`{name}` can only hold a whole equation"),
            )
            .with_hint("its lines are the lines of the equation, so nothing can stand around it")),
            name => Err(TexError::new(
                whole,
                eco_format!("unknown LaTeX environment `{name}`"),
            )
            .with_hint("Folio reads the matrices (`matrix`, `pmatrix`, `bmatrix`, …), `cases`, `array` and the alignment environments (`align`, `gather`, …)")),
        }
    }

    /// The column specification of `array`, which may only align columns:
    /// the alignment of each.
    fn array_columns(&mut self, begin: Range<usize>) -> TexResult<Vec<ColumnAlign>> {
        let (spec, range) = self.raw_arg(begin, "its columns")?;
        if let Some((i, c)) = spec
            .char_indices()
            .find(|&(_, c)| !matches!(c, 'l' | 'c' | 'r') && !c.is_whitespace())
        {
            let at = range.start + i;
            return Err(TexError::new(
                at..at + c.len_utf8(),
                eco_format!("`{c}` in the columns of `array` is not supported"),
            )
            .with_hint("the columns of an array can be `l`, `c` or `r`; rules between them are not supported"));
        }
        Ok(spec
            .chars()
            .filter_map(|c| match c {
                'l' => Some(ColumnAlign::Left),
                'c' => Some(ColumnAlign::Center),
                'r' => Some(ColumnAlign::Right),
                _ => None,
            })
            .collect())
    }

    /// The cells of a matrix, up to `\end{name}`. Short rows are filled
    /// with empty cells, as TeX sets them.
    fn matrix(
        &mut self,
        name: &str,
        open: Option<char>,
        close: Option<char>,
        align: Vec<ColumnAlign>,
        begin: Range<usize>,
    ) -> TexResult<Node> {
        let mut rows: Vec<Vec<Row>> = vec![Vec::new()];
        loop {
            let cell = self.row_items(Ctx::new(Level::Cell))?.into_nodes();
            let row = rows.last_mut().expect("there is always a row");
            let (tok, range) = self.next();
            match tok {
                Tok::Amp => row.push(Row::new(cell)),
                Tok::Cmd("\\") => {
                    self.line_break_args();
                    row.push(Row::new(cell));
                    rows.push(Vec::new());
                }
                Tok::Cmd("end") => {
                    if !(cell.is_empty() && row.is_empty()) {
                        row.push(Row::new(cell));
                    }
                    self.pos = range.start;
                    self.expect_end(name, begin.clone())?;
                    break;
                }
                Tok::Eof => {
                    return Err(TexError::new(
                        begin,
                        eco_format!("missing `\\end{{{name}}}`"),
                    ));
                }
                _ => return Err(self.stray(tok, range)),
            }
        }
        if rows.last().is_some_and(Vec::is_empty) {
            rows.pop();
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return Err(TexError::new(begin, eco_format!("`{name}` has no cells")));
        }
        for row in &mut rows {
            row.resize_with(width, Row::default);
        }
        Ok(Node::Matrix(Arc::new(Matrix {
            rows,
            open,
            close,
            align,
            span: self.span,
        })))
    }

    /// The cases of `cases`, up to `\end{name}`.
    fn cases(&mut self, name: &str, begin: Range<usize>) -> TexResult<Node> {
        let mut cases = Vec::new();
        loop {
            let case = self.row_items(Ctx::new(Level::Case))?.into_nodes();
            let (tok, range) = self.next();
            match tok {
                Tok::Cmd("\\") => {
                    self.line_break_args();
                    cases.push(Row::new(case));
                }
                Tok::Cmd("end") => {
                    if !case.is_empty() {
                        cases.push(Row::new(case));
                    }
                    self.pos = range.start;
                    self.expect_end(name, begin.clone())?;
                    break;
                }
                Tok::Eof => {
                    return Err(TexError::new(
                        begin,
                        eco_format!("missing `\\end{{{name}}}`"),
                    ));
                }
                _ => return Err(self.stray(tok, range)),
            }
        }
        if cases.is_empty() {
            return Err(TexError::new(begin, eco_format!("`{name}` has no cases")));
        }
        Ok(Node::Cases(Arc::new(Cases {
            cases,
            span: self.span,
        })))
    }

    /// The error of a token where it cannot stand.
    fn stray(&self, tok: Tok<'s>, range: Range<usize>) -> TexError {
        match tok {
            Tok::Close => TexError::new(range, "unmatched closing brace `}`"),
            Tok::Cmd("right") => TexError::new(range, "`\\right` without `\\left`"),
            Tok::Cmd("end") => TexError::new(range, "`\\end` without `\\begin`"),
            Tok::Cmd("\\") => TexError::new(
                range,
                "a line break can only be at the top level of an equation",
            )
            .with_hint("cases are separated by `\\\\` in `\\begin{cases}`, the rows of a matrix in a matrix environment"),
            Tok::Amp => misplaced_align_point(range),
            Tok::Char(']') => TexError::new(range, "unexpected `]`"),
            Tok::Char(c) if is_closer(c) => TexError::new(range, eco_format!("unexpected `{c}`")),
            _ => TexError::new(range, "unexpected token"),
        }
    }
}

/// Rows.
impl<'s> Parser<'s> {
    /// Enters a nested part, which may nest at most `max_depth` deep.
    fn enter(&mut self, range: &Range<usize>) -> TexResult<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            let mut error = TexError::new(range.clone(), "math is nested too deeply");
            error.too_deep = true;
            return Err(error);
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Reads a row up to what ends it in `ctx`, which it leaves unread.
    fn row_items(&mut self, ctx: Ctx) -> TexResult<Items> {
        let mut items = Items::default();
        loop {
            let (tok, range) = self.peek();
            match tok {
                Tok::Eof | Tok::Close | Tok::Cmd("right" | "end") => break,
                Tok::Char(']') if ctx.bracket => break,
                _ if ctx.closer && self.closer_at(self.pos).is_some() => break,
                Tok::Amp if ctx.level == Level::Cell => break,
                Tok::Cmd("\\") if ctx.level != Level::Nested => break,
                _ => {}
            }
            self.pos = range.end;
            self.token(tok, range, ctx, &mut items)?;
        }
        Ok(items)
    }

    /// Reads what a token starts into `items`.
    fn token(
        &mut self,
        tok: Tok<'s>,
        range: Range<usize>,
        ctx: Ctx,
        items: &mut Items,
    ) -> TexResult<()> {
        let span = self.span;
        match tok {
            Tok::Eof | Tok::Close => unreachable!("rows end there"),
            Tok::Open => {
                let row = self.group(range)?;
                items.items.push(Item::Group(row));
            }
            Tok::Sup | Tok::Sub => self.script(tok == Tok::Sup, range, items)?,
            Tok::Primes(count) => self.primes(count, range, items)?,
            Tok::Amp => match ctx.level {
                Level::Line | Level::Case => {
                    items.align.get_or_insert(range.clone());
                    items.push(Node::AlignPoint(span));
                }
                Level::Cell | Level::Nested => return Err(misplaced_align_point(range)),
            },
            Tok::Hash => {
                return Err(TexError::new(range, "`#` is a special character in LaTeX")
                    .with_hint("write `\\#` for the character"));
            }
            Tok::Char(c) => self.character(c, range, ctx, items)?,
            Tok::Cmd(name) => self.command(name, range, ctx, items)?,
        }
        Ok(())
    }

    /// A group `{..}`, after its opening brace.
    fn group(&mut self, open: Range<usize>) -> TexResult<Row> {
        self.enter(&open)?;
        let items = self.row_items(Ctx::new(Level::Nested))?;
        let (tok, range) = self.next();
        if tok != Tok::Close {
            return Err(match tok {
                Tok::Eof => TexError::new(open, "unclosed brace"),
                tok => self.stray(tok, range),
            });
        }
        self.leave();
        Ok(Row::new(items.into_nodes()))
    }

    /// A character: a number, a letter, a bracket that may pair with a
    /// closing one, or a symbol.
    fn character(
        &mut self,
        c: char,
        range: Range<usize>,
        ctx: Ctx,
        items: &mut Items,
    ) -> TexResult<()> {
        let span = self.span;
        if c.is_numeric() {
            let rest = &self.src[range.start..];
            let mut len = rest
                .char_indices()
                .find(|(_, c)| !c.is_numeric())
                .map_or(rest.len(), |(i, _)| i);
            if rest[len..].starts_with('.') {
                let frac = rest[len + 1..]
                    .char_indices()
                    .find(|(_, c)| !c.is_numeric())
                    .map_or(rest.len() - len - 1, |(i, _)| i);
                if frac > 0 {
                    len += 1 + frac;
                }
            }
            self.pos = range.start + len;
            items.push(Node::number(rest[..len].into(), span));
            return Ok(());
        }
        if is_opener(c) {
            return self.bracket(c, range, ctx, items);
        }
        items.push(self.char_node(c, range));
        Ok(())
    }

    /// The node of a character other than a digit, with the combining
    /// marks that follow it.
    fn char_node(&mut self, c: char, range: Range<usize>) -> Node {
        let rest = &self.src[range.end..];
        let marks = rest
            .char_indices()
            .find(|&(_, c)| !is_combining(c))
            .map_or(rest.len(), |(i, _)| i);
        if marks > 0 {
            self.pos = range.end + marks;
            return Node::grapheme(self.src[range.start..range.end + marks].into(), self.span);
        }
        Node::symbol(
            match c {
                '-' => '−',
                '*' => '∗',
                // A tie is a space, as `\ ` is.
                '~' => '\u{2004}',
                c => c,
            },
            self.span,
        )
    }

    /// An opening bracket: with the first closing bracket after it at the
    /// same level, a delimited body; without one, a symbol before what
    /// follows it.
    fn bracket(
        &mut self,
        open: char,
        range: Range<usize>,
        ctx: Ctx,
        items: &mut Items,
    ) -> TexResult<()> {
        self.enter(&range)?;
        let inner = self.row_items(Ctx {
            closer: true,
            ..ctx
        })?;
        self.leave();
        let ends_index = ctx.bracket && self.peek().0 == Tok::Char(']');
        let closer = if ends_index {
            None
        } else {
            self.closer_at(self.pos)
        };
        let Some((close, close_end)) = closer else {
            // Without a partner: the bracket is a symbol, and what followed
            // it stays in the row.
            items.push(Node::symbol(open, self.span));
            if let Some(align) = inner.align.clone() {
                items.align.get_or_insert(align);
            }
            items.items.extend(inner.items);
            return Ok(());
        };
        if let Some(align) = inner.align {
            return Err(misplaced_align_point(align)
                .with_hint("`&` aligns the lines of an equation, or the parts of a case"));
        }
        self.pos = close_end;
        let body = Row::new(inner.into_nodes());
        items.push(Node::Delimited(Arc::new(Delimited {
            open: Some(open),
            body,
            close: Some(close),
            span: self.span,
        })));
        Ok(())
    }

    /// The closing bracket at `at`, if one is there, and where it ends: a
    /// character, a control sequence such as `\}` or `\rangle`, or either
    /// after a size such as `\bigr`.
    fn closer_at(&self, at: usize) -> Option<(char, usize)> {
        let (tok, range) = self.token_at(at);
        match tok {
            Tok::Char(c) if is_closer(c) => Some((c, range.end)),
            Tok::Cmd(name) => match command(name) {
                Some(Cmd::Symbol(c)) if is_closer(c) => Some((c, range.end)),
                Some(Cmd::Size) => match self.token_at(range.end) {
                    (Tok::Char(c), next) if is_closer(c) => Some((c, next.end)),
                    (Tok::Cmd(name), next) => symbol_of(name)
                        .filter(|&c| is_closer(c))
                        .map(|c| (c, next.end)),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    /// A superscript or subscript, attached to the last item of the row.
    fn script(&mut self, sup: bool, range: Range<usize>, items: &mut Items) -> TexResult<()> {
        let arg = self.arg(range.clone(), if sup { "superscript" } else { "subscript" })?;
        let span = self.span;
        let last = items.items.pop();
        let node = match last {
            Some(Item::Node(Node::Attach(attach))) => {
                let has = if sup { &attach.sup } else { &attach.sub };
                if has.is_some() {
                    return Err(double_script(sup, range));
                }
                let mut attach = (*attach).clone();
                *(if sup {
                    &mut attach.sup
                } else {
                    &mut attach.sub
                }) = Some(arg);
                Node::Attach(Arc::new(attach))
            }
            Some(Item::Node(Node::Nary(nary))) if nary.sub.is_some() || nary.sup.is_some() => {
                let has = if sup { &nary.sup } else { &nary.sub };
                if has.is_some() {
                    return Err(double_script(sup, range));
                }
                let mut nary = (*nary).clone();
                *(if sup { &mut nary.sup } else { &mut nary.sub }) = Some(arg);
                Node::Nary(Arc::new(nary))
            }
            last => {
                let base = match last {
                    None => Row::default(),
                    Some(Item::Node(node)) => Row::new([node]),
                    Some(Item::Group(row)) => row,
                };
                let (sub, sup) = if sup {
                    (None, Some(arg))
                } else {
                    (Some(arg), None)
                };
                Node::attach(base, sub, sup, 0, span)
            }
        };
        items.push(node);
        Ok(())
    }

    /// Primes, attached to the last item of the row before its
    /// superscript.
    fn primes(&mut self, count: usize, range: Range<usize>, items: &mut Items) -> TexResult<()> {
        let span = self.span;
        let node = match items.items.pop() {
            Some(Item::Node(Node::Attach(attach))) => {
                if attach.sup.is_some() || attach.primes > 0 {
                    return Err(double_script(true, range)
                        .with_hint("a prime is a superscript: write `f'^2`, not `f^2'`"));
                }
                let mut attach = (*attach).clone();
                attach.primes = u8::try_from(count).unwrap_or(u8::MAX);
                Node::Attach(Arc::new(attach))
            }
            last => {
                let base = match last {
                    None => Row::default(),
                    Some(Item::Node(node)) => Row::new([node]),
                    Some(Item::Group(row)) => row,
                };
                Node::attach(base, None, None, count, span)
            }
        };
        items.push(node);
        Ok(())
    }

    /// The argument of a command or a script: a group, or one token.
    fn arg(&mut self, cmd: Range<usize>, what: &str) -> TexResult<Row> {
        let (tok, range) = self.next();
        let missing = || {
            TexError::new(cmd.clone(), eco_format!("missing {what}"))
                .with_hint("write it after the command, in braces if it is more than one character")
        };
        match tok {
            Tok::Open => self.group(range),
            Tok::Char(c) if c.is_numeric() => Ok(Row::new([Node::number(c.into(), self.span)])),
            Tok::Char(c) if c != ']' => Ok(Row::new([self.char_node(c, range)])),
            // A bracket as an argument is one symbol, as in TeX.
            Tok::Cmd(name) if symbol_of(name).is_some_and(|c| is_opener(c) || is_closer(c)) => {
                let c = symbol_of(name).expect("a symbol");
                Ok(Row::new([Node::symbol(c, self.span)]))
            }
            Tok::Cmd(name)
                if !matches!(
                    name,
                    "\\" | "begin" | "end" | "left" | "right" | "label" | "middle"
                ) && command(name).is_some() =>
            {
                self.enter(&range)?;
                let mut items = Items::default();
                self.command(name, range, Ctx::new(Level::Nested), &mut items)?;
                self.leave();
                Ok(Row::new(items.into_nodes()))
            }
            Tok::Cmd("") => Err(TexError::new(range, "a backslash ends the formula")),
            Tok::Cmd(name) if command(name).is_none() => Err(unknown_command(name, range)),
            _ => Err(missing()),
        }
    }

    /// The optional argument of `\sqrt`, in brackets.
    fn optional_arg(&mut self) -> TexResult<Option<Row>> {
        let Some(open) = self.eat_char('[') else {
            return Ok(None);
        };
        self.enter(&open)?;
        let items = self.row_items(Ctx {
            bracket: true,
            ..Ctx::new(Level::Nested)
        })?;
        self.leave();
        let (tok, range) = self.next();
        if tok != Tok::Char(']') {
            return Err(match tok {
                Tok::Eof => TexError::new(open, "unclosed bracket"),
                tok => self.stray(tok, range),
            });
        }
        Ok(Some(Row::new(items.into_nodes())))
    }
}

/// Commands.
impl<'s> Parser<'s> {
    /// A control sequence and its arguments.
    fn command(
        &mut self,
        name: &'s str,
        range: Range<usize>,
        ctx: Ctx,
        items: &mut Items,
    ) -> TexResult<()> {
        let span = self.span;
        let Some(cmd) = command(name) else {
            if name.is_empty() {
                return Err(TexError::new(range, "a backslash ends the formula"));
            }
            return Err(unknown_command(name, range));
        };
        match cmd {
            Cmd::Symbol(c) => {
                if is_opener(c) {
                    return self.bracket(c, range, ctx, items);
                }
                items.push(Node::symbol(c, span));
            }
            Cmd::Spaces(n) => {
                for _ in 0..n {
                    items.push(Node::symbol('\u{2003}', span));
                }
            }
            Cmd::Ignore => {}
            Cmd::Size => {
                let (tok, next) = self.peek();
                if !is_delimiter(tok) {
                    return Err(
                        TexError::new(range, eco_format!("`\\{name}` takes a delimiter"))
                            .with_hint("as in `\\big(` or `\\Bigl\\{`"),
                    );
                }
                self.pos = next.end;
                self.token(tok, next, ctx, items)?;
            }
            Cmd::Operator(limits) => items.push(Node::Operator(Operator {
                name: name.into(),
                limits,
                span,
            })),
            Cmd::OperatorName => {
                let limits = self.eat_directly('*');
                let (text, text_range) = self.raw_arg(range, "the operator's name")?;
                let text: EcoString = text.chars().filter(|c| !c.is_whitespace()).collect();
                if text.is_empty() || !text.chars().all(char::is_alphabetic) {
                    return Err(TexError::new(
                        text_range,
                        "an operator's name is made of letters",
                    ));
                }
                if text == "d" {
                    self.differential(items);
                } else {
                    items.push(Node::Operator(Operator {
                        name: text,
                        limits,
                        span,
                    }));
                }
            }
            Cmd::Differential => self.differential(items),
            Cmd::Mod(paren) => {
                let op = Node::Operator(Operator {
                    name: "mod".into(),
                    limits: false,
                    span,
                });
                if paren {
                    let arg = self.arg(range, "the modulus")?;
                    let mut body = vec![op];
                    body.extend(arg.nodes().iter().cloned());
                    items.push(Node::Delimited(Arc::new(Delimited {
                        open: Some('('),
                        body: Row::new(body),
                        close: Some(')'),
                        span,
                    })));
                } else {
                    items.push(op);
                }
            }
            Cmd::Frac => {
                let num = self.arg(range.clone(), "numerator")?;
                let den = self.arg(range, "denominator")?;
                items.push(Node::Frac(Arc::new(Frac { num, den, span })));
            }
            Cmd::Sqrt => {
                let index = self.optional_arg()?;
                let radicand = self.arg(range, "radicand")?;
                items.push(Node::Root(Arc::new(Root {
                    index,
                    radicand,
                    span,
                })));
            }
            Cmd::Accent(accent) => {
                let base = self.arg(range, "base")?;
                items.push(Node::Accent(Arc::new(Accent { base, accent, span })));
            }
            Cmd::Set(over) => {
                let annotation = self.arg(range.clone(), "annotation")?;
                let base = self.arg(range.clone(), "relation")?;
                if !matches!(base.nodes(), [Node::Symbol(atom)] if atom.class == MathClass::Relation)
                {
                    return Err(TexError::new(
                        range,
                        eco_format!("`\\{name}` sets an annotation over or under a relation only"),
                    )
                    .with_hint(if over {
                        "over another body, write an accent (`\\dot{x}`) or a brace with its annotation: `\\overbrace{a + b}^{n}`"
                    } else {
                        "under an operator, write its limits (`\\lim_{x \\to 0}`); under another body, a brace with its annotation: `\\underbrace{a + b}_{n}`"
                    }));
                }
                let (sub, sup) = if over {
                    (None, Some(annotation))
                } else {
                    (Some(annotation), None)
                };
                items.push(Node::Attach(Arc::new(Attach {
                    base,
                    sub,
                    sup,
                    primes: 0,
                    pre_sub: None,
                    pre_sup: None,
                    span,
                })));
            }
            Cmd::Vector => {
                let base = self.arg(range, "vector")?;
                items.push(Node::Vector(Arc::new(Vector {
                    base,
                    unit: false,
                    span,
                })));
            }
            Cmd::Decoration(kind) => {
                let body = self.arg(range, "body")?;
                let braces = !matches!(kind, DecorationKind::Overline | DecorationKind::Underline);
                let script = if kind.is_over() { Tok::Sup } else { Tok::Sub };
                let annotation = match self.peek() {
                    (tok, script_range) if braces && tok == script => {
                        self.pos = script_range.end;
                        Some(self.arg(script_range, "annotation")?)
                    }
                    _ => None,
                };
                items.push(Node::Decoration(Arc::new(Decoration {
                    kind,
                    body,
                    annotation,
                    span,
                })));
            }
            Cmd::Style(variant) => {
                let body = self.arg(range, "body")?;
                if variant == MathVariant::Upright
                    && let [Node::Ident(atom)] = body.nodes()
                    && atom.text == "d"
                {
                    self.differential(items);
                    return Ok(());
                }
                items.push(Node::Styled(Arc::new(Styled {
                    variant,
                    body,
                    span,
                })));
            }
            Cmd::Text(variant) => {
                let (text, _) = self.raw_arg(range, "text")?;
                if text.is_empty() {
                    return Ok(());
                }
                let node = Node::text(text, span);
                items.push(match variant {
                    None => node,
                    Some(variant) => Node::Styled(Arc::new(Styled {
                        variant,
                        body: Row::new([node]),
                        span,
                    })),
                });
            }
            Cmd::Not => {
                let (tok, next) = self.next();
                let negated = match tok {
                    Tok::Char(c) => negate(c),
                    Tok::Cmd(name) => symbol_of(name).and_then(negate),
                    _ => None,
                };
                let Some(c) = negated else {
                    return Err(TexError::new(
                        range.start..next.end,
                        "`\\not` cannot negate this",
                    )
                    .with_hint("write the negated relation itself, such as `\\neq`, `\\notin` or `\\nleq`"));
                };
                items.push(Node::symbol(c, span));
            }
            Cmd::Left => {
                let open = self.delimiter(range.clone())?;
                self.enter(&range)?;
                let inner = self.row_items(Ctx::new(Level::Nested))?;
                self.leave();
                let (tok, right) = self.next();
                if tok != Tok::Cmd("right") {
                    return Err(match tok {
                        Tok::Eof => TexError::new(range, "`\\left` without `\\right`"),
                        tok => self.stray(tok, right),
                    });
                }
                let close = self.delimiter(right)?;
                items.push(Node::Delimited(Arc::new(Delimited {
                    open,
                    body: Row::new(inner.into_nodes()),
                    close,
                    span,
                })));
            }
            Cmd::Begin => {
                self.enter(&range)?;
                let node = self.environment(range)?;
                self.leave();
                items.push(node);
            }
            Cmd::Label => {
                let (text, text_range) = self.raw_arg(range.clone(), "a label's name")?;
                let whole = range.start..text_range.end + 1;
                if ctx.level != Level::Line {
                    return Err(TexError::new(
                        whole,
                        "a label can only be at the top level of a line",
                    )
                    .with_hint("it names the line of the equation it stands in"));
                }
                if !folio_syntax::is_valid_label_literal_id(&text) {
                    return Err(TexError::new(
                        text_range,
                        eco_format!("`{text}` is not a valid label"),
                    )
                    .with_hint("a label is made of letters, digits, `_`, `-` and `:`, and does not end with `:` or `-`"));
                }
                if let Some((_, first)) = &self.label {
                    return Err(TexError::new(whole, "a line can have only one label")
                        .with_hint(eco_format!("it already has `{}`", &self.src[first.clone()])));
                }
                self.label = Some((text, whole));
            }
            Cmd::Unsupported(hint) => {
                let error = TexError::new(
                    range,
                    eco_format!("`\\{name}` is not supported in LaTeX math"),
                );
                return Err(match hint {
                    Some(hint) => error.with_hint(hint),
                    None => error,
                });
            }
            Cmd::Structural => return Err(self.stray(Tok::Cmd(name), range)),
        }
        Ok(())
    }

    /// The differential, which takes the thin space before it in.
    fn differential(&mut self, items: &mut Items) {
        if let Some(Item::Node(Node::Symbol(atom))) = items.items.last()
            && atom.text == "\u{2009}"
        {
            items.items.pop();
        }
        items.push(Node::Differential(self.span));
    }

    /// The delimiter after `\left` or `\right`: `none` for `.`.
    fn delimiter(&mut self, cmd: Range<usize>) -> TexResult<Option<char>> {
        let (tok, range) = self.next();
        let c = match tok {
            Tok::Char('.') => return Ok(None),
            Tok::Char('<') => Some('⟨'),
            Tok::Char('>') => Some('⟩'),
            Tok::Char(c) => Some(c),
            Tok::Cmd(name) => symbol_of(name),
            _ => None,
        };
        match c {
            Some(c) if is_delimiter_char(c) => Ok(Some(c)),
            _ => Err(TexError::new(
                cmd.start..range.end.max(cmd.end),
                eco_format!("`{}` takes a delimiter", &self.src[cmd]),
            )
            .with_hint("such as `(`, `[`, `\\{`, `|`, `\\langle`, or `.` for none")),
        }
    }
}

/// What a control sequence does.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Cmd {
    /// A symbol.
    Symbol(char),
    /// A number of quad spaces: `\qquad` is two.
    Spaces(usize),
    /// Something the tree leaves to layout: `\displaystyle`, `\limits`,
    /// `\!`.
    Ignore,
    /// A size for the delimiter that follows: `\big`, `\Bigl`.
    Size,
    /// A named operator, with whether its attachments are limits.
    Operator(bool),
    /// `\operatorname{name}`, or `\operatorname*{name}` with limits.
    OperatorName,
    /// The differential: `\dd`.
    Differential,
    /// `\bmod`, or `\pmod{n}` in parentheses.
    Mod(bool),
    /// `\frac{a}{b}` and its kin.
    Frac,
    /// `\sqrt[n]{x}`.
    Sqrt,
    /// An accent over its argument.
    Accent(char),
    /// `\vec{F}`.
    Vector,
    /// `\overset{a}{=}` (`true`) or `\underset{a}{=}`: an annotation over
    /// or under a relation, its limits.
    Set(bool),
    /// A line or brace over or under its argument.
    Decoration(DecorationKind),
    /// A math alphabet for its argument.
    Style(MathVariant),
    /// Text, upright or in an alphabet.
    Text(Option<MathVariant>),
    /// `\not`, which negates the relation after it.
    Not,
    /// `\left`.
    Left,
    /// `\begin{env}`.
    Begin,
    /// `\label{name}`.
    Label,
    /// A known command that is not supported, with a hint.
    Unsupported(Option<&'static str>),
    /// `\\`, `\right` and `\end`, which end rows and cannot stand alone.
    Structural,
}

/// What a control sequence does, or `None` for an unknown one.
fn command(name: &str) -> Option<Cmd> {
    use DecorationKind as D;
    use MathVariant as V;
    if let Some(c) = symbol_of(name) {
        return Some(Cmd::Symbol(c));
    }
    if let Some(limits) = crate::operator(name) {
        return Some(Cmd::Operator(limits));
    }
    Some(match name {
        "quad" => Cmd::Spaces(1),
        "qquad" => Cmd::Spaces(2),
        "!" | "negthinspace" | "negmedspace" | "negthickspace" | "displaystyle" | "textstyle"
        | "scriptstyle" | "scriptscriptstyle" | "limits" | "nolimits" => Cmd::Ignore,
        "big" | "Big" | "bigg" | "Bigg" | "bigl" | "Bigl" | "biggl" | "Biggl" | "bigr" | "Bigr"
        | "biggr" | "Biggr" | "bigm" | "Bigm" | "biggm" | "Biggm" => Cmd::Size,
        "operatorname" => Cmd::OperatorName,
        "dd" => Cmd::Differential,
        "bmod" | "mod" => Cmd::Mod(false),
        "pmod" => Cmd::Mod(true),
        "frac" | "dfrac" | "tfrac" | "cfrac" => Cmd::Frac,
        "sqrt" => Cmd::Sqrt,
        "vec" => Cmd::Vector,
        "overset" | "stackrel" => Cmd::Set(true),
        "underset" => Cmd::Set(false),
        "hat" | "widehat" => Cmd::Accent('\u{302}'),
        "grave" => Cmd::Accent('\u{300}'),
        "acute" => Cmd::Accent('\u{301}'),
        "tilde" | "widetilde" => Cmd::Accent('\u{303}'),
        "bar" => Cmd::Accent('\u{304}'),
        "breve" => Cmd::Accent('\u{306}'),
        "dot" => Cmd::Accent('\u{307}'),
        "ddot" => Cmd::Accent('\u{308}'),
        "dddot" => Cmd::Accent('\u{20DB}'),
        "mathring" => Cmd::Accent('\u{30A}'),
        "check" => Cmd::Accent('\u{30C}'),
        "overrightarrow" => Cmd::Accent('\u{20D7}'),
        "overleftarrow" => Cmd::Accent('\u{20D6}'),
        "overleftrightarrow" => Cmd::Accent('\u{20E1}'),
        "overline" => Cmd::Decoration(D::Overline),
        "underline" => Cmd::Decoration(D::Underline),
        "overbrace" => Cmd::Decoration(D::Overbrace),
        "underbrace" => Cmd::Decoration(D::Underbrace),
        "overbracket" => Cmd::Decoration(D::Overbracket),
        "underbracket" => Cmd::Decoration(D::Underbracket),
        "mathrm" | "mathup" => Cmd::Style(V::Upright),
        "mathit" => Cmd::Style(V::Italic),
        "mathbf" | "boldsymbol" | "bm" => Cmd::Style(V::Bold),
        "mathcal" | "mathscr" => Cmd::Style(V::Calligraphic),
        "mathfrak" => Cmd::Style(V::Fraktur),
        "mathbb" => Cmd::Style(V::DoubleStruck),
        "mathsf" => Cmd::Style(V::SansSerif),
        "mathtt" => Cmd::Style(V::Monospace),
        "text" | "textrm" | "textup" | "textnormal" | "mbox" => Cmd::Text(None),
        "textbf" => Cmd::Text(Some(V::Bold)),
        "textit" => Cmd::Text(Some(V::Italic)),
        "textsf" => Cmd::Text(Some(V::SansSerif)),
        "texttt" => Cmd::Text(Some(V::Monospace)),
        "not" => Cmd::Not,
        "left" => Cmd::Left,
        "begin" => Cmd::Begin,
        "label" => Cmd::Label,
        "\\" | "right" | "end" => Cmd::Structural,
        "nonumber" | "notag" => Cmd::Unsupported(Some(
            "a display is numbered as a whole, or only on its lines that have a `\\label{..}` when any has one; \
             to number some lines and not others, label those to number. To leave a display unnumbered, \
             set `numbering: none` on it in a stylesheet",
        )),
        "tag" => Cmd::Unsupported(Some(
            "equation numbers come from the `equation` counter and the `numbering` property; \
             a display cannot set its own",
        )),
        "binom" | "dbinom" | "tbinom" | "choose" => Cmd::Unsupported(Some(
            "the math tree has no binomial yet; write `\\begin{pmatrix} n \\\\ k \\end{pmatrix}`",
        )),
        "phantom" | "hphantom" | "vphantom" => Cmd::Unsupported(Some(
            "the math tree has no phantoms yet; align lines with `&` and space them with `\\quad`",
        )),
        "rm" => Cmd::Unsupported(Some("write `\\mathrm{..}` around what is upright")),
        "bf" => Cmd::Unsupported(Some("write `\\mathbf{..}` around what is bold")),
        "it" => Cmd::Unsupported(Some("write `\\mathit{..}` around what is italic")),
        "cal" => Cmd::Unsupported(Some("write `\\mathcal{..}` around what is calligraphic")),
        "over" => Cmd::Unsupported(Some("write the fraction as `\\frac{a}{b}`")),
        "si" | "SI" | "qty" | "unit" | "num" | "ang" => Cmd::Unsupported(Some(
            "quantities are native, outside `:tex`: `$qty(9.81, \"m s^-2\")$`, `$num(6.02e23)$`, `$unit(\"kg\")$`",
        )),
        "ce" | "pu" => Cmd::Unsupported(Some(
            "chemical formulas (mhchem) are not read; write subscripts with `\\mathrm`: `\\mathrm{H_2O}`",
        )),
        "substack" | "middle" | "boxed" | "cancel" | "color" | "hspace" | "intertext"
        | "newcommand" | "renewcommand" | "def" => Cmd::Unsupported(None),
        "[" | "]" | "(" | ")" => Cmd::Unsupported(Some(
            "the dollars already make the equation; remove `\\[`, `\\]`, `\\(` and `\\)`",
        )),
        _ => return None,
    })
}

/// The symbol a control sequence stands for.
fn symbol_of(name: &str) -> Option<char> {
    SYMBOLS
        .iter()
        .find(|&&(entry, _)| entry == name)
        .map(|&(_, c)| c)
}

/// The symbols of LaTeX math, by command name.
///
/// Each is the character that native math's name for it gives (SPEC §8.1),
/// so that `\epsilon` and `epsilon.alt` are the same symbol: LaTeX's
/// `\epsilon` and `\phi` are the lunate and closed forms, `\varepsilon` and
/// `\varphi` the open ones.
pub const SYMBOLS: &[(&str, char)] = &[
    // Greek letters.
    ("alpha", 'α'),
    ("beta", 'β'),
    ("gamma", 'γ'),
    ("delta", 'δ'),
    ("epsilon", 'ϵ'),
    ("varepsilon", 'ε'),
    ("zeta", 'ζ'),
    ("eta", 'η'),
    ("theta", 'θ'),
    ("vartheta", 'ϑ'),
    ("iota", 'ι'),
    ("kappa", 'κ'),
    ("lambda", 'λ'),
    ("mu", 'μ'),
    ("nu", 'ν'),
    ("xi", 'ξ'),
    ("omicron", 'ο'),
    ("pi", 'π'),
    ("varpi", 'ϖ'),
    ("rho", 'ρ'),
    ("varrho", 'ϱ'),
    ("sigma", 'σ'),
    ("varsigma", 'ς'),
    ("tau", 'τ'),
    ("upsilon", 'υ'),
    ("phi", 'ϕ'),
    ("varphi", 'φ'),
    ("chi", 'χ'),
    ("psi", 'ψ'),
    ("omega", 'ω'),
    ("Gamma", 'Γ'),
    ("Delta", 'Δ'),
    ("Theta", 'Θ'),
    ("Lambda", 'Λ'),
    ("Xi", 'Ξ'),
    ("Pi", 'Π'),
    ("Sigma", 'Σ'),
    ("Upsilon", 'Υ'),
    ("Phi", 'Φ'),
    ("Psi", 'Ψ'),
    ("Omega", 'Ω'),
    // Letter-like symbols.
    ("hbar", 'ℏ'),
    ("imath", 'ı'),
    ("jmath", 'ȷ'),
    ("ell", 'ℓ'),
    ("aleph", 'ℵ'),
    ("partial", '∂'),
    ("nabla", '∇'),
    ("infty", '∞'),
    ("degree", '°'),
    ("prime", '′'),
    ("angle", '∠'),
    ("emptyset", '∅'),
    ("varnothing", '∅'),
    ("dagger", '†'),
    ("Re", 'ℜ'),
    ("Im", 'ℑ'),
    // Binary operators.
    ("pm", '±'),
    ("mp", '∓'),
    ("times", '×'),
    ("otimes", '⊗'),
    ("oplus", '⊕'),
    ("div", '÷'),
    ("cdot", '⋅'),
    ("odot", '⊙'),
    ("ast", '∗'),
    ("star", '⋆'),
    ("circ", '∘'),
    ("bullet", '∙'),
    ("cup", '∪'),
    ("cap", '∩'),
    ("setminus", '∖'),
    ("wedge", '∧'),
    ("land", '∧'),
    ("vee", '∨'),
    ("lor", '∨'),
    ("neg", '¬'),
    ("lnot", '¬'),
    // Relations.
    ("ne", '≠'),
    ("neq", '≠'),
    ("equiv", '≡'),
    ("coloneqq", '≔'),
    ("approx", '≈'),
    ("approxeq", '≊'),
    ("sim", '∼'),
    ("simeq", '≃'),
    ("cong", '≅'),
    ("propto", '∝'),
    ("le", '≤'),
    ("leq", '≤'),
    ("ge", '≥'),
    ("geq", '≥'),
    ("leqslant", '⩽'),
    ("geqslant", '⩾'),
    ("ll", '≪'),
    ("gg", '≫'),
    ("nless", '≮'),
    ("ngtr", '≯'),
    ("nleq", '≰'),
    ("ngeq", '≱'),
    ("in", '∈'),
    ("notin", '∉'),
    ("ni", '∋'),
    ("subset", '⊂'),
    ("subseteq", '⊆'),
    ("supset", '⊃'),
    ("supseteq", '⊇'),
    ("parallel", '∥'),
    ("nparallel", '∦'),
    ("perp", '⟂'),
    ("mid", '∣'),
    ("nmid", '∤'),
    ("therefore", '∴'),
    ("because", '∵'),
    ("forall", '∀'),
    ("exists", '∃'),
    ("nexists", '∄'),
    // Arrows.
    ("to", '→'),
    ("rightarrow", '→'),
    ("leftarrow", '←'),
    ("gets", '←'),
    ("uparrow", '↑'),
    ("downarrow", '↓'),
    ("leftrightarrow", '↔'),
    ("updownarrow", '↕'),
    ("longrightarrow", '⟶'),
    ("longleftarrow", '⟵'),
    ("longleftrightarrow", '⟷'),
    ("Rightarrow", '⇒'),
    ("Leftarrow", '⇐'),
    ("Leftrightarrow", '⇔'),
    ("Longrightarrow", '⟹'),
    ("Longleftarrow", '⟸'),
    ("Longleftrightarrow", '⟺'),
    ("implies", '⟹'),
    ("impliedby", '⟸'),
    ("iff", '⟺'),
    ("mapsto", '↦'),
    ("leadsto", '⇝'),
    ("rightleftarrows", '⇄'),
    ("rightleftharpoons", '⇌'),
    // Large operators.
    ("sum", '∑'),
    ("prod", '∏'),
    ("coprod", '∐'),
    ("int", '∫'),
    ("iint", '∬'),
    ("iiint", '∭'),
    ("oint", '∮'),
    ("oiint", '∯'),
    ("oiiint", '∰'),
    ("bigcup", '⋃'),
    ("bigcap", '⋂'),
    ("bigoplus", '⨁'),
    ("bigotimes", '⨂'),
    // Dots and punctuation.
    ("dots", '…'),
    ("ldots", '…'),
    ("dotsc", '…'),
    ("cdots", '⋯'),
    ("dotsb", '⋯'),
    ("vdots", '⋮'),
    ("ddots", '⋱'),
    ("colon", ':'),
    // Delimiters.
    ("{", '{'),
    ("}", '}'),
    ("lbrace", '{'),
    ("rbrace", '}'),
    ("lbrack", '['),
    ("rbrack", ']'),
    ("langle", '⟨'),
    ("rangle", '⟩'),
    ("vert", '|'),
    ("lvert", '|'),
    ("rvert", '|'),
    ("|", '‖'),
    ("Vert", '‖'),
    ("lVert", '‖'),
    ("rVert", '‖'),
    ("lfloor", '⌊'),
    ("rfloor", '⌋'),
    ("lceil", '⌈'),
    ("rceil", '⌉'),
    ("backslash", '\\'),
    // Escaped characters.
    ("%", '%'),
    ("$", '$'),
    ("#", '#'),
    ("&", '&'),
    ("_", '_'),
    // Spaces.
    (",", '\u{2009}'),
    ("thinspace", '\u{2009}'),
    (":", '\u{205F}'),
    (">", '\u{205F}'),
    ("medspace", '\u{205F}'),
    (";", '\u{2004}'),
    ("thickspace", '\u{2004}'),
    (" ", '\u{2004}'),
    ("enspace", '\u{2002}'),
];

/// The relation `\not` makes of a symbol.
fn negate(c: char) -> Option<char> {
    Some(match c {
        '=' => '≠',
        '<' => '≮',
        '>' => '≯',
        '≤' => '≰',
        '≥' => '≱',
        '∈' => '∉',
        '∋' => '∌',
        '∃' => '∄',
        '≡' => '≢',
        '∼' => '≁',
        '≈' => '≉',
        '≃' => '≄',
        '≅' => '≇',
        '⊂' => '⊄',
        '⊃' => '⊅',
        '⊆' => '⊈',
        '⊇' => '⊉',
        '∥' => '∦',
        '∣' => '∤',
        _ => return None,
    })
}

/// Whether an alignment environment of LaTeX holds the lines of the
/// equation.
fn is_lines_env(name: &str) -> bool {
    matches!(
        name,
        "equation"
            | "equation*"
            | "align"
            | "align*"
            | "aligned"
            | "alignat"
            | "alignat*"
            | "alignedat"
            | "flalign"
            | "flalign*"
            | "gather"
            | "gather*"
            | "gathered"
            | "multline"
            | "multline*"
            | "split"
            | "eqnarray"
            | "eqnarray*"
    )
}

/// Whether a character opens a bracket pair, as in native math: `(` and
/// the characters of the opening class.
fn is_opener(c: char) -> bool {
    c == '(' || crate::class(c) == MathClass::Opening
}

/// Whether a character closes a bracket pair.
fn is_closer(c: char) -> bool {
    c == ')' || crate::class(c) == MathClass::Closing
}

/// Whether a character can follow `\left`, `\right` or `\big`.
fn is_delimiter_char(c: char) -> bool {
    is_opener(c)
        || is_closer(c)
        || crate::class(c) == MathClass::Fence
        || matches!(c, '/' | '\\' | '↑' | '↓' | '↕')
}

/// Whether a token is a delimiter, as `\big` takes.
fn is_delimiter(tok: Tok<'_>) -> bool {
    match tok {
        Tok::Char(c) => is_delimiter_char(c),
        Tok::Cmd(name) => symbol_of(name).is_some_and(is_delimiter_char),
        _ => false,
    }
}

/// Whether a character is a combining mark.
fn is_combining(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// The error of an alignment point where none can stand.
fn misplaced_align_point(range: Range<usize>) -> TexError {
    TexError::new(
        range,
        "an alignment point can only be at the top level of a line or a case",
    )
}

/// The error of a second superscript or subscript.
fn double_script(sup: bool, range: Range<usize>) -> TexError {
    let what = if sup { "superscript" } else { "subscript" };
    TexError::new(range, eco_format!("double {what}")).with_hint(eco_format!(
        "group the {what}s in braces: `x^{{a^b}}` or `{{x^a}}^b`"
    ))
}

/// The error of an unknown control sequence, with the known one it is
/// closest to as a hint.
fn unknown_command(name: &str, range: Range<usize>) -> TexError {
    let error = TexError::new(range, eco_format!("unknown LaTeX command `\\{name}`"));
    let names = SYMBOLS
        .iter()
        .map(|&(entry, _)| entry)
        .chain(NAMES.iter().copied())
        .chain(crate::symbols::operator_names());
    let best = names
        // Names differ in length by at most the distance, which keeps the
        // comparisons short, whatever the length of the unknown name.
        .filter(|entry| entry.len() > 1 && entry.len().abs_diff(name.len()) <= 2)
        .map(|entry| (distance(name, entry), entry))
        // A short name is close to too many others at a distance of two:
        // `\\si` is not a typo of `\\Pi`.
        .filter(|&(d, entry)| {
            let shorter = entry.len().min(name.len());
            d < shorter && d <= if shorter < 5 { 1 } else { 2 }
        })
        .min();
    match best {
        Some((_, entry)) => error.with_hint(eco_format!("did you mean `\\{entry}`?")),
        None => error.with_hint(
            "Folio reads the amsmath core of LaTeX math; the native syntax of SPEC §8.1 may have what you need",
        ),
    }
}

/// The names of the commands that are not symbols or operators, for
/// suggestions.
const NAMES: &[&str] = &[
    "quad",
    "qquad",
    "displaystyle",
    "textstyle",
    "limits",
    "nolimits",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "operatorname",
    "bmod",
    "pmod",
    "frac",
    "dfrac",
    "tfrac",
    "cfrac",
    "sqrt",
    "vec",
    "overset",
    "underset",
    "stackrel",
    "hat",
    "widehat",
    "grave",
    "acute",
    "tilde",
    "widetilde",
    "bar",
    "breve",
    "dot",
    "ddot",
    "dddot",
    "mathring",
    "check",
    "overrightarrow",
    "overleftarrow",
    "overline",
    "underline",
    "overbrace",
    "underbrace",
    "mathrm",
    "mathit",
    "mathbf",
    "boldsymbol",
    "mathcal",
    "mathscr",
    "mathfrak",
    "mathbb",
    "mathsf",
    "mathtt",
    "text",
    "textrm",
    "textbf",
    "textit",
    "mbox",
    "left",
    "right",
    "begin",
    "end",
    "label",
];

/// The edit distance between two names, in characters, counting the
/// transposition of two neighbours as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repr(src: &str) -> String {
        parse(src, Span::detached(), 100)
            .unwrap_or_else(|e| panic!("{src}: {e:?}"))
            .repr()
    }

    fn error(src: &str) -> (String, String) {
        let e = parse(src, Span::detached(), 100).unwrap_err();
        (e.message.to_string(), src[e.range].to_string())
    }

    #[test]
    fn scripts_follow_tex() {
        assert_eq!(repr("x^10"), "[attach([x], sup: [num(1)]), num(0)]");
        assert_eq!(repr("x_i^2"), "[attach([x], sub: [i], sup: [num(2)])]");
        assert_eq!(repr("x^2_i"), "[attach([x], sub: [i], sup: [num(2)])]");
        assert_eq!(repr("{a+b}^2"), "[attach([a, vary(+), b], sup: [num(2)])]");
        assert_eq!(repr("f'^2"), "[attach([f], sup: [num(2)], primes: 1)]");
        assert_eq!(
            repr("\\sum_{i=1}^n"),
            "[nary(∑, sub: [i, rel(=), num(1)], sup: [n])]"
        );
        assert_eq!(error("x^2^3"), ("double superscript".into(), "^".into()));
    }

    #[test]
    fn brackets_pair_as_in_native_math() {
        assert_eq!(repr("(a]"), "[delim('(', [a], ']')]");
        assert_eq!(repr("(a"), "[open((), a]");
        assert_eq!(repr("\\{a\\}"), "[delim('{', [a], '}')]");
        assert_eq!(repr("\\left. x \\right|"), "[delim(none, [x], '|')]");
    }

    #[test]
    fn unknown_commands_point_at_themselves() {
        assert_eq!(
            error("a + \\fracc{1}{2}"),
            ("unknown LaTeX command `\\fracc`".into(), "\\fracc".into())
        );
        let e = parse("\\alpah", Span::detached(), 100).unwrap_err();
        assert_eq!(e.hints, ["did you mean `\\alpha`?"]);
    }

    fn hint(src: &str) -> String {
        let e = parse(src, Span::detached(), 100).unwrap_err();
        e.hints.join(" | ")
    }

    #[test]
    fn suggestions_for_short_names_are_one_edit_away() {
        // Two edits match almost any short name (#353).
        for (src, not) in [("\\lamb", "lambda"), ("\\foo", "cos"), ("\\large", "arg")] {
            assert!(
                !hint(src).contains(&format!("`\\{not}`")),
                "{src}: {}",
                hint(src)
            );
        }
        assert_eq!(hint("\\coss"), "did you mean `\\cos`?");
        assert_eq!(hint("\\alpah"), "did you mean `\\alpha`?");
        assert_eq!(hint("\\lamdba"), "did you mean `\\lambda`?");
        assert_eq!(hint("\\mathbbb{R}"), "did you mean `\\mathbb`?");
    }

    #[test]
    fn commands_of_other_packages_point_to_folio() {
        assert!(hint("\\rm d").contains("`\\mathrm{..}`"));
        assert!(hint("\\bf x").contains("`\\mathbf{..}`"));
        assert!(hint("a \\over b").contains("`\\frac{a}{b}`"));
        for src in ["\\si{m}", "\\SI{9.81}{m}", "\\qty{9.81}{m}"] {
            assert!(hint(src).contains("`$qty(9.81, \"m s^-2\")$`"), "{src}");
        }
        assert!(hint("x \\nonumber").contains("`\\label{..}`"));
        assert!(hint("x \\tag{1}").contains("`equation` counter"));
        assert!(hint("\\binom{n}{k}").contains("pmatrix"));
    }

    #[test]
    fn annotations_over_relations() {
        assert_eq!(
            repr("a \\overset{!}{=} b"),
            "[a, attach([rel(=)], sup: [sym(!)]), b]"
        );
        assert_eq!(
            repr("a \\stackrel{?}{=} b"),
            "[a, attach([rel(=)], sup: [punct(?)]), b]"
        );
        assert_eq!(
            repr("x \\underset{n \\to 0}{\\to} y"),
            "[x, attach([rel(→)], sub: [n, rel(→), num(0)]), y]"
        );
        assert_eq!(
            error("\\overset{a}{x}"),
            (
                "`\\overset` sets an annotation over or under a relation only".into(),
                "\\overset".into()
            )
        );
        assert_eq!(
            repr("\\imath + \\jmath + \\hbar"),
            "[ı, vary(+), ȷ, vary(+), ℏ]"
        );
    }

    #[test]
    fn pre_scripts_on_an_empty_base() {
        // Isotope notation (#352): the scripts belong to the nucleus.
        assert_eq!(
            repr("{}^{14}_{6}C"),
            "[attach([C], pre-sub: [num(6)], pre-sup: [num(14)])]"
        );
        assert_eq!(
            repr("{}_{92}^{235}\\mathrm{U} + {}^1_0 n"),
            "[attach([upright([U])], pre-sub: [num(92)], pre-sup: [num(235)]), vary(+), \
             attach([n], pre-sub: [num(0)], pre-sup: [num(1)])]"
        );
        // Scripts after the nucleus too, and a pre-script alone.
        assert_eq!(
            repr("{}^{238}U^{2+}"),
            "[attach([U], sup: [num(2), vary(+)], pre-sup: [num(238)])]"
        );
        // Before an operator or a relation, an empty base stays a base.
        assert_eq!(repr("{}^2 = x"), "[attach([], sup: [num(2)]), rel(=), x]");
    }

    #[test]
    fn array_columns_keep_their_alignment() {
        assert_eq!(
            repr("\\begin{array}{lcr} a & b & c \\end{array}"),
            "[mat(none, [[a], [b], [c]], none, align: lcr)]"
        );
        assert_eq!(
            repr("\\begin{pmatrix} a \\end{pmatrix}"),
            "[mat('(', [[a]], ')')]"
        );
    }

    #[test]
    fn long_unknown_commands_are_cheap() {
        let src = format!("\\{}", "a".repeat(1_000_000));
        let e = parse(&src, Span::detached(), 100).unwrap_err();
        assert_eq!(e.range, 0..src.len());
    }

    #[test]
    fn nesting_is_limited() {
        let src = "{".repeat(200) + &"}".repeat(200);
        assert!(parse(&src, Span::detached(), 100).unwrap_err().too_deep);
    }

    #[test]
    fn distances() {
        assert_eq!(distance("alpah", "alpha"), 1);
        assert_eq!(distance("frac", "frac"), 0);
        assert_eq!(distance("", "ab"), 2);
    }
}
