// =============================================================================
// File: benches/camera_frame_allocs.rs — per-frame allocation counter (dhat)
// =============================================================================
//
// Not a criterion bench — a `harness=false` binary that runs the per-frame
// ingest hot path under the dhat heap profiler and prints EXACT allocation
// counts and bytes per frame. This answers "how many allocations does each
// decoded frame cost, times N cameras * 25 fps" with measured numbers instead
// of estimates, and quantifies the zero-copy win.
//
// It models the same callback body as `camera_frame_hotpath_perf.rs` but counts
// allocations rather than timing them. Two scenarios per resolution:
//   * current   — single Arc::from(slice) copy + storage insert + broadcast
//   * zero_copy — single Arc<[u8]> (no payload copy) + insert + broadcast
//
// Run: `cargo bench --bench camera_frame_allocs`
// (Criterion's --bench flag still routes here because it is a plain main()).
// It also accepts `FRAMES=<n>` to scale the iteration count (default 1000).

use std::sync::Arc;

use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::streaming::{StreamFilter, StreamingBus};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1600x1200", 1600, 1200),
];

fn metadata(w: u32, h: u32, size: usize) -> FrameMetadata {
    FrameMetadata {
        camera_id: "alloc-cam".into(),
        width: w,
        height: h,
        pixel_format: FramePixelFormat::Rgb24,
        timestamp_unix_ms: 1_715_500_000_000,
        pts: Some(7),
        frame_size_bytes: size,
    }
}

/// Run one scenario `frames` times and return (total_blocks, total_bytes)
/// charged during the loop, isolated via a dhat profiler scope.
fn measure<F: FnMut()>(label: &str, frames: usize, mut body: F) {
    let profiler = dhat::Profiler::builder().testing().build();
    let before = dhat::HeapStats::get();
    for _ in 0..frames {
        body();
    }
    let after = dhat::HeapStats::get();
    drop(profiler);

    let blocks = after.total_blocks - before.total_blocks;
    let bytes = after.total_bytes - before.total_bytes;
    println!(
        "{label:<28} frames={frames:>6}  allocs={blocks:>9}  bytes={bytes:>14}  \
         allocs/frame={:>4}  bytes/frame={:>10}",
        blocks / frames as u64,
        bytes / frames as u64,
    );
}

fn main() {
    let frames: usize = std::env::var("FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);

    println!("per-frame allocation profile ({frames} frames/scenario)\n");

    for &(label, w, h) in RESOLUTIONS {
        let size = (w * h * 3) as usize;
        let src = vec![0x7Eu8; size];
        let meta = metadata(w, h, size);

        // current: single Arc::from(slice) copy + insert + broadcast. This
        // mirrors the production appsink callback, which builds the shared
        // frame in ONE copy straight from the GStreamer map.
        {
            let storage = FrameStorage::new(2048);
            let bus = StreamingBus::new();
            let _sub = bus.subscribe_with_capacity("alloc-cam", StreamFilter::default(), 4096);
            measure(&format!("{label} current"), frames, || {
                let shared: Arc<[u8]> = Arc::from(src.as_slice());
                let stored = StoredFrame {
                    metadata: meta.clone(),
                    data: shared,
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = storage.insert(stored);
                bus.broadcast("alloc-cam", frame_ref, meta.clone());
            });
        }

        // zero_copy: the decoder output is wrapped ONCE in a shared Arc<[u8]>
        // (built outside the loop, as a zero-copy pull would hand us a buffer we
        // adopt without copying); per frame we only clone the Arc (refcount
        // bump, no alloc, no memcpy) plus the unavoidable storage+broadcast
        // bookkeeping. This is the floor the end-to-end zero-copy redesign aims
        // for and isolates the per-frame overhead that is NOT the payload copy.
        {
            let storage = FrameStorage::new(2048);
            let bus = StreamingBus::new();
            let _sub = bus.subscribe_with_capacity("alloc-cam", StreamFilter::default(), 4096);
            let shared: Arc<[u8]> = Arc::from(vec![0x7Eu8; size].into_boxed_slice());
            measure(&format!("{label} zero_copy"), frames, || {
                let stored = StoredFrame {
                    metadata: meta.clone(),
                    data: shared.clone(),
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = storage.insert(stored);
                bus.broadcast("alloc-cam", frame_ref, meta.clone());
            });
        }
        println!();
    }
}
