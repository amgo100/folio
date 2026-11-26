//! Named symbols of native math (SPEC §8.1): `alpha`, `sum`, `arrow.r`.
//!
//! A name is a base and modifiers separated by dots, as in `arrow.r.long`.
//! The modifiers may be written in any order: `arrow.long.r` is the same
//! symbol.

/// The symbols, by name: a base and its modifiers in a fixed order.
///
/// Names follow the conventions of Typst's symbol set, so that formulas
/// written for one read the same in the other: Greek letters by their
/// names, directions of arrows as `r`, `l`, `t`, `b`, and variants as
/// modifiers (`eq.not`, `lt.eq`).
const SYMBOLS: &[(&str, char)] = &[
    // Greek letters.
    ("alpha", 'α'),
    ("beta", 'β'),
    ("gamma", 'γ'),
    ("delta", 'δ'),
    ("epsilon", 'ε'),
    ("epsilon.alt", 'ϵ'),
    ("zeta", 'ζ'),
    ("eta", 'η'),
    ("theta", 'θ'),
    ("theta.alt", 'ϑ'),
    ("iota", 'ι'),
    ("kappa", 'κ'),
    ("lambda", 'λ'),
    ("mu", 'μ'),
    ("nu", 'ν'),
    ("xi", 'ξ'),
    ("omicron", 'ο'),
    ("pi", 'π'),
    ("pi.alt", 'ϖ'),
    ("rho", 'ρ'),
    ("rho.alt", 'ϱ'),
    ("sigma", 'σ'),
    ("sigma.alt", 'ς'),
    ("tau", 'τ'),
    ("upsilon", 'υ'),
    ("phi", 'φ'),
    ("phi.alt", 'ϕ'),
    ("chi", 'χ'),
    ("psi", 'ψ'),
    ("omega", 'ω'),
    ("Alpha", 'Α'),
    ("Beta", 'Β'),
    ("Gamma", 'Γ'),
    ("Delta", 'Δ'),
    ("Epsilon", 'Ε'),
    ("Zeta", 'Ζ'),
    ("Eta", 'Η'),
    ("Theta", 'Θ'),
    ("Iota", 'Ι'),
    ("Kappa", 'Κ'),
    ("Lambda", 'Λ'),
    ("Mu", 'Μ'),
    ("Nu", 'Ν'),
    ("Xi", 'Ξ'),
    ("Omicron", 'Ο'),
    ("Pi", 'Π'),
    ("Rho", 'Ρ'),
    ("Sigma", 'Σ'),
    ("Tau", 'Τ'),
    ("Upsilon", 'Υ'),
    ("Phi", 'Φ'),
    ("Chi", 'Χ'),
    ("Psi", 'Ψ'),
    ("Omega", 'Ω'),
    // Letter-like symbols.
    ("planck", 'ℎ'),
    ("planck.reduce", 'ℏ'),
    ("ell", 'ℓ'),
    ("aleph", 'ℵ'),
    ("partial", '∂'),
    ("nabla", '∇'),
    ("infinity", '∞'),
    ("oo", '∞'),
    ("degree", '°'),
    ("prime", '′'),
    ("prime.double", '″'),
    ("angle", '∠'),
    ("emptyset", '∅'),
    ("dagger", '†'),
    // Binary operators.
    ("plus", '+'),
    ("minus", '−'),
    ("plus.minus", '±'),
    ("minus.plus", '∓'),
    ("times", '×'),
    ("times.circle", '⊗'),
    ("plus.circle", '⊕'),
    ("div", '÷'),
    ("dot", '⋅'),
    ("dot.op", '⋅'),
    ("dot.circle", '⊙'),
    ("ast", '∗'),
    ("star", '⋆'),
    ("circle.small", '∘'),
    ("compose", '∘'),
    ("union", '∪'),
    ("sect", '∩'),
    ("without", '∖'),
    ("and", '∧'),
    ("or", '∨'),
    ("not", '¬'),
    // Relations.
    ("eq", '='),
    ("eq.not", '≠'),
    ("eq.def", '≝'),
    ("eq.triple", '≡'),
    ("equiv", '≡'),
    ("colon.eq", '≔'),
    ("approx", '≈'),
    ("approx.eq", '≊'),
    ("tilde.op", '∼'),
    ("tilde.eq", '≃'),
    ("tilde.equiv", '≅'),
    ("prop", '∝'),
    ("lt", '<'),
    ("gt", '>'),
    ("lt.eq", '≤'),
    ("gt.eq", '≥'),
    ("lt.eq.slant", '⩽'),
    ("gt.eq.slant", '⩾'),
    ("lt.double", '≪'),
    ("gt.double", '≫'),
    ("lt.not", '≮'),
    ("gt.not", '≯'),
    ("in", '∈'),
    ("in.not", '∉'),
    ("in.rev", '∋'),
    ("subset", '⊂'),
    ("subset.eq", '⊆'),
    ("supset", '⊃'),
    ("supset.eq", '⊇'),
    ("parallel", '∥'),
    ("perp", '⟂'),
    ("divides", '∣'),
    ("therefore", '∴'),
    ("because", '∵'),
    ("forall", '∀'),
    ("exists", '∃'),
    ("exists.not", '∄'),
    // Arrows.
    ("arrow.r", '→'),
    ("arrow.l", '←'),
    ("arrow.t", '↑'),
    ("arrow.b", '↓'),
    ("arrow.l.r", '↔'),
    ("arrow.t.b", '↕'),
    ("arrow.r.long", '⟶'),
    ("arrow.l.long", '⟵'),
    ("arrow.l.r.long", '⟷'),
    ("arrow.r.double", '⇒'),
    ("arrow.l.double", '⇐'),
    ("arrow.l.r.double", '⇔'),
    ("arrow.r.double.long", '⟹'),
    ("arrow.l.double.long", '⟸'),
    ("arrow.l.r.double.long", '⟺'),
    ("arrow.r.bar", '↦'),
    ("arrow.r.squiggly", '⇝'),
    ("arrows.rl", '⇄'),
    ("harpoons.rtlb", '⇌'),
    ("implies", '⟹'),
    ("iff", '⟺'),
    ("mapsto", '↦'),
    ("to", '→'),
    // Large operators.
    ("sum", '∑'),
    ("product", '∏'),
    ("product.co", '∐'),
    ("integral", '∫'),
    ("integral.double", '∬'),
    ("integral.triple", '∭'),
    ("integral.cont", '∮'),
    ("integral.surf", '∯'),
    ("integral.vol", '∰'),
    ("union.big", '⋃'),
    ("sect.big", '⋂'),
    ("plus.circle.big", '⨁'),
    ("times.circle.big", '⨂'),
    // Dots and punctuation.
    ("dots.h", '…'),
    ("dots.c", '⋯'),
    ("dots.v", '⋮'),
    ("dots.down", '⋱'),
    ("colon", ':'),
    ("comma", ','),
    // Delimiters.
    ("paren.l", '('),
    ("paren.r", ')'),
    ("bracket.l", '['),
    ("bracket.r", ']'),
    ("brace.l", '{'),
    ("brace.r", '}'),
    ("angle.l", '⟨'),
    ("angle.r", '⟩'),
    ("bar.v", '|'),
    ("bar.v.double", '‖'),
    ("floor.l", '⌊'),
    ("floor.r", '⌋'),
    ("ceil.l", '⌈'),
    ("ceil.r", '⌉'),
    // Spaces.
    ("thin", '\u{2009}'),
    ("med", '\u{205F}'),
    ("thick", '\u{2004}'),
    ("quad", '\u{2003}'),
    ("wide", '\u{2003}'),
];

/// The symbol a name stands for: `alpha` is `α`, and `arrow.r.long` and
/// `arrow.long.r` are both `⟶`.
pub fn symbol(name: &str) -> Option<char> {
    let mut parts = name.split('.');
    let base = parts.next()?;
    let mut modifiers: Vec<&str> = parts.collect();
    modifiers.sort_unstable();
    SYMBOLS.iter().find_map(|&(entry, c)| {
        let mut parts = entry.split('.');
        if parts.next() != Some(base) {
            return None;
        }
        let mut own: Vec<&str> = parts.collect();
        if own.len() != modifiers.len() {
            return None;
        }
        own.sort_unstable();
        (own == modifiers).then_some(c)
    })
}

/// Whether a name is the base of a symbol, with or without modifiers:
/// `arrow` is, as the base of `arrow.r`.
pub fn is_symbol_base(name: &str) -> bool {
    SYMBOLS
        .iter()
        .any(|&(entry, _)| entry.split('.').next() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_in_any_order() {
        assert_eq!(symbol("alpha"), Some('α'));
        assert_eq!(symbol("arrow.r.long"), Some('⟶'));
        assert_eq!(symbol("arrow.long.r"), Some('⟶'));
        assert_eq!(symbol("arrow.r.short"), None);
        assert_eq!(symbol("arrow"), None);
        assert!(is_symbol_base("arrow"));
        assert!(!is_symbol_base("as"));
    }

    #[test]
    fn names_are_unique() {
        for (i, &(a, _)) in SYMBOLS.iter().enumerate() {
            for &(b, _) in &SYMBOLS[i + 1..] {
                fn key(name: &str) -> Vec<&str> {
                    let mut parts: Vec<&str> = name.split('.').collect();
                    parts[1..].sort_unstable();
                    parts
                }
                assert_ne!(key(a), key(b), "{a} and {b} name the same symbol");
            }
        }
    }
}
