// ===== File: model/arch/hybrid/decode.rs — dekodowanie hybrydowe =====
use super::super::super::*;

impl Model {
    /// Uruchamia wspólny batched forward hybrydowego targetu.
    pub(crate) fn run_hybrid_batch_layers(&self, t: usize, commit_prefill: bool) -> Result<()> {
        self.hybrid_batch_entry_norm(t)?;
        if self.tp.is_some() {
            return self.run_hybrid_batch_layers_tp(t, commit_prefill);
        }
        for layer_index in 0..self.weights.layers.len() {
            self.hybrid_batch_mixer(layer_index, t, commit_prefill)?;
            self.hybrid_batch_ffn(layer_index, t)?;
            self.hybrid_batch_residual(layer_index, t)?;
        }
        Ok(())
    }

    /// Norma wejścia pierwszej warstwy dla chunka batchowego.
    pub(crate) fn hybrid_batch_entry_norm(&self, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &self.weights.layers[0].attn_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )
    }

    /// Część 1 chunka: mikser do projekcji wyjściowej włącznie.
    pub(crate) fn hybrid_batch_mixer(&self, layer_index: usize, t: usize, commit_prefill: bool) -> Result<()> {
        let layer = &self.weights.layers[layer_index];
        {
            match &layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(attention) => {
                    self.hybrid_verify_attention_layer(layer_index, attention, t)?
                }
                LayerMixer::DeltaNet(delta) => {
                    self.hybrid_verify_delta_layer(layer_index, delta, t, commit_prefill)?;
                    if commit_prefill {
                        self.commit_hybrid_prefill_delta_layer(layer_index)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Część 2 chunka: rezyduum miksera, norma przed FFN i blok FFN do `down`.
    pub(crate) fn hybrid_batch_ffn(&self, layer_index: usize, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let layer = &self.weights.layers[layer_index];
        self.kernels.rmsnorm_residual_f16(
            &pb.x,
            &pb.h,
            &pb.o_out,
            &layer.ffn_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;
        match &layer.ffn {
            LayerFfn::Dense(ffn) => self.ffn_dense_block(
                layer_index,
                ffn,
                FfnBlockBufs {
                    x: &pb.x,
                    gate: &pb.gate,
                    up: &pb.up,
                    act: &pb.act,
                    out: &pb.down,
                    gate_up: None,
                },
                t,
                &self.stream,
            ),
            // The same pair of buffers as the dense block: `pb.x` the normed
            // input, `pb.down` the [t, hidden] output. Reading the router back
            // to the host rules out graph capture, so the speculative verifier
            // refuses a MoE target before it gets here
            // (`validate_hybrid_speculation_target`).
            LayerFfn::Moe(moe) => {
                self.moe_prefill_ffn(moe, layer_index, t, p.hidden_size, &self.stream)
            }
        }
    }

    /// Część 3 chunka: rezyduum FFN i norma wejścia następnej warstwy.
    pub(crate) fn hybrid_batch_residual(&self, layer_index: usize, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let next_norm = if layer_index + 1 < self.weights.layers.len() {
            &self.weights.layers[layer_index + 1].attn_norm
        } else {
            &self.weights.output_norm
        };
        self.kernels.rmsnorm_residual_f16(
            &pb.x,
            &pb.h,
            &pb.down,
            next_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )
    }

    /// Record every launch of one decode step into a replayable graph.
    /// Stream capture does not execute the work, so buffer contents during
    /// capture are irrelevant — only addresses and launch geometry matter.
    pub(crate) fn capture_hybrid_step(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.hybrid_forward_staged(true, AttnSrc::Paged) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                // Przerywamy przechwytywanie, żeby strumień był dalej zdatny.
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    pub(crate) fn hybrid_batch_weights_capable(&self) -> bool {
        fn full_rows(weight: &DevWeight) -> bool {
            matches!(
                weight,
                DevWeight::F16 { .. }
                    | DevWeight::Q8_0 { .. }
                    | DevWeight::Q4K { .. }
                    | DevWeight::Q6K { .. }
                    | DevWeight::Q5K { .. }
                    | DevWeight::Q3K { .. }
                    | DevWeight::Q2K { .. }
                    | DevWeight::Q4_0 { .. }
                    | DevWeight::Q4_1 { .. }
                    | DevWeight::Q5_0 { .. }
                    | DevWeight::Q5_1 { .. }
                    | DevWeight::Iq4Nl { .. }
                    | DevWeight::Iq4Xs { .. }
                    | DevWeight::Mxfp4 { .. }
                    | DevWeight::Iq2Xs { .. }
                    | DevWeight::Iq2S { .. }
                    | DevWeight::Iq3S { .. }
                    | DevWeight::Iq2Xxs { .. }
                    | DevWeight::Iq3Xxs { .. }
                    | DevWeight::Iq1S { .. }
                    | DevWeight::Iq1M { .. }
                    | DevWeight::NvFp4 {
                        storage: NvFp4CtStorage::RowMajorE4M3 { .. },
                        ..
                    }
                    | DevWeight::NvFp4Gguf { .. }
            )
        }

        fn window_rows(weight: &DevWeight) -> bool {
            full_rows(weight) && !matches!(weight, DevWeight::NvFp4Gguf { .. })
        }

        self.is_hybrid()
            && self.tier.is_none()
            && matches!(
                self.weights.lm_head,
                DevWeight::F16 { .. } | DevWeight::Q8_0 { .. } | DevWeight::NvFp4Gguf { .. }
            )
            && self.weights.layers.iter().all(|layer| {
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return false;
                };
                let gate_up = match &ffn.gate_up {
                    GateUpWeights::Fused(weight) => window_rows(weight),
                    GateUpWeights::Split { gate, up } => full_rows(gate) && full_rows(up),
                };
                gate_up && full_rows(&ffn.down)
            })
    }

    /// Sprawdza pełny, niemutujący kontrakt batchowanego targetu hybrydowego.
    pub fn hybrid_batch_capable(&self) -> bool {
        self.weights.token_embd_host.is_some() && self.hybrid_batch_weights_capable()
    }

    /// Jedna warstwa kroku dekodowania modelu hybrydowego.
    ///
    /// Rozcięta na trzy części DOKŁADNIE w punktach, w których macierz wierszowo
    /// równoległa kończy się sumą cząstkową: po projekcji wyjściowej miksera i
    /// po `down` FFN. Podział na rangi wstawia tam redukcję, a jedna karta
    /// wywołuje te trzy części jedna po drugiej i nie widzi różnicy.
    ///
    /// To nie jest podział kosmetyczny — te dwa miejsca są jedynymi, w których
    /// cokolwiek przechodzi między kartami, i wynikają z kształtu warstwy:
    /// kolumnowa -> lokalne przetwarzanie -> wierszowa -> redukcja.
    pub(crate) fn hybrid_decode_layer(&self, l: usize, src: &AttnSrc) -> Result<()> {
        self.hybrid_decode_mixer(l, src)?;
        self.hybrid_decode_ffn(l)?;
        self.hybrid_decode_residual(l)
    }

    /// Część 1: mikser warstwy, do projekcji wyjściowej włącznie.
    pub(crate) fn hybrid_decode_mixer(&self, l: usize, src: &AttnSrc) -> Result<()> {
        let b = &self.bufs;
        match &self.weights.layers[l].mixer {
            LayerMixer::DeepseekAttention(_) => {
                unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
            }
            LayerMixer::Attention(a) => self.hybrid_attn_mixer(l, a, src),
            LayerMixer::DeltaNet(d) => {
                self.hybrid_delta_projections(l, d, &b.x, 1)?;
                self.hybrid_delta_mixer(l, d, 0)
            }
        }
    }

    /// Część 2: rezyduum miksera, norma przed FFN i cały blok FFN do `down`.
    pub(crate) fn hybrid_decode_ffn(&self, l: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let stream = &self.stream;
        let b = &self.bufs;
        let layer = &self.weights.layers[l];
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &b.h,
            &b.o_out,
            &layer.ffn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        match &layer.ffn {
            LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, l, hidden, stream),
            LayerFfn::Dense(ffn) => self.ffn_dense_block(
                l,
                ffn,
                FfnBlockBufs {
                    x: &b.x,
                    gate: &b.gate,
                    up: &b.up,
                    act: &b.act,
                    out: &b.down,
                    gate_up: Some(&b.gate_up),
                },
                1,
                stream,
            ),
        }
    }

    /// Część 3: rezyduum FFN i norma wejścia następnej warstwy.
    pub(crate) fn hybrid_decode_residual(&self, l: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let b = &self.bufs;
        let n_layers = self.weights.layers.len();
        let next_norm = if l + 1 < n_layers {
            &self.weights.layers[l + 1].attn_norm
        } else {
            &self.weights.output_norm
        };
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &b.h,
            &b.down,
            next_norm,
            1,
            hidden,
            eps,
            &self.stream,
        )
    }

    pub(crate) fn hybrid_forward_token(&self, token_id: u32, want_logits: bool, src: AttnSrc) -> Result<()> {
        self.stage_hybrid_embedding(token_id)?;
        self.hybrid_forward_staged(want_logits, src)
    }

    /// Wgrywa wejścia jednego chunka batchowego prefill na TĘ rangę.
    ///
    /// Dane hosta są wspólne dla całego podziału (te same tokeny, te same
    /// pozycje, ta sama tablica stron — strony przydziela wyłącznie ranga
    /// zerowa), ale bufory przypięte i docelowe należą do rangi, więc każda
    /// wykonuje wgranie u siebie.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stage_hybrid_batch_chunk(
        &self,
        chunk: &[u32],
        base: usize,
        t: usize,
        page_table: &[i32],
        ids: &[i32],
        positions: &[i32],
        visible_lens: &[i32],
        staging_slot: usize,
        wait_for_slot: bool,
    ) -> Result<Event> {
        let hidden_bytes = self.weights.descriptor.params.hidden_size * 2;
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrydowego prefill są gotowe");
        let host_staging = &hv.host_staging[staging_slot];
        let staging_ready = host_staging.ready.clone();
        if wait_for_slot {
            staging_ready.synchronize()?;
        }
        write_pinned(bytemuck::cast_slice(page_table), &host_staging.page_table)?;
        write_pinned(bytemuck::cast_slice(ids), &host_staging.ids)?;
        write_pinned(bytemuck::cast_slice(positions), &host_staging.positions)?;
        write_pinned(bytemuck::cast_slice(visible_lens), &host_staging.visible_lens)?;
        write_pinned(&(base as i32).to_le_bytes(), &host_staging.base_pos)?;
        write_pinned(&(t as i32).to_le_bytes(), &host_staging.accepted)?;
        let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
        })?;
        let staging_buffer = &host_staging.embedding;
        let staging = staging_buffer
            .host_ptr()
            .expect("pinned embedding ma mapowanie hosta");
        for (row_index, &token) in chunk.iter().enumerate() {
            let source = table
                .get(token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size)
                .ok_or_else(|| {
                    ForgeError::Scheduler(format!(
                        "token id {token} wykracza poza embedding targetu"
                    ))
                })?;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source.as_ptr() as *const u8,
                    staging.add(row_index * hidden_bytes),
                    hidden_bytes,
                );
            }
        }
        self.device.copy(
            &host_staging.page_table,
            0,
            &self.page_table_dev,
            0,
            page_table.len() * 4,
            &self.stream,
        )?;
        self.device
            .copy(&host_staging.ids, 0, &pb.ids, 0, t * 4, &self.stream)?;
        self.device.copy(
            &host_staging.positions,
            0,
            &pb.positions,
            0,
            t * 4,
            &self.stream,
        )?;
        self.device
            .copy(&host_staging.base_pos, 0, &hv.base_pos, 0, 4, &self.stream)?;
        self.device.copy(
            &host_staging.visible_lens,
            0,
            &hv.visible_lens,
            0,
            t * 4,
            &self.stream,
        )?;
        self.device
            .copy(&host_staging.accepted, 0, &hv.accepted, 0, 4, &self.stream)?;
        self.device
            .copy(staging_buffer, 0, &pb.h, 0, t * hidden_bytes, &self.stream)?;
        self.device.record_event(&staging_ready, &self.stream)?;
        Ok(staging_ready)
    }

    /// Wykonuje batch targetu hybrydowego ze wspólnymi GEMM FFN i głowy logits.
    /// Mixery (attention i DeltaNet) idą lane po lane, bo pula stanów aktywuje
    /// jeden lease naraz, a ich scratch jest jednotokenowy; batchują się norm,
    /// FFN i głowa logitów, czyli cała część ważona wagami. Stan każdego lane'a
    /// jest osobny i porządkowany na jednym streamie.
    pub(crate) fn record_hybrid_batch_forward(
        &mut self,
        seqs: &mut [&mut SeqKv],
        tokens: &[u32],
    ) -> Result<()> {
        let n = seqs.len();
        if n == 0 || tokens.len() != n {
            return Err(ForgeError::Unsupported(
                "hybrydowy batch targetu wymaga niepustego batcha i tokenu na lane".into(),
            ));
        }
        if n > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "hybrydowy batch {n} przekracza zarezerwowaną pojemność {}",
                self.batch_cap
            )));
        }
        self.ensure_hybrid_bufs()?;
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let mpp = self.max_pages_per_seq;
        let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("target hybrydowy nie ma hostowego embeddingu".into())
        })?;
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let staging = bb
            .pinned_embed
            .host_ptr()
            .expect("pinned embedding ma mapowanie hosta");
        for (lane, &token) in tokens.iter().enumerate() {
            let row = table
                .get(token as usize * hidden..(token as usize + 1) * hidden)
                .ok_or_else(|| {
                    ForgeError::Scheduler(format!(
                        "token id {token} wykracza poza embedding targetu"
                    ))
                })?;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr() as *const u8,
                    staging.add(lane * hidden * 2),
                    hidden * 2,
                );
            }
        }
        self.device
            .copy(&bb.pinned_embed, 0, &bb.h, 0, n * hidden * 2, &self.stream)?;
        self.kernels.rmsnorm_f16(
            &bb.x,
            &bb.h,
            &self.weights.layers[0].attn_norm,
            n,
            hidden,
            eps,
            &self.stream,
        )?;

        for layer_index in 0..self.weights.layers.len() {
            // Projekcje DeltaNet są bezstanowe, więc lecą raz dla całego batcha
            // z `bb.x` — jeden przebieg po wagach zamiast jednego na lane.
            if let LayerMixer::DeltaNet(delta) = &self.weights.layers[layer_index].mixer {
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.hybrid_delta_projections(layer_index, delta, &bb.x, n)?;
            }
            for (lane, seq) in seqs.iter_mut().enumerate() {
                self.activate_hybrid_sequence(seq)?;
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.device.copy(
                    &bb.x,
                    lane * hidden * 2,
                    &self.bufs.x,
                    0,
                    hidden * 2,
                    &self.stream,
                )?;
                match &self.weights.layers[layer_index].mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        self.device.copy(
                            &bb.positions,
                            lane * 4,
                            &self.bufs.pos,
                            0,
                            4,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &bb.seq_lens,
                            lane * 4,
                            &self.seq_len_dev,
                            0,
                            4,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &bb.page_table,
                            lane * mpp * 4,
                            &self.page_table_dev,
                            0,
                            mpp * 4,
                            &self.stream,
                        )?;
                        self.hybrid_attn_mixer(layer_index, attention, &AttnSrc::Paged)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        self.hybrid_delta_mixer(layer_index, delta, lane)?;
                    }
                }
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.device.copy(
                    &self.bufs.o_out,
                    0,
                    &bb.o_out,
                    lane * hidden * 2,
                    hidden * 2,
                    &self.stream,
                )?;
            }

            let layer = &self.weights.layers[layer_index];
            let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
            self.kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.o_out,
                &layer.ffn_norm,
                n,
                hidden,
                eps,
                &self.stream,
            )?;
            let ffn = layer.dense_ffn()?;
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    self.gemm_rows(&bb.gate, weight, &bb.x, n, 0, inter, &self.stream)?;
                    self.gemm_rows(&bb.up, weight, &bb.x, n, inter, inter, &self.stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&bb.gate, gate, &bb.x, n, &self.stream)?;
                    self.gemm(&bb.up, up, &bb.x, n, &self.stream)?;
                }
            }
            self.kernels.glu_mul_f16(
                self.ffn_act(),
                &bb.act,
                &bb.gate,
                &bb.up,
                n * inter,
                &self.stream,
            )?;
            self.gemm(&bb.down, &ffn.down, &bb.act, n, &self.stream)?;
            let next_norm = if layer_index + 1 < self.weights.layers.len() {
                &self.weights.layers[layer_index + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            self.kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.down,
                next_norm,
                n,
                hidden,
                eps,
                &self.stream,
            )?;
        }
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        self.logits_gemm(&bb.logits, &bb.x, n, &self.stream)
    }

}
