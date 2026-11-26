//! The component interface of Folio (SPEC §3): the [`Component`] trait, the
//! component [`Registry`] and manifests, the declarations of the element,
//! property and template extension points, and the interface types shared
//! with layout and rendering (glyph runs, graphic primitives, [`Fragment`],
//! [`Regions`], [`Abs`] and other units).
//!
//! Components reach the compiler only through this crate, so it depends on no
//! other part of the core; `tests/dependency_direction.rs` enforces that. The
//! interface is defined in WebAssembly-compatible terms (SPEC §3.4): plain data
//! in and out, with no references into core internals, so a WebAssembly tier
//! can later serve the same interface.
//!
//! # Extension points
//!
//! This version of the interface has the first three extension points of SPEC
//! §3.2, which are all declarative:
//!
//! - **Elements**: [`ElementSchema`]s with fields, kind, body slots and a
//!   counter, and the [`LocaleString`]s their generated words use.
//! - **Properties**: [`PropertyDecl`]s with a type, an initial value,
//!   inheritance, and whether they affect layout or only paint.
//! - **Templates**: default and named render templates ([`TemplateDecl`]),
//!   written in the stylesheet language.
//!
//! A component's elements and properties live in its namespace
//! ([`ComponentName::namespace`]), so they can always be written qualified
//! (`exam.question`); how unqualified names resolve is decided by the core
//! from the project's lockfile (SPEC §3.6).

#![forbid(unsafe_code)]

mod component;
mod draw;
mod element;
mod fragment;
mod geom;
mod names;
mod property;
mod registry;
mod template;
mod text;
mod value;

pub use self::component::{
    Component, ComponentManifest, Declarations, INTERFACE_VERSION, SUPPORTED_INTERFACES,
};
pub use self::draw::{
    Color, Dash, FillRule, LineCap, LineJoin, Paint, Path, PathItem, Shape, Stroke,
};
pub use self::element::{
    BODY, Body, ContentKind, CounterDecl, ElementKind, ElementSchema, FieldDecl, LAYOUT_HINTS,
    LocaleString, SlotDecl, UNIVERSAL_FIELDS, Within,
};
pub use self::fragment::{BaselineGrid, Fragment, FragmentItem, ObjectBox, Regions};
pub use self::geom::{Abs, Em, Point, Ratio, Rect, Scalar, Size};
pub use self::names::{ComponentName, Name, QualifiedName, Target, is_ident};
pub use self::property::{Effect, PropertyDecl};
pub use self::registry::{Registered, Registry, RegistryError};
pub use self::template::TemplateDecl;
pub use self::text::{FontId, Glyph, GlyphRun, Location, SourceRef};
pub use self::value::{FieldType, PropertyType, Value};
