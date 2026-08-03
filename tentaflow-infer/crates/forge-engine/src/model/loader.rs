// ===== File: model/loader.rs — wczytanie wag, paczki FP8, kalibracja =====
use super::*;

impl Model {
    pub(crate) fn nvfp4_tile_requested(layout: Nvfp4GgufLayout, capable: bool) -> Result<bool> {
        match layout {
            Nvfp4GgufLayout::RowMajor36 => Ok(false),
            Nvfp4GgufLayout::TileN128K64 if capable => Ok(true),
            Nvfp4GgufLayout::TileN128K64 => Err(ForgeError::Unsupported(
                "TileN128K64 wymaga NVIDIA warp32 i pełnego zestawu artefaktów NVFP4".into(),
            )),
        }
    }

    pub(crate) fn validate_nvfp4_tile_repacked(requested: bool, repacked_weights: usize) -> Result<()> {
        if requested && repacked_weights == 0 {
            return Err(ForgeError::Unsupported(
                "TileN128K64 wymaga co najmniej jednej kwalifikującej się wagi GGUF NVFP4".into(),
            ));
        }
        Ok(())
    }

    /// Ładuje model rozłożony na `devices` jako podział tensor-parallel.
    ///
    /// KOLEJNOŚĆ JEST WYMUSZONA: rangi powstają PIERWSZE, a dostęp P2P otwiera
    /// się dopiero nad ICH urządzeniami. Odwrotna kolejność — otwarcie kart
    /// najpierw, a potem budowanie na nich modeli — dałaby na każdej karcie
    /// drugi komplet pul pamięci i drugi zestaw artefaktów kerneli, a bufory,
    /// na których liczy redukcja, nie byłyby tymi, na których liczy ranga.
    pub fn load_tp(devices: &[Arc<dyn Device>], path: &Path, cfg: ModelConfig) -> Result<Self> {
        let world = devices.len();
        if world < 2 {
            return Err(ForgeError::Scheduler(
                "podział tensor-parallel wymaga co najmniej dwóch kart".into(),
            ));
        }
        let mut ranks = Vec::with_capacity(world);
        for (rank, device) in devices.iter().enumerate() {
            let mut rank_cfg = cfg.clone();
            rank_cfg.tp_shard = forge_formats::TpShard::new(rank, world)?;
            let model = Model::load_gguf(device.clone(), path, rank_cfg)?;
            model.tp_refuse_uncovered(&cfg)?;
            ranks.push(model);
        }
        // Redukcja czyta cudzą sumę cząstkową WPROST, więc bez P2P nie ma jej
        // czym wykonać. Objazd przez hosta i tak kosztowałby więcej, niż podział
        // daje, więc to odmowa, a nie wolniejsza droga.
        if !crate::cluster::enable_peer_mesh(devices) {
            return Err(ForgeError::Scheduler(
                "podział na rangi wymaga, żeby karty widziały swoją pamięć (P2P)".into(),
            ));
        }
        let mut zero = ranks.remove(0);
        let hidden = zero.weights.descriptor.params.hidden_size;
        let events = std::iter::once(&zero)
            .chain(ranks.iter())
            .map(|member| member.device.create_event())
            .collect::<Result<Vec<_>>>()?;
        let read_events = std::iter::once(&zero)
            .chain(ranks.iter())
            .map(|member| member.device.create_event())
            .collect::<Result<Vec<_>>>()?;
        let acc = std::iter::once(&zero)
            .chain(ranks.iter())
            .map(|member| {
                member
                    .device
                    .alloc(hidden * 4, MemKind::Device, Pool::Activations)
            })
            .collect::<Result<Vec<_>>>()?;
        tracing::info!(
            world,
            hidden,
            heads = zero.weights.descriptor.params.n_heads,
            kv_heads = zero.weights.descriptor.params.n_kv_heads,
            inter = zero.weights.descriptor.params.intermediate_size,
            "model podzielony na rangi; każda widzi swój fragment"
        );
        zero.tp = Some(Box::new(TpSpmd {
            ranks,
            events,
            read_events,
            acc,
        }));
        Ok(zero)
    }

    pub fn load_gguf(device: Arc<dyn Device>, path: &Path, cfg: ModelConfig) -> Result<Self> {
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;
        let target_tile = Self::nvfp4_tile_requested(
            cfg.nvfp4_gguf_layout,
            kernels.supports_nvfp4_gguf_tile_n128_k64(),
        )?;
        let repacked_weights = Cell::new(0);
        let tile_context = target_tile.then_some((&kernels, &stream, &repacked_weights));
        let sink: Arc<TieredWeightDevice> = Arc::new(TieredWeightDevice::new(
            device.clone(),
            cfg.weight_host_budget,
        ));
        let sink_dev: Arc<dyn Device> = sink.clone();
        let spill = Self::open_spill(&cfg, "gguf")?;
        let mut weights = ModelWeights::load_gguf(
            &sink_dev,
            path,
            cfg.native_mtp,
            tile_context,
            spill.as_ref(),
            cfg.weight_host_budget,
            cfg.layer_range,
            cfg.tp_shard,
        )?;
        Self::report_residency(sink.residency());
        Self::validate_nvfp4_tile_repacked(target_tile, repacked_weights.get())?;
        weights.nvfp4_repacked_weights = repacked_weights.get();
        Self::finish(sink_dev, weights, cfg, kernels, stream, spill)
    }

    pub fn load_safetensors_dir(
        device: Arc<dyn Device>,
        dir: &Path,
        cfg: ModelConfig,
    ) -> Result<Self> {
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;
        let target_tile = Self::nvfp4_tile_requested(
            cfg.nvfp4_gguf_layout,
            kernels.supports_nvfp4_gguf_tile_n128_k64(),
        )?;
        let repacked_weights = Cell::new(0);
        let tile_context = target_tile.then_some((&kernels, &stream, &repacked_weights));
        let sink: Arc<TieredWeightDevice> = Arc::new(TieredWeightDevice::new(
            device.clone(),
            cfg.weight_host_budget,
        ));
        let sink_dev: Arc<dyn Device> = sink.clone();
        let spill = Self::open_spill(&cfg, "safetensors")?;
        let mut weights = ModelWeights::load_safetensors_dir(
            &sink_dev,
            dir,
            cfg.native_mtp,
            tile_context,
            (&kernels, &stream, cfg.nvfp4_ct_layout),
            spill.as_ref(),
            cfg.weight_host_budget,
        )?;
        Self::report_residency(sink.residency());
        Self::validate_nvfp4_tile_repacked(target_tile, repacked_weights.get())?;
        weights.nvfp4_repacked_weights = repacked_weights.get();
        Self::finish(sink_dev, weights, cfg, kernels, stream, spill)
    }

    /// Otwiera plik zrzutu wag, gdy konfiguracja wskazała katalog.
    fn open_spill(cfg: &ModelConfig, tag: &str) -> Result<Option<ExpertSpill>> {
        let spill = cfg
            .weight_spill_dir
            .as_ref()
            .map(|dir| ExpertSpill::create(dir, tag))
            .transpose()?;
        if let Some(spill) = spill.as_ref() {
            tracing::info!(path = ?spill.path(), "otwarto plik zrzutu wag ekspertów");
        }
        Ok(spill)
    }

    pub fn nvfp4_gguf_layout_summary(&self) -> (Nvfp4GgufLayout, usize) {
        let count = self.weights.nvfp4_repacked_weights;
        let layout = if count == 0 {
            Nvfp4GgufLayout::RowMajor36
        } else {
            Nvfp4GgufLayout::TileN128K64
        };
        (layout, count)
    }

    /// Rezerwuje pamięć stanów targetu i MTP przed dopuszczeniem wielu requestów.
    pub fn preflight_hybrid_state_slots(&mut self, slots: usize) -> Result<()> {
        if slots == 0 {
            return Err(ForgeError::Scheduler(
                "preflight wymaga co najmniej jednego slotu".into(),
            ));
        }
        self.hybrid_states
            .as_mut()
            .ok_or_else(|| ForgeError::Unsupported("model nie jest hybrydowy".into()))?
            .ensure_capacity(slots)
    }

    pub(crate) fn nvfp4_ct_projection(weight: &DevWeight) -> Option<Nvfp4CtProjection> {
        let DevWeight::NvFp4 {
            storage: NvFp4CtStorage::S0N64K128 { .. },
            rows,
            cols,
            ..
        } = weight
        else {
            return None;
        };
        nvfp4_ct_projection_for_shape(*rows, *cols)
    }

    pub(crate) fn nvfp4_ct_model_capable(&self) -> bool {
        if !matches!(
            std::env::var("FORGE_NVFP4_CT_BM16").ok().as_deref(),
            None | Some("1")
        ) {
            return false;
        }
        let params = &self.weights.descriptor.params;
        let dimensions_capable = params
            .n_heads
            .checked_mul(params.head_dim)
            .zip(params.n_kv_heads.checked_mul(params.head_dim))
            .is_some_and(|(q_dim, kv_dim)| {
                nvfp4_ct_dimensions_capable(
                    params.hidden_size,
                    q_dim,
                    kv_dim,
                    params.intermediate_size,
                )
            });
        dimensions_capable
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && self.weights.layers.iter().all(|layer| {
                let QkvWeights::Fused(qkv) = &layer.attn().attn_qkv else {
                    return false;
                };
                let Ok(ffn) = layer.dense_ffn() else {
                    return false;
                };
                let GateUpWeights::Fused(gate_up) = &ffn.gate_up else {
                    return false;
                };
                Self::nvfp4_ct_projection(qkv) == Some(Nvfp4CtProjection::Qkv)
                    && Self::nvfp4_ct_projection(&layer.attn().attn_o)
                        == Some(Nvfp4CtProjection::Output)
                    && Self::nvfp4_ct_projection(gate_up) == Some(Nvfp4CtProjection::GateUp)
                    && Self::nvfp4_ct_projection(&ffn.down) == Some(Nvfp4CtProjection::Down)
            })
    }

    /// Build the W4A8 SmoothQuant packs from a one-time calibration pass over
    /// `calib_tokens` (a fixed built-in passage tokenized by the caller). Runs
    /// the coherent Q4_K prefill path collecting per-input-channel activation
    /// abs-max at the four linear inputs, then repacks every dense projection
    /// from the resident GGUF weights with per-channel migration folded into the
    /// weight (and the reciprocal into the GEMM's activation quantizer). Must be
    /// called once after load when `FORGE_GEMM=w4a8`; before it runs `w4a8` is
    /// `None` and prefill stays on the Q4_K path.
    pub fn calibrate_w4a8(&mut self, path: &Path, calib_tokens: &[u32]) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "W4A8 calibration supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        if calib_tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty W4A8 calibration input".into()));
        }
        let p = &self.weights.descriptor.params;
        let n_layers = self.weights.layers.len();
        let (hidden, q_dim, inter) = (p.hidden_size, p.max_q_dim(), p.intermediate_size);
        // Default is the identity requant (no SmoothQuant): measured best on the
        // Q4_K→W4A8 path, where the two-level requant error dominates and
        // migrating activation outliers only inflates the weights (see
        // docs/BENCH_COMPARISON.md). `FORGE_W4A8_ALPHA=<0..1>` opts into
        // SmoothQuant and triggers the calibration forward.
        let alpha = std::env::var("FORGE_W4A8_ALPHA")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(-1.0);

        // Ensure the Q4_K path runs during calibration (packs not built yet).
        self.weights.w4a8 = None;
        let stats = if alpha >= 0.0 {
            self.calib = Some(CalibAccum::new(n_layers, hidden, q_dim, inter));
            let mut seq = self.new_seq();
            let mut res = Ok(());
            for chunk in calib_tokens.chunks(MAX_PREFILL_CHUNK) {
                if let Err(e) = self.prefill_forward(&mut seq, chunk, true) {
                    res = Err(e);
                    break;
                }
            }
            self.release_seq(&mut seq);
            res?;
            let acc = self.calib.take().expect("calib accumulator set above");
            CalibStats {
                attn_in: acc.attn_in,
                attn_out: acc.attn_out,
                ffn_in: acc.ffn_in,
                down_in: acc.down_in,
                alpha,
            }
        } else {
            // Identity: smoothing_scale ignores the (unused) stats and returns 1.
            CalibStats {
                attn_in: vec![vec![0.0; hidden]; n_layers],
                attn_out: vec![vec![0.0; q_dim]; n_layers],
                ffn_in: vec![vec![0.0; hidden]; n_layers],
                down_in: vec![vec![0.0; inter]; n_layers],
                alpha,
            }
        };
        let layers = self
            .weights
            .rebuild_w4a8_smoothed(self.device.as_ref(), path, &stats)?;
        self.weights.w4a8 = Some(layers);
        Ok(())
    }


    /// Build the fp8 (e4m3) prefill packs from the resident GGUF weights. No
    /// calibration pass is needed (e4m3's exponent captures the per-row range),
    /// so this just dequantizes every dense projection and repacks it to e4m3
    /// with a per-row scale. Must be called once after load when
    /// `FORGE_GEMM=fp8`; before it runs `fp8` is `None` and prefill stays on the
    /// resident (Q4_K MMQ) path.
    pub fn build_fp8(&mut self, path: &Path) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "fp8 prefill supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        self.weights.fp8 = None;
        if !self.build_fp8_gpu()? {
            let layers = self.weights.rebuild_fp8(self.device.as_ref(), path)?;
            self.weights.fp8 = Some(layers);
        }
        self.weights.fp8_modular = crate::weights::fp8_modular_enabled();
        Ok(())
    }

    /// Pack one resident projection (row window) to e4m3 on the GPU.
    fn pack_fp8_gpu_window(
        &self,
        buf: &DevBuffer,
        quant: QuantKind,
        output_scale: f32,
        row_off: usize,
        rows: usize,
        cols: usize,
    ) -> Result<Fp8Weight> {
        let qweight = self
            .device
            .alloc(rows * cols, MemKind::Device, Pool::Weights)?;
        let scales = self
            .device
            .alloc(rows * 4, MemKind::Device, Pool::Weights)?;
        self.kernels.pack_gguf_fp8(
            &qweight,
            &scales,
            buf,
            row_off,
            rows,
            cols,
            quant,
            output_scale,
            &self.stream,
        )?;
        Ok(Fp8Weight {
            qweight,
            scales,
            rows,
            cols,
        })
    }

    /// Build the fp8 packs from the RESIDENT GGUF weights on the GPU (no disk
    /// re-read, no CPU dequant). Returns `Ok(false)` when any projection is
    /// not Q4_K/Q6_K/Q8_0 — the caller falls back to the CPU rebuild.
    pub fn build_fp8_gpu(&mut self) -> Result<bool> {
        let pack_full = |w: &DevWeight| -> Result<Option<Fp8Weight>> {
            let Some((buf, rows, cols, quant, output_scale)) = w.fp8_repack_source() else {
                return Ok(None);
            };
            self.pack_fp8_gpu_window(buf, quant, output_scale, 0, rows, cols)
                .map(Some)
        };
        let pack_window =
            |w: &DevWeight, row_off: usize, rows: usize| -> Result<Option<Fp8Weight>> {
                let Some((buf, _, cols, quant, output_scale)) = w.fp8_repack_source() else {
                    return Ok(None);
                };
                self.pack_fp8_gpu_window(buf, quant, output_scale, row_off, rows, cols)
                    .map(Some)
            };
        // Nothing is allocated before every projection passes the format
        // check, so a refusal leaves the weights pool untouched.
        for layer in &self.weights.layers {
            let LayerMixer::Attention(a) = &layer.mixer else {
                return Ok(false);
            };
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(false);
            };
            let mut ws: Vec<&DevWeight> = vec![&a.attn_o, &ffn.down];
            match &a.attn_qkv {
                QkvWeights::Split { q, k, v } => ws.extend([q, k, v]),
                QkvWeights::FusedQk { qk, v } => ws.extend([qk, v]),
                QkvWeights::Fused(qkv) => ws.push(qkv),
            }
            match &ffn.gate_up {
                GateUpWeights::Split { gate, up } => ws.extend([gate, up]),
                GateUpWeights::Fused(gu) => ws.push(gu),
            }
            if ws.into_iter().any(|w| w.fp8_repack_source().is_none()) {
                return Ok(false);
            }
        }
        let p = &self.weights.descriptor.params;
        let q_rows = p.n_heads * p.head_dim;
        let kv_rows = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let mut layers = Vec::with_capacity(self.weights.layers.len());
        for layer in &self.weights.layers {
            let LayerMixer::Attention(a) = &layer.mixer else {
                return Ok(false);
            };
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(false);
            };
            let (q, k, v) = match &a.attn_qkv {
                QkvWeights::Split { q, k, v } => (pack_full(q)?, pack_full(k)?, pack_full(v)?),
                QkvWeights::FusedQk { qk, v } => (
                    pack_window(qk, 0, q_rows)?,
                    pack_window(qk, q_rows, kv_rows)?,
                    pack_full(v)?,
                ),
                QkvWeights::Fused(qkv) => (
                    pack_window(qkv, 0, q_rows)?,
                    pack_window(qkv, q_rows, kv_rows)?,
                    pack_window(qkv, q_rows + kv_rows, kv_rows)?,
                ),
            };
            let (gate, up) = match &ffn.gate_up {
                GateUpWeights::Split { gate, up } => (pack_full(gate)?, pack_full(up)?),
                GateUpWeights::Fused(gu) => {
                    (pack_window(gu, 0, inter)?, pack_window(gu, inter, inter)?)
                }
            };
            let attn_o = pack_full(&a.attn_o)?;
            let down = pack_full(&ffn.down)?;
            match (q, k, v, attn_o, gate, up, down) {
                (Some(q), Some(k), Some(v), Some(attn_o), Some(gate), Some(up), Some(down)) => {
                    layers.push(Fp8Layer {
                        q,
                        k,
                        v,
                        attn_o,
                        gate,
                        up,
                        down,
                    });
                }
                _ => return Ok(false),
            }
        }
        self.stream.synchronize()?;
        self.weights.fp8 = Some(layers);
        self.weights.fp8_modular = true;
        Ok(true)
    }

    /// Auto-enable the Modular fp8 prefill for a dense GGUF model when the
    /// device has native fp8 tensor cores, every projection shape has a
    /// committed `gemm_fp8_mod_{rows}_{cols}` instance and the weights pool
    /// holds the e4m3 packs. Returns `Ok(false)` (prefill stays on the native
    /// GGUF path) when any gate fails; nothing is allocated before all gates
    /// pass, so a refusal leaves the model untouched.
    pub fn build_fp8_modular_auto(&mut self, path: &Path) -> Result<Fp8PackOutcome> {
        if self.is_hybrid() || self.weights.is_moe() || !self.device.caps().fp8_native {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let q_rows = p.n_heads * p.head_dim;
        let kv_rows = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        // (rows, cols, count) per layer: q, k, v, o, gate, up, down.
        let shapes = [
            (q_rows, hidden, 1usize),
            (kv_rows, hidden, 2),
            (hidden, q_rows, 1),
            (inter, hidden, 2),
            (hidden, inter, 1),
        ];
        let arts = self.kernels.artifacts();
        if shapes
            .iter()
            .any(|(rows, cols, _)| !arts.has(&format!("gemm_fp8_mod_{rows}_{cols}")))
        {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let per_layer: usize = shapes
            .iter()
            .map(|(rows, cols, n)| n * (rows * cols + rows * 4))
            .sum();
        let required = per_layer * self.weights.descriptor.params.block_count;
        let available = self.device.pool_available(Pool::Weights).unwrap_or(0);
        tracing::info!(required, available, "preflight paczek fp8mod dla GGUF");
        if required > available {
            return Ok(Fp8PackOutcome::PoolShortfall {
                required,
                available,
            });
        }
        self.weights.fp8 = None;
        if self.build_fp8_gpu()? {
            return Ok(Fp8PackOutcome::Built);
        }
        let layers = self.weights.rebuild_fp8(self.device.as_ref(), path)?;
        self.weights.fp8 = Some(layers);
        self.weights.fp8_modular = true;
        Ok(Fp8PackOutcome::Built)
    }

    fn pack_nvfp4_rows(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Result<Fp8Weight> {
        let DevWeight::NvFp4 {
            storage,
            inv_global_scale,
            rows: source_rows,
            cols,
        } = weight
        else {
            return Err(ForgeError::Unsupported(
                "fp8mod-ffn wymaga rezydentnych wag NVFP4".into(),
            ));
        };
        let row_end = row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Format("przepełnienie zakresu wierszy FP8".into()))?;
        if row_end > *source_rows {
            return Err(ForgeError::Format(format!(
                "zakres wierszy FP8 {}..{} przekracza {source_rows}",
                row_offset, row_end
            )));
        }
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let output_scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        let launch_result = match storage {
            NvFp4CtStorage::RowMajorE4M3 { packed, scales } => self.kernels.pack_nvfp4_fp8(
                &qweight,
                &output_scales,
                packed,
                scales,
                *cols,
                row_offset,
                rows,
                *inv_global_scale,
                &self.stream,
            ),
            NvFp4CtStorage::S0N64K128 { .. } => {
                let window = weight.nvfp4_ct_row_window(row_offset, rows)?;
                let view =
                    Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
                self.kernels.pack_nvfp4_ct_s0_fp8(
                    &qweight,
                    &output_scales,
                    view,
                    window.row_offset(),
                    window.rows(),
                    *inv_global_scale,
                    &self.stream,
                )
            }
        };
        cleanup_after_error(launch_result, || {
            let _ = self.stream.synchronize();
        })?;
        Ok(Fp8Weight {
            qweight,
            scales: output_scales,
            rows,
            cols: *cols,
        })
    }

    fn pack_f16_weight(&self, weight: &DevWeight) -> Result<Fp8Weight> {
        let DevWeight::F16 { buf, rows, cols } = weight else {
            return Err(ForgeError::Unsupported(
                "przepakowanie lm_head FP8 wymaga źródła F16".into(),
            ));
        };
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        let launch_result =
            self.kernels
                .pack_f16_fp8(&qweight, &scales, buf, *cols, *rows, &self.stream);
        cleanup_after_error(launch_result, || {
            let _ = self.stream.synchronize();
        })?;
        Ok(Fp8Weight {
            qweight,
            scales,
            rows: *rows,
            cols: *cols,
        })
    }

    fn fp8_pack_allocation_bytes(rows: usize, cols: usize) -> Option<usize> {
        let weight = rows
            .checked_mul(cols)?
            .max(1)
            .checked_next_multiple_of(256)?;
        let scales = rows.checked_mul(4)?.max(1).checked_next_multiple_of(256)?;
        weight.checked_add(scales)
    }

    fn preflight_nvfp4_pack(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Option<usize> {
        let DevWeight::NvFp4 {
            storage,
            rows: source_rows,
            cols,
            ..
        } = weight
        else {
            return None;
        };
        let row_end = row_offset.checked_add(rows)?;
        if rows == 0
            || row_end > *source_rows
            || !self.kernels.supports_fp8_modular_shape(rows, *cols)
        {
            return None;
        }
        match storage {
            NvFp4CtStorage::RowMajorE4M3 { .. } if !cols.is_multiple_of(16) => return None,
            NvFp4CtStorage::RowMajorE4M3 { .. } => {}
            NvFp4CtStorage::S0N64K128 { .. } => {
                weight.nvfp4_ct_row_window(row_offset, rows).ok()?;
            }
        }
        Self::fp8_pack_allocation_bytes(rows, *cols)
    }

    fn fp8_build_step<T>(&self, result: Result<T>) -> Result<T> {
        cleanup_after_error(result, || {
            let _ = self.stream.synchronize();
        })
    }

    /// Buduje na GPU opt-in paczki FP8 dla Q/O oraz projekcji FFN checkpointu NVFP4.
    pub fn build_fp8_ffn(&mut self) -> Result<Fp8PackOutcome> {
        if self.weights.fp8_ffn.is_some() {
            return Ok(Fp8PackOutcome::Built);
        }
        if !self.device.caps().fp8_native
            || !self.kernels.supports_fp8_hybrid_packers()
            || !self.kernels.supports_fp8_logits()
        {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        if self.is_hybrid() || self.weights.is_moe() {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let mut required_bytes = 0usize;
        let mut add_required = |bytes: Option<usize>| -> Option<()> {
            required_bytes = required_bytes.checked_add(bytes?)?;
            Some(())
        };
        let params = &self.weights.descriptor.params;
        let Some(q_rows) = params.n_heads.checked_mul(params.head_dim) else {
            return Ok(Fp8PackOutcome::Unsupported);
        };
        for layer in &self.weights.layers {
            let q_source = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => weight,
                QkvWeights::Split { q, .. } => q,
            };
            let kv_rows = params.n_kv_heads * params.head_dim;
            let kv_ok = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    add_required(self.preflight_nvfp4_pack(w, q_rows, kv_rows)).is_some()
                        && add_required(self.preflight_nvfp4_pack(w, q_rows + kv_rows, kv_rows))
                            .is_some()
                }
                QkvWeights::FusedQk { qk, v } => {
                    add_required(self.preflight_nvfp4_pack(qk, q_rows, kv_rows)).is_some()
                        && add_required(self.preflight_nvfp4_pack(v, 0, v.rows())).is_some()
                }
                QkvWeights::Split { k, v, .. } => {
                    add_required(self.preflight_nvfp4_pack(k, 0, k.rows())).is_some()
                        && add_required(self.preflight_nvfp4_pack(v, 0, v.rows())).is_some()
                }
            };
            if add_required(self.preflight_nvfp4_pack(q_source, 0, q_rows)).is_none()
                || !kv_ok
                || add_required(self.preflight_nvfp4_pack(
                    &layer.attn().attn_o,
                    0,
                    layer.attn().attn_o.rows(),
                ))
                .is_none()
            {
                return Ok(Fp8PackOutcome::Unsupported);
            }
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(Fp8PackOutcome::Unsupported);
            };
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    if weight.rows() % 2 != 0 {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                    let rows = weight.rows() / 2;
                    if add_required(self.preflight_nvfp4_pack(weight, 0, rows)).is_none()
                        || add_required(self.preflight_nvfp4_pack(weight, rows, rows)).is_none()
                    {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                }
                GateUpWeights::Split { gate, up } => {
                    if add_required(self.preflight_nvfp4_pack(gate, 0, gate.rows())).is_none()
                        || add_required(self.preflight_nvfp4_pack(up, 0, up.rows())).is_none()
                    {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                }
            }
            if add_required(self.preflight_nvfp4_pack(&ffn.down, 0, ffn.down.rows())).is_none() {
                return Ok(Fp8PackOutcome::Unsupported);
            }
        }
        let fp8_head_supported = match &self.weights.lm_head {
            DevWeight::F16 { rows, cols, .. } => {
                if !cols.is_multiple_of(256) {
                    false
                } else if add_required(Self::fp8_pack_allocation_bytes(*rows, *cols)).is_none() {
                    return Ok(Fp8PackOutcome::Unsupported);
                } else {
                    true
                }
            }
            _ => false,
        };
        let Some(available) = self.device.pool_available(Pool::Weights) else {
            return Ok(Fp8PackOutcome::Unsupported);
        };
        tracing::info!(
            required_bytes,
            available_bytes = available,
            "preflight rezydentnych paczek FP8"
        );
        if required_bytes > available {
            return Ok(Fp8PackOutcome::PoolShortfall {
                required: required_bytes,
                available,
            });
        }

        self.device.synchronize()?;
        let mut layers = Vec::with_capacity(self.weights.layers.len());
        for layer in &self.weights.layers {
            let q = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => {
                    self.fp8_build_step(self.pack_nvfp4_rows(weight, 0, q_rows))?
                }
                QkvWeights::Split { q, .. } => {
                    self.fp8_build_step(self.pack_nvfp4_rows(q, 0, q.rows()))?
                }
            };
            let kv_rows = self.weights.descriptor.params.n_kv_heads
                * self.weights.descriptor.params.head_dim;
            let (k, v) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => (
                    self.fp8_build_step(self.pack_nvfp4_rows(w, q_rows, kv_rows))?,
                    self.fp8_build_step(self.pack_nvfp4_rows(w, q_rows + kv_rows, kv_rows))?,
                ),
                QkvWeights::FusedQk { qk, v } => (
                    self.fp8_build_step(self.pack_nvfp4_rows(qk, q_rows, kv_rows))?,
                    self.fp8_build_step(self.pack_nvfp4_rows(v, 0, v.rows()))?,
                ),
                QkvWeights::Split { k, v, .. } => (
                    self.fp8_build_step(self.pack_nvfp4_rows(k, 0, k.rows()))?,
                    self.fp8_build_step(self.pack_nvfp4_rows(v, 0, v.rows()))?,
                ),
            };
            let attn_o = self.fp8_build_step(self.pack_nvfp4_rows(
                &layer.attn().attn_o,
                0,
                layer.attn().attn_o.rows(),
            ))?;
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                unreachable!("modele MoE zostały odrzucone przed przepakowaniem")
            };
            let (gate, up) = match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    let rows = weight.rows() / 2;
                    (
                        self.fp8_build_step(self.pack_nvfp4_rows(weight, 0, rows))?,
                        self.fp8_build_step(self.pack_nvfp4_rows(weight, rows, rows))?,
                    )
                }
                GateUpWeights::Split { gate, up } => (
                    self.fp8_build_step(self.pack_nvfp4_rows(gate, 0, gate.rows()))?,
                    self.fp8_build_step(self.pack_nvfp4_rows(up, 0, up.rows()))?,
                ),
            };
            let down = self.fp8_build_step(self.pack_nvfp4_rows(&ffn.down, 0, ffn.down.rows()))?;
            layers.push(crate::weights::Fp8FfnLayer {
                q,
                k,
                v,
                attn_o,
                gate,
                up,
                down,
            });
        }
        let fp8_lm_head = match (&self.weights.lm_head, fp8_head_supported) {
            (DevWeight::F16 { .. }, true) => {
                Some(self.fp8_build_step(self.pack_f16_weight(&self.weights.lm_head))?)
            }
            _ => None,
        };
        cleanup_after_error(self.stream.synchronize(), || {
            let _ = self.device.synchronize();
        })?;
        tracing::info!(
            resident_pack_bytes = required_bytes,
            layer_count = layers.len(),
            kv_packs = 0,
            available_after_bytes = self.device.pool_available(Pool::Weights),
            "paczki FP8 gotowe do publikacji"
        );
        self.weights.fp8_lm_head = fp8_lm_head;
        self.weights.fp8_ffn = Some(layers);
        self.weights.fp8_modular = crate::weights::fp8_ffn_modular_enabled();
        self.decode_graph = None;
        self.decode_hybrid_graph = None;
        self.decode_moe_graph = None;
        self.decode_rot_graph = None;
        self.batch_graphs.clear();
        Ok(Fp8PackOutcome::Built)
    }

}
