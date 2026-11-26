//! The semantic math tree of Folio (SPEC §8).
//!
//! Evaluation turns an equation's native syntax (SPEC §8.1), and later its
//! LaTeX input, into a [`Formula`]: lines of [`Row`]s of [`Node`]s. The tree
//! says what a formula is, not how it looks: a fraction, a base with
//! attachments, an n-ary operator with its limits, a vector, a matrix. Math
//! layout (SPEC §8.2) sets it with the font's OpenType MATH table, and the
//! Office writers map it to OMML element by element (SPEC §8.3):
//!
//! | Node | OMML |
//! | --- | --- |
//! | [`Node::Frac`] | `m:f` |
//! | [`Node::Attach`] | `m:sSub`, `m:sSup`, `m:sSubSup`; `m:limLow`, `m:limUpp` under an [`Operator`] with limits |
//! | [`Node::Nary`] | `m:nary`, whose `m:e` is the operand that follows it |
//! | [`Node::Root`] | `m:rad` |
//! | [`Node::Delimited`] | `m:d` |
//! | [`Node::Matrix`] | `m:m`, in an `m:d` for its delimiters |
//! | [`Node::Cases`] | `m:eqArr` in an `m:d` with an opening brace |
//! | [`Node::Accent`], [`Node::Vector`] | `m:acc` (or bold `m:r`, as the stylesheet chooses for vectors) |
//! | [`Node::Decoration`] | `m:bar` (lines), `m:groupChr` with an `m:limLow` or `m:limUpp` (braces) |
//! | [`Node::Styled`] | `m:sty` and `m:scr` on the runs inside |
//! | [`Node::Operator`] | `m:func` with an upright `m:fName` |
//! | atoms and text | `m:r` runs; `m:nor` for text |
//! | lines with alignment points | `m:eqArr` with `&` in its runs |
//!
//! Every node keeps the span of the source it came from, so errors,
//! span maps and the editor can point at it. Rows cache their hash and
//! their depth, and share their parts, so that a script can build large
//! formulas from smaller ones without copying or rehashing them.

#![forbid(unsafe_code)]

mod symbols;

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::{self, Debug, Formatter, Write};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use ecow::{EcoString, EcoVec};
use folio_syntax::Span;

pub use self::symbols::{is_symbol_base, symbol};
pub use unicode_math_class::MathClass;

/// A formula: what one equation holds.
///
/// An inline formula has one line. A display formula has a line for each
/// part between `\` line breaks, and each line may hold alignment points
/// and end with a label (`<eq:suvat>`) that names that line, so that it
/// can be numbered and referred to on its own (SPEC §8.1).
#[derive(Clone, Default, Eq, PartialEq, Hash)]
pub struct Formula {
    lines: EcoVec<Line>,
    depth: u32,
}

impl Formula {
    /// A formula of these lines.
    pub fn new(lines: impl IntoIterator<Item = Line>) -> Self {
        let lines: EcoVec<Line> = lines.into_iter().collect();
        let depth = lines.iter().map(|line| line.row.depth()).max().unwrap_or(0);
        Self { lines, depth }
    }

    /// A formula of one line, without a label.
    pub fn row(row: Row, span: Span) -> Self {
        Self::new([Line {
            row,
            label: None,
            span,
        }])
    }

    /// The lines, in order. An empty formula has none.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// The row of a formula that is one line without a label, as it is
    /// placed in another formula, or `None` for an empty formula.
    pub fn as_row(&self) -> Option<&Row> {
        match self.lines.as_slice() {
            [line] if line.label.is_none() => Some(&line.row),
            _ => None,
        }
    }

    /// The labels of the lines, in order, with the spans they were written
    /// at.
    pub fn labels(&self) -> impl Iterator<Item = &Label> {
        self.lines.iter().filter_map(|line| line.label.as_ref())
    }

    /// How deeply the formula nests: as deeply as its deepest row
    /// ([`Row::depth`]). Cached, so this takes constant time.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// A compact description of the tree, for snapshot tests and debugging.
    /// Lines are separated by ` \ `, as in the source:
    /// `[v, ^[2], rel(=), u] <eq:suvat> \ [..]`.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push_str(" \\ ");
            }
            line.row.write_repr(&mut out);
            if let Some(label) = &line.label {
                write!(out, " <{}>", label.name).unwrap();
            }
        }
        out
    }
}

impl Debug for Formula {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Formula({})", self.repr())
    }
}

/// One line of a formula.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Line {
    /// What the line holds. Its top level may hold [`Node::AlignPoint`]s,
    /// which split it into cells aligned with the cells of the other lines.
    pub row: Row,
    /// The label that ends the line, if any.
    pub label: Option<Label>,
    /// Where the line starts: its first node, or the line break that ends
    /// an empty line.
    pub span: Span,
}

/// A label that names a line of a formula: `<eq:suvat>`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Label {
    /// The name, without the angle brackets.
    pub name: EcoString,
    /// Where it was written.
    pub span: Span,
}

/// A sequence of nodes, set one after another: the body of a formula, a
/// fraction's numerator, an attachment, an argument.
///
/// A row caches its hash, its depth and the number of nodes it holds,
/// counting those of the rows inside, so that hashing a row takes time in
/// its own length only. It shares its nodes, and nodes share their parts,
/// so cloning is cheap.
#[derive(Clone)]
pub struct Row {
    nodes: EcoVec<Node>,
    depth: u32,
    size: u64,
    hash: u64,
}

impl Row {
    /// A row of these nodes.
    pub fn new(nodes: impl IntoIterator<Item = Node>) -> Self {
        let nodes: EcoVec<Node> = nodes.into_iter().collect();
        let depth = nodes
            .iter()
            .map(Node::depth)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let size = nodes.iter().fold(nodes.len() as u64, |size, node| {
            size.saturating_add(node.size())
        });
        let mut hasher = DefaultHasher::new();
        nodes.hash(&mut hasher);
        Self {
            nodes,
            depth,
            size,
            hash: hasher.finish(),
        }
    }

    /// The nodes, in order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Whether the row has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// How deeply the row nests: `1` for a row of atoms, and one more than
    /// the deepest row inside its nodes otherwise.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The number of nodes in the row and in the rows inside it, counting
    /// shared rows as often as they occur (saturating). It measures what
    /// walking the whole tree costs.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The cell boundaries of a line: the row split at its top-level
    /// alignment points, as rows of the nodes between them.
    pub fn cells(&self) -> Vec<Row> {
        self.nodes
            .split(|node| matches!(node, Node::AlignPoint(_)))
            .map(|cell| Row::new(cell.iter().cloned()))
            .collect()
    }

    /// A compact description of the row, as [`Formula::repr`] gives it.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out);
        out
    }

    fn write_repr(&self, out: &mut String) {
        out.push('[');
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            node.write_repr(out);
        }
        out.push(']');
    }
}

impl Default for Row {
    fn default() -> Self {
        Self::new([])
    }
}

impl Hash for Row {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        if self.nodes.as_ptr() == other.nodes.as_ptr() && self.nodes.len() == other.nodes.len() {
            return true;
        }
        if self.hash != other.hash || self.size != other.size {
            return false;
        }
        if self.depth < 2 {
            return self.nodes == other.nodes;
        }
        compare_once(
            self.nodes.as_ptr() as usize,
            other.nodes.as_ptr() as usize,
            || self.nodes == other.nodes,
        )
    }
}

impl Eq for Row {}

impl Debug for Row {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Row({})", self.repr())
    }
}

/// The pairs of rows found equal while a comparison runs on this thread,
/// by the address of their nodes, and how many comparisons are running.
type Comparisons = (u32, HashSet<(usize, usize)>);

thread_local! {
    static EQUAL: RefCell<Comparisons> = RefCell::new((0, HashSet::new()));
}

/// Compares two rows whose nodes are at `a` and `b` with `compare`,
/// remembering the pairs found equal until the outermost comparison ends.
/// Two rows built alike, each holding one row many times, are
/// exponentially large trees; comparing each pair of rows once makes
/// comparing them linear. While a comparison runs, both rows are borrowed,
/// so their nodes cannot move.
fn compare_once(a: usize, b: usize, compare: impl FnOnce() -> bool) -> bool {
    struct Leave;

    impl Drop for Leave {
        fn drop(&mut self) {
            EQUAL.with_borrow_mut(|(running, equal)| {
                *running -= 1;
                if *running == 0 {
                    equal.clear();
                }
            });
        }
    }

    let known = EQUAL.with_borrow_mut(|(running, equal)| {
        *running += 1;
        equal.contains(&(a, b))
    });
    let _leave = Leave;
    if known {
        return true;
    }
    let result = compare();
    if result {
        EQUAL.with_borrow_mut(|(_, equal)| equal.insert((a, b)));
    }
    result
}

/// A node of the math tree.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Node {
    /// A variable: a letter, set in italic unless styled otherwise. `x`,
    /// `α` (written `alpha`).
    Ident(Atom),
    /// A number: `25`, `3.14`. Its digits stay together.
    Number(Atom),
    /// A symbol that is neither a letter nor a number: an operator, a
    /// relation, a delimiter, punctuation. Its class decides its spacing.
    Symbol(Atom),
    /// Text in quotes, set upright as words: `"if"`, `"kg"`.
    Text(Atom),
    /// A named operator set upright: `sin`, `log`, `lim`.
    Operator(Operator),
    /// The differential `d` of `dif t`, upright, with the space before it
    /// that separates it from the integrand.
    Differential(Span),
    /// A fraction: `(m v - m u) / t`, `frac(a, b)`.
    Frac(Arc<Frac>),
    /// A base with subscripts, superscripts or primes: `v^2`, `x_i`, `f'`.
    Attach(Arc<Attach>),
    /// An n-ary operator with its limits: `sum_(i=1)^n`, `integral_0^T`.
    Nary(Arc<Nary>),
    /// A root: `sqrt(x)`, `root(3, x)`, `√x`.
    Root(Arc<Root>),
    /// A body between delimiters that grow with it: `(a + b)`, `abs(x)`.
    Delimited(Arc<Delimited>),
    /// A matrix: `mat(1, 2; 3, 4)`.
    Matrix(Arc<Matrix>),
    /// Cases, one per row, behind an opening brace: `cases(x &"if" x > 0, ..)`.
    Cases(Arc<Cases>),
    /// An accent over a base: `tilde(x)`, `dot(x)`.
    Accent(Arc<Accent>),
    /// A vector or a unit vector: `vec(F)`, `hat(i)`. How vectors look,
    /// bold or with an arrow, is a stylesheet choice (SPEC §8.1).
    Vector(Arc<Vector>),
    /// A line or brace over or under a body, with an optional annotation:
    /// `overline(x)`, `underbrace(a + b, n)`.
    Decoration(Arc<Decoration>),
    /// A body in another math alphabet: `bold(x)`, `upright(d)`, `cal(L)`.
    Styled(Arc<Styled>),
    /// An alignment point: `&`. Only the top level of a line or a case
    /// holds them.
    AlignPoint(Span),
}

impl Node {
    /// Where the node was written.
    pub fn span(&self) -> Span {
        match self {
            Self::Ident(atom) | Self::Number(atom) | Self::Symbol(atom) | Self::Text(atom) => {
                atom.span
            }
            Self::Operator(op) => op.span,
            Self::Differential(span) | Self::AlignPoint(span) => *span,
            Self::Frac(frac) => frac.span,
            Self::Attach(attach) => attach.span,
            Self::Nary(nary) => nary.span,
            Self::Root(root) => root.span,
            Self::Delimited(delimited) => delimited.span,
            Self::Matrix(matrix) => matrix.span,
            Self::Cases(cases) => cases.span,
            Self::Accent(accent) => accent.span,
            Self::Vector(vector) => vector.span,
            Self::Decoration(decoration) => decoration.span,
            Self::Styled(styled) => styled.span,
        }
    }

    /// The rows the node holds, in source order.
    pub fn rows(&self) -> Vec<&Row> {
        fn opt(row: &Option<Row>) -> Option<&Row> {
            row.as_ref()
        }
        match self {
            Self::Ident(_)
            | Self::Number(_)
            | Self::Symbol(_)
            | Self::Text(_)
            | Self::Operator(_)
            | Self::Differential(_)
            | Self::AlignPoint(_) => vec![],
            Self::Frac(frac) => vec![&frac.num, &frac.den],
            Self::Attach(attach) => std::iter::once(&attach.base)
                .chain(opt(&attach.sub))
                .chain(opt(&attach.sup))
                .collect(),
            Self::Nary(nary) => opt(&nary.sub).into_iter().chain(opt(&nary.sup)).collect(),
            Self::Root(root) => opt(&root.index)
                .into_iter()
                .chain([&root.radicand])
                .collect(),
            Self::Delimited(delimited) => vec![&delimited.body],
            Self::Matrix(matrix) => matrix.rows.iter().flatten().collect(),
            Self::Cases(cases) => cases.cases.iter().collect(),
            Self::Accent(accent) => vec![&accent.base],
            Self::Vector(vector) => vec![&vector.base],
            Self::Decoration(decoration) => std::iter::once(&decoration.body)
                .chain(opt(&decoration.annotation))
                .collect(),
            Self::Styled(styled) => vec![&styled.body],
        }
    }

    /// How deeply the node nests: as deeply as its deepest row, or `0` for
    /// a node without rows.
    pub fn depth(&self) -> u32 {
        self.rows().into_iter().map(Row::depth).max().unwrap_or(0)
    }

    /// The number of nodes in the node's rows (saturating).
    fn size(&self) -> u64 {
        self.rows()
            .into_iter()
            .fold(0u64, |size, row| size.saturating_add(row.size()))
    }

    fn write_repr(&self, out: &mut String) {
        let row = |out: &mut String, row: &Row| row.write_repr(out);
        let opt = |out: &mut String, key: &str, row: &Option<Row>| {
            if let Some(row) = row {
                write!(out, ", {key}: ").unwrap();
                row.write_repr(out);
            }
        };
        match self {
            Self::Ident(atom) => out.push_str(&atom.text),
            Self::Number(atom) => write!(out, "num({})", atom.text).unwrap(),
            Self::Symbol(atom) => write!(out, "{}({})", class_name(atom.class), atom.text).unwrap(),
            Self::Text(atom) => write!(out, "text({:?})", atom.text).unwrap(),
            Self::Operator(op) => {
                write!(out, "op({}", op.name).unwrap();
                if op.limits {
                    out.push_str(", limits");
                }
                out.push(')');
            }
            Self::Differential(_) => out.push_str("dif"),
            Self::Frac(frac) => {
                out.push_str("frac(");
                row(out, &frac.num);
                out.push_str(", ");
                row(out, &frac.den);
                out.push(')');
            }
            Self::Attach(attach) => {
                out.push_str("attach(");
                row(out, &attach.base);
                opt(out, "sub", &attach.sub);
                opt(out, "sup", &attach.sup);
                if attach.primes > 0 {
                    write!(out, ", primes: {}", attach.primes).unwrap();
                }
                out.push(')');
            }
            Self::Nary(nary) => {
                write!(out, "nary({}", nary.op).unwrap();
                opt(out, "sub", &nary.sub);
                opt(out, "sup", &nary.sup);
                out.push(')');
            }
            Self::Root(root) => {
                out.push_str("root(");
                if let Some(index) = &root.index {
                    out.push_str("index: ");
                    row(out, index);
                    out.push_str(", ");
                }
                row(out, &root.radicand);
                out.push(')');
            }
            Self::Delimited(delimited) => {
                out.push_str("delim(");
                write_delim(out, delimited.open);
                out.push_str(", ");
                row(out, &delimited.body);
                out.push_str(", ");
                write_delim(out, delimited.close);
                out.push(')');
            }
            Self::Matrix(matrix) => {
                out.push_str("mat(");
                write_delim(out, matrix.open);
                for cells in &matrix.rows {
                    out.push_str(", ");
                    out.push('[');
                    for (i, cell) in cells.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        row(out, cell);
                    }
                    out.push(']');
                }
                out.push_str(", ");
                write_delim(out, matrix.close);
                out.push(')');
            }
            Self::Cases(cases) => {
                out.push_str("cases(");
                for (i, case) in cases.cases.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    row(out, case);
                }
                out.push(')');
            }
            Self::Accent(accent) => {
                write!(out, "accent({}, ", accent.accent).unwrap();
                row(out, &accent.base);
                out.push(')');
            }
            Self::Vector(vector) => {
                out.push_str(if vector.unit { "unit-vec(" } else { "vec(" });
                row(out, &vector.base);
                out.push(')');
            }
            Self::Decoration(decoration) => {
                write!(out, "{}(", decoration.kind.name()).unwrap();
                row(out, &decoration.body);
                opt(out, "annotation", &decoration.annotation);
                out.push(')');
            }
            Self::Styled(styled) => {
                write!(out, "{}(", styled.variant.name()).unwrap();
                row(out, &styled.body);
                out.push(')');
            }
            Self::AlignPoint(_) => out.push('&'),
        }
    }
}

fn write_delim(out: &mut String, delim: Option<char>) {
    match delim {
        Some(c) => write!(out, "{c:?}").unwrap(),
        None => out.push_str("none"),
    }
}

/// The short name of a class, as [`Formula::repr`] writes it.
fn class_name(class: MathClass) -> &'static str {
    match class {
        MathClass::Normal => "sym",
        MathClass::Alphabetic => "alpha",
        MathClass::Binary => "bin",
        MathClass::Closing => "close",
        MathClass::Diacritic => "diacritic",
        MathClass::Fence => "fence",
        MathClass::GlyphPart => "part",
        MathClass::Large => "large",
        MathClass::Opening => "open",
        MathClass::Punctuation => "punct",
        MathClass::Relation => "rel",
        MathClass::Space => "space",
        MathClass::Unary => "unary",
        MathClass::Vary => "vary",
        MathClass::Special => "special",
    }
}

/// The Unicode math class of a character (SPEC §8.2), `Normal` for a
/// character without one.
pub fn class(c: char) -> MathClass {
    unicode_math_class::class(c).unwrap_or(MathClass::Normal)
}

/// A leaf of the tree: a letter, a number, a symbol or quoted text.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Atom {
    /// The text: one character or grapheme cluster, the digits of a
    /// number, or the words of quoted text.
    pub text: EcoString,
    /// The math class that spaces it from its neighbours (SPEC §8.2): that
    /// of its character, `Alphabetic` for letters, `Normal` for numbers and
    /// text.
    pub class: MathClass,
    /// Where it was written.
    pub span: Span,
}

/// A named operator: `sin`, `lim`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Operator {
    /// Its name, as set: `sin`, `lim`, `max`.
    pub name: EcoString,
    /// Whether attachments go below and above it in display style, like
    /// the limits of `lim_(x -> 0)`, rather than beside it.
    pub limits: bool,
    /// Where it was written.
    pub span: Span,
}

/// A fraction.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Frac {
    /// The numerator, without the parentheses that grouped it in `(a) / b`.
    pub num: Row,
    /// The denominator, likewise.
    pub den: Row,
    /// Where it was written.
    pub span: Span,
}

/// A base with attachments.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Attach {
    /// The base.
    pub base: Row,
    /// The subscript, or the lower limit of an operator with limits.
    pub sub: Option<Row>,
    /// The superscript, or the upper limit.
    pub sup: Option<Row>,
    /// The number of primes after the base: `f''` has 2. They come before
    /// the superscript.
    pub primes: u8,
    /// Where it was written.
    pub span: Span,
}

/// An n-ary operator such as a sum, a product or an integral, with its
/// limits. Its operand is not part of the node: it is what follows the
/// operator in its row, up to the next relation or binary operator, which
/// is what the OMML writer puts in `m:e`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Nary {
    /// The operator: `∑`, `∫`.
    pub op: char,
    /// The lower limit: `i = 1` of `sum_(i=1)^n`.
    pub sub: Option<Row>,
    /// The upper limit.
    pub sup: Option<Row>,
    /// Where it was written.
    pub span: Span,
}

/// A root.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Root {
    /// The index: `3` of a cube root, none for a square root.
    pub index: Option<Row>,
    /// The radicand.
    pub radicand: Row,
    /// Where it was written.
    pub span: Span,
}

/// A body between delimiters that grow with it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Delimited {
    /// The opening delimiter, or none for a body closed on one side only.
    pub open: Option<char>,
    /// The body.
    pub body: Row,
    /// The closing delimiter.
    pub close: Option<char>,
    /// Where it was written.
    pub span: Span,
}

/// A matrix of cells, row by row, with its delimiters.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Matrix {
    /// The rows, each of the same number of cells.
    pub rows: Vec<Vec<Row>>,
    /// The opening delimiter: `(` by default.
    pub open: Option<char>,
    /// The closing delimiter: `)` by default.
    pub close: Option<char>,
    /// Where it was written.
    pub span: Span,
}

/// Cases behind an opening brace, one row each. A case's top level may hold
/// [`Node::AlignPoint`]s, which align its parts with those of the other
/// cases.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Cases {
    /// The cases, in order.
    pub cases: Vec<Row>,
    /// Where it was written.
    pub span: Span,
}

/// An accent over a base.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Accent {
    /// The base.
    pub base: Row,
    /// The accent, as its combining character: U+0303 for a tilde.
    pub accent: char,
    /// Where it was written.
    pub span: Span,
}

/// A vector or a unit vector. Semantic: the stylesheet decides whether it
/// is set bold or with an arrow, or with a hat for a unit vector (SPEC
/// §8.1).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Vector {
    /// The vector's symbol: `F` of `vec(F)`.
    pub base: Row,
    /// Whether it is a unit vector: `hat(i)`.
    pub unit: bool,
    /// Where it was written.
    pub span: Span,
}

/// A line or brace over or under a body.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Decoration {
    /// What is drawn, and where.
    pub kind: DecorationKind,
    /// The body.
    pub body: Row,
    /// An annotation beyond a brace or bracket: `n` of `underbrace(x, n)`.
    pub annotation: Option<Row>,
    /// Where it was written.
    pub span: Span,
}

/// What a [`Decoration`] draws.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DecorationKind {
    /// A line over the body.
    Overline,
    /// A line under the body.
    Underline,
    /// A brace over the body.
    Overbrace,
    /// A brace under the body.
    Underbrace,
    /// A bracket over the body.
    Overbracket,
    /// A bracket under the body.
    Underbracket,
}

impl DecorationKind {
    /// Its name in native math: `overline`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Overline => "overline",
            Self::Underline => "underline",
            Self::Overbrace => "overbrace",
            Self::Underbrace => "underbrace",
            Self::Overbracket => "overbracket",
            Self::Underbracket => "underbracket",
        }
    }

    /// Whether it is drawn over the body.
    pub fn is_over(self) -> bool {
        matches!(self, Self::Overline | Self::Overbrace | Self::Overbracket)
    }
}

/// A body in another math alphabet.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Styled {
    /// The alphabet.
    pub variant: MathVariant,
    /// The body.
    pub body: Row,
    /// Where it was written.
    pub span: Span,
}

/// A math alphabet: the style letters and digits are set in.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum MathVariant {
    /// Upright serif: `upright(d)`.
    Upright,
    /// Italic serif, which single letters have by default: `italic(2)`.
    Italic,
    /// Bold upright serif: `bold(x)`.
    Bold,
    /// Calligraphic or script: `cal(L)`.
    Calligraphic,
    /// Fraktur: `frak(g)`.
    Fraktur,
    /// Double-struck, for number sets: `bb(R)`.
    DoubleStruck,
    /// Sans-serif: `sans(x)`.
    SansSerif,
    /// Monospace: `mono(x)`.
    Monospace,
}

impl MathVariant {
    /// Its name in native math: `upright`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Upright => "upright",
            Self::Italic => "italic",
            Self::Bold => "bold",
            Self::Calligraphic => "cal",
            Self::Fraktur => "frak",
            Self::DoubleStruck => "bb",
            Self::SansSerif => "sans",
            Self::Monospace => "mono",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(text: &str) -> Node {
        Node::Ident(Atom {
            text: text.into(),
            class: MathClass::Alphabetic,
            span: Span::detached(),
        })
    }

    fn frac(num: Row, den: Row) -> Node {
        Node::Frac(Arc::new(Frac {
            num,
            den,
            span: Span::detached(),
        }))
    }

    #[test]
    fn rows_built_alike_compare_and_hash_in_linear_time() {
        // Two rows, each a tree of 2^60 nodes built from shared parts.
        let (mut a, mut b, mut c) = (
            Row::new([ident("x")]),
            Row::new([ident("x")]),
            Row::new([ident("y")]),
        );
        for _ in 0..60 {
            a = Row::new([frac(a.clone(), a)]);
            b = Row::new([frac(b.clone(), b)]);
            c = Row::new([frac(c.clone(), c)]);
        }
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.depth(), 61);
        assert_eq!(a.size(), (1 << 61) - 1);
        let hash = |row: &Row| {
            let mut state = DefaultHasher::new();
            row.hash(&mut state);
            state.finish()
        };
        assert_eq!(hash(&a), hash(&b));
    }

    #[test]
    fn cells_split_at_alignment_points() {
        let row = Row::new([ident("a"), Node::AlignPoint(Span::detached()), ident("b")]);
        assert_eq!(row.cells().len(), 2);
        assert_eq!(row.repr(), "[a, &, b]");
    }
}
