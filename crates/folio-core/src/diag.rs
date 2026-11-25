//! Diagnostics: the one type every stage reports problems with, and every
//! front end shows (SPEC §4.3, §7.9, §12.1, §12.3).
//!
//! A [`Diagnostic`] has a severity, an optional lint code (such as
//! `override-literal`), a message, a primary span, related spans with their
//! own messages ([`Related`]), hints, and quick fixes ([`Fix`]) that a
//! language server or the editor can apply.
//!
//! Spans are [`DiagSpan`]s: a span number in a file, which stays the same
//! when an edit elsewhere in the file moves the text, so a query that reports
//! one is equal across such edits and its dependents are not re-run (early
//! cutoff). A front end resolves them to paths, lines and columns only when it
//! shows them.
//!
//! # Collection
//!
//! Queries do not return their diagnostics: they [`report`] them, and a
//! front end collects those of a query and of every query it depends on with
//! the query's generated `accumulated::<Reported>` function, outside queries
//! (`folio::compile` does this for a build). Diagnostics are salsa
//! accumulator values, stored with the memo of the query that reported
//! them, so a query that re-runs replaces its own diagnostics, and one that
//! does not re-run keeps them. Because they are not part of a query's
//! return value, a query whose result is unchanged after an edit is cut off
//! even when its diagnostics changed, and collecting still sees the new
//! ones.

use std::fmt::{self, Display, Formatter};

use ecow::{EcoString, EcoVec};
use folio_syntax::{DiagSpan, SyntaxDiagnostic};

use crate::component::TemplateError;
use crate::db::Db;
use crate::project::ProjectError;

/// How serious a diagnostic is.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    /// The output may be wrong or missing.
    Error,
    /// The output is complete but probably not what was meant.
    Warning,
}

impl Severity {
    /// The severity's name in lower case: `error` or `warning`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// A problem found while compiling, with the source it is about.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// The name of the lint or check that reported it, such as
    /// `override-literal`, when it can be named, for example to be allowed
    /// or denied.
    pub code: Option<EcoString>,
    /// The source it is about; detached when it has none.
    pub span: DiagSpan,
    /// What is wrong.
    pub message: EcoString,
    /// Other source that explains it, each with a message.
    pub related: EcoVec<Related>,
    /// How to fix it.
    pub hints: EcoVec<EcoString>,
    /// Edits that fix it.
    pub fixes: EcoVec<Fix>,
}

impl Diagnostic {
    /// A diagnostic with no code, related spans, hints or fixes.
    pub fn new(
        severity: Severity,
        span: impl Into<DiagSpan>,
        message: impl Into<EcoString>,
    ) -> Self {
        Self {
            severity,
            code: None,
            span: span.into(),
            message: message.into(),
            related: EcoVec::new(),
            hints: EcoVec::new(),
            fixes: EcoVec::new(),
        }
    }

    /// An error.
    pub fn error(span: impl Into<DiagSpan>, message: impl Into<EcoString>) -> Self {
        Self::new(Severity::Error, span, message)
    }

    /// A warning.
    pub fn warning(span: impl Into<DiagSpan>, message: impl Into<EcoString>) -> Self {
        Self::new(Severity::Warning, span, message)
    }

    /// Sets the code.
    pub fn with_code(mut self, code: impl Into<EcoString>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Adds a related span with a message.
    pub fn with_related(
        mut self,
        span: impl Into<DiagSpan>,
        message: impl Into<EcoString>,
    ) -> Self {
        self.related.push(Related {
            span: span.into(),
            message: message.into(),
        });
        self
    }

    /// Adds a hint.
    pub fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }

    /// Adds a fix.
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }

    /// Whether this is an error.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// The diagnostic's severity, code and message, then its hints, one per
/// line. Spans are not shown: they need the files to resolve.
impl Display for Diagnostic {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.severity.name())?;
        if let Some(code) = &self.code {
            write!(f, "[{code}]")?;
        }
        write!(f, ": {}", self.message)?;
        for hint in &self.hints {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

/// A related span of a diagnostic: source that explains it, such as the
/// declaration of a field whose value has the wrong type. Terminals show it
/// as a secondary label of the excerpt; a language server, as related
/// information.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Related {
    /// The source.
    pub span: DiagSpan,
    /// What it has to do with the diagnostic.
    pub message: EcoString,
}

/// A quick fix: edits that, applied together, fix a diagnostic.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Fix {
    /// What the fix does, as an editor offers it: "Use the token
    /// `tok.accent`".
    pub title: EcoString,
    /// The edits, in any order. They do not overlap.
    pub edits: EcoVec<Edit>,
}

impl Fix {
    /// A fix without edits yet.
    pub fn new(title: impl Into<EcoString>) -> Self {
        Self {
            title: title.into(),
            edits: EcoVec::new(),
        }
    }

    /// A fix that replaces the source of one span.
    pub fn replace(
        title: impl Into<EcoString>,
        span: impl Into<DiagSpan>,
        with: impl Into<EcoString>,
    ) -> Self {
        Self::new(title).with_edit(span, with)
    }

    /// Adds an edit that replaces the source of a span. An empty
    /// replacement deletes it; an empty span (from a byte range) inserts.
    pub fn with_edit(mut self, span: impl Into<DiagSpan>, with: impl Into<EcoString>) -> Self {
        self.edits.push(Edit {
            span: span.into(),
            replacement: with.into(),
        });
        self
    }
}

/// One edit of a [`Fix`].
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Edit {
    /// The source to replace.
    pub span: DiagSpan,
    /// What to replace it with.
    pub replacement: EcoString,
}

/// Hints that point at part of the source become related spans, shown where
/// they apply; the others stay hints.
impl From<SyntaxDiagnostic> for Diagnostic {
    fn from(diag: SyntaxDiagnostic) -> Self {
        let severity = if diag.is_error {
            Severity::Error
        } else {
            Severity::Warning
        };
        let mut out = Self::new(severity, diag.span, diag.message);
        for hint in diag.hints {
            out = if hint.span.is_detached() {
                out.with_hint(hint.v)
            } else {
                out.with_related(hint.span, hint.v)
            };
        }
        out
    }
}

/// An error in `folio.toml` or `folio.lock`.
impl From<ProjectError> for Diagnostic {
    fn from(error: ProjectError) -> Self {
        Self {
            hints: error.hints,
            ..Self::error(error.span, error.message)
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

/// A diagnostic as reported by a query: the salsa accumulator that
/// [`report`] adds to.
#[salsa::accumulator]
#[derive(Debug)]
pub struct Reported(pub Diagnostic);

/// Reports a diagnostic from the query that is running.
///
/// # Panics
///
/// Panics when no query is running.
pub fn report(db: &dyn Db, diagnostic: Diagnostic) {
    use salsa::Accumulator;
    Reported(diagnostic).accumulate(db);
}
