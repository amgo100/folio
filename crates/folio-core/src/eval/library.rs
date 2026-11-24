//! The standard library: global functions, the `calc` module, methods of
//! values, data loading, the built-in elements content can call, and the
//! elements of the project's components.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Color, QualifiedName, Scalar};
use folio_syntax::Span;

use super::content::Content;
use super::error::{At, EvalError, EvalResult, StrResult, bail};
use super::ops::{self, a_type};
use super::value::{Arg, Args, Array, Dict, Func, Module, NativeFunc, Scope, Value};
use super::vm::{FromValue, NativeFn, Vm};
use crate::component::ComponentHost;
use crate::db::{Bytes, data_file, source_file};

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
        let namespace = component.manifest.namespace();
        let elements: BTreeMap<EcoString, Value> = component
            .declarations
            .elements
            .iter()
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
    ("panic", panic),
    ("range", range),
    ("read", read),
    ("repr", repr),
    ("rgb", rgb),
    ("str", str),
    ("toml", toml),
    ("type", type_),
];

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
fn repr(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let value: Value = args.expect("value")?;
    Ok(Value::Str(value.repr()))
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
fn panic(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let span = args.span;
    let mut message = EcoString::from("panicked");
    let mut first = true;
    while let Some(arg) = args.eat() {
        message.push_str(if first { " with: " } else { ", " });
        first = false;
        match arg.value {
            Value::Str(s) => message.push_str(&s),
            v => message.push_str(&v.repr()),
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
        u64::try_from(len.saturating_mul(16)).unwrap_or(u64::MAX),
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
fn load(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<(Bytes, Span)> {
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
    let bytes = if let Some(file) = data_file(vm.db, vm.project, path.clone()) {
        file.bytes(vm.db).clone()
    } else if let Some(file) = source_file(vm.db, vm.project, path.clone()) {
        Bytes::from(file.source(vm.db).text().as_bytes())
    } else {
        bail!(span, "file `{}` not found", path.as_str());
    };
    vm.alloc(bytes.len() as u64, span)?;
    Ok((bytes, span))
}

fn load_text(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<(EcoString, Span)> {
    let (bytes, span) = load(vm, args)?;
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok((text.into(), span)),
        Err(_) => bail!(span, "the file is not valid UTF-8 text"),
    }
}

/// `read(path)`: a file's text.
fn read(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    load_text(vm, args).map(|(text, _)| Value::Str(text))
}

/// `json(path)`: the data in a JSON file.
fn json(vm: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    let (bytes, span) = load(vm, args)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|err| eco_format!("the file is not valid JSON: {err}"))
        .at(span)?;
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
    let (text, span) = load_text(vm, args)?;
    let table = ::toml::de::DeTable::parse(&text)
        .map_err(|err| eco_format!("the file is not valid TOML: {}", err.message().trim_end()))
        .at(span)?;
    Ok(from_toml_table(table.into_inner()))
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
    let (text, span) = load_text(vm, args)?;
    let delimiter = match delimiter.as_deref().map(|d| {
        let mut chars = d.chars();
        (chars.next(), chars.next())
    }) {
        None => ',',
        Some((Some(c), None)) => c,
        Some(_) => bail!(span, "the delimiter must be one character"),
    };
    parse_csv(&text, delimiter).at(span)
}

fn parse_csv(text: &str, delimiter: char) -> StrResult<Value> {
    let mut rows = Array::new();
    let mut row = Array::new();
    let mut field = EcoString::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut line = 1;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push(c);
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            '"' => return Err(eco_format!("a quote in an unquoted field in line {line}")),
            c if c == delimiter => row.push(Value::Str(std::mem::take(&mut field))),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                row.push(Value::Str(std::mem::take(&mut field)));
                rows.push(Value::Array(std::mem::take(&mut row)));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("a quoted field does not end".into());
    }
    if any {
        row.push(Value::Str(field));
        rows.push(Value::Array(row));
    }
    Ok(Value::Array(rows))
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
    unary_float(args, f64::exp)
}

fn calc_sin(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, f64::sin)
}

fn calc_cos(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, f64::cos)
}

fn calc_tan(_: &mut Vm<'_>, args: &mut Args) -> EvalResult<Value> {
    angle_or_number(args, f64::tan)
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
            Some(base) => x.log(base),
            None => x.log10(),
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
        _ => real(as_f64(&base).powf(as_f64(&exp)), span),
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
            let factor = 10f64.powi(i32::try_from(digits.clamp(-300, 300)).unwrap_or(0));
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
    let result = match target {
        Value::Str(s) => str_method(vm, s, name, &mut args, span)?,
        Value::Array(items) => array_method(vm, items, name, &mut args, span)?,
        Value::Dict(dict) => dict_method(dict, name, &mut args, span)?,
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
            Value::Func(Func::With(std::sync::Arc::new((
                func,
                Args {
                    span: args.span,
                    items: applied,
                },
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
    let chars: Vec<char> = s.chars().collect();
    Ok(match name {
        "len" => Value::Int(s.len() as i64),
        "first" | "last" => {
            let c = if name == "first" {
                chars.first()
            } else {
                chars.last()
            };
            match c {
                Some(c) => Value::Str((*c).into()),
                None => bail!(span, "the string is empty"),
            }
        }
        "at" => {
            let i: i64 = args.expect("index")?;
            let default: Option<Value> = args.named("default")?;
            match (index(chars.len(), i), default) {
                (Ok(i), _) => Value::Str(chars[i].into()),
                (Err(_), Some(d)) => d,
                (Err(e), None) => return Err(EvalError::new(span, e).into()),
            }
        }
        "slice" => {
            let (start, end) = slice_bounds(args, chars.len(), span)?;
            Value::Str(chars[start..end].iter().collect::<String>().into())
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
            let parts: Array = match &sep {
                Some(sep) if sep.is_empty() => {
                    bail!(span, "the separator must not be empty")
                }
                Some(sep) => s
                    .split(sep.as_str())
                    .map(|p| Value::Str(p.into()))
                    .collect(),
                None => s.split_whitespace().map(|p| Value::Str(p.into())).collect(),
            };
            vm.alloc(16 * parts.len() as u64, span)?;
            Value::Array(parts)
        }
        "trim" => Value::Str(s.trim().into()),
        "upper" => Value::Str(s.to_uppercase()),
        "lower" => Value::Str(s.to_lowercase()),
        "rev" => Value::Str(chars.iter().rev().collect::<String>().into()),
        "codepoints" => {
            vm.alloc(16 * chars.len() as u64, span)?;
            Value::Array(chars.iter().map(|c| Value::Str((*c).into())).collect())
        }
        "replace" => {
            let from: EcoString = args.expect("pattern")?;
            let to: EcoString = args.expect("replacement")?;
            if from.is_empty() {
                bail!(span, "the pattern must not be empty");
            }
            let count = s.matches(from.as_str()).count() as u64;
            vm.alloc(count.saturating_mul(to.len() as u64), span)?;
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
    let args = Args {
        span,
        items: values
            .iter()
            .map(|value| Arg {
                span,
                name: None,
                scope: None,
                value: value.clone(),
            })
            .collect(),
    };
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
                    out.push(item.clone());
                }
            }
            Value::Array(out)
        }
        "map" => {
            let func: Func = args.expect("function")?;
            let mut out = Array::with_capacity(items.len());
            for item in items.iter() {
                out.push(call_with(vm, &func, std::slice::from_ref(item), span)?);
            }
            Value::Array(out)
        }
        "fold" => {
            let mut acc: Value = args.expect("initial value")?;
            let func: Func = args.expect("function")?;
            for item in items.iter() {
                acc = call_with(vm, &func, &[acc, item.clone()], span)?;
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
            for item in iter {
                acc = if name == "sum" {
                    vm.alloc(item.shallow_size(), span)?;
                    ops::add(acc, item)
                } else {
                    ops::mul(acc, item)
                }
                .at(span)?;
            }
            acc
        }
        "rev" => Value::Array(items.iter().rev().cloned().collect()),
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
            vm.alloc(32 * items.len() as u64, span)?;
            Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        Value::Array(EcoVec::from([
                            Value::Int(start.saturating_add(i as i64)),
                            v.clone(),
                        ]))
                    })
                    .collect(),
            )
        }
        "zip" => {
            let other: Array = args.expect("array")?;
            vm.alloc(32 * items.len().min(other.len()) as u64, span)?;
            Value::Array(
                items
                    .iter()
                    .zip(other.iter())
                    .map(|(a, b)| Value::Array(EcoVec::from([a.clone(), b.clone()])))
                    .collect(),
            )
        }
        "flatten" => {
            let mut out = Array::new();
            flatten(&items, &mut out);
            vm.alloc(16 * out.len() as u64, span)?;
            Value::Array(out)
        }
        _ => bail!(span, "an array has no method `{name}`"),
    })
}

fn flatten(items: &Array, out: &mut Array) {
    for item in items {
        match item {
            Value::Array(inner) => flatten(inner, out),
            v => out.push(v.clone()),
        }
    }
}

fn dict_method(dict: Dict, name: &str, args: &mut Args, span: Span) -> EvalResult<Value> {
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
                .map(|(k, v)| Value::Array(EcoVec::from([Value::Str(k.clone()), v.clone()])))
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
    fn csv_quotes_and_line_breaks() {
        let rows = parse_csv("a,\"b,c\"\r\n\"x\"\"y\",\"two\nlines\"\n", ',').unwrap();
        assert_eq!(rows.repr(), r#"(("a", "b,c"), ("x\"y", "two\nlines"))"#);
        assert!(parse_csv("a,\"b\n", ',').is_err());
        assert!(parse_csv("a\"b\n", ',').is_err());
        assert_eq!(parse_csv("", ',').unwrap().repr(), "()");
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
