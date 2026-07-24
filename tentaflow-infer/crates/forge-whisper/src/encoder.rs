// ===== File: encoder.rs — Whisper audio encoder: conv stem + pre-norm transformer =====
// mel [80, 3000] → conv1(gelu) → conv2(gelu, stride 2) → [d, 1500], then a
// CPU transpose to [1500, d] that also folds in the (stored, sinusoidal)
// positional embeddings — a 1.5 MB round trip per request, acceptable at v0
// and the obvious spot for a device transpose kernel later. Cross-attention
// K/V for every decoder layer are precomputed here so the decode loop only
// runs per-token work.

use forge_types::Result;
use half::f16;

use crate::WhisperModel;

impl WhisperModel {
    /// Run the encoder over [80 * 3000] log-mel features, filling
    /// `scratch.enc_states` and the per-decoder-layer cross K/V buffers.
    pub(crate) fn encode(&mut self, features: &[f32]) -> Result<()> {
        let cfg = &self.weights.config;
        let d = cfg.d_model;
        let t_enc = cfg.max_source_positions;
        let t_in = t_enc * 2;
        let ffn = cfg.encoder_ffn_dim;
        let heads = cfg.encoder_attention_heads;
        let head_dim = cfg.head_dim();
        let scale = 1.0 / (head_dim as f32).sqrt();
        let row = d * 2; // bytes per [d] f16 row
        let s = &self.scratch;
        let k = &self.kernels;
        let stream = &self.stream;

        let mel_f16: Vec<f16> = features.iter().map(|&v| f16::from_f32(v)).collect();
        self.device
            .write(bytemuck::cast_slice(&mel_f16), &s.mel, 0)?;

        k.conv1d_k3_f16(
            &s.conv1_out,
            &s.mel,
            &self.weights.conv1_w,
            &self.weights.conv1_b,
            cfg.num_mel_bins,
            d,
            t_in,
            t_in,
            1,
            true,
            stream,
        )?;
        k.conv1d_k3_f16(
            &s.conv2_out,
            &s.conv1_out,
            &self.weights.conv2_w,
            &self.weights.conv2_b,
            d,
            d,
            t_in,
            t_enc,
            2,
            true,
            stream,
        )?;
        stream.synchronize()?;

        // [d, T] → [T, d] on the CPU, adding positional embeddings in f32.
        let mut conv_bytes = vec![0u8; d * t_enc * 2];
        self.device.read(&s.conv2_out, 0, &mut conv_bytes)?;
        let conv: &[f16] = bytemuck::cast_slice(&conv_bytes);
        let pos = &self.weights.enc_pos_host;
        let mut h = vec![f16::ZERO; t_enc * d];
        for c in 0..d {
            for t in 0..t_enc {
                h[t * d + c] = f16::from_f32(conv[c * t_enc + t].to_f32() + pos[t * d + c]);
            }
        }
        self.device.write(bytemuck::cast_slice(&h), &s.enc_h, 0)?;

        k.layernorm_f16(
            &s.enc_x,
            &s.enc_h,
            &self.weights.enc_layers[0].self_attn_ln.w,
            &self.weights.enc_layers[0].self_attn_ln.b,
            t_enc,
            d,
            crate::LN_EPS,
            stream,
        )?;

        let n_layers = self.weights.enc_layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.enc_layers[l];
            let a = &layer.self_attn;

            for t in 0..t_enc {
                let off = t * row;
                k.gemv_f16_bias_at(&s.q, off, &a.q_w, &s.enc_x, off, &a.q_b, d, d, stream)?;
                k.gemv_f16_at(&s.k, off, &a.k_w, &s.enc_x, off, d, d, stream)?;
                k.gemv_f16_bias_at(&s.v, off, &a.v_w, &s.enc_x, off, &a.v_b, d, d, stream)?;
            }
            k.attn_full_f16(
                &s.attn, &s.q, &s.k, &s.v, t_enc, heads, heads, head_dim, t_enc, false, 0, scale,
                stream,
            )?;
            for t in 0..t_enc {
                let off = t * row;
                k.gemv_f16_bias_at(&s.proj, off, &a.o_w, &s.attn, off, &a.o_b, d, d, stream)?;
            }
            k.layernorm_residual_f16(
                &s.enc_x,
                &s.enc_h,
                &s.proj,
                &layer.final_ln.w,
                &layer.final_ln.b,
                t_enc,
                d,
                crate::LN_EPS,
                stream,
            )?;

            for t in 0..t_enc {
                k.gemv_f16_bias_at(
                    &s.ffn,
                    t * ffn * 2,
                    &layer.fc1_w,
                    &s.enc_x,
                    t * row,
                    &layer.fc1_b,
                    ffn,
                    d,
                    stream,
                )?;
            }
            // In-place GELU is safe: purely elementwise.
            k.gelu_f16(&s.ffn, &s.ffn, t_enc * ffn, stream)?;
            for t in 0..t_enc {
                k.gemv_f16_bias_at(
                    &s.proj,
                    t * row,
                    &layer.fc2_w,
                    &s.ffn,
                    t * ffn * 2,
                    &layer.fc2_b,
                    d,
                    ffn,
                    stream,
                )?;
            }

            // Next sublayer's pre-norm; the last layer writes the post-norm
            // encoder states directly.
            let (out, ln) = if l + 1 < n_layers {
                (&s.enc_x, &self.weights.enc_layers[l + 1].self_attn_ln)
            } else {
                (&s.enc_states, &self.weights.enc_ln)
            };
            k.layernorm_residual_f16(
                out,
                &s.enc_h,
                &s.proj,
                &ln.w,
                &ln.b,
                t_enc,
                d,
                crate::LN_EPS,
                stream,
            )?;
        }

        // Cross-attention K/V per decoder layer over the final encoder states.
        for (l, layer) in self.weights.dec_layers.iter().enumerate() {
            let ca = &layer.cross_attn;
            for t in 0..t_enc {
                let off = t * row;
                k.gemv_f16_at(
                    &s.cross_k[l],
                    off,
                    &ca.k_w,
                    &s.enc_states,
                    off,
                    d,
                    d,
                    stream,
                )?;
                k.gemv_f16_bias_at(
                    &s.cross_v[l],
                    off,
                    &ca.v_w,
                    &s.enc_states,
                    off,
                    &ca.v_b,
                    d,
                    d,
                    stream,
                )?;
            }
        }
        stream.synchronize()?;
        Ok(())
    }
}
