// ===== File: model/graph.rs — przechwytywanie i odtwarzanie grafow CUDA =====
use super::*;

/// `FORGE_HYBRID_DECODE_GRAPH=0` wykonuje krok hybrydowy jawnym łańcuchem
/// zamiast odtwarzania grafu.
///
/// Podział FFN na karty i tak nie może użyć grafu (ROCm przerywa asercją przy
/// przechwytywaniu rozwidlenia między kartami), więc bez tego przełącznika nie da
/// się oddzielić zysku z drugiej karty od kosztu utraconego odtwarzania.
fn hybrid_decode_graph_requested() -> bool {
    std::env::var("FORGE_HYBRID_DECODE_GRAPH").map_or(true, |value| value != "0")
}

impl Model {
    pub(crate) fn step_launch(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        self.tick_moe_residency()?;
        let p = self.weights.descriptor.params.clone();
        let pos = seq.len;

        if pos >= p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {pos} exceeds model context {}",
                p.max_position_embeddings
            )));
        }
        if self.is_hybrid() {
            self.activate_hybrid_sequence(seq)?;
        }

        self.tier_ensure_capacity(seq, 1)?;
        if self.tier.is_some() {
            if !seq.spilled.is_empty() {
                if self.tier_can_restore(seq) {
                    // The whole sequence fits again with the watermark reserve
                    // intact: bring it back and take the graphed fast path.
                    self.tier_restore_or_recompute(seq)?;
                } else {
                    return self.step_streamed(seq, token_id);
                }
            }
        }
        self.record_token(seq, token_id);

        let page_boundary = seq.len.is_multiple_of(self.kv.cfg.page_size);
        if page_boundary {
            // A new page is about to be allocated; reclaim a cached prefix page
            // if the free stack is empty so decode growth never starves behind
            // the prefix cache (no-op when the cache is inactive/empty).
            self.ensure_free_pages(1);
        }
        self.kv.grow(seq)?;
        self.upload_decode_inputs(token_id, pos)?;

        // The page table changes when a page is appended — and goes stale when
        // another sequence used the single-stream path, or batched growth /
        // tier restores rewrote this sequence's pages.
        if page_boundary || self.pt_seq != seq.id {
            self.upload_page_table(seq)?;
        }

        // Dekodowanie hybrydowe (uwaga + DeltaNet): rekurencyjny skan po
        // rezydentnym stanie SSM. Wstawienie embeddingu zależy od `token_id` i
        // zostaje poza grafem; reszta kroku czyta pozycję i długość sekwencji z
        // buforów urządzenia, więc jest przechwytywalna i odtwarzana bez
        // uruchamiania ~1200 kerneli po kolei — profil pokazał, że te przerwy
        // między uruchomieniami to 15,8% czasu fazy liczenia.
        if self.is_hybrid() {
            self.ensure_hybrid_bufs()?;
            self.stage_hybrid_embedding(token_id)?;
            // Krok obejmujący dwie karty idzie jawnym łańcuchem: przechwycenie
            // rozwidlenia strumienia między kartami ROCm przerywa asercją we
            // własnym runtime (patrz `run_step`).
            if self.tp_ffn.is_some() || self.tp.is_some() || !hybrid_decode_graph_requested() {
                return self.hybrid_forward_staged(true, AttnSrc::Paged);
            }
            let slot = seq
                .hybrid_state
                .expect("aktywna sekwencja hybrydowa ma przypisany slot")
                .slot;
            if !self.decode_hybrid_graph.contains_key(&slot) {
                let graph = self.capture_hybrid_step()?;
                self.decode_hybrid_graph.insert(slot, graph);
            }
            let graph = self
                .decode_hybrid_graph
                .get(&slot)
                .expect("captured above")
                .clone();
            return self.device.launch_graph(&graph, &self.stream);
        }

        // Routed MoE decode: the device-side grouped expert dispatch keeps the
        // router selection on-device (no host readback), so a fully-gidx model
        // records into a replayable graph like the dense path. A model with a
        // fallback expert quant (e.g. Q8_0) still reads back per layer and runs
        // the explicit chain each step.
        if self.weights.is_moe() {
            if self.moe_fully_gidx() {
                if self.decode_moe_graph.is_none() {
                    let graph = self.capture_step_moe()?;
                    self.decode_moe_graph = Some(graph);
                }
                let graph = self
                    .decode_moe_graph
                    .as_ref()
                    .expect("captured above")
                    .clone();
                return self.device.launch_graph(&graph, &self.stream);
            }
            return self.run_step_moe();
        }

        // Podział FFN na karty: krok obejmuje pracę dwóch urządzeń, więc idzie
        // jawnym łańcuchem zamiast przechwyconego grafu.
        //
        // Nie z ostrożności — sprawdzone. Przechwycenie kroku obejmującego dwie
        // karty (fork strumienia przez zdarzenie i join z powrotem, czyli wzorzec,
        // który CUDA opisuje jako poprawny) ROCm przerywa asercją we WŁASNYM
        // runtime: `hip::Stream*` … Assertion '__n < this->size()' failed, zrzut
        // pamięci zamiast błędu do obsłużenia. Kosztuje to zmierzone 26 us na
        // warstwę, czyli 0,83 ms na token — tyle wynosi premia za odtwarzanie
        // grafu, której podział nie może na tym sterowniku dostać.
        if self.tp_ffn.is_some() {
            return self.run_step_separate(AttnSrc::Paged);
        }

        // Podział SPMD modelu gęstego: ta sama pętla warstw na każdej randze,
        // z dwiema redukcjami. Idzie jawnym łańcuchem z tego samego powodu, co
        // wyżej — przechwycenie kroku obejmującego dwie karty ROCm przerywa
        // asercją we własnym runtime.
        if self.tp.is_some() {
            return self.dense_forward_staged_tp(&AttnSrc::Paged, true);
        }

        // Rot decode commits the current token into the packed store + ring and
        // reads it back through the split-K attn_decode_rot. The pack kernel
        // takes the token position from `bufs.pos` (device-resident), so the
        // chain is position-independent and captured once like the f16 path.
        if self.kv.cfg.quant.is_rot() {
            if self.decode_rot_graph.is_none() {
                let graph = self.capture_decode_rot()?;
                self.decode_rot_graph = Some(graph);
            }
            let graph = self
                .decode_rot_graph
                .as_ref()
                .expect("captured above")
                .clone();
            return self.device.launch_graph(&graph, &self.stream);
        }

        if self.decode_graph.is_none() {
            let graph = self.capture_step()?;
            self.decode_graph = Some(graph);
        }
        let graph = self.decode_graph.as_ref().expect("captured above").clone();
        self.device.launch_graph(&graph, &self.stream)
    }

    /// Capture the rotational decode step into a replayable graph. The recorded
    /// launches read all per-step inputs (token id, position, seq len, page
    /// table) from device buffers refreshed before each replay, so one capture
    /// serves every token.
    fn capture_decode_rot(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_rot(AttnSrc::Paged);
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    fn capture_step(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = if self.fused_decode_supported() {
            self.run_step_fused(AttnSrc::Paged)
        } else {
            self.run_step_separate(AttnSrc::Paged)
        };
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                // Abort the capture so the stream is usable again.
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// Record one batched forward + logit head over `n` rows into the model
    /// stream (no sampling — that runs param-dependent, outside the graph).
    /// Mirrors the prefill dataflow (rmsnorm rows=n, batched GEMM projections,
    /// row-batched silu/residual) but swaps causal prefill attention for the
    /// per-sequence paged flash-decode. Lanes `0..resident` attend through
    /// their page tables in one launch; `streamed` lanes (packed at the tail
    /// of the batch: spilled KV that exceeds free VRAM) attend one at a time
    /// over the tier staging slabs holding their full context per layer. A
    /// batch with streamed lanes is never graph-captured; pure-resident
    /// buckets stay captured (`streamed` empty, `resident == n`).
    pub(crate) fn record_batch_forward(
        &self,
        n: usize,
        resident: usize,
        streamed: &[(usize, &SeqKv)],
    ) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let eps = p.rms_norm_eps;
        let scale = p.attn_scale_at(0);
        let kernels = &self.kernels;
        let stream = &self.stream;
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let n_layers = self.weights.layers.len();

        kernels.gather_rows_f16(
            &bb.h,
            &self.weights.token_embd_f16,
            &bb.ids,
            n,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &bb.x,
            &bb.h,
            &self.weights.layers[0].attn_norm,
            n,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            let mut segmented_qkv = false;
            // Raw q/k/v projections (no norm/rope here — attn_decode_split folds
            // the q/k-norm + RoPE + paged append into its per-seq prologue).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    if let (Some(qkv), Some(workspace)) = (&bb.nvfp4_ct_qkv, &bb.nvfp4_ct_workspace)
                    {
                        segmented_qkv = self.gemm_nvfp4_ct_direct(
                            qkv,
                            workspace,
                            w,
                            &bb.x,
                            n,
                            Nvfp4CtProjection::Qkv,
                            stream,
                        )?;
                    }
                    if !segmented_qkv {
                        self.gemm_rows(&bb.q, w, &bb.x, n, 0, q_dim, stream)?;
                        self.gemm_rows(&bb.k, w, &bb.x, n, q_dim, kv_dim, stream)?;
                        self.gemm_rows(&bb.v, w, &bb.x, n, q_dim + kv_dim, kv_dim, stream)?;
                    }
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&bb.q, qk, &bb.x, n, 0, q_dim, stream)?;
                    self.gemm_rows(&bb.k, qk, &bb.x, n, q_dim, kv_dim, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemm(&bb.q, q, &bb.x, n, stream)?;
                    self.gemm(&bb.k, k, &bb.x, n, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
            }
            let (q_input, q_offset, k_input, k_offset, v_input, v_offset) = if segmented_qkv {
                let qkv = bb
                    .nvfp4_ct_qkv
                    .as_ref()
                    .expect("segmentowany QKV wymaga bufora padded");
                let physical_m =
                    nvfp4_ct_physical_m(n).expect("segmentowany QKV ma fizyczny kafel");
                (
                    qkv,
                    0,
                    qkv,
                    physical_m * q_dim * 2,
                    qkv,
                    physical_m * (q_dim + kv_dim) * 2,
                )
            } else {
                (&bb.q, 0, &bb.k, 0, &bb.v, 0)
            };
            if resident > 0 {
                kernels.attn_decode_split(
                    &bb.attn_parts,
                    q_input,
                    q_offset,
                    k_input,
                    k_offset,
                    v_input,
                    v_offset,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &bb.page_table,
                    &bb.seq_lens,
                    &bb.positions,
                    resident,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &bb.attn_out,
                    &bb.attn_parts,
                    resident,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
            }
            for &(lane, seq) in streamed {
                // Lane-scalar pos/len land in the single-seq buffers the
                // n_seqs=1 launch reads at index 0; the lane's q/k/v rows are
                // addressed by byte offset. The attention appends the token
                // into staging and the tail page mirrors back to the canonical
                // slab, exactly like the single-stream staged step. All copies
                // and launches ride the compute stream, so slab reuse across
                // lanes is stream-ordered.
                let tier = self.tier.as_ref().expect("streamed lanes require tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                let db = &self.bufs;
                self.device
                    .copy(&bb.positions, lane * 4, &db.pos, 0, 4, stream)?;
                self.device
                    .copy(&bb.seq_lens, lane * 4, &self.seq_len_dev, 0, 4, stream)?;
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_split(
                    &db.attn_parts,
                    q_input,
                    q_offset + lane * q_dim * 2,
                    k_input,
                    k_offset + lane * kv_dim * 2,
                    v_input,
                    v_offset + lane * kv_dim * 2,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    &db.pos,
                    1,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &db.attn_out,
                    &db.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
                self.device.copy(
                    &db.attn_out,
                    0,
                    &bb.attn_out,
                    lane * q_dim * 2,
                    q_dim * 2,
                    stream,
                )?;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &slot.stage[0],
                    lp * rb,
                    &self.kv.k[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &slot.stage[1],
                    lp * rb,
                    &self.kv.v[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
            }
            let mut specialized_o = false;
            if let Some(workspace) = &bb.nvfp4_ct_workspace {
                specialized_o = self.gemm_nvfp4_ct_direct(
                    &bb.o_out,
                    workspace,
                    &layer.attn().attn_o,
                    &bb.attn_out,
                    n,
                    Nvfp4CtProjection::Output,
                    stream,
                )?;
            }
            if !specialized_o {
                self.gemm(&bb.o_out, &layer.attn().attn_o, &bb.attn_out, n, stream)?;
            }
            kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.o_out,
                &layer.ffn_norm,
                n,
                hidden,
                eps,
                stream,
            )?;

            // Warstwa routowana idzie jednym grupowanym zgłoszeniem: ekspert
            // czyta swoje wagi raz dla wszystkich linii, które go wybrały.
            if let LayerFfn::Moe(moe) = &layer.ffn {
                self.moe_batch_ffn(moe, n, hidden, stream)?;
                let next_norm = if l + 1 < n_layers {
                    &self.weights.layers[l + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                kernels.rmsnorm_residual_f16(
                    &bb.x, &bb.h, &bb.down, next_norm, n, hidden, eps, stream,
                )?;
                continue;
            }

            let mut segmented_gate_up = false;
            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    if let (Some(gate_up), Some(workspace)) =
                        (&bb.nvfp4_ct_gate_up, &bb.nvfp4_ct_workspace)
                    {
                        segmented_gate_up = self.gemm_nvfp4_ct_direct(
                            gate_up,
                            workspace,
                            w,
                            &bb.x,
                            n,
                            Nvfp4CtProjection::GateUp,
                            stream,
                        )?;
                    }
                    if !segmented_gate_up {
                        self.gemm_rows(&bb.gate, w, &bb.x, n, 0, inter, stream)?;
                        self.gemm_rows(&bb.up, w, &bb.x, n, inter, inter, stream)?;
                    }
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&bb.gate, gate, &bb.x, n, stream)?;
                    self.gemm(&bb.up, up, &bb.x, n, stream)?;
                }
            }
            if segmented_gate_up {
                let physical_m =
                    nvfp4_ct_physical_m(n).expect("segmentowany GateUp ma fizyczny kafel");
                kernels.glu_mul_f16_at(
                    self.ffn_act(),
                    &bb.act,
                    bb.nvfp4_ct_gate_up
                        .as_ref()
                        .expect("segmentowany GateUp wymaga bufora padded"),
                    0,
                    physical_m * inter * 2,
                    n * inter,
                    stream,
                )?;
            } else {
                kernels.glu_mul_f16(
                    self.ffn_act(),
                    &bb.act,
                    &bb.gate,
                    &bb.up,
                    n * inter,
                    stream,
                )?;
            }
            let down = &layer.dense_ffn()?.down;
            let mut specialized_down = false;
            if let Some(workspace) = &bb.nvfp4_ct_workspace {
                specialized_down = self.gemm_nvfp4_ct_direct(
                    &bb.down,
                    workspace,
                    down,
                    &bb.act,
                    n,
                    Nvfp4CtProjection::Down,
                    stream,
                )?;
            }
            if !specialized_down {
                self.gemm(&bb.down, down, &bb.act, n, stream)?;
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels
                .rmsnorm_residual_f16(&bb.x, &bb.h, &bb.down, next_norm, n, hidden, eps, stream)?;
        }

        // Głowa liczy się na szerokości, dla której istnieje wsadowy przemiat:
        // jej wagi czyta się wtedy raz, a nie raz na linię.
        self.logits_gemm(&bb.logits, &bb.x, super::caps::head_batch_width(n), stream)
    }

    /// Capture `record_batch_forward(bucket)` into a replayable graph.
    pub(crate) fn capture_batch_forward(&self, bucket: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.record_batch_forward(bucket, bucket, &[]) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }
}
