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
use folio_text::{Glyph, Lang, Paragraph, Segment, SegmentKind};

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
    hits: AtomicU64,
    misses: AtomicU64,
    /// The glyphs of hyphens, by the font, features and language they are
    /// set in, and the hyphen.
    hyphens: Mutex<HashMap<HyphenKey, Hyphen>>,
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
        let key = Key {
            paragraph: input.shaped.paragraph.clone(),
            style: input.style,
            segments: input.segments.clone(),
            objects: input.objects.clone(),
            widths: slots.iter().map(|s| s.width).collect(),
        };
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
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let out = paragraphs
            .iter()
            .map(|(input, slots)| self.layout(input, slots, None))
            .collect();
        if now.is_multiple_of(8) {
            lock(&self.entries).retain(|_, entry| entry.used + MAX_AGE >= now);
        }
        out
    }

    /// The glyphs of a hyphen `c` in a segment's font, features and
    /// language ([`hyphen_glyphs`]), shaped once per world.
    pub(super) fn hyphen(&self, segment: &Segment, c: char) -> Hyphen {
        let key = (segment.kind.clone(), segment.lang.clone(), c);
        if let Some(found) = lock(&self.hyphens).get(&key) {
            return found.clone();
        }
        let glyphs = hyphen_glyphs(segment, c);
        lock(&self.hyphens).insert(key, glyphs.clone());
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
