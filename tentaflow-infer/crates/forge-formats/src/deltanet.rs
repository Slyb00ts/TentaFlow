// ===== File: deltanet.rs — CPU golden reference for Gated-DeltaNet linear attention =====
// Bit-for-bit intent-match of llama.cpp's Gated-DeltaNet math (models/
// delta-net-base.cpp, autoregressive path) plus the depthwise causal conv and
// the output gated-RMSNorm from models/qwen35moe.cpp. This is the numeric
// oracle the Mojo `deltanet` kernel and the engine's DeltaNet layer are
// validated against — the recurrent (per-token) scan, which is correct and
// simplest; the chunked-parallel prefill is a tracked perf follow-up.
//
// Per DeltaNet layer the mixed q|k|v stream is produced by an in-projection,
// passed through a per-channel causal conv (kernel width `d_conv`) + SiLU,
// split into q/k (`n_group` heads) and v (`dt_rank` heads), q/k are
// L2-normalized per head and (since `dt_rank > n_group`) repeated to `dt_rank`
// heads. The recurrence then runs per value-head with a `d_state × d_state`
// state matrix `S`:
//
//   decay        = exp(g)                    (g = log-decay for this head/token)
//   S[i,j]      *= decay
//   kv_pred[j]   = Σ_i k[i]·S[i,j]
//   d[j]         = beta·(v[j] − kv_pred[j])
//   S[i,j]      += k[i]·d[j]                  (rank-1 delta update)
//   o[j]         = Σ_i (q[i]/√d_state)·S[i,j] (query the UPDATED state)
//
// followed by `o = rmsnorm(o) · silu(z)` (gated RMSNorm) and the out-projection.

/// Depthwise **causal** 1-D convolution with kernel width `k`, evaluated one
/// step at a time. `window` holds the last `k-1` inputs for this channel
/// (oldest first); `x_new` is the current input. `taps[0]` multiplies the
/// oldest sample, `taps[k-1]` the newest (`x_new`) — matching ggml_ssm_conv.
/// Returns the conv output; the caller rotates `window` afterwards.
pub fn causal_conv1d_step(window: &[f32], x_new: f32, taps: &[f32]) -> f32 {
    let k = taps.len();
    debug_assert_eq!(window.len(), k - 1);
    let mut acc = 0.0f32;
    for j in 0..k - 1 {
        acc += taps[j] * window[j];
    }
    acc += taps[k - 1] * x_new;
    acc
}

/// Advance a per-channel conv `window` (length `k-1`, oldest first) after
/// consuming `x_new`: drop the oldest, append the newest.
pub fn causal_conv1d_advance(window: &mut [f32], x_new: f32) {
    let n = window.len();
    for i in 0..n.saturating_sub(1) {
        window[i] = window[i + 1];
    }
    if n > 0 {
        window[n - 1] = x_new;
    }
}

/// SiLU (x·sigmoid(x)).
#[inline]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + (-x).exp())
}

/// Softplus (ln(1+e^x)), numerically stable.
#[inline]
pub fn softplus(x: f32) -> f32 {
    if x > 20.0 {
        x
    } else {
        (1.0 + x.exp()).ln()
    }
}

/// One Gated-DeltaNet recurrence step for a single value-head. `state` is the
/// `d_state × d_state` matrix in row-major `[i*d_state + j]` (i = key index,
/// j = value index), updated in place. `q`/`k`/`v` are length `d_state`
/// (q/k already L2-normalized upstream), `g` is the per-head log-decay, `beta`
/// the per-head write gate. Writes the head output (length `d_state`) into
/// `out`. Mirrors `build_delta_net_autoregressive`.
#[allow(clippy::too_many_arguments)]
pub fn gated_delta_step(
    state: &mut [f32],
    d_state: usize,
    q: &[f32],
    k: &[f32],
    v: &[f32],
    g: f32,
    beta: f32,
    out: &mut [f32],
) {
    debug_assert_eq!(state.len(), d_state * d_state);
    let decay = g.exp();
    // Decay the whole state, then compute kv_pred[j] = Σ_i k[i]·S[i,j].
    let mut kv_pred = vec![0.0f32; d_state];
    for i in 0..d_state {
        let ki = k[i];
        let row = &mut state[i * d_state..i * d_state + d_state];
        for j in 0..d_state {
            row[j] *= decay;
            kv_pred[j] += ki * row[j];
        }
    }
    // d[j] = beta·(v[j] − kv_pred[j]); rank-1 update S[i,j] += k[i]·d[j].
    let mut d = vec![0.0f32; d_state];
    for j in 0..d_state {
        d[j] = beta * (v[j] - kv_pred[j]);
    }
    for i in 0..d_state {
        let ki = k[i];
        let row = &mut state[i * d_state..i * d_state + d_state];
        for j in 0..d_state {
            row[j] += ki * d[j];
        }
    }
    // o[j] = Σ_i (q[i]/√d_state)·S[i,j] over the UPDATED state.
    let inv_sqrt = 1.0f32 / (d_state as f32).sqrt();
    for o in out.iter_mut().take(d_state) {
        *o = 0.0;
    }
    for i in 0..d_state {
        let qi = q[i] * inv_sqrt;
        let row = &state[i * d_state..i * d_state + d_state];
        for j in 0..d_state {
            out[j] += qi * row[j];
        }
    }
}

/// Per-head log-decay `g` from the raw alpha projection: softplus(alpha + dt)
/// scaled by `a` (`a = -exp(A_log)`, already stored negative). Mirrors
/// qwen35moe.cpp: `gate = (alpha + dt).softplus() * ssm_a`.
#[inline]
pub fn delta_log_decay(alpha: f32, dt_bias: f32, a: f32) -> f32 {
    softplus(alpha + dt_bias) * a
}

/// L2-normalize a per-head vector in place (`x / (‖x‖₂ over d + eps)`); matches
/// ggml_l2_norm applied to the conv q/k before the recurrence.
pub fn l2_norm(x: &mut [f32], eps: f32) {
    let ss: f32 = x.iter().map(|&v| v * v).sum();
    let inv = 1.0f32 / (ss + eps).sqrt();
    for v in x.iter_mut() {
        *v *= inv;
    }
}

/// Output gated RMSNorm for one value-head: `rmsnorm(o, weight) · silu(z)`.
/// `weight`/`z` are length `d`. Mirrors qwen35moe.cpp `build_norm_gated`
/// (RMSNorm has no elementwise bias; the gate is `silu(z)`).
pub fn gated_rmsnorm(o: &[f32], z: &[f32], weight: &[f32], eps: f32, out: &mut [f32]) {
    let d = o.len();
    let ss: f32 = o.iter().map(|&v| v * v).sum::<f32>() / d as f32;
    let inv = 1.0f32 / (ss + eps).sqrt();
    for j in 0..d {
        out[j] = (o[j] * inv * weight[j]) * silu(z[j]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv_step_and_advance() {
        // taps oldest→newest, window = last 3 inputs.
        let taps = [0.5f32, -1.0, 2.0, 0.25];
        let mut window = [1.0f32, 2.0, 3.0];
        let x = 4.0;
        // 0.5*1 - 1*2 + 2*3 + 0.25*4 = 0.5 -2 +6 +1 = 5.5
        let y = causal_conv1d_step(&window, x, &taps);
        assert!((y - 5.5).abs() < 1e-6);
        causal_conv1d_advance(&mut window, x);
        assert_eq!(window, [2.0, 3.0, 4.0]);
    }

    #[test]
    fn delta_step_matches_direct_formula() {
        // Small d_state=3, verify against an independent recomputation.
        let d = 3;
        let mut s = vec![0.1, 0.2, -0.3, 0.4, -0.5, 0.6, -0.7, 0.8, 0.9];
        let s0 = s.clone();
        let q = [0.5f32, -0.2, 0.3];
        let k = [0.1f32, 0.4, -0.2];
        let v = [1.0f32, -1.0, 0.5];
        let g = -0.25f32;
        let beta = 0.7f32;
        let mut out = vec![0.0f32; d];
        gated_delta_step(&mut s, d, &q, &k, &v, g, beta, &mut out);

        // Independent reference.
        let decay = g.exp();
        let mut sr = s0.clone();
        for x in sr.iter_mut() {
            *x *= decay;
        }
        let mut kv = [0.0f32; 3];
        for i in 0..d {
            for j in 0..d {
                kv[j] += k[i] * sr[i * d + j];
            }
        }
        let mut dd = [0.0f32; 3];
        for j in 0..d {
            dd[j] = beta * (v[j] - kv[j]);
        }
        for i in 0..d {
            for j in 0..d {
                sr[i * d + j] += k[i] * dd[j];
            }
        }
        let inv = 1.0f32 / (d as f32).sqrt();
        let mut outr = [0.0f32; 3];
        for i in 0..d {
            for j in 0..d {
                outr[j] += q[i] * inv * sr[i * d + j];
            }
        }
        for j in 0..d {
            assert!((out[j] - outr[j]).abs() < 1e-6, "out[{j}]");
        }
        for idx in 0..d * d {
            assert!((s[idx] - sr[idx]).abs() < 1e-6, "state[{idx}]");
        }
    }

    #[test]
    fn delta_step_beta_zero_only_decays() {
        // beta = 0 → no write; state only decays, o = decay·(q/√d)·S_prev.
        let d = 4;
        let mut s: Vec<f32> = (0..d * d).map(|i| (i as f32) * 0.01 - 0.05).collect();
        let s0 = s.clone();
        let q = [0.3f32, 0.1, -0.2, 0.4];
        let k = [0.9f32, -0.8, 0.7, 0.2];
        let v = [1.0f32; 4];
        let g = -0.5f32;
        let mut out = vec![0.0f32; d];
        gated_delta_step(&mut s, d, &q, &k, &v, g, 0.0, &mut out);
        let decay = g.exp();
        for idx in 0..d * d {
            assert!((s[idx] - s0[idx] * decay).abs() < 1e-6);
        }
    }

    #[test]
    fn log_decay_is_negative_and_scaled() {
        // a = -exp(A_log) < 0, softplus > 0 → g < 0 (a decay).
        let a = -(0.3f32.exp());
        let g = delta_log_decay(0.2, -0.1, a);
        assert!(g < 0.0);
        assert!((g - softplus(0.1) * a).abs() < 1e-6);
    }

    #[test]
    fn gated_rmsnorm_known_values() {
        let o = [3.0f32, 4.0];
        let z = [0.0f32, 100.0]; // silu(0)=0, silu(100)≈100
        let w = [1.0f32, 1.0];
        let mut out = [0.0f32; 2];
        gated_rmsnorm(&o, &z, &w, 0.0, &mut out);
        // ss = (9+16)/2 = 12.5, inv = 1/sqrt(12.5)
        let inv = 1.0f32 / 12.5f32.sqrt();
        assert!((out[0] - (3.0 * inv * silu(0.0))).abs() < 1e-5);
        assert!((out[1] - (4.0 * inv * silu(100.0))).abs() < 1e-3);
        assert!(out[0].abs() < 1e-6); // gated off
    }

    #[test]
    fn l2_norm_unit() {
        let mut x = [3.0f32, 4.0];
        l2_norm(&mut x, 0.0);
        assert!((x[0] - 0.6).abs() < 1e-6);
        assert!((x[1] - 0.8).abs() < 1e-6);
    }
}
