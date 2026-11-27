//! The style stage (SPEC §2.1, §5): matching stylesheet rules and
//! computing a style for every node of the document tree.
//!
//! - **Properties** ([`Properties`]): the built-in properties of SPEC §5.2
//!   and those of the components in use, each with a type, an initial
//!   value, inheritance, and whether it affects layout or only paint
//!   (SPEC §10.2), sorted into property groups.
//! - **Rules** ([`cascade()`]): a build reads the core's base stylesheet
//!   ([`BASE_STYLESHEET`]), its project stylesheets and what they
//!   `@import`, its variant's overlay stylesheets, and the overrides
//!   sheet. Tokens (`$accent`) are defined across them, and
//!   values are evaluated with the operators and standard library of
//!   scripts. `@variant` and `@target` blocks apply to the builds they
//!   name. Rules with `:step(..)` are collected for step styling, not
//!   applied.
//! - **Cascade**: origin first (defaults < base stylesheet < imported
//!   theme < project < variant overlay < target overlay < overrides sheet
//!   < local overrides in content), then specificity, then source order. There is no
//!   `!important`.
//! - **Matching** ([`styles()`]): a node is matched by a query keyed by
//!   what rules can see of it, so an insertion re-matches only the
//!   siblings whose position or neighbours a rule tests.
//! - **Computed styles** ([`ComputedStyle`]): text properties inherit, box
//!   properties do not; `em` lengths become absolute; values are stored as
//!   shared property groups with cheap hashes.
//!
//! This is the stage that becomes `folio-style` (SPEC §2.2) once its
//! interface settles.

mod computed;
mod props;
mod rules;
mod selector;
mod tree;
mod value;

pub use self::computed::{Computed, ComputedStyle, PropertyGroup};
pub use self::props::{GroupId, PropId, Properties, Property};
pub use self::rules::{
    BASE_STYLESHEET, Cascade, Decl, DeclValue, Origin, Priority, Rule, StepRule, Steps, cascade,
};
pub use self::selector::{
    Attr, AttrOp, AttrTest, Combinator, Compound, Need, Needs, NodeName, NodeView, Position,
    Pseudo, Selector, Specificity, Subject,
};
pub(crate) use self::tree::{Child, content_elements, styled_children};
pub use self::tree::{NOTE_PART, Styled, StyledNode, VECTOR_PART, styles};
pub use self::value::{Declared, coerce, compute};
