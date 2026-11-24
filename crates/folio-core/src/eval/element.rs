//! Element instances (SPEC §4.2, §4.3): every element, whether markup or a
//! call made it, is checked against its schema when it is made.
//!
//! A call's named arguments are fields and its positional arguments fill
//! the schema's positional fields and then the body. Each field must be
//! declared by the schema or be universal, and its value must fit the
//! field's type; required fields must be given, and the others take their
//! defaults. The body must fit the content model: inline content, blocks,
//! or the element's slots. Each problem is an error at the argument, or at
//! the call when an argument is missing.

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{
    BODY, Body, ContentKind, ElementKind, ElementSchema, FieldType, LAYOUT_HINTS, Name,
    QualifiedName, Target, UNIVERSAL_FIELDS, Value as Constant,
};
use folio_syntax::{Span, is_valid_label_literal_id};

use super::content::{Content, Element, Node, Slot};
use super::error::{EvalError, EvalResult, StrResult, bail};
use super::ops::a_type;
use super::value::{Arg, Args, Dict, Length, Value};
use super::vm::Vm;
use crate::db::data_file;
use crate::model::{self, schema};

/// The universal fields whose value may be written as a bare keyword, as a
/// stylesheet writes it: `placement: wrap`, `handout: keep`.
pub(crate) const KEYWORD_FIELDS: &[&str] =
    &["placement", "side", "wrap-shape", "break-before", "handout"];

/// Fields no element has, because they are formatting (SPEC §4.2).
const FORMATTING: &[&str] = &[
    "font",
    "font-size",
    "font-weight",
    "font-style",
    "size",
    "color",
    "colour",
    "fill",
    "background",
    "weight",
    "style",
];

/// A field as a call gave it.
struct Given {
    /// The field, with its scope: `override@markscheme`.
    key: EcoString,
    /// The field without its scope.
    field: EcoString,
    /// Whether it was given by position.
    positional: bool,
    arg: Arg,
}

impl Vm<'_> {
    /// Makes an instance of an element from the arguments of a call, or
    /// from the fields markup gives it.
    pub(crate) fn instantiate(
        &mut self,
        name: &QualifiedName,
        args: Args,
        span: Span,
    ) -> EvalResult<Content> {
        let Some(schema) = schema(self.db, self.project, name) else {
            bail!(span, "unknown element `{name}`");
        };

        // Name every argument.
        let mut positional = schema.positional_order();
        let mut given: Vec<Given> = Vec::new();
        for arg in args.items {
            let (field, key, by_position) = match (&arg.name, &arg.scope) {
                (Some(field), Some(scope)) => {
                    self.check_scope(name, field, scope, arg.span)?;
                    (field.clone(), eco_format!("{field}@{scope}"), false)
                }
                (Some(field), None) => (field.clone(), field.clone(), false),
                (None, _) => match positional.next() {
                    Some(field) => (field.into(), field.into(), true),
                    None => return Err(unexpected_positional(name, schema, arg.span).into()),
                },
            };
            if let Some(other) = given.iter().find(|g| g.key == key) {
                if key == BODY && other.positional != by_position {
                    bail!(
                        span,
                        "duplicate field `body`: it is given by name and as the body"
                    );
                }
                bail!(arg.span, "duplicate field `{key}`");
            }
            given.push(Given {
                key,
                field,
                positional: by_position,
                arg,
            });
        }

        // Check each field.
        let mut fields = Dict::new();
        let mut label = None;
        for Given {
            key, field, arg, ..
        } in given
        {
            let arg_span = arg.span;
            let value = arg.value;
            if field == BODY {
                if let Some(value) = self.body(name, schema, value, arg_span)? {
                    match value {
                        CheckedBody::Content(content) => {
                            fields.insert(key, Value::Content(content))
                        }
                        CheckedBody::Slots(slots) => {
                            for (slot, content) in slots {
                                fields.insert(slot, Value::Content(content));
                            }
                        }
                    }
                }
            } else if UNIVERSAL_FIELDS.contains(&field.as_str()) {
                match self.universal(&field, value, arg_span)? {
                    Universal::Field(value) => fields.insert(key, value),
                    Universal::Label(id) => label = Some(id),
                }
            } else if let Some(decl) = schema.field(&field) {
                let value = coerce(&decl.ty, value, arg_span)
                    .map_err(|message| field_error(name, &field, message, arg_span))?;
                if name.namespace.is_none() && name.name.as_str() == "image" && field == "path" {
                    fields.insert(key, self.image_path(value, arg_span)?);
                } else {
                    fields.insert(key, value);
                }
            } else {
                return Err(unknown_field(name, schema, &field, arg_span).into());
            }
        }

        // Required fields and defaults.
        for decl in &schema.fields {
            if fields.get(decl.name.as_str()).is_some() {
                continue;
            }
            match &decl.default {
                None => {
                    return Err(EvalError::new(
                        span,
                        eco_format!("`{name}` needs the field `{}`", decl.name),
                    )
                    .with_hint(eco_format!("give it as `{}: ..`", decl.name))
                    .into());
                }
                Some(Constant::None) => {}
                Some(default) => fields.insert(decl.name.as_str().into(), constant(default)),
            }
        }

        let block = matches!(fields.get("block"), Some(Value::Bool(true)));
        let kind = match name.name.as_str() {
            "raw" | "math" if name.namespace.is_none() && block => ElementKind::Block,
            _ => schema.kind,
        };
        Ok(Content::node(Node::Element(Element {
            name: name.clone(),
            kind,
            fields,
            label,
            span,
        })))
    }

    /// Makes a slot's sub-block: `#statement[..]` in `#example[..]`.
    pub(crate) fn slot(
        &mut self,
        parent: &QualifiedName,
        slot: &Name,
        mut args: Args,
        span: Span,
    ) -> EvalResult<Value> {
        let body = match args.eat() {
            Some(Arg {
                name: None, value, ..
            }) => value,
            Some(arg) => bail!(arg.span, "a slot takes only its content"),
            None => Value::Content(Content::new()),
        };
        if let Some(arg) = args.eat() {
            bail!(arg.span, "a slot takes only its content");
        }
        let body = match body {
            Value::Content(content) => content,
            Value::Str(text) => Content::node(Node::Text { text, span }),
            Value::None => Content::new(),
            other => bail!(
                span,
                "expected content for slot `{slot}`, found {}",
                a_type(other.type_name())
            ),
        };
        Ok(Value::Content(Content::node(Node::Slot(Slot {
            parent: parent.clone(),
            name: slot.clone(),
            body,
            span,
        }))))
    }

    /// Checks that a field may be limited to a variant or target, and that
    /// the scope names one.
    fn check_scope(
        &self,
        element: &QualifiedName,
        field: &str,
        scope: &str,
        span: Span,
    ) -> EvalResult<()> {
        if field != "override" && !LAYOUT_HINTS.contains(&field) {
            return Err(EvalError::new(
                span,
                eco_format!("field `{field}` of `{element}` cannot be limited to a variant or target"),
            )
            .with_hint("only `override` and layout hints can; content that differs by variant belongs in `#if variant == ..`")
            .into());
        }
        let manifest = self.project.manifest(self.db);
        let is_variant = manifest.variants.iter().any(|v| v.name.v.as_str() == scope);
        let is_target = Target::ALL.iter().any(|t| t.as_str() == scope);
        if !is_variant && !is_target {
            let mut names: Vec<String> = manifest
                .variants
                .iter()
                .map(|v| format!("`{}`", v.name.v))
                .collect();
            names.extend(Target::ALL.iter().map(|t| format!("`{}`", t.as_str())));
            return Err(EvalError::new(
                span,
                eco_format!("`{scope}` is neither a variant nor a target"),
            )
            .with_hint(eco_format!(
                "a field can be limited to {}",
                names.join(", ")
            ))
            .into());
        }
        Ok(())
    }

    /// Checks a body against the element's content model.
    fn body(
        &self,
        name: &QualifiedName,
        schema: &ElementSchema,
        value: Value,
        span: Span,
    ) -> EvalResult<Option<CheckedBody>> {
        let content = match value {
            Value::None => return Ok(None),
            Value::Content(content) => content,
            Value::Str(text) => Content::node(Node::Text { text, span }),
            other => bail!(
                span,
                "expected content for the body of `{name}`, found {}",
                a_type(other.type_name())
            ),
        };
        match &schema.body {
            Body::None => Err(EvalError::new(span, eco_format!("`{name}` takes no body"))
                .with_hint(match schema.fields.first() {
                    Some(field) => {
                        eco_format!("its fields are given by name: `{}: ..`", field.name)
                    }
                    None => "remove the content block".into(),
                })
                .into()),
            Body::Content(kind) => {
                check_slots(&content)?;
                if *kind == ContentKind::Inline {
                    check_inline(&content, || eco_format!("the body of `{name}`"), span)?;
                }
                Ok(Some(CheckedBody::Content(content)))
            }
            Body::Slots(slots) => {
                let mut filled: Vec<(EcoString, Content)> = Vec::new();
                for node in content.nodes() {
                    match node {
                        Node::Space | Node::Parbreak => {}
                        Node::Slot(slot) if &slot.parent == name => {
                            let decl = slots
                                .iter()
                                .find(|s| s.name == slot.name)
                                .expect("slots are defined from the schema");
                            if filled.iter().any(|(n, _)| n.as_str() == slot.name.as_str()) {
                                bail!(
                                    slot.span,
                                    "slot `{}` of `{name}` is filled twice",
                                    slot.name
                                );
                            }
                            check_slots(&slot.body)?;
                            if decl.content == ContentKind::Inline {
                                check_inline(
                                    &slot.body,
                                    || eco_format!("slot `{}` of `{name}`", slot.name),
                                    slot.span,
                                )?;
                            }
                            filled.push((slot.name.as_str().into(), slot.body.clone()));
                        }
                        Node::Slot(slot) => return Err(misplaced_slot(slot).into()),
                        other => {
                            let names: Vec<String> =
                                slots.iter().map(|s| format!("`#{}[..]`", s.name)).collect();
                            return Err(EvalError::new(
                                node_span(other).unwrap_or(span),
                                eco_format!("the body of `{name}` holds only its slots"),
                            )
                            .with_hint(eco_format!("put this content in {}", names.join(" or ")))
                            .into());
                        }
                    }
                }
                Ok(Some(CheckedBody::Slots(filled)))
            }
        }
    }

    /// Checks a universal field (SPEC §4.2, §5.4).
    fn universal(&self, field: &str, value: Value, span: Span) -> EvalResult<Universal> {
        let error = |message: &str| -> EvalResult<Universal> {
            Err(EvalError::new(span, eco_format!("`{field}` {message}")).into())
        };
        Ok(Universal::Field(match (field, value) {
            ("class", Value::Str(text)) => {
                let names: EcoVec<Value> = text
                    .split_whitespace()
                    .map(|n| Value::Str(n.into()))
                    .collect();
                check_classes(&names, span)?;
                Value::Array(names)
            }
            ("class", Value::Array(names)) => {
                check_classes(&names, span)?;
                Value::Array(names)
            }
            ("class", _) => return error("takes class names, such as `\"hard\"`"),
            ("id", Value::Str(id) | Value::Label(id)) => {
                if !is_valid_label_literal_id(&id) {
                    return Err(EvalError::new(span, eco_format!("`{id}` is not a valid label"))
                        .with_hint("a label has letters, digits, `-`, `_` and `:`, and does not end with `:` or `-`")
                        .into());
                }
                return Ok(Universal::Label(id));
            }
            ("id", _) => return error("takes a label, such as `<fig:ball>`"),
            ("steps", Value::Int(step)) if step >= 1 => Value::Str(eco_format!("{step}")),
            ("steps", Value::Str(steps)) => {
                check_steps(&steps).map_err(|m| EvalError::new(span, m))?;
                Value::Str(steps)
            }
            ("steps", _) => return error("takes steps such as `\"2-\"` or `\"1,3-4\"`"),
            ("handout", Value::Str(word)) if word == "keep" => Value::Str(word),
            ("handout", _) => return error("takes `keep`"),
            ("override", Value::Dict(dict)) => Value::Dict(dict),
            ("override", _) => {
                return error("takes a dictionary of properties, such as `(space-before: 24pt)`");
            }
            // Layout hints are stored; the style stage checks them against
            // their properties.
            (_, value) => value,
        }))
    }

    /// Resolves an image's path against the file it is written in, and
    /// checks that the file exists.
    fn image_path(&self, value: Value, span: Span) -> EvalResult<Value> {
        let Value::Str(text) = value else {
            return Ok(value);
        };
        let Some(path) = self.file.join(&text) else {
            return Err(
                EvalError::new(span, eco_format!("`{text}` is not a path in the project"))
                    .with_hint("Folio reads files only inside the project directory")
                    .into(),
            );
        };
        if data_file(self.db, self.project, path.clone()).is_none() {
            bail!(span, "file `{}` not found", path.as_str());
        }
        Ok(Value::Str(path.as_str().into()))
    }
}

/// A checked body: content, or the content of each slot.
enum CheckedBody {
    Content(Content),
    Slots(Vec<(EcoString, Content)>),
}

/// A checked universal field: a value to store, or the element's label.
enum Universal {
    Field(Value),
    Label(EcoString),
}

/// Converts a value to a field's type: strings become text in content
/// fields, and content fields must be inline.
fn coerce(ty: &FieldType, value: Value, span: Span) -> StrResult<Value> {
    let found = || {
        eco_format!(
            "expected {}, found {}",
            describe(ty),
            a_type(value.type_name())
        )
    };
    Ok(match (ty, &value) {
        (_, Value::None) => value,
        (FieldType::Content, Value::Content(content)) => {
            check_inline_str(content)?;
            check_no_slots(content)?;
            value
        }
        (FieldType::Content, Value::Str(text)) => Value::Content(Content::node(Node::Text {
            text: text.clone(),
            span,
        })),
        (FieldType::Str, Value::Str(_))
        | (FieldType::Int, Value::Int(_))
        | (FieldType::Number, Value::Int(_) | Value::Float(_))
        | (FieldType::Bool, Value::Bool(_))
        | (FieldType::Ref, Value::Label(_))
        | (FieldType::Length, Value::Length(_))
        | (FieldType::Color, Value::Color(_)) => value,
        (FieldType::Choice(options), Value::Str(text)) => {
            if !options.contains(text) {
                let list: Vec<String> = options.iter().map(|o| format!("{o:?}")).collect();
                return Err(eco_format!(
                    "expected one of {}, found {text:?}",
                    list.join(", ")
                ));
            }
            value
        }
        (FieldType::Ref, Value::Str(text)) => {
            return Err(eco_format!("a reference is a label; write `<{text}>`"));
        }
        (FieldType::Array(item), Value::Array(items)) => {
            let mut out = EcoVec::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                out.push(coerce(item, v.clone(), span).map_err(|m| eco_format!("item {i}: {m}"))?);
            }
            Value::Array(out)
        }
        (FieldType::Dict(item), Value::Dict(dict)) => {
            let mut out = Dict::new();
            for (k, v) in dict.iter() {
                out.insert(
                    k.clone(),
                    coerce(item, v.clone(), span).map_err(|m| eco_format!("key {k:?}: {m}"))?,
                );
            }
            Value::Dict(out)
        }
        _ => return Err(found()),
    })
}

/// A field type as an error message names it.
fn describe(ty: &FieldType) -> EcoString {
    match ty {
        FieldType::Content => "content".into(),
        FieldType::Str => "a string".into(),
        FieldType::Int => "an integer".into(),
        FieldType::Number => "a number".into(),
        FieldType::Bool => "a boolean".into(),
        FieldType::Choice(_) => "a string".into(),
        FieldType::Ref => "a label".into(),
        FieldType::Array(item) => eco_format!("an array of {}", plural(item)),
        FieldType::Dict(item) => eco_format!("a dictionary of {}", plural(item)),
        FieldType::Length => "a length".into(),
        FieldType::Color => "a colour".into(),
    }
}

fn plural(ty: &FieldType) -> EcoString {
    match ty {
        FieldType::Content => "content".into(),
        FieldType::Str | FieldType::Choice(_) => "strings".into(),
        FieldType::Int => "integers".into(),
        FieldType::Number => "numbers".into(),
        FieldType::Bool => "booleans".into(),
        FieldType::Ref => "labels".into(),
        FieldType::Array(_) => "arrays".into(),
        FieldType::Dict(_) => "dictionaries".into(),
        FieldType::Length => "lengths".into(),
        FieldType::Color => "colours".into(),
    }
}

/// A default value as the evaluator represents it.
fn constant(value: &Constant) -> Value {
    match value {
        Constant::None => Value::None,
        Constant::Auto => Value::Auto,
        Constant::Bool(v) => Value::Bool(*v),
        Constant::Int(v) => Value::Int(*v),
        Constant::Number(v) => Value::Float(*v),
        // A default for a content field is its text.
        Constant::Str(text) => Value::Str(text.clone()),
        Constant::Keyword(word) => Value::Str(word.as_str().into()),
        Constant::Length(abs) => Value::Length(Length::abs(*abs)),
        Constant::Em(em) => Value::Length(Length::em(*em)),
        Constant::Ratio(ratio) => Value::Ratio(*ratio),
        Constant::Color(color) => Value::Color(*color),
        Constant::Array(items) => Value::Array(items.iter().map(constant).collect()),
        Constant::Dict(pairs) => {
            let mut dict = Dict::new();
            for (key, value) in pairs {
                dict.insert(key.as_str().into(), constant(value));
            }
            Value::Dict(dict)
        }
    }
}

/// Checks that content is inline: no paragraph breaks, and no blocks.
fn check_inline(content: &Content, what: impl Fn() -> EcoString, span: Span) -> EvalResult<()> {
    for node in content.nodes() {
        match node {
            Node::Parbreak => bail!(
                span,
                "{} takes inline content, but has a paragraph break",
                what()
            ),
            Node::Element(element) if model::is_block(element.kind) => {
                return Err(EvalError::new(
                    element.span,
                    eco_format!(
                        "{} takes inline content, but `{}` is a block",
                        what(),
                        element.name
                    ),
                )
                .into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// [`check_inline`] for a content field, whose caller adds the span.
fn check_inline_str(content: &Content) -> StrResult<()> {
    for node in content.nodes() {
        match node {
            Node::Parbreak => return Err("expected inline content, found a paragraph break".into()),
            Node::Element(element) if model::is_block(element.kind) => {
                return Err(eco_format!(
                    "expected inline content, found `{}`, which is a block",
                    element.name
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Checks that content holds no slot's sub-block, which belongs directly
/// in the body of its element.
pub(crate) fn check_slots(content: &Content) -> EvalResult<()> {
    for node in content.nodes() {
        if let Node::Slot(slot) = node {
            return Err(misplaced_slot(slot).into());
        }
    }
    Ok(())
}

fn check_no_slots(content: &Content) -> StrResult<()> {
    for node in content.nodes() {
        if let Node::Slot(slot) = node {
            return Err(eco_format!(
                "`#{}` fills a slot of `{}`, so it belongs directly in its body",
                slot.name,
                slot.parent
            ));
        }
    }
    Ok(())
}

pub(crate) fn misplaced_slot(slot: &Slot) -> EvalError {
    EvalError::new(
        slot.span,
        eco_format!(
            "`#{}` fills a slot of `{}`, so it belongs directly in its body",
            slot.name,
            slot.parent
        ),
    )
}

/// Where a node was made, if it records it.
fn node_span(node: &Node) -> Option<Span> {
    match node {
        Node::Text { span, .. } => Some(*span),
        Node::Element(element) => Some(element.span),
        Node::Slot(slot) => Some(slot.span),
        Node::Rule(rule) => Some(rule.span()),
        Node::Space | Node::Linebreak | Node::Parbreak | Node::Quote { .. } => None,
    }
}

fn check_classes(names: &[Value], span: Span) -> EvalResult<()> {
    if names.is_empty() {
        bail!(span, "`class` needs at least one class name");
    }
    for name in names {
        match name {
            Value::Str(text) if folio_component::is_ident(text) => {}
            Value::Str(text) => bail!(span, "`{text}` is not a class name"),
            other => bail!(
                span,
                "expected a class name, found {}",
                a_type(other.type_name())
            ),
        }
    }
    Ok(())
}

/// Checks a step specification (SPEC §10.1): comma-separated items, each a
/// step (`3`, `+`, `.`), a range (`2-4`), or an open range (`2-`, `-3`).
fn check_steps(steps: &str) -> StrResult<()> {
    let step = |s: &str| {
        s == "+"
            || s == "."
            || (!s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && s.parse::<u32>().is_ok_and(|n| n >= 1))
    };
    let valid = !steps.trim().is_empty()
        && steps
            .split(',')
            .map(str::trim)
            .all(|item| match item.split_once('-') {
                None => step(item),
                Some((from, to)) => {
                    (from.is_empty() || step(from))
                        && (to.is_empty() || step(to))
                        && !(from.is_empty() && to.is_empty())
                }
            });
    if valid {
        Ok(())
    } else {
        Err(eco_format!(
            "`{steps}` is not a step specification; write steps such as `\"3\"`, `\"2-4\"`, `\"2-\"` or `\"1,3-4\"`"
        ))
    }
}

fn field_error(element: &QualifiedName, field: &str, message: EcoString, span: Span) -> EvalError {
    EvalError::new(
        span,
        eco_format!("field `{field}` of `{element}`: {message}"),
    )
}

fn unknown_field(
    element: &QualifiedName,
    schema: &ElementSchema,
    field: &str,
    span: Span,
) -> EvalError {
    let mut error = EvalError::new(span, eco_format!("`{element}` has no field `{field}`"));
    if FORMATTING.contains(&field) {
        error = error.with_hint(
            "formatting belongs in a stylesheet; give the element a `class`, or use `override` for this one instance",
        );
    } else if schema.slot(field).is_some() {
        error = error.with_hint(eco_format!(
            "`{field}` is a slot: fill it with `#{field}[..]` in the body"
        ));
    } else if !schema.fields.is_empty() {
        let names: Vec<String> = schema
            .fields
            .iter()
            .map(|f| format!("`{}`", f.name))
            .collect();
        error = error.with_hint(eco_format!("its fields are {}", names.join(", ")));
    }
    error
}

fn unexpected_positional(element: &QualifiedName, schema: &ElementSchema, span: Span) -> EvalError {
    let error = EvalError::new(span, "unexpected argument");
    match schema.positional_order().count() {
        0 => error.with_hint(eco_format!("`{element}` takes its fields by name")),
        _ => error.with_hint(eco_format!(
            "`{element}` takes {} by position; give other fields by name",
            schema
                .positional_order()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
}
