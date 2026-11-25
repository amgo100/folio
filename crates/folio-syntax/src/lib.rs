// Derived from typst-syntax v0.15.1, crates/typst-syntax/src/lib.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Lexer, parser and lossless concrete syntax tree for Folio content (`.fol`)
//! and stylesheets (`.fsty`), with one expression grammar shared by scripts,
//! stylesheet values and render templates (SPEC §2.1, §2.2).
//!
//! The crate started as a fork of `typst-syntax` v0.15.1 (Apache-2.0); files
//! derived from Typst keep an attribution header (see `NOTICE` at the
//! repository root).
//!
//! - [`parse`] turns content markup (SPEC §4.2) into a lossless,
//!   error-tolerant concrete syntax tree of [`SyntaxNode`]s, and
//!   [`parse_stylesheet`] does the same for stylesheets (SPEC §5.1). Printing
//!   the tree ([`SyntaxNode::full_text`]) gives back the source byte for
//!   byte, and a broken construct becomes an error node while the rest of the
//!   file still parses.
//! - [`ast`] is a typed view over that tree.
//! - [`Source`] holds one file's text and tree and reparses only the smallest
//!   enclosing block after an edit, reusing untouched nodes (SPEC §2.3).
//!   [`Source::stylesheet`] makes a source that is reparsed as a stylesheet.
//! - [`NodeIds`] gives nodes an identity that lasts across edits, through a
//!   diff of the trees before and after (SPEC §12.4).
//!
//! Stylesheets follow design C of SPEC Appendix A: rule structure (selectors,
//! at-rules, blocks, property names) is fixed syntax, and expressions appear
//! only in property values and render templates. Values use the expression
//! grammar of scripts, where in addition:
//!
//! - `$accent` is a token, `#1f5fa8` a colour and `16:9` a ratio; `<` is
//!   always an operator, since stylesheets write labels as `#{fig:ball}`.
//! - An argument can be several items written one after another
//!   (`strong(term() " " counter())`), braces hold such items
//!   (`if field(title) { ": " field(title) }`), and an item can be guarded by
//!   a condition (`badge(..) if field(difficulty) == "extension"`).
//! - A sign after a space and directly before its operand starts a new item,
//!   as in CSS (`margin: 0 -1mm`); inside parentheses and conditions it is
//!   an operator.
//! - A name directly followed by an attribute test is a selector
//!   (`slide-break: heading[level=2], slide;`).
//! - Code and content blocks (`[..]`) hold script and markup as in content
//!   files, without the additions above.
//!
//! `render`, `slide-master` and `slide-layout` start a render template, a
//! slide master or a slide layout only when a name or a string follows, so
//! elements with those names can still be selected (`render.x { .. }`).
//!
//! Scoped rules in content (`#rule question:first { .. }`) use the same
//! selectors and declarations, with tokens and colours written as code
//! (`tok.accent`, `rgb("1f5fa8")`), and `name[..]` a call with a content
//! argument as elsewhere in content.
//!
//! Where Folio differs from Typst's markup:
//!
//! - Labels consist of letters, digits, `-`, `_` and `:`, start with a
//!   letter, digit or `_`, and do not end with `:` or `-`. They cannot contain
//!   `.`, which separates a label from a published value
//!   (`<timer:task>.remaining`). Trailing `:` and `-` after a reference are
//!   punctuation.
//! - A named argument can be limited to a variant or target:
//!   `override@markscheme: (..)` (SPEC §5.4).
//! - `#element` declares an element schema (SPEC §4.3), `#rule` is a scoped
//!   style rule (SPEC §5.4) and `$:tex ...$` is LaTeX math input (SPEC §8.1).
//!   `element` and `rule` are keywords only in those positions, so they stay
//!   available as names elsewhere (such as the `rule(...)` template primitive).
//! - Styling lives in stylesheets, so Typst's `set`, `show` and `context`
//!   are not part of the language; they are ordinary identifiers.
//! - Numbers also take the duration units `ms`, `s` and `min` (SPEC §10), and
//!   the language tag of raw text runs to the first space (` ```c++ `).

#![forbid(unsafe_code)]

pub mod ast;

mod file;
mod highlight;
mod identity;
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
pub use self::identity::{NodeId, NodeIds};
pub use self::kind::SyntaxKind;
pub use self::lexer::{
    is_id_continue, is_id_start, is_ident, is_newline, is_valid_label_literal_id, link_prefix,
    split_newlines,
};
pub use self::lines::Lines;
pub use self::node::{Diagnosis, LinkedChildren, LinkedNode, Side, SyntaxDiagnostic, SyntaxNode};
pub use self::parser::{parse, parse_code, parse_math, parse_stylesheet};
pub use self::source::Source;
pub use self::span::{
    DiagSpan, DiagSpanKind, RangeMapper, Span, SpanKind, SpanNumber, Spanned, SubRange,
};

use self::lexer::Lexer;
use self::parser::{reparse_block, reparse_markup, reparse_style_block, reparse_stylesheet_items};

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
