//! Bidirectional text (UAX #9): embedding levels for a paragraph, and the
//! visual order of the runs of a line.

use std::ops::Range;

use unicode_bidi::{BidiClass, Direction, Level, ParagraphBidiInfo, bidi_class};

use crate::lang::Dir;

/// The embedding levels of a paragraph: its own level and one per byte of
/// its text. The paragraph's direction is that of its first strongly
/// directional character (rules P2 and P3), or `fallback` when it has
/// none.
pub(crate) fn levels(text: &str, fallback: Dir) -> (u8, Vec<u8>) {
    let dir = match unicode_bidi::get_base_direction(text) {
        Direction::Ltr => Dir::Ltr,
        Direction::Rtl => Dir::Rtl,
        Direction::Mixed => fallback,
    };
    let base = match dir {
        Dir::Ltr => Level::ltr(),
        Dir::Rtl => Level::rtl(),
    };
    // Text without right-to-left characters in a left-to-right paragraph
    // is all at level 0.
    if dir == Dir::Ltr && !text.chars().any(is_rtl_or_explicit) {
        return (0, vec![0; text.len()]);
    }
    let info = ParagraphBidiInfo::new(text, Some(base));
    let levels = info.levels.iter().map(|l| l.number()).collect();
    (base.number(), levels)
}

/// Whether a character can raise an embedding level in a left-to-right
/// paragraph: a right-to-left letter or number, or an explicit
/// formatting character.
fn is_rtl_or_explicit(c: char) -> bool {
    use BidiClass::*;
    matches!(
        bidi_class(c),
        R | AL | AN | RLE | RLO | RLI | LRE | LRO | LRI | FSI | PDF | PDI
    )
}

/// The runs of one line in visual order, left to right, each with its
/// resolved level.
///
/// `runs` are the paragraph's runs in logical order with their levels;
/// those that overlap `line` are cut to it. Whitespace at the end of the
/// line, and before a tab, takes the paragraph's level (rule L1); then
/// every sequence of runs at a level of at least each odd level is
/// reversed, from the highest level down (rule L2). The text within a run
/// keeps its logical order: shaping already put the glyphs of a
/// right-to-left run in visual order.
pub(crate) fn reorder(
    text: &str,
    base: u8,
    runs: impl IntoIterator<Item = (Range<usize>, u8)>,
    line: Range<usize>,
) -> Vec<(Range<usize>, u8)> {
    let reset = reset_ranges(text, line.clone());
    let mut logical: Vec<(Range<usize>, u8)> = Vec::new();
    for (range, level) in runs {
        let start = range.start.max(line.start);
        let end = range.end.min(line.end);
        if start >= end {
            continue;
        }
        // Split the run where rule L1 resets the level.
        let mut pos = start;
        for r in &reset {
            if r.end <= pos || r.start >= end {
                continue;
            }
            let (a, b) = (r.start.max(pos), r.end.min(end));
            if pos < a {
                logical.push((pos..a, level));
            }
            logical.push((a..b, base));
            pos = b;
        }
        if pos < end {
            logical.push((pos..end, level));
        }
    }

    let Some(max) = logical.iter().map(|(_, l)| *l).max() else {
        return logical;
    };
    let Some(min_odd) = logical.iter().map(|(_, l)| *l).filter(|l| l % 2 == 1).min() else {
        return logical;
    };
    for level in (min_odd..=max).rev() {
        let mut i = 0;
        while i < logical.len() {
            if logical[i].1 < level {
                i += 1;
                continue;
            }
            let start = i;
            while i < logical.len() && logical[i].1 >= level {
                i += 1;
            }
            logical[start..i].reverse();
        }
    }
    logical
}

/// The byte ranges of a line whose level rule L1 resets to the paragraph's:
/// segment separators (tabs), the whitespace and isolate formatters before
/// them, and those at the end of the line.
fn reset_ranges(text: &str, line: Range<usize>) -> Vec<Range<usize>> {
    use BidiClass::*;
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut pending: Option<usize> = None;
    for (i, c) in text[line.clone()].char_indices() {
        let i = line.start + i;
        match bidi_class(c) {
            WS | FSI | LRI | RLI | PDI | BN => {
                pending.get_or_insert(i);
            }
            S | B => {
                let start = pending.take().unwrap_or(i);
                out.push(start..i + c.len_utf8());
            }
            _ => pending = None,
        }
    }
    if let Some(start) = pending {
        out.push(start..line.end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_to_right_text_is_level_zero() {
        let (base, levels) = levels("Hello, world", Dir::Rtl);
        assert_eq!(base, 0);
        assert!(levels.iter().all(|&l| l == 0));
    }

    #[test]
    fn neutral_text_takes_the_fallback() {
        assert_eq!(levels("123", Dir::Rtl).0, 1);
        assert_eq!(levels("123", Dir::Ltr).0, 0);
    }

    #[test]
    fn embeds_right_to_left_text() {
        let text = "ab שלום cd";
        let (base, levels) = levels(text, Dir::Ltr);
        assert_eq!(base, 0);
        let hebrew = text.find('ש').unwrap();
        assert_eq!(levels[0], 0);
        assert_eq!(levels[hebrew], 1);
    }

    #[test]
    fn reorders_a_line() {
        let text = "ab XY cd";
        let runs = [(0..3, 0), (3..5, 1), (5..8, 0)];
        let visual = reorder(text, 0, runs, 0..8);
        assert_eq!(visual, [(0..3, 0), (3..5, 1), (5..8, 0)]);
        // Nested: an LTR run inside an RTL paragraph keeps its place
        // between the reversed RTL runs.
        let runs = [(0..2, 1), (2..4, 2), (4..6, 1)];
        let visual = reorder("aabbcc", 1, runs, 0..6);
        assert_eq!(visual, [(4..6, 1), (2..4, 2), (0..2, 1)]);
    }

    #[test]
    fn trailing_space_takes_the_paragraph_level() {
        // An RTL run ending in a space, at the end of an LTR line.
        let visual = reorder("ab XY ", 0, [(0..3, 0), (3..6, 1)], 0..6);
        assert_eq!(visual, [(0..3, 0), (3..5, 1), (5..6, 0)]);
    }
}
