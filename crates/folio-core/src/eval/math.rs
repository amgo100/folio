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
use folio_math::{self as math, Atom, Formula, Line, MathClass, Row};
use folio_syntax::Span;
use folio_syntax::ast::{self, AstNode, MathTextKind};

use super::MAX_NESTING;
use super::content::{Content, Node};
use super::error::{EvalError, EvalResult, bail};
use super::library::{MathFunc, MathItem, math_item};
use super::ops::a_type;
use super::value::{Arg, Args, Value};
use super::vm::{Vm, nesting_exceeded};

/// The bytes a node of the math tree counts against the memory limit.
const NODE: u64 = 16;

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
        Ok(Formula::new(lines))
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

    /// A row of nodes, which may nest at most [`MAX_NESTING`] deep.
    fn row_of(&mut self, nodes: Vec<math::Node>, span: Span) -> EvalResult<Row> {
        self.alloc(nodes.len() as u64 * NODE, span)?;
        let row = Row::new(nodes);
        if row.depth() > MAX_NESTING {
            return Err(nesting_exceeded(span, "math is").into());
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
                MathTextKind::Grapheme(grapheme) => atom(grapheme.clone(), span),
            }),
            ast::Expr::MathIdent(ident) => self.math_name(ident.get(), level, span, out)?,
            ast::Expr::MathFieldAccess(access) => {
                let name = access.to_untyped().full_text();
                match folio_math::symbol(&name) {
                    Some(c) => out.push(symbol(c, span)),
                    None if matches!(math_item(&name, true), Some(MathItem::Func(_))) => {
                        return Err(not_called(&name, span).into());
                    }
                    None => return Err(unknown_symbol(&name, span).into()),
                }
            }
            ast::Expr::MathShorthand(shorthand) => out.push(symbol(shorthand.get(), span)),
            ast::Expr::Escape(escape) => out.push(atom(escape.get().into(), span)),
            ast::Expr::Str(text) => out.push(math::Node::Text(Atom {
                text: text.get(),
                class: MathClass::Normal,
                span,
            })),
            ast::Expr::MathPrimes(primes) => {
                out.push(symbol_text(primes_text(primes.count()), span));
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
                out.push(attached(base, sub, sup, primes, span));
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
            Some(MathItem::Symbol(c)) => out.push(symbol(c, span)),
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
                let args = self.math_func_args(call.args())?;
                let result = self.call(func, args, span)?;
                return self.place_in_math(result, level, span, out);
            }
            self.place_in_math(value, level, callee.span(), out)?;
            return self.call_as_group(call.args(), false, out);
        }
        if PENDING.contains(&name.as_str()) {
            out.push(math::Node::Operator(math::Operator {
                name: name.clone(),
                limits: false,
                span: callee.span(),
            }));
            return self.call_as_group(call.args(), true, out);
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
                self.call_as_group(call.args(), false, out)
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
    /// arguments are errors, except in the calls of [`PENDING`].
    fn call_as_group(
        &mut self,
        args: ast::MathArgs<'_>,
        named: bool,
        out: &mut Vec<math::Node>,
    ) -> EvalResult<()> {
        let mut body = Vec::new();
        for item in args.content_items() {
            match item {
                ast::MathArgItem::Arg(ast::Arg::Pos(expr)) => {
                    self.math_expr(expr, Level::Nested, &mut body)?
                }
                ast::MathArgItem::Arg(ast::Arg::Named(arg)) if named => {
                    body.push(math::Node::Text(Atom {
                        text: arg.name().get().clone(),
                        class: MathClass::Normal,
                        span: arg.name().span(),
                    }));
                    body.push(symbol(':', arg.span()));
                    self.math_expr(arg.expr(), Level::Nested, &mut body)?;
                }
                ast::MathArgItem::Arg(arg) => {
                    bail!(
                        arg.span(),
                        "only a function can take named or spread arguments"
                    )
                }
                ast::MathArgItem::Comma(c, node) | ast::MathArgItem::Semicolon(c, node) => {
                    body.push(symbol(c, node.span()))
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
    /// string or code embedded with `#`, which keep their values.
    fn math_func_args(&mut self, args: ast::MathArgs<'_>) -> EvalResult<Args> {
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
            let value = self.math_arg_value(expr)?;
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
        for item in args.arg_items() {
            match item.arg {
                ast::Arg::Pos(expr) => pos.push((expr, item.ends_in_semicolon)),
                ast::Arg::Named(named)
                    if func == MathFunc::Matrix && named.name().get() == "delim" =>
                {
                    delim = Some((named.expr(), named.span()));
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
            Value::Str(text) => out.push(text_atom(text, span)),
            Value::Math(formula) => self.splice(&formula, level, span, out)?,
            Value::Content(content) => {
                // Runs of text and spaces become one text each.
                let mut run: Option<(EcoString, Span)> = None;
                for node in content.nodes() {
                    match node {
                        Node::Text { text, span: own } => {
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
                        out.push(text_atom(text.trim_end().into(), at));
                    }
                    match node {
                        Node::Element(element)
                            if element.name
                                == QualifiedName::unqualified(
                                    Name::new("math").expect("a valid name"),
                                ) =>
                        {
                            match (element.field("tree"), element.field("tex")) {
                                (Some(Value::Math(formula)), _) => {
                                    self.splice(formula, level, span, out)?
                                }
                                (_, Some(Value::Str(_))) => {
                                    bail!(span, "LaTeX math cannot be placed in native math yet")
                                }
                                // Markup makes every `math` element with
                                // one of the two; scripts cannot call it.
                                _ => {}
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
                        out.push(text_atom(text.into(), at));
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
        self.alloc(row.nodes().len() as u64 * NODE, span)?;
        out.extend(row.nodes().iter().cloned());
        Ok(())
    }
}

/// The quantity functions of SPEC §8.1, which #42 adds. Until then a call
/// of one is set as written, its name upright before its arguments in
/// parentheses, so equations that use them still build.
const PENDING: &[&str] = &["qty", "unit", "num"];

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

/// A base with attachments, or an n-ary operator with its limits when the
/// base is one.
fn attached(
    base: Row,
    sub: Option<Row>,
    sup: Option<Row>,
    primes: usize,
    span: Span,
) -> math::Node {
    if primes == 0
        && let [math::Node::Nary(nary)] = base.nodes()
        && nary.sub.is_none()
        && nary.sup.is_none()
    {
        return math::Node::Nary(Arc::new(math::Nary {
            op: nary.op,
            sub,
            sup,
            span,
        }));
    }
    math::Node::Attach(Arc::new(math::Attach {
        base,
        sub,
        sup,
        primes: u8::try_from(primes).unwrap_or(u8::MAX),
        span,
    }))
}

/// The node of a grapheme written in an equation: a letter, a symbol or a
/// large operator.
fn atom(text: EcoString, span: Span) -> math::Node {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => symbol(c, span),
        (Some(c), Some(_)) if c.is_alphabetic() => math::Node::Ident(Atom {
            text,
            class: MathClass::Alphabetic,
            span,
        }),
        _ => symbol_text(text, span),
    }
}

/// The node of a character: a letter is a variable, a large operator an
/// n-ary operator without limits, anything else a symbol of its class.
fn symbol(c: char, span: Span) -> math::Node {
    let class = math::class(c);
    if c.is_alphabetic() || class == MathClass::Alphabetic {
        return math::Node::Ident(Atom {
            text: c.into(),
            class: MathClass::Alphabetic,
            span,
        });
    }
    if class == MathClass::Large {
        return math::Node::Nary(Arc::new(math::Nary {
            op: c,
            sub: None,
            sup: None,
            span,
        }));
    }
    symbol_text(c.into(), span)
}

/// A symbol of the class of its first character.
fn symbol_text(text: EcoString, span: Span) -> math::Node {
    let class = text.chars().next().map_or(MathClass::Normal, math::class);
    math::Node::Symbol(Atom { text, class, span })
}

/// A number, with a minus sign before it when it is negative.
fn number(text: &str, span: Span, out: &mut Vec<math::Node>) {
    let digits = match text.strip_prefix('-') {
        Some(digits) => {
            out.push(symbol('−', span));
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

/// Upright text.
fn text_atom(text: EcoString, span: Span) -> math::Node {
    math::Node::Text(Atom {
        text,
        class: MathClass::Normal,
        span,
    })
}

/// A float as it is set: its digits, or a mantissa times a power of ten
/// for one that Rust writes with an exponent (`6.02e23`), or infinity.
fn float(value: f64, span: Span, out: &mut Vec<math::Node>) -> EvalResult<()> {
    if value.is_nan() {
        bail!(span, "NaN cannot be placed in an equation");
    }
    if value.is_infinite() {
        if value < 0.0 {
            out.push(symbol('−', span));
        }
        out.push(symbol('∞', span));
        return Ok(());
    }
    let text = format!("{value:?}");
    let Some((mantissa, exponent)) = text.split_once('e') else {
        number(&text, span, out);
        return Ok(());
    };
    number(mantissa, span, out);
    out.push(symbol('×', span));
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
