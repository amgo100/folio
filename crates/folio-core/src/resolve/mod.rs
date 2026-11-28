//! The Resolve stage (SPEC §2.1, §4.5): counters, the markers of numbered
//! elements and list items, labels and cross-references.
//!
//! [`resolve_tree`] walks the document tree beside the sequenced styled
//! tree, pairing the two by location ([`crate::locate`]), so that only the
//! elements a build shows are counted, and only their labels can be
//! referred to. Its result, [`Resolution`], keys everything by location:
//!
//! - **Counters**: an element whose schema declares a counter steps it
//!   once per instance. The counter is named after the element
//!   (`figure`, `exam.question`), except display equations', which is
//!   `equation`. Headings count by level: a heading of level 2 steps the
//!   second value and clears those below, so its values are `[2, 1]` in
//!   *Section 2.1*. A counter declared `within` an element restarts at each
//!   of that element's instances and takes their values before its own
//!   (`[2, 1]` for the first figure after the second chapter), and
//!   `counter-reset` restarts the counters it names at each element it
//!   applies to. A counter `scoped` to elements opens a new level inside
//!   each of their instances and gets its value back on leaving it, so
//!   `exam.part` scoped to `exam.question` and `exam.part` numbers parts
//!   *(a)*, *(i)*, *(ii)*, *(b)*. The items of an `enum` are counted within
//!   it in the same way, from 1 or from the `number` an item gives.
//!   `counter-reset` names a component's counter qualified or not; an
//!   unqualified name resolves through the lockfile's bindings.
//! - **Numbers**: the values formatted by the element's `numbering` pattern
//!   ([`Pattern`]), or none where it is `none`. A display equation is
//!   numbered once, or, when lines of it have labels, on each labelled
//!   line.
//! - **Markers**: the text set before an element's content: its number
//!   (`2.1 `), a word and its number before a figure's caption
//!   (`Figure 3: `), or the string of `marker`. The text stage sets it in
//!   the element's first paragraph, or in its caption.
//! - **References**: each `@label` as the text it shows, by its target's
//!   `ref-format`: the target's reference word in the reference's language
//!   ([`crate::locale`]) and its number in the pattern it was numbered
//!   with (an item's own, or its enum's), trimmed of the pattern's text
//!   before and after (`Figure 3`, `Section 2.1`, `b` for an item `(b)`).
//!   An element without `numbering`, or whose pattern has no counting
//!   symbol, still counts, and a reference to it shows the values as `1.1`
//!   would. A reference to a label that no shown element has, or to
//!   an element without a counter, is a warning, and shows [`UNRESOLVED`].
//!
//! - **Footnotes** (SPEC §7.8): a footnote counts with the counter
//!   `footnote`, and its number is its marker, at its anchor and before its
//!   note. `counter-reset: footnote` restarts it at the elements it applies
//!   to, so `heading[level=1] { counter-reset: footnote; }` numbers the
//!   footnotes of each section from 1; on `page`, it restarts on every
//!   page, which only layout knows: the page-number fixpoint measures each
//!   footnote's page like a page-number reference's target
//!   ([`Resolution::set_pages`]), so per-page numbers are in the layout
//!   part of the flow state (SPEC §2.3).
//! - **Page-number references**: a reference with the page form shows the
//!   page its target is on (`page 4`), which only layout knows. Resolve
//!   gives each the page numbers of the pass before
//!   ([`Resolution::set_pages`]), in the bounded fixpoint of
//!   [`page_refs`].
//!
//! Counters are resolved per build, on the variant's own tree, except the
//! shared counters of a numbering group (SPEC §6.1), whose values come from
//! the group's common tree ([`crate::variants::common`]), counted by the
//! same walk without reporting anything ([`count_common`]).

mod numbering;
pub mod page_refs;
pub mod query;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{
    CounterDecl, ElementKind, Location, Name, QualifiedName, Registered, Value as StyleValue,
    Within,
};
use folio_syntax::Span;
use folio_text::Lang;

pub use self::numbering::Pattern;
pub use self::page_refs::PageNumbers;
pub use self::query::{Entry, Query, QueryResult};

use crate::component::Reason;
use crate::db::{Build, Db, component_host};
use crate::diag::{self, Diagnostic};
use crate::eval::{Element, Value};
use crate::locale;
use crate::locate::StyledChildren;
use crate::model::{document, schema};
use crate::style::{
    Child, ComputedStyle, PropId, Properties, Styled, StyledNode, content_elements, styled_children,
};

/// The no-break space between a word and its number, and after a marker,
/// so that neither is left alone at the end of a line.
pub const NBSP: char = '\u{a0}';

/// What a reference or a marker shows where it cannot resolve: the mark
/// that tells a reader something is missing, as LaTeX shows it.
pub const UNRESOLVED: &str = "??";

/// What can be numbered and referred to: an element instance, or a line of
/// a display equation.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Target {
    /// The element instance.
    pub location: Location,
    /// The line of an equation, counted from 0; `None` for the element.
    pub line: Option<u32>,
}

impl Target {
    /// The element instance itself.
    pub fn element(location: Location) -> Self {
        Self {
            location,
            line: None,
        }
    }
}

/// A numbered element instance or equation line.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Number {
    /// What is numbered.
    pub target: Target,
    /// The counter that numbers it: `heading`, `figure`, `equation`,
    /// `enum-item`, `exam.question`.
    pub counter: EcoString,
    /// The counter's values for it: one for a flat counter, one per level
    /// for a heading, and the values of the instance it is counted within
    /// before its own.
    pub values: EcoVec<u64>,
    /// The values formatted by the element's `numbering` pattern, or `None`
    /// where `numbering` is `none`.
    pub number: Option<EcoString>,
}

/// Text that the text stage sets before an element's content (SPEC §4.5).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Marker {
    /// The element instance.
    pub location: Location,
    /// The text, with the space after it.
    pub text: EcoString,
    /// The part it goes before (`caption`), or `None` for the element's
    /// first paragraph.
    pub part: Option<Name>,
}

/// A label and what it names.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Labelled {
    /// The label, without angle brackets.
    pub label: EcoString,
    /// What it names.
    pub target: Target,
    /// The element, or the equation whose line it names.
    pub element: QualifiedName,
    /// Where the label's element (or line) was made.
    pub span: Span,
}

/// A cross-reference, resolved.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Reference {
    /// The `ref` element instance.
    pub location: Location,
    /// The label it names.
    pub label: EcoString,
    /// What the label names, or `None` when no element the build shows has
    /// it.
    pub target: Option<Target>,
    /// The text it shows: `Figure 3`, `page 4`, or [`UNRESOLVED`].
    pub text: EcoString,
    /// For a page-number reference, the word before its page number: the
    /// reference word for a page in the reference's language, or its
    /// supplement; empty for the number alone. `None` for a reference to
    /// a number.
    pub page: Option<EcoString>,
    /// Where the reference was made.
    pub span: Span,
}

/// What the Resolve stage found in a build: numbers, markers, labels and
/// references, each keyed by location and listed in document order.
///
/// Later stages read it beside the step-styled tree: the text stage sets
/// markers and reference text, and page-number references (#45),
/// footnotes (#46), figures (#47) and PDF links (#48) build on it.
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    /// The numbered instances and equation lines.
    pub numbers: EcoVec<Number>,
    /// The markers.
    pub markers: EcoVec<Marker>,
    /// The labels of the elements the build shows, each once: the first
    /// element that has it.
    pub labels: EcoVec<Labelled>,
    /// The references.
    pub references: EcoVec<Reference>,
    /// The page numbers the page-number references show
    /// ([`Resolution::set_pages`]): of the pass before, for the targets of
    /// the references in the order they are first referred to, then, when
    /// footnotes are numbered per page, the footnotes. Empty when the build
    /// has neither.
    pub pages: PageNumbers,
    /// The footnotes, in document order.
    pub footnotes: EcoVec<Footnote>,
    /// Whether footnotes are numbered from 1 on every page: `page {
    /// counter-reset: footnote; }`.
    pub footnotes_per_page: bool,
    /// The queries: totals, outlines and figure lists (SPEC §4.5), in
    /// document order.
    pub queries: EcoVec<Query>,
    index: Index,
}

/// A footnote, as its numbers per page are set ([`Resolution::set_pages`]).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Footnote {
    /// The `footnote` element instance.
    pub location: Location,
    /// Its value of the counter `footnote` in document order, which
    /// `counter-reset` on elements restarts.
    pub value: u64,
    /// The pattern of its `numbering`, if it has one.
    pub pattern: Option<Pattern>,
    /// Whether its marker is its number, rather than a string or none.
    pub numbered_marker: bool,
    /// Where it was made.
    pub span: Span,
}

/// Lookups into a [`Resolution`] by location, made once.
#[derive(Debug, Clone, Default)]
struct Index {
    numbers: HashMap<Target, usize>,
    /// The first number of each location, for an equation numbered by line.
    first: HashMap<Location, usize>,
    markers: HashMap<Location, usize>,
    labels: HashMap<EcoString, usize>,
    references: HashMap<Location, usize>,
    queries: HashMap<Location, usize>,
}

impl PartialEq for Resolution {
    fn eq(&self, other: &Self) -> bool {
        // The index is made from the lists.
        self.numbers == other.numbers
            && self.markers == other.markers
            && self.labels == other.labels
            && self.references == other.references
            && self.pages == other.pages
            && self.footnotes == other.footnotes
            && self.footnotes_per_page == other.footnotes_per_page
            && self.queries == other.queries
    }
}

impl Eq for Resolution {}

impl Hash for Resolution {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.numbers.hash(state);
        self.markers.hash(state);
        self.labels.hash(state);
        self.references.hash(state);
        self.pages.hash(state);
        self.footnotes.hash(state);
        self.footnotes_per_page.hash(state);
        self.queries.hash(state);
    }
}

impl Resolution {
    /// The number of an element instance or equation line. An equation
    /// numbered by line answers for itself with its first numbered line.
    pub fn number(&self, target: Target) -> Option<&Number> {
        let i = self.index.numbers.get(&target).copied().or_else(|| {
            if target.line.is_some() {
                return None;
            }
            self.index.first.get(&target.location).copied()
        })?;
        Some(&self.numbers[i])
    }

    /// The marker of an element instance.
    pub fn marker(&self, location: Location) -> Option<&Marker> {
        Some(&self.markers[*self.index.markers.get(&location)?])
    }

    /// What a label names.
    pub fn label(&self, label: &str) -> Option<&Labelled> {
        Some(&self.labels[*self.index.labels.get(label)?])
    }

    /// The result of a query element instance: a total, an outline or a
    /// figure list.
    pub fn query(&self, location: Location) -> Option<&Query> {
        Some(&self.queries[*self.index.queries.get(&location)?])
    }

    /// The reference that a `ref` element instance is.
    pub fn reference(&self, location: Location) -> Option<&Reference> {
        Some(&self.references[*self.index.references.get(&location)?])
    }

    /// What layout must find the pages of: the targets of the page-number
    /// references, each once, in the order they are first referred to,
    /// then, when footnotes are numbered per page, the footnotes, then the
    /// instances that outlines and figure lists list.
    pub fn page_targets(&self) -> Vec<Location> {
        let mut seen = HashSet::new();
        let footnotes = self
            .footnotes
            .iter()
            .filter(|_| self.footnotes_per_page)
            .map(|f| f.location);
        let entries = self.queries.iter().flat_map(|q| match &q.result {
            QueryResult::List(entries) => entries.iter().map(|e| e.target).collect(),
            QueryResult::Total(_) => Vec::new(),
        });
        self.references
            .iter()
            .filter(|r| r.page.is_some())
            .filter_map(|r| r.target)
            .map(|t| t.location)
            .chain(footnotes)
            .chain(entries)
            .filter(|l| seen.insert(*l))
            .collect()
    }

    /// Sets the page-number references with the page numbers of a pass
    /// ([`page_refs`]): each shows its word and its target's page, or
    /// [`UNRESOLVED`] where the pass did not find its target on a page.
    /// When footnotes are numbered per page, each is numbered among the
    /// footnotes of its page, as the pass found them; before any pass
    /// knows a page, in document order.
    pub fn set_pages(&mut self, numbers: &PageNumbers) {
        let numbers = numbers.for_targets(self.page_targets());
        if self.footnotes_per_page {
            self.number_footnotes(&numbers);
        }
        for r in self.references.make_mut() {
            let (Some(word), Some(target)) = (&r.page, r.target) else {
                continue;
            };
            let page = match numbers.page(target.location) {
                Some(page) => eco_format!("{page}"),
                None => UNRESOLVED.into(),
            };
            r.text = if word.is_empty() {
                page
            } else {
                eco_format!("{word}{NBSP}{page}")
            };
        }
        for q in self.queries.make_mut() {
            let QueryResult::List(entries) = &mut q.result else {
                continue;
            };
            for entry in entries.make_mut() {
                entry.page = match numbers.page(entry.target) {
                    Some(page) => eco_format!("{page}"),
                    None => UNRESOLVED.into(),
                };
            }
        }
        self.pages = numbers;
    }

    /// Numbers the footnotes among those of their pages: a footnote on
    /// another page than the one before it, or whose counter an element
    /// restarted, is numbered 1. Footnotes on no known page count on from
    /// the one before, so before any page is known they keep their
    /// numbers in document order.
    fn number_footnotes(&mut self, numbers: &PageNumbers) {
        // The references to each footnote, found once.
        let mut refs: HashMap<Location, Vec<usize>> = HashMap::new();
        for (i, r) in self.references.iter().enumerate() {
            if r.page.is_none()
                && let Some(target) = r.target
            {
                refs.entry(target.location).or_default().push(i);
            }
        }
        let mut previous: Option<(Option<u32>, u64)> = None;
        for i in 0..self.footnotes.len() {
            let footnote = &self.footnotes[i];
            let page = numbers.page(footnote.location);
            let value = match previous {
                Some((before, n)) if before == page && footnote.value != 1 => n + 1,
                _ => 1,
            };
            previous = Some((page, value));
            let target = Target::element(footnote.location);
            let number = footnote.pattern.as_ref().map(|p| p.format(&[value]));
            let numbered = footnote.numbered_marker;
            if let Some(&n) = self.index.numbers.get(&target) {
                let entry = &mut self.numbers.make_mut()[n];
                // A reference to the footnote shows its number as the
                // footnote does: the number, after its supplement if any.
                let shown = |values: &[u64]| shown(footnote.pattern.as_ref(), values);
                let (old, new) = (shown(&entry.values), shown(&[value]));
                entry.values = EcoVec::from([value]);
                entry.number = number.clone();
                // A footnote has no reference word, so its references
                // end with the number.
                if old != new
                    && let Some(indices) = refs.get(&footnote.location)
                {
                    let references = self.references.make_mut();
                    for &r in indices {
                        let r = &mut references[r];
                        if let Some(prefix) = r.text.strip_suffix(old.as_str()) {
                            r.text = eco_format!("{prefix}{new}");
                        }
                    }
                }
            }
            if numbered
                && let Some(number) = number
                && let Some(&m) = self.index.markers.get(&footnote.location)
            {
                self.markers.make_mut()[m].text = eco_format!("{number}{NBSP}");
            }
        }
    }

    fn reindex(&mut self) {
        let mut index = Index::default();
        for (i, n) in self.numbers.iter().enumerate() {
            index.numbers.entry(n.target).or_insert(i);
            index.first.entry(n.target.location).or_insert(i);
        }
        for (i, m) in self.markers.iter().enumerate() {
            index.markers.entry(m.location).or_insert(i);
        }
        for (i, l) in self.labels.iter().enumerate() {
            index.labels.entry(l.label.clone()).or_insert(i);
        }
        for (i, r) in self.references.iter().enumerate() {
            index.references.entry(r.location).or_insert(i);
        }
        for (i, q) in self.queries.iter().enumerate() {
            index.queries.entry(q.location).or_insert(i);
        }
        self.index = index;
    }
}

/// Resolves the counters, markers, labels and references of a build's
/// styled tree, which a stage before may have dropped nodes from: an
/// element without a node is not counted, and its label names nothing.
/// Reports references that do not resolve.
pub fn resolve_tree(db: &dyn Db, build: Build<'_>, styled: &Styled) -> Resolution {
    let Some(root) = crate::pipeline::selected_root(db, build) else {
        return Resolution::default();
    };
    let mut walk = Walk::new(db, build, &styled.properties);
    walk.shared = shared(db, build, &mut walk);
    walk.out.footnotes_per_page = page_resets(db, build, styled);
    walk.element(root, &styled.root);
    // Numbers and labels are looked up as references resolve.
    walk.out.reindex();
    walk.references();
    // Titles show the references' text.
    walk.out.reindex();
    walk.queries(root, &styled.root);
    let mut out = walk.out;
    out.reindex();
    out
}

/// Counts the common tree of a numbering group (SPEC §6.1;
/// [`crate::variants::common`]): every counter's values for each counted
/// instance. It reports nothing: what the tree holds is reported by the
/// builds of the variants that show it.
pub fn count_common(
    db: &dyn Db,
    build: Build<'_>,
    root: &Element,
    styled: &StyledNode,
) -> Vec<Number> {
    let Some(selected) = crate::pipeline::select(db, build).as_ref() else {
        return Vec::new();
    };
    let mut walk = Walk::new(db, build, &selected.styled.properties);
    walk.quiet = true;
    walk.element(root, styled);
    walk.out.numbers.into_iter().collect()
}

/// The counters a build shares with the other variants of its numbering
/// group, with the values the group's common tree gives them; `None` when
/// it shares none. Reports a shared counter that names no counter, at its
/// name in the manifest.
fn shared(db: &dyn Db, build: Build<'_>, walk: &mut Walk<'_>) -> Option<Shared> {
    let group_build = crate::variants::common::group_build(db, build)?;
    let project = build.project(db);
    let variant = build.variant(db).as_ref()?;
    let group = project.manifest(db).numbering_group(variant.as_str())?;
    if group.shared_counters.is_empty() {
        return None;
    }
    let mut counters = HashSet::new();
    for name in &group.shared_counters {
        let written: EcoString = name.v.to_string().into();
        match walk.counter_key(&written) {
            Ok(key) => {
                counters.insert(key);
            }
            Err(_) => diag::report(
                db,
                Diagnostic::warning(
                    name.span,
                    eco_format!(
                        "there is no counter `{written}`, so numbering group `{}` cannot share it",
                        group.name.v
                    ),
                )
                .with_hint(
                    "counters are named after the elements they number, such as `figure` or a \
                     component's `question`, and display equations are counted by `equation`",
                ),
            ),
        }
    }
    if counters.is_empty() {
        return None;
    }
    crate::variants::common::check_target_overlays(db, build, &group.name.v);
    // A variant of the group that cannot be styled reports so in its own
    // build; meanwhile each variant counts its own content.
    let common = crate::variants::common::common_numbers(db, group_build).as_ref()?;
    Some(Shared {
        counters,
        values: common
            .numbers
            .iter()
            .map(|(target, counter, values)| (*target, (counter.clone(), values.clone())))
            .collect(),
    })
}

/// The counters a build shares with its numbering group, and the values
/// the group's common tree gives each counted instance.
struct Shared {
    counters: HashSet<EcoString>,
    values: HashMap<Target, (EcoString, EcoVec<u64>)>,
}

/// Whether `counter-reset` on `page` restarts the footnotes on every page.
/// Reports the other counters it names, which do not restart per page.
fn page_resets(db: &dyn Db, build: Build<'_>, styled: &Styled) -> bool {
    let props = &styled.properties;
    let names: Vec<EcoString> = match styled.page.value(props, props.builtin("counter-reset")) {
        None | Some(StyleValue::None) => return false,
        Some(StyleValue::Array(items)) => items.iter().filter_map(counter_name).collect(),
        Some(value) => counter_name(value).into_iter().collect(),
    };
    let mut footnotes = false;
    for name in names {
        if name == "footnote" {
            footnotes = true;
            continue;
        }
        diag::report(
            db,
            Diagnostic::warning(
                page_reset_span(db, build),
                eco_format!("only footnotes are numbered per page, so `{name}` does not restart on each page"),
            )
            .with_hint(
                "restart other counters at the elements that start their sections, such as \
                 `heading[level=1] { counter-reset: figure; }`",
            ),
        );
    }
    footnotes
}

/// Where the `counter-reset` that applies to `page` is declared: the last
/// rule that matches the page and declares it, as the style stage matches
/// them.
fn page_reset_span(db: &dyn Db, build: Build<'_>) -> Span {
    let cascade = crate::style::cascade(db, build.base(db));
    let reset = cascade.properties.builtin("counter-reset");
    let page = crate::style::NodeView {
        subject: crate::style::Subject {
            name: crate::style::NodeName::Page,
            classes: EcoVec::new(),
            label: None,
            fields: EcoVec::new(),
        },
        pos: crate::style::Position::default(),
        prev: EcoVec::new(),
        children: EcoVec::new(),
    };
    cascade
        .rules
        .iter()
        .filter(|rule| rule.selector.matches(&[&page]))
        .flat_map(|rule| &rule.declarations)
        .rfind(|decl| decl.prop == reset)
        .map_or_else(Span::detached, |decl| decl.span)
}

/// The property ids the walk reads.
struct Ids {
    numbering: PropId,
    counter_reset: PropId,
    marker: PropId,
    ref_format: PropId,
    lang: PropId,
    decimal_marker: PropId,
}

impl Ids {
    fn new(props: &Properties) -> Self {
        Self {
            numbering: props.builtin("numbering"),
            counter_reset: props.builtin("counter-reset"),
            marker: props.builtin("marker"),
            ref_format: props.builtin("ref-format"),
            lang: props.builtin("lang"),
            decimal_marker: props.builtin("decimal-marker"),
        }
    }
}

/// What the walk knows of an element's schema.
#[derive(Clone)]
struct Counted {
    /// Its counter, if it has one.
    counter: Option<CounterDecl>,
    /// Its reference key, if it has one.
    ref_key: Option<Name>,
}

/// A counter's state.
#[derive(Debug, Clone, Default)]
struct Counter {
    /// The values of the instance it was last restarted within.
    prefix: EcoVec<u64>,
    /// Its own values: one, or one per level for headings.
    values: Vec<u64>,
    /// How many times it was restarted `within` an element, so that a
    /// scope that closes after a restart inside it keeps the restart.
    restarts: u64,
}

/// A counter declared `within` an element, and its name.
struct WithinDecl {
    counter: EcoString,
    within: Within,
}

/// A counter scoped to an element, and its name.
struct ScopeDecl {
    counter: EcoString,
    scope: QualifiedName,
}

/// A reference waiting for every label of the tree.
struct PendingRef {
    location: Location,
    label: EcoString,
    span: Span,
    supplement: Option<EcoString>,
    lang: Lang,
    /// Whether it refers to its target's page.
    page: bool,
}

struct Walk<'a> {
    db: &'a dyn Db,
    build: Build<'a>,
    props: &'a Properties,
    ids: Ids,
    default_lang: Lang,
    /// The counters declared `within` an element.
    within: Vec<WithinDecl>,
    /// The counters scoped to elements.
    scopes: Vec<ScopeDecl>,
    /// The counters, by name. Never iterated.
    counters: HashMap<EcoString, Counter>,
    /// The item counters of the enums being walked, innermost last, with
    /// each enum's `numbering`, which its items take where they have none.
    lists: Vec<(u64, Option<Pattern>)>,
    /// The labels already taken.
    seen: HashSet<EcoString>,
    /// What the walk knows of each element's schema.
    schemas: HashMap<QualifiedName, Counted>,
    /// The counter each name in `counter-reset` resolved to, or why not.
    counter_keys: HashMap<EcoString, Result<EcoString, EcoString>>,
    /// What has been reported, so that each is reported once per build.
    reported: HashSet<EcoString>,
    out: Resolution,
    refs: Vec<PendingRef>,
    /// The style of each target, for its `ref-format`.
    styles: HashMap<Location, ComputedStyle>,
    /// The pattern each labelled element instance was numbered with, where
    /// it has one: its own `numbering`, or, for an item, its enum's. A
    /// reference shows the number as the target does.
    patterns: HashMap<Location, Pattern>,
    /// The counters shared with the build's numbering group, whose values
    /// come from the group's common tree.
    shared: Option<Shared>,
    /// Whether the walk reports nothing: the walk of a common tree.
    quiet: bool,
    /// The queries waiting for the whole tree.
    pending_queries: Vec<query::Pending>,
    /// The elements of components' aggregates (D-72-1), by qualified name.
    aggregates: HashSet<QualifiedName>,
}

/// The elements of the components' aggregates (D-72-1), by qualified
/// name.
fn aggregate_elements(db: &dyn Db, build: Build<'_>) -> HashSet<QualifiedName> {
    let Some(host) = component_host(db, build.project(db)).as_ref() else {
        return HashSet::new();
    };
    host.components()
        .flat_map(|c| {
            let namespace = c.manifest.namespace();
            c.declarations
                .aggregates
                .iter()
                .map(move |a| QualifiedName::qualified(namespace.clone(), a.name.clone()))
        })
        .collect()
}

/// The counters that components' elements declare `within` another
/// element. No built-in element's counter is.
fn within_decls(db: &dyn Db, build: Build<'_>) -> Vec<WithinDecl> {
    let Some(host) = component_host(db, build.project(db)).as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for component in host.components() {
        let namespace = component.manifest.namespace();
        for element in &component.declarations.elements {
            if let Some(CounterDecl {
                within: Some(within),
                ..
            }) = &element.counter
            {
                let name = QualifiedName::qualified(namespace.clone(), element.name.clone());
                out.push(WithinDecl {
                    counter: name.to_string().into(),
                    within: Within::new(
                        own(component, &within.element),
                        within.fields.iter().cloned(),
                    ),
                });
            }
        }
    }
    out
}

/// An element name in a component's counter declaration as the document
/// tree names the element: an unqualified name of one of the component's
/// own elements qualified with its namespace, and any other name, a
/// built-in element's or another component's qualified one, as written.
fn own(component: &Registered, name: &QualifiedName) -> QualifiedName {
    if name.namespace.is_none() && component.element(name.name.as_str()).is_some() {
        return QualifiedName::qualified(component.manifest.namespace(), name.name.clone());
    }
    name.clone()
}

/// The counters that components' elements declare scoped to elements. No
/// built-in element's counter is: an `enum`'s items are counted within it
/// by the walk itself.
fn scope_decls(db: &dyn Db, build: Build<'_>) -> Vec<ScopeDecl> {
    let Some(host) = component_host(db, build.project(db)).as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for component in host.components() {
        let namespace = component.manifest.namespace();
        for element in &component.declarations.elements {
            let Some(counter) = &element.counter else {
                continue;
            };
            let name = QualifiedName::qualified(namespace.clone(), element.name.clone());
            for scope in &counter.scope {
                out.push(ScopeDecl {
                    counter: name.to_string().into(),
                    scope: own(component, scope),
                });
            }
        }
    }
    out
}

impl<'a> Walk<'a> {
    fn new(db: &'a dyn Db, build: Build<'a>, props: &'a Properties) -> Self {
        let default_lang = Lang::new(build.project(db).manifest(db).project.lang.v.as_str())
            .unwrap_or_else(Lang::english);
        Self {
            db,
            build,
            props,
            ids: Ids::new(props),
            default_lang,
            within: within_decls(db, build),
            scopes: scope_decls(db, build),
            counters: HashMap::new(),
            lists: Vec::new(),
            seen: HashSet::new(),
            schemas: HashMap::new(),
            counter_keys: HashMap::new(),
            reported: HashSet::new(),
            out: Resolution::default(),
            refs: Vec::new(),
            styles: HashMap::new(),
            patterns: HashMap::new(),
            shared: None,
            quiet: false,
            pending_queries: Vec::new(),
            aggregates: aggregate_elements(db, build),
        }
    }

    /// The values the common tree gives a target of a shared counter, in
    /// place of `own`, the values the build's own tree gives it.
    fn shared_values(&self, counter: &str, target: Target, own: EcoVec<u64>) -> EcoVec<u64> {
        match &self.shared {
            Some(shared) if shared.counters.contains(counter) => shared
                .values
                .get(&target)
                .filter(|(name, _)| name == counter)
                .map_or(own, |(_, values)| values.clone()),
            _ => own,
        }
    }
}

impl Walk<'_> {
    fn element(&mut self, element: &Element, node: &StyledNode) {
        self.visit(element, node);
        let scoped = self.open_scopes(element);
        let list = is_builtin(&element.name, "enum");
        if list {
            let pattern = self.pattern(&node.style);
            self.lists.push((0, pattern));
        }
        for (child, child_node) in styled_children(element, node) {
            match child {
                Child::Element(child) => self.element(child, child_node),
                Child::Part(_, content) => {
                    let mut kids = StyledChildren::new(child_node);
                    for child in content_elements(content) {
                        if let Some(child_node) = kids.element(child) {
                            self.element(child, child_node);
                        }
                    }
                }
            }
        }
        if list {
            self.lists.pop();
        }
        self.close_scopes(scoped);
    }

    /// Opens a new level of each counter scoped to an element, after the
    /// element itself has stepped them, and returns what to restore when
    /// its subtree ends: each counter's state before, or `None` where it
    /// had none.
    fn open_scopes(&mut self, element: &Element) -> Vec<(EcoString, Option<Counter>)> {
        let mut saved: Vec<(EcoString, Option<Counter>)> = Vec::new();
        for decl in &self.scopes {
            if decl.scope != element.name || saved.iter().any(|(name, _)| *name == decl.counter) {
                continue;
            }
            let before = self.counters.get(&decl.counter).cloned();
            // The new level keeps the values of the instance the counter
            // was restarted `within`.
            let (prefix, restarts) = before
                .as_ref()
                .map_or_else(Default::default, |c| (c.prefix.clone(), c.restarts));
            self.counters.insert(
                decl.counter.clone(),
                Counter {
                    prefix,
                    values: Vec::new(),
                    restarts,
                },
            );
            saved.push((decl.counter.clone(), before));
        }
        saved
    }

    /// Restores the counters an element's subtree opened levels of. A
    /// counter restarted `within` an element inside the subtree keeps the
    /// restart: restarts go in document order, through scopes.
    fn close_scopes(&mut self, saved: Vec<(EcoString, Option<Counter>)>) {
        for (name, before) in saved {
            let restarted = self
                .counters
                .get(&name)
                .filter(|now| now.restarts != before.as_ref().map_or(0, |c| c.restarts))
                .map(|now| Counter {
                    prefix: now.prefix.clone(),
                    values: Vec::new(),
                    restarts: now.restarts,
                });
            match restarted.or(before) {
                Some(counter) => self.counters.insert(name, counter),
                None => self.counters.remove(&name),
            };
        }
    }

    /// Counts an element, records its label and marker, and takes its
    /// references.
    fn visit(&mut self, element: &Element, node: &StyledNode) {
        let style = &node.style;
        self.reset(element, style);

        let counted = self.counted(&element.name);
        let mut numbered = false;
        if is_builtin(&element.name, "math") {
            numbered = self.equation(element, style);
        } else if is_builtin(&element.name, "enum-item") {
            let next = match element.field("number") {
                Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
                _ => self.lists.last().map_or(1, |(n, _)| n + 1),
            };
            if let Some((count, _)) = self.lists.last_mut() {
                *count = next;
            }
            let values = EcoVec::from([next]);
            // An item takes its enum's pattern where it has none.
            let pattern = self
                .pattern(style)
                .or_else(|| self.lists.last().and_then(|(_, p)| p.clone()));
            self.number(element, "enum-item".into(), values, pattern.as_ref());
            numbered = true;
        } else if counted.counter.is_some() {
            let name: EcoString = element.name.to_string().into();
            let values = self.step(&name, element);
            let values = self.shared_values(&name, Target::element(element.location), values);
            let pattern = self.pattern(style);
            self.number(element, name, values, pattern.as_ref());
            numbered = true;
        } else if self.pattern(style).is_some() && !is_list(&element.name) {
            self.report_once(
                eco_format!("numbering:{}", element.name),
                Diagnostic::warning(
                    element.span,
                    eco_format!(
                        "`{}` has no counter, so its `numbering` has no effect",
                        element.name
                    ),
                )
                .with_hint(
                    "headings, figures, equations, notes and the items of `+` lists are numbered",
                ),
            );
        }

        if let Some(label) = &element.label
            && self.seen.insert(label.clone())
        {
            self.label(label.clone(), Target::element(element.location), element);
        }
        if numbered || element.label.is_some() {
            self.styles.insert(element.location, style.clone());
        }
        let markers = self.out.markers.len();
        self.marker(element, node, numbered, &counted);
        if is_builtin(&element.name, "footnote")
            && let Some(number) = self.out.numbers.last()
            && number.target.location == element.location
        {
            let marker = self.out.markers.get(markers);
            let numbered_marker = marker.is_some()
                && !matches!(
                    style.value(self.props, self.ids.marker),
                    Some(StyleValue::Str(_))
                );
            self.out.footnotes.push(Footnote {
                location: element.location,
                value: number.values.last().copied().unwrap_or(1),
                pattern: self.pattern(style),
                numbered_marker,
                span: element.span,
            });
        }
        self.restart_within(element);

        self.query(element, style);

        if is_builtin(&element.name, "ref")
            && let Some(Value::Label(label)) = element.field("target")
        {
            let supplement = match element.field("supplement") {
                Some(Value::Content(content)) => Some(content.plain_text().trim().into()),
                _ => None,
            };
            self.refs.push(PendingRef {
                location: element.location,
                label: label.clone(),
                span: element.span,
                supplement,
                lang: self.lang(style),
                page: is_page_form(element),
            });
        }
    }

    /// Takes a query element, to resolve once the whole tree is walked.
    fn query(&mut self, element: &Element, style: &ComputedStyle) {
        let list = is_builtin(&element.name, "outline") || is_builtin(&element.name, "list-of");
        let total = is_builtin(&element.name, "total");
        if !list && !total {
            self.aggregate_query(element, style);
            return;
        }
        let Some(Value::Str(target)) = element.field("target") else {
            return;
        };
        let kind = if total {
            let Some(Value::Str(field)) = element.field("field") else {
                return;
            };
            let marker = locale::number_format(
                &self.lang(style),
                style.value(self.props, self.ids.decimal_marker),
            )
            .decimal_marker;
            query::Kind::Total {
                field: field.clone(),
                marker,
            }
        } else {
            query::Kind::List
        };
        self.pending_queries.push(query::Pending {
            location: element.location,
            span: element.span,
            target: target.clone(),
            kind,
        });
    }

    /// Takes the element of a component's aggregate (D-72-1), to resolve
    /// once the whole tree is walked.
    fn aggregate_query(&mut self, element: &Element, style: &ComputedStyle) {
        let Some(namespace) = &element.name.namespace else {
            return;
        };
        if !self.aggregates.contains(&element.name) {
            return;
        }
        let marker = locale::number_format(
            &self.lang(style),
            style.value(self.props, self.ids.decimal_marker),
        )
        .decimal_marker;
        self.pending_queries.push(query::Pending {
            location: element.location,
            span: element.span,
            target: element.name.to_string().into(),
            kind: query::Kind::Aggregate {
                namespace: namespace.clone(),
                name: element.name.name.clone(),
                marker,
            },
        });
    }

    /// Resolves the queries the walk took (SPEC §4.5): the entries of
    /// outlines and figure lists from `root`, the variant's own tree, with
    /// the markers the walk set; totals from the numbering group's common
    /// tree, or from `root` outside a group.
    fn queries(&mut self, root: &Element, styled: &StyledNode) {
        let pending = std::mem::take(&mut self.pending_queries);
        if pending.is_empty() {
            return;
        }
        let group = crate::variants::common::group_build(self.db, self.build);
        let common =
            group.and_then(|group| crate::variants::common::common_tree(self.db, group).as_ref());
        // Only the instances the build styled are shown, so only they are
        // listed.
        let mut shown = HashSet::new();
        if pending.iter().any(|q| matches!(q.kind, query::Kind::List)) {
            fn collect(node: &StyledNode, out: &mut HashSet<Location>) {
                out.insert(node.location);
                for child in &node.children {
                    collect(child, out);
                }
            }
            collect(styled, &mut shown);
        }
        let mut out = Vec::with_capacity(pending.len());
        for q in pending {
            let result = match &q.kind {
                query::Kind::List => {
                    let resolution = &self.out;
                    // A reference in a title shows the text Resolve gave it.
                    let text = |element: &Element| {
                        is_builtin(&element.name, "ref").then(|| {
                            resolution
                                .reference(element.location)
                                .map_or(UNRESOLVED.into(), |r| r.text.clone())
                        })
                    };
                    let entries = query::instances(root, &q.target)
                        .into_iter()
                        .filter(|element| shown.contains(&element.location))
                        .filter_map(|element| {
                            let marker = resolution.marker(element.location).and_then(|m| {
                                // A marker in a part other than the caption
                                // is not the entry's.
                                (m.part.is_none()
                                    || m.part.as_ref().is_some_and(|p| p.as_str() == "caption"))
                                .then(|| m.text.clone())
                            });
                            let title = query::title(element, &text);
                            // An instance with neither, such as a figure
                            // without a caption, has nothing to list.
                            if marker.is_none() && title.is_empty() {
                                return None;
                            }
                            Some(Entry {
                                target: element.location,
                                level: query::level(element),
                                marker,
                                title,
                                page: UNRESOLVED.into(),
                            })
                        })
                        .collect();
                    QueryResult::List(entries)
                }
                query::Kind::Aggregate {
                    namespace,
                    name,
                    marker,
                } => {
                    self.warn_group_unavailable(group.is_some() && common.is_none(), q.span);
                    let value = crate::aggregate::aggregate(
                        self.db,
                        self.build,
                        namespace.clone(),
                        name.clone(),
                    );
                    match value.as_ref().and_then(|v| query::show(v, *marker)) {
                        Some(text) => QueryResult::Total(text),
                        None => {
                            let qualified =
                                QualifiedName::qualified(namespace.clone(), name.clone());
                            self.report(query::no_value(q.span, &qualified));
                            QueryResult::Total(UNRESOLVED.into())
                        }
                    }
                }
                query::Kind::Total { field, marker } => {
                    self.warn_group_unavailable(group.is_some() && common.is_none(), q.span);
                    let tree = common.map_or(root, |c| &c.root);
                    let (sum, wrong) = query::sum(query::instances(tree, &q.target), field);
                    if let Some(element) = wrong {
                        self.report(query::not_a_number(q.span, field, element));
                    }
                    QueryResult::Total(query::format(sum, *marker))
                }
            };
            out.push(Query {
                location: q.location,
                result,
            });
        }
        self.out.queries = out.into_iter().collect();
    }

    /// Warns at a total or aggregate that counts only what its variant
    /// shows, as its numbering group's common tree could not be made.
    fn warn_group_unavailable(&mut self, unavailable: bool, span: Span) {
        if !unavailable {
            return;
        }
        let Some(variant) = self.build.variant(self.db) else {
            return;
        };
        let manifest = self.build.project(self.db).manifest(self.db);
        if let Some(decl) = manifest.numbering_group(variant.as_str()) {
            let name = decl.name.v.to_string();
            self.report(query::group_unavailable(span, &name));
        }
    }

    fn label(&mut self, label: EcoString, target: Target, element: &Element) {
        self.out.labels.push(Labelled {
            label,
            target,
            element: element.name.clone(),
            span: element.span,
        });
    }

    /// Numbers a display equation: once, or on each line that has a label.
    /// Returns whether it was numbered.
    fn equation(&mut self, element: &Element, style: &ComputedStyle) -> bool {
        if element.kind != ElementKind::Block {
            return false;
        }
        let pattern = self.pattern(style);
        let lines: Vec<(u32, EcoString)> = match element.field("tree") {
            Some(Value::Math(formula)) => formula
                .lines()
                .iter()
                .enumerate()
                .filter_map(|(i, line)| {
                    let label = line.label.as_ref()?;
                    Some((u32::try_from(i).ok()?, label.name.clone()))
                })
                .collect(),
            _ => Vec::new(),
        };
        let name = EcoString::from("equation");
        if lines.is_empty() {
            let values = self.step(&name, element);
            let values = self.shared_values(&name, Target::element(element.location), values);
            self.number(element, name, values, pattern.as_ref());
            return true;
        }
        for (line, label) in lines {
            let values = self.step(&name, element);
            let target = Target {
                location: element.location,
                line: Some(line),
            };
            let values = self.shared_values(&name, target, values);
            let number = pattern.as_ref().map(|p| p.format(&values));
            if let Some(pattern) = &pattern {
                self.patterns.insert(element.location, pattern.clone());
            }
            self.out.numbers.push(Number {
                target,
                counter: name.clone(),
                values,
                number,
            });
            if self.seen.insert(label.clone()) {
                self.label(label, target, element);
            }
        }
        true
    }

    fn number(
        &mut self,
        element: &Element,
        counter: EcoString,
        values: EcoVec<u64>,
        pattern: Option<&Pattern>,
    ) {
        let number = pattern.map(|p| p.format(&values));
        // Only a labelled element can be referred to.
        if let Some(pattern) = pattern
            && element.label.is_some()
        {
            self.patterns.insert(element.location, pattern.clone());
        }
        self.out.numbers.push(Number {
            target: Target::element(element.location),
            counter,
            values,
            number,
        });
    }

    /// Steps a counter for an element and returns its values.
    fn step(&mut self, name: &EcoString, element: &Element) -> EcoVec<u64> {
        let counter = self.counters.entry(name.clone()).or_default();
        if is_builtin(&element.name, "heading") {
            let level = match element.field("level") {
                Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(1).clamp(1, 64),
                _ => 1,
            };
            counter.values.resize(level, 0);
            counter.values[level - 1] += 1;
        } else {
            match counter.values.first_mut() {
                Some(n) => *n += 1,
                None => counter.values.push(1),
            }
        }
        counter
            .prefix
            .iter()
            .chain(&counter.values)
            .copied()
            .collect()
    }

    /// Restarts the counters an element's `counter-reset` names.
    fn reset(&mut self, element: &Element, style: &ComputedStyle) {
        let names: Vec<EcoString> = match style.value(self.props, self.ids.counter_reset) {
            None | Some(StyleValue::None) => return,
            Some(StyleValue::Array(items)) => items.iter().filter_map(counter_name).collect(),
            Some(value) => counter_name(value).into_iter().collect(),
        };
        for name in names {
            let key = match self.counter_keys.get(&name).cloned() {
                Some(key) => key,
                None => {
                    let key = self.counter_key(&name);
                    self.counter_keys.insert(name.clone(), key.clone());
                    key
                }
            };
            let key = match key {
                Ok(key) => key,
                Err(message) => {
                    self.report_once(
                        eco_format!("counter-reset:{name}"),
                        Diagnostic::warning(element.span, message).with_hint(
                            "counters are named after the elements they number, such as `figure` \
                             or a component's `question`, and display equations are counted by \
                             `equation`",
                        ),
                    );
                    continue;
                }
            };
            // A counter restarted `within` an element keeps its values.
            self.counters.entry(key).or_default().values.clear();
        }
    }

    /// Restarts the counters declared `within` an element, with its values
    /// before theirs.
    fn restart_within(&mut self, element: &Element) {
        if self.within.is_empty() {
            return;
        }
        let values = self
            .out
            .numbers
            .last()
            .filter(|n| n.target.location == element.location)
            .map(|n| n.values.clone())
            .unwrap_or_default();
        for decl in &self.within {
            if decl.within.element == element.name && fields_match(&decl.within, element) {
                let restarts = self.counters.get(&decl.counter).map_or(0, |c| c.restarts);
                self.counters.insert(
                    decl.counter.clone(),
                    Counter {
                        prefix: values.clone(),
                        values: Vec::new(),
                        restarts: restarts + 1,
                    },
                );
            }
        }
    }

    /// The counter a name in `counter-reset` names: a built-in one, or that
    /// of an element a component declares with one, named qualified or not.
    /// An unqualified name resolves as element names do, through the
    /// lockfile's bindings (SPEC §3.6). Fails with the warning's message.
    fn counter_key(&mut self, name: &str) -> Result<EcoString, EcoString> {
        if BUILTIN_COUNTERS.contains(&name) {
            return Ok(name.into());
        }
        let no_counter =
            || eco_format!("there is no counter `{name}`, so `counter-reset` does not restart it");
        let Some(written) = QualifiedName::new(name) else {
            return Err(no_counter());
        };
        let Some(host) = component_host(self.db, self.build.project(self.db)).as_ref() else {
            return Err(no_counter());
        };
        let qualified = match host.element(&written) {
            Ok(resolved) => QualifiedName::qualified(
                resolved.component.manifest.namespace(),
                resolved.item.name.clone(),
            ),
            Err(error) => {
                return Err(match error.reason {
                    Reason::Ambiguous(_) | Reason::StaleBinding(_) => {
                        eco_format!("{error}, so `counter-reset` does not restart it")
                    }
                    _ => no_counter(),
                });
            }
        };
        if self.counted(&qualified).counter.is_none() {
            return Err(no_counter());
        }
        Ok(qualified.to_string().into())
    }

    /// Sets the marker of an element: its number before its content, a
    /// figure's word and number before its caption, or the string of its
    /// `marker`.
    fn marker(&mut self, element: &Element, node: &StyledNode, numbered: bool, counted: &Counted) {
        let style = &node.style;
        let figure = is_builtin(&element.name, "figure");
        let part = figure.then(|| Name::new("caption").expect("an identifier"));
        let text = match style.value(self.props, self.ids.marker) {
            Some(StyleValue::None) => return,
            Some(StyleValue::Str(text)) => eco_format!("{text}{NBSP}"),
            _ => {
                // Equations are numbered by maths layout, at the line.
                if !numbered || is_builtin(&element.name, "math") {
                    return;
                }
                let Some(number) = self.out.numbers.last().and_then(|n| n.number.clone()) else {
                    return;
                };
                if figure {
                    let lang = self.lang(style);
                    // A caption starts a sentence: `Rys. 1: ..`.
                    match self.word(&element.name, counted, &lang) {
                        Some((word, _)) => eco_format!("{}{NBSP}{number}: ", capitalise(&word)),
                        None => eco_format!("{number}: "),
                    }
                } else {
                    eco_format!("{number}{NBSP}")
                }
            }
        };
        if figure && !matches!(element.field("caption"), Some(Value::Content(_))) {
            return;
        }
        self.out.markers.push(Marker {
            location: element.location,
            text,
            part,
        });
    }

    /// Resolves the references, once every label is known.
    fn references(&mut self) {
        let refs = std::mem::take(&mut self.refs);
        let mut resolved = Vec::with_capacity(refs.len());
        for r in refs {
            let labelled = self.out.label(&r.label).cloned();
            let page = r.page.then(|| self.page_word(&r));
            let (target, text) = match labelled {
                None => {
                    self.missing(&r);
                    (None, EcoString::from(UNRESOLVED))
                }
                // The page is set once a pass has measured it.
                Some(labelled) if page.is_some() => (Some(labelled.target), UNRESOLVED.into()),
                Some(labelled) => (Some(labelled.target), self.reference_text(&r, &labelled)),
            };
            resolved.push(Reference {
                location: r.location,
                label: r.label,
                target,
                text,
                page,
                span: r.span,
            });
        }
        self.out.references = resolved.into_iter().collect();
    }

    /// The text of a reference to a label that names something.
    fn reference_text(&mut self, r: &PendingRef, labelled: &Labelled) -> EcoString {
        let number = self.out.number(labelled.target).cloned();
        let Some(number) = number else {
            self.report(
                Diagnostic::warning(
                    r.span,
                    eco_format!(
                        "`<{}>` names {}, which is not numbered, so the reference has no number to show",
                        r.label,
                        a_element(&labelled.element)
                    ),
                )
                .with_hint(
                    "refer to a heading, figure, equation, note or item of a `+` list, \
                     which are numbered",
                ),
            );
            return UNRESOLVED.into();
        };
        let style = self.styles.get(&labelled.target.location).cloned();
        // The pattern the target was numbered with: an item's own, or its
        // enum's (#325).
        let pattern = self.patterns.get(&labelled.target.location);
        let shown = shown(pattern, &number.values);
        let number_only = matches!(
            style.as_ref().and_then(|s| s.value(self.props, self.ids.ref_format)),
            Some(StyleValue::Keyword(k)) if k.as_str() == "number"
        );
        if let Some(supplement) = &r.supplement {
            return if supplement.is_empty() {
                shown
            } else {
                eco_format!("{supplement}{NBSP}{shown}")
            };
        }
        if number_only {
            return shown;
        }
        let counted = self.counted(&labelled.element);
        match self.word(&labelled.element, &counted, &r.lang) {
            Some((word, true)) => eco_format!("{word}{NBSP}({shown})"),
            Some((word, false)) => eco_format!("{word}{NBSP}{shown}"),
            None => shown,
        }
    }

    /// The word before the page number of a page-number reference: its
    /// supplement, or the reference word for a page in its language.
    fn page_word(&self, r: &PendingRef) -> EcoString {
        if let Some(supplement) = &r.supplement {
            return supplement.clone();
        }
        locale::word(&r.lang, "page")
            .map(|w| w.text.into())
            .unwrap_or_default()
    }

    /// Reports a reference to a label that no element of the build has.
    fn missing(&mut self, r: &PendingRef) {
        if let Some(hidden) = self.hidden(&r.label) {
            let project = self.build.project(self.db);
            let strict = project.manifest(self.db).formatting.mode.v
                == crate::project::FormattingMode::Strict;
            let message = eco_format!(
                "`<{}>` names {} that {} does not show, so the reference has nothing to refer to",
                r.label,
                a_element(&hidden),
                match self.build.variant(self.db) {
                    Some(variant) => eco_format!("variant `{variant}`"),
                    None => "the document".into(),
                },
            );
            let diagnostic = if strict {
                Diagnostic::error(r.span, message)
            } else {
                Diagnostic::warning(r.span, message)
            };
            diag::report(
                self.db,
                diagnostic.with_code("reference-hidden").with_hint(
                    "show the element in this variant, or hide the reference with it, as in \
                     `#in-variant(\"markscheme\")[see @label]`",
                ),
            );
            return;
        }
        let labels: Vec<&str> = self.out.labels.iter().map(|l| l.label.as_str()).collect();
        let diagnostic = Diagnostic::warning(
            r.span,
            eco_format!(
                "no element has the label `<{}>`, so the reference has nothing to refer to",
                r.label
            ),
        );
        let diagnostic = match crate::project::suggest(&r.label, &labels) {
            Some(close) => diagnostic.with_hint(eco_format!("did you mean `<{close}>`?")),
            None => diagnostic
                .with_hint("label the element the reference means, such as `= Forces <forces>`"),
        };
        diag::report(self.db, diagnostic);
    }

    /// The name of the element with a label that the variant does not
    /// show (SPEC §4.5): one that its document tree has but Select left
    /// out. `None` if no element of the document tree has the label.
    fn hidden(&self, label: &str) -> Option<QualifiedName> {
        fn find(element: &Element, label: &str) -> Option<QualifiedName> {
            if element.label.as_deref() == Some(label) {
                return Some(element.name.clone());
            }
            element.fields.iter().find_map(|(_, value)| match value {
                Value::Content(content) => content.nodes().iter().find_map(|node| match node {
                    crate::eval::Node::Element(child) => find(child, label),
                    _ => None,
                }),
                _ => None,
            })
        }
        let project = self.build.project(self.db);
        let doc = document(self.db, project, self.build.variant(self.db).clone()).as_ref()?;
        find(&doc.root, label)
    }

    /// The reference word of an element in a language, and whether its
    /// number is set in brackets, or `None` if it has
    /// no reference key.
    fn word(
        &mut self,
        name: &QualifiedName,
        counted: &Counted,
        lang: &Lang,
    ) -> Option<(EcoString, bool)> {
        let key = if is_builtin(name, "math") {
            "equation"
        } else {
            counted.ref_key.as_ref()?.as_str()
        };
        if name.namespace.is_none() {
            return locale::word(lang, key).map(|w| (w.text.into(), w.brackets));
        }
        let host = component_host(self.db, self.build.project(self.db)).as_ref()?;
        let component = host.element(name).ok()?.component;
        locale::component_word(component, lang, key).map(|w| (w.text.into(), w.brackets))
    }

    /// The pattern of a style's `numbering`, if it is not `none`.
    fn pattern(&self, style: &ComputedStyle) -> Option<Pattern> {
        match style.value(self.props, self.ids.numbering) {
            Some(StyleValue::Str(text)) => Some(Pattern::parse(text)),
            _ => None,
        }
    }

    /// The language text in a style is set in.
    fn lang(&self, style: &ComputedStyle) -> Lang {
        match style.value(self.props, self.ids.lang) {
            Some(StyleValue::Str(tag)) => {
                Lang::new(tag).unwrap_or_else(|| self.default_lang.clone())
            }
            _ => self.default_lang.clone(),
        }
    }

    /// What the walk needs of an element's schema.
    fn counted(&mut self, name: &QualifiedName) -> Counted {
        if let Some(counted) = self.schemas.get(name) {
            return counted.clone();
        }
        let schema = schema(self.db, self.build.project(self.db), name);
        let counted = Counted {
            counter: schema.and_then(|s| s.counter.clone()),
            ref_key: schema.and_then(|s| s.ref_key.clone()),
        };
        self.schemas.insert(name.clone(), counted.clone());
        counted
    }

    fn report(&self, diagnostic: Diagnostic) {
        if !self.quiet {
            diag::report(self.db, diagnostic);
        }
    }

    fn report_once(&mut self, key: EcoString, diagnostic: Diagnostic) {
        if !self.quiet && self.reported.insert(key) {
            diag::report(self.db, diagnostic);
        }
    }
}

/// A number as a reference shows it: formatted by the pattern its element
/// was numbered with, without the pattern's text before and after. An
/// element without a pattern, or with one that shows no values, still has
/// its counter's values, shown as `1.1` would.
fn shown(pattern: Option<&Pattern>, values: &[u64]) -> EcoString {
    match pattern.filter(|p| p.counts()) {
        Some(pattern) => pattern.format_trimmed(values),
        None => Pattern::parse("1.1").format(values),
    }
}

/// The names of the built-in counters that `counter-reset` can name.
const BUILTIN_COUNTERS: &[&str] = &["heading", "figure", "footnote", "sidenote", "equation"];

/// A word with its first letter in upper case, to start a sentence.
fn capitalise(word: &str) -> EcoString {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => EcoString::new(),
    }
}

/// The name of a counter in a `counter-reset` value.
fn counter_name(value: &StyleValue) -> Option<EcoString> {
    match value {
        StyleValue::Keyword(name) => Some(name.as_str().into()),
        StyleValue::Str(name) => Some(name.clone()),
        _ => None,
    }
}

/// Whether an element has the field values a `within` asks for.
fn fields_match(within: &Within, element: &Element) -> bool {
    within
        .fields
        .iter()
        .all(|(name, want)| match (element.field(name.as_str()), want) {
            (Some(Value::Int(a)), StyleValue::Int(b)) => a == b,
            (Some(Value::Bool(a)), StyleValue::Bool(b)) => a == b,
            (Some(Value::Str(a)), StyleValue::Str(b)) => a == b,
            (Some(Value::Str(a)), StyleValue::Keyword(b)) => a.as_str() == b.as_str(),
            _ => false,
        })
}

/// An element's name with its article, for messages: `a heading`, `an
/// emph`.
fn a_element(name: &QualifiedName) -> EcoString {
    let text = name.to_string();
    let vowel = text.chars().next().is_some_and(|c| "aeiou".contains(c));
    eco_format!("{} `{text}`", if vowel { "an" } else { "a" })
}

/// Whether an element is the built-in element of a name.
fn is_builtin(name: &QualifiedName, builtin: &str) -> bool {
    name.namespace.is_none() && name.name.as_str() == builtin
}

/// Whether a `ref` element refers to its target's page rather than its
/// number.
fn is_page_form(element: &Element) -> bool {
    matches!(element.field("form"), Some(Value::Str(form)) if form.as_str() == "page")
}

/// Whether an element is a list, whose `numbering` its items take.
fn is_list(name: &QualifiedName) -> bool {
    ["list", "enum", "terms"]
        .iter()
        .any(|list| is_builtin(name, list))
}
