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

use std::collections::BTreeSet;

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

/// What a set of rules needs to see of nodes: their sibling dependencies.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Needs {
    /// For the node being styled.
    pub subject: Need,
    /// For its ancestors and previous siblings.
    pub context: Need,
    /// The fields attribute tests name.
    pub fields: BTreeSet<EcoString>,
}

impl Needs {
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
            let need = compound.need();
            if i == last {
                self.subject = self.subject.union(need);
                self.subject.prev = self.subject.prev.max(run);
            } else {
                self.context = self.context.union(need);
                if ends_segment {
                    self.context.prev = self.context.prev.max(run);
                }
            }
            compound.fields(&mut self.fields);
        }
    }

    /// Adds what another set needs.
    pub fn extend(&mut self, other: &Self) {
        self.subject = self.subject.union(other.subject);
        self.context = self.context.union(other.context);
        self.fields.extend(other.fields.iter().cloned());
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
/// pseudo-classes, then element names (SPEC §5.1).
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
    pub fn matches(&self, chain: &[&NodeView]) -> bool {
        !chain.is_empty()
            && self.match_at(
                self.steps.len() - 1,
                At {
                    level: 0,
                    offset: 0,
                },
                chain,
            )
    }

    fn match_at(&self, i: usize, at: At, chain: &[&NodeView]) -> bool {
        let (combinator, compound) = &self.steps[i];
        let Some(view) = at.view(chain) else {
            return false;
        };
        if !compound.matches(view) {
            return false;
        }
        if i == 0 {
            return true;
        }
        match combinator.expect("compounds after the first have a combinator") {
            Combinator::Child => at
                .parent(chain)
                .is_some_and(|p| self.match_at(i - 1, p, chain)),
            Combinator::Descendant => {
                let mut ancestor = at.parent(chain);
                while let Some(p) = ancestor {
                    if self.match_at(i - 1, p, chain) {
                        return true;
                    }
                    ancestor = p.parent(chain);
                }
                false
            }
            Combinator::NextSibling => at
                .prev(chain)
                .is_some_and(|p| self.match_at(i - 1, p, chain)),
        }
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

    fn fields(&self, out: &mut BTreeSet<EcoString>) {
        out.extend(self.attrs.iter().map(|a| a.field.clone()));
        for pseudo in &self.pseudos {
            if let Pseudo::Has(inner) = pseudo {
                inner.fields(out);
            }
        }
    }

    fn add_specificity(&self, out: &mut Specificity) {
        if self.label.is_some() {
            out.0 += 1;
        }
        out.1 += (self.classes.len() + self.attrs.len()) as u16;
        if self.name.is_some() {
            out.2 += 1;
        }
        for pseudo in &self.pseudos {
            match pseudo {
                // `:has` counts as its argument does, as in CSS.
                Pseudo::Has(inner) => inner.add_specificity(out),
                _ => out.1 += 1,
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
        assert_eq!(needs.subject.prev, 1);
        assert_eq!(needs.context.prev, 0);
        assert!(needs.subject.has_sibling_dependencies());
        let mut needs = Needs::default();
        needs.add(&descendant);
        assert!(!needs.subject.has_sibling_dependencies());
        assert!(!needs.context.has_sibling_dependencies());
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
}
