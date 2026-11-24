//! Looking faces up by family and variant.

use std::collections::BTreeMap;

use ecow::{EcoString, EcoVec};

use crate::font::{FontInfo, FontStyle, FontVariant};

/// What lookup knows of a face: its family names and variant. Loaded faces
/// give it through their [`FontInfo`]; system fonts give it before they
/// are read.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FaceKey {
    /// Every family name the face answers to.
    pub families: EcoVec<EcoString>,
    /// Its weight, style and stretch.
    pub variant: FontVariant,
}

impl From<&FontInfo> for FaceKey {
    fn from(info: &FontInfo) -> Self {
        Self {
            families: info.families.clone(),
            variant: info.variant,
        }
    }
}

/// An index of faces by family name, which picks the face of a family
/// closest to a requested variant. Faces are numbered in the order they
/// were added, and among equally close faces the first added wins, so the
/// order decides ties deterministically.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct FontBook {
    variants: Vec<FontVariant>,
    /// Faces by family name in lower case.
    families: BTreeMap<EcoString, Vec<usize>>,
}

impl FontBook {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// A book of faces, numbered in order.
    pub fn from_infos<'a>(infos: impl IntoIterator<Item = &'a FontInfo>) -> Self {
        let mut book = Self::new();
        for info in infos {
            book.push(&FaceKey::from(info));
        }
        book
    }

    /// Adds a face and returns its number.
    pub fn push(&mut self, key: &FaceKey) -> usize {
        let index = self.variants.len();
        self.variants.push(key.variant);
        for family in &key.families {
            let list = self.families.entry(fold(family)).or_default();
            if list.last() != Some(&index) {
                list.push(index);
            }
        }
        index
    }

    /// The number of faces.
    pub fn len(&self) -> usize {
        self.variants.len()
    }

    /// Whether it has no faces.
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty()
    }

    /// Whether a family has any face. Family names are compared without
    /// regard to case.
    pub fn contains_family(&self, family: &str) -> bool {
        self.families.contains_key(&fold(family))
    }

    /// The faces of a family, in the order they were added.
    pub fn family(&self, family: &str) -> &[usize] {
        self.families.get(&fold(family)).map_or(&[], Vec::as_slice)
    }

    /// The family names, in lower case.
    pub fn family_names(&self) -> impl Iterator<Item = &str> {
        self.families.keys().map(EcoString::as_str)
    }

    /// The face of a family closest to a variant: the right style first
    /// (italic falls back to oblique and the reverse, either to upright),
    /// then the nearest weight, then the nearest stretch.
    pub fn select(&self, family: &str, variant: FontVariant) -> Option<usize> {
        self.family(family)
            .iter()
            .copied()
            .min_by_key(|&i| distance(self.variants[i], variant))
    }
}

/// How far a face's variant is from a requested one; smaller is closer.
fn distance(have: FontVariant, want: FontVariant) -> (u8, u16, bool, u16) {
    use FontStyle::*;
    let style = match (want.style, have.style) {
        (a, b) if a == b => 0,
        (Italic, Oblique) | (Oblique, Italic) => 1,
        _ => 2,
    };
    // Prefer the lighter face for weights up to normal and the heavier one
    // above, as CSS does, when two are equally far.
    let weight = have.weight.abs_diff(want.weight);
    let wrong_side = if want.weight <= 500 {
        have.weight > want.weight
    } else {
        have.weight < want.weight
    };
    (
        style,
        weight,
        wrong_side,
        have.stretch.abs_diff(want.stretch),
    )
}

/// A family name as lookup compares it.
fn fold(family: &str) -> EcoString {
    family.trim().to_lowercase().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(family: &str, weight: u16, style: FontStyle) -> FaceKey {
        FaceKey {
            families: EcoVec::from([EcoString::from(family)]),
            variant: FontVariant {
                weight,
                style,
                stretch: 1000,
            },
        }
    }

    #[test]
    fn selects_the_closest_variant() {
        let mut book = FontBook::new();
        let regular = book.push(&key("Serif", 400, FontStyle::Normal));
        let bold = book.push(&key("Serif", 700, FontStyle::Normal));
        let italic = book.push(&key("Serif", 400, FontStyle::Italic));
        let want = |weight, style| FontVariant {
            weight,
            style,
            stretch: 1000,
        };
        assert_eq!(
            book.select("serif", want(400, FontStyle::Normal)),
            Some(regular)
        );
        assert_eq!(
            book.select("SERIF ", want(800, FontStyle::Normal)),
            Some(bold)
        );
        assert_eq!(
            book.select("Serif", want(700, FontStyle::Italic)),
            Some(italic)
        );
        assert_eq!(
            book.select("Serif", want(400, FontStyle::Oblique)),
            Some(italic)
        );
        assert_eq!(
            book.select("Serif", want(550, FontStyle::Normal)),
            Some(bold)
        );
        assert_eq!(book.select("Sans", FontVariant::NORMAL), None);
        assert!(book.contains_family("serif"));
    }
}
