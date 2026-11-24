//! Memoised, parallel shaping (SPEC §2.3).

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rayon::prelude::*;

use crate::paragraph::{Paragraph, Shaped, shape};
use crate::shape::TextFont;

/// How many calls of [`Cache::shape_all`] a shaped paragraph is kept
/// without being asked for.
const MAX_AGE: u64 = 32;

/// Shaped paragraphs, memoised by their [`Paragraph`], and the fonts they
/// are shaped in.
///
/// Shaping is a pure function of a paragraph, so a paragraph that is equal
/// to one shaped before, in any compile, gets the earlier result. A
/// compile hands every paragraph of its document to
/// [`shape_all`](Self::shape_all), which shapes the new ones in parallel
/// and forgets those that no call asked for in a while; an edit therefore
/// re-shapes only the paragraphs it changed.
#[derive(Default)]
pub struct Cache {
    paragraphs: Mutex<HashMap<Paragraph, Entry>>,
    fonts: Mutex<HashMap<(usize, usize, u32), TextFont>>,
    /// Counts the calls of `shape_all`.
    clock: AtomicU64,
}

struct Entry {
    shaped: Arc<Shaped>,
    /// The clock when it was last asked for.
    used: u64,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The handle of the face at `index` in a font file's bytes, made once
    /// for as long as the cache holds it. `None` when the bytes hold no
    /// font the shaper reads at that index.
    pub fn font<D>(&self, data: &D, index: u32) -> Option<TextFont>
    where
        D: AsRef<[u8]> + Clone + Send + Sync + 'static,
    {
        let bytes = data.as_ref();
        let key = (bytes.as_ptr() as usize, bytes.len(), index);
        let mut fonts = lock(&self.fonts);
        if let Some(font) = fonts.get(&key) {
            return Some(font.clone());
        }
        let font = TextFont::new(Arc::new(data.clone()), index)?;
        fonts.insert(key, font.clone());
        Some(font)
    }

    /// The shaped form of a paragraph: memoised, or shaped now.
    ///
    /// # Panics
    ///
    /// When the paragraph's segments do not cover its text at character
    /// boundaries.
    pub fn shape(&self, paragraph: &Paragraph) -> Arc<Shaped> {
        let now = self.clock.load(Ordering::Relaxed);
        if let Some(entry) = lock(&self.paragraphs).get_mut(paragraph) {
            entry.used = now;
            return entry.shaped.clone();
        }
        let shaped = Arc::new(shape(paragraph));
        lock(&self.paragraphs)
            .entry(paragraph.clone())
            .or_insert(Entry { shaped, used: now })
            .shaped
            .clone()
    }

    /// Shapes paragraphs in parallel, each memoised, and then forgets the
    /// paragraphs that no call has asked for in a while.
    pub fn shape_all(&self, paragraphs: &[Paragraph]) -> Vec<Arc<Shaped>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let out: Vec<Arc<Shaped>> = paragraphs.par_iter().map(|p| self.shape(p)).collect();
        if now.is_multiple_of(8) {
            self.evict(now);
        }
        out
    }

    /// Forgets paragraphs not asked for in the last [`MAX_AGE`] calls, and
    /// fonts that no handle outside the cache refers to.
    fn evict(&self, now: u64) {
        lock(&self.paragraphs).retain(|_, entry| entry.used + MAX_AGE >= now);
        lock(&self.fonts).retain(|_, font| font.is_shared());
    }

    /// The number of paragraphs held.
    pub fn len(&self) -> usize {
        lock(&self.paragraphs).len()
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
            .field("fonts", &lock(&self.fonts).len())
            .finish()
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
