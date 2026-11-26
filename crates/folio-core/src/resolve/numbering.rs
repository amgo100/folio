//! Numbering patterns (SPEC §5.1, §5.2): the strings of `numbering`, such as
//! `"1."`, `"(a)"`, `"i"` or `"1.1"`, which format a counter's values.
//!
//! A pattern is text with counting symbols in it: `1` (1, 2, 3), `a` and `A`
//! (a, b, …, z, aa), `i` and `I` (roman numerals) and `*` (*, †, ‡, §, ¶,
//! ‖, then doubled). The first symbol formats the first value, the second
//! the second, and the last symbol formats every value after; the text
//! before a symbol separates its value from the one before, and the text
//! after the last symbol ends the number. A value past the symbols is
//! separated by the text before the last symbol, or by the ending when
//! there is none: `"1."` formats the values 2 and 3 as `2.3.`, and `"1.a"`
//! as `2.c`. A pattern without a counting symbol is shown as it is.

use ecow::{EcoString, eco_format};

/// A parsed numbering pattern.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Pattern {
    /// Each counting symbol with the text before it.
    pieces: Vec<(EcoString, Symbol)>,
    /// The text after the last symbol.
    suffix: EcoString,
}

/// A counting symbol.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
enum Symbol {
    Arabic,
    LowerLatin,
    UpperLatin,
    LowerRoman,
    UpperRoman,
    Symbols,
}

impl Symbol {
    fn of(c: char) -> Option<Self> {
        Some(match c {
            '1' => Self::Arabic,
            'a' => Self::LowerLatin,
            'A' => Self::UpperLatin,
            'i' => Self::LowerRoman,
            'I' => Self::UpperRoman,
            '*' => Self::Symbols,
            _ => return None,
        })
    }

    /// Formats a value. Zero, which a heading level that was skipped has,
    /// is `0` in every system.
    fn format(self, n: u64) -> EcoString {
        if n == 0 {
            return "0".into();
        }
        match self {
            Self::Arabic => eco_format!("{n}"),
            Self::LowerLatin => latin(n, b'a'),
            Self::UpperLatin => latin(n, b'A'),
            Self::LowerRoman => roman(n).to_lowercase(),
            Self::UpperRoman => roman(n),
            Self::Symbols => {
                const SYMBOLS: [char; 6] = ['*', '†', '‡', '§', '¶', '‖'];
                let len = SYMBOLS.len() as u64;
                let symbol = SYMBOLS[((n - 1) % len) as usize];
                // Far beyond any document's notes: shown as a number.
                let times = (n - 1) / len + 1;
                if times > 8 {
                    return eco_format!("{n}");
                }
                std::iter::repeat_n(symbol, times as usize).collect()
            }
        }
    }
}

/// `n` in bijective base 26: a, …, z, aa, ab, …
fn latin(mut n: u64, a: u8) -> EcoString {
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(char::from(a + (n % 26) as u8));
        n /= 26;
    }
    letters.iter().rev().copied().collect()
}

/// `n` in roman numerals, up to 3999; larger numbers in arabic numerals.
fn roman(mut n: u64) -> EcoString {
    if n >= 4000 {
        return eco_format!("{n}");
    }
    const TABLE: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = EcoString::new();
    for (value, letters) in TABLE {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    out
}

impl Pattern {
    /// Parses a pattern. Every string is a pattern.
    pub fn parse(text: &str) -> Self {
        let mut pieces = Vec::new();
        let mut current = EcoString::new();
        for c in text.chars() {
            match Symbol::of(c) {
                Some(symbol) => pieces.push((std::mem::take(&mut current), symbol)),
                None => current.push(c),
            }
        }
        Self {
            pieces,
            suffix: current,
        }
    }

    /// Formats a counter's values, as an element shows its number:
    /// `"(a)"` formats 2 as `(b)`.
    pub fn format(&self, values: &[u64]) -> EcoString {
        self.apply(values, false)
    }

    /// Formats a counter's values without the text before the first symbol
    /// and after the last, as a reference shows the number: `"(a)"` formats
    /// 2 as `b`, and `"1."` formats 1 and 2 as `1.2`.
    pub fn format_trimmed(&self, values: &[u64]) -> EcoString {
        self.apply(values, true)
    }

    fn apply(&self, values: &[u64], trimmed: bool) -> EcoString {
        let Some((last_prefix, last)) = self.pieces.last() else {
            return self.suffix.clone();
        };
        let mut out = EcoString::new();
        for (i, &n) in values.iter().enumerate() {
            let (prefix, symbol) = match self.pieces.get(i) {
                Some((prefix, symbol)) => (prefix, *symbol),
                None if last_prefix.is_empty() => (&self.suffix, *last),
                None => (last_prefix, *last),
            };
            if i > 0 || !trimmed {
                out.push_str(prefix);
            }
            out.push_str(&symbol.format(n));
        }
        if !trimmed {
            out.push_str(&self.suffix);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(pattern: &str, values: &[u64]) -> String {
        Pattern::parse(pattern).format(values).to_string()
    }

    fn trimmed(pattern: &str, values: &[u64]) -> String {
        Pattern::parse(pattern).format_trimmed(values).to_string()
    }

    #[test]
    fn patterns() {
        assert_eq!(format("1.", &[3]), "3.");
        assert_eq!(format("1.", &[2, 3]), "2.3.");
        assert_eq!(format("1.1", &[2, 3, 4]), "2.3.4");
        assert_eq!(format("1.1", &[2]), "2");
        assert_eq!(format("(a)", &[2]), "(b)");
        assert_eq!(format("1.a)", &[2, 3]), "2.c)");
        assert_eq!(format("i", &[4]), "iv");
        assert_eq!(format("I.", &[1994]), "MCMXCIV.");
        assert_eq!(format("A", &[28]), "AB");
        assert_eq!(format("a", &[26]), "z");
        assert_eq!(format("*", &[2]), "†");
        assert_eq!(format("*", &[8]), "††");
        assert_eq!(format("1.", &[0, 2]), "0.2.");
        assert_eq!(format("§", &[1]), "§");
        assert_eq!(format("", &[1]), "");
    }

    #[test]
    fn trimmed_patterns() {
        assert_eq!(trimmed("1.", &[3]), "3");
        assert_eq!(trimmed("1.", &[2, 3]), "2.3");
        assert_eq!(trimmed("(a)", &[2]), "b");
        assert_eq!(trimmed("(1)", &[4]), "4");
        assert_eq!(trimmed("1.1", &[1, 2]), "1.2");
    }

    #[test]
    fn large_numbers() {
        assert_eq!(format("i", &[4000]), "4000");
        assert_eq!(format("*", &[100]), "100");
        assert_eq!(format("a", &[u64::MAX]).len(), 14);
    }
}
