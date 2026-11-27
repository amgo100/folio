//! A formula as text to be read: the alternate description of a PDF
//! `Formula` (SPEC §8.3, §11.1).
//!
//! The reading is written from the math tree, never from the source, so
//! that only what evaluation made of a formula reaches it: no comments, no
//! script code, and nothing another variant would show (SPEC §6.4). It is
//! linear and close to native math, so that it reads as the formula is
//! written and set:
//!
//! - atoms as they are set, separated by spaces, with no space after an
//!   opening delimiter or before a closing one or punctuation:
//!   `v = u + a t`, `f(x)`;
//! - fractions as `num/den`, attachments as `base_sub^sup`, primes as
//!   `'`, with a part that is more than one atom in parentheses:
//!   `(m v − m u)/t`, `x_(i + 1)^2`;
//! - n-ary operators as their symbol with their limits: `∑_(i = 1)^n`;
//! - everything else as the native function that makes it: `sqrt(x)`,
//!   `root(3, x)`, `vec(F)`, `hat(i)`, `tilde(x)`, `overline(x)`,
//!   `bold(x)`, `mat(1, 2; 3, 4)` (with `delim: "|"; ` first for other
//!   delimiters), `cases(..)`, and an accent without a name of its own as
//!   `accent(x, ^)` or `accent(x, U+20E1)`;
//! - quantities as they are set in the formula's number format, without
//!   the separators of digit groups: `9.81 m s^(−2)`;
//! - lines separated by `; `, without their labels or alignment points.
//!
//! The reading is at most [`MAX_LEN`] bytes long, so that a formula built
//! from shared parts, which may be exponentially large, is read in time
//! linear in the reading.

use crate::{Formula, MathClass, Node, NumberFormat, Row};

/// The most bytes a reading has before it is cut short with `…`.
pub const MAX_LEN: usize = 16 * 1024;

impl Formula {
    /// The formula as text to be read, with its quantities in `format`
    /// (see the [module documentation](self)). An empty formula reads as
    /// the empty string.
    pub fn reading(&self, format: &NumberFormat) -> String {
        let mut reader = Reader {
            out: String::new(),
            space: false,
            operand: true,
            scripted: false,
            full: false,
            format,
        };
        for (i, line) in self.lines().iter().enumerate() {
            if i > 0 {
                reader.semicolon();
            }
            reader.row(&line.row);
        }
        if reader.full {
            reader.out.push('…');
        }
        reader.out
    }
}

/// Writes a reading.
struct Reader<'a> {
    out: String,
    /// Whether the last token may be followed by a space.
    space: bool,
    /// Whether an operand comes next: at the start, and after an operator,
    /// an opening or punctuation. A sign there is a prefix, and an opening
    /// is spaced from what comes before.
    operand: bool,
    /// Whether the last token ends a fraction or an attachment, which an
    /// opening is spaced from, so that `1/2 (a + b)` does not read as
    /// `1/2(a + b)`.
    scripted: bool,
    /// Whether the reading reached [`MAX_LEN`].
    full: bool,
    format: &'a NumberFormat,
}

impl Reader<'_> {
    /// Writes a token, after a space if `before` and the last token allow
    /// one; `after` says whether the next may follow after a space.
    fn token(&mut self, text: &str, before: bool, after: bool) {
        if self.full || text.is_empty() {
            return;
        }
        if self.out.len() + text.len() + 1 > MAX_LEN {
            self.full = true;
            return;
        }
        if before && self.space && !self.out.is_empty() {
            self.out.push(' ');
        }
        // Whitespace inside an atom reads as one space.
        let mut words = text.split_whitespace();
        if let Some(first) = words.next() {
            self.out.push_str(first);
            for word in words {
                self.out.push(' ');
                self.out.push_str(word);
            }
        }
        self.space = after;
        self.operand = !after;
        self.scripted = false;
    }

    /// An operator, spaced on both sides, after which an operand follows.
    fn operator(&mut self, text: &str) {
        self.token(text, true, true);
        self.operand = true;
    }

    /// An ordinary token, spaced on both sides.
    fn word(&mut self, text: &str) {
        self.token(text, true, true);
    }

    /// The opening of a function: `sqrt(`.
    fn open(&mut self, name: &str) {
        if !self.full {
            let mut text = String::with_capacity(name.len() + 1);
            text.push_str(name);
            text.push('(');
            self.token(&text, true, false);
        }
    }

    /// The end of a function or a parenthesised part.
    fn close(&mut self) {
        self.token(")", false, true);
    }

    /// A comma between arguments.
    fn comma(&mut self) {
        self.token(",", false, true);
        self.operand = true;
    }

    /// A semicolon between lines or the rows of a matrix.
    fn semicolon(&mut self) {
        self.token(";", false, true);
        self.operand = true;
    }

    /// A function of rows, separated by commas: `root(3, x)`.
    fn func(&mut self, name: &str, args: &[&Row]) {
        self.open(name);
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                self.comma();
            }
            self.row(arg);
        }
        self.close();
    }

    /// The nodes of a row, in order.
    fn row(&mut self, row: &Row) {
        for node in row.nodes() {
            if self.full {
                return;
            }
            self.node(node);
        }
    }

    /// A row that is part of a fraction or an attachment, glued to what
    /// comes before it, in parentheses unless it reads as one token.
    fn part(&mut self, row: &Row) {
        self.space = false;
        self.operand = true;
        if is_simple(row) {
            self.row(row);
        } else {
            self.token("(", false, false);
            self.row(row);
            self.close();
        }
    }

    /// A glue token: no space on either side, as `^` or `/`.
    fn glue(&mut self, text: &str) {
        self.token(text, false, false);
    }

    fn node(&mut self, node: &Node) {
        match node {
            Node::Ident(atom) | Node::Text(atom) => self.word(&atom.text),
            Node::Number(atom) => {
                // Digit groups read as one number.
                let digits: String = atom.text.chars().filter(|c| !c.is_whitespace()).collect();
                self.word(&digits);
            }
            Node::Symbol(atom) => match atom.class {
                MathClass::Space => {}
                MathClass::Opening => self.token(&atom.text, self.operand || self.scripted, false),
                MathClass::Closing => self.token(&atom.text, false, true),
                MathClass::Punctuation => {
                    self.token(&atom.text, false, true);
                    self.operand = true;
                }
                // A sign before an operand: `−2`, `x = −1`.
                _ if self.operand && is_sign(&atom.text) => self.token(&atom.text, true, false),
                MathClass::Binary
                | MathClass::Relation
                | MathClass::Large
                | MathClass::Vary
                | MathClass::Unary => self.operator(&atom.text),
                _ => self.word(&atom.text),
            },
            Node::Operator(op) => self.word(&op.name),
            Node::Differential(_) => self.word("d"),
            Node::Frac(frac) => {
                self.space_before();
                self.part(&frac.num);
                self.glue("/");
                self.part(&frac.den);
                self.space = true;
                self.operand = false;
                self.scripted = true;
            }
            Node::Attach(attach) => {
                self.space_before();
                self.part(&attach.base);
                for _ in 0..attach.primes {
                    self.glue("'");
                }
                if let Some(sub) = &attach.sub {
                    self.glue("_");
                    self.part(sub);
                }
                if let Some(sup) = &attach.sup {
                    self.glue("^");
                    self.part(sup);
                }
                self.space = true;
                self.operand = false;
                self.scripted = true;
            }
            Node::Nary(nary) => {
                let mut op = [0; 4];
                self.word(nary.op.encode_utf8(&mut op));
                if let Some(sub) = &nary.sub {
                    self.glue("_");
                    self.part(sub);
                }
                if let Some(sup) = &nary.sup {
                    self.glue("^");
                    self.part(sup);
                }
                self.space = true;
                self.operand = true;
            }
            Node::Root(root) => match &root.index {
                None => self.func("sqrt", &[&root.radicand]),
                Some(index) => self.func("root", &[index, &root.radicand]),
            },
            Node::Delimited(delimited) => {
                let mut buf = [0; 4];
                if let Some(open) = delimited.open {
                    self.token(
                        open.encode_utf8(&mut buf),
                        self.operand || self.scripted,
                        false,
                    );
                }
                self.row(&delimited.body);
                if let Some(close) = delimited.close {
                    self.token(close.encode_utf8(&mut buf), false, true);
                }
            }
            Node::Matrix(matrix) => {
                self.open("mat");
                // Delimiters other than parentheses mean something: a
                // determinant, a norm.
                if (matrix.open, matrix.close) != (Some('('), Some(')')) {
                    let delim = match matrix.open {
                        None => "none".to_string(),
                        Some(open) => format!("\"{open}\""),
                    };
                    self.token("delim:", true, true);
                    self.word(&delim);
                    self.semicolon();
                }
                for (i, cells) in matrix.rows.iter().enumerate() {
                    if i > 0 {
                        self.semicolon();
                    }
                    for (j, cell) in cells.iter().enumerate() {
                        if j > 0 {
                            self.comma();
                        }
                        self.row(cell);
                    }
                }
                self.close();
            }
            Node::Cases(cases) => {
                let rows: Vec<&Row> = cases.cases.iter().collect();
                self.func("cases", &rows);
            }
            Node::Accent(accent) => match accent_name(accent.accent) {
                Some(name) => self.func(name, &[&accent.base]),
                None => {
                    // A combining mark alone would combine with the space
                    // before it.
                    self.open("accent");
                    self.row(&accent.base);
                    self.comma();
                    let mark = match accent.accent {
                        '\u{302}' => "^".to_string(),
                        c => format!("U+{:04X}", u32::from(c)),
                    };
                    self.word(&mark);
                    self.close();
                }
            },
            Node::Vector(vector) => {
                self.func(if vector.unit { "hat" } else { "vec" }, &[&vector.base]);
            }
            Node::Decoration(decoration) => match &decoration.annotation {
                None => self.func(decoration.kind.name(), &[&decoration.body]),
                Some(annotation) => {
                    self.func(decoration.kind.name(), &[&decoration.body, annotation]);
                }
            },
            Node::Styled(styled) => self.func(styled.variant.name(), &[&styled.body]),
            Node::AlignPoint(_) => {}
            Node::Quantity(quantity) => self.row(&quantity.lower(self.format)),
        }
    }

    /// Writes the space before a node that glues its parts together, as an
    /// ordinary token would have one.
    fn space_before(&mut self) {
        if !self.full && self.space && !self.out.is_empty() {
            if self.out.len() + 1 > MAX_LEN {
                self.full = true;
                return;
            }
            self.out.push(' ');
        }
        self.space = false;
    }
}

/// Whether an atom is a sign that may stand before an operand.
fn is_sign(text: &str) -> bool {
    matches!(text, "−" | "-" | "+" | "±" | "∓")
}

/// Whether a row reads as one token, so that it needs no parentheses as a
/// part of a fraction or an attachment.
fn is_simple(row: &Row) -> bool {
    match row.nodes() {
        [node] => match node {
            // A number's digit groups read as one number.
            Node::Number(_) => true,
            Node::Ident(atom) | Node::Text(atom) | Node::Symbol(atom) => {
                !atom.text.chars().any(char::is_whitespace)
            }
            Node::Operator(_)
            | Node::Differential(_)
            | Node::Root(_)
            | Node::Matrix(_)
            | Node::Cases(_)
            | Node::Accent(_)
            | Node::Vector(_)
            | Node::Decoration(_)
            | Node::Styled(_) => true,
            Node::Delimited(delimited) => delimited.open.is_some() && delimited.close.is_some(),
            Node::Frac(_)
            | Node::Attach(_)
            | Node::Nary(_)
            | Node::AlignPoint(_)
            | Node::Quantity(_) => false,
        },
        _ => false,
    }
}

/// The native name of an accent, as `tilde(x)` writes it.
fn accent_name(accent: char) -> Option<&'static str> {
    Some(match accent {
        '\u{300}' => "grave",
        '\u{301}' => "acute",
        '\u{303}' => "tilde",
        '\u{304}' | '\u{305}' => "macron",
        '\u{306}' => "breve",
        '\u{307}' => "dot",
        '\u{308}' => "dot.double",
        '\u{20DB}' => "dot.triple",
        '\u{30A}' => "circle",
        '\u{30C}' => "caron",
        '\u{20D7}' => "arrow",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use folio_syntax::Span;

    use super::*;
    use crate::{Frac, Quantity, tex};

    fn read(latex: &str) -> String {
        let formula = tex::parse(latex, Span::detached(), 256).unwrap();
        formula.reading(&NumberFormat::POINT)
    }

    #[test]
    fn atoms_are_spaced_as_written() {
        assert_eq!(read("v = u + a t"), "v = u + a t");
        assert_eq!(read("f(x) = x, y"), "f(x) = x, y");
        assert_eq!(read("\\sin x"), "sin x");
        assert_eq!(read("x = -1 + 2 (a - b)"), "x = −1 + 2(a − b)");
        assert_eq!(
            read("\\frac{1}{2} (a + b) + x^2 (c)"),
            "1/2 (a + b) + x^2 (c)"
        );
    }

    #[test]
    fn parts_of_more_than_one_atom_are_parenthesised() {
        assert_eq!(read("\\frac{1}{2}"), "1/2");
        assert_eq!(read("\\frac{m v - m u}{t}"), "(m v − m u)/t");
        assert_eq!(read("x_{i+1}^2 + y'"), "x_(i + 1)^2 + y'");
        assert_eq!(read("E = m c^2"), "E = m c^2");
        assert_eq!(read("\\sum_{i=1}^n i"), "∑_(i = 1)^n i");
    }

    #[test]
    fn functions_read_as_native_math() {
        assert_eq!(read("\\sqrt{x}"), "sqrt(x)");
        assert_eq!(read("\\sqrt[3]{x + 1}"), "root(3, x + 1)");
        assert_eq!(read("\\tilde{x}"), "tilde(x)");
        assert_eq!(read("\\hat{\\theta}"), "accent(θ, ^)");
        assert_eq!(read("\\overline{a b}"), "overline(a b)");
        assert_eq!(read("\\mathbf{x}"), "bold(x)");
        assert_eq!(
            read("\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}"),
            "mat(1, 2; 3, 4)"
        );
        assert_eq!(
            read("\\begin{vmatrix} 1 & 2 \\\\ -3 & 4 \\end{vmatrix}"),
            "mat(delim: \"|\"; 1, 2; −3, 4)"
        );
    }

    #[test]
    fn quantities_read_in_their_number_format() {
        let quantity = Quantity {
            value: Some(crate::Number::parse("12345.5").unwrap()),
            pm: None,
            unit: Some(crate::Unit::parse("m s^-2").unwrap()),
            span: Span::detached(),
        };
        let formula = Formula::row(
            Row::new([Node::Quantity(Arc::new(quantity))]),
            Span::detached(),
        );
        assert_eq!(formula.reading(&NumberFormat::COMMA), "12345,5 m s^(−2)");
    }

    #[test]
    fn lines_are_separated_and_labels_left_out() {
        assert_eq!(read("a &= b \\\\ c &= d"), "a = b; c = d");
        assert_eq!(read("a = b \\\\ -c = (d)"), "a = b; −c = (d)");
    }

    #[test]
    fn a_formula_of_shared_parts_is_cut_short() {
        let mut row = Row::new([Node::symbol('x', Span::detached())]);
        for _ in 0..60 {
            row = Row::new([Node::Frac(Arc::new(Frac {
                num: row.clone(),
                den: row,
                span: Span::detached(),
            }))]);
        }
        let reading = Formula::row(row, Span::detached()).reading(&NumberFormat::POINT);
        assert!(reading.len() <= MAX_LEN + '…'.len_utf8());
        assert!(reading.ends_with('…'));
    }
}
