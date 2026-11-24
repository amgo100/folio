//! The values scripts compute with (SPEC §4.4).

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Formatter, Write};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Abs, Color, Em, Name, QualifiedName, Ratio, Scalar};
use folio_syntax::{Span, SyntaxNode};
use indexmap::IndexMap;

use super::content::Content;
use super::vm::NativeFn;
use crate::project::{Date, ProjectPath};

/// A value: what an expression evaluates to.
///
/// Equality and hashing are structural, so values can be keys of memoised
/// calls and results can be compared for early cutoff (SPEC §2.3). The `==`
/// operator of the language is looser: it compares `1` and `1.0` as equal.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub enum Value {
    /// `none`: no value.
    #[default]
    None,
    /// `auto`: a value the core decides.
    Auto,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(Scalar),
    /// A length: absolute, relative to the font size, or both.
    Length(Length),
    /// An angle.
    Angle(Angle),
    /// A ratio or percentage: `50%`.
    Ratio(Ratio),
    /// A fraction of the remaining space: `1fr`.
    Fraction(Fraction),
    /// A duration: `2.4s`, `5min`.
    Duration(Duration),
    /// A colour: `rgb("1f5fa8")`.
    Color(Color),
    /// A string.
    Str(EcoString),
    /// A label: `<fig:ball>`.
    Label(EcoString),
    /// A calendar date, such as `today`.
    Date(Date),
    /// Markup content.
    Content(Content),
    /// An array.
    Array(Array),
    /// A dictionary.
    Dict(Dict),
    /// A function.
    Func(Func),
    /// The arguments a function was called with, as an argument sink
    /// (`..args`) holds them.
    Args(Args),
    /// A module: an imported file, or a namespace such as `calc`.
    Module(Module),
}

impl Value {
    /// The name of the value's type, as `type` returns it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Length(_) => "length",
            Self::Angle(_) => "angle",
            Self::Ratio(_) => "ratio",
            Self::Fraction(_) => "fraction",
            Self::Duration(_) => "duration",
            Self::Color(_) => "color",
            Self::Str(_) => "str",
            Self::Label(_) => "label",
            Self::Date(_) => "date",
            Self::Content(_) => "content",
            Self::Array(_) => "array",
            Self::Dict(_) => "dictionary",
            Self::Func(_) => "function",
            Self::Args(_) => "arguments",
            Self::Module(_) => "module",
        }
    }

    /// The value as source code would write it, where it can be written:
    /// `"text"`, `(1, 2)`, `12pt`.
    pub fn repr(&self) -> EcoString {
        let mut out = EcoString::new();
        self.write_repr(&mut out);
        out
    }

    fn write_repr(&self, out: &mut EcoString) {
        match self {
            Self::None => out.push_str("none"),
            Self::Auto => out.push_str("auto"),
            Self::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Self::Int(v) => write!(out, "{v}").unwrap(),
            Self::Float(v) => out.push_str(&repr_float(v.get())),
            Self::Length(v) => out.push_str(&v.repr()),
            Self::Angle(v) => write!(out, "{}deg", repr_float(v.to_deg())).unwrap(),
            Self::Ratio(v) => write!(out, "{}%", repr_float(v.get() * 100.0)).unwrap(),
            Self::Fraction(v) => write!(out, "{}fr", repr_float(v.get())).unwrap(),
            Self::Duration(v) => write!(out, "{}s", repr_float(v.seconds())).unwrap(),
            Self::Color(c) => {
                write!(out, "rgb(\"{:02x}{:02x}{:02x}", c.r, c.g, c.b).unwrap();
                if c.a != 255 {
                    write!(out, "{:02x}", c.a).unwrap();
                }
                out.push_str("\")");
            }
            Self::Str(s) => write!(out, "{s:?}").unwrap(),
            Self::Label(l) => write!(out, "<{l}>").unwrap(),
            Self::Date(d) => write!(out, "date({d})").unwrap(),
            Self::Content(c) => out.push_str(&c.repr()),
            Self::Array(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_repr(out);
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Self::Dict(dict) => {
                if dict.is_empty() {
                    out.push_str("(:)");
                    return;
                }
                out.push('(');
                for (i, (key, value)) in dict.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if folio_syntax::is_ident(key) {
                        out.push_str(key);
                    } else {
                        write!(out, "{key:?}").unwrap();
                    }
                    out.push_str(": ");
                    value.write_repr(out);
                }
                out.push(')');
            }
            Self::Func(func) => out.push_str(&func.repr()),
            Self::Args(args) => {
                out.push_str("arguments(");
                for (i, arg) in args.items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if let Some(name) = &arg.name {
                        write!(out, "{name}: ").unwrap();
                    }
                    arg.value.write_repr(out);
                }
                out.push(')');
            }
            Self::Module(m) => write!(out, "<module {}>", m.name()).unwrap(),
        }
    }

    /// The number of bytes a value takes, as the memory limit counts them
    /// (SPEC §4.4): the length of a string, and a fixed size per item of an
    /// array, dictionary or content sequence. Items are not counted
    /// recursively, because they are shared rather than copied.
    pub(crate) fn shallow_size(&self) -> u64 {
        const ITEM: u64 = 16;
        let len = match self {
            Self::Str(s) => return s.len() as u64,
            Self::Array(a) => a.len(),
            Self::Dict(d) => d.len(),
            Self::Content(c) => c.len(),
            Self::Args(a) => a.items.len(),
            _ => 1,
        };
        len as u64 * ITEM
    }
}

/// Writes a float as code would: `1.0`, `0.25`, `1e30`.
pub(crate) fn repr_float(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{v:?}")
}

/// An array of values.
pub type Array = EcoVec<Value>;

/// A length: an absolute part and a part relative to the font size,
/// `2em + 3pt`.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Length {
    /// The absolute part.
    pub abs: Abs,
    /// The part relative to the font size.
    pub em: Em,
}

impl Length {
    /// An absolute length.
    pub fn abs(abs: Abs) -> Self {
        Self {
            abs,
            em: Em::zero(),
        }
    }

    /// A length relative to the font size.
    pub fn em(em: Em) -> Self {
        Self {
            abs: Abs::zero(),
            em,
        }
    }

    fn repr(&self) -> String {
        match (self.abs.is_zero(), self.em.is_zero()) {
            (false, false) => format!(
                "{}pt + {}em",
                repr_float(self.abs.to_pt()),
                repr_float(self.em.get())
            ),
            (true, false) => format!("{}em", repr_float(self.em.get())),
            _ => format!("{}pt", repr_float(self.abs.to_pt())),
        }
    }
}

/// An angle, stored in radians.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Angle(pub Scalar);

impl Angle {
    /// The angle in degrees.
    pub fn to_deg(self) -> f64 {
        self.0.get().to_degrees()
    }
}

/// A fraction of the remaining space: `1fr`.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Fraction(pub Scalar);

impl Fraction {
    /// The number of fractions.
    pub fn get(self) -> f64 {
        self.0.get()
    }
}

/// A duration, stored in seconds.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Duration(pub Scalar);

impl Duration {
    /// The duration in seconds.
    pub fn seconds(self) -> f64 {
        self.0.get()
    }
}

/// A dictionary: string keys in insertion order.
///
/// Two dictionaries are equal only when their pairs are in the same order,
/// because iteration order is visible to scripts.
#[derive(Clone, Default)]
pub struct Dict(Arc<IndexMap<EcoString, Value>>);

impl Dict {
    /// An empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of pairs.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no pairs.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value of a key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// The pairs, in insertion order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&EcoString, &Value)> {
        self.0.iter()
    }

    /// Inserts a pair, replacing the value of an existing key in place.
    pub fn insert(&mut self, key: EcoString, value: Value) {
        Arc::make_mut(&mut self.0).insert(key, value);
    }

    /// Removes a key, keeping the order of the others.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        Arc::make_mut(&mut self.0).shift_remove(key)
    }

    /// The value of a key, to change it, inserting `none` first if the key
    /// is missing.
    pub(crate) fn get_or_insert_mut(&mut self, key: EcoString) -> &mut Value {
        Arc::make_mut(&mut self.0).entry(key).or_default()
    }

    /// The value of a key, to change it.
    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        Arc::make_mut(&mut self.0).get_mut(key)
    }
}

impl PartialEq for Dict {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.iter().eq(other.0.iter())
    }
}

impl Eq for Dict {}

impl Hash for Dict {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.0.len());
        for pair in self.0.iter() {
            pair.hash(state);
        }
    }
}

impl Debug for Dict {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl FromIterator<(EcoString, Value)> for Dict {
    fn from_iter<T: IntoIterator<Item = (EcoString, Value)>>(iter: T) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
    }
}

/// Variables by name: the bindings of a module, or the variables a closure
/// captured.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Scope(Arc<BTreeMap<EcoString, Value>>);

impl Scope {
    /// The value of a variable.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    /// The variables, by name.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&EcoString, &Value)> {
        self.0.iter()
    }

    /// The number of variables.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no variables.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<BTreeMap<EcoString, Value>> for Scope {
    fn from(map: BTreeMap<EcoString, Value>) -> Self {
        Self(Arc::new(map))
    }
}

/// A module: the bindings and content of an evaluated file, or a namespace
/// of functions such as `calc`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Module(Arc<ModuleRepr>);

#[derive(Debug, Eq, PartialEq, Hash)]
struct ModuleRepr {
    name: EcoString,
    scope: Scope,
    content: Content,
}

impl Module {
    /// A module with a name, bindings and content.
    pub fn new(name: impl Into<EcoString>, scope: Scope, content: Content) -> Self {
        Self(Arc::new(ModuleRepr {
            name: name.into(),
            scope,
            content,
        }))
    }

    /// The module's name: a file's stem, or the namespace.
    pub fn name(&self) -> &EcoString {
        &self.0.name
    }

    /// The top-level bindings.
    pub fn scope(&self) -> &Scope {
        &self.0.scope
    }

    /// The content of the file.
    pub fn content(&self) -> &Content {
        &self.0.content
    }
}

/// A function: built into the standard library, a closure defined in a
/// script, or an element.
#[derive(Clone, Eq, PartialEq, Hash)]
pub enum Func {
    /// A function of the standard library.
    Native(NativeFunc),
    /// A closure defined in a script.
    Closure(Closure),
    /// An element: calling it makes an instance (SPEC §4.3).
    Element(QualifiedName),
    /// A slot of an element, `statement` of `example`: calling it makes the
    /// sub-block that fills the slot. It is defined only while the
    /// arguments of a call of the element are evaluated.
    Slot(Arc<(QualifiedName, Name)>),
    /// A function with some arguments already applied: `f.with(x)`.
    With(Arc<(Func, Args)>),
}

impl Func {
    /// The function's name, if it has one.
    pub fn name(&self) -> Option<EcoString> {
        match self {
            Self::Native(f) => Some(f.name.into()),
            Self::Closure(c) => c.name().cloned(),
            Self::Element(name) => Some(eco_format!("{name}")),
            Self::Slot(slot) => Some(slot.1.as_str().into()),
            Self::With(inner) => inner.0.name(),
        }
    }

    fn repr(&self) -> EcoString {
        match self {
            Self::Closure(c) if c.name().is_none() => "(..) => ..".into(),
            _ => self.name().unwrap_or_default(),
        }
    }
}

impl Debug for Func {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Func({})", self.repr())
    }
}

/// A function of the standard library.
#[derive(Copy, Clone)]
pub struct NativeFunc {
    /// The name, unique in the library: `range`, `calc.sqrt`.
    pub name: &'static str,
    pub(crate) func: NativeFn,
}

impl PartialEq for NativeFunc {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for NativeFunc {}

impl Debug for NativeFunc {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "NativeFunc({})", self.name)
    }
}

impl Hash for NativeFunc {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

/// A closure: a function defined in a script, with the variables it
/// captured.
///
/// Its hash is computed once, when it is made, because calls are memoised
/// with the closure as part of the key and the key is hashed on every call
/// (SPEC §2.3).
#[derive(Clone)]
pub struct Closure(Arc<(u64, ClosureRepr)>);

#[derive(Hash, Eq, PartialEq)]
struct ClosureRepr {
    /// The `Closure` syntax node: parameters and body.
    node: SyntaxNode,
    /// The file it was defined in, which paths in its body are relative to.
    file: ProjectPath,
    /// The name of a closure defined with `let f(..) = ..`.
    name: Option<EcoString>,
    /// The values of the free variables of its body, when it was made.
    captured: Scope,
    /// The values of its named parameters' defaults, in order.
    defaults: EcoVec<Value>,
}

impl Closure {
    pub(crate) fn new(
        node: SyntaxNode,
        file: ProjectPath,
        name: Option<EcoString>,
        captured: Scope,
        defaults: EcoVec<Value>,
    ) -> Self {
        let repr = ClosureRepr {
            node,
            file,
            name,
            captured,
            defaults,
        };
        let mut state = DefaultHasher::new();
        repr.hash(&mut state);
        Self(Arc::new((state.finish(), repr)))
    }

    /// The name of a closure defined with `let f(..) = ..`.
    pub fn name(&self) -> Option<&EcoString> {
        self.0.1.name.as_ref()
    }

    /// The closure's syntax: its parameters and body.
    pub fn node(&self) -> &SyntaxNode {
        &self.0.1.node
    }

    /// The file it was defined in.
    pub fn file(&self) -> &ProjectPath {
        &self.0.1.file
    }

    /// The variables it captured.
    pub fn captured(&self) -> &Scope {
        &self.0.1.captured
    }

    /// The values of its named parameters' defaults, in order.
    pub fn defaults(&self) -> &[Value] {
        &self.0.1.defaults
    }
}

impl PartialEq for Closure {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || (self.0.0 == other.0.0 && self.0.1 == other.0.1)
    }
}

impl Eq for Closure {}

impl Hash for Closure {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.0);
    }
}

impl Debug for Closure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Closure")
            .field("name", &self.0.1.name)
            .field("file", &self.0.1.file)
            .field("captured", &self.0.1.captured)
            .finish_non_exhaustive()
    }
}

/// The arguments of a call.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Args {
    /// The span of the whole call.
    pub span: Span,
    /// The arguments, positional and named, in order.
    pub items: EcoVec<Arg>,
}

/// One argument of a call.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Arg {
    /// The span of the argument.
    pub span: Span,
    /// The name of a named argument.
    pub name: Option<EcoString>,
    /// The variant or target a named element field is limited to:
    /// `markscheme` in `override@markscheme: (..)` (SPEC §5.4).
    pub scope: Option<EcoString>,
    /// The value.
    pub value: Value,
}
