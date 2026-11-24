//! The paragraph builder (SPEC §7.2): a shaped paragraph broken into lines
//! of given widths, each line placed and set.

use std::ops::Range;
use std::time::Instant;

use ecow::EcoVec;
use folio_component::{Abs, Ratio};
use folio_text::{BreakKind, ItemKind, Paragraph, Segment, SegmentKind, Shaped};

use super::linebreak::{self, Breakable, Candidate, Measure, Params, Totals};

/// What the paragraph builder reads of a paragraph's style.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ParagraphStyle {
    /// The space between one line's descent and the next line's ascent.
    pub leading: Abs,
    /// The indent of the first line, at its start.
    pub indent: Abs,
    /// Where lines that are not stretched to their full width sit.
    pub align: Align,
    /// Whether lines are stretched and shrunk to their full width.
    pub justify: bool,
    /// Whether punctuation and hyphens at the ends of lines hang into the
    /// margin.
    pub protrusion: bool,
    /// The paragraph's font size, which the stretch of ragged lines is
    /// measured in.
    pub font_size: Abs,
}

/// Where a line sits in its width.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Align {
    /// At the start: the left in left-to-right text.
    Start,
    /// In the middle.
    Center,
    /// At the end.
    End,
}

/// What the paragraph builder reads of the style and face of a segment of
/// a paragraph.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct SegmentMetrics {
    /// The font size.
    pub size: Abs,
    /// The space added after every character.
    pub tracking: Abs,
    /// How far the face reaches above the baseline at this size.
    pub ascent: Abs,
    /// How far it reaches below the baseline, as a positive length.
    pub descent: Abs,
}

/// A paragraph to break: shaped, with the metrics of its segments and the
/// widths of its inline objects.
#[derive(Debug, Clone)]
pub struct ParagraphInput<'a> {
    /// The shaped paragraph.
    pub shaped: &'a Shaped,
    /// Its style.
    pub style: ParagraphStyle,
    /// The metrics of each of its segments ([`Paragraph::segments`]).
    pub segments: EcoVec<SegmentMetrics>,
    /// The width of each inline object, in the order of the text. Objects
    /// past the end of the list are zero wide until inline objects are
    /// laid out.
    pub objects: EcoVec<Abs>,
}

/// The width and offset of a line, given per line index (SPEC §7.7): the
/// last slot of a list repeats for the lines past it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct LineSlot {
    /// Where the line's space starts, from the left of the region.
    pub offset: Abs,
    /// How wide it is.
    pub width: Abs,
}

/// A paragraph broken into lines.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ParagraphLayout {
    /// The lines, in order.
    pub lines: EcoVec<Line>,
    /// The height from the top of the first line to the bottom of the
    /// last.
    pub height: Abs,
    /// Whether the breaks are total fit's. `false` for a paragraph that
    /// was broken greedily because total fit took too long.
    pub optimal: bool,
}

/// A line of a paragraph.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Line {
    /// The bytes of the paragraph's text on the line, from the break before
    /// it to the break after it.
    pub range: Range<usize>,
    /// Where the line's content ends: before the spaces and the forced
    /// break at its end, which are not drawn.
    pub content_end: usize,
    /// Whether a hyphen is drawn at the line's end: it ends at a
    /// hyphenation point or soft hyphen.
    pub hyphen: bool,
    /// Whether a hyphen is drawn at the line's start: the previous line
    /// ends at an explicit hyphen that the language repeats (Polish).
    pub repeated_hyphen: bool,
    /// Where drawing starts, from the start of the line's slot: the left
    /// edge of the first character drawn, which hangs to the left of the
    /// slot where it protrudes.
    pub x: Abs,
    /// The width of the line as set: its content with the indent, hyphens
    /// and adjusted spaces.
    pub width: Abs,
    /// How far each interword space is stretched (positive, as a fraction
    /// of half its width) or shrunk (negative, as a fraction of a third of
    /// its width). See [`Line::space`].
    pub glue: Ratio,
    /// The baseline, from the top of the paragraph.
    pub baseline: Abs,
    /// How far the line reaches above its baseline.
    pub ascent: Abs,
    /// How far it reaches below its baseline.
    pub descent: Abs,
    /// How far the line is wider than its slot, or zero.
    pub overflow: Abs,
    /// Whether its spaces are stretched so far that the line looks loose
    /// (TeX's `\hbadness`): only in justified paragraphs.
    pub underfull: bool,
}

impl Line {
    /// The width of an interword space of a natural width on this line.
    pub fn space(&self, natural: Abs) -> Abs {
        let glue = self.glue.get();
        let adjust = if glue >= 0.0 {
            natural * (glue * STRETCH)
        } else {
            natural * (glue * SHRINK)
        };
        natural + adjust
    }
}

/// How far an interword space stretches, as a fraction of its width.
const STRETCH: f64 = 1.0 / 2.0;
/// How far it shrinks.
const SHRINK: f64 = 1.0 / 3.0;
/// How far the stretch of a ragged line reaches, in ems.
const RAGGED_STRETCH: f64 = 2.0;
/// How much wider than its slot a line may be before it counts as overfull
/// (TeX's `\hfuzz`).
const OVERFULL_FUZZ: f64 = 0.1;
/// The badness above which a justified line counts as underfull (TeX's
/// `\hbadness`).
const UNDERFULL_BADNESS: f64 = 1000.0;
/// The cost of breaking at a hyphenation point (`\hyphenpenalty`) or
/// after an explicit hyphen or dash (`\exhyphenpenalty`).
const HYPHEN_PENALTY: f64 = 50.0;

/// Breaks a paragraph into lines of the slots' widths and places them.
/// Total fit runs until `deadline`; past it, the paragraph is broken
/// greedily ([`ParagraphLayout::optimal`]). Line positions are relative to
/// the slots: add a line's slot offset to its `x`.
///
/// # Panics
///
/// When `slots` is empty.
pub fn layout_paragraph(
    input: &ParagraphInput<'_>,
    slots: &[LineSlot],
    deadline: Option<Instant>,
) -> ParagraphLayout {
    assert!(!slots.is_empty(), "a paragraph needs a line slot");
    let prepared = Prepared::new(input);
    let params = Params {
        justify: input.style.justify,
        indent: input.style.indent.to_pt(),
        ragged_stretch: input.style.font_size.to_pt() * RAGGED_STRETCH,
        protrusion: input.style.protrusion,
    };
    let widths: Vec<f64> = slots.iter().map(|s| s.width.to_pt()).collect();
    let broken = linebreak::break_lines(&prepared.breakable, &widths, &params, deadline);
    prepared.set(input, &broken.ends, &widths, &params, broken.optimal)
}

/// A paragraph prepared for breaking: its breakable form, and what setting
/// its lines needs.
struct Prepared {
    breakable: Breakable,
    /// The paragraph's direction: whether it runs right to left.
    rtl: bool,
}

impl Prepared {
    fn new(input: &ParagraphInput<'_>) -> Self {
        let shaped = input.shaped;
        let text = shaped.text();
        let segments = &shaped.paragraph.segments;
        let len = text.len();

        // The advance of every cluster, at the byte it starts at.
        let mut advance = vec![0.0; len + 1];
        let mut objects = input.objects.iter();
        for item in &shaped.items {
            let metrics = &input.segments[item.segment];
            match &item.kind {
                ItemKind::Glyphs {
                    glyphs,
                    units_per_em,
                } => {
                    let scale = metrics.size.to_pt() / f64::from(*units_per_em);
                    for glyph in glyphs {
                        advance[glyph.cluster as usize] += f64::from(glyph.x_advance) * scale;
                    }
                }
                ItemKind::Object => {
                    for (i, _) in text[item.range.clone()].char_indices() {
                        let width = objects.next().map_or(0.0, |w| w.to_pt());
                        advance[item.range.start + i] += width;
                    }
                }
                ItemKind::Missing => {}
            }
        }
        // Tracking after every character but a forced break.
        let mut segment = 0;
        for (i, c) in text.char_indices() {
            while segments[segment].range.end <= i {
                segment += 1;
            }
            if c != '\u{2028}' {
                advance[i] += input.segments[segment].tracking.to_pt();
            }
        }

        // Running totals, with interword spaces as glue.
        let mut totals = vec![Totals::default(); len + 1];
        let mut running = Totals::default();
        let mut chars = text.char_indices().peekable();
        for (i, slot) in totals.iter_mut().enumerate() {
            *slot = running;
            running.width += advance[i];
            if let Some(&(at, c)) = chars.peek()
                && at == i
            {
                chars.next();
                if is_interword(c) {
                    running.stretch += advance[i] * STRETCH;
                    running.shrink += advance[i] * SHRINK;
                }
            }
        }

        let char_width = |at: usize| -> f64 {
            let next = text[at..].chars().next().map_or(at, |c| at + c.len_utf8());
            totals[next].width - totals[at].width
        };
        let mut hyphens: Vec<Option<f64>> = vec![None; segments.len()];
        let mut hyphen_width = |offset: usize| -> f64 {
            // The hyphen is set in the segment of the character before it.
            let i = segments
                .partition_point(|s| s.range.end < offset)
                .min(segments.len() - 1);
            *hyphens[i].get_or_insert_with(|| {
                hyphen_advance(&segments[i], &input.segments[i]).unwrap_or(0.0)
            })
        };

        let first = text.chars().next();
        let start =
            Candidate::paragraph_start(first.map_or(0.0, |c| protrusion(c).0 * char_width(0)));
        let mut candidates = Vec::with_capacity(shaped.breaks.len());
        for b in &shaped.breaks {
            let offset = b.offset;
            let content_end = content_end(text, offset);
            let before = text[..content_end].chars().next_back();
            let after_dash = before.is_some_and(is_dash);
            let (penalty, flagged) = match b.kind {
                BreakKind::Mandatory => (0.0, false),
                BreakKind::Hyphenate | BreakKind::RepeatHyphen => (HYPHEN_PENALTY, true),
                BreakKind::Normal if after_dash => (HYPHEN_PENALTY, true),
                BreakKind::Normal => (0.0, false),
            };
            let end_extra = if b.kind == BreakKind::Hyphenate {
                hyphen_width(offset)
            } else {
                0.0
            };
            let start_extra = if b.kind == BreakKind::RepeatHyphen {
                char_width(offset - 1)
            } else {
                0.0
            };
            let protrude_end = if end_extra > 0.0 {
                protrusion('-').1 * end_extra
            } else {
                before.map_or(0.0, |c| {
                    protrusion(c).1 * char_width(content_end - c.len_utf8())
                })
            };
            let protrude_start = if start_extra > 0.0 {
                protrusion('-').0 * start_extra
            } else {
                text[offset..]
                    .chars()
                    .next()
                    .map_or(0.0, |c| protrusion(c).0 * char_width(offset))
            };
            candidates.push(Candidate {
                offset,
                kind: b.kind,
                penalty,
                flagged,
                content_end,
                end: totals[content_end],
                start: totals[offset],
                end_extra,
                start_extra,
                protrude_end,
                protrude_start,
            });
        }

        Self {
            breakable: Breakable { start, candidates },
            rtl: shaped.level % 2 == 1,
        }
    }

    /// Sets the lines that end at the candidates `ends`.
    fn set(
        &self,
        input: &ParagraphInput<'_>,
        ends: &[usize],
        widths: &[f64],
        params: &Params,
        optimal: bool,
    ) -> ParagraphLayout {
        let segments = &input.shaped.paragraph.segments;
        let mut lines = EcoVec::with_capacity(ends.len());
        let mut start = &self.breakable.start;
        let mut y = Abs::zero();
        let mut prev_descent: Option<Abs> = None;
        for (index, &e) in ends.iter().enumerate() {
            let end = &self.breakable.candidates[e];
            let measure = self.breakable.measure(start, end, index, widths, params);
            let range = start.offset..end.offset;
            let content_end = end.content_end.max(start.offset);

            let (ascent, descent) = extent(segments, &input.segments, start.offset..content_end);
            y = match prev_descent {
                Some(d) => y + d + input.style.leading + ascent,
                None => ascent,
            };
            prev_descent = Some(descent);

            let (fit, glue) = set_width(&measure, params.justify, end);
            let slot = Breakable::line_width(widths, index);
            let free = slot - fit;
            let (protrude_start, protrude_end) = if params.protrusion {
                (start.protrude_start, end.protrude_end)
            } else {
                (0.0, 0.0)
            };
            let filled = params.justify && end.kind != BreakKind::Mandatory;
            let align = if filled {
                Align::Start
            } else {
                input.style.align
            };
            // The offset of the fitted line within its slot.
            let shift = match (align, self.rtl) {
                (Align::Start, false) | (Align::End, true) => 0.0,
                (Align::Center, _) => free / 2.0,
                (Align::End, false) | (Align::Start, true) => free,
            };
            // Characters at the left edge hang out of it.
            let left_protrusion = if self.rtl {
                protrude_end
            } else {
                protrude_start
            };
            let overflow = measure.overflow();
            let ratio = measure.ratio();
            lines.push(Line {
                range,
                content_end,
                hyphen: end.kind == BreakKind::Hyphenate,
                repeated_hyphen: start.kind == BreakKind::RepeatHyphen,
                x: Abs::pt(shift - left_protrusion),
                width: Abs::pt(fit + protrude_start + protrude_end),
                glue: Ratio::new(glue),
                baseline: y,
                ascent,
                descent,
                overflow: Abs::pt(if overflow > OVERFULL_FUZZ {
                    overflow
                } else {
                    0.0
                }),
                underfull: filled && ratio > 0.0 && measure.badness() > UNDERFULL_BADNESS,
            });
            start = end;
        }
        let height = y + prev_descent.unwrap_or_default();
        ParagraphLayout {
            lines,
            height,
            optimal,
        }
    }
}

/// The width a line is set at, and the ratio its interword glue is
/// adjusted by: stretched to the full width in a justified paragraph,
/// except on a line that ends at a forced break or the paragraph's end;
/// shrunk where it is wider than its slot, but never past its shrink.
fn set_width(measure: &Measure, justify: bool, end: &Candidate) -> (f64, f64) {
    let ratio = measure.ratio();
    let glue = if !ratio.is_finite() {
        // Nothing can stretch, or an overfull line of nothing that shrinks.
        if ratio < 0.0 && measure.shrink > 0.0 {
            -1.0
        } else {
            0.0
        }
    } else if ratio < 0.0 {
        ratio.max(-1.0)
    } else if justify && end.kind != BreakKind::Mandatory {
        ratio
    } else {
        0.0
    };
    let fit = if glue >= 0.0 {
        let stretch = if measure.stretch.is_finite() {
            measure.stretch
        } else {
            0.0
        };
        measure.natural + glue * stretch
    } else {
        measure.natural + glue * measure.shrink
    };
    (fit, glue)
}

/// The ascent and descent of the text in a range: the largest of its
/// segments'. An empty line has those of the segment it starts in.
fn extent(segments: &[Segment], metrics: &[SegmentMetrics], range: Range<usize>) -> (Abs, Abs) {
    let mut ascent = Abs::zero();
    let mut descent = Abs::zero();
    let mut any = false;
    for (segment, m) in segments.iter().zip(metrics) {
        let overlaps = segment.range.start < range.end && range.start < segment.range.end;
        if overlaps && !matches!(segment.kind, SegmentKind::Object) {
            ascent = ascent.max(m.ascent);
            descent = descent.max(m.descent);
            any = true;
        }
    }
    if !any {
        let i = segments
            .partition_point(|s| s.range.end <= range.start)
            .min(segments.len() - 1);
        ascent = metrics[i].ascent;
        descent = metrics[i].descent;
    }
    (ascent, descent)
}

/// Where the content of a line that ends at a break ends: before a forced
/// break and the spaces before the break.
fn content_end(text: &str, offset: usize) -> usize {
    let mut end = offset;
    let head = &text[..end];
    if let Some(stripped) = head.strip_suffix('\u{2028}') {
        end = stripped.len();
    }
    let trimmed = text[..end].trim_end_matches(|c: char| c.is_whitespace() && !is_fixed_space(c));
    trimmed.len()
}

/// Whether a character is an interword space, which stretches and shrinks
/// in a justified line.
fn is_interword(c: char) -> bool {
    matches!(c, ' ' | '\u{A0}')
}

/// Whether a space keeps its place at a line's end: a no-break space, or a
/// forced break.
fn is_fixed_space(c: char) -> bool {
    matches!(c, '\u{A0}' | '\u{202F}' | '\u{2007}' | '\u{2028}')
}

/// Whether a character is a hyphen or dash, after which a break costs as
/// much as at a hyphenation point.
fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2013}' | '\u{2014}')
}

/// How far a character hangs into the margin at the start and at the end
/// of a line, as fractions of its advance.
fn protrusion(c: char) -> (f64, f64) {
    match c {
        '.' | ',' => (0.0, 0.7),
        '-' | '\u{2010}' | '\u{AD}' => (0.0, 0.7),
        '\u{2013}' => (0.0, 0.5),
        '\u{2014}' => (0.0, 0.3),
        ':' | ';' => (0.0, 0.5),
        '!' | '?' => (0.0, 0.2),
        '\'' | '"' => (0.5, 0.5),
        '\u{2018}' | '\u{201C}' | '\u{201E}' | '\u{201A}' => (0.7, 0.7),
        '\u{2019}' | '\u{201D}' => (0.7, 0.7),
        '\u{00AB}' | '\u{00BB}' | '\u{2039}' | '\u{203A}' => (0.5, 0.5),
        '(' | '[' => (0.1, 0.0),
        ')' | ']' => (0.0, 0.1),
        _ => (0.0, 0.0),
    }
}

/// The advance of a hyphen in a segment's font and size, with tracking.
fn hyphen_advance(segment: &Segment, metrics: &SegmentMetrics) -> Option<f64> {
    let SegmentKind::Text {
        font: Some(font),
        features,
    } = &segment.kind
    else {
        return None;
    };
    let hyphen = Paragraph {
        text: "-".into(),
        segments: EcoVec::from([Segment {
            range: 0..1,
            kind: SegmentKind::Text {
                font: Some(font.clone()),
                features: features.clone(),
            },
            lang: segment.lang.clone(),
            hyphenate: false,
        }]),
    };
    let shaped = folio_text::shape(&hyphen);
    let item = shaped.items.first()?;
    let ItemKind::Glyphs {
        glyphs,
        units_per_em,
    } = &item.kind
    else {
        return None;
    };
    let units: i64 = glyphs.iter().map(|g| i64::from(g.x_advance)).sum();
    Some(units as f64 * metrics.size.to_pt() / f64::from(*units_per_em) + metrics.tracking.to_pt())
}
