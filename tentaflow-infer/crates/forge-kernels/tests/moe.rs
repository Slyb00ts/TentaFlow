// ===== File: moe.rs — MoE router kernel vs CPU reference =====
// moe_router_f16 must reproduce the HF routing math (softmax over all experts,
// then top-k, optional renormalization) with the SAME selection and weights a
// sequential CPU pass produces. Skips cleanly with no CUDA device.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

/// Test musi biec na realnym urzadzeniu dowolnego backendu — wczesniejsze
/// wiazanie z CUDA cicho pomijalo caly plik na AMD.
fn device() -> Option<Arc<dyn Device>> {
    match forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 16 << 20,
            activations: 32 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("brak urzadzenia GPU dla testow MoE: {e}");
            None
        }
    }
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn upload_f32(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let bytes = unsafe { std::slice::from_raw_parts(vals.as_ptr() as *const u8, vals.len() * 4) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn cpu_topk(logits: &[f32], top_k: usize, norm_topk: bool) -> (Vec<i32>, Vec<f32>) {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let denom: f32 = logits.iter().map(|&value| (value - max).exp()).sum();
    let mut probabilities: Vec<f32> = logits
        .iter()
        .map(|&value| (value - max).exp() / denom)
        .collect();
    let mut ids = Vec::with_capacity(top_k);
    let mut weights = Vec::with_capacity(top_k);
    for _ in 0..top_k {
        let (index, weight) = probabilities
            .iter()
            .enumerate()
            .max_by(|(left_index, left), (right_index, right)| {
                left.partial_cmp(right)
                    .unwrap()
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, &weight)| (index, weight))
            .unwrap();
        ids.push(index as i32);
        weights.push(weight);
        probabilities[index] = f32::NEG_INFINITY;
    }
    if norm_topk {
        let sum: f32 = weights.iter().sum();
        for weight in &mut weights {
            *weight /= sum;
        }
    }
    (ids, weights)
}

/// CPU reference: f16-round inputs, f32 dot, softmax over all experts, top-k
/// (ties to the lowest index), optional renormalization.
fn cpu_router(
    x: &[f32],
    w: &[f32],
    hidden: usize,
    n_expert: usize,
    top_k: usize,
    norm_topk: bool,
) -> (Vec<i32>, Vec<f32>) {
    let r = |v: f32| f16::from_f32(v).to_f32();
    let mut logits = vec![0f32; n_expert];
    for (e, logit) in logits.iter_mut().enumerate() {
        let mut acc = 0f32;
        for j in 0..hidden {
            acc += r(x[j]) * r(w[e * hidden + j]);
        }
        *logit = acc;
    }
    let mx = logits.iter().cloned().fold(f32::MIN, f32::max);
    let denom: f32 = logits.iter().map(|&l| (l - mx).exp()).sum();
    let mut probs: Vec<f32> = logits.iter().map(|&l| (l - mx).exp() / denom).collect();
    let mut ids = Vec::with_capacity(top_k);
    let mut weights = Vec::with_capacity(top_k);
    let mut wsum = 0f32;
    for _ in 0..top_k {
        let mut best_i = 0usize;
        let mut best_v = f32::MIN;
        for (n, &p) in probs.iter().enumerate() {
            if p > best_v {
                best_v = p;
                best_i = n;
            }
        }
        ids.push(best_i as i32);
        weights.push(best_v);
        wsum += best_v;
        probs[best_i] = f32::MIN;
    }
    if norm_topk && wsum > 0.0 {
        for wv in &mut weights {
            *wv /= wsum;
        }
    }
    (ids, weights)
}

fn run_case(n_tokens: usize, hidden: usize, n_expert: usize, top_k: usize, norm_topk: bool) {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // Distinct, non-degenerate token/expert patterns so no two experts tie.
    let x: Vec<f32> = (0..n_tokens * hidden)
        .map(|i| (((i * 31 + 7) % 23) as f32 - 11.0) * 0.05)
        .collect();
    let w: Vec<f32> = (0..n_expert * hidden)
        .map(|i| (((i * 17 + 3) % 29) as f32 - 14.0) * 0.03 + (i % n_expert.max(1)) as f32 * 0.001)
        .collect();

    let x_dev = upload_f16(dev.as_ref(), &x);
    let w_dev = upload_f16(dev.as_ref(), &w);
    let ids_dev = dev
        .alloc(n_tokens * top_k * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    // Kernel routera zlicza wybory ekspertow; licznik musi istniec, choc test
    // sprawdza tylko wybor i wagi.
    let counts_dev = dev
        .alloc(n_expert * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&vec![0u8; n_expert * 4], &counts_dev, 0).unwrap();
    let wt_dev = dev
        .alloc(n_tokens * top_k * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .moe_router_f16(
            &ids_dev,
            &wt_dev,
            &x_dev,
            &w_dev,
            &counts_dev,
            n_tokens,
            hidden,
            n_expert,
            top_k,
            norm_topk,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let mut ids_bytes = vec![0u8; n_tokens * top_k * 4];
    dev.read(&ids_dev, 0, &mut ids_bytes).unwrap();
    let gpu_ids: Vec<i32> = ids_bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut wt_bytes = vec![0u8; n_tokens * top_k * 4];
    dev.read(&wt_dev, 0, &mut wt_bytes).unwrap();
    let gpu_w: Vec<f32> = wt_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    for t in 0..n_tokens {
        let (cpu_ids, cpu_w) = cpu_router(
            &x[t * hidden..(t + 1) * hidden],
            &w,
            hidden,
            n_expert,
            top_k,
            norm_topk,
        );
        let g_ids = &gpu_ids[t * top_k..(t + 1) * top_k];
        let g_w = &gpu_w[t * top_k..(t + 1) * top_k];
        assert_eq!(g_ids, cpu_ids.as_slice(), "token {t} expert ids mismatch");
        for j in 0..top_k {
            assert!(
                (g_w[j] - cpu_w[j]).abs() < 1e-3,
                "token {t} weight {j}: gpu {} vs cpu {}",
                g_w[j],
                cpu_w[j]
            );
        }
    }
}

#[test]
fn router_olmoe_shape_no_renorm() {
    // OLMoE: 64 experts, top-8, hidden 2048, no renormalization.
    run_case(4, 2048, 64, 8, false);
}

#[test]
fn router_qwen3moe_shape_renorm() {
    // Qwen3-MoE: 128 experts, top-8, renormalized routing weights.
    run_case(3, 2048, 128, 8, true);
}

#[test]
fn router_single_token() {
    run_case(1, 1024, 32, 4, true);
}

#[test]
fn topk_qwen3moe_logits_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let n_tokens = 3;
    let n_expert = 256;
    let top_k = 8;
    let logits: Vec<f32> = (0..n_tokens * n_expert)
        .map(|index| ((index * 37 % 509) as f32 - 254.0) * 0.03125)
        .collect();
    let logits_dev = upload_f32(dev.as_ref(), &logits);
    let ids_dev = dev
        .alloc(n_tokens * top_k * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let weights_dev = dev
        .alloc(n_tokens * top_k * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let counts_dev = dev
        .alloc(n_expert * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&vec![0u8; n_expert * 4], &counts_dev, 0).unwrap();

    kernels
        .moe_topk_f32(
            &ids_dev,
            &weights_dev,
            &logits_dev,
            &counts_dev,
            n_tokens,
            n_expert,
            top_k,
            true,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let mut ids_bytes = vec![0u8; n_tokens * top_k * 4];
    dev.read(&ids_dev, 0, &mut ids_bytes).unwrap();
    let ids: Vec<i32> = ids_bytes
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let mut weights_bytes = vec![0u8; n_tokens * top_k * 4];
    dev.read(&weights_dev, 0, &mut weights_bytes).unwrap();
    let weights: Vec<f32> = weights_bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    for token in 0..n_tokens {
        let (expected_ids, expected_weights) = cpu_topk(
            &logits[token * n_expert..(token + 1) * n_expert],
            top_k,
            true,
        );
        assert_eq!(&ids[token * top_k..(token + 1) * top_k], expected_ids);
        for (actual, expected) in weights[token * top_k..(token + 1) * top_k]
            .iter()
            .zip(expected_weights)
        {
            assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
        }
    }
}
