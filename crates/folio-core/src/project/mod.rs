//! The project manifest (`folio.toml`) and lockfile (`folio.lock`), read into
//! typed models (SPEC §2.4, §3.6, §4.1, §5.5, §6.1, §6.4).
//!
//! [`Manifest::parse`] and [`Lockfile::parse`] read the TOML text, check every
//! key and value, and return either the model or every error found, each with
//! a span on the offending key or value. Unknown keys are errors, with a
//! suggestion when a known key is close. Values keep their spans, so later
//! stages can point back into the manifest (a missing entry file, a locked
//! version that is not available).
//!
//! Both models are salsa inputs ([`ManifestInput`], [`LockfileInput`]): the
//! host parses the files and sets the results, and queries that read them
//! re-run only when they change (SPEC §2.3).

mod de;
mod inputs;
mod lock;
mod manifest;
mod values;

use std::fmt::{self, Display, Formatter};

use ecow::{EcoString, EcoVec};
use folio_syntax::DiagSpan;

pub use self::inputs::{LockfileInput, ManifestInput};
pub use self::lock::{Binding, Bindings, LockedFont, LockedPackage, Lockfile};
pub use self::manifest::{
    ComponentDecl, ComponentSource, Components, Formatting, FormattingMode, Manifest,
    NumberingGroup, Output, OutputPattern, Project, Variant,
};
pub use self::values::{
    ComponentName, Date, Edition, FIRST_PARTY_COMPONENTS, Lang, Name, PathPattern, ProjectPath,
    QualifiedName, RelativePath, Sha256, Target, is_first_party,
};

/// A value read from `folio.toml` or `folio.lock`, with the span of its
/// source text. Values filled in by default have a detached span.
pub type Spanned<T> = folio_syntax::Spanned<T, DiagSpan>;

/// An error in `folio.toml` or `folio.lock`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ProjectError {
    /// The key or value the error is about. A missing key points at the
    /// header of its table, or at the start of the file.
    pub span: DiagSpan,
    /// What is wrong.
    pub message: EcoString,
    /// How to fix it.
    pub hints: EcoVec<EcoString>,
}

impl ProjectError {
    fn hint(&mut self, hint: impl Into<EcoString>) -> &mut Self {
        self.hints.push(hint.into());
        self
    }
}

impl Display for ProjectError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        for hint in &self.hints {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProjectError {}
