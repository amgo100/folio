//! Paragraphs: the text of a paragraph with its fonts and languages, and
//! the same paragraph itemised, shaped and with its break opportunities.

use std::fmt::Write;
use std::ops::Range;

use ecow::{EcoString, EcoVec};

use crate::bidi;
use crate::breaks::{self, Break, BreakKind, LangSpan};
use crate::lang::{Dir, Lang};
use crate::script::{Script, iso_code, script_runs};
use crate::shape::{Feature, Glyph, RunProps, TextFont, shape_run};

/// The object replacement character, which stands for an inline object
/// (an image, inline math, a note's anchor) in a paragraph's text.
pub const OBJECT: char = '\u{FFFC}';

/// A paragraph to shape: its text, and segments that give every byte of it
/// a font, a language and the other settings shaping and breaking read.
///
/// A paragraph is also the key under which its shaped form is memoised
/// ([`crate::Cache`]), so it holds only what decides that form: equal
/// paragraphs shape alike.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Paragraph {
    /// The text. A forced line break is U+2028 LINE SEPARATOR, and an
    /// inline object is [`OBJECT`].
    pub text: EcoString,
    /// The segments, in order, covering the text without gaps or overlaps.
    /// Each starts and ends at a character boundary.
    pub segments: EcoVec<Segment>,
}

/// A range of a paragraph set alike.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Segment {
    /// The byte range in the paragraph's text.
    pub range: Range<usize>,
    /// What the range holds.
    pub kind: SegmentKind,
    /// Its language, if it has one. It chooses language-specific glyph
    /// forms, hyphenation patterns and line-end rules.
    pub lang: Option<Lang>,
    /// Whether its words may be hyphenated.
    pub hyphenate: bool,
}

/// What a segment holds.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum SegmentKind {
    /// Text, set in a font: the one font fallback chose for it, or none
    /// when no font of its chain has its characters.
    Text {
        /// The font.
        font: Option<TextFont>,
        /// The OpenType features turned on or off.
        features: EcoVec<Feature>,
    },
    /// Inline objects, one [`OBJECT`] each, which layout sizes itself.
    Object,
}

/// A paragraph itemised, shaped and with its break opportunities.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Shaped {
    /// The paragraph it was made from.
    pub paragraph: Paragraph,
    /// The paragraph's embedding level: 0 when it runs left to right, 1
    /// when it runs right to left.
    pub level: u8,
    /// The items, in logical order, covering the text.
    pub items: EcoVec<Item>,
    /// The line-break opportunities, in order, ending with the mandatory
    /// break at the end of the text.
    pub breaks: EcoVec<Break>,
}

/// A range of a paragraph in one segment, one embedding level and one
/// script.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Item {
    /// The byte range in the paragraph's text.
    pub range: Range<usize>,
    /// The index of the segment it belongs to.
    pub segment: usize,
    /// Its embedding level: even runs left to right, odd right to left.
    pub level: u8,
    /// Its script.
    pub script: Script,
    /// What it holds.
    pub kind: ItemKind,
}

/// What an item holds.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ItemKind {
    /// Shaped text: glyphs in visual order, measured in font units of the
    /// segment's font.
    Glyphs {
        /// The glyphs.
        glyphs: EcoVec<Glyph>,
        /// The size of the font's em square in font units.
        units_per_em: u16,
    },
    /// Text that no font has, which is drawn as nothing; the font stage
    /// reports it.
    Missing,
    /// Inline objects.
    Object,
}

impl Item {
    /// Whether the item runs right to left.
    pub fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

impl Paragraph {
    /// The direction of the paragraph's first segment's language, which
    /// a paragraph without strongly directional text runs in.
    fn fallback_dir(&self) -> Dir {
        self.segments
            .iter()
            .find_map(|s| s.lang.as_ref())
            .map_or(Dir::Ltr, Lang::dir)
    }

    /// Checks that the segments cover the text at character boundaries.
    fn check(&self) {
        let mut pos = 0;
        for segment in &self.segments {
            assert_eq!(segment.range.start, pos, "segments must cover the text");
            assert!(
                self.text.is_char_boundary(segment.range.end),
                "segments must end at character boundaries"
            );
            pos = segment.range.end;
        }
        assert_eq!(pos, self.text.len(), "segments must cover the text");
    }
}

/// Itemises and shapes a paragraph and finds its break opportunities,
/// without memoising it. See [`crate::Cache::shape`].
///
/// # Panics
///
/// When the segments do not cover the text at character boundaries.
pub fn shape(paragraph: &Paragraph) -> Shaped {
    paragraph.check();
    let text = paragraph.text.as_str();
    let (level, levels) = bidi::levels(text, paragraph.fallback_dir());
    let scripts = script_runs(text);

    // Item boundaries: segments, levels and scripts.
    let mut cuts: Vec<usize> = Vec::new();
    cuts.extend(paragraph.segments.iter().map(|s| s.range.start));
    cuts.extend(scripts.iter().map(|(r, _)| r.start));
    let mut prev_level = None;
    for (i, c) in text.char_indices() {
        if prev_level != Some(levels[i]) {
            cuts.push(i);
            prev_level = Some(levels[i]);
        }
        // Objects are items of their own.
        if c == OBJECT {
            cuts.push(i);
            cuts.push(i + c.len_utf8());
        }
    }
    cuts.push(text.len());
    cuts.sort_unstable();
    cuts.dedup();

    let mut items = EcoVec::new();
    let mut segment = 0;
    let mut script = 0;
    for pair in cuts.windows(2) {
        let range = pair[0]..pair[1];
        if range.is_empty() {
            continue;
        }
        while paragraph.segments[segment].range.end <= range.start {
            segment += 1;
        }
        while scripts[script].0.end <= range.start {
            script += 1;
        }
        let seg = &paragraph.segments[segment];
        let level = levels[range.start];
        let script = scripts[script].1;
        let kind = match &seg.kind {
            SegmentKind::Object => ItemKind::Object,
            SegmentKind::Text { font: None, .. } => ItemKind::Missing,
            SegmentKind::Text {
                font: Some(font),
                features,
            } => {
                let props = RunProps {
                    font,
                    rtl: level % 2 == 1,
                    script,
                    lang: seg.lang.as_ref(),
                    features,
                };
                ItemKind::Glyphs {
                    glyphs: shape_run(text, range.clone(), &props).into(),
                    units_per_em: font.units_per_em(),
                }
            }
        };
        items.push(Item {
            range,
            segment,
            level,
            script,
            kind,
        });
    }

    let spans: Vec<LangSpan<'_>> = paragraph
        .segments
        .iter()
        .map(|s| LangSpan {
            range: s.range.clone(),
            lang: s.lang.as_ref(),
            hyphenate: s.hyphenate,
        })
        .collect();
    let breaks = breaks::breaks(text, &spans).into();
    Shaped {
        paragraph: paragraph.clone(),
        level,
        items,
        breaks,
    }
}

impl Shaped {
    /// The text.
    pub fn text(&self) -> &str {
        &self.paragraph.text
    }

    /// The parts of the items of one line, in visual order from left to
    /// right: each item's range cut to the line, split where whitespace at
    /// the line's end takes the paragraph's level, with its resolved level
    /// (UAX #9, rules L1 and L2). A line is a range from one break
    /// opportunity, or the start, to the next.
    pub fn visual_runs(&self, line: Range<usize>) -> Vec<(Range<usize>, u8)> {
        bidi::reorder(
            self.text(),
            self.level,
            self.items.iter().map(|i| (i.range.clone(), i.level)),
            line,
        )
    }

    /// A description of the paragraph for golden tests and debugging: its
    /// level, then one line per item with its text, level, script, language
    /// and glyphs (by name where the font names them, with advances and
    /// offsets in font units), then the text with its break opportunities
    /// marked: `|` normal, `-` hyphenation, `=` repeated hyphen, `¶`
    /// mandatory.
    pub fn repr(&self) -> String {
        let text = self.text();
        let mut out = String::new();
        let dir = if self.level % 2 == 1 { "rtl" } else { "ltr" };
        writeln!(out, "paragraph {dir}").unwrap();
        for item in &self.items {
            let segment = &self.paragraph.segments[item.segment];
            let lang = segment.lang.as_ref().map_or("-", Lang::as_str);
            write!(
                out,
                "  {:?} level {} {} {lang}",
                &text[item.range.clone()],
                item.level,
                iso_code(item.script),
            )
            .unwrap();
            match &item.kind {
                ItemKind::Object => out.push_str(" object\n"),
                ItemKind::Missing => out.push_str(" missing\n"),
                ItemKind::Glyphs { glyphs, .. } => {
                    out.push('\n');
                    let SegmentKind::Text {
                        font: Some(font), ..
                    } = &segment.kind
                    else {
                        unreachable!("glyphs come from a font")
                    };
                    for glyph in glyphs {
                        let name = font
                            .glyph_name(glyph.id)
                            .unwrap_or_else(|| ecow::eco_format!("gid{}", glyph.id));
                        write!(out, "    {name} @{} +{}", glyph.cluster, glyph.x_advance).unwrap();
                        if glyph.x_offset != 0 || glyph.y_offset != 0 {
                            write!(out, " ({}, {})", glyph.x_offset, glyph.y_offset).unwrap();
                        }
                        if !glyph.safe_to_break {
                            out.push_str(" unsafe");
                        }
                        out.push('\n');
                    }
                }
            }
        }
        writeln!(out, "breaks {}", mark_breaks(text, &self.breaks)).unwrap();
        out
    }
}

/// The text with its break opportunities marked, as [`Shaped::repr`]
/// writes it, with line separators as `⏎`.
pub fn mark_breaks(text: &str, breaks: &[Break]) -> String {
    let mut out = String::new();
    let mut next = breaks.iter().peekable();
    for (i, c) in text.char_indices().chain([(text.len(), '\0')]) {
        while let Some(b) = next.next_if(|b| b.offset <= i) {
            out.push(match b.kind {
                BreakKind::Normal => '|',
                BreakKind::Hyphenate => '-',
                BreakKind::RepeatHyphen => '=',
                BreakKind::Mandatory => '¶',
            });
        }
        match c {
            '\0' => {}
            '\u{2028}' => out.push('⏎'),
            _ => out.push(c),
        }
    }
    out
}
