//! Selectors (SPEC §5.1): compiled selectors, their specificity, what they
//! need to see of the document, and matching.
//!
//! Matching does not walk the document. It sees a node through a
//! [`NodeView`]: the node's name, classes, label and tested fields, and only
//! as much of its position, siblings and children as the build's rules use
//! ([`Needs`]). Ancestors are seen through views of their own. Two nodes
//! with equal views and equal ancestor views match the same rules, so the
//! style stage matches a node again only when its view changes: after an
//! insertion that is the inserted node, and the siblings whose position or
//! previous siblings some rule tests (SPEC §5.1).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use ecow::{EcoString, EcoVec};
use folio_component::{Name, QualifiedName};

use crate::eval::{self, ops};

pub use folio_syntax::ast::{AttrOp, Combinator};

/// The name a node has for selectors.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NodeName {
    /// An element: a built-in name, or a component element qualified with
    /// its namespace.
    Element(QualifiedName),
    /// A part of an element: the content of a slot or of a content field
    /// other than the body, such as `solution` in `example > solution`.
    Part(Name),
    /// A page master: `page`.
    Page,
}

/// What selectors see of a node itself.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Subject {
    /// Its name.
    pub name: NodeName,
    /// Its classes.
    pub classes: EcoVec<EcoString>,
    /// Its label.
    pub label: Option<EcoString>,
    /// The fields that attribute tests of the build's rules name, sorted by
    /// name; a field that is not set is left out.
    pub fields: EcoVec<(EcoString, Attr)>,
}

/// The value of a field as attribute tests see it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Attr {
    /// A number, string, boolean or label.
    Value(eval::Value),
    /// Content, arrays and other values that tests can only find set.
    Set,
}

impl Attr {
    /// The field's value as attribute tests see it, or `None` if it counts
    /// as not set.
    pub fn of(value: &eval::Value) -> Option<Self> {
        use eval::Value as V;
        Some(match value {
            V::None => return None,
            V::Bool(_) | V::Int(_) | V::Float(_) | V::Str(_) | V::Ratio(_) | V::Length(_) => {
                Self::Value(value.clone())
            }
            V::Label(label) => Self::Value(V::Str(label.clone())),
            V::Content(content) if content.is_empty() => return None,
            _ => Self::Set,
        })
    }
}

/// A node's position among its siblings, as far as rules test it.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Position {
    /// Whether it is the first element among its siblings: `:first`.
    pub first: bool,
    /// Whether it is the last: `:last`.
    pub last: bool,
    /// Its position from 1: `:nth(n)`.
    pub index: u32,
    /// Its column in a table, from 1: `:nth-col(n)`.
    pub column: u32,
}

/// What matching sees of a node (see the module documentation).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct NodeView {
    /// The node itself.
    pub subject: Subject,
    /// Its position, with only the parts that rules test.
    pub pos: Position,
    /// Its previous siblings, nearest first, as far back as `+` chains
    /// reach.
    pub prev: EcoVec<NodeView>,
    /// Its direct children, if a `:has` rule may test them.
    pub children: EcoVec<Subject>,
}

/// What rules need to see of a node beyond its subject: in the compound
/// they style (the subject), or in the compounds before it (the context:
/// ancestors and previous siblings).
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Need {
    /// `:first`.
    pub first: bool,
    /// `:last`.
    pub last: bool,
    /// `:nth(n)`.
    pub index: bool,
    /// `:nth-col(n)`.
    pub column: bool,
    /// How many previous siblings `+` chains look at.
    pub prev: usize,
    /// `:has(..)`.
    pub children: bool,
}

impl Need {
    /// Whether any rule depends on siblings here: an insertion then
    /// changes the views of the siblings of the inserted node (SPEC §5.1).
    pub fn has_sibling_dependencies(&self) -> bool {
        self.last || self.index || self.column || self.prev > 0 || self.first
    }

    fn union(self, other: Self) -> Self {
        Self {
            first: self.first || other.first,
            last: self.last || other.last,
            index: self.index || other.index,
            column: self.column || other.column,
            prev: self.prev.max(other.prev),
            children: self.children || other.children,
        }
    }

    /// The parts of a position this need keeps.
    pub fn position(&self, full: Position) -> Position {
        Position {
            first: self.first && full.first,
            last: self.last && full.last,
            index: if self.index { full.index } else { 0 },
            column: if self.column { full.column } else { 0 },
        }
    }
}

/// What a set of rules needs to see of nodes, by the nodes' names: a
/// compound that names an element adds what it tests only to the views of
/// nodes of that name. `heading[level=1]` makes headings show their level
/// and no other node; `heading + paragraph` makes paragraphs show their
/// previous sibling, as subjects, and headings what the rule tests of
/// them, as context.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Needs {
    /// What compounds without a name need, of every node.
    any: NameNeeds,
    /// What nodes of a name need: what compounds of that name need, and
    /// what compounds without a name do.
    named: BTreeMap<NodeName, NameNeeds>,
}

/// What a set of rules needs to see of the nodes of one name.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct NameNeeds {
    /// As the node being styled.
    pub subject: Need,
    /// As an ancestor or a previous sibling.
    pub context: Need,
    /// The fields attribute tests name.
    pub fields: BTreeSet<EcoString>,
}

/// The part a node plays for a selector.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Role {
    /// The node being styled.
    Subject,
    /// An ancestor or a previous sibling of it.
    Context,
}

impl NameNeeds {
    /// What a node needs in a role.
    pub fn need(&self, role: Role) -> &Need {
        match role {
            Role::Subject => &self.subject,
            Role::Context => &self.context,
        }
    }

    fn extend(&mut self, other: &Self) {
        self.subject = self.subject.union(other.subject);
        self.context = self.context.union(other.context);
        self.fields.extend(other.fields.iter().cloned());
    }
}

impl Needs {
    /// What the rules need to see of a node of a name.
    pub fn of(&self, name: &NodeName) -> &NameNeeds {
        self.named.get(name).unwrap_or(&self.any)
    }

    /// Adds what a selector needs.
    pub fn add(&mut self, selector: &Selector) {
        let last = selector.steps.len() - 1;
        let mut run = 0;
        for (i, (combinator, compound)) in selector.steps.iter().enumerate() {
            run = match combinator {
                Some(Combinator::NextSibling) => run + 1,
                _ => 0,
            };
            let ends_segment = selector
                .steps
                .get(i + 1)
                .is_none_or(|(c, _)| *c != Some(Combinator::NextSibling));
            let mut add = NameNeeds::default();
            let need = compound.need();
            if i == last {
                add.subject = need;
                add.subject.prev = run;
            } else {
                add.context = need;
                if ends_segment {
                    add.context.prev = run;
                }
            }
            compound.own_fields(&mut add.fields);
            self.add_for(compound.name.as_ref(), &add);
            // The children that `:has` tests are seen with the fields its
            // compounds name, as nodes of their own names.
            for pseudo in &compound.pseudos {
                if let Pseudo::Has(inner) = pseudo {
                    let mut add = NameNeeds::default();
                    inner.own_fields(&mut add.fields);
                    self.add_for(inner.name.as_ref(), &add);
                }
            }
        }
    }

    /// Adds what the nodes of a name need, or every node for `None`.
    fn add_for(&mut self, name: Option<&NodeName>, add: &NameNeeds) {
        match name {
            Some(name) => self
                .named
                .entry(name.clone())
                .or_insert_with(|| self.any.clone())
                .extend(add),
            None => {
                self.any.extend(add);
                for needs in self.named.values_mut() {
                    needs.extend(add);
                }
            }
        }
    }

    /// Adds what another set needs.
    pub fn extend(&mut self, other: &Self) {
        self.add_for(None, &other.any);
        for (name, needs) in &other.named {
            self.add_for(Some(name), needs);
        }
    }
}

/// A compiled selector: compound selectors joined by combinators.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Selector {
    /// The compounds from left to right, each with the combinator that
    /// joins it to the one before (`None` for the first).
    pub steps: EcoVec<(Option<Combinator>, Compound)>,
}

/// The specificity of a selector: ids, then classes, attribute tests and
/// pseudo-classes, then element names (SPEC §5.1). Each count saturates
/// at `u16::MAX`.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Specificity(pub u16, pub u16, pub u16);

impl Selector {
    /// The selector's specificity.
    pub fn specificity(&self) -> Specificity {
        let mut out = Specificity::default();
        for (_, compound) in &self.steps {
            compound.add_specificity(&mut out);
        }
        out
    }

    /// The compound that the selector styles.
    pub fn subject(&self) -> &Compound {
        &self.steps.last().expect("a selector has a compound").1
    }

    /// Whether the selector matches the node whose view is `chain[0]`, with
    /// the views of its ancestors after it, nearest first.
    ///
    /// Matching is polynomial in the chain's length: a descendant
    /// combinator tries each ancestor, and a place in the chain where the
    /// compounds up to some step failed to match is remembered, so another
    /// way of reaching it from a later step does not try it again.
    pub fn matches(&self, chain: &[&NodeView]) -> bool {
        !chain.is_empty()
            && self.match_at(
                self.steps.len() - 1,
                At {
                    level: 0,
                    offset: 0,
                },
                chain,
                &mut Failures::default(),
            )
    }

    fn match_at(&self, i: usize, at: At, chain: &[&NodeView], failed: &mut Failures) -> bool {
        let (combinator, compound) = &self.steps[i];
        if failed.contains(i, at) {
            return false;
        }
        let Some(view) = at.view(chain) else {
            return false;
        };
        if !compound.matches(view) {
            return false;
        }
        if i == 0 {
            return true;
        }
        let found = match combinator.expect("compounds after the first have a combinator") {
            Combinator::Child => at
                .parent(chain)
                .is_some_and(|p| self.match_at(i - 1, p, chain, failed)),
            Combinator::Descendant => {
                let mut ancestor = at.parent(chain);
                let mut found = false;
                while let Some(p) = ancestor {
                    if self.match_at(i - 1, p, chain, failed) {
                        found = true;
                        break;
                    }
                    ancestor = p.parent(chain);
                }
                found
            }
            Combinator::NextSibling => at
                .prev(chain)
                .is_some_and(|p| self.match_at(i - 1, p, chain, failed)),
        };
        // Only a compound that a descendant combinator joins to the next
        // can be reached from more than one place, so only its failures
        // are worth remembering: through `>` and `+` each place has one
        // way in, and that way in is remembered if it can recur.
        if !found
            && self
                .steps
                .get(i + 1)
                .is_some_and(|(c, _)| *c == Some(Combinator::Descendant))
        {
            failed.insert(i, at);
        }
        found
    }
}

/// The places in a chain of views from which the compounds up to a step
/// failed to match although the step's own compound matched there:
/// `(step, level, offset)`. Only a selector with a descendant combinator
/// before its third compound, or a later one, records any, so the set is
/// allocated only when it is first needed.
#[derive(Default)]
struct Failures(Option<HashSet<(usize, usize, usize)>>);

impl Failures {
    fn contains(&self, i: usize, at: At) -> bool {
        self.0
            .as_ref()
            .is_some_and(|set| set.contains(&(i, at.level, at.offset)))
    }

    fn insert(&mut self, i: usize, at: At) {
        self.0
            .get_or_insert_with(HashSet::new)
            .insert((i, at.level, at.offset));
    }
}

/// A place in a chain of views: an ancestor level, and a previous sibling
/// of the node at that level (0 for the node itself).
#[derive(Debug, Copy, Clone)]
struct At {
    level: usize,
    offset: usize,
}

impl At {
    fn view<'a>(self, chain: &[&'a NodeView]) -> Option<&'a NodeView> {
        let node = chain.get(self.level)?;
        match self.offset {
            0 => Some(node),
            k => node.prev.get(k - 1),
        }
    }

    fn parent(self, chain: &[&NodeView]) -> Option<Self> {
        (self.level + 1 < chain.len()).then_some(Self {
            level: self.level + 1,
            offset: 0,
        })
    }

    fn prev(self, chain: &[&NodeView]) -> Option<Self> {
        let node = chain.get(self.level)?;
        (self.offset < node.prev.len()).then_some(Self {
            level: self.level,
            offset: self.offset + 1,
        })
    }
}

/// A compound selector: `heading[level=1]:first`.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Compound {
    /// The name, if the compound has one.
    pub name: Option<NodeName>,
    /// Classes the node must have.
    pub classes: EcoVec<EcoString>,
    /// The label the node must have: `#{fig:ball}`.
    pub label: Option<EcoString>,
    /// Attribute tests.
    pub attrs: EcoVec<AttrTest>,
    /// Structural pseudo-classes.
    pub pseudos: EcoVec<Pseudo>,
}

/// An attribute test: `[level=1]`, `[marks>3]`, `[title]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct AttrTest {
    /// The field.
    pub field: EcoString,
    /// The comparison and the value compared with; `None` tests that the
    /// field is set.
    pub test: Option<(AttrOp, eval::Value)>,
}

/// A structural pseudo-class.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Pseudo {
    /// `:first`.
    First,
    /// `:last`.
    Last,
    /// `:nth(n)`, from 1.
    Nth(u32),
    /// `:nth-col(n)`, from 1.
    NthCol(u32),
    /// `:has(child)`: a direct child matches the compound, which has no
    /// pseudo-classes.
    Has(Box<Compound>),
}

impl Compound {
    fn need(&self) -> Need {
        let mut need = Need::default();
        for pseudo in &self.pseudos {
            match pseudo {
                Pseudo::First => need.first = true,
                Pseudo::Last => need.last = true,
                Pseudo::Nth(_) => need.index = true,
                Pseudo::NthCol(_) => need.column = true,
                Pseudo::Has(_) => need.children = true,
            }
        }
        need
    }

    /// The fields the compound's own attribute tests name, without those
    /// of `:has`.
    fn own_fields(&self, out: &mut BTreeSet<EcoString>) {
        out.extend(self.attrs.iter().map(|a| a.field.clone()));
    }

    fn add_specificity(&self, out: &mut Specificity) {
        // Each count saturates, so that a generated selector with more
        // than 65,535 classes neither overflows nor wraps to a low
        // specificity (SPEC §5.1).
        let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        if self.label.is_some() {
            out.0 = out.0.saturating_add(1);
        }
        out.1 = out
            .1
            .saturating_add(count(self.classes.len().saturating_add(self.attrs.len())));
        if self.name.is_some() {
            out.2 = out.2.saturating_add(1);
        }
        for pseudo in &self.pseudos {
            match pseudo {
                // `:has` counts as its argument does, as in CSS.
                Pseudo::Has(inner) => inner.add_specificity(out),
                _ => out.1 = out.1.saturating_add(1),
            }
        }
    }

    /// Whether the compound matches a node.
    fn matches(&self, view: &NodeView) -> bool {
        self.matches_subject(&view.subject)
            && self.pseudos.iter().all(|pseudo| match pseudo {
                Pseudo::First => view.pos.first,
                Pseudo::Last => view.pos.last,
                Pseudo::Nth(n) => view.pos.index == *n,
                Pseudo::NthCol(n) => view.pos.column == *n,
                Pseudo::Has(inner) => view.children.iter().any(|c| inner.matches_subject(c)),
            })
    }

    /// Whether the compound matches a node, apart from its pseudo-classes.
    fn matches_subject(&self, subject: &Subject) -> bool {
        self.name.as_ref().is_none_or(|n| *n == subject.name)
            && self
                .label
                .as_ref()
                .is_none_or(|l| Some(l) == subject.label.as_ref())
            && self.classes.iter().all(|c| subject.classes.contains(c))
            && self.attrs.iter().all(|a| a.matches(subject))
    }
}

impl AttrTest {
    fn matches(&self, subject: &Subject) -> bool {
        let value = subject
            .fields
            .binary_search_by(|(f, _)| f.as_str().cmp(&self.field))
            .ok()
            .map(|i| &subject.fields[i].1);
        let Some(value) = value else {
            return matches!(self.test, Some((AttrOp::Neq, _)));
        };
        let Some((op, expected)) = &self.test else {
            return true;
        };
        let Attr::Value(value) = value else {
            return *op == AttrOp::Neq;
        };
        match op {
            AttrOp::Eq => ops::equal(value, expected),
            AttrOp::Neq => !ops::equal(value, expected),
            AttrOp::Lt => ops::compare(value, expected).is_ok_and(|o| o.is_lt()),
            AttrOp::Leq => ops::compare(value, expected).is_ok_and(|o| o.is_le()),
            AttrOp::Gt => ops::compare(value, expected).is_ok_and(|o| o.is_gt()),
            AttrOp::Geq => ops::compare(value, expected).is_ok_and(|o| o.is_ge()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(name: &str) -> NodeName {
        NodeName::Element(QualifiedName::unqualified(Name::new(name).unwrap()))
    }

    fn compound(name: &str) -> Compound {
        Compound {
            name: Some(el(name)),
            ..Compound::default()
        }
    }

    fn subject(name: &str) -> Subject {
        Subject {
            name: el(name),
            classes: EcoVec::new(),
            label: None,
            fields: EcoVec::new(),
        }
    }

    fn view(name: &str) -> NodeView {
        NodeView {
            subject: subject(name),
            pos: Position::default(),
            prev: EcoVec::new(),
            children: EcoVec::new(),
        }
    }

    fn selector(steps: &[(Option<Combinator>, Compound)]) -> Selector {
        Selector {
            steps: steps.iter().cloned().collect(),
        }
    }

    #[test]
    fn combinators() {
        use Combinator::*;
        let doc = view("doc");
        let section = view("section");
        let mut paragraph = view("paragraph");
        paragraph.prev.push(view("heading"));

        let descendant = selector(&[
            (None, compound("doc")),
            (Some(Descendant), compound("paragraph")),
        ]);
        assert!(descendant.matches(&[&paragraph, &section, &doc]));
        let child = selector(&[
            (None, compound("doc")),
            (Some(Child), compound("paragraph")),
        ]);
        assert!(!child.matches(&[&paragraph, &section, &doc]));
        let sibling = selector(&[
            (None, compound("heading")),
            (Some(NextSibling), compound("paragraph")),
        ]);
        assert!(sibling.matches(&[&paragraph, &section, &doc]));
        // The heading's parent is the paragraph's.
        let scoped = selector(&[
            (None, compound("section")),
            (Some(Child), compound("heading")),
            (Some(NextSibling), compound("paragraph")),
        ]);
        assert!(scoped.matches(&[&paragraph, &section, &doc]));
        assert_eq!(scoped.specificity(), Specificity(0, 0, 3));

        let mut needs = Needs::default();
        needs.add(&scoped);
        let of_paragraph = needs.of(&el("paragraph"));
        assert_eq!(of_paragraph.subject.prev, 1);
        assert_eq!(of_paragraph.context.prev, 0);
        assert!(of_paragraph.subject.has_sibling_dependencies());
        // Only paragraphs see their previous siblings.
        assert_eq!(needs.of(&el("heading")).subject.prev, 0);
        assert_eq!(needs.of(&el("list")), &NameNeeds::default());
        let mut needs = Needs::default();
        needs.add(&descendant);
        let of_paragraph = needs.of(&el("paragraph"));
        assert!(!of_paragraph.subject.has_sibling_dependencies());
        assert!(!of_paragraph.context.has_sibling_dependencies());
    }

    #[test]
    fn needs_by_name() {
        use Combinator::*;
        let mut level = compound("heading");
        level.attrs.push(AttrTest {
            field: "level".into(),
            test: Some((AttrOp::Eq, eval::Value::Int(1))),
        });
        let mut first = Compound::default();
        first.classes.push("x".into());
        first.pseudos.push(Pseudo::First);
        let mut has = compound("figure");
        let mut marked = compound("strong");
        marked.attrs.push(AttrTest {
            field: "marks".into(),
            test: None,
        });
        has.pseudos.push(Pseudo::Has(Box::new(marked)));

        let mut needs = Needs::default();
        needs.add(&selector(&[(None, level)]));
        needs.add(&selector(&[(None, has)]));
        fn fields(needs: &Needs, name: &str) -> Vec<String> {
            needs
                .of(&el(name))
                .fields
                .iter()
                .map(|f| f.to_string())
                .collect()
        }
        assert_eq!(fields(&needs, "heading"), ["level"]);
        assert_eq!(fields(&needs, "strong"), ["marks"]);
        assert!(fields(&needs, "paragraph").is_empty());
        assert!(needs.of(&el("figure")).subject.children);
        assert!(!needs.of(&el("heading")).subject.children);

        // A compound without a name tests every node, whether its name
        // was seen before or after it.
        needs.add(&selector(&[
            (None, first),
            (Some(Descendant), compound("paragraph")),
        ]));
        for name in ["heading", "strong", "paragraph", "section"] {
            assert!(needs.of(&el(name)).context.first, "{name}");
        }
        assert!(!needs.of(&el("paragraph")).subject.first);
        assert_eq!(fields(&needs, "heading"), ["level"]);
    }

    #[test]
    fn pseudo_classes_and_attributes() {
        let mut heading = view("heading");
        heading.pos = Position {
            first: true,
            last: false,
            index: 1,
            column: 0,
        };
        heading
            .subject
            .fields
            .push(("level".into(), Attr::Value(eval::Value::Int(2))));
        heading.children.push(subject("strong"));

        let mut c = compound("heading");
        c.pseudos.push(Pseudo::First);
        c.attrs.push(AttrTest {
            field: "level".into(),
            test: Some((AttrOp::Gt, eval::Value::Int(1))),
        });
        assert!(c.matches(&heading));
        assert_eq!(
            selector(&[(None, c.clone())]).specificity(),
            Specificity(0, 2, 1)
        );
        c.pseudos.push(Pseudo::Nth(2));
        assert!(!c.matches(&heading));

        let mut has = compound("heading");
        has.pseudos.push(Pseudo::Has(Box::new(compound("strong"))));
        assert!(has.matches(&heading));
        assert_eq!(selector(&[(None, has)]).specificity(), Specificity(0, 0, 2));

        let mut unset = compound("heading");
        unset.attrs.push(AttrTest {
            field: "title".into(),
            test: None,
        });
        assert!(!unset.matches(&heading));
    }

    #[test]
    fn descendant_chains_do_not_backtrack_exponentially() {
        use Combinator::*;
        // `figure list-item … (13×) paragraph` against a paragraph in 28
        // nested list items, with no figure among them: without the memo
        // of failed places this tries every way of choosing 13 ancestors
        // out of 28, some 37 million.
        let paragraph = view("paragraph");
        let item = view("list-item");
        let doc = view("doc");
        let mut chain = vec![&paragraph];
        chain.extend(std::iter::repeat_n(&item, 28));
        chain.push(&doc);
        let mut steps = vec![(None, compound("figure"))];
        steps.extend(std::iter::repeat_n(
            (Some(Descendant), compound("list-item")),
            13,
        ));
        steps.push((Some(Descendant), compound("paragraph")));
        assert!(!selector(&steps).matches(&chain));
        // The same chain with `doc` first matches.
        steps[0].1 = compound("doc");
        assert!(selector(&steps).matches(&chain));
        // Asking for one list item more than the chain has fails, after
        // trying each place once.
        let mut steps = vec![(None, compound("doc"))];
        steps.extend(std::iter::repeat_n(
            (Some(Descendant), compound("list-item")),
            29,
        ));
        steps.push((Some(Descendant), compound("paragraph")));
        assert!(!selector(&steps).matches(&chain));
        steps.remove(1);
        assert!(selector(&steps).matches(&chain));
    }

    #[test]
    fn specificity_saturates() {
        let mut classes = compound("strong");
        classes.classes = std::iter::repeat_n(EcoString::from("a"), 65_536).collect();
        // One compound: the count does not wrap to 0.
        assert_eq!(
            selector(&[(None, classes.clone())]).specificity(),
            Specificity(0, u16::MAX, 1)
        );
        assert!(
            selector(&[(None, classes.clone())]).specificity()
                > selector(&[(None, compound("strong"))]).specificity()
        );
        // Several compounds: the sum does not overflow.
        let mut half = compound("paragraph");
        half.classes = std::iter::repeat_n(EcoString::from("a"), 40_000).collect();
        let sum = selector(&[(None, half), (Some(Combinator::Descendant), classes)]).specificity();
        assert_eq!(sum, Specificity(0, u16::MAX, 2));
    }
}
