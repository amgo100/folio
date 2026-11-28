//! Evaluation (SPEC §2.1, §4.4): runs the scripting layer of content files
//! and produces the content tree.
//!
//! This is the stage that becomes `folio-eval` (SPEC §2.2) once its
//! interface settles. The style stage evaluates stylesheet values with the
//! same operators and standard library; render
//! templates will be evaluated here too, with the same expression grammar.
//!
//! # Memoisation
//!
//! Evaluation is a set of queries in the database of [`crate::db`]
//! (SPEC §2.3):
//!
//! - **Files.** Each content file is evaluated by its own query into a
//!   [`Module`]: its top-level bindings and its content. An import or
//!   include reads the imported file's query, so a file is evaluated once
//!   however often it is imported.
//! - **Calls.** Each call of a closure is a query keyed by the closure and
//!   the arguments without their spans, so the same call at two sites is
//!   evaluated once. Spans reach a call's result only through its errors
//!   and argument sinks: a closure with a sink is keyed with them, and a
//!   call that fails is evaluated again with them, so its errors point at
//!   the arguments. A closure holds its syntax and the values
//!   of the variables it uses from outside (its captures), not its whole
//!   environment. Editing the body of one function therefore changes that
//!   closure, and the closures that capture it, and re-runs their calls; the
//!   calls of every other function are reused.
//! - **Depth.** Depth matters only to the depth limit, so a file, and a
//!   call made from a file's own code, is evaluated at depth 1 wherever it
//!   is imported or made. Each evaluation reports the deepest call or
//!   import it made, and the caller moves that to its real depth: if it
//!   stays within the limit the result is the same, and otherwise the file
//!   or call is evaluated again at its real depth, where it fails as it
//!   would have. A call made inside a call is keyed by its depth, which
//!   grows along every chain of calls, so recursion ends at the limit.
//! - **Keys.** Hashing a call's key, and comparing two results for early
//!   cutoff, walk values as trees, and a value can hold one part many
//!   times: a loop that doubles an array by holding it twice makes a tree
//!   exponentially larger than the value. So containers that hold
//!   containers cache their hash where their clones share it, and a
//!   comparison compares each pair of parts once.
//! - **Variant.** The variant is a key, not an input. A file or call is
//!   first evaluated for all variants at once. If it reads `variant`, that
//!   evaluation stops, and it is evaluated again for the build's variant.
//!   Code that never reads `variant`, including the functions it calls, is
//!   evaluated once for every variant.
//!
//! # Partial evaluation
//!
//! Compilation is partial (SPEC §12.2): a construct of a file's markup that
//! fails becomes an error marker in its place, an `error` element whose
//! body shows `[error]` and whose span is the construct's, and the rest of
//! the file is still evaluated. A construct fails when it does not parse,
//! when its code has a syntax error (`check_source` reports both, and code
//! is not evaluated around syntax errors), or when evaluating it is an
//! error, which the file's query reports with its trace. Only a file's own
//! markup recovers: markup inside code fails as a whole, so an error in a
//! function reaches the markup that called it. The variables that a failed
//! `let` or import would have bound are poisoned, and using one fails
//! without an error of its own, and so does a closure that uses one. A
//! label after a construct that failed is dropped. Exceeding the step or
//! memory limit is not recovered from, since everything after it would
//! fail too; nor are errors outside markup, such as a `break` outside a
//! loop. The depth limit is: the next construct starts at the file's
//! depth again.
//!
//! # Sandbox
//!
//! Scripts read files only through the database: sources and data files of
//! the project, by paths that cannot leave it. There is no network access
//! and no clock: `today` is the date of the manifest's `project.today`, or
//! the one the host sets (SPEC §4.4).
//!
//! # Limits
//!
//! Evaluation is limited in steps, memory and call depth ([`Limits`]);
//! exceeding a limit is an error at the expression or call where it
//! happened. The counts are deterministic: a memoised call reports what it
//! cost, and its caller counts that whether the call ran or was reused, so a
//! document that is within the limits stays within them whatever the state
//! of the memo tables. Memory is counted as the bytes that strings, arrays,
//! dictionaries and content grow by, which bounds what a script can
//! allocate.
//!
//! Values nest at most [`MAX_NESTING`] deep: content in content, arrays in
//! arrays, closures that capture closures. Every value caches its depth
//! ([`Value::depth`]), so the check is cheap; an expression whose value
//! would nest more deeply is an error, and so is an assignment or a
//! mutating method that would nest the variable it changes more deeply,
//! which then changes nothing. Values are hashed, compared and dropped
//! recursively, and so is content in the stages after evaluation, so this
//! limit at the source is what keeps all of them within the stack.
//!
//! Imports cannot form a cycle: import paths are string literals, and an
//! import of a file that imports, directly or not, the importing file is an
//! error. A call made inside a call is keyed by its depth, so a recursive
//! call is a different query from its caller. The interpreter grows its stack when it
//! runs low, so deep expressions in deep calls do not exhaust the stack of
//! the thread that compiles.

mod captures;
mod content;
mod decl;
mod element;
mod error;
mod library;
mod math;
pub(crate) mod ops;
mod value;
mod vm;

use std::collections::{BTreeSet, VecDeque};

use ecow::EcoVec;
use folio_component::Name;
use folio_syntax::ast;

pub use self::content::{Content, Element, Node, Slot};
pub use self::decl::{
    Declared, PROJECT, ProjectElements, Schema, file_elements, is_project, project_component,
    project_elements,
};
pub(crate) use self::element::canonical_fields;
pub use self::error::{EvalError, Tracepoint};
pub use self::value::{
    Angle, Applied, Arg, Args, Array, Closure, Dict, Duration, Fraction, Func, Length, Module,
    NativeFunc, Scope, Value,
};
pub(crate) use self::vm::float_spelling;

use self::error::Stop;
use self::vm::Vm;
use crate::db::{Db, Project, SourceFile, dependencies, source_file};
use crate::diag;
use crate::project::{Date, ProjectPath};

/// How deeply a value may nest (SPEC §4.4): content in content, arrays in
/// arrays, and so on, as [`Value::depth`] counts it. A value that would nest
/// more deeply is an error at the expression that makes it.
///
/// Unlike [`Limits`], this is not for the host to set. Hashing, comparing
/// and dropping a value, and the stages after evaluation, walk it
/// recursively, so the limit is what keeps them within the stack of a
/// thread: 2 MiB for the threads that compile builds in parallel, in a
/// debug build too. The deepest walk is the PDF structure tree's, which
/// nests three tags for each level of a nested list and is serialised
/// recursively by `krilla`; it overflows such a thread at about 145 levels.
/// No document nests nearly as deeply as the limit.
pub const MAX_NESTING: u32 = 100;

/// The limits of evaluation (SPEC §4.4, §14.3). A host sets them on the
/// world; the defaults suit any document.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Limits {
    /// The most steps one evaluation of a file may take, counting the steps
    /// of the calls and imports it makes. A step is one expression, or one
    /// iteration of a loop.
    pub steps: u64,
    /// The most bytes the values of one evaluation of a file may grow by:
    /// strings, arrays, dictionaries and content, and the data files read.
    pub memory: u64,
    /// The deepest nesting of calls and imports.
    pub depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: 20_000_000,
            memory: 512 * 1024 * 1024,
            depth: 64,
        }
    }
}

/// What an evaluation cost: steps taken and bytes allocated.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub(crate) struct Cost {
    pub steps: u64,
    pub bytes: u64,
}

/// Which variant and version a file or call is evaluated for.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) enum Mode {
    /// For every variant and version at once: reading `variant` or
    /// `version` stops evaluation.
    Shared,
    /// For one variant, or for the only build of a project without variants
    /// (`None`), and one version of a shuffled paper, or none for a project
    /// without versions (SPEC §6.4).
    Variant(Option<Name>, Option<Name>),
}

/// The result of a memoised evaluation, what it cost, and how deeply it
/// nested calls and imports.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct Memo<T> {
    pub outcome: Result<T, Stop>,
    pub cost: Cost,
    /// The deepest call or import the evaluation made, counted from the
    /// depth it was evaluated at: that depth if it made none.
    pub reached: u32,
    /// The errors that markup turned into markers, in this evaluation and
    /// in the evaluations of the files and calls it used. They are
    /// reported by whoever uses the evaluation in the end, not by its
    /// query, because an evaluation can be replaced by another one of the
    /// same file or call (at another depth, for a variant, or with spans)
    /// whose errors differ.
    pub recovered: EcoVec<EvalError>,
}

/// Evaluates a project's entry file for a variant (`None` for a project
/// without variants) and a version (`None` for a project without versions,
/// or a build that keeps the source order) and returns its content, or
/// `None` after reporting why it could not be evaluated.
///
/// It does not read the target, so all targets of a variant share it. The
/// files it imports and the functions it calls are evaluated once for all
/// variants and versions unless they read `variant` or `version`.
#[salsa::tracked(returns(ref))]
pub fn evaluate(
    db: &dyn Db,
    project: Project,
    variant: Option<Name>,
    version: Option<Name>,
) -> Option<Content> {
    let entry = project.manifest(db).project.entry.v.clone();
    // A missing entry file is reported by `build_files`.
    let file = source_file(db, project, entry)?;
    let memo = eval_file_for(db, project, file, &Mode::Variant(variant, version), 0);
    for error in &memo.recovered {
        diag::report(db, error.clone().into());
    }
    match &memo.outcome {
        Ok(module) => Some(module.content().clone()),
        Err(Stop::Error(error)) => {
            diag::report(db, (**error).clone().into());
            None
        }
        Err(Stop::Reported | Stop::NeedsVariant) => None,
    }
}

/// Evaluates a file for a mode, first for all variants and then, if it read
/// `variant`, for the mode's variant.
pub(crate) fn eval_file_for<'db>(
    db: &'db dyn Db,
    project: Project,
    file: SourceFile,
    mode: &Mode,
    depth: u32,
) -> &'db Memo<Module> {
    let shared = eval_file(db, project, file, Mode::Shared, depth);
    match (&shared.outcome, mode) {
        (Err(Stop::NeedsVariant), Mode::Variant(..)) => {
            eval_file(db, project, file, mode.clone(), depth)
        }
        _ => shared,
    }
}

/// Evaluates a content file into a module. `depth` is the depth it is
/// evaluated at: 0 for the entry file, and 1 for an imported file unless
/// it reaches the depth limit at its import's depth (see the module docs).
#[salsa::tracked(returns(ref))]
fn eval_file(
    db: &dyn Db,
    project: Project,
    file: SourceFile,
    mode: Mode,
    depth: u32,
) -> Memo<Module> {
    let source = file.source(db);
    let path = file.path(db).clone();
    let mut vm = Vm::new(db, project, path.clone(), mode, depth, Scope::default());
    // The file's markup compiles around what fails (see the module docs).
    vm.recover = true;
    vm.file_level = true;
    let outcome = match source.root().cast::<ast::Markup<'_>>() {
        Some(markup) => vm.module(markup, path.stem()),
        None => Err(EvalError::new(
            source.root().span(),
            "only content files (`.fol`) can be evaluated",
        )
        .into()),
    };
    Memo {
        outcome,
        cost: vm.cost,
        reached: vm.reached,
        recovered: vm.recovered.into_iter().collect(),
    }
}

/// Calls a closure. `depth` is the depth the call is evaluated at: 1 for a
/// call made from a file's code unless it reaches the depth limit at its
/// own depth, and one more than its caller's for a call made in a call (see
/// the module docs).
#[salsa::tracked(returns(ref))]
pub(crate) fn call_closure(
    db: &dyn Db,
    project: Project,
    closure: Closure,
    args: Args,
    depth: u32,
    mode: Mode,
) -> Memo<Value> {
    let mut vm = Vm::new(
        db,
        project,
        closure.file().clone(),
        mode,
        depth,
        closure.captured().clone(),
    );
    // What the call makes is open until it returns to its call site, so
    // the memo serves every site (SPEC §2.3).
    vm.origin = crate::locate::Origin::OPEN;
    let outcome = vm.closure_body(&closure, args);
    Memo {
        outcome,
        cost: vm.cost,
        reached: vm.reached,
        recovered: vm.recovered.into_iter().collect(),
    }
}

/// Calls a function of the standard library or a closure, as a stylesheet
/// value does (`rgb("1f5fa8")`, `calc.max(..)`), from the stylesheet at
/// `file`. Stylesheets have no variables, so the call cannot read
/// `variant`; a function that does is an error.
pub(crate) fn call_function(
    db: &dyn Db,
    project: Project,
    file: ProjectPath,
    func: Func,
    args: Args,
    span: folio_syntax::Span,
) -> Result<Value, EvalError> {
    let mut vm = Vm::new(db, project, file, Mode::Shared, 0, Scope::default());
    let result = vm.call(func, args, span);
    for error in std::mem::take(&mut vm.recovered) {
        diag::report(db, error.into());
    }
    match result {
        Ok(value) => Ok(value),
        Err(Stop::Error(error)) => Err(*error),
        Err(Stop::NeedsVariant) => Err(EvalError::new(
            span,
            "a stylesheet value cannot depend on the variant or the version",
        )
        .with_hint("put rules for one variant in `@variant name { .. }`")),
        Err(Stop::Reported) => Err(EvalError::new(span, "this function cannot be evaluated")),
    }
}

/// A numeric literal with its unit, as the evaluator represents it.
pub(crate) fn numeric(value: f64, unit: ast::Unit) -> Value {
    vm::numeric(value, unit)
}

/// The standard library and the elements of the project's components: the
/// variables every file sees unless it shadows them.
#[salsa::tracked(returns(ref))]
pub(crate) fn globals(db: &dyn Db, project: Project) -> Scope {
    library::globals(crate::db::component_host(db, project).as_ref())
}

/// The date scripts see as `today`: the host's, or the manifest's
/// `project.today`.
#[salsa::tracked(returns(copy))]
pub(crate) fn today(db: &dyn Db, project: Project) -> Option<Date> {
    project
        .today(db)
        .or_else(|| project.manifest(db).project.today.as_ref().map(|d| d.v))
}

/// The files a file names literally in its imports and includes, directly
/// or through others. A file is in it only when the imports form a cycle.
#[salsa::tracked(returns(ref))]
pub(crate) fn reachable(db: &dyn Db, project: Project, file: SourceFile) -> BTreeSet<ProjectPath> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([file]);
    while let Some(file) = queue.pop_front() {
        for dep in dependencies(db, file) {
            if seen.insert(dep.path.clone())
                && let Some(next) = source_file(db, project, dep.path.clone())
            {
                queue.push_back(next);
            }
        }
    }
    seen
}
