//! The Elements extension point: element schemas and locale strings (SPEC
//! §3.2, §4.3).

use ecow::{EcoString, EcoVec};

use crate::names::{Name, QualifiedName};
use crate::value::{FieldType, Value};

/// The schema of an element a component provides: its fields, kind,
/// content model and counter (SPEC §4.3).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ElementSchema {
    /// The element's name within its component's namespace: `question`.
    pub name: Name,
    /// How layout and the target writers treat the element.
    pub kind: ElementKind,
    /// The semantic fields, in declaration order.
    pub fields: EcoVec<FieldDecl>,
    /// The fields a call may give by position, in order: `path` for
    /// `#image("ball.svg")`. The name `body` stands for the body; a body
    /// that is not named here comes after the fields that are, so a call's
    /// trailing content block fills it. With no positional fields, the only
    /// positional argument a call takes is its body.
    pub positional: EcoVec<Name>,
    /// What the body may contain.
    pub body: Body,
    /// The element's counter, if it is numbered.
    pub counter: Option<CounterDecl>,
    /// The word that references to the element use, looked up in the locale
    /// strings: `@q:1` renders as *Question 1* with the key `question`.
    pub ref_key: Option<Name>,
}

impl ElementSchema {
    /// A schema with no fields, no body, no counter and no reference key.
    pub fn new(name: Name, kind: ElementKind) -> Self {
        Self {
            name,
            kind,
            fields: EcoVec::new(),
            positional: EcoVec::new(),
            body: Body::None,
            counter: None,
            ref_key: None,
        }
    }

    /// The field with a name.
    pub fn field(&self, name: &str) -> Option<&FieldDecl> {
        self.fields.iter().find(|f| f.name.as_str() == name)
    }

    /// The slot with a name, if the body is made of slots.
    pub fn slot(&self, name: &str) -> Option<&SlotDecl> {
        match &self.body {
            Body::Slots(slots) => slots.iter().find(|s| s.name.as_str() == name),
            _ => None,
        }
    }

    /// The order in which a call's positional arguments fill fields: the
    /// [`positional`](Self::positional) fields, with `body` standing for the
    /// body, and the body last if they do not place it.
    pub fn positional_order(&self) -> impl Iterator<Item = &str> {
        let body_placed = self.positional.iter().any(|n| n.as_str() == BODY);
        let body_last = !body_placed && self.body != Body::None;
        self.positional
            .iter()
            .map(Name::as_str)
            .chain(body_last.then_some(BODY))
    }
}

/// The name under which an element's body is stored and given by name:
/// `#figure(body: [..])` is `#figure[..]`. No field or slot may have it.
pub const BODY: &str = "body";

/// How an element behaves in layout (SPEC §4.3).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ElementKind {
    /// A block in the flow.
    Block,
    /// Inline, in a line of text.
    Inline,
    /// A placeable block (SPEC §7.5).
    Float,
    /// Anchored to a line and placeable in a margin (SPEC §7.6).
    Note,
    /// A slide (SPEC §10).
    Slide,
}

/// A field of an element.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FieldDecl {
    /// The field's name: `marks`.
    pub name: Name,
    /// The field's type.
    pub ty: FieldType,
    /// The default value. A field without one is required; a field that
    /// defaults to [`Value::None`] is optional.
    pub default: Option<Value>,
    /// Whether the field holds per-instance layout data, such as a drawing's
    /// scale, rather than formatting. Only such fields may hold lengths or
    /// colours (SPEC §5.5).
    pub layout_data: bool,
}

impl FieldDecl {
    /// A required field.
    pub fn required(name: Name, ty: FieldType) -> Self {
        Self {
            name,
            ty,
            default: None,
            layout_data: false,
        }
    }

    /// A field with a default value.
    pub fn with_default(name: Name, ty: FieldType, default: Value) -> Self {
        Self {
            default: Some(default),
            ..Self::required(name, ty)
        }
    }
}

/// The fields every element accepts in content besides its own (SPEC §4.2,
/// §5.4): `class`, `id`, `steps`, `handout`, `override`, and the layout
/// hints, which start at `placement`. A schema may not declare a field or
/// slot of the same name.
pub const UNIVERSAL_FIELDS: &[&str] = &[
    "class",
    "id",
    "steps",
    "handout",
    "override",
    "placement",
    "side",
    "width",
    "wrap-shape",
    "column-tracks",
    "break-before",
    "keep-with-next",
];

/// The layout hints among the [`UNIVERSAL_FIELDS`]: per-instance layout
/// decisions written in content (SPEC §5.4).
pub const LAYOUT_HINTS: &[&str] = &[
    "placement",
    "side",
    "width",
    "wrap-shape",
    "column-tracks",
    "break-before",
    "keep-with-next",
];

/// The content model of an element's body (SPEC §4.3).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Body {
    /// No body.
    None,
    /// A body of one kind of content.
    Content(ContentKind),
    /// Named slots, each filled with a sub-block: `#statement[..]`. An
    /// instance stores each slot's content as a field of the slot's name.
    Slots(EcoVec<SlotDecl>),
}

/// What a body or slot may contain.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ContentKind {
    /// Inline content: text, emphasis, inline math.
    Inline,
    /// Blocks: paragraphs, lists, figures, other block elements.
    Blocks,
}

/// A named slot of a body.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SlotDecl {
    /// The slot's name: `solution`.
    pub name: Name,
    /// What the slot may contain.
    pub content: ContentKind,
}

/// The counter of a numbered element (SPEC §4.3, §4.5).
///
/// The counter is named after the element and steps once per instance.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct CounterDecl {
    /// The element whose instances reset the counter, if any: `within
    /// heading(level: 1)`.
    pub within: Option<Within>,
    /// The elements the counter is scoped to: `scoped(question, part)`.
    /// Entering an instance of one opens a new level of the counter,
    /// counted from 1 among the instances inside it, and leaving it
    /// restores the value the counter had before. The element itself may be
    /// one, so that its instances nest: parts *(a)*, *(i)*, *(ii)*, *(b)*.
    /// Each is qualified if it belongs to another component.
    pub scope: EcoVec<QualifiedName>,
}

/// The element that resets a counter, optionally limited to instances whose
/// fields have given values.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Within {
    /// The element, qualified if it belongs to another component.
    pub element: QualifiedName,
    /// Field values an instance must have to reset the counter: `level: 1`.
    pub fields: EcoVec<(Name, Value)>,
}

/// A word that generated text uses, in one language (SPEC §4.5): the
/// English for `question` is *Question*.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LocaleString {
    /// The language, as a BCP 47 tag: `en`, `pl`.
    pub lang: EcoString,
    /// The key: an element's reference key, or another word the component
    /// generates, such as `turn-over`.
    pub key: Name,
    /// The text.
    pub text: EcoString,
}
