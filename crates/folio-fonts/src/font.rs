//! One face of a font file: names, style, metrics, coverage and embedding
//! permissions.

use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ecow::{EcoString, EcoVec};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider, Tag};

use crate::data::FontData;

/// A face of a font file, parsed once. Cheap to clone.
#[derive(Clone)]
pub struct Font(Arc<Inner>);

struct Inner {
    data: FontData,
    index: u32,
    info: FontInfo,
    metrics: Metrics,
    coverage: Coverage,
}

impl Font {
    /// Parses the face at `index` of a font file, or `None` when it is not
    /// a font the parser reads.
    pub fn new(data: FontData, index: u32) -> Option<Self> {
        let face = FontRef::from_index(&data, index).ok()?;
        let info = FontInfo::from_face(&face)?;
        let metrics = Metrics::from_face(&face);
        let coverage = Coverage::from_face(&face);
        Some(Self(Arc::new(Inner {
            data,
            index,
            info,
            metrics,
            coverage,
        })))
    }

    /// The contents of the file the face is in.
    pub fn data(&self) -> &FontData {
        &self.0.data
    }

    /// The index of the face in its file; 0 unless the file is a
    /// collection.
    pub fn index(&self) -> u32 {
        self.0.index
    }

    /// Names, style and properties.
    pub fn info(&self) -> &FontInfo {
        &self.0.info
    }

    /// Vertical metrics.
    pub fn metrics(&self) -> &Metrics {
        &self.0.metrics
    }

    /// The characters it has glyphs for.
    pub fn coverage(&self) -> &Coverage {
        &self.0.coverage
    }

    /// Whether it has a glyph for a character.
    pub fn has_char(&self, c: char) -> bool {
        self.0.coverage.contains(c)
    }

    /// The parsed face, for the stages that read glyphs and tables.
    pub fn font_ref(&self) -> FontRef<'_> {
        FontRef::from_index(&self.0.data, self.0.index).expect("the face parsed when it was loaded")
    }
}

impl PartialEq for Font {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.index == other.0.index && self.0.data == other.0.data)
    }
}

impl Eq for Font {}

impl Hash for Font {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.index.hash(state);
        self.0.info.postscript_name.hash(state);
        self.0.data.hash(state);
    }
}

impl Debug for Font {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Font({})", self.0.info.postscript_name)
    }
}

/// What lookup and the lockfile know a face by.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontInfo {
    /// The family name stylesheets use: the typographic family, or the
    /// family when there is none.
    pub family: EcoString,
    /// Every family name it answers to, `family` first.
    pub families: EcoVec<EcoString>,
    /// The PostScript name, which identifies the face within its family and
    /// in `folio.lock`.
    pub postscript_name: EcoString,
    /// Weight, style and stretch.
    pub variant: FontVariant,
    /// Whether every glyph has the same advance.
    pub monospace: bool,
    /// Whether it has an OpenType MATH table, as math layout needs.
    pub math: bool,
    /// Whether it is a variable font.
    pub variable: bool,
    /// What its licence allows a document to embed (OS/2 `fsType`).
    pub embedding: Embedding,
}

/// Family names that differ from what stylesheets write, as the bundled New
/// Computer Modern files name their families.
const FAMILY_ALIASES: &[(&str, &str)] = &[
    ("NewComputerModern10", "New Computer Modern"),
    ("NewComputerModernMath", "New Computer Modern Math"),
];

impl FontInfo {
    fn from_face(face: &FontRef<'_>) -> Option<Self> {
        let mut families = EcoVec::new();
        for id in [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME] {
            for name in names(face, id) {
                push_family(&mut families, name);
            }
        }
        let family = families.first()?.clone();
        let postscript_name = names(face, StringId::POSTSCRIPT_NAME)
            .into_iter()
            .next()
            .unwrap_or_else(|| family.clone());
        let attributes = face.attributes();
        let style = match attributes.style {
            skrifa::attribute::Style::Normal => FontStyle::Normal,
            skrifa::attribute::Style::Italic => FontStyle::Italic,
            skrifa::attribute::Style::Oblique(_) => FontStyle::Oblique,
        };
        let weight = attributes.weight.value().round().clamp(1.0, 1000.0) as u16;
        let stretch = (attributes.stretch.ratio() * 1000.0)
            .round()
            .clamp(1.0, 10000.0) as u16;
        let metrics = face.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        Some(Self {
            family,
            families,
            postscript_name,
            variant: FontVariant {
                weight,
                style,
                stretch,
            },
            monospace: metrics.is_monospace,
            math: face.table_data(Tag::new(b"MATH")).is_some(),
            variable: !face.axes().is_empty(),
            embedding: Embedding::from_face(face),
        })
    }
}

/// Adds a family name, after the name stylesheets use for it if it has an
/// alias, unless the list has it.
pub(crate) fn push_family(families: &mut EcoVec<EcoString>, name: EcoString) {
    let alias = FAMILY_ALIASES
        .iter()
        .find(|(raw, _)| *raw == name)
        .map(|(_, alias)| EcoString::from(*alias));
    for name in alias.into_iter().chain([name]) {
        if !families.contains(&name) {
            families.push(name);
        }
    }
}

/// The stretch in thousandths for an OS/2 width class from 1 to 9.
pub(crate) fn stretch(width_class: u16) -> u16 {
    match width_class {
        1 => 500,
        2 => 625,
        3 => 750,
        4 => 875,
        6 => 1125,
        7 => 1250,
        8 => 1500,
        9 => 2000,
        _ => 1000,
    }
}

/// The strings of a name, English first, each once.
fn names(face: &FontRef<'_>, id: StringId) -> Vec<EcoString> {
    let mut found: Vec<(bool, EcoString)> = face
        .localized_strings(id)
        .filter_map(|name| {
            let english = name.language().is_some_and(|l| l.starts_with("en"));
            let text: String = name.chars().collect();
            let text = text.trim();
            (!text.is_empty()).then(|| (!english, EcoString::from(text)))
        })
        .collect();
    // Stable, so that names of one language keep the file's order.
    found.sort_by_key(|(other, _)| *other);
    let mut out: Vec<EcoString> = Vec::new();
    for (_, name) in found {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The weight, style and stretch of a face, or those a stylesheet asks for.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct FontVariant {
    /// From 100 (thin) to 900 (black); 400 is normal and 700 bold.
    pub weight: u16,
    /// Upright, italic or oblique.
    pub style: FontStyle,
    /// The width in thousandths of the normal width: 1000 is normal, 750
    /// condensed.
    pub stretch: u16,
}

impl FontVariant {
    /// Normal weight, upright, normal width.
    pub const NORMAL: Self = Self {
        weight: 400,
        style: FontStyle::Normal,
        stretch: 1000,
    };
}

impl Default for FontVariant {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Whether a face is upright, italic or oblique.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub enum FontStyle {
    /// Upright.
    #[default]
    Normal,
    /// Cursive, designed slanted.
    Italic,
    /// The upright design, slanted.
    Oblique,
}

/// Vertical metrics in font units; divide by [`Self::units_per_em`] for
/// ems.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Metrics {
    /// The units of the em square.
    pub units_per_em: u16,
    /// The typographic ascender.
    pub ascender: i16,
    /// The typographic descender, negative below the baseline.
    pub descender: i16,
    /// The extra space between lines that the font suggests.
    pub line_gap: i16,
    /// The height of capital letters.
    pub cap_height: i16,
    /// The height of lower-case letters.
    pub x_height: i16,
    /// The position of an underline, negative below the baseline.
    pub underline_position: i16,
    /// The thickness of an underline.
    pub underline_thickness: i16,
    /// The position of a strikeout above the baseline.
    pub strikeout_position: i16,
    /// The thickness of a strikeout.
    pub strikeout_thickness: i16,
}

impl Metrics {
    fn from_face(face: &FontRef<'_>) -> Self {
        let m = face.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        let units_per_em = m.units_per_em;
        let units = |v: f32| v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        // Common fallbacks, in fractions of the em, for fonts without
        // the values (as in Typst and browsers).
        let em = |fraction: f32| units(f32::from(units_per_em) * fraction);
        let ascender = units(m.ascent);
        let cap_height = m
            .cap_height
            .map(units)
            .filter(|&h| h > 0)
            .unwrap_or(ascender);
        let x_height = m.x_height.map(units).filter(|&h| h > 0).unwrap_or(em(0.5));
        Self {
            units_per_em,
            ascender,
            descender: units(m.descent),
            line_gap: units(m.leading),
            cap_height,
            x_height,
            underline_position: m.underline.map_or(em(-0.1), |d| units(d.offset)),
            underline_thickness: m.underline.map_or(em(0.06), |d| units(d.thickness)),
            strikeout_position: m.strikeout.map_or(x_height / 2, |d| units(d.offset)),
            strikeout_thickness: m.strikeout.map_or(em(0.06), |d| units(d.thickness)),
        }
    }

    /// A value in font units as a fraction of the em.
    pub fn to_em(&self, units: i16) -> f64 {
        f64::from(units) / f64::from(self.units_per_em)
    }
}

/// What a font's licence allows a document to embed, from the OS/2
/// `fsType` field (SPEC §2.5).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Embedding {
    /// The embedding permission.
    pub permission: Permission,
    /// Whether a subset may be embedded, rather than the whole font.
    pub subsetting: bool,
    /// Whether only bitmaps may be embedded, not outlines.
    pub bitmap_only: bool,
}

/// The embedding permission of a font's licence.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Permission {
    /// It may be embedded and installed on the reader's system.
    Installable,
    /// It may be embedded in documents that the reader can edit.
    Editable,
    /// It may be embedded for viewing and printing only.
    PreviewAndPrint,
    /// It must not be embedded.
    Restricted,
}

impl Embedding {
    fn from_face(face: &FontRef<'_>) -> Self {
        let (version, flags) = face
            .os2()
            .map_or((0, 0), |os2| (os2.version(), os2.fs_type()));
        // The most permissive of the permission bits that are set, as
        // version 2 of the table says. Later versions make the bits
        // exclusive, but fonts in the wild still set several.
        let permission = if flags & 0xF == 0 {
            Permission::Installable
        } else if flags & 8 != 0 {
            Permission::Editable
        } else if flags & 4 != 0 {
            Permission::PreviewAndPrint
        } else {
            Permission::Restricted
        };
        // The subsetting and bitmap bits exist from version 2.
        let later = version >= 2;
        Self {
            permission,
            subsetting: !(later && flags & 0x0100 != 0),
            bitmap_only: later && flags & 0x0200 != 0,
        }
    }

    /// Whether the font's outlines may be embedded in a PDF at all.
    pub fn allows_embedding(&self) -> bool {
        self.permission != Permission::Restricted && !self.bitmap_only
    }
}

/// The characters a face has glyphs for, as sorted ranges of code points.
#[derive(Clone, Default, Eq, PartialEq, Hash)]
pub struct Coverage(EcoVec<(u32, u32)>);

impl Coverage {
    fn from_face(face: &FontRef<'_>) -> Self {
        let codepoints = face
            .charmap()
            .mappings()
            .filter(|(_, glyph)| glyph.to_u32() != 0)
            .map(|(c, _)| c)
            .collect();
        Self::from_codepoints(codepoints)
    }

    /// The coverage of a set of code points.
    pub fn from_codepoints(mut codepoints: Vec<u32>) -> Self {
        codepoints.sort_unstable();
        codepoints.dedup();
        let mut ranges: EcoVec<(u32, u32)> = EcoVec::new();
        for c in codepoints {
            match ranges.make_mut().last_mut() {
                Some((_, end)) if *end + 1 == c => *end = c,
                _ => ranges.push((c, c)),
            }
        }
        Self(ranges)
    }

    /// Whether it covers a character.
    pub fn contains(&self, c: char) -> bool {
        let c = u32::from(c);
        self.0
            .binary_search_by(|&(start, end)| {
                if end < c {
                    std::cmp::Ordering::Less
                } else if start > c {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// The number of characters covered.
    pub fn len(&self) -> usize {
        self.0.iter().map(|&(s, e)| (e - s + 1) as usize).sum()
    }

    /// Whether it covers no character.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Debug for Coverage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Coverage({} characters)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_merges_ranges() {
        let coverage = Coverage::from_codepoints(vec![0x61, 0x63, 0x62, 0x100, 0x62]);
        assert_eq!(coverage.0.as_slice(), &[(0x61, 0x63), (0x100, 0x100)]);
        assert!(coverage.contains('b'));
        assert!(!coverage.contains('d'));
        assert!(coverage.contains('\u{100}'));
        assert_eq!(coverage.len(), 4);
    }
}
