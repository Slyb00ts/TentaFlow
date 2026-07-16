// ===== File: decoder.rs — Whisper text decoder: greedy loop with KV caches =====
// Per-step: token + learned positional embedding, then per layer self-attn
// (contiguous K/V appended each step), cross-attn over the precomputed
// encoder K/V, and the GELU MLP — residual/LayerNorm chained through the
// fused layernorm_residual kernel exactly like the LLM engine. Logits come
// from the tied embedding matrix via gemv_f16_out_f32; token suppression
// (suppress_tokens every step, begin_suppress_tokens on the first sampled
// token) is applied on the CPU before the argmax.

use forge_types::Result;

use crate::WhisperModel;

impl WhisperModel {
    /// Run one decoder position; returns f32 logits over the vocabulary.
    fn decode_step(&mut self, token: u32, pos: usize) -> Result<Vec<f32>> {
        let cfg = &self.weights.config;
        let d = cfg.d_model;
        let ffn = cfg.decoder_ffn_dim;
        let heads = cfg.decoder_attention_heads;
        let head_dim = cfg.head_dim();
        let scale = 1.0 / (head_dim as f32).sqrt();
        let t_enc = cfg.max_source_positions;
        let row = d * 2;
        let s = &self.scratch;
        let k = &self.kernels;
        let stream = &self.stream;

        self.device
            .write(bytemuck::cast_slice(&[token as i32]), &s.ids, 0)?;
        self.device
            .write(bytemuck::cast_slice(&[pos as i32]), &s.pos_ids, 0)?;

        k.gather_rows_f16(&s.dec_h, &self.weights.tok_emb, &s.ids, 1, d, stream)?;
        k.gather_rows_f16(&s.pos_row, &self.weights.dec_pos, &s.pos_ids, 1, d, stream)?;
        // h = tok_emb + pos_emb fused with the first pre-norm.
        k.layernorm_residual_f16(
            &s.dec_x,
            &s.dec_h,
            &s.pos_row,
            &self.weights.dec_layers[0].self_attn_ln.w,
            &self.weights.dec_layers[0].self_attn_ln.b,
            1,
            d,
            crate::LN_EPS,
            stream,
        )?;

        let n_layers = self.weights.dec_layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.dec_layers[l];

            // Self-attention: K/V of this position go straight into the cache.
            let sa = &layer.self_attn;
            k.gemv_f16_bias(&s.dec_q, &sa.q_w, &s.dec_x, &sa.q_b, d, d, stream)?;
            k.gemv_f16_at(&s.self_k[l], pos * row, &sa.k_w, &s.dec_x, 0, d, d, stream)?;
            k.gemv_f16_bias_at(
                &s.self_v[l],
                pos * row,
                &sa.v_w,
                &s.dec_x,
                0,
                &sa.v_b,
                d,
                d,
                stream,
            )?;
            k.attn_full_f16(
                &s.dec_attn,
                &s.dec_q,
                &s.self_k[l],
                &s.self_v[l],
                1,
                heads,
                heads,
                head_dim,
                pos + 1,
                true,
                pos,
                scale,
                stream,
            )?;
            k.gemv_f16_bias(&s.dec_o, &sa.o_w, &s.dec_attn, &sa.o_b, d, d, stream)?;
            k.layernorm_residual_f16(
                &s.dec_x,
                &s.dec_h,
                &s.dec_o,
                &layer.cross_attn_ln.w,
                &layer.cross_attn_ln.b,
                1,
                d,
                crate::LN_EPS,
                stream,
            )?;

            // Cross-attention over the precomputed encoder K/V.
            let ca = &layer.cross_attn;
            k.gemv_f16_bias(&s.dec_q, &ca.q_w, &s.dec_x, &ca.q_b, d, d, stream)?;
            k.attn_full_f16(
                &s.dec_attn,
                &s.dec_q,
                &s.cross_k[l],
                &s.cross_v[l],
                1,
                heads,
                heads,
                head_dim,
                t_enc,
                false,
                0,
                scale,
                stream,
            )?;
            k.gemv_f16_bias(&s.dec_o, &ca.o_w, &s.dec_attn, &ca.o_b, d, d, stream)?;
            k.layernorm_residual_f16(
                &s.dec_x,
                &s.dec_h,
                &s.dec_o,
                &layer.final_ln.w,
                &layer.final_ln.b,
                1,
                d,
                crate::LN_EPS,
                stream,
            )?;

            k.gemv_f16_bias(&s.dec_ffn, &layer.fc1_w, &s.dec_x, &layer.fc1_b, ffn, d, stream)?;
            k.gelu_f16(&s.dec_ffn, &s.dec_ffn, ffn, stream)?;
            k.gemv_f16_bias(&s.dec_o, &layer.fc2_w, &s.dec_ffn, &layer.fc2_b, d, ffn, stream)?;

            let ln = if l + 1 < n_layers {
                &self.weights.dec_layers[l + 1].self_attn_ln
            } else {
                &self.weights.dec_ln
            };
            k.layernorm_residual_f16(
                &s.dec_x, &s.dec_h, &s.dec_o, &ln.w, &ln.b, 1, d, crate::LN_EPS, stream,
            )?;
        }

        k.gemv_f16_out_f32(
            &s.logits,
            &self.weights.tok_emb,
            &s.dec_x,
            cfg.vocab_size,
            d,
            stream,
        )?;
        stream.synchronize()?;

        let mut bytes = vec![0u8; cfg.vocab_size * 4];
        self.device.read(&s.logits, 0, &mut bytes)?;
        Ok(bytemuck::cast_slice::<u8, f32>(&bytes).to_vec())
    }

    /// Greedy decode after `prompt` (forced tokens); returns generated tokens
    /// without the terminating end-of-text.
    pub(crate) fn greedy_decode(&mut self, prompt: &[u32]) -> Result<Vec<u32>> {
        let max_positions = self.weights.config.max_target_positions;
        let eot = self.weights.config.eos_token_id;
        let vocab = self.weights.config.vocab_size;

        let mut logits = Vec::new();
        for (pos, &tok) in prompt.iter().enumerate() {
            logits = self.decode_step(tok, pos)?;
        }

        let mut generated = Vec::new();
        let mut pos = prompt.len();
        while pos < max_positions {
            for &t in &self.suppress {
                if (t as usize) < vocab {
                    logits[t as usize] = f32::NEG_INFINITY;
                }
            }
            if generated.is_empty() {
                for &t in &self.begin_suppress {
                    if (t as usize) < vocab {
                        logits[t as usize] = f32::NEG_INFINITY;
                    }
                }
            }
            let next = logits
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as u32)
                .expect("non-empty logits");
            if next == eot {
                break;
            }
            generated.push(next);
            if pos + 1 >= max_positions {
                break;
            }
            logits = self.decode_step(next, pos)?;
            pos += 1;
        }
        Ok(generated)
    }
}
