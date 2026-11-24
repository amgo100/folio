//! Property tests of line breaking (SPEC §7.2, §14.2): total fit finds
//! the breaks with the least demerits, every paragraph is broken, and
//! first fit stands in when total fit runs out of time.

use std::time::Instant;

use folio_core::layout::linebreak::{
    Breakable, Candidate, Outcome, Params, Pass, Totals, break_lines, demerits, fitness, greedy,
    total_fit,
};
use folio_text::BreakKind;
use proptest::prelude::*;

/// A piece of a generated paragraph.
#[derive(Debug, Clone)]
enum Piece {
    /// A word, as syllables between hyphenation points.
    Word(Vec<f64>),
    /// A forced line break.
    Forced,
}

/// A paragraph of words with interword spaces as glue, hyphenation points
/// inside words, and forced breaks.
fn paragraph(pieces: &[Piece], space: f64, hyphen: f64) -> Breakable {
    let mut candidates = Vec::new();
    let mut totals = Totals::default();
    let mut offset = 0;
    let push =
        |candidates: &mut Vec<Candidate>, kind, offset, content_end, end: Totals, start: Totals| {
            let hyphenated = kind == BreakKind::Hyphenate;
            candidates.push(Candidate {
                offset,
                kind,
                penalty: if hyphenated { 50.0 } else { 0.0 },
                flagged: hyphenated,
                content_end,
                end,
                start,
                end_extra: if hyphenated { hyphen } else { 0.0 },
                start_extra: 0.0,
                protrude_end: 0.0,
                protrude_start: 0.0,
            });
        };
    for (i, piece) in pieces.iter().enumerate() {
        let last = i == pieces.len() - 1;
        match piece {
            Piece::Word(syllables) => {
                for (j, &w) in syllables.iter().enumerate() {
                    totals.width += w;
                    offset += 1;
                    if j + 1 < syllables.len() {
                        push(
                            &mut candidates,
                            BreakKind::Hyphenate,
                            offset,
                            offset,
                            totals,
                            totals,
                        );
                    }
                }
                let content_end = offset;
                let end = totals;
                if last {
                    push(
                        &mut candidates,
                        BreakKind::Mandatory,
                        offset,
                        content_end,
                        end,
                        totals,
                    );
                } else if matches!(pieces[i + 1], Piece::Word(_)) {
                    totals.width += space;
                    totals.stretch += space / 2.0;
                    totals.shrink += space / 3.0;
                    offset += 1;
                    push(
                        &mut candidates,
                        BreakKind::Normal,
                        offset,
                        content_end,
                        end,
                        totals,
                    );
                }
            }
            Piece::Forced => {
                offset += 1;
                push(
                    &mut candidates,
                    BreakKind::Mandatory,
                    offset,
                    offset - 1,
                    totals,
                    totals,
                );
            }
        }
    }
    Breakable {
        start: Candidate::paragraph_start(0.0),
        candidates,
    }
}

/// Words of one to three syllables, each at least as wide as a hyphen,
/// sometimes a forced break between them, ending in a word.
fn pieces(max: usize) -> impl Strategy<Value = Vec<Piece>> {
    let word = prop::collection::vec(1.5..8.0f64, 1..=3).prop_map(Piece::Word);
    let piece = prop_oneof![9 => word.clone(), 1 => Just(Piece::Forced)];
    (prop::collection::vec(piece, 0..max), word).prop_map(|(mut pieces, last)| {
        // No forced break first or twice in a row, as markup gives.
        pieces.dedup_by(|a, b| matches!((a, b), (Piece::Forced, Piece::Forced)));
        if matches!(pieces.first(), Some(Piece::Forced)) {
            pieces.remove(0);
        }
        pieces.push(last);
        pieces
    })
}

fn params() -> impl Strategy<Value = Params> {
    (any::<bool>(), 0.0..4.0f64).prop_map(|(justify, indent)| Params {
        justify,
        indent,
        ragged_stretch: 20.0,
        protrusion: false,
    })
}

fn widths() -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(9.0..30.0f64, 1..=3)
}

/// The demerits of breaking at `ends`, if every line is within the
/// tolerance, as total fit counts them.
fn evaluate(
    p: &Breakable,
    ends: &[usize],
    widths: &[f64],
    params: &Params,
    tolerance: f64,
) -> Option<f64> {
    let mut start = &p.start;
    let mut prev_fitness = 2;
    let mut total = 0.0;
    for (line, &e) in ends.iter().enumerate() {
        let end = &p.candidates[e];
        let measure = p.measure(start, end, line, widths, params);
        if measure.ratio() < -1.0 || measure.badness() > tolerance {
            return None;
        }
        total += demerits(
            &measure,
            end,
            start,
            prev_fitness,
            e == p.candidates.len() - 1,
        );
        prev_fitness = fitness(&measure, end);
        start = end;
    }
    Some(total)
}

/// The least demerits of any breaks within the tolerance, by trying every
/// set of breaks.
fn brute_force(
    p: &Breakable,
    widths: &[f64],
    params: &Params,
    tolerance: f64,
    hyphenate: bool,
) -> Option<f64> {
    let optional: Vec<usize> = p
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.kind != BreakKind::Mandatory && (hyphenate || c.kind != BreakKind::Hyphenate)
        })
        .map(|(i, _)| i)
        .collect();
    let mut best: Option<f64> = None;
    for mask in 0u32..(1 << optional.len()) {
        let ends: Vec<usize> = p
            .candidates
            .iter()
            .enumerate()
            .filter(|&(i, c)| {
                c.kind == BreakKind::Mandatory
                    || optional
                        .iter()
                        .position(|&o| o == i)
                        .is_some_and(|bit| mask & (1 << bit) != 0)
            })
            .map(|(i, _)| i)
            .collect();
        if let Some(d) = evaluate(p, &ends, widths, params, tolerance) {
            best = Some(best.map_or(d, |b: f64| b.min(d)));
        }
    }
    best
}

fn assert_well_formed(p: &Breakable, ends: &[usize]) {
    assert!(ends.windows(2).all(|w| w[0] < w[1]), "{ends:?}");
    assert_eq!(ends.last(), Some(&(p.candidates.len() - 1)));
    for (i, c) in p.candidates.iter().enumerate() {
        if c.kind == BreakKind::Mandatory {
            assert!(ends.contains(&i), "forced break {i} not in {ends:?}");
        }
    }
}

/// How far the lines of a paragraph are wider than their widths, in sum.
fn overflow(p: &Breakable, ends: &[usize], widths: &[f64], params: &Params) -> f64 {
    let mut start = &p.start;
    let mut sum = 0.0;
    for (line, &e) in ends.iter().enumerate() {
        let end = &p.candidates[e];
        sum += p.measure(start, end, line, widths, params).overflow();
        start = end;
    }
    sum
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Each pass of total fit finds the least demerits of all breaks within
    /// its tolerance, or that there are none.
    #[test]
    fn total_fit_is_optimal(
        pieces in pieces(10),
        params in params(),
        widths in widths(),
        hyphen in 0.5..1.5f64,
    ) {
        let p = paragraph(&pieces, 2.5, hyphen);
        prop_assume!(p.candidates.len() <= 16);
        for (pass, tolerance, hyphenate) in [(Pass::First, 100.0, false), (Pass::Second, 200.0, true)] {
            let expected = brute_force(&p, &widths, &params, tolerance, hyphenate);
            match total_fit(&p, &widths, &params, pass, None) {
                Outcome::Found(ends, d) => {
                    assert_well_formed(&p, &ends);
                    let expected = expected.expect("brute force finds breaks too");
                    prop_assert!((d - expected).abs() <= 1e-6 * expected.abs().max(1.0),
                        "{pass:?}: total fit {d}, best {expected}");
                    let check = evaluate(&p, &ends, &widths, &params, tolerance);
                    prop_assert!(check.is_some_and(|c| (c - d).abs() <= 1e-6 * d.abs().max(1.0)));
                }
                Outcome::Infeasible => prop_assert!(expected.is_none(), "{pass:?}: missed {expected:?}"),
                Outcome::OutOfTime => unreachable!(),
            }
        }
    }

    /// Every paragraph is broken, at all its forced breaks, and without an
    /// overfull line where first fit has none.
    #[test]
    fn every_paragraph_is_broken(
        pieces in pieces(60),
        params in params(),
        widths in widths(),
        hyphen in 0.5..1.5f64,
    ) {
        let p = paragraph(&pieces, 2.5, hyphen);
        let broken = break_lines(&p, &widths, &params, None);
        prop_assert!(broken.optimal);
        assert_well_formed(&p, &broken.ends);
        let first_fit = greedy(&p, &widths, &params);
        assert_well_formed(&p, &first_fit);
        if overflow(&p, &first_fit, &widths, &params) == 0.0 {
            prop_assert_eq!(overflow(&p, &broken.ends, &widths, &params), 0.0);
        }
    }

    /// Total fit is never worse than first fit by its own measure.
    #[test]
    fn total_fit_is_never_worse_than_first_fit(
        pieces in pieces(60),
        params in params(),
        widths in widths(),
        hyphen in 0.5..1.5f64,
    ) {
        let p = paragraph(&pieces, 2.5, hyphen);
        let first_fit = greedy(&p, &widths, &params);
        if let Some(g) = evaluate(&p, &first_fit, &widths, &params, 200.0) {
            let Outcome::Found(_, d) = total_fit(&p, &widths, &params, Pass::Second, None) else {
                panic!("first fit is within the tolerance, so total fit finds breaks");
            };
            prop_assert!(d <= g + 1e-6 * g.abs().max(1.0), "total fit {d}, first fit {g}");
        }
    }

    /// Past its deadline, a paragraph is broken by first fit; before it,
    /// by total fit.
    #[test]
    fn a_deadline_falls_back_to_first_fit(
        pieces in pieces(200),
        params in params(),
        widths in widths(),
    ) {
        let p = paragraph(&pieces, 2.5, 1.0);
        let past = break_lines(&p, &widths, &params, Some(Instant::now()));
        if past.optimal {
            // Too few candidates to reach a check of the deadline.
            prop_assert_eq!(past.ends, break_lines(&p, &widths, &params, None).ends);
        } else {
            prop_assert_eq!(past.ends, greedy(&p, &widths, &params));
        }
        let far = Instant::now() + std::time::Duration::from_secs(3600);
        prop_assert!(break_lines(&p, &widths, &params, Some(far)).optimal);
    }
}
