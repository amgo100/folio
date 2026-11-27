//! Native math (SPEC §8.1): evaluating an equation's syntax into a formula
//! of the math tree ([`folio_math`]).
//!
//! An equation's body is split into lines at its top-level `\` line breaks,
//! and a label that ends a line names it (`<eq:suvat>`). Alignment points
//! `&` stay in their line, or in their case of `cases(..)`, and are errors
//! anywhere else. Single letters are variables. A longer name is looked up
//! first in the script's own scopes, so `#let` bindings and the parameters
//! of a closure can be used in equations, and then among the names of math
//! ([`library::math_item`](super::library::math_item)): symbols, operators,
//! `dif`, and functions such as `frac` and `vec`. A name neither knows is an
//! error: a product of variables is written with spaces, `a s`, and text
//! in quotes, `"as"`.
//!
//! Values embedded with `#` become part of the formula: a formula is
//! placed as its row, a number as a number, a string as text. Calling a
//! script's function in an equation passes it each argument as an inline
//! formula, and places what it returns.

use std::sync::Arc;

use ecow::{EcoString, eco_format};
use folio_component::{Name, QualifiedName};
use folio_math::{self as math, Atom, Formula, Line, MathClass, Number, Row, Unit};
use folio_syntax::ast::{self, AstNode, MathTextKind};
use folio_syntax::{Span, SubRange, SyntaxKind};

use super::MAX_NESTING;
use super::content::{Content, Node};
use super::error::{EvalError, EvalResult, Stop, bail};
use super::library::{MathFunc, MathItem, math_item};
use super::ops::a_type;
use super::value::{Arg, Args, Value};
use super::vm::{Vm, nesting_exceeded};

/// The bytes a node of the math tree counts against the memory limit: at
/// least what a node and its share of the row that holds it take.
const NODE: u64 = 64;

/// The most nodes a formula may hold, counting a part as often as it
/// occurs (SPEC §4.4), as [`Row::size`] counts them.
///
/// A formula placed in another is shared, not copied, so a script that
/// places a formula twice in each of its steps doubles the formula's tree
/// at little cost. Text collection and layout walk the whole tree, at
/// about a microsecond a node, so the limit keeps one formula's layout
/// within a tenth of a second. A page of display maths holds a few hundred
/// nodes.
pub(crate) const MAX_FORMULA_NODES: u64 = 100_000;

/// Where a math expression stands, which decides whether it may hold an
/// alignment point.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Level {
    /// The top level of a line of the formula.
    Line,
    /// The top level of a case of `cases(..)`.
    Case,
    /// Anywhere else.
    Nested,
}

impl Vm<'_> {
    /// Evaluates an equation's body into a formula of one line per part
    /// between top-level line breaks.
    pub(super) fn formula(&mut self, body: ast::Math<'_>, span: Span) -> EvalResult<Formula> {
        let mut lines = Vec::new();
        let mut nodes = Vec::new();
        let mut label: Option<math::Label> = None;
        let mut start: Option<Span> = None;
        for expr in body.exprs() {
            match expr {
                ast::Expr::Space(_) => continue,
                ast::Expr::Linebreak(linebreak) => {
                    let row = self.row_of(std::mem::take(&mut nodes), linebreak.span())?;
                    lines.push(Line {
                        row,
                        label: label.take(),
                        span: start.take().unwrap_or(linebreak.span()),
                    });
                    continue;
                }
                ast::Expr::Label(new) => {
                    // The lexer makes a label only where it ends a line, so
                    // a second one cannot follow the first.
                    label = Some(math::Label {
                        name: new.get().into(),
                        span: new.span(),
                    });
                    start.get_or_insert(new.span());
                    continue;
                }
                _ => {}
            }
            if let Some(label) = &label {
                return Err(label_not_at_end(label.span).into());
            }
            start.get_or_insert(expr.span());
            self.math_expr(expr, Level::Line, &mut nodes)?;
        }
        if !nodes.is_empty() || label.is_some() {
            let row = self.row_of(nodes, span)?;
            lines.push(Line {
                row,
                label,
                span: start.unwrap_or(span),
            });
        }
        let formula = Formula::new(lines);
        if formula_size(&formula) > MAX_FORMULA_NODES {
            return Err(too_large(span).into());
        }
        Ok(formula)
    }

    /// Reads a LaTeX equation's body into a formula (SPEC §8.1). Its nodes
    /// have the span of the body, and its errors point at the part of the
    /// body they are about.
    pub(super) fn tex_formula(
        &mut self,
        eq: ast::TexEquation<'_>,
        span: Span,
    ) -> EvalResult<Formula> {
        if eq.mode() != "tex" {
            // The lexer reports an unknown mode.
            return Err(Stop::Reported);
        }
        let Some(body) = eq
            .to_untyped()
            .children()
            .find(|node| node.kind() == SyntaxKind::TexText)
        else {
            return Ok(Formula::default());
        };
        let text = body.leaf_text();
        let formula = math::tex::parse(text, body.span(), MAX_NESTING).map_err(|error| {
            let message = if error.too_deep {
                eco_format!("math is nested more than {MAX_NESTING} deep")
            } else {
                error.message
            };
            let mut eval_error = EvalError::new(body.span(), message)
                .with_sub_range(tex_sub_range(text, error.range));
            for hint in error.hints {
                eval_error = eval_error.with_hint(hint);
            }
            Stop::from(eval_error)
        })?;
        if formula.depth() > MAX_NESTING {
            return Err(nesting_exceeded(body.span(), "math is").into());
        }
        let size = formula_size(&formula);
        if size > MAX_FORMULA_NODES {
            return Err(too_large(body.span()).into());
        }
        self.alloc(size.saturating_mul(NODE), span)?;
        Ok(formula)
    }

    /// An inline or display `math` element holding a formula.
    pub(super) fn math_content(
        &mut self,
        formula: Formula,
        block: bool,
        span: Span,
    ) -> EvalResult<Content> {
        let content = self.builtin("math", [("block", Value::Bool(block))], span)?;
        let Some(element) = content.as_element() else {
            return Ok(content);
        };
        let mut element = element.clone();
        element.fields.insert("tree".into(), Value::Math(formula));
        Ok(Content::node(Node::Element(element)))
    }

    /// A row of nodes, which may nest at most [`MAX_NESTING`] deep and hold
    /// at most [`MAX_FORMULA_NODES`] nodes.
    fn row_of(&mut self, nodes: Vec<math::Node>, span: Span) -> EvalResult<Row> {
        self.alloc(nodes.len() as u64 * NODE, span)?;
        let row = Row::new(nodes);
        if row.depth() > MAX_NESTING {
            return Err(nesting_exceeded(span, "math is").into());
        }
        if row.size() > MAX_FORMULA_NODES {
            return Err(too_large(span).into());
        }
        Ok(row)
    }

    /// Evaluates one math expression into a row.
    fn math_row(&mut self, expr: ast::Expr<'_>, level: Level) -> EvalResult<Row> {
        let mut nodes = Vec::new();
        self.math_expr(expr, level, &mut nodes)?;
        self.row_of(nodes, expr.span())
    }

    /// Evaluates one math expression, appending its nodes to `out`.
    fn math_expr(
        &mut self,
        expr: ast::Expr<'_>,
        level: Level,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        let span = expr.span();
        self.step(span)?;
        match expr {
            ast::Expr::Space(_) => {}
            // A group: what parentheses held, or a base and its arguments.
            ast::Expr::Math(group) => {
                for child in group.exprs() {
                    self.math_expr(child, level, out)?;
                }
            }
            ast::Expr::MathText(text) => out.push(match text.get() {
                MathTextKind::Number(number) => math::Node::Number(Atom {
                    text: number.clone(),
                    class: MathClass::Normal,
                    span,
                }),
                MathTextKind::Grapheme(grapheme) => math::Node::grapheme(grapheme.clone(), span),
            }),
            ast::Expr::MathIdent(ident) => self.math_name(ident.get(), level, span, out)?,
            ast::Expr::MathFieldAccess(access) => {
                let name = access.to_untyped().full_text();
                match folio_math::symbol(&name) {
                    Some(c) => out.push(math::Node::symbol(c, span)),
                    None if matches!(math_item(&name, true), Some(MathItem::Func(_))) => {
                        return Err(not_called(&name, span).into());
                    }
                    None => return Err(unknown_symbol(&name, span).into()),
                }
            }
            ast::Expr::MathShorthand(shorthand) => {
                out.push(math::Node::symbol(shorthand.get(), span))
            }
            ast::Expr::Escape(escape) => out.push(math::Node::grapheme(escape.get().into(), span)),
            ast::Expr::Str(text) => out.push(math::Node::Text(Atom {
                text: text.get(),
                class: MathClass::Normal,
                span,
            })),
            ast::Expr::MathPrimes(primes) => {
                out.push(math::Node::symbol_text(primes_text(primes.count()), span));
            }
            ast::Expr::MathAlignPoint(_) => {
                if level == Level::Nested {
                    return Err(misplaced_align_point(span)
                        .with_hint("`&` aligns the lines of an equation, or the parts of a case")
                        .into());
                }
                out.push(math::Node::AlignPoint(span));
            }
            ast::Expr::Linebreak(_) => {
                return Err(EvalError::new(
                    span,
                    "a line break can only be at the top level of an equation",
                )
                .with_hint("cases are separated by commas: `cases(a, b)`")
                .into());
            }
            ast::Expr::Label(label) => return Err(label_not_at_end(label.span()).into()),
            ast::Expr::MathDelimited(delimited) => {
                let body = self.math_row(ast::Expr::Math(delimited.body()), Level::Nested)?;
                out.push(math::Node::Delimited(Arc::new(math::Delimited {
                    open: delimiter(delimited.open()),
                    body,
                    close: delimiter(delimited.close()),
                    span,
                })));
            }
            ast::Expr::MathAttach(attach) => {
                let base = self.math_row(attach.base(), Level::Nested)?;
                let sub = match attach.bottom() {
                    Some(sub) => Some(self.math_row(sub, Level::Nested)?),
                    None => None,
                };
                let sup = match attach.top() {
                    Some(sup) => Some(self.math_row(sup, Level::Nested)?),
                    None => None,
                };
                let primes = attach.primes().map_or(0, |p| p.count());
                out.push(math::Node::attach(base, sub, sup, primes, span));
            }
            ast::Expr::MathFrac(frac) => {
                let num = self.math_row(frac.num(), Level::Nested)?;
                let den = self.math_row(frac.denom(), Level::Nested)?;
                out.push(math::Node::Frac(Arc::new(math::Frac { num, den, span })));
            }
            ast::Expr::MathRoot(root) => {
                let index = match root.index() {
                    Some(index) => Some(self.row_of(
                        vec![math::Node::Number(Atom {
                            text: eco_format!("{index}"),
                            class: MathClass::Normal,
                            span,
                        })],
                        span,
                    )?),
                    None => None,
                };
                let radicand = self.math_row(root.radicand(), Level::Nested)?;
                out.push(math::Node::Root(Arc::new(math::Root {
                    index,
                    radicand,
                    span,
                })));
            }
            ast::Expr::MathCall(call) => self.math_call(call, level, out)?,
            // Code embedded with `#`.
            _ => {
                let value = self.expr(expr)?;
                self.place_in_math(value, level, span, out)?;
            }
        }
        Ok(())
    }

    /// A name in an equation: a binding of the script, or a name of math.
    fn math_name(
        &mut self,
        name: &EcoString,
        level: Level,
        span: Span,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        if let Some(value) = self.math_binding(name)? {
            return self.place_in_math(value, level, span, out);
        }
        match math_item(name, false) {
            Some(MathItem::Symbol(c)) => out.push(math::Node::symbol(c, span)),
            Some(MathItem::Operator { limits }) => out.push(math::Node::Operator(math::Operator {
                name: name.clone(),
                limits,
                span,
            })),
            Some(MathItem::Differential) => out.push(math::Node::Differential(span)),
            Some(MathItem::Func(_)) => return Err(not_called(name, span).into()),
            None => return Err(unknown_name(name, span).into()),
        }
        Ok(())
    }

    /// A call in an equation: of a script's function, of a function of
    /// math, or of something that is not a function, which is set before
    /// its arguments in parentheses, as `f(x)` is.
    fn math_call(
        &mut self,
        call: ast::MathCall<'_>,
        level: Level,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        let span = call.span();
        let callee = call.callee();
        let name: EcoString = match callee {
            ast::MathAccess::MathIdent(ident) => ident.get().clone(),
            ast::MathAccess::MathFieldAccess(access) => access.to_untyped().full_text(),
        };
        if let ast::MathAccess::MathIdent(_) = callee
            && let Some(value) = self.math_binding(&name)?
        {
            if let Value::Func(func) = value {
                let args = self.math_func_args(call.args(), false)?;
                let result = self.call(func, args, span)?;
                return self.place_in_math(result, level, span, out);
            }
            self.place_in_math(value, level, callee.span(), out)?;
            return self.call_as_group(call.args(), out);
        }
        match math_item(&name, true) {
            Some(MathItem::Func(func)) => {
                let node = self.math_func(func, &name, call.args(), span)?;
                out.push(node);
                Ok(())
            }
            Some(_) => {
                match callee {
                    ast::MathAccess::MathIdent(ident) => {
                        self.math_name(ident.get(), Level::Nested, ident.span(), out)?
                    }
                    ast::MathAccess::MathFieldAccess(access) => {
                        self.math_expr(ast::Expr::MathFieldAccess(access), Level::Nested, out)?
                    }
                }
                self.call_as_group(call.args(), out)
            }
            None => match callee {
                ast::MathAccess::MathIdent(_) => Err(unknown_name(&name, callee.span()).into()),
                ast::MathAccess::MathFieldAccess(_) => {
                    Err(unknown_symbol(&name, callee.span()).into())
                }
            },
        }
    }

    /// The arguments of a call that is not a function's, set as they were
    /// written: in parentheses, separated by commas and semicolons. Named
    /// arguments are errors.
    fn call_as_group(
        &mut self,
        args: ast::MathArgs<'_>,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        let mut body = Vec::new();
        for item in args.content_items() {
            match item {
                ast::MathArgItem::Arg(ast::Arg::Pos(expr)) => {
                    self.math_expr(expr, Level::Nested, &mut body)?
                }
                ast::MathArgItem::Arg(arg) => {
                    bail!(
                        arg.span(),
                        "only a function can take named or spread arguments"
                    )
                }
                ast::MathArgItem::Comma(c, node) | ast::MathArgItem::Semicolon(c, node) => {
                    body.push(math::Node::symbol(c, node.span()))
                }
                ast::MathArgItem::Space(_)
                | ast::MathArgItem::LeftParen(..)
                | ast::MathArgItem::RightParen(..) => {}
            }
        }
        let span = args.span();
        let body = self.row_of(body, span)?;
        out.push(math::Node::Delimited(Arc::new(math::Delimited {
            open: Some('('),
            body,
            close: Some(')'),
            span,
        })));
        Ok(())
    }

    /// The arguments of a script's function called in an equation: each
    /// positional or named argument an inline formula, unless it is a
    /// string or code embedded with `#`, which keep their values. With
    /// `values`, in an argument of a quantity, numbers and bound names are
    /// values too ([`value_in_math`](Self::value_in_math)).
    fn math_func_args(&mut self, args: ast::MathArgs<'_>, values: bool) -> EvalResult<Args> {
        let mut items = Vec::new();
        for item in args.arg_items() {
            if item.ends_in_semicolon {
                bail!(
                    item.arg.span(),
                    "a semicolon separates the rows of `mat(..)`, not the arguments of a function"
                );
            }
            let (name, expr) = match item.arg {
                ast::Arg::Pos(expr) => (None, expr),
                ast::Arg::Named(named) => (Some(named.name().get().clone()), named.expr()),
                ast::Arg::Spread(spread) => {
                    bail!(spread.span(), "arguments cannot be spread in an equation")
                }
            };
            let span = expr.span();
            let value = match values {
                true => match self.value_in_math(expr)? {
                    Some(value) => value,
                    None => self.math_arg_value(expr)?,
                },
                false => self.math_arg_value(expr)?,
            };
            items.push(Arg {
                span,
                name,
                scope: None,
                value,
            });
        }
        Ok(Args::new(args.span(), items.into_iter().collect()))
    }

    /// The value of an argument in an equation.
    fn math_arg_value(&mut self, expr: ast::Expr<'_>) -> EvalResult<Value> {
        if let ast::Expr::Str(text) = expr {
            return Ok(Value::Str(text.get()));
        }
        if !is_math(expr) {
            return self.expr(expr);
        }
        let span = expr.span();
        let row = self.math_row(expr, Level::Nested)?;
        let content = self.math_content(Formula::row(row, span), false, span)?;
        Ok(Value::Content(content))
    }

    /// Builds the node a function of math makes.
    fn math_func(
        &mut self,
        func: MathFunc,
        name: &str,
        args: ast::MathArgs<'_>,
        span: Span,
    ) -> EvalResult<math::Node> {
        let mut pos: Vec<(ast::Expr<'_>, bool)> = Vec::new();
        let mut delim: Option<(ast::Expr<'_>, Span)> = None;
        let mut pm: Option<ast::Expr<'_>> = None;
        let mut digits: Option<ast::Expr<'_>> = None;
        let numeric = matches!(func, MathFunc::Quantity | MathFunc::Num);
        for item in args.arg_items() {
            match item.arg {
                ast::Arg::Pos(expr) => pos.push((expr, item.ends_in_semicolon)),
                ast::Arg::Named(named)
                    if func == MathFunc::Matrix && named.name().get() == "delim" =>
                {
                    delim = Some((named.expr(), named.span()));
                }
                ast::Arg::Named(named) if numeric && named.name().get() == "pm" => {
                    pm = Some(named.expr());
                }
                ast::Arg::Named(named) if numeric && named.name().get() == "digits" => {
                    digits = Some(named.expr());
                }
                ast::Arg::Named(named) => bail!(
                    named.span(),
                    "`{name}` has no argument `{}`",
                    named.name().get()
                ),
                ast::Arg::Spread(spread) => {
                    bail!(spread.span(), "arguments cannot be spread in an equation")
                }
            }
        }
        if func != MathFunc::Matrix
            && let Some(&(expr, _)) = pos.iter().find(|(_, semicolon)| *semicolon)
        {
            bail!(
                expr.span(),
                "a semicolon separates the rows of `mat(..)`, not the arguments of `{name}`"
            );
        }
        let exprs: Vec<ast::Expr<'_>> = pos.iter().map(|&(expr, _)| expr).collect();
        Ok(match func {
            MathFunc::Quantity | MathFunc::Unit | MathFunc::Num => {
                let (value, unit) = match (func, exprs.as_slice()) {
                    (MathFunc::Quantity, &[value, unit]) => (Some(value), Some(unit)),
                    (MathFunc::Unit, &[unit]) => (None, Some(unit)),
                    (MathFunc::Num, &[value]) => (Some(value), None),
                    (MathFunc::Quantity, _) => {
                        return Err(arity(name, span, "a value and a unit").into());
                    }
                    (MathFunc::Unit, _) => return Err(arity(name, span, "a unit").into()),
                    _ => return Err(arity(name, span, "a value").into()),
                };
                let value = match value {
                    Some(expr) => Some((self.math_number(expr)?, expr.span())),
                    None => None,
                };
                let pm = match pm {
                    Some(expr) => Some((self.math_number(expr)?, expr.span())),
                    None => None,
                };
                let unit = match unit {
                    Some(expr) => match self.value_in_math(expr)? {
                        Some(value) => Some(Spanned::of(value, expr)),
                        None => {
                            return Err(EvalError::new(expr.span(), "expected a unit")
                                .with_hint("a unit is written in quotes: `\"m s^-2\"`")
                                .into());
                        }
                    },
                    None => None,
                };
                let digits = match digits {
                    Some(expr) => match self.value_in_math(expr)? {
                        Some(value) => Some((value, expr.span())),
                        None => bail!(expr.span(), "expected an integer"),
                    },
                    None => None,
                };
                math::Node::Quantity(Arc::new(quantity(value, pm, unit, digits, span)?))
            }
            MathFunc::Frac => {
                let [num, den] =
                    self.math_args::<2>(name, &exprs, span, "a numerator and a denominator")?;
                math::Node::Frac(Arc::new(math::Frac { num, den, span }))
            }
            MathFunc::Sqrt => {
                let [radicand] = self.math_args::<1>(name, &exprs, span, "a radicand")?;
                math::Node::Root(Arc::new(math::Root {
                    index: None,
                    radicand,
                    span,
                }))
            }
            MathFunc::Root => {
                let [index, radicand] =
                    self.math_args::<2>(name, &exprs, span, "an index and a radicand")?;
                math::Node::Root(Arc::new(math::Root {
                    index: Some(index),
                    radicand,
                    span,
                }))
            }
            MathFunc::Vector | MathFunc::UnitVector => {
                if exprs.len() != 1 {
                    return Err(arity(name, span, "the vector's symbol")
                        .with_hint("a column vector is a matrix: `mat(1; 2; 3)`")
                        .into());
                }
                let [base] = self.math_args::<1>(name, &exprs, span, "the vector's symbol")?;
                math::Node::Vector(Arc::new(math::Vector {
                    base,
                    unit: func == MathFunc::UnitVector,
                    span,
                }))
            }
            MathFunc::Fence(open, close) => {
                let [body] = self.math_args::<1>(name, &exprs, span, "a body")?;
                math::Node::Delimited(Arc::new(math::Delimited {
                    open: Some(open),
                    body,
                    close: Some(close),
                    span,
                }))
            }
            MathFunc::NamedAccent(accent) => {
                let [base] = self.math_args::<1>(name, &exprs, span, "a base")?;
                math::Node::Accent(Arc::new(math::Accent { base, accent, span }))
            }
            MathFunc::Accent => {
                let [base, accent] =
                    self.math_args::<2>(name, &exprs, span, "a base and an accent")?;
                let accent = combining(&accent).ok_or_else(|| {
                    EvalError::new(exprs[1].span(), "this is not an accent")
                        .with_hint("an accent is one character, as in `accent(x, \"~\")` or `accent(x, arrow.r)`")
                })?;
                math::Node::Accent(Arc::new(math::Accent { base, accent, span }))
            }
            MathFunc::Decoration(kind) => {
                let braces = !matches!(
                    kind,
                    math::DecorationKind::Overline | math::DecorationKind::Underline
                );
                let (body, annotation) = match exprs.as_slice() {
                    [body] => (self.math_row(*body, Level::Nested)?, None),
                    [body, annotation] if braces => (
                        self.math_row(*body, Level::Nested)?,
                        Some(self.math_row(*annotation, Level::Nested)?),
                    ),
                    _ if braces => {
                        return Err(arity(name, span, "a body and an optional annotation").into());
                    }
                    _ => return Err(arity(name, span, "a body").into()),
                };
                math::Node::Decoration(Arc::new(math::Decoration {
                    kind,
                    body,
                    annotation,
                    span,
                }))
            }
            MathFunc::Styled(variant) => {
                let [body] = self.math_args::<1>(name, &exprs, span, "a body")?;
                math::Node::Styled(Arc::new(math::Styled {
                    variant,
                    body,
                    span,
                }))
            }
            MathFunc::Cases => {
                if exprs.is_empty() {
                    return Err(arity(name, span, "at least one case").into());
                }
                let mut cases = Vec::with_capacity(exprs.len());
                for &expr in &exprs {
                    cases.push(self.math_row(expr, Level::Case)?);
                }
                math::Node::Cases(Arc::new(math::Cases { cases, span }))
            }
            MathFunc::Matrix => {
                let (open, close) = match delim {
                    None => (Some('('), Some(')')),
                    Some((expr, span)) => self.matrix_delims(expr, span)?,
                };
                let mut rows: Vec<Vec<Row>> = vec![Vec::new()];
                let mut row_spans = vec![span];
                for (i, &(expr, semicolon)) in pos.iter().enumerate() {
                    let cell = self.math_row(expr, Level::Nested)?;
                    let last = rows.last_mut().expect("there is always a row");
                    if last.is_empty() {
                        *row_spans.last_mut().expect("there is always a row") = expr.span();
                    }
                    last.push(cell);
                    if semicolon && i + 1 < pos.len() {
                        rows.push(Vec::new());
                        row_spans.push(span);
                    }
                }
                if rows.iter().all(Vec::is_empty) {
                    return Err(arity(name, span, "at least one cell").into());
                }
                let width = rows[0].len();
                if let Some(i) = rows.iter().position(|row| row.len() != width) {
                    return Err(EvalError::new(
                        row_spans[i],
                        eco_format!(
                            "this row has {}, but the first has {}",
                            cells(rows[i].len()),
                            cells(width)
                        ),
                    )
                    .with_hint("every row of a matrix has the same number of cells")
                    .into());
                }
                math::Node::Matrix(Arc::new(math::Matrix {
                    rows,
                    open,
                    close,
                    span,
                }))
            }
        })
    }

    /// The number an argument of `qty` or `num` gives: a decimal literal as
    /// written in the equation (`9.81`, `-0.5`, `6.02e23`), which keeps its
    /// digits, or a value as [`value_in_math`](Self::value_in_math) gives
    /// it.
    fn math_number(&mut self, expr: ast::Expr<'_>) -> EvalResult<Number> {
        let span = expr.span();
        if let Some(text) = literal_text(expr) {
            match Number::parse(&text) {
                Ok(number) => return Ok(number),
                // A literal that is out of range says so: one with too
                // many digits or too large a power of ten.
                Err(message)
                    if text.starts_with(|c: char| c.is_ascii_digit() || "-−+.".contains(c))
                        && text
                            .chars()
                            .all(|c| c.is_ascii_digit() || "-−+.eE".contains(c))
                        && text.contains(|c: char| c.is_ascii_digit()) =>
                {
                    return Err(EvalError::new(span, message).into());
                }
                Err(_) => {}
            }
        }
        match self.value_in_math(expr)? {
            Some(value) => number_of(&value, span),
            None => Err(EvalError::new(span, "expected a number")
                .with_hint("write a number such as `9.81` or `6.02e23`, or a value with `#`")
                .into()),
        }
    }

    /// The value of an argument of `qty`, `unit` or `num` in an equation,
    /// as a script sees it: a number as an integer or a float, a name the
    /// script binds as its value, a call of a script's function, whose
    /// arguments are such values too (`range-of(speed, angle)`), a string,
    /// or code embedded with `#`. `None` for other math, which is no value.
    fn value_in_math(&mut self, expr: ast::Expr<'_>) -> EvalResult<Option<Value>> {
        if !is_math(expr) {
            return self.math_arg_value(expr).map(Some);
        }
        if let Some(text) = literal_text(expr)
            && let Ok(number) = Number::parse(&text)
        {
            return Ok(Some(number_value(&number)));
        }
        match expr {
            ast::Expr::MathIdent(ident) => self.math_binding(ident.get()),
            ast::Expr::MathCall(call) => {
                let ast::MathAccess::MathIdent(ident) = call.callee() else {
                    return Ok(None);
                };
                let Some(Value::Func(func)) = self.math_binding(ident.get())? else {
                    return Ok(None);
                };
                let args = self.math_func_args(call.args(), true)?;
                self.call(func, args, call.span()).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Exactly `N` positional arguments, each as a row.
    fn math_args<const N: usize>(
        &mut self,
        name: &str,
        exprs: &[ast::Expr<'_>],
        span: Span,
        what: &str,
    ) -> EvalResult<[Row; N]> {
        if exprs.len() != N {
            return Err(arity(name, span, what).into());
        }
        let mut rows = Vec::with_capacity(N);
        for &expr in exprs {
            rows.push(self.math_row(expr, Level::Nested)?);
        }
        Ok(rows.try_into().expect("exactly N rows"))
    }

    /// The delimiters `mat(.., delim: "[")` asks for: a string of one
    /// opening delimiter, whose closing one follows, or `none`.
    fn matrix_delims(
        &mut self,
        expr: ast::Expr<'_>,
        span: Span,
    ) -> EvalResult<(Option<char>, Option<char>)> {
        let value = self.math_arg_value(expr)?;
        let pair = match &value {
            Value::None => Some((None, None)),
            Value::Str(text) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(open), None) => closing(open).map(|close| (Some(open), Some(close))),
                    _ => None,
                }
            }
            _ => None,
        };
        pair.ok_or_else(|| {
            EvalError::new(
                span,
                eco_format!(
                    "expected an opening delimiter or `none`, found {}",
                    match &value {
                        Value::Str(text) => eco_format!("{text:?}"),
                        other => a_type(other.type_name()).into(),
                    }
                ),
            )
            .with_hint("one of \"(\", \"[\", \"{\", \"|\", \"‖\" or \"⟨\", or `#none`")
            .into()
        })
    }

    /// Places a value in a formula: a formula as its row, content that is
    /// a formula or text, a number, or a string as text. What it places at
    /// `level` may hold alignment points only where `level` allows them.
    fn place_in_math(
        &mut self,
        value: Value,
        level: Level,
        span: Span,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        match value {
            Value::None => {}
            Value::Int(n) => number(&n.to_string(), span, out),
            Value::Float(f) => float(f.get(), span, out)?,
            Value::Str(text) => out.push(math::Node::text(text, span)),
            Value::Math(formula) => self.splice(&formula, level, span, out)?,
            Value::Content(content) => {
                // Runs of text and spaces become one text each.
                let mut run: Option<(EcoString, Span)> = None;
                for node in content.nodes() {
                    match node {
                        Node::Text { text, span: own } => {
                            // Content shares its texts; the run copies them.
                            self.alloc(text.len() as u64, span)?;
                            let own = if own.is_detached() { span } else { *own };
                            run.get_or_insert_with(|| (EcoString::new(), own))
                                .0
                                .push_str(text);
                            continue;
                        }
                        Node::Space(_) => {
                            if let Some((text, _)) = &mut run {
                                text.push(' ');
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some((text, at)) = run.take() {
                        out.push(math::Node::text(text.trim_end().into(), at));
                    }
                    match node {
                        Node::Element(element)
                            if element.name
                                == QualifiedName::unqualified(
                                    Name::new("math").expect("a valid name"),
                                ) =>
                        {
                            // Markup makes every `math` element with a
                            // tree; scripts cannot call it.
                            if let Some(Value::Math(formula)) = element.field("tree") {
                                self.splice(formula, level, span, out)?
                            }
                        }
                        _ => {
                            return Err(EvalError::new(
                                span,
                                "this content cannot be placed in an equation",
                            )
                            .with_hint("an equation can hold formulas, numbers and text in quotes")
                            .into());
                        }
                    }
                }
                if let Some((text, at)) = run {
                    let text = text.trim_end();
                    if !text.is_empty() {
                        out.push(math::Node::text(text.into(), at));
                    }
                }
            }
            other => bail!(
                span,
                "{} cannot be placed in an equation",
                a_type(other.type_name())
            ),
        }
        Ok(())
    }

    /// Places a formula's row in another formula, at `level`.
    fn splice(
        &mut self,
        formula: &Formula,
        level: Level,
        span: Span,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        if formula.lines().is_empty() {
            return Ok(());
        }
        let Some(row) = formula.as_row() else {
            return Err(EvalError::new(
                span,
                "a formula of several lines or with a label cannot be placed in another",
            )
            .into());
        };
        if level == Level::Nested
            && row
                .nodes()
                .iter()
                .any(|node| matches!(node, math::Node::AlignPoint(_)))
        {
            return Err(misplaced_align_point(span)
                .with_hint("this formula holds an alignment point `&`")
                .into());
        }
        // The row is shared, not copied, but the stages after evaluation
        // walk every node of it, each time it is placed.
        if row.size() > MAX_FORMULA_NODES {
            return Err(too_large(span).into());
        }
        self.alloc(row.size().saturating_mul(NODE), span)?;
        out.extend(row.nodes().iter().cloned());
        Ok(())
    }
}

/// The number of nodes in a formula's lines ([`Row::size`]).
fn formula_size(formula: &Formula) -> u64 {
    formula
        .lines()
        .iter()
        .fold(0u64, |size, line| size.saturating_add(line.row.size()))
}

/// A formula of more than [`MAX_FORMULA_NODES`] nodes.
fn too_large(span: Span) -> EvalError {
    EvalError::new(
        span,
        eco_format!("a formula holds more than {MAX_FORMULA_NODES} nodes"),
    )
    .with_hint("a formula placed in another counts as often as it is placed")
}

/// The part of a LaTeX formula an error is about: its range, or, for an
/// empty range at the end of the formula, the character before it.
fn tex_sub_range(text: &str, range: std::ops::Range<usize>) -> Option<SubRange> {
    if range.start < range.end {
        return SubRange::new(range.start, range.end);
    }
    let before = text[..range.start].chars().next_back()?;
    SubRange::new(range.start - before.len_utf8(), range.start)
}

/// A value with the span of the expression it came from, and the text of
/// that expression when it is a string literal written without escapes,
/// so that an error can point into it.
pub(super) struct Spanned {
    value: Value,
    span: Span,
    literal: bool,
}

impl Spanned {
    /// The value of an expression.
    fn of(value: Value, expr: ast::Expr<'_>) -> Self {
        let literal = match (&value, expr) {
            (Value::Str(text), ast::Expr::Str(_)) => {
                expr.to_untyped().leaf_text().len() == text.len() + 2
            }
            _ => false,
        };
        Self {
            value,
            span: expr.span(),
            literal,
        }
    }

    /// A value whose errors point at all of `span`.
    pub(super) fn new(value: Value, span: Span) -> Self {
        Self {
            value,
            span,
            literal: false,
        }
    }
}

/// The text of a math expression that may be a decimal literal, without
/// its spaces: `-9.81`, `6.02e23` (which math reads as a number and a
/// name). `None` for an expression that is not math.
fn literal_text(expr: ast::Expr<'_>) -> Option<String> {
    if !is_math(expr) {
        return None;
    }
    Some(
        expr.to_untyped()
            .full_text()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect(),
    )
}

/// A number as a script's value: an integer when it is one that fits, a
/// float otherwise.
fn number_value(number: &Number) -> Value {
    let text = number.to_string();
    if number.frac.is_empty()
        && number.exponent.is_none()
        && let Ok(n) = text.parse::<i64>()
    {
        return Value::Int(n);
    }
    Value::Float(folio_component::Scalar::new(
        text.parse::<f64>().unwrap_or(f64::NAN),
    ))
}

/// The number a value gives for `qty` or `num`: an integer, a float, or a
/// string holding a decimal literal, which keeps its digits as written
/// (`"9.80"`).
pub(super) fn number_of(value: &Value, span: Span) -> EvalResult<Number> {
    match value {
        Value::Int(n) => Ok(Number::from_int(*n)),
        Value::Float(f) => Number::from_float(f.get())
            .ok_or_else(|| EvalError::new(span, "a quantity cannot be NaN or infinite").into()),
        Value::Str(text) => Number::parse(text).map_err(|message| {
            EvalError::new(span, message)
                .with_hint("a number in a string is written as in code: `\"9.80\"`, `\"6.02e23\"`")
                .into()
        }),
        other => bail!(
            span,
            "expected a number, found {}",
            a_type(other.type_name())
        ),
    }
}

/// The most significant figures `digits:` can ask for.
const MAX_FIGURES: i64 = 100;

/// Builds what `qty`, `unit` and `num` make, in math and in code: checks
/// the uncertainty, reads the unit, and rounds the value to `digits`
/// significant figures.
pub(super) fn quantity(
    value: Option<(Number, Span)>,
    pm: Option<(Number, Span)>,
    unit: Option<Spanned>,
    digits: Option<(Value, Span)>,
    span: Span,
) -> EvalResult<math::Quantity> {
    if let Some((pm, at)) = &pm
        && pm.negative
    {
        bail!(*at, "an uncertainty cannot be negative");
    }
    let unit = match unit {
        None => None,
        Some(Spanned {
            value: Value::Str(text),
            span: at,
            literal,
        }) => Some(Unit::parse(&text).map_err(|error| {
            let mut eval_error = EvalError::new(at, error.message);
            if literal {
                eval_error = eval_error
                    .with_sub_range(SubRange::new(error.range.start + 1, error.range.end + 1));
            }
            if let Some(hint) = error.hint {
                eval_error = eval_error.with_hint(hint);
            }
            Stop::from(eval_error)
        })?),
        Some(Spanned {
            value, span: at, ..
        }) => {
            return Err(EvalError::new(
                at,
                eco_format!("expected a unit, found {}", a_type(value.type_name())),
            )
            .with_hint("a unit is written in quotes: `\"m s^-2\"`")
            .into());
        }
    };
    let figures = match digits {
        None | Some((Value::None, _)) => None,
        Some((Value::Int(n), _)) if (1..=MAX_FIGURES).contains(&n) => Some(n as u32),
        Some((value, at)) => {
            return Err(EvalError::new(
                at,
                match value {
                    Value::Int(n) => eco_format!("{n} is not a number of significant figures"),
                    other => {
                        eco_format!("expected an integer, found {}", a_type(other.type_name()))
                    }
                },
            )
            .with_hint(eco_format!(
                "`digits` is the number of significant figures, from 1 to {MAX_FIGURES}"
            ))
            .into());
        }
    };
    let value = value.map(|(number, _)| match figures {
        Some(figures) => number.round(figures),
        None => number,
    });
    Ok(math::Quantity {
        value,
        pm: pm.map(|(number, _)| number),
        unit,
        span,
    })
}

/// Whether an expression in an equation is math, rather than a string or
/// code embedded with `#`.
fn is_math(expr: ast::Expr<'_>) -> bool {
    matches!(
        expr,
        ast::Expr::Math(_)
            | ast::Expr::MathText(_)
            | ast::Expr::MathIdent(_)
            | ast::Expr::MathFieldAccess(_)
            | ast::Expr::MathShorthand(_)
            | ast::Expr::MathAlignPoint(_)
            | ast::Expr::MathCall(_)
            | ast::Expr::MathDelimited(_)
            | ast::Expr::MathAttach(_)
            | ast::Expr::MathPrimes(_)
            | ast::Expr::MathFrac(_)
            | ast::Expr::MathRoot(_)
            | ast::Expr::Escape(_)
            | ast::Expr::Linebreak(_)
            | ast::Expr::Space(_)
            | ast::Expr::Label(_)
    )
}

/// A number, with a minus sign before it when it is negative.
fn number(text: &str, span: Span, out: &mut Vec<math::Node>) {
    let digits = match text.strip_prefix('-') {
        Some(digits) => {
            out.push(math::Node::symbol('−', span));
            digits
        }
        None => text,
    };
    out.push(math::Node::Number(Atom {
        text: digits.into(),
        class: MathClass::Normal,
        span,
    }));
}

/// The primes of `f'''`, as the characters that hold them.
fn primes_text(count: usize) -> EcoString {
    match count {
        1 => "′".into(),
        2 => "″".into(),
        3 => "‴".into(),
        4 => "⁗".into(),
        n => "′".repeat(n).into(),
    }
}

/// The character of a delimiter of `(x)`, `[|x|]`.
fn delimiter(expr: ast::Expr<'_>) -> Option<char> {
    match expr {
        ast::Expr::MathShorthand(shorthand) => Some(shorthand.get()),
        other => other.to_untyped().leaf_text().chars().next(),
    }
}

/// The closing delimiter that goes with an opening one.
fn closing(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '|' => '|',
        '‖' => '‖',
        '⟨' => '⟩',
        '⌊' => '⌋',
        '⌈' => '⌉',
        '⟦' => '⟧',
        _ => return None,
    })
}

/// The combining character of an accent given as one character: `~` or
/// `∼` is a combining tilde. A combining mark stays as it is; any other
/// character is no accent.
fn combining(row: &Row) -> Option<char> {
    let [node] = row.nodes() else {
        return None;
    };
    let text = match node {
        math::Node::Symbol(atom) | math::Node::Text(atom) | math::Node::Ident(atom) => &atom.text,
        _ => return None,
    };
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    Some(match c {
        '`' => '\u{300}',
        '´' => '\u{301}',
        '^' | 'ˆ' => '\u{302}',
        '~' | '∼' | '˜' => '\u{303}',
        '¯' | '‾' => '\u{304}',
        '˘' => '\u{306}',
        '.' | '⋅' | '˙' => '\u{307}',
        '¨' => '\u{308}',
        '˚' | '∘' => '\u{30A}',
        'ˇ' => '\u{30C}',
        '→' => '\u{20D7}',
        '←' => '\u{20D6}',
        '↔' => '\u{20E1}',
        c if is_combining(c) => c,
        _ => return None,
    })
}

/// A number of cells: `1 cell`, `2 cells`.
fn cells(n: usize) -> EcoString {
    if n == 1 {
        "1 cell".into()
    } else {
        eco_format!("{n} cells")
    }
}

/// The error of an alignment point where none can stand.
fn misplaced_align_point(span: Span) -> EvalError {
    EvalError::new(
        span,
        "an alignment point can only be at the top level of a line or a case",
    )
}

/// A float as it is set: its digits, or a mantissa times a power of ten
/// for one that Rust writes with an exponent (`6.02e23`), or infinity.
fn float(value: f64, span: Span, out: &mut Vec<math::Node>) -> EvalResult<()> {
    if value.is_nan() {
        bail!(span, "NaN cannot be placed in an equation");
    }
    if value.is_infinite() {
        if value < 0.0 {
            out.push(math::Node::symbol('−', span));
        }
        out.push(math::Node::symbol('∞', span));
        return Ok(());
    }
    let text = format!("{value:?}");
    let Some((mantissa, exponent)) = text.split_once('e') else {
        number(&text, span, out);
        return Ok(());
    };
    number(mantissa, span, out);
    out.push(math::Node::symbol('×', span));
    let mut power = Vec::new();
    number(exponent, span, &mut power);
    out.push(math::Node::Attach(Arc::new(math::Attach {
        base: Row::new([math::Node::Number(Atom {
            text: "10".into(),
            class: MathClass::Normal,
            span,
        })]),
        sub: None,
        sup: Some(Row::new(power)),
        primes: 0,
        span,
    })));
    Ok(())
}

/// Whether a character is a combining mark of the blocks that hold accents.
fn is_combining(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// The error of a function of math called with the wrong arguments.
fn arity(name: &str, span: Span, what: &str) -> EvalError {
    EvalError::new(span, eco_format!("`{name}` takes {what}"))
}

/// The error of a name that is neither bound nor a name of math.
fn unknown_name(name: &str, span: Span) -> EvalError {
    let mut error = EvalError::new(span, eco_format!("unknown name `{name}` in an equation"));
    let letters: Vec<String> = name.chars().map(String::from).collect();
    if name.chars().all(char::is_alphabetic) && letters.len() <= 4 {
        error = error.with_hint(eco_format!(
            "a product of variables is written with spaces: `{}`",
            letters.join(" ")
        ));
    }
    error.with_hint(eco_format!(
        "text in an equation is written in quotes: `\"{name}\"`"
    ))
}

/// The error of a function of math named without its arguments.
fn not_called(name: &str, span: Span) -> EvalError {
    EvalError::new(span, eco_format!("`{name}` is a function")).with_hint(eco_format!(
        "call it with its arguments in parentheses: `{name}(..)`"
    ))
}

/// The error of a dotted name that is not a symbol.
fn unknown_symbol(name: &str, span: Span) -> EvalError {
    let base = name.split('.').next().unwrap_or(name);
    let error = EvalError::new(span, eco_format!("unknown symbol `{name}`"));
    if folio_math::is_symbol_base(base) {
        error.with_hint(eco_format!("`{base}` has no variant with these modifiers"))
    } else {
        error
    }
}

/// The error of a label that does not end a line.
fn label_not_at_end(span: Span) -> EvalError {
    EvalError::new(span, "a label in an equation ends a line")
        .with_hint("write it before the line break `\\` or the closing `$`")
}
