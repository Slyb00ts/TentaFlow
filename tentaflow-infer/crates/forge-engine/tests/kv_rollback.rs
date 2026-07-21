// ===== File: kv_rollback.rs — paged KV rollback correctness (speculative reject) =====
// Proves `KvCache::rollback` (SPEC §6 speculative verification): after growing a
// sequence and rolling it back to a shorter length, the length, page count and
// free-page pool are exactly what a sequence grown straight to that length would
// have — so discarding rejected draft positions leaves the cache in a clean,
// reusable state. Needs a CUDA device only to allocate the (unused-by-this-test)
// per-layer slabs; skips cleanly without one.

use std::sync::Arc;

use forge_engine::kv::{KvCache, KvConfig, KvQuant};
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;

fn cache(page_size: usize, n_pages: usize) -> Option<KvCache> {
    let device = match CudaDevice::new(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 64 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: no CUDA device: {e}");
            return None;
        }
    };
    let dev: Arc<dyn Device> = device;
    Some(
        KvCache::new(
            dev.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 64,
                page_size,
                n_pages,
                max_pages_per_seq: n_pages,
                quant: KvQuant::F16,
            },
        )
        .expect("alloc kv cache"),
    )
}

/// Page count that a sequence of `len` tokens occupies (matches `grow`).
fn pages_for(len: usize, page_size: usize) -> usize {
    len.div_ceil(page_size)
}

#[test]
fn rollback_frees_tail_pages_and_matches_fresh_growth() {
    let page_size = 4;
    let n_pages = 16;
    let Some(mut kv) = cache(page_size, n_pages) else {
        return;
    };

    let mut seq = kv.new_seq();
    for _ in 0..10 {
        kv.grow(&mut seq).expect("grow");
    }
    assert_eq!(seq.len, 10);
    assert_eq!(seq.pages.len(), pages_for(10, page_size)); // 3 pages
    assert_eq!(kv.free_page_count(), n_pages - 3);

    // Roll back into the middle of the second page: one tail page is freed.
    kv.rollback(&mut seq, 6);
    assert_eq!(seq.len, 6);
    assert_eq!(seq.pages.len(), pages_for(6, page_size)); // 2 pages
    assert_eq!(kv.free_page_count(), n_pages - 2);

    // Exactly to a page boundary: still two pages (positions 0..3 => page 0,
    // 4..7 => page 1, but only 4 tokens means page 1 holds position 4..7's
    // first slot — pages_for(5)=2). Then down to one full page.
    kv.rollback(&mut seq, 4);
    assert_eq!(seq.pages.len(), pages_for(4, page_size)); // 1 page
    assert_eq!(kv.free_page_count(), n_pages - 1);

    // A no-op rollback to the current length changes nothing.
    kv.rollback(&mut seq, 4);
    assert_eq!(seq.len, 4);
    assert_eq!(kv.free_page_count(), n_pages - 1);

    // Full rollback returns every page to the pool.
    kv.rollback(&mut seq, 0);
    assert_eq!(seq.len, 0);
    assert!(seq.pages.is_empty());
    assert_eq!(kv.free_page_count(), n_pages);

    // The post-rollback state must match a sequence grown straight to a length:
    // grow to 6, and it holds the same pages/pool as the rolled-back-to-6 case.
    let mut fresh = kv.new_seq();
    for _ in 0..6 {
        kv.grow(&mut fresh).expect("grow");
    }
    assert_eq!(fresh.pages.len(), pages_for(6, page_size));
    assert_eq!(kv.free_page_count(), n_pages - pages_for(6, page_size));
}
