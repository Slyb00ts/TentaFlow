// ===== File: model/arch/dense.rs — gesty blok transformera =====
use super::super::*;

fn pre_residual_norm(
    kernels: &Kernels,
    norm: Option<&DevBuffer>,
    delta: &DevBuffer,
    rows: usize,
    hidden: usize,
    eps: f32,
    stream: &Stream,
) -> Result<()> {
    if let Some(w) = norm {
        kernels.rmsnorm_f16(delta, delta, w, rows, hidden, eps, stream)?;
    }
    Ok(())
}

impl Model {
    /// Nieliniowość bramkowanego FFN tego modelu.
    pub(crate) fn ffn_act(&self) -> forge_formats::FfnActivation {
        self.weights.descriptor.params.ffn_activation
    }

    /// True when `w` can be consumed by the fused decode kernels
    /// (gemv_norm / gemv_norm_silu / gemv_residual format + column coverage).
    fn fused_decode_weight_ok(w: &DevWeight) -> bool {
        match w {
            DevWeight::Fp8Row { .. } => false,
            DevWeight::F16 { cols, .. } => cols.is_multiple_of(8),
            DevWeight::Q8_0 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::NvFp4 {
                storage: NvFp4CtStorage::RowMajorE4M3 { .. },
                cols,
                ..
            } => cols.is_multiple_of(16),
            DevWeight::NvFp4 {
                storage: NvFp4CtStorage::S0N64K128 { .. },
                cols,
                ..
            } => cols.is_multiple_of(128),
            DevWeight::NvFp4Gguf { .. } => false,
            // Q4_K stages per-32-column x sums in shared memory
            // (Q4K_MAX_SEGS in gemv2.mojo bounds cols at 32768).
            DevWeight::Q4K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q6K { cols, .. } => cols.is_multiple_of(256),
            // Q5_K shares Q4_K's 32-column x-sum staging bound; Q2_K stages
            // 16-column sums with the same 32768 ceiling.
            DevWeight::Q5K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q3K { cols, .. } => cols.is_multiple_of(256),
            DevWeight::Q2K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q4_0 { cols, .. }
            | DevWeight::Q4_1 { cols, .. }
            | DevWeight::Q5_0 { cols, .. }
            | DevWeight::Q5_1 { cols, .. }
            | DevWeight::Iq4Nl { cols, .. }
            | DevWeight::Mxfp4 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::Iq4Xs { cols, .. }
            | DevWeight::Iq2Xs { cols, .. }
            | DevWeight::Iq2S { cols, .. }
            | DevWeight::Iq3S { cols, .. }
            | DevWeight::Iq2Xxs { cols, .. }
            | DevWeight::Iq3Xxs { cols, .. }
            | DevWeight::Iq1S { cols, .. }
            | DevWeight::Iq1M { cols, .. } => cols.is_multiple_of(256),
        }
    }

    /// The fused decode step carries the residual stream as an (h, h32)
    /// pair with no standalone normed-x buffer and needs a hidden size that
    /// fits the kernels' shared-memory staging. QKV and gate/up may stay
    /// split (mixed formats, e.g. Q4_K q/k + Q6_K v, or Q5_K gate + Q6_K
    /// up): each projection then runs its own gemv_norm launch — same
    /// per-row math, only the norm recompute is repeated (gate/up adds an
    /// elementwise silu_mul). Anything else records the separate chain.
    pub(crate) fn fused_decode_supported(&self) -> bool {
        // Łańcuch scalony liczy `attn_o` i `ffn_down` przez `gemv_residual`,
        // czyli dokłada rezyduum W TYM SAMYM kernelu co projekcję. Pod podziałem
        // wynik tej projekcji jest dopiero sumą CZĄSTKOWĄ, więc rezyduum wolno
        // dodać dopiero po redukcji — ranga idzie łańcuchem rozdzielonym, który
        // ma te dwa kroki osobno.
        self.tp_partial.is_none()
            && Self::fused_decode_available(&self.weights, self.device.caps().vendor)
    }

    pub(crate) fn fused_decode_available(weights: &ModelWeights, vendor: forge_types::Vendor) -> bool {
        let p = &weights.descriptor.params;
        // Kernele `gemv_norm_*` przeliczaja norme w KAZDEJ grupie roboczej i sa
        // strojone pod NVIDIA. Na gfx1030 profiler pokazal 182,95 us na wywolanie
        // dla projekcji FFN Mistrala (33 MB, czyli 181 GB/s), podczas gdy zwykly
        // GEMV na tej samej karcie robi 466 GB/s. Rozdzielenie normy i GEMV dalo
        // tam 67,2 -> 78,6 tok/s, a na Qwen3 286,6 -> 315,2. Dlatego poza NVIDIA
        // idzie sciezka rozdzielna.
        if vendor != forge_types::Vendor::Nvidia {
            return false;
        }
        if p.hidden_size > 8192 {
            return false;
        }
        // Naprzemienna geometria uwagi (Gemma 4: warstwy okienne 256/8 głowic i
        // globalne 512/1, dwie podstawy rope) nie da się wyrazić w fused
        // `qkv_post`, który zapieka jedną geometrię i jedną podstawę rope na całe
        // wywołanie. Takie modele idą ścieżką rozdzielną, liczącą wymiary per
        // warstwa.
        if p.alt_attn.is_some() {
            return false;
        }
        weights.layers.iter().all(|l| {
            // Routed MoE FFN has no fused single-GEMV decode kernel; MoE models
            // take the dedicated routed path (never this fused chain).
            let LayerFfn::Dense(dffn) = &l.ffn else {
                return false;
            };
            let qkv_ok = match &l.attn().attn_qkv {
                QkvWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                QkvWeights::FusedQk { qk, v } => {
                    Self::fused_decode_weight_ok(qk) && Self::fused_decode_weight_ok(v)
                }
                QkvWeights::Split { q, k, v } => {
                    Self::fused_decode_weight_ok(q)
                        && Self::fused_decode_weight_ok(k)
                        && Self::fused_decode_weight_ok(v)
                }
            };
            let gate_up_ok = match &dffn.gate_up {
                GateUpWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                // Mixed-format gate/up (e.g. Q5_K gate + Q6_K up) stays in
                // the fused chain: each projection runs its own gemv_norm
                // and a silu_mul combines them (see record_step_fused).
                GateUpWeights::Split { gate, up } => {
                    Self::fused_decode_weight_ok(gate) && Self::fused_decode_weight_ok(up)
                }
            };
            qkv_ok
                && gate_up_ok
                && Self::fused_decode_weight_ok(&l.attn().attn_o)
                && Self::fused_decode_weight_ok(&dffn.down)
        })
    }

    /// Batched GEMM over row-major activations x[t][col].
    /// Gęsty blok FFN: `out = down · act(gate·x, up·x)` dla `n_tokens` tokenów.
    ///
    /// JEDNA implementacja dla wszystkich ścieżek hybrydy — dekodowania,
    /// prefillu layer-major, weryfikacji draftu MTP i verifiera segmentowanego.
    /// Wcześniej ten sam blok był przepisany osobno w każdej z nich, przez co
    /// każda zmiana (a podział na karty w szczególności) wymagała wpięcia w
    /// każde miejsce z osobna i mogła cicho ominąć te, o których się zapomniało.
    pub(crate) fn ffn_dense_block(
        &self,
        layer: usize,
        ffn: &crate::weights::DenseFfn,
        bufs: FfnBlockBufs<'_>,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let hidden = self.weights.descriptor.params.hidden_size;
        let inter = self.weights.descriptor.params.intermediate_size;
        // Podział na karty wpina się TUTAJ — w jedynym miejscu, w którym ten blok
        // jest liczony. Dzięki temu obejmuje każdą ścieżkę hybrydy naraz i żadna
        // nie może go cicho ominąć.
        if let Some(tp) = &self.tp_ffn {
            let split = if n_tokens == 1 {
                tp.forward(stream, layer, bufs.x, bufs.out, self.ffn_act())?;
                true
            } else {
                tp.forward_batch(
                    stream,
                    layer,
                    n_tokens,
                    hidden,
                    self.ffn_act(),
                    bufs.x,
                    bufs.out,
                )?
            };
            if split {
                return Ok(());
            }
        }
        match &ffn.gate_up {
            GateUpWeights::Fused(weight) => match (n_tokens, bufs.gate_up) {
                // Jeden token ze scalonym `gate_up`: pojedyncza projekcja do
                // wspólnego bufora, a bramkowanie czyta obie połowy przez
                // przesunięcie.
                (1, Some(gate_up)) => {
                    self.gemv(gate_up, weight, bufs.x, stream)?;
                    self.kernels.glu_mul_f16_at(
                        self.ffn_act(),
                        bufs.act,
                        gate_up,
                        0,
                        inter * 2,
                        inter,
                        stream,
                    )?;
                }
                _ => {
                    self.gemm_rows(bufs.gate, weight, bufs.x, n_tokens, 0, inter, stream)?;
                    self.gemm_rows(bufs.up, weight, bufs.x, n_tokens, inter, inter, stream)?;
                    self.kernels.glu_mul_f16(
                        self.ffn_act(),
                        bufs.act,
                        bufs.gate,
                        bufs.up,
                        n_tokens * inter,
                        stream,
                    )?;
                }
            },
            GateUpWeights::Split { gate, up } => {
                if n_tokens == 1 {
                    // Obie projekcje czytają TĘ SAMĄ znormalizowaną aktywację,
                    // więc idą jednym uruchomieniem ze wspólną kwantyzacją.
                    let pair = [(bufs.gate, gate), (bufs.up, up)];
                    if !self.gemv_nvfp4_gguf_group(&pair, bufs.x, stream)?
                        && !self.gemv_q4_k_group(&pair, bufs.x, stream)?
                        && !self.gemv_mixed_group(&pair, bufs.x, stream)?
                    {
                        self.gemv(bufs.gate, gate, bufs.x, stream)?;
                        self.gemv(bufs.up, up, bufs.x, stream)?;
                    }
                } else {
                    self.gemm(bufs.gate, gate, bufs.x, n_tokens, stream)?;
                    self.gemm(bufs.up, up, bufs.x, n_tokens, stream)?;
                }
                self.kernels.glu_mul_f16(
                    self.ffn_act(),
                    bufs.act,
                    bufs.gate,
                    bufs.up,
                    n_tokens * inter,
                    stream,
                )?;
            }
        }
        // Dekodowanie ma własną rodzinę GEMV, prefill kafel GEMM — tak było i tak
        // musi zostać, bo to różne zaokrąglenie i różny wygenerowany tekst.
        if n_tokens == 1 {
            self.row_parallel_gemv(bufs.out, &ffn.down, bufs.act, stream)
        } else {
            self.row_parallel_gemm(bufs.out, &ffn.down, bufs.act, n_tokens, stream)
        }
    }

    /// Prefill modelu gęstego pod podziałem: token po tokenie, tą samą warstwą
    /// co dekodowanie.
    ///
    /// Batchowy prefill liczy warstwę własnym kodem, poza dwoma punktami
    /// redukcji, więc na randze z pociętymi wagami dałby wynik po cichu zły.
    /// Ta ścieżka jest wolniejsza, ale przechodzi przez `dense_decode_*`, czyli
    /// przez redukcje — i to jest ta sama decyzja, którą podjęła hybryda.
    pub(crate) fn prefill_dense_split(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        logits: SplitPrefillLogits,
    ) -> Result<Vec<f32>> {
        let p = self.weights.descriptor.params.clone();
        let vocab = p.vocab_size;
        let page_size = self.kv.cfg.page_size;
        let mut last_logits = Vec::new();
        for (i, &tok) in tokens.iter().enumerate() {
            let pos = seq.len;
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            let page_boundary = seq.len.is_multiple_of(page_size);
            self.kv.grow(seq)?;
            self.upload_decode_inputs(tok, pos)?;
            if page_boundary || self.pt_seq != seq.id {
                self.upload_page_table(seq)?;
            }
            let last = i + 1 == tokens.len();
            self.dense_forward_staged_tp(
                &AttnSrc::Paged,
                last && logits != SplitPrefillLogits::None,
            )?;
            if last && logits == SplitPrefillLogits::Host {
                self.device.copy(
                    &self.bufs.logits,
                    0,
                    &self.bufs.pinned_logits,
                    0,
                    vocab * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let lp = self
                    .bufs
                    .pinned_logits
                    .host_ptr()
                    .expect("pinned buffer has host mapping")
                    as *const f32;
                last_logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
            }
        }
        Ok(last_logits)
    }

    /// Prefill modelu gęstego liczy warstwę WŁASNYM kodem, poza dwoma punktami
    /// redukcji, więc na randze z pociętymi wagami dałby wynik po cichu zły.
    /// Sterownik go jeszcze nie prowadzi, więc jest to twarda odmowa.
    pub(crate) fn refuse_split_prefill(&self) -> Result<()> {
        match self.tp_partial {
            Some(_) => Err(ForgeError::Unsupported(
                "podział na rangi nie obejmuje jeszcze prefillu modelu gęstego".into(),
            )),
            None => Ok(()),
        }
    }

    pub(crate) fn prefill_forward(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        wait_for_completion: bool,
    ) -> Result<usize> {
        self.prefill_forward_lanes(&mut [seq], &[tokens], wait_for_completion, None)
    }

    pub(crate) fn prefill_forward_lanes(
        &mut self,
        seqs: &mut [&mut SeqKv],
        token_lanes: &[&[u32]],
        wait_for_completion: bool,
        mixed_decode: Option<&MixedDecodeRows>,
    ) -> Result<usize> {
        self.ensure_kv_reuse_healthy()?;
        let p = self.weights.descriptor.params.clone();
        let batch = seqs.len();
        if batch == 0 || token_lanes.len() != batch {
            return Err(ForgeError::Scheduler(
                "prefill wymaga tej samej liczby sekwencji i lane'ów tokenów".into(),
            ));
        }
        let n_tokens = token_lanes[0].len();
        if n_tokens == 0 {
            return Err(ForgeError::Scheduler("empty prefill chunk".into()));
        }
        if token_lanes.iter().any(|tokens| tokens.len() != n_tokens) {
            return Err(ForgeError::Scheduler(
                "batch prefill wymaga równej liczby tokenów w każdym lane".into(),
            ));
        }
        let t = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie batch prefill".into()))?;
        // Mixed step: `db` decode rows ride the chunk's GEMMs/norms at row
        // offset `t`. They stay RAW through the chunk's qk-norm/rope/append —
        // `attn_decode_split` folds its own norm + RoPE + paged append. The
        // caller uploaded their ids into `MixedDecodeRows` and their attention
        // metadata (page tables, seq_lens, positions) into the batch buffers.
        let db = mixed_decode.map_or(0, |m| m.b);
        if db > 0
            && (batch != 1
                || self.calib.is_some()
                || self.tier.is_some()
                || self.kv.cfg.quant.is_rot()
                || self.weights.is_moe())
        {
            return Err(ForgeError::Unsupported(
                "mixed prefill+decode wymaga pojedynczego gęstego lane bez rot/tier/kalibracji"
                    .into(),
            ));
        }
        let rows = t + db;
        if rows > MAX_PREFILL_CHUNK {
            return Err(ForgeError::Scheduler(format!(
                "prefill chunk {rows} exceeds MAX_PREFILL_CHUNK {MAX_PREFILL_CHUNK}"
            )));
        }
        if batch > 1 && !self.dense_prefill_batch_capable(batch, n_tokens) {
            return Err(ForgeError::Unsupported(
                "batch prefill nie spełnia pełnego kontraktu backendu, modelu i artefaktów".into(),
            ));
        }
        let base_pos = seqs[0].len;
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        let mut total_new_pages = 0usize;
        for seq in seqs.iter_mut() {
            let new_len = seq.len.checked_add(n_tokens).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie długości sekwencji prefill".into())
            })?;
            if new_len > p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    new_len - 1,
                    p.max_position_embeddings
                )));
            }
            self.tier_ensure_capacity(seq, n_tokens)?;
            let required_pages = new_len.div_ceil(self.kv.cfg.page_size);
            if required_pages > self.kv.cfg.max_pages_per_seq {
                return Err(ForgeError::Scheduler(format!(
                    "sequence requires {required_pages} KV pages, limit is {}",
                    self.kv.cfg.max_pages_per_seq
                )));
            }
            total_new_pages = total_new_pages
                .checked_add(required_pages.saturating_sub(seq.pages.len()))
                .ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie liczby stron batch prefill".into())
                })?;
        }
        self.ensure_free_pages(total_new_pages);
        if total_new_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "batch prefill wymaga {total_new_pages} stron KV, dostępnych jest {}",
                self.kv.free_page_count()
            )));
        }
        self.ensure_prefill_bufs()?;
        if batch > 1 {
            self.ensure_batch(batch)?;
        }
        grow_prefill_lanes_transactional(&mut self.kv, seqs, n_tokens)?;
        if self.tier.is_some() || self.prefix_cache.is_some() {
            for (seq, tokens) in seqs.iter_mut().zip(token_lanes.iter()) {
                if seq.tokens.len() == seq.prefilled_len {
                    seq.prefilled_len += n_tokens;
                }
                seq.tokens.extend_from_slice(tokens);
            }
        }

        let streamed = batch == 1 && !seqs[0].spilled.is_empty();
        if streamed {
            self.tier
                .as_mut()
                .expect("spilled pages imply tiering")
                .prepare_streaming(seqs[0])?;
        }
        let mut page_tables = vec![-1i32; batch * self.max_pages_per_seq];
        let mut base_positions = Vec::with_capacity(batch);
        let mut ids = Vec::with_capacity(t);
        let mut positions = Vec::with_capacity(t);
        for (lane, (seq, tokens)) in seqs.iter().zip(token_lanes.iter()).enumerate() {
            let table = &mut page_tables
                [lane * self.max_pages_per_seq..(lane + 1) * self.max_pages_per_seq];
            table[..seq.pages.len()].copy_from_slice(&seq.pages);
            base_positions.push(seq.len as i32 - n_tokens as i32);
            ids.extend(tokens.iter().map(|&id| id as i32));
            positions.extend((seq.len - n_tokens..seq.len).map(|position| position as i32));
        }
        let segmented = if batch == 1 {
            self.device
                .write(bytemuck::cast_slice(&page_tables), &self.page_table_dev, 0)?;
            self.pt_seq = seqs[0].id;
            None
        } else {
            let bb = self.batch_bufs.as_ref().expect("batch prefill ma bufory");
            self.device
                .write(bytemuck::cast_slice(&page_tables), &bb.page_table, 0)?;
            self.device
                .write(bytemuck::cast_slice(&base_positions), &bb.seq_lens, 0)?;
            self.pt_seq = 0;
            Some((bb.page_table.clone(), bb.seq_lens.clone()))
        };
        let mut ids = ids;
        if let Some(m) = mixed_decode {
            ids.extend_from_slice(&m.ids);
        }
        let pb = self.prefill_bufs.as_ref().expect("allocated above");
        self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
        self.device
            .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;

        // W4A8 SmoothQuant calibration: pull the accumulator out of `self` so
        // the per-layer captures can borrow it mutably alongside the immutable
        // `pb`/`device` borrows. Restored before return; `None` in normal runs.
        let mut calib = self.calib.take();

        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;

        // fp8mod fused-norm path: the Modular fp8 GEMM shares ONE per-token e4m3
        // activation across q/k/v (and across gate/up) by folding the activation
        // quant into the preceding RMSNorm. Only when the fp8 packs are loaded, the
        // Modular kernel is selected, and no W4A8 calibration is capturing mid-layer
        // f16 activations.
        //
        // Q4_K prefill takes the plain rmsnorm (f16) → per-projection
        // `quantize_act_q8_1` → native int8 GEMM sequence (`gemm_q4_k_i8mma_at`
        // routes to `gemm_q4k_i8_native`). The old fused rmsnorm→`block_q8_1_mmq`
        // path was an MMQ-only perf optimization (one shared DS4 activation across
        // q/k/v & gate/up); it was retired with the CUDA MMQ kernel. The
        // shared-activation reuse can be re-added on top of the native kernel later.
        let fp8mod_fuse = self.weights.fp8.is_some() && self.weights.fp8_modular && calib.is_none();
        let fp8mod_ffn_fuse =
            self.weights.fp8_ffn.is_some() && self.weights.fp8_modular && calib.is_none();

        let mut trace = PrefillTrace::new();
        trace.start(self.device.as_ref());

        // Etap pipeline'u, który nie zaczyna się od warstwy zerowej, dostaje
        // strumień rezydualny z poprzedniej karty — `pb.h` jest już wypełniony,
        // więc embeddingu się nie liczy. Granicą etapu jest właśnie `pb.h`, a
        // nie znormalizowane `pb.x`: następny etap normalizuje po swojemu.
        if self.stage_first_layer == 0 {
            kernels.gather_rows_f16(
                &pb.h,
                &self.weights.token_embd_f16,
                &pb.ids,
                rows,
                hidden,
                stream,
            )?;
        }
        // Rodzina Gemma mnoży embedding przez sqrt(hidden). Norma RMS jest na
        // to niewrażliwa, ale strumień rezydualny już nie — bez tego wyjście
        // jest ciche. Skalowanie dotyczy WYŁĄCZNIE świeżo pobranego embeddingu:
        // etap dalszy dostaje już przeskalowany rezydual i drugie mnożenie
        // rozjechałoby stan.
        if self.stage_first_layer == 0 {
            if let Some(factor) = p.embd_scale {
                kernels.scale_f16(&pb.h, rows * hidden, factor, stream)?;
            }
        }
        self.trace_f16("stage_in", &pb.h, (rows - 1) * hidden * 2, hidden);
        // Layer 0's attn-norm feeds the q/k/v projections.
        // Hybryda tez: odkad K/V sa w paczkach FP8, wszystkie trzy projekcje
        // czytaja te sama aktywacje, wiec kwantyzowanie jej osobno dla kazdej
        // bylo trzykrotna powtorka tej samej pracy.
        if fp8mod_fuse || fp8mod_ffn_fuse {
            kernels.rmsnorm_fp8_shared(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                rows,
                hidden,
                eps,
                stream,
            )?;
        } else {
            kernels.rmsnorm_f16(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                rows,
                hidden,
                eps,
                stream,
            )?;
        }
        self.trace_f16("embd", &pb.h, (rows - 1) * hidden * 2, hidden);
        self.trace_f16("attn_norm-0", &pb.x, (rows - 1) * hidden * 2, hidden);
        trace.mark(self.device.as_ref(), "embed");

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            // Geometria per warstwa: przy naprzemiennej uwadze szerokości
            // projekcji i offsety sekcji scalonego q|k|v różnią się między
            // warstwami, więc muszą być liczone tutaj, a nie raz na model.
            let head_dim = p.head_dim_at(l);
            let n_kv_heads = p.n_kv_heads_at(l);
            let scale = p.attn_scale_at(l);
            let q_dim = p.n_heads * head_dim;
            let kv_dim = n_kv_heads * head_dim;
            let layer = &self.weights.layers[l];

            // W4A8 prefill (non-default): each projection is its own logical
            // pack, so q/k/v are three standalone GEMMs. The Q4_K weights stay
            // loaded for decode + the logit head.
            let w4a8_layer = self.weights.w4a8.as_ref().map(|v| &v[l]);
            let fp8_layer = self.weights.fp8.as_ref().map(|v| &v[l]);
            let fp8_ffn_layer = self.weights.fp8_ffn.as_ref().map(|v| &v[l]);

            // Calibration capture 1/4: q/k/v input (attn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.attn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            // Prefill outputs must stay [T, dim] contiguous per projection
            // (attention/rope/append index (t*heads+h)*head_dim), so a fused
            // matrix is consumed as three row-window GEMMs into separate
            // buffers — same weight bytes, no second copy in VRAM.
            // Format wag i uklad QKV rozstrzygniete raz; dalej trzy identyczne
            // wywolania zamiast dwunastu kombinacji rozpisanych recznie.
            let projs = self.qkv_projections(
                layer,
                w4a8_layer,
                fp8_layer,
                fp8_ffn_layer,
                q_dim,
                kv_dim,
            );
            let shared_act = fp8mod_fuse || fp8mod_ffn_fuse;
            self.project(&pb.q, &projs[0], &pb.x, rows, shared_act, stream)?;
            self.project(&pb.k, &projs[1], &pb.x, rows, shared_act, stream)?;
            self.project(&pb.v, &projs[2], &pb.x, rows, shared_act, stream)?;

            if l == 0 {
                self.trace_f16("Qcur-0", &pb.q, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("Kcur-0", &pb.k, (rows - 1) * kv_dim * 2, kv_dim);
            }
            trace.mark(self.device.as_ref(), "gemm_qkv");

            // QK-norm granularity: OLMoE normalizes the whole q/k projection
            // once per token (rows = t), Qwen3 normalizes per head (rows =
            // t*n_heads). Dense non-OLMoE arches keep the per-head form
            // bit-for-bit (qk_norm_over_hidden == false).
            let attn_w = layer.attn();
            match (
                attn_w.q_norm.as_ref(),
                attn_w.k_norm.as_ref(),
                attn_w.v_norm.as_ref(),
            ) {
                // Wszystkie trzy normy per głowica: jedno uruchomienie zamiast
                // trzech (rodzina Gemma).
                (Some(qn), Some(kn), Some(vn)) if !p.qk_norm_over_hidden => {
                    kernels.rmsnorm_qkv_f16(
                        &pb.q,
                        &pb.k,
                        &pb.v,
                        qn,
                        kn,
                        vn,
                        t * p.n_heads,
                        t * n_kv_heads,
                        head_dim,
                        eps,
                        stream,
                    )?;
                }
                _ => {
                    if let Some(qn) = attn_w.q_norm.as_ref() {
                        if p.qk_norm_over_hidden {
                            kernels.rmsnorm_f16(&pb.q, &pb.q, qn, t, q_dim, eps, stream)?;
                        } else {
                            kernels.rmsnorm_f16(
                                &pb.q,
                                &pb.q,
                                qn,
                                t * p.n_heads,
                                head_dim,
                                eps,
                                stream,
                            )?;
                        }
                    }
                    if let Some(kn) = attn_w.k_norm.as_ref() {
                        if p.qk_norm_over_hidden {
                            kernels.rmsnorm_f16(&pb.k, &pb.k, kn, t, kv_dim, eps, stream)?;
                        } else {
                            kernels.rmsnorm_f16(
                                &pb.k,
                                &pb.k,
                                kn,
                                t * n_kv_heads,
                                head_dim,
                                eps,
                                stream,
                            )?;
                        }
                    }
                    if let Some(vn) = attn_w.v_norm.as_ref() {
                        kernels.rmsnorm_f16(
                            &pb.v,
                            &pb.v,
                            vn,
                            t * n_kv_heads,
                            head_dim,
                            eps,
                            stream,
                        )?;
                    }
                }
            }

            kernels.rope_neox_f16(
                &pb.q,
                &pb.positions,
                t,
                p.n_heads,
                head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            kernels.rope_neox_f16(
                &pb.k,
                &pb.positions,
                t,
                n_kv_heads,
                head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            if l == 0 {
                self.trace_f16("Qrope-0", &pb.q, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("Krope-0", &pb.k, (rows - 1) * kv_dim * 2, kv_dim);
            }
            trace.mark(self.device.as_ref(), "norm_rope");

            if let KvQuant::Rot { bits, .. } = self.kv.cfg.quant {
                // Rot: rotate+quant the chunk's rope'd K/V (linear pb.k/pb.v)
                // straight into the full-history packed store + residual ring —
                // no f16 slab. Packing must land before the attention launch,
                // which reads the packed store causally.
                let ring_slots = self
                    .kv
                    .cfg
                    .quant
                    .ring_slots()
                    .expect("rot mode has ring_slots");
                kernels.kv_pack_rot(
                    &self.kv.k_packed[self.target_kv_layer(l)],
                    &self.kv.v_packed[self.target_kv_layer(l)],
                    &self.kv.k_scale[self.target_kv_layer(l)],
                    &self.kv.v_scale[self.target_kv_layer(l)],
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &pb.k,
                    0,
                    &pb.v,
                    0,
                    &self.page_table_dev,
                    &pb.positions,
                    t,
                    n_kv_heads,
                    self.kv.cfg.page_size,
                    head_dim,
                    ring_slots,
                    bits,
                    stream,
                )?;
                trace.mark(self.device.as_ref(), "kv_pack_rot");
                if streamed {
                    // The chunk's packed K/V just landed in resident tail
                    // pages; staging pulls the full logical history (spilled
                    // chunks + resident pages) so the causal attention sees
                    // every position through the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seqs[0], l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                } else {
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k_packed[self.target_kv_layer(l)],
                        &self.kv.v_packed[self.target_kv_layer(l)],
                        &self.kv.k_scale[self.target_kv_layer(l)],
                        &self.kv.v_scale[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            } else {
                // Causal attention reads the chunk's own K/V from the cache, so
                // the batch append must land before the attention launch.
                if let Some((page_tables, base_positions)) = &segmented {
                    kernels.kv_append_batch_segmented_f16(
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &pb.k,
                        &pb.v,
                        page_tables,
                        base_positions,
                        batch,
                        n_tokens,
                        self.max_pages_per_seq,
                        n_kv_heads,
                        self.kv.cfg.page_size,
                        head_dim,
                        stream,
                    )?;
                } else {
                    kernels.kv_append_batch(
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &pb.k,
                        &pb.v,
                        &self.page_table_dev,
                        base_pos,
                        t,
                        n_kv_heads,
                        self.kv.cfg.page_size,
                        head_dim,
                        self.kv.cfg.dtype(),
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "kv_append");
                if streamed {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seqs[0], l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        self.attn_window(l),
                        stream,
                    )?;
                } else if let Some((page_tables, base_positions)) = &segmented {
                    // FA HD128 stoi na `mma` NVIDII. Karty bez tego artefaktu
                    // liczą ten sam segment przenośnym kaflem — ta sama
                    // matematyka, inny kernel.
                    if head_dim == 128
                        && kernels
                            .artifacts()
                            .has("attn_prefill_fa_segmented_f16_hd128")
                    {
                        kernels.attn_prefill_fa_segmented_f16_hd128(
                            &pb.attn_out,
                            &pb.q,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            page_tables,
                            base_positions,
                            batch,
                            n_tokens,
                            p.n_heads,
                            n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_prefill_segmented_tiled_f16(
                            &pb.attn_out,
                            &pb.q,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            page_tables,
                            base_positions,
                            batch,
                            n_tokens,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    }
                } else {
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        self.attn_window(l),
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            }

            if let Some(m) = mixed_decode {
                // Decode rows: RAW q/k/v at row offset `t` — the fused split
                // kernel applies qk-norm + RoPE, appends each row's K/V to its
                // own sequence and attends over its pages (batch metadata
                // uploaded by `mixed_prefill_decode_step`).
                let bb = self
                    .batch_bufs
                    .as_ref()
                    .expect("mixed step ma batch bufory");
                kernels.attn_decode_split(
                    &bb.attn_parts,
                    &pb.q,
                    t * q_dim * 2,
                    &pb.k,
                    t * kv_dim * 2,
                    &pb.v,
                    t * kv_dim * 2,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &bb.page_table,
                    &bb.seq_lens,
                    &bb.positions,
                    m.b,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
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
                    m.b,
                    p.n_heads,
                    head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
                self.device.copy(
                    &bb.attn_out,
                    0,
                    &pb.attn_out,
                    t * q_dim * 2,
                    m.b * q_dim * 2,
                    stream,
                )?;
            }

            // Calibration capture 2/4: o_proj input (attention output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.attn_out,
                    &mut cal.attn_out[l],
                    t,
                    &mut cal.scratch,
                )?;
            }
            if let Some(wl) = w4a8_layer {
                self.gemm_w4a8(&pb.o_out, &wl.attn_o, &pb.attn_out, rows, stream)?;
            } else if let Some(fl) = fp8_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, rows, stream)?;
            } else if let Some(fl) = fp8_ffn_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, rows, stream)?;
            } else {
                self.gemm(&pb.o_out, &layer.attn().attn_o, &pb.attn_out, rows, stream)?;
            }
            if l == 0 {
                self.trace_f16("attn_out-0", &pb.attn_out, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("kqv_out-0", &pb.o_out, (rows - 1) * hidden * 2, hidden);
            }
            trace.mark(self.device.as_ref(), "gemm_o");
            let fp8mod_fuse_gateup =
                (fp8mod_fuse || fp8mod_ffn_fuse) && matches!(layer.ffn, LayerFfn::Dense(_));
            if fp8mod_fuse_gateup {
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            } else {
                close_block(
                    kernels,
                    layer.post_attn_norm.as_ref(),
                    None,
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res");

            // Calibration capture 3/4: gate/up input (ffn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.ffn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            match &layer.ffn {
                LayerFfn::Dense(dffn) => {
                    if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.gate, &wl.gate, &pb.x, rows, stream)?;
                        self.gemm_w4a8(&pb.up, &wl.up, &pb.x, rows, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        if fp8mod_fuse {
                            self.gemm_fp8_prequant(&pb.gate, &fl.gate, rows, stream)?;
                            self.gemm_fp8_prequant(&pb.up, &fl.up, rows, stream)?;
                        } else {
                            self.gemm_fp8(&pb.gate, &fl.gate, &pb.x, rows, stream)?;
                            self.gemm_fp8(&pb.up, &fl.up, &pb.x, rows, stream)?;
                        }
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8_prequant(&pb.gate, &fl.gate, rows, stream)?;
                        self.gemm_fp8_prequant(&pb.up, &fl.up, rows, stream)?;
                    } else {
                        match &dffn.gate_up {
                            GateUpWeights::Fused(w) => {
                                self.gemm_rows(&pb.gate, w, &pb.x, rows, 0, inter, stream)?;
                                self.gemm_rows(&pb.up, w, &pb.x, rows, inter, inter, stream)?;
                            }
                            GateUpWeights::Split { gate, up } => {
                                self.gemm(&pb.gate, gate, &pb.x, rows, stream)?;
                                self.gemm(&pb.up, up, &pb.x, rows, stream)?;
                            }
                        }
                    }
                    trace.mark(self.device.as_ref(), "gemm_gateup");
                    // SwiGLU + kwantyzacja jednym kernelem, gdy wynik i tak
                    // trafia do projekcji FP8: wtedy bufor posredni [T, inter]
                    // nie jedzie trzy razy przez HBM. Kalibracja czyta `pb.act`,
                    // wiec przy niej zostaje stara para kerneli.
                    let fused_act = fp8mod_ffn_fuse
                        && calib.is_none()
                        && matches!(self.ffn_act(), forge_formats::FfnActivation::SiLU)
                        && kernels.silu_mul_quant_fp8(&pb.gate, &pb.up, inter, rows, stream)?;
                    if !fused_act {
                        kernels.glu_mul_f16(
                            self.ffn_act(),
                            &pb.act,
                            &pb.gate,
                            &pb.up,
                            rows * inter,
                            stream,
                        )?;
                    }
                    trace.mark(self.device.as_ref(), "silu");
                    // Calibration capture 4/4: down_proj input (SwiGLU output).
                    if let Some(cal) = calib.as_mut() {
                        self.device.synchronize()?;
                        CalibAccum::absorb(
                            self.device.as_ref(),
                            &pb.act,
                            &mut cal.down_in[l],
                            t,
                            &mut cal.scratch,
                        )?;
                    }
                    if fused_act {
                        let fl = fp8_ffn_layer.expect("fused_act wymaga paczek FP8");
                        self.gemm_fp8_prequant(&pb.down, &fl.down, rows, stream)?;
                    } else if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.down, &wl.down, &pb.act, rows, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, rows, stream)?;
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, rows, stream)?;
                    } else {
                        self.gemm(&pb.down, &dffn.down, &pb.act, rows, stream)?;
                    }
                    trace.mark(self.device.as_ref(), "gemm_down");
                }
                LayerFfn::Moe(moe) => {
                    // Per-token routed experts written into pb.down [t, hidden].
                    self.moe_prefill_ffn(moe, l, t, hidden, stream)?;
                    trace.mark(self.device.as_ref(), "moe_ffn");
                }
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            // This norm feeds the NEXT layer's q/k/v (or, for the last layer, the
            // logit head — never fused, keeps the f16 hidden state).
            if l + 1 < n_layers && (fp8mod_fuse || fp8mod_ffn_fuse) {
                pre_residual_norm(
                    kernels,
                    layer.post_ffw_norm.as_ref(),
                    &pb.down,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x, &pb.h, &pb.down, next_norm, rows, hidden, eps, stream,
                )?;
                layer_output_scale(
                    kernels,
                    layer.layer_output_scale,
                    &pb.h,
                    rows * hidden,
                    stream,
                )?;
            } else {
                close_block(
                    kernels,
                    layer.post_ffw_norm.as_ref(),
                    layer.layer_output_scale,
                    &pb.x,
                    &pb.h,
                    &pb.down,
                    next_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res2");
            self.trace_f16(
                &format!("l_out-{l}"),
                &pb.h,
                (rows - 1) * hidden * 2,
                hidden,
            );
        }

        if wait_for_completion || tier_t0.is_some() {
            self.synchronize_kv_fatal("dense prefill forward")?;
        }
        if let (Some(tier), Some(t0)) = (&self.tier, tier_t0) {
            // Measured prefill rate feeds the transfer-vs-recompute estimate.
            tier.note_prefill(t, t0.elapsed().as_secs_f64());
        }
        trace.report(t);
        self.calib = calib;
        Ok(t)
    }

    /// Przepuszcza chunk przez warstwy TEGO etapu i zatrzymuje się na granicy —
    /// bez głowy logitów.
    ///
    /// Etap zerowy pobiera embedding sam; etap dalszy oczekuje, że wołający
    /// wpisał już rezydual poprzedniej karty do `stage_hidden`. Tokeny podaje
    /// się każdemu etapowi, bo poza embeddingiem wyznaczają pozycje RoPE i
    /// dopisanie do KV. Zwraca liczbę wierszy chunka.
    pub fn prefill_stage(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<usize> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "etap pipeline'u obsługuje na razie wyłącznie model dense".into(),
            ));
        }
        self.prefill_forward(seq, tokens, true)
    }

    /// Wykonuje pośredni dense prefill bez głowy logits i opróżnia stream przed
    /// ponownym użyciem współdzielonych buforów przez następny chunk.
    pub fn prefill_chunk_device_sync(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        if self.tp.is_some() && !self.is_hybrid() {
            self.prefill_dense_split(seq, tokens, SplitPrefillLogits::None)?;
            return self.stream.synchronize();
        }
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "device-only prefill chunk obsługuje wyłącznie model dense".into(),
            ));
        }
        self.profile_target_start()?;
        self.prefill_forward(seq, tokens, true)?;
        self.profile_target_end()
    }

    /// Sprawdza pełny kontrakt równego dense prefill dla kubełka B4/B8/B16.
    pub fn dense_prefill_batch_capable(&self, batch: usize, n_tokens: usize) -> bool {
        let logits = match &self.weights.lm_head {
            DevWeight::F16 { rows, cols, .. } => DensePrefillLogitsKind::F16 {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q8_0 { rows, cols, .. } => DensePrefillLogitsKind::Q8_0 {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::NvFp4Gguf {
                layout: Nvfp4GgufLayout::RowMajor36,
                rows,
                cols,
                ..
            } => DensePrefillLogitsKind::NvFp4Gguf {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q4K { rows, cols, .. } => DensePrefillLogitsKind::Q4K {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q6K { rows, cols, .. } => DensePrefillLogitsKind::Q6K {
                rows: *rows,
                cols: *cols,
            },
            _ => return false,
        };
        let head_dim = self.weights.descriptor.params.head_dim;
        matches!(batch, 4 | 8 | 16)
            && n_tokens > 0
            && batch
                .checked_mul(n_tokens)
                .is_some_and(|total| total <= MAX_PREFILL_CHUNK)
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && self.tier.is_none()
            && self.kv.cfg.dtype() == DType::F16
            && self
                .kernels
                .dense_prefill_batch_capable(head_dim, batch, logits)
    }

    /// Sprawdza wszystkie kubełki wymagane przez wymuszony rollout schedulera.
    pub fn dense_prefill_rollout_capable(&self) -> bool {
        [4usize, 8, 16]
            .into_iter()
            .all(|batch| self.dense_prefill_batch_capable(batch, 1))
    }

    /// Wykonuje równy pośredni chunk bez głowy logitów i opróżnia stream.
    pub fn prefill_batch_device_sync(
        &mut self,
        seqs: &mut [&mut SeqKv],
        token_lanes: &[&[u32]],
    ) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        let batch = seqs.len();
        let n_tokens = token_lanes.first().map_or(0, |tokens| tokens.len());
        if !self.dense_prefill_batch_capable(batch, n_tokens) {
            return Err(ForgeError::Unsupported(
                "batch prefill nie spełnia kontraktu B4/B8/B16".into(),
            ));
        }
        run_dense_prefill_transaction(
            self,
            seqs,
            |model, seqs| {
                model.prefill_forward_lanes(seqs, token_lanes, true, None)?;
                Ok(())
            },
            |model| model.synchronize_kv_fatal("rollback dense prefill"),
            |model, seqs, snapshots| {
                restore_prefill_seq_snapshots(&mut model.kv, seqs, snapshots);
                model.pt_seq = 0;
            },
        )
    }

    pub(crate) fn upload_decode_inputs(&self, token_id: u32, pos: usize) -> Result<()> {
        let slot = self.claim_staging_slot()?;
        let offset = slot * STAGING_IN_BYTES;
        let host = self
            .bufs
            .pinned_in
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            let vals = [token_id as i32, pos as i32, (pos + 1) as i32];
            std::ptr::copy_nonoverlapping(vals.as_ptr() as *const u8, host.add(offset), 12);
        }
        self.device.copy(
            &self.bufs.pinned_in,
            offset,
            &self.bufs.ids,
            0,
            4,
            &self.stream,
        )?;
        self.device.copy(
            &self.bufs.pinned_in,
            offset + 4,
            &self.bufs.pos,
            0,
            4,
            &self.stream,
        )?;
        self.device.copy(
            &self.bufs.pinned_in,
            offset + 8,
            &self.seq_len_dev,
            0,
            4,
            &self.stream,
        )?;
        self.device
            .record_event(&self.staging_events[slot], &self.stream)?;
        for rank in self.tp_ranks() {
            rank.upload_decode_inputs(token_id, pos)?;
        }
        Ok(())
    }

    /// One decode step for the rotational KV modes. Mirrors the non-fused
    /// decode chain (explicit rmsnorm → qkv → norm/rope) but commits the
    /// appended token into the packed low-bit store + residual ring and reads
    /// it back through the split-K attn_decode_rot / attn_decode_combine_rot
    /// pair (rotate q once, score in rotated space, inverse-rotate the V
    /// accumulator). The pack kernel takes the position from `bufs.pos`, so the
    /// paged variant records cleanly into a CUDA graph. `src` selects the
    /// attention's store: the paged packed regions (captured) or the tier
    /// staging slabs carrying the sequence's full packed context per layer
    /// (streamed path, never captured; the residual ring is a global overlay
    /// and always reads in place).
    pub(crate) fn run_step_rot(&self, src: AttnSrc) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = p.attn_scale_at(0);
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;
        let bits = self.kv.cfg.quant.bits().expect("rot mode has bits");

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        let ring_slots = self
            .kv
            .cfg
            .quant
            .ring_slots()
            .expect("rot mode has ring_slots");
        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            // Gemma 4 zmienia geometrię głowic między warstwami okiennymi a globalnymi.
            let head_dim = p.head_dim_at(l);
            let n_kv_heads = p.n_kv_heads_at(l);
            let layer = &self.weights.layers[l];
            // Produce the rope'd q (attention query) plus the rope'd K/V as
            // LINEAR buffers so the pack kernel rotates them into the packed
            // store + residual ring. No paged f16 append (there is no f16 slab).
            // Returned tuple: (q_buf, q_off, k_src, k_off, v_src, v_off).
            let (q_buf, q_off, k_src, k_off, v_src, v_off): (
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
            ) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemv(&b.qkv, w, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels.rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv, k_byte_off, kn, n_kv_heads, head_dim, eps, stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemv(&b.qkv, qk, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels.rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv, k_byte_off, kn, n_kv_heads, head_dim, eps, stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.v, 0)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16(&b.k, &b.k, kn, n_kv_heads, head_dim, eps, stream)?;
                    }
                    kernels.rope_neox_f16(
                        &b.q,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16(
                        &b.k,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    if let Some(vn) = &layer.attn().v_norm {
                        kernels.rmsnorm_f16(&b.v, &b.v, vn, n_kv_heads, head_dim, eps, stream)?;
                    }
                    (&b.q, 0, &b.k, 0, &b.v, 0)
                }
            };

            // Rotate+quant the token into the packed store + residual ring, then
            // attend over the dual region (ring for the recent window, packed
            // for older). q_buf's q head occupies head_dim*n_heads at q_off.
            kernels.kv_pack_rot(
                &self.kv.k_packed[self.target_kv_layer(l)],
                &self.kv.v_packed[self.target_kv_layer(l)],
                &self.kv.k_scale[self.target_kv_layer(l)],
                &self.kv.v_scale[self.target_kv_layer(l)],
                &self.kv.k[self.target_kv_layer(l)],
                &self.kv.v[self.target_kv_layer(l)],
                k_src,
                k_off,
                v_src,
                v_off,
                &self.page_table_dev,
                &self.bufs.pos,
                1,
                n_kv_heads,
                self.kv.cfg.page_size,
                head_dim,
                ring_slots,
                bits,
                stream,
            )?;
            match &src {
                AttnSrc::Paged => {
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &self.kv.k_packed[self.target_kv_layer(l)],
                        &self.kv.v_packed[self.target_kv_layer(l)],
                        &self.kv.k_scale[self.target_kv_layer(l)],
                        &self.kv.v_scale[self.target_kv_layer(l)],
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                AttnSrc::Staged(seq) => {
                    // The pack above landed this token in the canonical packed
                    // store's resident tail page; staging materializes the full
                    // packed history (spilled chunks + resident pages) for this
                    // layer behind the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &tb.identity_pt,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
            }
            kernels.attn_decode_combine_rot(
                &b.attn_out,
                &b.attn_parts,
                1,
                p.n_heads,
                head_dim,
                ATTN_DECODE_SPLITS,
                stream,
            )?;

            self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
            close_block(
                kernels,
                layer.post_attn_norm.as_ref(),
                None,
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv(&b.gate_up, w, &b.x, stream)?;
                    kernels.glu_mul_f16_at(
                        self.ffn_act(),
                        &b.act,
                        &b.gate_up,
                        0,
                        inter * 2,
                        inter,
                        stream,
                    )?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv(&b.gate, gate, &b.x, stream)?;
                    self.gemv(&b.up, up, &b.x, stream)?;
                    kernels.glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
                }
            }
            self.gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            close_block(
                kernels,
                layer.post_ffw_norm.as_ref(),
                layer.layer_output_scale,
                &b.x,
                &b.h,
                &b.down,
                next_norm,
                1,
                hidden,
                eps,
                stream,
            )?;
        }

        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Czy podział umie policzyć chunk batchowy tego modelu.
    ///
    /// Warunkiem są WYŁĄCZNIE trzy macierze wierszowo równoległe: ich wynik jest
    /// sumą cząstkową, więc każda potrzebuje GEMM z wyjściem f32 dla dużego `T`.
    /// Format bez takiego kernela (dziś Q4_K i Q6_K) schodzi na prefill
    /// sekwencyjny — wolniejszy, ale liczący to samo.
    pub(crate) fn split_batch_prefill_capable(&self) -> bool {
        let partial_capable = |w: &DevWeight| {
            matches!(
                w,
                DevWeight::F16 { .. } | DevWeight::Q8_0 { .. } | DevWeight::NvFp4Gguf { .. }
            )
        };
        self.weights.layers.iter().all(|layer| {
            let mixer_ok = match &layer.mixer {
                LayerMixer::Attention(a) => partial_capable(&a.attn_o),
                LayerMixer::DeltaNet(d) => partial_capable(&d.out_proj),
                LayerMixer::DeepseekAttention(_) => false,
            };
            let ffn_ok = match &layer.ffn {
                LayerFfn::Dense(ffn) => partial_capable(&ffn.down),
                LayerFfn::Moe(_) => false,
            };
            mixer_ok && ffn_ok
        })
    }

    /// One decode step of the non-fused (separate-kernel) chain: explicit
    /// rmsnorm → qkv GEMVs → qkv_post (norm/rope/paged append) → attention →
    /// ffn. `src` selects the attention's K/V source: the paged cache
    /// (recorded into the replayable graph) or the tier staging slabs holding
    /// the sequence's full context per layer (streamed path, never captured).
    /// Część 1 warstwy gęstej: mikser do projekcji wyjściowej włącznie.
    pub(crate) fn dense_decode_mixer(&self, l: usize, src: &AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let layer = &self.weights.layers[l];
        // Geometria bywa różna per warstwa (Gemma 4), więc szerokości i
        // offsety sekcji scalonego q|k|v muszą być liczone W PĘTLI —
        // policzone raz dla całego modelu wskazywały poza bufor warstwy.
        let head_dim = p.head_dim_at(l);
        let n_kv_heads = p.n_kv_heads_at(l);
        let scale = p.attn_scale_at(l);
        let q_dim = p.n_heads * head_dim;
        let kv_dim = p.n_kv_heads_at(l) * head_dim;
        // Byte offsets of the K and V sections inside the fused q|k|v
        // decode buffer (q occupies rows 0..q_dim, so its offset is 0).
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;

        // Fused layers project q|k|v with ONE GEMV into one buffer,
        // then qkv_post fuses the whole q/k-norm + RoPE + kv-append
        // stretch into a second single launch (sections resolved via
        // host-computed byte offsets; rotated K lands directly in the
        // cache, so the K section of b.qkv is left un-rotated —
        // nothing reads it after this point).
        let q_buf = match &layer.attn().attn_qkv {
            QkvWeights::Fused(w) => {
                self.gemv(&b.qkv, w, &b.x, stream)?;
                kernels.qkv_post_f16(
                    &b.qkv,
                    0,
                    &b.qkv,
                    k_byte_off,
                    &b.qkv,
                    v_byte_off,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &b.pos,
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
                    self.kv.cfg.page_size,
                    eps,
                    p.rope_theta_at(l),
                    stream,
                )?;
                &b.qkv
            }
            QkvWeights::FusedQk { qk, v } => {
                // q|k land at the front of b.qkv (same section
                // offsets as the fully fused layout); v is projected
                // into its own buffer and handed to qkv_post by
                // pointer.
                self.gemv(&b.qkv, qk, &b.x, stream)?;
                self.gemv(&b.v, v, &b.x, stream)?;
                kernels.qkv_post_f16(
                    &b.qkv,
                    0,
                    &b.qkv,
                    k_byte_off,
                    &b.v,
                    0,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &b.pos,
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
                    self.kv.cfg.page_size,
                    eps,
                    p.rope_theta_at(l),
                    stream,
                )?;
                &b.qkv
            }
            QkvWeights::Split { q, k, v } => {
                self.gemv(&b.q, q, &b.x, stream)?;
                self.gemv(&b.k, k, &b.x, stream)?;
                self.gemv(&b.v, v, &b.x, stream)?;
                let aw = layer.attn();
                match (aw.q_norm.as_ref(), aw.k_norm.as_ref(), aw.v_norm.as_ref()) {
                    (Some(qn), Some(kn), Some(vn)) => {
                        kernels.rmsnorm_qkv_f16(
                            &b.q, &b.k, &b.v, qn, kn, vn, p.n_heads, n_kv_heads, head_dim,
                            eps, stream,
                        )?;
                    }
                    _ => {
                        if let Some(qn) = aw.q_norm.as_ref() {
                            kernels.rmsnorm_f16(
                                &b.q, &b.q, qn, p.n_heads, head_dim, eps, stream,
                            )?;
                        }
                        if let Some(kn) = aw.k_norm.as_ref() {
                            kernels.rmsnorm_f16(
                                &b.k, &b.k, kn, n_kv_heads, head_dim, eps, stream,
                            )?;
                        }
                        if let Some(vn) = aw.v_norm.as_ref() {
                            kernels.rmsnorm_f16(
                                &b.v, &b.v, vn, n_kv_heads, head_dim, eps, stream,
                            )?;
                        }
                    }
                }
                kernels.rope_neox_f16(
                    &b.q,
                    &b.pos,
                    1,
                    p.n_heads,
                    head_dim,
                    p.rope_theta_at(l),
                    self.rope_freqs_at(&p, l),
                    stream,
                )?;
                kernels.rope_neox_f16(
                    &b.k,
                    &b.pos,
                    1,
                    p.n_kv_heads_at(l),
                    head_dim,
                    p.rope_theta_at(l),
                    self.rope_freqs_at(&p, l),
                    stream,
                )?;
                kernels.kv_append_f16(
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &b.k,
                    &b.v,
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    n_kv_heads,
                    self.kv.cfg.page_size,
                    head_dim,
                    stream,
                )?;
                &b.q
            }
        };

        match &src {
            AttnSrc::Paged => {
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    q_buf,
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    1,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
            AttnSrc::Staged(seq) => {
                // qkv_post / kv_append above already committed the new
                // token to the canonical paged slab; staging picks it
                // up through the resident-page D2D copies.
                let tier = self
                    .tier
                    .as_ref()
                    .expect("staged attention requires tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    q_buf,
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    1,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
        }

        self.row_parallel_gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
        Ok(())
    }

    /// Część 2: rezyduum miksera, norma przed FFN i blok FFN do `down`.
    pub(crate) fn dense_decode_ffn(&self, l: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let layer = &self.weights.layers[l];
        close_block(
            kernels,
            layer.post_attn_norm.as_ref(),
            None,
            &b.x,
            &b.h,
            &b.o_out,
            &layer.ffn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        match &self.tp_ffn {
            Some(tp) => tp.forward(stream, l, &b.x, &b.down, self.ffn_act())?,
            None => {
                match &layer.dense_ffn()?.gate_up {
                    GateUpWeights::Fused(w) => {
                        self.gemv(&b.gate_up, w, &b.x, stream)?;
                        kernels.glu_mul_f16_at(
                            self.ffn_act(),
                            &b.act,
                            &b.gate_up,
                            0,
                            inter * 2,
                            inter,
                            stream,
                        )?;
                    }
                    GateUpWeights::Split { gate, up } => {
                        self.gemv(&b.gate, gate, &b.x, stream)?;
                        self.gemv(&b.up, up, &b.x, stream)?;
                        kernels.glu_mul_f16(
                            self.ffn_act(),
                            &b.act,
                            &b.gate,
                            &b.up,
                            inter,
                            stream,
                        )?;
                    }
                }
                self.row_parallel_gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;
            }
        }

        Ok(())
    }

    /// Część 3: rezyduum FFN i norma wejścia następnej warstwy.
    pub(crate) fn dense_decode_residual(&self, l: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let n_layers = self.weights.layers.len();
        let layer = &self.weights.layers[l];
        let next_norm = if l + 1 < n_layers {
            &self.weights.layers[l + 1].attn_norm
        } else {
            &self.weights.output_norm
        };
        close_block(
            kernels,
            layer.post_ffw_norm.as_ref(),
            layer.layer_output_scale,
            &b.x,
            &b.h,
            &b.down,
            next_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        Ok(())
    }

    pub(crate) fn run_step_separate(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;

        {
            kernels.gather_rows_f16(
                &b.h,
                &self.weights.token_embd_f16,
                &b.ids,
                1,
                hidden,
                stream,
            )?;
            // Skalowanie embeddingu (rodzina Gemma) — jak w prefillu.
            if let Some(factor) = p.embd_scale {
                kernels.scale_f16(&b.h, hidden, factor, stream)?;
            }
            kernels.rmsnorm_f16(
                &b.x,
                &b.h,
                &self.weights.layers[0].attn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            let n_layers = self.weights.layers.len();
            for l in 0..n_layers {
                self.dense_decode_mixer(l, &src)?;
                self.dense_decode_ffn(l)?;
                self.dense_decode_residual(l)?;
            }

            self.logits_gemv(&b.logits, &b.x, stream)
        }
    }

    pub(crate) fn decode_o_out(model: &Model) -> &DevBuffer {
        &model.bufs.o_out
    }

    pub(crate) fn decode_down(model: &Model) -> &DevBuffer {
        &model.bufs.down
    }

    pub(crate) fn prefill_o_out(model: &Model) -> &DevBuffer {
        &model
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe")
            .o_out
    }

    pub(crate) fn prefill_down(model: &Model) -> &DevBuffer {
        &model
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe")
            .down
    }

    pub(crate) fn run_step_fused(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = p.attn_scale_at(0);
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;

        self.trace_f16("embd", &b.h, 0, hidden);

        let n_layers = self.weights.layers.len();
        if let AttnSrc::Staged(seq) = &src {
            // Ping-pong staging: layer l+1 restores on the tier's transfer
            // stream while layer l computes. Both slots start "free" relative
            // to any prior compute work, and slot 0 prestages layer 0.
            let tier = self
                .tier
                .as_ref()
                .expect("staged attention requires tiering");
            let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
            let xfer = tier.xfer_stream();
            for slot in &tb.slots {
                self.device.record_event(&slot.free, stream)?;
            }
            self.device.wait_event(xfer, &tb.slots[0].free)?;
            tier.stage_layer(&self.kv, seq, 0, &tb.slots[0].stage, 0, xfer)?;
            self.device.record_event(&tb.slots[0].ready, xfer)?;
        }
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Fused QKV projects with one gemv_norm into the fused buffer;
            // split layers (mixed formats) run one gemv_norm per projection —
            // per-row math is identical, only the block-level norm recompute
            // repeats. Both feed attn_decode_split via buffer + byte offset.
            let (q_buf, q_off, k_buf, k_off, v_buf, v_off) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w_qkv) => {
                    self.gemv_norm(&b.qkv, w_qkv, &layer.attn_norm, l == 0, eps, stream)?;
                    if l == 0 {
                        self.trace_f16("Qcur-0", &b.qkv, 0, q_dim);
                    }
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    // The fused q|k rows land at the front of b.qkv, exactly
                    // where the Fused layout puts them; v goes to its own
                    // buffer.
                    self.gemv_norm(&b.qkv, qk, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.v, 0usize)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv_norm(&b.q, q, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.k, k, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.q, 0usize, &b.k, 0usize, &b.v, 0usize)
                }
            };
            let gqa_q_heads = p.n_kv_heads.checked_mul(4);
            let use_gqa = std::env::var("FORGE_ATTN_GQA").ok().as_deref() != Some("0")
                && self.device.caps().vendor == Vendor::Nvidia
                && kernels.supports_attn_decode_gqa4_f16_hd128()
                && self.kv.cfg.dtype() == forge_types::DType::F16
                && p.head_dim == 128
                && gqa_q_heads == Some(p.n_heads)
                && layer.attn().q_norm.is_none()
                && layer.attn().k_norm.is_none();
            let attn_splits = if use_gqa {
                ATTN_DECODE_GQA_SPLITS
            } else {
                ATTN_DECODE_SPLITS
            };
            match &src {
                AttnSrc::Paged => {
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
                AttnSrc::Staged(seq) => {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let xfer = tier.xfer_stream();
                    let s = l % STAGE_SLOTS;
                    // Prestage the NEXT layer into the other slot on the
                    // transfer stream while this layer computes.
                    if l + 1 < n_layers {
                        let ns = (l + 1) % STAGE_SLOTS;
                        self.device.wait_event(xfer, &tb.slots[ns].free)?;
                        tier.stage_layer(&self.kv, seq, l + 1, &tb.slots[ns].stage, ns, xfer)?;
                        self.device.record_event(&tb.slots[ns].ready, xfer)?;
                    }
                    self.device.wait_event(stream, &tb.slots[s].ready)?;
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
            }
            if use_gqa {
                kernels.attn_decode_combine_gqa2_f16_hd128(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    attn_splits,
                    stream,
                )?;
            } else {
                kernels.attn_decode_combine_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    attn_splits,
                    stream,
                )?;
            }
            if let AttnSrc::Staged(seq) = &src {
                // The kernel appended this token's rope'd K/V into the staging
                // tail page; mirror that page back into the canonical paged
                // cache so future steps (and spills) see it, then mark the
                // slot free for the transfer stream to restage.
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let s = l % STAGE_SLOTS;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &tb.slots[s].stage[0],
                    lp * rb,
                    &self.kv.k[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &tb.slots[s].stage[1],
                    lp * rb,
                    &self.kv.v[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.record_event(&tb.slots[s].free, stream)?;
            }
            self.gemv_residual(&layer.attn().attn_o, &b.attn_out, stream)?;
            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_norm_silu(&b.act, w, &layer.ffn_norm, eps, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    // Mixed-format gate/up: two gemv_norm launches (same
                    // per-row math as the fused silu kernels, the norm
                    // recompute repeats) + the elementwise SwiGLU combine.
                    // Rounding matches gemv_norm_silu: both projections are
                    // stored as f16 before silu_mul reads them.
                    self.gemv_norm(&b.gate, gate, &layer.ffn_norm, false, eps, stream)?;
                    self.gemv_norm(&b.up, up, &layer.ffn_norm, false, eps, stream)?;
                    kernels.glu_mul_f16(
                        self.ffn_act(),
                        &b.act,
                        &b.gate,
                        &b.up,
                        p.intermediate_size,
                        stream,
                    )?;
                }
            }
            self.gemv_residual(&layer.dense_ffn()?.down, &b.act, stream)?;
        }

        kernels.rmsnorm_h32_f16(
            &b.x,
            &b.h,
            &b.h32,
            &self.weights.output_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Run one batched decode step: advance every sequence in `seqs` by its
    /// input token in `tokens`, sampling each successor on the GPU with its own
    /// params. Returns the `B` next-token ids. The forward+logit head replays a
    /// per-bucket CUDA graph (dead lanes padded to the bucket, never sampled);
    /// sampling runs after the replay so per-seq params (and the greedy/top-k
    /// mix) need no re-capture.
    pub fn batched_decode(
        &mut self,
        seqs: &mut [&mut SeqKv],
        tokens: &[u32],
        params: &[SeqSampleParams],
    ) -> Result<Vec<u32>> {
        self.ensure_kv_reuse_healthy()?;
        let b = seqs.len();
        if b == 0 {
            return Ok(Vec::new());
        }
        if tokens.len() != b || params.len() != b {
            return Err(ForgeError::Scheduler(
                "batched_decode: seqs/tokens/params length mismatch".into(),
            ));
        }
        if self.is_hybrid() && !self.hybrid_batch_capable() {
            return Err(ForgeError::Unsupported(
                "hybrydowy batch nie spełnia kontraktu modelu lub pamięci KV".into(),
            ));
        }
        // Rot modes commit each appended token into the packed low-bit store on
        // the single-stream decode path only; the batched path would append to
        // the f16 slab without packing, leaving the packed store stale. Refuse
        // rather than read a stale store. (Batched rot decode is a follow-up.)
        if self.kv.cfg.quant.is_rot() {
            return Err(ForgeError::Unsupported(
                "rotational KV (rot4/rot3) supports single-stream decode only; \
                 disable batching for this model"
                    .into(),
            ));
        }
        // MoE routing chooses experts per token from a host readback, so the
        // batched forward cannot be graph-captured; MoE decodes one sequence at
        // a time (batched grouped-GEMM MoE is a tracked follow-up).
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "MoE models support single-stream decode only; disable batching".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        for (seq, &token) in seqs.iter().zip(tokens) {
            if seq.len >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    seq.len, p.max_position_embeddings
                )));
            }
            if token as usize >= p.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token id {token} exceeds model vocabulary {}",
                    p.vocab_size
                )));
            }
        }
        let growth_pages = self.kv.batch_growth_pages(seqs.iter().map(|seq| &**seq))?;
        // Batched growth appends pages without refreshing the single-stream
        // page table; invalidate it so the next single-stream step re-uploads.
        self.pt_seq = 0;
        if self.tier.is_some() {
            // Spilled sequences that fit back into free pages are restored
            // (plain fits-check, no reserve: restoring beats streaming when
            // possible); the rest stay streamed and join the batch through
            // the tier staging attention. The balance pass then guarantees a
            // free page per lane's potential boundary growth, spilling the
            // globally coldest prefixes — after it, lane residency is fixed.
            for seq in seqs.iter_mut() {
                if !seq.spilled.is_empty() && seq.spilled_page_count() <= self.kv.free_page_count()
                {
                    self.tier_restore_or_recompute(seq)?;
                }
            }
            self.tier_balance(seqs, b)?;
        }
        self.ensure_batch(b)?;
        // Streamed lanes (spilled KV) pack at the tail of the lane order: the
        // batch-wide paged attention launch covers exactly the leading
        // resident lanes, and each streamed lane attends over the staging
        // slabs. A mixed batch runs uncaptured at its exact size; a
        // pure-resident batch replays the per-bucket graph (dead lanes
        // padded).
        let mut order: Vec<usize> = (0..b).collect();
        order.sort_by_key(|&i| !seqs[i].spilled.is_empty());
        let resident = seqs.iter().filter(|s| s.spilled.is_empty()).count();
        let mixed = resident < b;
        let bucket = if mixed { b } else { self.bucket_for(b) };
        if b > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "batch {b} exceeds provisioned cap {}",
                self.batch_cap
            )));
        }

        // Reclaim cached prefix pages if the free stack cannot cover a boundary
        // page for every lane (no-op when the prefix cache is inactive/empty).
        self.ensure_free_pages(growth_pages);
        if growth_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "batch KV growth needs {growth_pages} pages, cache has {} free",
                self.kv.free_page_count()
            )));
        }
        if self.tier.is_some() {
            for (seq, &tok) in seqs.iter_mut().zip(tokens) {
                seq.tokens.push(tok);
            }
        }

        // Grow each sequence by one token and gather its position/page table
        // in lane order. Streamed lanes' page tables keep -1 for spilled
        // pages; only the identity-table staging path reads their context.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; bucket * 3]; // [ids | positions | seq_lens]
        let mut pt = vec![-1i32; bucket * mpp];
        for (lane, &i) in order.iter().enumerate() {
            let seq = &mut *seqs[i];
            let pos = seq.len;
            self.kv.grow(seq)?;
            meta[lane] = tokens[i] as i32;
            meta[bucket + lane] = pos as i32;
            meta[2 * bucket + lane] = (pos + 1) as i32;
            pt[lane * mpp..lane * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        // Dead lanes replay sequence 0's inputs so they compute harmlessly
        // (captured path only; the mixed path runs at its exact size).
        if !mixed {
            let lane0_pt: Vec<i32> = pt[..mpp].to_vec();
            for i in b..bucket {
                meta[i] = meta[0];
                meta[bucket + i] = meta[bucket];
                meta[2 * bucket + i] = meta[2 * bucket];
                pt[i * mpp..i * mpp + mpp].copy_from_slice(&lane0_pt);
            }
        }

        let bb = self.batch_bufs.as_ref().expect("provisioned above");
        // Upload meta (ids/positions/seq_lens) and the page table via pinned H2D.
        let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, bucket * 3 * 4);
        }
        self.device
            .copy(&bb.pinned_meta, 0, &bb.ids, 0, bucket * 4, &self.stream)?;
        self.device.copy(
            &bb.pinned_meta,
            bucket * 4,
            &bb.positions,
            0,
            bucket * 4,
            &self.stream,
        )?;
        self.device.copy(
            &bb.pinned_meta,
            2 * bucket * 4,
            &bb.seq_lens,
            0,
            bucket * 4,
            &self.stream,
        )?;
        let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, bucket * mpp * 4);
        }
        self.device.copy(
            &bb.pinned_pt,
            0,
            &bb.page_table,
            0,
            bucket * mpp * 4,
            &self.stream,
        )?;

        if self.is_hybrid() {
            if mixed {
                return Err(ForgeError::Unsupported(
                    "hybrydowy batch B2 nie obsługuje tieringu KV".into(),
                ));
            }
            self.record_hybrid_batch_forward(seqs, tokens)?;
            self.pt_seq = 0;
        } else if mixed {
            let tier = self.tier.as_mut().expect("mixed batch requires tiering");
            for &i in &order[resident..] {
                tier.prepare_streaming(seqs[i])?;
            }
            let streamed: Vec<(usize, &SeqKv)> = order[resident..]
                .iter()
                .enumerate()
                .map(|(j, &i)| (resident + j, &*seqs[i]))
                .collect();
            self.record_batch_forward(b, resident, &streamed)?;
        } else {
            // Replay the bucket's forward+logits graph (capture on first use).
            if !self.batch_graphs.contains_key(&bucket) {
                let g = self.capture_batch_forward(bucket)?;
                self.batch_graphs.insert(bucket, g);
            }
            let graph = self.batch_graphs.get(&bucket).expect("captured").clone();
            self.device.launch_graph(&graph, &self.stream)?;
        }

        // Sample the B live rows on the GPU (outside the graph so the per-seq
        // param mix is free), in lane order. Greedy-only batches take the
        // argmax fast path.
        let lane_params: Vec<SeqSampleParams> = order.iter().map(|&i| params[i].clone()).collect();
        let logits = self
            .batch_bufs
            .as_ref()
            .expect("provisioned")
            .logits
            .clone();
        self.batch_sample_from(&logits, b, &lane_params)?;

        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device
            .copy(&bb.out_ids, 0, &bb.pinned_out, 0, b * 4, &self.stream)?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(op, b) };
        let mut out = vec![0u32; b];
        for (lane, &i) in order.iter().enumerate() {
            let id = ids[lane];
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "batched sampler returned out-of-range token {id} for seq {i}"
                )));
            }
            out[i] = id as u32;
        }
        Ok(out)
    }

    /// Whether one scheduler iteration may fold `b` decode rows into a dense
    /// prefill chunk forward (`mixed_prefill_decode_step`).
    pub fn mixed_step_capable(&self, b: usize) -> bool {
        // The head runs at b(+1) rows — an arbitrary count, so it must be a
        // format whose batched logits path takes any row count (F16/Q8_0
        // GEMMs, Q4_K/Q6_K per-lane GEMV). NvFp4Gguf heads only have
        // power-of-two batch kernels and keep the two-phase iteration.
        let head_ok = matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. }
                | DevWeight::Q8_0 { .. }
                | DevWeight::Q4K { .. }
                | DevWeight::Q6K { .. }
        );
        b > 0
            && b <= self.batch_cap.max(1)
            && head_ok
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && !self.kv.cfg.quant.is_rot()
            && self.tier.is_none()
            && self.calib.is_none()
    }

    /// One mixed step: the `b` decode sequences' tokens ride the prefill
    /// chunk's GEMMs/norms as extra rows (decode attention runs through the
    /// fused split kernel over the batch metadata), so a long prompt no longer
    /// stalls decode. Returns the decode sequences' next tokens and — when the
    /// chunk completes the prompt — the prefill sequence's first token.
    #[allow(clippy::too_many_arguments)]
    pub fn mixed_prefill_decode_step(
        &mut self,
        decode_seqs: &mut [&mut SeqKv],
        decode_tokens: &[u32],
        decode_params: &[SeqSampleParams],
        prefill_seq: &mut SeqKv,
        chunk: &[u32],
        final_params: Option<SeqSampleParams>,
    ) -> Result<(Vec<u32>, Option<u32>)> {
        self.ensure_kv_reuse_healthy()?;
        let b = decode_seqs.len();
        if !self.mixed_step_capable(b) || chunk.is_empty() {
            return Err(ForgeError::Unsupported(
                "mixed step nie spełnia kontraktu modelu".into(),
            ));
        }
        if decode_tokens.len() != b || decode_params.len() != b {
            return Err(ForgeError::Scheduler(
                "mixed step: seqs/tokens/params length mismatch".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        for (seq, &token) in decode_seqs.iter().zip(decode_tokens) {
            if seq.len >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    seq.len, p.max_position_embeddings
                )));
            }
            if token as usize >= p.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token id {token} exceeds model vocabulary {}",
                    p.vocab_size
                )));
            }
            if !seq.spilled.is_empty() {
                return Err(ForgeError::Unsupported(
                    "mixed step nie obsługuje spillowanych sekwencji".into(),
                ));
            }
        }
        let growth_pages = self
            .kv
            .batch_growth_pages(decode_seqs.iter().map(|seq| &**seq))?;
        self.pt_seq = 0;
        self.ensure_batch(b)?;
        self.ensure_free_pages(growth_pages);
        if growth_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "mixed step: wzrost KV wymaga {growth_pages} stron, wolnych {}",
                self.kv.free_page_count()
            )));
        }

        // Grow each decode sequence by one token; upload ids into the mixed
        // rows and positions/seq_lens/page tables into the batch buffers the
        // fused decode attention reads.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; b * 2]; // [positions | seq_lens]
        let mut pt = vec![-1i32; b * mpp];
        let mut ids = Vec::with_capacity(b);
        for (lane, seq) in decode_seqs.iter_mut().enumerate() {
            let pos = seq.len;
            self.kv.grow(seq)?;
            ids.push(decode_tokens[lane] as i32);
            meta[lane] = pos as i32;
            meta[b + lane] = (pos + 1) as i32;
            pt[lane * mpp..lane * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        {
            // Pinned H2D like `batched_decode` — pageable `device.write`
            // staggers the stream with synchronous staging copies.
            let bb = self.batch_bufs.as_ref().expect("provisioned above");
            let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, b * 2 * 4);
            }
            self.device
                .copy(&bb.pinned_meta, 0, &bb.positions, 0, b * 4, &self.stream)?;
            self.device
                .copy(&bb.pinned_meta, b * 4, &bb.seq_lens, 0, b * 4, &self.stream)?;
            let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, b * mpp * 4);
            }
            self.device.copy(
                &bb.pinned_pt,
                0,
                &bb.page_table,
                0,
                b * mpp * 4,
                &self.stream,
            )?;
        }

        let mixed = MixedDecodeRows { b, ids };
        let t = self.prefill_forward_lanes(&mut [prefill_seq], &[chunk], false, Some(&mixed))?;

        // Logits: decode rows [t..t+b] (+ the chunk's last row when the prompt
        // completes) copied into the batch scratch, one GEMM, batched sampling.
        let hidden = p.hidden_size;
        let row_bytes = hidden * 2;
        let sample_rows = b + usize::from(final_params.is_some());
        {
            let pb = self.prefill_bufs.as_ref().expect("prefill bufs live");
            let bb = self.batch_bufs.as_ref().expect("provisioned above");
            self.device
                .copy(&pb.x, t * row_bytes, &bb.x, 0, b * row_bytes, &self.stream)?;
            if final_params.is_some() {
                self.device.copy(
                    &pb.x,
                    (t - 1) * row_bytes,
                    &bb.x,
                    b * row_bytes,
                    row_bytes,
                    &self.stream,
                )?;
            }
            let logits = bb.logits.clone();
            self.logits_gemm(&logits, &bb.x, sample_rows, &self.stream)?;
        }
        let mut lane_params: Vec<SeqSampleParams> = decode_params.to_vec();
        if let Some(fp) = final_params {
            lane_params.push(fp);
        }
        let logits = self
            .batch_bufs
            .as_ref()
            .expect("provisioned")
            .logits
            .clone();
        self.batch_sample_from(&logits, sample_rows, &lane_params)?;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device.copy(
            &bb.out_ids,
            0,
            &bb.pinned_out,
            0,
            sample_rows * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let raw = unsafe { std::slice::from_raw_parts(op, sample_rows) };
        let mut out = Vec::with_capacity(b);
        for (lane, &id) in raw.iter().take(b).enumerate() {
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "mixed sampler returned out-of-range token {id} for lane {lane}"
                )));
            }
            out.push(id as u32);
        }
        let final_id = if sample_rows > b {
            let id = raw[b];
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "mixed sampler returned out-of-range prompt token {id}"
                )));
            }
            Some(id as u32)
        } else {
            None
        };
        Ok((out, final_id))
    }

}
