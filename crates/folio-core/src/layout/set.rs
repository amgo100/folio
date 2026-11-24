//! Setting lines (SPEC §7.2): a line of a broken paragraph as glyph runs,
//! in visual order, with its interword spaces stretched or shrunk as the
//! paragraph builder decided, tracking, and the hyphens its breaks add.

use std::ops::Range;

use ecow::{EcoString, EcoVec};
use folio_component::{Abs, Em, FontId, FragmentItem, Glyph, GlyphRun, Paint, Point};

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
    let start_hyphen = line.repeated_hyphen.then_some(line.range.start);
    let end_hyphen = line.hyphen.then_some(line.content_end);
    let (left, right) = if rtl {
        (end_hyphen, start_hyphen)
    } else {
        (start_hyphen, end_hyphen)
    };
    if let Some(at) = left {
        pen = runs.hyphen(info, segment_at(at), at, pen);
    }

    for (piece, _) in shaped.visual_runs(line.range.clone()) {
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
            // Objects are zero wide until they are laid out; missing text
            // is drawn as nothing.
            continue;
        };
        let Some(paint) = info.segments.get(item.segment) else {
            continue;
        };
        let Some(face) = paint.face else {
            continue;
        };
        let mut clusters: Vec<u32> = item_glyphs.iter().map(|g| g.cluster).collect();
        clusters.sort_unstable();
        clusters.dedup();
        let cluster_end = |c: u32| -> usize {
            let next = clusters.partition_point(|&x| x <= c);
            clusters.get(next).map_or(item.range.end, |&n| n as usize)
        };
        let size = paint.size;
        let mut last_cluster = None;
        for g in item_glyphs {
            let cluster = g.cluster as usize;
            if !range.contains(&cluster) {
                continue;
            }
            let end = cluster_end(g.cluster);
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
                source: info.source(cluster),
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

    if let Some(at) = right {
        runs.hyphen(info, segment_at(at), at, pen);
    }
    runs.flush();
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

    /// Sets a hyphen of segment `segment` at `pen`, for the text at byte
    /// `at`, as a run of its own, and returns the pen after it.
    fn hyphen(&mut self, info: &ParagraphInfo, segment: usize, at: usize, pen: Abs) -> Abs {
        self.flush();
        let Some(paint) = info.segments.get(segment) else {
            return pen;
        };
        let (Some(face), Some((hyphen, units_per_em))) = (paint.face, &paint.hyphen) else {
            return pen;
        };
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
                    range: 0..1,
                    source: info.source(at.saturating_sub(1)),
                }
            })
            .collect();
        let run = GlyphRun {
            font: FontId(face),
            size,
            fill: paint.fill.clone(),
            text: "-".into(),
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
