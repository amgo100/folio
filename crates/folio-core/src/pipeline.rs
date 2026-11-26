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
//!    ([`pages`](crate::layout::pages())), which read the step-styled tree.
//! 9. **Emit**: the `folio` crate's `emit` query, which hands a
//!    fixed-layout target's pages to its writer. The Office targets fork
//!    after step styling and will take the step-styled tree (M4, M5).
//!
//! Every stage from Style on runs per variant and target. Until the
//! milestones that fill them in, Select, Sequence and Step styling pass the
//! styled tree on unchanged: Select until variants (M2), Sequence until
//! slides (M4), and Step styling until steps (M4). Resolve passes it on with
//! the counters, markers, labels and references it resolved
//! ([`crate::resolve`]). They are queries already, so the stages after
//! them read the tree from the stage before, and an edit that leaves the
//! styled tree equal stops at Style (early cutoff, SPEC §2.3).

use ecow::eco_format;
use folio_component::Target;

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::layout::pages;
use crate::resolve::page_refs::{self, EXTRA_PASSES};
use crate::resolve::{PageNumbers, Resolution, resolve_tree};
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
/// cross-references, citations and queries resolved. Counters, markers and
/// references are resolved ([`Resolution`]); citations and queries come
/// later. The tree is the sequenced one: what Resolve generates is keyed by
/// location, and the text stage sets it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Resolved {
    /// The tree.
    pub styled: Styled,
    /// What was resolved.
    pub resolution: Resolution,
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
/// (stage 6), or `None` when the document could not be styled: counters,
/// markers and references so far ([`resolve_tree`]).
///
/// Page-number references take the page numbers of the pass before
/// ([`page_numbers`]), which makes Resolve part of the cycle that runs
/// through layout ([`crate::resolve::page_refs`]). A fixed-layout build
/// reads its own pages; the Office targets read those of the PDF build of
/// their variant, whose page numbers are the cached results of their page
/// fields (SPEC §11.2). A build without page-number references reads no
/// pages, so it is in no cycle.
#[salsa::tracked(returns(ref), cycle_initial = resolve_initial)]
pub fn resolve(db: &dyn Db, build: Build<'_>) -> Option<Resolved> {
    let sequenced = sequence(db, build).as_ref()?;
    let mut resolution = resolve_tree(db, build, &sequenced.styled);
    if resolution.has_page_refs() {
        let numbers = page_numbers(db, paged(db, build));
        resolution.set_pages(numbers);
        if numbers.frozen {
            report_unsettled(db, &resolution);
        }
    }
    Some(Resolved {
        styled: sequenced.styled.clone(),
        resolution,
    })
}

/// What the cycle through layout starts from when salsa enters it at
/// Resolve: nothing resolved yet, as for a document that could not be
/// styled. The stages after it have nothing to lay out, so the pass that
/// follows knows no page, as when the cycle is entered anywhere else.
fn resolve_initial(
    _db: &dyn Db,
    _id: salsa::Id,
    _last: Option<&Option<Resolved>>,
    _build: Build<'_>,
) -> Option<Resolved> {
    None
}

/// The build whose pages the page-number references of a build show: the
/// build itself where it is laid out, else the PDF build of its variant.
fn paged<'db>(db: &'db dyn Db, build: Build<'db>) -> Build<'db> {
    match build.target(db) {
        Target::Pdf | Target::Live => build,
        Target::Docx | Target::Pptx => Build::new(
            db,
            build.project(db),
            Target::Pdf,
            build.variant(db).clone(),
        ),
    }
}

/// The page numbers that a build's page-number references show: one step
/// of the bounded fixpoint ([`page_refs`]). It reads the pages laid out
/// with the numbers of the pass before and gives the numbers they measure,
/// or those numbers again where the pages measure them, or once the passes
/// are used up. Before any pass, and where the document could not be
/// laid out, every page is unknown.
#[salsa::tracked(returns(ref), cycle_initial = page_numbers_initial)]
pub fn page_numbers(db: &dyn Db, build: Build<'_>) -> PageNumbers {
    let Some(pages) = pages(db, build).as_ref() else {
        return PageNumbers::default();
    };
    let used = &pages.page_refs;
    let targets: Vec<_> = used.pages.iter().map(|(target, _)| *target).collect();
    let measured: Vec<Option<u32>> = pages
        .pages_of(&targets)
        .into_iter()
        .map(|page| page.and_then(|i| u32::try_from(i + 1).ok()))
        .collect();
    page_refs::next(used, &measured)
}

/// What the cycle through layout starts from when salsa enters it at
/// [`page_numbers`]: no page known.
fn page_numbers_initial(
    _db: &dyn Db,
    _id: salsa::Id,
    _last: Option<&PageNumbers>,
    _build: Build<'_>,
) -> PageNumbers {
    PageNumbers::default()
}

/// Warns at each page-number reference whose target's page did not
/// settle, so its number was frozen.
fn report_unsettled(db: &dyn Db, resolution: &Resolution) {
    let numbers = &resolution.pages;
    for r in &resolution.references {
        let Some(target) = r.target.filter(|_| r.page.is_some()) else {
            continue;
        };
        if !numbers.unsettled.contains(&target.location) {
            continue;
        }
        diag::report(
            db,
            Diagnostic::warning(
                r.span,
                eco_format!(
                    "the page of `<{}>` did not settle after {EXTRA_PASSES} more layout passes, \
                     so this reference shows the last pass's page, which may be wrong",
                    r.label
                ),
            )
            .with_hint(
                "the reference's own number moves its target between pages; rewording the \
                 text before the target, or a break before it, settles it",
            ),
        );
    }
}

/// What the Resolve stage resolved in a build, or `None` when the document
/// could not be styled.
pub fn resolution<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<&'db Resolution> {
    resolve(db, build).as_ref().map(|r| &r.resolution)
}

/// Applies the `:step` rules of a build per step (stage 7), or `None` when
/// the document could not be styled. Until steps (M4) it passes the tree
/// on.
#[salsa::tracked(returns(ref), cycle_initial = step_style_initial)]
pub fn step_style(db: &dyn Db, build: Build<'_>) -> Option<StepStyled> {
    let resolved = resolve(db, build).as_ref()?;
    Some(StepStyled {
        styled: resolved.styled.clone(),
    })
}

/// What the cycle of page-number references starts from when salsa enters
/// it at Step styling: nothing to lay out ([`resolve`]).
fn step_style_initial(
    _db: &dyn Db,
    _id: salsa::Id,
    _last: Option<&Option<StepStyled>>,
    _build: Build<'_>,
) -> Option<StepStyled> {
    None
}

/// The tree a build lays out or maps: the styled tree after every stage up
/// to Step styling, or `None` when the document could not be styled.
pub fn step_styled<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<&'db Styled> {
    step_style(db, build).as_ref().map(|s| &s.styled)
}
