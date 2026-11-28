//! The standard library: global functions, the `calc` module, methods of
//! values, data loading, the built-in elements content can call, and the
//! elements of the project's components.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use ecow::{EcoString, eco_format};
use folio_component::{Color, ElementKind, QualifiedName, Scalar};
use folio_syntax::{DiagSpan, FileId, Span, ast};

use super::MAX_NESTING;
use super::content::{Content, Node};
use super::error::{At, EvalError, EvalResult, Stop, StrResult, bail};
use super::ops::{self, a_type};
use super::value::{
    Applied, Arg, Args, Array, Dict, Func, ITEM, Module, NODE, NativeFunc, Scope, Value,
};
use super::vm::{FromValue, NativeFn, Vm};
use crate::component::ComponentHost;
use crate::db::{Bytes, FileKind, data_file, missing_source, report_read, source_file};
use crate::locate::{Frame, item_key};

/// The global scope: the library, the built-in elements content can call,
/// and the elements of the components in use, unqualified where the name
/// resolves and in their namespace (`exam.question`).
pub(crate) fn globals(host: Option<&ComponentHost>) -> Scope {
    let mut scope = BTreeMap::new();
    for &(name, func) in GLOBALS {
        scope.insert(name.into(), native(name, func));
    }
    let mut calc: BTreeMap<EcoString, Value> = CALC
        .iter()
        .map(|&(name, func)| (name.trim_start_matches("calc.").into(), native(name, func)))
        .collect();
    calc.insert("pi".into(), Value::Float(Scalar::new(std::f64::consts::PI)));
    calc.insert("e".into(), Value::Float(Scalar::new(std::f64::consts::E)));
    scope.insert(
        "calc".into(),
        Value::Module(Module::new("calc", calc.into(), Content::new())),
    );
    // `tok.warn` names a stylesheet token, which the style stage resolves
    // for each build (SPEC §5.4).
    scope.insert("tok".into(), Value::Module(Module::tokens()));

    // The built-in elements come before components' elements: a component
    // element named like one is reached only qualified (`exam.figure`).
    for builtin in crate::model::builtins().iter().filter(|b| b.callable) {
        let name = &builtin.schema.name;
        scope.entry(name.as_str().into()).or_insert_with(|| {
            Value::Func(Func::Element(QualifiedName::unqualified(name.clone())))
        });
    }

    let Some(host) = host else {
        return scope.into();
    };
    let mut names = Vec::new();
    for component in host.components() {
        // The project's own elements are variables where they are declared
        // and imported, not everywhere (SPEC §4.3).
        if super::is_project(component) {
            continue;
        }
        let namespace = component.manifest.namespace();
        let elements: BTreeMap<EcoString, Value> = component
            .elements()
            .map(|schema| {
                names.push(schema.name.clone());
                let name = QualifiedName::qualified(namespace.clone(), schema.name.clone());
                (
                    schema.name.as_str().into(),
                    Value::Func(Func::Element(name)),
                )
            })
            .collect();
        // The library's names come first: a component cannot change what
        // `calc` or `range` means.
        scope.entry(namespace.as_str().into()).or_insert_with(|| {
            Value::Module(Module::new(
                namespace.as_str(),
                elements.into(),
                Content::new(),
            ))
        });
    }
    for name in names {
        if let Ok(resolved) = host.element(&QualifiedName::unqualified(name.clone())) {
            let qualified = QualifiedName::qualified(
                resolved.component.manifest.namespace(),
                resolved.item.name.clone(),
            );
            scope
                .entry(name.as_str().into())
                .or_insert(Value::Func(Func::Element(qualified)));
        }
    }
    scope.into()
}

fn native(name: &'static str, func: NativeFn) -> Value {
    Value::Func(Func::Native(NativeFunc { name, func }))
}

const GLOBALS: &[(&str, NativeFn)] = &[
    ("assert", assert),
    ("csv", csv),
    ("float", float),
    ("int", int),
    ("json", json),
    ("num", num_func),
    ("panic", panic),
    ("qty", qty_func),
    ("range", range),
    ("read", read),
    ("repr", repr),
    ("rgb", rgb),
    ("shuffle", shuffle),
    ("str", str),
    ("toml", toml),
    ("type", type_),
    ("unit", unit_func),
    ("yaml", yaml),
];

/// `qty(9.81, "m s^-2", pm: 0.02, digits: 3)` in text: an inline formula
/// of the quantity, as `$qty(..)$` makes it (SPEC §8.1). A value is an
/// integer, a float, or a string that keeps its digits (`"9.80"`).
fn qty_func(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    quantity_func(vm, args, true, true)
}

/// `unit("kg m^2 s^-1")` in text.
fn unit_func(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    quantity_func(vm, args, false, true)
}

/// `num(6.02e23, pm: .., digits: ..)` in text.
fn num_func(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    quantity_func(vm, args, true, false)
}

fn quantity_func(vm: &mut Vm<'_>, args: &mut Args, value: bool, unit: bool) -> EvalResult<Value> {
    use super::math::{Spanned, quantity};
    let span = args.span;
    let mut positional = |what: &str| match args.eat() {
        Some(arg) => Ok(arg),
        None => Err(EvalError::new(
            span,
            eco_format!("missing argument: {what}"),
        )),
    };
    let value = if value {
        let arg = positional("value")?;
        Some((spelled_number(&arg)?, arg.span))
    } else {
        None
    };
    let unit = if unit {
        let arg = positional("unit")?;
        Some(Spanned::new(arg.value, arg.span))
    } else {
        None
    };
    let (pm, digits) = if value.is_some() {
        // Errors point at the argument, or at the call.
        let at = |args: &Args, name: &str| {
            args.items()
                .iter()
                .rev()
                .find(|arg| arg.name.as_deref() == Some(name) && arg.scope.is_none())
                .map_or(span, |arg| arg.span)
        };
        let pm_at = at(args, "pm");
        // The last `pm:` wins, as `named` takes it; its spelling with it.
        let pm_spelling = args
            .items()
            .iter()
            .rev()
            .find(|arg| arg.name.as_deref() == Some("pm") && arg.scope.is_none())
            .and_then(|arg| arg.spelling.clone());
        let pm = match args.named::<Value>("pm")? {
            Some(v) => Some((
                spelled_number(&Arg {
                    span: pm_at,
                    name: None,
                    scope: None,
                    value: v,
                    spelling: pm_spelling,
                })?,
                pm_at,
            )),
            None => None,
        };
        let digits_at = at(args, "digits");
        let digits = args.named::<Value>("digits")?.map(|v| (v, digits_at));
        (pm, digits)
    } else {
        (None, None)
    };
    let quantity = quantity(value, pm, unit, digits, span)?;
    let formula = folio_math::Formula::row(
        folio_math::Row::new([folio_math::Node::Quantity(std::sync::Arc::new(quantity))]),
        span,
    );
    Ok(Value::Content(vm.math_content(formula, false, span)?))
}

/// The number of an argument of `qty` or `num` in text: a float literal as
/// it was written, which keeps its digits and its power of ten as in an
/// equation (`3.00e8`, `2.0`; D-324-1), or the number its value gives.
/// A literal that cannot be read (too many digits, too large a power of
/// ten) is an error that says why, as in an equation.
fn spelled_number(arg: &Arg) -> EvalResult<folio_math::Number> {
    if let (Value::Float(_), Some(spelling)) = (&arg.value, &arg.spelling) {
        return folio_math::Number::parse(spelling)
            .map_err(|message| EvalError::new(arg.span, message).into());
    }
    super::math::number_of(&arg.value, arg.span)
}

/// The `calc` module. Its transcendental functions come from `libm`, not
/// the platform's maths library, so that they give the same bits on every
/// platform (SPEC §2.4).
const CALC: &[(&str, NativeFn)] = &[
    ("calc.abs", calc_abs),
    ("calc.ceil", calc_ceil),
    ("calc.clamp", calc_clamp),
    ("calc.cos", calc_cos),
    ("calc.even", calc_even),
    ("calc.exp", calc_exp),
    ("calc.floor", calc_floor),
    ("calc.log", calc_log),
    ("calc.max", calc_max),
    ("calc.min", calc_min),
    ("calc.odd", calc_odd),
    ("calc.pow", calc_pow),
    ("calc.quo", calc_quo),
    ("calc.rem", calc_rem),
    ("calc.round", calc_round),
    ("calc.sin", calc_sin),
    ("calc.sqrt", calc_sqrt),
    ("calc.tan", calc_tan),
];

fn float_value(v: f64) -> Value {
    Value::Float(Scalar::new(v))
}

/// A computed float, or an error for one that is not a number, which would
/// otherwise become zero silently.
fn real(v: f64, span: Span) -> EvalResult<Value> {
    if v.is_nan() {
        bail!(span, "the result is not a number");
    }
    Ok(float_value(v))
}

// ---- Global functions ----

/// `type(value)`: the name of the value's type.
fn type_(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let value: Value = args.expect("value")?;
    Ok(Value::Str(value.type_name().into()))
}

/// `repr(value)`: the value as code writes it.
fn repr(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let value: Value = args.expect("value")?;
    Ok(Value::Str(vm.repr(&value, span)?))
}

/// `str(value)`: a number, label or string as a string.
fn str(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let value: Value = args.expect("value")?;
    Ok(Value::Str(match value {
        Value::Str(s) | Value::Label(s) => s,
        v @ (Value::Int(_) | Value::Float(_) | Value::Bool(_)) => v.repr(),
        v => bail!(span, "cannot convert {} to a string", a_type(v.type_name())),
    }))
}

/// `int(value)`: a boolean, float or string as an integer.
fn int(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let value: Value = args.expect("value")?;
    Ok(Value::Int(match value {
        Value::Bool(b) => i64::from(b),
        Value::Int(i) => i,
        Value::Float(f) if f.get().is_finite() && f.get().abs() < 9.2e18 => f.get().trunc() as i64,
        Value::Str(s) => match s.trim().parse() {
            Ok(i) => i,
            Err(_) => bail!(span, "{s:?} is not an integer"),
        },
        v => bail!(
            span,
            "cannot convert {} to an integer",
            a_type(v.type_name())
        ),
    }))
}

/// `float(value)`: an integer, ratio or string as a float.
fn float(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let value: Value = args.expect("value")?;
    real(
        match value {
            Value::Int(i) => i as f64,
            Value::Float(f) => f.get(),
            Value::Ratio(r) => r.get(),
            Value::Str(s) => match s.trim().parse() {
                Ok(f) => f,
                Err(_) => bail!(span, "{s:?} is not a number"),
            },
            v => bail!(span, "cannot convert {} to a float", a_type(v.type_name())),
        },
        span,
    )
}

/// `assert(condition, message: ..)`.
fn assert(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let condition: bool = args.expect("condition")?;
    let message: Option<EcoString> = args.named("message")?;
    if !condition {
        match message {
            Some(message) => bail!(span, "assertion failed: {message}"),
            None => bail!(span, "assertion failed"),
        }
    }
    Ok(Value::None)
}

/// `panic(..values)`: stops with an error.
fn panic(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let mut message = EcoString::from("panicked");
    let mut first = true;
    while let Some(arg) = args.eat() {
        message.push_str(if first { " with: " } else { ", " });
        first = false;
        match arg.value {
            Value::Str(s) => message.push_str(&s),
            v => message.push_str(&vm.repr(&v, arg.span)?),
        }
    }
    bail!(span, "{message}")
}

/// `range(end)`, `range(start, end, step: 1)`: an array of integers.
fn range(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let first: i64 = args.expect("end")?;
    let (start, end) = match args.find::<i64>()? {
        Some(end) => (first, end),
        None => (0, first),
    };
    let step: i64 = args.named("step")?.unwrap_or(1);
    if step == 0 {
        bail!(span, "the step of a range must not be zero");
    }
    let len = if (step > 0 && end > start) || (step < 0 && end < start) {
        (i128::from(end) - i128::from(start))
            .unsigned_abs()
            .div_ceil(step.unsigned_abs().into())
    } else {
        0
    };
    vm.alloc(
        u64::try_from(len.saturating_mul(ITEM.into())).unwrap_or(u64::MAX),
        span,
    )?;
    let mut out = Array::new();
    let mut x = start;
    while (step > 0 && x < end) || (step < 0 && x > end) {
        out.push(Value::Int(x));
        match x.checked_add(step) {
            Some(next) => x = next,
            None => break,
        }
    }
    Ok(Value::Array(out))
}

/// `shuffle(items, seed: "B")`: the array's items in an order fixed by the
/// seed, a string or an integer (an integer seeds as its decimal digits, so
/// `seed: 2` and `seed: "2"` agree). `#shuffle(seed: "B")[..]` reorders the
/// blocks, floats and slides at the top level of its body in the same way,
/// and leaves the text, inline elements, notes, spaces and breaks between
/// them where they are. `seed: none` keeps
/// the order, so that a build without versions is not shuffled (SPEC §6.4).
fn shuffle(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let items: Value = args.expect("items")?;
    let seed = match args.named::<Value>("seed")? {
        None => {
            return Err(EvalError::new(span, "missing argument: seed")
                .with_hint("name the seed, as in `shuffle(items, seed: version)`")
                .into());
        }
        Some(Value::None) => None,
        Some(Value::Str(s)) => Some(s),
        Some(Value::Int(i)) => Some(eco_format!("{i}")),
        Some(v) => bail!(
            span,
            "expected a string, an integer or none as the seed, found {}",
            a_type(v.type_name())
        ),
    };
    match items {
        Value::Array(items) => {
            let Some(seed) = seed else {
                return Ok(Value::Array(items));
            };
            vm.alloc(2 * ITEM * items.len() as u64, span)?;
            let order = permutation(seed.as_bytes(), items.len());
            let slice = items.as_slice();
            Ok(Value::Array(
                order.into_iter().map(|i| slice[i].clone()).collect(),
            ))
        }
        Value::Content(body) => {
            let Some(seed) = seed else {
                return Ok(Value::Content(body));
            };
            let nodes = body.nodes();
            let at: Vec<usize> = (0..nodes.len())
                .filter(|&i| {
                    matches!(&nodes[i], Node::Element(e)
                        if !matches!(e.kind, ElementKind::Inline | ElementKind::Note))
                })
                .collect();
            vm.alloc(2 * ITEM * nodes.len() as u64, span)?;
            let order = permutation(seed.as_bytes(), at.len());
            let mut out: Vec<Node> = nodes.to_vec();
            for (slot, from) in at.iter().zip(order) {
                out[*slot] = nodes[at[from]].clone();
            }
            Ok(Value::Content(out.into_iter().collect()))
        }
        v => bail!(
            span,
            "expected an array or content to shuffle, found {}",
            a_type(v.type_name())
        ),
    }
}

/// The order of `len` items that a seed fixes: index `k` of the result is
/// the index of the item put `k`th. The algorithm is part of the language
/// (SPEC §6.4), so that a version printed once can be printed again by a
/// later compiler: the seed's bytes hashed with 64-bit FNV-1a start a
/// SplitMix64 generator, which drives a Fisher–Yates shuffle from the last
/// item down, each draw bounded without bias by Lemire's method.
pub(crate) fn permutation(seed: &[u8], len: usize) -> Vec<usize> {
    let mut state = seed.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let mut next = move || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    // A number below `n`, every one equally likely.
    let mut below = |n: u64| -> u64 {
        let threshold = n.wrapping_neg() % n;
        loop {
            let m = u128::from(next()) * u128::from(n);
            if (m as u64) >= threshold {
                return (m >> 64) as u64;
            }
        }
    };
    let mut order: Vec<usize> = (0..len).collect();
    for i in (1..len).rev() {
        let j = below(i as u64 + 1) as usize;
        order.swap(i, j);
    }
    order
}

/// `rgb("1f5fa8")`, `rgb(31, 95, 168)`, with an optional alpha.
fn rgb(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let first: Value = args.expect("colour")?;
    if let Value::Str(hex) = &first {
        return parse_hex(hex).map(Value::Color).at(span);
    }
    let mut channels = vec![first];
    while let Some(arg) = args.eat() {
        channels.push(arg.value);
    }
    if !(3..=4).contains(&channels.len()) {
        bail!(span, "expected three or four channels");
    }
    let mut out = [255u8; 4];
    for (slot, value) in out.iter_mut().zip(channels) {
        *slot = match value {
            Value::Int(i) => u8::try_from(i)
                .map_err(|_| EcoString::from("a channel must be between 0 and 255"))
                .at(span)?,
            Value::Ratio(r) => (r.get().clamp(0.0, 1.0) * 255.0).round() as u8,
            v => bail!(
                span,
                "expected an integer or ratio, found {}",
                a_type(v.type_name())
            ),
        };
    }
    Ok(Value::Color(Color::rgba(out[0], out[1], out[2], out[3])))
}

fn parse_hex(hex: &str) -> StrResult<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let digits: Option<Vec<u8>> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect();
    let digits = digits.ok_or_else(|| eco_format!("{hex:?} is not a hexadecimal colour"))?;
    let channel = |i: usize| digits[i] * 16 + digits[i + 1];
    let short = |i: usize| digits[i] * 17;
    Ok(match digits.len() {
        3 => Color::rgb(short(0), short(1), short(2)),
        4 => Color::rgba(short(0), short(1), short(2), short(3)),
        6 => Color::rgb(channel(0), channel(2), channel(4)),
        8 => Color::rgba(channel(0), channel(2), channel(4), channel(6)),
        _ => return Err(eco_format!("{hex:?} is not a hexadecimal colour")),
    })
}

// ---- Data loading ----

/// Reads a file of the project by a path relative to the file being
/// evaluated. This is the only way scripts read files (SPEC §4.4).
/// Returns the file's contents, the span of the path, and the file id that
/// errors in the file can point into, if it has one.
fn load(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<(Bytes, Span, Option<FileId>)> {
    let arg = args
        .eat()
        .ok_or_else(|| EvalError::new(args.span, "missing argument: path"))?;
    let span = arg.span;
    let text = EcoString::from_value(arg.value).at(span)?;
    let Some(path) = vm.file.join(&text) else {
        return Err(
            EvalError::new(span, eco_format!("`{text}` is not a path in the project"))
                .with_hint("Folio reads files only inside the project directory")
                .into(),
        );
    };
    report_read(vm.db, path.clone(), FileKind::Data);
    let (bytes, id) = if let Some(file) = data_file(vm.db, vm.project, path.clone()) {
        (file.bytes(vm.db).clone(), file.id(vm.db))
    } else if let Some(file) = source_file(vm.db, vm.project, path.clone()) {
        let source = file.source(vm.db);
        (Bytes::from(source.text().as_bytes()), Some(source.id()))
    } else if vm.project.unreadable(vm.db).contains_key(&path) {
        // A source the host found but could not take: say why.
        let (message, hint) = missing_source(vm.db, vm.project, &path);
        let mut error = EvalError::new(span, message);
        if let Some(hint) = hint {
            error = error.with_hint(hint);
        }
        return Err(error.into());
    } else {
        bail!(span, "file `{}` not found", path.as_str());
    };
    vm.alloc(bytes.len() as u64, span)?;
    Ok((bytes, span, id))
}

fn load_text(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<(EcoString, Span, Option<FileId>)> {
    let (bytes, span, id) = load(vm, args)?;
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok((text.into(), span, id)),
        Err(err) => {
            // What comes before the first invalid byte is text.
            let valid = std::str::from_utf8(&bytes[..err.valid_up_to()]).unwrap_or_default();
            let (line, column) = line_column(valid, valid.len());
            let message =
                eco_format!("the file is not valid UTF-8 text (line {line}, column {column})");
            // A file that is not text has no lines to show.
            Err(DataError {
                message,
                range: None,
            }
            .at(span, id))
        }
    }
}

/// An error in a data file, with the bytes of the file it is about.
#[derive(Debug, Clone, Eq, PartialEq)]
struct DataError {
    /// What is wrong, and where by line and column.
    message: EcoString,
    /// The bytes it is about, if it knows.
    range: Option<std::ops::Range<usize>>,
}

impl DataError {
    /// An error at a byte range of `text`, whose line and column the
    /// message ends with.
    fn new(text: &str, message: impl Into<EcoString>, range: std::ops::Range<usize>) -> Self {
        let (line, column) = line_column(text, range.start);
        Self {
            message: eco_format!("{} (line {line}, column {column})", message.into()),
            range: Some(range),
        }
    }

    /// The error of a call at `span` that read a file with the id, which
    /// shows the place in the file too when it has an id.
    fn at(self, span: Span, id: Option<FileId>) -> Stop {
        let mut error = EvalError::new(span, self.message);
        if let (Some(id), Some(range)) = (id, self.range) {
            error = error.with_related(DiagSpan::from_range(id, range), "here in the file");
        }
        error.into()
    }
}

/// `read(path)`: a file's text.
fn read(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    load_text(vm, args).map(|(text, ..)| Value::Str(text))
}

/// `json(path)`: the data in a JSON file.
fn json(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (bytes, span, id) = load(vm, args)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| {
        // An error without a place, or in a file that is not text, has no
        // lines to show.
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if err.line() > 0 => text,
            _ => {
                return DataError {
                    message: eco_format!("the file is not valid JSON: {err}"),
                    range: None,
                }
                .at(span, id);
            }
        };
        // serde_json counts columns in bytes, from 1, and ends its message
        // with them; the other readers count characters.
        let start = text
            .split_inclusive('\n')
            .take(err.line().saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            + err.column().saturating_sub(1);
        let start = text.floor_char_boundary(start.min(text.len()));
        let end = text.ceil_char_boundary((start + 1).min(text.len()));
        let message = err.to_string();
        let suffix = format!(" at line {} column {}", err.line(), err.column());
        let message = message.strip_suffix(&suffix).unwrap_or(&message);
        DataError::new(
            text,
            eco_format!("the file is not valid JSON: {message}"),
            start..end,
        )
        .at(span, id)
    })?;
    Ok(from_json(value))
}

fn from_json(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::None,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => float_value(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::Str(s.into()),
        serde_json::Value::Array(items) => Value::Array(items.into_iter().map(from_json).collect()),
        serde_json::Value::Object(map) => Value::Dict(
            map.into_iter()
                .map(|(k, v)| (EcoString::from(k), from_json(v)))
                .collect(),
        ),
    }
}

/// `toml(path)`: the data in a TOML file.
fn toml(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (text, span, id) = load_text(vm, args)?;
    let table = ::toml::de::DeTable::parse(&text).map_err(|err| {
        let message = eco_format!("the file is not valid TOML: {}", err.message().trim_end());
        match err.span() {
            Some(range) => DataError::new(&text, message, range),
            None => DataError {
                message,
                range: None,
            },
        }
        .at(span, id)
    })?;
    Ok(from_toml_table(table.into_inner()))
}

/// The line and column, both from 1, of a byte offset in a text; the
/// column counts characters.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..text.floor_char_boundary(offset.min(text.len()))];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        before.matches('\n').count() + 1,
        before[line_start..].chars().count() + 1,
    )
}

fn from_toml_table(table: ::toml::de::DeTable<'_>) -> Value {
    Value::Dict(
        table
            .into_iter()
            .map(|(k, v)| (EcoString::from(&*k.into_inner()), from_toml(v.into_inner())))
            .collect(),
    )
}

fn from_toml(value: ::toml::de::DeValue<'_>) -> Value {
    use ::toml::de::DeValue;
    match value {
        DeValue::String(s) => Value::Str((&*s).into()),
        DeValue::Integer(i) => {
            let digits = i.as_str().replace('_', "");
            match i64::from_str_radix(&digits, i.radix()) {
                Ok(v) => Value::Int(v),
                Err(_) => float_value(digits.parse().unwrap_or(0.0)),
            }
        }
        DeValue::Float(f) => float_value(f.as_str().replace('_', "").parse().unwrap_or(0.0)),
        DeValue::Boolean(b) => Value::Bool(b),
        DeValue::Datetime(dt) => match (dt.date, dt.time, dt.offset) {
            (Some(d), None, None) => Value::Date(crate::project::Date {
                year: d.year,
                month: d.month,
                day: d.day,
            }),
            _ => Value::Str(dt.to_string().into()),
        },
        DeValue::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| from_toml(item.into_inner()))
                .collect(),
        ),
        DeValue::Table(table) => from_toml_table(table),
    }
}

/// `csv(path, delimiter: ",")`: the rows of a CSV file, as arrays of
/// strings. Quoted fields may contain the delimiter, line breaks and doubled
/// quotes (RFC 4180).
fn csv(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let delimiter: Option<EcoString> = args.named("delimiter")?;
    let (text, span, id) = load_text(vm, args)?;
    let delimiter = match delimiter.as_deref().map(|d| {
        let mut chars = d.chars();
        (chars.next(), chars.next())
    }) {
        None => ',',
        Some((Some(c), None)) => c,
        Some(_) => bail!(span, "the delimiter must be one character"),
    };
    parse_csv(&text, delimiter).map_err(|err| err.at(span, id))
}

fn parse_csv(text: &str, delimiter: char) -> Result<Value, DataError> {
    let mut rows = Array::new();
    let mut row = Array::new();
    let mut field = EcoString::new();
    let mut chars = text.char_indices().peekable();
    // Where the quoted field being read starts.
    let mut quoted = None;
    let mut any = false;
    while let Some((i, c)) = chars.next() {
        any = true;
        if quoted.is_some() {
            match c {
                '"' if chars.peek().is_some_and(|&(_, c)| c == '"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = None,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = Some(i),
            '"' => {
                return Err(DataError::new(
                    text,
                    "a quote in an unquoted field",
                    i..i + 1,
                ));
            }
            c if c == delimiter => row.push(Value::Str(std::mem::take(&mut field))),
            '\r' if chars.peek().is_some_and(|&(_, c)| c == '\n') => {}
            '\n' => {
                row.push(Value::Str(std::mem::take(&mut field)));
                rows.push(Value::Array(std::mem::take(&mut row)));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if let Some(start) = quoted {
        return Err(DataError::new(
            text,
            "a quoted field does not end",
            start..start + 1,
        ));
    }
    if any {
        row.push(Value::Str(field));
        rows.push(Value::Array(row));
    }
    Ok(Value::Array(rows))
}

/// `yaml(path)`: the data in a YAML file: the value of its one document,
/// or `none` when it has none. Mappings keep the order of their keys, and a
/// key that is not a string becomes the string it is written as. Plain
/// scalars are read by the YAML 1.2 core schema: `null`, `true`, integers
/// and floats; quoted ones are strings. An alias is the value of its
/// anchor.
fn yaml(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (text, span, id) = load_text(vm, args)?;
    let (value, repeated) = parse_yaml(&text).map_err(|err| err.at(span, id))?;
    // The file's bytes are counted already; what its aliases repeat is not.
    vm.alloc(repeated.saturating_mul(ITEM), span)?;
    Ok(value)
}

/// A mapping or sequence that is being read, with the anchor it sets and
/// the size of what it holds so far.
enum YamlNode {
    Sequence(Array, usize, u64),
    /// A mapping, with the key whose value comes next, if one came.
    Mapping(Dict, Option<EcoString>, usize, u64),
}

/// What an anchor names: its value, or why a scalar could not be read as
/// one, which is an error only where an alias uses it as a value; its
/// size; and for a scalar the text it is written as, which an alias used
/// as a key takes.
struct YamlAnchor {
    value: Result<Value, EcoString>,
    size: u64,
    text: Option<EcoString>,
}

/// Reads a YAML file's one document. Returns its value and the items its
/// aliases repeat, each alias counting the items of the value it names, so
/// that a file whose aliases repeat each other cannot stand for more items
/// than the memory limit allows.
fn parse_yaml(text: &str) -> Result<(Value, u64), DataError> {
    use saphyr_parser::{Event, Parser, ScalarStyle};

    // The parser counts characters; diagnostics count bytes.
    let byte = |chars: usize| {
        text.char_indices()
            .nth(chars)
            .map_or(text.len(), |(i, _)| i)
    };
    let error = |message: EcoString, span: saphyr_parser::Span| {
        let start = byte(span.start.index());
        let end = byte(span.end.index().max(span.start.index() + 1));
        DataError::new(text, message, start..end)
    };
    let nested = || eco_format!("the data is nested more than {MAX_NESTING} deep");
    let mut stack: Vec<YamlNode> = Vec::new();
    let mut anchors: BTreeMap<usize, YamlAnchor> = BTreeMap::new();
    // The documents' values, and where the second one starts.
    let mut documents: Vec<Value> = Vec::new();
    let mut second = None;
    let mut repeated: u64 = 0;
    for event in Parser::new_from_str(text) {
        let (event, place) = event.map_err(|err| {
            let marker = *err.marker();
            error(
                eco_format!("the file is not valid YAML: {}", err.info()),
                saphyr_parser::Span::new(marker, marker),
            )
        })?;
        let key_next = matches!(stack.last(), Some(YamlNode::Mapping(_, None, ..)));
        // A value that is complete, with the anchor it sets and its size.
        let (value, anchor, size) = match event {
            Event::Scalar(scalar, style, anchor, tag) => {
                let tag = tag.as_deref().filter(|tag| tag.is_yaml_core_schema());
                let value = yaml_scalar(
                    &scalar,
                    style == ScalarStyle::Plain,
                    tag.map(|t| &*t.suffix),
                );
                let written: EcoString = scalar.as_ref().into();
                if anchor > 0 {
                    anchors.insert(
                        anchor,
                        YamlAnchor {
                            value: value.clone(),
                            size: 1,
                            text: Some(written.clone()),
                        },
                    );
                }
                // A key keeps the text it is written as.
                if key_next {
                    if let Some(YamlNode::Mapping(_, key, ..)) = stack.last_mut() {
                        *key = Some(written);
                    }
                    continue;
                }
                (value.map_err(|err| error(err, place))?, 0, 1)
            }
            Event::Alias(id) => {
                let named = anchors
                    .get(&id)
                    .ok_or_else(|| error("an alias names no anchor".into(), place))?;
                if key_next {
                    let key = named
                        .text
                        .clone()
                        .ok_or_else(|| error("a key must be a scalar".into(), place))?;
                    if let Some(YamlNode::Mapping(_, slot, ..)) = stack.last_mut() {
                        *slot = Some(key);
                    }
                    continue;
                }
                let value = named.value.clone().map_err(|err| error(err, place))?;
                if stack.len() as u64 + u64::from(value.depth()) > u64::from(MAX_NESTING) {
                    return Err(error(nested(), place));
                }
                repeated = repeated.saturating_add(named.size);
                (value, 0, named.size)
            }
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => {
                if key_next {
                    return Err(error("a key must be a scalar".into(), place));
                }
                if stack.len() >= MAX_NESTING as usize {
                    return Err(error(nested(), place));
                }
                stack.push(if matches!(event, Event::SequenceStart(..)) {
                    YamlNode::Sequence(Array::new(), anchor, 1)
                } else {
                    YamlNode::Mapping(Dict::new(), None, anchor, 1)
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => match stack.pop() {
                Some(YamlNode::Sequence(items, anchor, size)) => {
                    (Value::Array(items), anchor, size)
                }
                Some(YamlNode::Mapping(map, _, anchor, size)) => (Value::Dict(map), anchor, size),
                None => continue,
            },
            Event::DocumentStart(_) => {
                if documents.len() == 1 {
                    second = Some(place);
                }
                documents.push(Value::None);
                continue;
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => continue,
        };
        if anchor > 0 {
            anchors.insert(
                anchor,
                YamlAnchor {
                    value: Ok(value.clone()),
                    size,
                    text: None,
                },
            );
        }
        match stack.last_mut() {
            Some(YamlNode::Sequence(items, _, total)) => {
                items.push(value);
                *total = total.saturating_add(size);
            }
            Some(YamlNode::Mapping(map, key, _, total)) => {
                let key = key.take().expect("a value follows its key");
                if map.get(&key).is_some() {
                    return Err(error(eco_format!("the key `{key}` is given twice"), place));
                }
                map.insert(key, value);
                *total = total.saturating_add(size);
            }
            None => {
                if let Some(document) = documents.last_mut() {
                    *document = value;
                }
            }
        }
    }
    match documents.len() {
        0 => Ok((Value::None, 0)),
        1 => Ok((documents.pop().expect("there is one"), repeated)),
        n => {
            let message = eco_format!("the file holds {n} YAML documents, and `yaml` reads one");
            Err(match second {
                Some(place) => error(message, place),
                None => DataError {
                    message,
                    range: None,
                },
            })
        }
    }
}

/// A YAML scalar by the core schema: a plain one by its text or its tag
/// (`!!int`), a quoted one as a string unless a tag says otherwise.
fn yaml_scalar(text: &str, plain: bool, tag: Option<&str>) -> StrResult<Value> {
    let int = |text: &str| -> Option<Value> {
        let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
        let (radix, digits) = if let Some(hex) = text.strip_prefix("0x") {
            (16, hex)
        } else if let Some(oct) = text.strip_prefix("0o") {
            (8, oct)
        } else {
            (10, digits)
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // A decimal integer is parsed with its sign, so that the smallest
        // one fits.
        let parsed = if radix == 10 {
            text.parse::<i64>()
        } else {
            i64::from_str_radix(digits, radix)
        };
        Some(match parsed {
            Ok(v) => Value::Int(v),
            // Too large for an integer: a float, as in JSON.
            Err(_) if radix == 10 => float_value(text.parse().ok()?),
            Err(_) => return None,
        })
    };
    let float = |text: &str| -> StrResult<Option<Value>> {
        let unsigned = text.trim_start_matches(['-', '+']);
        let negative = text.starts_with('-');
        if matches!(unsigned, ".inf" | ".Inf" | ".INF") && unsigned.len() + 1 >= text.len() {
            let inf = if negative {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            return Ok(Some(float_value(inf)));
        }
        if matches!(text, ".nan" | ".NaN" | ".NAN") {
            return Err("`.nan` is not a number Folio can hold".into());
        }
        // [-+]? ( \. [0-9]+ | [0-9]+ ( \. [0-9]* )? ) ( [eE] [-+]? [0-9]+ )?
        let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
            Some(i) => (&unsigned[..i], Some(&unsigned[i + 1..])),
            None => (unsigned, None),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
        let valid = unsigned.len() + 1 >= text.len()
            && digits(whole)
            && digits(fraction)
            && (!whole.is_empty() || !fraction.is_empty())
            && (!whole.is_empty() || mantissa.starts_with('.'))
            && exponent.is_none_or(|e| {
                let e = e.strip_prefix(['-', '+']).unwrap_or(e);
                !e.is_empty() && digits(e)
            });
        Ok(valid
            .then(|| text.parse::<f64>().ok())
            .flatten()
            .map(float_value))
    };
    let null = matches!(text, "" | "~" | "null" | "Null" | "NULL");
    let bool = match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    };
    let wrong = |what: &str| eco_format!("`{text}` is not {what}");
    match tag {
        Some("str") => Ok(Value::Str(text.into())),
        Some("null") if null => Ok(Value::None),
        Some("null") => Err(wrong("null")),
        Some("bool") => bool.map(Value::Bool).ok_or_else(|| wrong("a boolean")),
        Some("int") => int(text).ok_or_else(|| wrong("an integer")),
        Some("float") => match int(text) {
            Some(Value::Int(i)) => Ok(float_value(i as f64)),
            Some(v) => Ok(v),
            None => float(text)?.ok_or_else(|| wrong("a float")),
        },
        _ if !plain => Ok(Value::Str(text.into())),
        _ if null => Ok(Value::None),
        _ => {
            if let Some(b) = bool {
                return Ok(Value::Bool(b));
            }
            if let Some(v) = int(text) {
                return Ok(v);
            }
            Ok(float(text)?.unwrap_or_else(|| Value::Str(text.into())))
        }
    }
}

// ---- calc ----

fn num(args: &mut Args, what: &str) -> EvalResult<Value> {
    let span = args.span;
    match args.expect::<Value>(what)? {
        v @ (Value::Int(_) | Value::Float(_)) => Ok(v),
        v => bail!(span, "expected a number, found {}", a_type(v.type_name())),
    }
}

fn as_f64(value: &Value) -> f64 {
    match value {
        Value::Int(i) => *i as f64,
        Value::Float(f) => f.get(),
        _ => 0.0,
    }
}

fn calc_abs(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    match args.expect::<Value>("value")? {
        Value::Int(i) => i
            .checked_abs()
            .map(Value::Int)
            .ok_or_else(|| "value is too large".into())
            .at(span),
        Value::Float(f) => Ok(float_value(f.get().abs())),
        v => ops::neg(v.clone())
            .map(|n| match ops::compare(&v, &n) {
                Ok(Ordering::Less) => n,
                _ => v,
            })
            .at(span),
    }
}

fn unary_float(args: &mut Args, f: fn(f64) -> f64) -> EvalResult<Value> {
    let span = args.span;
    let x = num(args, "value")?;
    real(f(as_f64(&x)), span)
}

fn calc_sqrt(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let x = as_f64(&num(args, "value")?);
    if x < 0.0 {
        bail!(span, "cannot take the square root of a negative number");
    }
    Ok(float_value(x.sqrt()))
}

fn calc_exp(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    unary_float(args, libm::exp)
}

fn calc_sin(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, libm::sin)
}

fn calc_cos(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, libm::cos)
}

fn calc_tan(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, libm::tan)
}

fn angle_or_number(args: &mut Args, f: fn(f64) -> f64) -> EvalResult<Value> {
    let span = args.span;
    let x = match args.expect::<Value>("angle")? {
        Value::Angle(a) => a.0.get(),
        v @ (Value::Int(_) | Value::Float(_)) => as_f64(&v),
        v => bail!(
            span,
            "expected an angle or number, found {}",
            a_type(v.type_name())
        ),
    };
    real(f(x), span)
}

fn calc_log(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let x = as_f64(&num(args, "value")?);
    let base: Option<f64> = args.named("base")?;
    if x <= 0.0 {
        bail!(span, "the logarithm is defined only for positive numbers");
    }
    real(
        match base {
            Some(base) => libm::log(x) / libm::log(base),
            None => libm::log10(x),
        },
        span,
    )
}

fn calc_pow(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let base = num(args, "base")?;
    let exp = num(args, "exponent")?;
    match (&base, &exp) {
        (Value::Int(b), Value::Int(e)) if *e >= 0 => {
            let e = u32::try_from(*e)
                .map_err(|_| EcoString::from("the exponent is too large"))
                .at(span)?;
            b.checked_pow(e)
                .map(Value::Int)
                .ok_or_else(|| "the result is too large".into())
                .at(span)
        }
        _ => real(libm::pow(as_f64(&base), as_f64(&exp)), span),
    }
}

fn rounding(args: &mut Args, f: fn(f64) -> f64) -> EvalResult<Value> {
    let span = args.span;
    match num(args, "value")? {
        Value::Int(i) => Ok(Value::Int(i)),
        v => {
            let x = f(as_f64(&v));
            if x.is_finite() && x.abs() < 9.2e18 {
                Ok(Value::Int(x as i64))
            } else {
                bail!(span, "the result is too large for an integer")
            }
        }
    }
}

fn calc_floor(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    rounding(args, f64::floor)
}

fn calc_ceil(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    rounding(args, f64::ceil)
}

/// `calc.round(x, digits: 0)`: an integer without digits, a float with.
fn calc_round(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let digits: Option<i64> = args.named("digits")?;
    match digits {
        None | Some(0) => rounding(args, f64::round),
        Some(digits) => {
            let x = as_f64(&num(args, "value")?);
            let factor = libm::pow(10.0, digits.clamp(-300, 300) as f64);
            real((x * factor).round() / factor, span)
        }
    }
}

fn int_pair(args: &mut Args) -> EvalResult<(i64, i64, Span)> {
    let span = args.span;
    let a: i64 = args.expect("dividend")?;
    let b: i64 = args.expect("divisor")?;
    if b == 0 {
        bail!(span, "cannot divide by zero");
    }
    Ok((a, b, span))
}

fn calc_rem(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (a, b, span) = int_pair(args)?;
    a.checked_rem(b)
        .map(Value::Int)
        .ok_or_else(|| "the result is too large".into())
        .at(span)
}

/// `calc.quo(a, b)`: the quotient rounded down, as Typst's is; `calc.rem`
/// keeps the sign of the dividend, as Typst's does.
fn calc_quo(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (a, b, span) = int_pair(args)?;
    let (Some(q), Some(r)) = (a.checked_div(b), a.checked_rem(b)) else {
        bail!(span, "the result is too large");
    };
    Ok(Value::Int(if r != 0 && (r < 0) != (b < 0) {
        q - 1
    } else {
        q
    }))
}

fn calc_even(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let n: i64 = args.expect("integer")?;
    Ok(Value::Bool(n % 2 == 0))
}

fn calc_odd(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let n: i64 = args.expect("integer")?;
    Ok(Value::Bool(n % 2 != 0))
}

fn extremum(args: &mut Args, pick: Ordering) -> EvalResult<Value> {
    let span = args.span;
    let mut best: Option<Value> = None;
    while let Some(arg) = args.eat() {
        best = Some(match best {
            None => arg.value,
            Some(b) => {
                if ops::compare(&arg.value, &b).at(arg.span)? == pick {
                    arg.value
                } else {
                    b
                }
            }
        });
    }
    best.ok_or_else(|| EvalError::new(span, "expected at least one value").into())
}

fn calc_min(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    extremum(args, Ordering::Less)
}

fn calc_max(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    extremum(args, Ordering::Greater)
}

fn calc_clamp(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let value: Value = args.expect("value")?;
    let min: Value = args.expect("minimum")?;
    let max: Value = args.expect("maximum")?;
    if ops::compare(&max, &min).at(span)?.is_lt() {
        bail!(span, "the maximum must not be smaller than the minimum");
    }
    Ok(if ops::compare(&value, &min).at(span)?.is_lt() {
        min
    } else if ops::compare(&value, &max).at(span)?.is_gt() {
        max
    } else {
        value
    })
}

// ---- Methods ----

/// The methods that change the value they are called on.
pub(crate) fn is_mutating_method(name: &str) -> bool {
    matches!(name, "push" | "pop" | "insert" | "remove")
}

/// An index into a collection of `len` items; negative from the end.
pub(crate) fn index(len: usize, index: i64) -> StrResult<usize> {
    let resolved = if index < 0 {
        i64::try_from(len)
            .ok()
            .and_then(|len| len.checked_add(index))
    } else {
        Some(index)
    };
    resolved
        .and_then(|i| usize::try_from(i).ok())
        .filter(|&i| i < len)
        .ok_or_else(|| eco_format!("index out of bounds (index: {index}, len: {len})"))
}

/// Calls a method that changes its target.
pub(crate) fn call_mutating(
    target: &mut Value,
    name: &str,
    mut args: Args,
    span: Span,
) -> EvalResult<Value> {
    let result = match target {
        Value::Array(items) => match name {
            "push" => {
                items.push(args.expect("value")?);
                Value::None
            }
            "pop" => match items.pop() {
                Some(v) => v,
                None => bail!(span, "the array is empty"),
            },
            "insert" => {
                let i: i64 = args.expect("index")?;
                let value: Value = args.expect("value")?;
                let at = if i == items.len() as i64 {
                    items.len()
                } else {
                    index(items.len(), i).at(span)?
                };
                items.insert(at, value);
                Value::None
            }
            _ => {
                let i: i64 = args.expect("index")?;
                let at = index(items.len(), i).at(span)?;
                items.remove(at)
            }
        },
        Value::Dict(dict) => match name {
            "insert" => {
                let key: EcoString = args.expect("key")?;
                let value: Value = args.expect("value")?;
                dict.insert(key, value);
                Value::None
            }
            "remove" => {
                let key: EcoString = args.expect("key")?;
                match dict.remove(&key) {
                    Some(v) => v,
                    None => bail!(span, "dictionary does not contain key {key:?}"),
                }
            }
            _ => bail!(span, "a dictionary has no method `{name}`"),
        },
        v => bail!(span, "{} has no method `{name}`", a_type(v.type_name())),
    };
    args.finish()?;
    Ok(result)
}

/// Calls a method that does not change its target.
pub(crate) fn call_method(
    vm: &mut Vm<'_>,
    target: Value,
    name: &str,
    mut args: Args,
    span: Span,
) -> EvalResult<Value> {
    if let Value::Args(a) = &target
        && matches!(name, "pos" | "named")
    {
        vm.alloc(NODE * a.items.len() as u64, span)?;
    }
    let result = match target {
        Value::Str(s) => str_method(vm, s, name, &mut args, span)?,
        Value::Array(items) => array_method(vm, items, name, &mut args, span)?,
        Value::Dict(dict) => dict_method(vm, dict, name, &mut args, span)?,
        Value::Content(content) => match (name, content.as_element()) {
            ("fields", Some(element)) => Value::Dict(element.fields.clone()),
            ("at", Some(element)) => {
                let field: EcoString = args.expect("field")?;
                let default: Option<Value> = args.named("default")?;
                match (element.field(&field), default) {
                    (Some(v), _) => v.clone(),
                    (None, Some(d)) => d,
                    (None, None) => bail!(span, "`{}` has no field `{field}`", element.name),
                }
            }
            ("fields" | "at", None) => {
                bail!(span, "only content that is one element has fields")
            }
            _ => bail!(span, "content has no method `{name}`"),
        },
        Value::Func(func) if name == "with" => {
            let applied = std::mem::take(&mut args.items);
            args.changed();
            Value::Func(Func::With(std::sync::Arc::new(Applied::new(
                func,
                Args::new(args.span, applied),
            ))))
        }
        Value::Args(a) => match name {
            "pos" => Value::Array(
                a.items
                    .iter()
                    .filter(|arg| arg.name.is_none())
                    .map(|arg| arg.value.clone())
                    .collect(),
            ),
            "named" => Value::Dict(
                a.items
                    .iter()
                    .filter_map(|arg| Some((arg.name.clone()?, arg.value.clone())))
                    .collect(),
            ),
            _ => bail!(span, "arguments have no method `{name}`"),
        },
        Value::Date(date) if name == "display" => Value::Str(eco_format!("{date}")),
        Value::Length(length) if length.em.is_zero() => {
            let pt = length.abs.to_pt();
            float_value(match name {
                "pt" => pt,
                "mm" => pt * 25.4 / 72.0,
                "cm" => pt * 2.54 / 72.0,
                "inches" => pt / 72.0,
                _ => bail!(span, "a length has no method `{name}`"),
            })
        }
        v => bail!(span, "{} has no method `{name}`", a_type(v.type_name())),
    };
    args.finish()?;
    Ok(result)
}

fn str_method(
    vm: &mut Vm<'_>,
    s: EcoString,
    name: &str,
    args: &mut Args,
    span: Span,
) -> EvalResult<Value> {
    Ok(match name {
        "len" => Value::Int(s.len() as i64),
        "first" | "last" => {
            let c = if name == "first" {
                s.chars().next()
            } else {
                s.chars().next_back()
            };
            match c {
                Some(c) => Value::Str(c.into()),
                None => bail!(span, "the string is empty"),
            }
        }
        "at" => {
            let i: i64 = args.expect("index")?;
            let default: Option<Value> = args.named("default")?;
            match (index(s.chars().count(), i), default) {
                (Ok(i), _) => Value::Str(s.chars().nth(i).unwrap_or_default().into()),
                (Err(_), Some(d)) => d,
                (Err(e), None) => return Err(EvalError::new(span, e).into()),
            }
        }
        "slice" => {
            let (start, end) = slice_bounds(args, s.chars().count(), span)?;
            let chars = || s.chars().skip(start).take(end - start);
            vm.alloc(chars().map(|c| c.len_utf8() as u64).sum(), span)?;
            Value::Str(chars().collect::<String>().into())
        }
        "contains" => Value::Bool(s.contains(args.expect::<EcoString>("pattern")?.as_str())),
        "starts-with" => Value::Bool(s.starts_with(args.expect::<EcoString>("pattern")?.as_str())),
        "ends-with" => Value::Bool(s.ends_with(args.expect::<EcoString>("pattern")?.as_str())),
        "find" => {
            let pattern: EcoString = args.expect("pattern")?;
            if s.contains(pattern.as_str()) {
                Value::Str(pattern)
            } else {
                Value::None
            }
        }
        "position" => {
            let pattern: EcoString = args.expect("pattern")?;
            match s.find(pattern.as_str()) {
                Some(byte) => Value::Int(s[..byte].chars().count() as i64),
                None => Value::None,
            }
        }
        "split" => {
            let sep: Option<EcoString> = args.find()?;
            let parts = match &sep {
                Some(sep) if sep.is_empty() => {
                    bail!(span, "the separator must not be empty")
                }
                Some(sep) => s.matches(sep.as_str()).count() + 1,
                None => s.split_whitespace().count(),
            };
            vm.alloc((ITEM * parts as u64).saturating_add(s.len() as u64), span)?;
            Value::Array(match &sep {
                Some(sep) => s
                    .split(sep.as_str())
                    .map(|p| Value::Str(p.into()))
                    .collect(),
                None => s.split_whitespace().map(|p| Value::Str(p.into())).collect(),
            })
        }
        "trim" => {
            vm.alloc(s.trim().len() as u64, span)?;
            Value::Str(s.trim().into())
        }
        "upper" | "lower" => {
            // Changing case can lengthen a character to at most three.
            vm.alloc(3 * s.len() as u64, span)?;
            Value::Str(if name == "upper" {
                s.to_uppercase()
            } else {
                s.to_lowercase()
            })
        }
        "rev" => {
            vm.alloc(s.len() as u64, span)?;
            Value::Str(s.chars().rev().collect::<String>().into())
        }
        "codepoints" => {
            vm.alloc(
                (ITEM * s.chars().count() as u64).saturating_add(s.len() as u64),
                span,
            )?;
            Value::Array(s.chars().map(|c| Value::Str(c.into())).collect())
        }
        "replace" => {
            let from: EcoString = args.expect("pattern")?;
            let to: EcoString = args.expect("replacement")?;
            if from.is_empty() {
                bail!(span, "the pattern must not be empty");
            }
            // The result is a new string, with each match replaced.
            let count = s.matches(from.as_str()).count() as u64;
            let kept = (s.len() as u64).saturating_sub(count.saturating_mul(from.len() as u64));
            vm.alloc(
                kept.saturating_add(count.saturating_mul(to.len() as u64)),
                span,
            )?;
            Value::Str(s.replace(from.as_str(), &to))
        }
        _ => bail!(span, "a string has no method `{name}`"),
    })
}

/// The `start` and `end` (or `count:`) of a slice, resolved.
fn slice_bounds(args: &mut Args, len: usize, span: Span) -> EvalResult<(usize, usize)> {
    let start: i64 = args.expect("start")?;
    let end: Option<i64> = args.find()?;
    let count: Option<i64> = args.named("count")?;
    let start = if start == len as i64 {
        len
    } else {
        index(len, start).at(span)?
    };
    let end = match (end, count) {
        (Some(end), _) if end == len as i64 => len,
        (Some(end), _) => index(len, end).at(span)?,
        (None, Some(count)) => usize::try_from(count)
            .ok()
            .and_then(|c| start.checked_add(c))
            .filter(|&e| e <= len)
            .ok_or_else(|| "the count is out of bounds".into())
            .at(span)?,
        (None, None) => len,
    };
    if end < start {
        bail!(span, "the end of the slice is before its start");
    }
    Ok((start, end))
}

/// Calls a function with one or two values as positional arguments.
fn call_with(vm: &mut Vm<'_>, func: &Func, values: &[Value], span: Span) -> EvalResult<Value> {
    let args = Args::new(
        span,
        values
            .iter()
            .map(|value| Arg {
                span,
                name: None,
                scope: None,
                value: value.clone(),
                spelling: None,
            })
            .collect(),
    );
    vm.call(func.clone(), args, span)
}

fn predicate(vm: &mut Vm<'_>, func: &Func, value: &Value, span: Span) -> EvalResult<bool> {
    match call_with(vm, func, std::slice::from_ref(value), span)? {
        Value::Bool(b) => Ok(b),
        v => bail!(
            span,
            "the function must return a boolean, not {}",
            a_type(v.type_name())
        ),
    }
}

fn array_method(
    vm: &mut Vm<'_>,
    items: Array,
    name: &str,
    args: &mut Args,
    span: Span,
) -> EvalResult<Value> {
    Ok(match name {
        "len" => Value::Int(items.len() as i64),
        "first" | "last" => {
            let item = if name == "first" {
                items.first()
            } else {
                items.last()
            };
            match item {
                Some(v) => v.clone(),
                None => bail!(span, "the array is empty"),
            }
        }
        "at" => {
            let i: i64 = args.expect("index")?;
            let default: Option<Value> = args.named("default")?;
            match (index(items.len(), i), default) {
                (Ok(i), _) => items[i].clone(),
                (Err(_), Some(d)) => d,
                (Err(e), None) => return Err(EvalError::new(span, e).into()),
            }
        }
        "slice" => {
            let (start, end) = slice_bounds(args, items.len(), span)?;
            vm.alloc(ITEM * (end - start) as u64, span)?;
            Value::Array(items[start..end].iter().cloned().collect())
        }
        "contains" => {
            let value: Value = args.expect("value")?;
            Value::Bool(items.iter().any(|item| ops::equal(item, &value)))
        }
        "find" | "position" => {
            let func: Func = args.expect("function")?;
            let mut found = Value::None;
            for (i, item) in items.iter().enumerate() {
                if predicate(vm, &func, item, span)? {
                    found = if name == "find" {
                        item.clone()
                    } else {
                        Value::Int(i as i64)
                    };
                    break;
                }
            }
            found
        }
        "filter" => {
            let func: Func = args.expect("function")?;
            let mut out = Array::new();
            for item in items.iter() {
                if predicate(vm, &func, item, span)? {
                    vm.alloc(ITEM, span)?;
                    out.push(item.clone());
                }
            }
            Value::Array(out)
        }
        // Each call is an iteration of the call (SPEC §6.1), so the
        // instances it makes pair across variants by item.
        "map" => {
            let func: Func = args.expect("function")?;
            vm.alloc(ITEM * items.len() as u64, span)?;
            let mut out = Array::with_capacity(items.len());
            let outer = vm.origin;
            for (i, item) in items.iter().enumerate() {
                vm.origin = outer.within(Frame::Iteration {
                    span,
                    index: i as u64,
                    item: item_key(item),
                });
                let value = call_with(vm, &func, std::slice::from_ref(item), span);
                vm.origin = outer;
                out.push(value?);
            }
            Value::Array(out)
        }
        // Each call is an iteration, as for `map`; anchoring a call
        // reaches only what it made, not the accumulator it was given.
        "fold" => {
            let mut acc: Value = args.expect("initial value")?;
            let func: Func = args.expect("function")?;
            let outer = vm.origin;
            for (i, item) in items.iter().enumerate() {
                vm.origin = outer.within(Frame::Iteration {
                    span,
                    index: i as u64,
                    item: item_key(item),
                });
                let result = call_with(vm, &func, &[acc, item.clone()], span);
                vm.origin = outer;
                acc = result?;
            }
            acc
        }
        "any" | "all" => {
            let func: Func = args.expect("function")?;
            let want = name == "any";
            let mut result = !want;
            for item in items.iter() {
                if predicate(vm, &func, item, span)? == want {
                    result = want;
                    break;
                }
            }
            Value::Bool(result)
        }
        "sum" | "product" => {
            let default: Option<Value> = args.named("default")?;
            let mut iter = items.into_iter();
            let Some(mut acc) = iter.next() else {
                return match default {
                    Some(d) => Ok(d),
                    None => bail!(span, "cannot {name} an empty array without a default"),
                };
            };
            let op = if name == "sum" {
                ast::BinOp::Add
            } else {
                ast::BinOp::Mul
            };
            for item in iter {
                acc = vm.apply(op, acc, item, span)?;
            }
            acc
        }
        "rev" => {
            vm.alloc(ITEM * items.len() as u64, span)?;
            Value::Array(items.iter().rev().cloned().collect())
        }
        "join" => {
            let sep: Option<Value> = args.find()?;
            let mut acc = Value::None;
            for (i, item) in items.into_iter().enumerate() {
                if i > 0
                    && let Some(sep) = &sep
                {
                    vm.alloc(sep.shallow_size(), span)?;
                    acc = ops::join(acc, sep.clone()).at(span)?;
                }
                vm.alloc(item.shallow_size(), span)?;
                acc = ops::join(acc, item).at(span)?;
            }
            acc
        }
        "sorted" => {
            let key: Option<Func> = args.named("key")?;
            // The items with their keys, and then the sorted array.
            vm.alloc(3 * ITEM * items.len() as u64, span)?;
            let mut keyed = Vec::with_capacity(items.len());
            for item in items.iter() {
                let k = match &key {
                    Some(f) => call_with(vm, f, std::slice::from_ref(item), span)?,
                    None => item.clone(),
                };
                keyed.push((k, item.clone()));
            }
            let mut error = None;
            keyed.sort_by(|(a, _), (b, _)| {
                ops::compare(a, b).unwrap_or_else(|e| {
                    error.get_or_insert(e);
                    Ordering::Equal
                })
            });
            if let Some(e) = error {
                return Err(EvalError::new(span, e).into());
            }
            Value::Array(keyed.into_iter().map(|(_, v)| v).collect())
        }
        "enumerate" => {
            let start: i64 = args.named("start")?.unwrap_or(0);
            vm.alloc(3 * ITEM * items.len() as u64, span)?;
            Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        Value::Array(Array::from([
                            Value::Int(start.saturating_add(i as i64)),
                            v.clone(),
                        ]))
                    })
                    .collect(),
            )
        }
        "zip" => {
            let other: Array = args.expect("array")?;
            vm.alloc(3 * ITEM * items.len().min(other.len()) as u64, span)?;
            Value::Array(
                items
                    .iter()
                    .zip(other.iter())
                    .map(|(a, b)| Value::Array(Array::from([a.clone(), b.clone()])))
                    .collect(),
            )
        }
        "flatten" => {
            let mut out = Array::new();
            flatten(vm, &items, &mut out, span)?;
            Value::Array(out)
        }
        _ => bail!(span, "an array has no method `{name}`"),
    })
}

/// Appends the items of `items` and of the arrays inside it to `out`.
///
/// An array can hold one array many times, as `(a, a)` does, so the walk
/// can be exponentially longer than the memory the array takes. It counts
/// every item it visits, arrays too, before it goes on.
fn flatten(vm: &mut Vm<'_>, items: &Array, out: &mut Array, span: Span) -> EvalResult<()> {
    for item in items {
        vm.alloc(ITEM, span)?;
        match item {
            Value::Array(inner) => flatten(vm, inner, out, span)?,
            v => out.push(v.clone()),
        }
    }
    Ok(())
}

fn dict_method(
    vm: &mut Vm<'_>,
    dict: Dict,
    name: &str,
    args: &mut Args,
    span: Span,
) -> EvalResult<Value> {
    if matches!(name, "keys" | "values" | "pairs") {
        let per = if name == "pairs" { 3 * ITEM } else { ITEM };
        vm.alloc(per * dict.len() as u64, span)?;
    }
    Ok(match name {
        "len" => Value::Int(dict.len() as i64),
        "at" => {
            let key: EcoString = args.expect("key")?;
            let default: Option<Value> = args.named("default")?;
            match (dict.get(&key), default) {
                (Some(v), _) => v.clone(),
                (None, Some(d)) => d,
                (None, None) => bail!(span, "dictionary does not contain key {key:?}"),
            }
        }
        "keys" => Value::Array(dict.iter().map(|(k, _)| Value::Str(k.clone())).collect()),
        "values" => Value::Array(dict.iter().map(|(_, v)| v.clone()).collect()),
        "pairs" => Value::Array(
            dict.iter()
                .map(|(k, v)| Value::Array(Array::from([Value::Str(k.clone()), v.clone()])))
                .collect(),
        ),
        _ => bail!(span, "a dictionary has no method `{name}`"),
    })
}

#[cfg(test)]
mod tests {
    use folio_component::Name;

    use super::*;

    #[test]
    fn permutations_are_fixed_by_their_seed() {
        // The orders are part of the language: a version printed once must
        // come out the same from every later compiler (SPEC §6.4).
        assert_eq!(permutation(b"A", 6), [2, 5, 1, 3, 4, 0]);
        assert_eq!(permutation(b"B", 6), [1, 0, 5, 2, 4, 3]);
        assert_eq!(permutation(b"B", 10), [8, 2, 3, 0, 9, 1, 5, 4, 7, 6]);
        assert_eq!(permutation(b"", 0), Vec::<usize>::new());
        assert_eq!(permutation(b"A", 1), [0]);
        for seed in [&b"A"[..], b"B", b"version 3", b"2"] {
            for len in [2, 5, 13, 100] {
                let order = permutation(seed, len);
                assert_eq!(order, permutation(seed, len), "the same twice");
                let mut sorted = order.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, (0..len).collect::<Vec<_>>(), "a permutation");
            }
        }
        assert_ne!(permutation(b"A", 10), permutation(b"B", 10));
    }

    #[test]
    fn csv_quotes_and_line_breaks() {
        let rows = parse_csv("a,\"b,c\"\r\n\"x\"\"y\",\"two\nlines\"\n", ',').unwrap();
        assert_eq!(rows.repr(), r#"(("a", "b,c"), ("x\"y", "two\nlines"))"#);
        assert!(parse_csv("a,\"b\n", ',').is_err());
        assert!(parse_csv("a\"b\n", ',').is_err());
        assert_eq!(parse_csv("", ',').unwrap().repr(), "()");
    }

    #[test]
    fn yaml_values() {
        let value = parse_yaml(
            "z: 1\na: [true, ~, 0x1f, -2.5e3, .inf, 'quoted 1', \"x\"]\n\
             m:\n  y: !!str 12\n  2: plain text\n  b: !!float 3\n",
        )
        .unwrap()
        .0;
        assert_eq!(
            value.repr(),
            r#"(z: 1, a: (true, none, 31, -2500.0, inf, "quoted 1", "x"), m: (y: "12", "2": "plain text", b: 3.0))"#
        );
        // Anchors and aliases.
        let value = parse_yaml("base: &b {k: 1}\ncopy: *b\n").unwrap().0;
        assert_eq!(value.repr(), "(base: (k: 1), copy: (k: 1))");
        // Documents.
        assert_eq!(parse_yaml("").unwrap().0.repr(), "none");
        assert_eq!(parse_yaml("--- 3\n").unwrap().0.repr(), "3");
        assert_eq!(
            parse_yaml("--- 1\n--- 2\n").unwrap_err().message,
            "the file holds 2 YAML documents, and `yaml` reads one (line 2, column 1)"
        );
        // Errors say where.
        assert_eq!(
            parse_yaml("a: 1\na: 2\n").unwrap_err().message,
            "the key `a` is given twice (line 2, column 4)"
        );
        assert!(
            parse_yaml("a: [1, 2\n")
                .unwrap_err()
                .message
                .starts_with("the file is not valid YAML: ")
        );
        assert_eq!(
            parse_yaml("? [1]\n: x\n").unwrap_err().message,
            "a key must be a scalar (line 1, column 3)"
        );
        assert_eq!(
            parse_yaml("x: .nan\n").unwrap_err().message,
            "`.nan` is not a number Folio can hold (line 1, column 4)"
        );
        assert_eq!(
            parse_yaml("x: !!int abc\n").unwrap_err().message,
            "`abc` is not an integer (line 1, column 4)"
        );
        // Keys keep the text they are written as, aliases included.
        let value = parse_yaml("&h 0x1f: a\n.nan: b\n*h : c\n")
            .unwrap_err()
            .message;
        assert_eq!(value, "the key `0x1f` is given twice (line 3, column 6)");
        let value = parse_yaml("k: &f 1.5\n*f : y\n&n ~: z\n.nan: w\n")
            .unwrap()
            .0;
        assert_eq!(
            value.repr(),
            r#"(k: 1.5, "1.5": "y", "~": "z", ".nan": "w")"#
        );
        // An alias counts as the items it repeats.
        let mut laughs = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..9 {
            let refs = vec![format!("*a{}", i - 1); 10].join(", ");
            laughs.push_str(&format!("a{i}: &a{i} [{refs}]\n"));
        }
        let (_, repeated) = parse_yaml(&laughs).unwrap();
        assert!(repeated > 1_000_000_000, "{repeated}");
        // A file without aliases repeats nothing.
        assert_eq!(parse_yaml("[1, [2, 3], {a: 4}]").unwrap().1, 0);
        // An alias cannot nest a value beyond the limit.
        let deep = |name: &str, inner: &str| {
            format!(
                "{name}: &{name} {}{inner}{}\n",
                "[".repeat(60),
                "]".repeat(60)
            )
        };
        let file = deep("a", "1") + &deep("b", "*a");
        assert!(
            parse_yaml(&file)
                .unwrap_err()
                .message
                .starts_with("the data is nested more than 100 deep")
        );
        // The second document is where a file holding two goes wrong.
        assert_eq!(parse_yaml("1\n---\n2\n").unwrap_err().range, Some(2..5));
        // Nesting stops at the limit, however deep the file goes.
        assert!(parse_yaml(&"[".repeat(100_000)).is_err());
        let block: String = (0..150).map(|i| format!("{}-\n", "  ".repeat(i))).collect();
        assert!(
            parse_yaml(&block)
                .unwrap_err()
                .message
                .starts_with("the data is nested more than 100 deep")
        );
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_yaml(&ok).is_ok());
    }

    proptest::proptest! {
        /// Any text, however malformed, gives a value or an error.
        #[test]
        fn yaml_never_panics(text in r"([-:?,&*!#'\x22\[\]{}|>~. \n\t0-9a-z]|\x22\\u00e9\x22|---|\.\.\.){0,200}") {
            let _ = parse_yaml(&text);
        }
    }

    #[test]
    fn yaml_scalars_by_the_core_schema() {
        let repr = |text: &str| yaml_scalar(text, true, None).unwrap().repr();
        assert_eq!(repr("12"), "12");
        assert_eq!(repr("+12"), "12");
        assert_eq!(repr("-0o17"), r#""-0o17""#);
        assert_eq!(repr("0o17"), "15");
        assert_eq!(repr("1.5"), "1.5");
        assert_eq!(repr(".5"), "0.5");
        assert_eq!(repr("5."), "5.0");
        assert_eq!(repr("1e3"), "1000.0");
        assert_eq!(repr("99999999999999999999"), "1e20");
        assert_eq!(repr("-.INF"), "-inf");
        assert_eq!(repr("1_000"), r#""1_000""#);
        assert_eq!(repr("-9223372036854775808"), "-9223372036854775808");
        assert_eq!(repr("yes"), r#""yes""#);
        assert_eq!(repr("."), r#"".""#);
        assert_eq!(repr("e3"), r#""e3""#);
        assert_eq!(repr("Null"), "none");
        assert_eq!(yaml_scalar("12", false, None).unwrap().repr(), r#""12""#);
    }

    #[test]
    fn line_columns() {
        assert_eq!(line_column("ab\nčd", 0), (1, 1));
        assert_eq!(line_column("ab\nčd", 3), (2, 1));
        assert_eq!(line_column("ab\nčd", 5), (2, 2));
        assert_eq!(line_column("ab", 99), (1, 3));
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex("1f5fa8").unwrap(), Color::rgb(0x1f, 0x5f, 0xa8));
        assert_eq!(parse_hex("#fff").unwrap(), Color::WHITE);
        assert!(parse_hex("12345").is_err());
        assert!(parse_hex("zz0000").is_err());
    }

    #[test]
    fn indices() {
        assert_eq!(index(3, -1), Ok(2));
        assert_eq!(index(3, 0), Ok(0));
        assert!(index(3, 3).is_err());
        assert!(index(3, -4).is_err());
        assert!(index(0, i64::MIN).is_err());
    }

    #[test]
    fn names() {
        for (name, _) in GLOBALS.iter().chain(CALC) {
            assert!(
                Name::new(name.trim_start_matches("calc.")).is_some(),
                "{name}"
            );
        }
    }
}

/// What a name means in an equation, where the script's own bindings do
/// not define it (SPEC §8.1): a symbol, an operator, the differential, or
/// a math function.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum MathItem {
    /// A symbol: `alpha`, `sum`, `arrow.r`.
    Symbol(char),
    /// A named operator, set upright: `sin`, and `lim`, whose limits go
    /// below and above it in display style.
    Operator {
        /// Whether its attachments are limits.
        limits: bool,
    },
    /// The differential of `dif t`.
    Differential,
    /// A function that builds a node of the math tree when called:
    /// `frac(a, b)`.
    Func(MathFunc),
}

/// The functions of native math, each building a node of the math tree.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum MathFunc {
    /// `frac(num, den)`.
    Frac,
    /// `sqrt(x)`.
    Sqrt,
    /// `root(index, x)`.
    Root,
    /// `vec(F)`, a vector.
    Vector,
    /// `hat(i)`, a unit vector.
    UnitVector,
    /// `qty(9.81, "m s^-2", pm: 0.02, digits: 3)`, a quantity.
    Quantity,
    /// `unit("kg m^2")`, a unit alone.
    Unit,
    /// `num(6.02e23, pm: .., digits: ..)`, a number alone.
    Num,
    /// `mat(a, b; c, d, delim: "[")`.
    Matrix,
    /// `cases(a, b)`.
    Cases,
    /// A body between fixed delimiters: `abs(x)`, `norm(x)`, `floor(x)`,
    /// `ceil(x)`.
    Fence(char, char),
    /// `accent(x, "~")`: an accent given as an argument.
    Accent,
    /// An accent by name, as its combining character: `tilde(x)`.
    NamedAccent(char),
    /// A line or brace over or under a body: `overline(x)`,
    /// `underbrace(x, n)`.
    Decoration(folio_math::DecorationKind),
    /// A body in another alphabet: `bold(x)`.
    Styled(folio_math::MathVariant),
}

/// The functions of native math, by name.
const MATH_FUNCS: &[(&str, MathFunc)] = {
    use folio_math::DecorationKind as D;
    use folio_math::MathVariant as V;
    &[
        ("frac", MathFunc::Frac),
        ("sqrt", MathFunc::Sqrt),
        ("root", MathFunc::Root),
        ("vec", MathFunc::Vector),
        ("hat", MathFunc::UnitVector),
        ("qty", MathFunc::Quantity),
        ("unit", MathFunc::Unit),
        ("num", MathFunc::Num),
        ("mat", MathFunc::Matrix),
        ("cases", MathFunc::Cases),
        ("abs", MathFunc::Fence('|', '|')),
        ("norm", MathFunc::Fence('‖', '‖')),
        ("floor", MathFunc::Fence('⌊', '⌋')),
        ("ceil", MathFunc::Fence('⌈', '⌉')),
        ("accent", MathFunc::Accent),
        ("grave", MathFunc::NamedAccent('\u{300}')),
        ("acute", MathFunc::NamedAccent('\u{301}')),
        ("tilde", MathFunc::NamedAccent('\u{303}')),
        ("macron", MathFunc::NamedAccent('\u{304}')),
        ("breve", MathFunc::NamedAccent('\u{306}')),
        ("dot", MathFunc::NamedAccent('\u{307}')),
        ("dot.double", MathFunc::NamedAccent('\u{308}')),
        ("dot.triple", MathFunc::NamedAccent('\u{20DB}')),
        ("circle", MathFunc::NamedAccent('\u{30A}')),
        ("caron", MathFunc::NamedAccent('\u{30C}')),
        ("arrow", MathFunc::NamedAccent('\u{20D7}')),
        ("overline", MathFunc::Decoration(D::Overline)),
        ("underline", MathFunc::Decoration(D::Underline)),
        ("overbrace", MathFunc::Decoration(D::Overbrace)),
        ("underbrace", MathFunc::Decoration(D::Underbrace)),
        ("overbracket", MathFunc::Decoration(D::Overbracket)),
        ("underbracket", MathFunc::Decoration(D::Underbracket)),
        ("upright", MathFunc::Styled(V::Upright)),
        ("italic", MathFunc::Styled(V::Italic)),
        ("bold", MathFunc::Styled(V::Bold)),
        ("cal", MathFunc::Styled(V::Calligraphic)),
        ("frak", MathFunc::Styled(V::Fraktur)),
        ("bb", MathFunc::Styled(V::DoubleStruck)),
        ("sans", MathFunc::Styled(V::SansSerif)),
        ("mono", MathFunc::Styled(V::Monospace)),
    ]
};

/// What a name means in an equation (SPEC §8.1). A name that is both a
/// function and a symbol, such as `dot`, is the function where it is
/// `called` and the symbol elsewhere.
pub(crate) fn math_item(name: &str, called: bool) -> Option<MathItem> {
    let func = || {
        MATH_FUNCS
            .iter()
            .find(|&&(n, _)| n == name)
            .map(|&(_, func)| MathItem::Func(func))
    };
    if called && let Some(item) = func() {
        return Some(item);
    }
    if name == "dif" {
        return Some(MathItem::Differential);
    }
    if let Some(limits) = folio_math::operator(name) {
        return Some(MathItem::Operator { limits });
    }
    if let Some(c) = folio_math::symbol(name) {
        return Some(MathItem::Symbol(c));
    }
    func()
}
