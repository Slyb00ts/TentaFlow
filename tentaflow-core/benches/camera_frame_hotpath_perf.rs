// =============================================================================
// File: benches/camera_frame_hotpath_perf.rs — per-frame ingest hot-path bench
// =============================================================================
//
// Reproduces the per-frame work the GStreamer appsink callback does in
// `services/camera_ingest/rtsp.rs::install_frame_callback` and `fakefile.rs`,
// WITHOUT GStreamer, RTSP, or any network — so we can measure and profile the
// scaling cost toward thousands of cameras @ 25 fps deterministically.
//
// The production callback, per decoded RGB24 frame, does:
//   1. `Arc::from(map.as_slice())`          — one alloc + one memcpy of the frame
//   2. `frame_storage().insert(stored)`     — LRU lock + put (+ metadata clone)
//   3. `streaming_bus().broadcast(...)`     — DashMap shard lock + per-sub send
//
// Groups:
//   * frame_clone_pipeline — the raw alloc+copy+Arc step (1) at several
//     resolutions; this is the dominant per-frame cost.
//   * insert_broadcast     — storage insert + bus broadcast (2+3) with 0/1/4
//     subscribers, the lock-contention surface.
//   * full_callback        — the whole callback body for ONE frame, the unit
//     replicated N_cameras * 25 times per second.
//   * zero_copy_baseline   — the SAME path if the source buffer were adopted by
//     a single Arc with NO payload copy (lower bound the optimization targets).
//
// Resolutions cover the stated production case (1600x1200 RGB24 = 5.76 MB) plus
// a 1080p and a 720p point so the alloc/copy curve is visible.
//
// Run: `cargo bench --bench camera_frame_hotpath_perf`
// Per-camera-second cost = full_callback_time * 25; multiply by camera count.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::streaming::{StreamFilter, StreamingBus};

/// Resolutions exercised, as (label, width, height). RGB24 => w*h*3 bytes.
const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("720p", 1280, 720),       // 2.76 MB
    ("1080p", 1920, 1080),     // 6.22 MB
    ("1600x1200", 1600, 1200), // 5.76 MB — the production camera case
];

/// A pre-decoded source frame standing in for the `gst::BufferMap` slice. In
/// production this is borrowed from the GStreamer buffer; here it is a Vec we
/// only ever read from, so `&src[..]` is the analogue of `map.as_slice()`.
fn source_buffer(width: u32, height: u32) -> Vec<u8> {
    vec![0x7Eu8; (width * height * 3) as usize]
}

fn metadata(width: u32, height: u32, size: usize) -> FrameMetadata {
    FrameMetadata {
        camera_id: "bench-cam".into(),
        width,
        height,
        pixel_format: FramePixelFormat::Rgb24,
        timestamp_unix_ms: 1_715_500_000_000,
        pts: Some(7),
        frame_size_bytes: size,
    }
}

// -----------------------------------------------------------------------------
// 1. frame_clone_pipeline — the to_vec + Arc::from step in isolation.
//    This is the dominant per-frame cost: one alloc + one ~MB memcpy, then a
//    second alloc for the boxed slice the Arc wraps.
// -----------------------------------------------------------------------------

fn bench_frame_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_clone_pipeline");
    for &(label, w, h) in RESOLUTIONS {
        let src = source_buffer(w, h);
        group.throughput(Throughput::Bytes(src.len() as u64));
        group.bench_with_input(BenchmarkId::new("arc_from_slice", label), &src, |b, src| {
            b.iter(|| {
                // Mirror of the production hot path: build the shared frame in
                // ONE copy straight from the GStreamer map slice (one alloc +
                // one memcpy, no intermediate Vec).
                let shared: Arc<[u8]> = Arc::from(src.as_slice());
                black_box(shared);
            });
        });
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 2. zero_copy_baseline — the SAME data made shareable with a single alloc and
//    NO copy out of an owned buffer (Arc<[u8]> built once). Lower bound the
//    zero-copy optimization can approach. We construct the Arc from a moved Vec
//    so there is exactly one allocation and no memcpy of the payload.
// -----------------------------------------------------------------------------

fn bench_zero_copy_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("zero_copy_baseline");
    for &(label, w, h) in RESOLUTIONS {
        let size = (w * h * 3) as usize;
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("single_arc", label), &size, |b, &size| {
            b.iter_batched(
                // Owned buffer handed over per-iter, as a zero-copy source would
                // hand ownership of the decoder output.
                || vec![0x7Eu8; size],
                |owned| {
                    let shared: Arc<[u8]> = Arc::from(owned.into_boxed_slice());
                    black_box(shared);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 3. insert_broadcast — storage insert + bus broadcast (steps 3+4) under
//    0 / 1 / 4 subscribers. The frame bytes are pre-shared so this isolates the
//    LRU lock + DashMap shard lock + per-subscriber try_send.
// -----------------------------------------------------------------------------

fn bench_insert_broadcast(c: &mut Criterion) {
    let (_, w, h) = RESOLUTIONS[2]; // 1600x1200 production case
    let size = (w * h * 3) as usize;
    let shared: Arc<[u8]> = Arc::from(vec![0x7Eu8; size].into_boxed_slice());
    let meta = metadata(w, h, size);

    let mut group = c.benchmark_group("insert_broadcast");
    for subs in [0usize, 1, 4] {
        group.bench_with_input(
            BenchmarkId::new("insert_then_broadcast", subs),
            &subs,
            |b, &subs| {
                let storage = FrameStorage::new(2048);
                let bus = StreamingBus::new();
                // Keep subscriber handles alive so try_send has a live channel.
                let mut keep = Vec::with_capacity(subs);
                for _ in 0..subs {
                    keep.push(bus.subscribe_with_capacity(
                        "bench-cam",
                        StreamFilter::default(),
                        1024,
                    ));
                }
                b.iter(|| {
                    let stored = StoredFrame {
                        metadata: meta.clone(),
                        data: shared.clone(),
                        created_at: std::time::Instant::now(),
                    };
                    let frame_ref = storage.insert(stored);
                    bus.broadcast("bench-cam", frame_ref, meta.clone());
                    // Drain so the bounded channels never backpressure and we
                    // keep measuring the no-drop hot path.
                    for s in keep.iter_mut() {
                        black_box(s.dropped_pending());
                    }
                });
            },
        );
    }
    group.finish();
}

// -----------------------------------------------------------------------------
// 4. full_callback — the ENTIRE appsink callback body for one frame, including
//    the to_vec + Arc::from, the LRU insert and the broadcast. This is the unit
//    replicated `cameras * 25` times per second. Reported per resolution with
//    one subscriber (the live-view tile) attached.
// -----------------------------------------------------------------------------

fn bench_full_callback(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_callback");
    for &(label, w, h) in RESOLUTIONS {
        let src = source_buffer(w, h);
        let size = src.len();
        let meta = metadata(w, h, size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("one_subscriber", label), &src, |b, src| {
            let storage = FrameStorage::new(2048);
            let bus = StreamingBus::new();
            let _sub = bus.subscribe_with_capacity("bench-cam", StreamFilter::default(), 1024);
            b.iter(|| {
                // ---- production callback body, verbatim shape ----
                let shared: Arc<[u8]> = Arc::from(src.as_slice());
                let frame_size = shared.len();
                let mut m = meta.clone();
                m.frame_size_bytes = frame_size;
                let stored = StoredFrame {
                    metadata: m.clone(),
                    data: shared,
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = storage.insert(stored);
                bus.broadcast("bench-cam", frame_ref, m);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_frame_clone,
    bench_zero_copy_baseline,
    bench_insert_broadcast,
    bench_full_callback,
);
criterion_main!(benches);
