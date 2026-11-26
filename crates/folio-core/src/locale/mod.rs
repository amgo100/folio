//! Locale strings (SPEC §4.5): the words that generated text uses, such as
//! *Figure* in "Figure 2" or *Rozdział* in "Rozdział 3", chosen by the
//! `lang` of the text they are set in.
//!
//! The core holds the words of the built-in elements' reference keys in
//! English and Polish. Components supply the words of their own keys
//! ([`folio_component::LocaleString`]), English at least; a word that a
//! component lacks in a language falls back to the core's, then to the
//! component's English.

use folio_component::Registered;
use folio_text::Lang;

/// The languages whose words the core has, by primary subtag, English
/// first: the fallback for every other language.
pub const LANGUAGES: [&str; 2] = ["en", "pl"];

/// The core's words: `(key, English, Polish)`. A key is the reference key
/// of a built-in element (SPEC §4.3). The words are in the nominative case,
/// as they stand in a reference or a caption on their own.
const WORDS: &[(&str, &str, &str)] = &[
    ("section", "Section", "Rozdział"),
    ("figure", "Figure", "Rysunek"),
    ("equation", "Equation", "Równanie"),
];

/// The core's word for a key in a language, or in English when the core has
/// no words in that language. `None` for a key the core does not know.
pub fn word(lang: &Lang, key: &str) -> Option<&'static str> {
    let (_, en, pl) = WORDS.iter().find(|(k, ..)| *k == key)?;
    Some(match lang.primary() {
        "pl" => pl,
        _ => en,
    })
}

/// The word for a key of a component's element: the component's own in the
/// language (by its full tag, then by its primary subtag), else the core's,
/// else the component's English.
pub fn component_word<'a>(component: &'a Registered, lang: &Lang, key: &str) -> Option<&'a str> {
    let tag = lang.to_string();
    component
        .locale_string(&tag, key)
        .or_else(|| component.locale_string(lang.primary(), key))
        .or_else(|| word(lang, key).filter(|_| LANGUAGES.contains(&lang.primary())))
        .or_else(|| component.locale_string("en", key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_by_language() {
        let pl = Lang::new("pl").unwrap();
        let en_gb = Lang::new("en-GB").unwrap();
        let de = Lang::new("de").unwrap();
        assert_eq!(word(&pl, "figure"), Some("Rysunek"));
        assert_eq!(word(&en_gb, "section"), Some("Section"));
        // English for a language the core has no words in.
        assert_eq!(word(&de, "equation"), Some("Equation"));
        assert_eq!(word(&pl, "nothing"), None);
    }
}
