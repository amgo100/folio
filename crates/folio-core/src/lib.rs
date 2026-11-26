//! The compile stages from evaluation to layout (SPEC §2.1): evaluation,
//! model, style, the stages between style and layout (Select, Sequence,
//! Resolve and Step styling), fonts, text, math, drawing, layout and
//! animation.
//!
//! These start in one crate and move into their own `folio-*` crates as
//! their interfaces settle (SPEC §2.2). This crate depends on
//! `folio-component`, never the reverse, and sees components only through
//! its registry ([`component`]).
//!
//! Every stage is a query in the database of [`db`] (SPEC §2.3).

#![forbid(unsafe_code)]

pub mod component;
pub mod db;
pub mod diag;
pub mod eval;
pub mod fonts;
pub mod layout;
pub mod locate;
pub mod model;
pub mod pipeline;
pub mod project;
pub mod span_map;
pub mod style;
pub mod text;
