// ===== File: deltanet.rs — Gated-DeltaNet kernels vs forge-formats CPU oracle =====
// The DeltaNet decode kernels (conv+SiLU, per-head L2 norm, the rank-1 gated
// state scan, output gated-RMSNorm, log-decay / beta gates) must reproduce the
// forge_formats::deltanet reference within f16 rounding. Skips cleanly with no
// CUDA device.

use std::sync::Arc;

use forge_formats::deltanet;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::{DeltaStateLayout, Kernels};
use forge_types::MemKind;
use half::f16;

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 16 << 20,
            activations: 128 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping CUDA DeltaNet tests: {e}");
            None
        }
    }
}

fn r(v: f32) -> f32 {
    f16::from_f32(v).to_f32()
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

fn read_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

fn read_f32(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    read_f32_at(dev, buf, 0, n)
}

fn read_f32_at(dev: &dyn Device, buf: &DevBuffer, byte_offset: usize, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 4];
    dev.read(buf, byte_offset, &mut bytes).unwrap();
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn seed(i: usize) -> f32 {
    ((i as i64 * 37 % 19) - 9) as f32 * 0.13
}

/// Depthwise causal conv (width 4) + SiLU vs deltanet::causal_conv1d_step and
/// the window advance, for every channel.
#[test]
fn conv_silu_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let conv_dim = 8192usize;
    let d_conv = 4usize;
    let win_n = d_conv - 1;
    let win: Vec<f32> = (0..conv_dim * win_n).map(|i| seed(i * 3 + 7)).collect();
    let xnew: Vec<f32> = (0..conv_dim).map(|c| seed(c * 7 + 1)).collect();
    let weight: Vec<f32> = (0..conv_dim * d_conv).map(|i| seed(i + 2) * 0.5).collect();

    let win_dev = upload_f16(dev.as_ref(), &win);
    let x_dev = upload_f16(dev.as_ref(), &xnew);
    let w_dev = upload_f16(dev.as_ref(), &weight);
    let out_dev = dev
        .alloc(conv_dim * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .deltanet_conv_silu_f16(
            &out_dev, &win_dev, &x_dev, &w_dev, conv_dim, d_conv, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let gpu = read_f16(dev.as_ref(), &out_dev, conv_dim);
    let gpu_win = read_f16(dev.as_ref(), &win_dev, conv_dim * win_n);

    let mut max_err = 0f32;
    let mut win_err = 0f32;
    for c in 0..conv_dim {
        let w16: Vec<f32> = (0..d_conv).map(|j| r(weight[c * d_conv + j])).collect();
        let mut window: Vec<f32> = (0..win_n).map(|j| r(win[c * win_n + j])).collect();
        let x = r(xnew[c]);
        let y = deltanet::causal_conv1d_step(&window, x, &w16);
        let refv = deltanet::silu(y);
        max_err = max_err.max((gpu[c] - refv).abs());
        deltanet::causal_conv1d_advance(&mut window, x);
        for j in 0..win_n {
            win_err = win_err.max((gpu_win[c * win_n + j] - r(window[j])).abs());
        }
    }
    assert!(max_err < 2e-3, "conv+silu max_err {max_err}");
    assert!(win_err < 1e-3, "conv window advance err {win_err}");
}

/// Per-head L2 norm vs deltanet::l2_norm.
#[test]
fn l2norm_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let n_heads = 32usize;
    let d_state = 128usize;
    let eps = 1e-6f32;
    let x: Vec<f32> = (0..n_heads * d_state).map(|i| seed(i * 2 + 3)).collect();
    let x_dev = upload_f16(dev.as_ref(), &x);
    let out_dev = dev
        .alloc(n_heads * d_state * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .l2norm_heads_f16(&out_dev, &x_dev, n_heads, d_state, eps, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let gpu = read_f16(dev.as_ref(), &out_dev, n_heads * d_state);

    let mut max_err = 0f32;
    for h in 0..n_heads {
        let mut v: Vec<f32> = (0..d_state).map(|j| r(x[h * d_state + j])).collect();
        deltanet::l2_norm(&mut v, eps);
        for j in 0..d_state {
            max_err = max_err.max((gpu[h * d_state + j] - v[j]).abs());
        }
    }
    assert!(max_err < 2e-3, "l2norm max_err {max_err}");
}

/// One gated-delta step per v-head vs deltanet::gated_delta_step (output AND
/// the in-place state update).
#[test]
fn gated_step_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let nh = 32usize;
    let ds = 128usize;
    let state: Vec<f32> = (0..nh * ds * ds).map(|i| seed(i + 5) * 0.05).collect();
    let q: Vec<f32> = (0..nh * ds).map(|i| seed(i * 5 + 1)).collect();
    let k: Vec<f32> = (0..nh * ds).map(|i| seed(i * 3 + 2)).collect();
    let v: Vec<f32> = (0..nh * ds).map(|i| seed(i * 7 + 4)).collect();
    let g: Vec<f32> = (0..nh).map(|h| -0.1 - (h % 5) as f32 * 0.05).collect();
    let beta: Vec<f32> = (0..nh).map(|h| 0.3 + (h % 4) as f32 * 0.1).collect();

    let state_dev = upload_f32(dev.as_ref(), &state);
    let q_dev = upload_f16(dev.as_ref(), &q);
    let k_dev = upload_f16(dev.as_ref(), &k);
    let v_dev = upload_f16(dev.as_ref(), &v);
    let g_dev = upload_f32(dev.as_ref(), &g);
    let beta_dev = upload_f32(dev.as_ref(), &beta);
    let out_dev = dev
        .alloc(nh * ds * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .deltanet_gated_step_f16(
            &out_dev, &state_dev, &q_dev, &k_dev, &v_dev, &g_dev, &beta_dev, nh, ds, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let gpu_out = read_f16(dev.as_ref(), &out_dev, nh * ds);
    let gpu_state = read_f32(dev.as_ref(), &state_dev, nh * ds * ds);

    let mut out_err = 0f32;
    let mut st_err = 0f32;
    for h in 0..nh {
        // The kernel reads q/k/v through f16; mirror that in the oracle inputs.
        let qh: Vec<f32> = (0..ds).map(|j| r(q[h * ds + j])).collect();
        let kh: Vec<f32> = (0..ds).map(|j| r(k[h * ds + j])).collect();
        let vh: Vec<f32> = (0..ds).map(|j| r(v[h * ds + j])).collect();
        let mut sh = state[h * ds * ds..(h + 1) * ds * ds].to_vec();
        let mut out = vec![0f32; ds];
        deltanet::gated_delta_step(&mut sh, ds, &qh, &kh, &vh, g[h], beta[h], &mut out);
        for j in 0..ds {
            out_err = out_err.max((gpu_out[h * ds + j] - out[j]).abs());
        }
        for idx in 0..ds * ds {
            st_err = st_err.max((gpu_state[h * ds * ds + idx] - sh[idx]).abs());
        }
    }
    assert!(out_err < 3e-3, "delta step out max_err {out_err}");
    assert!(st_err < 1e-4, "delta step state max_err {st_err}");
}

/// Produkcyjny H48/D128 zachowuje bitową semantykę poprzedniego zapisu stanu
/// dla decay=0,5, a każdy element wyjścia zastępuje sygnalizacyjny NaN.
#[test]
fn gated_step_h48_d128_state_is_bit_exact_and_preserves_guards() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    const HEADS: usize = 48;
    const D_STATE: usize = 128;
    const GUARD_BYTES: usize = 256;
    const CANARY: u8 = 0xA5;
    const OUTPUT_SENTINEL: u16 = 0x7E01;
    let vector_elements = HEADS * D_STATE;
    let state_elements = HEADS * D_STATE * D_STATE;
    let state: Vec<f32> = (0..state_elements)
        .map(|i| ((i as i64 * 17 % 31) - 15) as f32 * (1.0 / 4096.0))
        .collect();
    let q: Vec<f32> = (0..vector_elements)
        .map(|i| ((i as i64 * 7 % 17) - 8) as f32 * (1.0 / 64.0))
        .collect();
    let k: Vec<f32> = (0..vector_elements)
        .map(|i| ((i as i64 * 11 % 19) - 9) as f32 * (1.0 / 64.0))
        .collect();
    let v: Vec<f32> = (0..vector_elements)
        .map(|i| ((i as i64 * 13 % 23) - 11) as f32 * (1.0 / 64.0))
        .collect();
    let g = vec![-std::f32::consts::LN_2; HEADS];
    let beta = vec![0.5f32; HEADS];

    let mut state_bytes = Vec::with_capacity(state_elements * 4 + GUARD_BYTES);
    for value in &state {
        state_bytes.extend_from_slice(&value.to_le_bytes());
    }
    state_bytes.extend(std::iter::repeat_n(CANARY, GUARD_BYTES));
    let state_dev = dev
        .alloc(state_bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&state_bytes, &state_dev, 0).unwrap();

    let mut output_bytes = Vec::with_capacity(vector_elements * 2 + GUARD_BYTES);
    for _ in 0..vector_elements {
        output_bytes.extend_from_slice(&OUTPUT_SENTINEL.to_le_bytes());
    }
    output_bytes.extend(std::iter::repeat_n(CANARY, GUARD_BYTES));
    let out_dev = dev
        .alloc(output_bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&output_bytes, &out_dev, 0).unwrap();

    let q_dev = upload_f16(dev.as_ref(), &q);
    let k_dev = upload_f16(dev.as_ref(), &k);
    let v_dev = upload_f16(dev.as_ref(), &v);
    let g_dev = upload_f32(dev.as_ref(), &g);
    let beta_dev = upload_f32(dev.as_ref(), &beta);
    kernels
        .deltanet_gated_step_f16(
            &out_dev, &state_dev, &q_dev, &k_dev, &v_dev, &g_dev, &beta_dev, HEADS, D_STATE,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let mut expected_state = state;
    for head in 0..HEADS {
        let head_base = head * D_STATE;
        let state_base = head * D_STATE * D_STATE;
        for column in 0..D_STATE {
            let mut kv = 0.0f32;
            for row in 0..D_STATE {
                let index = state_base + row * D_STATE + column;
                let decayed = expected_state[index] * 0.5;
                kv = r(k[head_base + row]).mul_add(decayed, kv);
            }
            let delta = 0.5 * (r(v[head_base + column]) - kv);
            for row in 0..D_STATE {
                let index = state_base + row * D_STATE + column;
                let decayed = expected_state[index] * 0.5;
                expected_state[index] = delta.mul_add(r(k[head_base + row]), decayed);
            }
        }
    }
    let mut expected_state_bytes = Vec::with_capacity(state_elements * 4 + GUARD_BYTES);
    for value in expected_state {
        expected_state_bytes.extend_from_slice(&value.to_le_bytes());
    }
    expected_state_bytes.extend(std::iter::repeat_n(CANARY, GUARD_BYTES));
    let mut actual_state_bytes = vec![0u8; expected_state_bytes.len()];
    dev.read(&state_dev, 0, &mut actual_state_bytes).unwrap();
    assert_eq!(actual_state_bytes, expected_state_bytes);

    let mut actual_output_bytes = vec![0u8; output_bytes.len()];
    dev.read(&out_dev, 0, &mut actual_output_bytes).unwrap();
    for bytes in actual_output_bytes[..vector_elements * 2].chunks_exact(2) {
        let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
        assert_ne!(bits, OUTPUT_SENTINEL);
        assert!(f16::from_bits(bits).is_finite());
    }
    assert_eq!(
        &actual_output_bytes[vector_elements * 2..],
        &output_bytes[vector_elements * 2..]
    );
}

/// Dwa kolejne kafle muszą dawać ten sam wynik i stan co jeden pełny skan.
#[test]
fn scan_inplace_offset_t128_matches_t256() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let steps = 256usize;
    let tile = 128usize;
    let n_heads = 1usize;
    let d_state = 128usize;
    let vector_elements = steps * n_heads * d_state;
    let state_elements = n_heads * d_state * d_state;
    let q: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 5 + 1) * 0.25)
        .collect();
    let k: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 7 + 2) * 0.25)
        .collect();
    let v: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 11 + 3) * 0.25)
        .collect();
    let g: Vec<f32> = (0..steps * n_heads)
        .map(|i| -0.01 - (i % 7) as f32 * 0.005)
        .collect();
    let beta: Vec<f32> = (0..steps * n_heads)
        .map(|i| 0.2 + (i % 5) as f32 * 0.1)
        .collect();
    let state: Vec<f32> = (0..state_elements).map(|i| seed(i + 9) * 0.01).collect();

    let q_dev = upload_f16(dev.as_ref(), &q);
    let k_dev = upload_f16(dev.as_ref(), &k);
    let v_dev = upload_f16(dev.as_ref(), &v);
    let g_dev = upload_f32(dev.as_ref(), &g);
    let beta_dev = upload_f32(dev.as_ref(), &beta);
    let full_state = upload_f32(dev.as_ref(), &state);
    let tiled_state = upload_f32(dev.as_ref(), &state);
    let full_out = dev
        .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let tiled_out = dev
        .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .deltanet_gated_scan_inplace_f16(
            &full_out,
            &full_state,
            &q_dev,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            steps,
            n_heads,
            d_state,
            &stream,
        )
        .unwrap();
    for token_offset in [0, tile] {
        kernels
            .deltanet_gated_scan_inplace_f16_at(
                &tiled_out,
                &tiled_state,
                &q_dev,
                &k_dev,
                &v_dev,
                &g_dev,
                &beta_dev,
                token_offset,
                tile,
                n_heads,
                d_state,
                &stream,
            )
            .unwrap();
    }
    dev.synchronize().unwrap();

    assert_eq!(
        read_f16(dev.as_ref(), &tiled_out, vector_elements),
        read_f16(dev.as_ref(), &full_out, vector_elements)
    );
    assert_eq!(
        read_f32(dev.as_ref(), &tiled_state, state_elements),
        read_f32(dev.as_ref(), &full_state, state_elements)
    );
}

/// ValueKey zachowuje logiczny stan referencji, a oba jego tryby kończą
/// z identycznym fizycznym stanem.
#[test]
fn value_key_scan_matches_key_value_and_inplace() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if kernels.preferred_delta_state_layout(128) != DeltaStateLayout::ValueKey {
        return;
    }
    let stream = dev.create_stream().unwrap();

    const STEPS: usize = 3;
    const HEADS: usize = 2;
    const D_STATE: usize = 128;
    let vector_elements = STEPS * HEADS * D_STATE;
    let state_elements = HEADS * D_STATE * D_STATE;
    let state_key_value: Vec<f32> = (0..state_elements)
        .map(|i| seed(i * 13 + 7) * 0.01)
        .collect();
    let mut state_value_key = vec![0.0f32; state_elements];
    for head in 0..HEADS {
        for key in 0..D_STATE {
            for value in 0..D_STATE {
                state_value_key[head * D_STATE * D_STATE + value * D_STATE + key] =
                    state_key_value[head * D_STATE * D_STATE + key * D_STATE + value];
            }
        }
    }
    let q: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 5 + 1) * 0.25)
        .collect();
    let k: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 7 + 2) * 0.25)
        .collect();
    let v: Vec<f32> = (0..vector_elements)
        .map(|i| seed(i * 11 + 3) * 0.25)
        .collect();
    let g: Vec<f32> = (0..STEPS * HEADS)
        .map(|i| -0.02 - (i % 3) as f32 * 0.01)
        .collect();
    let beta: Vec<f32> = (0..STEPS * HEADS)
        .map(|i| 0.25 + (i % 4) as f32 * 0.1)
        .collect();
    let q_dev = upload_f16(dev.as_ref(), &q);
    let k_dev = upload_f16(dev.as_ref(), &k);
    let v_dev = upload_f16(dev.as_ref(), &v);
    let g_dev = upload_f32(dev.as_ref(), &g);
    let beta_dev = upload_f32(dev.as_ref(), &beta);
    let key_value_state = upload_f32(dev.as_ref(), &state_key_value);
    let value_key_state = upload_f32(dev.as_ref(), &state_value_key);
    let value_key_inplace_state = upload_f32(dev.as_ref(), &state_value_key);
    let key_value_checkpoints = dev
        .alloc(STEPS * state_elements * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    let value_key_checkpoints = dev
        .alloc(STEPS * state_elements * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    let value_key_final = dev
        .alloc(state_elements * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    let value_key_recomputed = dev
        .alloc(state_elements * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    let decisions = dev.alloc(8, MemKind::Device, Pool::Activations).unwrap();
    dev.write(bytemuck::cast_slice(&[2i32, 0]), &decisions, 0)
        .unwrap();
    let key_value_out = dev
        .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let value_key_out = dev
        .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let value_key_inplace_out = dev
        .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    let alignment_error = kernels
        .deltanet_value_key_scan_checkpoints_f16_at(
            &value_key_out,
            &value_key_checkpoints,
            1,
            &value_key_state,
            &q_dev,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            1,
            STEPS,
            HEADS,
            &stream,
        )
        .unwrap_err();
    assert!(alignment_error.to_string().contains("wyrównany do f32"));

    kernels
        .deltanet_gated_scan_f16(
            &key_value_out,
            &key_value_checkpoints,
            &key_value_state,
            &q_dev,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            STEPS,
            HEADS,
            D_STATE,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_value_key_scan_checkpoints_f16(
            &value_key_out,
            &value_key_checkpoints,
            &value_key_state,
            &q_dev,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            1,
            STEPS,
            HEADS,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_value_key_scan_inplace_f16(
            &value_key_inplace_out,
            &value_key_final,
            &value_key_inplace_state,
            &q_dev,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            1,
            STEPS,
            HEADS,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_value_key_commit_recompute_f32(
            &value_key_recomputed,
            &value_key_state,
            &k_dev,
            &v_dev,
            &g_dev,
            &beta_dev,
            &decisions,
            1,
            STEPS,
            HEADS,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let reference = read_f32(dev.as_ref(), &key_value_checkpoints, STEPS * state_elements);
    let candidate = read_f32(dev.as_ref(), &value_key_checkpoints, STEPS * state_elements);
    let mut max_state_error = 0.0f32;
    for token in 0..STEPS {
        for head in 0..HEADS {
            for key in 0..D_STATE {
                for value in 0..D_STATE {
                    let key_value = ((token * HEADS + head) * D_STATE + key) * D_STATE + value;
                    let value_key = ((token * HEADS + head) * D_STATE + value) * D_STATE + key;
                    max_state_error =
                        max_state_error.max((reference[key_value] - candidate[value_key]).abs());
                }
            }
        }
    }
    assert!(max_state_error < 2e-4, "state max_err {max_state_error}");
    let reference_out = read_f16(dev.as_ref(), &key_value_out, vector_elements);
    let candidate_out = read_f16(dev.as_ref(), &value_key_out, vector_elements);
    let max_output_error = reference_out
        .iter()
        .zip(candidate_out)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_output_error < 3e-3, "output max_err {max_output_error}");
    assert_eq!(
        read_f32(dev.as_ref(), &value_key_final, state_elements),
        candidate[(STEPS - 1) * state_elements..]
    );
    assert_eq!(
        read_f32(dev.as_ref(), &value_key_recomputed, state_elements),
        candidate[state_elements..2 * state_elements]
    );
    assert_eq!(
        read_f16(dev.as_ref(), &value_key_inplace_out, vector_elements),
        read_f16(dev.as_ref(), &value_key_out, vector_elements)
    );
}

/// Persistent ValueKey zachowuje wyniki, stan między launchami i granice buforów.
#[test]
fn value_key_persistent_matches_inplace_across_loop_boundaries() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if kernels.preferred_delta_state_layout(128) != DeltaStateLayout::ValueKey {
        return;
    }
    let stream = dev.create_stream().unwrap();

    const HEADS: usize = 1;
    const D_STATE: usize = 128;
    const CANARY_BYTES: usize = 64;
    const CANARY: u8 = 0xa5;
    let state_elements = HEADS * D_STATE * D_STATE;
    let state_bytes = state_elements * 4;
    let initial_state: Vec<f32> = (0..state_elements)
        .map(|i| seed(i * 13 + 7) * 0.01)
        .collect();

    for steps in [1usize, 127, 128, 129, 2048] {
        let vector_elements = steps * HEADS * D_STATE;
        let vector_bytes = vector_elements * 2;
        let q: Vec<f32> = (0..vector_elements)
            .map(|i| seed(i * 5 + steps) * 0.125)
            .collect();
        let k: Vec<f32> = (0..vector_elements)
            .map(|i| seed(i * 7 + steps + 1) * 0.125)
            .collect();
        let v: Vec<f32> = (0..vector_elements)
            .map(|i| seed(i * 11 + steps + 2) * 0.125)
            .collect();
        let g: Vec<f32> = (0..steps).map(|i| -0.01 - (i % 7) as f32 * 0.005).collect();
        let beta: Vec<f32> = (0..steps).map(|i| 0.2 + (i % 5) as f32 * 0.1).collect();
        let q_dev = upload_f16(dev.as_ref(), &q);
        let k_dev = upload_f16(dev.as_ref(), &k);
        let v_dev = upload_f16(dev.as_ref(), &v);
        let g_dev = upload_f32(dev.as_ref(), &g);
        let beta_dev = upload_f32(dev.as_ref(), &beta);

        let persistent_state = dev
            .alloc(state_bytes + CANARY_BYTES, MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(
            &vec![CANARY; state_bytes + CANARY_BYTES],
            &persistent_state,
            0,
        )
        .unwrap();
        dev.write(bytemuck::cast_slice(&initial_state), &persistent_state, 0)
            .unwrap();
        let persistent_out = dev
            .alloc(
                vector_bytes + CANARY_BYTES,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        dev.write(
            &vec![CANARY; vector_bytes + CANARY_BYTES],
            &persistent_out,
            0,
        )
        .unwrap();
        let inplace_state = upload_f32(dev.as_ref(), &initial_state);
        let checkpoint_initial_state = upload_f32(dev.as_ref(), &initial_state);
        let inplace_out = dev
            .alloc(vector_bytes, MemKind::Device, Pool::Activations)
            .unwrap();
        let checkpoint_state = dev
            .alloc(steps * state_bytes, MemKind::Device, Pool::Weights)
            .unwrap();
        let checkpoint_out = dev
            .alloc(vector_bytes, MemKind::Device, Pool::Activations)
            .unwrap();

        kernels
            .deltanet_value_key_scan_persistent_f16(
                &persistent_out,
                &persistent_state,
                &q_dev,
                &k_dev,
                &v_dev,
                &g_dev,
                &beta_dev,
                steps,
                HEADS,
                &stream,
            )
            .unwrap();
        kernels
            .deltanet_value_key_scan_inplace_f16(
                &inplace_out,
                &inplace_state,
                &inplace_state,
                &q_dev,
                &k_dev,
                &v_dev,
                &g_dev,
                &beta_dev,
                1,
                steps,
                HEADS,
                &stream,
            )
            .unwrap();
        kernels
            .deltanet_value_key_scan_checkpoints_f16(
                &checkpoint_out,
                &checkpoint_state,
                &checkpoint_initial_state,
                &q_dev,
                &k_dev,
                &v_dev,
                &g_dev,
                &beta_dev,
                1,
                steps,
                HEADS,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let persistent_state_values = read_f32(dev.as_ref(), &persistent_state, state_elements);
        let inplace_state_values = read_f32(dev.as_ref(), &inplace_state, state_elements);
        let max_state_error = persistent_state_values
            .iter()
            .zip(&inplace_state_values)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_state_error < 2e-4,
            "T={steps}: state max_err {max_state_error}"
        );
        let checkpoint_final = read_f32_at(
            dev.as_ref(),
            &checkpoint_state,
            (steps - 1) * state_bytes,
            state_elements,
        );
        let max_checkpoint_state_error = persistent_state_values
            .iter()
            .zip(&checkpoint_final)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_checkpoint_state_error < 2e-4,
            "T={steps}: checkpoint state max_err {max_checkpoint_state_error}"
        );
        let persistent_output_values = read_f16(dev.as_ref(), &persistent_out, vector_elements);
        let inplace_output_values = read_f16(dev.as_ref(), &inplace_out, vector_elements);
        let max_output_error = persistent_output_values
            .iter()
            .zip(&inplace_output_values)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_output_error < 3e-3,
            "T={steps}: output max_err {max_output_error}"
        );
        let checkpoint_output_values = read_f16(dev.as_ref(), &checkpoint_out, vector_elements);
        let max_checkpoint_output_error = persistent_output_values
            .iter()
            .zip(&checkpoint_output_values)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_checkpoint_output_error < 3e-3,
            "T={steps}: checkpoint output max_err {max_checkpoint_output_error}"
        );

        let mut state_canary = vec![0u8; CANARY_BYTES];
        dev.read(&persistent_state, state_bytes, &mut state_canary)
            .unwrap();
        assert_eq!(state_canary, vec![CANARY; CANARY_BYTES]);
        let mut output_canary = vec![0u8; CANARY_BYTES];
        dev.read(&persistent_out, vector_bytes, &mut output_canary)
            .unwrap();
        assert_eq!(output_canary, vec![CANARY; CANARY_BYTES]);

        if matches!(steps, 128 | 129) {
            let first_steps = steps - 1;
            let split_state = upload_f32(dev.as_ref(), &initial_state);
            let mut split_output_values = Vec::with_capacity(vector_elements);
            for (start, count) in [(0usize, first_steps), (first_steps, 1usize)] {
                let vector_start = start * HEADS * D_STATE;
                let vector_end = (start + count) * HEADS * D_STATE;
                let split_q = upload_f16(dev.as_ref(), &q[vector_start..vector_end]);
                let split_k = upload_f16(dev.as_ref(), &k[vector_start..vector_end]);
                let split_v = upload_f16(dev.as_ref(), &v[vector_start..vector_end]);
                let split_g = upload_f32(dev.as_ref(), &g[start..start + count]);
                let split_beta = upload_f32(dev.as_ref(), &beta[start..start + count]);
                let split_out = dev
                    .alloc(
                        count * HEADS * D_STATE * 2,
                        MemKind::Device,
                        Pool::Activations,
                    )
                    .unwrap();
                kernels
                    .deltanet_value_key_scan_persistent_f16(
                        &split_out,
                        &split_state,
                        &split_q,
                        &split_k,
                        &split_v,
                        &split_g,
                        &split_beta,
                        count,
                        HEADS,
                        &stream,
                    )
                    .unwrap();
                dev.synchronize().unwrap();
                split_output_values.extend(read_f16(
                    dev.as_ref(),
                    &split_out,
                    count * HEADS * D_STATE,
                ));
            }
            assert_eq!(split_output_values, persistent_output_values);
            assert_eq!(
                read_f32(dev.as_ref(), &split_state, state_elements),
                persistent_state_values
            );
        }
    }
}

/// Output gated RMSNorm vs deltanet::gated_rmsnorm.
#[test]
fn gated_rmsnorm_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let nh = 32usize;
    let ds = 128usize;
    let eps = 1e-6f32;
    let o: Vec<f32> = (0..nh * ds).map(|i| seed(i * 11 + 1)).collect();
    let z: Vec<f32> = (0..nh * ds).map(|i| seed(i * 13 + 2)).collect();
    let weight: Vec<f32> = (0..ds).map(|j| 1.0 + (j % 5) as f32 * 0.1).collect();

    let o_dev = upload_f16(dev.as_ref(), &o);
    let z_dev = upload_f16(dev.as_ref(), &z);
    let w_dev = upload_f16(dev.as_ref(), &weight);
    let out_dev = dev
        .alloc(nh * ds * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .deltanet_gated_rmsnorm_f16(&out_dev, &o_dev, &z_dev, &w_dev, nh, ds, eps, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let gpu = read_f16(dev.as_ref(), &out_dev, nh * ds);

    let mut max_err = 0f32;
    for h in 0..nh {
        let oh: Vec<f32> = (0..ds).map(|j| r(o[h * ds + j])).collect();
        let zh: Vec<f32> = (0..ds).map(|j| r(z[h * ds + j])).collect();
        let wh: Vec<f32> = (0..ds).map(|j| r(weight[j])).collect();
        let mut out = vec![0f32; ds];
        deltanet::gated_rmsnorm(&oh, &zh, &wh, eps, &mut out);
        for j in 0..ds {
            max_err = max_err.max((gpu[h * ds + j] - out[j]).abs());
        }
    }
    assert!(max_err < 3e-3, "gated_rmsnorm max_err {max_err}");
}

/// Per-head log-decay and beta-sigmoid vs deltanet::delta_log_decay.
#[test]
fn log_decay_and_beta_match_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let nh = 32usize;
    let alpha: Vec<f32> = (0..nh).map(|h| seed(h * 3 + 1) * 0.5).collect();
    let dt: Vec<f32> = (0..nh).map(|h| seed(h * 5 + 2) * 0.1).collect();
    let a: Vec<f32> = (0..nh).map(|h| -(seed(h) * 0.2).exp()).collect();

    let alpha_dev = upload_f16(dev.as_ref(), &alpha);
    let dt_dev = upload_f16(dev.as_ref(), &dt);
    let a_dev = upload_f16(dev.as_ref(), &a);
    let g_dev = dev
        .alloc(nh * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let b_dev = dev
        .alloc(nh * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .deltanet_log_decay_f32(&g_dev, &alpha_dev, &dt_dev, &a_dev, nh, &stream)
        .unwrap();
    kernels
        .deltanet_beta_sigmoid_f32(&b_dev, &alpha_dev, nh, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let gpu_g = read_f32(dev.as_ref(), &g_dev, nh);
    let gpu_b = read_f32(dev.as_ref(), &b_dev, nh);

    let mut g_err = 0f32;
    let mut b_err = 0f32;
    for h in 0..nh {
        let refg = deltanet::delta_log_decay(r(alpha[h]), r(dt[h]), r(a[h]));
        g_err = g_err.max((gpu_g[h] - refg).abs());
        let refb = 1.0 / (1.0 + (-r(alpha[h])).exp());
        b_err = b_err.max((gpu_b[h] - refb).abs());
    }
    assert!(g_err < 1e-3, "log_decay max_err {g_err}");
    assert!(b_err < 1e-4, "beta_sigmoid max_err {b_err}");
}
