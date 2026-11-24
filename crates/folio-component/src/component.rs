//! Components and their manifests (SPEC §3, §3.5, §3.6).

use ecow::EcoVec;

use crate::element::{ElementSchema, LocaleString};
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

    /// What the component adds through the element, property and template
    /// extension points.
    fn declarations(&self) -> Declarations;
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

/// What a component adds through the element, property and template
/// extension points (SPEC §3.2).
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
}
