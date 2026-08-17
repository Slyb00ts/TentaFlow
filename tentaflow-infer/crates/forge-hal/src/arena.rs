// ===== File: arena.rs — VRAM sub-allocators: bump (weights), page slab (KV cache), generation ring (activations) =====
//
// All three operate on plain byte offsets into a base allocation obtained once
// at device init, so no driver allocation ever happens in the hot path. They
// are backend-agnostic (pure arithmetic) and unit-tested on the host.

use forge_types::{ForgeError, Result};
use std::collections::BTreeMap;

/// Alignment applied to every sub-allocation. 256 B satisfies all CUDA
/// texture/vector-load requirements and keeps kernels free to use the widest
/// load instructions.
pub(crate) use crate::DEVICE_ALLOC_ALIGN as ALLOC_ALIGN;

// Checked: near-usize::MAX requests must surface as OOM, not wrap around and
// alias live allocations in release builds.
fn align_up(value: usize, align: usize) -> Option<usize> {
    value.checked_next_multiple_of(align)
}

/// Monotonic bump allocator for weights: individual frees are no-ops because
/// weights live for the model lifetime; reclamation is whole-pool reset on
/// model unload.
pub(crate) struct BumpArena {
    capacity: usize,
    cursor: usize,
}

impl BumpArena {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            cursor: 0,
        }
    }

    pub(crate) fn alloc(&mut self, bytes: usize) -> Result<usize> {
        let end = align_up(bytes.max(1), ALLOC_ALIGN)
            .and_then(|size| self.cursor.checked_add(size))
            .filter(|&end| end <= self.capacity)
            .ok_or(ForgeError::OutOfMemory {
                requested: bytes,
                available: self.capacity - self.cursor,
            })?;
        let offset = self.cursor;
        self.cursor = end;
        Ok(offset)
    }

    pub(crate) fn available(&self) -> usize {
        self.capacity - self.cursor
    }
}

/// Page-granular free-list allocator for the KV cache. Allocations are rounded
/// up to whole pages; adjacent free ranges are coalesced so multi-page
/// contiguous requests keep succeeding under churn.
pub(crate) struct SlabArena {
    page_size: usize,
    capacity: usize,
    /// offset -> length, page-aligned, non-adjacent (coalesced on free).
    free: BTreeMap<usize, usize>,
}

impl SlabArena {
    pub(crate) fn new(capacity: usize, page_size: usize) -> Result<Self> {
        if page_size == 0 || !page_size.is_multiple_of(ALLOC_ALIGN) {
            return Err(ForgeError::Device(format!(
                "KV page size {page_size} must be a non-zero multiple of {ALLOC_ALIGN}"
            )));
        }
        // Truncate to whole pages so the tail can never be handed out partially.
        let capacity = (capacity / page_size) * page_size;
        let mut free = BTreeMap::new();
        if capacity > 0 {
            free.insert(0, capacity);
        }
        Ok(Self {
            page_size,
            capacity,
            free,
        })
    }

    /// First-fit allocation of `bytes` rounded up to whole pages. Returns the
    /// byte offset and the rounded size actually reserved.
    pub(crate) fn alloc(&mut self, bytes: usize) -> Result<(usize, usize)> {
        let size = align_up(bytes.max(1), self.page_size).ok_or(ForgeError::OutOfMemory {
            requested: bytes,
            available: self.free.values().copied().max().unwrap_or(0),
        })?;
        let candidate = self
            .free
            .iter()
            .find(|(_, &len)| len >= size)
            .map(|(&off, &len)| (off, len));
        let Some((offset, len)) = candidate else {
            let available = self.free.values().copied().max().unwrap_or(0);
            return Err(ForgeError::OutOfMemory {
                requested: size,
                available,
            });
        };
        self.free.remove(&offset);
        if len > size {
            self.free.insert(offset + size, len - size);
        }
        Ok((offset, size))
    }

    /// Bytes still free, across all ranges.
    ///
    /// A sum and not the largest range: the caller sizing a KV budget asks how
    /// many equal slabs fit, and every one of them is a separate allocation.
    /// The largest range would answer a question nobody asks.
    pub(crate) fn available(&self) -> usize {
        self.free.values().sum()
    }

    /// Return a previously allocated range, coalescing with neighbours.
    pub(crate) fn free(&mut self, offset: usize, size: usize) {
        debug_assert!(offset.is_multiple_of(self.page_size) && size.is_multiple_of(self.page_size));
        debug_assert!(offset + size <= self.capacity);
        let mut start = offset;
        let mut len = size;
        if let Some((&prev_off, &prev_len)) = self.free.range(..offset).next_back() {
            if prev_off + prev_len == offset {
                self.free.remove(&prev_off);
                start = prev_off;
                len += prev_len;
            }
        }
        if let Some(&next_len) = self.free.get(&(offset + size)) {
            self.free.remove(&(offset + size));
            len += next_len;
        }
        self.free.insert(start, len);
    }
}

/// Generation-stamped ring for activations. Allocation is a bump; there is no
/// per-buffer free. `reset` retires the whole generation at once, which is the
/// per-iteration reclamation model of the execution layer. The live counter
/// exists to catch resets while the engine still holds buffers from the
/// current generation (use-after-reset would silently corrupt activations).
pub(crate) struct RingArena {
    capacity: usize,
    cursor: usize,
    generation: u64,
    live: usize,
}

impl RingArena {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            cursor: 0,
            generation: 0,
            live: 0,
        }
    }

    /// Returns (offset, generation). Exhaustion within a generation is an OOM:
    /// wrapping before `reset` would overwrite data the current iteration
    /// still reads.
    pub(crate) fn alloc(&mut self, bytes: usize) -> Result<(usize, u64)> {
        let end = align_up(bytes.max(1), ALLOC_ALIGN)
            .and_then(|size| self.cursor.checked_add(size))
            .filter(|&end| end <= self.capacity)
            .ok_or(ForgeError::OutOfMemory {
                requested: bytes,
                available: self.capacity - self.cursor,
            })?;
        let offset = self.cursor;
        self.cursor = end;
        self.live += 1;
        Ok((offset, self.generation))
    }

    /// Buffer drop notification; stale-generation drops are no-ops.
    pub(crate) fn on_drop(&mut self, generation: u64) {
        if generation == self.generation {
            self.live -= 1;
        }
    }

    pub(crate) fn available(&self) -> usize {
        self.capacity - self.cursor
    }

    pub(crate) fn reset(&mut self) -> Result<u64> {
        if self.live > 0 {
            return Err(ForgeError::Device(format!(
                "activations reset with {} live buffer(s) in generation {}",
                self.live, self.generation
            )));
        }
        self.cursor = 0;
        self.generation += 1;
        Ok(self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_aligns_and_exhausts() {
        let mut a = BumpArena::new(1024);
        assert_eq!(a.alloc(1).unwrap(), 0);
        assert_eq!(a.alloc(300).unwrap(), 256);
        // 1 B rounded to 256, 300 B rounded to 512 → cursor at 768.
        assert_eq!(a.available(), 256);
        assert_eq!(a.alloc(256).unwrap(), 768);
        assert_eq!(a.available(), 0);
        assert!(matches!(a.alloc(512), Err(ForgeError::OutOfMemory { .. })));
    }

    #[test]
    fn slab_reuses_freed_pages() {
        let mut s = SlabArena::new(4096, 1024).unwrap();
        let (o1, s1) = s.alloc(1000).unwrap();
        assert_eq!((o1, s1), (0, 1024));
        let (o2, _) = s.alloc(2048).unwrap();
        assert_eq!(o2, 1024);
        s.free(o1, s1);
        // Freed head page is reused for a fitting request (alloc-free-alloc
        // returns the same page).
        let (o3, _) = s.alloc(512).unwrap();
        assert_eq!(o3, 0);
    }

    #[test]
    fn slab_coalesces_neighbours() {
        let mut s = SlabArena::new(4096, 1024).unwrap();
        let (a, sa) = s.alloc(1024).unwrap();
        let (b, sb) = s.alloc(1024).unwrap();
        let (c, sc) = s.alloc(1024).unwrap();
        let (_d, _) = s.alloc(1024).unwrap();
        s.free(a, sa);
        s.free(c, sc);
        s.free(b, sb);
        // a+b+c coalesced back into one contiguous 3-page range.
        let (o, sz) = s.alloc(3 * 1024).unwrap();
        assert_eq!((o, sz), (0, 3072));
    }

    #[test]
    fn slab_rejects_oversized_contiguous_request() {
        let mut s = SlabArena::new(4096, 1024).unwrap();
        let (a, sa) = s.alloc(1024).unwrap();
        let (_b, _) = s.alloc(1024).unwrap();
        let (c, sc) = s.alloc(1024).unwrap();
        let (_d, _) = s.alloc(1024).unwrap();
        s.free(a, sa);
        s.free(c, sc);
        // 2 free pages exist but not contiguously.
        assert!(matches!(s.alloc(2048), Err(ForgeError::OutOfMemory { .. })));
    }

    #[test]
    fn ring_generations_gate_reset() {
        let mut r = RingArena::new(1024);
        let (o1, g1) = r.alloc(256).unwrap();
        assert_eq!((o1, g1), (0, 0));
        assert!(r.reset().is_err());
        r.on_drop(g1);
        assert_eq!(r.reset().unwrap(), 1);
        let (o2, g2) = r.alloc(256).unwrap();
        // Cursor rewound: same offset, new generation.
        assert_eq!((o2, g2), (0, 1));
        // Stale-generation drop must not disturb the live counter.
        r.on_drop(g1);
        assert!(r.reset().is_err());
        r.on_drop(g2);
        assert!(r.reset().is_ok());
    }

    #[test]
    fn near_max_requests_are_oom_not_overflow() {
        // Sizes near usize::MAX used to wrap in align_up / cursor arithmetic
        // in release builds, handing out an aliasing offset instead of OOM.
        for bytes in [usize::MAX, usize::MAX - 1, usize::MAX - ALLOC_ALIGN + 1] {
            let mut b = BumpArena::new(1024);
            b.alloc(512).unwrap();
            assert!(matches!(
                b.alloc(bytes),
                Err(ForgeError::OutOfMemory { .. })
            ));
            // The failed alloc must not have moved the cursor.
            assert_eq!(b.alloc(256).unwrap(), 512);

            let mut s = SlabArena::new(4096, 1024).unwrap();
            assert!(matches!(
                s.alloc(bytes),
                Err(ForgeError::OutOfMemory { .. })
            ));
            assert_eq!(s.alloc(1024).unwrap().0, 0);

            let mut r = RingArena::new(1024);
            r.alloc(512).unwrap();
            assert!(matches!(
                r.alloc(bytes),
                Err(ForgeError::OutOfMemory { .. })
            ));
            assert_eq!(r.alloc(256).unwrap().0, 512);
        }
    }

    #[test]
    fn ring_oom_within_generation() {
        let mut r = RingArena::new(512);
        let (_o, g) = r.alloc(512).unwrap();
        assert!(matches!(r.alloc(1), Err(ForgeError::OutOfMemory { .. })));
        r.on_drop(g);
    }
}
