// =============================================================================
// File: benches/recording_perf.rs — M1.W8 Chunk D performance acceptance
// =============================================================================
//
// Criterion micro-benches that drive the M1.W8 hot paths against the
// `tentavision-plan.md` §17.8 targets:
//
//   * snapshot_save (320x240 + 1280x720 PNG encode + atomic write) < 50 ms p99
//   * recording_url_issue (HMAC sign)                              < 1 ms p99
//   * recording_url_verify (HMAC verify)                           < 1 ms p99
//   * frame_url_issue                                              < 1 ms p99
//   * frame_url_verify                                             < 1 ms p99
//
// Run: `cargo bench --features camera,dashboard-api --bench recording_perf
//      -- --quick --noplot`

#![cfg(feature = "camera")]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use tentaflow_core::services::recording::save_snapshot_rgb24;
use tentaflow_core::services::signed_urls::{SignedUrlIssuer, UrlScope};

fn rgb_buf(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push((x % 256) as u8);
            v.push((y % 256) as u8);
            v.push(((x + y) % 256) as u8);
        }
    }
    v
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn bench_snapshot_save(c: &mut Criterion) {
    // Pin HOME to a per-bench temp dir so the snapshot writes don't pollute
    // the developer's real `~/.tentaflow/recordings/`.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", tmp.path());
    let runtime = rt();

    let mut group = c.benchmark_group("snapshot_save");
    for (w, h) in [(320u32, 240u32), (1280u32, 720u32)] {
        let rgb = rgb_buf(w, h);
        group.bench_with_input(
            BenchmarkId::new("rgb24_png", format!("{w}x{h}")),
            &rgb,
            |b, payload| {
                b.iter(|| {
                    runtime.block_on(async {
                        let saved = save_snapshot_rgb24(
                            "bench_cam",
                            black_box(payload),
                            black_box(w),
                            black_box(h),
                        )
                        .await
                        .expect("save");
                        // Reclaim disk so the bench dir does not balloon over
                        // long runs. Failure is harmless (tempdir cleanup on drop).
                        let _ = tokio::fs::remove_file(&saved.file_path).await;
                    })
                });
            },
        );
    }
    group.finish();
}

fn bench_recording_url_issue(c: &mut Criterion) {
    let issuer = SignedUrlIssuer::new_for_tests(UrlScope::Recording, [0x42u8; 32]);
    let mut counter = 0u64;
    c.bench_function("recording_url_issue", |b| {
        b.iter(|| {
            counter = counter.wrapping_add(1);
            let r = format!("snap_{:032x}-0000-0000-0000-000000000000", counter & 0xffff_ffff);
            let _ = black_box(issuer.issue(black_box(r), black_box(120)).expect("issue"));
        });
    });
}

fn bench_recording_url_verify(c: &mut Criterion) {
    let issuer = SignedUrlIssuer::new_for_tests(UrlScope::Recording, [0x42u8; 32]);
    let url = issuer
        .issue("snap_aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(), 600)
        .expect("issue");
    c.bench_function("recording_url_verify", |b| {
        b.iter(|| {
            issuer
                .verify(
                    black_box(&url.ref_id),
                    black_box(url.expiry_unix_ms),
                    black_box(&url.token_b64),
                )
                .expect("verify");
        });
    });
}

fn bench_frame_url_issue(c: &mut Criterion) {
    let issuer = SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [0x55u8; 32]);
    let mut counter = 0u64;
    c.bench_function("frame_url_issue", |b| {
        b.iter(|| {
            counter = counter.wrapping_add(1);
            let r = format!(
                "frame_{:08x}-0000-0000-0000-000000000000",
                counter & 0xffff_ffff
            );
            let _ = black_box(issuer.issue(black_box(r), black_box(120)).expect("issue"));
        });
    });
}

fn bench_frame_url_verify(c: &mut Criterion) {
    let issuer = SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [0x55u8; 32]);
    let url = issuer
        .issue("frame_bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(), 300)
        .expect("issue");
    c.bench_function("frame_url_verify", |b| {
        b.iter(|| {
            issuer
                .verify(
                    black_box(&url.ref_id),
                    black_box(url.expiry_unix_ms),
                    black_box(&url.token_b64),
                )
                .expect("verify");
        });
    });
}

criterion_group!(
    benches,
    bench_snapshot_save,
    bench_recording_url_issue,
    bench_recording_url_verify,
    bench_frame_url_issue,
    bench_frame_url_verify,
);
criterion_main!(benches);
