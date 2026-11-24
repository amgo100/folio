//! Text for Folio (SPEC §7.2): itemisation, bidi, shaping, line-break
//! opportunities, hyphenation and the typographic rules of languages.
//!
//! A [`Paragraph`] is the text of one paragraph with [`Segment`]s that give
//! every byte a font (chosen by fallback in the font stage), a language and
//! OpenType features. [`shape`] turns it into a [`Shaped`] paragraph:
//!
//! - **Bidi** (UAX #9, `unicode-bidi`): the paragraph runs in the direction
//!   of its first strongly directional character, or of its language when
//!   it has none, and every character gets an embedding level.
//!   [`Shaped::visual_runs`] gives the visual order of the runs of a line.
//! - **Itemisation**: the text is cut into [`Item`]s of one segment, one
//!   level and one script ([`script_runs`], UAX #24).
//! - **Shaping** (`harfrust`): each item is shaped in its font with its
//!   direction, script, language and features; the text around it is the
//!   shaper's context. Glyphs are in font units, and each records whether
//!   a line may start or end before it without shaping again.
//! - **Break opportunities** (UAX #14, `icu_segmenter`, with models for the
//!   scripts written without spaces), and hyphenation points by language
//!   (`hypher`), as [`Break`]s.
//! - **Language rules** ([`Lang`]): quotation marks; in Polish, a word of
//!   one letter (a, i, o, u, w, z and their capitals) never ends a line,
//!   and a line that breaks at an explicit hyphen repeats it on the next.
//!
//! A [`Cache`] memoises shaped paragraphs and shapes new ones in parallel
//! with `rayon`.
//!
//! This is the `folio-text` crate of SPEC §2.2. It depends on no other
//! Folio crate, so that `folio-component` may depend on it: fonts come in as
//! bytes ([`TextFont`]).

#![forbid(unsafe_code)]

mod bidi;
mod breaks;
mod cache;
mod lang;
mod paragraph;
mod script;
mod shape;

pub use self::breaks::{Break, BreakKind};
pub use self::cache::Cache;
pub use self::lang::{Dir, Hyphenation, Lang, Quotes};
pub use self::paragraph::{
    Item, ItemKind, OBJECT, Paragraph, Segment, SegmentKind, Shaped, mark_breaks, shape,
};
pub use self::script::{Script, script_runs};
pub use self::shape::{Feature, Glyph, TextFont};
