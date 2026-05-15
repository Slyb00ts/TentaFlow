// =============================================================================
// File: services/frame_storage/lru.rs — LRU-backed frame cache implementation
// =============================================================================
//
// Bounded shared cache: oldest entries are evicted when capacity is reached.
// `get` is a copy of the metadata + a cheap `Arc<[u8]>` clone of the byte
// payload; `remove` is the one-shot semantics path used by the
// Service-to-Core PickupToken flow.

use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use lru::LruCache;
use parking_lot::Mutex;

/// Opaque handle to a frame stored in the cache. Format: `frame_<uuid-v4>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawFrameRef(String);

impl RawFrameRef {
    pub fn new() -> Self {
        Self(format!("frame_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl Default for RawFrameRef {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RawFrameRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Pixel format carried in `FrameMetadata`. F1a only ever produces `Rgb24`
/// (the GStreamer pipeline forces `video/x-raw,format=RGB`); future
/// connectors will add variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePixelFormat {
    Rgb24,
}

#[derive(Debug, Clone)]
pub struct FrameMetadata {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: FramePixelFormat,
    pub timestamp_unix_ms: u64,
    /// GStreamer presentation timestamp in nanoseconds when available.
    pub pts: Option<u64>,
    pub frame_size_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct StoredFrame {
    pub metadata: FrameMetadata,
    pub data: Arc<[u8]>,
    pub created_at: Instant,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FrameStorageStats {
    pub inserted_total: u64,
    pub evicted_total: u64,
    pub removed_total: u64,
    pub hit_count: u64,
    pub miss_count: u64,
    pub cache_size: u64,
    pub capacity: u64,
}

pub struct FrameStorage {
    capacity: NonZeroUsize,
    inner: Mutex<LruCache<RawFrameRef, StoredFrame>>,
    inserted_total: AtomicU64,
    evicted_total: AtomicU64,
    removed_total: AtomicU64,
    hit_count: AtomicU64,
    miss_count: AtomicU64,
}

impl FrameStorage {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity >= 1");
        Self {
            capacity: cap,
            inner: Mutex::new(LruCache::new(cap)),
            inserted_total: AtomicU64::new(0),
            evicted_total: AtomicU64::new(0),
            removed_total: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
        }
    }

    /// Insert a frame, returning the freshly generated ref. If the cache is at
    /// capacity, the least-recently-used entry is evicted atomically — we
    /// allocate the ref before taking the lock to keep the critical section
    /// minimal.
    pub fn insert(&self, frame: StoredFrame) -> RawFrameRef {
        let r = RawFrameRef::new();
        let mut g = self.inner.lock();
        if g.len() >= self.capacity.get() {
            // `put` will perform LRU eviction; explicitly accounting for it
            // here lets stats track evictions separately from misses.
            self.evicted_total.fetch_add(1, Ordering::Relaxed);
        }
        g.put(r.clone(), frame);
        self.inserted_total.fetch_add(1, Ordering::Relaxed);
        r
    }

    /// Read access — bumps the entry's LRU position so an active reader keeps
    /// the frame alive.
    pub fn get(&self, frame_ref: &RawFrameRef) -> Option<StoredFrame> {
        let mut g = self.inner.lock();
        match g.get(frame_ref) {
            Some(f) => {
                self.hit_count.fetch_add(1, Ordering::Relaxed);
                Some(f.clone())
            }
            None => {
                self.miss_count.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// One-shot consume — Chunk C's PickupToken flow uses this to make a frame
    /// unavailable after the first fetch.
    pub fn remove(&self, frame_ref: &RawFrameRef) -> Option<StoredFrame> {
        let mut g = self.inner.lock();
        let v = g.pop(frame_ref);
        if v.is_some() {
            self.removed_total.fetch_add(1, Ordering::Relaxed);
        }
        v
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    pub fn stats(&self) -> FrameStorageStats {
        FrameStorageStats {
            inserted_total: self.inserted_total.load(Ordering::Relaxed),
            evicted_total: self.evicted_total.load(Ordering::Relaxed),
            removed_total: self.removed_total.load(Ordering::Relaxed),
            hit_count: self.hit_count.load(Ordering::Relaxed),
            miss_count: self.miss_count.load(Ordering::Relaxed),
            cache_size: self.len() as u64,
            capacity: self.capacity.get() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_frame(camera_id: &str, payload: &[u8]) -> StoredFrame {
        StoredFrame {
            metadata: FrameMetadata {
                camera_id: camera_id.into(),
                width: 4,
                height: 2,
                pixel_format: FramePixelFormat::Rgb24,
                timestamp_unix_ms: 1,
                pts: None,
                frame_size_bytes: payload.len(),
            },
            data: Arc::from(payload.to_vec().into_boxed_slice()),
            created_at: Instant::now(),
        }
    }

    #[test]
    fn test_lru_insert_get_basic() {
        let s = FrameStorage::new(8);
        let r = s.insert(mk_frame("cam", &[1, 2, 3]));
        let got = s.get(&r).expect("hit");
        assert_eq!(&*got.data, &[1, 2, 3]);
        assert_eq!(got.metadata.camera_id, "cam");
        let stats = s.stats();
        assert_eq!(stats.inserted_total, 1);
        assert_eq!(stats.hit_count, 1);
    }

    #[test]
    fn test_lru_eviction_at_capacity() {
        let s = FrameStorage::new(3);
        let r1 = s.insert(mk_frame("c", &[1]));
        let r2 = s.insert(mk_frame("c", &[2]));
        let r3 = s.insert(mk_frame("c", &[3]));
        let _r4 = s.insert(mk_frame("c", &[4]));
        let _r5 = s.insert(mk_frame("c", &[5]));
        assert!(s.get(&r1).is_none(), "oldest evicted");
        assert!(s.get(&r2).is_none(), "second oldest evicted");
        assert!(s.get(&r3).is_some(), "third remains");
        let stats = s.stats();
        assert_eq!(stats.inserted_total, 5);
        assert_eq!(stats.evicted_total, 2);
        assert_eq!(stats.cache_size, 3);
        assert_eq!(stats.capacity, 3);
    }

    #[test]
    fn test_lru_remove_one_shot() {
        let s = FrameStorage::new(4);
        let r = s.insert(mk_frame("c", &[9]));
        let taken = s.remove(&r).expect("present");
        assert_eq!(&*taken.data, &[9]);
        assert!(s.get(&r).is_none(), "removed entry stays gone");
        let stats = s.stats();
        assert_eq!(stats.removed_total, 1);
        assert_eq!(stats.miss_count, 1);
    }

    #[test]
    fn test_lru_stats() {
        let s = FrameStorage::new(2);
        assert_eq!(s.stats().capacity, 2);
        let r1 = s.insert(mk_frame("c", &[1]));
        let r2 = s.insert(mk_frame("c", &[2]));
        let _r3 = s.insert(mk_frame("c", &[3]));
        // r1 should be evicted (LRU order). r2/r3 remain.
        assert!(s.get(&r1).is_none());
        assert!(s.get(&r2).is_some());
        let stats = s.stats();
        assert_eq!(stats.inserted_total, 3);
        assert_eq!(stats.evicted_total, 1);
        assert_eq!(stats.miss_count, 1);
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.cache_size, 2);
    }
}
