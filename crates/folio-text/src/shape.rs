//! Shaping with `harfrust`: text of one font, direction, script and
//! language to positioned glyphs.

use std::collections::HashMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use ecow::EcoString;
use harfrust::{
    Buffer, BufferFlags, Direction, GlyphId, Language, ShapeOptions, ShapePlan, ShaperFont, Tag,
};

use crate::lang::Lang;
use crate::script::{Script, iso_code};

/// A font face that text can be shaped in.
///
/// Make one per face and keep it: it holds the tables the shaper prepares
/// from the font, and the shaping plans made for it, which every paragraph
/// set in the face shares. Two handles are equal when they are made from
/// the same bytes in memory and the same index, so comparing them never
/// compares font files.
#[derive(Clone)]
pub struct TextFont(Arc<FontInner>);

struct FontInner {
    font: harfrust::Font,
    /// The address and length of the font's bytes, which identify it while
    /// this handle keeps them alive.
    addr: usize,
    len: usize,
    index: u32,
    units_per_em: u16,
    plans: Mutex<HashMap<PlanKey, Arc<ShapePlan>>>,
}

/// What a shaping plan is made for.
#[derive(Clone, Eq, PartialEq, Hash)]
struct PlanKey {
    rtl: bool,
    script: Option<[u8; 4]>,
    lang: Option<Lang>,
    features: Vec<Feature>,
}

impl TextFont {
    /// The face at `index` in a font file's bytes. `None` when the bytes
    /// hold no font that the shaper reads at that index.
    pub fn new(data: Arc<dyn AsRef<[u8]> + Send + Sync>, index: u32) -> Option<Self> {
        let bytes: &[u8] = (*data).as_ref();
        let (addr, len) = (bytes.as_ptr() as usize, bytes.len());
        let font = harfrust::Font::new(data, index)?;
        let units_per_em = font.units_per_em();
        Some(Self(Arc::new(FontInner {
            font,
            addr,
            len,
            index,
            units_per_em,
            plans: Mutex::new(HashMap::new()),
        })))
    }

    /// The size of the face's em square in font units, which glyph
    /// advances and offsets are measured in.
    pub fn units_per_em(&self) -> u16 {
        self.0.units_per_em
    }

    /// The address of the face's bytes.
    pub(crate) fn addr(&self) -> usize {
        self.0.addr
    }

    /// Whether a handle other than this one refers to the face.
    pub(crate) fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// The name of a glyph, from the font's `post` table or CFF charset.
    pub fn glyph_name(&self, id: u32) -> Option<EcoString> {
        let name = self.0.font.glyph_name(GlyphId::new(id))?;
        Some(name.as_str().into())
    }

    /// The plan for shaping text in this face with these settings, made
    /// once per process.
    fn plan(&self, key: PlanKey) -> Arc<ShapePlan> {
        let mut plans = self.0.plans.lock().unwrap_or_else(|e| e.into_inner());
        plans
            .entry(key)
            .or_insert_with_key(|key| {
                let features: Vec<harfrust::Feature> = key
                    .features
                    .iter()
                    .copied()
                    .map(Feature::to_harfrust)
                    .collect();
                let language = key.lang.as_ref().and_then(to_language);
                Arc::new(ShapePlan::new(
                    &self.0.font,
                    if key.rtl {
                        Direction::RightToLeft
                    } else {
                        Direction::LeftToRight
                    },
                    key.script
                        .and_then(|s| harfrust::Script::from_iso15924_tag(Tag::new(&s))),
                    language.as_ref(),
                    &features,
                ))
            })
            .clone()
    }
}

impl PartialEq for TextFont {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.addr == other.0.addr
                && self.0.len == other.0.len
                && self.0.index == other.0.index)
    }
}

impl Eq for TextFont {}

impl Hash for TextFont {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.addr.hash(state);
        self.0.len.hash(state);
        self.0.index.hash(state);
    }
}

impl Debug for TextFont {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "TextFont({} bytes, {})", self.0.len, self.0.index)
    }
}

/// An OpenType feature setting, such as `smcp` or `liga=0`.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Feature {
    /// The feature's tag, padded with spaces to four bytes.
    pub tag: [u8; 4],
    /// Its value: 1 turns it on, 0 off, and other numbers choose an
    /// alternate.
    pub value: u32,
}

impl Feature {
    /// Parses a setting: a tag (`smcp`, `+smcp`), a tag turned off
    /// (`-liga`), or a tag with a value (`ss01=1`, `salt=2`). A tag has one
    /// to four printable ASCII characters. `None` when the text is not a
    /// setting.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (tag, value) = if let Some(tag) = text.strip_prefix('-') {
            (tag, 0)
        } else if let Some((tag, value)) = text.split_once('=') {
            (tag.trim(), value.trim().parse().ok()?)
        } else {
            (text.strip_prefix('+').unwrap_or(text), 1)
        };
        if tag.is_empty() || tag.len() > 4 || !tag.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        let mut bytes = [b' '; 4];
        bytes[..tag.len()].copy_from_slice(tag.as_bytes());
        Some(Self { tag: bytes, value })
    }

    fn to_harfrust(self) -> harfrust::Feature {
        harfrust::Feature::new(Tag::new(&self.tag), self.value, ..)
    }
}

impl Display for Feature {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let tag = String::from_utf8_lossy(&self.tag);
        write!(f, "{}={}", tag.trim_end(), self.value)
    }
}

impl FromStr for Feature {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Self::parse(s).ok_or(())
    }
}

/// A shaped glyph, measured in font units of its face.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Glyph {
    /// The glyph's id in its face.
    pub id: u32,
    /// How far the pen moves after the glyph.
    pub x_advance: i32,
    /// The horizontal offset from the pen position.
    pub x_offset: i32,
    /// The vertical offset from the baseline, upwards.
    pub y_offset: i32,
    /// The byte offset in the paragraph's text of the first character the
    /// glyph shows. Glyphs that show the same characters (a ligature's
    /// parts, a base and its marks) share it.
    pub cluster: u32,
    /// Whether the text may be cut before this glyph's cluster without
    /// shaping again: a line that ends or starts here gets the same glyphs
    /// as shaping its part alone would give.
    pub safe_to_break: bool,
}

/// What a run of text is shaped with.
pub(crate) struct RunProps<'a> {
    pub font: &'a TextFont,
    pub rtl: bool,
    pub script: Script,
    pub lang: Option<&'a Lang>,
    pub features: &'a [Feature],
}

/// How many characters on each side of a run the shaper sees as context.
const CONTEXT: usize = 5;

/// Shapes `range` of a paragraph's text. The text around the run is its
/// context, so that joining and contextual forms continue across the run's
/// ends. The glyphs are in visual order: reversed for a right-to-left run.
pub(crate) fn shape_run(text: &str, range: Range<usize>, props: &RunProps<'_>) -> Vec<Glyph> {
    let script = match props.script {
        Script::Common | Script::Inherited | Script::Unknown => None,
        script => Some(tag_bytes(iso_code(script))),
    };
    // The last setting of a tag wins, as in a stylesheet.
    let mut features: Vec<Feature> = Vec::with_capacity(props.features.len());
    for feature in props.features.iter().rev() {
        if !features.iter().any(|f| f.tag == feature.tag) {
            features.push(*feature);
        }
    }
    features.reverse();
    let key = PlanKey {
        rtl: props.rtl,
        script,
        lang: props.lang.cloned(),
        features: features.clone(),
    };
    let plan = props.font.plan(key);

    let mut buffer = Buffer::new();
    for (i, c) in text[range.clone()].char_indices() {
        buffer.push(u32::from(c), (range.start + i) as u32);
    }
    buffer.set_direction(if props.rtl {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    });
    buffer.set_script(script.and_then(|s| harfrust::Script::from_iso15924_tag(Tag::new(&s))));
    buffer.set_language(props.lang.and_then(to_language));
    let before = &text[..range.start];
    let context_start = before
        .char_indices()
        .rev()
        .nth(CONTEXT - 1)
        .map_or(0, |(i, _)| i);
    buffer.set_pre_context(&before[context_start..]);
    buffer.set_post_context(&text[range.end..]);
    let mut flags = BufferFlags::empty();
    if range.start == 0 {
        flags |= BufferFlags::BEGINNING_OF_TEXT;
    }
    if range.end == text.len() {
        flags |= BufferFlags::END_OF_TEXT;
    }
    buffer.set_flags(flags);

    let hb_features: Vec<harfrust::Feature> =
        features.iter().copied().map(Feature::to_harfrust).collect();
    let shaper = ShaperFont::new(&props.font.0.font);
    let options = ShapeOptions::new().plan(Some(&plan)).features(&hb_features);
    if harfrust::shape(&shaper, &mut buffer, options).is_err() {
        return Vec::new();
    }

    buffer
        .glyph_infos()
        .iter()
        .zip(buffer.glyph_positions())
        .map(|(info, pos)| Glyph {
            id: info.glyph_id,
            x_advance: pos.x_advance,
            x_offset: pos.x_offset,
            y_offset: pos.y_offset,
            cluster: info.cluster,
            safe_to_break: !info.unsafe_to_break(),
        })
        .collect()
}

fn tag_bytes(code: &str) -> [u8; 4] {
    let mut out = [b' '; 4];
    for (o, b) in out.iter_mut().zip(code.bytes()) {
        *o = b;
    }
    out
}

fn to_language(lang: &Lang) -> Option<Language> {
    Language::from_str(lang.as_str()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_setting_of_a_tag_wins() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../folio-fonts/bundled/SourceSerif4-Regular.otf"
        ))
        .unwrap();
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(data);
        let font = TextFont::new(data, 0).unwrap();
        let shape = |features: &[&str]| {
            let features: Vec<Feature> = features.iter().map(|f| f.parse().unwrap()).collect();
            let props = RunProps {
                font: &font,
                rtl: false,
                script: Script::Latin,
                lang: None,
                features: &features,
            };
            shape_run("office", 0..6, &props).len()
        };
        // `ffi` is one glyph with ligatures, three without.
        assert_eq!(shape(&[]), 4);
        assert_eq!(shape(&["liga", "-liga"]), 6);
        assert_eq!(shape(&["-liga", "liga"]), 4);
    }

    #[test]
    fn parses_features() {
        let f = |s| Feature::parse(s).map(|f| f.to_string());
        assert_eq!(f("smcp").as_deref(), Some("smcp=1"));
        assert_eq!(f("+kern").as_deref(), Some("kern=1"));
        assert_eq!(f("-liga").as_deref(), Some("liga=0"));
        assert_eq!(f("salt=2").as_deref(), Some("salt=2"));
        assert_eq!(f("cv1").as_deref(), Some("cv1=1"));
        assert_eq!(Feature::parse("cv1").unwrap().tag, *b"cv1 ");
        assert!(Feature::parse("").is_none());
        assert!(Feature::parse("toolong").is_none());
        assert!(Feature::parse("liga=x").is_none());
        assert!(Feature::parse("li ga").is_none());
    }
}
