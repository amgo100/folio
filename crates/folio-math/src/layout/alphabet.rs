//! Math alphabets (SPEC §8.2): letters and digits mapped to the
//! Mathematical Alphanumeric Symbols of Unicode, which math fonts draw in
//! each style.

use crate::MathVariant;

/// The style a character is set in.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) enum Alphabet {
    Upright,
    Italic,
    Bold,
    Script,
    Fraktur,
    DoubleStruck,
    SansSerif,
    Monospace,
}

impl Alphabet {
    /// The alphabet of a variant written in the source.
    pub(crate) fn of(variant: MathVariant) -> Self {
        match variant {
            MathVariant::Upright => Self::Upright,
            MathVariant::Italic => Self::Italic,
            MathVariant::Bold => Self::Bold,
            MathVariant::Calligraphic => Self::Script,
            MathVariant::Fraktur => Self::Fraktur,
            MathVariant::DoubleStruck => Self::DoubleStruck,
            MathVariant::SansSerif => Self::SansSerif,
            MathVariant::Monospace => Self::Monospace,
        }
    }
}

/// The alphabet a variable is set in without a variant: italic for Latin
/// letters and lowercase Greek, upright for uppercase Greek and anything
/// else, as TeX sets them.
pub(crate) fn auto(c: char) -> Alphabet {
    match c {
        'a'..='z' | 'A'..='Z' | 'ı' | 'ȷ' => Alphabet::Italic,
        'α'..='ω' | 'ϵ' | 'ϑ' | 'ϰ' | 'ϕ' | 'ϱ' | 'ϖ' | '∂' => Alphabet::Italic,
        _ => Alphabet::Upright,
    }
}

/// A text with each character mapped to an alphabet.
pub(crate) fn style(text: &str, alphabet: impl Fn(char) -> Alphabet) -> String {
    text.chars().map(|c| map(c, alphabet(c))).collect()
}

/// A character in an alphabet, or itself where the alphabet has no form
/// of it.
pub(crate) fn map(c: char, alphabet: Alphabet) -> char {
    use Alphabet as A;
    let latin = |upper: u32, lower: u32| -> Option<u32> {
        match c {
            'A'..='Z' => Some(upper + (c as u32 - 'A' as u32)),
            'a'..='z' => Some(lower + (c as u32 - 'a' as u32)),
            _ => None,
        }
    };
    let digits = |zero: u32| -> Option<u32> {
        c.to_digit(10)
            .filter(|_| c.is_ascii_digit())
            .map(|d| zero + d)
    };
    let mapped = match alphabet {
        A::Upright => None,
        A::Italic => match c {
            'h' => Some(0x210E),
            'ı' => Some(0x1D6A4),
            'ȷ' => Some(0x1D6A5),
            _ => latin(0x1D434, 0x1D44E).or_else(|| greek(c, 0x1D6E2, 0x1D6FC)),
        },
        A::Bold => latin(0x1D400, 0x1D41A)
            .or_else(|| greek(c, 0x1D6A8, 0x1D6C2))
            .or_else(|| digits(0x1D7CE)),
        A::Script => match c {
            'B' => Some(0x212C),
            'E' => Some(0x2130),
            'F' => Some(0x2131),
            'H' => Some(0x210B),
            'I' => Some(0x2110),
            'L' => Some(0x2112),
            'M' => Some(0x2133),
            'R' => Some(0x211B),
            'e' => Some(0x212F),
            'g' => Some(0x210A),
            'o' => Some(0x2134),
            _ => latin(0x1D49C, 0x1D4B6),
        },
        A::Fraktur => match c {
            'C' => Some(0x212D),
            'H' => Some(0x210C),
            'I' => Some(0x2111),
            'R' => Some(0x211C),
            'Z' => Some(0x2128),
            _ => latin(0x1D504, 0x1D51E),
        },
        A::DoubleStruck => match c {
            'C' => Some(0x2102),
            'H' => Some(0x210D),
            'N' => Some(0x2115),
            'P' => Some(0x2119),
            'Q' => Some(0x211A),
            'R' => Some(0x211D),
            'Z' => Some(0x2124),
            _ => latin(0x1D538, 0x1D552).or_else(|| digits(0x1D7D8)),
        },
        A::SansSerif => latin(0x1D5A0, 0x1D5BA).or_else(|| digits(0x1D7E2)),
        A::Monospace => latin(0x1D670, 0x1D68A).or_else(|| digits(0x1D7F6)),
    };
    mapped.and_then(char::from_u32).unwrap_or(c)
}

/// A Greek letter in a math alphabet whose capitals start at `upper` and
/// whose small letters start at `lower`.
fn greek(c: char, upper: u32, lower: u32) -> Option<u32> {
    let code = c as u32;
    match c {
        'Α'..='Ω' => Some(upper + (code - 0x391)),
        'ϴ' => Some(upper + 17),
        '∇' => Some(upper + 25),
        'α'..='ω' => Some(lower + (code - 0x3B1)),
        '∂' => Some(lower + 25),
        'ϵ' => Some(lower + 26),
        'ϑ' => Some(lower + 27),
        'ϰ' => Some(lower + 28),
        'ϕ' => Some(lower + 29),
        'ϱ' => Some(lower + 30),
        'ϖ' => Some(lower + 31),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_map_to_their_alphabets() {
        assert_eq!(map('x', Alphabet::Italic), '𝑥');
        assert_eq!(map('h', Alphabet::Italic), 'ℎ');
        assert_eq!(map('R', Alphabet::DoubleStruck), 'ℝ');
        assert_eq!(map('L', Alphabet::Script), 'ℒ');
        assert_eq!(map('F', Alphabet::Bold), '𝐅');
        assert_eq!(map('α', Alphabet::Italic), '𝛼');
        assert_eq!(map('Ω', Alphabet::Italic), '𝛺');
        assert_eq!(map('2', Alphabet::Bold), '𝟐');
        assert_eq!(map('2', Alphabet::Italic), '2');
        assert_eq!(map('+', Alphabet::Bold), '+');
        assert_eq!(auto('Γ'), Alphabet::Upright);
        assert_eq!(auto('γ'), Alphabet::Italic);
    }
}
