// ===== File: kv.rs — paged KV cache (per-layer K/V page pools + page tables) =====
// Layout per layer: [n_pages, n_kv_heads, page_size, head_dim] f16, matching
// attn_decode_f16. v0 allocates one contiguous slab per layer and hands out
// logical pages; the HAL KvCache pool arena underneath keeps frees cheap.

use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{ForgeError, MemKind, Result};

pub struct KvConfig {
    pub n_layers: usize,
    pub n_kv_heads: usize,
    pub head_dim: usize,
    pub page_size: usize,
    pub n_pages: usize,
    pub max_pages_per_seq: usize,
}

pub struct KvCache {
    pub cfg: KvConfig,
    /// K/V slabs, one pair per layer.
    pub k: Vec<DevBuffer>,
    pub v: Vec<DevBuffer>,
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
        let bytes = cfg
            .n_pages
            .checked_mul(page_elems)
            .and_then(|e| e.checked_mul(2))
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
        // Stack of free physical page ids shared across layers: a logical page
        // maps to the same physical index in every layer, halving bookkeeping.
        let free_pages = (0..cfg.n_pages as i32).rev().collect();
        Ok(KvCache {
            cfg,
            k,
            v,
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

    /// Byte offset of (page, slot) for one head-row write.
    pub fn token_offset(&self, page: i32, slot: usize, kv_head: usize) -> usize {
        let page_stride = self.cfg.n_kv_heads * self.cfg.page_size * self.cfg.head_dim;
        ((page as usize) * page_stride
            + kv_head * self.cfg.page_size * self.cfg.head_dim
            + slot * self.cfg.head_dim)
            * 2
    }

    pub fn free_page_count(&self) -> usize {
        self.free_pages.len()
    }
}
