// ===== File: kv.rs — paged KV cache (per-layer K/V page pools + page tables) =====
// Layout per layer: [n_pages, n_kv_heads, page_size, head_dim] elements of
// `dtype` (f16 canonical | fp8-e4m3, half the bytes/bandwidth), matching the
// attention kernels. v0 allocates one contiguous slab per layer and hands out
// logical pages; the HAL KvCache pool arena underneath keeps frees cheap.

use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{DType, ForgeError, MemKind, Result};

/// KV cache storage mode. F16/Fp8 store the cache verbatim in that element type
/// (bit-exact canonical paths). Rot stores older tokens as a Walsh-Hadamard
/// rotated + low-bit (3/4-bit) packed region with a per-(token,head) f16 scale
/// (TurboQuant-class; SPEC.md §5.5). The rot modes keep an f16 slab too so the
/// proven prefill attention runs bit-exact and the low-bit copy is committed
/// right after append; decode attention reads the packed region directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvQuant {
    F16,
    Fp8,
    Rot {
        /// 3 or 4.
        bits: u8,
        /// Most-recent tokens kept at f16 fidelity (config knob; SPEC default
        /// 128). Reported by the engine; the correctness-first phase quantizes
        /// the whole cached region, so this bounds the reported target only.
        residual_window: usize,
        /// Context length past which a sequence switches to the rotational
        /// store (SPEC default 4096 — rotation overhead loses below it).
        activate_at: usize,
    },
}

impl KvQuant {
    /// Element type of the f16/fp8 slab this mode allocates. Rot keeps an f16
    /// slab (prefill runs on it bit-exact); its low-bit region is separate.
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
}

pub struct KvCache {
    pub cfg: KvConfig,
    /// K/V slabs, one pair per layer.
    pub k: Vec<DevBuffer>,
    pub v: Vec<DevBuffer>,
    /// Rotational low-bit region (rot modes only; empty otherwise): densely
    /// packed 3/4-bit codes and per-(token,head) f16 amax scales, one pair of
    /// buffers per layer, addressed exactly like the f16 slab
    /// ([n_pages, n_kv_heads, page_size, ·]).
    pub k_packed: Vec<DevBuffer>,
    pub v_packed: Vec<DevBuffer>,
    pub k_scale: Vec<DevBuffer>,
    pub v_scale: Vec<DevBuffer>,
    free_pages: Vec<i32>,
}

/// One sequence's view of the cache: its page table (host mirror) and length.
pub struct SeqKv {
    pub pages: Vec<i32>,
    pub len: usize,
}

impl KvCache {
    pub fn new(device: &dyn Device, cfg: KvConfig) -> Result<Self> {
        let page_elems = cfg.n_kv_heads * cfg.page_size * cfg.head_dim;
        let slots = cfg.n_pages * cfg.n_kv_heads * cfg.page_size;
        let bytes = cfg
            .n_pages
            .checked_mul(page_elems)
            .and_then(|e| e.checked_mul(cfg.dtype().size()))
            .ok_or_else(|| ForgeError::Scheduler("kv cache size overflow".into()))?;
        let mut k = Vec::with_capacity(cfg.n_layers);
        let mut v = Vec::with_capacity(cfg.n_layers);
        // The per-layer slabs are static for the model lifetime — paging is a
        // logical overlay managed here — so they come from the bump (Weights)
        // pool; the HAL KvCache slab arena serves fixed-size page churn only.
        for _ in 0..cfg.n_layers {
            k.push(device.alloc(bytes, MemKind::Device, Pool::Weights)?);
            v.push(device.alloc(bytes, MemKind::Device, Pool::Weights)?);
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
                k_packed.push(device.alloc(packed_bytes, MemKind::Device, Pool::Weights)?);
                v_packed.push(device.alloc(packed_bytes, MemKind::Device, Pool::Weights)?);
                k_scale.push(device.alloc(scale_bytes, MemKind::Device, Pool::Weights)?);
                v_scale.push(device.alloc(scale_bytes, MemKind::Device, Pool::Weights)?);
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

    pub fn new_seq(&self) -> SeqKv {
        SeqKv {
            pages: Vec::new(),
            len: 0,
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

    pub fn release(&mut self, seq: &mut SeqKv) {
        self.free_pages.append(&mut seq.pages);
        seq.len = 0;
    }

    pub fn free_page_count(&self) -> usize {
        self.free_pages.len()
    }
}
