//! Lexer, parser and lossless concrete syntax tree for Folio content (`.fol`)
//! and stylesheets (`.fsty`), with one expression grammar shared by scripts,
//! stylesheet values and render templates (SPEC §2.1, §2.2).
//!
//! This crate will start as a fork of `typst-syntax` (Apache-2.0). Files
//! derived from Typst keep an attribution header; see `NOTICE` at the
//! repository root.

#![forbid(unsafe_code)]
