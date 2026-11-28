//! Computed styles (SPEC §5.1, §5.2): a value for every property of a node,
//! stored as shared, reference-counted property groups.
//!
//! A computed style holds one [`PropertyGroup`] per group of the registry.
//! Groups are interned while a build is styled, so nodes with equal groups
//! share one allocation, and nodes with equal styles share one style. A
//! group that no rule touches for a node is not copied: it is the parent's
//! group when its properties inherit, and the initial group otherwise. Each
//! group carries its hash, and a second hash over its layout-affecting
//! properties only, so the style hashes in layout cache keys (SPEC §2.3)
//! cost one combination per group.

use std::collections::{BTreeMap, HashSet};
use std::fmt::{self, Debug, Formatter};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use ecow::EcoVec;
use folio_component::{Abs, Effect, Value};
use folio_syntax::SyntaxNode;

use super::props::{GroupId, PropId, Properties};
use super::rules::DeclValue;
use super::value::{coerce, compute as compute_value};

/// The computed value of a property.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Computed {
    /// A value as plain data, with lengths absolute.
    Value(Value),
    /// A render template: the declaration that set it, which the template
    /// stage expands per element (SPEC §5.3).
    Template(SyntaxNode),
}

impl Computed {
    /// The value, unless it is a template.
    pub fn value(&self) -> Option<&Value> {
        match self {
            Self::Value(value) => Some(value),
            Self::Template(_) => None,
        }
    }
}

/// The computed values of one property group, in slot order.
#[derive(Clone)]
pub struct PropertyGroup {
    group: GroupId,
    values: EcoVec<Computed>,
    hash: u64,
    layout_hash: u64,
}

impl PropertyGroup {
    fn new(props: &Properties, group: GroupId, values: EcoVec<Computed>) -> Self {
        let mut all = DefaultHasher::new();
        let mut layout = DefaultHasher::new();
        group.hash(&mut all);
        group.hash(&mut layout);
        for (id, value) in props.members(group).iter().zip(&values) {
            value.hash(&mut all);
            if props.get(*id).effect == Effect::Layout {
                value.hash(&mut layout);
            }
        }
        Self {
            group,
            values,
            hash: all.finish(),
            layout_hash: layout.finish(),
        }
    }

    /// The group.
    pub fn id(&self) -> GroupId {
        self.group
    }

    /// The values, in slot order.
    pub fn values(&self) -> &[Computed] {
        &self.values
    }

    /// A hash of every value.
    pub fn hash_value(&self) -> u64 {
        self.hash
    }

    /// A hash of the values of the layout-affecting properties: equal for
    /// groups that differ only in paint (SPEC §10.2).
    pub fn layout_hash(&self) -> u64 {
        self.layout_hash
    }
}

impl Debug for PropertyGroup {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PropertyGroup")
            .field("group", &self.group)
            .field("values", &self.values)
            .finish()
    }
}

impl PartialEq for PropertyGroup {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.group == other.group && self.values == other.values
    }
}

impl Eq for PropertyGroup {}

impl Hash for PropertyGroup {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

/// The computed style of a node: one shared group per property group.
/// Cloning it is one reference count.
#[derive(Clone)]
pub struct ComputedStyle(Arc<StyleData>);

struct StyleData {
    groups: Vec<Arc<PropertyGroup>>,
    hash: u64,
    layout_hash: u64,
}

impl ComputedStyle {
    fn new(groups: Vec<Arc<PropertyGroup>>) -> Self {
        let mut all = DefaultHasher::new();
        let mut layout = DefaultHasher::new();
        for group in &groups {
            all.write_u64(group.hash);
            layout.write_u64(group.layout_hash);
        }
        Self(Arc::new(StyleData {
            groups,
            hash: all.finish(),
            layout_hash: layout.finish(),
        }))
    }

    /// The computed value of a property.
    pub fn get(&self, props: &Properties, id: PropId) -> &Computed {
        let prop = props.get(id);
        &self.0.groups[prop.group.index()].values[prop.slot as usize]
    }

    /// The computed value of a property, unless it is a template.
    pub fn value(&self, props: &Properties, id: PropId) -> Option<&Value> {
        self.get(props, id).value()
    }

    /// A property group.
    pub fn group(&self, group: GroupId) -> &Arc<PropertyGroup> {
        &self.0.groups[group.index()]
    }

    /// The groups, by group index.
    pub fn groups(&self) -> &[Arc<PropertyGroup>] {
        &self.0.groups
    }

    /// A hash of every value, cheap to read.
    pub fn hash_value(&self) -> u64 {
        self.0.hash
    }

    /// A hash of the layout-affecting values: what layout cache keys use
    /// (SPEC §2.3, §10.2).
    pub fn layout_hash(&self) -> u64 {
        self.0.layout_hash
    }

    /// Whether two styles are the same allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Debug for ComputedStyle {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "ComputedStyle({:016x})", self.0.hash)
    }
}

impl PartialEq for ComputedStyle {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
            || (self.0.hash == other.0.hash
                && self
                    .0
                    .groups
                    .iter()
                    .zip(&other.0.groups)
                    .all(|(a, b)| Arc::ptr_eq(a, b) || a == b))
    }
}

impl Eq for ComputedStyle {}

impl Hash for ComputedStyle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash);
    }
}

/// Makes computed styles, sharing equal groups and equal styles.
pub(super) struct Styler<'a> {
    props: &'a Properties,
    groups: HashSet<Arc<PropertyGroup>>,
    styles: HashSet<ComputedStyle>,
    initial: ComputedStyle,
    /// The font size `initial` resolves `em` lengths at.
    initial_size: Abs,
    font_size: PropId,
    /// What each group's properties have in common, by group index.
    kinds: Vec<GroupKind>,
}

/// What the properties of a group have in common, which decides where a
/// group that no declaration touches comes from.
#[derive(Debug, Copy, Clone)]
struct GroupKind {
    /// Every property inherits.
    all_inherit: bool,
    /// No property inherits.
    none_inherit: bool,
    /// A property that inherits has an initial value in `em`.
    em_inherited: bool,
    /// A property that does not inherit has an initial value in `em`.
    em_own: bool,
}

impl<'a> Styler<'a> {
    pub(super) fn new(props: &'a Properties) -> Self {
        let font_size = props.builtin("font-size");
        let mut this = Self {
            props,
            groups: HashSet::new(),
            styles: HashSet::new(),
            initial: ComputedStyle::new(Vec::new()),
            initial_size: Abs::pt(11.0),
            font_size,
            kinds: (0..props.group_count())
                .map(|g| {
                    let members = props.members(GroupId(g as u16));
                    let all = |f: &dyn Fn(PropId) -> bool| members.iter().all(|&id| f(id));
                    let any = |f: &dyn Fn(PropId) -> bool| members.iter().any(|&id| f(id));
                    let inherited = |id: PropId| props.get(id).inherited;
                    let em = |id: PropId| has_em(&props.get(id).initial);
                    GroupKind {
                        all_inherit: all(&inherited),
                        none_inherit: all(&|id| !inherited(id)),
                        em_inherited: any(&|id| em(id) && inherited(id)),
                        em_own: any(&|id| em(id) && !inherited(id)),
                    }
                })
                .collect(),
        };
        let size = match &props.get(font_size).initial {
            Value::Length(size) => *size,
            _ => Abs::pt(11.0),
        };
        this.initial_size = size;
        let groups = (0..props.group_count())
            .map(|g| {
                let group = GroupId(g as u16);
                let values = props
                    .members(group)
                    .iter()
                    .map(|&id| {
                        let initial = &props.get(id).initial;
                        Computed::Value(resolve_em(initial, size))
                    })
                    .collect();
                this.group(PropertyGroup::new(props, group, values))
            })
            .collect();
        this.initial = this.style(groups);
        this
    }

    fn group(&mut self, group: PropertyGroup) -> Arc<PropertyGroup> {
        if let Some(shared) = self.groups.get(&group) {
            return shared.clone();
        }
        let group = Arc::new(group);
        self.groups.insert(group.clone());
        group
    }

    fn style(&mut self, groups: Vec<Arc<PropertyGroup>>) -> ComputedStyle {
        let style = ComputedStyle::new(groups);
        if let Some(shared) = self.styles.get(&style) {
            return shared.clone();
        }
        self.styles.insert(style.clone());
        style
    }

    /// `style` with the values of the properties `ids` taken from `from`:
    /// a parent made of two, such as the document's style with the
    /// language and font of a note's anchor (SPEC §7.8).
    pub(super) fn adopt(
        &mut self,
        style: &ComputedStyle,
        from: &ComputedStyle,
        ids: &[PropId],
    ) -> ComputedStyle {
        let props = self.props;
        let mut groups = style.0.groups.clone();
        let mut changed = false;
        for &id in ids {
            let prop = props.get(id);
            let (group, slot) = (prop.group, prop.slot as usize);
            let value = from.get(props, id);
            if groups[group.index()].values[slot] == *value {
                continue;
            }
            let mut values = groups[group.index()].values.clone();
            values.make_mut()[slot] = value.clone();
            groups[group.index()] = self.group(PropertyGroup::new(props, group, values));
            changed = true;
        }
        if !changed {
            return style.clone();
        }
        self.style(groups)
    }

    /// The style of a node with a parent's style and the declarations that
    /// apply to it, lowest priority first: a later declaration of a
    /// property replaces an earlier one. A value that does not fit its
    /// property is left out; the caller checks values that a stylesheet did
    /// not.
    pub(super) fn compute<'d>(
        &mut self,
        parent: Option<&ComputedStyle>,
        decls: impl IntoIterator<Item = (PropId, &'d DeclValue)>,
    ) -> ComputedStyle {
        let props = self.props;
        let cascaded: BTreeMap<PropId, &DeclValue> = decls.into_iter().collect();
        let base = parent.unwrap_or(&self.initial).clone();

        // The font size first: `em` lengths of the node are relative to it,
        // and those of its font size to the parent's.
        let parent_size = self.size_of(&base);
        let size = match cascaded.get(&self.font_size) {
            Some(DeclValue::Value(value)) => match compute_value(value, parent_size) {
                Ok(Value::Length(size)) => size,
                _ => parent_size,
            },
            _ => parent_size,
        };

        let mut touched_groups = vec![false; props.group_count()];
        for &id in cascaded.keys() {
            touched_groups[props.get(id).group.index()] = true;
        }
        let mut groups = Vec::with_capacity(props.group_count());
        for (g, &touched) in touched_groups.iter().enumerate() {
            let group = GroupId(g as u16);
            let members = props.members(group);
            let GroupKind {
                all_inherit,
                none_inherit,
                em_inherited,
                em_own,
            } = self.kinds[g];
            // An initial value in `em` is relative to the node's own font
            // size, so a group that falls back to one cannot be shared
            // when the size differs from the initial style's.
            let em_initial =
                size != self.initial_size && (em_own || (em_inherited && parent.is_none()));
            // The group the node has where no declaration touches it: the
            // parent's, or the initial one.
            let untouched = if em_initial {
                None
            } else if all_inherit || parent.is_none() {
                Some(base.group(group))
            } else if none_inherit {
                Some(self.initial.group(group))
            } else {
                None
            };
            if let Some(untouched) = untouched {
                // Only the declared values can differ from it, and a group
                // whose declarations set the values it has anyway is the
                // same group, shared without hashing it again.
                let mut values = untouched.values.clone();
                let mut changed = false;
                if touched {
                    for (slot, &id) in members.iter().enumerate() {
                        let Some(decl) = cascaded.get(&id) else {
                            continue;
                        };
                        let value = self.declared(id, decl, parent, size);
                        if values[slot] != value {
                            values.make_mut()[slot] = value;
                            changed = true;
                        }
                    }
                }
                groups.push(if changed {
                    self.group(PropertyGroup::new(props, group, values))
                } else {
                    untouched.clone()
                });
                continue;
            }
            let mut values = EcoVec::with_capacity(members.len());
            for &id in members {
                values.push(match cascaded.get(&id) {
                    Some(decl) => self.declared(id, decl, parent, size),
                    None => self.fallback(id, parent, size),
                });
            }
            groups.push(self.group(PropertyGroup::new(props, group, values)));
        }
        if groups
            .iter()
            .zip(base.groups())
            .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            return base;
        }
        self.style(groups)
    }

    /// The computed value of a declared property at a font size. A value
    /// that does not fit its property falls back as if it were not
    /// declared. The font size itself is `size`, which already holds that
    /// fallback, the parent's size, where the declared one is not a length.
    fn declared(
        &self,
        id: PropId,
        decl: &DeclValue,
        parent: Option<&ComputedStyle>,
        size: Abs,
    ) -> Computed {
        let prop = self.props.get(id);
        match decl {
            DeclValue::Template(node) => Computed::Template(node.clone()),
            DeclValue::Value(_) if id == self.font_size => Computed::Value(Value::Length(size)),
            DeclValue::Value(value) => match compute_value(value, size) {
                Ok(v) => {
                    let v = coerce(v, &prop.ty, true);
                    if prop.ty.accepts(&v) {
                        Computed::Value(v)
                    } else {
                        self.fallback(id, parent, size)
                    }
                }
                Err(_) => self.fallback(id, parent, size),
            },
        }
    }

    /// The font size of a style, which `em` lengths are relative to.
    fn size_of(&self, style: &ComputedStyle) -> Abs {
        match style.value(self.props, self.font_size) {
            Some(Value::Length(size)) => *size,
            _ => Abs::pt(11.0),
        }
    }

    /// The value of a property that no declaration sets: the parent's if
    /// it inherits and there is a parent, the initial value otherwise, with
    /// `em` lengths relative to the node's font size.
    fn fallback(&self, id: PropId, parent: Option<&ComputedStyle>, size: Abs) -> Computed {
        let prop = self.props.get(id);
        match parent {
            Some(parent) if prop.inherited => parent.get(self.props, id).clone(),
            _ if size != self.initial_size && has_em(&prop.initial) => {
                Computed::Value(resolve_em(&prop.initial, size))
            }
            _ => self.initial.get(self.props, id).clone(),
        }
    }
}

/// Whether a value holds `em` lengths.
fn has_em(value: &Value) -> bool {
    match value {
        Value::Em(_) => true,
        Value::Array(items) => items.iter().any(has_em),
        _ => false,
    }
}

/// Makes `em` lengths of an initial value absolute.
fn resolve_em(value: &Value, size: Abs) -> Value {
    match value {
        Value::Em(em) => Value::Length(em.at(size)),
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve_em(v, size)).collect()),
        other => other.clone(),
    }
}
