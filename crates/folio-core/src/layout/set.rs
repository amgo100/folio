//! Setting lines (SPEC §7.2): a line of a broken paragraph as glyph runs,
//! in visual order, with its interword spaces stretched or shrunk as the
//! paragraph builder decided, tracking, and the hyphens its breaks add.

use std::ops::Range;

use ecow::{EcoString, EcoVec};
use folio_component::{
    Abs, Em, FontId, Fragment, FragmentItem, Glyph, GlyphRun, ObjectBox, Paint, Point, Size,
};
use folio_text::{Item, ItemKind, Paragraph, Segment};

use super::flow::{ParagraphInfo, em, glyphs};

/// Sets line `index` of a paragraph with its baseline at `baseline`, adding
/// its glyph runs to `out`. Positions are from the top-left of the body.
pub(super) fn set_line(
    info: &ParagraphInfo,
    index: usize,
    baseline: Abs,
    out: &mut Vec<(Point, FragmentItem)>,
) {
    let shaped = &info.shaped;
    let text = shaped.text();
    let line = &info.layout.lines[index];
    let rtl = shaped.level % 2 == 1;
    let mut pen = info.slot.offset + line.x;
    if index == 0 && !rtl {
        pen += info.indent;
    }
    let mut runs = Runs {
        text,
        baseline,
        out,
        current: None,
    };

    // A hyphen at the left: the repeated one of a left-to-right line, or
    // the inserted one of a right-to-left line.
    let segments = &shaped.paragraph.segments;
    let segment_at = |offset: usize| {
        segments
            .partition_point(|s| s.range.end < offset)
            .min(segments.len().saturating_sub(1))
    };
    // The explicit hyphen a line repeats, which shows the hyphen that ends
    // the line before, and the one a break inserts, which shows no text.
    let start_hyphen = if line.repeated_hyphen {
        text[..line.range.start]
            .chars()
            .next_back()
            .map(|c| (line.range.start - c.len_utf8()..line.range.start, c))
    } else {
        None
    };
    let end_hyphen = line
        .hyphen
        .then_some((line.content_end..line.content_end, '-'));
    let (left, right) = if rtl {
        (end_hyphen, start_hyphen)
    } else {
        (start_hyphen, end_hyphen)
    };
    if let Some((shows, c)) = left {
        pen = runs.hyphen(info, segment_at(shows.end), shows, c, pen);
    }

    for (piece, level) in shaped.visual_runs(line.range.clone()) {
        let range = piece.start..piece.end.min(line.content_end);
        if range.is_empty() {
            continue;
        }
        let i = shaped
            .items
            .partition_point(|it| it.range.end <= range.start);
        let Some(item) = shaped.items.get(i) else {
            continue;
        };
        let Some((item_glyphs, units_per_em)) = glyphs(&item.kind) else {
            // Objects without a size are as wide as nothing, and missing
            // text is drawn as nothing.
            if matches!(item.kind, ItemKind::Object) && !info.objects.is_empty() {
                let tracking = info
                    .segments
                    .get(item.segment)
                    .map_or(Abs::zero(), |paint| paint.tracking);
                let mut at: Vec<usize> = text[range.clone()]
                    .char_indices()
                    .map(|(i, _)| range.start + i)
                    .collect();
                if level % 2 == 1 {
                    at.reverse();
                }
                for at in at {
                    pen = runs.object(info, at, tracking, pen);
                }
            }
            continue;
        };
        let Some(paint) = info.segments.get(item.segment) else {
            continue;
        };
        let Some(face) = paint.face else {
            continue;
        };
        let size = paint.size;
        let segment = &segments[item.segment];
        let mut last_cluster = None;
        for placed in line_glyphs(text, item, item_glyphs, units_per_em, segment, range) {
            let Placed {
                glyph: g,
                cluster,
                end,
                units_per_em,
            } = placed;
            let mut advance = em(g.x_advance, units_per_em).at(size);
            if last_cluster != Some(cluster) {
                last_cluster = Some(cluster);
                let chars = &text[cluster..end.min(text.len())];
                let tracked = chars.chars().filter(|&c| c != '\u{2028}').count();
                let natural = advance + paint.tracking;
                advance += paint.tracking * tracked as f64;
                if matches!(chars, " " | "\u{A0}") {
                    advance += line.space(natural) - natural;
                }
            }
            let glyph = Glyph {
                id: u16::try_from(g.id).unwrap_or(0),
                x_advance: per_em(advance, size),
                x_offset: em(g.x_offset, units_per_em),
                y_offset: em(g.y_offset, units_per_em),
                range: 0..0,
                source: info.source(cluster..end.min(text.len())),
            };
            pen = runs.push(
                Key {
                    face,
                    size,
                    fill: paint.fill.clone(),
                    item: i,
                },
                pen,
                glyph,
                cluster..end,
            );
        }
    }

    if let Some((shows, c)) = right {
        runs.hyphen(info, segment_at(shows.end), shows, c, pen);
    }
    runs.flush();
}

/// A glyph of a line, in visual order, with the bytes of the paragraph's
/// text it shows.
struct Placed {
    glyph: folio_text::Glyph,
    /// Where its cluster starts in the paragraph's text.
    cluster: usize,
    /// Where its cluster ends.
    end: usize,
    /// The units per em its advances and offsets are in.
    units_per_em: u16,
}

/// The glyphs of the part `range` of an item that a line holds, in visual
/// order. Where the line's edge cuts through a cluster of the item (a
/// ligature broken at a hyphenation point), the part is shaped again on its
/// own, so that each line draws exactly its own characters; the part may
/// then be a little wider or narrower than the paragraph builder measured.
fn line_glyphs(
    text: &str,
    item: &Item,
    glyphs: &[folio_text::Glyph],
    units_per_em: u16,
    segment: &Segment,
    range: Range<usize>,
) -> Vec<Placed> {
    let mut clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
    clusters.sort_unstable();
    clusters.dedup();
    let starts = |at: usize| {
        at <= item.range.start
            || at >= item.range.end
            || u32::try_from(at).is_ok_and(|at| clusters.binary_search(&at).is_ok())
    };
    if starts(range.start) && starts(range.end) {
        let end_of = |c: u32| {
            let next = clusters.partition_point(|&x| x <= c);
            clusters.get(next).map_or(item.range.end, |&n| n as usize)
        };
        return glyphs
            .iter()
            .filter(|g| range.contains(&(g.cluster as usize)))
            .map(|g| Placed {
                glyph: *g,
                cluster: g.cluster as usize,
                end: end_of(g.cluster),
                units_per_em,
            })
            .collect();
    }

    let part = Paragraph {
        text: text[range.clone()].into(),
        segments: EcoVec::from([Segment {
            range: 0..range.len(),
            kind: segment.kind.clone(),
            lang: segment.lang.clone(),
            hyphenate: false,
        }]),
    };
    let reshaped = folio_text::shape(&part);
    let mut items: Vec<&Item> = reshaped.items.iter().collect();
    if item.is_rtl() {
        items.reverse();
    }
    let mut out = Vec::new();
    for sub in items {
        let ItemKind::Glyphs {
            glyphs,
            units_per_em,
        } = &sub.kind
        else {
            continue;
        };
        let mut clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
        clusters.sort_unstable();
        clusters.dedup();
        for g in glyphs {
            let next = clusters.partition_point(|&x| x <= g.cluster);
            let end = clusters.get(next).map_or(sub.range.end, |&n| n as usize);
            out.push(Placed {
                glyph: *g,
                cluster: range.start + g.cluster as usize,
                end: range.start + end,
                units_per_em: *units_per_em,
            });
        }
    }
    out
}

/// A length at a font size as ems.
fn per_em(length: Abs, size: Abs) -> Em {
    if size.to_pt() > 0.0 {
        Em::new(length.to_pt() / size.to_pt())
    } else {
        Em::new(0.0)
    }
}

/// What glyphs of one run share.
#[derive(Clone, PartialEq)]
struct Key {
    face: u32,
    size: Abs,
    fill: Paint,
    /// The item of the paragraph, so a run's text is one range of it.
    item: usize,
}

/// A run being collected.
struct Current {
    key: Key,
    /// Where the run starts.
    x: Abs,
    glyphs: Vec<(Glyph, Range<usize>)>,
}

/// Glyphs collected into runs.
struct Runs<'a> {
    text: &'a str,
    baseline: Abs,
    out: &'a mut Vec<(Point, FragmentItem)>,
    current: Option<Current>,
}

impl Runs<'_> {
    /// Adds a glyph at `pen`, showing the bytes `range` of the paragraph's
    /// text, and returns the pen after it.
    fn push(&mut self, key: Key, pen: Abs, glyph: Glyph, range: Range<usize>) -> Abs {
        if self.current.as_ref().is_some_and(|c| c.key != key) {
            self.flush();
        }
        let size = key.size;
        let advance = glyph.x_advance.at(size);
        let current = self.current.get_or_insert_with(|| Current {
            key,
            x: pen,
            glyphs: Vec::new(),
        });
        current.glyphs.push((glyph, range));
        pen + advance
    }

    /// Ends the current run.
    fn flush(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let lo = current
            .glyphs
            .iter()
            .map(|(_, r)| r.start)
            .min()
            .unwrap_or(0);
        let hi = current
            .glyphs
            .iter()
            .map(|(_, r)| r.end)
            .max()
            .unwrap_or(lo);
        let text: EcoString = self.text.get(lo..hi).unwrap_or_default().into();
        let glyphs: EcoVec<Glyph> = current
            .glyphs
            .into_iter()
            .map(|(mut glyph, range)| {
                glyph.range = offset(range.start - lo)..offset(range.end - lo);
                glyph
            })
            .collect();
        self.out.push((
            Point::new(current.x, self.baseline),
            FragmentItem::Text(GlyphRun {
                font: FontId(current.key.face),
                size: current.key.size,
                fill: current.key.fill,
                text,
                glyphs,
            }),
        ));
    }

    /// Places the object whose character is at byte `at` of the
    /// paragraph's text at `pen`, if it has a size, and returns the pen
    /// after it and its tracking. Called only for paragraphs with sized
    /// objects, which list every object.
    fn object(&mut self, info: &ParagraphInfo, at: usize, tracking: Abs, pen: Abs) -> Abs {
        let Ok(i) = info.objects.binary_search_by_key(&at, |o| o.at) else {
            return pen;
        };
        let object = &info.objects[i];
        // An object without a size takes no space but its tracking, as the
        // paragraph builder measured it.
        let Some(m) = object.metrics else {
            return pen + tracking;
        };
        self.flush();
        let top = self.baseline - m.shift - m.ascent;
        // A chunk of a formula draws its glyphs and rules, in a box of the
        // formula's element, which the PDF writer tags it by.
        if let Some(items) = &object.content {
            let mut chunk = Fragment::new(Size::new(m.width, m.ascent + m.descent))
                .with_location(object.location);
            chunk.items = items
                .iter()
                .map(|(pos, item)| (Point::new(pos.x, m.ascent + pos.y), item.clone()))
                .collect();
            self.out
                .push((Point::new(pen, top), FragmentItem::Group(chunk)));
            return pen + m.width + tracking;
        }
        self.out.push((
            Point::new(pen, top),
            FragmentItem::Object(ObjectBox {
                size: Size::new(m.width, m.ascent + m.descent),
                baseline: m.ascent,
                location: object.location,
            }),
        ));
        pen + m.width + tracking
    }

    /// Sets a hyphen `c` of segment `segment` at `pen`, showing the bytes
    /// `shows` of the paragraph's text, as a run of its own, and returns
    /// the pen after it.
    fn hyphen(
        &mut self,
        info: &ParagraphInfo,
        segment: usize,
        shows: Range<usize>,
        c: char,
        pen: Abs,
    ) -> Abs {
        self.flush();
        let Some(paint) = info.segments.get(segment) else {
            return pen;
        };
        let Some(face) = paint.face else {
            return pen;
        };
        let Some((_, hyphen, units_per_em)) = paint.hyphens.iter().find(|h| h.0 == c) else {
            return pen;
        };
        let len = u32::try_from(c.len_utf8()).unwrap_or(1);
        let source = info.source(shows);
        let size = paint.size;
        let count = hyphen.len();
        let glyphs: EcoVec<Glyph> = hyphen
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let mut advance = em(g.x_advance, *units_per_em).at(size);
                if i + 1 == count {
                    advance += paint.tracking;
                }
                Glyph {
                    id: u16::try_from(g.id).unwrap_or(0),
                    x_advance: per_em(advance, size),
                    x_offset: em(g.x_offset, *units_per_em),
                    y_offset: em(g.y_offset, *units_per_em),
                    range: 0..len,
                    source: source.clone(),
                }
            })
            .collect();
        let run = GlyphRun {
            font: FontId(face),
            size,
            fill: paint.fill.clone(),
            text: c.to_string().into(),
            glyphs,
        };
        let width = run.width();
        self.out
            .push((Point::new(pen, self.baseline), FragmentItem::Text(run)));
        pen + width
    }
}

fn offset(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}
