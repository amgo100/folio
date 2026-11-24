//! The component interface of Folio (SPEC §3): extension-point traits, the
//! component registry and manifests, and the interface types shared with
//! layout and rendering (glyph runs, graphic primitives, `Fragment`,
//! `Regions`).
//!
//! Components reach the compiler only through this crate, so it depends on no
//! other part of the core. `tests/dependency_direction.rs` enforces that.

#![forbid(unsafe_code)]
