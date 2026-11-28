//! The stages between Style and Layout (SPEC §2.1): Select, Sequence,
//! Resolve and Step styling, each a query on a [`Build`].
//!
//! The nine stages of §2.1 are these queries:
//!
//! 1. **Parse**: the host parses a source when it sets it, reparsing only
//!    the smallest block an edit touches ([`SourceFile`](crate::db::SourceFile));
//!    [`check_source`](crate::db::check_source) reports its syntax errors.
//! 2. **Evaluate**: [`evaluate`](crate::eval::evaluate) and the document
//!    model ([`document`]). Neither reads the
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
//! Every stage from Style on runs per variant and target. Select leaves
//! out what the variant does not show ([`crate::variants::select`]). Until
//! the milestones that fill them in, Sequence and Step styling pass the
//! styled tree on unchanged: Sequence until slides (M4), and Step styling
//! until steps (M4). Resolve passes it on with
//! the counters, markers, labels and references it resolved
//! ([`crate::resolve`]). They are queries already, so the stages after
//! them read the tree from the stage before, and an edit that leaves the
//! styled tree equal stops at Style (early cutoff, SPEC §2.3).

use ecow::eco_format;

use crate::db::{Build, Db};
use crate::diag::{self, Diagnostic};
use crate::eval::Element;
use crate::layout::pages;
use crate::model::document;
use crate::resolve::page_refs::{self, EXTRA_PASSES};
use crate::resolve::{PageNumbers, Resolution, resolve_tree};
use crate::style::{Styled, styles};

/// The styled tree after Select (SPEC §2.1, §6.2, §6.3): the nodes the
/// variant does not show removed, and placeholders substituted
/// ([`crate::variants::select`]).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Selected {
    /// The tree.
    pub styled: Styled,
    /// The `doc` element of the document tree with the same content left
    /// out or replaced, or `None` when the variant shows all of the
    /// document tree ([`selected_root`]).
    pub root: Option<Element>,
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
/// could not be styled: what the variant does not show is left out of the
/// styled tree and the document tree, and placeholders replace content
/// (SPEC §6.2, §6.3; [`crate::variants::select`]).
#[salsa::tracked(returns(ref))]
pub fn select(db: &dyn Db, build: Build<'_>) -> Option<Selected> {
    let styled = styles(db, build).as_ref()?;
    let project = build.project(db);
    let variant = build.variant(db).clone();
    let doc = document(db, project, variant.clone()).as_ref()?;
    let selection = crate::variants::select::select(
        db,
        project.manifest(db),
        variant.as_ref(),
        &styled.properties,
        &doc.root,
        &styled.root,
    );
    Some(Selected {
        styled: Styled {
            root: selection.styled,
            ..styled.clone()
        },
        root: selection.root,
    })
}

/// The `doc` element of the document tree that a build shows: the
/// document tree after Select, which the stages after it read beside the
/// styled tree; or `None` when the document could not be styled.
pub fn selected_root<'db>(db: &'db dyn Db, build: Build<'db>) -> Option<&'db Element> {
    let build = build.base(db);
    let selected = select(db, build).as_ref()?;
    match &selected.root {
        Some(root) => Some(root),
        None => document(db, build.project(db), build.variant(db).clone())
            .as_ref()
            .map(|doc| &doc.root),
    }
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
/// Page-number references show the page numbers that [`settle`] found
/// for the build, or, for a layout pass of the fixpoint, those the pass
/// was made with. What does not depend on pages is resolved once for a
/// build and its passes ([`resolve_counters`]).
#[salsa::tracked(returns(ref))]
pub fn resolve(db: &dyn Db, build: Build<'_>) -> Option<Resolved> {
    let counted = resolve_counters(db, build.base(db)).as_ref()?;
    let mut resolution = counted.resolution.clone();
    if !resolution.page_targets().is_empty() {
        match build.pass(db) {
            Some(numbers) => resolution.set_pages(numbers),
            None => {
                let numbers = settle(db, build);
                resolution.set_pages(numbers);
                report_unsettled(db, &resolution);
            }
        }
    }
    Some(Resolved {
        styled: counted.styled.clone(),
        resolution,
    })
}

/// Resolves what does not depend on pages: [`resolve`] without the page
/// numbers of page-number references, which show [`UNRESOLVED`] here.
///
/// [`UNRESOLVED`]: crate::resolve::UNRESOLVED
#[salsa::tracked(returns(ref))]
pub fn resolve_counters(db: &dyn Db, build: Build<'_>) -> Option<Resolved> {
    let sequenced = sequence(db, build).as_ref()?;
    Some(Resolved {
        styled: sequenced.styled.clone(),
        resolution: resolve_tree(db, build, &sequenced.styled),
    })
}

/// The page numbers that a build's page-number references show: the
/// bounded fixpoint of [`page_refs`], run as layout passes of the build
/// (SPEC §2.3).
///
/// The first pass knows no page. Each pass is the build with its
/// references set to the numbers the pass before measured, a key of its
/// own ([`Build::pass`]), so every pass is memoised like the build, and
/// the shaping, line-break and page caches serve the paragraphs and pages
/// that a pass leaves as they were. The passes stop when one measures the
/// numbers it was set with, or freeze the numbers after the extra passes;
/// the build is then laid out with them, as the last pass was. The
/// passes always start from no page known, so the numbers do not depend on
/// which stage a host asks for first or on what an earlier revision
/// found (SPEC §2.4).
///
/// A DOCX or PPTX build measures its tree with Folio's layout too, which
/// gives the cached results of its page fields (SPEC §11.2); the text
/// stage reports no layout warnings for it.
#[salsa::tracked(returns(ref))]
pub fn settle(db: &dyn Db, build: Build<'_>) -> PageNumbers {
    let build = build.base(db);
    let project = build.project(db);
    let variant = build.variant(db).clone();
    let Some(counted) = resolve_counters(db, build).as_ref() else {
        return PageNumbers::default();
    };
    let mut used = PageNumbers::default().for_targets(counted.resolution.page_targets());
    loop {
        let pass = Build::with_pass(
            db,
            project,
            build.target(db),
            variant.clone(),
            Some(used.clone()),
        );
        let Some(pages) = pages(db, pass).as_ref() else {
            return used;
        };
        let targets: Vec<_> = used.pages.iter().map(|(target, _)| *target).collect();
        let measured: Vec<Option<u32>> = pages
            .pages_of(&targets)
            .into_iter()
            .map(|page| page.and_then(|i| u32::try_from(i + 1).ok()))
            .collect();
        let next = page_refs::next(&used, &measured);
        if next == used || next.frozen {
            return next;
        }
        used = next;
    }
}

/// Warns at each page-number reference whose target's page did not
/// settle, so its number was frozen, and at each whose target the build's
/// layout puts on no page, so it shows [`UNRESOLVED`]; and at each
/// footnote numbered per page whose page did not settle.
///
/// [`UNRESOLVED`]: crate::resolve::UNRESOLVED
fn report_unsettled(db: &dyn Db, resolution: &Resolution) {
    let numbers = &resolution.pages;
    for footnote in &resolution.footnotes {
        if resolution.footnotes_per_page && numbers.unsettled.contains(&footnote.location) {
            diag::report(
                db,
                Diagnostic::warning(
                    footnote.span,
                    eco_format!(
                        "the page of this footnote did not settle after {EXTRA_PASSES} more layout \
                         passes, so its number, counted on its page, may be wrong"
                    ),
                )
                .with_hint(
                    "the width of footnote numbers moves this footnote between pages; rewording \
                     the text before it settles it",
                ),
            );
        }
    }
    for r in &resolution.references {
        let Some(target) = r.target.filter(|_| r.page.is_some()) else {
            continue;
        };
        if numbers.unsettled.contains(&target.location) {
            diag::report(
                db,
                Diagnostic::warning(
                    r.span,
                    eco_format!(
                        "the page of `<{}>` did not settle after {EXTRA_PASSES} more layout \
                         passes, so this reference shows the last pass's page, which may be wrong",
                        r.label
                    ),
                )
                .with_hint(
                    "the reference's own number moves its target between pages; rewording \
                     the text before the target, or a break before it, settles it",
                ),
            );
        } else if numbers.page(target.location).is_none() {
            diag::report(
                db,
                Diagnostic::warning(
                    r.span,
                    eco_format!(
                        "`<{}>` is on no page, so the reference has no page number to show",
                        r.label
                    ),
                )
                .with_hint(
                    "an inline element has no box of its own to find; refer to a block \
                     element, such as a heading, a figure or a display equation",
                ),
            );
        }
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
