//! Line breaking (SPEC §7.2): Knuth–Plass total fit over a paragraph's break
//! opportunities, with a width per line index, and a greedy first fit for
//! when total fit runs out of time.
//!
//! The algorithm sees a paragraph as a [`Breakable`]: its break
//! opportunities, each with the running totals of width, stretch and shrink
//! of the text before it. A line from one opportunity to the next has the
//! difference of their totals as its natural width, plus a hyphen where it
//! ends at a hyphenation point and minus the protrusion of its outer
//! characters. Interword spaces are glue: they stretch and shrink in
//! justified paragraphs, and keep their width in ragged ones, whose lines
//! end in glue of [`Params::ragged_stretch`] instead, as in TeX.
//!
//! Total fit finds the breaks that minimise the sum of the lines'
//! demerits, which grow with the cube of how far each line's spaces are
//! stretched or shrunk, and with penalties for breaking at hyphens, for two
//! hyphenated lines in a row and for a very loose line next to a tight one
//! (TeX's model, *The TeXbook*, chapter 14). It runs in up to three passes:
//! without hyphenation and with a strict tolerance; with hyphenation and a
//! looser one; and a final pass that accepts any line, preferring loose
//! lines to overfull ones, so that every paragraph is broken.
//!
//! Widths are given per line index, the last repeating for the lines after
//! it, so a shape that changes from line to line (text wrap, §7.7) breaks
//! with the same algorithm.

use std::time::Instant;

use folio_text::BreakKind;

/// Width, stretch and shrink, in points.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct Totals {
    /// The natural width.
    pub width: f64,
    /// How far the glue can stretch.
    pub stretch: f64,
    /// How far the glue can shrink.
    pub shrink: f64,
}

/// A place where a line may end, with what the paragraph holds before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The byte offset where the next line starts.
    pub offset: usize,
    /// The kind of break.
    pub kind: BreakKind,
    /// The cost of breaking here: positive discourages, negative
    /// encourages.
    pub penalty: f64,
    /// Whether a line that ends here ends in a hyphen, explicit or
    /// inserted, which two lines in a row should not.
    pub flagged: bool,
    /// The byte offset where the content of a line that ends here ends:
    /// before the spaces and the forced break that the line drops.
    pub content_end: usize,
    /// The totals of the paragraph up to `content_end`.
    pub end: Totals,
    /// The totals of the paragraph up to `offset`.
    pub start: Totals,
    /// The width added at the end of a line that ends here: an inserted
    /// hyphen.
    pub end_extra: f64,
    /// The width added at the start of the line after this break: the
    /// hyphen that Polish repeats.
    pub start_extra: f64,
    /// How far the last character of a line that ends here hangs out.
    pub protrude_end: f64,
    /// How far the first character of the line after this break hangs
    /// out.
    pub protrude_start: f64,
}

impl Candidate {
    /// The start of a paragraph, as a candidate that lines start after.
    pub fn paragraph_start(protrude_start: f64) -> Self {
        Self {
            offset: 0,
            kind: BreakKind::Normal,
            penalty: 0.0,
            flagged: false,
            content_end: 0,
            end: Totals::default(),
            start: Totals::default(),
            end_extra: 0.0,
            start_extra: 0.0,
            protrude_end: 0.0,
            protrude_start,
        }
    }

    /// Whether a line that ends here is not stretched to the full width:
    /// the last line of a paragraph, and a line ended by a forced break.
    fn fills(&self) -> bool {
        self.kind == BreakKind::Mandatory
    }
}

/// A paragraph as line breaking sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakable {
    /// The start of the paragraph ([`Candidate::paragraph_start`]).
    pub start: Candidate,
    /// The break opportunities, in order, ending with the mandatory break
    /// at the end of the paragraph.
    pub candidates: Vec<Candidate>,
}

/// How a paragraph is broken.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    /// Whether lines are stretched and shrunk to their full width.
    pub justify: bool,
    /// The indent of the first line.
    pub indent: f64,
    /// The stretch at the end of a line of a ragged paragraph.
    pub ragged_stretch: f64,
    /// Whether characters hang into the margins.
    pub protrusion: bool,
}

/// The demerits of every line (TeX's `\linepenalty`).
const LINE_PENALTY: f64 = 10.0;
/// Two lines in a row that end in hyphens (`\doublehyphendemerits`).
const DOUBLE_HYPHEN_DEMERITS: f64 = 10_000.0;
/// A hyphen at the end of the second-last line (`\finalhyphendemerits`).
const FINAL_HYPHEN_DEMERITS: f64 = 5_000.0;
/// A line whose fitness class is two or more from its neighbour's
/// (`\adjdemerits`).
const ADJ_DEMERITS: f64 = 10_000.0;
/// The tolerance of the first pass, without hyphenation
/// (`\pretolerance`).
const PRETOLERANCE: f64 = 100.0;
/// The tolerance of the second pass, with hyphenation (`\tolerance`).
const TOLERANCE: f64 = 200.0;
/// The badness of a line that cannot stretch far enough.
pub const INF_BAD: f64 = 10_000.0;
/// The demerits of an overfull line in the final pass: more than any
/// number of loose lines a paragraph could otherwise have.
const OVERFULL_DEMERITS: f64 = 1e12;
/// Lengths closer than this, in points, are equal: sums of advances carry
/// rounding errors.
pub const EPSILON: f64 = 1e-9;
/// How many candidates total fit looks at between checks of its deadline.
const CHECK_EVERY: usize = 64;

/// What a line measures, before it is set.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Measure {
    /// The natural width, with the indent, an inserted or repeated hyphen,
    /// and minus protrusion.
    pub natural: f64,
    /// How far it can stretch: infinite for a line that fills.
    pub stretch: f64,
    /// How far it can shrink.
    pub shrink: f64,
    /// The width it should have.
    pub width: f64,
}

impl Measure {
    /// How far the glue is stretched (positive) or shrunk (negative), as a
    /// fraction of how far it can be. Below -1 the line is overfull.
    pub fn ratio(&self) -> f64 {
        let delta = self.width - self.natural;
        if delta > 0.0 {
            if self.stretch.is_infinite() {
                0.0
            } else if self.stretch > 0.0 {
                delta / self.stretch
            } else {
                f64::INFINITY
            }
        } else if delta < 0.0 {
            if self.shrink > 0.0 {
                delta / self.shrink
            } else {
                f64::NEG_INFINITY
            }
        } else {
            0.0
        }
    }

    /// TeX's badness of the line: 100 times the cube of its ratio, at most
    /// [`INF_BAD`]; [`INF_BAD`] for an overfull line.
    pub fn badness(&self) -> f64 {
        if self.is_overfull() {
            INF_BAD
        } else {
            badness(self.ratio().max(-1.0))
        }
    }

    /// Whether the line is wider than its width even at its tightest.
    pub fn is_overfull(&self) -> bool {
        self.overflow() > 0.0
    }

    /// How far the line is wider than its width at its tightest, or zero.
    /// Rounding errors below [`EPSILON`] are not overflow.
    pub fn overflow(&self) -> f64 {
        let overflow = self.natural - self.shrink - self.width;
        if overflow > EPSILON { overflow } else { 0.0 }
    }
}

fn badness(ratio: f64) -> f64 {
    if ratio < -1.0 {
        INF_BAD
    } else {
        (100.0 * ratio.abs().powi(3)).min(INF_BAD)
    }
}

/// TeX's fitness class of a line that ends at a candidate: 0 very loose,
/// 1 loose, 2 decent, 3 tight. A line that fills is decent unless it
/// shrinks.
pub fn fitness(measure: &Measure, end: &Candidate) -> u8 {
    let ratio = measure.ratio();
    if end.fills() && ratio >= 0.0 {
        return 2;
    }
    let b = badness(ratio);
    if ratio < 0.0 {
        if b > 12.0 { 3 } else { 2 }
    } else if b > 99.0 {
        0
    } else if b > 12.0 {
        1
    } else {
        2
    }
}

impl Breakable {
    /// The width a line of index `line` should have: its entry of
    /// `widths`, or the last entry for lines past them.
    ///
    /// # Panics
    ///
    /// When `widths` is empty.
    pub fn line_width(widths: &[f64], line: usize) -> f64 {
        widths[line.min(widths.len() - 1)]
    }

    /// What the line of index `line` from `start` (the paragraph's start,
    /// or a break) to the candidate `end` measures.
    pub fn measure(
        &self,
        start: &Candidate,
        end: &Candidate,
        line: usize,
        widths: &[f64],
        params: &Params,
    ) -> Measure {
        // A line of nothing but spaces before a forced break is empty.
        let end_totals = if end.content_end >= start.offset {
            end.end
        } else {
            start.start
        };
        let mut natural = end_totals.width - start.start.width + end.end_extra + start.start_extra;
        if line == 0 {
            natural += params.indent;
        }
        if params.protrusion {
            natural -= start.protrude_start + end.protrude_end;
        }
        let (stretch, shrink) = if end.fills() {
            let shrink = if params.justify {
                end_totals.shrink - start.start.shrink
            } else {
                0.0
            };
            (f64::INFINITY, shrink)
        } else if params.justify {
            (
                end_totals.stretch - start.start.stretch,
                end_totals.shrink - start.start.shrink,
            )
        } else {
            (params.ragged_stretch, 0.0)
        };
        Measure {
            natural,
            stretch,
            shrink,
            width: Self::line_width(widths, line),
        }
    }

    /// The candidate a line ends at, by index, or the paragraph's start for
    /// `None`.
    fn at(&self, index: Option<usize>) -> &Candidate {
        index.map_or(&self.start, |i| &self.candidates[i])
    }
}

/// How a paragraph was broken.
#[derive(Debug, Clone, PartialEq)]
pub struct Broken {
    /// The index of the candidate each line ends at, in order. The last is
    /// the paragraph's end.
    pub ends: Vec<usize>,
    /// Whether the breaks are total fit's. `false` when total fit ran out
    /// of time and lines were filled greedily.
    pub optimal: bool,
}

/// Breaks a paragraph into lines of the given widths, by total fit; or
/// greedily when `deadline` passes before total fit is done. An empty
/// paragraph (without candidates) has no lines.
///
/// # Panics
///
/// When `widths` is empty or the last candidate is not mandatory.
pub fn break_lines(
    paragraph: &Breakable,
    widths: &[f64],
    params: &Params,
    deadline: Option<Instant>,
) -> Broken {
    assert!(!widths.is_empty(), "a paragraph needs a line width");
    let Some(last) = paragraph.candidates.last() else {
        return Broken {
            ends: Vec::new(),
            optimal: true,
        };
    };
    assert_eq!(
        last.kind,
        BreakKind::Mandatory,
        "a paragraph ends in a break"
    );
    let has_hyphens = paragraph
        .candidates
        .iter()
        .any(|c| c.kind == BreakKind::Hyphenate);
    for pass in [Pass::First, Pass::Second, Pass::Final] {
        // Without hyphenation points, the second pass is the first with a
        // looser tolerance; TeX skips the first then.
        if pass == Pass::First && !has_hyphens {
            continue;
        }
        match total_fit(paragraph, widths, params, pass, deadline) {
            Outcome::Found(ends, _) => {
                return Broken {
                    ends,
                    optimal: true,
                };
            }
            Outcome::Infeasible => {}
            Outcome::OutOfTime => break,
        }
    }
    Broken {
        ends: greedy(paragraph, widths, params),
        optimal: false,
    }
}

/// A pass of total fit.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Pass {
    /// Without hyphenation points, accepting lines of badness up to 100
    /// (TeX's `\pretolerance`).
    First,
    /// With them, accepting lines of badness up to 200 (TeX's
    /// `\tolerance`).
    Second,
    /// Accepting any line, overfull ones at a cost that makes every other
    /// choice cheaper.
    Final,
}

impl Pass {
    fn tolerance(self) -> f64 {
        match self {
            Self::First => PRETOLERANCE,
            Self::Second => TOLERANCE,
            Self::Final => f64::INFINITY,
        }
    }
}

/// What a pass of total fit found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The breaks, and the sum of the lines' demerits.
    Found(Vec<usize>, f64),
    /// No breaks within the pass's tolerance.
    Infeasible,
    /// The deadline passed.
    OutOfTime,
}

/// A way to break the paragraph up to a candidate.
#[derive(Debug, Clone)]
struct Node {
    /// The candidate, or `None` for the paragraph's start.
    at: Option<usize>,
    /// The number of lines before it.
    lines: usize,
    /// The fitness class of the line that ends at it.
    fitness: u8,
    /// The sum of the demerits of the lines up to it.
    demerits: f64,
    /// The node of the previous break.
    prev: Option<usize>,
}

/// The demerits of a line that ends at the candidate `end` (TeX's
/// formula), after a line that ends at `prev_end` in the fitness class
/// `prev_fitness` (decent for the paragraph's start).
pub fn demerits(
    measure: &Measure,
    end: &Candidate,
    prev_end: &Candidate,
    prev_fitness: u8,
    is_last: bool,
) -> f64 {
    let mut d = if measure.is_overfull() {
        // Only the final pass takes overfull lines: the wider, the worse.
        let overflow = measure.overflow();
        OVERFULL_DEMERITS * (1.0 + overflow * overflow)
    } else {
        (LINE_PENALTY + measure.badness()).powi(2)
    };
    if end.kind != BreakKind::Mandatory {
        if end.penalty >= 0.0 {
            d += end.penalty * end.penalty;
        } else {
            d -= end.penalty * end.penalty;
        }
    }
    if prev_end.flagged && end.flagged && !is_last {
        d += DOUBLE_HYPHEN_DEMERITS;
    }
    if prev_end.flagged && is_last {
        d += FINAL_HYPHEN_DEMERITS;
    }
    if fitness(measure, end).abs_diff(prev_fitness) > 1 {
        d += ADJ_DEMERITS;
    }
    d
}

/// One pass of total fit: the breaks with the least demerits whose lines
/// are within the pass's tolerance.
pub fn total_fit(
    paragraph: &Breakable,
    widths: &[f64],
    params: &Params,
    pass: Pass,
    deadline: Option<Instant>,
) -> Outcome {
    let tolerance = pass.tolerance();
    let count = paragraph.candidates.len();
    // Lines past the last width all have the same width, so their nodes
    // at a candidate differ only in their demerits.
    let last_class = widths.len() - 1;
    let mut nodes = vec![Node {
        at: None,
        lines: 0,
        fitness: 2,
        demerits: 0.0,
        prev: None,
    }];
    let mut active: Vec<usize> = vec![0];
    // The best node per (fitness class, line class) at a candidate.
    let mut best: Vec<(u8, usize, f64, usize)> = Vec::new();

    for (b, end) in paragraph.candidates.iter().enumerate() {
        if deadline.is_some_and(|d| b % CHECK_EVERY == CHECK_EVERY - 1 && Instant::now() > d) {
            return Outcome::OutOfTime;
        }
        if pass == Pass::First && end.kind == BreakKind::Hyphenate {
            continue;
        }
        let is_last = b == count - 1;
        best.clear();
        let mut i = 0;
        while i < active.len() {
            let a = active[i];
            let node = &nodes[a];
            let start = paragraph.at(node.at);
            let measure = paragraph.measure(start, end, node.lines, widths, params);
            let overfull = measure.is_overfull();
            let acceptable = if overfull {
                pass == Pass::Final
            } else {
                measure.badness() <= tolerance
            };
            if acceptable {
                let d = node.demerits + demerits(&measure, end, start, node.fitness, is_last);
                let fit = fitness(&measure, end);
                let class = (node.lines + 1).min(last_class);
                match best.iter_mut().find(|e| e.0 == fit && e.1 == class) {
                    Some(entry) if entry.2 <= d => {}
                    Some(entry) => *entry = (fit, class, d, a),
                    None => best.push((fit, class, d, a)),
                }
            }
            // A line that is already too wide only gets wider, and no line
            // goes past a forced break.
            if overfull || end.kind == BreakKind::Mandatory {
                active.remove(i);
            } else {
                i += 1;
            }
        }
        // Of nodes with equal demerits, the one found first is kept above;
        // the new nodes join the active list in a fixed order.
        best.sort_by_key(|x| (x.0, x.1));
        for &(fit, _, d, prev) in &best {
            nodes.push(Node {
                at: Some(b),
                lines: nodes[prev].lines + 1,
                fitness: fit,
                demerits: d,
                prev: Some(prev),
            });
            active.push(nodes.len() - 1);
        }
        if active.is_empty() {
            return Outcome::Infeasible;
        }
    }

    // The last candidate is mandatory, so the active nodes are those at it.
    let Some(&end) = active.iter().min_by(|&&x, &&y| {
        nodes[x]
            .demerits
            .total_cmp(&nodes[y].demerits)
            .then(x.cmp(&y))
    }) else {
        return Outcome::Infeasible;
    };
    let total = nodes[end].demerits;
    let mut ends = Vec::new();
    let mut node = Some(end);
    while let Some(n) = node {
        if let Some(at) = nodes[n].at {
            ends.push(at);
        }
        node = nodes[n].prev;
    }
    ends.reverse();
    Outcome::Found(ends, total)
}

/// First fit: each line takes as much as fits, at its tightest for a
/// justified paragraph, and a piece wider than the line takes a line of
/// its own.
pub fn greedy(paragraph: &Breakable, widths: &[f64], params: &Params) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut start: Option<usize> = None;
    let mut fitting: Option<usize> = None;
    let mut b = 0;
    while b < paragraph.candidates.len() {
        let end = &paragraph.candidates[b];
        let measure = paragraph.measure(paragraph.at(start), end, ends.len(), widths, params);
        if measure.is_overfull()
            && let Some(f) = fitting
        {
            // Break at the last candidate that fit and measure again from
            // there.
            ends.push(f);
            start = Some(f);
            fitting = None;
            b = f + 1;
            continue;
        }
        if end.kind == BreakKind::Mandatory || measure.is_overfull() {
            ends.push(b);
            start = Some(b);
            fitting = None;
        } else {
            fitting = Some(b);
        }
        b += 1;
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paragraph of words of the given widths with spaces of width 1,
    /// stretch 0.5 and shrink 1/3 between them.
    fn words(widths: &[f64]) -> Breakable {
        let mut candidates = Vec::new();
        let mut totals = Totals::default();
        let mut offset = 0;
        for (i, &w) in widths.iter().enumerate() {
            totals.width += w;
            offset += 1;
            let end = totals;
            let content_end = offset;
            let last = i == widths.len() - 1;
            if !last {
                totals.width += 1.0;
                totals.stretch += 0.5;
                totals.shrink += 1.0 / 3.0;
                offset += 1;
            }
            candidates.push(Candidate {
                offset,
                kind: if last {
                    BreakKind::Mandatory
                } else {
                    BreakKind::Normal
                },
                penalty: 0.0,
                flagged: false,
                content_end,
                end,
                start: totals,
                end_extra: 0.0,
                start_extra: 0.0,
                protrude_end: 0.0,
                protrude_start: 0.0,
            });
        }
        Breakable {
            start: Candidate::paragraph_start(0.0),
            candidates,
        }
    }

    fn params() -> Params {
        Params {
            justify: true,
            indent: 0.0,
            ragged_stretch: 20.0,
            protrusion: false,
        }
    }

    #[test]
    fn breaks_where_lines_fit() {
        let p = words(&[3.0, 3.0, 3.0, 3.0]);
        // Two words and a space are 7 wide.
        let broken = break_lines(&p, &[7.0], &params(), None);
        assert_eq!(broken.ends, vec![1, 3]);
        assert!(broken.optimal);
    }

    #[test]
    fn total_fit_weighs_the_whole_paragraph() {
        // First fit squeezes five words into the first line; total fit
        // sets four a little loose, which costs less in all.
        let p = words(&[1.0, 1.0, 1.0, 5.0, 1.0, 2.0, 2.0]);
        let widths = [12.0];
        assert_eq!(greedy(&p, &widths, &params()), vec![4, 6]);
        assert_eq!(break_lines(&p, &widths, &params(), None).ends, vec![3, 6]);
    }

    #[test]
    fn a_word_wider_than_the_line_is_overfull() {
        let p = words(&[3.0, 20.0, 3.0]);
        let broken = break_lines(&p, &[10.0], &params(), None);
        assert_eq!(broken.ends, vec![0, 1, 2]);
        assert!(broken.optimal);
    }

    #[test]
    fn widths_per_line() {
        let p = words(&[3.0, 3.0, 3.0, 3.0, 3.0, 3.0]);
        let broken = break_lines(&p, &[3.0, 11.0], &params(), None);
        assert_eq!(broken.ends, vec![0, 3, 5]);
    }

    #[test]
    fn a_passed_deadline_breaks_greedily() {
        let widths: Vec<f64> = (0..200).map(|i| 2.0 + f64::from(i % 5)).collect();
        let p = words(&widths);
        let past = Instant::now();
        let broken = break_lines(&p, &[20.0], &params(), Some(past));
        assert!(!broken.optimal);
        assert_eq!(broken.ends, greedy(&p, &[20.0], &params()));
    }
}
