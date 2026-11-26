//! Locale strings (SPEC §4.5): the words that generated text uses, such as
//! *Figure* in "Figure 2" or *rozdz.* in "rozdz. 3", chosen by the `lang`
//! of the text they are set in.
//!
//! The core holds the words of the built-in elements' reference keys in
//! English and Polish; the Polish ones are abbreviations that do not
//! inflect (*rozdz. 2*, *rys. 1*, *wzór (3)*). Components supply the words
//! of their own keys ([`folio_component::LocaleString`]), English at least; a word that a
//! component lacks in a language falls back to the core's, then to the
//! component's English.

use folio_component::Registered;
use folio_text::Lang;

/// The languages whose words the core has, by primary subtag, English
/// first: the fallback for every other language.
pub const LANGUAGES: [&str; 2] = ["en", "pl"];

/// A word of generated text, and how a number follows it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Word<'a> {
    /// The word: *Figure*, *rys.*.
    pub text: &'a str,
    /// Whether the number after it is set in brackets: *wzór (3)*.
    pub brackets: bool,
}

impl<'a> Word<'a> {
    /// A word whose number follows it as it is.
    pub const fn plain(text: &'a str) -> Self {
        Self {
            text,
            brackets: false,
        }
    }
}

/// The core's words: `(key, English, Polish)`. A key is the reference key
/// of a built-in element (SPEC §4.3). The Polish words are abbreviations,
/// which do not inflect, so a reference reads right in any case of the
/// sentence around it (decision D-44-1): *zob. rys. 1*, *na rys. 1*.
const WORDS: &[(&str, Word<'static>, Word<'static>)] = &[
    ("section", Word::plain("Section"), Word::plain("rozdz.")),
    ("figure", Word::plain("Figure"), Word::plain("rys.")),
    (
        "equation",
        Word::plain("Equation"),
        Word {
            text: "wzór",
            brackets: true,
        },
    ),
];

/// The core's word for a key in a language, or in English when the core has
/// no words in that language. `None` for a key the core does not know.
pub fn word(lang: &Lang, key: &str) -> Option<Word<'static>> {
    let (_, en, pl) = WORDS.iter().find(|(k, ..)| *k == key)?;
    Some(match lang.primary() {
        "pl" => *pl,
        _ => *en,
    })
}

/// The word for a key of a component's element: the component's own in the
/// language (by its full tag, then by its primary subtag), else the core's,
/// else the component's English.
pub fn component_word<'a>(component: &'a Registered, lang: &Lang, key: &str) -> Option<Word<'a>> {
    let tag = lang.to_string();
    component
        .locale_string(&tag, key)
        .or_else(|| component.locale_string(lang.primary(), key))
        .map(Word::plain)
        .or_else(|| word(lang, key).filter(|_| LANGUAGES.contains(&lang.primary())))
        .or_else(|| component.locale_string("en", key).map(Word::plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_by_language() {
        let pl = Lang::new("pl").unwrap();
        let en_gb = Lang::new("en-GB").unwrap();
        let de = Lang::new("de").unwrap();
        assert_eq!(word(&pl, "figure"), Some(Word::plain("rys.")));
        assert_eq!(word(&pl, "equation").map(|w| w.brackets), Some(true));
        assert_eq!(word(&en_gb, "section"), Some(Word::plain("Section")));
        // English for a language the core has no words in.
        assert_eq!(word(&de, "equation"), Some(Word::plain("Equation")));
        assert_eq!(word(&pl, "nothing"), None);
    }
}
