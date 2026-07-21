// =============================================================================
// Plik: kv.rs
// Opis: Zarządza stronicowanym cache K/V oraz przydziałem stron sekwencji.
// Przykład: KvCache::new(device, config)
// =============================================================================

use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{DType, ForgeError, MemKind, Result};

/// KV cache storage mode. F16/Fp8 store the cache verbatim in that element type
/// (bit-exact canonical paths). Rot stores the full history as a Walsh-Hadamard
/// rotated + low-bit (3/4-bit) packed region with a per-(token,head) f16 scale
/// (TurboQuant-class; SPEC.md §5.5), plus a small residual ring keeping the
/// most-recent `residual_window` tokens at rotated f16 fidelity. There is NO
/// full-context f16 slab: total rot KV ≈ packed(all) + f16(window) ≈ 0.5 B/elem,
/// ~3.9× (rot4) / ~5× (rot3) less than the f16 cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvQuant {
    F16,
    Fp8,
    Rot {
        /// 3 or 4.
        bits: u8,
        /// Most-recent tokens kept at rotated f16 fidelity in the residual ring
        /// (SPEC default 128); older tokens are served from the low-bit store.
        residual_window: usize,
        /// Context length past which a sequence switches to the rotational
        /// store (SPEC default 4096 — rotation overhead loses below it).
        activate_at: usize,
    },
}

impl KvQuant {
    /// Element type of the f16/fp8 cache. Rot has no f16 slab; this is the
    /// element type of its residual ring (f16) for sizing only.
    pub fn slab_dtype(self) -> DType {
        match self {
            KvQuant::Fp8 => DType::F8E4M3,
            KvQuant::F16 | KvQuant::Rot { .. } => DType::F16,
        }
    }

    pub fn is_rot(self) -> bool {
        matches!(self, KvQuant::Rot { .. })
    }

    pub fn bits(self) -> Option<u8> {
        match self {
            KvQuant::Rot { bits, .. } => Some(bits),
            _ => None,
        }
    }

    /// Residual-window ring depth in tokens (rot only), clamped to at least 1
    /// so `pos % ring_slots` is well-defined in the kernels.
    pub fn ring_slots(self) -> Option<usize> {
        match self {
            KvQuant::Rot {
                residual_window, ..
            } => Some(residual_window.max(1)),
            _ => None,
        }
    }

    /// Packed low-bit bytes per (token, head) vector for `head_dim`.
    pub fn packed_bytes(self, head_dim: usize) -> Option<usize> {
        self.bits().map(|b| head_dim * b as usize / 8)
    }
}

pub struct KvConfig {
    pub n_layers: usize,
    pub n_kv_heads: usize,
    pub head_dim: usize,
    pub page_size: usize,
    pub n_pages: usize,
    pub max_pages_per_seq: usize,
    /// Storage mode (F16 | Fp8 | Rot{..}); validated at model load.
    pub quant: KvQuant,
}

impl KvConfig {
    /// Element type of the f16/fp8 slab (F16 for rot).
    pub fn dtype(&self) -> DType {
        self.quant.slab_dtype()
    }

    /// Bytes of ONE page of each tier-spillable region of one layer, in the
    /// order `KvCache::tier_layer_regions` returns the buffers. F16/Fp8 spill
    /// the paged K and V slabs; rot spills the paged packed store + scales
    /// (the residual ring is a small global overlay and never spills).
    pub fn tier_region_bytes(&self) -> Vec<usize> {
        let slots = self.n_kv_heads * self.page_size;
        match self.quant.packed_bytes(self.head_dim) {
            Some(pb) => {
                let packed = slots * pb;
                let scale = slots * 2;
                vec![packed, packed, scale, scale]
            }
            None => {
                let page = slots * self.head_dim * self.dtype().size();
                vec![page, page]
            }
        }
    }
}

pub struct KvCache {
    pub cfg: KvConfig,
    /// K/V storage, one pair per layer. F16/Fp8: the full paged slab
    /// ([n_pages, n_kv_heads, page_size, head_dim]). Rot: the small residual
    /// ring ([ring_slots, n_kv_heads, head_dim] f16, rotated), indexed by
    /// `pos % ring_slots` — NO full-context slab.
    pub k: Vec<DevBuffer>,
    pub v: Vec<DevBuffer>,
    /// Rotational low-bit region (rot modes only; empty otherwise): densely
    /// packed 3/4-bit codes and per-(token,head) f16 amax scales, one pair of
    /// buffers per layer, addressed like the paged f16 slab would be
    /// ([n_pages, n_kv_heads, page_size, ·]) — the full history for reclaim.
    pub k_packed: Vec<DevBuffer>,
    pub v_packed: Vec<DevBuffer>,
    pub k_scale: Vec<DevBuffer>,
    pub v_scale: Vec<DevBuffer>,
    free_pages: Vec<i32>,
}

/// A contiguous run of logical pages spilled to a tier chunk (tier.rs). The
/// spilled region is always the oldest prefix of the sequence, so ranges are
/// contiguous from logical page 0 upward.
pub struct SpilledRange {
    pub first_page: usize,
    pub n_pages: usize,
    /// TierManager chunk id holding the pages' K/V bytes for every layer.
    pub chunk: u64,
}

/// One sequence's view of the cache: its page table (host mirror) and length.
/// With tiering, `pages` entries of spilled pages are -1 and `spilled` maps
/// them to tier chunks; `tokens` retains the token ids (recompute path).
pub struct SeqKv {
    /// Process-unique id; the model tracks which sequence's page table is
    /// currently uploaded to the device.
    pub id: u64,
    pub pages: Vec<i32>,
    pub len: usize,
    pub spilled: Vec<SpilledRange>,
    /// Token ids appended so far (recorded only when tiering is enabled).
    pub tokens: Vec<u32>,
    /// Prefix of `tokens` appended via prefill (bit-identically recomputable
    /// by re-prefilling; decode-appended KV is transfer-restored only).
    pub prefilled_len: usize,
    /// Leading `pages` entries borrowed from the radix prefix cache (SPEC §5.2):
    /// shared, read-only, and NOT freed on release — the cache owns them. The
    /// rest of `pages` is private to this sequence.
    pub shared_pages: usize,
    /// Prefix-cache node whose borrow this sequence holds (deepest matched
    /// prefix), released and extended on completion. `None` = no borrow.
    pub prefix_node: Option<crate::prefix::NodeId>,
}

impl SeqKv {
    /// First logical page still resident (everything below is spilled).
    pub fn resident_frontier(&self) -> usize {
        self.spilled
            .last()
            .map(|r| r.first_page + r.n_pages)
            .unwrap_or(0)
    }

    pub fn spilled_page_count(&self) -> usize {
        self.spilled.iter().map(|r| r.n_pages).sum()
    }
}

impl KvCache {
    pub fn new(device: &dyn Device, cfg: KvConfig) -> Result<Self> {
        let page_elems = cfg.n_kv_heads * cfg.page_size * cfg.head_dim;
        let slots = cfg.n_pages * cfg.n_kv_heads * cfg.page_size;
        // Rot allocates only the residual ring here (the full history lives in
        // the low-bit packed region below); f16/fp8 allocate the full paged
        // slab. `ring_slots` reuses the same page/head/head_dim element layout
        // but with `ring_slots` slots instead of `n_pages * page_size`.
        let kv_bytes = if let Some(ring_slots) = cfg.quant.ring_slots() {
            ring_slots
                .checked_mul(cfg.n_kv_heads * cfg.head_dim)
                .and_then(|e| e.checked_mul(cfg.dtype().size()))
                .ok_or_else(|| ForgeError::Scheduler("kv ring size overflow".into()))?
        } else {
            cfg.n_pages
                .checked_mul(page_elems)
                .and_then(|e| e.checked_mul(cfg.dtype().size()))
                .ok_or_else(|| ForgeError::Scheduler("kv cache size overflow".into()))?
        };
        let mut k = Vec::with_capacity(cfg.n_layers);
        let mut v = Vec::with_capacity(cfg.n_layers);
        // Bufory warstw zajmują ciągłe zakresy dedykowanej puli KV; logiczne
        // strony pozostają nakładką zarządzaną przez ten cache.
        for _ in 0..cfg.n_layers {
            k.push(device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?);
            v.push(device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?);
        }
        // Rotational low-bit region: packed codes (u8) + f16 scales per slot.
        let mut k_packed = Vec::new();
        let mut v_packed = Vec::new();
        let mut k_scale = Vec::new();
        let mut v_scale = Vec::new();
        if let Some(pb) = cfg.quant.packed_bytes(cfg.head_dim) {
            let packed_bytes = slots
                .checked_mul(pb)
                .ok_or_else(|| ForgeError::Scheduler("rot packed size overflow".into()))?;
            let scale_bytes = slots * 2;
            for _ in 0..cfg.n_layers {
                k_packed.push(device.alloc(packed_bytes, MemKind::Device, Pool::KvCache)?);
                v_packed.push(device.alloc(packed_bytes, MemKind::Device, Pool::KvCache)?);
                k_scale.push(device.alloc(scale_bytes, MemKind::Device, Pool::KvCache)?);
                v_scale.push(device.alloc(scale_bytes, MemKind::Device, Pool::KvCache)?);
            }
        }
        // Stack of free physical page ids shared across layers: a logical page
        // maps to the same physical index in every layer, halving bookkeeping.
        let free_pages = (0..cfg.n_pages as i32).rev().collect();
        Ok(KvCache {
            cfg,
            k,
            v,
            k_packed,
            v_packed,
            k_scale,
            v_scale,
            free_pages,
        })
    }

    /// Per-layer buffers the tier manager spills, in the region order of
    /// `KvConfig::tier_region_bytes`.
    pub fn tier_layer_regions(&self, l: usize) -> Vec<&DevBuffer> {
        if self.cfg.quant.is_rot() {
            vec![
                &self.k_packed[l],
                &self.v_packed[l],
                &self.k_scale[l],
                &self.v_scale[l],
            ]
        } else {
            vec![&self.k[l], &self.v[l]]
        }
    }

    pub fn new_seq(&self) -> SeqKv {
        static NEXT_SEQ_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        SeqKv {
            id: NEXT_SEQ_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            pages: Vec::new(),
            len: 0,
            spilled: Vec::new(),
            tokens: Vec::new(),
            prefilled_len: 0,
            shared_pages: 0,
            prefix_node: None,
        }
    }

    /// Ensure capacity for one more token; allocates a page on boundary.
    pub fn grow(&mut self, seq: &mut SeqKv) -> Result<()> {
        if seq.len.is_multiple_of(self.cfg.page_size) {
            if seq.pages.len() >= self.cfg.max_pages_per_seq {
                return Err(ForgeError::Scheduler(format!(
                    "sequence exceeds max_pages_per_seq {}",
                    self.cfg.max_pages_per_seq
                )));
            }
            let page = self
                .free_pages
                .pop()
                .ok_or_else(|| ForgeError::Scheduler("kv cache out of pages".into()))?;
            seq.pages.push(page);
        }
        seq.len += 1;
        Ok(())
    }

    /// Truncate a sequence back to `new_len` tokens, freeing any tail pages the
    /// shorter length no longer needs (speculative rollback: rejected draft
    /// positions are discarded). Pages are freed from the tail only, so a
    /// borrowed prefix's shared pages are never touched. The device page table
    /// still lists the freed slots beyond `new_len`; the next step re-uploads it
    /// (callers invalidate `pt_seq`), and attention never reads past `seq.len`.
    pub fn rollback(&mut self, seq: &mut SeqKv, new_len: usize) {
        debug_assert!(new_len <= seq.len, "rollback target beyond current len");
        let needed = if new_len == 0 {
            0
        } else {
            new_len.div_ceil(self.cfg.page_size)
        };
        debug_assert!(
            needed >= seq.shared_pages,
            "rollback must not drop borrowed shared pages"
        );
        while seq.pages.len() > needed {
            let page = seq.pages.pop().expect("pages non-empty while above needed");
            if page >= 0 {
                self.free_pages.push(page);
            }
        }
        seq.len = new_len;
    }

    pub fn release(&mut self, seq: &mut SeqKv) {
        // Spilled entries are -1 (their bytes live in tier chunks, dropped by
        // the tier manager) and must not enter the free-page stack.
        self.free_pages
            .extend(seq.pages.drain(..).filter(|&p| p >= 0));
        seq.len = 0;
        seq.spilled.clear();
        seq.tokens.clear();
        seq.prefilled_len = 0;
        seq.shared_pages = 0;
        seq.prefix_node = None;
    }

    pub fn free_page_count(&self) -> usize {
        self.free_pages.len()
    }

    pub(crate) fn pop_free(&mut self) -> Option<i32> {
        self.free_pages.pop()
    }

    pub(crate) fn push_free(&mut self, page: i32) {
        self.free_pages.push(page);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use forge_hal::cpu::CpuDevice;
    use forge_hal::{Event, ExecGraph, KernelHandle, LaunchArgs, LaunchConfig, Module, Stream};
    use forge_types::DeviceCaps;

    use super::*;

    struct PoolTrackingDevice {
        inner: Arc<dyn Device>,
        allocations: Mutex<Vec<(usize, MemKind, Pool)>>,
    }

    impl PoolTrackingDevice {
        fn new() -> Self {
            Self {
                inner: CpuDevice::new(),
                allocations: Mutex::new(Vec::new()),
            }
        }

        fn pools(&self) -> Vec<Pool> {
            self.allocations
                .lock()
                .expect("rejestr alokacji nie powinien być zatruty")
                .iter()
                .map(|(_, _, pool)| *pool)
                .collect()
        }
    }

    impl Device for PoolTrackingDevice {
        fn caps(&self) -> &DeviceCaps {
            self.inner.caps()
        }

        fn alloc(&self, bytes: usize, kind: MemKind, pool: Pool) -> Result<DevBuffer> {
            self.allocations
                .lock()
                .expect("rejestr alokacji nie powinien być zatruty")
                .push((bytes, kind, pool));
            self.inner.alloc(bytes, kind, pool)
        }

        fn pool_available(&self, pool: Pool) -> Option<usize> {
            self.inner.pool_available(pool)
        }

        fn create_stream(&self) -> Result<Stream> {
            self.inner.create_stream()
        }

        fn create_event(&self) -> Result<Event> {
            self.inner.create_event()
        }

        fn record_event(&self, event: &Event, stream: &Stream) -> Result<()> {
            self.inner.record_event(event, stream)
        }

        fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
            self.inner.wait_event(stream, event)
        }

        fn elapsed_event_ms(&self, start: &Event, end: &Event) -> Result<Option<f32>> {
            self.inner.elapsed_event_ms(start, end)
        }

        fn copy(
            &self,
            src: &DevBuffer,
            src_offset: usize,
            dst: &DevBuffer,
            dst_offset: usize,
            bytes: usize,
            stream: &Stream,
        ) -> Result<()> {
            self.inner
                .copy(src, src_offset, dst, dst_offset, bytes, stream)
        }

        fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
            self.inner.write(src, dst, dst_offset)
        }

        fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
            self.inner.read(src, src_offset, dst)
        }

        fn load_module(&self, image: &[u8]) -> Result<Module> {
            self.inner.load_module(image)
        }

        fn launch(
            &self,
            kernel: &KernelHandle,
            cfg: &LaunchConfig,
            args: &LaunchArgs,
            stream: &Stream,
        ) -> Result<()> {
            self.inner.launch(kernel, cfg, args, stream)
        }

        fn synchronize(&self) -> Result<()> {
            self.inner.synchronize()
        }

        fn begin_capture(&self, stream: &Stream) -> Result<()> {
            self.inner.begin_capture(stream)
        }

        fn end_capture(&self, stream: &Stream) -> Result<ExecGraph> {
            self.inner.end_capture(stream)
        }

        fn launch_graph(&self, graph: &ExecGraph, stream: &Stream) -> Result<()> {
            self.inner.launch_graph(graph, stream)
        }

        fn reset_activations(&self) -> Result<u64> {
            self.inner.reset_activations()
        }
    }

    fn config(quant: KvQuant) -> KvConfig {
        KvConfig {
            n_layers: 2,
            n_kv_heads: 2,
            head_dim: 32,
            page_size: 4,
            n_pages: 3,
            max_pages_per_seq: 3,
            quant,
        }
    }

    #[test]
    fn wszystkie_bufory_kv_uzywaja_dedykowanej_puli() {
        let device = PoolTrackingDevice::new();

        let f16 = KvCache::new(&device, config(KvQuant::F16)).unwrap();
        let rot = KvCache::new(
            &device,
            config(KvQuant::Rot {
                bits: 4,
                residual_window: 8,
                activate_at: 16,
            }),
        )
        .unwrap();
        let pools = device.pools();

        assert_eq!(pools.len(), 16);
        assert!(pools.iter().all(|pool| *pool == Pool::KvCache));
        drop((f16, rot));
    }

    #[test]
    fn release_oddaje_wszystkie_przydzielone_strony() {
        let device = CpuDevice::new();
        let mut cache = KvCache::new(device.as_ref(), config(KvQuant::F16)).unwrap();
        let mut seq = cache.new_seq();

        for _ in 0..9 {
            cache.grow(&mut seq).unwrap();
        }
        assert_eq!(cache.free_page_count(), 0);
        cache.release(&mut seq);

        assert_eq!(cache.free_page_count(), cache.cfg.n_pages);
        assert_eq!(seq.len, 0);
        assert!(seq.pages.is_empty());
    }
}
