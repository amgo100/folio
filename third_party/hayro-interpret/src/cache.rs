use crate::util::hash128;
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Null, ObjRef, Object, Stream};
use kurbo::Affine;
use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type CacheMap = HashMap<u128, Option<Box<dyn Any + Send + Sync>>>;
// Folio: the cache also carries the limits of one interpretation, as every
// nested context, pattern, soft mask and image shares it (see `Budget`).
#[derive(Clone)]
pub(crate) struct Cache(Arc<Mutex<CacheMap>>, pub(crate) Arc<Budget>);

// Folio: what one interpretation of a page (with every form, pattern, soft
// mask and glyph it draws) may do, so that a hostile file ends in bounded
// time: the work is counted (an operator is one unit, a nested
// interpretation `NESTED_COST`) and stops at `MAX_WORK`, and the images
// decoded together may take at most `hayro_syntax::MAX_IMAGE_BYTES`. Each
// limit reached sends a warning to the settings' warning sink. (A form
// XObject that draws itself is not drawn again: see `Context::forms`.)
/// The most work one interpretation does (Folio).
pub(crate) const MAX_WORK: u64 = 10_000_000;
/// The work a nested interpretation costs (Folio).
pub(crate) const NESTED_COST: u64 = 50;

/// The images an interpretation has decoded: their bytes together, and
/// the objects among them, which count once however often they are drawn
/// (Folio).
#[derive(Default)]
pub(crate) struct Images {
    bytes: u64,
    objects: std::collections::HashSet<u128>,
}

#[derive(Default)]
pub(crate) struct Budget {
    work: AtomicU64,
    images: Mutex<Images>,
    exhausted: AtomicBool,
}

impl Budget {
    /// Spends `units` of work; `false` once the work is used up, with one
    /// warning the first time.
    pub(crate) fn spend(&self, units: u64, sink: &crate::WarningSinkFn) -> bool {
        let before = self.work.fetch_add(units, Ordering::Relaxed);
        if before.saturating_add(units) <= MAX_WORK {
            return true;
        }
        if !self.exhausted.swap(true, Ordering::Relaxed) {
            sink(crate::InterpreterWarning::TooComplex);
        }
        false
    }

    /// Takes `bytes` of decoded image data from what the interpretation may
    /// decode; `false`, taking nothing, when that is more than is left. An
    /// image (`key`) is taken once, however often it is decoded. Each
    /// decoding also spends work. (Each drawing of an image can keep a copy
    /// of it, as `hayro-svg` writes one per drawing; Folio limits those
    /// copies in the drawing it converts to.)
    pub(crate) fn take_image_bytes(
        &self,
        key: u128,
        bytes: u64,
        sink: &crate::WarningSinkFn,
    ) -> bool {
        // Every decoding is work, as a nested interpretation is, and in
        // proportion to its size (a unit a kilobyte), so that an image drawn
        // over and over ends too, however small: each drawing is a copy in
        // what a device such as `hayro-svg` writes.
        if !self.spend(NESTED_COST + bytes / 1024, sink) {
            return false;
        }
        let mut images = self.images.lock().unwrap();
        if images.objects.contains(&key) {
            return true;
        }
        let total = images.bytes.saturating_add(bytes);
        if total > hayro_syntax::MAX_IMAGE_BYTES {
            drop(images);
            sink(crate::InterpreterWarning::ImageTooLarge);
            return false;
        }
        images.bytes = total;
        images.objects.insert(key);
        true
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())), Arc::default())
    }

    pub(crate) fn get_or_insert_with<T: Clone + Send + Sync + 'static>(
        &self,
        id: u128,
        f: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let mut locked = self.0.lock().unwrap();

        // We can't use `get_or_insert_with` here, because if the closure makes another access to the
        // cache, we end up with a deadlock.
        match locked.entry(id) {
            Entry::Occupied(o) => o
                .get()
                .as_ref()
                .and_then(|val| val.downcast_ref::<T>().cloned()),
            Entry::Vacant(_) => {
                drop(locked);
                let val = f();
                self.0.lock().unwrap().insert(
                    id,
                    val.clone()
                        .map(|val| Box::new(val) as Box<dyn Any + Send + Sync>),
                );

                val
            }
        }
    }
}

/// A trait for objects that can generate a unique cache key.
pub trait CacheKey {
    /// Returns the cache key for this object.
    fn cache_key(&self) -> u128;
}

impl<T: CacheKey, U: CacheKey> CacheKey for (T, U) {
    fn cache_key(&self) -> u128 {
        hash128(&(self.0.cache_key(), self.1.cache_key()))
    }
}

impl CacheKey for Dict<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Stream<'_> {
    fn cache_key(&self) -> u128 {
        self.dict().cache_key()
    }
}

impl CacheKey for Null {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for bool {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for hayro_syntax::object::Number {
    fn cache_key(&self) -> u128 {
        hash128(&self.as_f64().to_bits())
    }
}

impl CacheKey for hayro_syntax::object::String<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.as_ref())
    }
}

impl CacheKey for Name<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for Array<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Object<'_> {
    fn cache_key(&self) -> u128 {
        match self {
            Object::Null(n) => n.cache_key(),
            Object::Boolean(b) => b.cache_key(),
            Object::Number(n) => n.cache_key(),
            Object::String(s) => s.cache_key(),
            Object::Name(n) => n.cache_key(),
            Object::Dict(d) => d.cache_key(),
            Object::Array(a) => a.cache_key(),
            Object::Stream(s) => s.cache_key(),
        }
    }
}

impl CacheKey for ObjRef {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl<T: CacheKey> CacheKey for MaybeRef<T> {
    fn cache_key(&self) -> u128 {
        match self {
            Self::Ref(r) => r.cache_key(),
            Self::NotRef(o) => o.cache_key(),
        }
    }
}

impl CacheKey for Affine {
    fn cache_key(&self) -> u128 {
        let c = self.as_coeffs();
        hash128(&[
            c[0].to_bits(),
            c[1].to_bits(),
            c[2].to_bits(),
            c[3].to_bits(),
            c[4].to_bits(),
            c[5].to_bits(),
        ])
    }
}

impl CacheKey for u128 {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}
