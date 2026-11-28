//! The exam component of Folio (SPEC §13.1): question papers and their mark
//! schemes from one source.
//!
//! It adds, in the namespace `exam`:
//!
//! - **Elements** ([`elements`]): `question(marks)`, `part(marks)`,
//!   `answer(lines)`, `mark(code)` and `markscheme-note`. Questions count
//!   through the paper and parts are scoped to questions and parts, so they
//!   nest: *1*, *(a)*, *(i)*, *(ii)*, *(b)* (SPEC §4.5).
//! - **An aggregate** ([`marks`]): `total-marks`, the marks of the paper,
//!   which content shows with `#exam.total-marks()`. A question or part
//!   without marks of its own counts the marks of its parts.
//! - **Lints** ([`marks`]): a question's or part's marks must equal the sum
//!   of its parts' (`exam.marks-sum`).
//! - **Templates** ([`templates`]): `markscheme-box` for answers, `badge`
//!   for mark codes and `markscheme-table` for questions and parts, and the
//!   default templates that set a question's trailer and a part's marks.
//! - **A default stylesheet** (`src/exam.fsty`): questions and parts
//!   numbered, answers shown as answer space and mark codes and notes
//!   hidden, except in the variant `markscheme`, which shows them (SPEC
//!   §6.2, §6.3).
//!
//! The component depends only on `folio-component` (SPEC §2.2), and is
//! versioned with the compiler.

#![forbid(unsafe_code)]

pub mod elements;
pub mod marks;
pub mod templates;

use ecow::{EcoString, EcoVec};
use folio_component::{
    AggregateDecl, Component, ComponentManifest, ComponentName, Declarations, InstanceTree,
    LintDiagnostic, LintReads, Name, Target, Value,
};

/// The component's name, which is also its namespace.
pub const NAME: &str = "exam";

/// The most lines `#answer(lines: n)` sets as ruled lines: more than an A4
/// page holds at the default spacing. A larger number is a warning
/// (`exam.answer-lines`) and sets no lines.
pub const MAX_LINES: i64 = 40;

/// The default stylesheet, before the rules that `answer(lines)` needs
/// ([`stylesheet`]).
const SHEET: &str = include_str!("exam.fsty");

/// The exam component.
#[derive(Debug, Default, Copy, Clone)]
pub struct Exam;

impl Exam {
    /// The exam component.
    pub fn new() -> Self {
        Self
    }
}

impl Component for Exam {
    fn manifest(&self) -> ComponentManifest {
        let version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("the crate's version is semver");
        ComponentManifest::new(
            ComponentName::new(NAME).expect("`exam` is a component name"),
            version,
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let mut declarations = Declarations::new();
        declarations.elements = elements::schemas();
        declarations.locale_strings = elements::locale_strings();
        declarations.templates = templates::templates();
        declarations.stylesheet = Some(stylesheet());
        declarations.aggregates = EcoVec::from([
            AggregateDecl::new(name(marks::TOTAL_MARKS)).reading([name(elements::MARKS)])
        ]);
        declarations.lint_reads =
            LintReads::Fields(EcoVec::from([name(elements::MARKS), name(elements::LINES)]));
        declarations
    }

    fn lint(&self, view: &InstanceTree) -> EcoVec<LintDiagnostic> {
        marks::lint(view)
    }

    fn aggregate(&self, name: &str, view: &InstanceTree) -> Option<Value> {
        match name {
            marks::TOTAL_MARKS => Some(marks::total(view)),
            _ => None,
        }
    }
}

/// The default stylesheet: `src/exam.fsty`, then a rule for each number of
/// lines up to [`MAX_LINES`] that gives an answer with `lines: n` that many
/// ruled lines (`rule-lines`, SPEC §6.3), and the mark scheme's reset of
/// them, which must come after those rules.
pub fn stylesheet() -> EcoString {
    let mut sheet = EcoString::from(SHEET);
    sheet.push_str(
        "\n// `#answer(lines: n)` sets `rule-lines: n` (SPEC §6.3), for n up to the\n\
         // component's limit; the mark scheme shows the answer instead.\n",
    );
    for n in 1..=MAX_LINES {
        sheet.push_str(&format!("answer[lines={n}] {{ rule-lines: {n}; }}\n"));
    }
    sheet.push_str("@variant markscheme { answer[lines] { rule-lines: none; } }\n");
    sheet
}

/// A name the component spells out, which is always valid.
fn name(text: &str) -> Name {
    Name::new(text).expect("the component's names are identifiers")
}
