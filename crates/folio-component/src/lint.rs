//! Lint diagnostics: what a component's lints report about its element
//! instances (SPEC §3.2).

use ecow::{EcoString, EcoVec};

use crate::names::Name;
use crate::text::Location;

/// A problem a component's lint finds in its element instances.
///
/// It is plain data and points at instances by [`Location`]; the core
/// turns locations into source spans and shows the diagnostic like its own,
/// with the code qualified by the component's namespace (`exam.marks-sum`).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct LintDiagnostic {
    /// The lint's code within the component's namespace: `marks-sum`.
    pub code: Name,
    /// How serious it is.
    pub severity: Severity,
    /// What is wrong, as one sentence.
    pub message: EcoString,
    /// The instance it concerns.
    pub at: Location,
    /// The field of the instance it concerns, if one.
    pub field: Option<Name>,
    /// Other instances that explain it, each with a message: the parts
    /// whose marks do not add up.
    pub related: EcoVec<Related>,
    /// Suggestions for a fix.
    pub hints: EcoVec<EcoString>,
}

impl LintDiagnostic {
    /// A warning about an instance.
    pub fn warning(code: Name, at: Location, message: impl Into<EcoString>) -> Self {
        Self::new(Severity::Warning, code, at, message)
    }

    /// An error about an instance.
    pub fn error(code: Name, at: Location, message: impl Into<EcoString>) -> Self {
        Self::new(Severity::Error, code, at, message)
    }

    fn new(severity: Severity, code: Name, at: Location, message: impl Into<EcoString>) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            at,
            field: None,
            related: EcoVec::new(),
            hints: EcoVec::new(),
        }
    }

    /// The diagnostic, pointing at a field of its instance.
    pub fn with_field(mut self, field: Name) -> Self {
        self.field = Some(field);
        self
    }

    /// The diagnostic, with another instance that explains it.
    pub fn with_related(mut self, at: Location, message: impl Into<EcoString>) -> Self {
        self.related.push(Related {
            at,
            message: message.into(),
        });
        self
    }

    /// The diagnostic, with a suggestion for a fix.
    pub fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }
}

/// How serious a lint diagnostic is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Severity {
    /// Something the author should look at; the build is unaffected.
    Warning,
    /// Something wrong in the document, shown as an error.
    Error,
}

/// An instance that helps explain a lint diagnostic.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct Related {
    /// The instance.
    pub at: Location,
    /// What it has to do with the diagnostic.
    pub message: EcoString,
}
