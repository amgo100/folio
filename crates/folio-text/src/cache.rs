//! Memoised, parallel shaping (SPEC §2.3).

use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, mpsc};

use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::paragraph::{Paragraph, Shaped, shape};
use crate::shape::TextFont;

/// How many calls of [`Cache::shape_all`] a shaped paragraph is kept
/// without being asked for.
const MAX_AGE: u64 = 32;

/// The most results of [`Cache::parallel`] that a thread sends back at
/// once: few enough that the caller, which checks for cancellation as it
/// takes them, is not kept waiting for long (SPEC §2.3).
const CHUNK: usize = 8;

/// The threads that shape paragraphs and run the other work handed to
/// [`Cache::parallel`], shared by every cache of the process. They run
/// nothing else, so a caller waiting for them never runs other work of its
/// own thread pool meanwhile.
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
/// [`shape_all`](Self::shape_all), which shapes the new ones in parallel
/// and forgets those that no call asked for in a while; an edit therefore
/// re-shapes only the paragraphs it changed.
#[derive(Default)]
pub struct Cache {
    paragraphs: Mutex<HashMap<Paragraph, Entry>>,
    fonts: Mutex<HashMap<(usize, usize, u32), TextFont>>,
    /// Counts the calls of `shape_all`.
    clock: AtomicU64,
    /// The clock when the cache last forgot paragraphs.
    evicted: AtomicU64,
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
    /// may itself be a thread of another `rayon` pool.
    ///
    /// # Panics
    ///
    /// When a paragraph's segments do not cover its text at character
    /// boundaries.
    pub fn shape_all(&self, paragraphs: &[Paragraph]) -> Vec<Arc<Shaped>> {
        self.shape_all_checked(paragraphs, &mut || {})
    }

    /// Shapes paragraphs as [`shape_all`](Self::shape_all) does, calling
    /// `check` on the caller's thread after each paragraph it shapes.
    ///
    /// `check` may unwind, as a database query that its revision has
    /// cancelled does (SPEC §2.3): the paragraphs not yet shaped are then
    /// dropped, and those shaped are kept in the cache for the next call.
    ///
    /// # Panics
    ///
    /// When a paragraph's segments do not cover its text at character
    /// boundaries, or `check` panics.
    pub fn shape_all_checked(
        &self,
        paragraphs: &[Paragraph],
        check: &mut dyn FnMut(),
    ) -> Vec<Arc<Shaped>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let mut out: Vec<Option<Arc<Shaped>>> = vec![None; paragraphs.len()];
        // The paragraphs to shape, each once, with where their results go.
        let mut misses: Vec<Paragraph> = Vec::new();
        let mut targets: Vec<Vec<usize>> = Vec::new();
        {
            let mut cached = lock(&self.paragraphs);
            let mut missing: HashMap<&Paragraph, usize> = HashMap::new();
            for (i, paragraph) in paragraphs.iter().enumerate() {
                if let Some(entry) = cached.get_mut(paragraph) {
                    entry.used = now;
                    out[i] = Some(entry.shaped.clone());
                } else if let Some(&m) = missing.get(paragraph) {
                    targets[m].push(i);
                } else {
                    missing.insert(paragraph, misses.len());
                    misses.push(paragraph.clone());
                    targets.push(vec![i]);
                }
            }
        }

        Self::parallel(misses, shape, &mut |m, paragraph, shaped| {
            let shaped = lock(&self.paragraphs)
                .entry(paragraph.clone())
                .or_insert(Entry {
                    shaped: Arc::new(shaped),
                    used: now,
                })
                .shaped
                .clone();
            for &i in &targets[m] {
                out[i] = Some(shaped.clone());
            }
            check();
        });

        // Every eighth call, counted from the last that forgot: a call that
        // a cancelled query left unfinished does not skip it.
        let last = self.evicted.load(Ordering::Relaxed);
        if now >= last + 8
            && self
                .evicted
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.evict(now);
        }
        out.into_iter()
            .map(|s| s.expect("every paragraph is shaped"))
            .collect()
    }

    /// Runs `f` on each item, in parallel on the threads that shape
    /// paragraphs when there are several items, and hands each result to
    /// `each` on the caller's thread with the item's index and the item,
    /// as the results come in, a few at a time.
    ///
    /// The order in which `each` sees the results depends on scheduling;
    /// the index says where each belongs. The caller blocks on a channel,
    /// which runs no other work, rather than in a `rayon` join, which may,
    /// so it may hold thread-local state, as a thread running a database
    /// query does, and may itself be a thread of another `rayon` pool.
    ///
    /// `each` may unwind, as a database query that its revision has
    /// cancelled does (SPEC §2.3): the items not yet started are then
    /// skipped, and the threads drop the items they were working on when
    /// they finish them.
    ///
    /// # Panics
    ///
    /// When `f` panics on an item (with its panic, on the caller's thread),
    /// or `each` panics.
    pub fn parallel<I, O, F>(items: Vec<I>, f: F, each: &mut dyn FnMut(usize, &I, O))
    where
        I: Send + Sync + 'static,
        O: Send + 'static,
        F: Fn(&I) -> O + Send + Sync + 'static,
    {
        let pool = POOL.as_ref().filter(|_| items.len() > 1);
        let Some(pool) = pool else {
            for (i, item) in items.iter().enumerate() {
                each(i, item, f(item));
            }
            return;
        };
        let items = Arc::new(items);
        let stop = Arc::new(AtomicBool::new(false));
        // Tells the job to start no more items when the caller is done
        // with it, by unwinding too.
        let _stop = Stop(stop.clone());
        // The results go back a few at a time, so that the caller wakes
        // less often than once per item, and enough chunks are left to
        // balance the threads' load.
        let size = (items.len() / (pool.current_num_threads() * 4)).clamp(1, CHUNK);
        let (tx, rx) = mpsc::channel();
        let job = items.clone();
        pool.spawn(move || {
            // The job shares its items and drops them after a panic; the
            // shared state that shaping touches, the plans of its fonts,
            // tolerates a panic while their lock is held.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job.par_chunks(size)
                    .enumerate()
                    .for_each_with(tx.clone(), |tx, (c, chunk)| {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let start = c * size;
                        let mut out = Vec::with_capacity(chunk.len());
                        for (j, item) in chunk.iter().enumerate() {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item)));
                            match result {
                                Ok(result) => out.push((start + j, result)),
                                // The caller gets the panic at once, and
                                // the other threads stop.
                                Err(panic) => {
                                    stop.store(true, Ordering::Relaxed);
                                    let _ = tx.send(Err(panic));
                                    return;
                                }
                            }
                        }
                        // The caller may have stopped listening.
                        let _ = tx.send(Ok(out));
                    });
            }));
            if let Err(panic) = result {
                let _ = tx.send(Err(panic));
            }
        });
        let mut left = items.len();
        while left > 0 {
            match rx.recv() {
                Ok(Ok(out)) => {
                    left -= out.len();
                    for (i, out) in out {
                        each(i, &items[i], out);
                    }
                }
                Ok(Err(panic)) => std::panic::resume_unwind(panic),
                Err(_) => unreachable!("the job sends every result or its panic"),
            }
        }
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

/// Sets its flag when dropped, even by unwinding.
struct Stop(Arc<AtomicBool>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Locks a mutex. A thread that panicked while holding it left its map
/// whole: each insertion or removal is one call.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
