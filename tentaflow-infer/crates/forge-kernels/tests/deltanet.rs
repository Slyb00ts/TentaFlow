// ===== File: deltanet.rs — Gated-DeltaNet kernels vs forge-formats CPU oracle =====
// The DeltaNet decode kernels (conv+SiLU, per-head L2 norm, the rank-1 gated
// state scan, output gated-RMSNorm, log-decay / beta gates) must reproduce the
// forge_formats::deltanet reference within f16 rounding. Skips cleanly with no
// CUDA device.

use std::sync::Arc;

use forge_formats::deltanet;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
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
    let buf = dev.alloc(bytes.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn upload_f32(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let bytes = unsafe { std::slice::from_raw_parts(vals.as_ptr() as *const u8, vals.len() * 4) };
    let buf = dev.alloc(bytes.len(), MemKind::Device, Pool::Weights).unwrap();
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
    let mut bytes = vec![0u8; n * 4];
    dev.read(buf, 0, &mut bytes).unwrap();
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
        .deltanet_conv_silu_f16(&out_dev, &win_dev, &x_dev, &w_dev, conv_dim, d_conv, &stream)
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
    let g_dev = dev.alloc(nh * 4, MemKind::Device, Pool::Activations).unwrap();
    let b_dev = dev.alloc(nh * 4, MemKind::Device, Pool::Activations).unwrap();

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
