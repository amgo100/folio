//! Line-break opportunities (UAX #14), hyphenation points, and the line-end
//! rules of languages (SPEC §7.2).

use std::ops::Range;
use std::sync::LazyLock;

use icu_segmenter::LineSegmenter;
use icu_segmenter::options::LineBreakOptions;

use crate::lang::Lang;

/// A place where a line may end.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Break {
    /// The byte offset in the paragraph's text where the next line starts.
    pub offset: usize,
    /// What kind of break it is.
    pub kind: BreakKind,
}

/// The kind of a line-break opportunity.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum BreakKind {
    /// The line must end here: after a forced line break, and at the end of
    /// the paragraph.
    Mandatory,
    /// The line may end here: after a space, after a dash or explicit
    /// hyphen, or between the characters of scripts written without spaces.
    Normal,
    /// The line may end here inside a word, at a hyphenation point or a
    /// soft hyphen, with a hyphen drawn at its end.
    Hyphenate,
    /// The line may end here after an explicit hyphen, which the next line
    /// repeats at its start, as Polish does.
    RepeatHyphen,
}

/// The language and hyphenation setting of a range of a paragraph.
#[derive(Debug, Clone)]
pub(crate) struct LangSpan<'a> {
    pub range: Range<usize>,
    pub lang: Option<&'a Lang>,
    pub hyphenate: bool,
}

/// The UAX #14 segmenter, with the models of the scripts written without
/// spaces (Thai, Lao, Khmer, Burmese). Chinese and Japanese break between
/// ideographs by the rules themselves.
static SEGMENTER: LazyLock<icu_segmenter::LineSegmenterBorrowed<'static>> =
    LazyLock::new(|| LineSegmenter::new_auto(LineBreakOptions::default()));

/// The line-break opportunities of a paragraph, in order, ending with the
/// mandatory break at its end. Empty text has none.
///
/// `spans` give the language of every byte and whether it may be
/// hyphenated; text outside them has no language. A word is hyphenated
/// when its first letter's span allows it and its language has patterns,
/// unless the author placed soft hyphens in it. A space after a word that
/// its language binds to the next ([`Lang::binds_to_next`]) is not a
/// break.
pub(crate) fn breaks(text: &str, spans: &[LangSpan<'_>]) -> Vec<Break> {
    let lang_at = |offset: usize| {
        spans
            .iter()
            .find(|s| s.range.contains(&offset))
            .or_else(|| spans.last().filter(|s| s.range.end == offset))
    };
    let mut out: Vec<Break> = Vec::new();
    let mut prev = 0;
    for offset in SEGMENTER.segment_str(text) {
        if offset == 0 {
            continue;
        }
        let before = text[..offset].chars().next_back();
        let kind = match before {
            _ if offset == text.len() => BreakKind::Mandatory,
            Some('\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}') => {
                BreakKind::Mandatory
            }
            Some('\u{AD}') => BreakKind::Hyphenate,
            Some('-' | '\u{2010}')
                if lang_at(offset - 1)
                    .and_then(|s| s.lang)
                    .is_some_and(Lang::repeats_hyphen) =>
            {
                BreakKind::RepeatHyphen
            }
            _ => BreakKind::Normal,
        };

        // Hyphenation points of the word in this segment.
        let segment = prev..offset;
        hyphenate(text, segment, spans, &lang_at, &mut out);
        prev = offset;

        if kind == BreakKind::Normal && binds(text, offset, &lang_at) {
            continue;
        }
        out.push(Break { offset, kind });
    }
    out
}

/// Pushes the hyphenation points of the word in a segment between two
/// break opportunities: its letters, without the punctuation and spaces
/// around them.
fn hyphenate<'a>(
    text: &str,
    segment: Range<usize>,
    spans: &[LangSpan<'a>],
    lang_at: &impl Fn(usize) -> Option<&'a LangSpan<'a>>,
    out: &mut Vec<Break>,
) {
    if !spans.iter().any(|s| s.hyphenate) {
        return;
    }
    let piece = &text[segment.clone()];
    // The author's soft hyphens replace automatic hyphenation of the word.
    let after_soft = text[..segment.start].ends_with('\u{AD}');
    if after_soft || piece.contains('\u{AD}') {
        return;
    }
    let Some(first) = piece.find(char::is_alphabetic) else {
        return;
    };
    let rest = &piece[first..];
    let len = rest
        .char_indices()
        .find(|&(_, c)| !c.is_alphabetic() && !is_mark(c))
        .map_or(rest.len(), |(i, _)| i);
    let word = &rest[..len];
    // A word that continues past letters (`H2O`, `don't`) is left whole;
    // a closing quotation mark after it (`’`) does not continue it.
    let mut tail = rest[len..].chars();
    let continues = match tail.next() {
        Some('\'' | '’') => tail.next().is_some_and(char::is_alphabetic),
        Some(c) => c.is_alphanumeric(),
        None => false,
    };
    if continues {
        return;
    }
    let start = segment.start + first;
    let Some(span) = lang_at(start) else { return };
    if !span.hyphenate {
        return;
    }
    let Some(hyphenation) = span.lang.and_then(Lang::hyphenation) else {
        return;
    };
    for point in hyphenation.points(word) {
        out.push(Break {
            offset: start + point,
            kind: BreakKind::Hyphenate,
        });
    }
}

/// Whether the break at `offset`, after a space, follows a word that its
/// language binds to the next word.
fn binds<'a>(
    text: &str,
    offset: usize,
    lang_at: &impl Fn(usize) -> Option<&'a LangSpan<'a>>,
) -> bool {
    let head = &text[..offset];
    let word_end = head.trim_end_matches(|c: char| c.is_whitespace() && c != '\u{2028}');
    if word_end.len() == head.len() {
        return false;
    }
    let start = word_end
        .char_indices()
        .rev()
        .find(|&(_, c)| !c.is_alphanumeric() && !is_mark(c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &word_end[start..];
    if word.is_empty() {
        return false;
    }
    lang_at(start)
        .and_then(|s| s.lang)
        .is_some_and(|lang| lang.binds_to_next(word))
}

/// Whether a character is a combining mark, which belongs to the letter
/// before it.
fn is_mark(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, lang: &Lang, hyphenate: bool) -> Vec<(usize, BreakKind)> {
        let spans = [LangSpan {
            range: 0..text.len(),
            lang: Some(lang),
            hyphenate,
        }];
        breaks(text, &spans)
            .into_iter()
            .map(|b| (b.offset, b.kind))
            .collect()
    }

    /// The text with `|` at normal breaks, `-` at hyphenation points, `=`
    /// at repeated hyphens and `¶` at mandatory breaks.
    fn marked(text: &str, lang: &Lang, hyphenate: bool) -> String {
        let breaks = run(text, lang, hyphenate);
        let mut out = String::new();
        for (i, c) in text.char_indices() {
            for (_, kind) in breaks.iter().filter(|(o, _)| *o == i) {
                out.push(match kind {
                    BreakKind::Normal => '|',
                    BreakKind::Hyphenate => '-',
                    BreakKind::RepeatHyphen => '=',
                    BreakKind::Mandatory => '¶',
                });
            }
            out.push(c);
        }
        if breaks.last().is_some_and(|b| b.1 == BreakKind::Mandatory) {
            out.push('¶');
        }
        out
    }

    #[test]
    fn breaks_between_words() {
        let en = Lang::english();
        assert_eq!(marked("A ball, thrown.", &en, false), "A |ball, |thrown.¶");
        assert_eq!(marked("", &en, false), "");
        assert_eq!(marked("line\u{2028}next", &en, false), "line\u{2028}¶next¶");
    }

    #[test]
    fn hyphenates_words() {
        let en = Lang::english();
        assert_eq!(
            marked("An extensive, (wonderful) test", &en, true),
            "An |ex-ten-sive, |(won-der-ful) |test¶"
        );
        // Soft hyphens replace the patterns; words with digits or an
        // apostrophe stay whole, but not words in quotation marks.
        assert_eq!(
            marked(
                "exten\u{AD}sive H2Oextensive wonderful’s ‘wonderful’",
                &en,
                true
            ),
            "exten\u{AD}-sive |H2Oextensive |wonderful’s |‘won-der-ful’¶"
        );
    }

    #[test]
    fn polish_single_letters_stay_with_the_next_word() {
        let pl = Lang::polish();
        assert_eq!(
            marked(
                "Kot i pies, a w domu (z bratem) o tym. W lesie u nas",
                &pl,
                false
            ),
            "Kot |i pies, |a w domu |(z bratem) |o tym. |W lesie |u nas¶"
        );
        // In English, `a` and `I` are ordinary words.
        let en = Lang::english();
        assert_eq!(marked("I saw a cat", &en, false), "I |saw |a |cat¶");
    }

    #[test]
    fn polish_repeats_explicit_hyphens() {
        assert_eq!(
            marked("biało-czerwony", &Lang::polish(), false),
            "biało-=czerwony¶"
        );
        assert_eq!(
            marked("well-known", &Lang::english(), false),
            "well-|known¶"
        );
    }

    #[test]
    fn breaks_scripts_without_spaces() {
        let th = Lang::new("th").unwrap();
        let breaks = run("ภาษาไทยง่ายนิดเดียว", &th, false);
        assert!(breaks.len() > 2, "{breaks:?}");
        let ja = Lang::new("ja").unwrap();
        let breaks = run("日本語の文章", &ja, false);
        assert!(breaks.len() > 2, "{breaks:?}");
    }
}
