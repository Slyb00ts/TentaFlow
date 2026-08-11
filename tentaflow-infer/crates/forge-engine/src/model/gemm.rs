// ===== File: model/gemm.rs — projekcje i mnozenia na poziomie modelu =====
use super::*;
use forge_kernels::{GroupedAct, GroupedTiles};

impl Model {
    /// Wykonuje kilka pełnych projekcji GGUF NVFP4 ze wspólną kwantyzacją
    /// aktywacji Q8_1. Zwraca `false`, gdy choć jedna waga ma inny format.
    /// Wykonuje kilka projekcji Q8_0 jednym uruchomieniem. Zwraca `false`, gdy
    /// choć jedna waga ma inny format albo szerokości się różnią.
    pub(crate) fn gemv_q8_0_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::Q8_0 {
                buf,
                rows,
                cols: weight_cols,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Ok(false);
            }
            cols = Some(*weight_cols);
            group.push((output, buf, *rows));
        }
        let Some(cols) = cols else {
            return Ok(false);
        };
        self.kernels
            .gemv_q8_0_dp4a_group_f16(&group, x, cols, stream)
    }

    /// To samo dla Q4_K: `Q4_K_M` trzyma w tym formacie projekcje uwagi,
    /// wejściowe projekcje DeltaNet oraz `gate`/`up` FFN.
    pub(crate) fn gemv_q4_k_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::Q4K {
                buf,
                rows,
                cols: weight_cols,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Ok(false);
            }
            cols = Some(*weight_cols);
            group.push((output, buf, *rows));
        }
        let Some(cols) = cols else {
            return Ok(false);
        };
        if cols > Kernels::DP4A_MAX_COLS {
            return Ok(false);
        }
        self.kernels.gemv_q4_k_dp4a_group_f16(
            &group,
            x,
            cols,
            self.q4k_decode_model_family(),
            stream,
        )
    }

    /// Grupa projekcji o RÓŻNYCH formatach — jedno uruchomienie.
    ///
    /// `Q4_K_M` dobiera format per tensor, więc jednorodne grupowanie omijało
    /// najliczniejsze trójki i czwórki: `q`/`k` w Q4_K obok `v` w Q6_K, oraz
    /// wejściową projekcję DeltaNet w Q6_K obok bramki w Q4_K.
    pub(crate) fn gemv_mixed_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let (buf, rows, weight_cols, quant) = match weight {
                DevWeight::Q4K { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q4K),
                DevWeight::Q6K { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q6K),
                DevWeight::Q8_0 { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q8_0),
                _ => return Ok(false),
            };
            if cols.is_some_and(|value| value != weight_cols) {
                return Ok(false);
            }
            cols = Some(weight_cols);
            group.push((output, buf, rows, quant));
        }
        let Some(cols) = cols else { return Ok(false) };
        if cols > Kernels::DP4A_MAX_COLS {
            return Ok(false);
        }
        self.kernels
            .gemv_mixed_dp4a_group_f16(&group, x, cols, stream)
    }

    pub(crate) fn gemv_nvfp4_gguf_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut weight_layout = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols: weight_cols,
                layout,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Err(ForgeError::Format(
                    "projekcje NVFP4 współdzielące Q8_1 mają różne szerokości".into(),
                ));
            }
            if weight_layout.is_some_and(|value| value != *layout) {
                return Ok(false);
            }
            weight_layout = Some(*layout);
            cols = Some(*weight_cols);
            group.push(Nvfp4GgufQ8Projection {
                output,
                weights: buf,
                rows: *rows,
                output_scale: *output_scale,
            });
        }
        let Some(cols) = cols else { return Ok(false) };
        let layout = weight_layout.unwrap_or(Nvfp4GgufLayout::RowMajor36);
        if layout == Nvfp4GgufLayout::TileN128K64 {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_layout_f16(&group, x, cols, layout, stream)?;
        } else if self.device.caps().vendor == Vendor::Nvidia {
            for projection in group {
                self.kernels.gemv_nvfp4_gguf_b1_f16(
                    projection.output,
                    projection.weights,
                    x,
                    projection.rows,
                    cols,
                    projection.output_scale,
                    stream,
                )?;
            }
        } else {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_f16(&group, x, cols, stream)?;
        }
        Ok(true)
    }

    pub(crate) fn logits_gemv(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        // Głowa NIE korzysta z paczki `fp8_lm_head`, choć ta jest budowana razem
        // z paczkami FFN. e4m3 ma 3-bitową mantysę w warstwie, która wprost
        // wybiera token: użycie jej TYLKO tutaj dawało inny strumień greedy w
        // pojedynczym strumieniu niż w batchu (który liczy głowę w F16), czyli
        // jakość zależną od współbieżności. Paczka zostaje dla prefillu, gdzie
        // liczą się aktywacje, a nie wybór tokena.
        if let Some(tp) = &self.tp_ffn {
            if tp.forward_logits(stream, x, y_f32)? {
                self.postprocess_logits(y_f32, 0, self.weights.lm_head.rows(), stream)?;
                return Ok(());
            }
        }
        self.logits_weight_gemv(y_f32, 0, x, 0, &self.weights.lm_head, stream)
    }

    pub(crate) fn logits_weight_gemv(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        x: &DevBuffer,
        x_off: usize,
        weight: &DevWeight,
        stream: &Stream,
    ) -> Result<()> {
        self.gemv_out_f32(y_f32, y_off, x, x_off, weight, stream)?;
        self.postprocess_logits(y_f32, y_off, weight.rows(), stream)?;
        // Maska tokenów zabronionych: kopie stream-ordered z jednoelementowego
        // bufora -inf, więc mieszczą się w przechwytywanym grafie decode.
        if let Some(neg_inf) = &self.weights.neg_inf {
            for &id in &self.weights.descriptor.params.suppress_tokens {
                let slot = y_off + id as usize;
                if slot < y_off + weight.rows() {
                    self.device.copy(neg_inf, 0, y_f32, slot * 4, 4, stream)?;
                }
            }
        }
        Ok(())
    }

    fn postprocess_logits(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let scale = self.weights.descriptor.params.logit_scale;
        if scale != 1.0 {
            self.kernels.scale_f32(y_f32, y_off, n, scale, stream)?;
        }
        // Ograniczenie logitów (Gemma): cap * tanh(x / cap). Nakładane tutaj, bo
        // to jedyne wyjście głowy — sampling i logprob widzą już wartości po capie.
        let cap = self.weights.descriptor.params.final_logit_softcap;
        if cap > 0.0 {
            self.kernels.softcap_f32(y_f32, y_off, n, cap, stream)?;
        }
        Ok(())
    }

    pub(crate) fn gemm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_rows(y, w, x, n_tokens, 0, w.rows(), stream)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_nvfp4_ct_direct(
        &self,
        y_padded: &DevBuffer,
        workspace: &DevBuffer,
        weight: &DevWeight,
        x_padded: &DevBuffer,
        logical_m: usize,
        projection: Nvfp4CtProjection,
        stream: &Stream,
    ) -> Result<bool> {
        if nvfp4_ct_physical_m(logical_m).is_none()
            || Self::nvfp4_ct_projection(weight) != Some(projection)
        {
            return Ok(false);
        }
        let DevWeight::NvFp4 {
            inv_global_scale,
            rows,
            ..
        } = weight
        else {
            return Ok(false);
        };
        let window = weight.nvfp4_ct_row_window(0, *rows)?;
        let view = Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
        self.kernels.gemm_nvfp4_ct_padded(
            y_padded,
            if projection == Nvfp4CtProjection::GateUp {
                None
            } else {
                Some(workspace)
            },
            view,
            x_padded,
            logical_m,
            projection,
            *inv_global_scale,
            stream,
        )?;
        Ok(true)
    }

    pub(crate) fn gemm_q8_prepared(
        &self,
        y: &DevBuffer,
        weight: &DevWeight,
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        let DevWeight::Q8_0 {
            buf, rows, cols, ..
        } = weight
        else {
            return Err(ForgeError::Format(
                "przygotowana grupa DeltaNet wymaga wag Q8_0".into(),
            ));
        };
        self.kernels
            .gemm_q8_0_i8mma_prepared_at(y, buf, 0, prepared, *rows, *cols, n_tokens)
    }

    pub(crate) fn gemm_q8_prepared_triplet(
        &self,
        outputs: [&DevBuffer; 3],
        weights: [&DevWeight; 3],
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        fn projection<'a>(
            output: &'a DevBuffer,
            weight: &'a DevWeight,
        ) -> Result<(Q8PreparedProjection<'a>, usize)> {
            let DevWeight::Q8_0 {
                buf, rows, cols, ..
            } = weight
            else {
                return Err(ForgeError::Format(
                    "fused grupa DeltaNet wymaga wag Q8_0".into(),
                ));
            };
            Ok((
                Q8PreparedProjection {
                    output,
                    weights: buf,
                    weight_byte_offset: 0,
                    rows: *rows,
                },
                *cols,
            ))
        }
        let (gate, cols) = projection(outputs[0], weights[0])?;
        let (alpha, alpha_cols) = projection(outputs[1], weights[1])?;
        let (beta, beta_cols) = projection(outputs[2], weights[2])?;
        if alpha_cols != cols || beta_cols != cols {
            return Err(ForgeError::Format(
                "fused grupa DeltaNet wymaga wspólnego rozmiaru wejścia".into(),
            ));
        }
        self.kernels.gemm_q8_0_i8mma_prepared_triplet(
            &[gate, alpha, beta],
            prepared,
            cols,
            n_tokens,
        )
    }

    /// W4A8 prefill projection GEMM (per-token int8 activation quant + int4xint8
    /// GEMM). Each W4A8 weight is a standalone logical matrix, so no windowing.
    /// Wykonuje projekcję wybraną wcześniej. `shared_act` mówi, że fuzowana
    /// norma wyemitowała już wspólną aktywację e4m3 i nie trzeba jej kwantyzować
    /// per projekcja.
    pub(crate) fn project(
        &self,
        y: &DevBuffer,
        p: &ProjectionPlan<'_>,
        x: &DevBuffer,
        rows: usize,
        shared_act: bool,
        stream: &Stream,
    ) -> Result<()> {
        match p {
            ProjectionPlan::W4A8(w) => self.gemm_w4a8(y, w, x, rows, stream),
            ProjectionPlan::Fp8(w) if shared_act => self.gemm_fp8_prequant(y, w, rows, stream),
            ProjectionPlan::Fp8(w) => self.gemm_fp8(y, w, x, rows, stream),
            ProjectionPlan::Rows {
                w,
                row_off,
                rows: n,
            } => self.gemm_rows(y, w, x, rows, *row_off, *n, stream),
        }
    }

    /// Q/K/V warstwy `l` w jednej postaci, niezależnie od formatu i układu wag.
    pub(crate) fn qkv_projections<'w>(
        &'w self,
        layer: &'w LayerWeights,
        w4a8: Option<&'w W4A8Layer>,
        fp8: Option<&'w Fp8Layer>,
        fp8_ffn: Option<&'w Fp8FfnLayer>,
        q_rows: usize,
        kv_rows: usize,
    ) -> [ProjectionPlan<'w>; 3] {
        if let Some(wl) = w4a8 {
            return [
                ProjectionPlan::W4A8(&wl.q),
                ProjectionPlan::W4A8(&wl.k),
                ProjectionPlan::W4A8(&wl.v),
            ];
        }
        if let Some(fl) = fp8 {
            return [
                ProjectionPlan::Fp8(&fl.q),
                ProjectionPlan::Fp8(&fl.k),
                ProjectionPlan::Fp8(&fl.v),
            ];
        }
        if let Some(fl) = fp8_ffn {
            return [
                ProjectionPlan::Fp8(&fl.q),
                ProjectionPlan::Fp8(&fl.k),
                ProjectionPlan::Fp8(&fl.v),
            ];
        }
        match &layer.attn().attn_qkv {
            QkvWeights::Fused(w) => [
                ProjectionPlan::Rows {
                    w,
                    row_off: 0,
                    rows: q_rows,
                },
                ProjectionPlan::Rows {
                    w,
                    row_off: q_rows,
                    rows: kv_rows,
                },
                ProjectionPlan::Rows {
                    w,
                    row_off: q_rows + kv_rows,
                    rows: kv_rows,
                },
            ],
            QkvWeights::FusedQk { qk, v } => [
                ProjectionPlan::Rows {
                    w: qk,
                    row_off: 0,
                    rows: q_rows,
                },
                ProjectionPlan::Rows {
                    w: qk,
                    row_off: q_rows,
                    rows: kv_rows,
                },
                ProjectionPlan::Rows {
                    w: v,
                    row_off: 0,
                    rows: v.rows(),
                },
            ],
            QkvWeights::Split { q, k, v } => [
                ProjectionPlan::Rows {
                    w: q,
                    row_off: 0,
                    rows: q.rows(),
                },
                ProjectionPlan::Rows {
                    w: k,
                    row_off: 0,
                    rows: k.rows(),
                },
                ProjectionPlan::Rows {
                    w: v,
                    row_off: 0,
                    rows: v.rows(),
                },
            ],
        }
    }

    pub(crate) fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        w: &W4A8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels.gemm_w4a8(
            y,
            &w.qweight,
            &w.s2_zeros,
            &w.s2_scales,
            &w.s1_scales,
            &w.inv_smooth,
            x,
            w.rows,
            w.cols,
            n_tokens,
            stream,
        )
    }

    /// fp8 (e4m3) prefill projection GEMM (per-token e4m3 activation quant +
    /// e4m3×e4m3 tensor-core GEMM). Each fp8 weight is a standalone logical
    /// matrix, so no windowing. `FORGE_GEMM=fp8`.
    pub(crate) fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if self.weights.fp8_modular {
            return self.kernels.gemm_fp8_modular(
                y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
            );
        }
        self.kernels.gemm_fp8(
            y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
        )
    }

    /// fp8mod projection over the shared per-token e4m3 activation the preceding
    /// fused rmsnorm→fp8 emitted (q/k/v share one, gate/up share one) — no
    /// per-projection activation requant. `FORGE_GEMM=fp8mod` only.
    pub(crate) fn gemm_fp8_prequant(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels
            .gemm_fp8_modular_prequant(y, &w.qweight, &w.scales, w.rows, w.cols, n_tokens, stream)
    }

    /// Single-token GEMV over a row window of `w` (`y = W[row_off..+n_rows]·x`).
    /// The routed-MoE expert path uses this instead of the batched `gemm_rows`:
    /// a decode step feeds one token, and the GEMM tile (BM=64) then launches
    /// only `n_rows/64` blocks — far too few to saturate the SMs, so the GPU
    /// stays at idle clocks. The per-row GEMV kernels launch `n_rows/8` blocks
    /// (8 experts queued back-to-back per layer keep the device busy enough to
    /// boost). Formats without an offset GEMV variant fall back to the tile.
    pub(crate) fn gemv_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::Q4K { buf, cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_at(
                    y,
                    buf,
                    row_off * (cols / 256) * 144,
                    x,
                    n_rows,
                    *cols,
                    stream,
                )
            }
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemv_q6_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 210,
                x,
                n_rows,
                *cols,
                stream,
            ),
            _ => self.gemm_rows(y, w, x, 1, row_off, n_rows, stream),
        }
    }

    /// One projection of EVERY expert over the block of grouped rows that chose
    /// it, in one grid. `tiles` says per block which expert it reads and which
    /// rows are its own, so the tile itself is the ungrouped tile — grouping
    /// picks the block's placement rather than the launch's.
    #[allow(clippy::too_many_arguments)]
    /// Format wspólny dla trzech projekcji warstwy, żeby aktywację przygotować
    /// raz na jej postać, a nie raz na stos.
    pub(crate) fn grouped_stack_quant(stack: &ExpertStack) -> Result<QuantKind> {
        stack.representative().block_quant().ok_or_else(|| {
            ForgeError::Unsupported("gemm_grouped_stack called for a blockless expert".into())
        })
    }

    pub(crate) fn gemm_grouped_stack(
        &self,
        y: &DevBuffer,
        stack: &ExpertStack,
        act: &GroupedAct<'_>,
        tiles: &GroupedTiles<'_>,
        n_rows: usize,
        selections: usize,
        stream: &Stream,
    ) -> Result<()> {
        let w = stack.representative();
        let quant = Self::grouped_stack_quant(stack)?;
        self.kernels.gemm_grouped_experts(
            quant,
            y,
            stack.table(),
            act,
            GroupedTiles {
                expert: tiles.expert,
                first: tiles.first,
                end: tiles.end,
                count: tiles.count,
                rows: tiles.rows,
            },
            n_rows,
            w.cols(),
            selections,
            stream,
        )
    }

    /// Every selection of a routed step through one stack, in ONE launch.
    ///
    /// The device analog of `gemv_rows`: each selection's weight base is
    /// resolved from the stack's device-resident pointer table inside the
    /// kernel, so nothing is read back to the host. The selection is a grid
    /// dimension rather than a launch of its own. `share` is how many
    /// selections read the same row of `x` — `top_k` while the activation is
    /// still per token, one once every selection owns its row.
    /// Only the quants `expert_stack_gidx` accepts reach here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemv_rows_gidx_batch(
        &self,
        y: &DevBuffer,
        stack: &ExpertStack,
        x: &DevBuffer,
        ids: &DevBuffer,
        selections: usize,
        share: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        let w = stack.representative();
        let quant = w.block_quant().ok_or_else(|| {
            ForgeError::Unsupported("gemv_rows_gidx_batch called for a blockless expert".into())
        })?;
        self.kernels.gemv_gidx_batch(
            quant,
            y,
            stack.table(),
            x,
            n_rows,
            w.cols(),
            ids,
            selections,
            share,
            stream,
        )
    }

    /// Batchowa głowa logitów: y[b, vocab] f32 = lm_head · x[b, hidden].
    pub(crate) fn logits_gemm(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let rows = self.weights.lm_head.rows();
        let batch_needs_postprocess = match &self.weights.lm_head {
            DevWeight::F16 { buf, rows, cols } => {
                self.kernels
                    .gemm_f16_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream)?;
                true
            }
            DevWeight::Q8_0 { buf, rows, cols } if (2..=8).contains(&n_tokens) => {
                self.kernels.gemm_q8_0_f16_exact_out_f32_at(
                    y_f32, buf, 0, x, *rows, *cols, n_tokens, stream,
                )?;
                true
            }
            DevWeight::Q8_0 { buf, rows, cols } => {
                self.kernels
                    .gemm_q8_0_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream)?;
                true
            }
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout: Nvfp4GgufLayout::RowMajor36,
            } if matches!(n_tokens, 2 | 4 | 8 | 16) => {
                self.kernels.gemm_nvfp4_gguf_out_f32_batch(
                    y_f32,
                    buf,
                    x,
                    *rows,
                    *cols,
                    n_tokens,
                    *output_scale,
                    stream,
                )?;
                true
            }
            // Q6_K ma batchowy przemiat z wyjściem f32: jeden odczyt wag na
            // cały batch. Q4_K głowy go nie ma, więc zostaje przy przemiatnięciu
            // per token (patrz niżej).
            // Przemiat istnieje do ośmiu linii, więc szerszy krok jedzie ÓSEMKAMI:
            // przy 64 liniach osiem odczytów wag głowy zamiast sześćdziesięciu.
            DevWeight::Q6K { buf, rows, cols } if n_tokens > 8 => {
                let mut done = 0usize;
                while done + 8 <= n_tokens {
                    if !self.kernels.gemv_q6_k_dp4a_batch_out_f32_at(
                        y_f32,
                        done * *rows * 4,
                        buf,
                        0,
                        x,
                        done * *cols * 2,
                        *rows,
                        *cols,
                        8,
                        stream,
                    )? {
                        break;
                    }
                    done += 8;
                }
                if done > 0 {
                    self.postprocess_logits(y_f32, 0, done * *rows, stream)?;
                }
                for lane in done..n_tokens {
                    self.logits_weight_gemv(
                        y_f32,
                        lane * *rows * 4,
                        x,
                        lane * *cols * 2,
                        &self.weights.lm_head,
                        stream,
                    )?;
                }
                false
            }
            DevWeight::Q6K { buf, rows, cols }
                if self.kernels.gemv_q6_k_dp4a_batch_out_f32_at(
                    y_f32, 0, buf, 0, x, 0, *rows, *cols, n_tokens, stream,
                )? =>
            {
                true
            }
            // Pozostałe głowy K-kwantowe nie mają batchowego GEMM-a out-f32;
            // przemiat dp4a per token zostawia resztę kroku batchowego bez
            // zmian (głowa to jeden odczyt wag na token, stos warstw nadal jest
            // amortyzowany po batchu).
            w @ (DevWeight::Q4K { rows, cols, .. }
            | DevWeight::Q5K { rows, cols, .. }
            | DevWeight::Q6K { rows, cols, .. }) => {
                for lane in 0..n_tokens {
                    self.logits_weight_gemv(
                        y_f32,
                        lane * *rows * 4,
                        x,
                        lane * *cols * 2,
                        w,
                        stream,
                    )?;
                }
                false
            }
            _ => {
                return Err(ForgeError::Unsupported(
                    "batchowa głowa logitów nie obsługuje tego formatu ani szerokości".into(),
                ));
            }
        };
        if batch_needs_postprocess {
            self.postprocess_logits(y_f32, 0, n_tokens * rows, stream)?;
        }
        Ok(())
    }
}
