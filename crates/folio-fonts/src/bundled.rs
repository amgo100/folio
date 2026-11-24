//! The font families bundled with the compiler (SPEC §2.5).

use std::sync::OnceLock;

use ecow::EcoVec;

use crate::data::FontData;
use crate::file::{FontFile, FontOrigin};

/// The bundled families, as stylesheets name them.
pub const BUNDLED_FAMILIES: &[&str] = &[
    "New Computer Modern",
    "New Computer Modern Math",
    "Source Serif 4",
    "Source Sans 3",
    "Source Code Pro",
    "STIX Two Text",
    "STIX Two Math",
];

macro_rules! bundled_files {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../bundled/", $name)))),*]
    };
}

/// Every bundled file by name. See `bundled/README.md` for versions,
/// sources and licences.
const FILES: &[(&str, &[u8])] = bundled_files![
    "NewCM10-Regular.otf",
    "NewCM10-Bold.otf",
    "NewCM10-Italic.otf",
    "NewCM10-BoldItalic.otf",
    "NewCMMath-Regular.otf",
    "SourceSerif4-Regular.otf",
    "SourceSerif4-Bold.otf",
    "SourceSerif4-It.otf",
    "SourceSerif4-BoldIt.otf",
    "SourceSans3-Regular.otf",
    "SourceSans3-Bold.otf",
    "SourceSans3-It.otf",
    "SourceSans3-BoldIt.otf",
    "SourceCodePro-Regular.otf",
    "SourceCodePro-Bold.otf",
    "SourceCodePro-It.otf",
    "SourceCodePro-BoldIt.otf",
    "STIXTwoText-Regular.ttf",
    "STIXTwoText-Bold.ttf",
    "STIXTwoText-Italic.ttf",
    "STIXTwoText-BoldItalic.ttf",
    "STIXTwoMath-Regular.ttf",
];

/// The bundled font files. Their contents are compiled into the binary,
/// and every call returns the same files, so their faces are parsed once
/// per process and shared by every compile.
pub fn bundled() -> EcoVec<FontFile> {
    static BUNDLED: OnceLock<EcoVec<FontFile>> = OnceLock::new();
    BUNDLED
        .get_or_init(|| {
            FILES
                .iter()
                .map(|&(name, data)| {
                    FontFile::new(
                        FontOrigin::Bundled(name.into()),
                        FontData::from_static(data),
                    )
                })
                .collect()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::FontBook;
    use crate::font::{FontStyle, FontVariant, Permission};

    #[test]
    fn every_bundled_file_parses_and_allows_embedding() {
        for file in bundled() {
            let faces = file.faces().unwrap_or_else(|e| panic!("{file:?}: {e}"));
            assert_eq!(faces.len(), 1, "{file:?}");
            let info = faces[0].info();
            assert!(info.embedding.allows_embedding(), "{file:?}");
            assert_ne!(info.embedding.permission, Permission::Restricted);
            assert!(faces[0].has_char('A'), "{file:?}");
        }
    }

    #[test]
    fn every_bundled_family_has_the_four_styles_or_is_math() {
        let files = bundled();
        let faces: Vec<_> = files
            .iter()
            .map(|f| f.faces().unwrap()[0].clone())
            .collect();
        let book = FontBook::from_infos(faces.iter().map(|f| f.info()));
        for family in BUNDLED_FAMILIES {
            let regular = book.select(family, FontVariant::NORMAL);
            let regular = &faces[regular.unwrap_or_else(|| panic!("{family}"))];
            if family.ends_with("Math") {
                assert!(regular.info().math, "{family}");
                continue;
            }
            let bold_italic = FontVariant {
                weight: 700,
                style: FontStyle::Italic,
                ..FontVariant::NORMAL
            };
            let found = &faces[book.select(family, bold_italic).unwrap()];
            assert_eq!(found.info().variant.weight, 700, "{family}");
            assert_eq!(found.info().variant.style, FontStyle::Italic, "{family}");
        }
    }

    #[test]
    fn bundled_files_are_shared() {
        let a = bundled();
        let b = bundled();
        assert!(a[0].faces().unwrap()[0] == b[0].faces().unwrap()[0]);
        assert_eq!(a, b);
    }
}
