//! Memoised, parallel shaping (SPEC §2.3).

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, mpsc};

use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::paragraph::{Paragraph, Shaped, shape};
use crate::shape::TextFont;

/// How many calls of [`Cache::shape_all`] a shaped paragraph is kept
/// without being asked for.
const MAX_AGE: u64 = 32;

/// The threads that shape paragraphs, shared by every cache of the
/// process. They run nothing but shaping, so a caller waiting for them never
/// runs other work of its own thread pool meanwhile.
static POOL: LazyLock<Option<ThreadPool>> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|i| format!("folio-shape-{i}"))
        .build()
        .ok()
});

/// Shaped paragraphs, memoised by their [`Paragraph`], and the fonts they
/// are shaped in.
///
/// Shaping is a pure function of a paragraph, so a paragraph that is equal
/// to one shaped before, in any compile, gets the earlier result. A
/// compile hands every paragraph of its document to
/// [`shape_all`](Self::shape_all), which shapes the new ones (in parallel
/// when the shaping pool has several threads) and forgets those that no
/// call asked for in a while; an edit therefore re-shapes only the
/// paragraphs it changed.
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
    ///
    /// The handle keeps a clone of `data`. A clone that shares the bytes, as
    /// a reference-counted buffer's does, lets later calls with the same
    /// bytes find the handle; a clone that copies them gets a new handle
    /// each time.
    pub fn font<D>(&self, data: &D, index: u32) -> Option<TextFont>
    where
        D: AsRef<[u8]> + Clone + Send + Sync + 'static,
    {
        let bytes = data.as_ref();
        let key = (bytes.as_ptr() as usize, bytes.len(), index);
        let mut fonts = lock(&self.fonts);
        // Only a handle whose own bytes are at that address is the font:
        // bytes the cache does not hold may have been freed, and others
        // loaded at their address since.
        if let Some(font) = fonts.get(&key)
            && font.addr() == key.0
        {
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

    /// Shapes paragraphs, each memoised, and then forgets the paragraphs
    /// that no call has asked for in a while.
    ///
    /// The paragraphs not in the cache are shaped in parallel on threads of
    /// the cache's own, which share no state with the caller's thread. The
    /// caller waits for them without running other work, so it may hold
    /// thread-local state, as a thread running a database query does, and
    /// may itself be a thread of another `rayon` pool. When the shaping
    /// pool has one thread (as with `RAYON_NUM_THREADS=1`), the caller
    /// shapes them itself, in order.
    ///
    /// # Panics
    ///
    /// When a paragraph's segments do not cover its text at character
    /// boundaries.
    pub fn shape_all(&self, paragraphs: &[Paragraph]) -> Vec<Arc<Shaped>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let mut out: Vec<Option<Arc<Shaped>>> = vec![None; paragraphs.len()];
        // The paragraphs to shape, each once, with where their results go.
        let mut misses: Vec<(Paragraph, Vec<usize>)> = Vec::new();
        {
            let mut cached = lock(&self.paragraphs);
            let mut missing: HashMap<&Paragraph, usize> = HashMap::new();
            for (i, paragraph) in paragraphs.iter().enumerate() {
                if let Some(entry) = cached.get_mut(paragraph) {
                    entry.used = now;
                    out[i] = Some(entry.shaped.clone());
                } else if let Some(&m) = missing.get(paragraph) {
                    misses[m].1.push(i);
                } else {
                    missing.insert(paragraph, misses.len());
                    misses.push((paragraph.clone(), vec![i]));
                }
            }
        }

        let shaped = shape_many(misses.iter().map(|(p, _)| p.clone()).collect());
        let mut cached = lock(&self.paragraphs);
        for ((paragraph, targets), shaped) in misses.into_iter().zip(shaped) {
            let shaped = cached
                .entry(paragraph)
                .or_insert(Entry {
                    shaped: Arc::new(shaped),
                    used: now,
                })
                .shaped
                .clone();
            for i in targets {
                out[i] = Some(shaped.clone());
            }
        }
        drop(cached);

        if now.is_multiple_of(8) {
            self.evict(now);
        }
        out.into_iter()
            .map(|s| s.expect("every paragraph is shaped"))
            .collect()
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

/// Shapes paragraphs, in parallel on the shaping threads when there are
/// several paragraphs and several threads. The caller blocks on a channel,
/// which runs no other work, rather than in a `rayon` join, which may.
///
/// With a single shaping thread (`RAYON_NUM_THREADS=1`), handing the job over
/// gains nothing, so the caller shapes in order itself. This also keeps the
/// shaping on the thread that the instruction-count benchmarks measure.
fn shape_many(paragraphs: Vec<Paragraph>) -> Vec<Shaped> {
    let pool = POOL
        .as_ref()
        .filter(|pool| paragraphs.len() > 1 && pool.current_num_threads() > 1);
    let Some(pool) = pool else {
        return paragraphs.iter().map(shape).collect();
    };
    let (tx, rx) = mpsc::channel();
    pool.spawn(move || {
        // The job owns its paragraphs and drops them after a panic; the
        // shared state it touches, the plans of its fonts, tolerates a
        // panic while their lock is held.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            paragraphs.par_iter().map(shape).collect::<Vec<Shaped>>()
        }));
        // The caller is waiting for this.
        let _ = tx.send(result);
    });
    match rx.recv() {
        Ok(Ok(shaped)) => shaped,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => unreachable!("the shaping job always sends its result"),
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
