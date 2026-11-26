//! Evaluation errors, and the other ways evaluation stops.

use ecow::{EcoString, EcoVec};
use folio_syntax::{DiagSpan, Span, SubRange};

use crate::diag::Diagnostic;

/// An error that stopped evaluation, with the calls it happened in.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct EvalError {
    /// Where it happened.
    pub span: Span,
    /// The part of the node at `span` it is about, if not all of it: the
    /// command of a LaTeX formula, whose source is one node.
    pub sub_range: Option<SubRange>,
    /// What is wrong.
    pub message: EcoString,
    /// How to fix it.
    pub hints: EcoVec<EcoString>,
    /// The calls and imports it happened in, innermost first.
    pub trace: EcoVec<Tracepoint>,
}

impl EvalError {
    /// An error without hints or trace.
    pub fn new(span: Span, message: impl Into<EcoString>) -> Self {
        Self {
            span,
            sub_range: None,
            message: message.into(),
            hints: EcoVec::new(),
            trace: EcoVec::new(),
        }
    }

    /// Whether it or a call in its trace has a detached span: one that
    /// points nowhere, like the spans of arguments that a call's key
    /// leaves out.
    pub(crate) fn has_detached_span(&self) -> bool {
        self.span.is_detached() || self.trace.iter().any(|point| point.span.is_detached())
    }

    /// Narrows the error to a part of the node at its span.
    pub fn with_sub_range(mut self, sub_range: Option<SubRange>) -> Self {
        self.sub_range = sub_range;
        self
    }

    /// Adds a hint.
    pub fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }

    /// Adds a call or import that the error happened in, unless the trace
    /// has it already, as it has for every level of a recursion.
    pub(crate) fn trace_point(&mut self, span: Span, message: EcoString) {
        let point = Tracepoint { span, message };
        if !self.trace.contains(&point) {
            self.trace.push(point);
        }
    }
}

/// A call or import that an error happened in.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Tracepoint {
    /// The call or import.
    pub span: Span,
    /// What it is: "in this call of `f`".
    pub message: EcoString,
}

/// The trace becomes related spans, which front ends show next to the
/// error.
impl From<EvalError> for Diagnostic {
    fn from(error: EvalError) -> Self {
        let mut diag = Diagnostic::error(
            DiagSpan::from_span(error.span, error.sub_range),
            error.message,
        );
        for hint in error.hints {
            diag = diag.with_hint(hint);
        }
        for point in error.trace {
            diag = diag.with_related(point.span, point.message);
        }
        diag
    }
}

/// Why evaluation stopped before producing a value.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) enum Stop {
    /// An error.
    Error(Box<EvalError>),
    /// A problem that another query has reported already, such as syntax
    /// errors in the file.
    Reported,
    /// The code read `variant` while being evaluated for all variants at
    /// once, so it must be evaluated per variant (SPEC §2.1).
    NeedsVariant,
}

impl From<EvalError> for Stop {
    fn from(error: EvalError) -> Self {
        Self::Error(Box::new(error))
    }
}

/// The result of evaluating something.
pub(crate) type EvalResult<T> = Result<T, Stop>;

/// The result of an operation that does not know where it is: its caller
/// adds the span with [`At::at`].
pub(crate) type StrResult<T> = Result<T, EcoString>;

/// Adds a span to an error that has none.
pub(crate) trait At<T> {
    /// The result, with an error at `span`.
    fn at(self, span: Span) -> EvalResult<T>;
}

impl<T> At<T> for StrResult<T> {
    fn at(self, span: Span) -> EvalResult<T> {
        self.map_err(|message| EvalError::new(span, message).into())
    }
}

/// Returns an error from the enclosing function: `bail!(span, "..")`.
macro_rules! bail {
    ($span:expr, $($fmt:tt)*) => {
        return Err($crate::eval::error::EvalError::new(
            $span,
            ::ecow::eco_format!($($fmt)*),
        ).into())
    };
}

pub(crate) use bail;
