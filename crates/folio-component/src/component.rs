//! Components and their manifests (SPEC §3, §3.5, §3.6).

use ecow::{EcoString, EcoVec};

use crate::element::{ElementSchema, LocaleString};
use crate::instance::InstanceTree;
use crate::lint::LintDiagnostic;
use crate::names::{ComponentName, Name, Target};
use crate::property::PropertyDecl;
use crate::template::TemplateDecl;

/// The version of the component interface that this crate defines. A
/// component declares the version it targets (`folio-component = "1"`); the
/// interface stays stable across the compiler versions that support it
/// (SPEC §3.5).
pub const INTERFACE_VERSION: u32 = 1;

/// The interface versions this compiler can host.
pub const SUPPORTED_INTERFACES: &[u32] = &[INTERFACE_VERSION];

/// A component: a versioned unit that adds elements, properties and
/// templates through the extension points (SPEC §3).
///
/// Everything a component hands over is plain data with no references into
/// the core, so the same interface can later be served by a WebAssembly
/// component (SPEC §3.4). Calls must be pure and deterministic (SPEC §3.5):
/// the core may call them any number of times and caches the results.
pub trait Component: Send + Sync {
    /// The component's name, version, interface version and targets.
    fn manifest(&self) -> ComponentManifest;

    /// What the component adds through the element, property, default
    /// stylesheet and template extension points.
    fn declarations(&self) -> Declarations;

    /// The component's lints (SPEC §3.2): checks over the instances of its
    /// own elements in a build. The core calls it once per build and view,
    /// and shows what it returns like its own diagnostics. A component
    /// without lints keeps the default, which finds nothing.
    fn lint(&self, view: &InstanceTree) -> EcoVec<LintDiagnostic> {
        let _ = view;
        EcoVec::new()
    }
}

/// What identifies a component and what it supports.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ComponentManifest {
    /// The component's name: `exam`, `@school/house`. It also gives the
    /// namespace that qualifies its elements and properties
    /// ([`ComponentName::namespace`]).
    pub name: ComponentName,
    /// The component's own version. First-party components carry the
    /// compiler's version (SPEC §2.2).
    pub version: semver::Version,
    /// The interface version the component targets: [`INTERFACE_VERSION`].
    pub interface: u32,
    /// The targets the component maps its elements to. On any other target
    /// the core falls back to a generic mapping, then a placeholder with a
    /// warning; a missing mapping never fails a build (SPEC §3.5).
    pub targets: EcoVec<Target>,
}

impl ComponentManifest {
    /// The namespace of the component's elements and properties.
    pub fn namespace(&self) -> Name {
        self.name.namespace()
    }
}

/// What a component adds through the element, property, default stylesheet
/// and template extension points (SPEC §3.2).
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Declarations {
    /// Element schemas.
    pub elements: EcoVec<ElementSchema>,
    /// Stylesheet properties.
    pub properties: EcoVec<PropertyDecl>,
    /// Default and named render templates for the component's elements.
    pub templates: EcoVec<TemplateDecl>,
    /// Words the component's elements generate, per language.
    pub locale_strings: EcoVec<LocaleString>,
    /// The component's default stylesheet (SPEC §3.2, §5.1), as source text
    /// in the stylesheet language: the default look of its elements, with
    /// `@variant` and `@target` blocks where it varies.
    ///
    /// Its rules rank below every stylesheet of a project, `@variant` and
    /// `@target` blocks included. Unqualified names in it resolve to the
    /// component's own elements and properties first, then to the built-in
    /// ones, whatever the project's lockfile binds; another component's
    /// names must be qualified. It may not `@import`.
    pub stylesheet: Option<EcoString>,
}
