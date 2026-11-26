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
//!
//! It also holds how numbers are written in each language (SPEC §8.1): the
//! decimal marker of quantities and numbers, a point in English and a
//! comma in Polish ([`number_format`]).

use folio_component::{Registered, Value};
use folio_math::NumberFormat;
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
/// of a built-in element (SPEC §4.3), or `page`, the word of a page-number
/// reference (*page 4*, *s. 4*). The Polish words are abbreviations,
/// which do not inflect, so a reference reads right in any case of the
/// sentence around it (decision D-44-1): *zob. rys. 1*, *na rys. 1*.
const WORDS: &[(&str, Word<'static>, Word<'static>)] = &[
    ("section", Word::plain("Section"), Word::plain("rozdz.")),
    ("figure", Word::plain("Figure"), Word::plain("rys.")),
    ("page", Word::plain("page"), Word::plain("s.")),
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

/// The languages that write a decimal comma, by primary subtag; every
/// other language writes a point. The languages of Europe write a comma,
/// except English, Irish, Maltese and Swiss German (`de-CH`, which writes a
/// point).
const COMMA_LANGUAGES: &[&str] = &[
    "be", "bg", "bs", "ca", "cs", "da", "de", "el", "es", "et", "eu", "fi", "fo", "fr", "gl", "hr",
    "hu", "id", "is", "it", "kk", "lt", "lv", "mk", "nb", "nl", "nn", "no", "pl", "pt", "ro", "ru",
    "sk", "sl", "sq", "sr", "sv", "tr", "uk", "vi",
];

/// The regions where a language of [`COMMA_LANGUAGES`] writes a point,
/// as `language-REGION`: German and Italian in Switzerland and
/// Liechtenstein, and Spanish in Mexico, the United States, Central
/// America and the Caribbean.
const POINT_REGIONS: &[&str] = &[
    "de-CH", "de-LI", "it-CH", "es-MX", "es-US", "es-PR", "es-DO", "es-GT", "es-HN", "es-NI",
    "es-PA", "es-SV",
];

/// The region subtag of a language tag, if it has one: `CH` of `de-CH`
/// and of `de-Latn-CH-1996`.
fn region(tag: &str) -> Option<&str> {
    tag.split(['-', '_']).skip(1).find(|subtag| {
        (subtag.len() == 2 && subtag.bytes().all(|b| b.is_ascii_alphabetic()))
            || (subtag.len() == 3 && subtag.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The decimal marker of a language: `,` in Polish, `.` in English.
pub fn decimal_marker(lang: &Lang) -> char {
    let primary = lang.primary();
    if let Some(region) = region(lang.as_str())
        && POINT_REGIONS.iter().any(|tag| {
            tag.split_once('-').is_some_and(|(language, r)| {
                language.eq_ignore_ascii_case(primary) && r.eq_ignore_ascii_case(region)
            })
        })
    {
        return '.';
    }
    if COMMA_LANGUAGES.contains(&primary) {
        ','
    } else {
        '.'
    }
}

/// How quantities and numbers are written in text of a language, with the
/// computed value of the `decimal-marker` property: `point` or `comma`,
/// or `auto` (or no value) for the language's own marker.
pub fn number_format(lang: &Lang, decimal_marker_value: Option<&Value>) -> NumberFormat {
    let marker = match decimal_marker_value {
        Some(Value::Keyword(k)) if k.as_str() == "point" => '.',
        Some(Value::Keyword(k)) if k.as_str() == "comma" => ',',
        _ => decimal_marker(lang),
    };
    NumberFormat::with_marker(marker)
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

    #[test]
    fn decimal_markers_by_language() {
        let lang = |tag: &str| Lang::new(tag).unwrap();
        assert_eq!(decimal_marker(&lang("en")), '.');
        assert_eq!(decimal_marker(&lang("en-GB")), '.');
        assert_eq!(decimal_marker(&lang("pl")), ',');
        assert_eq!(decimal_marker(&lang("de")), ',');
        assert_eq!(decimal_marker(&lang("de-CH")), '.');
        assert_eq!(decimal_marker(&lang("de-Latn-CH")), '.');
        assert_eq!(decimal_marker(&lang("es-MX")), '.');
        assert_eq!(decimal_marker(&lang("es-ES")), ',');
        assert_eq!(decimal_marker(&lang("ja")), '.');
        let comma = Value::Keyword(folio_component::Name::new("comma").unwrap());
        let point = Value::Keyword(folio_component::Name::new("point").unwrap());
        assert_eq!(
            number_format(&lang("en"), Some(&comma)),
            NumberFormat::COMMA
        );
        assert_eq!(
            number_format(&lang("pl"), Some(&point)),
            NumberFormat::POINT
        );
        assert_eq!(
            number_format(&lang("pl"), Some(&Value::Auto)),
            NumberFormat::COMMA
        );
        assert_eq!(number_format(&lang("en"), None), NumberFormat::POINT);
    }
}
