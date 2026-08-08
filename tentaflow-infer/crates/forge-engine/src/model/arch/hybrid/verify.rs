// ===== File: model/arch/hybrid/verify.rs — weryfikacja draftu na sciezce hybrydowej =====
use super::super::super::*;

impl Model {
    pub(crate) fn ensure_hybrid_verify_bufs(&mut self, cap: usize) -> Result<()> {
        for index in 0..self.tp_rank_count() {
            let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
            rank.ensure_hybrid_verify_bufs(cap)?;
        }
        if self
            .hybrid_verify_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= cap)
        {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p
            .ssm
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("target MTP nie jest hybrydowy".into()))?;
        let q_dim = p.n_heads * p.head_dim;
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let n_v = ssm.n_v_heads();
        let conv_elems = conv_dim
            .checked_mul(ssm.d_conv - 1)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie okna conv verifiera MTP".into()))?;
        let state_elems = n_v
            .checked_mul(ssm.d_state)
            .and_then(|value| value.checked_mul(ssm.d_state))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie stanu verifiera MTP".into()))?;
        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let base_pos = a32("mtp base position", &[1])?;
        let visible_lens = a32("mtp visible lengths", &[cap])?;
        let attn_parts = device.alloc(
            hybrid_verify_attention_parts_bytes(cap, p.n_heads, p.head_dim)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let q_full = a16(
            "mtp q full",
            &[cap, hybrid_q_full_cols(q_dim, conv_dim, p.hidden_size)],
        )?;
        let qc = a16("mtp q", &[cap, q_dim.max(value_dim)])?;
        let gatec = a16("mtp q gate", &[cap, q_dim.max(value_dim)])?;
        let gated = a16("mtp gated attention", &[cap, q_dim.max(value_dim)])?;
        let qkv_mixed = if cap > 4 {
            q_full.clone()
        } else {
            a16("mtp mixed qkv", &[cap, conv_dim])?
        };
        let z = a16("mtp z", &[cap, value_dim])?;
        let q32 = if cap > 4 {
            qc.clone()
        } else {
            a16("mtp q32", &[cap, value_dim])?
        };
        let k32 = if cap > 4 {
            gatec.clone()
        } else {
            a16("mtp k32", &[cap, value_dim])?
        };
        let vtok = if cap > 4 {
            gated.clone()
        } else {
            a16("mtp v", &[cap, value_dim])?
        };
        let alpha = a16("mtp alpha", &[cap, n_v])?;
        let beta_raw = a16("mtp beta raw", &[cap, n_v])?;
        let g = a32("mtp g", &[cap, n_v])?;
        let beta_f = a32("mtp beta", &[cap, n_v])?;
        let o = a16("mtp recurrence output", &[cap, value_dim])?;
        let normed = if cap > 4 {
            o.clone()
        } else {
            a16("mtp recurrence norm", &[cap, value_dim])?
        };
        let state_checkpoints = if cap > 4 {
            a32("mtp state checkpoints", &[1])?
        } else {
            a32("mtp state checkpoints", &[cap, state_elems])?
        };
        let accepted = a32("mtp accepted", &[2])?;
        let pinned_decision = alloc_checked(
            device.as_ref(),
            "mtp pinned decision",
            &[2],
            4,
            MemKind::PinnedHost,
        )?;
        let pinned = |name: &str, dims: &[usize], element_bytes: usize| {
            alloc_checked(
                device.as_ref(),
                name,
                dims,
                element_bytes,
                MemKind::PinnedHost,
            )
        };
        let host_staging = (0..HYBRID_HOST_STAGING_SLOTS)
            .map(|_| {
                Ok(HybridHostStaging {
                    embedding: pinned("mtp pinned embedding", &[cap, p.hidden_size], 2)?,
                    page_table: pinned("mtp pinned page table", &[self.max_pages_per_seq], 4)?,
                    ids: pinned("mtp pinned ids", &[cap], 4)?,
                    positions: pinned("mtp pinned positions", &[cap], 4)?,
                    visible_lens: pinned("mtp pinned visible lengths", &[cap], 4)?,
                    base_pos: pinned("mtp pinned base position", &[1], 4)?,
                    accepted: pinned("mtp pinned accepted", &[2], 4)?,
                    mtp_page_table: pinned(
                        "mtp pinned catch-up page table",
                        &[self.max_pages_per_seq],
                        4,
                    )?,
                    mtp_positions: pinned("mtp pinned catch-up positions", &[cap], 4)?,
                    mtp_visible_lens: pinned("mtp pinned catch-up visible lengths", &[cap], 4)?,
                    mtp_base_pos: pinned("mtp pinned catch-up base position", &[1], 4)?,
                    mtp_seq_len: pinned("mtp pinned catch-up sequence length", &[1], 4)?,
                    mtp_position: pinned("mtp pinned catch-up position", &[1], 4)?,
                    ready: device.create_event()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let has_delta = self
            .weights
            .layers
            .iter()
            .any(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)));
        let shared_delta_base = if cap > 4 && has_delta {
            Some((
                a16("mtp współdzielony conv initial", &[conv_elems])?,
                a16("mtp współdzielone conv checkpoints", &[cap, conv_elems])?,
                a32("mtp współdzielony state initial", &[1])?,
            ))
        } else {
            None
        };
        let delta_base = self
            .weights
            .layers
            .iter()
            .map(|layer| match &layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::DeltaNet(_) => {
                    let buffers = if let Some((conv_initial, conv_checkpoints, state_initial)) =
                        shared_delta_base.as_ref()
                    {
                        (
                            conv_initial.clone(),
                            conv_checkpoints.clone(),
                            state_initial.clone(),
                        )
                    } else {
                        (
                            a16("mtp conv initial", &[conv_elems])?,
                            a16("mtp conv checkpoints", &[cap, conv_elems])?,
                            a32("mtp state initial", &[state_elems])?,
                        )
                    };
                    Ok(Some(buffers))
                }
                LayerMixer::Attention(_) => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        let checkpoint_stride = cap
            .checked_mul(state_elems)
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie offsetu checkpointów MTP".into())
            })?;
        let retained_state_checkpoints = if cap <= 4 {
            Some(a32(
                "mtp retained state checkpoints",
                &[
                    self.weights
                        .layers
                        .iter()
                        .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
                        .count(),
                    cap,
                    state_elems,
                ],
            )?)
        } else {
            None
        };
        let retain_checkpoints = retained_state_checkpoints.is_some();
        let mut delta_index = 0usize;
        let delta = delta_base
            .into_iter()
            .map(|base| match base {
                Some((conv_initial, conv_checkpoints, state_initial)) => {
                    let checkpoint_byte_offset =
                        delta_index.checked_mul(checkpoint_stride).ok_or_else(|| {
                            ForgeError::Scheduler(
                                "przepełnienie offsetu warstwy DeltaNet MTP".into(),
                            )
                        })?;
                    delta_index += 1;
                    let commit = if cap > 4 {
                        DeltaVerifyCommit::InPlacePrefill
                    } else if retain_checkpoints {
                        DeltaVerifyCommit::Retained {
                            checkpoint_byte_offset,
                        }
                    } else {
                        DeltaVerifyCommit::Recompute {
                            q: a16("mtp delta q", &[cap, value_dim])?,
                            k: a16("mtp delta k", &[cap, value_dim])?,
                            v: a16("mtp delta v", &[cap, value_dim])?,
                            g: a32("mtp delta g", &[cap, n_v])?,
                            beta: a32("mtp delta beta", &[cap, n_v])?,
                        }
                    };
                    Ok(Some(DeltaVerifyCache {
                        commit,
                        conv_initial,
                        conv_checkpoints,
                        state_initial,
                    }))
                }
                None => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_verify_graphs.clear();
        self.hybrid_verify_graph_disabled.clear();
        self.hybrid_verify_bufs = Some(HybridVerifyBufs {
            cap,
            base_pos,
            visible_lens,
            attn_parts,
            q_full,
            qc,
            gatec,
            gated,
            qkv_mixed,
            z,
            q32,
            k32,
            vtok,
            alpha,
            beta_raw,
            g,
            beta_f,
            o,
            normed,
            state_checkpoints,
            retained_state_checkpoints,
            accepted,
            pinned_decision,
            host_staging,
            delta,
        });
        Ok(())
    }

    pub(crate) fn hybrid_verify_delta_layer(
        &self,
        layer_index: usize,
        delta: &DeltaNetWeights,
        t: usize,
        inplace_prefill: bool,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let d_state = ssm.d_state;
        let conv_elems = conv_dim * (ssm.d_conv - 1);
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let cache = hv.delta[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma cache verifiera");
        let state = self.active_ssm()[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");

        self.gemm(&hv.qkv_mixed, &delta.in_proj, &pb.x, t, stream)?;
        let mut prepared = Self::delta_input_q8_cols(delta)
            .filter(|_| matches!(t, 6 | 8 | 32 | 128))
            .map(|cols| self.kernels.prepare_q8_1(&pb.x, cols, t, stream))
            .transpose()?;
        let fused_q8_triplet = prepared.is_some() && matches!(t, 32 | 128);
        if let Some(prepared) = prepared.as_mut() {
            if fused_q8_triplet {
                self.gemm_q8_prepared_triplet(
                    [&hv.z, &hv.alpha, &hv.beta_raw],
                    [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj],
                    prepared,
                    t,
                )?;
            } else if !inplace_prefill {
                self.gemm_q8_prepared(&hv.z, &delta.gate_proj, prepared, t)?;
            }
            if !fused_q8_triplet {
                self.gemm_q8_prepared(&hv.alpha, &delta.alpha_proj, prepared, t)?;
                self.gemm_q8_prepared(&hv.beta_raw, &delta.beta_proj, prepared, t)?;
            }
        } else {
            if !inplace_prefill {
                self.gemm(&hv.z, &delta.gate_proj, &pb.x, t, stream)?;
            }
            self.gemm(&hv.alpha, &delta.alpha_proj, &pb.x, t, stream)?;
            self.gemm(&hv.beta_raw, &delta.beta_proj, &pb.x, t, stream)?;
        }

        self.device.copy(
            &state.conv,
            0,
            &cache.conv_initial,
            0,
            conv_elems * 2,
            stream,
        )?;
        kernels.deltanet_prepare_f16(
            &hv.q32,
            &hv.k32,
            &hv.vtok,
            &hv.g,
            &hv.beta_f,
            &cache.conv_checkpoints,
            &cache.conv_initial,
            &hv.qkv_mixed,
            &delta.conv1d,
            &hv.alpha,
            &hv.beta_raw,
            &delta.dt_bias,
            &delta.a,
            t,
            n_k,
            n_v,
            d_state,
            ssm.d_conv,
            p.rms_norm_eps,
            stream,
        )?;
        if inplace_prefill && !fused_q8_triplet {
            if let Some(prepared) = prepared.as_mut() {
                self.gemm_q8_prepared(&hv.z, &delta.gate_proj, prepared, t)?;
            } else {
                self.gemm(&hv.z, &delta.gate_proj, &pb.x, t, stream)?;
            }
        }
        drop(prepared);
        if inplace_prefill {
            match self.delta_state_layout() {
                DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_inplace_f16(
                    &hv.o,
                    &state.state,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    1,
                    t,
                    n_v,
                    stream,
                )?,
                DeltaStateLayout::KeyValue => kernels.deltanet_gated_scan_inplace_f16(
                    &hv.o,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    t,
                    n_v,
                    d_state,
                    stream,
                )?,
            }
        } else {
            let (state_checkpoints, checkpoint_byte_offset) = match &cache.commit {
                DeltaVerifyCommit::InPlacePrefill => {
                    return Err(ForgeError::Scheduler(
                        "scratch prefill wymaga skanu DeltaNet in-place".into(),
                    ));
                }
                DeltaVerifyCommit::Retained {
                    checkpoint_byte_offset,
                } => (
                    hv.retained_state_checkpoints
                        .as_ref()
                        .expect("retained checkpointy DeltaNet są zaalokowane"),
                    *checkpoint_byte_offset,
                ),
                DeltaVerifyCommit::Recompute { .. } => (&hv.state_checkpoints, 0),
            };
            match self.delta_state_layout() {
                DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_checkpoints_f16_at(
                    &hv.o,
                    state_checkpoints,
                    checkpoint_byte_offset,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    1,
                    t,
                    n_v,
                    stream,
                )?,
                DeltaStateLayout::KeyValue => kernels.deltanet_gated_scan_f16_at(
                    &hv.o,
                    state_checkpoints,
                    checkpoint_byte_offset,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    t,
                    n_v,
                    d_state,
                    stream,
                )?,
            }
            if let DeltaVerifyCommit::Recompute { q, k, v, g, beta } = &cache.commit {
                self.device
                    .copy(&hv.q32, 0, q, 0, t * value_dim * 2, stream)?;
                self.device
                    .copy(&hv.k32, 0, k, 0, t * value_dim * 2, stream)?;
                self.device
                    .copy(&hv.vtok, 0, v, 0, t * value_dim * 2, stream)?;
                self.device.copy(&hv.g, 0, g, 0, t * n_v * 4, stream)?;
                self.device
                    .copy(&hv.beta_f, 0, beta, 0, t * n_v * 4, stream)?;
            }
        }
        kernels.deltanet_gated_rmsnorm_f16(
            &hv.normed,
            &hv.o,
            &hv.z,
            &delta.ssm_norm,
            t * n_v,
            d_state,
            p.rms_norm_eps,
            stream,
        )?;
        self.row_parallel_gemm(&pb.o_out, &delta.out_proj, &hv.normed, t, stream)
    }

    /// Powyżej tylu zapytań uwagę liczy kafel zrównoleglony po TOKENACH.
    ///
    /// Kernel dekodowy dzieli KONTEKST między bloki i jest szybszy, dopóki
    /// zapytań jest za mało, żeby zapełnić kartę. Kafel prefillowy bierze
    /// szesnaście zapytań na blok, więc przy tej liczbie ma ich już więcej niż
    /// karta ma multiprocesorów, a każde dodatkowe zapytanie jest dla niego
    /// darmowe zamiast być kolejnym przebiegiem po kontekście.
    const HYBRID_ATTN_TOKEN_PARALLEL: usize = 64;

    pub(crate) fn hybrid_verify_attention_layer(
        &self,
        layer_index: usize,
        attention: &AttnWeights,
        t: usize,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&hv.q_full, q, &pb.x, t, stream)?;
        kernels.deinterleave_gate_f16(
            &hv.qc,
            &hv.gatec,
            &hv.q_full,
            p.head_dim,
            t * q_dim,
            stream,
        )?;
        if let Some(norm) = &attention.q_norm {
            kernels.rmsnorm_f16(
                &hv.qc,
                &hv.qc,
                norm,
                t * p.n_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        self.gemm(&pb.k, k, &pb.x, t, stream)?;
        self.gemm(&pb.v, v, &pb.x, t, stream)?;
        if let Some(norm) = &attention.k_norm {
            kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                t * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        let n_rot = self.hybrid_n_rot();
        kernels.rope_neox_partial_f16(
            &hv.qc,
            &pb.positions,
            t,
            p.n_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.kv_append_batch_device_pos_f16(
            &self.kv.k[self.target_kv_layer(layer_index)],
            &self.kv.v[self.target_kv_layer(layer_index)],
            &pb.k,
            &pb.v,
            &self.page_table_dev,
            &hv.base_pos,
            t,
            p.n_kv_heads,
            self.kv.cfg.page_size,
            p.head_dim,
            stream,
        )?;
        if self.kernels.attn_verify_split8_f16_hd256(
            &pb.attn_out,
            &hv.attn_parts,
            &hv.qc,
            &self.kv.k[self.target_kv_layer(layer_index)],
            &self.kv.v[self.target_kv_layer(layer_index)],
            &self.page_table_dev,
            &hv.visible_lens,
            t,
            p.n_heads,
            p.n_kv_heads,
            self.kv.cfg.page_size,
            self.max_pages_per_seq,
            1.0 / (p.head_dim as f32).sqrt(),
            stream,
        )? {
        } else if self.device.caps().vendor == Vendor::Nvidia && t < Self::HYBRID_ATTN_TOKEN_PARALLEL {
            kernels.attn_decode_batch_exact_f16_hd256(
                &pb.attn_out,
                &hv.qc,
                &self.kv.k[self.target_kv_layer(layer_index)],
                &self.kv.v[self.target_kv_layer(layer_index)],
                &self.page_table_dev,
                &hv.visible_lens,
                t,
                p.n_heads,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                1.0 / (p.head_dim as f32).sqrt(),
                stream,
            )?;
        } else {
            kernels.attn_prefill_device_pos_f16_hd256(
                &pb.attn_out,
                &hv.qc,
                &self.kv.k[self.target_kv_layer(layer_index)],
                &self.kv.v[self.target_kv_layer(layer_index)],
                &self.page_table_dev,
                &hv.base_pos,
                t,
                p.n_heads,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                1.0 / (p.head_dim as f32).sqrt(),
                stream,
            )?;
        }
        kernels.sigmoid_mul_f16(&hv.gated, &pb.attn_out, &hv.gatec, t * q_dim, stream)?;
        debug_assert_eq!(attention.attn_o.cols(), q_dim);
        debug_assert!(pb.k.len() >= t * kv_dim * 2);
        self.row_parallel_gemm(&pb.o_out, &attention.attn_o, &hv.gated, t, stream)
    }

    /// Zatwierdza na GPU stan odpowiadający zaakceptowanemu prefiksowi.
    fn run_hybrid_verify_postlude(&self, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let vb = self.verify_bufs.as_ref().expect("bufory verify są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        self.kernels
            .mtp_verify_decide(&hv.accepted, &vb.ids, &pb.ids, t, &self.stream)?;
        self.kernels.mtp_select_row_f16(
            &self.bufs.h,
            &pb.h,
            &hv.accepted,
            p.hidden_size,
            &self.stream,
        )?;
        self.kernels.mtp_select_row_f16(
            &self.bufs.x,
            &pb.x,
            &hv.accepted,
            p.hidden_size,
            &self.stream,
        )?;
        self.kernels.mtp_select_row_f32(
            &self.bufs.logits,
            &vb.logits,
            &hv.accepted,
            p.vocab_size,
            &self.stream,
        )?;

        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let conv_elems = ssm.conv_dim() * (ssm.d_conv - 1);
        for (layer_index, cache) in hv.delta.iter().enumerate() {
            let Some(cache) = cache else { continue };
            let state = self.active_ssm()[layer_index]
                .as_ref()
                .expect("warstwa DeltaNet ma stan");
            match &cache.commit {
                DeltaVerifyCommit::InPlacePrefill => {
                    return Err(ForgeError::Scheduler(
                        "scratch prefill nie może zatwierdzać verifiera MTP".into(),
                    ));
                }
                DeltaVerifyCommit::Retained {
                    checkpoint_byte_offset,
                } => self.kernels.deltanet_commit_checkpoint_f32_at(
                    &state.state,
                    hv.retained_state_checkpoints
                        .as_ref()
                        .expect("retained checkpointy DeltaNet są zaalokowane"),
                    *checkpoint_byte_offset,
                    &hv.accepted,
                    t,
                    ssm.n_v_heads(),
                    ssm.d_state,
                    &self.stream,
                )?,
                DeltaVerifyCommit::Recompute { q, k, v, g, beta } => {
                    match self.delta_state_layout() {
                        DeltaStateLayout::ValueKey => {
                            self.kernels.deltanet_value_key_commit_recompute_f32(
                                &state.state,
                                &state.state,
                                k,
                                v,
                                g,
                                beta,
                                &hv.accepted,
                                1,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?
                        }
                        DeltaStateLayout::KeyValue => {
                            self.kernels.deltanet_gated_scan_f16(
                                &hv.o,
                                &hv.state_checkpoints,
                                &state.state,
                                q,
                                k,
                                v,
                                g,
                                beta,
                                t,
                                ssm.n_v_heads(),
                                ssm.d_state,
                                &self.stream,
                            )?;
                            self.kernels.deltanet_commit_checkpoint_f32(
                                &state.state,
                                &hv.state_checkpoints,
                                &hv.accepted,
                                t,
                                ssm.n_v_heads(),
                                ssm.d_state,
                                &self.stream,
                            )?;
                        }
                    }
                }
            }
            self.kernels.mtp_select_row_f16(
                &state.conv,
                &cache.conv_checkpoints,
                &hv.accepted,
                conv_elems,
                &self.stream,
            )?;
        }
        self.device
            .copy(&hv.accepted, 0, &hv.pinned_decision, 0, 8, &self.stream)
    }

    /// Uruchamia stałą część verifiera hybrydowego bez synchronizacji z hostem.
    pub(crate) fn run_hybrid_verify_compute(&self, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.run_hybrid_batch_layers(t, false)?;

        let vb = self.verify_bufs.as_ref().expect("bufory verify są gotowe");
        self.logits_gemm(&vb.logits, &pb.x, t, &self.stream)?;
        self.kernels.sample_batched_argmax_f32(
            &vb.ids,
            &vb.logits,
            t,
            p.vocab_size,
            &self.stream,
        )?;
        self.run_hybrid_verify_postlude(t)
    }

    /// Przechwytuje rozgrzany łańcuch verifiera dla stałego T.
    fn capture_hybrid_verify_compute(&self, t: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.run_hybrid_verify_compute(t) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(error) => {
                let _ = self.device.end_capture(&self.stream);
                Err(error)
            }
        }
    }

    /// Po pierwszym wykonaniu eager zapisuje graf właściwy dla slotu i T.
    pub(crate) fn capture_hybrid_verify_graph_if_needed(&mut self, slot: usize, t: usize) {
        if std::env::var("FORGE_HYBRID_VERIFY_GRAPH").is_ok_and(|value| value == "0") {
            return;
        }
        if !self.device.caps().supports_graph_capture {
            return;
        }
        if !matches!(t, 3 | 4) {
            return;
        }
        let key = (slot, t);
        if self.hybrid_verify_graphs.contains_key(&key)
            || self.hybrid_verify_graph_disabled.contains(&key)
        {
            return;
        }
        match self.capture_hybrid_verify_compute(t) {
            Ok(captured) => {
                self.hybrid_verify_graphs.insert(key, captured);
            }
            Err(error) => {
                tracing::warn!(
                    "wyłączono capture grafu hybrid verifier slot={slot} T={t}: {error}"
                );
                self.hybrid_verify_graph_disabled.insert(key);
            }
        }
    }

}
