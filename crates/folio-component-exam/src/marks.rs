//! Marks: the paper's total (an aggregate) and the check that a question's
//! or part's marks are the sum of its parts' (a lint), SPEC §3.2, §13.1.
//!
//! An instance's marks are its own `marks` when it has them; otherwise
//! they are computed, as the sum of the marks of its parts. So a question
//! may give its total, its parts' marks, or both, and the paper's total
//! counts each mark once.

use ecow::{EcoVec, eco_format};
use folio_component::{Instance, InstanceTree, LintDiagnostic, Scalar, Value};

use crate::elements::{ANSWER, LINES, MARKS, PART, QUESTION};
use crate::{MAX_LINES, name};

/// The name of the aggregate of the paper's marks: `#exam.total-marks()`.
pub const TOTAL_MARKS: &str = "total-marks";

/// A number of marks: whole, or with a fraction where a paper gives half
/// marks.
#[derive(Debug, Copy, Clone, PartialEq)]
enum Marks {
    Int(i64),
    Float(f64),
}

impl Marks {
    const ZERO: Self = Self::Int(0);

    fn add(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_add(b)
                .map_or(Self::Float(a as f64 + b as f64), Self::Int),
            (a, b) => Self::Float(a.float() + b.float()),
        }
    }

    fn float(self) -> f64 {
        match self {
            Self::Int(n) => n as f64,
            Self::Float(x) => x,
        }
    }

    /// Whether two numbers of marks are the same, allowing for the
    /// rounding of fractions such as 0.1 + 0.2.
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a == b,
            (a, b) => (a.float() - b.float()).abs() <= 1e-9 * a.float().abs().max(1.0),
        }
    }

    fn value(self) -> Value {
        match self {
            Self::Int(n) => Value::Int(n),
            Self::Float(x) if x.fract() == 0.0 && x.abs() < 9.0e15 => Value::Int(x as i64),
            Self::Float(x) => Value::Number(Scalar::new(x)),
        }
    }

    fn text(self) -> String {
        match self.value() {
            Value::Int(n) => n.to_string(),
            Value::Number(x) => x.get().to_string(),
            _ => unreachable!("marks are numbers"),
        }
    }
}

/// An instance's own marks, if it gives a number.
fn own(instance: &Instance) -> Option<Marks> {
    match instance.field(MARKS)? {
        Value::Int(n) => Some(Marks::Int(*n)),
        Value::Number(x) if x.get().is_finite() => Some(Marks::Float(x.get())),
        _ => None,
    }
}

fn is(instance: &Instance, element: &str) -> bool {
    instance.name.name.as_str() == element
}

/// The questions and parts directly inside an instance's parts, or at the
/// top of the tree.
fn parts(instance: &Instance) -> impl Iterator<Item = &Instance> {
    instance.children().flat_map(scorable)
}

/// The instance if it is a question or part, else the questions and parts
/// in it nearest to the top: marks inside an answer still count.
fn scorable(instance: &Instance) -> Box<dyn Iterator<Item = &Instance> + '_> {
    if is(instance, QUESTION) || is(instance, PART) {
        Box::new(std::iter::once(instance))
    } else {
        Box::new(instance.children().flat_map(scorable))
    }
}

/// The marks of the parts of an instance, if any has marks.
fn of_parts(instance: &Instance) -> Option<Marks> {
    parts(instance).filter_map(marks).reduce(Marks::add)
}

/// An instance's marks: its own, or else its parts'.
fn marks(instance: &Instance) -> Option<Marks> {
    own(instance).or_else(|| of_parts(instance))
}

/// The marks of the paper: of each question and part that no other holds,
/// its own marks or else its parts'.
pub fn total(view: &InstanceTree) -> Value {
    view.roots
        .iter()
        .flat_map(scorable)
        .filter_map(marks)
        .fold(Marks::ZERO, Marks::add)
        .value()
}

/// The exam's lints:
///
/// - `exam.marks-sum`: a question or part whose own marks differ from the
///   sum of its parts' marks, where any of its parts has marks.
/// - `exam.answer-lines`: an answer with more ruled lines than the
///   component sets ([`MAX_LINES`]), or fewer than one.
pub fn lint(view: &InstanceTree) -> EcoVec<LintDiagnostic> {
    let mut out = EcoVec::new();
    for instance in view.iter() {
        if is(instance, QUESTION) || is(instance, PART) {
            marks_sum(instance, &mut out);
        } else if is(instance, ANSWER) {
            lines(instance, &mut out);
        }
    }
    out
}

fn marks_sum(instance: &Instance, out: &mut EcoVec<LintDiagnostic>) {
    let (Some(given), Some(sum)) = (own(instance), of_parts(instance)) else {
        return;
    };
    if given.same(sum) {
        return;
    }
    let what = instance.name.name.as_str();
    let mut diagnostic = LintDiagnostic::warning(
        name("marks-sum"),
        instance.location,
        eco_format!(
            "the {what} has {} {}, but its parts have {} {}",
            given.text(),
            plural(given),
            sum.text(),
            plural(sum),
        ),
    )
    .with_field(name(MARKS))
    .with_hint(eco_format!(
        "set `marks: {}`, or leave `marks` out to count the parts' marks",
        sum.text()
    ));
    for part in parts(instance) {
        let message = match marks(part) {
            Some(m) => eco_format!("this part has {} {}", m.text(), plural(m)),
            None => "this part has no marks".into(),
        };
        diagnostic = diagnostic.with_related(part.location, message);
    }
    out.push(diagnostic);
}

fn lines(instance: &Instance, out: &mut EcoVec<LintDiagnostic>) {
    let Some(Value::Int(n)) = instance.field(LINES) else {
        return;
    };
    if (1..=MAX_LINES).contains(n) {
        return;
    }
    let message = if *n < 1 {
        eco_format!("an answer cannot have {n} lines, so it has none")
    } else {
        eco_format!("an answer has at most {MAX_LINES} lines, so this one has none")
    };
    out.push(
        LintDiagnostic::warning(name("answer-lines"), instance.location, message)
            .with_field(name(LINES))
            .with_hint(
                "give fewer lines, or set the answer space's height in a stylesheet \
                 with `answer-height`",
            ),
    );
}

fn plural(marks: Marks) -> &'static str {
    if marks.same(Marks::Int(1)) {
        "mark"
    } else {
        "marks"
    }
}
