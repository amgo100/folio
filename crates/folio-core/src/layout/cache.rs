//! The line-break cache (SPEC §2.3): each paragraph's lines, keyed on its
//! content, the style values the paragraph builder reads, and the
//! sequence of its line widths.

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use ecow::EcoVec;
use folio_component::Abs;
use folio_text::{Glyph, Lang, Paragraph, Segment, SegmentKind, Shaped};

use super::paragraph::{
    LineSlot, ParagraphInput, ParagraphLayout, ParagraphStyle, SegmentMetrics, hyphen_glyphs,
    layout_paragraph,
};

/// How many calls of [`Cache::layout_all`] a paragraph's lines are kept
/// without being asked for.
const MAX_AGE: u64 = 32;

/// Paragraphs broken into lines, memoised.
///
/// The key is what decides the breaks: the paragraph's content (its text,
/// fonts, languages and break opportunities, [`Paragraph`]), the values
/// the paragraph builder reads from its computed styles
/// ([`ParagraphStyle`], [`SegmentMetrics`]), the widths of its inline
/// objects, and the sequence of its line widths. Slot offsets are not part
/// of it: they move lines without changing their breaks, so lines are
/// placed relative to their slots. A paint-only style change, or a move of
/// a paragraph to a place with the same widths, therefore finds its lines
/// here.
///
/// Only total fit's breaks are kept: a paragraph broken greedily because
/// total fit ran out of time is broken again when asked next.
#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<Key, Entry>>,
    /// Counts the calls of `layout_all`.
    clock: AtomicU64,
    /// The clock when the cache last forgot paragraphs.
    evicted: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    /// The glyphs of hyphens, by the font, features and language they are
    /// set in, and the hyphen, with the clock when each was last asked for.
    /// They are forgotten like paragraphs, so that the fonts they hold can
    /// be dropped by the text cache too.
    hyphens: Mutex<HashMap<HyphenKey, (Hyphen, u64)>>,
}

/// A hyphen in a font, with features and a language.
type HyphenKey = (SegmentKind, Option<Lang>, char);

/// The glyphs of a hyphen and the units per em they are measured in.
pub(super) type Hyphen = Option<(EcoVec<Glyph>, u16)>;

#[derive(Clone, Eq, PartialEq, Hash)]
struct Key {
    paragraph: Paragraph,
    style: ParagraphStyle,
    segments: EcoVec<SegmentMetrics>,
    objects: EcoVec<Abs>,
    widths: EcoVec<Abs>,
}

impl Key {
    fn new(input: &ParagraphInput<'_>, slots: &[LineSlot]) -> Self {
        Self {
            paragraph: input.shaped.paragraph.clone(),
            style: input.style,
            segments: input.segments.clone(),
            objects: input.objects.clone(),
            widths: slots.iter().map(|s| s.width).collect(),
        }
    }
}

/// A paragraph to break on another thread: what its input borrows, owned.
struct Job {
    key: Key,
    shaped: Shaped,
    slots: Vec<LineSlot>,
}

impl Job {
    fn run(&self) -> ParagraphLayout {
        let input = ParagraphInput {
            shaped: &self.shaped,
            style: self.key.style,
            segments: self.key.segments.clone(),
            objects: self.key.objects.clone(),
        };
        layout_paragraph(&input, &self.slots, None)
    }
}

struct Entry {
    layout: Arc<ParagraphLayout>,
    /// The clock when it was last asked for.
    used: u64,
}

/// How often the cache had a paragraph's lines.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Stats {
    /// Paragraphs whose lines were in the cache.
    pub hits: u64,
    /// Paragraphs that were broken.
    pub misses: u64,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines of a paragraph in slots: memoised, or broken now. Total
    /// fit runs until `deadline`; a paragraph broken greedily after it is
    /// returned but not kept. Line positions are relative to the slots
    /// ([`ParagraphLayout`]).
    ///
    /// # Panics
    ///
    /// When `slots` is empty, or the input has fewer segment metrics than
    /// the paragraph has segments.
    pub fn layout(
        &self,
        input: &ParagraphInput<'_>,
        slots: &[LineSlot],
        deadline: Option<Instant>,
    ) -> Arc<ParagraphLayout> {
        let now = self.clock.load(Ordering::Relaxed);
        let key = Key::new(input, slots);
        if let Some(entry) = lock(&self.entries).get_mut(&key) {
            entry.used = now;
            self.hits.fetch_add(1, Ordering::Relaxed);
            return entry.layout.clone();
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let layout = Arc::new(layout_paragraph(input, slots, deadline));
        if !layout.optimal {
            return layout;
        }
        lock(&self.entries)
            .entry(key)
            .or_insert(Entry { layout, used: now })
            .layout
            .clone()
    }

    /// The lines of paragraphs, each in its slots and memoised, without a
    /// deadline; then forgets the paragraphs that no call has asked for in
    /// a while.
    ///
    /// # Panics
    ///
    /// When a paragraph's slots are empty, or an input has fewer segment
    /// metrics than its paragraph has segments.
    pub fn layout_all(
        &self,
        paragraphs: &[(ParagraphInput<'_>, &[LineSlot])],
    ) -> Vec<Arc<ParagraphLayout>> {
        self.layout_all_checked(paragraphs, &mut || {})
    }

    /// The lines of paragraphs as [`layout_all`](Self::layout_all) gives
    /// them, calling `check` on the caller's thread after each paragraph
    /// it breaks.
    ///
    /// The paragraphs not in the cache are broken in parallel, on the
    /// threads that shape paragraphs ([`folio_text::Cache::parallel`]),
    /// and each result is put in its paragraph's place, so the lines do
    /// not depend on how the work was scheduled (SPEC §2.4). `check` may
    /// unwind, as a database query that its revision has cancelled does
    /// (SPEC §2.3): the paragraphs not yet broken are then dropped, and
    /// those broken are kept in the cache for the next call.
    ///
    /// # Panics
    ///
    /// When a paragraph's slots are empty, an input has fewer segment
    /// metrics than its paragraph has segments, or `check` panics.
    pub fn layout_all_checked(
        &self,
        paragraphs: &[(ParagraphInput<'_>, &[LineSlot])],
        check: &mut dyn FnMut(),
    ) -> Vec<Arc<ParagraphLayout>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let mut out: Vec<Option<Arc<ParagraphLayout>>> = vec![None; paragraphs.len()];
        // The paragraphs to break, each once, with where their lines go.
        let mut misses: Vec<Job> = Vec::new();
        let mut targets: Vec<Vec<usize>> = Vec::new();
        {
            let mut entries = lock(&self.entries);
            let mut missing: HashMap<Key, usize> = HashMap::new();
            for (i, (input, slots)) in paragraphs.iter().enumerate() {
                let key = Key::new(input, slots);
                if let Some(entry) = entries.get_mut(&key) {
                    entry.used = now;
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    out[i] = Some(entry.layout.clone());
                } else if let Some(&m) = missing.get(&key) {
                    // Broken once; counted when its lines are.
                    targets[m].push(i);
                } else {
                    missing.insert(key.clone(), misses.len());
                    misses.push(Job {
                        key,
                        shaped: input.shaped.clone(),
                        slots: slots.to_vec(),
                    });
                    targets.push(vec![i]);
                }
            }
        }

        folio_text::Cache::parallel(misses, Job::run, &mut |m, job, layout| {
            // The first paragraph was broken; the others with its key are
            // found in the cache, as they would be one after another, when
            // its lines are kept.
            let (found, broken) = if layout.optimal {
                (targets[m].len() - 1, 1)
            } else {
                (0, targets[m].len())
            };
            self.hits.fetch_add(found as u64, Ordering::Relaxed);
            self.misses.fetch_add(broken as u64, Ordering::Relaxed);
            let layout = Arc::new(layout);
            let layout = if layout.optimal {
                lock(&self.entries)
                    .entry(job.key.clone())
                    .or_insert(Entry { layout, used: now })
                    .layout
                    .clone()
            } else {
                layout
            };
            for &i in &targets[m] {
                out[i] = Some(layout.clone());
            }
            check();
        });

        // Every eighth call, counted from the last that forgot: a call that
        // a cancelled query left unfinished does not skip it.
        if now >= self.evicted.load(Ordering::Relaxed) + 8 {
            self.evicted.store(now, Ordering::Relaxed);
            lock(&self.entries).retain(|_, entry| entry.used + MAX_AGE >= now);
            lock(&self.hyphens).retain(|_, (_, used)| *used + MAX_AGE >= now);
        }
        out.into_iter()
            .map(|layout| layout.expect("every paragraph is broken"))
            .collect()
    }

    /// The glyphs of a hyphen `c` in a segment's font, features and
    /// language ([`hyphen_glyphs`]), shaped once per world.
    pub(super) fn hyphen(&self, segment: &Segment, c: char) -> Hyphen {
        let now = self.clock.load(Ordering::Relaxed);
        let key = (segment.kind.clone(), segment.lang.clone(), c);
        if let Some((found, used)) = lock(&self.hyphens).get_mut(&key) {
            *used = now;
            return found.clone();
        }
        let glyphs = hyphen_glyphs(segment, c);
        lock(&self.hyphens).insert(key, (glyphs.clone(), now));
        glyphs
    }

    /// How often the cache had a paragraph's lines since it was made.
    pub fn stats(&self) -> Stats {
        Stats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// The number of paragraphs held.
    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    /// The number of hyphens held.
    pub fn hyphens(&self) -> usize {
        lock(&self.hyphens).len()
    }

    /// Whether no paragraph is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Debug for Cache {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("paragraphs", &self.len())
            .field("stats", &self.stats())
            .finish()
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hyphens that no layout asks for are forgotten like paragraphs, so
    /// that the fonts they hold are not kept for ever.
    #[test]
    fn forgets_hyphens() {
        let cache = Cache::new();
        let segment = |_| Segment {
            range: 0..1,
            kind: SegmentKind::Object,
            lang: None,
            hyphenate: true,
        };
        cache.hyphen(&segment(()), '-');
        cache.hyphen(&segment(()), '\u{2010}');
        assert_eq!(cache.hyphens(), 2);
        for _ in 0..MAX_AGE {
            cache.layout_all(&[]);
            // One hyphen is asked for on every layout.
            cache.hyphen(&segment(()), '-');
        }
        for _ in 0..8 {
            cache.layout_all(&[]);
            cache.hyphen(&segment(()), '-');
        }
        assert_eq!(cache.hyphens(), 1);
    }
}
