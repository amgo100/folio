//! The interpreter: evaluates markup, expressions and patterns in a scope.

use std::collections::BTreeMap;
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Abs, BODY, Body, Em, Name, QualifiedName, Ratio, Scalar, UNIVERSAL_FIELDS};
use folio_syntax::Span;
use folio_syntax::ast::{self, AstNode, Unit};

use super::captures::captures;
use super::content::{Content, Node};
use super::element;
use super::error::{At, EvalError, EvalResult, Stop, StrResult, bail};
use super::library;
use super::ops::{self, a_type};
use super::value::{
    Angle, Arg, Args, Array, Closure, Dict, Duration, Fraction, Func, Length, Module, Scope, Value,
};
use super::{Cost, Limits, Mode, eval_file_for, globals, reachable, today};
use crate::component::Reason;
use crate::db::{Db, Project, component_host, source_file};
use crate::project::ProjectPath;

/// How much stack must be left before an expression is evaluated: enough
/// for the frames between two expressions, a call through its query
/// included, in a debug build.
const STACK_RED_ZONE: usize = 256 * 1024;

/// The size of a new stack segment.
const STACK_SEGMENT: usize = 4 * 1024 * 1024;

/// A function of the standard library. It takes its arguments from `args`;
/// the caller reports the ones it leaves.
pub(crate) type NativeFn = fn(&mut Vm<'_>, &mut Args) -> EvalResult<Value>;

/// A `break`, `continue` or `return` on its way to the loop or function it
/// leaves.
enum Flow {
    Break(Span),
    Continue(Span),
    Return(Option<Value>),
}

/// The state of one evaluation: of a file, or of one closure call.
pub(crate) struct Vm<'a> {
    pub(crate) db: &'a dyn Db,
    pub(crate) project: Project,
    /// The file the code being evaluated is in, which paths are relative to.
    pub(crate) file: ProjectPath,
    mode: Mode,
    /// The depth of the call or import being evaluated.
    depth: u32,
    globals: &'a Scope,
    scopes: Scopes,
    limits: Limits,
    /// What this evaluation has cost so far, including the calls and
    /// imports it made.
    pub(crate) cost: Cost,
    flow: Option<Flow>,
}

/// The variables of an evaluation: the captured variables of a closure,
/// which cannot be assigned, and the scopes of the code, innermost last.
struct Scopes {
    captured: Scope,
    layers: Vec<BTreeMap<EcoString, Value>>,
}

impl Scopes {
    fn get(&self, name: &str) -> Option<&Value> {
        self.layers
            .iter()
            .rev()
            .find_map(|layer| layer.get(name))
            .or_else(|| self.captured.get(name))
    }

    fn get_mut(&mut self, name: &str) -> StrResult<&mut Value> {
        if let Some(value) = self.layers.iter_mut().rev().find_map(|l| l.get_mut(name)) {
            return Ok(value);
        }
        if self.captured.get(name).is_some() {
            return Err(eco_format!(
                "variables from outside the function are read-only and cannot be changed"
            ));
        }
        Err(eco_format!("unknown variable: {name}"))
    }

    fn define(&mut self, name: EcoString, value: Value) {
        self.layers
            .last_mut()
            .expect("there is always a scope")
            .insert(name, value);
    }
}

impl<'a> Vm<'a> {
    pub(crate) fn new(
        db: &'a dyn Db,
        project: Project,
        file: ProjectPath,
        mode: Mode,
        depth: u32,
        captured: Scope,
    ) -> Self {
        Self {
            db,
            project,
            file,
            mode,
            depth,
            globals: globals(db, project),
            scopes: Scopes {
                captured,
                layers: vec![BTreeMap::new()],
            },
            limits: *project.limits(db),
            cost: Cost::default(),
            flow: None,
        }
    }

    /// Counts one step against the limit.
    pub(crate) fn step(&mut self, span: Span) -> EvalResult<()> {
        self.cost.steps += 1;
        if self.cost.steps > self.limits.steps {
            return Err(steps_exceeded(span, self.limits).into());
        }
        Ok(())
    }

    /// Counts bytes against the memory limit, before they are allocated.
    pub(crate) fn alloc(&mut self, bytes: u64, span: Span) -> EvalResult<()> {
        self.cost.bytes = self.cost.bytes.saturating_add(bytes);
        if self.cost.bytes > self.limits.memory {
            return Err(memory_exceeded(span, self.limits).into());
        }
        Ok(())
    }

    /// Counts what a call or import cost against the limits.
    fn charge(&mut self, cost: Cost, span: Span) -> EvalResult<()> {
        self.cost.steps = self.cost.steps.saturating_add(cost.steps);
        if self.cost.steps > self.limits.steps {
            return Err(steps_exceeded(span, self.limits).into());
        }
        self.alloc(cost.bytes, span)
    }

    fn define(&mut self, name: EcoString, value: Value) {
        self.scopes.define(name, value);
    }

    fn enter(&mut self) {
        self.scopes.layers.push(BTreeMap::new());
    }

    fn exit(&mut self) {
        self.scopes.layers.pop();
    }

    /// Evaluates a file's markup into a module.
    pub(crate) fn module(&mut self, markup: ast::Markup<'_>, name: &str) -> EvalResult<Module> {
        let content = self.markup(markup)?;
        match self.flow.take() {
            Some(Flow::Break(span)) => bail!(span, "cannot break outside of a loop"),
            Some(Flow::Continue(span)) => bail!(span, "cannot continue outside of a loop"),
            Some(Flow::Return(_)) => {
                bail!(markup.span(), "cannot return outside of a function")
            }
            None => {}
        }
        let scope = self.scopes.layers.pop().unwrap_or_default();
        Ok(Module::new(name, scope.into(), content))
    }

    /// Binds a closure's parameters to the arguments and evaluates its body.
    pub(crate) fn closure_body(&mut self, closure: &Closure, mut args: Args) -> EvalResult<Value> {
        let Some(syntax) = closure.node().cast::<ast::Closure<'_>>() else {
            bail!(args.span, "this function cannot be called");
        };
        if let Some(name) = closure.name() {
            self.define(name.clone(), Value::Func(Func::Closure(closure.clone())));
        }
        let params: Vec<ast::Param<'_>> = syntax.params().children().collect();
        let mut defaults = closure.defaults().iter();
        for (i, param) in params.iter().enumerate() {
            match *param {
                ast::Param::Pos(pattern) => {
                    let Some(arg) = args.eat() else {
                        bail!(args.span, "missing argument: {}", pattern_name(pattern));
                    };
                    self.pattern(pattern, arg.value, true)?;
                }
                ast::Param::Named(named) => {
                    let default = defaults.next().cloned().unwrap_or_default();
                    let value = args.named::<Value>(named.name().get())?.unwrap_or(default);
                    self.define(named.name().get().clone(), value);
                }
                ast::Param::Spread(spread) => {
                    // The sink leaves the positional arguments that later
                    // positional parameters take, and the named ones that
                    // later named parameters take.
                    let later = &params[i + 1..];
                    let keep_pos = later
                        .iter()
                        .filter(|p| matches!(p, ast::Param::Pos(_)))
                        .count();
                    let keep_named: Vec<&str> = later
                        .iter()
                        .filter_map(|p| match p {
                            ast::Param::Named(n) => Some(n.name().get().as_str()),
                            _ => None,
                        })
                        .collect();
                    let sink = args.take_sink(keep_pos, &keep_named);
                    if let Some(ident) = spread.sink_ident() {
                        self.define(ident.get().clone(), Value::Args(sink));
                    }
                }
            }
        }
        args.finish()?;
        let value = self.expr(syntax.body())?;
        match self.flow.take() {
            Some(Flow::Return(value)) => Ok(value.unwrap_or_default()),
            Some(Flow::Break(span)) => bail!(span, "cannot break outside of a loop"),
            Some(Flow::Continue(span)) => bail!(span, "cannot continue outside of a loop"),
            None => Ok(value),
        }
    }

    // ---- Markup ----

    /// Evaluates markup. A label at its end that has no element to attach
    /// to is an error.
    fn markup(&mut self, markup: ast::Markup<'_>) -> EvalResult<Content> {
        let (content, label) = self.markup_labelled(markup)?;
        if let Some(label) = label {
            return Err(label_without_element(label.span()).into());
        }
        Ok(content)
    }

    /// Evaluates markup, and returns a label at its end that has no element
    /// before it, for the heading or list item around it to take.
    fn markup_labelled<'s>(
        &mut self,
        markup: ast::Markup<'s>,
    ) -> EvalResult<(Content, Option<ast::Label<'s>>)> {
        let mut out = Content::new();
        let exprs: Vec<ast::Expr<'s>> = markup.exprs().collect();
        for (i, &expr) in exprs.iter().enumerate() {
            if let ast::Expr::Label(label) = expr {
                out.trim_end_spaces();
                match out.last_element_mut() {
                    Some(element) if element.label.is_some() => {
                        bail!(label.span(), "this element already has a label");
                    }
                    Some(element) => element.label = Some(label.get().into()),
                    None => {
                        let trailing = exprs[i + 1..]
                            .iter()
                            .all(|e| matches!(e, ast::Expr::Space(_)));
                        if !trailing {
                            return Err(label_without_element(label.span()).into());
                        }
                        return Ok((out, Some(label)));
                    }
                }
                continue;
            }
            let content = self.markup_expr(expr)?;
            out.extend(content);
            if self.flow.is_some() {
                break;
            }
        }
        Ok((out, None))
    }

    fn markup_expr(&mut self, expr: ast::Expr<'_>) -> EvalResult<Content> {
        self.nest(|vm| vm.markup_expr_inner(expr))
    }

    fn markup_expr_inner(&mut self, expr: ast::Expr<'_>) -> EvalResult<Content> {
        let span = expr.span();
        let text = |text: EcoString| Content::node(Node::Text { text, span });
        Ok(match expr {
            ast::Expr::Text(t) => {
                self.step(span)?;
                text(t.get().clone())
            }
            ast::Expr::Space(_) => Content::node(Node::Space),
            ast::Expr::Linebreak(_) => Content::node(Node::Linebreak),
            ast::Expr::Parbreak(_) => Content::node(Node::Parbreak),
            ast::Expr::Escape(e) => text(e.get().into()),
            ast::Expr::Shorthand(s) => text(s.get().into()),
            ast::Expr::SmartQuote(q) => Content::node(Node::Quote { double: q.double() }),
            ast::Expr::Strong(s) => {
                self.step(span)?;
                let body = self.markup(s.body())?;
                self.builtin("strong", [("body", body.into())], span)?
            }
            ast::Expr::Emph(e) => {
                self.step(span)?;
                let body = self.markup(e.body())?;
                self.builtin("emph", [("body", body.into())], span)?
            }
            ast::Expr::Raw(raw) => {
                self.step(span)?;
                let lines: Vec<&str> = raw.lines().map(|l| l.get().as_str()).collect();
                let mut fields = vec![
                    ("text", Value::Str(lines.join("\n").into())),
                    ("block", Value::Bool(raw.block())),
                ];
                if let Some(lang) = raw.lang() {
                    fields.push(("lang", Value::Str(lang.get().clone())));
                }
                self.builtin("raw", fields, span)?
            }
            ast::Expr::Link(link) => {
                self.step(span)?;
                self.builtin("link", [("dest", Value::Str(link.get().clone()))], span)?
            }
            ast::Expr::Ref(r) => {
                self.step(span)?;
                let mut fields = vec![("target", Value::Label(r.target().into()))];
                if let Some(supplement) = r.supplement() {
                    fields.push(("supplement", self.markup(supplement.body())?.into()));
                }
                self.builtin("ref", fields, span)?
            }
            ast::Expr::Heading(heading) => {
                self.step(span)?;
                let (body, label) = self.markup_labelled(heading.body())?;
                let level = i64::try_from(heading.depth().get()).unwrap_or(i64::MAX);
                labelled(
                    self.builtin(
                        "heading",
                        [("level", Value::Int(level)), ("body", body.into())],
                        span,
                    )?,
                    label,
                )
            }
            ast::Expr::ListItem(item) => {
                self.step(span)?;
                let (body, label) = self.markup_labelled(item.body())?;
                labelled(
                    self.builtin("list-item", [("body", body.into())], span)?,
                    label,
                )
            }
            ast::Expr::EnumItem(item) => {
                self.step(span)?;
                let (body, label) = self.markup_labelled(item.body())?;
                let mut fields = Vec::new();
                if let Some(number) = item.number() {
                    fields.push((
                        "number",
                        Value::Int(i64::try_from(number).unwrap_or(i64::MAX)),
                    ));
                }
                fields.push(("body", body.into()));
                labelled(self.builtin("enum-item", fields, span)?, label)
            }
            ast::Expr::TermItem(item) => {
                self.step(span)?;
                let term = self.markup(item.term())?;
                let (description, label) = self.markup_labelled(item.description())?;
                labelled(
                    self.builtin(
                        "terms-item",
                        [("term", term.into()), ("body", description.into())],
                        span,
                    )?,
                    label,
                )
            }
            // Math is kept as source until the math tree exists (M1).
            ast::Expr::Equation(eq) => {
                self.step(span)?;
                let body = eq.body().to_untyped().full_text();
                self.builtin(
                    "math",
                    [
                        ("text", Value::Str(body.trim().into())),
                        ("block", Value::Bool(eq.block())),
                    ],
                    span,
                )?
            }
            ast::Expr::TexEquation(eq) => {
                self.step(span)?;
                self.builtin(
                    "math",
                    [
                        ("text", Value::Str(eq.body().trim().into())),
                        ("block", Value::Bool(eq.block())),
                        ("tex", Value::Str(eq.mode().into())),
                    ],
                    span,
                )?
            }
            _ => {
                let value = self.expr(expr)?;
                display(value, span)
            }
        })
    }

    // ---- Code ----

    /// Evaluates an expression.
    pub(crate) fn expr(&mut self, expr: ast::Expr<'_>) -> EvalResult<Value> {
        let span = expr.span();
        self.step(span)?;
        self.nest(|vm| vm.expr_inner(expr))
    }

    /// Runs `f`, which recurses into a nested expression, on a new stack
    /// segment if the stack is running low. Calls nest at most
    /// [`Limits::depth`] deep and expressions at most as deep as the parser
    /// allows, but deep expressions in deep calls would exhaust a thread's
    /// stack.
    fn nest<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, || f(self))
    }

    /// The depth of a call or import made here, or an error if it is too
    /// deep.
    fn deeper(&self, span: Span) -> EvalResult<u32> {
        let depth = self.depth + 1;
        if depth > self.limits.depth {
            return Err(depth_exceeded(span, self.limits).into());
        }
        Ok(depth)
    }

    fn expr_inner(&mut self, expr: ast::Expr<'_>) -> EvalResult<Value> {
        let span = expr.span();
        match expr {
            ast::Expr::Text(_)
            | ast::Expr::Space(_)
            | ast::Expr::Linebreak(_)
            | ast::Expr::Parbreak(_)
            | ast::Expr::Escape(_)
            | ast::Expr::Shorthand(_)
            | ast::Expr::SmartQuote(_)
            | ast::Expr::Strong(_)
            | ast::Expr::Emph(_)
            | ast::Expr::Raw(_)
            | ast::Expr::Link(_)
            | ast::Expr::Ref(_)
            | ast::Expr::Heading(_)
            | ast::Expr::ListItem(_)
            | ast::Expr::EnumItem(_)
            | ast::Expr::TermItem(_)
            | ast::Expr::Equation(_)
            | ast::Expr::TexEquation(_) => self.markup_expr(expr).map(Value::Content),
            ast::Expr::Label(label) => Ok(Value::Label(label.get().into())),
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
            | ast::Expr::MathRoot(_) => bail!(span, "math is evaluated only in equations"),
            ast::Expr::TokenName(_)
            | ast::Expr::Color(_)
            | ast::Expr::Ratio(_)
            | ast::Expr::CompoundSelector(_)
            | ast::Expr::TemplateBlock(_)
            | ast::Expr::Sequence(_)
            | ast::Expr::Guarded(_) => Err(EvalError::new(span, "this is stylesheet syntax")
                .with_hint("it can be used in stylesheets, not in scripts")
                .into()),
            ast::Expr::Ident(ident) => self.lookup(ident),
            ast::Expr::None(_) => Ok(Value::None),
            ast::Expr::Auto(_) => Ok(Value::Auto),
            ast::Expr::Bool(v) => Ok(Value::Bool(v.get())),
            ast::Expr::Int(v) => Ok(Value::Int(v.get())),
            ast::Expr::Float(v) => Ok(Value::Float(Scalar::new(v.get()))),
            ast::Expr::Numeric(v) => {
                let (value, unit) = v.get();
                Ok(numeric(value, unit))
            }
            ast::Expr::Str(s) => {
                let s = s.get();
                self.alloc(s.len() as u64, span)?;
                Ok(Value::Str(s))
            }
            ast::Expr::CodeBlock(block) => {
                self.enter();
                let result = self.code(block.body().exprs());
                self.exit();
                result
            }
            ast::Expr::ContentBlock(block) => {
                self.enter();
                let result = self.markup(block.body());
                self.exit();
                result.map(Value::Content)
            }
            ast::Expr::Parenthesized(p) => self.expr(p.expr()),
            ast::Expr::Array(array) => self.array(array),
            ast::Expr::Dict(dict) => self.dict(dict),
            ast::Expr::Unary(unary) => {
                let value = self.expr(unary.expr())?;
                match unary.op() {
                    ast::UnOp::Pos => ops::pos(value),
                    ast::UnOp::Neg => ops::neg(value),
                    ast::UnOp::Not => ops::not(value),
                }
                .at(span)
            }
            ast::Expr::Binary(binary) => self.binary(binary),
            ast::Expr::FieldAccess(access) => {
                let target = self.expr(access.target())?;
                self.field(target, access.field().get(), access.field().span())
            }
            ast::Expr::FuncCall(call) => self.func_call(call),
            ast::Expr::Closure(closure) => self.closure(closure),
            ast::Expr::LetBinding(binding) => self.let_binding(binding),
            ast::Expr::DestructAssignment(assign) => {
                let value = self.expr(assign.value())?;
                self.pattern(assign.pattern(), value, false)?;
                Ok(Value::None)
            }
            ast::Expr::Conditional(cond) => {
                if self.condition(cond.condition())? {
                    self.expr(cond.if_body())
                } else if let Some(body) = cond.else_body() {
                    self.expr(body)
                } else {
                    Ok(Value::None)
                }
            }
            ast::Expr::WhileLoop(while_loop) => self.while_loop(while_loop),
            ast::Expr::ForLoop(for_loop) => self.for_loop(for_loop),
            ast::Expr::ModuleImport(import) => self.import(import),
            ast::Expr::ModuleInclude(include) => {
                let module = self.load_expr(include.source())?;
                Ok(Value::Content(module.content().clone()))
            }
            ast::Expr::LoopBreak(_) => {
                self.flow = Some(Flow::Break(span));
                Ok(Value::None)
            }
            ast::Expr::LoopContinue(_) => {
                self.flow = Some(Flow::Continue(span));
                Ok(Value::None)
            }
            ast::Expr::FuncReturn(ret) => {
                let value = ret.body().map(|body| self.expr(body)).transpose()?;
                self.flow = Some(Flow::Return(value));
                Ok(Value::None)
            }
            ast::Expr::ScopedRule(rule) => Ok(Value::Content(Content::node(Node::Rule(
                rule.to_untyped().clone(),
            )))),
            ast::Expr::ElementDecl(_) => Err(EvalError::new(
                span,
                "element declarations are not supported yet",
            )
            .with_hint("elements come from components for now")
            .into()),
        }
    }

    /// Evaluates the expressions of a code block and joins their values.
    fn code<'s>(&mut self, exprs: impl Iterator<Item = ast::Expr<'s>>) -> EvalResult<Value> {
        let mut output = Value::None;
        for expr in exprs {
            let value = self.expr(expr)?;
            output = self.join(output, value, expr.span())?;
            if self.flow.is_some() {
                break;
            }
        }
        Ok(output)
    }

    fn join(&mut self, lhs: Value, rhs: Value, span: Span) -> EvalResult<Value> {
        if !matches!(lhs, Value::None) {
            self.alloc(rhs.shallow_size(), span)?;
        }
        ops::join(lhs, rhs).at(span)
    }

    fn lookup(&mut self, ident: ast::Ident<'_>) -> EvalResult<Value> {
        let name = ident.get();
        if let Some(value) = self.scopes.get(name) {
            return Ok(value.clone());
        }
        match name.as_str() {
            "variant" => return self.variant(),
            "today" => {
                return match today(self.db, self.project) {
                    Some(date) => Ok(Value::Date(date)),
                    None => Err(EvalError::new(ident.span(), "no date is set for `today`")
                        .with_hint("set `today` in `[project]` of folio.toml, or pass a date when building")
                        .into()),
                };
            }
            _ => {}
        }
        if let Some(value) = self.globals.get(name) {
            return Ok(value.clone());
        }
        // An element name that components provide but that does not resolve,
        // because it is ambiguous or its binding is stale, says why.
        if let Some(host) = component_host(self.db, self.project)
            && let Some(element) = Name::new(name)
            && let Err(error) = host.element(&QualifiedName::unqualified(element))
            && error.reason != Reason::Unknown
        {
            bail!(ident.span(), "{error}");
        }
        bail!(ident.span(), "unknown variable: {name}")
    }

    /// The value of `variant`: the name of the variant being built, or
    /// `none` for a project without variants. Evaluation for all variants
    /// at once stops here.
    fn variant(&self) -> EvalResult<Value> {
        match &self.mode {
            Mode::Shared => Err(Stop::NeedsVariant),
            Mode::Variant(Some(name)) => Ok(Value::Str(name.as_str().into())),
            Mode::Variant(None) => Ok(Value::None),
        }
    }

    fn condition(&mut self, expr: ast::Expr<'_>) -> EvalResult<bool> {
        match self.expr(expr)? {
            Value::Bool(b) => Ok(b),
            v => bail!(
                expr.span(),
                "expected a boolean, found {}",
                a_type(v.type_name())
            ),
        }
    }

    fn array(&mut self, array: ast::Array<'_>) -> EvalResult<Value> {
        let mut out = Array::new();
        for item in array.items() {
            match item {
                ast::ArrayItem::Pos(expr) => out.push(self.expr(expr)?),
                ast::ArrayItem::Spread(spread) => match self.expr(spread.expr())? {
                    Value::None => {}
                    Value::Array(items) => out.extend(items),
                    v => bail!(
                        spread.span(),
                        "cannot spread {} into an array",
                        a_type(v.type_name())
                    ),
                },
            }
        }
        self.alloc(
            Value::Array(EcoVec::new()).shallow_size() * out.len() as u64,
            array.span(),
        )?;
        Ok(Value::Array(out))
    }

    fn dict(&mut self, dict: ast::Dict<'_>) -> EvalResult<Value> {
        let mut out = Dict::new();
        for item in dict.items() {
            match item {
                ast::DictItem::Named(named) => {
                    let value = self.expr(named.expr())?;
                    out.insert(named.name().get().clone(), value);
                }
                ast::DictItem::Keyed(keyed) => {
                    let key = match self.expr(keyed.key())? {
                        Value::Str(s) => s,
                        v => bail!(
                            keyed.key().span(),
                            "expected a string as key, found {}",
                            a_type(v.type_name())
                        ),
                    };
                    let value = self.expr(keyed.expr())?;
                    out.insert(key, value);
                }
                ast::DictItem::Spread(spread) => match self.expr(spread.expr())? {
                    Value::None => {}
                    Value::Dict(other) => {
                        for (key, value) in other.iter() {
                            out.insert(key.clone(), value.clone());
                        }
                    }
                    v => bail!(
                        spread.span(),
                        "cannot spread {} into a dictionary",
                        a_type(v.type_name())
                    ),
                },
            }
        }
        self.alloc(16 * out.len() as u64, dict.span())?;
        Ok(Value::Dict(out))
    }

    fn binary(&mut self, binary: ast::Binary<'_>) -> EvalResult<Value> {
        use ast::BinOp::*;
        let span = binary.span();
        let op = binary.op();
        match op {
            And | Or => {
                let lhs = self.condition(binary.lhs())?;
                if lhs == (op == Or) {
                    return Ok(Value::Bool(lhs));
                }
                Ok(Value::Bool(self.condition(binary.rhs())?))
            }
            Assign => {
                let value = self.expr(binary.rhs())?;
                *self.access(binary.lhs(), true)? = value;
                Ok(Value::None)
            }
            AddAssign | SubAssign | MulAssign | DivAssign => {
                let rhs = self.expr(binary.rhs())?;
                let lhs = std::mem::take(self.access(binary.lhs(), false)?);
                let op = match op {
                    AddAssign => Add,
                    SubAssign => Sub,
                    MulAssign => Mul,
                    _ => Div,
                };
                let value = self.apply(op, lhs, rhs, span)?;
                *self.access(binary.lhs(), false)? = value;
                Ok(Value::None)
            }
            Eq | Neq | Lt | Leq | Gt | Geq | In | NotIn => {
                let lhs = self.expr(binary.lhs())?;
                let rhs = self.expr(binary.rhs())?;
                let result = match op {
                    Eq => ops::equal(&lhs, &rhs),
                    Neq => !ops::equal(&lhs, &rhs),
                    Lt => ops::compare(&lhs, &rhs).at(span)?.is_lt(),
                    Leq => ops::compare(&lhs, &rhs).at(span)?.is_le(),
                    Gt => ops::compare(&lhs, &rhs).at(span)?.is_gt(),
                    Geq => ops::compare(&lhs, &rhs).at(span)?.is_ge(),
                    In => ops::contains(&lhs, &rhs).at(span)?,
                    _ => !ops::contains(&lhs, &rhs).at(span)?,
                };
                Ok(Value::Bool(result))
            }
            Add | Sub | Mul | Div => {
                let lhs = self.expr(binary.lhs())?;
                let rhs = self.expr(binary.rhs())?;
                self.apply(op, lhs, rhs, span)
            }
        }
    }

    /// Applies an arithmetic operator, counting what a concatenation or
    /// repetition allocates before it happens.
    fn apply(&mut self, op: ast::BinOp, lhs: Value, rhs: Value, span: Span) -> EvalResult<Value> {
        let grows = |v: &Value| {
            matches!(
                v,
                Value::Str(_) | Value::Array(_) | Value::Content(_) | Value::Dict(_)
            )
        };
        let bytes = match (op, &lhs, &rhs) {
            (ast::BinOp::Add, a, b) if grows(a) && grows(b) => b.shallow_size(),
            (ast::BinOp::Mul, a, Value::Int(n)) | (ast::BinOp::Mul, Value::Int(n), a)
                if grows(a) =>
            {
                a.shallow_size()
                    .saturating_mul(u64::try_from(*n).unwrap_or(0))
            }
            _ => 0,
        };
        self.alloc(bytes, span)?;
        match op {
            ast::BinOp::Add => ops::add(lhs, rhs),
            ast::BinOp::Sub => ops::sub(lhs, rhs),
            ast::BinOp::Mul => ops::mul(lhs, rhs),
            _ => ops::div(lhs, rhs),
        }
        .at(span)
    }

    /// The place an assignment or a mutating method changes: a variable, a
    /// dictionary field, or an item (`x.at(i)`). A plain assignment
    /// (`insert`) adds a missing dictionary field; anything else needs it.
    fn access<'s>(&'s mut self, expr: ast::Expr<'_>, insert: bool) -> EvalResult<&'s mut Value> {
        match expr {
            ast::Expr::Ident(ident) => self.scopes.get_mut(ident.get()).at(ident.span()),
            ast::Expr::Parenthesized(p) => self.access(p.expr(), insert),
            ast::Expr::FieldAccess(access) => {
                let field = access.field();
                match self.access(access.target(), false)? {
                    Value::Dict(dict) => {
                        if !insert && dict.get(field.get()).is_none() {
                            bail!(
                                field.span(),
                                "dictionary does not contain key {:?}",
                                field.get()
                            );
                        }
                        Ok(dict.get_or_insert_mut(field.get().clone()))
                    }
                    v => bail!(
                        field.span(),
                        "cannot change the fields of {}",
                        a_type(v.type_name())
                    ),
                }
            }
            ast::Expr::FuncCall(call) => {
                let span = call.span();
                let ast::Expr::FieldAccess(access) = call.callee() else {
                    bail!(span, "cannot change the result of a call");
                };
                if access.field().get() != "at" {
                    bail!(span, "cannot change the result of a call");
                }
                let mut args = self.args(call.args(), span, false)?;
                let key: Value = args.expect("index or key")?;
                args.finish()?;
                match self.access(access.target(), false)? {
                    Value::Array(items) => {
                        let Value::Int(i) = key else {
                            bail!(span, "expected an integer index");
                        };
                        let index = library::index(items.len(), i).at(span)?;
                        Ok(&mut items.make_mut()[index])
                    }
                    Value::Dict(dict) => {
                        let Value::Str(key) = key else {
                            bail!(span, "expected a string key");
                        };
                        match dict.get_mut(&key) {
                            Some(value) => Ok(value),
                            None => bail!(span, "dictionary does not contain key {key:?}"),
                        }
                    }
                    v => bail!(span, "cannot change the items of {}", a_type(v.type_name())),
                }
            }
            _ => bail!(expr.span(), "cannot change a temporary value"),
        }
    }

    fn func_call(&mut self, call: ast::FuncCall<'_>) -> EvalResult<Value> {
        let span = call.span();
        if let ast::Expr::FieldAccess(access) = call.callee() {
            let name = access.field().get();
            let target = self.expr(access.target())?;
            // `push`, `pop`, `insert` and `remove` change an array or
            // dictionary in place, unless the dictionary holds a function of
            // that name. The target is evaluated again, as a place.
            let mutates = match &target {
                Value::Array(_) => true,
                Value::Dict(dict) => !matches!(dict.get(name), Some(Value::Func(_))),
                _ => false,
            };
            if mutates && library::is_mutating_method(name) {
                let args = self.args(call.args(), span, false)?;
                self.alloc(16, span)?;
                let target = self.access(access.target(), false)?;
                return library::call_mutating(target, name, args, span);
            }
            let func = match &target {
                Value::Module(module) => match module.scope().get(name) {
                    Some(func) => Some(func.clone()),
                    None => bail!(
                        access.field().span(),
                        "module `{}` does not contain `{name}`",
                        module.name()
                    ),
                },
                Value::Dict(dict) => match dict.get(name) {
                    Some(func @ Value::Func(_)) => Some(func.clone()),
                    _ => None,
                },
                _ => None,
            };
            let args = self.call_args(func.as_ref(), call.args(), span)?;
            return match func {
                Some(func) => self.call_value(func, args, span),
                None => library::call_method(self, target, name, args, span),
            };
        }
        let callee = self.expr(call.callee())?;
        let args = self.call_args(Some(&callee), call.args(), span)?;
        self.call_value(callee, args, span)
    }

    /// Evaluates the arguments of a call. For a call of an element, bare
    /// keywords are values of the fields that take them (`placement:
    /// wrap`), and the element's slots are defined as functions that make
    /// their sub-blocks (`#statement[..]`).
    fn call_args(
        &mut self,
        callee: Option<&Value>,
        args: ast::Args<'_>,
        span: Span,
    ) -> EvalResult<Args> {
        let Some(Value::Func(Func::Element(name))) = callee else {
            return self.args(args, span, false);
        };
        let slots = match crate::model::schema(self.db, self.project, name).map(|s| &s.body) {
            Some(Body::Slots(slots)) => slots.clone(),
            _ => return self.args(args, span, true),
        };
        self.enter();
        for slot in &slots {
            let func = Func::Slot(Arc::new((name.clone(), slot.name.clone())));
            self.define(slot.name.as_str().into(), Value::Func(func));
        }
        let result = self.args(args, span, true);
        self.exit();
        result
    }

    fn args(&mut self, args: ast::Args<'_>, span: Span, element: bool) -> EvalResult<Args> {
        let mut items = EcoVec::new();
        for arg in args.items() {
            match arg {
                ast::Arg::Pos(expr) => items.push(Arg {
                    span: expr.span(),
                    name: None,
                    scope: None,
                    value: self.expr(expr)?,
                }),
                ast::Arg::Named(named) => {
                    let name = named.name().get().clone();
                    let value = match named.expr() {
                        ast::Expr::Ident(word)
                            if element && element::KEYWORD_FIELDS.contains(&name.as_str()) =>
                        {
                            self.step(word.span())?;
                            Value::Str(word.get().clone())
                        }
                        expr => self.expr(expr)?,
                    };
                    items.push(Arg {
                        span: named.span(),
                        name: Some(name),
                        scope: named.scope().map(|s| s.get().clone()),
                        value,
                    });
                }
                ast::Arg::Spread(spread) => match self.expr(spread.expr())? {
                    Value::None => {}
                    Value::Array(values) => {
                        items.extend(values.into_iter().map(|value| Arg {
                            span: spread.span(),
                            name: None,
                            scope: None,
                            value,
                        }));
                    }
                    Value::Dict(dict) => {
                        items.extend(dict.iter().map(|(key, value)| Arg {
                            span: spread.span(),
                            name: Some(key.clone()),
                            scope: None,
                            value: value.clone(),
                        }));
                    }
                    Value::Args(args) => items.extend(args.items),
                    v => bail!(
                        spread.span(),
                        "cannot spread {} into arguments",
                        a_type(v.type_name())
                    ),
                },
            }
        }
        Ok(Args { span, items })
    }

    fn call_value(&mut self, callee: Value, args: Args, span: Span) -> EvalResult<Value> {
        match callee {
            Value::Func(func) => self.call(func, args, span),
            v => bail!(span, "{} is not a function", a_type(v.type_name())),
        }
    }

    /// Calls a function.
    pub(crate) fn call(&mut self, func: Func, args: Args, span: Span) -> EvalResult<Value> {
        match func {
            Func::Native(native) => {
                let mut args = args;
                let value = (native.func)(self, &mut args)?;
                args.finish()?;
                Ok(value)
            }
            Func::Closure(closure) => self.call_closure(closure, args, span),
            Func::Element(name) => self.instantiate(&name, args, span).map(Value::Content),
            Func::Slot(slot) => self.slot(&slot.0, &slot.1, args, span),
            Func::With(with) => {
                let (func, applied) = &*with;
                let mut all = applied.clone();
                all.span = args.span;
                all.items.extend(args.items);
                self.call(func.clone(), all, span)
            }
        }
    }

    /// Calls a closure through its memoised query: for all variants first,
    /// and for this evaluation's variant if the call reads `variant`.
    fn call_closure(&mut self, closure: Closure, args: Args, span: Span) -> EvalResult<Value> {
        let depth = self.deeper(span)?;
        let name = closure.name().cloned();
        let shared = super::call_closure(
            self.db,
            self.project,
            closure.clone(),
            args.clone(),
            depth,
            Mode::Shared,
        );
        let memo = match (&shared.outcome, &self.mode) {
            (Err(Stop::NeedsVariant), Mode::Variant(_)) => super::call_closure(
                self.db,
                self.project,
                closure,
                args,
                depth,
                self.mode.clone(),
            ),
            _ => shared,
        };
        self.charge(memo.cost, span)?;
        match &memo.outcome {
            Ok(value) => Ok(value.clone()),
            Err(Stop::Error(error)) => {
                let mut error = (**error).clone();
                error.trace_point(
                    span,
                    match name {
                        Some(name) => eco_format!("in this call of `{name}`"),
                        None => "in this call".into(),
                    },
                );
                Err(error.into())
            }
            Err(stop) => Err(stop.clone()),
        }
    }

    fn closure(&mut self, closure: ast::Closure<'_>) -> EvalResult<Value> {
        let mut defaults = EcoVec::new();
        for param in closure.params().children() {
            if let ast::Param::Named(named) = param {
                defaults.push(self.expr(named.expr())?);
            }
        }
        let captured = captures(closure, |name| self.scopes.get(name).cloned());
        let closure = Closure::new(
            closure.to_untyped().clone(),
            self.file.clone(),
            closure.name().map(|name| name.get().clone()),
            captured.into(),
            defaults,
        );
        Ok(Value::Func(Func::Closure(closure)))
    }

    fn let_binding(&mut self, binding: ast::LetBinding<'_>) -> EvalResult<Value> {
        match binding.kind() {
            ast::LetBindingKind::Normal(pattern) => {
                let value = match binding.init() {
                    Some(init) => self.expr(init)?,
                    None => Value::None,
                };
                self.pattern(pattern, value, true)?;
            }
            ast::LetBindingKind::Closure(name) => {
                let value = match binding.init() {
                    Some(ast::Expr::Closure(closure)) => self.closure(closure)?,
                    _ => bail!(binding.span(), "expected a function"),
                };
                self.define(name.get().clone(), value);
            }
        }
        Ok(Value::None)
    }

    /// Binds (`let`) or assigns a value to a pattern.
    fn pattern(&mut self, pattern: ast::Pattern<'_>, value: Value, bind: bool) -> EvalResult<()> {
        match pattern {
            ast::Pattern::Normal(ast::Expr::Ident(ident)) if bind => {
                self.define(ident.get().clone(), value);
            }
            ast::Pattern::Normal(expr) => {
                if bind {
                    bail!(expr.span(), "cannot bind to this expression");
                }
                *self.access(expr, true)? = value;
            }
            ast::Pattern::Placeholder(_) => {}
            ast::Pattern::Parenthesized(p) => self.pattern(p.pattern(), value, bind)?,
            ast::Pattern::Destructuring(destructuring) => match value {
                Value::Array(items) => self.destructure_array(destructuring, items, bind)?,
                Value::Dict(dict) => self.destructure_dict(destructuring, dict, bind)?,
                v => bail!(
                    pattern.span(),
                    "cannot destructure {}",
                    a_type(v.type_name())
                ),
            },
        }
        Ok(())
    }

    fn sink(&mut self, spread: ast::Spread<'_>, value: Value, bind: bool) -> EvalResult<()> {
        if bind {
            if let Some(ident) = spread.sink_ident() {
                self.define(ident.get().clone(), value);
            }
        } else if let Some(expr) = spread.sink_expr() {
            *self.access(expr, true)? = value;
        }
        Ok(())
    }

    fn destructure_array(
        &mut self,
        destructuring: ast::Destructuring<'_>,
        items: Array,
        bind: bool,
    ) -> EvalResult<()> {
        let span = destructuring.span();
        let parts: Vec<_> = destructuring.items().collect();
        let fixed = parts
            .iter()
            .filter(|p| !matches!(p, ast::DestructuringItem::Spread(_)))
            .count();
        let has_sink = parts.len() > fixed;
        if items.len() < fixed {
            bail!(span, "not enough elements to destructure");
        }
        if !has_sink && items.len() > fixed {
            bail!(span, "too many elements to destructure");
        }
        let mut i = 0;
        for part in parts {
            match part {
                ast::DestructuringItem::Pattern(pattern) => {
                    self.pattern(pattern, items[i].clone(), bind)?;
                    i += 1;
                }
                ast::DestructuringItem::Named(named) => {
                    bail!(
                        named.span(),
                        "cannot destructure a named pattern from an array"
                    );
                }
                ast::DestructuringItem::Spread(spread) => {
                    let n = items.len() - fixed;
                    let rest: Array = items[i..i + n].iter().cloned().collect();
                    i += n;
                    self.sink(spread, Value::Array(rest), bind)?;
                }
            }
        }
        Ok(())
    }

    fn destructure_dict(
        &mut self,
        destructuring: ast::Destructuring<'_>,
        dict: Dict,
        bind: bool,
    ) -> EvalResult<()> {
        let mut used = Vec::new();
        let mut sink = None;
        for part in destructuring.items() {
            match part {
                ast::DestructuringItem::Pattern(ast::Pattern::Normal(ast::Expr::Ident(ident))) => {
                    let Some(value) = dict.get(ident.get()) else {
                        bail!(
                            ident.span(),
                            "dictionary does not contain key {:?}",
                            ident.get()
                        );
                    };
                    used.push(ident.get().clone());
                    self.pattern(
                        ast::Pattern::Normal(ast::Expr::Ident(ident)),
                        value.clone(),
                        bind,
                    )?;
                }
                ast::DestructuringItem::Named(named) => {
                    let name = named.name();
                    let Some(value) = dict.get(name.get()) else {
                        bail!(
                            name.span(),
                            "dictionary does not contain key {:?}",
                            name.get()
                        );
                    };
                    used.push(name.get().clone());
                    self.pattern(named.pattern(), value.clone(), bind)?;
                }
                ast::DestructuringItem::Spread(spread) => sink = Some(spread),
                ast::DestructuringItem::Pattern(pattern) => bail!(
                    pattern.span(),
                    "cannot destructure an unnamed pattern from a dictionary"
                ),
            }
        }
        if let Some(spread) = sink {
            let rest: Dict = dict
                .iter()
                .filter(|(key, _)| !used.contains(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            self.sink(spread, Value::Dict(rest), bind)?;
        }
        Ok(())
    }

    fn while_loop(&mut self, while_loop: ast::WhileLoop<'_>) -> EvalResult<Value> {
        let mut output = Value::None;
        let body = while_loop.body();
        loop {
            self.step(while_loop.span())?;
            if !self.condition(while_loop.condition())? {
                break;
            }
            let value = self.expr(body)?;
            output = self.join(output, value, body.span())?;
            if self.leave_loop() {
                break;
            }
        }
        Ok(output)
    }

    /// Handles a `break`, `continue` or `return` at the end of a loop body:
    /// whether to leave the loop.
    fn leave_loop(&mut self) -> bool {
        match self.flow {
            Some(Flow::Break(_)) => {
                self.flow = None;
                true
            }
            Some(Flow::Continue(_)) => {
                self.flow = None;
                false
            }
            Some(Flow::Return(_)) => true,
            None => false,
        }
    }

    fn for_loop(&mut self, for_loop: ast::ForLoop<'_>) -> EvalResult<Value> {
        let iterable = self.expr(for_loop.iterable())?;
        let items: Array = match iterable {
            Value::Array(items) => items,
            Value::Dict(dict) => dict
                .iter()
                .map(|(k, v)| Value::Array(EcoVec::from([Value::Str(k.clone()), v.clone()])))
                .collect(),
            Value::Str(s) => s.chars().map(|c| Value::Str(c.into())).collect(),
            Value::Args(args) => args.items.into_iter().map(|arg| arg.value).collect(),
            v => bail!(
                for_loop.iterable().span(),
                "cannot loop over {}",
                a_type(v.type_name())
            ),
        };
        let mut output = Value::None;
        let body = for_loop.body();
        for item in items {
            self.step(for_loop.span())?;
            self.enter();
            let result = self
                .pattern(for_loop.pattern(), item, true)
                .and_then(|()| self.expr(body));
            self.exit();
            output = self.join(output, result?, body.span())?;
            if self.leave_loop() {
                break;
            }
        }
        Ok(output)
    }

    fn import(&mut self, import: ast::ModuleImport<'_>) -> EvalResult<Value> {
        let module = self.load_expr(import.source())?;
        if let Some(name) = import.new_name() {
            self.define(name.get().clone(), Value::Module(module.clone()));
        }
        match import.imports() {
            None if import.new_name().is_none() => match import.bare_name() {
                Ok(name) => self.define(name, Value::Module(module)),
                Err(_) => bail!(
                    import.source().span(),
                    "cannot name the imported module after this path"
                ),
            },
            None => {}
            Some(ast::Imports::Wildcard) => {
                for (name, value) in module.scope().iter() {
                    self.define(name.clone(), value.clone());
                }
            }
            Some(ast::Imports::Items(items)) => {
                for item in items.iter() {
                    let mut value = Value::Module(module.clone());
                    for ident in item.path().iter() {
                        value = match value {
                            Value::Module(m) => match m.scope().get(ident.get()) {
                                Some(v) => v.clone(),
                                None => bail!(
                                    ident.span(),
                                    "module `{}` does not contain `{}`",
                                    m.name(),
                                    ident.get()
                                ),
                            },
                            v => bail!(ident.span(), "{} is not a module", a_type(v.type_name())),
                        };
                    }
                    self.define(item.bound_name().get().clone(), value);
                }
            }
        }
        Ok(Value::None)
    }

    /// The module an import or include names: a file by a literal path, or
    /// a module value.
    fn load_expr(&mut self, source: ast::Expr<'_>) -> EvalResult<Module> {
        if let ast::Expr::Str(path) = source {
            return self.load(&path.get(), path.span());
        }
        match self.expr(source)? {
            Value::Module(module) => Ok(module),
            Value::Str(_) => Err(EvalError::new(source.span(), "the path must be written as a string")
                .with_hint("Folio finds the files a document reads before it runs any script, so paths cannot be computed")
                .into()),
            v => bail!(source.span(), "cannot import from {}", a_type(v.type_name())),
        }
    }

    /// Evaluates a content file named by a path in this file, through its
    /// memoised query.
    fn load(&mut self, text: &str, span: Span) -> EvalResult<Module> {
        if text.starts_with('@') {
            bail!(span, "packages are not supported yet");
        }
        // These are the errors that `check_source` and `build_files` report
        // for the same path, so a front end shows them once.
        let Some(path) = self.file.join(text) else {
            return Err(
                EvalError::new(span, eco_format!("`{text}` is not a path in the project"))
                    .with_hint("Folio reads files only inside the project directory")
                    .into(),
            );
        };
        if path.extension() != Some("fol") {
            return Err(
                EvalError::new(span, eco_format!("`{text}` is not a content file"))
                    .with_hint("only `.fol` files can be imported or included")
                    .into(),
            );
        }
        let Some(file) = source_file(self.db, self.project, path.clone()) else {
            return Err(
                EvalError::new(span, eco_format!("file `{}` not found", path.as_str()))
                    .with_hint("Folio reads `.fol` and `.fsty` files inside the project")
                    .into(),
            );
        };
        if path == self.file || reachable(self.db, self.project, file).contains(&self.file) {
            return Err(EvalError::new(span, "cyclic import")
                .with_hint(eco_format!(
                    "`{}` imports or includes this file, directly or through other files",
                    path.as_str()
                ))
                .into());
        }
        let depth = self.deeper(span)?;
        let memo = eval_file_for(self.db, self.project, file, &self.mode, depth);
        self.charge(memo.cost, span)?;
        match &memo.outcome {
            Ok(module) => Ok(module.clone()),
            Err(Stop::Error(error)) => {
                let mut error = (**error).clone();
                error.trace_point(span, eco_format!("in `{}`, evaluated here", path.as_str()));
                Err(error.into())
            }
            Err(stop) => Err(stop.clone()),
        }
    }
}

/// Attaches the label that a heading's or list item's body ended with.
fn labelled(mut content: Content, label: Option<ast::Label<'_>>) -> Content {
    if let (Some(label), Some(element)) = (label, content.last_element_mut()) {
        element.label = Some(label.get().into());
    }
    content
}

/// A value shown in markup: content as is, strings as text, `none` as
/// nothing, and anything else as its code representation.
fn display(value: Value, span: Span) -> Content {
    match value {
        Value::None => Content::new(),
        Value::Content(content) => content,
        Value::Str(text) => Content::node(Node::Text { text, span }),
        other => Content::node(Node::Text {
            text: other.repr(),
            span,
        }),
    }
}

impl Vm<'_> {
    /// Builds a built-in element that markup produces, checked against its
    /// schema like a call.
    fn builtin<'f>(
        &mut self,
        name: &str,
        fields: impl IntoIterator<Item = (&'f str, Value)>,
        span: Span,
    ) -> EvalResult<Content> {
        let name =
            QualifiedName::unqualified(Name::new(name).expect("built-in names are identifiers"));
        let items = fields
            .into_iter()
            .map(|(field, value)| Arg {
                span,
                name: Some(field.into()),
                scope: None,
                value,
            })
            .collect();
        self.instantiate(&name, Args { span, items }, span)
    }

    /// A field of a value: `dict.key`, `module.name`, `heading.level`. A
    /// field of an element that its schema declares, or a universal one,
    /// is `none` when it was not given.
    fn field(&self, target: Value, name: &str, span: Span) -> EvalResult<Value> {
        if let Value::Content(content) = &target
            && let Some(element) = content.as_element()
            && element.field(name).is_none()
            && name != "label"
            && let Some(schema) = crate::model::schema(self.db, self.project, &element.name)
            && (schema.field(name).is_some()
                || schema.slot(name).is_some()
                || (name == BODY && schema.body != Body::None)
                || UNIVERSAL_FIELDS.contains(&name))
        {
            return Ok(match name {
                "id" => element.label.clone().map_or(Value::None, Value::Label),
                _ => Value::None,
            });
        }
        field(target, name, span)
    }
}

/// A field of a value, as it is stored.
fn field(target: Value, name: &str, span: Span) -> EvalResult<Value> {
    match &target {
        Value::Dict(dict) => match dict.get(name) {
            Some(value) => Ok(value.clone()),
            None => bail!(span, "dictionary does not contain key {name:?}"),
        },
        Value::Module(module) => match module.scope().get(name) {
            Some(value) => Ok(value.clone()),
            None => bail!(span, "module `{}` does not contain `{name}`", module.name()),
        },
        Value::Content(content) => match content.as_element() {
            Some(element) => match element.field(name) {
                Some(value) => Ok(value.clone()),
                None if name == "label" => {
                    Ok(element.label.clone().map_or(Value::None, Value::Label))
                }
                None => bail!(span, "`{}` has no field `{name}`", element.name),
            },
            None => bail!(span, "only content that is one element has fields"),
        },
        Value::Date(date) => match name {
            "year" => Ok(Value::Int(date.year.into())),
            "month" => Ok(Value::Int(date.month.into())),
            "day" => Ok(Value::Int(date.day.into())),
            _ => bail!(span, "a date has no field `{name}`"),
        },
        v => bail!(span, "{} has no fields", a_type(v.type_name())),
    }
}

fn numeric(value: f64, unit: Unit) -> Value {
    match unit {
        Unit::Pt => Value::Length(Length::abs(Abs::pt(value))),
        Unit::Mm => Value::Length(Length::abs(Abs::mm(value))),
        Unit::Cm => Value::Length(Length::abs(Abs::cm(value))),
        Unit::In => Value::Length(Length::abs(Abs::inches(value))),
        Unit::Em => Value::Length(Length::em(Em::new(value))),
        Unit::Rad => Value::Angle(Angle(Scalar::new(value))),
        Unit::Deg => Value::Angle(Angle(Scalar::new(value.to_radians()))),
        Unit::Fr => Value::Fraction(Fraction(Scalar::new(value))),
        Unit::Percent => Value::Ratio(Ratio::new(value / 100.0)),
        Unit::Ms => Value::Duration(Duration(Scalar::new(value / 1000.0))),
        Unit::S => Value::Duration(Duration(Scalar::new(value))),
        Unit::Min => Value::Duration(Duration(Scalar::new(value * 60.0))),
    }
}

/// How a positional parameter is named in "missing argument" errors.
fn pattern_name(pattern: ast::Pattern<'_>) -> EcoString {
    match pattern {
        ast::Pattern::Normal(ast::Expr::Ident(ident)) => ident.get().clone(),
        _ => pattern.to_untyped().full_text(),
    }
}

fn label_without_element(span: Span) -> EvalError {
    EvalError::new(span, "a label must follow an element").with_hint(
        "labels attach to the element before them, such as a heading, a figure or a call",
    )
}

fn steps_exceeded(span: Span, limits: Limits) -> EvalError {
    EvalError::new(
        span,
        eco_format!("evaluation exceeded the limit of {} steps", limits.steps),
    )
    .with_hint("check for a loop that does not end")
}

fn memory_exceeded(span: Span, limits: Limits) -> EvalError {
    EvalError::new(
        span,
        eco_format!(
            "evaluation exceeded the memory limit of {} MiB",
            limits.memory / (1024 * 1024)
        ),
    )
    .with_hint("check for a string, array or content that keeps growing")
}

fn depth_exceeded(span: Span, limits: Limits) -> EvalError {
    EvalError::new(
        span,
        eco_format!(
            "calls and imports are nested more than {} deep",
            limits.depth
        ),
    )
    .with_hint("check for a function that calls itself without end")
}

// ---- Arguments ----

/// Converts a value into an argument type.
pub(crate) trait FromValue: Sized {
    fn from_value(value: Value) -> StrResult<Self>;
}

fn expected(what: &str, value: &Value) -> EcoString {
    eco_format!("expected {what}, found {}", a_type(value.type_name()))
}

impl FromValue for Value {
    fn from_value(value: Value) -> StrResult<Self> {
        Ok(value)
    }
}

impl FromValue for i64 {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Int(v) => Ok(v),
            v => Err(expected("an integer", &v)),
        }
    }
}

impl FromValue for f64 {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Int(v) => Ok(v as f64),
            Value::Float(v) => Ok(v.get()),
            v => Err(expected("a number", &v)),
        }
    }
}

impl FromValue for bool {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Bool(v) => Ok(v),
            v => Err(expected("a boolean", &v)),
        }
    }
}

impl FromValue for EcoString {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Str(v) => Ok(v),
            v => Err(expected("a string", &v)),
        }
    }
}

impl FromValue for Array {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Array(v) => Ok(v),
            v => Err(expected("an array", &v)),
        }
    }
}

impl FromValue for Func {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Func(v) => Ok(v),
            v => Err(expected("a function", &v)),
        }
    }
}

impl FromValue for Content {
    fn from_value(value: Value) -> StrResult<Self> {
        match value {
            Value::Content(v) => Ok(v),
            Value::None => Ok(Content::new()),
            Value::Str(text) => Ok(Content::node(Node::Text {
                text,
                span: Span::detached(),
            })),
            v => Err(expected("content", &v)),
        }
    }
}

impl Args {
    /// Takes the first positional argument.
    pub(crate) fn eat(&mut self) -> Option<Arg> {
        let index = self.items.iter().position(|arg| arg.name.is_none())?;
        Some(self.items.remove(index))
    }

    /// Takes the first positional argument, converted, if there is one.
    pub(crate) fn find<T: FromValue>(&mut self) -> EvalResult<Option<T>> {
        match self.eat() {
            Some(arg) => T::from_value(arg.value).at(arg.span).map(Some),
            None => Ok(None),
        }
    }

    /// Takes the first positional argument, converted.
    pub(crate) fn expect<T: FromValue>(&mut self, what: &str) -> EvalResult<T> {
        match self.eat() {
            Some(arg) => T::from_value(arg.value).at(arg.span),
            None => bail!(self.span, "missing argument: {what}"),
        }
    }

    /// Takes every named argument with a name (the last one wins), converted.
    pub(crate) fn named<T: FromValue>(&mut self, name: &str) -> EvalResult<Option<T>> {
        let mut found = None;
        while let Some(index) = self
            .items
            .iter()
            .position(|arg| arg.name.as_deref() == Some(name) && arg.scope.is_none())
        {
            let arg = self.items.remove(index);
            found = Some(T::from_value(arg.value).at(arg.span)?);
        }
        Ok(found)
    }

    /// Takes every argument except the last `keep_pos` positional ones and
    /// the named ones in `keep_named`.
    fn take_sink(&mut self, keep_pos: usize, keep_named: &[&str]) -> Args {
        let positional = self.items.iter().filter(|a| a.name.is_none()).count();
        let mut take_pos = positional.saturating_sub(keep_pos);
        let mut sink = EcoVec::new();
        let mut rest = EcoVec::new();
        for arg in std::mem::take(&mut self.items) {
            let take = match &arg.name {
                None if take_pos > 0 => {
                    take_pos -= 1;
                    true
                }
                None => false,
                Some(name) => !keep_named.contains(&name.as_str()),
            };
            if take {
                sink.push(arg);
            } else {
                rest.push(arg);
            }
        }
        self.items = rest;
        Args {
            span: self.span,
            items: sink,
        }
    }

    /// Fails at the first argument nobody took.
    pub(crate) fn finish(self) -> EvalResult<()> {
        match self.items.first() {
            None => Ok(()),
            Some(Arg {
                name: Some(name),
                scope: Some(scope),
                span,
                ..
            }) => Err(
                EvalError::new(*span, eco_format!("unexpected argument: {name}@{scope}"))
                    .with_hint("only element fields can be limited to a variant or target")
                    .into(),
            ),
            Some(Arg {
                name: Some(name),
                span,
                ..
            }) => bail!(*span, "unexpected argument: {name}"),
            Some(arg) => bail!(arg.span, "unexpected argument"),
        }
    }
}
