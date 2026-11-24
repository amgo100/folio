//! The compile stages from evaluation to layout (SPEC §2.1): evaluation,
//! model, style, text, fonts, math, drawing, layout and animation.
//!
//! These start in one crate and move into their own `folio-*` crates as
//! their interfaces settle (SPEC §2.2). This crate depends on
//! `folio-component`, never the reverse.

#![forbid(unsafe_code)]
