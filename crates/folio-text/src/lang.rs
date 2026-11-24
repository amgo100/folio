//! Languages and their typographic rules (SPEC §7.2): quotation marks,
//! hyphenation patterns, the default writing direction, and line-end rules.

use std::fmt::{self, Debug, Display, Formatter};

use ecow::EcoString;

/// A BCP 47 language tag, such as `en`, `pl` or `en-GB`.
///
/// Tags compare without regard to case, as BCP 47 says: the tag is stored
/// in the case BCP 47 recommends (`en-GB`, `sr-Latn`, `zh-Hant-TW`).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Lang(EcoString);

impl Lang {
    /// English.
    pub fn english() -> Self {
        Self("en".into())
    }

    /// Polish.
    pub fn polish() -> Self {
        Self("pl".into())
    }

    /// Parses a tag: a primary subtag of two or three letters, then any
    /// subtags of one to eight letters or digits, separated by hyphens.
    /// `None` when the text is not such a tag.
    pub fn new(tag: &str) -> Option<Self> {
        let mut subtags = tag.split('-');
        let primary = subtags.next()?;
        let valid = (2..=3).contains(&primary.len())
            && primary.bytes().all(|b| b.is_ascii_alphabetic())
            && subtags.all(|s| {
                (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if !valid {
            return None;
        }
        let mut out = EcoString::from(primary.to_ascii_lowercase());
        for subtag in tag.split('-').skip(1) {
            out.push('-');
            let all_alpha = subtag.bytes().all(|b| b.is_ascii_alphabetic());
            match subtag.len() {
                // A region.
                2 if all_alpha => out.push_str(&subtag.to_ascii_uppercase()),
                // A script.
                4 if all_alpha => {
                    out.push_str(&subtag[..1].to_ascii_uppercase());
                    out.push_str(&subtag[1..].to_ascii_lowercase());
                }
                _ => out.push_str(&subtag.to_ascii_lowercase()),
            }
        }
        Some(Self(out))
    }

    /// The tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The primary language subtag, in lower case: `en` for `en-GB`.
    pub fn primary(&self) -> &str {
        self.0.split('-').next().unwrap_or_default()
    }

    /// The language's quotation marks.
    pub fn quotes(&self) -> Quotes {
        match self.primary() {
            // Polish nests „…‚…’…”. The inner closing mark is the
            // apostrophe, so an apostrophe written as `'` after a letter
            // (`Shakespeare'a`) is right whichever it is taken for.
            "pl" => Quotes::new(('„', '”'), ('‚', '’')),
            "de" | "cs" | "sk" | "sl" | "lt" | "is" => Quotes::new(('„', '“'), ('‚', '‘')),
            "ru" | "uk" | "be" => Quotes::new(('«', '»'), ('„', '“')),
            "fr" | "es" | "it" | "el" | "ca" => Quotes::new(('«', '»'), ('“', '”')),
            _ => Quotes::new(('“', '”'), ('‘', '’')),
        }
    }

    /// The language's hyphenation patterns, if Folio has them.
    pub fn hyphenation(&self) -> Option<Hyphenation> {
        let lang = hypher::Lang::from_iso(self.primary().as_bytes().try_into().ok()?)?;
        let (left, right) = lang.bounds();
        Some(Hyphenation { lang, left, right })
    }

    /// The direction the language is written in, which sets a paragraph's
    /// direction when its text has no strongly directional character.
    pub fn dir(&self) -> Dir {
        match self.primary() {
            "ar" | "arc" | "ckb" | "dv" | "fa" | "he" | "ks" | "ps" | "sd" | "ug" | "ur" | "yi" => {
                Dir::Rtl
            }
            _ => Dir::Ltr,
        }
    }

    /// Whether a word may not end a line in this language, so that the
    /// space after it does not break. In Polish these are the words of one
    /// letter: a, i, o, u, w, z and their capitals (SPEC §7.2).
    pub fn binds_to_next(&self, word: &str) -> bool {
        match self.primary() {
            "pl" => matches!(
                word,
                "a" | "i" | "o" | "u" | "w" | "z" | "A" | "I" | "O" | "U" | "W" | "Z"
            ),
            _ => false,
        }
    }

    /// Whether a line that breaks after an explicit hyphen repeats the
    /// hyphen at the start of the next line, as Polish does:
    /// `biało-` / `-czerwony`.
    pub fn repeats_hyphen(&self) -> bool {
        self.primary() == "pl"
    }
}

impl Display for Lang {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Debug for Lang {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Lang({})", self.0)
    }
}

/// A writing direction.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Dir {
    /// Left to right.
    Ltr,
    /// Right to left.
    Rtl,
}

/// A language's quotation marks: the double pair, which markup's `"`
/// gives, and the single pair, which `'` gives.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Quotes {
    /// The opening and closing double quotation marks.
    pub double: (char, char),
    /// The opening and closing single quotation marks.
    pub single: (char, char),
}

impl Quotes {
    const fn new(double: (char, char), single: (char, char)) -> Self {
        Self { double, single }
    }

    /// The mark for a quote in markup, given the character before it and
    /// the one after it: an apostrophe for `'` between two letters, an
    /// opening mark at the start, after a space or an opening bracket, dash
    /// or quotation mark, and a closing mark otherwise.
    pub fn mark(&self, double: bool, before: Option<char>, after: Option<char>) -> char {
        let (open, close) = if double { self.double } else { self.single };
        if !double
            && before.is_some_and(char::is_alphanumeric)
            && after.is_some_and(char::is_alphabetic)
        {
            return '’';
        }
        let opens = match before {
            None => true,
            Some(c) => {
                c.is_whitespace()
                    || matches!(c, '(' | '[' | '{' | '\u{FFFC}' | '–' | '—' | '-' | '/')
                    || c == open
                    || is_opening_quote(c)
            }
        };
        if opens { open } else { close }
    }
}

/// Whether a character is an opening quotation mark in some language.
fn is_opening_quote(c: char) -> bool {
    matches!(c, '“' | '‘' | '„' | '‚' | '«')
}

/// A language's hyphenation patterns, with the fewest letters a
/// hyphenation leaves at the start and at the end of a word.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Hyphenation {
    pub(crate) lang: hypher::Lang,
    /// The fewest letters before the first hyphen.
    pub left: usize,
    /// The fewest letters after the last hyphen.
    pub right: usize,
}

impl Hyphenation {
    /// The byte offsets in a word at which it may be hyphenated.
    pub fn points(&self, word: &str) -> Vec<usize> {
        let mut offset = 0;
        let mut out = Vec::new();
        let mut syllables = hypher::hyphenate_bounded(word, self.lang, self.left, self.right);
        if let Some(first) = syllables.next() {
            offset += first.len();
        }
        for syllable in syllables {
            out.push(offset);
            offset += syllable.len();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags() {
        assert_eq!(Lang::new("PL").unwrap().as_str(), "pl");
        assert_eq!(Lang::new("en-GB").unwrap().as_str(), "en-GB");
        assert_eq!(Lang::new("en-gb"), Lang::new("EN-GB"));
        assert_eq!(Lang::new("sr-latn-rs").unwrap().as_str(), "sr-Latn-RS");
        assert_eq!(Lang::new("es-419").unwrap().as_str(), "es-419");
        assert_eq!(Lang::new("en-GB").unwrap().primary(), "en");
        assert!(Lang::new("english").is_none());
        assert!(Lang::new("").is_none());
        assert!(Lang::new("en-").is_none());
    }

    #[test]
    fn hyphenates_english_and_polish() {
        let en = Lang::english().hyphenation().unwrap();
        assert_eq!(en.points("extensive"), [2, 5]);
        let pl = Lang::polish().hyphenation().unwrap();
        // wy-kształ-ciu-chy, in bytes: `ł` takes two.
        assert_eq!(pl.points("wykształciuchy"), [2, 9, 12]);
        assert!(Lang::new("xx").unwrap().hyphenation().is_none());
    }

    #[test]
    fn chooses_quotation_marks() {
        let pl = Lang::polish().quotes();
        assert_eq!(pl.mark(true, None, Some('T')), '„');
        assert_eq!(pl.mark(true, Some('t'), Some(' ')), '”');
        assert_eq!(pl.mark(false, Some('e'), Some('a')), '’');
        let en = Lang::english().quotes();
        assert_eq!(en.mark(true, Some(' '), Some('a')), '“');
        assert_eq!(en.mark(false, Some('“'), Some('a')), '‘');
        assert_eq!(en.mark(false, Some('n'), Some('t')), '’');
        assert_eq!(en.mark(false, Some('s'), Some(' ')), '’');
    }

    #[test]
    fn polish_single_letters_bind() {
        let pl = Lang::polish();
        for word in ["a", "i", "o", "u", "w", "z", "A", "I", "O", "U", "W", "Z"] {
            assert!(pl.binds_to_next(word), "{word}");
        }
        assert!(!pl.binds_to_next("e"));
        assert!(!pl.binds_to_next("we"));
        assert!(!Lang::english().binds_to_next("a"));
    }
}
