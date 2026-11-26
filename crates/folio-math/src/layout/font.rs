// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/fragment/glyph.rs
// and crates/typst-layout/src/math/shaping.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! A math font: the constants, glyph information and glyph constructions of
//! its OpenType MATH table, read once and kept, and text shaped in it with
//! the math features (`ssty`, `flac`, `dtls`).

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::FontId;
use folio_text::{Feature, ItemKind, Paragraph, Segment, SegmentKind, TextFont};
use skrifa::raw::tables::math::{Math, MathConstant, MathKernCorner, StretchAxis};
use skrifa::raw::{FontRef, TableProvider};
use skrifa::{GlyphId, MetadataProvider};

/// How many shaped texts a font keeps.
const MAX_SHAPED: usize = 4096;

/// The bytes of a font file, shared.
pub type FontBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// A font that formulas are set in: one face, with what math layout reads
/// of its MATH table, read once and shared by every formula set in it.
///
/// Two handles are equal when they are made for the same face id; a build
/// gives each face one id.
#[derive(Clone)]
pub struct MathFont(Arc<Inner>);

struct Inner {
    id: FontId,
    data: FontBytes,
    index: u32,
    text: TextFont,
    upem: f64,
    /// The constants of the MATH table in ems, by [`MathConstant`]'s
    /// numeric value; `None` for a face without a MATH table.
    constants: Option<Vec<f64>>,
    /// The overlap that assembly parts must at least have, in ems.
    min_overlap: f64,
    glyphs: Mutex<HashMap<u16, Arc<GlyphInfo>>>,
    shaped: Mutex<HashMap<ShapeKey, Option<Arc<[Shaped]>>>>,
}

/// What text was shaped with.
#[derive(Clone, Eq, PartialEq, Hash)]
struct ShapeKey {
    text: EcoString,
    features: Features,
}

/// The math features text is shaped with.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub(crate) struct Features {
    /// The script-style level: 0 none, 1 script, 2 scriptscript.
    pub ssty: u8,
    /// Flattened accents, for accents over tall bases.
    pub flac: bool,
    /// Dotless forms, for letters under accents.
    pub dtls: bool,
}

/// A glyph of shaped text, in ems.
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) struct Shaped {
    pub id: u16,
    /// The byte offset in the text of the first character it shows.
    pub cluster: u32,
    pub advance: f64,
    pub x_offset: f64,
    pub y_offset: f64,
}

/// What math layout reads of a glyph, in ems.
#[derive(Debug, Clone, Default)]
pub(crate) struct GlyphInfo {
    /// The horizontal advance.
    pub advance: f64,
    /// How far the glyph's ink reaches above and below the baseline (the
    /// latter positive downwards).
    pub ascent: f64,
    pub descent: f64,
    /// The width of the ink.
    pub ink_width: f64,
    pub ink_height: f64,
    pub italics: Option<f64>,
    pub accent: Option<f64>,
    pub extended: bool,
    /// The kerns of each corner (top right, top left, bottom right, bottom
    /// left): bands of a maximum height and a kern.
    pub kerns: [Option<Kern>; 4],
    pub horizontal: Option<Construction>,
    pub vertical: Option<Construction>,
}

/// The bands of a corner's kern: each up to a height (none for the last),
/// with its kern.
pub(crate) type Kern = Vec<(Option<f64>, f64)>;

/// How a glyph grows along an axis: larger variants, and an assembly of
/// parts for anything larger still.
#[derive(Debug, Clone, Default)]
pub(crate) struct Construction {
    /// The variants, smallest first, with their advances along the axis.
    pub variants: Vec<(u16, f64)>,
    pub assembly: Option<Assembly>,
}

/// An assembly of parts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Assembly {
    pub italics: f64,
    pub parts: Vec<Part>,
}

/// A part of an assembly.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Part {
    pub glyph: u16,
    pub start: f64,
    pub end: f64,
    pub full: f64,
    pub extender: bool,
}

/// A corner of a glyph, for kerning scripts against it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Corner {
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

impl Corner {
    /// The opposite corner, which a script kerns with.
    pub(crate) fn inv(self) -> Self {
        match self {
            Self::TopRight => Self::BottomLeft,
            Self::TopLeft => Self::BottomRight,
            Self::BottomRight => Self::TopLeft,
            Self::BottomLeft => Self::TopRight,
        }
    }
}

/// The constants math layout reads, by name.
#[derive(Debug, Copy, Clone)]
#[allow(missing_docs)]
pub(crate) enum Constant {
    ScriptPercentScaleDown,
    ScriptScriptPercentScaleDown,
    DisplayOperatorMinHeight,
    AxisHeight,
    AccentBaseHeight,
    FlattenedAccentBaseHeight,
    SubscriptShiftDown,
    SubscriptTopMax,
    SubscriptBaselineDropMin,
    SuperscriptShiftUp,
    SuperscriptShiftUpCramped,
    SuperscriptBottomMin,
    SuperscriptBaselineDropMax,
    SubSuperscriptGapMin,
    SuperscriptBottomMaxWithSubscript,
    SpaceAfterScript,
    UpperLimitGapMin,
    UpperLimitBaselineRiseMin,
    LowerLimitGapMin,
    LowerLimitBaselineDropMin,
    FractionNumeratorShiftUp,
    FractionNumeratorDisplayStyleShiftUp,
    FractionDenominatorShiftDown,
    FractionDenominatorDisplayStyleShiftDown,
    FractionNumeratorGapMin,
    FractionNumDisplayStyleGapMin,
    FractionRuleThickness,
    FractionDenominatorGapMin,
    FractionDenomDisplayStyleGapMin,
    OverbarVerticalGap,
    OverbarRuleThickness,
    OverbarExtraAscender,
    UnderbarVerticalGap,
    UnderbarRuleThickness,
    UnderbarExtraDescender,
    RadicalVerticalGap,
    RadicalDisplayStyleVerticalGap,
    RadicalRuleThickness,
    RadicalExtraAscender,
    RadicalKernBeforeDegree,
    RadicalKernAfterDegree,
    RadicalDegreeBottomRaisePercent,
}

impl Constant {
    fn raw(self) -> MathConstant {
        use MathConstant as M;
        match self {
            Self::ScriptPercentScaleDown => M::ScriptPercentScaleDown,
            Self::ScriptScriptPercentScaleDown => M::ScriptScriptPercentScaleDown,
            Self::DisplayOperatorMinHeight => M::DisplayOperatorMinHeight,
            Self::AxisHeight => M::AxisHeight,
            Self::AccentBaseHeight => M::AccentBaseHeight,
            Self::FlattenedAccentBaseHeight => M::FlattenedAccentBaseHeight,
            Self::SubscriptShiftDown => M::SubscriptShiftDown,
            Self::SubscriptTopMax => M::SubscriptTopMax,
            Self::SubscriptBaselineDropMin => M::SubscriptBaselineDropMin,
            Self::SuperscriptShiftUp => M::SuperscriptShiftUp,
            Self::SuperscriptShiftUpCramped => M::SuperscriptShiftUpCramped,
            Self::SuperscriptBottomMin => M::SuperscriptBottomMin,
            Self::SuperscriptBaselineDropMax => M::SuperscriptBaselineDropMax,
            Self::SubSuperscriptGapMin => M::SubSuperscriptGapMin,
            Self::SuperscriptBottomMaxWithSubscript => M::SuperscriptBottomMaxWithSubscript,
            Self::SpaceAfterScript => M::SpaceAfterScript,
            Self::UpperLimitGapMin => M::UpperLimitGapMin,
            Self::UpperLimitBaselineRiseMin => M::UpperLimitBaselineRiseMin,
            Self::LowerLimitGapMin => M::LowerLimitGapMin,
            Self::LowerLimitBaselineDropMin => M::LowerLimitBaselineDropMin,
            Self::FractionNumeratorShiftUp => M::FractionNumeratorShiftUp,
            Self::FractionNumeratorDisplayStyleShiftUp => M::FractionNumeratorDisplayStyleShiftUp,
            Self::FractionDenominatorShiftDown => M::FractionDenominatorShiftDown,
            Self::FractionDenominatorDisplayStyleShiftDown => {
                M::FractionDenominatorDisplayStyleShiftDown
            }
            Self::FractionNumeratorGapMin => M::FractionNumeratorGapMin,
            Self::FractionNumDisplayStyleGapMin => M::FractionNumDisplayStyleGapMin,
            Self::FractionRuleThickness => M::FractionRuleThickness,
            Self::FractionDenominatorGapMin => M::FractionDenominatorGapMin,
            Self::FractionDenomDisplayStyleGapMin => M::FractionDenomDisplayStyleGapMin,
            Self::OverbarVerticalGap => M::OverbarVerticalGap,
            Self::OverbarRuleThickness => M::OverbarRuleThickness,
            Self::OverbarExtraAscender => M::OverbarExtraAscender,
            Self::UnderbarVerticalGap => M::UnderbarVerticalGap,
            Self::UnderbarRuleThickness => M::UnderbarRuleThickness,
            Self::UnderbarExtraDescender => M::UnderbarExtraDescender,
            Self::RadicalVerticalGap => M::RadicalVerticalGap,
            Self::RadicalDisplayStyleVerticalGap => M::RadicalDisplayStyleVerticalGap,
            Self::RadicalRuleThickness => M::RadicalRuleThickness,
            Self::RadicalExtraAscender => M::RadicalExtraAscender,
            Self::RadicalKernBeforeDegree => M::RadicalKernBeforeDegree,
            Self::RadicalKernAfterDegree => M::RadicalKernAfterDegree,
            Self::RadicalDegreeBottomRaisePercent => M::RadicalDegreeBottomRaisePercent,
        }
    }

    /// Whether the constant is a percentage rather than a length.
    fn is_percent(self) -> bool {
        matches!(
            self,
            Self::ScriptPercentScaleDown
                | Self::ScriptScriptPercentScaleDown
                | Self::RadicalDegreeBottomRaisePercent
        )
    }

    /// The value TeX's Computer Modern would give, for a font without a
    /// MATH table: in ems, or as a fraction for a percentage.
    fn fallback(self) -> f64 {
        match self {
            Self::ScriptPercentScaleDown => 0.7,
            Self::ScriptScriptPercentScaleDown => 0.5,
            Self::DisplayOperatorMinHeight => 1.3,
            Self::AxisHeight => 0.25,
            Self::AccentBaseHeight | Self::FlattenedAccentBaseHeight => 0.45,
            Self::SubscriptShiftDown => 0.15,
            Self::SubscriptTopMax => 0.35,
            Self::SubscriptBaselineDropMin => 0.2,
            Self::SuperscriptShiftUp => 0.36,
            Self::SuperscriptShiftUpCramped => 0.29,
            Self::SuperscriptBottomMin => 0.11,
            Self::SuperscriptBaselineDropMax => 0.39,
            Self::SubSuperscriptGapMin => 0.16,
            Self::SuperscriptBottomMaxWithSubscript => 0.35,
            Self::SpaceAfterScript => 0.05,
            Self::UpperLimitGapMin | Self::LowerLimitGapMin => 0.17,
            Self::UpperLimitBaselineRiseMin => 0.11,
            Self::LowerLimitBaselineDropMin => 0.6,
            Self::FractionNumeratorShiftUp => 0.39,
            Self::FractionNumeratorDisplayStyleShiftUp => 0.68,
            Self::FractionDenominatorShiftDown => 0.35,
            Self::FractionDenominatorDisplayStyleShiftDown => 0.69,
            Self::FractionNumeratorGapMin | Self::FractionDenominatorGapMin => 0.04,
            Self::FractionNumDisplayStyleGapMin | Self::FractionDenomDisplayStyleGapMin => 0.12,
            Self::FractionRuleThickness
            | Self::OverbarRuleThickness
            | Self::UnderbarRuleThickness
            | Self::RadicalRuleThickness
            | Self::OverbarExtraAscender
            | Self::UnderbarExtraDescender
            | Self::RadicalExtraAscender => 0.04,
            Self::OverbarVerticalGap | Self::UnderbarVerticalGap => 0.12,
            Self::RadicalVerticalGap => 0.05,
            Self::RadicalDisplayStyleVerticalGap => 0.15,
            Self::RadicalKernBeforeDegree => 0.28,
            Self::RadicalKernAfterDegree => -0.56,
            Self::RadicalDegreeBottomRaisePercent => 0.6,
        }
    }
}

impl MathFont {
    /// The face at `index` of a font file, shaped through `text`, the
    /// shaping handle of the same face, and named `id` in the output.
    /// `None` when the bytes hold no face at that index.
    pub fn new(data: FontBytes, index: u32, text: TextFont, id: FontId) -> Option<Self> {
        let font = FontRef::from_index((*data).as_ref(), index).ok()?;
        let upem = f64::from(font.head().ok()?.units_per_em().max(1));
        let math = font.math().ok();
        let constants = math.as_ref().and_then(|math| {
            let table = math.math_constants().ok()?;
            Some(
                MathConstant::ALL
                    .iter()
                    .map(|&c| f64::from(table.constant(c)))
                    .collect::<Vec<f64>>(),
            )
        });
        let min_overlap = math
            .as_ref()
            .and_then(|m| m.math_variants().ok())
            .map_or(0.0, |v| {
                f64::from(v.min_connector_overlap().to_u16()) / upem
            });
        Some(Self(Arc::new(Inner {
            id,
            data,
            index,
            text,
            upem,
            constants,
            min_overlap,
            glyphs: Mutex::new(HashMap::new()),
            shaped: Mutex::new(HashMap::new()),
        })))
    }

    /// The id glyph runs name the face by.
    pub fn id(&self) -> FontId {
        self.0.id
    }

    /// Whether the face has a MATH table: whether it is made for maths.
    pub fn has_math(&self) -> bool {
        self.0.constants.is_some()
    }

    /// A constant: in ems, or as a fraction for a percentage.
    pub(crate) fn constant(&self, constant: Constant) -> f64 {
        let Some(values) = &self.0.constants else {
            return constant.fallback();
        };
        let raw = values[constant.raw() as usize];
        if constant.is_percent() {
            raw / 100.0
        } else {
            raw / self.0.upem
        }
    }

    /// The overlap assembly parts must at least have, in ems.
    pub(crate) fn min_overlap(&self) -> f64 {
        self.0.min_overlap
    }

    fn em(&self, units: impl Into<f64>) -> f64 {
        units.into() / self.0.upem
    }

    /// What math layout reads of a glyph, read once.
    pub(crate) fn glyph(&self, id: u16) -> Arc<GlyphInfo> {
        if let Some(info) = lock(&self.0.glyphs).get(&id) {
            return info.clone();
        }
        let info = Arc::new(self.read_glyph(id));
        lock(&self.0.glyphs).insert(id, info.clone());
        info
    }

    fn read_glyph(&self, id: u16) -> GlyphInfo {
        let Ok(font) = FontRef::from_index((*self.0.data).as_ref(), self.0.index) else {
            return GlyphInfo::default();
        };
        let gid = GlyphId::new(u32::from(id));
        let metrics = font.glyph_metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        let mut info = GlyphInfo {
            advance: self.em(metrics.advance_width(gid).unwrap_or(0.0)),
            ..GlyphInfo::default()
        };
        if let Some(bounds) = metrics.bounds(gid) {
            info.ascent = self.em(bounds.y_max);
            info.descent = -self.em(bounds.y_min);
            info.ink_width = self.em(bounds.x_max - bounds.x_min);
            info.ink_height = self.em(bounds.y_max - bounds.y_min);
        }
        let Ok(math) = font.math() else {
            return info;
        };
        self.read_math(&math, gid, &mut info);
        info
    }

    fn read_math(&self, math: &Math<'_>, gid: GlyphId, info: &mut GlyphInfo) {
        if let Ok(glyph_info) = math.math_glyph_info() {
            info.italics = glyph_info
                .math_italics_correction_info()
                .and_then(|t| t.ok())
                .and_then(|t| t.correction(gid))
                .map(|v| self.em(v));
            info.accent = glyph_info
                .math_top_accent_attachment()
                .and_then(|t| t.ok())
                .and_then(|t| t.attachment(gid))
                .map(|v| self.em(v));
            info.extended = glyph_info.is_extended_shape(gid);
            if let Some(Ok(kerns)) = glyph_info.math_kern_info() {
                let corners = [
                    MathKernCorner::TopRight,
                    MathKernCorner::TopLeft,
                    MathKernCorner::BottomRight,
                    MathKernCorner::BottomLeft,
                ];
                for (slot, corner) in info.kerns.iter_mut().zip(corners) {
                    *slot = kerns.kern(gid, corner).map(|kern| {
                        kern.entries_for_ppem(0)
                            .map(|e| {
                                (
                                    e.max_height.map(|h| self.em(h.value)),
                                    self.em(e.kern.value),
                                )
                            })
                            .collect()
                    });
                }
            }
        }
        if let Ok(variants) = math.math_variants() {
            for (axis, slot) in [
                (StretchAxis::Horizontal, &mut info.horizontal),
                (StretchAxis::Vertical, &mut info.vertical),
            ] {
                let Some(construction) = variants.glyph_construction(gid, axis) else {
                    continue;
                };
                let variants = construction
                    .math_glyph_variant_records()
                    .iter()
                    .map(|v| {
                        (
                            v.variant_glyph().to_u16(),
                            self.em(v.advance_measurement().to_u16()),
                        )
                    })
                    .collect();
                let assembly =
                    construction
                        .glyph_assembly()
                        .and_then(|a| a.ok())
                        .map(|a| Assembly {
                            italics: self.em(a.italics_correction().value().to_i16()),
                            parts: a
                                .part_records()
                                .iter()
                                .map(|p| Part {
                                    glyph: p.glyph_id().to_u16(),
                                    start: self.em(p.start_connector_length().to_u16()),
                                    end: self.em(p.end_connector_length().to_u16()),
                                    full: self.em(p.full_advance().to_u16()),
                                    extender: p.part_flags().contains(
                                        skrifa::raw::tables::math::PartFlags::EXTENDER_FLAG,
                                    ),
                                })
                                .collect(),
                        });
                *slot = Some(Construction { variants, assembly });
            }
        }
    }

    /// The kern at a corner of a glyph at a height in ems, or zero where
    /// the font gives none.
    pub(crate) fn kern_at_height(&self, id: u16, corner: Corner, height: f64) -> f64 {
        let info = self.glyph(id);
        let index = match corner {
            Corner::TopRight => 0,
            Corner::TopLeft => 1,
            Corner::BottomRight => 2,
            Corner::BottomLeft => 3,
        };
        let Some(bands) = &info.kerns[index] else {
            return 0.0;
        };
        // The first band whose maximum height the height does not pass.
        bands
            .iter()
            .find(|(max, _)| max.is_none_or(|max| height <= max))
            .or(bands.last())
            .map_or(0.0, |(_, kern)| *kern)
    }

    /// Text shaped with math features, or `None` when the face has no glyph
    /// for one of its characters.
    pub(crate) fn shape(&self, text: &str, features: Features) -> Option<Arc<[Shaped]>> {
        let key = ShapeKey {
            text: text.into(),
            features,
        };
        if let Some(shaped) = lock(&self.0.shaped).get(&key) {
            return shaped.clone();
        }
        let shaped = self.shape_uncached(text, features);
        let mut cache = lock(&self.0.shaped);
        // The cache only saves shaping again: a long session that shapes
        // many texts starts it afresh rather than let it grow.
        if cache.len() >= MAX_SHAPED {
            cache.clear();
        }
        cache.insert(key, shaped.clone());
        shaped
    }

    fn shape_uncached(&self, text: &str, features: Features) -> Option<Arc<[Shaped]>> {
        if text.is_empty() {
            return Some(Arc::from(Vec::new()));
        }
        let mut list = EcoVec::new();
        if features.ssty > 0 {
            list.push(Feature {
                tag: *b"ssty",
                value: u32::from(features.ssty),
            });
        }
        if features.flac {
            list.push(Feature {
                tag: *b"flac",
                value: 1,
            });
        }
        if features.dtls {
            list.push(Feature {
                tag: *b"dtls",
                value: 1,
            });
        }
        // A combining mark alone is shaped after a no-break space, so that
        // the shaper takes it for a cluster of its own and draws no dotted
        // circle; only the mark's glyph is kept, at its own advance.
        let mark = {
            let mut chars = text.chars();
            matches!((chars.next(), chars.next()), (Some(c), None) if is_mark(c))
        };
        let shaped_text: EcoString = if mark {
            eco_format!("\u{A0}{text}")
        } else {
            text.into()
        };
        let paragraph = Paragraph {
            text: shaped_text.clone(),
            segments: EcoVec::from([Segment {
                range: 0..shaped_text.len(),
                kind: SegmentKind::Text {
                    font: Some(self.0.text.clone()),
                    features: list,
                },
                lang: None,
                hyphenate: false,
            }]),
        };
        let shaped = folio_text::shape(&paragraph);
        let mut out = Vec::new();
        for item in &shaped.items {
            let ItemKind::Glyphs {
                glyphs,
                units_per_em,
            } = &item.kind
            else {
                return None;
            };
            let upem = f64::from((*units_per_em).max(1));
            for (i, g) in glyphs.iter().enumerate() {
                if g.id == 0 {
                    return None;
                }
                let id = u16::try_from(g.id).ok()?;
                if mark {
                    // The space comes first.
                    if i == 0 {
                        continue;
                    }
                    out.push(Shaped {
                        id,
                        cluster: 0,
                        advance: self.glyph(id).advance,
                        x_offset: 0.0,
                        y_offset: 0.0,
                    });
                    continue;
                }
                out.push(Shaped {
                    id,
                    cluster: g.cluster,
                    advance: f64::from(g.x_advance) / upem,
                    x_offset: f64::from(g.x_offset) / upem,
                    y_offset: f64::from(g.y_offset) / upem,
                });
            }
        }
        Some(Arc::from(out))
    }

    /// Whether the face has a glyph for every character of a text.
    pub fn covers(&self, text: &str) -> bool {
        let Ok(font) = FontRef::from_index((*self.0.data).as_ref(), self.0.index) else {
            return false;
        };
        let charmap = font.charmap();
        text.chars().all(|c| charmap.map(c).is_some())
    }

    /// Calls `retry` with the features that turn `flac` and `ssty` off in
    /// turn, from the strongest setting below `features`, until it returns
    /// `true`.
    pub(crate) fn fallback_features(features: Features, mut retry: impl FnMut(Features) -> bool) {
        const OPTIONS: [(bool, u8); 6] = [
            (true, 2),
            (true, 1),
            (true, 0),
            (false, 2),
            (false, 1),
            (false, 0),
        ];
        for (flac, ssty) in OPTIONS {
            if (flac && !features.flac) || ssty > features.ssty {
                continue;
            }
            if (flac, ssty) == (features.flac, features.ssty) {
                continue;
            }
            if retry(Features {
                ssty,
                flac,
                dtls: features.dtls,
            }) {
                break;
            }
        }
    }
}

impl PartialEq for MathFont {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id && self.0.text == other.0.text
    }
}

impl Eq for MathFont {}

impl Hash for MathFont {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
        self.0.index.hash(state);
    }
}

impl Debug for MathFont {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "MathFont({})", self.0.id.0)
    }
}

/// Whether a character is a combining mark, which shapers attach to the
/// character before it.
fn is_mark(c: char) -> bool {
    matches!(
        c,
        '\u{300}'..='\u{36F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
