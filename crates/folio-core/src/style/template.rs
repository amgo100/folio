//! Render templates (SPEC §5.3): compiling `render` blocks, choosing an
//! element's template, and expanding it in the Style stage.
//!
//! A template is compiled once per build, with the cascade: its parts
//! (`header`, `body`) become trees of the closed primitive set ([`Item`]),
//! whose literal values (lengths, strings, tokens) are evaluated by the
//! stylesheet evaluator and whose conditions read the element's fields
//! through the operators of the expression language. The primitives are
//! checked against the element's schema, so `slot(answer)` on an element
//! without that slot is an error at the `render` block, in every build.
//!
//! Expanding a template for an element gives an [`Expansion`]: the pieces
//! the text stage collects, each with the computed style of the node it
//! stands for. The nodes a template makes are matched like the parts of
//! the element: its header as the part `header`, `strong` and `emph` as
//! those elements, and `badge`, `row`, `stack`, `box` and `rule` as parts
//! of those names, so `example > header { .. }` and `example badge { .. }`
//! style them. A `grid` and its cells are matched as `grid` and `cell`, so
//! a template's grid looks like a table. Expansions are memoised per
//! build by template, element context, style and the values of the fields
//! the template reads.

use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{BODY, Body, ElementSchema, FieldType, Name, QualifiedName};
use folio_syntax::Span;
use folio_syntax::ast::{self, AstNode};

use super::computed::ComputedStyle;
use super::props::Properties;
use super::rules::{Decl, DeclValue, Origin};
use super::selector::NodeName;
use super::value::{Declared, Values};
use crate::diag::{self, Diagnostic};
use crate::eval::{self, Element, ops};

/// The names of the primitives of templates (SPEC §5.3), as messages list
/// them.
const PRIMITIVES: &str = "`row`, `stack`, `grid`, `box`, `rule`, `slot`, `field`, `counter`, \
                          `term`, `text`, `strong`, `emph`, `badge`, `image`, `space` and \
                          `align-start`, `align-center`, `align-end`";

/// The graphic primitives of SPEC §9.1, which templates may use. They are
/// drawn once graphics are (M4).
const GRAPHICS: &[&str] = &["path", "shape", "arrow", "label", "axis", "group"];

/// The names of the parts a template's primitives make, which selectors
/// can name like the parts of an element (see the module docs).
pub(super) const TEMPLATE_PARTS: &[&str] = &["header", "badge", "row", "stack", "box", "rule"];

/// The name of the part of an element that a template's header is.
pub const HEADER_PART: &str = "header";

/// How a template exports to Office (SPEC §5.3): which row of the table
/// there it falls in, by the most demanding primitive it uses.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum OfficeClass {
    /// Block properties only: a paragraph style named by `export-name`, or
    /// in PowerPoint the master's text styles or a formatted shape.
    Block,
    /// A header row above the body: a one-cell table with a named table
    /// style, or grouped shapes.
    HeaderBody,
    /// A `grid` with several cells: a table with a named table style.
    Grid,
    /// Graphic primitives: a picture, or editable shapes with
    /// `export-as: native`.
    Graphic,
}

/// A compiled render template.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Template {
    /// The element it renders.
    pub element: QualifiedName,
    /// Its name, or `None` for the element's default template.
    pub name: Option<Name>,
    /// Where it comes from and its place in source order: of two templates
    /// for one element with one name, the later origin, then the later
    /// one, wins.
    pub priority: (Origin, u32),
    /// The `render` block.
    pub span: Span,
    /// How it exports to Office.
    pub office: OfficeClass,
    header: Option<EcoVec<Item>>,
    body: Option<EcoVec<Item>>,
    /// The fields it reads, which its expansion depends on.
    reads: EcoVec<Name>,
}

impl Template {
    /// Whether it has a header part.
    pub fn has_header(&self) -> bool {
        self.header.is_some()
    }

    /// Whether it has a body part. A template without one shows the
    /// element's content as the element would without a template.
    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }
}

/// A primitive of a template, compiled.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum Item {
    /// Text: a string, or `text(..)`.
    Text(EcoString),
    /// A field's value, or the content of a content field, inline.
    Field(Name),
    /// The element's number.
    Counter,
    /// The element's reference word.
    Term,
    /// The content of the body (`slot()`), a slot or a content field, as
    /// blocks.
    Slot(Name),
    /// `strong`, `emph` or `badge`: inline items in a style.
    Inline(Inline, EcoVec<Item>),
    /// `row`, `stack` or `grid`: blocks of items. A grid's columns are the
    /// value its `columns` argument gives, `column-tracks` as a stylesheet
    /// writes them.
    Layout(Layout, EcoVec<EcoVec<Item>>, Option<Declared>, Span),
    /// `box(..)`: a block.
    Box(EcoVec<Item>),
    /// `align-start`, `align-center` or `align-end`.
    Align(&'static str, EcoVec<Item>, Span),
    /// `rule(..)`: a rule, with the border it draws if one is given.
    Rule(Option<Declared>, Span),
    /// `space(..)`: a space of a length.
    Space(Declared, Span),
    /// `image(path)`: an image of the project, with its width if given.
    Image(EcoString, Option<Declared>, Span),
    /// A graphic primitive of SPEC §9, which draws nothing yet.
    Graphic,
    /// Items shown if a condition holds, else others.
    If(Cond, EcoVec<Item>, EcoVec<Item>),
}

/// An inline primitive.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
enum Inline {
    Strong,
    Emph,
    Badge,
}

/// A layout primitive.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
enum Layout {
    Row,
    Stack,
    Grid,
}

/// A condition of a template, read per element.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum Cond {
    /// A value.
    Value(eval::Value),
    /// A field's value; `none` where it is not set.
    Field(Name),
    /// `not`.
    Not(Box<Cond>),
    /// A binary operator.
    Binary(ast::BinOp, Box<Cond>, Box<Cond>),
}

impl Cond {
    /// The condition's value for an element.
    fn eval(&self, element: &Element) -> eval::Value {
        use ast::BinOp::*;
        match self {
            Self::Value(value) => value.clone(),
            Self::Field(name) => element
                .field(name.as_str())
                .cloned()
                .unwrap_or(eval::Value::None),
            Self::Not(inner) => eval::Value::Bool(!truthy(&inner.eval(element))),
            Self::Binary(op, lhs, rhs) => {
                let bool = eval::Value::Bool;
                match op {
                    And => bool(truthy(&lhs.eval(element)) && truthy(&rhs.eval(element))),
                    Or => bool(truthy(&lhs.eval(element)) || truthy(&rhs.eval(element))),
                    Eq => bool(ops::equal(&lhs.eval(element), &rhs.eval(element))),
                    Neq => bool(!ops::equal(&lhs.eval(element), &rhs.eval(element))),
                    _ => {
                        let ord = ops::compare(&lhs.eval(element), &rhs.eval(element));
                        bool(ord.is_ok_and(|ord| match op {
                            Lt => ord.is_lt(),
                            Leq => ord.is_le(),
                            Gt => ord.is_gt(),
                            _ => ord.is_ge(),
                        }))
                    }
                }
            }
        }
    }

    /// The fields it reads.
    fn reads(&self, out: &mut EcoVec<Name>) {
        match self {
            Self::Value(_) => {}
            Self::Field(name) => {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
            Self::Not(inner) => inner.reads(out),
            Self::Binary(_, lhs, rhs) => {
                lhs.reads(out);
                rhs.reads(out);
            }
        }
    }
}

/// Whether a value holds as a condition: it is set and not `none`,
/// `false`, empty text, empty content or an empty collection.
fn truthy(value: &eval::Value) -> bool {
    match value {
        eval::Value::None => false,
        eval::Value::Bool(b) => *b,
        eval::Value::Str(s) => !s.is_empty(),
        eval::Value::Content(c) => !c.is_empty(),
        eval::Value::Array(a) => !a.is_empty(),
        eval::Value::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

/// What compiling templates needs from the cascade's compiler.
pub(super) struct Compile<'a> {
    pub(super) db: &'a dyn crate::db::Db,
    pub(super) values: &'a Values<'a>,
    /// The element's schema, which slots and fields are checked against.
    pub(super) schema: Option<&'a ElementSchema>,
    /// Whether the template applies to the build: the warning that
    /// graphic primitives are not drawn is given only then.
    pub(super) applies: bool,
}

impl Compile<'_> {
    /// Compiles a `render` block for an element. `None` after an error that
    /// leaves nothing to render, which is reported.
    pub(super) fn template(
        &self,
        render: ast::RenderBlock<'_>,
        element: QualifiedName,
        priority: (Origin, u32),
    ) -> Option<Template> {
        let span = render.to_untyped().span();
        let mut header = None;
        let mut body = None;
        let mut ok = true;
        for part in render.parts() {
            if part.to_untyped().diagnosis().errors {
                ok = false;
                continue;
            }
            let name = part.property();
            let slot = match name.get().as_str() {
                "header" if part.namespace().is_none() => &mut header,
                "body" if part.namespace().is_none() => &mut body,
                _ => {
                    self.error(
                        name.span(),
                        eco_format!("a template has no part `{}`", name.get()),
                        Some("a template's parts are `header` and `body`"),
                    );
                    ok = false;
                    continue;
                }
            };
            if slot.is_some() {
                self.error(
                    name.span(),
                    eco_format!("the part `{}` is given twice", name.get()),
                    None,
                );
                ok = false;
                continue;
            }
            let mut items = EcoVec::new();
            for expr in part.values() {
                ok &= self.push(expr, &mut items);
            }
            *slot = Some(items);
        }
        if !ok {
            return None;
        }
        if header.is_none() && body.is_none() {
            self.error(
                span,
                "this template has no parts",
                Some("write `body: ..;`"),
            );
            return None;
        }
        let mut reads = EcoVec::new();
        let mut office = if header.is_some() {
            OfficeClass::HeaderBody
        } else {
            OfficeClass::Block
        };
        for items in header.iter().chain(&body) {
            scan(items, &mut reads, &mut office);
        }
        if office == OfficeClass::Graphic && self.applies {
            diag::report(
                self.db,
                Diagnostic::warning(
                    span,
                    "graphic primitives are not drawn yet, so this template leaves them out",
                )
                .with_hint("graphics arrive in a later version (SPEC §9)"),
            );
        }
        Some(Template {
            element,
            name: render.name().and_then(|n| Name::new(n.get())),
            priority,
            span,
            office,
            header,
            body,
            reads,
        })
    }

    /// Compiles an item, and adds what it gives to `out`. `false` after an
    /// error, which is reported.
    fn push(&self, expr: ast::Expr<'_>, out: &mut EcoVec<Item>) -> bool {
        match expr {
            ast::Expr::Str(s) => out.push(Item::Text(s.get())),
            ast::Expr::Sequence(seq) => {
                let mut ok = true;
                for item in seq.items() {
                    ok &= self.push(item, out);
                }
                return ok;
            }
            ast::Expr::TemplateBlock(block) => {
                let mut ok = true;
                for item in block.items() {
                    ok &= self.push(item, out);
                }
                return ok;
            }
            ast::Expr::Guarded(guarded) => {
                let Some(cond) = self.cond(guarded.condition()) else {
                    return false;
                };
                let mut then = EcoVec::new();
                if !self.push(guarded.item(), &mut then) {
                    return false;
                }
                out.push(Item::If(cond, then, EcoVec::new()));
            }
            ast::Expr::Conditional(conditional) => {
                let Some(cond) = self.cond(conditional.condition()) else {
                    return false;
                };
                let mut then = EcoVec::new();
                let mut otherwise = EcoVec::new();
                let mut ok = self.push(conditional.if_body(), &mut then);
                if let Some(body) = conditional.else_body() {
                    ok &= self.push(body, &mut otherwise);
                }
                if !ok {
                    return false;
                }
                out.push(Item::If(cond, then, otherwise));
            }
            ast::Expr::FuncCall(call) => return self.call(call, out),
            ast::Expr::Ident(ident) => {
                let name = ident.get();
                self.error(
                    ident.span(),
                    eco_format!("`{name}` is not a template item"),
                    Some(if is_primitive(name) {
                        "call it: write `name(..)`"
                    } else {
                        "write text in quotes, or use a primitive"
                    }),
                );
                return false;
            }
            // A token or a literal: its value as text.
            other => {
                let Some(value) = self.values.single(other) else {
                    return false;
                };
                match text_of(&value) {
                    Some(text) => out.push(Item::Text(text)),
                    None => {
                        self.error(
                            other.span(),
                            "this is not text or a template primitive",
                            Some(&format!("a template is built from {PRIMITIVES}")),
                        );
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Compiles a call of a primitive.
    fn call(&self, call: ast::FuncCall<'_>, out: &mut EcoVec<Item>) -> bool {
        let ast::Expr::Ident(callee) = call.callee() else {
            self.error(
                call.callee().span(),
                "a template calls its primitives by name",
                None,
            );
            return false;
        };
        let span = call.span();
        let name = callee.get().as_str();
        let args: Vec<ast::Arg<'_>> = call.args().items().collect();
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for arg in &args {
            match *arg {
                ast::Arg::Pos(expr) => positional.push(expr),
                ast::Arg::Named(n) => named.push(n),
                ast::Arg::Spread(spread) => {
                    self.error(
                        spread.span(),
                        "a template primitive takes no spread arguments",
                        None,
                    );
                    return false;
                }
            }
        }
        let allowed: &[&str] = match name {
            "grid" => &["columns"],
            "image" => &["width"],
            _ => &[],
        };
        for n in &named {
            if !allowed.contains(&n.name().get().as_str()) {
                self.error(
                    n.name().span(),
                    eco_format!("`{name}` has no argument `{}`", n.name().get()),
                    None,
                );
                return false;
            }
        }
        let named_value = |key: &str| {
            named
                .iter()
                .find(|n| n.name().get().as_str() == key)
                .map(|n| n.expr())
        };
        // The items of every positional argument, in order.
        let all = |out: &mut EcoVec<Item>| {
            let mut ok = true;
            for &expr in &positional {
                ok &= self.push(expr, out);
            }
            ok
        };
        let item = match name {
            "row" | "stack" | "grid" => {
                let mut cells = EcoVec::new();
                for &expr in &positional {
                    let mut cell = EcoVec::new();
                    if !self.push(expr, &mut cell) {
                        return false;
                    }
                    cells.push(cell);
                }
                let columns = match named_value("columns") {
                    Some(expr) => Some(match self.value(expr) {
                        Some(value) => value,
                        None => return false,
                    }),
                    None => None,
                };
                let layout = match name {
                    "row" => Layout::Row,
                    "stack" => Layout::Stack,
                    _ => Layout::Grid,
                };
                Item::Layout(layout, cells, columns, span)
            }
            "box" | "strong" | "emph" | "badge" | "align-start" | "align-center" | "align-end" => {
                let mut items = EcoVec::new();
                if !all(&mut items) {
                    return false;
                }
                match name {
                    "box" => Item::Box(items),
                    "strong" => Item::Inline(Inline::Strong, items),
                    "emph" => Item::Inline(Inline::Emph, items),
                    "badge" => Item::Inline(Inline::Badge, items),
                    "align-start" => Item::Align("start", items, span),
                    "align-center" => Item::Align("center", items, span),
                    _ => Item::Align("end", items, span),
                }
            }
            "slot" => {
                let slot = match positional[..] {
                    [] => Name::new(BODY).expect("an identifier"),
                    [expr] => match self.ident(expr, "slot") {
                        Some(name) => name,
                        None => return false,
                    },
                    _ => {
                        return self
                            .arity(span, "`slot` takes one slot name, or none for the body");
                    }
                };
                if !self.check_slot(&slot, span) {
                    return false;
                }
                Item::Slot(slot)
            }
            "field" => {
                let [expr] = positional[..] else {
                    return self.arity(span, "`field` takes one field name");
                };
                let Some(field) = self.ident(expr, "field") else {
                    return false;
                };
                if !self.check_field(&field, expr.span()) {
                    return false;
                }
                Item::Field(field)
            }
            "counter" | "term" => {
                if !positional.is_empty() {
                    return self.arity(span, &format!("`{name}` takes no arguments"));
                }
                if name == "counter" {
                    Item::Counter
                } else {
                    Item::Term
                }
            }
            "text" => {
                let [expr] = positional[..] else {
                    return self.arity(span, "`text` takes one string");
                };
                let Some(value) = self.values.single(expr) else {
                    return false;
                };
                match text_of(&value) {
                    Some(text) => Item::Text(text),
                    None => {
                        self.error(expr.span(), "`text` takes a string", None);
                        return false;
                    }
                }
            }
            "rule" => {
                let stroke = match positional[..] {
                    [] => None,
                    [expr] => match self.value(expr) {
                        Some(value) => Some(value),
                        None => return false,
                    },
                    _ => {
                        return self.arity(
                            span,
                            "`rule` takes one border, such as `rule(0.4pt)` or `rule(1pt dashed $accent)`",
                        );
                    }
                };
                Item::Rule(stroke, span)
            }
            "space" => {
                let [expr] = positional[..] else {
                    return self.arity(span, "`space` takes one length");
                };
                let Some(value) = self.value(expr) else {
                    return false;
                };
                Item::Space(value, span)
            }
            "image" => {
                let [expr] = positional[..] else {
                    return self.arity(span, "`image` takes the path of an image of the project");
                };
                let Some(Declared::Value(eval::Value::Str(path))) = self.values.single(expr) else {
                    self.error(expr.span(), "`image` takes a path in quotes", None);
                    return false;
                };
                let width = match named_value("width") {
                    Some(expr) => Some(match self.value(expr) {
                        Some(value) => value,
                        None => return false,
                    }),
                    None => None,
                };
                Item::Image(path, width, span)
            }
            graphic if GRAPHICS.contains(&graphic) => Item::Graphic,
            _ => {
                self.error(
                    callee.span(),
                    eco_format!("`{name}` is not a template primitive"),
                    Some(&format!("a template is built from {PRIMITIVES}")),
                );
                return false;
            }
        };
        out.push(item);
        true
    }

    /// The value of an argument written as a stylesheet value: one item,
    /// or several written one after another (`1pt dashed $accent`).
    fn value(&self, expr: ast::Expr<'_>) -> Option<Declared> {
        match expr {
            ast::Expr::Sequence(seq) => {
                let items: Option<EcoVec<Declared>> =
                    seq.items().map(|item| self.values.single(item)).collect();
                items.map(Declared::List)
            }
            _ => self.values.single(expr),
        }
    }

    /// The name an argument gives: a bare word.
    fn ident(&self, expr: ast::Expr<'_>, primitive: &str) -> Option<Name> {
        match expr {
            ast::Expr::Ident(ident) => Name::new(ident.get()),
            _ => {
                self.error(
                    expr.span(),
                    eco_format!("`{primitive}` takes a name, without quotes"),
                    None,
                );
                None
            }
        }
    }

    /// Checks that the element has a slot, a content field or a body of
    /// this name.
    fn check_slot(&self, name: &Name, span: Span) -> bool {
        let Some(schema) = self.schema else {
            return true;
        };
        let found = if name.as_str() == BODY {
            !matches!(schema.body, Body::None | Body::Slots(_))
        } else {
            matches!(&schema.body, Body::Slots(slots) if slots.iter().any(|s| &s.name == name))
                || schema
                    .fields
                    .iter()
                    .any(|f| &f.name == name && f.ty == FieldType::Content)
        };
        if !found {
            let what = if name.as_str() == BODY {
                eco_format!("`{}` has no body of its own", schema.name)
            } else {
                eco_format!("`{}` has no slot or content field `{name}`", schema.name)
            };
            let slots: Vec<String> = match &schema.body {
                Body::Slots(slots) => slots.iter().map(|s| format!("`{}`", s.name)).collect(),
                _ => Vec::new(),
            };
            let hint = (!slots.is_empty()).then(|| format!("its slots are {}", slots.join(", ")));
            self.error(span, what, hint.as_deref());
        }
        found
    }

    /// Checks that the element has a field of this name.
    fn check_field(&self, name: &Name, span: Span) -> bool {
        let Some(schema) = self.schema else {
            return true;
        };
        if schema.fields.iter().any(|f| &f.name == name)
            || matches!(&schema.body, Body::Slots(slots) if slots.iter().any(|s| &s.name == name))
        {
            return true;
        }
        self.error(
            span,
            eco_format!("`{}` has no field `{name}`", schema.name),
            None,
        );
        false
    }

    /// Compiles a condition.
    fn cond(&self, expr: ast::Expr<'_>) -> Option<Cond> {
        Some(match expr {
            ast::Expr::Parenthesized(p) => return self.cond(p.expr()),
            ast::Expr::FuncCall(call) if matches!(call.callee(), ast::Expr::Ident(i) if i.get() == "field") =>
            {
                let args: Vec<_> = call.args().items().collect();
                let [ast::Arg::Pos(expr)] = args[..] else {
                    self.arity(call.span(), "`field` takes one field name");
                    return None;
                };
                let name = self.ident(expr, "field")?;
                if !self.check_field(&name, expr.span()) {
                    return None;
                }
                Cond::Field(name)
            }
            ast::Expr::FuncCall(call) if matches!(call.callee(), ast::Expr::Ident(i) if is_primitive(i.get())) =>
            {
                self.error(
                    call.span(),
                    "a template's condition reads fields only",
                    Some("test a field: `if field(title) { .. }`"),
                );
                return None;
            }
            ast::Expr::Unary(unary) if unary.op() == ast::UnOp::Not => {
                Cond::Not(Box::new(self.cond(unary.expr())?))
            }
            ast::Expr::Binary(binary) => {
                use ast::BinOp::*;
                let op = binary.op();
                if !matches!(op, And | Or | Eq | Neq | Lt | Leq | Gt | Geq) {
                    // Arithmetic on literals only.
                    return self.literal(expr);
                }
                let lhs = self.cond(binary.lhs())?;
                let rhs = self.cond(binary.rhs())?;
                Cond::Binary(op, Box::new(lhs), Box::new(rhs))
            }
            // `field(difficulty) == extension` compares with a string.
            ast::Expr::Ident(ident) => Cond::Value(eval::Value::Str(ident.get().clone())),
            _ => return self.literal(expr),
        })
    }

    /// A literal of a condition, evaluated once.
    fn literal(&self, expr: ast::Expr<'_>) -> Option<Cond> {
        match self.values.single(expr)? {
            Declared::Value(value) => Some(Cond::Value(value)),
            Declared::Keyword(word) => Some(Cond::Value(eval::Value::Str(word.as_str().into()))),
            _ => {
                self.error(expr.span(), "a condition compares single values", None);
                None
            }
        }
    }

    fn arity(&self, span: Span, message: &str) -> bool {
        self.error(span, message, None);
        false
    }

    fn error(&self, span: Span, message: impl Into<EcoString>, hint: Option<&str>) {
        let mut diagnostic = Diagnostic::error(span, message);
        if let Some(hint) = hint {
            diagnostic = diagnostic.with_hint(hint);
        }
        diag::report(self.db, diagnostic);
    }
}

/// Whether a name is a primitive of templates.
fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "row"
            | "stack"
            | "grid"
            | "box"
            | "rule"
            | "slot"
            | "field"
            | "counter"
            | "term"
            | "text"
            | "strong"
            | "emph"
            | "badge"
            | "image"
            | "space"
            | "align-start"
            | "align-center"
            | "align-end"
    ) || GRAPHICS.contains(&name)
}

/// A literal value as the text it shows, if it is text or a number.
fn text_of(value: &Declared) -> Option<EcoString> {
    match value {
        Declared::Value(eval::Value::Str(s)) => Some(s.clone()),
        Declared::Value(v @ (eval::Value::Int(_) | eval::Value::Float(_))) => Some(v.repr()),
        _ => None,
    }
}

/// Collects the fields items read, and raises the Office class to what
/// they use.
fn scan(items: &[Item], reads: &mut EcoVec<Name>, office: &mut OfficeClass) {
    for item in items {
        match item {
            Item::Field(name) => {
                if !reads.contains(name) {
                    reads.push(name.clone());
                }
            }
            Item::Inline(_, inner) | Item::Box(inner) | Item::Align(_, inner, _) => {
                scan(inner, reads, office);
            }
            Item::Layout(layout, cells, _, _) => {
                if *layout == Layout::Grid && cells.len() > 1 {
                    *office = (*office).max(OfficeClass::Grid);
                }
                for cell in cells {
                    scan(cell, reads, office);
                }
            }
            Item::Graphic => *office = OfficeClass::Graphic,
            Item::If(cond, then, otherwise) => {
                cond.reads(reads);
                scan(then, reads, office);
                scan(otherwise, reads, office);
            }
            Item::Text(_)
            | Item::Counter
            | Item::Term
            | Item::Slot(_)
            | Item::Rule(..)
            | Item::Space(..)
            | Item::Image(..) => {}
        }
    }
}

/// A template expanded for an element: what the text stage collects in
/// place of the element's content (SPEC §5.3).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Expansion {
    /// The template's name, or `None` for the element's default template.
    pub name: Option<Name>,
    /// The template's `render` block.
    pub span: Span,
    /// How the template exports to Office (SPEC §5.3), which the DOCX and
    /// PPTX writers read from the styled tree.
    pub office: OfficeClass,
    /// The header: a block of the part [`HEADER_PART`] before the body.
    pub header: Option<Arranged>,
    /// The body, in the element's own block; `None` where the template has
    /// no body, so the element's content is shown as it would be without
    /// one.
    pub body: Option<EcoVec<Piece>>,
}

/// A block of pieces in a style.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Arranged {
    /// The block's computed style.
    pub style: ComputedStyle,
    /// What it holds, in order.
    pub items: EcoVec<Piece>,
}

/// A piece of an expanded template. Text, numbers, inline fields and
/// images run together into paragraphs; the other pieces are blocks.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Piece {
    /// Text in a style.
    Text {
        /// The text.
        text: EcoString,
        /// Its style.
        style: ComputedStyle,
    },
    /// The element's number, which the Resolve stage gives.
    Counter {
        /// Its style.
        style: ComputedStyle,
    },
    /// The content of a content field, inline, in a style.
    Field {
        /// The field.
        name: Name,
        /// The style of its text.
        style: ComputedStyle,
    },
    /// The content of the body ([`BODY`]), a slot or a content field, as
    /// blocks, in the style of its part; in a line, as an inline element's
    /// template places it, in the style of the primitives around it.
    Slot {
        /// The slot.
        name: Name,
        /// The style of the primitives around it.
        style: ComputedStyle,
    },
    /// An image of the project.
    Image {
        /// Its path in the project.
        path: EcoString,
        /// Its width: `auto`, a length, or a ratio of its lines.
        width: folio_component::Value,
        /// Its style.
        style: ComputedStyle,
        /// The primitive.
        span: Span,
    },
    /// A block.
    Block(Block),
}

/// A block a template makes.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Block {
    /// What kind of block.
    pub kind: BlockKind,
    /// Its computed style.
    pub style: ComputedStyle,
    /// What it holds.
    pub children: EcoVec<Piece>,
}

/// What a block of a template is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum BlockKind {
    /// A row: a grid of one row, whose blocks are its cells.
    Row,
    /// A grid, whose blocks are its cells.
    Grid,
    /// A cell of a row or grid.
    Cell,
    /// A block of a stack, or a box.
    Box,
    /// A rule.
    Rule,
    /// An empty block of a height.
    Space,
}

impl BlockKind {
    /// The part name the text stage gives the block.
    pub fn part(self) -> &'static str {
        match self {
            Self::Row => "row",
            Self::Grid => "grid",
            Self::Cell => "cell",
            Self::Box => "box",
            Self::Rule => "rule",
            Self::Space => "space",
        }
    }

    /// Whether the block is a grid, whose blocks are its cells.
    pub fn is_grid(self) -> bool {
        matches!(self, Self::Row | Self::Grid)
    }
}

/// What expanding a template needs of the style stage: the styles of the
/// nodes it makes.
pub(super) trait Restyle {
    /// Where a node is matched: the views of its ancestors.
    type Cx: Copy;

    /// The style of a node the template makes, named `name`, under `parent`
    /// in context `cx`, with `extra` declarations of the primitive above
    /// every rule; and the context of its own children.
    fn restyle(
        &mut self,
        name: NodeName,
        cx: Self::Cx,
        parent: &ComputedStyle,
        extra: EcoVec<Decl>,
    ) -> (ComputedStyle, Self::Cx);

    /// The style of a node no rule matches, such as a cell of a row, under
    /// `parent` with `extra` declarations.
    fn plain(&mut self, parent: &ComputedStyle, extra: EcoVec<Decl>) -> ComputedStyle;

    /// The reference word of an element in the language of a style.
    fn term(&mut self, element: &QualifiedName, style: &ComputedStyle) -> Option<EcoString>;

    /// The properties.
    fn props(&self) -> &Properties;
}

/// The fields of an element that a template reads, with their values: a
/// key of the template's expansion.
pub(super) fn read_fields(template: &Template, element: &Element) -> EcoVec<eval::Value> {
    template
        .reads
        .iter()
        .map(|name| {
            element
                .field(name.as_str())
                .cloned()
                .unwrap_or(eval::Value::None)
        })
        .collect()
}

/// Expands a template for an element whose style is `style` and whose
/// context, as its parts see it, is `cx`.
pub(super) fn expand<R: Restyle>(
    restyle: &mut R,
    template: &Template,
    element: &Element,
    style: &ComputedStyle,
    cx: R::Cx,
) -> Expansion {
    let mut ex = Expander {
        restyle,
        element,
        name: &element.name,
    };
    let header = template.header.as_ref().map(|items| {
        let part = NodeName::Part(Name::new(HEADER_PART).expect("an identifier"));
        let (style, cx) = ex.restyle.restyle(part, cx, style, EcoVec::new());
        let at = At {
            cx,
            block: style.clone(),
            inline: style.clone(),
        };
        Arranged {
            items: ex.items(items, &at),
            style,
        }
    });
    let body = template.body.as_ref().map(|items| {
        let at = At {
            cx,
            block: style.clone(),
            inline: style.clone(),
        };
        ex.items(items, &at)
    });
    trim(Expansion {
        name: template.name.clone(),
        span: template.span,
        office: template.office,
        header,
        body,
    })
}

/// An expansion whose header holds nothing, as when its conditions all
/// fail, has no header block.
fn trim(mut expansion: Expansion) -> Expansion {
    if expansion
        .header
        .as_ref()
        .is_some_and(|h| h.items.is_empty())
    {
        expansion.header = None;
    }
    expansion
}

struct Expander<'r, 'e, R: Restyle> {
    restyle: &'r mut R,
    element: &'e Element,
    name: &'e QualifiedName,
}

/// Where items are expanded: the context of their nodes, the style of the
/// block they are in, and the style of their text.
struct At<C> {
    cx: C,
    block: ComputedStyle,
    inline: ComputedStyle,
}

impl<C: Copy> At<C> {
    fn with(&self, cx: C, style: &ComputedStyle) -> Self {
        Self {
            cx,
            block: style.clone(),
            inline: style.clone(),
        }
    }
}

impl<R: Restyle> Expander<'_, '_, R> {
    fn items(&mut self, items: &[Item], at: &At<R::Cx>) -> EcoVec<Piece> {
        let mut out = EcoVec::new();
        for item in items {
            self.item(item, at, &mut out);
        }
        out
    }

    fn item(&mut self, item: &Item, at: &At<R::Cx>, out: &mut EcoVec<Piece>) {
        match item {
            Item::Text(text) => out.push(Piece::Text {
                text: text.clone(),
                style: at.inline.clone(),
            }),
            Item::Field(name) => match self.element.field(name.as_str()) {
                None | Some(eval::Value::None) => {}
                Some(eval::Value::Content(content)) if content.is_empty() => {}
                Some(eval::Value::Content(_)) => out.push(Piece::Field {
                    name: name.clone(),
                    style: at.inline.clone(),
                }),
                Some(value) => {
                    let text = match value {
                        eval::Value::Str(s) => s.clone(),
                        other => other.repr(),
                    };
                    out.push(Piece::Text {
                        text,
                        style: at.inline.clone(),
                    });
                }
            },
            Item::Counter => out.push(Piece::Counter {
                style: at.inline.clone(),
            }),
            Item::Term => {
                if let Some(text) = self.restyle.term(self.name, &at.inline) {
                    out.push(Piece::Text {
                        text,
                        style: at.inline.clone(),
                    });
                }
            }
            Item::Slot(name) => out.push(Piece::Slot {
                name: name.clone(),
                style: at.inline.clone(),
            }),
            Item::Inline(kind, items) => {
                let name = match kind {
                    Inline::Strong => element_name("strong"),
                    Inline::Emph => element_name("emph"),
                    Inline::Badge => part_name("badge"),
                };
                let (style, cx) = self.restyle.restyle(name, at.cx, &at.inline, EcoVec::new());
                let inner = At {
                    cx,
                    block: at.block.clone(),
                    inline: style,
                };
                for item in items.iter() {
                    self.item(item, &inner, out);
                }
            }
            Item::Layout(layout, cells, columns, span) => {
                out.push(Piece::Block(
                    self.layout(*layout, cells, columns, *span, at),
                ));
            }
            Item::Box(items) => {
                let (style, cx) =
                    self.restyle
                        .restyle(part_name("box"), at.cx, &at.block, EcoVec::new());
                let children = self.items(items, &at.with(cx, &style));
                out.push(Piece::Block(Block {
                    kind: BlockKind::Box,
                    style,
                    children,
                }));
            }
            Item::Align(align, items, span) => {
                let extra = self.decls(&[("align", Declared::Keyword(name(align)))], *span);
                let style = self.restyle.plain(&at.block, extra);
                let children = self.items(items, &at.with(at.cx, &style));
                out.push(Piece::Block(Block {
                    kind: BlockKind::Box,
                    style,
                    children,
                }));
            }
            Item::Rule(stroke, span) => {
                let extra = match stroke {
                    Some(stroke) => self.decls(&[("border-top", stroke.clone())], *span),
                    None => EcoVec::new(),
                };
                let (style, _) = self
                    .restyle
                    .restyle(part_name("rule"), at.cx, &at.block, extra);
                out.push(Piece::Block(Block {
                    kind: BlockKind::Rule,
                    style,
                    children: EcoVec::new(),
                }));
            }
            Item::Space(length, span) => {
                let extra = self.decls(&[("min-height", length.clone())], *span);
                let style = self.restyle.plain(&at.block, extra);
                out.push(Piece::Block(Block {
                    kind: BlockKind::Space,
                    style,
                    children: EcoVec::new(),
                }));
            }
            Item::Image(path, width, span) => {
                let width = width
                    .as_ref()
                    .and_then(|w| {
                        let size = self.font_size(&at.inline);
                        super::value::compute(w, size).ok()
                    })
                    .unwrap_or(folio_component::Value::Auto);
                out.push(Piece::Image {
                    path: path.clone(),
                    width,
                    style: at.inline.clone(),
                    span: *span,
                });
            }
            Item::Graphic => {}
            Item::If(cond, then, otherwise) => {
                let shown = if truthy(&cond.eval(self.element)) {
                    then
                } else {
                    otherwise
                };
                for item in shown.iter() {
                    self.item(item, at, out);
                }
            }
        }
    }

    /// A row, a stack or a grid.
    fn layout(
        &mut self,
        layout: Layout,
        cells: &[EcoVec<Item>],
        columns: &Option<Declared>,
        span: Span,
        at: &At<R::Cx>,
    ) -> Block {
        // The cells that show something; a cell whose condition fails is
        // left out, with its column.
        let shown: Vec<Vec<&Item>> = cells
            .iter()
            .map(|cell| {
                let mut flat = Vec::new();
                flatten(cell, self.element, &mut flat);
                flat
            })
            .filter(|cell| !cell.is_empty())
            .collect();
        let (name, kind) = match layout {
            Layout::Row => (part_name("row"), BlockKind::Row),
            Layout::Stack => (part_name("stack"), BlockKind::Box),
            Layout::Grid => (element_name("grid"), BlockKind::Grid),
        };
        let extra = match layout {
            // A cell that is aligned is as wide as what it holds, and the
            // others share what is left: `row(a, align-end(b))` sets `b`
            // at the end of the line.
            Layout::Row => {
                let tracks = shown
                    .iter()
                    .map(|cell| match cell[..] {
                        [Item::Align(..)] => Declared::Value(eval::Value::Auto),
                        [Item::Space(length, _)] => length.clone(),
                        _ => Declared::Value(eval::Value::Fraction(eval::Fraction(
                            folio_component::Scalar::new(1.0),
                        ))),
                    })
                    .collect();
                self.decls(&[("column-tracks", Declared::List(tracks))], span)
            }
            Layout::Grid => match columns {
                Some(columns) => self.decls(&[("column-tracks", columns.clone())], span),
                None => EcoVec::new(),
            },
            Layout::Stack => EcoVec::new(),
        };
        let (style, cx) = self.restyle.restyle(name, at.cx, &at.block, extra);
        let children = shown
            .iter()
            .map(|cell| match layout {
                Layout::Row => self.row_cell(cell, cx, &style),
                Layout::Grid => {
                    let (cell_style, cell_cx) =
                        self.restyle
                            .restyle(element_name("cell"), cx, &style, EcoVec::new());
                    let mut children = EcoVec::new();
                    let inner = At {
                        cx: cell_cx,
                        block: cell_style.clone(),
                        inline: cell_style.clone(),
                    };
                    for item in cell {
                        self.item(item, &inner, &mut children);
                    }
                    Piece::Block(Block {
                        kind: BlockKind::Cell,
                        style: cell_style,
                        children,
                    })
                }
                Layout::Stack => {
                    let item_style = self.restyle.plain(&style, EcoVec::new());
                    let mut children = EcoVec::new();
                    let inner = At {
                        cx,
                        block: item_style.clone(),
                        inline: item_style.clone(),
                    };
                    for item in cell {
                        self.item(item, &inner, &mut children);
                    }
                    Piece::Block(Block {
                        kind: BlockKind::Box,
                        style: item_style,
                        children,
                    })
                }
            })
            .collect();
        Block {
            kind,
            style,
            children,
        }
    }

    /// A cell of a row. A cell that is aligned takes the alignment, and one
    /// that holds a badge alone is the badge's box, so that the badge's
    /// padding, border and background frame its text.
    fn row_cell(&mut self, cell: &[&Item], cx: R::Cx, row: &ComputedStyle) -> Piece {
        let (align, inner): (Option<(&str, Span)>, Vec<&Item>) = match cell {
            [Item::Align(align, items, span)] => {
                let mut flat = Vec::new();
                flatten(items, self.element, &mut flat);
                (Some((*align, *span)), flat)
            }
            _ => (None, cell.to_vec()),
        };
        let extra = match align {
            Some((align, span)) => self.decls(&[("align", Declared::Keyword(name(align)))], span),
            None => EcoVec::new(),
        };
        if let [Item::Inline(Inline::Badge, items)] = inner[..] {
            let (style, badge_cx) = self.restyle.restyle(part_name("badge"), cx, row, extra);
            let at = At {
                cx: badge_cx,
                block: style.clone(),
                inline: style.clone(),
            };
            let children = self.items(items, &at);
            return Piece::Block(Block {
                kind: BlockKind::Cell,
                style,
                children,
            });
        }
        let style = self.restyle.plain(row, extra);
        let at = At {
            cx,
            block: style.clone(),
            inline: style.clone(),
        };
        let mut children = EcoVec::new();
        for item in inner {
            self.item(item, &at, &mut children);
        }
        Piece::Block(Block {
            kind: BlockKind::Cell,
            style,
            children,
        })
    }

    /// Declarations of built-in properties, made by a primitive at `span`.
    fn decls(&self, values: &[(&str, Declared)], span: Span) -> EcoVec<Decl> {
        let props = self.restyle.props();
        values
            .iter()
            .map(|(name, value)| Decl {
                prop: props.builtin(name),
                value: DeclValue::Value(value.clone()),
                span,
            })
            .collect()
    }

    fn font_size(&self, style: &ComputedStyle) -> folio_component::Abs {
        let props = self.restyle.props();
        match style.value(props, props.builtin("font-size")) {
            Some(folio_component::Value::Length(size)) => *size,
            _ => folio_component::Abs::pt(11.0),
        }
    }
}

/// The items of a cell with its conditions decided for an element.
fn flatten<'a>(items: &'a [Item], element: &Element, out: &mut Vec<&'a Item>) {
    for item in items {
        match item {
            Item::If(cond, then, otherwise) => {
                let shown = if truthy(&cond.eval(element)) {
                    then
                } else {
                    otherwise
                };
                flatten(shown, element, out);
            }
            Item::Graphic => {}
            other => out.push(other),
        }
    }
}

fn element_name(name: &str) -> NodeName {
    NodeName::Element(QualifiedName::unqualified(
        Name::new(name).expect("an identifier"),
    ))
}

fn part_name(name: &str) -> NodeName {
    NodeName::Part(Name::new(name).expect("an identifier"))
}

fn name(text: &str) -> Name {
    Name::new(text).expect("an identifier")
}

/// The template of an element, among those that render it, by its
/// computed `template`: the default template for `auto`, the named
/// template for a name, and none for `none`. Of several with one name,
/// the one of the highest priority.
pub(super) fn choose<'t>(
    templates: &'t [Arc<Template>],
    element: &QualifiedName,
    name: Option<&str>,
) -> Option<&'t Arc<Template>> {
    templates
        .iter()
        .filter(|t| &t.element == element && t.name.as_ref().map(|n| n.as_str()) == name)
        .max_by_key(|t| t.priority)
}
