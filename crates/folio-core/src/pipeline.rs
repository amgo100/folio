//! The stages between Style and Layout (SPEC §2.1): Select, Sequence,
//! Resolve and Step styling, each a query on a [`Build`].
//!
//! The nine stages of §2.1 are these queries:
//!
//! 1. **Parse**: the host parses a source when it sets it, reparsing only
//!    the smallest block an edit touches ([`SourceFile`](crate::db::SourceFile));
//!    [`check_source`](crate::db::check_source) reports its syntax errors.
//! 2. **Evaluate**: [`evaluate`](crate::eval::evaluate) and the document
//!    model ([`document`](crate::model::document)). Neither reads the
//!    target, and code that does not read `variant` is evaluated once for
//!    every variant.
//! 3. **Style**: [`styles()`].
//! 4. **Select**: [`select()`].
//! 5. **Sequence**: [`sequence()`].
//! 6. **Resolve**: [`resolve()`].
//! 7. **Step styling**: [`step_style()`].
//! 8. **Layout**, for the fixed-layout targets (PDF, live): the text stage
//!    ([`text`](crate::text::text)), the paragraph builder
//!    ([`lines`](crate::layout::lines)) and the page builder
//!    ([`pages`](crate::layout::pages)), which read the step-styled tree.
//! 9. **Emit**: the `folio` crate's `emit` query, which hands a
//!    fixed-layout target's pages to its writer. The Office targets fork
//!    after step styling and will take the step-styled tree (M4, M5).
//!
//! Every stage from Style on runs per variant and target. Until the
//! milestones that fill them in, Select, Sequence, Resolve and Step styling
//! pass the styled tree on unchanged: Select until variants (M2), Sequence
//! until slides (M4), Resolve until counters and references (M1), and Step
//! styling until steps (M4). They are queries already, so the stages after
//! them read the tree from the stage before, and an edit that leaves the
//! styled tree equal stops at Style (early cutoff, SPEC §2.3).

use crate::db::{Build, Db};
use crate::style::{Styled, styles};

/// The styled tree after Select (SPEC §2.1, §6.3): the nodes whose computed
/// `display` is `none` removed, and answer space or placeholders
/// substituted. Until variants (M2) it is the styled tree.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Selected {
    /// The tree.
    pub styled: Styled,
}

/// The selected tree after Sequence (SPEC §2.1, §10.1): split into slides,
/// with the steps of each slide numbered. Until slides (M4) a document is
/// one flow of pages with no steps.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Sequenced {
    /// The tree.
    pub styled: Styled,
}

/// The sequenced tree after Resolve (SPEC §2.1, §4.5): counters,
/// cross-references, citations and queries resolved. Until counters and
/// references (M1) nothing is resolved.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Resolved {
    /// The tree.
    pub styled: Styled,
}

/// The resolved tree after Step styling (SPEC §2.1, §10.2): the collected
/// `:step` rules applied as a style delta per step. Until steps (M4) every
/// node keeps the style it was given. Layout reads this tree, and the
/// Office targets will map it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct StepStyled {
    /// The tree.
    pub styled: Styled,
}

/// Selects the content of a build (stage 4), or `None` when the document
/// could not be styled. Until variants (M2) it keeps every node.
#[salsa::tracked(returns(ref))]
pub fn select(db: &dyn Db, build: Build<'_>) -> Option<Selected> {
    let styled = styles(db, build).as_ref()?;
    Some(Selected {
        styled: styled.clone(),
    })
}

/// Sequences the selected content of a build into slides and steps (stage
/// 5), or `None` when the document could not be styled. Until slides (M4)
/// it passes the tree on.
#[salsa::tracked(returns(ref))]
pub fn sequence(db: &dyn Db, build: Build<'_>) -> Option<Sequenced> {
    let selected = select(db, build).as_ref()?;
    Some(Sequenced {
        styled: selected.styled.clone(),
    })
}

/// Resolves the counters, references, citations and queries of a build
/// (stage 6), or `None` when the document could not be styled. Until
/// counters and references (M1) it passes the tree on.
#[salsa::tracked(returns(ref))]
pub fn resolve(db: &dyn Db, build: Build<'_>) -> Option<Resolved> {
    let sequenced = sequence(db, build).as_ref()?;
    Some(Resolved {
        styled: sequenced.styled.clone(),
    })
}

/// Applies the `:step` rules of a build per step (stage 7), or `None` when
/// the document could not be styled. Until steps (M4) it passes the tree
/// on.
#[salsa::tracked(returns(ref))]
pub fn step_style(db: &dyn Db, build: Build<'_>) -> Option<StepStyled> {
    let resolved = resolve(db, build).as_ref()?;
    Some(StepStyled {
        styled: resolved.styled.clone(),
    })
}

/// The tree a build lays out or maps: the styled tree after every stage up
/// to Step styling, or `None` when the document could not be styled.
pub fn step_styled<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<&'db Styled> {
    step_style(db, build).as_ref().map(|s| &s.styled)
}
