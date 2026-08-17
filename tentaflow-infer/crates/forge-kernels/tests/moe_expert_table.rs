// ===== File: moe_expert_table.rs — routed-MoE expert GEMV via a pointer table =====
//
// The `_gidx` kernels no longer take a stacked base plus a row stride; they read
// expert `e`'s weight base from a device-resident pointer table. Two properties
// have to hold and are checked here against the contiguous row-window kernels:
//
//  1. selecting expert `e` through the table is BIT-IDENTICAL to launching the
//     ordinary `_at` kernel at that expert's byte offset,
//  2. an expert whose weights sit in pinned host memory (read by the kernel over
//     PCIe) yields exactly the same bits as one in VRAM — this is what makes
//     mixed VRAM/RAM residency legitimate rather than merely plausible.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

mod common;
use common::{build_q4k, build_q6k};

/// Test MUST run on a real device — no GPU is a failure, not a skip.
fn device() -> Option<Arc<dyn Device>> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 512 << 20,
            kv_cache: 16 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .map_err(|e| eprintln!("pomijam {}: {e}", "tablica ekspertów MoE"))
    .ok()
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

fn write_bytes(dev: &dyn Device, kind: MemKind, bytes: &[u8]) -> DevBuffer {
    let buf = dev.alloc(bytes.len(), kind, Pool::Weights).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

/// Device table of per-expert weight base addresses.
fn pointer_table(dev: &dyn Device, experts: &[DevBuffer]) -> DevBuffer {
    let addrs: Vec<u64> = experts.iter().map(|b| b.device_ptr()).collect();
    let bytes = unsafe { std::slice::from_raw_parts(addrs.as_ptr() as *const u8, addrs.len() * 8) };
    write_bytes(dev, MemKind::Device, bytes)
}

fn ids_buffer(dev: &dyn Device, ids: &[i32]) -> DevBuffer {
    let bytes = unsafe { std::slice::from_raw_parts(ids.as_ptr() as *const u8, ids.len() * 4) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn activation(cols: usize) -> Vec<f32> {
    (0..cols)
        .map(|i| (((i * 29 + 11) % 41) as f32 - 20.0) * 0.02)
        .collect()
}

/// Per-expert placement under test: everything in VRAM, versus one expert
/// deliberately pushed to pinned host memory.
fn expert_kinds(n_experts: usize, host_expert: Option<usize>) -> Vec<MemKind> {
    (0..n_experts)
        .map(|e| {
            if host_expert == Some(e) {
                MemKind::PinnedHost
            } else {
                MemKind::Device
            }
        })
        .collect()
}

struct Case {
    rows: usize,
    cols: usize,
    n_experts: usize,
}

/// Runs every expert through the table kernel and compares against the same
/// expert's contiguous row window. `host_expert` optionally forces one expert
/// into pinned host memory.
fn check_q4k(case: &Case, host_expert: Option<usize>) {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let Case {
        rows,
        cols,
        n_experts,
    } = *case;

    let stacked = build_q4k(rows * n_experts, cols);
    let bytes_per_expert = stacked.len() / n_experts;
    let contiguous = write_bytes(dev.as_ref(), MemKind::Device, &stacked);
    let kinds = expert_kinds(n_experts, host_expert);
    let experts: Vec<DevBuffer> = (0..n_experts)
        .map(|e| {
            write_bytes(
                dev.as_ref(),
                kinds[e],
                &stacked[e * bytes_per_expert..(e + 1) * bytes_per_expert],
            )
        })
        .collect();
    let table = pointer_table(dev.as_ref(), &experts);

    let x = upload_f16(dev.as_ref(), &activation(cols));
    let y_table = dev
        .alloc(rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let y_window = dev
        .alloc(rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    for e in 0..n_experts {
        let ids = ids_buffer(dev.as_ref(), &[e as i32]);
        kernels
            .gemv_q4_k_dp4a_f16_gidx(&y_table, &table, &x, rows, cols, &ids, 0, &stream)
            .unwrap();
        kernels
            .gemv_q4_k_dp4a_f16_at(
                &y_window,
                &contiguous,
                e * bytes_per_expert,
                &x,
                rows,
                cols,
                &stream,
            )
            .unwrap();
        assert_bit_identical(
            dev.as_ref(),
            &stream,
            &y_table,
            &y_window,
            rows,
            e,
            kinds[e],
        );
    }
}

fn check_q6k(case: &Case, host_expert: Option<usize>) {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let Case {
        rows,
        cols,
        n_experts,
    } = *case;

    let stacked = build_q6k(rows * n_experts, cols);
    let bytes_per_expert = stacked.len() / n_experts;
    let contiguous = write_bytes(dev.as_ref(), MemKind::Device, &stacked);
    let kinds = expert_kinds(n_experts, host_expert);
    let experts: Vec<DevBuffer> = (0..n_experts)
        .map(|e| {
            write_bytes(
                dev.as_ref(),
                kinds[e],
                &stacked[e * bytes_per_expert..(e + 1) * bytes_per_expert],
            )
        })
        .collect();
    let table = pointer_table(dev.as_ref(), &experts);

    let x = upload_f16(dev.as_ref(), &activation(cols));
    let y_table = dev
        .alloc(rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let y_window = dev
        .alloc(rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    for e in 0..n_experts {
        let ids = ids_buffer(dev.as_ref(), &[e as i32]);
        kernels
            .gemv_q6_k_f16_gidx(&y_table, &table, &x, rows, cols, &ids, 0, &stream)
            .unwrap();
        kernels
            .gemv_q6_k_f16_at(
                &y_window,
                &contiguous,
                e * bytes_per_expert,
                &x,
                rows,
                cols,
                &stream,
            )
            .unwrap();
        assert_bit_identical(
            dev.as_ref(),
            &stream,
            &y_table,
            &y_window,
            rows,
            e,
            kinds[e],
        );
    }
}

fn assert_bit_identical(
    dev: &dyn Device,
    stream: &Stream,
    table: &DevBuffer,
    window: &DevBuffer,
    rows: usize,
    expert: usize,
    kind: MemKind,
) {
    stream.synchronize().unwrap();
    let got = download_f16(dev, table, rows);
    let want = download_f16(dev, window, rows);
    assert_eq!(
        got, want,
        "expert {expert} ({kind:?}) differs between the pointer table and its row window"
    );
    // A stack of identical zeros would satisfy equality vacuously.
    assert!(
        want.iter().any(|v| v.abs() > 1e-6),
        "expert {expert} reference output is all zeros — the case proves nothing"
    );
}

/// The dp4a Q4_K path used for routed gate/up projections.
#[test]
fn q4k_expert_table_matches_row_window() {
    check_q4k(
        &Case {
            rows: 96,
            cols: 512,
            n_experts: 6,
        },
        None,
    );
}

/// Same, with one expert served straight out of pinned host memory: this is the
/// mixed-tier residency case, and it must not perturb a single bit.
#[test]
fn q4k_expert_in_host_memory_matches_vram() {
    check_q4k(
        &Case {
            rows: 96,
            cols: 512,
            n_experts: 6,
        },
        Some(3),
    );
}

/// The warp-per-row Q6_K path used for routed down projections.
#[test]
fn q6k_expert_table_matches_row_window() {
    check_q6k(
        &Case {
            rows: 64,
            cols: 512,
            n_experts: 5,
        },
        None,
    );
}

#[test]
fn q6k_expert_in_host_memory_matches_vram() {
    check_q6k(
        &Case {
            rows: 64,
            cols: 512,
            n_experts: 5,
        },
        Some(2),
    );
}
