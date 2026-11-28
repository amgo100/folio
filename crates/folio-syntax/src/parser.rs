// Derived from typst-syntax v0.15.1, crates/typst-syntax/src/parser.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

use std::mem;
use std::ops::{DerefMut, Index, IndexMut, Range};

use ecow::{EcoString, eco_format};
use rustc_hash::{FxHashMap, FxHashSet};
use unicode_math_class::MathClass;

use crate::set::{SyntaxSet, syntax_set};
use crate::utils::{default_math_class, defer};
use crate::{Lexer, SyntaxKind, SyntaxMode, SyntaxNode, ast, set};

mod style;

pub use self::style::parse_stylesheet;
pub(super) use self::style::{reparse_style_block, reparse_stylesheet_items};

// Picked by gut feeling.
const MAX_DEPTH: u32 = 256;

/// How tall an operator chain may make the tree (#380). Nesting alone within
/// `MAX_DEPTH` makes a tree about three levels tall per level of depth (766
/// for `#f(` nested 255 times), and short operator chains at each level of
/// a formula make it up to about five (`a(…)^2/x` nested), or six with a
/// signed script, which wraps its sign and operand (`a(…)^-2/x`). This leaves room
/// for both, so in practice only a long chain reaches it; math that deep is
/// past the evaluator's limit of 100 rows in any case. The bound depends on
/// nothing around the chain, only on its operands. A cut chain is an error,
/// and the reparser parses in full an edit to a tree with an error or one
/// that makes one (`reparser::reusable`), so an incremental reparse decides
/// as a full parse does.
const MAX_CHAIN_HEIGHT: usize = 8 * MAX_DEPTH as usize;

/// Parses a source file as top-level markup.
pub fn parse(text: &str) -> SyntaxNode {
    let mut p = Parser::new(text, 0, SyntaxMode::Markup);
    markup_exprs(&mut p, true, syntax_set!(End));
    p.finish_into(SyntaxKind::Markup)
}

/// Parses top-level code.
pub fn parse_code(text: &str) -> SyntaxNode {
    let mut p = Parser::new(text, 0, SyntaxMode::Code);
    code_exprs(&mut p, syntax_set!(End));
    p.finish_into(SyntaxKind::Code)
}

/// Parses top-level math.
pub fn parse_math(text: &str) -> SyntaxNode {
    let mut p = Parser::new(text, 0, SyntaxMode::Math);
    math_exprs(&mut p, syntax_set!(End));
    p.finish_into(SyntaxKind::Math)
}

/// Parses markup expressions until a stop condition is met.
fn markup(p: &mut Parser<'_>, at_start: bool, wrap_trivia: bool, stop_set: SyntaxSet) {
    let m = if wrap_trivia {
        p.before_trivia()
    } else {
        p.marker()
    };
    markup_exprs(p, at_start, stop_set);
    if wrap_trivia {
        p.flush_trivia();
    }
    p.wrap(m, SyntaxKind::Markup);
}

/// Parses a sequence of markup expressions.
fn markup_exprs(p: &mut Parser<'_>, mut at_start: bool, stop_set: SyntaxSet) {
    debug_assert!(stop_set.contains(SyntaxKind::End));
    let Some(p) = p.check_depth_until(stop_set) else {
        return;
    };

    at_start |= p.had_newline();
    let mut nesting: usize = 0;
    // Keep going if we're at a nested right-bracket regardless of the stop set.
    while !p.at_set(stop_set) || (nesting > 0 && p.at(SyntaxKind::RightBracket)) {
        markup_expr(p, at_start, &mut nesting);
        at_start = p.had_newline();
    }
}

/// Reparses a subsection of markup incrementally.
pub(super) fn reparse_markup(
    text: &str,
    range: Range<usize>,
    at_start: &mut bool,
    nesting: &mut usize,
    top_level: bool,
) -> Option<Vec<SyntaxNode>> {
    let mut p = Parser::new(text, range.start, SyntaxMode::Markup);
    *at_start |= p.had_newline();
    while !p.end() && p.current_start() < range.end {
        // If not top-level and at a new RightBracket, stop the reparse.
        if !top_level && *nesting == 0 && p.at(SyntaxKind::RightBracket) {
            break;
        }
        markup_expr(&mut p, *at_start, nesting);
        *at_start = p.had_newline();
    }
    (p.balanced && p.current_start() == range.end).then(|| p.finish())
}

/// Parses a single markup expression. This includes markup elements like text,
/// headings, strong/emph, lists/enums, etc. This is also the entry point for
/// parsing math equations and embedded code expressions.
fn markup_expr(p: &mut Parser<'_>, at_start: bool, nesting: &mut usize) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };

    match p.current() {
        SyntaxKind::LeftBracket => {
            *nesting += 1;
            p.convert_and_eat(SyntaxKind::Text);
        }
        SyntaxKind::RightBracket if *nesting > 0 => {
            *nesting -= 1;
            p.convert_and_eat(SyntaxKind::Text);
        }
        SyntaxKind::RightBracket => {
            p.unexpected();
            p.hint("try using a backslash escape: \\]");
        }

        SyntaxKind::Shebang => p.eat(),

        SyntaxKind::Text
        | SyntaxKind::Linebreak
        | SyntaxKind::Escape
        | SyntaxKind::Shorthand
        | SyntaxKind::SmartQuote
        | SyntaxKind::Link
        | SyntaxKind::Label => p.eat(),

        SyntaxKind::Raw => p.eat(), // Raw is handled entirely in the Lexer.
        SyntaxKind::TexEquation => p.eat(), // So is LaTeX math.

        SyntaxKind::Hash => embedded_code_expr(p),
        SyntaxKind::Star => strong(p),
        SyntaxKind::Underscore => emph(p),
        SyntaxKind::HeadingMarker if at_start => heading(p),
        SyntaxKind::ListMarker if at_start => list_item(p),
        SyntaxKind::EnumMarker if at_start => enum_item(p),
        SyntaxKind::TermMarker if at_start => term_item(p),
        SyntaxKind::TablePipe if at_start && p.at_line_start() => table_row(p),
        SyntaxKind::RefMarker => reference(p),
        SyntaxKind::Dollar => equation(p),

        SyntaxKind::HeadingMarker
        | SyntaxKind::ListMarker
        | SyntaxKind::EnumMarker
        | SyntaxKind::TermMarker
        | SyntaxKind::TablePipe
        | SyntaxKind::Colon => p.convert_and_eat(SyntaxKind::Text),

        _ => p.unexpected(),
    }
}

/// Parses strong content: `*Strong*`.
fn strong(p: &mut Parser<'_>) {
    // In a cell of a table row, it ends at a pipe and at the end of the
    // line, as the cell does.
    let (nl_mode, stop) = if p.in_cell {
        (
            AtNewline::Stop,
            syntax_set!(Star, TablePipe, RightBracket, End),
        )
    } else {
        (
            AtNewline::StopParBreak,
            syntax_set!(Star, RightBracket, End),
        )
    };
    p.with_nl_mode(nl_mode, |p| {
        let m = p.marker();
        p.assert(SyntaxKind::Star);
        markup(p, false, true, stop);
        let had_closing = p.expect_closing_delimiter(m, SyntaxKind::Star);
        p.wrap(m, SyntaxKind::Strong);
        if had_closing && p[m].len() == 2 {
            p[m].warn("no text within stars");
            let hint = "using multiple consecutive stars (e.g. **) has no \
                        additional effect";
            p[m].hint(hint);
        }
    });
}

/// Parses emphasized content: `_Emphasized_`.
fn emph(p: &mut Parser<'_>) {
    // In a cell of a table row, it ends at a pipe and at the end of the
    // line, as the cell does.
    let (nl_mode, stop) = if p.in_cell {
        (
            AtNewline::Stop,
            syntax_set!(Underscore, TablePipe, RightBracket, End),
        )
    } else {
        (
            AtNewline::StopParBreak,
            syntax_set!(Underscore, RightBracket, End),
        )
    };
    p.with_nl_mode(nl_mode, |p| {
        let m = p.marker();
        p.assert(SyntaxKind::Underscore);
        markup(p, false, true, stop);
        let had_closing = p.expect_closing_delimiter(m, SyntaxKind::Underscore);
        p.wrap(m, SyntaxKind::Emph);
        if had_closing && p[m].len() == 2 {
            p[m].warn("no text within underscores");
            let hint = "using multiple consecutive underscores (e.g. __) has no \
                        additional effect";
            p[m].hint(hint);
        }
    });
}

/// Parses a section heading: `= Introduction`.
fn heading(p: &mut Parser<'_>) {
    p.with_nl_mode(AtNewline::Stop, |p| {
        let m = p.marker();
        p.assert(SyntaxKind::HeadingMarker);
        markup(p, false, false, syntax_set!(Label, RightBracket, End));
        p.wrap(m, SyntaxKind::Heading);
    });
}

/// Parses an item in a bullet list: `- ...`.
fn list_item(p: &mut Parser<'_>) {
    p.with_nl_mode(AtNewline::RequireColumn(p.current_column()), |p| {
        let m = p.marker();
        p.assert(SyntaxKind::ListMarker);
        markup(p, true, false, syntax_set!(RightBracket, End));
        p.wrap(m, SyntaxKind::ListItem);
    });
}

/// Parses an item in an enumeration (numbered list): `+ ...` or `1. ...`.
fn enum_item(p: &mut Parser<'_>) {
    p.with_nl_mode(AtNewline::RequireColumn(p.current_column()), |p| {
        let m = p.marker();
        p.assert(SyntaxKind::EnumMarker);
        markup(p, true, false, syntax_set!(RightBracket, End));
        p.wrap(m, SyntaxKind::EnumItem);
    });
}

/// Parses an item in a term list: `/ Term: Details`.
fn term_item(p: &mut Parser<'_>) {
    p.with_nl_mode(AtNewline::RequireColumn(p.current_column()), |p| {
        let m = p.marker();
        p.with_nl_mode(AtNewline::Stop, |p| {
            p.assert(SyntaxKind::TermMarker);
            markup(p, false, false, syntax_set!(Colon, RightBracket, End));
        });
        p.expect(SyntaxKind::Colon);
        markup(p, true, false, syntax_set!(RightBracket, End));
        p.wrap(m, SyntaxKind::TermItem);
    });
}

/// Parses a table row: `| a | b |` (SPEC §7.4). The row ends at the end of
/// its line, or at the bracket that closes the content block it is in. Its
/// cells are the markup between its pipes, trimmed of spaces: `||` holds an
/// empty cell, and text after the last pipe is a cell too.
fn table_row(p: &mut Parser<'_>) {
    p.with_nl_mode(AtNewline::Stop, |p| {
        let m = p.marker();
        while p.eat_if(SyntaxKind::TablePipe) {
            if p.at_set(syntax_set!(RightBracket, End)) {
                break;
            }
            table_cell(p);
        }
        p.wrap(m, SyntaxKind::TableRow);
    });
}

/// Parses a cell of a table row, up to the next pipe. A pipe inside maths,
/// raw text, embedded code or a content block belongs to that, and one
/// inside brackets of the cell's text does not end the cell either.
fn table_cell(p: &mut Parser<'_>) {
    let m = p.marker();
    let mut nesting: usize = 0;
    let in_cell = mem::replace(&mut p.in_cell, true);
    while !p.at_set(syntax_set!(TablePipe, RightBracket, End))
        || (nesting > 0 && p.at_set(syntax_set!(TablePipe, RightBracket)))
    {
        markup_expr(p, false, &mut nesting);
    }
    p.in_cell = in_cell;
    p.wrap(m, SyntaxKind::Markup);
}

/// Parses a reference: `@target`, `@target[..]`.
fn reference(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::RefMarker);
    if p.directly_at(SyntaxKind::LeftBracket) {
        content_block(p);
    }
    p.wrap(m, SyntaxKind::Ref);
}

/// Parses a mathematical equation: `$x$`, `$ x^2 $`.
fn equation(p: &mut Parser<'_>) {
    let m = p.marker();
    p.enter_modes(SyntaxMode::Math, AtNewline::Continue, |p| {
        p.assert(SyntaxKind::Dollar);
        math(p, syntax_set!(Dollar, End));
        p.expect_closing_delimiter(m, SyntaxKind::Dollar);
    });
    p.wrap(m, SyntaxKind::Equation);
}

/// Parses the contents of a mathematical equation: `x^2 + 1`.
fn math(p: &mut Parser<'_>, stop_set: SyntaxSet) {
    let m = p.marker();
    math_exprs(p, stop_set);
    p.wrap(m, SyntaxKind::Math);
}

/// Parses a sequence of math expressions. Returns the number of expressions
/// parsed (including errors).
fn math_exprs(p: &mut Parser<'_>, stop_set: SyntaxSet) -> usize {
    debug_assert!(stop_set.contains(SyntaxKind::End));
    let Some(p) = p.check_depth_until(stop_set) else {
        return 1;
    };

    let mut count = 0;
    while !p.at_set(stop_set) {
        if p.at(SyntaxKind::Label) {
            // A label that ends a line of an equation: `<eq:suvat>`.
            p.eat();
        } else if p.at_set(set::MATH_EXPR) {
            math_expr(p);
        } else {
            p.unexpected();
        }
        count += 1;
    }
    count
}

/// Parses a single math expression: This includes math elements like
/// attachment, fractions, roots, and embedded code expressions.
fn math_expr(p: &mut Parser<'_>) {
    math_expr_prec(p, 0, syntax_set!())
}

/// Parses a math expression with at least the given precedence, possibly
/// chaining with another operator by returning early.
///
/// Every operator the loop here wraps around its left operand makes the tree
/// one level taller without a deeper call, so a chain such as `x/x/…/x`
/// stops once what it has wrapped is [`MAX_CHAIN_HEIGHT`] tall, and the rest
/// of the chain becomes one error ([`math_chain_error`]). A right-associative
/// chain such as `x^x^…^x` nests calls and stops at the depth limit in the
/// same way. The tree stays shallow, so the recursive walks of later stages
/// (numbering, evaluation, dropping) do not overflow the stack.
fn math_expr_prec(p: &mut Parser<'_>, min_prec: u8, stop_set: SyntaxSet) {
    if p.depth >= MAX_DEPTH {
        // An operand at the limit: the operand of `^` in a long `x^x^…`.
        math_chain_error(p, None, min_prec, stop_set);
        return;
    }
    let Some(p) = &mut p.increase_depth() else {
        return;
    };

    let m = p.marker();
    let mut continuable = false;
    match p.current() {
        SyntaxKind::Hash => embedded_code_expr(p),

        // The lexer manages creating full MathFieldAccess nodes if needed.
        SyntaxKind::MathIdent | SyntaxKind::MathFieldAccess => {
            continuable = true;
            p.eat();
            // Parse a function call for an identifier or field access.
            if MATH_FUNC_PREC >= min_prec && p.directly_at(SyntaxKind::LeftParen) {
                math_args(p);
                p.wrap(m, SyntaxKind::MathCall);
                continuable = false;
            }
        }

        SyntaxKind::LeftBrace | SyntaxKind::LeftParen => {
            math_delimited(p);
        }
        SyntaxKind::RightBrace if p.current_text() == "|]" => {
            p.convert_and_eat(SyntaxKind::MathShorthand);
        }
        SyntaxKind::Dot
        | SyntaxKind::Bang
        | SyntaxKind::Comma
        | SyntaxKind::Semicolon
        | SyntaxKind::RightBrace
        | SyntaxKind::RightParen => {
            p.convert_and_eat(SyntaxKind::MathText);
        }

        SyntaxKind::MathText => {
            continuable = is_math_alphabetic(p.current_text());
            p.eat();
        }

        SyntaxKind::Linebreak | SyntaxKind::MathAlignPoint | SyntaxKind::MathShorthand => p.eat(),

        SyntaxKind::MathPrimes | SyntaxKind::Escape | SyntaxKind::Str => {
            continuable = true;
            p.eat();
        }

        SyntaxKind::Root => {
            p.eat();
            let m2 = p.marker();
            math_expr_prec(p, MATH_ROOT_PREC, syntax_set!());
            math_unparen(p, m2);
            p.wrap(m, SyntaxKind::MathRoot);
        }

        _ => p.expected("expression"),
    }

    // Maybe recognize an implicit function call: a 'continuable' token followed
    // by delimiters will group as one with the precedence of a normal function.
    // E.g. `a(b)/c` parses as `(a(b))/c` when `a` is continuable.
    if continuable
        && MATH_FUNC_PREC >= min_prec
        && !p.had_trivia()
        && p.at_set(syntax_set!(LeftBrace, LeftParen))
    {
        math_delimited(p);
        p.wrap(m, SyntaxKind::Math);
    }

    // Parse infix and postfix operators. The general form of a parsed op looks
    // like: `MathAttach[ MathText("x"), Hat("^"), MathText("2") ]`.
    let mut wrapped = false;
    while !p.at_set(stop_set)
        && let op_kind = p.current()
        && let had_trivia = p.had_trivia()
        && let Some((wrapper, infix_assoc, prec)) = math_op(op_kind, had_trivia)
        && prec >= min_prec
    {
        // Another wrap would make the tree too tall: `x/x/…/x`. The first
        // wrap is free, as for any construct, so that a single operator
        // never fails.
        if wrapped && p.too_tall(m) {
            math_chain_error(p, Some(m), min_prec, stop_set);
            break;
        }

        // Prepare a chaining set for the attachment operators.
        let mut chain_set = if wrapper == SyntaxKind::MathAttach {
            // Hat can chain with Underscore, Underscore can chain with Hat, and
            // Prime can chain with either (but prime can't interrupt a chain,
            // see below).
            syntax_set!(Hat, Underscore).remove(op_kind)
        } else {
            syntax_set!()
        };

        // Eat the operator itself.
        if op_kind == SyntaxKind::Bang {
            p.convert_and_eat(SyntaxKind::MathText);
        } else {
            p.eat();
        }

        // Slash is the only operator that removes parens from its left operand.
        if wrapper == SyntaxKind::MathFrac {
            math_unparen(p, m);
        }

        // Parse the operator's right operand.
        if let Some(assoc) = infix_assoc {
            let prec = match assoc {
                ast::Assoc::Left => prec + 1,
                ast::Assoc::Right => prec,
            };
            if wrapper == SyntaxKind::MathAttach {
                math_script(p, prec, chain_set);
            } else {
                let m_rhs = p.marker();
                math_expr_prec(p, prec, chain_set);
                math_unparen(p, m_rhs);
            }
        }

        // Avoid interrupting a chain when initially parsing a prime.
        // For `a^b'_c^d` the grouping is `(a^(b')_c)^d` and not `a^(b'_c^d)`.
        if !(op_kind == SyntaxKind::MathPrimes && p.at_set(stop_set)) {
            // Parse chained attachment operators as a single attachment.
            while p.at_set(chain_set) {
                chain_set = chain_set.remove(p.current());
                p.eat();
                math_script(p, prec, chain_set);
            }
        }

        // Finish the operator by wrapping from its left operand.
        p.wrap(m, wrapper);
        wrapped = true;
    }
}

/// Parses the rest of a math operator chain that reached the depth limit as
/// one error: from an operand here (`x^x^…`) or, when `from` is given, from
/// the chain so far there, which the error takes in, to an operator here
/// (`/x/x…`). Either way the error stands where an operand stands, so the
/// nodes around it keep their shape.
///
/// It takes operators and their operands token by token without nesting, so
/// the chain gives one error and the parser goes no deeper, however long the
/// chain. An operand is one token, or a bracketed group with the brackets
/// directly after it (`f(x)`), or embedded code, which is parsed as code. As
/// in a regular parse, it may be a closing bracket, a line break or an
/// alignment point. The chain ends before anything else, a label or the end
/// of the equation, which the enclosing sequence then parses, and before an
/// operator that binds less tightly than `min_prec`, which an enclosing
/// operator takes. At the depth limit, operators that would end a nested
/// operand in a regular parse, such as the `_` after `b` in `a^b_c`, are
/// part of the chain: the error takes them too. A chain cut by its height
/// stops before them (`stop_set`), as a regular parse does.
fn math_chain_error(p: &mut Parser<'_>, from: Option<Marker>, min_prec: u8, stop_set: SyntaxSet) {
    let m = from.unwrap_or_else(|| p.marker());
    if from.is_none() && !math_chain_operand(p, false) {
        // Nothing here can be an operand (a label or the end of the
        // equation): a regular parse would say so too.
        p.expected("expression");
        return;
    }
    // Cut by its height, the chain is where the parser has room, so what
    // ends its operand in a regular parse (the `^` after `b` in `a_b!!…^c`)
    // ends the error too. At the depth limit, the error takes such operators
    // in: the enclosing operator would take them with another operand at
    // the limit, and give another error.
    while from.is_none_or(|_| !p.at_set(stop_set))
        && let Some((_, assoc, prec)) = math_op(p.current(), p.had_trivia())
        && prec >= min_prec
    {
        let script = p.at_set(syntax_set!(Hat, Underscore));
        if p.at(SyntaxKind::Bang) {
            p.convert_and_eat(SyntaxKind::MathText);
        } else {
            p.eat();
        }
        if assoc.is_some() && !math_chain_operand(p, script) {
            break;
        }
    }
    p.wrap_error(
        m,
        if from.is_some() {
            CHAIN_TOO_LONG
        } else {
            TOO_DEEP
        },
    );
}

/// The error of a chain cut by its height ([`MAX_CHAIN_HEIGHT`]).
const CHAIN_TOO_LONG: &str = "operator chain is too long";

/// The error of what is parsed past the depth limit ([`MAX_DEPTH`]).
const TOO_DEEP: &str = "maximum parsing depth exceeded";

/// Eats one operand of a chain for [`math_chain_error`]: a token or a
/// bracketed group, then the groups directly after it. Returns `false`, having
/// eaten nothing, when no operand starts here.
fn math_chain_operand(p: &mut Parser<'_>, script: bool) -> bool {
    const END: SyntaxSet = syntax_set!(Dollar, End, Label);
    if p.at_set(END) {
        return false;
    }
    // A sign takes the operand after it in a script, as in a regular parse:
    // `x^-1` ([`math_script`]). The operand at the depth limit itself comes
    // after a sign that `math_script` has already taken.
    if script && math_sign_binds(p) {
        p.eat();
    }
    // A root takes the operand after it, as in a regular parse: `√x`.
    while p.at(SyntaxKind::Root) {
        p.eat();
        if p.at_set(END) {
            return true;
        }
    }
    math_chain_group(p);
    while !p.had_trivia() && p.at_set(syntax_set!(LeftBrace, LeftParen)) {
        math_chain_group(p);
    }
    true
}

/// Eats a token, or a bracketed group to its closing bracket. It stops early
/// only at the end of the equation. Embedded code inside a group is parsed
/// as code, so that a `$` or a quote in it is read in its own mode.
fn math_chain_group(p: &mut Parser<'_>) {
    let mut balance: usize = 0;
    loop {
        if p.at_set(syntax_set!(Dollar, End)) {
            break;
        }
        if p.at(SyntaxKind::Hash) {
            math_chain_code(p);
            if balance == 0 {
                break;
            }
            continue;
        }
        if p.at_set(syntax_set!(LeftBrace, LeftParen)) {
            balance += 1;
        } else if p.at_set(syntax_set!(RightBrace, RightParen)) {
            balance = balance.saturating_sub(1);
        }
        p.eat();
        if balance == 0 {
            break;
        }
    }
}

/// Parses the script after `^` or `_`: one operand, whose parentheses are
/// removed (`x^(a+b)`).
///
/// A sign directly followed by an operand binds to it, so that `10^-3`,
/// `e^-x` and `x_+1` take the signed operand as their script, as
/// `10^(-3)` does (SPEC §8.1, D-323-1), and so does a bracket after the
/// sign: `e^-(x^2/2)` (D-323-2). `±` and `∓` are signs too: `x^±1`
/// (D-323-3). A sign with a space or nothing after it is the script alone,
/// as in the electron `e^-` or the ion `Cl^- + Na^+`.
fn math_script(p: &mut Parser<'_>, prec: u8, stop_set: SyntaxSet) {
    let m = p.marker();
    if math_sign_binds(p) {
        p.eat();
        math_expr_prec(p, prec, stop_set);
        p.wrap(m, SyntaxKind::Math);
    } else {
        math_expr_prec(p, prec, stop_set);
        math_unparen(p, m);
    }
}

/// Whether the parser is at a sign (`-`, `+`, `−`, `±` or `∓`) directly
/// followed by something that starts an operand: a name, a number or letter, a large
/// operator such as `∑`, a bracket, a root, a string, an escape or embedded
/// code. A fence such as `|` or a `.` starts no operand of a script, signed or
/// not: `x^|x|` raises only the `|`.
fn math_sign_binds(p: &Parser<'_>) -> bool {
    let sign = match p.current() {
        SyntaxKind::MathShorthand => p.current_text() == "-",
        SyntaxKind::MathText => matches!(p.current_text(), "+" | "−" | "±" | "∓"),
        _ => false,
    };
    if !sign {
        return false;
    }
    let (kind, node) = p.lexer.clone().next();
    match kind {
        SyntaxKind::MathIdent
        | SyntaxKind::MathFieldAccess
        | SyntaxKind::Hash
        | SyntaxKind::LeftParen
        | SyntaxKind::Root
        | SyntaxKind::Str
        | SyntaxKind::Escape => true,
        // An opening bracket, but not `|]`, which the lexer gives as a brace.
        SyntaxKind::LeftBrace => true,
        SyntaxKind::MathText => node.leaf_text().chars().next().is_some_and(|c| {
            c.is_alphanumeric()
                || matches!(
                    default_math_class(c),
                    Some(MathClass::Alphabetic | MathClass::Normal | MathClass::Large)
                )
        }),
        _ => false,
    }
}

// These are declared here so they're easier to compare with `math_op`.
const MATH_FUNC_PREC: u8 = 2;
const MATH_ROOT_PREC: u8 = 2;

/// Precedence and wrapper kinds for infix and postfix math operators.
fn math_op(kind: SyntaxKind, had_trivia: bool) -> Option<(SyntaxKind, Option<ast::Assoc>, u8)> {
    let op = match kind {
        SyntaxKind::Slash => (SyntaxKind::MathFrac, Some(ast::Assoc::Left), 1),
        SyntaxKind::Underscore => (SyntaxKind::MathAttach, Some(ast::Assoc::Right), 2),
        SyntaxKind::Hat => (SyntaxKind::MathAttach, Some(ast::Assoc::Right), 2),
        SyntaxKind::MathPrimes if !had_trivia => (SyntaxKind::MathAttach, None, 2),
        SyntaxKind::Bang if !had_trivia => (SyntaxKind::Math, None, 3),
        _ => return None,
    };
    Some(op)
}

/// Whether text counts as alphabetic in math. For the `Text` and `MathText`
/// kinds, this causes them to group with parens as an implicit function call.
fn is_math_alphabetic(text: &str) -> bool {
    if let Some((0, c)) = text.char_indices().next_back() {
        // Just a single character.
        c.is_alphabetic() || default_math_class(c) == Some(MathClass::Alphabetic)
    } else {
        // Multiple characters.
        text.chars().all(char::is_alphabetic)
    }
}

/// Parses embedded code inside a chain's error ([`math_chain_group`]). The
/// parser is at the depth limit there, so the code is parsed with
/// [`CHAIN_CODE_DEPTH`] levels of room below it. Code in the error of a chain
/// in that code gets the room again, up to [`CHAIN_CODE_NESTING`] times, so
/// the parser's calls stay bounded; past that, its code is read as math
/// tokens. A chain cut by its height rather than at the depth limit leaves
/// room enough: its code is parsed as usual and does not count.
fn math_chain_code(p: &mut Parser<'_>) {
    let depth = p.depth;
    if depth <= MAX_DEPTH - CHAIN_CODE_DEPTH {
        embedded_code_expr(p);
        return;
    }
    if p.chain_code >= CHAIN_CODE_NESTING {
        p.eat();
        return;
    }
    p.depth = MAX_DEPTH - CHAIN_CODE_DEPTH;
    p.chain_code += 1;
    embedded_code_expr(p);
    p.chain_code -= 1;
    p.depth = depth;
}

/// The room below the depth limit for embedded code in a chain's error.
const CHAIN_CODE_DEPTH: u32 = 32;

/// How often embedded code in chain errors within one another gets that
/// room ([`math_chain_code`]).
const CHAIN_CODE_NESTING: u32 = 4;

/// Parse matched delimiters in math: `[x + y]`.
///
/// The lexer produces `{Left,Right}{Brace,Paren}` for delimiters, and it's our
/// job to convert them back to `MathText` or `MathShorthand` before eating.
fn math_delimited(p: &mut Parser<'_>) {
    let m = p.marker();
    if p.current_text() == "[|" {
        p.convert_and_eat(SyntaxKind::MathShorthand);
    } else {
        p.convert_and_eat(SyntaxKind::MathText);
    }
    let m_body = p.marker();
    math_exprs(p, syntax_set!(Dollar, End, RightBrace, RightParen));
    if p.at_set(syntax_set!(RightBrace, RightParen)) {
        p.wrap(m_body, SyntaxKind::Math);
        if p.current_text() == "|]" {
            p.convert_and_eat(SyntaxKind::MathShorthand);
        } else {
            p.convert_and_eat(SyntaxKind::MathText);
        }
        p.wrap(m, SyntaxKind::MathDelimited);
    } else {
        // If we had no closing delimiter, just produce a math sequence.
        p.wrap(m, SyntaxKind::Math);
    }
}

/// Remove one set of parentheses (if any) from a previously parsed expression
/// by converting to non-expression SyntaxKinds.
fn math_unparen(p: &mut Parser<'_>, m: Marker) {
    let Some(node) = p.nodes.get_mut(m.0) else {
        return;
    };
    if node.kind() != SyntaxKind::MathDelimited {
        return;
    }

    if let [first, .., last] = node.children_mut()
        && first.leaf_text() == "("
        && last.leaf_text() == ")"
    {
        first.convert_to_kind(SyntaxKind::LeftParen);
        last.convert_to_kind(SyntaxKind::RightParen);
        // Only convert if we did have regular parens.
        node.convert_to_kind(SyntaxKind::Math);
    }
}

/// Parse an argument list in math: `(a, b; c, d; size: #50%)`.
fn math_args(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::LeftParen);

    let mut seen = FxHashSet::default();
    while !p.at_set(syntax_set!(End, Dollar, RightParen)) {
        math_arg(p, &mut seen);
        match p.current() {
            SyntaxKind::End | SyntaxKind::Dollar | SyntaxKind::RightParen => {}
            SyntaxKind::Semicolon | SyntaxKind::Comma => p.eat(),
            _ => p.expected("comma or semicolon"),
        }
    }

    p.expect_closing_delimiter(m, SyntaxKind::RightParen);
    p.wrap(m, SyntaxKind::MathArgs);
}

/// Parses a single argument in a math argument list.
fn math_arg<'s>(p: &mut Parser<'s>, seen: &mut FxHashSet<&'s str>) {
    let m = p.marker();
    let start = p.current_start();

    let mut arg_kind = None;

    if let Some(spread) = p.lexer.maybe_math_spread_arg(start) {
        // Parses a spread argument: `..args`.
        arg_kind = Some(SyntaxKind::Spread);
        p.token.node = spread;
        p.eat();
    } else if let Some(named) = p.lexer.maybe_math_named_arg(start) {
        // Parses a named argument: `thickness: #12pt`.
        arg_kind = Some(SyntaxKind::Named);
        p.token.node = named;
        let text = p.current_text();
        p.eat();
        p.convert_and_eat(SyntaxKind::Colon);
        if !seen.insert(text) {
            p[m].convert_to_error(eco_format!("duplicate argument: {text}"));
        }
    }

    // Parses the argument itself.
    let m_arg = p.marker();
    let count = math_exprs(p, syntax_set!(End, Dollar, Comma, Semicolon, RightParen));

    if count == 0 {
        // This can't happen due to checks in `Lexer::maybe_math_spread_arg`.
        assert_ne!(arg_kind, Some(SyntaxKind::Spread));

        // Named arguments require a value.
        if arg_kind == Some(SyntaxKind::Named) {
            p.expected("expression");
        }
    }

    // Wrap math function arguments to join adjacent math content or create an
    // empty 'Math' node for when we have 0 args. We don't wrap when
    // `count == 1`, since wrapping would change the type of the expression
    // from potentially non-content to content. E.g. `$ func(#12pt) $` would
    // change the type of `#12pt` from size to content if wrapped.
    if count != 1 {
        p.wrap(m_arg, SyntaxKind::Math);
    }

    if let Some(kind) = arg_kind {
        p.wrap(m, kind);
    }
}

/// Parses the contents of a code block.
fn code(p: &mut Parser<'_>, stop_set: SyntaxSet) {
    let m = p.marker();
    code_exprs(p, stop_set);
    p.wrap(m, SyntaxKind::Code);
}

/// Parses a sequence of code expressions.
fn code_exprs(p: &mut Parser<'_>, stop_set: SyntaxSet) {
    debug_assert!(stop_set.contains(SyntaxKind::End));
    let Some(p) = p.check_depth_until(stop_set) else {
        return;
    };

    while !p.at_set(stop_set) {
        p.with_nl_mode(AtNewline::ContextualContinue, |p| {
            contextual_keyword(p);
            if !p.at_set(set::CODE_EXPR) {
                p.unexpected();
                return;
            }
            code_expr(p);
            if !p.at_set(stop_set) && !p.eat_if(SyntaxKind::Semicolon) {
                p.expected("semicolon or line break");
                if p.at(SyntaxKind::Label) {
                    p.hint("labels can only be applied in markup mode");
                    p.hint("try wrapping your code in a markup block (`[ ]`)");
                }
            }
        });
    }
}

/// Parses an atomic code expression embedded in markup or math.
fn embedded_code_expr(p: &mut Parser<'_>) {
    p.enter_modes(SyntaxMode::Code, AtNewline::Stop, |p| {
        p.assert(SyntaxKind::Hash);
        if p.had_trivia() || p.end() {
            p.expected("expression");
            return;
        }

        contextual_keyword(p);
        let stmt = p.at_set(set::STMT);
        code_expr_prec(p, true, 0);

        // Note: 2d math arguments rely on the `directly_at` check.
        let semi =
            (stmt || p.directly_at(SyntaxKind::Semicolon)) && p.eat_if(SyntaxKind::Semicolon);

        if stmt && !semi && !p.end() && !p.at(SyntaxKind::RightBracket) {
            p.expected("semicolon or line break");
        }
    });
}

/// Parses a single code expression.
fn code_expr(p: &mut Parser<'_>) {
    code_expr_prec(p, false, 0);
}

/// Parses a code expression with at least the given precedence.
///
/// Every call, field access and binary operator the loop here wraps around
/// its left operand makes the tree one level taller without a deeper call.
/// Once what it has wrapped is [`MAX_CHAIN_HEIGHT`] tall, the rest of the
/// chain (`x + x + …`, `f()()…`) is parsed without wrapping and becomes one
/// error. The tree stays shallow, so the
/// recursive walks of later stages do not overflow the stack.
fn code_expr_prec(p: &mut Parser<'_>, atomic: bool, min_prec: u8) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };

    let m = p.marker();
    if p.at_set(set::UNARY_OP) {
        if !atomic {
            let op = ast::UnOp::from_kind(p.current()).unwrap();
            p.eat();
            code_expr_prec(p, atomic, op.precedence());
            p.wrap(m, SyntaxKind::Unary);
        } else {
            p.unexpected();
            p.hint("to use a unary operator here, wrap the entire expression in parentheses");
        }
    } else {
        code_primary(p, atomic);
        // Element declarations and scoped rules are statements: nothing
        // attaches to them.
        if matches!(
            p.nodes.get(m.0).map(SyntaxNode::kind),
            Some(SyntaxKind::ElementDecl | SyntaxKind::ScopedRule)
        ) {
            return;
        }
    }

    // Where the rest of a chain past the limit starts, once it is parsed
    // flat. The whole chain then becomes one error, which stands where the
    // expression stands, if anything follows there.
    let mut chain_error = None;
    let wrap =
        |p: &mut Parser<'_>, kind: SyntaxKind, chain_error: &mut Option<(Marker, &'static str)>| {
            if chain_error.is_some() {
                return;
            }
            p.wrap(m, kind);
            if p.too_tall(m) {
                // The next wrap would make the tree too tall: parse the rest
                // flat.
                *chain_error = Some((p.marker(), CHAIN_TOO_LONG));
            }
        };

    loop {
        // In a stylesheet value, a name directly followed by an attribute
        // test is a selector: `slide-break: heading[level=2], slide;`. In
        // content, `name[..]` stays a call with a content argument.
        if p.style_values
            && p.lexer.style()
            && p.directly_at(SyntaxKind::LeftBracket)
            && p.nodes.len() == m.0 + 1
            && p.nodes[m.0].kind() == SyntaxKind::Ident
        {
            style::value_selector(p, m);
            return;
        }

        if p.directly_at(SyntaxKind::LeftParen) || p.directly_at(SyntaxKind::LeftBracket) {
            let start = p.marker();
            args(p);
            // Arguments parsed past the depth limit, as in a long
            // `f(x)(x)…` there: parse the rest flat, rather than give an
            // error for each call. Calls whose arguments parse are no error.
            if chain_error.is_none()
                && p.depth >= MAX_DEPTH
                && p.nodes.get(start.0).is_some_and(|n| n.diagnosis().errors)
            {
                chain_error = Some((start, TOO_DEEP));
            }
            wrap(p, SyntaxKind::FuncCall, &mut chain_error);
            continue;
        }

        let at_field_or_method =
            p.directly_at(SyntaxKind::Dot) && p.lexer.clone().next().0 == SyntaxKind::Ident;

        if atomic && !at_field_or_method {
            break;
        }

        if p.eat_if(SyntaxKind::Dot) {
            p.expect(SyntaxKind::Ident);
            wrap(p, SyntaxKind::FieldAccess, &mut chain_error);
            continue;
        }

        // In a stylesheet value, a sign after a space and directly before its
        // operand starts a new item, as in CSS: `margin: 0 -1mm`.
        if p.style_values
            && p.split_signs
            && p.at_set(syntax_set!(Plus, Minus))
            && p.had_trivia()
            && !p.lexer.clone().next().0.is_trivia()
        {
            break;
        }

        let op_start = p.marker();
        let binop = if p.at_set(set::BINARY_OP) {
            ast::BinOp::from_kind(p.current())
        } else if min_prec <= ast::BinOp::NotIn.precedence() && p.eat_if(SyntaxKind::Not) {
            if p.at(SyntaxKind::In) {
                Some(ast::BinOp::NotIn)
            } else {
                p.expected("keyword `in`");
                break;
            }
        } else {
            None
        };

        if let Some(op) = binop {
            let mut prec = op.precedence();
            if prec < min_prec {
                break;
            }

            match op.assoc() {
                ast::Assoc::Left => prec += 1,
                ast::Assoc::Right => {}
            }

            // The operand would be parsed past the depth limit, as in a long
            // `x = x = …` there: parse the rest flat, rather than give an
            // error for each operator.
            if chain_error.is_none() && p.depth >= MAX_DEPTH {
                chain_error = Some((op_start, TOO_DEEP));
            }

            p.eat();
            code_expr_prec(p, false, prec);
            wrap(p, SyntaxKind::Binary, &mut chain_error);
            continue;
        }

        break;
    }

    if let Some((start, message)) = chain_error
        && p.before_trivia().0 > start.0
    {
        p.wrap_error(m, message);
    }
}

/// Parses a primary in a code expression. These are the atoms that unary and
/// binary operations, functions calls, and field accesses start with / are
/// composed of.
fn code_primary(p: &mut Parser<'_>, atomic: bool) {
    let m = p.marker();
    match p.current() {
        SyntaxKind::Ident => {
            p.eat();
            if !atomic && p.at(SyntaxKind::Arrow) {
                p.wrap(m, SyntaxKind::Params);
                p.assert(SyntaxKind::Arrow);
                code_expr(p);
                p.wrap(m, SyntaxKind::Closure);
            }
        }
        SyntaxKind::Underscore if !atomic => {
            p.eat();
            if p.at(SyntaxKind::Arrow) {
                p.wrap(m, SyntaxKind::Params);
                p.eat();
                code_expr(p);
                p.wrap(m, SyntaxKind::Closure);
            } else if p.eat_if(SyntaxKind::Eq) {
                code_expr(p);
                p.wrap(m, SyntaxKind::DestructAssignment);
            } else {
                p[m].expected("expression");
            }
        }

        SyntaxKind::LeftBrace if p.style_values => style::template_block(p),
        SyntaxKind::LeftBrace => code_block(p),
        SyntaxKind::LeftBracket => content_block(p),
        SyntaxKind::LeftParen => expr_with_paren(p, atomic),
        SyntaxKind::Dollar => equation(p),
        SyntaxKind::Let => let_binding(p),
        SyntaxKind::Element => element_decl(p),
        SyntaxKind::Rule => scoped_rule(p),
        SyntaxKind::If => conditional(p),
        SyntaxKind::While => while_loop(p),
        SyntaxKind::For => for_loop(p),
        SyntaxKind::Import => module_import(p),
        SyntaxKind::Include => module_include(p),
        SyntaxKind::Break => break_stmt(p),
        SyntaxKind::Continue => continue_stmt(p),
        SyntaxKind::Return => return_stmt(p),

        SyntaxKind::Raw => p.eat(), // Raw is handled entirely in the Lexer.
        SyntaxKind::TexEquation => p.eat(), // So is LaTeX math.

        SyntaxKind::None
        | SyntaxKind::Auto
        | SyntaxKind::Int
        | SyntaxKind::Float
        | SyntaxKind::Bool
        | SyntaxKind::Numeric
        | SyntaxKind::Str
        | SyntaxKind::Label
        | SyntaxKind::TokenName
        | SyntaxKind::Color
        | SyntaxKind::Ratio => p.eat(),

        // Consume erroneous tokens for things like `#12p`, `#]`, or `#"abc\"`.
        _ if atomic => p.unexpected(),

        _ => p.expected("expression"),
    }
}

/// Reparses a full content or code block. This only succeeds if the new block
/// contains balanced delimiters.
pub(super) fn reparse_block(text: &str, range: Range<usize>) -> Option<SyntaxNode> {
    let mut p = Parser::new(text, range.start, SyntaxMode::Code);
    assert!(p.at(SyntaxKind::LeftBracket) || p.at(SyntaxKind::LeftBrace));
    block(&mut p);
    (p.balanced && p.prev_end() == range.end).then(|| p.finish().into_iter().next().unwrap())
}

/// Parses a content or code block.
///
/// In a stylesheet value, braces hold template items, so a conditional in
/// parentheses has the same bodies as one at the level of items.
fn block(p: &mut Parser<'_>) {
    match p.current() {
        SyntaxKind::LeftBracket => content_block(p),
        SyntaxKind::LeftBrace if p.style_values => style::template_block(p),
        SyntaxKind::LeftBrace => code_block(p),
        _ => p.expected("block"),
    }
}

/// Parses a code block: `{ let x = 1; x + 2 }`.
///
/// Code and content blocks hold script and markup, so inside them a
/// stylesheet's own syntax (token names, colours, template items) does not
/// apply.
fn code_block(p: &mut Parser<'_>) {
    let m = p.marker();
    let values = mem::replace(&mut p.style_values, false);
    p.enter_lexer(SyntaxMode::Code, false, AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBrace);
        code(p, syntax_set!(RightBrace, RightBracket, RightParen, End));
        p.expect_closing_delimiter(m, SyntaxKind::RightBrace);
    });
    p.style_values = values;
    p.wrap(m, SyntaxKind::CodeBlock);
}

/// Parses a content block: `[*Hi* there!]`.
fn content_block(p: &mut Parser<'_>) {
    let m = p.marker();
    let values = mem::replace(&mut p.style_values, false);
    let in_cell = mem::replace(&mut p.in_cell, false);
    p.enter_lexer(SyntaxMode::Markup, false, AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBracket);
        markup(p, true, true, syntax_set!(RightBracket, End));
        p.expect_closing_delimiter(m, SyntaxKind::RightBracket);
    });
    p.in_cell = in_cell;
    p.style_values = values;
    p.wrap(m, SyntaxKind::ContentBlock);
}

/// Parses a let binding: `let x = 1`.
fn let_binding(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Let);

    let m2 = p.marker();
    let mut closure = false;
    let mut other = false;

    if p.eat_if(SyntaxKind::Ident) {
        if p.directly_at(SyntaxKind::LeftParen) {
            params(p);
            closure = true;
        }
    } else {
        pattern(p, false, &mut FxHashSet::default(), None);
        other = true;
    }

    let f = if closure || other {
        Parser::expect
    } else {
        Parser::eat_if
    };
    if f(p, SyntaxKind::Eq) {
        code_expr(p);
    }

    if closure {
        p.wrap(m2, SyntaxKind::Closure);
    }

    p.wrap(m, SyntaxKind::LetBinding);
}

/// Turns the identifiers `element` and `rule` into keywords where they start
/// an element declaration (`element example(..)`) or a scoped rule
/// (`rule question { .. }`). This happens only at the start of a statement and
/// when a name or selector follows on the same line after a space, so
/// `element` and `rule` remain usable as ordinary names elsewhere, such as in
/// `rule(0.4pt)` or `element.fields`.
fn contextual_keyword(p: &mut Parser<'_>) {
    if !p.at(SyntaxKind::Ident) {
        return;
    }
    let kind = match p.current_text() {
        "element" => SyntaxKind::Element,
        "rule" => SyntaxKind::Rule,
        _ => return,
    };
    let starts = match (kind, p.peek_after_space()) {
        (SyntaxKind::Element, Some(next)) => next == SyntaxKind::Ident,
        (SyntaxKind::Rule, Some(next)) => {
            matches!(
                next,
                SyntaxKind::Ident | SyntaxKind::At | SyntaxKind::Dot | SyntaxKind::Colon
            ) || (next == SyntaxKind::Error
                && p.text[p.current_end()..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with("#{"))
        }
        _ => false,
    };
    if starts {
        p.convert_current(kind);
    }
}

/// Parses an element declaration (SPEC §4.3):
/// `element example(title: content = none) { kind: block }`.
fn element_decl(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Element);
    p.expect(SyntaxKind::Ident);
    if p.at(SyntaxKind::LeftParen) {
        schema_fields(p);
    } else {
        p.expected("field list");
    }
    if p.at(SyntaxKind::LeftBrace) {
        schema_body(p);
    }
    p.wrap(m, SyntaxKind::ElementDecl);
}

/// Parses the fields of an element declaration: `(title: content = none)`.
fn schema_fields(p: &mut Parser<'_>) {
    let m = p.marker();
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftParen);

        let mut seen = FxHashSet::default();
        while !p.current().is_terminator() {
            if !p.at(SyntaxKind::Ident) {
                p.unexpected();
                continue;
            }

            schema_field(p, &mut seen);

            if !p.current().is_terminator() {
                p.expect(SyntaxKind::Comma);
            }
        }

        p.expect_closing_delimiter(m, SyntaxKind::RightParen);
    });
    p.wrap(m, SyntaxKind::SchemaFields);
}

/// Parses a field of an element declaration: `title: content = none`. The type
/// is an expression that binds tighter than assignment, so that `=` starts the
/// default value.
fn schema_field<'s>(p: &mut Parser<'s>, seen: &mut FxHashSet<&'s str>) {
    let m = p.marker();
    let name = p.current_text();
    p.assert(SyntaxKind::Ident);
    if !seen.insert(name) {
        p[m].convert_to_error(eco_format!("duplicate field: {name}"));
    }

    if p.expect(SyntaxKind::Colon) {
        code_expr_prec(p, false, ast::BinOp::Assign.precedence() + 1);
    }
    if p.eat_if(SyntaxKind::Eq) {
        code_expr(p);
    }

    p.wrap(m, SyntaxKind::SchemaField);
}

/// Parses the body of an element declaration: `{ kind: block }`.
fn schema_body(p: &mut Parser<'_>) {
    let m = p.marker();
    p.enter_modes(SyntaxMode::Code, AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBrace);
        while !p.at_set(syntax_set!(RightBrace, RightBracket, RightParen, End)) {
            if p.at(SyntaxKind::Ident) {
                schema_entry(p);
            } else if !p.eat_if(SyntaxKind::Semicolon) {
                p.unexpected();
            }
        }
        p.expect_closing_delimiter(m, SyntaxKind::RightBrace);
    });
    p.wrap(m, SyntaxKind::SchemaBody);
}

/// Parses an entry in the body of an element declaration: `kind: block`. The
/// value is one or more expressions up to the end of the line or a semicolon:
/// `counter: within heading(level: 1)`.
fn schema_entry(p: &mut Parser<'_>) {
    let m = p.marker();
    p.with_nl_mode(AtNewline::Stop, |p| {
        p.assert(SyntaxKind::Ident);
        if p.expect(SyntaxKind::Colon) {
            let m_value = p.marker();
            while p.at_set(set::CODE_EXPR) {
                code_expr(p);
            }
            if p.marker() == m_value {
                p.expected("expression");
            }
        }
    });
    p.wrap(m, SyntaxKind::SchemaEntry);
}

/// Parses a scoped rule (SPEC §5.4): `rule question { numbering: "(a)"; }`,
/// `rule @target pptx figure { .. }`. The selectors and declarations are
/// those of stylesheets (SPEC §5.1), and the declaration block is lexed as
/// in a stylesheet, so its values name tokens `$accent` and colours
/// `#1f5fa8`.
fn scoped_rule(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Rule);
    while p.at(SyntaxKind::At) {
        rule_scope(p);
    }
    style::scoped_rule_body(p);
    p.wrap(m, SyntaxKind::ScopedRule);
}

/// Parses a variant or target scope of a scoped rule or an overlay:
/// `@target pptx`, `@variant slides, handout`.
fn rule_scope(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::At);
    if p.directly_at(SyntaxKind::Ident) {
        let m_kind = p.marker();
        let kind = p.current_text();
        p.eat();
        if !matches!(kind, "variant" | "target") {
            p[m_kind].convert_to_error(eco_format!("unknown rule scope: `@{kind}`"));
            p[m_kind].hint("a scope is `@variant name` or `@target name`");
        }
        loop {
            p.expect(SyntaxKind::Ident);
            if !p.eat_if(SyntaxKind::Comma) {
                break;
            }
        }
    } else {
        p.expected("`variant` or `target`");
    }
    p.wrap(m, SyntaxKind::RuleScope);
}

/// Parses an if-else conditional: `if x { y } else { z }`.
fn conditional(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::If);
    code_expr(p);
    block(p);
    if p.eat_if(SyntaxKind::Else) {
        if p.at(SyntaxKind::If) {
            conditional(p);
        } else {
            block(p);
        }
    }
    p.wrap(m, SyntaxKind::Conditional);
}

/// Parses a while loop: `while x { y }`.
fn while_loop(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::While);
    code_expr(p);
    block(p);
    p.wrap(m, SyntaxKind::WhileLoop);
}

/// Parses a for loop: `for x in y { z }`.
fn for_loop(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::For);

    let mut seen = FxHashSet::default();
    pattern(p, false, &mut seen, None);

    if p.at(SyntaxKind::Comma) {
        let node = p.eat_and_get();
        node.unexpected();
        node.hint("destructuring patterns must be wrapped in parentheses");
        if p.at_set(set::PATTERN) {
            pattern(p, false, &mut seen, None);
        }
    }

    p.expect(SyntaxKind::In);
    code_expr(p);
    block(p);
    p.wrap(m, SyntaxKind::ForLoop);
}

/// Parses a module import: `import "utils.fol": a, b, c`.
fn module_import(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Import);
    code_expr(p);
    if p.eat_if(SyntaxKind::As) {
        // Allow renaming a full module import.
        // If items are included, both the full module and the items are
        // imported at the same time.
        p.expect(SyntaxKind::Ident);
    }

    if p.eat_if(SyntaxKind::Colon) {
        if p.at(SyntaxKind::LeftParen) {
            p.with_nl_mode(AtNewline::Continue, |p| {
                let m2 = p.marker();
                p.assert(SyntaxKind::LeftParen);

                import_items(p);

                p.expect_closing_delimiter(m2, SyntaxKind::RightParen);
            });
        } else if !p.eat_if(SyntaxKind::Star) {
            import_items(p);
        }
    }

    p.wrap(m, SyntaxKind::ModuleImport);
}

/// Parses items to import from a module: `a, b, c`.
fn import_items(p: &mut Parser<'_>) {
    let m = p.marker();
    while !p.current().is_terminator() {
        let item_marker = p.marker();
        if !p.eat_if(SyntaxKind::Ident) {
            p.unexpected();
        }

        // Nested import path: `a.b.c`
        while p.eat_if(SyntaxKind::Dot) {
            p.expect(SyntaxKind::Ident);
        }

        p.wrap(item_marker, SyntaxKind::ImportItemPath);

        // Rename imported item.
        if p.eat_if(SyntaxKind::As) {
            p.expect(SyntaxKind::Ident);
            p.wrap(item_marker, SyntaxKind::RenamedImportItem);
        }

        if !p.current().is_terminator() {
            p.expect(SyntaxKind::Comma);
        }
    }

    p.wrap(m, SyntaxKind::ImportItems);
}

/// Parses a module include: `include "chapter1.fol"`.
fn module_include(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Include);
    code_expr(p);
    p.wrap(m, SyntaxKind::ModuleInclude);
}

/// Parses a break from a loop: `break`.
fn break_stmt(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Break);
    p.wrap(m, SyntaxKind::LoopBreak);
}

/// Parses a continue in a loop: `continue`.
fn continue_stmt(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Continue);
    p.wrap(m, SyntaxKind::LoopContinue);
}

/// Parses a return from a function: `return`, `return x + 1`.
fn return_stmt(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Return);
    if p.at_set(set::CODE_EXPR) {
        code_expr(p);
    }
    p.wrap(m, SyntaxKind::FuncReturn);
}

/// An expression that starts with a parenthesis.
fn expr_with_paren(p: &mut Parser<'_>, atomic: bool) {
    if atomic {
        // Atomic expressions aren't modified by operators that follow them, so
        // our first guess of array/dict will be correct.
        parenthesized_or_array_or_dict(p);
        return;
    }

    // If we've seen this position before and have a memoized result, restore it
    // and return. Otherwise, get a key to this position and a checkpoint to
    // restart from in case we make a wrong prediction.
    let Some((memo_key, checkpoint)) = p.restore_memo_or_checkpoint() else {
        return;
    };
    // The node length from when we restored.
    let prev_len = checkpoint.node_len;

    // When we reach a '(', we can't be sure what it is. First, we attempt to
    // parse as a simple parenthesized expression, array, or dictionary as
    // these are the most likely things. We can handle all of those in a single
    // pass.
    let kind = parenthesized_or_array_or_dict(p);

    // If, however, '=>' or '=' follows, we must backtrack and reparse as either
    // a parameter list or a destructuring. To be able to do that, we created a
    // parser checkpoint before our speculative parse, which we can restore.
    //
    // However, naive backtracking has a fatal flaw: It can lead to exponential
    // parsing time if we are constantly getting things wrong in a nested
    // scenario. The particular failure case for parameter parsing is the
    // following: `(x: (x: (x) => y) => y) => y`
    //
    // Such a structure will reparse over and over again recursively, leading to
    // a running time of O(2^n) for nesting depth n. To prevent this, we perform
    // a simple trick: When we have done the mistake of picking the wrong path
    // once and have subsequently parsed correctly, we save the result of that
    // correct parsing in the `p.memo` map. When we reach the same position
    // again, we can then just restore this result. In this way, no
    // parenthesized expression is parsed more than twice, leading to a worst
    // case running time of O(2n).
    if p.at(SyntaxKind::Arrow) {
        p.restore(checkpoint);
        let m = p.marker();
        params(p);
        if !p.expect(SyntaxKind::Arrow) {
            return;
        }
        code_expr(p);
        p.wrap(m, SyntaxKind::Closure);
    } else if p.at(SyntaxKind::Eq) && kind != SyntaxKind::Parenthesized {
        p.restore(checkpoint);
        let m = p.marker();
        destructuring_or_parenthesized(p, true, &mut FxHashSet::default());
        if !p.expect(SyntaxKind::Eq) {
            return;
        }
        code_expr(p);
        p.wrap(m, SyntaxKind::DestructAssignment);
    } else {
        return;
    }

    // Memoize result if we backtracked.
    p.memoize_parsed_nodes(memo_key, prev_len);
}

/// Parses either
/// - a parenthesized expression: `(1 + 2)`, or
/// - an array: `(1, "hi", 12cm)`, or
/// - a dictionary: `(thickness: 3pt, dash: "solid")`.
fn parenthesized_or_array_or_dict(p: &mut Parser<'_>) -> SyntaxKind {
    let mut state = GroupState {
        count: 0,
        maybe_just_parens: true,
        kind: None,
        seen: FxHashSet::default(),
    };

    // An edge case with parens is whether we can interpret a leading spread
    // expression as a dictionary, e.g. if we want `(..dict1, ..dict2)` to join
    // the two dicts.
    //
    // The issue is that we decide on the type of the parenthesized expression
    // here in the parser by the `SyntaxKind` we wrap with, instead of in eval
    // based on the type of the spread item.
    //
    // The current fix is that we allow a leading colon to force the
    // parenthesized value into a dict:
    // - `(..arr1, ..arr2)` is wrapped as an `Array`.
    // - `(: ..dict1, ..dict2)` is wrapped as a `Dict`.
    //
    // This does allow some unexpected expressions, such as `(: key: val)`, but
    // it's currently intentional.
    let m = p.marker();
    let split = mem::replace(&mut p.split_signs, false);
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftParen);
        if p.eat_if(SyntaxKind::Colon) {
            state.kind = Some(SyntaxKind::Dict);
        }

        while !p.current().is_terminator() {
            if !p.at_set(set::ARRAY_OR_DICT_ITEM) {
                p.unexpected();
                continue;
            }

            array_or_dict_item(p, &mut state);
            state.count += 1;

            if !p.current().is_terminator() && p.expect(SyntaxKind::Comma) {
                state.maybe_just_parens = false;
            }
        }

        p.expect_closing_delimiter(m, SyntaxKind::RightParen);
    });
    p.split_signs = split;

    let kind = if state.maybe_just_parens && state.count == 1 {
        SyntaxKind::Parenthesized
    } else {
        state.kind.unwrap_or(SyntaxKind::Array)
    };

    p.wrap(m, kind);
    kind
}

/// State for array/dictionary parsing.
struct GroupState {
    count: usize,
    /// Whether this is just a single expression in parens: `(a)`. Single
    /// element arrays require an explicit comma: `(a,)`, unless we're
    /// spreading: `(..a)`.
    maybe_just_parens: bool,
    /// The `SyntaxKind` to wrap as (if we've figured it out yet).
    kind: Option<SyntaxKind>,
    /// Store named arguments so we can give an error if they're repeated.
    seen: FxHashSet<EcoString>,
}

/// Parses a single item in an array or dictionary.
fn array_or_dict_item(p: &mut Parser<'_>, state: &mut GroupState) {
    let m = p.marker();

    if p.eat_if(SyntaxKind::Dots) {
        // Parses a spread item: `..item`.
        code_expr(p);
        p.wrap(m, SyntaxKind::Spread);
        state.maybe_just_parens = false;
        return;
    }

    code_expr(p);

    if p.eat_if(SyntaxKind::Colon) {
        // Parses a named/keyed pair: `name: item` or `"key": item`.
        code_expr(p);

        let node = &mut p[m];
        let pair_kind = match node.kind() {
            SyntaxKind::Ident => SyntaxKind::Named,
            _ => SyntaxKind::Keyed,
        };

        if let Some(key) = match node.cast::<ast::Expr<'_>>() {
            Some(ast::Expr::Ident(ident)) => Some(ident.get().clone()),
            Some(ast::Expr::Str(s)) => Some(s.get()),
            _ => None,
        } && !state.seen.insert(key.clone())
        {
            node.convert_to_error(eco_format!("duplicate key: {key}"));
        }

        p.wrap(m, pair_kind);
        state.maybe_just_parens = false;

        if state.kind == Some(SyntaxKind::Array) {
            p[m].expected("expression");
        } else {
            state.kind = Some(SyntaxKind::Dict);
        }
    } else {
        // Parses a positional item.
        if state.kind == Some(SyntaxKind::Dict) {
            p[m].expected("named or keyed pair");
        } else {
            state.kind = Some(SyntaxKind::Array)
        }
    }
}

/// Parses a function call's argument list: `(12pt, y)`.
fn args(p: &mut Parser<'_>) {
    if !p.directly_at(SyntaxKind::LeftParen) && !p.directly_at(SyntaxKind::LeftBracket) {
        p.expected("argument list");
        if p.at(SyntaxKind::LeftParen) || p.at(SyntaxKind::LeftBracket) {
            p.hint("there may not be any spaces before the argument list");
        }
    }

    let m = p.marker();
    if p.at(SyntaxKind::LeftParen) {
        let m2 = p.marker();
        p.with_nl_mode(AtNewline::Continue, |p| {
            p.assert(SyntaxKind::LeftParen);

            let mut seen = FxHashSet::default();
            while !p.current().is_terminator() {
                if p.style_values {
                    if !p.at_set(style::ARG_START) {
                        p.unexpected();
                        continue;
                    }
                    style::template_arg(p, &mut seen);
                } else {
                    if !p.at_set(set::ARG) {
                        p.unexpected();
                        continue;
                    }
                    arg(p, &mut seen);
                }

                if !p.current().is_terminator() {
                    p.expect(SyntaxKind::Comma);
                }
            }

            p.expect_closing_delimiter(m2, SyntaxKind::RightParen);
        });
    }

    while p.directly_at(SyntaxKind::LeftBracket) {
        content_block(p);
    }

    p.wrap(m, SyntaxKind::Args);
}

/// Parses a single argument in an argument list.
fn arg(p: &mut Parser<'_>, seen: &mut FxHashSet<EcoString>) {
    let m = p.marker();

    // Parses a spread argument: `..args`.
    if p.eat_if(SyntaxKind::Dots) {
        code_expr(p);
        p.wrap(m, SyntaxKind::Spread);
        return;
    }

    // Parses a normal positional argument or an argument name.
    let was_at_expr = p.at_set(set::CODE_EXPR);
    let name = p.current_text();
    let mut key = EcoString::from(name);
    code_expr(p);

    // Parses the scope of a named argument limited to a variant or target:
    // `override@markscheme: (..)` (SPEC §5.4).
    if p.directly_at(SyntaxKind::At)
        && p.nodes.get(m.0).map(SyntaxNode::kind) == Some(SyntaxKind::Ident)
    {
        p.eat();
        if p.directly_at(SyntaxKind::Ident) {
            key = eco_format!("{name}@{}", p.current_text());
            p.eat();
        } else {
            p.expected("variant or target name");
        }
        if !p.at(SyntaxKind::Colon) {
            p.expected("colon");
        }
    }

    // Parses a named argument: `thickness: 12pt`.
    if p.eat_if(SyntaxKind::Colon) {
        // Recover from bad argument name.
        if was_at_expr {
            if p[m].kind() != SyntaxKind::Ident {
                p[m].expected("identifier");
            } else if !seen.insert(key.clone()) {
                p[m].convert_to_error(eco_format!("duplicate argument: {key}"));
            }
        }

        code_expr(p);
        p.wrap(m, SyntaxKind::Named);
    }
}

/// Parses a closure's parameters: `(x, y)`.
fn params(p: &mut Parser<'_>) {
    let m = p.marker();
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftParen);

        let mut seen = FxHashSet::default();
        let mut sink = false;

        while !p.current().is_terminator() {
            if !p.at_set(set::PARAM) {
                p.unexpected();
                continue;
            }

            param(p, &mut seen, &mut sink);

            if !p.current().is_terminator() {
                p.expect(SyntaxKind::Comma);
            }
        }

        p.expect_closing_delimiter(m, SyntaxKind::RightParen);
    });
    p.wrap(m, SyntaxKind::Params);
}

/// Parses a single parameter in a parameter list.
fn param<'s>(p: &mut Parser<'s>, seen: &mut FxHashSet<&'s str>, sink: &mut bool) {
    let m = p.marker();

    // Parses argument sink: `..sink`.
    if p.eat_if(SyntaxKind::Dots) {
        if p.at_set(set::PATTERN_LEAF) {
            pattern_leaf(p, false, seen, Some("parameter"));
        }
        p.wrap(m, SyntaxKind::Spread);
        if mem::replace(sink, true) {
            p[m].convert_to_error("only one argument sink is allowed");
        }
        return;
    }

    // Parses a normal positional parameter or a parameter name.
    let was_at_pat = p.at_set(set::PATTERN);
    pattern(p, false, seen, Some("parameter"));

    // Parses a named parameter: `thickness: 12pt`.
    if p.eat_if(SyntaxKind::Colon) {
        // Recover from bad parameter name.
        if was_at_pat && p[m].kind() != SyntaxKind::Ident {
            p[m].expected("identifier");
        }

        code_expr(p);
        p.wrap(m, SyntaxKind::Named);
    }
}

/// Parses a binding or reassignment pattern.
fn pattern<'s>(
    p: &mut Parser<'s>,
    reassignment: bool,
    seen: &mut FxHashSet<&'s str>,
    dupe: Option<&'s str>,
) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };

    match p.current() {
        SyntaxKind::Underscore => p.eat(),
        SyntaxKind::LeftParen => destructuring_or_parenthesized(p, reassignment, seen),
        _ => pattern_leaf(p, reassignment, seen, dupe),
    }
}

/// Parses a destructuring pattern or just a parenthesized pattern.
fn destructuring_or_parenthesized<'s>(
    p: &mut Parser<'s>,
    reassignment: bool,
    seen: &mut FxHashSet<&'s str>,
) {
    let mut sink = false;
    let mut count = 0;
    let mut maybe_just_parens = true;

    let m = p.marker();
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftParen);

        while !p.current().is_terminator() {
            if !p.at_set(set::DESTRUCTURING_ITEM) {
                p.unexpected();
                continue;
            }

            destructuring_item(p, reassignment, seen, &mut maybe_just_parens, &mut sink);
            count += 1;

            if !p.current().is_terminator() && p.expect(SyntaxKind::Comma) {
                maybe_just_parens = false;
            }
        }

        p.expect_closing_delimiter(m, SyntaxKind::RightParen);
    });

    if maybe_just_parens && count == 1 && !sink {
        p.wrap(m, SyntaxKind::Parenthesized);
    } else {
        p.wrap(m, SyntaxKind::Destructuring);
    }
}

/// Parses an item in a destructuring pattern.
fn destructuring_item<'s>(
    p: &mut Parser<'s>,
    reassignment: bool,
    seen: &mut FxHashSet<&'s str>,
    maybe_just_parens: &mut bool,
    sink: &mut bool,
) {
    let m = p.marker();

    // Parse destructuring sink: `..rest`.
    if p.eat_if(SyntaxKind::Dots) {
        if p.at_set(set::PATTERN_LEAF) {
            pattern_leaf(p, reassignment, seen, None);
        }
        p.wrap(m, SyntaxKind::Spread);
        if mem::replace(sink, true) {
            p[m].convert_to_error("only one destructuring sink is allowed");
        }
        return;
    }

    // Parse a normal positional pattern or a destructuring key.
    let was_at_pat = p.at_set(set::PATTERN);

    // We must use a full checkpoint here (can't just clone the lexer) because
    // there may be trivia between the identifier and the colon we need to skip.
    let checkpoint = p.checkpoint();
    if !(p.eat_if(SyntaxKind::Ident) && p.at(SyntaxKind::Colon)) {
        p.restore(checkpoint);
        pattern(p, reassignment, seen, None);
    }

    // Parse named destructuring item.
    if p.eat_if(SyntaxKind::Colon) {
        // Recover from bad named destructuring.
        if was_at_pat && p[m].kind() != SyntaxKind::Ident {
            p[m].expected("identifier");
        }

        pattern(p, reassignment, seen, None);
        p.wrap(m, SyntaxKind::Named);
        *maybe_just_parens = false;
    }
}

/// Parses a leaf in a pattern - either an identifier or an expression
/// depending on whether it's a binding or reassignment pattern.
fn pattern_leaf<'s>(
    p: &mut Parser<'s>,
    reassignment: bool,
    seen: &mut FxHashSet<&'s str>,
    dupe: Option<&'s str>,
) {
    if p.current().is_keyword() {
        p.eat_and_get().expected("pattern");
        return;
    } else if !p.at_set(set::PATTERN_LEAF) {
        p.expected("pattern");
        return;
    }

    let m = p.marker();
    let text = p.current_text();

    // We parse an atomic expression even though we only want an identifier for
    // better error recovery. We can mark the whole expression as unexpected
    // instead of going through its pieces one by one.
    code_expr_prec(p, true, 0);

    if !reassignment {
        let node = &mut p[m];
        if node.kind() == SyntaxKind::Ident {
            if !seen.insert(text) {
                node.convert_to_error(eco_format!(
                    "duplicate {}: {text}",
                    dupe.unwrap_or("binding"),
                ));
            }
        } else {
            node.expected("pattern");
        }
    }
}

/// Manages parsing a stream of tokens into a tree of [`SyntaxNode`]s.
///
/// The implementation presents an interface that investigates a current `token`
/// with a [`SyntaxKind`] and can take one of the following actions:
///
/// 1. Eat a token: push `token` onto the `nodes` vector as a [leaf
///    node](`SyntaxNode::leaf`) and prepare a new `token` by calling into the
///    lexer.
/// 2. Wrap nodes from a marker to the end of `nodes` (excluding `token` and any
///    attached trivia) into an [inner node](`SyntaxNode::inner`) of a specific
///    `SyntaxKind`.
/// 3. Produce or convert nodes into an [error node](`SyntaxNode::error`) when
///    something expected is missing or something unexpected is found.
///
/// Overall the parser produces a nested tree of SyntaxNodes as a "_Concrete_
/// Syntax Tree." The raw Concrete Syntax Tree should contain the entire source
/// text, and is used as-is for e.g. syntax highlighting and IDE features. In
/// `ast.rs` the CST is interpreted as a lazy view over an "_Abstract_ Syntax
/// Tree." The AST module skips over irrelevant tokens -- whitespace, comments,
/// code parens, commas in function args, etc. -- as it iterates through the
/// tree.
///
/// ### Modes
///
/// The parser manages the transitions between the three modes of Folio through
/// [syntax modes](`SyntaxMode`) and [newline modes](`AtNewline`).
///
/// The syntax modes map to the three Folio modes and are stored in the lexer,
/// changing which `SyntaxKind`s it will generate.
///
/// The newline mode is used to determine whether a newline should end the
/// current expression. If so, the parser temporarily changes `token`'s kind to
/// a fake [`SyntaxKind::End`]. When the parser exits the mode the original
/// `SyntaxKind` is restored.
struct Parser<'s> {
    /// The source text shared with the lexer.
    text: &'s str,
    /// A lexer over the source text with multiple modes. Defines the boundaries
    /// of tokens and determines their [`SyntaxKind`]. Contains the [`SyntaxMode`]
    /// defining our current Folio mode.
    lexer: Lexer<'s>,
    /// The newline mode: whether to insert a temporary end at newlines.
    nl_mode: AtNewline,
    /// The current token under inspection, not yet present in `nodes`. This
    /// acts like a single item of lookahead for the parser.
    ///
    /// When wrapping, this is _not_ included in the wrapped nodes.
    token: Token,
    /// Whether the parser has the expected set of open/close delimiters. This
    /// only ever transitions from `true` to `false`.
    balanced: bool,
    /// Nodes representing the concrete syntax tree of previously parsed text.
    /// In Code and Math, includes previously parsed trivia, but not `token`.
    nodes: Vec<SyntaxNode>,
    /// Parser checkpoints for a given text index. Used for efficient parser
    /// backtracking similar to packrat parsing. See comments above in
    /// [`expr_with_paren`].
    memo: MemoArena,
    /// The current expression nesting depth.
    depth: u32,
    /// How many levels of embedded code in math chains' errors are being
    /// parsed ([`math_chain_code`]).
    chain_code: u32,
    /// Whether the parser is in a stylesheet value or render template, where
    /// a function argument can be several items, braces hold template items
    /// and a name followed by an attribute test is a selector.
    style_values: bool,
    /// Whether the parser is at the level of template items, where a sign
    /// after a space starts a new item (`margin: 0 -1mm`) rather than a
    /// subtraction. Not inside parentheses or conditions.
    split_signs: bool,
    /// Whether the parser is in a cell of a table row, where strong and
    /// emphasized text end at a pipe, as the cell does.
    in_cell: bool,
}

/// A single token returned from the lexer with a cached [`SyntaxKind`] and a
/// record of preceding trivia.
#[derive(Debug, Clone)]
struct Token {
    /// The [`SyntaxKind`] of the current token.
    kind: SyntaxKind,
    /// The [`SyntaxNode`] of the current token, ready to be eaten and pushed
    /// onto the end of `nodes`.
    node: SyntaxNode,
    /// The number of preceding trivia before this token.
    n_trivia: usize,
    /// Whether this token's preceding trivia contained a newline.
    newline: Option<Newline>,
    /// The index into `text` of the start of our current token (the end is
    /// stored as the lexer's cursor).
    start: usize,
    /// The index into `text` of the end of the previous token.
    prev_end: usize,
}

/// Information about newlines in a group of trivia.
#[derive(Debug, Copy, Clone)]
struct Newline {
    /// The column of the start of the next token in its line.
    column: Option<usize>,
    /// Whether any of our newlines were paragraph breaks.
    parbreak: bool,
}

/// How to proceed with parsing when at a newline.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum AtNewline {
    /// Continue at newlines.
    Continue,
    /// Stop at any newline.
    Stop,
    /// Continue only if there is a continuation with `else` or `.` (Code only).
    ContextualContinue,
    /// Stop only at a parbreak, not normal newlines (Markup only).
    StopParBreak,
    /// Require that the token's column be greater or equal to a column (Markup
    /// only). If this is `0`, acts like `Continue`; if this is `usize::MAX`,
    /// acts like `Stop`.
    RequireColumn(usize),
}

impl AtNewline {
    /// Whether to stop at a newline or continue based on the current context.
    fn stop_at(self, Newline { column, parbreak }: Newline, kind: SyntaxKind) -> bool {
        #[allow(clippy::match_like_matches_macro)]
        match self {
            AtNewline::Continue => false,
            AtNewline::Stop => true,
            AtNewline::ContextualContinue => match kind {
                SyntaxKind::Else | SyntaxKind::Dot => false,
                _ => true,
            },
            AtNewline::StopParBreak => parbreak,
            AtNewline::RequireColumn(min_col) => {
                // When the column is `None`, the newline doesn't start a
                // column, and we continue parsing. This may happen on the
                // boundary of syntax modes, since we only report a column in
                // Markup.
                column.is_some_and(|column| column <= min_col)
            }
        }
    }
}

/// A marker representing a node's position in the parser. Mainly used for
/// wrapping, but can also index into the parser to access the node, like
/// `p[m]`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct Marker(usize);

// Index into the parser with markers.
impl Index<Marker> for Parser<'_> {
    type Output = SyntaxNode;

    fn index(&self, m: Marker) -> &Self::Output {
        &self.nodes[m.0]
    }
}

impl IndexMut<Marker> for Parser<'_> {
    fn index_mut(&mut self, m: Marker) -> &mut Self::Output {
        &mut self.nodes[m.0]
    }
}

/// Creating/Consuming the parser and getting info about the current token.
impl<'s> Parser<'s> {
    /// Create a new parser starting from the given text offset and syntax mode.
    fn new(text: &'s str, offset: usize, mode: SyntaxMode) -> Self {
        Self::new_in(text, offset, mode, false)
    }

    /// Create a new parser starting from the given text offset and syntax
    /// mode, for a stylesheet if `style` is set.
    fn new_in(text: &'s str, offset: usize, mode: SyntaxMode, style: bool) -> Self {
        let mut lexer = Lexer::new(text, mode);
        lexer.set_style(style);
        lexer.jump(offset);
        let nl_mode = AtNewline::Continue;
        let mut nodes = vec![];
        let token = Self::lex(&mut nodes, &mut lexer, nl_mode);
        Self {
            text,
            lexer,
            nl_mode,
            token,
            balanced: true,
            nodes,
            memo: Default::default(),
            depth: 0,
            chain_code: 0,
            style_values: false,
            split_signs: false,
            in_cell: false,
        }
    }

    /// Consume the parser, yielding the full vector of parsed SyntaxNodes.
    fn finish(self) -> Vec<SyntaxNode> {
        self.nodes
    }

    /// Consume the parser, generating a single top-level node.
    fn finish_into(self, kind: SyntaxKind) -> SyntaxNode {
        assert!(self.at(SyntaxKind::End));
        SyntaxNode::inner(kind, self.finish())
    }

    /// Similar to a `peek()` function: returns the `kind` of the next token to
    /// be eaten.
    fn current(&self) -> SyntaxKind {
        self.token.kind
    }

    /// Whether the current token is a given [`SyntaxKind`].
    fn at(&self, kind: SyntaxKind) -> bool {
        self.token.kind == kind
    }

    /// Whether the current token is contained in a [`SyntaxSet`].
    fn at_set(&self, set: SyntaxSet) -> bool {
        set.contains(self.token.kind)
    }

    /// Whether we're at the end of the token stream.
    ///
    /// Note: This might be a fake end due to the newline mode.
    fn end(&self) -> bool {
        self.at(SyntaxKind::End)
    }

    /// If we're at the given `kind` with no preceding trivia tokens.
    fn directly_at(&self, kind: SyntaxKind) -> bool {
        self.token.kind == kind && !self.had_trivia()
    }

    /// Whether `token` had any preceding trivia.
    fn had_trivia(&self) -> bool {
        self.token.n_trivia > 0
    }

    /// Whether `token` had a newline among any of its preceding trivia.
    /// Whether only spaces and tabs stand before the current token on its
    /// line: a table row starts only there, not after a list marker or the
    /// `[` of a content block.
    fn at_line_start(&self) -> bool {
        self.text[..self.token.start]
            .chars()
            .rev()
            .take_while(|&c| !crate::is_newline(c))
            .all(|c| c == ' ' || c == '\t')
    }

    fn had_newline(&self) -> bool {
        self.token.newline.is_some()
    }

    /// The number of characters until the most recent newline from the start of
    /// the current token. Uses a cached value from the newline mode if present.
    fn current_column(&self) -> usize {
        self.token
            .newline
            .and_then(|newline| newline.column)
            .unwrap_or_else(|| self.lexer.column(self.token.start))
    }

    /// The current token's text.
    fn current_text(&self) -> &'s str {
        &self.text[self.token.start..self.current_end()]
    }

    /// The offset into `text` of the current token's start.
    fn current_start(&self) -> usize {
        self.token.start
    }

    /// The offset into `text` of the current token's end.
    fn current_end(&self) -> usize {
        self.lexer.cursor()
    }

    /// The offset into `text` of the previous token's end.
    fn prev_end(&self) -> usize {
        self.token.prev_end
    }

    /// The kind of the next non-trivia token after the current one.
    fn peek(&self) -> Option<SyntaxKind> {
        let mut lexer = self.lexer.clone();
        loop {
            match lexer.next().0 {
                SyntaxKind::End => return None,
                kind if kind.is_trivia() => {}
                kind => return Some(kind),
            }
        }
    }

    /// The kind of the next non-trivia token after the current one, if it is
    /// separated from the current token by trivia without a newline.
    fn peek_after_space(&self) -> Option<SyntaxKind> {
        let mut lexer = self.lexer.clone();
        let mut kind = lexer.next().0;
        if !kind.is_trivia() {
            return None;
        }
        while kind.is_trivia() {
            if lexer.newline() {
                return None;
            }
            kind = lexer.next().0;
        }
        (kind != SyntaxKind::End).then_some(kind)
    }
}

// The main parsing interface for generating tokens and eating/modifying nodes.
impl<'s> Parser<'s> {
    /// A marker that will point to the current token in the parser once it's
    /// been eaten.
    fn marker(&self) -> Marker {
        Marker(self.nodes.len())
    }

    /// A marker that will point to first trivia before this token in the
    /// parser (or the token itself if no trivia precede it).
    fn before_trivia(&self) -> Marker {
        Marker(self.nodes.len() - self.token.n_trivia)
    }

    /// Eat the current node and return a reference for in-place mutation.
    #[track_caller]
    fn eat_and_get(&mut self) -> &mut SyntaxNode {
        let offset = self.nodes.len();
        self.eat();
        &mut self.nodes[offset]
    }

    /// Eat the token if at `kind`. Returns `true` if eaten.
    ///
    /// Note: In Math and Code, this will ignore trivia in front of the
    /// `kind`, To forbid skipping trivia, consider using `eat_if_direct`.
    fn eat_if(&mut self, kind: SyntaxKind) -> bool {
        let at = self.at(kind);
        if at {
            self.eat();
        }
        at
    }

    /// Assert that we are at the given [`SyntaxKind`] and eat it. This should
    /// be used when moving between functions that expect to start with a
    /// specific token.
    #[track_caller]
    fn assert(&mut self, kind: SyntaxKind) {
        assert_eq!(self.token.kind, kind);
        self.eat();
    }

    /// Convert the current token's [`SyntaxKind`] without eating it.
    fn convert_current(&mut self, kind: SyntaxKind) {
        self.token.node.convert_to_kind(kind);
        self.token.kind = kind;
    }

    /// Convert the current token's [`SyntaxKind`] and eat it.
    fn convert_and_eat(&mut self, kind: SyntaxKind) {
        // Only need to replace the node here.
        self.token.node.convert_to_kind(kind);
        self.eat();
    }

    /// Eat the current token by saving it to the `nodes` vector, then move
    /// the lexer forward to prepare a new token.
    fn eat(&mut self) {
        self.nodes.push(mem::take(&mut self.token.node));
        self.token = Self::lex(&mut self.nodes, &mut self.lexer, self.nl_mode);
    }

    /// Detach the parsed trivia nodes from this token (but not newline info) so
    /// that subsequent wrapping will include the trivia.
    fn flush_trivia(&mut self) {
        self.token.n_trivia = 0;
        self.token.prev_end = self.token.start;
    }

    /// Wrap the nodes from a marker up to (but excluding) the current token in
    /// a new [inner node](`SyntaxNode::inner`) of the given kind. This is an
    /// easy interface for creating nested syntax nodes _after_ having parsed
    /// their children.
    fn wrap(&mut self, from: Marker, kind: SyntaxKind) {
        let to = self.before_trivia().0;
        let from = from.0.min(to);
        let children = self.nodes.drain(from..to).collect();
        self.nodes.insert(from, SyntaxNode::inner(kind, children));
    }

    /// Wrap the nodes from a marker up to (but excluding) the current token in
    /// a new [error node](`SyntaxNode::error`) with the given message. This is
    /// an easy interface for creating a syntax error _after_ having parsed its
    /// children.
    fn wrap_error(&mut self, from: Marker, message: impl Into<EcoString>) {
        let to = self.before_trivia().0;
        let from = from.0.min(to);
        let len: usize = self.nodes.drain(from..to).map(|node| node.len()).sum();
        let end = self.token.prev_end;
        let start = end - len;
        let text = &self.text[start..end];
        self.nodes
            .insert(from, SyntaxNode::error(message.into(), text));
    }

    /// Parse within the [`SyntaxMode`] for subsequent tokens (does not change the
    /// current token). This may re-lex the final token on exit.
    ///
    /// This function effectively repurposes the call stack as a stack of modes.
    fn enter_modes(
        &mut self,
        mode: SyntaxMode,
        stop: AtNewline,
        func: impl FnOnce(&mut Parser<'s>),
    ) {
        let style = self.lexer.style();
        self.enter_lexer(mode, style, stop, func);
    }

    /// Like [`Self::enter_modes`], but also sets whether code is lexed as in
    /// a stylesheet.
    fn enter_lexer(
        &mut self,
        mode: SyntaxMode,
        style: bool,
        stop: AtNewline,
        func: impl FnOnce(&mut Parser<'s>),
    ) {
        let previous = self.lexer.mode();
        let previous_style = self.lexer.style();
        self.lexer.set_mode(mode);
        self.lexer.set_style(style);
        self.with_nl_mode(stop, func);
        if mode != previous || style != previous_style {
            self.lexer.set_mode(previous);
            self.lexer.set_style(previous_style);
            self.lexer.jump(self.token.prev_end);
            self.nodes.truncate(self.nodes.len() - self.token.n_trivia);
            self.token = Self::lex(&mut self.nodes, &mut self.lexer, self.nl_mode);
        }
    }

    /// Lex the current token, a `#` before `{` in content code, as an id
    /// selector: `#{fig:ball}`.
    fn relex_id_selector(&mut self) {
        let start = self.token.start;
        self.lexer.jump(start + 1);
        let (kind, node) = self.lexer.id_selector(start);
        self.token.kind = kind;
        self.token.node = node;
    }

    /// Parse within the [`AtNewline`] mode for subsequent tokens (does not
    /// change the current token). This may re-lex the final token on exit.
    ///
    /// This function effectively repurposes the call stack as a stack of modes.
    fn with_nl_mode(&mut self, mode: AtNewline, func: impl FnOnce(&mut Parser<'s>)) {
        let previous = self.nl_mode;
        self.nl_mode = mode;
        func(self);
        self.nl_mode = previous;
        if let Some(newline) = self.token.newline
            && mode != previous
        {
            // Restore our actual token's kind or insert a fake end.
            let actual_kind = self.token.node.kind();
            if self.nl_mode.stop_at(newline, actual_kind) {
                self.token.kind = SyntaxKind::End;
            } else {
                self.token.kind = actual_kind;
            }
        }
    }

    /// Move the lexer forward and prepare the current token. In Code, this
    /// might insert a temporary [`SyntaxKind::End`] based on our newline mode.
    ///
    /// This is not a method on `self` because we need a valid token before we
    /// can initialize the parser.
    fn lex(nodes: &mut Vec<SyntaxNode>, lexer: &mut Lexer<'_>, nl_mode: AtNewline) -> Token {
        let prev_end = lexer.cursor();
        let mut start = prev_end;
        let (mut kind, mut node) = lexer.next();
        let mut n_trivia = 0;
        let mut had_newline = false;
        let mut parbreak = false;

        while kind.is_trivia() {
            had_newline |= lexer.newline(); // Newlines are always trivia.
            parbreak |= kind == SyntaxKind::Parbreak;
            n_trivia += 1;
            nodes.push(node);
            start = lexer.cursor();
            (kind, node) = lexer.next();
        }

        let newline = if had_newline {
            let column = (lexer.mode() == SyntaxMode::Markup).then(|| lexer.column(start));
            let newline = Newline { column, parbreak };
            if nl_mode.stop_at(newline, kind) {
                // Insert a temporary `SyntaxKind::End` to halt the parser.
                // The actual kind will be restored from `node` later.
                kind = SyntaxKind::End;
            }
            Some(newline)
        } else {
            None
        };

        Token {
            kind,
            node,
            n_trivia,
            newline,
            start,
            prev_end,
        }
    }
}

/// Extra parser state for efficiently recovering from mispredicted parses.
///
/// This is the same idea as packrat parsing, but we use it only in the limited
/// case of parenthesized structures. See [`expr_with_paren`] for more.
#[derive(Default)]
struct MemoArena {
    /// A single arena of previously parsed nodes (to reduce allocations).
    /// Memoized ranges refer to unique sections of the arena.
    arena: Vec<SyntaxNode>,
    /// A map from the parser's current position to a range of previously parsed
    /// nodes in the arena and a checkpoint of the parser's state. These allow
    /// us to reset the parser to avoid parsing the same location again.
    memo_map: FxHashMap<MemoKey, (Range<usize>, PartialState)>,
}

/// A type alias for the memo key so it doesn't get confused with other usizes.
///
/// The memo is keyed by the index into `text` of the current token's start.
type MemoKey = usize;

/// A checkpoint of the parser which can fully restore it to a previous state.
struct Checkpoint {
    node_len: usize,
    state: PartialState,
}

/// State needed to restore the parser's current token and the lexer (but not
/// the nodes vector).
#[derive(Clone)]
struct PartialState {
    cursor: usize,
    lex_mode: SyntaxMode,
    token: Token,
}

/// The Memoization interface.
impl Parser<'_> {
    /// Store the already parsed nodes and the parser state into the memo map by
    /// extending the arena and storing the extended range and a checkpoint.
    fn memoize_parsed_nodes(&mut self, key: MemoKey, prev_len: usize) {
        let Checkpoint { state, node_len } = self.checkpoint();
        let memo_start = self.memo.arena.len();
        self.memo
            .arena
            .extend_from_slice(&self.nodes[prev_len..node_len]);
        let arena_range = memo_start..self.memo.arena.len();
        self.memo.memo_map.insert(key, (arena_range, state));
    }

    /// Try to load a memoized result, return `None` if we did or `Some` (with a
    /// checkpoint and a key for the memo map) if we didn't.
    fn restore_memo_or_checkpoint(&mut self) -> Option<(MemoKey, Checkpoint)> {
        // We use the starting index of the current token as our key.
        let key: MemoKey = self.current_start();
        match self.memo.memo_map.get(&key).cloned() {
            Some((range, state)) => {
                self.nodes.extend_from_slice(&self.memo.arena[range]);
                // It's important that we don't truncate the nodes vector since
                // it may have grown or shrunk (due to other memoization or
                // error reporting) since we made this checkpoint.
                self.restore_partial(state);
                None
            }
            None => Some((key, self.checkpoint())),
        }
    }

    /// Restore the parser to the state at a checkpoint.
    fn restore(&mut self, checkpoint: Checkpoint) {
        self.nodes.truncate(checkpoint.node_len);
        self.restore_partial(checkpoint.state);
    }

    /// Restore parts of the checkpoint excluding the nodes vector.
    fn restore_partial(&mut self, state: PartialState) {
        self.lexer.jump(state.cursor);
        self.lexer.set_mode(state.lex_mode);
        self.token = state.token;
    }

    /// Save a checkpoint of the parser state.
    fn checkpoint(&self) -> Checkpoint {
        let node_len = self.nodes.len();
        let state = PartialState {
            cursor: self.lexer.cursor(),
            lex_mode: self.lexer.mode(),
            token: self.token.clone(),
        };
        Checkpoint { node_len, state }
    }
}

/// Functions for eating expected or unexpected tokens and generating errors if
/// we don't get what we expect.
impl Parser<'_> {
    /// Consume the given `kind` or produce an error.
    fn expect(&mut self, kind: SyntaxKind) -> bool {
        let at = self.at(kind);
        if at {
            self.eat();
        } else if kind == SyntaxKind::Ident && self.token.kind.is_keyword() {
            self.trim_errors();
            self.eat_and_get().expected(kind.name());
        } else {
            self.balanced &= !kind.is_grouping();
            self.expected(kind.name());
        }
        at
    }

    /// Consume the given closing delimiter or produce an error for the matching
    /// opening delimiter at `open`.
    #[track_caller]
    fn expect_closing_delimiter(&mut self, open: Marker, kind: SyntaxKind) -> bool {
        let at = self.eat_if(kind);
        if !at {
            self.nodes[open.0].convert_to_error("unclosed delimiter");
        }
        at
    }

    /// Produce an error that the given `thing` was expected. If the parser is
    /// at an erroneous token, this will instead eat that token and continue.
    fn expected(&mut self, thing: &str) {
        if self.token.kind.is_error() {
            // If we encounter an erroneous token when something was expected,
            // we need to actually consume the token. If we don't, and we then
            // proceed to exit our current lexing mode, future incremental
            // reparsing could fail to lex the token in the correct mode.
            //
            // Example: When parsing an unclosed string in `#import "str`,
            // we need to make sure the string is lexed as code, because if
            // it were lexed as markup, a future insert of a closing quote
            // would only be adjacent to markup text, and wouldn't lex as a
            // string when reparsing, causing a difference between the full
            // parse and the incremental parse.
            self.trim_errors();
            self.eat();
        } else if !self.after_error() {
            self.expected_at(self.before_trivia(), thing);
        }
    }

    /// Whether the last non-trivia node is an error.
    fn after_error(&mut self) -> bool {
        let m = self.before_trivia();
        m.0 > 0 && self.nodes[m.0 - 1].kind().is_error()
    }

    /// Produce an error that the given `thing` was expected at the position
    /// of the marker `m`.
    fn expected_at(&mut self, m: Marker, thing: &str) {
        let error = SyntaxNode::error(eco_format!("expected {thing}"), "");
        self.nodes.insert(m.0, error);
    }

    /// Add a hint to a trailing error.
    fn hint(&mut self, hint: &str) {
        let m = self.before_trivia();
        if let Some(error) = self.nodes.get_mut(m.0 - 1) {
            error.hint(hint);
        }
    }

    /// Consume the next token (if any) and produce an error stating that it was
    /// unexpected.
    fn unexpected(&mut self) {
        self.trim_errors();
        self.balanced &= !self.token.kind.is_grouping();
        self.eat_and_get().unexpected();
    }

    /// Remove trailing errors with zero length.
    fn trim_errors(&mut self) {
        let Marker(end) = self.before_trivia();
        let mut start = end;
        while start > 0
            && self.nodes[start - 1].kind().is_error()
            && self.nodes[start - 1].is_empty()
        {
            start -= 1;
        }
        self.nodes.drain(start..end);
    }

    /// Whether the node at `m`, the left operand of an operator chain so far,
    /// is too tall to be wrapped once more ([`MAX_CHAIN_HEIGHT`]).
    fn too_tall(&self, m: Marker) -> bool {
        self.nodes
            .get(m.0)
            .is_some_and(|node| node.height() >= MAX_CHAIN_HEIGHT)
    }

    /// Check if the maximum depth has been exceeded. If so, generate an error
    /// and try to make a best effort recovery using the `stop_set` as a guide.
    ///
    /// This function isn't strictly necessary, but it is an optimization to
    /// generate one combined error instead of an error message for every
    /// balanced set of tokens. In the pathological case an error would be
    /// generated by [`Self::increase_depth`] for every single token until
    /// the `stop_set` of the parent function is reached.
    fn check_depth_until(&mut self, stop_set: SyntaxSet) -> Option<&mut Self> {
        if self.depth < MAX_DEPTH {
            Some(self)
        } else {
            self.depth_check_error(Some(stop_set));
            None
        }
    }

    /// Check if the maximum depth has been exceeded. If so, generate an error
    /// and try to make a best effort recovery. Otherwise increase the depth and
    /// return a handle that automatically decreases it again when dropped.
    fn increase_depth(&mut self) -> Option<impl DerefMut<Target = Self>> {
        if self.depth < MAX_DEPTH {
            self.depth += 1;
            Some(defer(self, |p| p.depth -= 1))
        } else {
            self.depth_check_error(None);
            None
        }
    }

    /// Generate an error for an exceeded maximum depth check.
    fn depth_check_error(&mut self, stop_set: Option<SyntaxSet>) {
        let m = self.marker();

        let mut balance: usize = 0;
        // This function has to guarantee some sort of forward progress,
        // otherwise the parser might loop indefinitely. One token is eaten in
        // all cases, if that token is an opening delimiter, try to balance the
        // opening and closing grouping delimiters before continuing.
        self.with_nl_mode(AtNewline::Continue, |p| {
            loop {
                if p.at_set(syntax_set!(LeftBracket, LeftBrace, LeftParen)) {
                    balance = balance.saturating_add(1);
                } else if p.at_set(syntax_set!(RightBracket, RightBrace, RightParen)) {
                    balance = balance.saturating_sub(1);
                }
                p.eat();

                let at_stop = stop_set.is_none_or(|s| p.at_set(s));
                if (balance == 0 && at_stop) || p.end() {
                    break;
                }
            }
        });

        self.wrap_error(m, "maximum parsing depth exceeded");
    }
}
