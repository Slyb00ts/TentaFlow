// ===== File: model/arch/hybrid/core.rs — wspolna czesc sciezki hybrydowej =====
use super::super::super::*;

fn hybrid_layer_major_prefill_requested() -> bool {
    std::env::var("FORGE_HYBRID_LAYER_MAJOR_PREFILL").map_or(true, |value| value != "0")
}

fn hybrid_layer_major_attention() -> Result<HybridLayerMajorAttention> {
    match std::env::var("FORGE_HYBRID_LAYER_MAJOR_ATTN")
        .ok()
        .as_deref()
    {
        None | Some("fa") => Ok(HybridLayerMajorAttention::Flash),
        Some("exact") => Ok(HybridLayerMajorAttention::Exact),
        Some("prefill") => Ok(HybridLayerMajorAttention::Prefill),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_HYBRID_LAYER_MAJOR_ATTN wymaga exact/prefill/fa, otrzymano {value}"
        ))),
    }
}

impl Model {
    pub(crate) fn activate_hybrid_sequence(&mut self, seq: &mut SeqKv) -> Result<()> {
        let pool = self.hybrid_states.as_mut().ok_or_else(|| {
            ForgeError::Scheduler("model hybrydowy nie ma puli stanów DeltaNet".into())
        })?;
        let fresh = seq.hybrid_state.is_none();
        let lease = match seq.hybrid_state {
            Some(lease) => lease,
            None => {
                let lease = pool.acquire()?;
                seq.hybrid_state = Some(lease);
                lease
            }
        };
        pool.activate(lease, &self.stream)?;
        // Stan DeltaNet jest per ranga (ranga liczy własne głowice), ale slot i
        // generacja muszą być te same, bo lease jest jeden na sekwencję. Pule
        // rang są identyczne i dostają te same wołania, więc `acquire` wybiera
        // ten sam slot — sprawdzane, bo rozjazd dałby rangę liczącą na cudzym
        // stanie bez żadnego błędu.
        for index in 0..self.tp_rank_count() {
            let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
            let pool = rank.hybrid_states.as_mut().ok_or_else(|| {
                ForgeError::Scheduler("ranga podziału nie ma puli stanów DeltaNet".into())
            })?;
            if fresh {
                let mirrored = pool.acquire_exact_slot(lease.slot)?;
                pool.activate(mirrored, &rank.stream)?;
                continue;
            }
            let mirrored = pool.lease_for_slot(lease.slot)?;
            pool.activate(mirrored, &rank.stream)?;
        }
        // Borrowed prefix: the pages are already attached, and this is where the
        // recurrent half of that prefix lands. It runs after `activate`, whose
        // zeroing of a reused slot would otherwise wipe the checkpoint.
        self.restore_hybrid_checkpoint(seq)?;
        // Ten sam punkt obsługuje drugi kierunek: stan stoi teraz dokładnie na
        // `seq.len`, więc jeśli to granica strony, jest co utrwalić — niezależnie
        // od tego, która ścieżka (decode, batch, verify) po niego przyszła.
        self.roll_hybrid_checkpoint(seq)
    }

    pub(crate) fn active_ssm(&self) -> &[Option<SsmState>] {
        self.hybrid_states
            .as_ref()
            .expect("model hybrydowy ma pulę stanów")
            .active_layers()
    }

    pub(crate) fn delta_state_layout(&self) -> DeltaStateLayout {
        self.hybrid_states
            .as_ref()
            .expect("model hybrydowy ma pulę stanów")
            .layout()
    }

    pub(crate) fn delta_input_q8_cols(delta: &DeltaNetWeights) -> Option<usize> {
        let weights = [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj];
        let mut shared_cols = None;
        for weight in weights {
            let DevWeight::Q8_0 { cols, .. } = weight else {
                return None;
            };
            if shared_cols.is_some_and(|value| value != *cols) {
                return None;
            }
            shared_cols = Some(*cols);
        }
        shared_cols
    }

    /// Whether this is the hybrid attention/Gated-DeltaNet MoE arch (qwen35moe).
    pub fn is_hybrid(&self) -> bool {
        self.weights.descriptor.params.ssm.is_some()
    }

    /// Wariant uwagi dla layer-major, z zejściem na `Exact` gdy backend nie ma
    /// flash-attention.
    ///
    /// Domyślne `auto` wybiera Mojo FA HD256, ale ta rodzina stoi na `mma` i
    /// istnieje wyłącznie na NVIDII. Bez tego zejścia cała ścieżka layer-major
    /// przewracała się na `kernel not loaded` dopiero przy pierwszym żądaniu.
    /// Jawne `FORGE_HYBRID_LAYER_MAJOR_ATTN=fa` nadal jest błędem, jeśli
    /// artefaktu nie ma — prośba o konkretny wariant ma nie schodzić po cichu.
    pub(crate) fn hybrid_layer_major_attention_backend(&self) -> Result<HybridLayerMajorAttention> {
        let requested = hybrid_layer_major_attention()?;
        if requested == HybridLayerMajorAttention::Flash
            && std::env::var("FORGE_HYBRID_LAYER_MAJOR_ATTN").is_err()
            && !self.kernels.has_artifact("attn_prefill_fa_mojo_f16_hd256")
        {
            // Bez flash-attention schodzimy na wariant PREFILL, nie na `Exact`.
            // `Exact` liczy kernelem dekodowania, wiec caly chunk przechodzi
            // przez sciezke pisana pod jeden token: profil RX 7900 XT pokazal
            // 683 ms na 32 wywolania (13% calego prefillu), po 21 ms kazde.
            if self
                .kernels
                .has_artifact("attn_prefill_device_pos_f16_hd256")
            {
                return Ok(HybridLayerMajorAttention::Prefill);
            }
            return Ok(HybridLayerMajorAttention::Exact);
        }
        Ok(requested)
    }

    pub(crate) fn hybrid_layer_major_route_capable(&self) -> bool {
        hybrid_layer_major_prefill_requested()
            && self.hybrid_prefill_shape_capable()
            && self.kernels.hybrid_prefill_t128_artifacts_capable()
            && self.hybrid_layer_major_attention_backend().is_ok()
            && hybrid_layer_major_persistent_scan_requested().is_ok()
    }

    pub(crate) fn ensure_hybrid_layer_major_bufs(&mut self, cap: usize) -> Result<()> {
        if self
            .hybrid_layer_major_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= cap)
        {
            return Ok(());
        }
        if !self.hybrid_prefill_shape_capable() {
            return Err(ForgeError::Unsupported(
                "arena layer-major wymaga zweryfikowanego targetu hybrydowego NVIDIA".into(),
            ));
        }
        let shape = self.hybrid_prefill_scratch_shape().ok_or_else(|| {
            ForgeError::Unsupported("arena layer-major wymaga parametrów SSM".into())
        })?;
        let device_bytes = hybrid_layer_major_scratch_estimate(shape, cap)?;
        let required = hybrid_layer_major_activation_required(shape, cap)?;
        let available = self
            .device
            .pool_available(Pool::Activations)
            .ok_or_else(|| {
                ForgeError::Unsupported("backend nie raportuje budżetu areny layer-major".into())
            })?;
        if required > available {
            return Err(ForgeError::Unsupported(format!(
                "arena layer-major wymaga {required} bajtów, dostępne {available}"
            )));
        }
        drop(self.hybrid_layer_major_bufs.take());

        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let pinned = |name: &str, dims: &[usize], element_bytes: usize| {
            alloc_checked(
                device.as_ref(),
                name,
                dims,
                element_bytes,
                MemKind::PinnedHost,
            )
        };
        let shared_projection_cols = shape
            .q_dim
            .checked_mul(2)
            .map(|q| q.max(shape.conv_dim))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie projekcji areny layer-major".into())
            })?;
        let wide = shape.q_dim.max(shape.value_dim);
        if shape.inter < shared_projection_cols || wide < shape.hidden || wide < shape.kv_dim {
            return Err(ForgeError::Unsupported(
                "kształt areny layer-major nie pozwala współdzielić buforów fazowych".into(),
            ));
        }
        let conv_elems = shape
            .conv_dim
            .checked_mul(shape.d_conv - 1)
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie okna conv areny layer-major".into())
            })?;
        let h = a16("layer-major h", &[cap, shape.hidden])?;
        let x = a16("layer-major x", &[cap, shape.hidden])?;
        let v = a16("layer-major v", &[cap, shape.kv_dim])?;
        let gatec = a16("layer-major gatec i mixer", &[cap, wide])?;
        let gated = a16("layer-major gated i k", &[cap, wide])?;
        let z = a16("layer-major z", &[cap, shape.value_dim])?;
        let alpha = a16("layer-major alpha", &[cap, shape.n_v_heads])?;
        let beta_raw = a16("layer-major beta raw", &[cap, shape.n_v_heads])?;
        let g = a32("layer-major g", &[cap, shape.n_v_heads])?;
        let beta = a32("layer-major beta", &[cap, shape.n_v_heads])?;
        let o = a16("layer-major o i normed", &[cap, shape.value_dim])?;
        let gate = a16("layer-major q full, gate i act", &[cap, shape.inter])?;
        let up = a16("layer-major qc i up", &[cap, shape.inter])?;
        let q_full = gate.clone();
        let qc = up.clone();
        let k = gated.clone();
        let mixer_out = gatec.clone();
        let conv_initial = a16("layer-major conv initial", &[conv_elems])?;
        let conv_final = a16("layer-major conv final", &[conv_elems])?;
        let host_staging = (0..HYBRID_HOST_STAGING_SLOTS)
            .map(|_| {
                Ok(HybridLayerMajorHostStaging {
                    embedding: pinned("layer-major pinned embedding", &[128, shape.hidden], 2)?,
                    page_table: pinned(
                        "layer-major pinned page table",
                        &[shape.max_pages_per_seq],
                        4,
                    )?,
                    ids: pinned("layer-major pinned ids", &[128], 4)?,
                    positions: pinned("layer-major pinned positions", &[128], 4)?,
                    visible_lens: pinned("layer-major pinned visible lengths", &[128], 4)?,
                    base_pos: pinned("layer-major pinned base position", &[1], 4)?,
                    seq_len: pinned("layer-major pinned sequence length", &[1], 4)?,
                    position: pinned("layer-major pinned position", &[1], 4)?,
                    ready: device.create_event()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_layer_major_bufs = Some(HybridLayerMajorBufs {
            cap,
            device_bytes,
            h,
            x,
            k,
            v,
            q_full,
            qc,
            gatec,
            gated,
            z,
            alpha,
            beta_raw,
            g,
            beta,
            o,
            gate,
            up,
            mixer_out,
            conv_initial,
            conv_final,
            ids: a32("layer-major ids", &[cap])?,
            positions: a32("layer-major positions", &[cap])?,
            visible_lens: a32("layer-major visible lengths", &[cap])?,
            base_pos: a32("layer-major base position", &[1])?,
            host_staging,
        });
        Ok(())
    }

    /// NEOX partial-rotary width for the hybrid attention layers: M-RoPE over
    /// text positions rotates the first `2*Σ sections` dims of each head.
    pub(crate) fn hybrid_n_rot(&self) -> usize {
        let p = &self.weights.descriptor.params;
        p.rope_sections
            .map(|s| s.iter().sum::<u32>() as usize * 2)
            .unwrap_or(p.head_dim)
    }

    /// Allocate the hybrid single-token scratch (gated-attention de-interleave +
    /// DeltaNet conv/recurrence buffers) on first use.
    pub(crate) fn ensure_hybrid_bufs(&mut self) -> Result<()> {
        for index in 0..self.tp_rank_count() {
            let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
            rank.ensure_hybrid_bufs()?;
        }
        let rows = self.batch_cap.max(1);
        if self
            .hybrid_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.projection_rows >= rows)
        {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.clone().expect("hybrid model has ssm params");
        let q_dim = p.n_heads * p.head_dim;
        let q_full = q_dim * 2;
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let key_dim = ssm.key_dim();
        let nv = ssm.n_v_heads();
        let device = self.device.clone();
        let a16 = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let a32 = |elems: usize| device.alloc(elems * 4, MemKind::Device, Pool::Activations);
        self.hybrid_bufs = Some(HybridBufs {
            projection_rows: rows,
            batched_qkv_mixed: a16(rows * conv_dim)?,
            batched_z: a16(rows * value_dim)?,
            batched_alpha: a16(rows * nv)?,
            batched_beta_raw: a16(rows * nv)?,
            batched_q_full: a16(rows * q_full)?,
            q_full: a16(q_full)?,
            qc: a16(q_dim)?,
            gatec: a16(q_dim)?,
            gated: a16(q_dim)?,
            conv_out: a16(conv_dim)?,
            q16: a16(key_dim)?,
            k16: a16(key_dim)?,
            q32: a16(value_dim)?,
            k32: a16(value_dim)?,
            vtok: a16(value_dim)?,
            g: a32(nv)?,
            beta_f: a32(nv)?,
            o: a16(value_dim)?,
            normed: a16(rows * value_dim)?,
            pinned_embed: device.alloc(
                STAGING_SLOTS * p.hidden_size * 2,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
        });
        Ok(())
    }

    /// One token through the hybrid (gated-attention / Gated-DeltaNet + MoE)
    /// stack. Mirrors `run_step_moe`'s residual/norm skeleton, dispatching the
    /// token mixer by layer kind and folding in the gated shared expert. Inputs
    /// (`b.ids`/`b.pos`/`seq_len_dev`/page table) must be uploaded by the
    /// caller; the next-token logits land in `b.logits` when `want_logits`.
    /// Wstawia wiersz embeddingu tego tokena do bufora rezydualnego.
    ///
    /// Tablica embeddingu mieszka w RAM hosta (VRAM zostaje na wagi), więc
    /// wiersz idzie przez pamięć przypiętą i asynchroniczne H2D na strumieniu
    /// obliczeniowym. Kolejność strumienia serializuje to za ogonem poprzedniego
    /// tokena, więc nie trzeba blokującej synchronizacji.
    ///
    /// WYDZIELONE Z `hybrid_forward_token`, bo to JEDYNY krok kroku dekodowania
    /// zależny od `token_id` po stronie hosta — reszta czyta pozycję i długość
    /// sekwencji z buforów urządzenia. Dzięki temu reszta daje się przechwycić
    /// w graf i odtwarzać bez kosztu uruchamiania kerneli po kolei.
    pub(crate) fn stage_hybrid_embedding(&self, token_id: u32) -> Result<()> {
        let hidden = self.weights.descriptor.params.hidden_size;
        let host = self
            .weights
            .token_embd_host
            .as_ref()
            .expect("hybrid model has host embedding");
        let base = token_id as usize * hidden;
        let row = host.get(base..base + hidden).ok_or_else(|| {
            ForgeError::Scheduler(format!("token id {token_id} out of embedding range"))
        })?;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let slot = self.claim_staging_slot()?;
        let offset = slot * hidden * 2;
        let dst = hb
            .pinned_embed
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(row.as_ptr() as *const u8, dst.add(offset), hidden * 2);
        }
        self.device.copy(
            &hb.pinned_embed,
            offset,
            &self.bufs.h,
            0,
            hidden * 2,
            &self.stream,
        )?;
        self.device
            .record_event(&self.staging_events[slot], &self.stream)?;
        // Strumień rezydualny jest replikowany, więc embedding musi wylądować na
        // KAŻDEJ randze — inaczej rangi liczyłyby swoje fragmenty z różnych wejść.
        for rank in self.tp_ranks() {
            rank.stage_hybrid_embedding(token_id)?;
        }
        Ok(())
    }

    pub(crate) fn hybrid_forward_staged(&self, want_logits: bool, src: AttnSrc) -> Result<()> {
        if self.tp.is_some() {
            let AttnSrc::Paged = src else {
                return Err(ForgeError::Unsupported(
                    "podział na rangi nie obejmuje uwagi ze stagingu tieringu".into(),
                ));
            };
            return self.hybrid_forward_staged_tp(want_logits);
        }
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let n_layers = self.weights.layers.len();
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            self.hybrid_decode_layer(l, &src)?;

            if self.hybrid_debug {
                self.device.synchronize()?;
                let mut hb = vec![0u8; hidden * 2];
                self.device.read(&b.h, 0, &mut hb)?;
                let hf: &[f16] = bytemuck::cast_slice(&hb);
                let norm: f32 = hf.iter().map(|v| v.to_f32().powi(2)).sum::<f32>().sqrt();
                let kind = if matches!(self.weights.layers[l].mixer, LayerMixer::DeltaNet(_)) {
                    "delta"
                } else {
                    "attn"
                };
                eprintln!("  layer {l:2} [{kind}] ||h|| = {norm:.4}");
            }
        }

        if want_logits {
            self.logits_gemv(&b.logits, &b.x, stream)?;
        }
        Ok(())
    }

    /// Gated softmax-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. The Q
    /// projection is gated (`[q, gate]` interleaved per head), so q/gate are
    /// de-interleaved, per-head QK-norm + partial RoPE applied, causal decode
    /// attention run, then `out = attn ⊙ sigmoid(gate)` before the O projection.
    /// Projekcje Q/K/V warstwy uwagi dla WSZYSTKICH lane'ów naraz.
    ///
    /// Ten sam wniosek co przy wejściach DeltaNet: projekcje są bezstanowe, a
    /// liczone per lane każą karcie przeczytać wagi tyle razy, ile jest linii.
    pub(crate) fn hybrid_attn_projections(
        &self,
        a: &AttnWeights,
        x: &DevBuffer,
        n: usize,
    ) -> Result<()> {
        let QkvWeights::Split { q, k, v } = &a.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "hybrid attention expects split q/k/v weights".into(),
            ));
        };
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        self.gemm(&hb.batched_q_full, q, x, n, &self.stream)?;
        self.gemm(&bb.k, k, x, n, &self.stream)?;
        self.gemm(&bb.v, v, x, n, &self.stream)
    }

    pub(crate) fn hybrid_attn_mixer(&self, l: usize, a: &AttnWeights, src: &AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let head_dim = p.head_dim;
        let n_heads = p.n_heads;
        let n_kv = p.n_kv_heads;
        let q_dim = n_heads * head_dim;
        let eps = p.rms_norm_eps;
        let theta = p.rope_theta;
        let n_rot = self.hybrid_n_rot();
        let scale = p.attn_scale_at(l);
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let (wq, wk, wv) = match &a.attn_qkv {
            QkvWeights::Split { q, k, v } => (q, k, v),
            _ => {
                return Err(ForgeError::Unsupported(
                    "hybrid attention expects split q/k/v weights".into(),
                ))
            }
        };
        // Gated Q projection [2*q_dim], then de-interleave per head: q at
        // h*2*head_dim, gate at h*2*head_dim + head_dim.
        let triple = [(&hb.q_full, wq), (&b.k, wk), (&b.v, wv)];
        let qkv_grouped = self.gemv_nvfp4_gguf_group(&triple, &b.x, stream)?
            || self.gemv_q4_k_group(&triple, &b.x, stream)?
            || self.gemv_mixed_group(&triple, &b.x, stream)?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, wq, &b.x, stream)?;
            self.gemv(&b.k, wk, &b.x, stream)?;
            self.gemv(&b.v, wv, &b.x, stream)?;
        }
        self.hybrid_attn_mixer_from_qkv(l, a, src)
    }

    /// Druga połowa miksera uwagi: wszystko po projekcjach Q/K/V.
    ///
    /// Batch liczy te projekcje RAZ dla wszystkich lane'ów i wkłada wiersz
    /// swojej linii do jednotokenowego scratchu, więc wchodzi tutaj.
    pub(crate) fn hybrid_attn_mixer_from_qkv(
        &self,
        l: usize,
        a: &AttnWeights,
        src: &AttnSrc,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let head_dim = p.head_dim;
        let n_heads = p.n_heads;
        let n_kv = p.n_kv_heads;
        let q_dim = n_heads * head_dim;
        let eps = p.rms_norm_eps;
        let theta = p.rope_theta;
        let n_rot = self.hybrid_n_rot();
        let scale = p.attn_scale_at(l);
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        // Rozplecenie bramki, obie normy głowic i oba częściowe RoPE mieszczą
        // się w jednym uruchomieniu, gdy obie normy istnieją — same kernele
        // czytają po kilkadziesiąt kB, więc ich koszt to niemal wyłącznie
        // przestój, który karta płaci za każdą dyspozycję.
        match (&a.q_norm, &a.k_norm) {
            (Some(qn), Some(kn)) if kernels.attn_prepare_qk_capable(head_dim, n_rot) => {
                kernels.attn_prepare_qk_f16(
                    &hb.qc, &hb.gatec, &b.k, &hb.q_full, qn, kn, &b.pos, n_heads, n_kv, head_dim,
                    n_rot, theta, eps, stream,
                )?;
            }
            _ => {
                kernels.deinterleave_gate_f16(
                    &hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream,
                )?;
                if let Some(qn) = &a.q_norm {
                    kernels.rmsnorm_f16(&hb.qc, &hb.qc, qn, n_heads, head_dim, eps, stream)?;
                }
                if let Some(kn) = &a.k_norm {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, n_kv, head_dim, eps, stream)?;
                }
                kernels.rope_neox_partial_f16(
                    &hb.qc, &b.pos, 1, n_heads, head_dim, n_rot, theta, stream,
                )?;
                kernels
                    .rope_neox_partial_f16(&b.k, &b.pos, 1, n_kv, head_dim, n_rot, theta, stream)?;
            }
        }
        kernels.kv_append_f16(
            &self.kv.k[self.target_kv_layer(l)],
            &self.kv.v[self.target_kv_layer(l)],
            &b.k,
            &b.v,
            &self.page_table_dev,
            &self.seq_len_dev,
            n_kv,
            self.kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        match src {
            AttnSrc::Paged => {
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    &hb.qc,
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
            AttnSrc::Staged(seq) => {
                // Spilled sequence: kv_append above committed this token into
                // the resident tail of the canonical paged slab; staging then
                // materializes the FULL context for this attention layer (cold
                // pages streamed from RAM/NVMe, resident pages copied D2D) and
                // attention runs over it via the identity page table. Same
                // kernel + order as the paged path, so greedy tokens are
                // bit-identical to an untiered run.
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
                    &hb.qc,
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
        }
        // Output gate: out = attn ⊙ sigmoid(gate), applied on-device so the
        // whole mixer stays on the compute stream (no per-layer host sync).
        kernels.sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.row_parallel_gemv(&b.o_out, &a.attn_o, &hb.gated, stream)?;
        Ok(())
    }

    /// Gated-DeltaNet linear-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. Advances
    /// this layer's resident conv window + recurrent state by one token.
    /// Liczy cztery projekcje wejściowe DeltaNet dla `n_rows` wierszy `x` naraz.
    /// Są bezstanowe, więc wiersze mogą należeć do różnych sekwencji; jeden
    /// przebieg po wagach zastępuje `n_rows` przebiegów per lane. Trafia w tę
    /// samą rodzinę weight-stationary dp4a (`gemm_q8_0_i8mma_b*`) co ścieżka
    /// jednolane'owa, więc numeryka się nie zmienia.
    pub(crate) fn hybrid_delta_projections(
        &self,
        layer: usize,
        d: &DeltaNetWeights,
        x: &DevBuffer,
        n_rows: usize,
    ) -> Result<()> {
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        if n_rows > hb.projection_rows {
            return Err(ForgeError::Scheduler(format!(
                "projekcje DeltaNet: {n_rows} wierszy przekracza pojemność {}",
                hb.projection_rows
            )));
        }
        // Dwie duże projekcje mogą być rozłożone na karty — każda karta liczy
        // swój zakres wierszy OBU naraz, więc grupowanie zostaje po obu stronach.
        let split_big = match (&self.tp_ffn, n_rows) {
            (Some(tp), 1) => tp.forward_delta_projections(
                &self.stream,
                layer,
                x,
                &hb.batched_qkv_mixed,
                &hb.batched_z,
            )?,
            _ => false,
        };
        let mut projections: Vec<(&DevBuffer, &DevWeight)> = vec![
            (&hb.batched_alpha, &d.alpha_proj),
            (&hb.batched_beta_raw, &d.beta_proj),
        ];
        if !split_big {
            projections.insert(0, (&hb.batched_qkv_mixed, &d.in_proj));
            projections.insert(1, (&hb.batched_z, &d.gate_proj));
        }
        // Cztery projekcje czytają TEN SAM znormalizowany `x`, ale NIE MAJĄ tego
        // samego formatu: `in_proj` jest NVFP4, a bramka, alfa i beta Q8_0.
        // Dlatego grupujemy PER FORMAT — jednorodna próba na całej czwórce
        // odpadała i wszystkie cztery szły osobno. Osobno każda ma za małą
        // siatkę, żeby wypełnić kartę.
        if n_rows == 1 {
            let mut nvfp4: Vec<(&DevBuffer, &DevWeight)> = Vec::new();
            let mut q8: Vec<(&DevBuffer, &DevWeight)> = Vec::new();
            for &(y, w) in projections.iter() {
                match w {
                    DevWeight::NvFp4Gguf { .. } => nvfp4.push((y, w)),
                    DevWeight::Q8_0 { .. } => q8.push((y, w)),
                    _ => {
                        nvfp4.clear();
                        q8.clear();
                        break;
                    }
                }
            }
            // Jedna grupa na wszystkie cztery — najpierw jednorodna, a gdy
            // formaty się różnią, wariant mieszany.
            if nvfp4.is_empty()
                && (self.gemv_q4_k_group(&projections, x, &self.stream)?
                    || self.gemv_mixed_group(&projections, x, &self.stream)?)
            {
                return Ok(());
            }
            if nvfp4.len() + q8.len() == projections.len() {
                for (subset, is_nvfp4) in [(&nvfp4, true), (&q8, false)] {
                    if subset.is_empty() {
                        continue;
                    }
                    let fused = if is_nvfp4 {
                        self.gemv_nvfp4_gguf_group(subset, x, &self.stream)?
                    } else {
                        self.gemv_q8_0_group(subset, x, &self.stream)?
                    };
                    if !fused {
                        for &(y, w) in subset.iter() {
                            self.hybrid_project(y, w, x, 1)?;
                        }
                    }
                }
                return Ok(());
            }
        }
        for (y, w) in projections {
            self.hybrid_project(y, w, x, n_rows)?;
        }
        Ok(())
    }

    /// Jedna projekcja hybrydy: `gemv` dla pojedynczego wiersza, batchowy `gemm`
    /// dla wielu. Rozgałęzienie jest konieczne, bo ścieżka GEMM dla wag NVFP4
    /// GGUF odrzuca jeden token (`gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch`).
    fn hybrid_project(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_rows: usize,
    ) -> Result<()> {
        if n_rows == 1 {
            return self.gemv(y, w, x, &self.stream);
        }
        self.gemm(y, w, x, n_rows, &self.stream)
    }

    pub(crate) fn hybrid_delta_mixer(
        &self,
        l: usize,
        d: &DeltaNetWeights,
        lane: usize,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrid has ssm params");
        let eps = p.rms_norm_eps;
        let conv_dim = ssm.conv_dim();
        let d_conv = ssm.d_conv;
        let key_dim = ssm.key_dim();
        let value_dim = ssm.value_dim();
        let d_state = ssm.d_state;
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let rep = n_v / n_k;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let st = self.active_ssm()[l]
            .as_ref()
            .expect("DeltaNet layer has ssm state");

        // Projekcje wejściowe policzył `hybrid_delta_projections` dla wszystkich
        // lane'ów naraz. Konsumenci czytają swój wiersz przez przesunięcie
        // bajtowe, więc nie ma kopii do jednotokenowego scratchu.
        let qkv_off = lane * conv_dim * 2;
        let head_off = lane * n_v * 2;

        // Wstęp kroku — splot+SiLU, wycięcie v, normalizacje L2, powielenie GQA,
        // log-decay i beta — mieści się w jednym uruchomieniu, gdy geometria
        // pozwala podzielić pracę po głowicach K bez zależności między blokami.
        // Łańcuch poniżej robi ~9 us pracy na warstwę w siedmiu uruchomieniach,
        // a każde uruchomienie kosztuje jeszcze ~3,5 us przestoju.
        if kernels.deltanet_step_prepare_capable(d_state, n_k, n_v) {
            kernels.deltanet_step_prepare_f16(
                &hb.q32,
                &hb.k32,
                &hb.vtok,
                &hb.g,
                &hb.beta_f,
                &st.conv,
                &hb.batched_qkv_mixed,
                qkv_off,
                &d.conv1d,
                &hb.batched_alpha,
                head_off,
                &hb.batched_beta_raw,
                head_off,
                &d.dt_bias,
                &d.a,
                d_state,
                n_k,
                n_v,
                d_conv,
                eps,
                stream,
            )?;
            return self.hybrid_delta_mixer_tail(d, lane, st);
        }
        // Causal depthwise conv + SiLU (advances the conv window in place).
        kernels.deltanet_conv_silu_f16_at(
            &hb.conv_out,
            0,
            &st.conv,
            &hb.batched_qkv_mixed,
            qkv_off,
            &d.conv1d,
            conv_dim,
            d_conv,
            stream,
        )?;
        // Wyjście splotu niesie q, k i v jeden za drugim. Normalizacja czyta swój
        // wycinek przez przesunięcie bajtowe, a nie z osobnego bufora — dawne
        // trzy kopie D2D na warstwę były tylko po to, żeby zaczynać od zera.
        self.device.copy(
            &hb.conv_out,
            2 * key_dim * 2,
            &hb.vtok,
            0,
            value_dim * 2,
            stream,
        )?;
        // Per-head L2 norm on the key-head q/k (n_k heads over d_state).
        kernels.l2norm_heads_f16_at(&hb.q16, &hb.conv_out, 0, n_k, d_state, eps, stream)?;
        kernels.l2norm_heads_f16_at(
            &hb.k16,
            &hb.conv_out,
            key_dim * 2,
            n_k,
            d_state,
            eps,
            stream,
        )?;
        // Format GGUF przestawia tensory strony V do układu kafelkowego, więc
        // każda głowa V używa głowy K o indeksie `head % n_k`.
        kernels.deltanet_repeat_qk_f16(
            &hb.q32,
            &hb.k32,
            &hb.q16,
            &hb.k16,
            n_k * d_state,
            rep,
            stream,
        )?;
        // Per-head log-decay g = softplus(alpha + dt_bias)·a and beta gate.
        kernels.deltanet_log_decay_f32_at(
            &hb.g,
            0,
            &hb.batched_alpha,
            head_off,
            &d.dt_bias,
            &d.a,
            n_v,
            stream,
        )?;
        kernels.deltanet_beta_sigmoid_f32_at(
            &hb.beta_f,
            &hb.batched_beta_raw,
            head_off,
            n_v,
            stream,
        )?;
        self.hybrid_delta_mixer_tail(d, lane, st)
    }

    /// Rekurencja gated-delta, bramkowany RMSNorm wyjścia i projekcja wyjściowa
    /// — część kroku wspólna dla ścieżki scalonego wstępu i łańcucha kerneli.
    fn hybrid_delta_mixer_tail(
        &self,
        d: &DeltaNetWeights,
        lane: usize,
        st: &SsmState,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrid has ssm params");
        let eps = p.rms_norm_eps;
        let d_state = ssm.d_state;
        let n_v = ssm.n_v_heads();
        let z_off = lane * ssm.value_dim() * 2;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");

        // Rank-1 gated-delta recurrence (advances the state matrix in place).
        match self.delta_state_layout() {
            DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_inplace_f16(
                &hb.o, &st.state, &st.state, &hb.q32, &hb.k32, &hb.vtok, &hb.g, &hb.beta_f, 1, 1,
                n_v, stream,
            )?,
            DeltaStateLayout::KeyValue => kernels.deltanet_gated_step_f16(
                &hb.o, &st.state, &hb.q32, &hb.k32, &hb.vtok, &hb.g, &hb.beta_f, n_v, d_state,
                stream,
            )?,
        }
        // Output gated RMSNorm into this lane's row. The value-dim → hidden out
        // projection is NOT done here: `out_proj` is read once for the whole
        // group by `hybrid_delta_out_projection`, so a second lane costs
        // recurrence work but not another pass over the weight.
        kernels.deltanet_gated_rmsnorm_f16_at(
            &hb.normed,
            lane * ssm.value_dim() * 2,
            &hb.o,
            &hb.batched_z,
            z_off,
            &d.ssm_norm,
            n_v,
            d_state,
            eps,
            stream,
        )?;
        Ok(())
    }

    /// Value-dim → hidden output projection for a whole decode group: one pass
    /// over `out_proj` for `n_rows` lanes whose gated norms already sit in
    /// `hybrid_bufs.normed`.
    pub(crate) fn hybrid_delta_out_projection(
        &self,
        d: &DeltaNetWeights,
        out: &DevBuffer,
        n_rows: usize,
    ) -> Result<()> {
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        if n_rows == 1 {
            return self.row_parallel_gemv(out, &d.out_proj, &hb.normed, &self.stream);
        }
        self.hybrid_project(out, &d.out_proj, &hb.normed, n_rows)
    }

    pub(crate) fn checkpoint_hybrid_layer_major(
        &self,
        seq: &SeqKv,
    ) -> Result<HybridLayerMajorCheckpoint> {
        let verifier = self.hybrid_verify_bufs.as_ref().ok_or_else(|| {
            ForgeError::Scheduler("layer-major nie ma workspace verifiera do rollbacku".into())
        })?;
        let state_workspace = verifier
            .retained_state_checkpoints
            .as_ref()
            .ok_or_else(|| {
                ForgeError::Scheduler("layer-major nie ma retained checkpointów stanu".into())
            })?
            .clone();
        let conv_workspaces = verifier
            .delta
            .iter()
            .map(|cache| cache.as_ref().map(|cache| cache.conv_initial.clone()))
            .collect::<Vec<_>>();
        let state_bytes = self
            .active_ssm()
            .iter()
            .flatten()
            .next()
            .ok_or_else(|| ForgeError::Scheduler("layer-major nie ma stanu DeltaNet".into()))?
            .state
            .len();
        let delta_layers = self.active_ssm().iter().flatten().count();
        let kv_byte_offset = state_bytes.checked_mul(delta_layers).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie checkpointu stanów layer-major".into())
        })?;
        let kv_page_bytes = checked_scratch_bytes(
            "checkpoint strony KV layer-major",
            &[
                self.kv.cfg.n_kv_heads,
                self.kv.cfg.page_size,
                self.kv.cfg.head_dim,
            ],
            2,
        )?;
        let tail_page = if seq.len > 0 && !seq.len.is_multiple_of(self.kv.cfg.page_size) {
            let physical = *seq
                .pages
                .last()
                .ok_or_else(|| ForgeError::Scheduler("częściowy ogon KV nie ma strony".into()))?;
            Some(usize::try_from(physical).map_err(|_| {
                ForgeError::Unsupported("layer-major nie obsługuje spilled ogona KV".into())
            })?)
        } else {
            None
        };
        let kv_checkpoint_bytes = if tail_page.is_some() {
            checked_scratch_bytes(
                "checkpoint ogona KV layer-major",
                &[2, self.kv.k.len(), kv_page_bytes],
                1,
            )?
        } else {
            0
        };
        let required = kv_byte_offset
            .checked_add(kv_checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie workspace rollbacku layer-major".into())
            })?;
        if state_workspace.len() < required {
            return Err(ForgeError::Scheduler(format!(
                "retained checkpointy mają {} bajtów, rollback layer-major wymaga {required}",
                state_workspace.len()
            )));
        }
        let mut delta_index = 0usize;
        for (layer_index, state) in self.active_ssm().iter().enumerate() {
            let Some(state) = state else { continue };
            self.device.copy(
                &state.state,
                0,
                &state_workspace,
                delta_index * state_bytes,
                state_bytes,
                &self.stream,
            )?;
            let conv_workspace = conv_workspaces[layer_index].as_ref().ok_or_else(|| {
                ForgeError::Scheduler("warstwa DeltaNet nie ma checkpointu conv".into())
            })?;
            self.device.copy(
                &state.conv,
                0,
                conv_workspace,
                0,
                state.conv.len(),
                &self.stream,
            )?;
            delta_index += 1;
        }
        if let Some(physical) = tail_page {
            let source_offset = physical
                .checked_mul(kv_page_bytes)
                .ok_or_else(|| ForgeError::Scheduler("przepełnienie offsetu strony KV".into()))?;
            for layer in 0..self.kv.k.len() {
                for (kind, slab) in [&self.kv.k[layer], &self.kv.v[layer]]
                    .into_iter()
                    .enumerate()
                {
                    let destination_offset = kv_byte_offset + (2 * layer + kind) * kv_page_bytes;
                    self.device.copy(
                        slab,
                        source_offset,
                        &state_workspace,
                        destination_offset,
                        kv_page_bytes,
                        &self.stream,
                    )?;
                }
            }
        }
        Ok(HybridLayerMajorCheckpoint {
            base: seq.len,
            pages: seq.pages.clone(),
            tokens_len: seq.tokens.len(),
            prefilled_len: seq.prefilled_len,
            state_workspace,
            conv_workspaces,
            state_bytes,
            kv_byte_offset,
            kv_page_bytes,
            tail_page,
        })
    }

    pub(crate) fn rollback_hybrid_layer_major(
        &mut self,
        seq: &mut SeqKv,
        checkpoint: &HybridLayerMajorCheckpoint,
    ) -> Result<()> {
        let restore_result = (|| -> Result<()> {
            let mut delta_index = 0usize;
            for (layer_index, state) in self.active_ssm().iter().enumerate() {
                let Some(state) = state else { continue };
                self.device.copy(
                    &checkpoint.state_workspace,
                    delta_index * checkpoint.state_bytes,
                    &state.state,
                    0,
                    checkpoint.state_bytes,
                    &self.stream,
                )?;
                let conv_workspace = checkpoint.conv_workspaces[layer_index]
                    .as_ref()
                    .ok_or_else(|| {
                        ForgeError::Scheduler("rollback nie ma checkpointu conv".into())
                    })?;
                self.device.copy(
                    conv_workspace,
                    0,
                    &state.conv,
                    0,
                    state.conv.len(),
                    &self.stream,
                )?;
                delta_index += 1;
            }
            if let Some(physical) = checkpoint.tail_page {
                let destination_offset = physical
                    .checked_mul(checkpoint.kv_page_bytes)
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie offsetu rollbacku KV".into())
                    })?;
                for layer in 0..self.kv.k.len() {
                    for (kind, slab) in [&self.kv.k[layer], &self.kv.v[layer]]
                        .into_iter()
                        .enumerate()
                    {
                        let source_offset = checkpoint.kv_byte_offset
                            + (2 * layer + kind) * checkpoint.kv_page_bytes;
                        self.device.copy(
                            &checkpoint.state_workspace,
                            source_offset,
                            slab,
                            destination_offset,
                            checkpoint.kv_page_bytes,
                            &self.stream,
                        )?;
                    }
                }
            }
            self.device.synchronize()?;
            Ok(())
        })();
        self.kv.rollback(seq, checkpoint.base);
        seq.tokens.truncate(checkpoint.tokens_len);
        seq.prefilled_len = checkpoint.prefilled_len;
        self.pt_seq = 0;
        if seq.pages != checkpoint.pages {
            return Err(ForgeError::Scheduler(
                "rollback layer-major nie odtworzył mapy stron".into(),
            ));
        }
        restore_result
    }
}
