//! Script itemisation: splitting text into runs of one script (UAX #24).

use std::ops::Range;

pub use unicode_script::Script;
use unicode_script::UnicodeScript;

/// Splits text into runs of one script, each with its script.
///
/// Characters of no particular script (spaces, digits, punctuation:
/// `Common`) and combining marks (`Inherited`) join the run before them;
/// those at the start join the first run. A closing bracket takes the
/// script of its opening bracket instead, so that the brackets around a
/// word of another script match (UAX #24). Text with no character of a
/// particular script is one run in [`Script::Common`]. Empty text has no
/// runs.
pub fn script_runs(text: &str) -> Vec<(Range<usize>, Script)> {
    let mut out: Vec<(Range<usize>, Script)> = Vec::new();
    let mut start = 0;
    let mut current: Option<Script> = None;
    // Open brackets, with the script of the run they opened in.
    let mut brackets: Vec<(char, Option<Script>)> = Vec::new();
    for (i, c) in text.char_indices() {
        let script = c.script();
        if !is_neutral(script) {
            match current {
                Some(s) if s != script => {
                    out.push((start..i, s));
                    start = i;
                    current = Some(script);
                }
                None => current = Some(script),
                _ => {}
            }
            continue;
        }
        if let Some(close) = closing_bracket(c) {
            if brackets.len() == MAX_BRACKETS {
                brackets.remove(0);
            }
            brackets.push((close, current));
        } else if let Some(depth) = brackets.iter().rposition(|&(close, _)| close == c) {
            let (_, opened) = brackets[depth];
            brackets.truncate(depth);
            if let (Some(s), Some(opened)) = (current, opened)
                && s != opened
            {
                out.push((start..i, s));
                start = i;
                current = Some(opened);
            }
        }
    }
    if start < text.len() {
        out.push((start..text.len(), current.unwrap_or(Script::Common)));
    }
    out
}

/// How many open brackets the pairing remembers.
const MAX_BRACKETS: usize = 64;

/// The closing bracket of an opening one.
fn closing_bracket(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '⁅' => '⁆',
        '⟨' => '⟩',
        '〈' => '〉',
        '《' => '》',
        '「' => '」',
        '『' => '』',
        '【' => '】',
        '（' => '）',
        '［' => '］',
        '｛' => '｝',
        _ => return None,
    })
}

/// Whether characters of a script take the script of the text around them.
fn is_neutral(script: Script) -> bool {
    matches!(script, Script::Common | Script::Inherited | Script::Unknown)
}

/// The ISO 15924 code of a script, such as `Latn`.
pub(crate) fn iso_code(script: Script) -> &'static str {
    script.short_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(text: &str) -> Vec<(&str, &str)> {
        script_runs(text)
            .into_iter()
            .map(|(r, s)| (&text[r], iso_code(s)))
            .collect()
    }

    #[test]
    fn splits_scripts() {
        assert_eq!(
            runs("Abc, αβγ. Def"),
            [("Abc, ", "Latn"), ("αβγ. ", "Grek"), ("Def", "Latn")]
        );
        assert_eq!(runs("(1) שלום"), [("(1) שלום", "Hebr")]);
        assert_eq!(runs("1, 2"), [("1, 2", "Zyyy")]);
        // Brackets pair across a change of script.
        assert_eq!(
            runs("a (αβ) b"),
            [("a (", "Latn"), ("αβ", "Grek"), (") b", "Latn")]
        );
        assert_eq!(runs("a (αβ"), [("a (", "Latn"), ("αβ", "Grek")]);
        assert_eq!(runs("(αβ) b"), [("(αβ) ", "Grek"), ("b", "Latn")]);
        assert!(runs("").is_empty());
    }

    #[test]
    fn marks_join_their_base() {
        // e + combining acute, then Cyrillic.
        assert_eq!(runs("e\u{301}д"), [("e\u{301}", "Latn"), ("д", "Cyrl")]);
    }
}
