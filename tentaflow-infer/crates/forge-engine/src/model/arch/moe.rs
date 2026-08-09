// ===== File: model/arch/moe.rs — mieszanka ekspertow i ich rezydencja =====
use super::super::*;
use forge_kernels::GroupedTiles;

impl Model {
    /// Whether `stack` is a routed-expert stack the device-side grouped dispatch
    /// can index without a host readback: the dp4a Q4_K path (cols within the
    /// dp4a bound) and the warp-per-row Q6_K path have `_gidx` kernels that read
    /// the expert selection on-device. Other quants keep the host-readback loop.
    fn expert_stack_gidx(stack: &ExpertStack) -> bool {
        match stack.representative() {
            DevWeight::Q4K { cols, .. } => *cols <= Kernels::DP4A_MAX_COLS,
            DevWeight::Q6K { .. } | DevWeight::Mxfp4 { .. } => true,
            _ => false,
        }
    }

    /// True when every routed-expert projection of `moe` supports the
    /// device-indexed dispatch (so the whole layer runs with zero host readback).
    /// Warstwa ze stronicowanym ekspertem NIE kwalifikuje się: kernel `_gidx`
    /// nie ma jak zaadresować bloku, który leży na dysku, a stwierdzić tego
    /// przed uruchomieniem można tylko po odczycie wyboru routera na hoście.
    fn moe_gidx_capable(moe: &MoeFfn) -> bool {
        [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .all(|stack| Self::expert_stack_gidx(stack) && stack.fully_resident())
    }

    /// One decode step's worth of expert-residency upkeep.
    ///
    /// The router already tallies expert selections on-device for free, so the
    /// only cost here is the periodic round: read the tallies, refresh the
    /// popularity estimate, and move a bounded number of experts between VRAM
    /// and host memory. Rounds are rare and capped because the migration itself
    /// moves whole expert blocks — spending more on shuffling than the better
    /// placement returns would defeat the point.
    ///
    /// A model whose experts all fit in VRAM never has a host-resident expert,
    /// so `plan` returns nothing and this degenerates to a counter read.
    pub(crate) fn tick_moe_residency(&mut self) -> Result<()> {
        let Some(policy) = self.moe_residency.as_mut() else {
            return Ok(());
        };
        policy.tokens_since_round += 1;
        if policy.tokens_since_round < MOE_RESIDENCY_INTERVAL {
            return Ok(());
        }
        policy.tokens_since_round = 0;
        self.rebalance_moe_residency()
    }

    /// Ilu ekspertów siedzi w VRAM, a ilu w pamięci hosta, w całym modelu.
    /// `None` dla modeli bez routowanych ekspertów.
    pub fn moe_expert_residency(&self) -> Option<(usize, usize, usize)> {
        let mut vram = 0usize;
        let mut host = 0usize;
        let mut nvme = 0usize;
        let mut any = false;
        for layer in &self.weights.layers {
            let LayerFfn::Moe(moe) = &layer.ffn else {
                continue;
            };
            any = true;
            for stack in [&moe.gate_exps, &moe.up_exps, &moe.down_exps] {
                let (v, h, n) = stack.tier_counts();
                vram += v;
                host += h;
                nvme += n;
            }
        }
        any.then_some((vram, host, nvme))
    }

    fn rebalance_moe_residency(&mut self) -> Result<()> {
        // Tallies are written by kernels still queued on the model stream.
        self.stream.synchronize()?;
        let mut planned: Vec<Migration> = Vec::new();
        {
            let state = self
                .moe_residency
                .as_mut()
                .expect("residency state present for MoE models");
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                let LayerFfn::Moe(moe) = &layer.ffn else {
                    continue;
                };
                let counts = moe.usage.take(self.device.as_ref())?;
                state.policy.observe(layer_index, &counts);
                for (projection, stack) in [
                    (Projection::Gate, &moe.gate_exps),
                    (Projection::Up, &moe.up_exps),
                    (Projection::Down, &moe.down_exps),
                ] {
                    planned.extend(state.policy.candidates(
                        ProjectionId {
                            layer: layer_index,
                            projection,
                        },
                        stack,
                    ));
                }
            }
        }
        let planned = self
            .moe_residency
            .as_ref()
            .expect("residency state present")
            .policy
            .select_round(planned);
        if planned.is_empty() {
            return Ok(());
        }
        // The captured decode graph reads expert bases from the device-resident
        // pointer table, so a migration needs no re-capture — the table update
        // is picked up by the next replay.
        tracing::debug!(
            migrations = planned.len(),
            "runda rezydencji ekspertów: przenoszę do VRAM"
        );
        for migration in planned {
            let scratch = self
                .moe_residency
                .as_ref()
                .expect("residency state present")
                .scratch
                .clone();
            let LayerFfn::Moe(moe) = &self.weights.layers[migration.target.layer].ffn else {
                continue;
            };
            let stack = match migration.target.projection {
                Projection::Gate => &moe.gate_exps,
                Projection::Up => &moe.up_exps,
                Projection::Down => &moe.down_exps,
            };
            stack.promote_to_vram(
                self.device.as_ref(),
                migration.promote,
                migration.demote,
                &scratch,
                &self.stream,
            )?;
        }
        Ok(())
    }

    /// Whether every routed-MoE layer supports the device-side grouped dispatch
    /// (no host readback anywhere in the forward), so `run_step_moe` records
    /// cleanly into a replayable graph. False if any layer has a fallback quant
    /// (e.g. Q8_0 experts) that still needs a per-layer router readback.
    pub(crate) fn moe_fully_gidx(&self) -> bool {
        self.weights.layers.iter().all(|l| match &l.ffn {
            LayerFfn::Moe(moe) => Self::moe_gidx_capable(moe),
            LayerFfn::Dense(_) => true,
        })
    }

    /// Record the non-hybrid MoE decode step into a replayable graph. Only valid
    /// when `moe_fully_gidx()`: the expert dispatch reads the router selection on
    /// device (no readback), and all per-token inputs (token id, position, page
    /// table, seq len) come from device buffers refreshed before each replay.
    pub(crate) fn capture_step_moe(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_moe();
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step for a Mixture-of-Experts model (single token, paged f16
    /// cache). Attention mirrors the explicit separate chain but applies the
    /// model's QK-norm granularity (per-head for Qwen3-MoE, whole-vector for
    /// OLMoE); the FFN is replaced by `moe_decode_ffn`. Graph-captured when the
    /// model is fully gidx-capable (the routed experts are dispatched entirely
    /// on device); a fallback expert quant falls back to per-step launches.
    pub(crate) fn run_step_moe(&self) -> Result<()> {
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

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Project q/k/v into the separate b.q/b.k/b.v buffers regardless of
            // weight fusion (a fused matrix is read as three row-window GEMVs).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemm_rows(&b.q, w, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, w, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemm_rows(&b.v, w, &b.x, 1, q_dim + kv_dim, kv_dim, stream)?;
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&b.q, qk, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, qk, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
            }

            if let Some(qn) = &layer.attn().q_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, 1, q_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                }
            }
            if let Some(kn) = &layer.attn().k_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, 1, kv_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, p.n_kv_heads, p.head_dim, eps, stream)?;
                }
            }
            kernels.rope_neox_f16(
                &b.q,
                &b.pos,
                1,
                p.n_heads,
                p.head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            kernels.rope_neox_f16(
                &b.k,
                &b.pos,
                1,
                p.n_kv_heads,
                p.head_dim,
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
                p.n_kv_heads,
                self.kv.cfg.page_size,
                p.head_dim,
                stream,
            )?;
            kernels.attn_decode_f16(
                &b.attn_out,
                &b.attn_parts,
                &b.q,
                &self.kv.k[self.target_kv_layer(l)],
                &self.kv.v[self.target_kv_layer(l)],
                &self.page_table_dev,
                &self.seq_len_dev,
                1,
                p.n_heads,
                p.n_kv_heads,
                p.head_dim,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                scale,
                self.attn_window(l),
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

            match &layer.ffn {
                LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, l, hidden, stream)?,
                LayerFfn::Dense(_) => {
                    return Err(ForgeError::Unsupported(
                        "dense layer inside a MoE forward pass".into(),
                    ))
                }
            }

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

    /// Apply the routed experts for one token: `b.x` holds the FFN-normed
    /// input, `b.down` receives the weighted sum of the selected experts'
    /// SwiGLU outputs (plus the shared expert if present). The top-k experts
    /// are read back to the host to index the stacked expert weights.
    /// Routes `t` tokens: the projection first, then the selection.
    ///
    /// `moe_router_f16` does both in a single launch of one block per token,
    /// which at generation pushes the whole router matrix — a megabyte of
    /// weight — through one of the card's dozens of multiprocessors. The
    /// projection is an ordinary `experts x hidden` multiply and runs as one;
    /// only the selection is genuinely one block per token.
    fn moe_route(
        &self,
        moe: &MoeFfn,
        x: &DevBuffer,
        t: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let DevWeight::F16 { buf: router, .. } = &moe.router else {
            return Err(ForgeError::Unsupported("MoE router must be f16".into()));
        };
        if t == 1 {
            self.kernels
                .gemv_f16_out_f32(&mb.logits, router, x, moe.n_experts, hidden, stream)?;
        } else {
            self.kernels.gemm_f16_out_f32_at(
                &mb.logits,
                router,
                0,
                x,
                moe.n_experts,
                hidden,
                t,
                stream,
            )?;
        }
        self.kernels.moe_topk_f32(
            &mb.ids,
            &mb.weights,
            &mb.logits,
            moe.usage.counts(),
            t,
            moe.n_experts,
            moe.n_experts_used,
            moe.norm_topk,
            stream,
        )
    }

    /// Per-token sigmoid gate of the shared expert (`ffn_gate_inp_shexp · x`)
    /// for `t` tokens, left on the device in `mb.shared_scale`.
    fn moe_shared_gate(
        &self,
        gate: &DevWeight,
        x: &DevBuffer,
        t: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        if t == 1 {
            self.gemv(&mb.shared_logits, gate, x, stream)?;
        } else {
            self.gemm(&mb.shared_logits, gate, x, t, stream)?;
        }
        self.kernels
            .moe_sigmoid_f16_to_f32(&mb.shared_scale, &mb.shared_logits, t, stream)
    }

    /// Puts `t` shared-expert scales on their way to the host, for the dispatch
    /// paths that read the router back anyway — the copy rides that same sync.
    ///
    /// Without a shared-expert gate (OLMoE / Qwen3-MoE) the device buffer keeps
    /// the 1.0 seeded at load, so the expert folds in unscaled and the readback
    /// stays one code path.
    fn moe_stage_shared_scales(
        &self,
        moe: &MoeFfn,
        x: &DevBuffer,
        t: usize,
        stream: &Stream,
    ) -> Result<()> {
        if moe.shared.is_none() {
            return Ok(());
        }
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        if let Some(gate) = &moe.shared_gate {
            self.moe_shared_gate(gate, x, t, stream)?;
        }
        self.device
            .copy(&mb.shared_scale, 0, &mb.pinned_shared, 0, t * 4, stream)
    }

    /// The `t` staged scales, once the caller's sync has landed them.
    fn moe_shared_scales(mb: &MoeBufs, t: usize) -> &[f32] {
        let host = mb.pinned_shared.host_ptr().expect("pinned host mapping");
        unsafe { std::slice::from_raw_parts(host as *const f32, t) }
    }

    pub(crate) fn moe_decode_ffn(
        &self,
        moe: &MoeFfn,
        layer: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;

        // Device-side grouped dispatch: the router's selected ids/weights stay
        // ON the device and drive the expert GEMVs + accumulate through the
        // `_gidx` kernels, so the whole per-layer FFN runs as queued stream work
        // with ZERO host readback / synchronize. The expert count `k` is a fixed
        // model constant (not data-dependent), so the launch sequence is static.
        if Self::moe_gidx_capable(moe) {
            if let Some(sg) = &moe.shared_gate {
                self.moe_shared_gate(sg, &b.x, 1, stream)?;
            }
            self.moe_route(moe, &b.x, 1, hidden, stream)?;
            return self
                .moe_experts_accumulate_device(moe, &b.x, &b.down, 0, inter, hidden, k, stream);
        }

        // Fallback (expert quant without a `_gidx` kernel, e.g. Q8_0 down
        // projections): route on device but read the top-k selection back to
        // the host to launch the byte-offset expert GEMVs — one sync per layer.
        // Enqueue the shared-expert gate GEMV (when the arch has one) BEFORE the
        // router readback so its logit rides the SAME single sync as the top-k,
        // rather than forcing a second per-layer host round-trip.
        self.moe_stage_shared_scales(moe, &b.x, 1, stream)?;
        self.moe_route(moe, &b.x, 1, hidden, stream)?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                k,
            )
        };
        let shared_scale = Self::moe_shared_scales(mb, 1).first().copied().unwrap_or(1.0);
        self.fault_in_experts(moe, layer, ids)?;
        self.moe_experts_accumulate(
            moe,
            &b.x,
            &b.down,
            0,
            inter,
            hidden,
            ids,
            weights,
            shared_scale,
            stream,
        )
    }

    /// Device-side grouped expert dispatch for a single decode token: identical
    /// SwiGLU math to `moe_experts_accumulate`, but every routed expert's row
    /// window and routing weight are read ON DEVICE from `mb.ids`/`mb.weights`
    /// through the `_gidx` kernels — no host readback, no `synchronize`. The
    /// loop over `k` is over a fixed model constant, so the launch sequence is
    /// static and stream-ordered. The shared expert (row offset 0, host-known)
    /// reuses the ordinary GEMVs and folds in with the device-resident sigmoid
    /// gate scale. Bit-identical to the readback path for the routed experts;
    /// the only difference is the shared-gate sigmoid is computed on-GPU.
    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate_device(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        k: usize,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        // Gate and up multiply the SAME activation by two matrices of one
        // expert, so a kernel that takes both tables stages that activation once
        // and folds the gate function into its epilogue.
        let gate_stack = moe.gate_exps.representative();
        let up_stack = moe.up_exps.representative();
        let fused = gate_stack.block_quant() == up_stack.block_quant()
            && gate_stack.cols() == up_stack.cols()
            && gate_stack.block_quant().is_some_and(|quant| {
                self.kernels
                    .gemv_silu_gidx_batch(
                        quant,
                        &mb.sel_gate,
                        moe.gate_exps.table(),
                        moe.up_exps.table(),
                        x_in,
                        inter,
                        gate_stack.cols(),
                        &mb.ids,
                        k,
                        k,
                        stream,
                    )
                    .unwrap_or(false)
            });
        if !fused {
            self.gemv_rows_gidx_batch(
                &mb.sel_gate,
                &moe.gate_exps,
                x_in,
                &mb.ids,
                k,
                k,
                inter,
                stream,
            )?;
            self.gemv_rows_gidx_batch(
                &mb.sel_up,
                &moe.up_exps,
                x_in,
                &mb.ids,
                k,
                k,
                inter,
                stream,
            )?;
            self.kernels.glu_mul_f16(
                self.ffn_act(),
                &mb.sel_gate,
                &mb.sel_gate,
                &mb.sel_up,
                k * inter,
                stream,
            )?;
        }
        // One, not `k`: the half read here was written per SELECTION above, so
        // every selection already owns its row.
        self.gemv_rows_gidx_batch(
            &mb.sel_out,
            &moe.down_exps,
            &mb.sel_gate,
            &mb.ids,
            k,
            1,
            hidden,
            stream,
        )?;
        // Every selection sits at its own row, in the router's order, so the
        // slot table IS the identity over selections — nothing was reordered.
        self.kernels.moe_combine_f16(
            out,
            &mb.sel_out,
            &mb.identity,
            &mb.weights,
            1,
            hidden,
            k,
            true,
            stream,
        )?;
        if let Some(sh) = &moe.shared {
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            // mb.shared_scale holds this layer's device sigmoid gate scale when
            // the arch has a shared gate; for a gate-less shared expert it stays
            // at the 1.0 seeded once at load, so no per-layer write is needed.
            self.kernels.moe_scale_add_gidx_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                &mb.shared_scale,
                0,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Run each selected expert's SwiGLU over the single-token activation
    /// `x_in` (contiguous [hidden] at offset 0) and accumulate
    /// `weight * expert_out` into `out` at byte offset `out_off`. Reuses the
    /// quant GEMV machinery indexed by expert row-offset; the shared expert (if
    /// any) is folded in last. Scratch (`b.gate/up/act`, `mb.tmp`) is
    /// single-token sized, so this serves both the decode and prefill loops.
    /// Ściąga z dysku każdego wybranego eksperta, który nie jest rezydentny.
    ///
    /// Cały komplet warstwy idzie jednym zgłoszeniem: chybienia trzech
    /// projekcji są znane naraz, a NVMe oddaje pełną przepustowość dopiero przy
    /// głębokiej kolejce — po kolei płaciłoby się sumę opóźnień zamiast
    /// najdłuższego z nich.
    /// Zbiór różnych ekspertów wybranych w całym kawałku prefillu.
    fn chunk_expert_union(&self, ids: &[i32]) -> Vec<i32> {
        let mut union: Vec<i32> = ids.to_vec();
        union.sort_unstable();
        union.dedup();
        union
    }

    /// Czy komplet `count` ekspertów zmieści się naraz w slotach hosta każdej
    /// projekcji warstwy.
    fn expert_union_fits(&self, moe: &MoeFfn, count: usize) -> bool {
        [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .all(|stack| stack.fully_resident() || stack.host_slots() >= count)
    }

    fn fault_in_experts(&self, moe: &MoeFfn, layer: usize, ids: &[i32]) -> Result<()> {
        let Some(spill) = self.expert_spill.as_ref() else {
            return Ok(());
        };
        let wanted: Vec<usize> = ids.iter().map(|&e| e as usize).collect();
        // Bez zebranych liczników popularność jest zerowa i ofiarą pada
        // dowolny slot — to poprawne, tylko nieoptymalne przez pierwszą rundę.
        let empty = Vec::new();
        let popularity = self
            .moe_residency
            .as_ref()
            .map(|state| state.policy.popularity(layer))
            .unwrap_or(&empty);
        for stack in [&moe.gate_exps, &moe.up_exps, &moe.down_exps] {
            if stack.fully_resident() {
                continue;
            }
            stack.fault_in(self.device.as_ref(), spill, &wanted, popularity)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        ids: &[i32],
        weights: &[f32],
        shared_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        // A single-token GEMV over an expert = a row window of the stacked
        // expert matrix, i.e. gemm_rows at the expert row-offset (rows-per-
        // expert = inter for gate/up, hidden for down).
        for (j, (&e, &wt)) in ids.iter().zip(weights).enumerate() {
            let e = e as usize;
            if e >= moe.n_experts {
                return Err(ForgeError::Kernel(format!(
                    "router selected out-of-range expert {e}"
                )));
            }
            self.gemv_rows(&b.gate, moe.gate_exps.expert(e)?, x_in, 0, inter, stream)?;
            self.gemv_rows(&b.up, moe.up_exps.expert(e)?, x_in, 0, inter, stream)?;
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
            self.gemv_rows(&mb.tmp, moe.down_exps.expert(e)?, &b.act, 0, hidden, stream)?;
            self.kernels
                .moe_scale_add_f16(out, out_off, &mb.tmp, 0, hidden, wt, j == 0, stream)?;
        }
        // Shared always-on expert: a dense SwiGLU added on top, scaled by the
        // per-token sigmoid gate (`shared_scale`; 1.0 when the arch has no
        // shared-expert gate).
        if let Some(sh) = &moe.shared {
            // Shared expert down is [hidden, shared_inter], so cols = its width.
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            self.kernels.moe_scale_add_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                shared_scale,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Whether this layer's routed stacks multiply as ONE grid over every
    /// expert.
    ///
    /// A paged stack keeps the per-token loop: a grid spanning every expert has
    /// no address for a block that sits on disk.
    pub(crate) fn moe_grouped_capable(kernels: &Kernels, moe: &MoeFfn) -> bool {
        [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .all(|stack| {
                stack.fully_resident()
                    && stack
                        .representative()
                        .block_quant()
                        .is_some_and(|quant| kernels.supports_grouped_experts(quant))
            })
    }

    /// Routed experts of a prefill chunk, every expert in ONE grid.
    ///
    /// The per-token loop this replaces launched five kernels per selected
    /// expert per token: at 512 tokens and eight experts, a quarter of a
    /// million launches of a tile that covers seven blocks of a card with
    /// dozens of multiprocessors. Here each expert reads the block of rows that
    /// chose it, and the whole layer is three launches plus the gate.
    #[allow(clippy::too_many_arguments)]
    fn moe_grouped_ffn(
        &self,
        moe: &MoeFfn,
        ids: &[i32],
        t: usize,
        inter: usize,
        hidden: usize,
        x: &DevBuffer,
        out: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let (k, experts) = (moe.n_experts_used, moe.n_experts);
        let selections = t * k;

        // A counting sort over experts. `order[p]` is the token whose row the
        // gather puts at position p; `slots[sel]` says where that selection
        // landed, which is what puts the answers back afterwards.
        let mut starts = vec![0u32; experts + 1];
        for id in ids {
            let e = *id as usize;
            if e >= experts {
                return Err(ForgeError::Kernel(format!(
                    "router wybrał eksperta {e} przy {experts} w stosie"
                )));
            }
            starts[e + 1] += 1;
        }
        for e in 0..experts {
            starts[e + 1] += starts[e];
        }
        let mut cursor = starts.clone();
        let mut order = vec![0i32; selections];
        let mut slots = vec![0i32; selections];
        for (sel, id) in ids.iter().enumerate() {
            let at = &mut cursor[*id as usize];
            order[*at as usize] = (sel / k) as i32;
            slots[sel] = *at as i32;
            *at += 1;
        }
        self.device
            .write(bytemuck::cast_slice(&order), &mb.order, 0)?;
        self.device
            .write(bytemuck::cast_slice(&slots), &mb.slots, 0)?;

        // The stride is the NARROWEST tile among the three projections: a tile
        // covers its own width from `tile_first` and does not loop past it, and
        // the three need not share a format — Q4_K_M puts six bits on
        // `ffn_down` and four on the other two. Built wider, the rows past the
        // narrow tile's end belong to no launch.
        let stride = [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .filter_map(|stack| stack.representative().block_quant())
            .map(|quant| self.kernels.grouped_tile_rows(quant))
            .min()
            .expect("three projections of a routed layer");
        let (mut tile_expert, mut tile_first, mut tile_end) = (Vec::new(), Vec::new(), Vec::new());
        for e in 0..experts {
            let (from, to) = (starts[e] as usize, starts[e + 1] as usize);
            let mut at = from;
            while at < to {
                tile_expert.push(e as i32);
                tile_first.push(at as i32);
                tile_end.push(to as i32);
                at += stride;
            }
        }
        for (host, dev) in [
            (&tile_expert, &mb.tile_expert),
            (&tile_first, &mb.tile_first),
            (&tile_end, &mb.tile_end),
        ] {
            self.device.write(bytemuck::cast_slice(host), dev, 0)?;
        }
        let tiles = GroupedTiles {
            expert: &mb.tile_expert,
            first: &mb.tile_first,
            end: &mb.tile_end,
            count: tile_expert.len(),
        };

        self.kernels.gather_rows_f16(
            &mb.grouped_x,
            x,
            &mb.order,
            selections,
            hidden,
            stream,
        )?;
        // Gate i up czytają TE SAME wiersze, więc aktywacja idzie przez swoją
        // postać raz, a nie raz na projekcję.
        let ffn_act = self.kernels.prepare_grouped_act(
            Self::grouped_stack_quant(&moe.gate_exps)?,
            &mb.grouped_x,
            hidden,
            selections,
            stream,
        )?;
        for (stack, y) in [
            (&moe.gate_exps, &mb.grouped_gate),
            (&moe.up_exps, &mb.grouped_up),
        ] {
            self.gemm_grouped_stack(y, stack, &ffn_act, &tiles, inter, selections, stream)?;
        }
        // Elementwise, so the whole grouped block goes at once — the gate
        // function does not care which expert produced which row.
        self.kernels.glu_mul_f16(
            self.ffn_act(),
            &mb.grouped_gate,
            &mb.grouped_gate,
            &mb.grouped_up,
            selections * inter,
            stream,
        )?;
        let down_act = self.kernels.prepare_grouped_act(
            Self::grouped_stack_quant(&moe.down_exps)?,
            &mb.grouped_gate,
            inter,
            selections,
            stream,
        )?;
        self.gemm_grouped_stack(
            &mb.grouped_out,
            &moe.down_exps,
            &down_act,
            &tiles,
            hidden,
            selections,
            stream,
        )?;
        self.kernels.moe_combine_f16(
            out,
            &mb.grouped_out,
            &mb.slots,
            &mb.weights,
            t,
            hidden,
            k,
            true,
            stream,
        )?;
        self.moe_grouped_shared(moe, t, hidden, x, out, stream)
    }

    /// The always-on expert of a prefill chunk: a dense SwiGLU over all `t`
    /// rows, added on top of the routed sum.
    ///
    /// It runs AFTER the combine, which is what lets it borrow `mb.grouped_out`
    /// as its `[t, hidden]` landing — the routed answers have been folded into
    /// `pb.down` by then and that scratch is free. Each row is scaled by its own
    /// gate, still on the device, so nothing here reads back.
    #[allow(clippy::too_many_arguments)]
    fn moe_grouped_shared(
        &self,
        moe: &MoeFfn,
        t: usize,
        hidden: usize,
        x: &DevBuffer,
        out: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        let Some(sh) = &moe.shared else {
            return Ok(());
        };
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        // Gate i up idą przez scratch prefillu także w batchu dekodowania: jest
        // wymiarowany na pełny chunk, więc mieści każdą szerokość batcha, a
        // własny bufor dublowałby tę samą pamięć.
        let pb = self.prefill_bufs.as_ref().expect("prefill bufs allocated");
        let sh_inter = sh.down.cols();
        match &sh.gate_up {
            GateUpWeights::Fused(w) => {
                self.gemm_rows(&pb.gate, w, x, t, 0, sh_inter, stream)?;
                self.gemm_rows(&pb.up, w, x, t, sh_inter, sh_inter, stream)?;
            }
            GateUpWeights::Split { gate, up } => {
                self.gemm_rows(&pb.gate, gate, x, t, 0, gate.rows(), stream)?;
                self.gemm_rows(&pb.up, up, x, t, 0, up.rows(), stream)?;
            }
        }
        self.kernels
            .glu_mul_f16(self.ffn_act(), &pb.gate, &pb.gate, &pb.up, t * sh_inter, stream)?;
        self.gemm(&mb.grouped_out, &sh.down, &pb.gate, t, stream)?;
        // One expert per token, its row at its own index: the combine with
        // `top_k = 1` over the identity IS `pb.down[r] += gate[r] · shared[r]`.
        self.kernels.moe_combine_f16(
            out,
            &mb.grouped_out,
            &mb.identity,
            &mb.shared_scale,
            t,
            hidden,
            1,
            false,
            stream,
        )
    }

    /// Whether a decode batch may run this model's routed FFN as one grouped
    /// dispatch: every routed layer needs the grouped kernels and a fully
    /// resident expert stack. The scratch it borrows is allocated on demand and
    /// is deliberately NOT part of the answer — the engine asks this before the
    /// first prefill has run, and a `false` there would pin `batch_min` at 12.
    pub fn moe_batch_capable(&self) -> bool {
        self.weights.is_moe()
            && self.moe_bufs.is_some()
            && self.weights.layers.iter().all(|layer| match &layer.ffn {
                LayerFfn::Dense(_) => true,
                LayerFfn::Moe(moe) => {
                    Self::moe_gidx_capable(moe) || Self::moe_grouped_capable(&self.kernels, moe)
                }
            })
    }

    /// Routed experts for a decode batch.
    ///
    /// A decode batch spreads its selections thin: eight lanes picking eight of
    /// 128 experts land on about eighteen distinct ones, so a grouped tile built
    /// for 64 rows owns three or four. Measured on the decode shape, addressing
    /// the expert per selection on device beats grouping by 1,9x on gate/up —
    /// the tile's staging pipeline costs more than the duplicate expert reads
    /// the grouping saves, and those duplicates are cache hits anyway.
    ///
    /// So the whole layer runs off `mb.ids`: no counting sort, no gather, no
    /// scatter, and no `synchronize` — which is also what lets the step be
    /// recorded as a graph. Grouping stays where its tile is full: prefill.
    pub(crate) fn moe_batch_ffn(
        &self,
        moe: &MoeFfn,
        n: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let k = moe.n_experts_used;
        self.moe_stage_shared_scales(moe, &bb.x, n, stream)?;
        self.moe_route(moe, &bb.x, n, hidden, stream)?;
        if Self::moe_gidx_capable(moe) {
            return self.moe_batch_gidx_ffn(moe, n, hidden, stream);
        }
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, n * k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                n * k,
            )
        };
        self.moe_grouped_ffn(moe, ids, n, moe.moe_inter, hidden, &bb.x, &bb.down, stream)
    }

    /// The device-addressed expert chain over a whole decode batch: every one of
    /// the `n * k` selections is a row of its own, and `share = k` is what tells
    /// a selection which lane's activation it reads. The down projection takes
    /// `share = 1` because by then each selection already owns its row.
    fn moe_batch_gidx_ffn(
        &self,
        moe: &MoeFfn,
        n: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let (k, inter) = (moe.n_experts_used, moe.moe_inter);
        let selections = n * k;
        let gate_stack = moe.gate_exps.representative();
        let up_stack = moe.up_exps.representative();
        // Gate and up multiply the SAME activation, so one kernel that takes
        // both tables reads it once and folds the gate function in.
        let fused = gate_stack.block_quant() == up_stack.block_quant()
            && gate_stack.cols() == up_stack.cols()
            && gate_stack.block_quant().is_some_and(|quant| {
                self.kernels
                    .gemv_silu_gidx_batch(
                        quant,
                        &mb.grouped_gate,
                        moe.gate_exps.table(),
                        moe.up_exps.table(),
                        &bb.x,
                        inter,
                        gate_stack.cols(),
                        &mb.ids,
                        selections,
                        k,
                        stream,
                    )
                    .unwrap_or(false)
            });
        if !fused {
            self.gemv_rows_gidx_batch(
                &mb.grouped_gate,
                &moe.gate_exps,
                &bb.x,
                &mb.ids,
                selections,
                k,
                inter,
                stream,
            )?;
            self.gemv_rows_gidx_batch(
                &mb.grouped_up,
                &moe.up_exps,
                &bb.x,
                &mb.ids,
                selections,
                k,
                inter,
                stream,
            )?;
            self.kernels.glu_mul_f16(
                self.ffn_act(),
                &mb.grouped_gate,
                &mb.grouped_gate,
                &mb.grouped_up,
                selections * inter,
                stream,
            )?;
        }
        self.gemv_rows_gidx_batch(
            &mb.grouped_out,
            &moe.down_exps,
            &mb.grouped_gate,
            &mb.ids,
            selections,
            1,
            hidden,
            stream,
        )?;
        // Selections sit in the router's order, so the slot table IS the
        // identity over them — nothing was reordered on the way here.
        self.kernels.moe_combine_f16(
            &bb.down,
            &mb.grouped_out,
            &mb.identity,
            &mb.weights,
            n,
            hidden,
            k,
            true,
            stream,
        )?;
        self.moe_grouped_shared(moe, n, hidden, &bb.x, &bb.down, stream)
    }

    /// Routed experts for a prefill chunk: route all `t` tokens at once, then
    /// apply each token's top-k experts, writing `[t, hidden]` into `pb.down`.
    /// The router readback is one sync per layer.
    pub(crate) fn moe_prefill_ffn(
        &self,
        moe: &MoeFfn,
        layer: usize,
        t: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let pb = self.prefill_bufs.as_ref().expect("prefill bufs allocated");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;
        self.moe_stage_shared_scales(moe, &pb.x, t, stream)?;
        self.moe_route(moe, &pb.x, t, hidden, stream)?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, t * k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, t * k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                t * k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                t * k,
            )
        };
        // Prefill dotyka wielu tokenów, a te trafiają w mocno zachodzące zbiory
        // ekspertów. Ściągnięcie sumy całego kawałka jednym zgłoszeniem zamienia
        // `t` rund odczytu na jedną; gdy suma nie mieści się w slotach, zostaje
        // stronicowanie per token — wtedy i tak trzeba by ich wypierać nawzajem.
        if Self::moe_grouped_capable(&self.kernels, moe) {
            return self.moe_grouped_ffn(moe, ids, t, inter, hidden, &pb.x, &pb.down, stream);
        }
        let shared_scales = Self::moe_shared_scales(mb, t);
        let chunk_union = self.chunk_expert_union(ids);
        let union_fits = self.expert_union_fits(moe, chunk_union.len());
        if union_fits {
            self.fault_in_experts(moe, layer, &chunk_union)?;
        }
        for ti in 0..t {
            // Copy this token's normed hidden into a contiguous scratch row so
            // the single-token expert GEMVs read from offset 0.
            self.device
                .copy(&pb.x, ti * hidden * 2, &mb.xrow, 0, hidden * 2, stream)?;
            if !union_fits {
                // Stronicowanie nadpisuje przypiętą pamięć slotu z hosta, a
                // eksperci poprzednich tokenów mogą być jeszcze czytani przez
                // kernele w locie — bez tej bariery byłby to wyścig o wagi.
                stream.synchronize()?;
                self.fault_in_experts(moe, layer, &ids[ti * k..(ti + 1) * k])?;
            }
            self.moe_experts_accumulate(
                moe,
                &mb.xrow,
                &pb.down,
                ti * hidden * 2,
                inter,
                hidden,
                &ids[ti * k..(ti + 1) * k],
                &weights[ti * k..(ti + 1) * k],
                shared_scales.get(ti).copied().unwrap_or(1.0),
                stream,
            )?;
        }
        Ok(())
    }
}
