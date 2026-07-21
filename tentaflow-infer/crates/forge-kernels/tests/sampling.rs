// ===== File: sampling.rs — GPU sampling launchers vs CPU references =====
// Covers the launcher-computed grid geometry on a realistic vocab: greedy
// argmax bit-match (ties -> lowest index), repetition-penalty math, top-k
// membership of every draw across 10k fixed seeds, and per-(seed, step)
// determinism. Skips cleanly when no CUDA device is present.

use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;

const VOCAB: usize = 151_936;

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 4 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping CUDA sampling tests: {e}");
            None
        }
    }
}

fn logit(i: usize) -> f32 {
    (((i.wrapping_mul(2_654_435_761)) % 4093) as f32 - 2046.0) * 0.006
}

fn upload_f32(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let buf = dev
        .alloc(vals.len() * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytemuck::cast_slice(vals), &buf, 0).unwrap();
    buf
}

struct SampleBufs {
    out: DevBuffer,
    vals: DevBuffer,
    idx: DevBuffer,
}

fn sample_bufs(dev: &dyn Device) -> SampleBufs {
    SampleBufs {
        out: dev.alloc(8, MemKind::Device, Pool::Weights).unwrap(),
        vals: dev
            .alloc(forge_kernels::SAMPLE_SCRATCH_PAIRS * 4, MemKind::Device, Pool::Weights)
            .unwrap(),
        idx: dev
            .alloc(forge_kernels::SAMPLE_SCRATCH_PAIRS * 4, MemKind::Device, Pool::Weights)
            .unwrap(),
    }
}

fn read_id(dev: &dyn Device, out: &DevBuffer) -> i32 {
    let mut bytes = [0u8; 4];
    dev.read(out, 0, &mut bytes).unwrap();
    i32::from_le_bytes(bytes)
}

fn cpu_argmax(logits: &[f32]) -> usize {
    let mut best = 0usize;
    for (i, &v) in logits.iter().enumerate() {
        if v > logits[best] {
            best = i;
        }
    }
    best
}

#[test]
fn argmax_matches_cpu_including_ties() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());

    // Plain pseudo-random logits.
    let mut host: Vec<f32> = (0..VOCAB).map(logit).collect();
    let logits = upload_f32(dev.as_ref(), &host);
    kernels
        .sample_argmax_f32(&b.out, &b.vals, &b.idx, &logits, VOCAB, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out) as usize, cpu_argmax(&host));

    // Planted tie above everything: the LOWEST index must win.
    host[777] = 60.0;
    host[143_210] = 60.0;
    let logits = upload_f32(dev.as_ref(), &host);
    kernels
        .sample_argmax_f32(&b.out, &b.vals, &b.idx, &logits, VOCAB, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out), 777);
}

#[test]
fn penalize_then_argmax_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());

    let mut host: Vec<f32> = (0..VOCAB).map(logit).collect();
    let logits = upload_f32(dev.as_ref(), &host);
    // Penalize the CPU argmax hard enough to dethrone it.
    let top = cpu_argmax(&host);
    let pen_ids: Vec<i32> = vec![top as i32, 5, 99_999];
    let ids = dev
        .alloc(pen_ids.len() * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytemuck::cast_slice(&pen_ids), &ids, 0).unwrap();
    let penalty = 3.0f32;
    kernels
        .sample_penalize_f32(&logits, &ids, pen_ids.len(), penalty, &stream)
        .unwrap();
    kernels
        .sample_argmax_f32(&b.out, &b.vals, &b.idx, &logits, VOCAB, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    for &t in &pen_ids {
        let l = host[t as usize];
        host[t as usize] = if l > 0.0 { l / penalty } else { l * penalty };
    }
    let want = cpu_argmax(&host);
    assert_ne!(want, top, "penalty must change the winner for this test");
    assert_eq!(read_id(dev.as_ref(), &b.out) as usize, want);
}

#[test]
fn fused_histogram_penalties_match_cpu_for_argmax_and_topk() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());
    let original: Vec<f32> = (0..VOCAB).map(logit).collect();
    let top = cpu_argmax(&original);
    let ids = [top as i32, 5i32];
    let counts = [3i32, 1i32];
    let ids_buf = dev.alloc(8, MemKind::Device, Pool::Weights).unwrap();
    let counts_buf = dev.alloc(8, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&ids), &ids_buf, 0).unwrap();
    dev.write(bytemuck::cast_slice(&counts), &counts_buf, 0)
        .unwrap();
    let mut expected = original.clone();
    for (&token, &count) in ids.iter().zip(&counts) {
        let logit = &mut expected[token as usize];
        *logit = if *logit > 0.0 { *logit / 2.0 } else { *logit * 2.0 };
        *logit -= 0.25 + 0.5 * count as f32;
    }
    let want = cpu_argmax(&expected);

    let logits = upload_f32(dev.as_ref(), &original);
    kernels
        .sample_penalized_argmax_f32(
            &b.out,
            &logits,
            &ids_buf,
            &counts_buf,
            2,
            VOCAB,
            2.0,
            0.5,
            0.25,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out) as usize, want);

    let logits = upload_f32(dev.as_ref(), &original);
    kernels
        .sample_penalize_histogram_f32(
            &logits,
            &ids_buf,
            &counts_buf,
            2,
            VOCAB,
            2.0,
            0.5,
            0.25,
            &stream,
        )
        .unwrap();
    kernels
        .sample_topk_f32(
            &b.out, &b.vals, &b.idx, &logits, VOCAB, 40, 1.0, 1e-9, 0.0, 17, 3,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out) as usize, want);
}

#[test]
fn batched_histogram_penalties_match_cpu_and_ignore_oob() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let vocab = 8usize;
    let original = [1.0f32, 8.0, -2.0, 4.0, 0.0, 0.0, 0.0, 0.0,
                    3.0, 2.0, 7.0, -1.0, 0.0, 0.0, 0.0, 0.0];
    let logits = upload_f32(dev.as_ref(), &original);
    let ids = [1i32, 2, 99, 2];
    let counts = [2i32, 1, 1, 3];
    let offsets = [0i32, 3, 4];
    let repetitions = [2.0f32, 1.0];
    let frequencies = [0.5f32, 0.25];
    let presences = [0.25f32, -0.5];
    let ids_buf = dev.alloc(ids.len() * 4, MemKind::Device, Pool::Weights).unwrap();
    let counts_buf = dev.alloc(counts.len() * 4, MemKind::Device, Pool::Weights).unwrap();
    let offsets_buf = dev.alloc(offsets.len() * 4, MemKind::Device, Pool::Weights).unwrap();
    let repetitions_buf = upload_f32(dev.as_ref(), &repetitions);
    let frequencies_buf = upload_f32(dev.as_ref(), &frequencies);
    let presences_buf = upload_f32(dev.as_ref(), &presences);
    dev.write(bytemuck::cast_slice(&ids), &ids_buf, 0).unwrap();
    dev.write(bytemuck::cast_slice(&counts), &counts_buf, 0).unwrap();
    dev.write(bytemuck::cast_slice(&offsets), &offsets_buf, 0).unwrap();

    kernels
        .sample_batched_penalize_f32(
            &logits,
            vocab,
            &ids_buf,
            &counts_buf,
            &offsets_buf,
            &repetitions_buf,
            &frequencies_buf,
            &presences_buf,
            2,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let mut bytes = vec![0u8; original.len() * 4];
    dev.read(&logits, 0, &mut bytes).unwrap();
    let actual: &[f32] = bytemuck::cast_slice(&bytes);
    let mut expected = original;
    expected[1] = 8.0 / 2.0 - 0.25 - 0.5 * 2.0;
    expected[2] = -2.0 * 2.0 - 0.25 - 0.5;
    expected[vocab + 2] = 7.0 - (-0.5) - 0.25 * 3.0;
    assert_eq!(actual, expected);
}

#[test]
fn parallel_topk_handles_only_nan_and_negative_infinity() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());
    let mut host = vec![f32::NEG_INFINITY; VOCAB];
    host[1] = f32::NAN;
    host[99_999] = f32::NAN;
    let logits = upload_f32(dev.as_ref(), &host);
    let ids = [5i32];
    let counts = [1i32];
    let ids_buf = dev.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    let counts_buf = dev.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&ids), &ids_buf, 0).unwrap();
    dev.write(bytemuck::cast_slice(&counts), &counts_buf, 0)
        .unwrap();

    kernels
        .sample_penalize_histogram_f32(
            &logits,
            &ids_buf,
            &counts_buf,
            1,
            VOCAB,
            1.0,
            0.0,
            0.0,
            &stream,
        )
        .unwrap();
    kernels
        .sample_topk_f32(
            &b.out, &b.vals, &b.idx, &logits, VOCAB, 64, 1.0, 1.0, 0.0, 17, 3,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out), 0);
}

#[test]
fn parallel_topk_matches_seeded_golden() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());
    let logits = upload_f32(dev.as_ref(), &[1.0, 2.75, -4.75, 4.0, 0.0, 0.0, 0.0, 0.0]);

    kernels
        .sample_topk_f32(
            &b.out, &b.vals, &b.idx, &logits, 8, 2, 1.0, 1.0, 0.0, 0, 3, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out), 1);
}

#[test]
#[cfg(debug_assertions)]
fn fused_histogram_rejects_duplicate_ids() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());
    let logits = upload_f32(dev.as_ref(), &[0.0; 8]);
    let ids = [3i32, 3i32];
    let counts = [1i32, 2i32];
    let ids_buf = dev.alloc(8, MemKind::Device, Pool::Weights).unwrap();
    let counts_buf = dev.alloc(8, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&ids), &ids_buf, 0).unwrap();
    dev.write(bytemuck::cast_slice(&counts), &counts_buf, 0)
        .unwrap();

    let error = kernels
        .sample_penalized_argmax_f32(
            &b.out,
            &logits,
            &ids_buf,
            &counts_buf,
            2,
            8,
            1.0,
            0.0,
            0.0,
            &stream,
        )
        .unwrap_err();
    assert!(error.to_string().contains("unikalnych IDs"));
}

#[test]
fn topk_draws_stay_in_topk_set_and_replay_deterministically() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let b = sample_bufs(dev.as_ref());

    let host: Vec<f32> = (0..VOCAB).map(logit).collect();
    let logits = upload_f32(dev.as_ref(), &host);

    // CPU allowed set: values >= the k-th largest (tie-inclusive superset).
    let k = 40usize;
    let mut sorted = host.clone();
    sorted.sort_unstable_by(|a, b| b.total_cmp(a));
    let threshold = sorted[k - 1];

    let inv_t = 1.0 / 0.7f32;
    let mut first = None;
    let mut distinct = std::collections::HashSet::new();
    for draw in 0..10_000u64 {
        let seed = 0xC0FF_EE00 + (draw % 4);
        kernels
            .sample_topk_f32(
                &b.out, &b.vals, &b.idx, &logits, VOCAB, k, inv_t, 0.95, 0.0, seed, draw,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        let id = read_id(dev.as_ref(), &b.out);
        assert!(
            (0..VOCAB as i32).contains(&id) && host[id as usize] >= threshold,
            "draw {draw} produced {id} outside the top-{k} set"
        );
        first.get_or_insert(id);
        distinct.insert(id);
    }
    assert!(distinct.len() > 1, "10k draws never varied — not sampling");

    // Same (seed, step) must reproduce the same token.
    kernels
        .sample_topk_f32(
            &b.out, &b.vals, &b.idx, &logits, VOCAB, k, inv_t, 0.95, 0.0, 0xC0FF_EE00, 0,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(Some(read_id(dev.as_ref(), &b.out)), first);

    // top_p -> 0 degenerates to the argmax of the top-k set.
    kernels
        .sample_topk_f32(
            &b.out, &b.vals, &b.idx, &logits, VOCAB, k, inv_t, 1e-9, 0.0, 0xDEAD, 7, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    assert_eq!(read_id(dev.as_ref(), &b.out) as usize, cpu_argmax(&host));
}
