// ===== File: model/arch/hybrid/prefill.rs — prefill hybrydowy =====
use super::super::super::*;

fn hybrid_layer_major_tiled_prepare_requested() -> bool {
    std::env::var("FORGE_HYBRID_LAYER_MAJOR_DELTA_PREPARE")
        .map_or(true, |value| value != "segmented")
}

impl Model {
    /// Sprawdza target hybrydowy niezależnie od źródła tokenów draftu.
    /// Warunki BATCHOWEGO prefillu hybrydowego — te same co dla verifiera
    /// spekulacyjnego, ale BEZ wymogu na format glowy logitow.
    ///
    /// Prefill zwraca logity wylacznie OSTATNIEGO tokenu i liczy je zwykla
    /// sciezka `logits_gemv`, ktora obsluguje kazdy format. Wymog F16/Q8_0
    /// nalezy do verifiera, ktory liczy logity dla T pozycji naraz batchowym
    /// headem. Zwiazanie tych dwoch rzeczy jednym warunkiem kosztowalo bardzo
    /// duzo: GGUF Q4_K_M ma glowe Q6_K (konwencja llama.cpp), wiec KAZDY taki
    /// model hybrydowy schodzil na prefill token po tokenie. Qwen3.6-27B Q4_K_M:
    /// 28,4 wobec 200,2 tok/s prefillu, przy wyjsciu identycznym co do bajtu.
    pub(crate) fn hybrid_batched_prefill_capable(&self) -> bool {
        // Pozyczony prefiks znaczy tylko niezerowy `seq.len` — prefill dopisuje.
        self.is_hybrid()
            && self.weights.descriptor.params.head_dim == 256
            && matches!(self.kv.cfg.quant, KvQuant::F16)
            && self.tier.is_none()
    }

    fn ensure_hybrid_prefill_b2_bufs(&mut self) -> Result<()> {
        if self.hybrid_prefill_b2_bufs.is_some() {
            return Ok(());
        }
        const BATCH: usize = 2;
        const STEPS: usize = 32;
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("prefill B2 T32 wymaga targetu hybrydowego".into())
        })?;
        let elements = |name: &str, dimensions: &[usize]| {
            dimensions.iter().try_fold(1usize, |total, &dimension| {
                total.checked_mul(dimension).ok_or_else(|| {
                    ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {dimensions:?}"))
                })
            })
        };
        let bytes = |name: &str, dimensions: &[usize], element_bytes: usize| {
            elements(name, dimensions)?
                .checked_mul(element_bytes)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie bufora {name}")))
        };
        let total = elements("prefill B2 total", &[BATCH, STEPS])?;
        let q_dim = elements("prefill B2 q", &[p.n_heads, p.head_dim])?;
        let kv_dim = elements("prefill B2 kv", &[p.n_kv_heads, p.head_dim])?;
        let key_dim = elements("prefill B2 key", &[ssm.d_state, ssm.n_group])?;
        let n_v = ssm.n_v_heads();
        let value_dim = elements("prefill B2 value", &[ssm.d_state, n_v])?;
        let conv_dim = key_dim
            .checked_mul(2)
            .and_then(|value| value.checked_add(value_dim))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie conv prefill B2".into()))?;
        let conv_history = ssm
            .d_conv
            .checked_sub(1)
            .ok_or_else(|| ForgeError::Scheduler("prefill B2 wymaga d_conv > 0".into()))?;
        let conv_elems = elements("prefill B2 conv state", &[conv_dim, conv_history])?;
        let state_elems = elements("prefill B2 state", &[n_v, ssm.d_state, ssm.d_state])?;
        let q_full_cols = hybrid_q_full_cols(q_dim, conv_dim, p.hidden_size);
        let page_table_elems =
            elements("prefill B2 page tables", &[BATCH, self.max_pages_per_seq])?;
        let metadata_elems = elements("prefill B2 metadata rows", &[3, total])?
            .checked_add(BATCH)
            .and_then(|value| value.checked_add(page_table_elems))
            .and_then(|value| value.checked_add(BATCH * 2))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie metadata prefill B2".into()))?;
        let mut required = 0usize;
        let mut reserve = |name: &str, dimensions: &[usize], element_bytes: usize| -> Result<()> {
            let allocation = bytes(name, dimensions, element_bytes)?
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie alokacji {name}")))?;
            required = required.checked_add(allocation).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie preflightu prefill B2".into())
            })?;
            Ok(())
        };
        for (name, rows, cols, element_bytes) in [
            ("h", total, p.hidden_size, 2),
            ("x", total, p.hidden_size, 2),
            ("k", total, kv_dim, 2),
            ("v", total, kv_dim, 2),
            ("attn", total, q_dim, 2),
            ("o_out", total, p.hidden_size, 2),
            ("gate", total, p.intermediate_size, 2),
            ("up", total, p.intermediate_size, 2),
            ("down", total, p.hidden_size, 2),
            ("q_full", total, q_full_cols, 2),
            ("qc", total, q_dim.max(value_dim), 2),
            ("gatec", total, q_dim.max(value_dim), 2),
            ("gated", total, q_dim.max(value_dim), 2),
            ("qkv_mixed", total, conv_dim, 2),
            ("z", total, value_dim, 2),
            ("alpha", total, n_v, 2),
            ("beta_raw", total, n_v, 2),
            ("recurrence", total, value_dim, 2),
            ("normed", total, value_dim, 2),
        ] {
            reserve(name, &[rows, cols], element_bytes)?;
        }
        reserve("ids", &[total], 4)?;
        reserve("positions", &[total], 4)?;
        reserve("page tables", &[page_table_elems], 4)?;
        reserve("base positions", &[BATCH], 4)?;
        reserve("visible lengths", &[total], 4)?;
        reserve("decisions", &[BATCH, 2], 4)?;
        reserve("final hidden", &[BATCH, p.hidden_size], 2)?;
        reserve("logits", &[BATCH, p.vocab_size], 4)?;
        reserve("pinned metadata", &[metadata_elems], 4)?;
        reserve("pinned logits", &[BATCH, p.vocab_size], 4)?;
        reserve("final conv", &[BATCH, conv_elems], 2)?;
        reserve("final states", &[BATCH, state_elems], 4)?;
        let delta_layers = self
            .weights
            .layers
            .iter()
            .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
            .count();
        let mut per_delta = 0usize;
        for (name, dimensions, element_bytes) in [
            ("conv initial", vec![BATCH, conv_elems], 2),
            ("state initial", vec![BATCH, state_elems], 4),
            ("delta q", vec![total, value_dim], 2),
            ("delta k", vec![total, value_dim], 2),
            ("delta v", vec![total, value_dim], 2),
            ("delta g", vec![total, n_v], 4),
            ("delta beta", vec![total, n_v], 4),
        ] {
            let allocation = bytes(name, &dimensions, element_bytes)?
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie alokacji {name}")))?;
            per_delta = per_delta.checked_add(allocation).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie scratchu warstwy Delta prefill B2".into())
            })?;
        }
        required = per_delta
            .checked_mul(delta_layers)
            .and_then(|delta| required.checked_add(delta))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu prefill B2".into()))?;
        if self
            .device
            .pool_available(Pool::Activations)
            .is_some_and(|available| required > available)
        {
            return Err(ForgeError::OutOfMemory {
                requested: required,
                available: self.device.pool_available(Pool::Activations).unwrap_or(0),
            });
        }

        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let gate = a16("prefill B2 gate", &[total, p.intermediate_size])?;
        let delta = self
            .weights
            .layers
            .iter()
            .map(|layer| match layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(_) => Ok(None),
                LayerMixer::DeltaNet(_) => Ok(Some(HybridPrefillB2DeltaCache {
                    conv_initial: a16("prefill B2 conv initial", &[BATCH, conv_elems])?,
                    state_initial: a32("prefill B2 state initial", &[BATCH, state_elems])?,
                    q: a16("prefill B2 delta q", &[total, value_dim])?,
                    k: a16("prefill B2 delta k", &[total, value_dim])?,
                    v: a16("prefill B2 delta v", &[total, value_dim])?,
                    g: a32("prefill B2 delta g", &[total, n_v])?,
                    beta: a32("prefill B2 delta beta", &[total, n_v])?,
                })),
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_prefill_b2_bufs = Some(HybridPrefillB2Bufs {
            h: a16("prefill B2 h", &[total, p.hidden_size])?,
            x: a16("prefill B2 x", &[total, p.hidden_size])?,
            k: a16("prefill B2 k", &[total, kv_dim])?,
            v: a16("prefill B2 v", &[total, kv_dim])?,
            attn_out: a16("prefill B2 attention", &[total, q_dim])?,
            o_out: a16("prefill B2 mixer output", &[total, p.hidden_size])?,
            gate: gate.clone(),
            up: a16("prefill B2 up", &[total, p.intermediate_size])?,
            act: gate,
            down: a16("prefill B2 down", &[total, p.hidden_size])?,
            ids: a32("prefill B2 ids", &[total])?,
            positions: a32("prefill B2 positions", &[total])?,
            q_full: a16("prefill B2 q full", &[total, q_full_cols])?,
            qc: a16("prefill B2 qc", &[total, q_dim.max(value_dim)])?,
            gatec: a16("prefill B2 gatec", &[total, q_dim.max(value_dim)])?,
            gated: a16("prefill B2 gated", &[total, q_dim.max(value_dim)])?,
            qkv_mixed: a16("prefill B2 qkv mixed", &[total, conv_dim])?,
            z: a16("prefill B2 z", &[total, value_dim])?,
            alpha: a16("prefill B2 alpha", &[total, n_v])?,
            beta_raw: a16("prefill B2 beta raw", &[total, n_v])?,
            o: a16("prefill B2 recurrence", &[total, value_dim])?,
            normed: a16("prefill B2 recurrence norm", &[total, value_dim])?,
            page_tables: a32("prefill B2 page tables", &[page_table_elems])?,
            base_positions: a32("prefill B2 base positions", &[BATCH])?,
            visible_lens: a32("prefill B2 visible lengths", &[total])?,
            decisions: a32("prefill B2 decisions", &[BATCH, 2])?,
            final_hidden: a16("prefill B2 final hidden", &[BATCH, p.hidden_size])?,
            logits: a32("prefill B2 logits", &[BATCH, p.vocab_size])?,
            pinned_metadata: alloc_checked(
                device.as_ref(),
                "prefill B2 pinned metadata",
                &[metadata_elems],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_logits: alloc_checked(
                device.as_ref(),
                "prefill B2 pinned logits",
                &[BATCH, p.vocab_size],
                4,
                MemKind::PinnedHost,
            )?,
            final_conv: a16("prefill B2 final conv", &[BATCH, conv_elems])?,
            final_states: a32("prefill B2 final states", &[BATCH, state_elems])?,
            delta,
        });
        Ok(())
    }

    pub(crate) fn commit_hybrid_prefill_delta_layer(&self, layer_index: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrydowego prefill są gotowe");
        let cache = hv.delta[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma cache skanu");
        let state = self.active_ssm()[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        self.kernels.mtp_select_row_f16(
            &state.conv,
            &cache.conv_checkpoints,
            &hv.accepted,
            ssm.conv_dim() * (ssm.d_conv - 1),
            &self.stream,
        )
    }

    pub(crate) fn hybrid_prefill_contains_nvfp4(&self) -> bool {
        self.weights.layers.iter().any(|layer| {
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return false;
            };
            let gate_up = match &ffn.gate_up {
                GateUpWeights::Fused(weight) => matches!(weight, DevWeight::NvFp4Gguf { .. }),
                GateUpWeights::Split { gate, up } => {
                    matches!(gate, DevWeight::NvFp4Gguf { .. })
                        || matches!(up, DevWeight::NvFp4Gguf { .. })
                }
            };
            gate_up || matches!(ffn.down, DevWeight::NvFp4Gguf { .. })
        })
    }

    pub(crate) fn hybrid_prefill_scratch_shape(&self) -> Option<HybridPrefillScratchShape> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref()?;
        Some(HybridPrefillScratchShape {
            hidden: p.hidden_size,
            q_dim: p.n_heads.checked_mul(p.head_dim)?,
            kv_dim: p.n_kv_heads.checked_mul(p.head_dim)?,
            inter: p.intermediate_size,
            conv_dim: ssm.conv_dim(),
            value_dim: ssm.value_dim(),
            n_v_heads: ssm.n_v_heads(),
            d_state: ssm.d_state,
            d_conv: ssm.d_conv,
            delta_layers: self
                .weights
                .layers
                .iter()
                .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
                .count(),
            max_pages_per_seq: self.max_pages_per_seq,
        })
    }

    /// Kształt wymagany przez OBIE macierzowe ścieżki prefillu. Postać wagi do
    /// niego nie należy: projekcje liczą `gemm` i `ffn_dense_block`, a te
    /// rozpoznają format same.
    pub(crate) fn hybrid_prefill_shape_capable(&self) -> bool {
        let caps = self.device.caps();
        hybrid_prefill_t128_backend_capable(caps.vendor, caps.warp_size)
            && caps.max_threads_per_block >= 512
            && self.weights.descriptor.arch == "qwen35"
            && self.weights.token_embd_host.is_some()
            && self.validate_hybrid_speculation_target().is_ok()
            && self.weights.layers.iter().all(|l| matches!(l.ffn, LayerFfn::Dense(_)))
            && (self.weights.descriptor.params.ssm.as_ref())
                .is_some_and(|ssm| ssm.d_state == 128)
    }

    /// Dodatkowo cały FFN w NVFP4 — tego wymaga dobór chunka NVFP4, nie kernele.
    pub(crate) fn hybrid_prefill_extended_structural_capable(&self) -> bool {
        self.hybrid_prefill_shape_capable()
            && self.weights.layers.iter().all(|layer| {
                let nvfp4 = |w: &DevWeight| matches!(w, DevWeight::NvFp4Gguf { .. });
                let LayerFfn::Dense(ffn) = &layer.ffn else { return false };
                let gate_up = match &ffn.gate_up {
                    GateUpWeights::Fused(w) => nvfp4(w),
                    GateUpWeights::Split { gate, up } => nvfp4(gate) && nvfp4(up),
                };
                gate_up && nvfp4(&ffn.down)
            })
    }

    pub(crate) fn hybrid_prefill_t128_structural_capable(&self) -> bool {
        self.hybrid_prefill_extended_structural_capable()
            && self.kernels.hybrid_prefill_t128_artifacts_capable()
    }

    pub(crate) fn hybrid_prefill_extended_budget_capable(&self, chunk: usize) -> bool {
        let Some(shape) = self.hybrid_prefill_scratch_shape() else {
            return false;
        };
        let Ok(estimate) = hybrid_prefill_scratch_estimate(shape, chunk) else {
            return false;
        };
        hybrid_prefill_activation_budget_capable(
            estimate,
            self.device.pool_available(Pool::Activations),
        )
    }

    pub(crate) fn resolve_hybrid_prefill_chunk_size(&self, config: HybridPrefillChunkConfig) -> Result<usize> {
        if !self.is_hybrid() {
            return Ok(HYBRID_PREFILL_PORTABLE_CHUNK);
        }
        let caps = self.device.caps();
        let nvfp4_chunk_limit = hybrid_prefill_nvfp4_chunk_limit(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
        );
        let artifact_chunk_limit = self.kernels.hybrid_prefill_nvfp4_artifact_chunk_limit();
        let extended_capable = self.hybrid_prefill_extended_structural_capable();
        let executable_chunk_limit = nvfp4_chunk_limit.min(artifact_chunk_limit);
        let supported_limit = executable_chunk_limit.min(HYBRID_PREFILL_AUTO_CHUNK);
        let budget_chunk_limit = if extended_capable && supported_limit > 16 {
            [128, 32]
                .into_iter()
                .find(|&chunk| {
                    chunk <= supported_limit && self.hybrid_prefill_extended_budget_capable(chunk)
                })
                .unwrap_or(HYBRID_PREFILL_PORTABLE_CHUNK)
        } else {
            supported_limit.min(HYBRID_PREFILL_PORTABLE_CHUNK)
        };
        let auto_chunk_limit = supported_limit.min(budget_chunk_limit);
        // Osobny limit dla modeli bez NVFP4: ta sama miara budzetu scratcha,
        // ale bez sufitu `HYBRID_PREFILL_AUTO_CHUNK`, ktory dotyczy artefaktow
        // NVFP4.
        let legacy_chunk_limit = HYBRID_PREFILL_LADDER
            .into_iter()
            .find(|&chunk| self.hybrid_prefill_extended_budget_capable(chunk))
            .unwrap_or(HYBRID_PREFILL_LEGACY_CHUNK);
        resolve_hybrid_prefill_chunk_size(
            config,
            extended_capable,
            self.hybrid_prefill_t128_structural_capable()
                && auto_chunk_limit >= HYBRID_PREFILL_AUTO_CHUNK,
            self.hybrid_prefill_contains_nvfp4(),
            auto_chunk_limit,
            executable_chunk_limit,
            legacy_chunk_limit,
            self.kernels.prepared_q8_tiled_capable(),
        )
    }

    pub(crate) fn ensure_hybrid_prefill_capacity(&mut self, cap: usize) -> Result<()> {
        self.ensure_prefill_bufs()?;
        self.ensure_hybrid_verify_bufs(4)?;
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        let result = self.ensure_hybrid_verify_bufs(cap.max(4));
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        result
    }

    /// Sprawdza semantyczny kontrakt kerneli eksperymentalnego prefill B2 T32.
    pub fn hybrid_prefill_b2_capable(&self, chunk_tokens: usize) -> bool {
        let Some(ssm) = self.weights.descriptor.params.ssm.as_ref() else {
            return false;
        };
        let device_embedding = self.weights.mtp.as_ref().is_some_and(|mtp| {
            mtp.shares_target_embedding
                && matches!(
                    mtp.embedding,
                    MtpEmbedding::Device(DevWeight::F16 { .. })
                        | MtpEmbedding::Device(DevWeight::Q8_0 { .. })
                        | MtpEmbedding::Device(DevWeight::NvFp4Gguf { .. })
                )
        });
        let split_layout = self.weights.layers.iter().all(|layer| {
            let attention_ok = match &layer.mixer {
                // DeepSeek V4 ma własną ścieżkę; hybrydowe zdolności go nie dotyczą.
                LayerMixer::DeepseekAttention(_) => false,
                LayerMixer::Attention(attention) => {
                    matches!(attention.attn_qkv, QkvWeights::Split { .. })
                }
                LayerMixer::DeltaNet(_) => true,
            };
            let ffn_ok = match &layer.ffn {
                LayerFfn::Dense(ffn) => matches!(ffn.gate_up, GateUpWeights::Split { .. }),
                LayerFfn::Moe(_) => false,
            };
            attention_ok && ffn_ok
        });
        hybrid_prefill_b2_backend_capable(self.device.caps().vendor, self.device.caps().warp_size)
            && self.kernels.hybrid_prefill_b2_artifacts_capable()
            && chunk_tokens == 32
            && self.weights.descriptor.params.head_dim == 256
            && ssm.d_state == 128
            && ssm.d_conv > 0
            && !self.weights.is_moe()
            && matches!(self.kv.cfg.quant, KvQuant::F16)
            && self.tier.is_none()
            // Para B2 rolluje sie razem; z pozyczka niemierzona.
            && self.prefix_cache.is_none()
            && device_embedding
            && split_layout
            && self.hybrid_batch_weights_capable()
    }

    pub fn hybrid_layer_major_prefill_limit(&self) -> Option<usize> {
        if !self.hybrid_layer_major_route_capable() {
            return None;
        }
        let shape = self.hybrid_prefill_scratch_shape()?;
        let available = self.device.pool_available(Pool::Activations)?.checked_add(
            self.hybrid_layer_major_bufs
                .as_ref()
                .map_or(0, |bufs| bufs.device_bytes),
        )?;
        let budget_limit = [4096, 2048, 1024, 512, 128, 32]
            .into_iter()
            .find(|&tokens| {
                hybrid_layer_major_scratch_estimate(shape, tokens)
                    .ok()
                    .and_then(|bytes| bytes.checked_add(HYBRID_PREFILL_ACTIVATION_RESERVE))
                    .is_some_and(|required| required <= available)
            });
        budget_limit
            .into_iter()
            .chain(self.hybrid_layer_major_bufs.as_ref().map(|bufs| bufs.cap))
            .max()
    }

    /// Wykonuje atomowy target-only prefill dwóch segmentów T32 bez catch-up MTP.
    pub fn hybrid_prefill_b2_t32(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<[Vec<f32>; 2]> {
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, None, true)
    }

    /// Wykonuje target prefill B2, pozostawiając oba wiersze logits na urządzeniu.
    pub fn hybrid_prefill_b2_t32_device(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<()> {
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, None, false)
            .map(drop)
    }

    /// Próbkuje wskazany wiersz logits ostatniego prefill B2 na GPU.
    pub fn sample_hybrid_prefill_b2_logits(
        &mut self,
        lane: usize,
        sampler: &mut GpuSampler,
    ) -> Result<u32> {
        if lane >= 2 {
            return Err(ForgeError::Scheduler(
                "logity prefill B2 wymagają lane 0 lub 1".into(),
            ));
        }
        let logits = &self
            .hybrid_prefill_b2_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak logits prefill B2".into()))?
            .logits;
        let bytes = self.weights.descriptor.params.vocab_size * 4;
        self.device.copy(
            logits,
            lane * bytes,
            &self.bufs.logits,
            0,
            bytes,
            &self.stream,
        )?;
        self.sample_last_logits(sampler)
    }

    /// Próbkuje oba wiersze prefill B2 na GPU i odczytuje tylko dwa ID.
    pub fn sample_hybrid_prefill_b2_logits_batched(
        &mut self,
        samplers: &mut [&mut GpuSampler; 2],
    ) -> Result<[u32; 2]> {
        const BATCH: usize = 2;
        self.ensure_batch(BATCH)?;
        let vocab = self.weights.descriptor.params.vocab_size;
        let [first, second] = samplers;
        let params = [first.batch_params(vocab), second.batch_params(vocab)];
        let logits = self
            .hybrid_prefill_b2_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak logits prefill B2".into()))?
            .logits
            .clone();
        self.batch_sample_from(&logits, BATCH, &params)?;
        let batch = self.batch_bufs.as_ref().expect("batch sampler ma bufory");
        self.device.copy(
            &batch.out_ids,
            0,
            &batch.pinned_out,
            0,
            BATCH * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;
        let output = batch
            .pinned_out
            .host_ptr()
            .expect("pinned output ma mapowanie") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(output, BATCH) };
        let mut result = [0u32; BATCH];
        for lane in 0..BATCH {
            let id = ids[lane];
            if id < 0 || id as usize >= vocab {
                return Err(ForgeError::Kernel(format!(
                    "batch sampler prefill zwrócił token {id} poza słownikiem dla lane {lane}"
                )));
            }
            result[lane] = id as u32;
        }
        Ok(result)
    }

    pub(crate) fn hybrid_prefill_b2_t32_inner(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        fail_after_state_commit: Option<usize>,
        read_host_logits: bool,
    ) -> Result<[Vec<f32>; 2]> {
        const BATCH: usize = 2;
        const STEPS: usize = 32;
        const TOTAL: usize = BATCH * STEPS;
        if tokens.iter().any(|lane| lane.len() != STEPS) {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 wymaga dokładnie 32 tokenów w każdym lane".into(),
            ));
        }
        if !self.hybrid_prefill_b2_capable(STEPS) {
            return Err(ForgeError::Unsupported(
                "model nie spełnia semantycznego kontraktu prefill B2 T32".into(),
            ));
        }
        if seqs[0].id == seqs[1].id {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 wymaga dwóch różnych sekwencji".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        if tokens
            .iter()
            .flat_map(|lane| lane.iter())
            .any(|&token| token as usize >= p.vocab_size)
        {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 otrzymał token poza słownikiem".into(),
            ));
        }
        let bases = [seqs[0].len, seqs[1].len];
        let ends = [
            bases[0].checked_add(STEPS).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji prefill B2 T32 lane0".into())
            })?,
            bases[1].checked_add(STEPS).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji prefill B2 T32 lane1".into())
            })?,
        ];
        for end in ends {
            if end > p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    end - 1,
                    p.max_position_embeddings
                )));
            }
        }
        let required_pages = seqs
            .iter()
            .enumerate()
            .try_fold(0usize, |total, (lane, seq)| {
                let pages = ends[lane]
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                total.checked_add(pages).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie stron prefill B2 T32".into())
                })
            })?;
        self.ensure_free_pages(required_pages);
        if required_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "prefill B2 T32 wymaga {required_pages} stron KV, dostępne {}",
                self.kv.free_page_count()
            )));
        }
        self.preflight_hybrid_state_slots(2)?;
        self.ensure_hybrid_prefill_b2_bufs()?;
        for seq in seqs.iter_mut() {
            self.activate_hybrid_sequence(seq)?;
        }
        let leases = [
            seqs[0].hybrid_state.expect("lane0 ma lease"),
            seqs[1].hybrid_state.expect("lane1 ma lease"),
        ];
        let ssm = p.ssm.as_ref().expect("prefill B2 ma parametry SSM");
        let q_elements = TOTAL * p.n_heads * p.head_dim;
        let q_norm_rows = TOTAL * p.n_heads;
        let kv_norm_rows = TOTAL * p.n_kv_heads;
        let delta_norm_rows = TOTAL * ssm.n_v_heads();
        let state_bytes = ssm.n_v_heads() * ssm.d_state * ssm.d_state * 4;
        let mut snapshot_ready = false;
        let mut work_enqueued = false;
        let result = (|| {
            for seq in seqs.iter_mut() {
                for _ in 0..STEPS {
                    self.kv.grow(seq)?;
                }
            }
            let page_table_elems = BATCH * self.max_pages_per_seq;
            let mut metadata = Vec::with_capacity(3 * TOTAL + BATCH + page_table_elems + BATCH * 2);
            metadata.extend(
                tokens[0]
                    .iter()
                    .chain(tokens[1].iter())
                    .map(|&token| token as i32),
            );
            for lane in 0..BATCH {
                metadata.extend((bases[lane]..ends[lane]).map(|position| position as i32));
            }
            for lane in 0..BATCH {
                metadata.extend((bases[lane] + 1..=ends[lane]).map(|visible| visible as i32));
            }
            metadata.extend(bases.map(|base| base as i32));
            let page_table_offset = metadata.len();
            for seq in seqs.iter() {
                metadata.extend(seq.pages.iter().copied());
                metadata.resize(
                    metadata.len() + self.max_pages_per_seq - seq.pages.len(),
                    -1,
                );
            }
            metadata.extend([STEPS as i32, 0, STEPS as i32, 0]);
            let b2 = self
                .hybrid_prefill_b2_bufs
                .as_ref()
                .expect("scratch prefill B2 jest gotowy");
            write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_metadata)?;
            work_enqueued = true;
            let rows_bytes = TOTAL * 4;
            self.device
                .copy(&b2.pinned_metadata, 0, &b2.ids, 0, rows_bytes, &self.stream)?;
            self.device.copy(
                &b2.pinned_metadata,
                rows_bytes,
                &b2.positions,
                0,
                rows_bytes,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                2 * rows_bytes,
                &b2.visible_lens,
                0,
                rows_bytes,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                3 * rows_bytes,
                &b2.base_positions,
                0,
                BATCH * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                page_table_offset * 4,
                &b2.page_tables,
                0,
                page_table_elems * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                (page_table_offset + page_table_elems) * 4,
                &b2.decisions,
                0,
                BATCH * 2 * 4,
                &self.stream,
            )?;

            let embedding = self
                .weights
                .mtp
                .as_ref()
                .and_then(|mtp| mtp.shares_target_embedding.then_some(&mtp.embedding))
                .expect("capability sprawdziło device embedding");
            match embedding {
                MtpEmbedding::Device(DevWeight::F16 { buf, rows, cols }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding F16 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q8_0 { buf, rows, cols }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding Q8_0 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q8_0_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.vocab_size,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q4K { buf, rows, cols }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding Q4_K prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q4_k_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.vocab_size,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                    buf,
                    output_scale,
                    rows,
                    cols,
                    layout: Nvfp4GgufLayout::RowMajor36,
                }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding NVFP4 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_nvfp4_gguf_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.vocab_size,
                        p.hidden_size,
                        *output_scale,
                        &self.stream,
                    )?;
                }
                _ => unreachable!("capability ogranicza format embeddingu"),
            }

            for (layer_index, cache) in b2.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                for (lane, &lease) in leases.iter().enumerate() {
                    let (conv, state) = self
                        .hybrid_states
                        .as_ref()
                        .expect("model ma pulę hybrydową")
                        .state_buffers(lease, layer_index)?
                        .expect("warstwa DeltaNet ma stan");
                    self.device.copy(
                        &conv,
                        0,
                        &cache.conv_initial,
                        lane * conv.len(),
                        conv.len(),
                        &self.stream,
                    )?;
                    self.device.copy(
                        &state,
                        0,
                        &cache.state_initial,
                        lane * state.len(),
                        state.len(),
                        &self.stream,
                    )?;
                }
            }
            snapshot_ready = true;
            self.kernels.rmsnorm_f16(
                &b2.x,
                &b2.h,
                &self.weights.layers[0].attn_norm,
                TOTAL,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
                            unreachable!("capability wymaga rozdzielonych Q/K/V")
                        };
                        self.gemm(&b2.q_full, q, &b2.x, TOTAL, &self.stream)?;
                        self.kernels.deinterleave_gate_f16(
                            &b2.qc,
                            &b2.gatec,
                            &b2.q_full,
                            p.head_dim,
                            q_elements,
                            &self.stream,
                        )?;
                        if let Some(norm) = &attention.q_norm {
                            self.kernels.rmsnorm_f16(
                                &b2.qc,
                                &b2.qc,
                                norm,
                                q_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        self.gemm(&b2.k, k, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.v, v, &b2.x, TOTAL, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &b2.k,
                                &b2.k,
                                norm,
                                kv_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        let n_rot = self.hybrid_n_rot();
                        self.kernels.rope_neox_partial_f16(
                            &b2.qc,
                            &b2.positions,
                            TOTAL,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &b2.k,
                            &b2.positions,
                            TOTAL,
                            p.n_kv_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        let kv_layer = self.target_kv_layer(layer_index);
                        self.kernels.kv_append_batch_segmented_f16(
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.k,
                            &b2.v,
                            &b2.page_tables,
                            &b2.base_positions,
                            BATCH,
                            STEPS,
                            self.max_pages_per_seq,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        self.kernels.attn_verify_segmented_f16_hd256(
                            &b2.attn_out,
                            &b2.qc,
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.page_tables,
                            &b2.visible_lens,
                            BATCH,
                            STEPS,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            1.0 / (p.head_dim as f32).sqrt(),
                            &self.stream,
                        )?;
                        self.kernels.sigmoid_mul_f16(
                            &b2.gated,
                            &b2.attn_out,
                            &b2.gatec,
                            q_elements,
                            &self.stream,
                        )?;
                        self.gemm(&b2.o_out, &attention.attn_o, &b2.gated, TOTAL, &self.stream)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let cache = b2.delta[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma scratch B2");
                        self.gemm(&b2.qkv_mixed, &delta.in_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.z, &delta.gate_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.alpha, &delta.alpha_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.beta_raw, &delta.beta_proj, &b2.x, TOTAL, &self.stream)?;
                        self.kernels.deltanet_prepare_segmented_final_f16(
                            &cache.q,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.final_conv,
                            &cache.conv_initial,
                            &b2.qkv_mixed,
                            &delta.conv1d,
                            &b2.alpha,
                            &b2.beta_raw,
                            &delta.dt_bias,
                            &delta.a,
                            BATCH,
                            STEPS,
                            ssm.n_k_heads(),
                            ssm.n_v_heads(),
                            ssm.d_state,
                            ssm.d_conv,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        match self.delta_state_layout() {
                            DeltaStateLayout::ValueKey => {
                                self.kernels.deltanet_value_key_scan_inplace_f16(
                                    &b2.o,
                                    &b2.final_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    BATCH,
                                    STEPS,
                                    ssm.n_v_heads(),
                                    &self.stream,
                                )?
                            }
                            DeltaStateLayout::KeyValue => {
                                self.kernels.deltanet_gated_scan_segmented_shared_d128_f16(
                                    &b2.o,
                                    &b2.final_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    BATCH,
                                    STEPS,
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?
                            }
                        }
                        for (lane, &lease) in leases.iter().enumerate() {
                            let (conv, state) = self
                                .hybrid_states
                                .as_ref()
                                .expect("model ma pulę hybrydową")
                                .state_buffers(lease, layer_index)?
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &b2.final_conv,
                                lane * conv.len(),
                                &conv,
                                0,
                                conv.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &b2.final_states,
                                lane * state_bytes,
                                &state,
                                0,
                                state.len(),
                                &self.stream,
                            )?;
                        }
                        if let Some(lane) = fail_after_state_commit {
                            return Err(ForgeError::Scheduler(format!(
                                "wymuszony błąd rollbacku prefill B2 lane {lane}"
                            )));
                        }
                        self.kernels.deltanet_gated_rmsnorm_f16(
                            &b2.normed,
                            &b2.o,
                            &b2.z,
                            &delta.ssm_norm,
                            delta_norm_rows,
                            ssm.d_state,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        self.gemm(&b2.o_out, &delta.out_proj, &b2.normed, TOTAL, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &b2.x,
                    &b2.h,
                    &b2.o_out,
                    &layer.ffn_norm,
                    TOTAL,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    unreachable!("capability odrzuca MoE")
                };
                self.ffn_dense_block(
                    layer_index,
                    ffn,
                    FfnBlockBufs {
                        x: &b2.x,
                        gate: &b2.gate,
                        up: &b2.up,
                        act: &b2.act,
                        out: &b2.down,
                        gate_up: None,
                    },
                    TOTAL,
                    &self.stream,
                )?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &b2.x,
                    &b2.h,
                    &b2.down,
                    next_norm,
                    TOTAL,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
            }
            self.kernels.mtp_select_row_segmented_f16(
                &b2.final_hidden,
                &b2.x,
                &b2.decisions,
                BATCH,
                STEPS,
                p.hidden_size,
                &self.stream,
            )?;
            self.logits_gemm(&b2.logits, &b2.final_hidden, BATCH, &self.stream)?;
            if read_host_logits {
                self.device.copy(
                    &b2.logits,
                    0,
                    &b2.pinned_logits,
                    0,
                    BATCH * p.vocab_size * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let logits = b2
                    .pinned_logits
                    .host_ptr()
                    .expect("logity B2 mają mapowanie") as *const f32;
                Ok(std::array::from_fn(|lane| unsafe {
                    std::slice::from_raw_parts(logits.add(lane * p.vocab_size), p.vocab_size)
                        .to_vec()
                }))
            } else {
                Ok(std::array::from_fn(|_| Vec::new()))
            }
        })();

        self.pt_seq = 0;
        if let Err(error) = result {
            for lane in 0..BATCH {
                self.kv.rollback(seqs[lane], bases[lane]);
            }
            if snapshot_ready {
                let b2 = self
                    .hybrid_prefill_b2_bufs
                    .as_ref()
                    .expect("scratch prefill B2 jest gotowy");
                let rollback = (|| {
                    for (layer_index, cache) in b2.delta.iter().enumerate() {
                        let Some(cache) = cache else { continue };
                        for (lane, &lease) in leases.iter().enumerate() {
                            let (conv, state) = self
                                .hybrid_states
                                .as_ref()
                                .expect("model ma pulę hybrydową")
                                .state_buffers(lease, layer_index)?
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.conv_initial,
                                lane * conv.len(),
                                &conv,
                                0,
                                conv.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.state_initial,
                                lane * state.len(),
                                &state,
                                0,
                                state.len(),
                                &self.stream,
                            )?;
                        }
                    }
                    self.device.synchronize()
                })();
                if let Err(rollback) = rollback {
                    return Err(self
                        .hybrid_states
                        .as_mut()
                        .expect("model ma pulę hybrydową")
                        .poison(format!(
                            "błąd prefill B2 T32: {error}; rollback stanów nie powiódł się: {rollback}"
                        )));
                }
            } else if work_enqueued {
                if let Err(sync) = self.device.synchronize() {
                    return Err(self
                        .hybrid_states
                        .as_mut()
                        .expect("model ma pulę hybrydową")
                        .poison(format!(
                            "błąd prefill B2 T32: {error}; synchronizacja rollbacku nie powiodła się: {sync}"
                        )));
                }
            }
            return Err(error);
        }
        result
    }

    /// Prefill a prompt chunk for the hybrid arch as a sequential per-token
    /// recurrent scan (the DeltaNet state carries token-to-token). Returns the
    /// last token's next-token logits. Tier-aware: each token first spills the
    /// coldest attention KV if the hot pool is full, so a long prompt beyond the
    /// VRAM pool prefills by streaming older attention KV back per layer while
    /// the resident DeltaNet state advances untouched.
    pub(crate) fn prefill_hybrid(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.activate_hybrid_sequence(seq)?;
        // Warianty layer-major i batchowy liczą warstwę własnym kodem, poza
        // dwoma punktami redukcji, więc na randze z pociętymi wagami dałyby
        // wynik po cichu zły. Podział prowadzi prefill token po tokenie — przez
        // tę samą warstwę co dekodowanie, a więc z redukcjami na miejscu.
        // Layer-major liczy warstwę własnym kodem, poza dwoma punktami redukcji,
        // więc na randze z pociętymi wagami dałby wynik po cichu zły. Batchowy
        // przechodzi przez te punkty i podział go używa.
        let split = self.tp.is_some();
        if !split && tokens.len() >= 32 && self.hybrid_layer_major_route_capable() {
            return self.prefill_hybrid_layer_major(seq, tokens);
        }
        let batched_enabled =
            std::env::var("FORGE_HYBRID_BATCH_PREFILL").map_or(true, |value| value != "0");
        let batch_capable = !split || self.split_batch_prefill_capable();
        if batched_enabled
            && batch_capable
            && tokens.len() > 1
            && self.hybrid_batched_prefill_capable()
        {
            return self.prefill_hybrid_batched(seq, tokens);
        }
        self.ensure_hybrid_bufs()?;
        let p = self.weights.descriptor.params.clone();
        let vocab = p.vocab_size;
        let page_size = self.kv.cfg.page_size;
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        let mut last_logits = Vec::new();
        for (i, &tok) in tokens.iter().enumerate() {
            let pos = seq.len;
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            // Free VRAM pages for this token before growing; may spill the
            // coldest attention KV to RAM/NVMe. Retain the tokens (recompute
            // path) and track the still-purely-prefilled prefix.
            self.tier_ensure_capacity(seq, 1)?;
            if self.tier.is_some() {
                if seq.tokens.len() == seq.prefilled_len {
                    seq.prefilled_len += 1;
                }
                seq.tokens.push(tok);
            }
            let staged = self.tier.is_some() && !seq.spilled.is_empty();
            let page_boundary = seq.len.is_multiple_of(page_size);
            self.kv.grow(seq)?;
            self.upload_decode_inputs(tok, pos)?;
            let want = i + 1 == tokens.len();
            self.profile_target_start()?;
            if staged {
                self.tier
                    .as_mut()
                    .expect("staged implies tiering")
                    .prepare_streaming(seq)?;
                self.upload_page_table(seq)?;
                self.hybrid_forward_token(tok, want, AttnSrc::Staged(seq))?;
            } else {
                if page_boundary || self.pt_seq != seq.id {
                    self.upload_page_table(seq)?;
                }
                self.hybrid_forward_token(tok, want, AttnSrc::Paged)?;
            }
            self.profile_target_end()?;
            self.profile_catchup_start()?;
            self.mtp_catchup_token(seq, tok)?;
            self.profile_catchup_end()?;
            if want {
                self.device.copy(
                    &self.bufs.logits,
                    0,
                    &self.bufs.pinned_logits,
                    0,
                    vocab * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let lp =
                    self.bufs
                        .pinned_logits
                        .host_ptr()
                        .expect("pinned buffer has host mapping") as *const f32;
                last_logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
            }
        }
        // Feed the measured prefill rate into the tier's transfer-vs-recompute
        // estimate (bit-identical recompute is eligible only for prefill KV).
        if let (Some(t0), Some(tier)) = (tier_t0, self.tier.as_ref()) {
            if !tokens.is_empty() {
                tier.note_prefill(tokens.len(), t0.elapsed().as_secs_f64());
            }
        }
        Ok(last_logits)
    }

    /// Wykonuje prefill hybrydowego targetu w macierzowych chunkach i zatwierdza
    /// ostatni checkpoint rekurencji po każdym chunku.
    fn prefill_hybrid_layer_major(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.prefill_hybrid_layer_major_inner(seq, tokens, None, false, false)
    }

    pub(crate) fn prefill_hybrid_layer_major_inner(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        fail_after_layer: Option<usize>,
        fail_mtp_catchup: bool,
        fail_after_mtp_commit: bool,
    ) -> Result<Vec<f32>> {
        if tokens.is_empty() || tokens.len() > HYBRID_LAYER_MAJOR_MAX_TOKENS {
            return Err(ForgeError::Scheduler(
                "layer-major prefill otrzymał nieobsługiwaną długość".into(),
            ));
        }
        if seq.len + tokens.len() > self.weights.descriptor.params.max_position_embeddings {
            return Err(ForgeError::Scheduler(
                "layer-major prefill przekracza kontekst".into(),
            ));
        }
        {
            let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
            })?;
            for &token in tokens {
                let end = (token as usize + 1)
                    .checked_mul(self.weights.descriptor.params.hidden_size)
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie indeksu embeddingu".into())
                    })?;
                if end > table.len() {
                    return Err(ForgeError::Scheduler(format!(
                        "token id {token} wykracza poza embedding targetu"
                    )));
                }
            }
        }
        self.ensure_hybrid_verify_bufs(4)?;
        self.ensure_hybrid_layer_major_bufs(tokens.len())?;
        let arena = self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major jest gotowa")
            .clone();
        let p = self.weights.descriptor.params.clone();
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let t = tokens.len();
        let base = seq.len;
        let attention_backend = self.hybrid_layer_major_attention_backend()?;
        let persistent_scan = hybrid_layer_major_persistent_scan_requested()?
            && t > 128
            && self
                .kernels
                .supports_deltanet_gated_scan_persistent_d128_f16();
        let checkpoint = self.checkpoint_hybrid_layer_major(seq)?;
        let result = (|| {
            let new_pages = (base + t)
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len());
            self.ensure_free_pages(new_pages);
            for _ in 0..t {
                self.kv.grow(seq)?;
            }
            let mut page_table = vec![-1i32; self.max_pages_per_seq];
            page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.pt_seq = seq.id;

            let table = self
                .weights
                .token_embd_host
                .as_ref()
                .expect("embedding targetu sprawdzono przed mutacją KV");
            let hidden_bytes = p.hidden_size * 2;
            let staging = &arena.host_staging;
            let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
            for (chunk_index, chunk) in tokens.chunks(128).enumerate() {
                let offset = chunk_index * 128;
                let slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
                let host = &staging[slot];
                if staging_recorded[slot] {
                    host.ready.synchronize()?;
                }
                let ids: Vec<i32> = chunk.iter().map(|&id| id as i32).collect();
                let positions: Vec<i32> = (base + offset..base + offset + chunk.len())
                    .map(|position| position as i32)
                    .collect();
                let visible_lens: Vec<i32> = (base + offset + 1..=base + offset + chunk.len())
                    .map(|len| len as i32)
                    .collect();
                write_pinned(bytemuck::cast_slice(&ids), &host.ids)?;
                write_pinned(bytemuck::cast_slice(&positions), &host.positions)?;
                write_pinned(bytemuck::cast_slice(&visible_lens), &host.visible_lens)?;
                let destination = host
                    .embedding
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                for (row, &token) in chunk.iter().enumerate() {
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
                            destination.add(row * hidden_bytes),
                            hidden_bytes,
                        );
                    }
                }
                if chunk_index == 0 {
                    write_pinned(bytemuck::cast_slice(&page_table), &host.page_table)?;
                    write_pinned(&(base as i32).to_le_bytes(), &host.base_pos)?;
                    self.device.copy(
                        &host.page_table,
                        0,
                        &self.page_table_dev,
                        0,
                        page_table.len() * 4,
                        &self.stream,
                    )?;
                    self.device
                        .copy(&host.base_pos, 0, &arena.base_pos, 0, 4, &self.stream)?;
                }
                self.device.copy(
                    &host.embedding,
                    0,
                    &arena.h,
                    offset * hidden_bytes,
                    chunk.len() * hidden_bytes,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.ids,
                    0,
                    &arena.ids,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.positions,
                    0,
                    &arena.positions,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.visible_lens,
                    0,
                    &arena.visible_lens,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.record_event(&host.ready, &self.stream)?;
                staging_recorded[slot] = true;
            }

            self.profile_target_start()?;
            self.kernels.rmsnorm_f16(
                &arena.x,
                &arena.h,
                &self.weights.layers[0].attn_norm,
                t,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            let q_dim = p.n_heads * p.head_dim;
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
                            return Err(ForgeError::Unsupported(
                                "layer-major wymaga rozdzielonych Q/K/V".into(),
                            ));
                        };
                        self.gemm(&arena.q_full, q, &arena.x, t, &self.stream)?;
                        self.kernels.deinterleave_gate_f16(
                            &arena.qc,
                            &arena.gatec,
                            &arena.q_full,
                            p.head_dim,
                            t * q_dim,
                            &self.stream,
                        )?;
                        if let Some(norm) = &attention.q_norm {
                            self.kernels.rmsnorm_f16(
                                &arena.qc,
                                &arena.qc,
                                norm,
                                t * p.n_heads,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        self.gemm(&arena.k, k, &arena.x, t, &self.stream)?;
                        self.gemm(&arena.v, v, &arena.x, t, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &arena.k,
                                &arena.k,
                                norm,
                                t * p.n_kv_heads,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        let n_rot = self.hybrid_n_rot();
                        self.kernels.rope_neox_partial_f16(
                            &arena.qc,
                            &arena.positions,
                            t,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &arena.k,
                            &arena.positions,
                            t,
                            p.n_kv_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        let kv_layer = self.target_kv_layer(layer_index);
                        self.kernels.kv_append_batch_device_pos_f16(
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &arena.k,
                            &arena.v,
                            &self.page_table_dev,
                            &arena.base_pos,
                            t,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        match attention_backend {
                            HybridLayerMajorAttention::Exact => {
                                self.kernels.attn_decode_batch_exact_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    &arena.visible_lens,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    self.max_pages_per_seq,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                            HybridLayerMajorAttention::Prefill => {
                                self.kernels.attn_prefill_device_pos_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    &arena.base_pos,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                            HybridLayerMajorAttention::Flash => {
                                self.kernels.attn_prefill_fa_mojo_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    base,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                        }
                        self.kernels.sigmoid_mul_f16(
                            &arena.gated,
                            &arena.q_full,
                            &arena.gatec,
                            t * q_dim,
                            &self.stream,
                        )?;
                        self.gemm(
                            &arena.mixer_out,
                            &attention.attn_o,
                            &arena.gated,
                            t,
                            &self.stream,
                        )?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let state = self.active_ssm()[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma stan");
                        self.gemm(&arena.q_full, &delta.in_proj, &arena.x, t, &self.stream)?;
                        if let Some(cols) = Self::delta_input_q8_cols(delta) {
                            let mut prepared =
                                self.kernels.prepare_q8_1(&arena.x, cols, t, &self.stream)?;
                            self.gemm_q8_prepared_triplet(
                                [&arena.z, &arena.alpha, &arena.beta_raw],
                                [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj],
                                &mut prepared,
                                t,
                            )?;
                        } else {
                            self.gemm(&arena.z, &delta.gate_proj, &arena.x, t, &self.stream)?;
                            self.gemm(&arena.alpha, &delta.alpha_proj, &arena.x, t, &self.stream)?;
                            self.gemm(
                                &arena.beta_raw,
                                &delta.beta_proj,
                                &arena.x,
                                t,
                                &self.stream,
                            )?;
                        }
                        self.device.copy(
                            &state.conv,
                            0,
                            &arena.conv_initial,
                            0,
                            state.conv.len(),
                            &self.stream,
                        )?;
                        if hybrid_layer_major_tiled_prepare_requested()
                            && ssm.d_state == 128
                            && ssm.d_conv == 4
                            && self.kernels.supports_deltanet_prepare_tiled_d128_c4_f16()
                        {
                            self.kernels.deltanet_prepare_tiled_d128_c4_f16(
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                &arena.conv_final,
                                &arena.conv_initial,
                                &arena.q_full,
                                &delta.conv1d,
                                &arena.alpha,
                                &arena.beta_raw,
                                &delta.dt_bias,
                                &delta.a,
                                t,
                                ssm.n_k_heads(),
                                ssm.n_v_heads(),
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        } else {
                            self.kernels.deltanet_prepare_segmented_final_f16(
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                &arena.conv_final,
                                &arena.conv_initial,
                                &arena.q_full,
                                &delta.conv1d,
                                &arena.alpha,
                                &arena.beta_raw,
                                &delta.dt_bias,
                                &delta.a,
                                1,
                                t,
                                ssm.n_k_heads(),
                                ssm.n_v_heads(),
                                ssm.d_state,
                                ssm.d_conv,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        if self.delta_state_layout() == DeltaStateLayout::ValueKey {
                            self.kernels.deltanet_value_key_scan_persistent_f16(
                                &arena.o,
                                &state.state,
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?;
                        } else if persistent_scan {
                            self.kernels.deltanet_gated_scan_persistent_d128_f16(
                                &arena.o,
                                &state.state,
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?;
                        } else {
                            for token_offset in (0..t).step_by(128) {
                                self.kernels.deltanet_gated_scan_inplace_f16_at(
                                    &arena.o,
                                    &state.state,
                                    &arena.qc,
                                    &arena.gatec,
                                    &arena.gated,
                                    &arena.g,
                                    &arena.beta,
                                    token_offset,
                                    (t - token_offset).min(128),
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?;
                            }
                        }
                        self.device.copy(
                            &arena.conv_final,
                            0,
                            &state.conv,
                            0,
                            state.conv.len(),
                            &self.stream,
                        )?;
                        self.kernels.deltanet_gated_rmsnorm_f16(
                            &arena.o,
                            &arena.o,
                            &arena.z,
                            &delta.ssm_norm,
                            t * ssm.n_v_heads(),
                            ssm.d_state,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        self.gemm(&arena.mixer_out, &delta.out_proj, &arena.o, t, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &arena.x,
                    &arena.h,
                    &arena.mixer_out,
                    &layer.ffn_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return Err(ForgeError::Unsupported(
                        "layer-major nie obsługuje targetu MoE".into(),
                    ));
                };
                self.ffn_dense_block(
                    layer_index,
                    ffn,
                    FfnBlockBufs {
                        x: &arena.x,
                        gate: &arena.gate,
                        up: &arena.up,
                        // Bramkowanie liczone W MIEJSCU: `act` to ten sam bufor
                        // co `gate`.
                        act: &arena.gate,
                        out: &arena.mixer_out,
                        gate_up: None,
                    },
                    t,
                    &self.stream,
                )?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &arena.x,
                    &arena.h,
                    &arena.mixer_out,
                    next_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                if fail_after_layer == Some(layer_index) {
                    return Err(ForgeError::Scheduler(format!(
                        "wymuszony błąd layer-major po warstwie {layer_index}"
                    )));
                }
            }
            self.device.copy(
                &arena.x,
                (t - 1) * hidden_bytes,
                &self.bufs.x,
                0,
                hidden_bytes,
                &self.stream,
            )?;
            self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
            self.device.copy(
                &self.bufs.logits,
                0,
                &self.bufs.pinned_logits,
                0,
                p.vocab_size * 4,
                &self.stream,
            )?;
            self.profile_target_end()?;
            self.profile_catchup_start()?;
            self.mtp_catchup_layer_major_prefix(
                seq,
                tokens,
                &arena.x,
                fail_mtp_catchup,
                fail_after_mtp_commit,
            )?;
            let logits =
                self.bufs
                    .pinned_logits
                    .host_ptr()
                    .expect("pinned logits mają mapowanie hosta") as *const f32;
            Ok(unsafe { std::slice::from_raw_parts(logits, p.vocab_size) }.to_vec())
        })();
        if let Err(error) = result {
            return match self.rollback_hybrid_layer_major(seq, &checkpoint) {
                Ok(()) => Err(error),
                Err(rollback) => Err(self
                    .hybrid_states
                    .as_mut()
                    .expect("model hybrydowy ma pulę stanów")
                    .poison(format!(
                        "błąd layer-major: {error}; rollback nie powiódł się: {rollback}"
                    ))),
            };
        }
        result
    }

    fn prefill_hybrid_batched(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty prefill chunk".into()));
        }
        let p = self.weights.descriptor.params.clone();
        if seq.len + tokens.len() > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                seq.len + tokens.len() - 1,
                p.max_position_embeddings
            )));
        }
        self.ensure_hybrid_bufs()?;
        self.ensure_prefill_bufs()?;
        // Sumę cząstkową rangi trzyma bufor o stałym rozmiarze, więc chunk pod
        // podziałem nie może go przekroczyć.
        let chunk_size = match self.tp.is_some() {
            true => self.hybrid_prefill_chunk_size.min(MAX_SPLIT_PREFILL_CHUNK),
            false => self.hybrid_prefill_chunk_size,
        };
        let prefill_cap = tokens.len().min(chunk_size);
        let saved_graphs = if prefill_cap > 4 {
            self.ensure_hybrid_verify_bufs(4)?;
            std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
            Some((
                std::mem::take(&mut self.hybrid_verify_graphs),
                std::mem::take(&mut self.hybrid_verify_graph_disabled),
            ))
        } else {
            None
        };
        let result = (|| {
            self.ensure_hybrid_verify_bufs(prefill_cap)?;

            let hidden_bytes = p.hidden_size * 2;
            let mut last_logits = Vec::new();
            let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
            let mut offset = 0usize;
            let mut chunk_index = 0usize;
            while offset < tokens.len() {
                let remaining = tokens.len() - offset;
                let t = hybrid_prefill_step_size(remaining, chunk_size);
                let chunk = &tokens[offset..offset + t];
                offset += t;
                let t = chunk.len();
                let base = seq.len;
                let new_pages = (base + t)
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                self.ensure_free_pages(new_pages);
                for _ in 0..t {
                    self.kv.grow(seq)?;
                }

                let mut page_table = vec![-1i32; self.max_pages_per_seq];
                page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
                self.pt_seq = seq.id;

                let ids: Vec<i32> = chunk.iter().map(|&id| id as i32).collect();
                let positions: Vec<i32> =
                    (base..base + t).map(|position| position as i32).collect();
                let visible_lens: Vec<i32> = (base + 1..=base + t).map(|len| len as i32).collect();
                let staging_slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
                let staging_ready = self.stage_hybrid_batch_chunk(
                    chunk,
                    base,
                    t,
                    &page_table,
                    &ids,
                    &positions,
                    &visible_lens,
                    staging_slot,
                    staging_recorded[staging_slot],
                )?;
                // Ta sama porcja wejść na pozostałych rangach; strumień
                // rezydualny jest replikowany, więc każda musi dostać ten
                // sam embedding, te same pozycje i tę samą tablicę stron.
                for rank in self.tp_ranks() {
                    rank.stage_hybrid_batch_chunk(
                        chunk,
                        base,
                        t,
                        &page_table,
                        &ids,
                        &positions,
                        &visible_lens,
                        staging_slot,
                        staging_recorded[staging_slot],
                    )?;
                }
                staging_recorded[staging_slot] = true;

                self.profile_target_start()?;
                self.run_hybrid_batch_layers(t, true)?;
                let pb = self
                    .prefill_bufs
                    .as_ref()
                    .expect("bufory prefill są gotowe");
                self.device.copy(
                    &pb.x,
                    (t - 1) * hidden_bytes,
                    &self.bufs.x,
                    0,
                    hidden_bytes,
                    &self.stream,
                )?;
                if offset == tokens.len() {
                    self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
                    self.device.copy(
                        &self.bufs.logits,
                        0,
                        &self.bufs.pinned_logits,
                        0,
                        p.vocab_size * 4,
                        &self.stream,
                    )?;
                }
                self.profile_target_end()?;

                self.profile_catchup_start()?;
                if self.has_native_mtp() {
                    self.mtp_catchup_verified_prefix(seq, t, staging_slot, Some(&staging_ready))?;
                }
                self.profile_catchup_end()?;
                chunk_index += 1;
            }
            self.device.synchronize()?;
            let logits = self
                .bufs
                .pinned_logits
                .host_ptr()
                .expect("pinned buffer has host mapping") as *const f32;
            last_logits
                .extend_from_slice(unsafe { std::slice::from_raw_parts(logits, p.vocab_size) });
            Ok(last_logits)
        })();
        if result.is_err() {
            let _ = self.stream.synchronize();
        }
        restore_after(result, || {
            if let Some((graphs, disabled)) = saved_graphs {
                // Verifier decode zachowuje własne bufory cap=4 i przechwycone grafy.
                std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
                self.hybrid_verify_graphs = graphs;
                self.hybrid_verify_graph_disabled = disabled;
            }
        })
    }

}
