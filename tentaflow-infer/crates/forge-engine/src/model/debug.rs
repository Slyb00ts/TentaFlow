// ===== File: model/debug.rs — sledzenie, profil i punkty kontrolne testow =====
use super::*;

/// Zrzut sumy i skrajnych wartości bufora f16, włączany `FORGE_LAYER_TRACE=1`.
/// Służy do porównania warstwa po warstwie z `llama-eval-callback`, który podaje
/// te same sumy po stronie llama.cpp. Każdy zrzut synchronizuje kartę, więc jest
/// bezużyteczny w pomiarach wydajności i domyślnie wyłączony.
fn layer_trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FORGE_LAYER_TRACE").is_ok_and(|v| v == "1"))
}

impl Model {
    /// Wypisuje, ile wag zostało w VRAM, a ile jest czytane z pamięci hosta
    /// przez PCIe — bez tego łatwo nie zauważyć, że model cicho zszedł na
    /// wolniejszą ścieżkę.
    pub(crate) fn report_residency(res: WeightResidency) {
        if res.host_bytes == 0 {
            return;
        }
        let gib = |b: usize| b as f64 / (1024.0 * 1024.0 * 1024.0);
        tracing::warn!(
            vram_gib = format!("{:.2}", gib(res.vram_bytes)),
            host_gib = format!("{:.2}", gib(res.host_bytes)),
            host_pct = format!("{:.1}", res.host_fraction() * 100.0),
            "część wag nie zmieściła się w VRAM i jest czytana z pamięci hosta przez PCIe"
        );
    }

    /// Rozkład ekspertów po warstwach pamięci, logowany po załadowaniu modelu.
    pub(crate) fn report_expert_residency(&self) {
        let Some((vram, host, nvme)) = self.moe_expert_residency() else {
            return;
        };
        if host == 0 && nvme == 0 {
            return;
        }
        tracing::info!(
            experts_vram = vram,
            experts_host = host,
            experts_nvme = nvme,
            "eksperci MoE rozłożeni między VRAM a pamięć hosta; rezydencja będzie przestawiana wg popularności"
        );
    }

    /// Przygotowuje wszystkie eventy przed startem workera, aby ich alokacja
    /// nie wchodziła do TTFT mierzonego żądania.
    pub fn prepare_prefill_profiles(&mut self, prompt_tokens: usize, runs: usize) -> Result<()> {
        if prompt_tokens == 0 || runs == 0 {
            return Err(ForgeError::Scheduler(
                "profil prefill wymaga dodatniej liczby tokenów i przebiegów".into(),
            ));
        }
        if !self.prefill_profiles.is_empty() {
            return Err(ForgeError::Scheduler(
                "profil prefill został już przygotowany".into(),
            ));
        }
        let hybrid = self.is_hybrid();
        // Ten sam warunek, co w `prefill_hybrid`: podział na rangi NIE używa
        // layer-major, więc profil nie może rezerwować spanów na tę trasę.
        let layer_major_limit = self
            .hybrid_layer_major_prefill_limit()
            .filter(|_| prompt_tokens >= 32 && self.tp.is_none());
        if let Some(limit) = layer_major_limit {
            let arena_tokens = prompt_tokens.min(limit);
            self.ensure_hybrid_layer_major_bufs(arena_tokens)?;
            tracing::info!(
                tokens = arena_tokens,
                bytes = self
                    .hybrid_layer_major_bufs
                    .as_ref()
                    .expect("arena layer-major została zaalokowana")
                    .device_bytes,
                "zaalokowano prototypową arenę layer-major"
            );
        }
        // Ten sam predykat, co decyduje o trasie w `prefill_hybrid` — inaczej
        // profil rezerwuje spany na inna sciezke, niz sie wykona.
        let hybrid_batched = std::env::var("FORGE_HYBRID_BATCH_PREFILL")
            .map_or(true, |value| value != "0")
            && prompt_tokens > 1
            && (self.tp.is_none() || self.split_batch_prefill_capable())
            && self.hybrid_batched_prefill_capable();
        let target_spans = if let Some(limit) = layer_major_limit {
            prompt_tokens.div_ceil(limit)
        } else if hybrid_batched {
            hybrid_prefill_profile_spans(prompt_tokens, self.hybrid_prefill_chunk_size)
        } else if hybrid {
            prompt_tokens
        } else {
            prompt_tokens.div_ceil(MAX_PREFILL_CHUNK)
        };
        for _ in 0..runs {
            let mut target = Vec::with_capacity(target_spans);
            for _ in 0..target_spans {
                target.push(ProfileSpan {
                    start: self.device.create_timing_event()?,
                    end: self.device.create_timing_event()?,
                });
            }
            let catchup_spans = if layer_major_limit.is_some() || hybrid_batched {
                target_spans
            } else if hybrid {
                prompt_tokens
            } else {
                0
            };
            let mut catchup = Vec::with_capacity(catchup_spans);
            if hybrid {
                for _ in 0..catchup_spans {
                    catchup.push(ProfileSpan {
                        start: self.device.create_timing_event()?,
                        end: self.device.create_timing_event()?,
                    });
                }
            }
            self.prefill_profiles.push_back(PrefillProfileRun {
                target,
                catchup,
                target_cursor: 0,
                catchup_cursor: 0,
            });
        }
        Ok(())
    }

    pub fn take_prefill_profile(&mut self) -> Result<Option<PrefillProfile>> {
        let Some(run) = self.prefill_profiles.pop_front() else {
            return Ok(None);
        };
        if run.target_cursor != run.target.len() || run.catchup_cursor != run.catchup.len() {
            return Err(ForgeError::Scheduler(format!(
                "niepełny profil prefill: target {}/{}, MTP {}/{}",
                run.target_cursor,
                run.target.len(),
                run.catchup_cursor,
                run.catchup.len()
            )));
        }
        let target_gpu_ms = self.sum_profile_spans(&run.target)?;
        let mtp_catchup_gpu_ms = self.sum_profile_spans(&run.catchup)?;
        Ok(Some(PrefillProfile {
            target_gpu_ms,
            mtp_catchup_gpu_ms,
        }))
    }

    fn sum_profile_spans(&self, spans: &[ProfileSpan]) -> Result<Option<f64>> {
        if spans.is_empty() {
            return Ok(Some(0.0));
        }
        let mut total = 0.0;
        for span in spans {
            let Some(ms) = self.device.elapsed_event_ms(&span.start, &span.end)? else {
                return Ok(None);
            };
            total += f64::from(ms);
        }
        Ok(Some(total))
    }

    pub(crate) fn profile_target_start(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front() else {
            return Ok(());
        };
        let span = run.target.get(run.target_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil target prefill przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.start, &self.stream)
    }

    pub(crate) fn profile_target_end(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front_mut() else {
            return Ok(());
        };
        let span = run.target.get(run.target_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil target prefill przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.end, &self.stream)?;
        run.target_cursor += 1;
        Ok(())
    }

    pub(crate) fn profile_catchup_start(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front() else {
            return Ok(());
        };
        let span = run.catchup.get(run.catchup_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil MTP catch-up przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.start, &self.stream)
    }

    pub(crate) fn profile_catchup_end(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front_mut() else {
            return Ok(());
        };
        let span = run.catchup.get(run.catchup_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil MTP catch-up przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.end, &self.stream)?;
        run.catchup_cursor += 1;
        Ok(())
    }

    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_layer_major_arena_bytes(&mut self, cap: usize) -> Result<usize> {
        self.ensure_hybrid_layer_major_bufs(cap)?;
        Ok(self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major została zaalokowana")
            .device_bytes)
    }

    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_layer_major_rollback(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        fail_after_layer: Option<usize>,
        fail_mtp_catchup: bool,
        fail_after_mtp_commit: bool,
    ) -> Result<Vec<f32>> {
        self.activate_hybrid_sequence(seq)?;
        self.prefill_hybrid_layer_major_inner(
            seq,
            tokens,
            fail_after_layer,
            fail_mtp_catchup,
            fail_after_mtp_commit,
        )
    }

    /// Mierzy zdarzeniami urządzenia pojedynczy serialny chunk prefill.
    #[doc(hidden)]
    pub fn debug_prefill_chunk_gpu_ms(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
    ) -> Result<(Vec<f32>, f32)> {
        let start = self.device.create_timing_event()?;
        let end = self.device.create_timing_event()?;
        self.device.record_event(&start, &self.stream)?;
        let logits = self.prefill_chunk(seq, tokens)?;
        self.device.record_event(&end, &self.stream)?;
        self.device.synchronize()?;
        let elapsed = self.device.elapsed_event_ms(&start, &end)?.ok_or_else(|| {
            ForgeError::Unsupported("urządzenie nie obsługuje zdarzeń czasowych".into())
        })?;
        Ok((logits, elapsed))
    }

    /// Mierzy zdarzeniami urządzenia bezpośredni prefill B2 T32.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_t32_gpu_ms(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<([Vec<f32>; 2], f32)> {
        let start = self.device.create_timing_event()?;
        let end = self.device.create_timing_event()?;
        self.device.record_event(&start, &self.stream)?;
        let logits = self.hybrid_prefill_b2_t32(seqs, tokens)?;
        self.device.record_event(&end, &self.stream)?;
        self.device.synchronize()?;
        let elapsed = self.device.elapsed_event_ms(&start, &end)?.ok_or_else(|| {
            ForgeError::Unsupported("urządzenie nie obsługuje zdarzeń czasowych".into())
        })?;
        Ok((logits, elapsed))
    }

    /// Zwraca sumę logicznych rozmiarów dedykowanego scratchu prefill B2.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_scratch_bytes(&self) -> usize {
        let Some(bufs) = self.hybrid_prefill_b2_bufs.as_ref() else {
            return 0;
        };
        let fixed = [
            &bufs.h,
            &bufs.x,
            &bufs.k,
            &bufs.v,
            &bufs.attn_out,
            &bufs.o_out,
            &bufs.gate,
            &bufs.up,
            &bufs.act,
            &bufs.down,
            &bufs.ids,
            &bufs.positions,
            &bufs.q_full,
            &bufs.qc,
            &bufs.gatec,
            &bufs.gated,
            &bufs.qkv_mixed,
            &bufs.z,
            &bufs.alpha,
            &bufs.beta_raw,
            &bufs.o,
            &bufs.normed,
            &bufs.page_tables,
            &bufs.base_positions,
            &bufs.visible_lens,
            &bufs.decisions,
            &bufs.final_hidden,
            &bufs.logits,
            &bufs.pinned_metadata,
            &bufs.pinned_logits,
            &bufs.final_conv,
            &bufs.final_states,
        ]
        .into_iter()
        .map(DevBuffer::len)
        .sum::<usize>();
        fixed
            + bufs
                .delta
                .iter()
                .flatten()
                .map(|cache| {
                    cache.conv_initial.len()
                        + cache.state_initial.len()
                        + cache.q.len()
                        + cache.k.len()
                        + cache.v.len()
                        + cache.g.len()
                        + cache.beta.len()
                })
                .sum::<usize>()
    }

    /// Sprawdza osobny bufor `z` i potrójną projekcję scratchu prefill.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_prefill_triplet_contract(&mut self, cap: usize) -> Result<()> {
        if !matches!(cap, 32 | 128) {
            return Err(ForgeError::Scheduler(
                "test kontraktu triplet wymaga cap 32 lub 128".into(),
            ));
        }
        self.ensure_hybrid_prefill_capacity(cap)?;
        let (gate_w, gate_rows, alpha_w, alpha_rows, beta_w, beta_rows, cols) = self
            .weights
            .layers
            .iter()
            .find_map(|layer| {
                let LayerMixer::DeltaNet(delta) = &layer.mixer else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: gate,
                    rows: gate_rows,
                    cols,
                } = &delta.gate_proj
                else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: alpha,
                    rows: alpha_rows,
                    cols: alpha_cols,
                } = &delta.alpha_proj
                else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: beta,
                    rows: beta_rows,
                    cols: beta_cols,
                } = &delta.beta_proj
                else {
                    return None;
                };
                (*alpha_cols == *cols && *beta_cols == *cols).then(|| {
                    (
                        gate.clone(),
                        *gate_rows,
                        alpha.clone(),
                        *alpha_rows,
                        beta.clone(),
                        *beta_rows,
                        *cols,
                    )
                })
            })
            .ok_or_else(|| ForgeError::Unsupported("brak grupy DeltaNet Q8_0".into()))?;
        let pb_x = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe")
            .x
            .clone();
        let hv = self
            .hybrid_prefill_bufs
            .as_ref()
            .expect("scratch prefill hybrid jest gotowy");
        let qkv = hv.qkv_mixed.clone();
        let z = hv.z.clone();
        let alpha = hv.alpha.clone();
        let beta = hv.beta_raw.clone();
        if qkv.device_ptr() == z.device_ptr() {
            return Err(ForgeError::Scheduler(
                "scratch cap>4 aliasuje z z wejściem mixed qkv".into(),
            ));
        }
        let input_bytes = checked_scratch_bytes("test input triplet", &[cap, cols], 2)?;
        let host_x = (0..input_bytes / 2)
            .map(|index| f16::from_f32((index as f32 % 31.0 - 15.0) / 8.0))
            .collect::<Vec<_>>();
        self.device
            .write(bytemuck::cast_slice::<f16, u8>(&host_x), &pb_x, 0)?;
        let qkv_pattern = (0..qkv.len())
            .map(|index| ((index * 37 + 11) & 0xff) as u8)
            .collect::<Vec<_>>();
        self.device.write(&qkv_pattern, &qkv, 0)?;
        let baseline_z = self.device.alloc(
            checked_scratch_bytes("test baseline z", &[cap, gate_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let baseline_alpha = self.device.alloc(
            checked_scratch_bytes("test baseline alpha", &[cap, alpha_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let baseline_beta = self.device.alloc(
            checked_scratch_bytes("test baseline beta", &[cap, beta_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let mut baseline = self.kernels.prepare_q8_1(&pb_x, cols, cap, &self.stream)?;
        for (output, weights, rows) in [
            (&baseline_z, &gate_w, gate_rows),
            (&baseline_alpha, &alpha_w, alpha_rows),
            (&baseline_beta, &beta_w, beta_rows),
        ] {
            self.kernels.gemm_q8_0_i8mma_prepared_at(
                output,
                weights,
                0,
                &mut baseline,
                rows,
                cols,
                cap,
            )?;
        }
        drop(baseline);
        let mut fused = self.kernels.prepare_q8_1(&pb_x, cols, cap, &self.stream)?;
        self.kernels.gemm_q8_0_i8mma_prepared_triplet(
            &[
                Q8PreparedProjection {
                    output: &z,
                    weights: &gate_w,
                    weight_byte_offset: 0,
                    rows: gate_rows,
                },
                Q8PreparedProjection {
                    output: &alpha,
                    weights: &alpha_w,
                    weight_byte_offset: 0,
                    rows: alpha_rows,
                },
                Q8PreparedProjection {
                    output: &beta,
                    weights: &beta_w,
                    weight_byte_offset: 0,
                    rows: beta_rows,
                },
            ],
            &mut fused,
            cols,
            cap,
        )?;
        drop(fused);
        self.device.synchronize()?;
        let mut qkv_after = vec![0u8; qkv.len()];
        self.device.read(&qkv, 0, &mut qkv_after)?;
        if qkv_after != qkv_pattern {
            return Err(ForgeError::Kernel(
                "triplet nadpisał wejście mixed qkv przed deltanet_prepare".into(),
            ));
        }
        for (name, actual, expected) in [
            ("z", &z, &baseline_z),
            ("alpha", &alpha, &baseline_alpha),
            ("beta", &beta, &baseline_beta),
        ] {
            let mut actual_bytes = vec![0u8; expected.len()];
            let mut expected_bytes = vec![0u8; expected.len()];
            self.device.read(actual, 0, &mut actual_bytes)?;
            self.device.read(expected, 0, &mut expected_bytes)?;
            if actual_bytes != expected_bytes {
                return Err(ForgeError::Kernel(format!(
                    "fused triplet różni się bitowo dla projekcji {name}"
                )));
            }
        }
        Ok(())
    }

    /// Wymusza błąd po wykonaniu wskazanego lane transakcji catch-up MTP.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_mtp_catchup_b2_rollback(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        failed_lane: usize,
    ) -> Result<()> {
        if failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test rollbacku catch-up MTP wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_mtp_catchup_b2_inner(seqs, tokens, reset, Some(failed_lane), None)
    }

    /// Wymusza błąd lane oraz następującego po nim rollbacku pary MTP.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_mtp_catchup_b2_rollback_failure(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        failed_lane: usize,
        rollback_failed_lane: usize,
    ) -> Result<()> {
        if failed_lane >= 2 || rollback_failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test błędu rollbacku catch-up MTP wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_mtp_catchup_b2_inner(
            seqs,
            tokens,
            reset,
            Some(failed_lane),
            Some(rollback_failed_lane),
        )
    }

    /// Wymusza błąd po pierwszym zapisie stanu na potrzeby testu rollbacku.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_t32_rollback(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        failed_lane: usize,
    ) -> Result<[Vec<f32>; 2]> {
        if failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test rollbacku prefill B2 wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, Some(failed_lane), true)
    }

    /// Zrzuca stan hybrydowy do diagnostyki zgodności batch kontra serial.
    pub fn debug_hybrid_state_snapshot(&self) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.device.synchronize()?;
        let mut snapshot = Vec::new();
        for (name, buffer, element_bytes) in
            [("h", &self.bufs.h, 2usize), ("x", &self.bufs.x, 2usize)]
        {
            let mut bytes = vec![0u8; buffer.len()];
            self.device.read(buffer, 0, &mut bytes)?;
            snapshot.push((name.into(), element_bytes, bytes));
        }
        for (layer, state) in self.active_ssm().iter().enumerate() {
            let Some(state) = state else { continue };
            for (kind, buffer, element_bytes) in
                [("conv", &state.conv, 2usize), ("ssm", &state.state, 4usize)]
            {
                let mut bytes = vec![0u8; buffer.len()];
                self.device.read(buffer, 0, &mut bytes)?;
                snapshot.push((format!("layer.{layer}.{kind}"), element_bytes, bytes));
            }
        }
        Ok(snapshot)
    }

    /// Zrzuca logiczny KV i stany DeltaNet jednej sekwencji do testów parytetu.
    pub fn debug_hybrid_sequence_snapshot(
        &mut self,
        seq: &mut SeqKv,
    ) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.activate_hybrid_sequence(seq)?;
        self.device.synchronize()?;
        let lease = seq
            .hybrid_state
            .expect("aktywna sekwencja hybrydowa ma lease");
        let mut snapshot = Vec::new();
        for layer_index in 0..self.weights.layers.len() {
            if let Some((conv, state)) = self
                .hybrid_states
                .as_ref()
                .expect("model ma pulę hybrydową")
                .state_buffers(lease, layer_index)?
            {
                for (kind, buffer, element_bytes) in
                    [("conv", conv, 2usize), ("state", state, 4usize)]
                {
                    let mut data = vec![0u8; buffer.len()];
                    self.device.read(&buffer, 0, &mut data)?;
                    snapshot.push((format!("layer.{layer_index}.{kind}"), element_bytes, data));
                }
            }
        }
        let page_bytes = self.kv.cfg.n_kv_heads * self.kv.cfg.page_size * self.kv.cfg.head_dim * 2;
        for layer_index in 0..self.weights.layers.len() {
            let LayerMixer::Attention(_) = self.weights.layers[layer_index].mixer else {
                continue;
            };
            let kv_layer = self.target_kv_layer(layer_index);
            for (kind, slab) in [("k", &self.kv.k[kv_layer]), ("v", &self.kv.v[kv_layer])] {
                let mut data = vec![0u8; seq.pages.len() * page_bytes];
                for (logical, &physical) in seq.pages.iter().enumerate() {
                    if physical < 0 {
                        return Err(ForgeError::Scheduler(
                            "snapshot prefill B2 nie obsługuje stron spilled".into(),
                        ));
                    }
                    self.device.read(
                        slab,
                        physical as usize * page_bytes,
                        &mut data[logical * page_bytes..(logical + 1) * page_bytes],
                    )?;
                }
                snapshot.push((format!("layer.{layer_index}.kv.{kind}"), 2, data));
            }
        }
        snapshot.push(("seq.len".into(), 1, seq.len.to_le_bytes().to_vec()));
        snapshot.push((
            "seq.tokens".into(),
            4,
            bytemuck::cast_slice(&seq.tokens).to_vec(),
        ));
        Ok(snapshot)
    }

    /// Wymusza produkcyjną odbudowę KV i stanu hybrydowego z historii promptu.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn debug_recompute_seq(&mut self, seq: &mut SeqKv) -> Result<()> {
        if self.tier.is_none() {
            return Err(ForgeError::Scheduler(
                "test recompute wymaga włączonego tieringu".into(),
            ));
        }
        self.recompute_seq(seq)
    }

    /// Zrzuca carry oraz aktywny prefiks KV MTP w kolejności logicznych stron.
    pub fn debug_mtp_state_snapshot(&self, seq: &SeqKv) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.device.synchronize()?;
        let lease = seq
            .hybrid_state
            .ok_or_else(|| ForgeError::Unsupported("sekwencja nie ma lease stanu MTP".into()))?;
        let pool = self
            .hybrid_states
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("model nie ma aktywnego stanu MTP".into()))?;
        pool.validate(lease)?;
        let state = pool.slots[lease.slot].mtp.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("stan MTP sekwencji jest aktualnie używany".into())
        })?;
        let mtp_kv = pool
            .mtp_kv
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("cache MTP jest aktualnie używany".into()))?;
        let mut hidden = vec![0u8; state.recurrent_hidden.len()];
        self.device.read(&state.recurrent_hidden, 0, &mut hidden)?;
        let mut snapshot = vec![("mtp.hidden".into(), 2, hidden)];
        for (name, buffer) in [
            ("mtp.page_table", &state.page_table),
            ("mtp.seq_len", &state.seq_len),
            ("mtp.position", &state.position),
        ] {
            let mut bytes = vec![0u8; buffer.len()];
            self.device.read(buffer, 0, &mut bytes)?;
            snapshot.push((name.into(), 4, bytes));
        }
        let head_bytes = mtp_kv.cfg.head_dim * 2;
        for (name, buffers) in [("mtp.k", &mtp_kv.k), ("mtp.v", &mtp_kv.v)] {
            let mut bytes = Vec::with_capacity(state.seq.len * mtp_kv.cfg.n_kv_heads * head_bytes);
            for (offset, length) in logical_kv_regions(
                &state.seq.pages,
                state.seq.len,
                mtp_kv.cfg.page_size,
                mtp_kv.cfg.n_kv_heads,
                head_bytes,
            ) {
                let mut chunk = vec![0u8; length];
                self.device.read(&buffers[0], offset, &mut chunk)?;
                bytes.extend_from_slice(&chunk);
            }
            snapshot.push((name.into(), 2usize, bytes));
        }
        snapshot.push(("mtp.len".into(), 1, state.seq.len.to_le_bytes().to_vec()));
        Ok(snapshot)
    }

    /// Wykonuje pojedynczy referencyjny catch-up MTP po kroku targetu.
    pub fn debug_mtp_catchup_token(&mut self, seq: &mut SeqKv, token: u32) -> Result<()> {
        self.mtp_catchup_token(seq, token)
    }

    /// Fused decode step: six launches per layer instead of nine. The
    /// residual stream is carried as the (h f16, h32 f32) pair — every
    /// norm-consuming kernel recomputes the RMSNorm per block from that pair
    /// (bit-identical to the separate rmsnorm kernels, see decode_fused.mojo)
    /// and attn_decode_split folds the whole qkv_post stage into the
    /// attention prologue (the split/combine pair fills the GPU where one
    /// block per head could not). Layer 0 sums squares from h directly (h32
    /// is only materialized by the first gemv_residual of the step).
    ///
    /// `src` selects the attention's K/V home: the paged cache (recorded into
    /// the replayable decode graph) or the tier staging slabs carrying the
    /// sequence's full context per layer (streamed path, never captured). On
    /// the staged path attn_decode_split appends the new token INTO staging
    /// and the tail page is mirrored back to the canonical paged slab.
    pub(crate) fn trace_f32(&self, label: &str, buf: &DevBuffer, len: usize) {
        if !layer_trace_enabled() {
            return;
        }
        let _ = self.stream.synchronize();
        let mut bytes = vec![0u8; len * 4];
        if self.device.read(buf, 0, &mut bytes).is_err() {
            return;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let sum: f64 = values.iter().map(|v| *v as f64).sum();
        let (best, top) =
            values
                .iter()
                .enumerate()
                .fold((0usize, f32::NEG_INFINITY), |acc, (i, v)| {
                    if *v > acc.1 {
                        (i, *v)
                    } else {
                        acc
                    }
                });
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*b].total_cmp(&values[*a]));
        let head: Vec<String> = order
            .iter()
            .take(5)
            .map(|i| format!("{i}={:.4}", values[*i]))
            .collect();
        eprintln!(
            "TRACE {label}: suma {sum:.6} max id {best} = {top:.4} | top5 {} | id19887={:.4} id415={:.4}",
            head.join(" "),
            values.get(19887).copied().unwrap_or(f32::NAN),
            values.get(415).copied().unwrap_or(f32::NAN)
        );
    }

    pub(crate) fn trace_f16(&self, label: &str, buf: &DevBuffer, byte_offset: usize, len: usize) {
        if !layer_trace_enabled() {
            return;
        }
        let _ = self.stream.synchronize();
        let mut bytes = vec![0u8; len * 2];
        if self.device.read(buf, byte_offset, &mut bytes).is_err() {
            return;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect();
        let sum: f32 = values.iter().sum();
        eprintln!(
            "TRACE {label}: [{:.4}, {:.4}, {:.4} ... {:.4}] suma {sum:.6}",
            values[0],
            values[1],
            values[2],
            values[len - 1]
        );
    }

}
