//! The values scripts compute with (SPEC §4.4).

use std::collections::{BTreeMap, BTreeSet};
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

    /// How deeply the value nests (SPEC §4.4): `0` for a value that holds
    /// no other values, and one more than its deepest part for an array, a
    /// dictionary, arguments, a closure, a function with arguments applied,
    /// and an element or slot in content. Content is as deep as its deepest
    /// node, and a module as its deepest binding or its content. Every
    /// value caches its depth, so this takes constant time.
    pub fn depth(&self) -> u32 {
        match self {
            Self::Content(c) => c.depth(),
            Self::Array(a) => a.depth(),
            Self::Dict(d) => d.depth(),
            Self::Func(f) => f.depth(),
            Self::Args(a) => a.depth(),
            Self::Module(m) => m.depth(),
            _ => 0,
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

/// A cached depth, which equality, hashing and debug output ignore: it
/// follows from the value that holds it.
#[derive(Copy, Clone)]
struct Depth(u32);

impl PartialEq for Depth {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Depth {}

impl Hash for Depth {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

impl Debug for Depth {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
///
/// It caches its depth ([`Value::depth`]), which its methods keep exact.
/// Equality, hashing and debug output are those of its items.
#[derive(Clone)]
pub struct Array {
    items: EcoVec<Value>,
    depth: u32,
}

impl Array {
    /// An empty array.
    pub fn new() -> Self {
        Self {
            items: EcoVec::new(),
            depth: 1,
        }
    }

    /// An empty array with room for `capacity` items.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: EcoVec::with_capacity(capacity),
            depth: 1,
        }
    }

    /// How deeply the array nests: one more than its deepest item.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The items.
    pub fn as_slice(&self) -> &[Value] {
        &self.items
    }

    /// Appends an item.
    pub fn push(&mut self, value: Value) {
        self.depth = self.depth.max(value.depth().saturating_add(1));
        self.items.push(value);
    }

    /// Inserts an item at an index, shifting the ones after it.
    pub fn insert(&mut self, index: usize, value: Value) {
        self.depth = self.depth.max(value.depth().saturating_add(1));
        self.items.insert(index, value);
    }

    /// Removes and returns the last item.
    pub fn pop(&mut self) -> Option<Value> {
        let value = self.items.pop()?;
        self.removed(value.depth());
        Some(value)
    }

    /// Removes and returns the item at an index.
    pub fn remove(&mut self, index: usize) -> Value {
        let value = self.items.remove(index);
        self.removed(value.depth());
        value
    }

    /// Changes the item at an index in place, and returns what `f`
    /// returns.
    pub(crate) fn update<R>(&mut self, index: usize, f: impl FnOnce(&mut Value) -> R) -> R {
        let item = &mut self.items.make_mut()[index];
        let before = item.depth();
        let result = f(item);
        let after = item.depth();
        self.depth = self.depth.max(after.saturating_add(1));
        if after < before {
            self.removed(before);
        }
        result
    }

    /// Restores the depth after an item of depth `depth` was removed or
    /// made shallower: only an item at the greatest depth can lower it.
    fn removed(&mut self, depth: u32) {
        if depth.saturating_add(1) == self.depth {
            self.depth = depth_of(self.items.iter());
        }
    }
}

/// One more than the deepest of some values.
pub(crate) fn depth_of<'a>(values: impl IntoIterator<Item = &'a Value>) -> u32 {
    values
        .into_iter()
        .map(Value::depth)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

impl Default for Array {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for Array {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.items
    }
}

impl PartialEq for Array {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl Eq for Array {}

impl Hash for Array {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.items.hash(state);
    }
}

impl Debug for Array {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.items.fmt(f)
    }
}

impl From<EcoVec<Value>> for Array {
    fn from(items: EcoVec<Value>) -> Self {
        let depth = depth_of(items.iter());
        Self { items, depth }
    }
}

impl<const N: usize> From<[Value; N]> for Array {
    fn from(items: [Value; N]) -> Self {
        Self::from(EcoVec::from(items))
    }
}

impl FromIterator<Value> for Array {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        Self::from(iter.into_iter().collect::<EcoVec<_>>())
    }
}

impl Extend<Value> for Array {
    fn extend<T: IntoIterator<Item = Value>>(&mut self, iter: T) {
        for value in iter {
            self.push(value);
        }
    }
}

impl IntoIterator for Array {
    type Item = Value;
    type IntoIter = ecow::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a Array {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

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
#[derive(Clone)]
pub struct Dict {
    map: Arc<IndexMap<EcoString, Value>>,
    /// One more than the deepest value, kept exact by the methods.
    depth: u32,
}

impl Dict {
    /// An empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// How deeply the dictionary nests: one more than its deepest value.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The number of pairs.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether there are no pairs.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The value of a key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// The pairs, in insertion order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&EcoString, &Value)> {
        self.map.iter()
    }

    /// Inserts a pair, replacing the value of an existing key in place.
    pub fn insert(&mut self, key: EcoString, value: Value) {
        self.depth = self.depth.max(value.depth().saturating_add(1));
        if let Some(old) = Arc::make_mut(&mut self.map).insert(key, value) {
            self.removed(old.depth());
        }
    }

    /// Removes a key, keeping the order of the others.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let value = Arc::make_mut(&mut self.map).shift_remove(key)?;
        self.removed(value.depth());
        Some(value)
    }

    /// Changes the value of a key in place, inserting `none` first if the
    /// key is missing and `insert` is true, and returns what `f` returns;
    /// `None` if the key is missing and `insert` is false.
    pub(crate) fn update<R>(
        &mut self,
        key: &str,
        insert: bool,
        f: impl FnOnce(&mut Value) -> R,
    ) -> Option<R> {
        let map = Arc::make_mut(&mut self.map);
        if !map.contains_key(key) {
            if !insert {
                return None;
            }
            map.insert(key.into(), Value::None);
        }
        let value = map.get_mut(key).expect("the key was inserted");
        let before = value.depth();
        let result = f(value);
        let after = value.depth();
        self.depth = self.depth.max(after.saturating_add(1));
        if after < before {
            self.removed(before);
        }
        Some(result)
    }

    /// Restores the depth after a value of depth `depth` was removed or
    /// made shallower: only a value at the greatest depth can lower it.
    fn removed(&mut self, depth: u32) {
        if depth.saturating_add(1) == self.depth {
            self.depth = depth_of(self.map.values());
        }
    }
}

impl Default for Dict {
    fn default() -> Self {
        Self {
            map: Arc::default(),
            depth: 1,
        }
    }
}

impl PartialEq for Dict {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.map, &other.map) || self.map.iter().eq(other.map.iter())
    }
}

impl Eq for Dict {}

impl Hash for Dict {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.map.len());
        for pair in self.map.iter() {
            pair.hash(state);
        }
    }
}

impl Debug for Dict {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.map.iter()).finish()
    }
}

impl FromIterator<(EcoString, Value)> for Dict {
    fn from_iter<T: IntoIterator<Item = (EcoString, Value)>>(iter: T) -> Self {
        let map: IndexMap<EcoString, Value> = iter.into_iter().collect();
        let depth = depth_of(map.values());
        Self {
            map: Arc::new(map),
            depth,
        }
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
    failed: BTreeSet<EcoString>,
    /// The depth of its deepest binding or of its content.
    depth: Depth,
}

impl Module {
    /// A module with a name, bindings and content.
    pub fn new(name: impl Into<EcoString>, scope: Scope, content: Content) -> Self {
        Self::partial(name, scope, content, BTreeSet::new())
    }

    /// A module of a file that was evaluated around failures: `failed`
    /// holds the names that a failed `let` or import would have bound.
    pub(crate) fn partial(
        name: impl Into<EcoString>,
        scope: Scope,
        content: Content,
        failed: BTreeSet<EcoString>,
    ) -> Self {
        let depth = scope
            .iter()
            .map(|(_, value)| value.depth())
            .max()
            .unwrap_or(0)
            .max(content.depth());
        Self(Arc::new(ModuleRepr {
            name: name.into(),
            scope,
            content,
            failed,
            depth: Depth(depth),
        }))
    }

    /// How deeply the module nests: as deeply as its deepest binding or its
    /// content. A module adds no depth of its own, because modules nest
    /// only through imports, which the depth limit of calls bounds.
    pub fn depth(&self) -> u32 {
        self.0.depth.0
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

    /// Whether the module would bind a name but for a failure, which was
    /// reported where it happened.
    pub fn failed(&self, name: &str) -> bool {
        self.0.failed.contains(name)
    }

    /// The names the module would bind but for failures.
    pub(crate) fn failed_names(&self) -> &BTreeSet<EcoString> {
        &self.0.failed
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

    /// How deeply the function nests: a closure's depth, one more than the
    /// deeper of the function and the arguments for a function with
    /// arguments applied, and `0` otherwise.
    pub fn depth(&self) -> u32 {
        match self {
            Self::Closure(c) => c.depth(),
            Self::With(with) => with.0.depth().max(with.1.depth()).saturating_add(1),
            Self::Native(_) | Self::Element(_) | Self::Slot(_) => 0,
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
    /// One more than the deepest value it captured or holds as a default.
    depth: Depth,
}

impl Closure {
    pub(crate) fn new(
        node: SyntaxNode,
        file: ProjectPath,
        name: Option<EcoString>,
        captured: Scope,
        defaults: EcoVec<Value>,
    ) -> Self {
        let depth = depth_of(captured.iter().map(|(_, v)| v).chain(defaults.iter()));
        let repr = ClosureRepr {
            node,
            file,
            name,
            captured,
            defaults,
            depth: Depth(depth),
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

    /// How deeply the closure nests: one more than the deepest value it
    /// captured or holds as a default.
    pub fn depth(&self) -> u32 {
        self.0.1.depth.0
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
    pub(crate) items: EcoVec<Arg>,
    /// One more than the deepest value, as the arguments were made. Taking
    /// arguments leaves it an upper bound; arguments that are a value are
    /// never changed, so theirs is exact.
    depth: Depth,
}

impl Args {
    /// Arguments of a call.
    pub fn new(span: Span, items: EcoVec<Arg>) -> Self {
        let depth = depth_of(items.iter().map(|arg| &arg.value));
        Self {
            span,
            items,
            depth: Depth(depth),
        }
    }

    /// The arguments, positional and named, in order.
    pub fn items(&self) -> &[Arg] {
        &self.items
    }

    /// How deeply the arguments nest: one more than the deepest value.
    pub fn depth(&self) -> u32 {
        self.depth.0
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An array nested `depth` deep: `((..),)`.
    fn nested(depth: u32) -> Value {
        let mut value = Value::Array(Array::new());
        for _ in 1..depth {
            value = Value::Array(Array::from([value]));
        }
        value
    }

    #[test]
    fn values_count_their_depth() {
        assert_eq!(Value::Int(1).depth(), 0);
        assert_eq!(nested(1).depth(), 1);
        assert_eq!(nested(5).depth(), 5);
        let dict: Dict = [("x".into(), nested(3)), ("y".into(), Value::None)]
            .into_iter()
            .collect();
        assert_eq!(dict.depth(), 4);
        assert_eq!(Dict::new().depth(), 1);
        let args = Args::new(
            Span::detached(),
            EcoVec::from([Arg {
                span: Span::detached(),
                name: None,
                scope: None,
                value: nested(2),
            }]),
        );
        assert_eq!(args.depth(), 3);
        let with = Func::With(Arc::new((
            Func::Element(QualifiedName::unqualified(Name::new("strong").unwrap())),
            args,
        )));
        assert_eq!(with.depth(), 4);
    }

    #[test]
    fn arrays_keep_their_depth_exact() {
        let mut array = Array::new();
        array.push(Value::Int(1));
        assert_eq!(array.depth(), 1);
        array.push(nested(4));
        array.insert(0, nested(2));
        assert_eq!(array.depth(), 5);
        // Removing the deepest item lowers the depth; another does not.
        assert_eq!(array.remove(0), nested(2));
        assert_eq!(array.depth(), 5);
        assert_eq!(array.pop(), Some(nested(4)));
        assert_eq!(array.depth(), 1);
        // A change in place, deeper and then shallower.
        array.update(0, |item| *item = nested(6));
        assert_eq!(array.depth(), 7);
        array.update(0, |item| *item = Value::None);
        assert_eq!(array.depth(), 1);
        // Two items at the greatest depth: removing one keeps it.
        let mut array = Array::from([nested(3), nested(3)]);
        array.pop();
        assert_eq!(array.depth(), 4);
        // Extending and collecting count every item.
        array.extend([nested(8)]);
        assert_eq!(array.depth(), 9);
        let collected: Array = array.iter().cloned().collect();
        assert_eq!(collected.depth(), 9);
    }

    #[test]
    fn dictionaries_keep_their_depth_exact() {
        let mut dict = Dict::new();
        dict.insert("a".into(), nested(3));
        dict.insert("b".into(), nested(1));
        assert_eq!(dict.depth(), 4);
        // Replacing the deepest value with a shallower one lowers it.
        dict.insert("a".into(), Value::Int(0));
        assert_eq!(dict.depth(), 2);
        // A change in place, inserting the key when asked.
        assert_eq!(dict.update("c", false, |_| ()), None);
        assert_eq!(dict.update("c", true, |v| *v = nested(5)), Some(()));
        assert_eq!(dict.depth(), 6);
        assert_eq!(dict.remove("c"), Some(nested(5)));
        assert_eq!(dict.depth(), 2);
        assert_eq!(dict.remove("b"), Some(nested(1)));
        assert_eq!(dict.depth(), 1);
    }

    #[test]
    fn depth_is_not_part_of_equality_or_hashing() {
        // A value made in place equals one made at once, and hashes alike.
        let mut a = Array::from([nested(4)]);
        a.update(0, |item| *item = Value::Int(1));
        let b = Array::from([Value::Int(1)]);
        assert_eq!(a, b);
        let hash = |v: &Array| {
            let mut state = DefaultHasher::new();
            v.hash(&mut state);
            state.finish()
        };
        assert_eq!(hash(&a), hash(&b));
        assert_eq!(a.depth(), b.depth());
    }
}
