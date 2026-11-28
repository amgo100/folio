//! Stylesheet values (SPEC §5.1, Appendix A): evaluating the value of a
//! declaration or token with the shared expression language, and computing
//! it for an element.
//!
//! A value is evaluated once per build, into a [`Declared`] value: numbers,
//! lengths, colours and strings as the evaluator makes them, with the
//! operators and standard library of scripts (`$body-size * 1.2`,
//! `rgb("1f5fa8")`, `calc.max(..)`). Two things differ from scripts, because
//! stylesheets have no variables: a bare word is a keyword (`outer`,
//! `solid`), and a call of a name that is not a function of the standard
//! library is a keyword with arguments (`wipe(from-left)`). Tokens (`$accent`)
//! are looked up in the build's token table.
//!
//! Lengths relative to the font size stay relative until the value is
//! computed for an element ([`compute`]), so `1.2em` follows the element's
//! own font size.

use std::cell::RefCell;
use std::collections::HashMap;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Abs, Name, PropertyType, Ratio, Scalar, Value};
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{Span, SyntaxNode};

use super::rules::Origin;
use crate::db::{Db, Project};
use crate::diag;
use crate::eval::{self, Arg, Args, EvalError, Func, ops};
use crate::project::ProjectPath;

/// A value as a stylesheet declares it, before it is computed for an
/// element.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Declared {
    /// A value of the expression language.
    Value(eval::Value),
    /// A bare word: `outer`, `solid`.
    Keyword(Name),
    /// A keyword with arguments: `wipe(from-left)`.
    Call(Name, EcoVec<Declared>),
    /// Several items, separated by spaces or commas:
    /// `3mm 4mm`, `"Source Serif 4", "Noto Serif"`.
    List(EcoVec<Declared>),
}

/// Computes a declared value for an element with a font size: lengths
/// become absolute, and the result is plain data.
pub fn compute(value: &Declared, font_size: Abs) -> Result<Value, EcoString> {
    Ok(match value {
        Declared::Value(value) => plain(value, font_size)?,
        Declared::Keyword(word) => Value::Keyword(word.clone()),
        Declared::Call(name, args) => Value::Call(
            name.clone(),
            args.iter()
                .map(|a| compute(a, font_size))
                .collect::<Result<_, _>>()?,
        ),
        Declared::List(items) => Value::Array(
            items
                .iter()
                .map(|a| compute(a, font_size))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// A value of the evaluator as plain data.
fn plain(value: &eval::Value, font_size: Abs) -> Result<Value, EcoString> {
    use eval::Value as V;
    Ok(match value {
        V::None => Value::None,
        V::Auto => Value::Auto,
        V::Bool(v) => Value::Bool(*v),
        V::Int(v) => Value::Int(*v),
        V::Float(v) => Value::Number(Scalar::new(finite(v.get(), "number")?)),
        // Added as floats, so that a sum that is not a number (`inf - inf`)
        // is not first made zero, as a length makes it.
        V::Length(length) => Value::Length(Abs::pt(bounded(finite(
            length.abs.to_pt() + length.em.get() * font_size.to_pt(),
            "length",
        )?)?)),
        V::Angle(angle) => Value::Angle(Scalar::new(finite(angle.0.get(), "angle")?)),
        V::Ratio(ratio) => Value::Ratio(Ratio::new(finite(ratio.get(), "ratio")?)),
        V::Fraction(fr) => Value::Fraction(Scalar::new(finite(fr.0.get(), "fraction")?)),
        V::Duration(d) => Value::Duration(Scalar::new(finite(d.0.get(), "duration")?)),
        V::Color(color) => Value::Color(*color),
        V::Str(text) => Value::Str(text.clone()),
        V::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| plain(v, font_size))
                .collect::<Result<_, _>>()?,
        ),
        V::Dict(dict) => {
            let mut pairs = EcoVec::new();
            for (key, v) in dict.iter() {
                let Some(name) = Name::new(key) else {
                    return Err(eco_format!(
                        "the key {key:?} is not a name, so it cannot be part of a property value"
                    ));
                };
                pairs.push((name, plain(v, font_size)?));
            }
            Value::Dict(pairs)
        }
        // A keyword call from content, as a stylesheet writes it:
        // `minmax(2cm, 1fr)` in a `column-tracks` hint (SPEC §7.4).
        V::Call(name, args) => Value::Call(
            name.clone(),
            args.iter()
                .map(|v| plain(v, font_size))
                .collect::<Result<_, _>>()?,
        ),
        // Resolved only as a whole value or an item of a list (SPEC §5.4).
        V::Token(name) => {
            return Err(eco_format!(
                "`tok.{name}` can be a whole value or an item of a list only, not nested deeper"
            ));
        }
        other => {
            return Err(eco_format!(
                "{} cannot be a property value",
                ops::a_type(other.type_name())
            ));
        }
    })
}

/// A number of a property value, if it is finite: an infinite length or
/// number, such as `1e308pt * 10`, would reach layout and the PDF writer
/// (SPEC §14.2). A declaration whose value is not finite is an error where
/// the stylesheet declares it. Lengths are also [`bounded`], which is what
/// leaves out a value that is too long only at an element's font size.
fn finite(value: f64, what: &str) -> Result<f64, EcoString> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(eco_format!("this {what} is not finite"))
    }
}

/// The longest length a property value may have, in points: 1,000,000pt,
/// about 350 m (SPEC §5.1). It is far beyond any page, so no realistic
/// document reaches it, and wider than PDF's page limit, so it does not
/// tie the other targets to PDF's; within it, nothing layout computes from lengths, products of two
/// included, can overflow.
const MAX_LENGTH: f64 = 1_000_000.0;

/// A length in points, if it is within [`MAX_LENGTH`]. Like a value that is
/// not finite, a longer length is an error where the stylesheet declares
/// it, and is left out where it is longer only at an element's font size.
fn bounded(pt: f64) -> Result<f64, EcoString> {
    if pt.abs() <= MAX_LENGTH {
        Ok(pt)
    } else {
        Err(eco_format!(
            "this length is beyond ±{MAX_LENGTH}pt (about 350 m), the limit of a length"
        ))
    }
}

/// Coerces a computed value to a property's type where it is written in
/// another form: a zero without a unit is a length (`indent: 0`), and a
/// single item of a property that takes a list is a list of one
/// (`font: "Source Serif 4"`). With `content`, a string is also a keyword:
/// content writes hints such as `placement: wrap` as strings (SPEC §4.2),
/// while stylesheets write keywords bare.
pub fn coerce(value: Value, ty: &PropertyType, content: bool) -> Value {
    if ty.accepts(&value) {
        return value;
    }
    let mut candidates = Vec::new();
    match &value {
        Value::Int(0) => candidates.push(Value::Length(Abs::zero())),
        Value::Number(n) if n.get() == 0.0 => candidates.push(Value::Length(Abs::zero())),
        Value::Str(text) if content => {
            if let Some(word) = Name::new(text) {
                candidates.push(Value::Keyword(word));
            }
        }
        _ => {}
    }
    if !matches!(value, Value::Array(_)) {
        let single = candidates.first().cloned().unwrap_or_else(|| value.clone());
        candidates.push(Value::Array(EcoVec::from([value.clone()])));
        candidates.push(Value::Array(EcoVec::from([single])));
    }
    candidates
        .into_iter()
        .find(|c| ty.accepts(c))
        .unwrap_or(value)
}

/// Checks a declared value against a property's type, computed at a
/// nominal font size: only the kind of a length matters. `content` says
/// whether the value comes from content (see [`coerce`]).
pub(super) fn check(
    value: &Declared,
    ty: &PropertyType,
    content: bool,
) -> Result<Value, EcoString> {
    let computed = coerce(compute(value, Abs::pt(10.0))?, ty, content);
    if ty.accepts(&computed) {
        Ok(computed)
    } else {
        Err(eco_format!("expected {ty}, found {}", describe(&computed)))
    }
}

/// A short description of a value for errors: `keyword decimal`, `length`.
fn describe(value: &Value) -> EcoString {
    match value {
        Value::Keyword(word) => eco_format!("keyword `{word}`"),
        Value::Call(name, _) => eco_format!("`{name}(..)`"),
        Value::Array(items) => {
            let items: Vec<_> = items.iter().map(describe).collect();
            eco_format!("a list ({})", items.join(", "))
        }
        other => other.type_name().into(),
    }
}

/// Why a value could not be evaluated.
#[derive(Debug)]
pub(super) enum Fail {
    /// An error to report.
    Error(EvalError),
    /// An error that was reported already, in a token's definition.
    Reported,
}

impl From<EvalError> for Fail {
    fn from(error: EvalError) -> Self {
        Self::Error(error)
    }
}

type VResult<T> = Result<T, Fail>;

fn bail<T>(span: Span, message: impl Into<EcoString>) -> VResult<T> {
    Err(Fail::Error(EvalError::new(span, message)))
}

/// A token definition: `$accent: #1f5fa8;`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct TokenDef {
    /// The `TokenDecl` node.
    pub(super) node: SyntaxNode,
    /// The stylesheet it is in.
    pub(super) file: ProjectPath,
    /// The origin of the stylesheet or block it is in: a definition from a
    /// higher origin wins over one from a lower origin (SPEC §5.1).
    pub(super) origin: Origin,
}

/// The tokens of a build, evaluated when first used.
pub(super) struct Tokens {
    defs: HashMap<EcoString, TokenDef>,
    values: RefCell<HashMap<EcoString, Option<Declared>>>,
    /// The tokens being evaluated, to find cycles.
    active: RefCell<Vec<EcoString>>,
}

impl Tokens {
    /// The tokens defined by `defs`, in source order. Of the definitions
    /// of a name, the one from the highest origin wins, and among those
    /// the last (SPEC §5.1).
    pub(super) fn new(defs: impl IntoIterator<Item = (EcoString, TokenDef)>) -> Self {
        let mut defs: Vec<_> = defs.into_iter().collect();
        // A stable sort keeps source order within an origin, so a later
        // definition replaces an earlier one as the map is built.
        defs.sort_by_key(|(_, def)| def.origin);
        Self {
            defs: defs.into_iter().collect(),
            values: RefCell::default(),
            active: RefCell::default(),
        }
    }

    /// The value of a token, evaluated and reported once.
    fn get(&self, cx: &Values<'_>, name: &str, span: Span) -> VResult<Declared> {
        self.get_as(cx, name, span, "$")
    }

    /// [`Self::get`], with an unknown token named as `prefix` writes it:
    /// `$` in a stylesheet, `tok.` in content (SPEC §5.4).
    fn get_as(&self, cx: &Values<'_>, name: &str, span: Span, prefix: &str) -> VResult<Declared> {
        if let Some(value) = self.values.borrow().get(name) {
            return value.clone().ok_or(Fail::Reported);
        }
        let Some(def) = self.defs.get(name) else {
            let mut error = EvalError::new(span, eco_format!("unknown token `{prefix}{name}`"));
            if let Some(close) = closest(name, self.defs.keys()) {
                error = error.with_hint(eco_format!("did you mean `{prefix}{close}`?"));
            }
            return Err(error.into());
        };
        if self.active.borrow().iter().any(|n| n == name) {
            return bail(
                span,
                eco_format!("token `${name}` is defined in terms of itself"),
            );
        }
        self.active.borrow_mut().push(name.into());
        let decl = def
            .node
            .cast::<ast::TokenDecl<'_>>()
            .expect("a token definition");
        let inner = Values {
            file: def.file.clone(),
            ..*cx
        };
        let result = inner.items([decl.values().collect()].into(), decl.to_untyped().span());
        self.active.borrow_mut().pop();
        let value = match result {
            Ok(value) => Some(value),
            Err(Fail::Error(error)) => {
                diag::report(cx.db, error.into());
                None
            }
            Err(Fail::Reported) => None,
        };
        self.values.borrow_mut().insert(name.into(), value.clone());
        value.ok_or(Fail::Reported)
    }
}

/// The name among `names` closest to `name`, if one is close.
fn closest<'a>(name: &str, names: impl Iterator<Item = &'a EcoString>) -> Option<&'a EcoString> {
    names
        .map(|n| (distance(name, n), n))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, n)| n)
}

/// The edit distance between two names.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let next = (prev + cost).min(row[j] + 1).min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Evaluates the values of one stylesheet.
#[derive(Copy, Clone)]
pub(super) struct ValuesCx<'a> {
    pub(super) db: &'a dyn Db,
    pub(super) project: Project,
    pub(super) tokens: &'a Tokens,
}

/// [`ValuesCx`] in a file.
#[derive(Clone)]
pub(super) struct Values<'a> {
    pub(super) db: &'a dyn Db,
    pub(super) project: Project,
    pub(super) tokens: &'a Tokens,
    /// The stylesheet, which function calls see as theirs.
    pub(super) file: ProjectPath,
}

impl<'a> Values<'a> {
    pub(super) fn new(cx: ValuesCx<'a>, file: ProjectPath) -> Self {
        Self {
            db: cx.db,
            project: cx.project,
            tokens: cx.tokens,
            file,
        }
    }

    /// Evaluates a declaration's value, reporting errors: items separated
    /// by spaces within groups separated by commas. `None` after an error.
    pub(super) fn declaration(&self, decl: ast::Declaration<'_>) -> Option<Declared> {
        let groups = decl.groups();
        self.report(self.items(groups, decl.to_untyped().span()))
    }

    /// Evaluates one expression, as an attribute test's value, reporting
    /// errors.
    pub(super) fn single(&self, expr: ast::Expr<'_>) -> Option<Declared> {
        self.report(self.expr(expr))
    }

    /// The value of the token that content names `tok.name` (SPEC §5.4),
    /// as `$name` gives it, reporting errors at `span`.
    pub(super) fn token(&self, name: &str, span: Span) -> Option<Declared> {
        self.report(self.tokens.get_as(self, name, span, "tok."))
    }

    fn report(&self, result: VResult<Declared>) -> Option<Declared> {
        match result {
            Ok(value) => Some(value),
            Err(Fail::Error(error)) => {
                diag::report(self.db, error.into());
                None
            }
            Err(Fail::Reported) => None,
        }
    }

    fn items(&self, groups: Vec<Vec<ast::Expr<'_>>>, span: Span) -> VResult<Declared> {
        let mut out = EcoVec::new();
        for group in &groups {
            let mut items = EcoVec::new();
            for &expr in group {
                items.push(self.expr(expr)?);
            }
            out.push(match items.len() {
                0 => return bail(span, "a value is missing"),
                1 => items.into_iter().next().expect("one item"),
                _ => Declared::List(items),
            });
        }
        Ok(match out.len() {
            0 => return bail(span, "a value is missing"),
            1 => out.into_iter().next().expect("one group"),
            _ => Declared::List(out),
        })
    }

    fn expr(&self, expr: ast::Expr<'_>) -> VResult<Declared> {
        let span = expr.span();
        let value = |v| Ok(Declared::Value(v));
        match expr {
            ast::Expr::None(_) => value(eval::Value::None),
            ast::Expr::Auto(_) => value(eval::Value::Auto),
            ast::Expr::Bool(v) => value(eval::Value::Bool(v.get())),
            ast::Expr::Int(v) => value(eval::Value::Int(v.get())),
            ast::Expr::Float(v) => value(eval::Value::Float(Scalar::new(v.get()))),
            ast::Expr::Numeric(v) => {
                let (number, unit) = v.get();
                value(eval::numeric(number, unit))
            }
            ast::Expr::Str(s) => value(eval::Value::Str(s.get())),
            ast::Expr::Color(color) => {
                let [r, g, b, a] = color.rgba();
                value(eval::Value::Color(folio_component::Color::rgba(r, g, b, a)))
            }
            ast::Expr::Ratio(ratio) => {
                let (a, b) = ratio.get();
                let number = |v| Declared::Value(eval::Value::Float(Scalar::new(v)));
                Ok(Declared::List(EcoVec::from([number(a), number(b)])))
            }
            ast::Expr::TokenName(token) => self.tokens.get(self, token.get(), span),
            ast::Expr::Ident(ident) => Ok(Declared::Keyword(keyword(ident.get(), span)?)),
            ast::Expr::Parenthesized(p) => self.expr(p.expr()),
            ast::Expr::Array(array) => {
                let mut items = EcoVec::new();
                for item in array.items() {
                    match item {
                        ast::ArrayItem::Pos(expr) => items.push(self.expr(expr)?),
                        ast::ArrayItem::Spread(spread) => {
                            return bail(spread.span(), "a stylesheet value cannot spread");
                        }
                    }
                }
                Ok(collect(items))
            }
            ast::Expr::Dict(dict) => {
                let mut out = eval::Dict::new();
                for item in dict.items() {
                    match item {
                        ast::DictItem::Named(named) => {
                            let v = self.operand(named.expr())?;
                            out.insert(named.name().get().clone(), v);
                        }
                        ast::DictItem::Keyed(keyed) => {
                            let ast::Expr::Str(key) = keyed.key() else {
                                return bail(keyed.key().span(), "a key must be a string");
                            };
                            let v = self.operand(keyed.expr())?;
                            out.insert(key.get(), v);
                        }
                        ast::DictItem::Spread(spread) => {
                            return bail(spread.span(), "a stylesheet value cannot spread");
                        }
                    }
                }
                value(eval::Value::Dict(out))
            }
            ast::Expr::Unary(unary) => {
                let operand = self.operand(unary.expr())?;
                let result = match unary.op() {
                    ast::UnOp::Pos => ops::pos(operand),
                    ast::UnOp::Neg => ops::neg(operand),
                    ast::UnOp::Not => ops::not(operand),
                };
                result
                    .map(Declared::Value)
                    .map_err(|m| EvalError::new(span, m).into())
            }
            ast::Expr::Binary(binary) => self.binary(binary),
            ast::Expr::FieldAccess(access) => {
                // A name before a dot is a module of the standard library:
                // `calc.max`.
                let target = match access.target() {
                    // Content names a token `tok.warn`; a stylesheet, and a
                    // scoped rule, write `$warn` (SPEC §5.4).
                    ast::Expr::Ident(ident) if ident.get() == "tok" => {
                        let name = access.field();
                        return Err(EvalError::new(
                            span,
                            eco_format!("`tok.{}` names a token in content only", name.get()),
                        )
                        .with_hint(eco_format!("write `${}` here", name.get()))
                        .into());
                    }
                    ast::Expr::Ident(ident) => {
                        match eval::globals(self.db, self.project).get(ident.get()) {
                            Some(module @ eval::Value::Module(_)) => module.clone(),
                            _ => {
                                return bail(
                                    ident.span(),
                                    eco_format!("unknown module `{}`", ident.get()),
                                );
                            }
                        }
                    }
                    target => self.operand(target)?,
                };
                let field = access.field();
                match target {
                    eval::Value::Module(module) => match module.scope().get(field.get()) {
                        Some(v) => value(v.clone()),
                        None => bail(
                            field.span(),
                            eco_format!("module `{}` has no `{}`", module.name(), field.get()),
                        ),
                    },
                    other => bail(
                        span,
                        eco_format!(
                            "{} has no field `{}`",
                            ops::a_type(other.type_name()),
                            field.get()
                        ),
                    ),
                }
            }
            ast::Expr::FuncCall(call) => self.call(call),
            ast::Expr::TemplateBlock(_)
            | ast::Expr::Sequence(_)
            | ast::Expr::Guarded(_)
            | ast::Expr::Conditional(_) => Err(EvalError::new(span, "this is a render template")
                .with_hint(
                    "templates are values of template properties, such as `trailer` and `header`",
                )
                .into()),
            _ => Err(EvalError::new(span, "this cannot be a stylesheet value")
                .with_hint(
                    "a value is a number, length, colour, string, keyword, token or function call",
                )
                .into()),
        }
    }

    /// Evaluates an operand of an operator or function: it must be a value
    /// of the expression language, not a keyword.
    fn operand(&self, expr: ast::Expr<'_>) -> VResult<eval::Value> {
        match self.expr(expr)? {
            Declared::Value(v) => Ok(v),
            Declared::Keyword(word) => bail(
                expr.span(),
                eco_format!("the keyword `{word}` cannot be computed with"),
            ),
            Declared::Call(name, _) => bail(
                expr.span(),
                eco_format!("`{name}(..)` is a keyword, so it cannot be computed with"),
            ),
            Declared::List(_) => bail(expr.span(), "a list of values cannot be computed with"),
        }
    }

    fn binary(&self, binary: ast::Binary<'_>) -> VResult<Declared> {
        use ast::BinOp::*;
        let span = binary.span();
        let op = binary.op();
        let lhs = self.operand(binary.lhs())?;
        let rhs = self.operand(binary.rhs())?;
        let result = match op {
            Add => ops::add(lhs, rhs),
            Sub => ops::sub(lhs, rhs),
            Mul => ops::mul(lhs, rhs),
            Div => ops::div(lhs, rhs),
            Eq => Ok(eval::Value::Bool(ops::equal(&lhs, &rhs))),
            Neq => Ok(eval::Value::Bool(!ops::equal(&lhs, &rhs))),
            Lt | Leq | Gt | Geq => ops::compare(&lhs, &rhs).map(|ord| {
                eval::Value::Bool(match op {
                    Lt => ord.is_lt(),
                    Leq => ord.is_le(),
                    Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }),
            _ => return bail(span, "this operator cannot be used in a stylesheet value"),
        };
        result
            .map(Declared::Value)
            .map_err(|m| EvalError::new(span, m).into())
    }

    /// A call: of a function of the standard library, or a keyword with
    /// arguments.
    fn call(&self, call: ast::FuncCall<'_>) -> VResult<Declared> {
        let span = call.span();
        let callee = call.callee();
        let func = match callee {
            ast::Expr::Ident(ident) => {
                match eval::globals(self.db, self.project).get(ident.get()) {
                    Some(eval::Value::Func(
                        func @ (Func::Native(_) | Func::Closure(_) | Func::With(_)),
                    )) => Some(func.clone()),
                    _ => None,
                }
            }
            other => match self.operand(other)? {
                eval::Value::Func(func) => Some(func),
                v => {
                    return bail(
                        other.span(),
                        eco_format!("{} is not a function", ops::a_type(v.type_name())),
                    );
                }
            },
        };

        let Some(func) = func else {
            let ast::Expr::Ident(ident) = callee else {
                unreachable!("other callees are functions");
            };
            let name = keyword(ident.get(), ident.span())?;
            let mut args = EcoVec::new();
            for arg in call.args().items() {
                match arg {
                    ast::Arg::Pos(expr) => args.push(self.expr(expr)?),
                    _ => {
                        return bail(
                            arg.to_untyped().span(),
                            eco_format!("`{name}(..)` takes values by position only"),
                        );
                    }
                }
            }
            return Ok(Declared::Call(name, args));
        };

        let mut items = EcoVec::new();
        for arg in call.args().items() {
            match arg {
                ast::Arg::Pos(expr) => items.push(Arg {
                    span: expr.span(),
                    name: None,
                    scope: None,
                    value: self.operand(expr)?,
                    spelling: eval::float_spelling(expr),
                }),
                ast::Arg::Named(named) => items.push(Arg {
                    span: named.span(),
                    name: Some(named.name().get().clone()),
                    scope: None,
                    value: self.operand(named.expr())?,
                    spelling: eval::float_spelling(named.expr()),
                }),
                ast::Arg::Spread(spread) => {
                    return bail(spread.span(), "a stylesheet value cannot spread");
                }
            }
        }
        let args = Args::new(span, items);
        eval::call_function(self.db, self.project, self.file.clone(), func, args, span)
            .map(Declared::Value)
            .map_err(Fail::Error)
    }
}

/// A word as a keyword.
fn keyword(text: &str, span: Span) -> VResult<Name> {
    match Name::new(text) {
        Some(name) => Ok(name),
        None => bail(span, eco_format!("`{text}` is not a keyword")),
    }
}

/// Array items: one array value if they are all values, a list otherwise.
fn collect(items: EcoVec<Declared>) -> Declared {
    if items.iter().all(|i| matches!(i, Declared::Value(_))) {
        Declared::Value(eval::Value::Array(
            items
                .into_iter()
                .map(|i| match i {
                    Declared::Value(v) => v,
                    _ => unreachable!(),
                })
                .collect(),
        ))
    } else {
        Declared::List(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance() {
        assert_eq!(distance("accent", "acent"), 1);
        assert_eq!(distance("a", "b"), 1);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn coercion_and_checks() {
        let placement = PropertyType::Keyword(EcoVec::from([Name::new("wrap").unwrap()]));
        assert_eq!(
            coerce(Value::Str("wrap".into()), &placement, true),
            Value::Keyword(Name::new("wrap").unwrap())
        );
        let numbering = PropertyType::AnyOf(EcoVec::from([PropertyType::None, PropertyType::Str]));
        let decimal = Declared::Keyword(Name::new("decimal").unwrap());
        assert_eq!(
            check(&decimal, &numbering, false).unwrap_err(),
            "expected none | str, found keyword `decimal`"
        );
        // Stylesheets write keywords bare.
        assert!(
            check(
                &Declared::Value(eval::Value::Str("wrap".into())),
                &placement,
                false
            )
            .is_err()
        );
        let font = PropertyType::List(Box::new(PropertyType::Str));
        assert_eq!(
            coerce(Value::Str("A".into()), &font, false),
            Value::Array(EcoVec::from([Value::Str("A".into())]))
        );
        let em = Declared::Value(eval::Value::Length(eval::Length::em(
            folio_component::Em::new(2.0),
        )));
        assert_eq!(
            compute(&em, Abs::pt(11.0)).unwrap(),
            Value::Length(Abs::pt(22.0))
        );
    }

    #[test]
    fn lengths_are_bounded() {
        let length = |pt: f64, em: f64| {
            let mut length = eval::Length::em(folio_component::Em::new(em));
            length.abs = Abs::pt(pt);
            Declared::Value(eval::Value::Length(length))
        };
        let size = Abs::pt(10.0);
        for pt in [MAX_LENGTH, -MAX_LENGTH, 0.0] {
            assert_eq!(
                compute(&length(pt, 0.0), size).unwrap(),
                Value::Length(Abs::pt(pt))
            );
        }
        let error = "this length is beyond ±1000000pt (about 350 m), the limit of a length";
        for pt in [MAX_LENGTH + 1.0, -MAX_LENGTH - 1.0, 1e300, -1e300] {
            assert_eq!(compute(&length(pt, 0.0), size).unwrap_err(), error);
        }
        // An `em` length is bounded at the font size it is computed at.
        let em = length(0.0, 100_000.0);
        assert!(compute(&em, size).is_ok());
        assert_eq!(compute(&em, Abs::pt(11.0)).unwrap_err(), error);
        // Inside lists too.
        let list = Declared::List(EcoVec::from([length(1.0, 0.0), length(2e6, 0.0)]));
        assert_eq!(compute(&list, size).unwrap_err(), error);
    }
}
