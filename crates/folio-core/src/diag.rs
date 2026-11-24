//! Diagnostics that the compile queries report (SPEC §2.3).
//!
//! A diagnostic points at its source through a [`DiagSpan`]: a span number in
//! a file, which stays the same when an edit elsewhere in the file moves the
//! text, so a query that reports one is equal across such edits and its
//! dependents are not re-run (early cutoff).

use std::fmt::{self, Display, Formatter};

use ecow::{EcoString, EcoVec};
use folio_syntax::{DiagSpan, SyntaxDiagnostic};

use crate::component::TemplateError;

/// How serious a diagnostic is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    /// The output may be wrong or missing.
    Error,
    /// The output is complete but probably not what was meant.
    Warning,
}

/// A problem found while compiling, with the source it is about.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// The source it is about; detached when it has none.
    pub span: DiagSpan,
    /// What is wrong.
    pub message: EcoString,
    /// How to fix it.
    pub hints: EcoVec<EcoString>,
}

impl Diagnostic {
    /// An error.
    pub fn error(span: impl Into<DiagSpan>, message: impl Into<EcoString>) -> Self {
        Self {
            severity: Severity::Error,
            span: span.into(),
            message: message.into(),
            hints: EcoVec::new(),
        }
    }

    /// Adds a hint.
    pub fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }

    /// Whether this is an error.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.severity {
            Severity::Error => f.write_str("error: ")?,
            Severity::Warning => f.write_str("warning: ")?,
        }
        f.write_str(&self.message)?;
        for hint in &self.hints {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl From<SyntaxDiagnostic> for Diagnostic {
    fn from(diag: SyntaxDiagnostic) -> Self {
        Self {
            severity: if diag.is_error {
                Severity::Error
            } else {
                Severity::Warning
            },
            span: diag.span,
            message: diag.message,
            hints: diag.hints.into_iter().map(|hint| hint.v).collect(),
        }
    }
}

/// A template that does not parse is a bug in its component, which has no
/// source in the project to point at.
impl From<TemplateError> for Diagnostic {
    fn from(error: TemplateError) -> Self {
        Self::error(DiagSpan::detached(), error.to_string())
    }
}
