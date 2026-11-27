//! The values scripts compute with (SPEC §4.4).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{self, Debug, Formatter, Write};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Abs, Color, Em, Name, QualifiedName, Ratio, Scalar};
use folio_syntax::{Span, SyntaxNode};

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
    /// A formula of the math tree (SPEC §8): the `tree` field of a `math`
    /// element.
    Math(folio_math::Formula),
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
            Self::Math(_) => "formula",
        }
    }

    /// The value as source code would write it, where it can be written:
    /// `"text"`, `(1, 2)`, `12pt`.
    pub fn repr(&self) -> EcoString {
        let mut out = EcoString::new();
        let _ = self.write_repr(&mut out, usize::MAX);
        out
    }

    /// The value as [`Value::repr`] writes it, or [`TooLong`] as soon as it
    /// is longer than `limit` bytes.
    ///
    /// A value can hold one part many times, as `(a, a)` does, so its
    /// representation can be exponentially longer than the memory it takes.
    /// Every part writes at least one byte, so the work this does is bounded
    /// by the limit too.
    pub(crate) fn repr_within(&self, limit: usize) -> Result<EcoString, TooLong> {
        let mut out = EcoString::new();
        self.write_repr(&mut out, limit)?;
        Ok(out)
    }

    pub(crate) fn write_repr(&self, out: &mut EcoString, limit: usize) -> Result<(), TooLong> {
        if out.len() > limit {
            return Err(TooLong);
        }
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
            Self::Str(s) => write_str_repr(out, s, limit)?,
            Self::Label(l) => write!(out, "<{l}>").unwrap(),
            Self::Date(d) => write!(out, "date({d})").unwrap(),
            Self::Content(c) => c.write_repr(out, limit)?,
            Self::Array(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_repr(out, limit)?;
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Self::Dict(dict) => {
                if dict.is_empty() {
                    out.push_str("(:)");
                    return Ok(());
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
                    value.write_repr(out, limit)?;
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
                    arg.value.write_repr(out, limit)?;
                }
                out.push(')');
            }
            Self::Module(m) => write!(out, "<module {}>", m.name()).unwrap(),
            Self::Math(formula) => {
                // A formula is written whole, so its length is bounded
                // first: its nodes can share one long text.
                let mut budget = limit.saturating_sub(out.len()) as u64;
                for line in formula.lines() {
                    repr_budget(&line.row, &mut budget)?;
                }
                write!(out, "formula({})", formula.repr()).unwrap()
            }
        }
        if out.len() > limit {
            return Err(TooLong);
        }
        Ok(())
    }

    /// How deeply the value nests (SPEC §4.4): `0` for a value that holds
    /// no other values, and one more than its deepest part for an array, a
    /// dictionary, arguments, a closure, a function with arguments applied,
    /// and an element or slot in content. Content is as deep as its deepest
    /// node, a module as its deepest binding or its content, and a formula
    /// as its deepest row of the math tree. Every value caches its depth,
    /// so this takes constant time.
    pub fn depth(&self) -> u32 {
        match self {
            Self::Content(c) => c.depth(),
            Self::Array(a) => a.depth(),
            Self::Dict(d) => d.depth(),
            Self::Func(f) => f.depth(),
            Self::Args(a) => a.depth(),
            Self::Module(m) => m.depth(),
            Self::Math(formula) => formula.depth(),
            _ => 0,
        }
    }

    /// The number of bytes a value takes, as the memory limit counts them
    /// (SPEC §4.4): the length of a string, and a fixed size per item of an
    /// array ([`ITEM`]) or of a dictionary, content sequence or arguments
    /// ([`NODE`]). Items are not counted recursively, because they are
    /// shared rather than copied.
    pub(crate) fn shallow_size(&self) -> u64 {
        let (len, size) = match self {
            Self::Str(s) => return s.len() as u64,
            Self::Array(a) => (a.len(), ITEM),
            Self::Dict(d) => (d.len(), NODE),
            Self::Content(c) => (c.len(), NODE),
            Self::Args(a) => (a.items.len(), NODE),
            _ => (1, ITEM),
        };
        len as u64 * size
    }
}

/// The bytes an item of an array counts against the memory limit
/// ([`Value::shallow_size`]): at least what a value takes.
///
/// The sizes are fixed rather than measured, so that whether a document is
/// within the limit does not depend on the platform.
pub(crate) const ITEM: u64 = 64;

/// The bytes a node of content, a pair of a dictionary or an argument
/// counts against the memory limit ([`Value::shallow_size`]): at least what
/// each takes.
pub(crate) const NODE: u64 = 128;

/// Writes a string as code writes it, or fails without writing it when it
/// would make `out` longer than `limit` bytes: escaping can make a string
/// several times longer.
pub(crate) fn write_str_repr(out: &mut EcoString, s: &str, limit: usize) -> Result<(), TooLong> {
    let escaped = s.chars().try_fold(out.len().saturating_add(2), |len, c| {
        let len = len.saturating_add(c.escape_debug().map(char::len_utf8).sum());
        (len <= limit).then_some(len)
    });
    if escaped.is_none() {
        return Err(TooLong);
    }
    write!(out, "{s:?}").unwrap();
    Ok(())
}

/// Takes from `budget` at least the bytes the representation of `row`
/// writes, or fails once it is spent. Each node is visited once for each
/// time it occurs, which a formula's node limit bounds.
fn repr_budget(row: &folio_math::Row, budget: &mut u64) -> Result<(), TooLong> {
    use folio_math::Node;
    /// More than any node writes besides its text and its rows.
    const OWN: u64 = 64;
    for node in row.nodes() {
        let text = match node {
            Node::Ident(atom) | Node::Number(atom) | Node::Symbol(atom) | Node::Text(atom) => {
                // Escaped, a character takes at most ten bytes.
                10 * atom.text.len() as u64
            }
            Node::Operator(op) => op.name.len() as u64,
            _ => 0,
        };
        *budget = budget.checked_sub(OWN + text).ok_or(TooLong)?;
        for inner in node.rows() {
            repr_budget(inner, budget)?;
        }
    }
    Ok(())
}

/// A representation that would be longer than its limit
/// ([`Value::repr_within`]).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct TooLong;

/// A cached depth, which equality and hashing ignore: it follows from the
/// value that holds it.
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

/// The hash of a value that holds other values, computed when it is first
/// needed and shared by the value's clones.
///
/// Hashing walks a value as a tree. A value can hold one part many times,
/// as `(a, a)` does, and a loop that doubles an array this way makes one
/// whose tree is exponentially large. With the hash of each part cached
/// where its clones share it, hashing such a value hashes each part once.
/// A value whose hash is cached writes that hash where it would write its
/// parts, so values that are equal must agree on whether they cache: the
/// containers decide by their exact depth, and arguments and functions with
/// arguments applied always cache.
#[derive(Clone, Default)]
pub(crate) struct LazyHash(Option<Arc<OnceLock<u64>>>);

impl LazyHash {
    /// A hash that is cached, or written as the parts are.
    pub(crate) fn new(cached: bool) -> Self {
        Self(cached.then(Arc::default))
    }

    /// Whether containers of depth `depth` cache their hash: those that hold
    /// containers, whose parts can be shared.
    pub(crate) fn for_depth(depth: u32) -> Self {
        Self::new(depth >= 2)
    }

    /// Writes the hash of a value whose parts `parts` writes: the cached
    /// hash, computed first if needed, or else the parts.
    pub(crate) fn write<H: Hasher>(&self, state: &mut H, parts: impl FnOnce(&mut dyn Hasher)) {
        match &self.0 {
            Some(cell) => state.write_u64(*cell.get_or_init(|| {
                let mut hasher = DefaultHasher::new();
                parts(&mut hasher);
                hasher.finish()
            })),
            None => parts(state),
        }
    }

    /// The cached hash, if it was computed.
    pub(crate) fn get(&self) -> Option<u64> {
        self.0.as_ref()?.get().copied()
    }

    /// Whether two values certainly differ, because their cached hashes do.
    pub(crate) fn differ(&self, other: &Self) -> bool {
        matches!((self.get(), other.get()), (Some(a), Some(b)) if a != b)
    }

    /// Forgets the hash after the value changed. Its clones, which did not
    /// change, keep theirs.
    pub(crate) fn reset(&mut self, cached: bool) {
        match &mut self.0 {
            Some(cell) if cached => match Arc::get_mut(cell) {
                Some(cell) => {
                    cell.take();
                }
                None => *self = Self::new(true),
            },
            _ => *self = Self::new(cached),
        }
    }
}

/// Which equality a comparison of two values asks for.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) enum Equality {
    /// Structural equality: `Eq`.
    Strict,
    /// The language's `==`, which compares `1` and `1.0` as equal.
    Loose,
    /// The order of `<` and sorting: the pairs it found equal.
    Order,
}

/// The comparisons running on a thread, and the pairs of containers they
/// found equal, by the address of their storage and the equality.
type Comparisons = (u32, HashSet<(usize, usize, Equality)>);

thread_local! {
    static EQUAL: RefCell<Comparisons> = RefCell::new((0, HashSet::new()));
}

/// Compares two containers that hold containers, whose storage is at `a`
/// and `b`, with `compare`, remembering the pairs found equal until the
/// outermost comparison ends.
///
/// Like hashing, comparing walks values as trees, and two values built
/// alike, each holding its parts many times, are exponentially large trees.
/// Comparing each pair of parts once makes it linear. While a comparison
/// runs, both values are borrowed, so their storage cannot move or change.
pub(crate) fn compare_once(
    a: usize,
    b: usize,
    equality: Equality,
    compare: impl FnOnce() -> bool,
) -> bool {
    /// Leaves a comparison, forgetting the pairs when it was the outermost,
    /// also when a comparison unwinds.
    struct Leave;

    impl Drop for Leave {
        fn drop(&mut self) {
            EQUAL.with_borrow_mut(|(running, equal)| {
                *running -= 1;
                if *running == 0 {
                    equal.clear();
                }
            });
        }
    }

    let key = (a, b, equality);
    let known = EQUAL.with_borrow_mut(|(running, equal)| {
        *running += 1;
        equal.contains(&key)
    });
    let _leave = Leave;
    if known {
        return true;
    }
    let result = compare();
    if result {
        EQUAL.with_borrow_mut(|(_, equal)| equal.insert(key));
    }
    result
}

impl PartialEq for LazyHash {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for LazyHash {}

impl Debug for LazyHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("..")
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
    nesting: Nesting,
    hash: LazyHash,
}

impl Array {
    /// An empty array.
    pub fn new() -> Self {
        Self {
            items: EcoVec::new(),
            nesting: Nesting::EMPTY,
            hash: LazyHash::default(),
        }
    }

    /// An empty array with room for `capacity` items.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: EcoVec::with_capacity(capacity),
            nesting: Nesting::EMPTY,
            hash: LazyHash::default(),
        }
    }

    /// How deeply the array nests: one more than its deepest item.
    pub fn depth(&self) -> u32 {
        self.nesting.depth
    }

    /// The items.
    pub fn as_slice(&self) -> &[Value] {
        &self.items
    }

    /// The address of the array's storage, for [`compare_once`].
    pub(crate) fn storage(&self) -> usize {
        self.items.as_ptr() as usize
    }

    /// Appends an item.
    pub fn push(&mut self, value: Value) {
        self.nesting.added(value.depth());
        self.items.push(value);
        self.changed();
    }

    /// Inserts an item at an index, shifting the ones after it.
    pub fn insert(&mut self, index: usize, value: Value) {
        self.nesting.added(value.depth());
        self.items.insert(index, value);
        self.changed();
    }

    /// Removes and returns the last item.
    pub fn pop(&mut self) -> Option<Value> {
        let value = self.items.pop()?;
        self.removed(value.depth());
        self.changed();
        Some(value)
    }

    /// Removes and returns the item at an index.
    pub fn remove(&mut self, index: usize) -> Value {
        let value = self.items.remove(index);
        self.removed(value.depth());
        self.changed();
        value
    }

    /// Changes the item at an index in place, and returns what `f`
    /// returns.
    pub(crate) fn update<R>(&mut self, index: usize, f: impl FnOnce(&mut Value) -> R) -> R {
        let item = &mut self.items.make_mut()[index];
        let before = item.depth();
        let result = f(item);
        let after = item.depth();
        // The changed item counts as removed and added again.
        self.nesting.added(after);
        self.removed(before);
        self.changed();
        result
    }

    /// Forgets the hash after a change.
    fn changed(&mut self) {
        self.hash.reset(self.nesting.depth >= 2);
    }

    /// Uncounts an item of depth `depth` that was removed or replaced.
    fn removed(&mut self, depth: u32) {
        let items = &self.items;
        self.nesting
            .removed(depth, || Nesting::measure(items.iter()));
    }
}

/// One more than the deepest of some values.
pub(crate) fn depth_of<'a>(values: impl IntoIterator<Item = &'a Value>) -> u32 {
    Nesting::measure(values).depth
}

/// The depth of an array or a dictionary, kept exact as values come and go:
/// one more than its deepest value, and how many values are that deep. Only
/// removing the last of those lowers the depth, so only that rescans the
/// values, and taking items one by one from an array whose items are all
/// equally deep takes constant time each.
#[derive(Copy, Clone)]
struct Nesting {
    depth: u32,
    deepest: usize,
}

impl Nesting {
    /// No values.
    const EMPTY: Self = Self {
        depth: 1,
        deepest: 0,
    };

    /// The nesting of some values.
    fn measure<'a>(values: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut nesting = Self::EMPTY;
        for value in values {
            nesting.added(value.depth());
        }
        nesting
    }

    /// Counts a value of depth `depth` that was added.
    fn added(&mut self, depth: u32) {
        let depth = depth.saturating_add(1);
        if depth > self.depth {
            *self = Self { depth, deepest: 1 };
        } else if depth == self.depth {
            self.deepest += 1;
        }
    }

    /// Uncounts a value of depth `depth` that was removed, rescanning the
    /// values that are left when it was the last of the deepest.
    fn removed(&mut self, depth: u32, values: impl FnOnce() -> Self) {
        if depth.saturating_add(1) == self.depth {
            self.deepest -= 1;
            if self.deepest == 0 && self.depth > 1 {
                *self = values();
            }
        }
    }
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
        let same = self.items.as_ptr() == other.items.as_ptr() && self.len() == other.len();
        if same || self.len() != other.len() || self.hash.differ(&other.hash) {
            return same;
        }
        if self.nesting.depth < 2 {
            return self.items == other.items;
        }
        compare_once(
            self.items.as_ptr() as usize,
            other.items.as_ptr() as usize,
            Equality::Strict,
            || self.items == other.items,
        )
    }
}

impl Eq for Array {}

impl Hash for Array {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash
            .write(state, |mut state| self.items.hash(&mut state));
    }
}

impl Debug for Array {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.items.fmt(f)
    }
}

impl From<EcoVec<Value>> for Array {
    fn from(items: EcoVec<Value>) -> Self {
        let nesting = Nesting::measure(items.iter());
        Self {
            items,
            nesting,
            hash: LazyHash::for_depth(nesting.depth),
        }
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
    map: Arc<Pairs>,
    /// One more than the deepest value, kept exact by the methods.
    nesting: Nesting,
    hash: LazyHash,
}

impl Dict {
    /// An empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// How deeply the dictionary nests: one more than its deepest value.
    pub fn depth(&self) -> u32 {
        self.nesting.depth
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
        self.nesting.added(value.depth());
        if let Some(old) = Arc::make_mut(&mut self.map).insert(key, value) {
            self.removed(old.depth());
        }
        self.changed();
    }

    /// Removes a key, keeping the order of the others.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let value = Arc::make_mut(&mut self.map).remove(key)?;
        self.removed(value.depth());
        self.changed();
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
        let existed = map.get(key).is_some();
        if !existed {
            if !insert {
                return None;
            }
            map.insert(key.into(), Value::None);
        }
        let value = map.get_mut(key).expect("the key was inserted");
        let before = value.depth();
        let result = f(value);
        let after = value.depth();
        // The changed value counts as removed and added again. A key that
        // was inserted held `none`, which counts as added.
        self.nesting.added(after);
        if existed {
            self.removed(before);
        }
        self.changed();
        Some(result)
    }

    /// Forgets the hash after a change.
    fn changed(&mut self) {
        self.hash.reset(self.nesting.depth >= 2);
    }

    /// Uncounts a value of depth `depth` that was removed or replaced.
    fn removed(&mut self, depth: u32) {
        let map = &self.map;
        self.nesting
            .removed(depth, || Nesting::measure(map.values()));
    }
}

impl Default for Dict {
    fn default() -> Self {
        Self {
            map: Arc::default(),
            nesting: Nesting::EMPTY,
            hash: LazyHash::default(),
        }
    }
}

impl PartialEq for Dict {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.map, &other.map) {
            return true;
        }
        if self.len() != other.len() || self.hash.differ(&other.hash) {
            return false;
        }
        let compare = || self.map.iter().eq(other.map.iter());
        if self.nesting.depth < 2 {
            return compare();
        }
        compare_once(
            Arc::as_ptr(&self.map) as usize,
            Arc::as_ptr(&other.map) as usize,
            Equality::Strict,
            compare,
        )
    }
}

impl Dict {
    /// The address of the dictionary's storage, for [`compare_once`].
    pub(crate) fn storage(&self) -> usize {
        Arc::as_ptr(&self.map) as usize
    }
}

impl Eq for Dict {}

impl Hash for Dict {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.write(state, |mut state| {
            state.write_usize(self.map.len());
            for pair in self.map.iter() {
                pair.hash(&mut state);
            }
        });
    }
}

impl Debug for Dict {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.map.iter()).finish()
    }
}

impl FromIterator<(EcoString, Value)> for Dict {
    fn from_iter<T: IntoIterator<Item = (EcoString, Value)>>(iter: T) -> Self {
        let mut map = Pairs::default();
        for (key, value) in iter {
            map.insert(key, value);
        }
        let nesting = Nesting::measure(map.values());
        Self {
            map: Arc::new(map),
            nesting,
            hash: LazyHash::for_depth(nesting.depth),
        }
    }
}

/// The pairs of a dictionary in insertion order. A removed pair leaves an
/// empty slot rather than shifting the pairs after it, which would make
/// removing keys in insertion order take quadratic time; the slots are
/// compacted once at least half of them are empty, which keeps removal
/// amortised constant time. Empty slots are invisible: iteration, length,
/// equality and hashing see only the pairs, in insertion order.
#[derive(Clone, Default)]
struct Pairs {
    /// The pairs in insertion order, `None` where one was removed.
    slots: Vec<Option<(EcoString, Value)>>,
    /// The slot of each key. Only looked up, never iterated, so its order
    /// does not matter.
    index: HashMap<EcoString, usize>,
}

impl Pairs {
    /// The number of pairs.
    fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether there are no pairs.
    fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The value of a key.
    fn get(&self, key: &str) -> Option<&Value> {
        let slot = *self.index.get(key)?;
        self.slots[slot].as_ref().map(|(_, value)| value)
    }

    /// The value of a key, to change in place.
    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let slot = *self.index.get(key)?;
        self.slots[slot].as_mut().map(|(_, value)| value)
    }

    /// Inserts a pair at the end, or replaces the value of an existing key
    /// in place and returns the old value.
    fn insert(&mut self, key: EcoString, value: Value) -> Option<Value> {
        if let Some(&slot) = self.index.get(&key) {
            let (_, old) = self.slots[slot]
                .as_mut()
                .expect("indexed slots hold a pair");
            return Some(std::mem::replace(old, value));
        }
        self.index.insert(key.clone(), self.slots.len());
        self.slots.push(Some((key, value)));
        None
    }

    /// Removes a key, leaving an empty slot.
    fn remove(&mut self, key: &str) -> Option<Value> {
        let slot = self.index.remove(key)?;
        let (_, value) = self.slots[slot].take().expect("indexed slots hold a pair");
        if self.index.is_empty() {
            self.slots.clear();
        } else if self.slots.len() >= 2 * self.index.len() {
            self.compact();
        }
        Some(value)
    }

    /// Drops the empty slots, keeping the order of the pairs.
    fn compact(&mut self) {
        self.slots.retain(Option::is_some);
        for (slot, pair) in self.slots.iter().enumerate() {
            let (key, _) = pair.as_ref().expect("only pairs remain");
            *self.index.get_mut(key).expect("every pair is indexed") = slot;
        }
    }

    /// The pairs, in insertion order.
    fn iter(&self) -> PairsIter<'_> {
        PairsIter {
            slots: self.slots.iter(),
            remaining: self.len(),
        }
    }

    /// The values, in insertion order.
    fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }
}

/// The pairs of a dictionary, skipping empty slots.
struct PairsIter<'a> {
    slots: std::slice::Iter<'a, Option<(EcoString, Value)>>,
    /// The number of pairs not yet yielded.
    remaining: usize,
}

impl<'a> Iterator for PairsIter<'a> {
    type Item = (&'a EcoString, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        let (key, value) = self.slots.by_ref().flatten().next()?;
        self.remaining -= 1;
        Some((key, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for PairsIter<'_> {}

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
    With(Arc<Applied>),
}

impl Func {
    /// The function's name, if it has one.
    pub fn name(&self) -> Option<EcoString> {
        match self {
            Self::Native(f) => Some(f.name.into()),
            Self::Closure(c) => c.name().cloned(),
            Self::Element(name) => Some(eco_format!("{name}")),
            Self::Slot(slot) => Some(slot.1.as_str().into()),
            Self::With(inner) => inner.func().name(),
        }
    }

    /// How deeply the function nests: a closure's depth, one more than the
    /// deeper of the function and the arguments for a function with
    /// arguments applied, and `0` otherwise.
    pub fn depth(&self) -> u32 {
        match self {
            Self::Closure(c) => c.depth(),
            Self::With(with) => with
                .func()
                .depth()
                .max(with.args().depth())
                .saturating_add(1),
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

    /// Whether it has an argument sink (`..rest`), which holds the
    /// arguments it takes, spans and all.
    pub(crate) fn has_sink(&self) -> bool {
        self.node()
            .cast::<folio_syntax::ast::Closure<'_>>()
            .is_some_and(|c| {
                c.params()
                    .children()
                    .any(|p| matches!(p, folio_syntax::ast::Param::Spread(_)))
            })
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

/// A function with some arguments already applied: `f.with(x)`. Its hash
/// is cached, because a function can be applied to itself, `f.with(f)`, and
/// a chain of those holds one function many times.
#[derive(Debug)]
pub struct Applied {
    func: Func,
    args: Args,
    hash: LazyHash,
}

impl Applied {
    /// A function with arguments applied.
    pub fn new(func: Func, args: Args) -> Self {
        Self {
            func,
            args,
            hash: LazyHash::new(true),
        }
    }

    /// The function.
    pub fn func(&self) -> &Func {
        &self.func
    }

    /// The arguments applied.
    pub fn args(&self) -> &Args {
        &self.args
    }
}

impl PartialEq for Applied {
    fn eq(&self, other: &Self) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        if self.hash.differ(&other.hash) {
            return false;
        }
        compare_once(
            std::ptr::from_ref(self) as usize,
            std::ptr::from_ref(other) as usize,
            Equality::Strict,
            || self.func == other.func && self.args == other.args,
        )
    }
}

impl Eq for Applied {}

impl Hash for Applied {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.write(state, |mut state| {
            self.func.hash(&mut state);
            self.args.hash(&mut state);
        });
    }
}

/// The arguments of a call.
///
/// Their hash is cached, because a function can return its arguments
/// (`let f(..a) = a`), which can then be the arguments of another call
/// many times over: `f(a, a)`. A change to the arguments forgets it.
#[derive(Debug, Clone)]
pub struct Args {
    /// The span of the whole call.
    pub span: Span,
    /// The arguments, positional and named, in order.
    pub(crate) items: EcoVec<Arg>,
    /// One more than the deepest value, as the arguments were made. Taking
    /// arguments leaves it an upper bound; arguments that are a value are
    /// never changed, so theirs is exact.
    depth: Depth,
    hash: LazyHash,
}

impl Args {
    /// Arguments of a call.
    pub fn new(span: Span, items: EcoVec<Arg>) -> Self {
        let depth = depth_of(items.iter().map(|arg| &arg.value));
        Self {
            span,
            items,
            depth: Depth(depth),
            hash: LazyHash::new(true),
        }
    }

    /// Forgets the hash after the arguments changed.
    pub(crate) fn changed(&mut self) {
        self.hash.reset(true);
    }

    /// The arguments, positional and named, in order.
    pub fn items(&self) -> &[Arg] {
        &self.items
    }

    /// How deeply the arguments nest: one more than the deepest value.
    pub fn depth(&self) -> u32 {
        self.depth.0
    }

    /// The arguments with the spans of the call and of each argument
    /// detached, which makes the same call at two sites equal. The values
    /// keep their own spans.
    pub(crate) fn without_spans(&self) -> Self {
        let items = self
            .items
            .iter()
            .map(|arg| Arg {
                span: Span::detached(),
                ..arg.clone()
            })
            .collect();
        Self {
            span: Span::detached(),
            items,
            depth: self.depth,
            hash: LazyHash::new(true),
        }
    }
}

impl PartialEq for Args {
    fn eq(&self, other: &Self) -> bool {
        if self.span != other.span
            || self.items.len() != other.items.len()
            || self.hash.differ(&other.hash)
        {
            return false;
        }
        if self.items.as_ptr() == other.items.as_ptr() {
            return true;
        }
        compare_once(
            self.items.as_ptr() as usize,
            other.items.as_ptr() as usize,
            Equality::Strict,
            || self.items == other.items,
        )
    }
}

impl Eq for Args {}

impl Hash for Args {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.span.hash(state);
        self.hash
            .write(state, |mut state| self.items.hash(&mut state));
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

    /// The sizes the memory limit counts are at least what the values take
    /// on a 64-bit platform, so that the limit bounds what is allocated.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn counted_sizes_bound_the_real_ones() {
        use std::mem::size_of;
        assert!(size_of::<Value>() as u64 <= ITEM);
        assert!(size_of::<Arg>() as u64 <= NODE);
        assert!(size_of::<super::super::content::Node>() as u64 <= NODE);
        assert!(size_of::<(EcoString, Value)>() as u64 + 32 <= NODE);
        assert!(size_of::<folio_math::Node>() as u64 <= 64);
    }

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
        let with = Func::With(Arc::new(Applied::new(
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
    fn only_the_last_of_the_deepest_items_lowers_the_depth() {
        let mut array: Array = (0..1000).map(|_| nested(3)).collect();
        array.push(Value::Int(0));
        array.insert(0, nested(1));
        assert_eq!(array.depth(), 4);
        // Every deep item but the last leaves the depth as it is.
        for _ in 0..999 {
            array.remove(1);
            assert_eq!(array.depth(), 4);
        }
        array.remove(1);
        assert_eq!(array.depth(), 2);
        // An item made deeper and then as deep as before is counted once.
        array.update(0, |item| *item = nested(5));
        array.update(0, |item| *item = nested(5));
        assert_eq!(array.depth(), 6);
        array.update(0, |item| *item = Value::None);
        assert_eq!(array.depth(), 1);

        let mut dict = Dict::new();
        dict.insert("a".into(), nested(2));
        dict.insert("b".into(), nested(2));
        dict.insert("a".into(), nested(2));
        assert_eq!(dict.depth(), 3);
        dict.remove("a");
        assert_eq!(dict.depth(), 3);
        dict.update("b", false, |v| *v = Value::None);
        assert_eq!(dict.depth(), 1);
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

    fn hash_of(value: &impl Hash) -> u64 {
        let mut state = DefaultHasher::new();
        value.hash(&mut state);
        state.finish()
    }

    #[test]
    fn a_change_forgets_the_cached_hash() {
        // An array that holds arrays caches its hash. After each change it
        // hashes as a new array with the same items does, while a clone made
        // before the change keeps the old hash.
        let mut array = Array::from([nested(2), nested(2)]);
        let before = hash_of(&array);
        let clone = array.clone();
        array.push(nested(3));
        assert_eq!(
            hash_of(&array),
            hash_of(&Array::from([nested(2), nested(2), nested(3)]))
        );
        assert_ne!(hash_of(&array), before);
        assert_eq!(hash_of(&clone), before);
        array.update(0, |item| *item = Value::Int(1));
        assert_eq!(
            hash_of(&array),
            hash_of(&Array::from([Value::Int(1), nested(2), nested(3)]))
        );
        array.pop();
        array.remove(1);
        assert_eq!(hash_of(&array), hash_of(&Array::from([Value::Int(1)])));

        let mut dict: Dict = [("a".into(), nested(2))].into_iter().collect();
        let _ = hash_of(&dict);
        dict.insert("a".into(), nested(4));
        let fresh: Dict = [("a".into(), nested(4))].into_iter().collect();
        assert_eq!(hash_of(&dict), hash_of(&fresh));
        assert_eq!(dict, fresh);
        dict.update("a", false, |v| *v = Value::None);
        let fresh: Dict = [("a".into(), Value::None)].into_iter().collect();
        assert_eq!(hash_of(&dict), hash_of(&fresh));

        // Arguments forget theirs when an argument is taken.
        let arg = |value| Arg {
            span: Span::detached(),
            name: None,
            scope: None,
            value,
        };
        let mut args = Args::new(
            Span::detached(),
            EcoVec::from([arg(nested(3)), arg(Value::Int(1))]),
        );
        let _ = hash_of(&args);
        args.eat();
        let fresh = Args::new(Span::detached(), EcoVec::from([arg(Value::Int(1))]));
        assert_eq!(hash_of(&args), hash_of(&fresh));
        assert_eq!(args, fresh);
    }

    #[test]
    fn values_that_differ_by_a_shared_part_are_unequal() {
        let mut a = Array::from([nested(2)]);
        let mut b = Array::from([nested(2)]);
        for _ in 0..40 {
            a = Array::from([Value::Array(a.clone()), Value::Array(a)]);
            b = Array::from([Value::Array(b.clone()), Value::Array(b)]);
        }
        assert_eq!(a, b);
        let c = b.clone();
        b.update(0, |item| *item = nested(2));
        assert_ne!(b, c);
        assert_ne!(a, b);
        // Hashes once computed tell unequal values apart at once.
        assert_ne!(hash_of(&a), hash_of(&b));
        assert_ne!(a, b);
    }

    #[test]
    fn arguments_and_applied_functions_built_alike_compare_once_per_part() {
        // Arguments that hold the arguments before them twice, and
        // functions applied to themselves, each 2^60 parts as trees.
        let args = |value: &Value| {
            let arg = Arg {
                span: Span::detached(),
                name: None,
                scope: None,
                value: value.clone(),
            };
            Args::new(Span::detached(), EcoVec::from([arg.clone(), arg]))
        };
        let applied = |func: &Func| {
            let args = args(&Value::Func(func.clone()));
            Func::With(Arc::new(Applied::new(func.clone(), args)))
        };
        let strong = || Func::Element(QualifiedName::unqualified(Name::new("strong").unwrap()));
        let (mut a, mut b, mut c) = (Value::Int(1), Value::Int(1), Value::Int(2));
        let (mut f, mut g) = (strong(), strong());
        for _ in 0..60 {
            a = Value::Args(args(&a));
            b = Value::Args(args(&b));
            c = Value::Args(args(&c));
            f = applied(&f);
            g = applied(&g);
        }
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(f, g);
        assert_eq!(hash_of(&a), hash_of(&b));
        assert_eq!(hash_of(&f), hash_of(&g));
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

    #[test]
    fn removed_keys_leave_order_equality_hashing_and_depth_as_if_never_inserted() {
        // Removing keys from the front leaves empty slots, compacted along
        // the way; the result must match a dictionary built from what is
        // left, in every way a script or a memoised query can see.
        let mut dict = Dict::new();
        for i in 0..100 {
            let value = if i == 0 { nested(4) } else { Value::Int(i) };
            dict.insert(eco_format!("{i}"), value);
        }
        for i in 0..90 {
            dict.remove(&eco_format!("{i}"));
            let left: Dict = dict.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            assert_eq!(dict.len(), 99 - i);
            assert_eq!(dict.iter().len(), 99 - i);
            assert_eq!(dict, left);
            assert_eq!(hash_of(&dict), hash_of(&left));
            assert_eq!(dict.depth(), left.depth());
            // Fewer than half of the slots are ever empty, so removal stays
            // amortised constant time rather than shifting later pairs.
            assert!(dict.map.slots.len() < 2 * dict.len());
        }
        // A removed key inserted again goes to the end; an existing one is
        // replaced in place.
        dict.insert("5".into(), Value::Int(5));
        dict.insert("95".into(), Value::Int(-1));
        let keys: Vec<_> = dict.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "90", "91", "92", "93", "94", "95", "96", "97", "98", "99", "5"
            ]
        );
        assert_eq!(dict.get("95"), Some(&Value::Int(-1)));
        assert_eq!(dict.get("0"), None);
        assert_eq!(dict.remove("0"), None);
        // Two dictionaries with the same pairs in another order differ.
        let mut other = Dict::new();
        other.insert("b".into(), Value::Int(2));
        other.insert("a".into(), Value::Int(1));
        let mut ordered = Dict::new();
        ordered.insert("a".into(), Value::Int(1));
        ordered.insert("x".into(), Value::Int(0));
        ordered.insert("b".into(), Value::Int(2));
        ordered.remove("x");
        assert_ne!(ordered, other);
        other.remove("b");
        other.insert("b".into(), Value::Int(2));
        assert_eq!(ordered, other);
        assert_eq!(hash_of(&ordered), hash_of(&other));
        // Emptying a dictionary empties it.
        for key in ["a", "b"] {
            other.remove(key);
        }
        assert!(other.is_empty());
        assert_eq!(other, Dict::new());
        assert_eq!(hash_of(&other), hash_of(&Dict::new()));
    }
}
