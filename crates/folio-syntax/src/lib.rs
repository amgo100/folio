// Derived from typst-syntax v0.15.1, crates/typst-syntax/src/lib.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Lexer, parser and lossless concrete syntax tree for Folio content (`.fol`)
//! and stylesheets (`.fsty`), with one expression grammar shared by scripts,
//! stylesheet values and render templates (SPEC §2.1, §2.2).
//!
//! The crate started as a fork of `typst-syntax` v0.15.1 (Apache-2.0); files
//! derived from Typst keep an attribution header (see `NOTICE` at the
//! repository root). It currently parses content markup (SPEC §4.2):
//!
//! - [`parse`] turns source text into a lossless, error-tolerant concrete
//!   syntax tree of [`SyntaxNode`]s. Printing the tree gives back the source
//!   byte for byte, and a broken construct becomes an error node while the rest
//!   of the file still parses.
//! - [`ast`] is a typed view over that tree.
//! - [`Source`] holds one file's text and tree and reparses only the smallest
//!   enclosing block after an edit, reusing untouched nodes (SPEC §2.3).
//!
//! Where Folio differs from Typst's markup:
//!
//! - Labels consist of letters, digits, `-`, `_` and `:`, and do not end with
//!   `:` or `-`. They cannot contain `.`, which separates a label from a
//!   published value (`<timer:task>.remaining`). Trailing `:` and `-` after a
//!   reference are punctuation.
//! - `#element` declares an element schema (SPEC §4.3), `#rule` is a scoped
//!   style rule (SPEC §5.4) and `$:tex ...$` is LaTeX math input (SPEC §8.1).
//!   `element` and `rule` are keywords only in those positions, so they stay
//!   available as names elsewhere (such as the `rule(...)` template primitive).
//! - Styling lives in stylesheets, so Typst's `set`, `show` and `context`
//!   are not part of the language; they are ordinary identifiers.
//! - Numbers also take the duration units `ms`, `s` and `min` (SPEC §10).

#![forbid(unsafe_code)]

pub mod ast;

mod file;
mod highlight;
mod kind;
mod lexer;
mod lines;
mod node;
mod parser;
mod reparser;
mod set;
mod source;
mod span;
mod utils;

pub use self::file::FileId;
pub use self::highlight::{Tag, highlight, highlight_html};
pub use self::kind::SyntaxKind;
pub use self::lexer::{
    is_id_continue, is_id_start, is_ident, is_newline, is_valid_label_literal_id, link_prefix,
    split_newlines,
};
pub use self::lines::Lines;
pub use self::node::{Diagnosis, LinkedChildren, LinkedNode, Side, SyntaxDiagnostic, SyntaxNode};
pub use self::parser::{parse, parse_code, parse_math};
pub use self::source::Source;
pub use self::span::{
    DiagSpan, DiagSpanKind, RangeMapper, Span, SpanKind, SpanNumber, Spanned, SubRange,
};

use self::lexer::Lexer;
use self::parser::{reparse_block, reparse_markup};

/// The syntax mode of a portion of Folio source.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum SyntaxMode {
    /// Text and markup, as in the top level.
    Markup,
    /// Math atoms, operators, etc., as in equations.
    Math,
    /// Keywords, literals and operators, as after hashes.
    Code,
}
