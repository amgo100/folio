//! Element declarations (SPEC §4.3, decision D-58-1): the elements a project
//! declares in its own content files with `#element`.
//!
//! Declarations are fixed per project. Each sits at the top level of a
//! content file, with literal types and defaults, so the compiler collects
//! them from the files the entry file imports and includes before any
//! script runs ([`project_elements`]). They are hosted like a package
//! component's elements, in the namespace [`PROJECT`]
//! ([`project_component`], [`crate::component::ComponentHost::with_project`]),
//! so stylesheets, counters, references, lints and templates treat them as
//! they treat a component's. In content, a declared element is a variable
//! like any other: it is bound where it is declared, and other files
//! `#import` it.
//!
//! ```text
//! #element example(
//!   title: content = none,
//!   difficulty: choice("core", "extension") = "core",
//! ) {
//!   kind: block
//!   body: slots(statement: blocks, solution: blocks)
//!   counter: within heading(level: 1)
//!   ref-key: "example"
//!   words: (en: "Example", pl: "przykł.")
//! }
//! ```

use std::collections::{BTreeSet, VecDeque};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{
    BODY, Body, Component, ComponentManifest, ComponentName, ContentKind, CounterDecl,
    Declarations, ElementKind, ElementSchema, FieldDecl, FieldType, LocaleString, Name,
    QualifiedName, Registered, Registry, Target, UNIVERSAL_FIELDS, Value as Constant, Within,
};
use folio_syntax::Span;
use folio_syntax::ast::{self, AstNode};
use folio_text::Lang;

use super::value::{Length, Value};
use super::vm::numeric;
use crate::db::{Db, Project, SourceFile, dependencies, source_file};
use crate::diag::{self, Diagnostic};
use crate::locale;
use crate::model::builtin;
use crate::project::ProjectPath;

/// The namespace of the elements a project declares: `example` is
/// `project.example` when qualified. It is also the name of the component
/// that hosts them.
pub const PROJECT: &str = "project";

/// Whether a hosted component is the one that holds the project's own
/// elements.
pub fn is_project(component: &Registered) -> bool {
    component.name().as_str() == PROJECT
}

/// An element declaration of a file, as collected before evaluation.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Declared {
    /// The declaration's span.
    pub span: Span,
    /// The declared name and its span.
    pub name: (Name, Span),
    /// What the declaration declares, or `None` when it has errors, which
    /// were reported.
    pub schema: Option<Schema>,
}

/// A declared element: its schema and the words of its reference key.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Schema {
    /// The element's schema, with names in `within` and `scoped`
    /// unqualified when they name a project element or a built-in one.
    pub schema: ElementSchema,
    /// The words of `words: (..)`, or the core's English word when the
    /// reference key is one of the core's and no words are given.
    pub words: EcoVec<LocaleString>,
    /// The unqualified element names in `within` and `scoped`, with their
    /// spans: each must be a built-in element or a project element.
    pub names: EcoVec<(Name, Span)>,
}

/// The elements a project declares, checked across its files.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct ProjectElements {
    /// The declarations that declare an element, by file and span.
    pub accepted: EcoVec<(ProjectPath, Span, Name)>,
    /// The schemas and words of the accepted declarations, in the order
    /// their files were found.
    pub declarations: Declarations,
}

impl ProjectElements {
    /// Whether the declaration at a span of a file declares an element.
    pub fn accepts(&self, file: &ProjectPath, span: Span) -> bool {
        self.accepted
            .iter()
            .any(|(path, s, _)| path == file && *s == span)
    }

    /// The file that declares an element.
    pub fn file_of(&self, name: &str) -> Option<&ProjectPath> {
        self.accepted
            .iter()
            .find(|(_, _, n)| n.as_str() == name)
            .map(|(path, ..)| path)
    }
}

/// The element declarations at the top level of a content file. Reports
/// the errors in each.
///
/// A declaration's span and those of its parts stay the same across an
/// edit outside it, so an edit elsewhere in the file gives an equal result.
#[salsa::tracked(returns(ref))]
pub fn file_elements(db: &dyn Db, file: SourceFile) -> EcoVec<Declared> {
    let source = file.source(db);
    let Some(markup) = source.root().cast::<ast::Markup<'_>>() else {
        return EcoVec::new();
    };
    let mut out = EcoVec::new();
    for expr in markup.exprs() {
        let ast::Expr::ElementDecl(decl) = expr else {
            continue;
        };
        // A declaration that does not parse is reported by `check_source`.
        if decl.to_untyped().diagnosis().errors {
            continue;
        }
        let mut cx = Checker::default();
        let schema = cx.declaration(decl);
        for error in cx.errors {
            diag::report(db, error);
        }
        let name = decl.name();
        let Some(declared) = Name::new(name.get()) else {
            continue;
        };
        out.push(Declared {
            span: decl.span(),
            name: (declared, name.span()),
            schema,
        });
    }
    out
}

/// The elements the project declares: those of the content files that the
/// entry file imports and includes, directly or through others, in the
/// order the files are found. Reports a name declared twice, the second
/// time, and a name in `within` or `scoped` that is neither a built-in
/// element nor a declared one.
#[salsa::tracked(returns(ref))]
pub fn project_elements(db: &dyn Db, project: Project) -> ProjectElements {
    let entry = project.manifest(db).project.entry.v.clone();
    let mut queue: VecDeque<ProjectPath> = VecDeque::from([entry]);
    let mut seen = BTreeSet::new();
    let mut declared = Vec::new();
    while let Some(path) = queue.pop_front() {
        if !seen.insert(path.clone()) || path.extension() != Some("fol") {
            continue;
        }
        // A missing file is reported by `build_files`.
        let Some(file) = source_file(db, project, path.clone()) else {
            continue;
        };
        for decl in file_elements(db, file) {
            declared.push((path.clone(), decl));
        }
        for dep in dependencies(db, file) {
            queue.push_back(dep.path.clone());
        }
    }

    let mut out = ProjectElements::default();
    let mut elements = Vec::new();
    let mut words: Vec<LocaleString> = Vec::new();
    let mut checks = Vec::new();
    for (path, decl) in declared {
        let (name, name_span) = &decl.name;
        if let Some((first, ..)) = out.accepted.iter().find(|(_, _, n)| n == name) {
            diag::report(
                db,
                Diagnostic::error(
                    *name_span,
                    eco_format!("element `{name}` is already declared"),
                )
                .with_hint(eco_format!(
                    "it is declared in `{}`; rename one of them",
                    first.as_str()
                )),
            );
            continue;
        }
        let Some(schema) = decl.schema.clone() else {
            continue;
        };
        for word in schema.words {
            match words
                .iter()
                .find(|w| w.key == word.key && w.lang.eq_ignore_ascii_case(&word.lang))
            {
                Some(other) if other.text != word.text => diag::report(
                    db,
                    Diagnostic::error(
                        *name_span,
                        eco_format!(
                            "the word for `{}` in `{}` is already declared as {:?}",
                            word.key,
                            word.lang,
                            other.text
                        ),
                    )
                    .with_hint("elements that share a reference key share its words"),
                ),
                Some(_) => {}
                None => words.push(word),
            }
        }
        checks.extend(schema.names);
        out.accepted.push((path, decl.span, name.clone()));
        elements.push(schema.schema);
    }
    for (name, span) in checks {
        let known = builtin(name.as_str()).is_some() || elements.iter().any(|e| e.name == name);
        if !known {
            diag::report(
                db,
                Diagnostic::error(span, eco_format!("unknown element `{name}`")).with_hint(
                    "a counter names built-in elements or elements this project declares",
                ),
            );
        }
    }
    out.declarations.elements = elements.into_iter().collect();
    out.declarations.locale_strings = words.into_iter().collect();
    out
}

/// The component that hosts the project's own elements, or `None` when the
/// project declares none.
#[salsa::tracked(returns(ref))]
pub fn project_component(db: &dyn Db, project: Project) -> Option<Registered> {
    let elements = project_elements(db, project);
    if elements.declarations.elements.is_empty() {
        return None;
    }
    let component = ProjectComponent(elements.declarations.clone());
    let mut registry = Registry::new();
    if let Err(errors) = registry.register(component) {
        // The checks of [`file_elements`] and [`project_elements`] find
        // everything the registry checks, at the declarations.
        let span = elements.accepted.first().map_or(Span::detached(), |a| a.1);
        for error in errors {
            diag::report(db, Diagnostic::error(span, error.message));
        }
        return None;
    }
    registry.components().next().cloned()
}

/// The component that hosts a project's elements: declarations only, so
/// it has no lints.
struct ProjectComponent(Declarations);

impl Component for ProjectComponent {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new(PROJECT).expect("an identifier"),
            semver::Version::new(0, 0, 0),
            [Target::Pdf, Target::Docx, Target::Pptx, Target::Live],
        )
    }

    fn declarations(&self) -> Declarations {
        self.0.clone()
    }
}

/// Checks one declaration, collecting its errors.
#[derive(Default)]
struct Checker {
    errors: Vec<Diagnostic>,
}

impl Checker {
    fn error(&mut self, span: Span, message: impl Into<EcoString>) {
        self.errors.push(Diagnostic::error(span, message));
    }

    fn error_hint(&mut self, span: Span, message: impl Into<EcoString>, hint: &str) {
        self.errors
            .push(Diagnostic::error(span, message).with_hint(hint));
    }

    /// The schema a declaration declares, or `None` after an error.
    fn declaration(&mut self, decl: ast::ElementDecl<'_>) -> Option<Schema> {
        let ident = decl.name();
        let name = Name::new(ident.get())?;
        if builtin(name.as_str()).is_some() {
            self.error_hint(
                ident.span(),
                eco_format!("`{name}` is a built-in element"),
                "give the element another name",
            );
        }

        let mut fields: Vec<FieldDecl> = Vec::new();
        for field in decl.fields() {
            if let Some(field) = self.field(field) {
                fields.push(field);
            }
        }

        let mut seen: Vec<EcoString> = Vec::new();
        let mut kind = None;
        let mut body = None;
        let mut counter = None;
        let mut ref_key = None;
        let mut words = None;
        let mut names = EcoVec::new();
        for entry in decl.entries() {
            let key = entry.key();
            let values: Vec<ast::Expr<'_>> = entry.values().collect();
            let Some(&first) = values.first() else {
                // The parser reports the missing value.
                continue;
            };
            if seen.contains(key.get()) {
                self.error(key.span(), eco_format!("duplicate entry `{}`", key.get()));
                continue;
            }
            seen.push(key.get().clone());
            let single = |cx: &mut Self| {
                if let Some(extra) = values.get(1) {
                    cx.error(extra.span(), "expected the end of the entry");
                    return None;
                }
                Some(first)
            };
            match key.get().as_str() {
                "kind" => kind = single(self).and_then(|v| self.kind(v)),
                "body" => body = single(self).and_then(|v| self.body(v)),
                "counter" => counter = self.counter(&values, &mut names),
                "ref-key" => ref_key = single(self).and_then(|v| self.ref_key(v)),
                "words" => words = single(self).and_then(|v| self.words(v)),
                other => self.error_hint(
                    key.span(),
                    eco_format!("unknown entry `{other}`"),
                    "an element declaration has `kind`, `body`, `counter`, `ref-key` and `words`",
                ),
            }
        }

        let kind = kind.unwrap_or(ElementKind::Block);
        // Without a `body` entry, an inline element takes inline content
        // and the others take blocks.
        let body = body.unwrap_or(match kind {
            ElementKind::Inline | ElementKind::Note => Body::Content(ContentKind::Inline),
            _ => Body::Content(ContentKind::Blocks),
        });
        if let Body::Slots(slots) = &body {
            for slot in slots {
                if fields.iter().any(|f| f.name == slot.name) {
                    self.error(
                        decl.span(),
                        eco_format!(
                            "slot `{}` has the name of a field; an instance stores the slot's \
                             content under that name",
                            slot.name
                        ),
                    );
                }
            }
        }

        let words = match (&ref_key, words) {
            (Some((key, _)), Some((words, _))) => words
                .into_iter()
                .map(|(lang, text)| LocaleString::new(lang, key.clone(), text))
                .collect(),
            (None, Some((_, span))) => {
                self.error_hint(
                    span,
                    "`words` gives the words of a reference key, but there is none",
                    "add `ref-key: \"..\"`",
                );
                EcoVec::new()
            }
            (Some((key, span)), None) => {
                let english = Lang::new("en").expect("a language tag");
                match locale::word(&english, key.as_str()) {
                    Some(word) => EcoVec::from([LocaleString::new("en", key.clone(), word.text)]),
                    None => {
                        self.error_hint(
                            *span,
                            eco_format!("the reference key `{key}` has no words"),
                            "give them as `words: (en: \"..\")`, English at least",
                        );
                        EcoVec::new()
                    }
                }
            }
            (None, None) => EcoVec::new(),
        };

        if !self.errors.is_empty() {
            return None;
        }
        let mut schema = ElementSchema::new(name, kind);
        schema.fields = fields.into_iter().collect();
        schema.body = body;
        schema.counter = counter;
        schema.ref_key = ref_key.map(|(key, _)| key);
        Some(Schema {
            schema,
            words,
            names,
        })
    }

    /// A field: `title: content = none`.
    fn field(&mut self, field: ast::SchemaField<'_>) -> Option<FieldDecl> {
        let ident = field.name();
        let name = Name::new(ident.get())?;
        if name.as_str() == BODY {
            self.error_hint(
                ident.span(),
                "a field cannot be named `body`, which names the body",
                "content an element holds is its body, or a slot of it",
            );
            return None;
        }
        if UNIVERSAL_FIELDS.contains(&name.as_str()) {
            self.error(
                ident.span(),
                eco_format!("every element already has the field `{name}`"),
            );
            return None;
        }
        let ty_expr = field.ty();
        let (ty, layout_data) = self.field_type(ty_expr, true)?;
        if ty.is_presentational() && !layout_data {
            self.error_hint(
                ty_expr.span(),
                eco_format!("a field of type `{ty}` is formatting"),
                "a length or colour that is data of the instance, such as a drawing's scale, is \
                 marked `layout-data(..)`; formatting belongs in a stylesheet",
            );
            return None;
        }
        if contains_content(&ty) && !matches!(ty, FieldType::Content) {
            self.error_hint(
                ty_expr.span(),
                eco_format!("a field cannot have type `{ty}`"),
                "several pieces of content are child elements of the body",
            );
            return None;
        }
        let decl = match field.default() {
            None => FieldDecl::required(name, ty),
            Some(expr) => {
                let default = self.literal(expr)?;
                if !ty.accepts(&default) {
                    self.error(
                        expr.span(),
                        eco_format!(
                            "the default has type `{}`, but the field has type `{ty}`",
                            default.type_name()
                        ),
                    );
                    return None;
                }
                FieldDecl::with_default(name, ty, default)
            }
        };
        Some(decl.with_layout_data(layout_data))
    }

    /// A field's type, and whether it is marked `layout-data`.
    fn field_type(&mut self, expr: ast::Expr<'_>, top: bool) -> Option<(FieldType, bool)> {
        const TYPES: &str = "the types are `content`, `str`, `int`, `number`, `bool`, `ref`, \
                             `choice(..)`, `array(..)`, `dict(..)`, and `length` and `color` \
                             in `layout-data(..)`";
        let ty = match expr {
            ast::Expr::Ident(ident) => match ident.get().as_str() {
                "content" => FieldType::Content,
                "str" => FieldType::Str,
                "int" => FieldType::Int,
                "number" => FieldType::Number,
                "bool" => FieldType::Bool,
                "ref" => FieldType::Ref,
                "length" => FieldType::Length,
                "color" | "colour" => FieldType::Color,
                other => {
                    self.error_hint(expr.span(), eco_format!("unknown type `{other}`"), TYPES);
                    return None;
                }
            },
            ast::Expr::FuncCall(call) => {
                let ast::Expr::Ident(callee) = call.callee() else {
                    self.error_hint(expr.span(), "expected a type", TYPES);
                    return None;
                };
                let args: Vec<ast::Arg<'_>> = call.args().items().collect();
                match callee.get().as_str() {
                    "choice" => {
                        let mut options: Vec<EcoString> = Vec::new();
                        for arg in &args {
                            match arg {
                                ast::Arg::Pos(ast::Expr::Str(s)) if !s.get().is_empty() => {
                                    if options.contains(&s.get()) {
                                        self.error(
                                            s.span(),
                                            eco_format!("option {:?} is listed twice", s.get()),
                                        );
                                        return None;
                                    }
                                    options.push(s.get());
                                }
                                other => {
                                    self.error(
                                        other.to_untyped().span(),
                                        "an option of a choice is a string that is not empty",
                                    );
                                    return None;
                                }
                            }
                        }
                        if options.is_empty() {
                            self.error(expr.span(), "a choice needs at least one option");
                            return None;
                        }
                        FieldType::Choice(options.into_iter().collect())
                    }
                    which @ ("array" | "dict" | "layout-data") => {
                        let [ast::Arg::Pos(item)] = args[..] else {
                            self.error(
                                call.args().span(),
                                eco_format!("`{which}(..)` takes one type"),
                            );
                            return None;
                        };
                        if which == "layout-data" {
                            if !top {
                                self.error(
                                    expr.span(),
                                    "`layout-data(..)` marks a whole field, so it comes first",
                                );
                                return None;
                            }
                            let (ty, _) = self.field_type(item, false)?;
                            if !ty.is_presentational() {
                                self.error(
                                    item.span(),
                                    "only lengths and colours are marked `layout-data`",
                                );
                                return None;
                            }
                            return Some((ty, true));
                        }
                        let (item, _) = self.field_type(item, false)?;
                        match which {
                            "array" => FieldType::Array(Box::new(item)),
                            _ => FieldType::Dict(Box::new(item)),
                        }
                    }
                    other => {
                        self.error_hint(expr.span(), eco_format!("unknown type `{other}`"), TYPES);
                        return None;
                    }
                }
            }
            _ => {
                self.error_hint(expr.span(), "expected a type", TYPES);
                return None;
            }
        };
        Some((ty, false))
    }

    /// A literal value: a default, or a field value in `within`.
    fn literal(&mut self, expr: ast::Expr<'_>) -> Option<Constant> {
        Some(match expr {
            ast::Expr::None(_) => Constant::None,
            ast::Expr::Auto(_) => Constant::Auto,
            ast::Expr::Bool(v) => Constant::Bool(v.get()),
            ast::Expr::Int(v) => Constant::Int(v.get()),
            ast::Expr::Float(v) => Constant::Number(folio_component::Scalar::new(v.get())),
            ast::Expr::Str(v) => Constant::Str(v.get()),
            ast::Expr::Numeric(v) => {
                let (value, unit) = v.get();
                match numeric(value, unit) {
                    Value::Length(Length { abs, em }) if em.is_zero() => Constant::Length(abs),
                    Value::Length(Length { em, .. }) => Constant::Em(em),
                    Value::Ratio(ratio) => Constant::Ratio(ratio),
                    Value::Angle(angle) => Constant::Angle(angle.0),
                    Value::Duration(duration) => Constant::Duration(duration.0),
                    Value::Fraction(fraction) => Constant::Fraction(fraction.0),
                    _ => return self.not_literal(expr),
                }
            }
            ast::Expr::Unary(unary) if unary.op() == ast::UnOp::Neg => {
                match self.literal(unary.expr())? {
                    Constant::Int(v) => Constant::Int(v.checked_neg()?),
                    Constant::Number(v) => Constant::Number(folio_component::Scalar::new(-v.get())),
                    Constant::Length(v) => Constant::Length(-v),
                    Constant::Em(v) => Constant::Em(-v),
                    _ => return self.not_literal(expr),
                }
            }
            ast::Expr::Parenthesized(inner) => self.literal(inner.expr())?,
            ast::Expr::Array(array) => {
                let mut items = EcoVec::new();
                for item in array.items() {
                    match item {
                        ast::ArrayItem::Pos(item) => items.push(self.literal(item)?),
                        ast::ArrayItem::Spread(_) => return self.not_literal(expr),
                    }
                }
                Constant::Array(items)
            }
            ast::Expr::Dict(dict) => {
                let mut pairs: EcoVec<(Name, Constant)> = EcoVec::new();
                for item in dict.items() {
                    let ast::DictItem::Named(named) = item else {
                        return self.not_literal(expr);
                    };
                    let key = Name::new(named.name().get())?;
                    if pairs.iter().any(|(k, _)| *k == key) {
                        self.error(named.name().span(), eco_format!("duplicate key `{key}`"));
                        return None;
                    }
                    pairs.push((key, self.literal(named.expr())?));
                }
                Constant::Dict(pairs)
            }
            _ => return self.not_literal(expr),
        })
    }

    fn not_literal<T>(&mut self, expr: ast::Expr<'_>) -> Option<T> {
        self.error_hint(
            expr.span(),
            "expected a literal value",
            "the defaults of an element's fields are written out, so the element is known \
             before any script runs",
        );
        None
    }

    /// `kind: block`.
    fn kind(&mut self, expr: ast::Expr<'_>) -> Option<ElementKind> {
        let kind = match expr {
            ast::Expr::Ident(ident) => match ident.get().as_str() {
                "block" => Some(ElementKind::Block),
                "inline" => Some(ElementKind::Inline),
                "float" => Some(ElementKind::Float),
                "note" => Some(ElementKind::Note),
                "slide" => Some(ElementKind::Slide),
                _ => None,
            },
            _ => None,
        };
        if kind.is_none() {
            self.error_hint(
                expr.span(),
                "expected a kind",
                "the kinds are `block`, `inline`, `float`, `note` and `slide`",
            );
        }
        kind
    }

    /// `body: slots(statement: blocks, solution: blocks)`, `body: inline`.
    fn body(&mut self, expr: ast::Expr<'_>) -> Option<Body> {
        const HINT: &str = "a body is `none`, `inline`, `blocks` or `slots(name: blocks, ..)`";
        match expr {
            ast::Expr::None(_) => Some(Body::None),
            ast::Expr::Ident(_) => self.content_kind(expr).map(Body::Content),
            ast::Expr::FuncCall(call) if matches!(call.callee(), ast::Expr::Ident(i) if i.get() == "slots") =>
            {
                let mut slots: EcoVec<folio_component::SlotDecl> = EcoVec::new();
                for arg in call.args().items() {
                    let ast::Arg::Named(named) = arg else {
                        self.error_hint(
                            arg.to_untyped().span(),
                            "expected a named slot",
                            "write a slot as `name: blocks` or `name: inline`",
                        );
                        return None;
                    };
                    let ident = named.name();
                    let name = Name::new(ident.get())?;
                    if name.as_str() == BODY || UNIVERSAL_FIELDS.contains(&name.as_str()) {
                        self.error(
                            ident.span(),
                            eco_format!("a slot cannot be named `{name}`, which every element has"),
                        );
                        return None;
                    }
                    if slots.iter().any(|s| s.name == name) {
                        self.error(ident.span(), eco_format!("duplicate slot `{name}`"));
                        return None;
                    }
                    let content = self.content_kind(named.expr())?;
                    slots.push(folio_component::SlotDecl::new(name, content));
                }
                if slots.is_empty() {
                    self.error_hint(expr.span(), "a body of slots needs at least one slot", HINT);
                    return None;
                }
                Some(Body::Slots(slots))
            }
            _ => {
                self.error_hint(expr.span(), "expected a body", HINT);
                None
            }
        }
    }

    /// `blocks` or `inline`.
    fn content_kind(&mut self, expr: ast::Expr<'_>) -> Option<ContentKind> {
        match expr {
            ast::Expr::Ident(ident) if ident.get() == "blocks" => Some(ContentKind::Blocks),
            ast::Expr::Ident(ident) if ident.get() == "inline" => Some(ContentKind::Inline),
            _ => {
                self.error(expr.span(), "expected `blocks` or `inline`");
                None
            }
        }
    }

    /// `counter: auto`, `counter: within heading(level: 1)`, `counter:
    /// scoped(question, part)`, or `within` and `scoped` together.
    fn counter(
        &mut self,
        values: &[ast::Expr<'_>],
        names: &mut EcoVec<(Name, Span)>,
    ) -> Option<CounterDecl> {
        const HINT: &str = "a counter is `auto`, `within element(..)`, `scoped(element, ..)`, \
                            or `within` and `scoped` together";
        let mut counter = CounterDecl::new();
        if let [ast::Expr::Auto(_)] = values {
            return Some(counter);
        }
        let mut within_seen = false;
        let mut scoped_seen = false;
        let mut iter = values.iter().copied();
        while let Some(expr) = iter.next() {
            match expr {
                ast::Expr::Ident(ident) if ident.get() == "within" && !within_seen => {
                    within_seen = true;
                    let Some(target) = iter.next() else {
                        self.error_hint(expr.span(), "`within` needs an element", HINT);
                        return None;
                    };
                    let (element, args) = match target {
                        ast::Expr::FuncCall(call) => (call.callee(), Some(call.args())),
                        other => (other, None),
                    };
                    let element = self.element_name(element, names)?;
                    let mut fields: Vec<(Name, Constant)> = Vec::new();
                    for arg in args.into_iter().flat_map(|a| a.items()) {
                        let ast::Arg::Named(named) = arg else {
                            self.error_hint(
                                arg.to_untyped().span(),
                                "expected a field and its value",
                                "write `within heading(level: 1)`",
                            );
                            return None;
                        };
                        let field = Name::new(named.name().get())?;
                        fields.push((field, self.literal(named.expr())?));
                    }
                    counter.within = Some(Within::new(element, fields));
                }
                ast::Expr::FuncCall(call)
                    if !scoped_seen
                        && matches!(call.callee(), ast::Expr::Ident(i) if i.get() == "scoped") =>
                {
                    scoped_seen = true;
                    let mut scope = EcoVec::new();
                    for arg in call.args().items() {
                        let ast::Arg::Pos(element) = arg else {
                            self.error(arg.to_untyped().span(), "expected an element");
                            return None;
                        };
                        scope.push(self.element_name(element, names)?);
                    }
                    if scope.is_empty() {
                        self.error_hint(expr.span(), "`scoped` needs an element", HINT);
                        return None;
                    }
                    counter.scope = scope;
                }
                _ => {
                    self.error_hint(expr.span(), "expected a counter", HINT);
                    return None;
                }
            }
        }
        Some(counter)
    }

    /// An element name in a counter: `heading`, `exam.question`. An
    /// unqualified one is recorded, to be checked once every declaration
    /// of the project is known.
    fn element_name(
        &mut self,
        expr: ast::Expr<'_>,
        names: &mut EcoVec<(Name, Span)>,
    ) -> Option<QualifiedName> {
        match expr {
            ast::Expr::Ident(ident) => {
                let name = Name::new(ident.get())?;
                names.push((name.clone(), ident.span()));
                Some(QualifiedName::unqualified(name))
            }
            ast::Expr::FieldAccess(access) => match access.target() {
                ast::Expr::Ident(namespace) => Some(QualifiedName::qualified(
                    Name::new(namespace.get())?,
                    Name::new(access.field().get())?,
                )),
                _ => {
                    self.error(expr.span(), "expected an element name");
                    None
                }
            },
            _ => {
                self.error(expr.span(), "expected an element name");
                None
            }
        }
    }

    /// `ref-key: "example"`.
    fn ref_key(&mut self, expr: ast::Expr<'_>) -> Option<(Name, Span)> {
        if let ast::Expr::Str(s) = expr
            && let Some(key) = Name::new(&s.get())
        {
            return Some((key, expr.span()));
        }
        self.error_hint(
            expr.span(),
            "expected a reference key",
            "a reference key is a string that is a name, such as `\"example\"`",
        );
        None
    }

    /// `words: (en: "Example", pl: "przykł.")`.
    fn words(&mut self, expr: ast::Expr<'_>) -> Option<(Vec<(EcoString, EcoString)>, Span)> {
        const HINT: &str = "write the words by language, English at least: \
                            `words: (en: \"Example\", pl: \"przykł.\")`";
        let ast::Expr::Dict(dict) = expr else {
            self.error_hint(expr.span(), "expected words by language", HINT);
            return None;
        };
        let mut out: Vec<(EcoString, EcoString)> = Vec::new();
        for item in dict.items() {
            let (lang, value, span) = match item {
                ast::DictItem::Named(named) => (
                    named.name().get().clone(),
                    named.expr(),
                    named.name().span(),
                ),
                ast::DictItem::Keyed(keyed) => match keyed.key() {
                    ast::Expr::Str(s) => (s.get(), keyed.expr(), s.span()),
                    other => {
                        self.error_hint(other.span(), "expected a language", HINT);
                        return None;
                    }
                },
                ast::DictItem::Spread(spread) => {
                    self.error_hint(spread.span(), "expected a language", HINT);
                    return None;
                }
            };
            if Lang::new(&lang).is_none() {
                self.error(span, eco_format!("`{lang}` is not a language tag"));
                return None;
            }
            if out.iter().any(|(l, _)| l.eq_ignore_ascii_case(&lang)) {
                self.error(span, eco_format!("duplicate language `{lang}`"));
                return None;
            }
            let ast::Expr::Str(text) = value else {
                self.error(value.span(), "expected a word, as a string");
                return None;
            };
            if text.get().trim().is_empty() {
                self.error(value.span(), "a word cannot be empty");
                return None;
            }
            out.push((lang, text.get()));
        }
        if !out.iter().any(|(l, _)| l.eq_ignore_ascii_case("en")) {
            self.error_hint(expr.span(), "the words need English", HINT);
            return None;
        }
        Some((out, expr.span()))
    }
}

/// Whether a type holds content at any depth.
fn contains_content(ty: &FieldType) -> bool {
    match ty {
        FieldType::Content => true,
        FieldType::Array(item) | FieldType::Dict(item) => contains_content(item),
        _ => false,
    }
}
