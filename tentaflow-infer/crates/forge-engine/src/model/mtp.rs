// ===== File: model/mtp.rs — natywne MTP/NextN: propozycja, weryfikacja, catch-up =====
use super::*;

impl Model {
    fn take_mtp_runtime(
        &mut self,
        seq: &mut SeqKv,
    ) -> Result<(HybridStateLease, MtpDraftState, KvCache)> {
        self.activate_hybrid_sequence(seq)?;
        let lease = seq
            .hybrid_state
            .expect("aktywacja przydzieliła lease hybrydowy");
        let (state, kv) = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .take_mtp(lease)?;
        Ok((lease, state, kv))
    }

    fn take_mtp_runtime_pair(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
    ) -> Result<([HybridStateLease; 2], [MtpDraftState; 2], KvCache)> {
        self.activate_hybrid_sequence(seqs[0])?;
        let first = seqs[0]
            .hybrid_state
            .expect("aktywacja przydzieliła pierwszy lease hybrydowy");
        self.activate_hybrid_sequence(seqs[1])?;
        let second = seqs[1]
            .hybrid_state
            .expect("aktywacja przydzieliła drugi lease hybrydowy");
        let leases = [first, second];
        let (states, kv) = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .take_mtp_pair(leases)?;
        Ok((leases, states, kv))
    }

    fn restore_mtp_runtime(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
    ) -> Result<()> {
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .restore_mtp(lease, state, kv)
    }

    fn poison_mtp_runtime(&mut self, reason: String) -> ForgeError {
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .poison(reason)
    }

    fn finish_mtp_runtime<T>(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
        result: Result<T>,
    ) -> Result<T> {
        match (result, self.restore_mtp_runtime(lease, state, kv)) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(error), Err(restore_error)) => Err(ForgeError::Scheduler(format!(
                "błąd wykonania MTP: {error}; błąd przywrócenia lease: {restore_error}"
            ))),
        }
    }

    fn finish_mtp_runtime_pair<T>(
        &mut self,
        leases: [HybridStateLease; 2],
        states: [MtpDraftState; 2],
        kv: KvCache,
        result: Result<T>,
    ) -> Result<T> {
        let restore = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .restore_mtp_pair(leases, states, kv);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(error), Err(restore_error)) => Err(ForgeError::Scheduler(format!(
                "błąd wykonania pary MTP: {error}; błąd przywrócenia lease: {restore_error}"
            ))),
        }
    }

    /// Zwraca, czy checkpoint zawiera kompletny blok NextN gotowy do smoke MTP.
    pub fn has_native_mtp(&self) -> bool {
        self.weights
            .mtp
            .as_ref()
            .is_some_and(|mtp| mtp.runtime_supported())
            && self
                .hybrid_states
                .as_ref()
                .is_some_and(HybridStatePool::has_mtp)
    }

    pub fn mtp_host_embedding_gathers(&self) -> u64 {
        self.hybrid_states
            .as_ref()
            .map_or(0, HybridStatePool::mtp_host_embedding_gathers)
    }

    pub fn mtp_embedding_mode(&self) -> Option<&'static str> {
        self.weights
            .mtp
            .as_ref()
            .map(|weights| weights.embedding.mode())
    }

    /// Sprawdza niemutujący kontrakt pierwszego pionu native MTP B2.
    pub fn native_mtp_b2_capable(&self, seqs: [&SeqKv; 2], budget: usize) -> bool {
        matches!(budget, 2 | 3)
            && self.mtp_ngram_b2_model_capable()
            && seqs
                .iter()
                .all(|seq| self.native_mtp_available_budget(seq, budget) == budget)
    }

    /// Sprawdza strukturalny kontrakt wspólnego target verifiera N/N B2.
    pub fn mtp_ngram_b2_model_capable(&self) -> bool {
        self.validate_native_mtp_target().is_ok()
            && self.hybrid_batch_weights_capable()
            && matches!(self.kv.cfg.quant, KvQuant::F16)
            && self.tier.is_none()
            && self.prefix_cache.is_none()
            && native_mtp_b2_device_embedding(
                self.mtp_embedding_mode(),
                self.weights
                    .mtp
                    .as_ref()
                    .is_some_and(|mtp| mtp.shares_target_embedding),
            )
    }

    /// Sprawdza kontrakt wspólnego target verifiera dla dwóch pełnych draftów n-gram.
    pub fn mtp_ngram_b2_capable(&self, seqs: [&SeqKv; 2], budget: usize) -> bool {
        self.native_mtp_b2_capable(seqs, budget)
    }

    fn mtp_upload_scalar(
        &self,
        state: &mut MtpDraftState,
        value: i32,
        dst: &DevBuffer,
        dst_offset: usize,
    ) -> Result<()> {
        if state.pinned_scalar_recorded {
            state.pinned_scalar_ready.synchronize()?;
        }
        write_pinned(&value.to_le_bytes(), &state.pinned_scalar)?;
        self.device
            .copy(&state.pinned_scalar, 0, dst, dst_offset, 4, &self.stream)?;
        self.device
            .record_event(&state.pinned_scalar_ready, &self.stream)?;
        state.pinned_scalar_recorded = true;
        Ok(())
    }

    fn mtp_propose_pending(&mut self, seq: &mut SeqKv, fed: u32, k: usize) -> Result<Vec<u32>> {
        if k != 2 && k != 3 {
            return Err(ForgeError::Unsupported(
                "MTP propose_k obsługuje wyłącznie K=2 lub K=3".into(),
            ));
        }
        if !self.has_native_mtp() {
            return Err(ForgeError::Unsupported(
                "checkpoint MTP nie spełnia ograniczeń natywnego runtime".into(),
            ));
        }
        self.ensure_hybrid_bufs()?;
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut checkpoint_attempted = false;
        let mut result: Result<Vec<u32>> = (|| {
            if fed as usize >= self.weights.descriptor.params.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token wejściowy MTP {fed} wykracza poza słownik"
                )));
            }
            checkpoint_attempted = true;
            state.checkpoint(&self.stream)?;
            let initial_ids = [fed as i32, 0, 0, 0, 0];
            write_pinned(bytemuck::cast_slice(&initial_ids), &state.pinned_token_ids)?;
            self.device.copy(
                &state.pinned_token_ids,
                0,
                &state.token_ids,
                0,
                initial_ids.len() * 4,
                &self.stream,
            )?;
            self.device.copy(
                &state.token_ids,
                0,
                &self.bufs.sample_out,
                0,
                4,
                &self.stream,
            )?;

            for step in 0..=k {
                self.mtp_gather_embedding(&mut state, step)?;
                state.stage_step(&mut mtp_kv, &self.kernels, &self.stream)?;
                self.mtp_forward_one(&mut state, &mtp_kv, step < k)?;
                state.save_step_hidden(step, &self.stream)?;
                if step == k {
                    continue;
                }
                self.kernels.sample_argmax_f32(
                    &self.bufs.sample_out,
                    &self.bufs.sample_vals,
                    &self.bufs.sample_idx,
                    &state.logits,
                    self.weights.descriptor.params.vocab_size,
                    &self.stream,
                )?;
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.token_ids,
                    (step + 1) * 4,
                    4,
                    &self.stream,
                )?;
            }
            self.device.copy(
                &state.token_ids,
                0,
                &state.pinned_token_ids,
                0,
                5 * 4,
                &self.stream,
            )?;
            // Czekamy na TEN strumień, nie na całe urządzenie: `device.synchronize`
            // drenuje wszystkie strumienie i to on dawał 8,4 ms bezczynności GPU
            // na krok MTP (dwa drenaże: propose i weryfikacja).
            self.stream.synchronize()?;
            let host = state
                .pinned_token_ids
                .host_ptr()
                .expect("pinned token IDs mają mapowanie hosta");
            let ids = unsafe { std::slice::from_raw_parts(host as *const i32, k + 1) };
            let gather_status = unsafe { *(host as *const i32).add(4) };
            if gather_status != 0 {
                return Err(ForgeError::Kernel(
                    "MTP GPU gather odrzucił token poza zakresem słownika".into(),
                ));
            }
            Ok(ids[1..].iter().map(|&id| id as u32).collect())
        })();
        if result.is_err() {
            if state.checkpoint_len().is_some() {
                if let Err(rollback) = state.rollback(&mut mtp_kv, &self.stream) {
                    let execution = result.expect_err("wynik propose zawiera błąd");
                    result = Err(self.poison_mtp_runtime(format!(
                        "błąd propose MTP: {execution}; rollback nie powiódł się: {rollback}"
                    )));
                }
            } else if checkpoint_attempted {
                let execution = result.expect_err("wynik propose zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd propose MTP przed utworzeniem checkpointu: {execution}"
                )));
            }
        }
        self.pt_seq = 0;
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_propose_pending_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        k: usize,
        external_drafts: [Option<&[u32]>; 2],
    ) -> Result<()> {
        if !self.native_mtp_b2_capable([&*seqs[0], &*seqs[1]], k) {
            return Err(ForgeError::Unsupported(
                "para nie spełnia kontraktu native MTP B2".into(),
            ));
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let fed_i32 = validate_mtp_routed_inputs(vocab, fed, k, external_drafts)?;
        self.ensure_hybrid_bufs()?;
        let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let mut checkpoint_attempted = false;
        let mut result: Result<()> = (|| {
            let mut required_pages = 0usize;
            for state in &states {
                let end = state.seq.len.checked_add(k + 1).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie długości draftu MTP B2".into())
                })?;
                let end_pages = end.div_ceil(mtp_kv.cfg.page_size);
                if end_pages > mtp_kv.cfg.max_pages_per_seq {
                    return Err(ForgeError::Scheduler(
                        "draft MTP B2 przekracza limit stron sekwencji".into(),
                    ));
                }
                required_pages = required_pages
                    .checked_add(end_pages.saturating_sub(state.seq.pages.len()))
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie rezerwacji KV MTP B2".into())
                    })?;
            }
            if required_pages > mtp_kv.free_page_count() {
                return Err(ForgeError::Scheduler(format!(
                    "draft MTP B2 wymaga {required_pages} stron, dostępne {}",
                    mtp_kv.free_page_count()
                )));
            }

            checkpoint_attempted = true;
            for (lane, state) in states.iter_mut().enumerate() {
                state.checkpoint(&self.stream)?;
                let mut initial_ids = [fed_i32[lane], 0, 0, 0, 0];
                if let Some(draft) = external_drafts[lane] {
                    for (index, &token) in draft.iter().enumerate() {
                        initial_ids[index + 1] = i32::try_from(token).map_err(|_| {
                            ForgeError::Format("draft routed MTP przekracza i32".into())
                        })?;
                    }
                }
                write_pinned(bytemuck::cast_slice(&initial_ids), &state.pinned_token_ids)?;
                self.device.copy(
                    &state.pinned_token_ids,
                    0,
                    &state.token_ids,
                    0,
                    initial_ids.len() * 4,
                    &self.stream,
                )?;
            }

            for step in 0..=k {
                for (lane, state) in states.iter_mut().enumerate() {
                    if external_drafts[lane].is_some() {
                        continue;
                    }
                    self.device.copy(
                        &state.token_ids,
                        step * 4,
                        &self.bufs.sample_out,
                        0,
                        4,
                        &self.stream,
                    )?;
                    self.mtp_gather_embedding(state, step)?;
                    state.stage_step(&mut mtp_kv, &self.kernels, &self.stream)?;
                    self.mtp_forward_one(state, &mtp_kv, step < k)?;
                    state.save_step_hidden(step, &self.stream)?;
                    if step < k {
                        self.kernels.sample_argmax_f32(
                            &self.bufs.sample_out,
                            &self.bufs.sample_vals,
                            &self.bufs.sample_idx,
                            &state.logits,
                            vocab,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &self.bufs.sample_out,
                            0,
                            &state.token_ids,
                            (step + 1) * 4,
                            4,
                            &self.stream,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let checkpoints_complete = states.iter().all(|state| state.checkpoint_len().is_some());
            if let Err(rollback) = rollback_mtp_pair(&mut states, &mut mtp_kv, &self.stream) {
                let execution = result.expect_err("wynik propose B2 zawiera błąd");
                result = Err(self
                    .poison_mtp_runtime(format!("błąd propose MTP B2: {execution}; {rollback}")));
            } else if checkpoint_attempted && !checkpoints_complete {
                let execution = result.expect_err("wynik propose B2 zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd propose MTP B2 przed utworzeniem obu checkpointów: {execution}"
                )));
            }
        }
        self.pt_seq = 0;
        self.finish_mtp_runtime_pair(leases, states, mtp_kv, result)
    }

    /// Buduje dwa drafty K=2/3 w kolejności per krok i odtwarza oba stany MTP.
    pub fn mtp_propose_k_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        k: usize,
    ) -> Result<[Vec<u32>; 2]> {
        self.mtp_propose_pending_b2(seqs, fed, k, [None, None])?;
        let (leases, states, mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let readback = (|| {
            for state in &states {
                self.device.copy(
                    &state.token_ids,
                    0,
                    &state.pinned_token_ids,
                    0,
                    5 * 4,
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            let mut drafts = [Vec::with_capacity(k), Vec::with_capacity(k)];
            for (lane, state) in states.iter().enumerate() {
                let host = state
                    .pinned_token_ids
                    .host_ptr()
                    .expect("pinned token IDs mają mapowanie hosta");
                let ids = unsafe { std::slice::from_raw_parts(host as *const i32, k + 1) };
                if ids
                    .iter()
                    .any(|&id| id < 0 || id as usize >= self.weights.descriptor.params.vocab_size)
                {
                    return Err(ForgeError::Kernel(format!(
                        "MTP GPU gather lane {lane} odrzucił token poza słownikiem"
                    )));
                }
                drafts[lane].extend(ids[1..].iter().map(|&id| id as u32));
            }
            Ok(drafts)
        })();
        let drafts = self.finish_mtp_runtime_pair(leases, states, mtp_kv, readback)?;
        let first = self.rollback_mtp_pending(seqs[0]);
        let second = self.rollback_mtp_pending(seqs[1]);
        match (first, second) {
            (Ok(()), Ok(())) => Ok(drafts),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(first), Err(second)) => Err(ForgeError::Scheduler(format!(
                "rollback obu lane'ów MTP B2 nie powiódł się: lane0={first}; lane1={second}"
            ))),
        }
    }

    /// Buduje liniowy draft K=2/3 poza normalnym server flow i nie zmienia
    /// trwałego stanu KV/hidden bloku MTP.
    pub fn mtp_propose_k(&mut self, seq: &mut SeqKv, fed: u32, k: usize) -> Result<Vec<u32>> {
        let draft = self.mtp_propose_pending(seq, fed, k)?;
        self.rollback_mtp_pending(seq)?;
        Ok(draft)
    }

    fn rollback_mtp_pending(&mut self, seq: &mut SeqKv) -> Result<()> {
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = state
            .rollback(&mut mtp_kv, &self.stream)
            .and_then(|_| self.device.synchronize());
        if let Err(rollback) = &result {
            result =
                Err(self
                    .poison_mtp_runtime(format!("rollback stanu MTP nie powiódł się: {rollback}")));
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    pub(crate) fn reset_mtp_runtime(&mut self, seq: &mut SeqKv) -> Result<()> {
        if !self.has_native_mtp() {
            return Ok(());
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let result = state.reset(&mut mtp_kv, &self.stream);
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    pub(crate) fn mtp_catchup_token(&mut self, seq: &mut SeqKv, token: u32) -> Result<()> {
        if !self.has_native_mtp() {
            return Ok(());
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let result = self.mtp_catchup_token_pending(&mut state, &mut mtp_kv, token);
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_token_pending(
        &mut self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        token: u32,
    ) -> Result<()> {
        self.device.copy(
            &self.bufs.x,
            0,
            &state.catchup_hidden,
            0,
            state.catchup_hidden.len(),
            &self.stream,
        )?;
        let sample_out = self.bufs.sample_out.clone();
        self.mtp_upload_scalar(state, token as i32, &sample_out, 0)?;
        self.mtp_gather_embedding(state, 0)?;
        state.stage_step(mtp_kv, &self.kernels, &self.stream)?;
        self.mtp_forward_one(state, mtp_kv, false)?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            state.recurrent_hidden.len(),
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &self.bufs.x,
            0,
            state.catchup_hidden.len(),
            &self.stream,
        )
    }

    fn mtp_catchup_batch_host(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        t: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("batchowy catch-up MTP wymaga jednej warstwy".into())
        })?;
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        // Ścieżka wsadowa idzie przez `self.gemm`, który obsługuje każdy format
        // wagi; kontrola została, bo reszta catch-upu zakłada Q8_0.
        let DevWeight::Q8_0 { .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrydowego catch-up są gotowe");
        let stream = &self.stream;
        let (base, page_table, seq_len, position) = state.stage_batch(mtp_kv, t)?;
        let positions: Vec<i32> = (base..base + t).map(|value| value as i32).collect();
        let visible: Vec<i32> = (base + 1..=base + t).map(|value| value as i32).collect();
        let host_staging = &hv.host_staging[staging_slot];
        write_pinned(
            bytemuck::cast_slice(&page_table),
            &host_staging.mtp_page_table,
        )?;
        write_pinned(
            bytemuck::cast_slice(&positions),
            &host_staging.mtp_positions,
        )?;
        write_pinned(
            bytemuck::cast_slice(&visible),
            &host_staging.mtp_visible_lens,
        )?;
        write_pinned(&(base as i32).to_le_bytes(), &host_staging.mtp_base_pos)?;
        write_pinned(&seq_len.to_le_bytes(), &host_staging.mtp_seq_len)?;
        write_pinned(&position.to_le_bytes(), &host_staging.mtp_position)?;
        self.device.copy(
            &host_staging.mtp_page_table,
            0,
            &state.page_table,
            0,
            page_table.len() * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.mtp_seq_len, 0, &state.seq_len, 0, 4, stream)?;
        self.device
            .copy(&host_staging.mtp_position, 0, &state.position, 0, 4, stream)?;
        self.device.copy(
            &host_staging.mtp_positions,
            0,
            &pb.positions,
            0,
            t * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.mtp_base_pos, 0, &hv.base_pos, 0, 4, stream)?;
        self.device.copy(
            &host_staging.mtp_visible_lens,
            0,
            &hv.visible_lens,
            0,
            t * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.embedding, 0, &pb.h, 0, t * hidden * 2, stream)?;
        if let Some(event) = staging_ready {
            self.device.record_event(event, stream)?;
        }
        self.kernels.mtp_norm_join_shifted_f16(
            &hv.q_full,
            &pb.h,
            &pb.x,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            t,
            hidden,
            p.rms_norm_eps,
            stream,
        )?;
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &state.catchup_hidden,
            0,
            hidden * 2,
            stream,
        )?;
        // Projekcja eh_proj to zwykły GEMM `[t, 2h] x [h, 2h]ᵀ`. Kernel
        // `mtp_project_joined_q8_f16` bierze siatkę `(h/8, n_tokens)`, czyli
        // CZYTA CAŁĄ MACIERZ WAG NA KAŻDY TOKEN — dla promptu 4096 to 137,6 ms
        // zamiast jednego przejścia. Wsadowa ścieżka Q8_0 czyta wagi raz.
        // Kolejność redukcji jest inna niż w wariancie sekwencyjnym, ale to
        // PROPOSER: target i tak weryfikuje każdy token, więc wyjście się nie
        // zmienia — zmienić się może wyłącznie akceptacja.
        self.gemm(&pb.h, &layer.eh_proj, &hv.q_full, t, stream)?;

        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &layer.block.attn_norm,
            t,
            hidden,
            p.rms_norm_eps,
            stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q: _, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&pb.k, k, &pb.x, t, stream)?;
        self.gemm(&pb.v, v, &pb.x, t, stream)?;
        if let Some(norm) = &attention.k_norm {
            self.kernels.rmsnorm_f16(
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
        self.kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.kv_append_batch_device_pos_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &pb.k,
            &pb.v,
            &state.page_table,
            &hv.base_pos,
            t,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            hidden * 2,
            stream,
        )
    }

    /// Dogania stan MTP po zaakceptowanym prefiksie targetu bez liczenia logits.
    pub(crate) fn mtp_catchup_verified_prefix(
        &mut self,
        seq: &mut SeqKv,
        retained: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        if retained == 0 {
            return Err(ForgeError::Scheduler(
                "catch-up MTP wymaga co najmniej tokenu fed".into(),
            ));
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = (|| {
            state.checkpoint(&self.stream)?;
            self.mtp_catchup_verified_prefix_pending(
                &mut state,
                &mut mtp_kv,
                retained,
                staging_slot,
                staging_ready,
            )?;
            state.commit_catchup(retained)?;
            Ok(())
        })();
        if result.is_err() && state.checkpoint_len().is_some() {
            let rollback = state
                .rollback(&mut mtp_kv, &self.stream)
                .and_then(|_| self.device.synchronize());
            if let Err(rollback) = rollback {
                let execution = result.expect_err("wynik catch-up zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd catch-up MTP: {execution}; rollback nie powiódł się: {rollback}"
                )));
            }
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_verified_prefix_pending(
        &mut self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        retained: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        if retained == 0 {
            return Err(ForgeError::Scheduler(
                "catch-up MTP wymaga co najmniej tokenu fed".into(),
            ));
        }
        let hidden_bytes = self.weights.descriptor.params.hidden_size * 2;
        if retained > 1
            && self
                .weights
                .mtp
                .as_ref()
                .is_some_and(|mtp| mtp.shares_target_embedding)
        {
            self.mtp_catchup_batch_host(state, mtp_kv, retained, staging_slot, staging_ready)?;
            return self.device.copy(
                &state.catchup_hidden,
                0,
                &self.bufs.x,
                0,
                hidden_bytes,
                &self.stream,
            );
        }
        for row in 0..retained {
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("bufory prefill są gotowe");
            self.device.copy(
                &pb.x,
                row * hidden_bytes,
                &state.catchup_hidden,
                0,
                hidden_bytes,
                &self.stream,
            )?;
            self.device
                .copy(&pb.ids, row * 4, &self.bufs.sample_out, 0, 4, &self.stream)?;
            self.mtp_gather_embedding(state, row)?;
            state.stage_step(mtp_kv, &self.kernels, &self.stream)?;
            self.mtp_forward_one(state, mtp_kv, false)?;
            self.device.copy(
                &state.catchup_hidden,
                0,
                &state.recurrent_hidden,
                0,
                state.recurrent_hidden.len(),
                &self.stream,
            )?;
        }
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.device.copy(
            &pb.x,
            (retained - 1) * hidden_bytes,
            &self.bufs.x,
            0,
            hidden_bytes,
            &self.stream,
        )
    }

    pub(crate) fn mtp_catchup_layer_major_prefix(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        target_hidden: &DevBuffer,
        fail_after_pending: bool,
        fail_after_commit: bool,
    ) -> Result<()> {
        if !self.has_native_mtp() {
            self.profile_catchup_end()?;
            return self.device.synchronize();
        }
        let hidden_bytes = self.weights.descriptor.params.hidden_size * 2;
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = (|| {
            state.checkpoint(&self.stream)?;
            let batch_capable = self.weights.mtp.as_ref().is_some_and(|mtp| {
                mtp.shares_target_embedding
                    && mtp.layers.first().is_some_and(|layer| {
                        matches!(layer.eh_proj, DevWeight::Q8_0 { .. })
                            && matches!(layer.block.attn().attn_qkv, QkvWeights::Split { .. })
                    })
            });
            if batch_capable {
                self.mtp_catchup_layer_major_batch_pending(
                    &mut state,
                    &mut mtp_kv,
                    tokens,
                    target_hidden,
                )?;
            } else {
                for (row, &token) in tokens.iter().enumerate() {
                    self.device.copy(
                        target_hidden,
                        row * hidden_bytes,
                        &self.bufs.x,
                        0,
                        hidden_bytes,
                        &self.stream,
                    )?;
                    self.mtp_catchup_token_pending(&mut state, &mut mtp_kv, token)?;
                }
            }
            if fail_after_pending {
                return Err(ForgeError::Scheduler(
                    "wymuszony błąd layer-major catch-up MTP".into(),
                ));
            }
            state.validate_commit_catchup(tokens.len())?;
            self.profile_catchup_end()?;
            self.device.synchronize()?;
            if fail_after_commit {
                return Err(ForgeError::Scheduler(
                    "wymuszony błąd layer-major po commit MTP".into(),
                ));
            }
            state.apply_commit_catchup();
            Ok(())
        })();
        if result.is_err() && state.checkpoint_len().is_some() {
            let rollback = state
                .rollback(&mut mtp_kv, &self.stream)
                .and_then(|_| self.device.synchronize());
            if let Err(rollback) = rollback {
                let execution = result.expect_err("wynik catch-up zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd layer-major catch-up MTP: {execution}; rollback nie powiódł się: {rollback}"
                )));
            }
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_layer_major_batch_pending(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        tokens: &[u32],
        target_hidden: &DevBuffer,
    ) -> Result<()> {
        let arena = self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major jest gotowa");
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("batchowy catch-up MTP wymaga jednej warstwy".into())
        })?;
        // Ścieżka wsadowa idzie przez `self.gemm`; kontrola formatu została, bo
        // reszta catch-upu zakłada Q8_0.
        let DevWeight::Q8_0 { .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let QkvWeights::Split { q: _, k, v } = &layer.block.attn().attn_qkv else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        let p = &self.weights.descriptor.params;
        let hidden_bytes = p.hidden_size * 2;
        let t = tokens.len();
        let table = self
            .weights
            .token_embd_host
            .as_ref()
            .expect("współdzielony embedding targetu jest dostępny");
        let (base, page_table, seq_len, position) = state.stage_batch(mtp_kv, t)?;
        let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
        for (chunk_index, chunk) in tokens.chunks(128).enumerate() {
            let offset = chunk_index * 128;
            let slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
            let host = &arena.host_staging[slot];
            host.ready.synchronize()?;
            let positions: Vec<i32> = (base + offset..base + offset + chunk.len())
                .map(|value| value as i32)
                .collect();
            write_pinned(bytemuck::cast_slice(&positions), &host.positions)?;
            let destination = host
                .embedding
                .host_ptr()
                .expect("pinned embedding ma mapowanie hosta");
            for (row, &token) in chunk.iter().enumerate() {
                let source =
                    &table[token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size];
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
                write_pinned(&seq_len.to_le_bytes(), &host.seq_len)?;
                write_pinned(&position.to_le_bytes(), &host.position)?;
                self.device.copy(
                    &host.page_table,
                    0,
                    &state.page_table,
                    0,
                    page_table.len() * 4,
                    &self.stream,
                )?;
                self.device
                    .copy(&host.seq_len, 0, &state.seq_len, 0, 4, &self.stream)?;
                self.device
                    .copy(&host.position, 0, &state.position, 0, 4, &self.stream)?;
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
                &host.positions,
                0,
                &arena.positions,
                offset * 4,
                chunk.len() * 4,
                &self.stream,
            )?;
            self.device.record_event(&host.ready, &self.stream)?;
            staging_recorded[slot] = true;
        }
        debug_assert!(staging_recorded.into_iter().any(|recorded| recorded));
        self.kernels.mtp_norm_join_shifted_f16(
            &arena.q_full,
            &arena.h,
            target_hidden,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.device.copy(
            target_hidden,
            (t - 1) * hidden_bytes,
            &state.catchup_hidden,
            0,
            hidden_bytes,
            &self.stream,
        )?;
        // Zwykły GEMM `[t, 2h] x [h, 2h]ᵀ`. `mtp_project_joined_q8_f16` bierze
        // siatkę `(h/8, n_tokens)`, czyli CZYTA CAŁĄ MACIERZ WAG NA KAŻDY TOKEN.
        // Kolejność redukcji jest inna niż w wariancie sekwencyjnym, ale to
        // PROPOSER — target weryfikuje każdy token, więc wyjście się nie zmienia.
        self.gemm(&arena.h, &layer.eh_proj, &arena.q_full, t, &self.stream)?;
        self.kernels.rmsnorm_f16(
            &arena.x,
            &arena.h,
            &layer.block.attn_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.gemm(&arena.k, k, &arena.x, t, &self.stream)?;
        self.gemm(&arena.v, v, &arena.x, t, &self.stream)?;
        let attention = layer.block.attn();
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
        self.kernels.rope_neox_partial_f16(
            &arena.k,
            &arena.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            self.hybrid_n_rot(),
            p.rope_theta,
            &self.stream,
        )?;
        self.kernels.kv_append_batch_device_pos_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &arena.k,
            &arena.v,
            &state.page_table,
            &arena.base_pos,
            t,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            hidden_bytes,
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &self.bufs.x,
            0,
            hidden_bytes,
            &self.stream,
        )
    }

    fn mtp_catchup_verified_prefix_b2(
        &self,
        states: &mut [MtpDraftState; 2],
        mtp_kv: &mut KvCache,
        t: usize,
        external_sources: [bool; 2],
    ) -> Result<()> {
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("segmentowany catch-up MTP wymaga jednej warstwy".into())
        })?;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "segmentowany catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let total = 2usize
            .checked_mul(t)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie catch-up MTP B2".into()))?;
        let mut bases = [0i32; 2];
        let mut page_tables = vec![-1i32; 2 * self.max_pages_per_seq];
        for (lane, state) in states.iter_mut().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &state.recurrent_hidden,
                0,
                &self
                    .mtp_b2_bufs
                    .as_ref()
                    .expect("MTP B2 gotowy")
                    .mtp_initial_hidden,
                lane * hidden * 2,
                hidden * 2,
                &self.stream,
            )?;
            let (base, table, _, _) = state.stage_batch(mtp_kv, t)?;
            bases[lane] = i32::try_from(base).map_err(|_| {
                ForgeError::Scheduler("pozycja bazowa catch-up MTP przekracza i32".into())
            })?;
            let offset = lane * self.max_pages_per_seq;
            page_tables[offset..offset + table.len()].copy_from_slice(&table);
        }
        let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
        let mut metadata = Vec::with_capacity(2 + page_tables.len());
        metadata.extend_from_slice(&bases);
        metadata.extend_from_slice(&page_tables);
        write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_mtp_metadata)?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            0,
            &b2.base_positions,
            0,
            8,
            &self.stream,
        )?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            8,
            &b2.page_tables,
            0,
            page_tables.len() * 4,
            &self.stream,
        )?;
        for (lane, state) in states.iter().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &b2.page_tables,
                lane * self.max_pages_per_seq * 4,
                &state.page_table,
                0,
                self.max_pages_per_seq * 4,
                &self.stream,
            )?;
        }

        let pb = self.prefill_bufs.as_ref().expect("prefill gotowy");
        self.kernels.mtp_pack_verify_inputs(
            &pb.ids,
            &pb.positions,
            &b2.visible_lens,
            &states[0].token_ids,
            &states[1].token_ids,
            &b2.base_positions,
            t,
            &self.stream,
        )?;
        let masked_bases = [
            if external_sources[0] { bases[0] } else { -1 },
            if external_sources[1] { bases[1] } else { -1 },
        ];
        write_pinned(bytemuck::cast_slice(&masked_bases), &b2.pinned_mtp_metadata)?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            0,
            &b2.base_positions,
            0,
            8,
            &self.stream,
        )?;
        self.kernels.mtp_norm_join_shifted_segmented_f16(
            &b2.q_full,
            &b2.catchup_embeddings,
            &pb.x,
            &b2.mtp_initial_hidden,
            &layer.enorm,
            &layer.hnorm,
            2,
            t,
            hidden,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.kernels.mtp_project_joined_q8_f16(
            &pb.h,
            &b2.q_full,
            eh_proj,
            total,
            hidden,
            &self.stream,
        )?;
        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &layer.block.attn_norm,
            total,
            hidden,
            p.rms_norm_eps,
            &self.stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q: _, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&pb.k, k, &pb.x, total, &self.stream)?;
        self.gemm(&pb.v, v, &pb.x, total, &self.stream)?;
        if let Some(norm) = &attention.k_norm {
            self.kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                total * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                &self.stream,
            )?;
        }
        self.kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            total,
            p.n_kv_heads,
            p.head_dim,
            self.hybrid_n_rot(),
            p.rope_theta,
            &self.stream,
        )?;
        self.kernels.kv_append_batch_segmented_masked_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &pb.k,
            &pb.v,
            &b2.page_tables,
            &b2.base_positions,
            &b2.decisions,
            2,
            t,
            self.max_pages_per_seq,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            &self.stream,
        )?;
        self.kernels.mtp_commit_catchup_metadata_segmented(
            &b2.mtp_seq_lens,
            &b2.mtp_positions,
            &b2.base_positions,
            &b2.decisions,
            2,
            &self.stream,
        )?;
        for (lane, state) in states.iter().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &b2.mtp_seq_lens,
                lane * 4,
                &state.seq_len,
                0,
                4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.mtp_positions,
                lane * 4,
                &state.position,
                0,
                4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.selected_hidden,
                lane * hidden * 2,
                &state.recurrent_hidden,
                0,
                hidden * 2,
                &self.stream,
            )?;
        }
        Ok(())
    }

    fn mtp_gather_embedding(&self, state: &mut MtpDraftState, token_index: usize) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        let p = &self.weights.descriptor.params;
        match &mtp.embedding {
            MtpEmbedding::Device(DevWeight::F16 { buf, .. }) => self.kernels.gather_f16_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::Q8_0 { buf, .. }) => self.kernels.gather_q8_0_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::Q4K { buf, .. }) => self.kernels.gather_q4_k_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                layout: Nvfp4GgufLayout::RowMajor36,
                ..
            }) => self.kernels.gather_nvfp4_gguf_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                *output_scale,
                &self.stream,
            ),
            MtpEmbedding::Device(_) => Err(ForgeError::Unsupported(
                "GPU MTP wymaga embeddingu F16, Q8_0, Q4_K lub GGUF NVFP4".into(),
            )),
            MtpEmbedding::HostF16 => {
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.pinned_token_ids,
                    token_index * 4,
                    4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let ids = state
                    .pinned_token_ids
                    .host_ptr()
                    .expect("pinned token IDs mają mapowanie hosta")
                    as *const i32;
                let token_id = unsafe { *ids.add(token_index) };
                if token_id < 0 || token_id as usize >= p.vocab_size {
                    return Err(ForgeError::Kernel(format!(
                        "MTP argmax zwrócił token poza zakresem: {token_id}"
                    )));
                }
                let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                    ForgeError::Unsupported("brak hostowego embeddingu dla MTP low-memory".into())
                })?;
                let base = token_id as usize * p.hidden_size;
                let row = table.get(base..base + p.hidden_size).ok_or_else(|| {
                    ForgeError::Format("wiersz embeddingu MTP wykracza poza tabelę".into())
                })?;
                let slot = self.claim_staging_slot()?;
                let offset = slot * p.hidden_size * 2;
                let staging = self
                    .hybrid_bufs
                    .as_ref()
                    .expect("bufory hybrid zaalokowane")
                    .pinned_embed
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        row.as_ptr() as *const u8,
                        staging.add(offset),
                        p.hidden_size * 2,
                    );
                }
                self.device.copy(
                    &self
                        .hybrid_bufs
                        .as_ref()
                        .expect("bufory hybrid zaalokowane")
                        .pinned_embed,
                    offset,
                    &mtp.token_embedding,
                    0,
                    p.hidden_size * 2,
                    &self.stream,
                )?;
                self.device
                    .record_event(&self.staging_events[slot], &self.stream)?;
                state.record_host_embedding_gather();
                Ok(())
            }
        }
    }

    fn mtp_forward_one(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &KvCache,
        want_logits: bool,
    ) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        if mtp.layers.len() != 1 {
            return Err(ForgeError::Unsupported(format!(
                "runtime MTP obsługuje jeden blok, otrzymano {}",
                mtp.layers.len()
            )));
        }
        let layer = &mtp.layers[0];
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let head_dim = p.head_dim;
        let q_dim = p.n_heads * head_dim;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let b = &self.bufs;
        let hb = self
            .hybrid_bufs
            .as_ref()
            .expect("bufory hybrid zaalokowane");
        let stream = &self.stream;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "mtp_prepare wymaga eh_proj w Q8_0".into(),
            ));
        };
        self.kernels.mtp_prepare_f16(
            &state.prepared_hidden,
            &mtp.token_embedding,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            eh_proj,
            hidden,
            eps,
            stream,
        )?;
        self.kernels.rmsnorm_f16(
            &b.x,
            &state.prepared_hidden,
            &layer.block.attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        let qkv_grouped =
            self.gemv_nvfp4_gguf_group(&[(&hb.q_full, q), (&b.k, k), (&b.v, v)], &b.x, stream)?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, q, &b.x, stream)?;
            self.gemv(&b.k, k, &b.x, stream)?;
            self.gemv(&b.v, v, &b.x, stream)?;
        }
        self.kernels
            .deinterleave_gate_f16(&hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream)?;
        if let Some(norm) = &attention.q_norm {
            self.kernels
                .rmsnorm_f16(&hb.qc, &hb.qc, norm, p.n_heads, head_dim, eps, stream)?;
        }
        if let Some(norm) = &attention.k_norm {
            self.kernels
                .rmsnorm_f16(&b.k, &b.k, norm, p.n_kv_heads, head_dim, eps, stream)?;
        }
        let n_rot = self.hybrid_n_rot();
        self.kernels.rope_neox_partial_f16(
            &hb.qc,
            &state.position,
            1,
            p.n_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.rope_neox_partial_f16(
            &b.k,
            &state.position,
            1,
            p.n_kv_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.kv_append_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &b.k,
            &b.v,
            &state.page_table,
            &state.seq_len,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        self.kernels.attn_decode_f16(
            &b.attn_out,
            &b.attn_parts,
            &hb.qc,
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &state.page_table,
            &state.seq_len,
            1,
            p.n_heads,
            p.n_kv_heads,
            head_dim,
            mtp_kv.cfg.page_size,
            mtp_kv.cfg.max_pages_per_seq,
            1.0 / (head_dim as f32).sqrt(),
            // Głowa MTP pracuje na pełnym kontekście swojej sekwencji.
            0,
            stream,
        )?;
        self.kernels
            .sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.gemv(&b.o_out, &attention.attn_o, &hb.gated, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.o_out,
            &layer.block.ffn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let ffn = layer.block.dense_ffn()?;
        let GateUpWeights::Split { gate, up } = &ffn.gate_up else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych gate/up".into(),
            ));
        };
        self.gemv(&b.gate, gate, &b.x, stream)?;
        self.gemv(&b.up, up, &b.x, stream)?;
        self.kernels
            .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
        self.gemv(&b.down, &ffn.down, &b.act, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.down,
            &layer.shared_head_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.device.copy(
            &b.x,
            0,
            &state.recurrent_hidden,
            0,
            state.recurrent_hidden.len(),
            stream,
        )?;
        if want_logits {
            let output = mtp.draft_output.as_ref().unwrap_or(&mtp.output);
            self.logits_weight_gemv(&state.logits, 0, &b.x, 0, output, stream)?;
        }
        Ok(())
    }

    /// Sprawdza wspólne ograniczenia verifiera spekulacyjnego dla targetu.
    pub fn validate_speculation_target(&self, draft_tokens: usize) -> Result<()> {
        if self.is_hybrid() {
            if !matches!(draft_tokens, 2 | 3) {
                return Err(ForgeError::Unsupported(
                    "hybrydowy verifier spekulacyjny wymaga budżetu 2 lub 3".into(),
                ));
            }
            return self.validate_hybrid_speculation_target();
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support routed MoE models".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 KV cache".into(),
            ));
        }
        if self.tier.is_some() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support KV tiering".into(),
            ));
        }
        // Prefiks współdzielony NIE jest tu przeszkodą. Draft dopisuje się na
        // ogonie sekwencji, a `KvCache::rollback` zwalnia strony wyłącznie od
        // końca i nigdy nie schodzi poniżej `shared_pages`; darowizna obejmuje
        // tylko `prefilled_len`, więc żadna strona zapisana przez verifier nie
        // trafia do drzewa.
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. } | DevWeight::Q8_0 { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 or Q8_0 language-model head".into(),
            ));
        }
        Ok(())
    }

    pub fn validate_hybrid_speculation_target(&self) -> Result<()> {
        if !self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga hybrydowego targetu".into(),
            ));
        }
        if self.weights.descriptor.params.head_dim != 256 {
            return Err(ForgeError::Unsupported(format!(
                "hybrydowy verifier spekulacyjny wymaga head_dim=256, otrzymano {}",
                self.weights.descriptor.params.head_dim
            )));
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny nie obsługuje jeszcze targetu MoE".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga cache KV F16".into(),
            ));
        }
        // Hybrydowy verifier prowadzi obok siebie stan DeltaNet i cache draftu
        // MTP; z pożyczonym prefiksem nie jest zmierzony, więc zostaje poza.
        if self.tier.is_some() || self.prefix_cache.is_some() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga wyłączonego tieringu i prefix cache".into(),
            ));
        }
        // `logits_gemm` obsługuje też głowy K-kwantowe: Q6_K batchowym
        // przemiatem dp4a z wyjściem f32, Q4_K przemiatem per token.
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. }
                | DevWeight::Q8_0 { .. }
                | DevWeight::Q4K { .. }
                | DevWeight::Q6K { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "batchowy head hybrydowego targetu wymaga F16, Q8_0, Q4_K lub Q6_K".into(),
            ));
        }
        Ok(())
    }

    fn ensure_mtp_b2_bufs(&mut self) -> Result<()> {
        if self.mtp_b2_bufs.is_some() {
            return Ok(());
        }
        const BATCH: usize = 2;
        const STEPS: usize = 4;
        let checked_mul = |name: &str, left: usize, right: usize| {
            left.checked_mul(right).ok_or_else(|| {
                ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {left} * {right}"))
            })
        };
        let checked_add = |name: &str, left: usize, right: usize| {
            left.checked_add(right).ok_or_else(|| {
                ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {left} + {right}"))
            })
        };
        let total = checked_mul("mtp b2 total", BATCH, STEPS)?;
        let p = &self.weights.descriptor.params;
        let ssm = p
            .ssm
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("target MTP B2 nie jest hybrydowy".into()))?;
        if ssm.d_state != 128 {
            return Err(ForgeError::Unsupported(format!(
                "target MTP B2 wymaga d_state=128, otrzymano {}",
                ssm.d_state
            )));
        }
        let q_dim = checked_mul("mtp b2 q", p.n_heads, p.head_dim)?;
        let key_dim = checked_mul("mtp b2 key", ssm.d_state, ssm.n_group)?;
        let n_v = ssm.n_v_heads();
        let value_dim = checked_mul("mtp b2 value", ssm.d_state, n_v)?;
        let doubled_key = checked_mul("mtp b2 doubled key", key_dim, 2)?;
        let conv_dim = checked_add("mtp b2 conv", doubled_key, value_dim)?;
        let conv_history = ssm
            .d_conv
            .checked_sub(1)
            .ok_or_else(|| ForgeError::Scheduler("MTP B2 wymaga d_conv > 0".into()))?;
        let conv_elems = checked_mul("mtp b2 conv history", conv_dim, conv_history)?;
        let state_head = checked_mul("mtp b2 state head", ssm.d_state, ssm.d_state)?;
        let state_elems = checked_mul("mtp b2 state", n_v, state_head)?;
        let doubled_q = checked_mul("mtp b2 doubled q", q_dim, 2)?;
        let doubled_hidden = checked_mul("mtp b2 doubled hidden", p.hidden_size, 2)?;
        let q_full_cols = doubled_q.max(conv_dim).max(doubled_hidden);
        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let delta = self
            .weights
            .layers
            .iter()
            .map(|layer| match layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(_) => Ok(None),
                LayerMixer::DeltaNet(_) => Ok(Some(MtpB2DeltaCache {
                    conv_initial: a16("mtp b2 conv initial", &[BATCH, conv_elems])?,
                    conv_checkpoints: a16("mtp b2 conv checkpoints", &[BATCH, STEPS, conv_elems])?,
                    state_initial: a32("mtp b2 state initial", &[BATCH, state_elems])?,
                    q: a16("mtp b2 delta q", &[total, value_dim])?,
                    k: a16("mtp b2 delta k", &[total, value_dim])?,
                    v: a16("mtp b2 delta v", &[total, value_dim])?,
                    g: a32("mtp b2 delta g", &[total, n_v])?,
                    beta: a32("mtp b2 delta beta", &[total, n_v])?,
                })),
            })
            .collect::<Result<Vec<_>>>()?;
        self.mtp_b2_bufs = Some(MtpB2Bufs {
            q_full: a16("mtp b2 q full", &[total, q_full_cols])?,
            qc: a16("mtp b2 q", &[total, q_dim.max(value_dim)])?,
            gatec: a16("mtp b2 q gate", &[total, q_dim.max(value_dim)])?,
            gated: a16("mtp b2 gated", &[total, q_dim.max(value_dim)])?,
            qkv_mixed: a16("mtp b2 qkv mixed", &[total, conv_dim])?,
            z: a16("mtp b2 z", &[total, value_dim])?,
            alpha: a16("mtp b2 alpha", &[total, n_v])?,
            beta_raw: a16("mtp b2 beta raw", &[total, n_v])?,
            o: a16("mtp b2 recurrence output", &[total, value_dim])?,
            normed: a16("mtp b2 recurrence norm", &[total, value_dim])?,
            page_tables: a32("mtp b2 page tables", &[BATCH, self.max_pages_per_seq])?,
            base_positions: a32("mtp b2 base positions", &[BATCH])?,
            visible_lens: a32("mtp b2 visible lengths", &[total])?,
            decisions: a32("mtp b2 decisions", &[BATCH, 2])?,
            pinned_decisions: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned decisions",
                &[BATCH * 2 + BATCH * 5],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_metadata: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned metadata",
                &[BATCH + BATCH * self.max_pages_per_seq],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_mtp_metadata: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned catch-up metadata",
                &[BATCH + BATCH * self.max_pages_per_seq],
                4,
                MemKind::PinnedHost,
            )?,
            catchup_embeddings: a16("mtp b2 catch-up embeddings", &[total, p.hidden_size])?,
            mtp_initial_hidden: a16("mtp b2 catch-up initial hidden", &[BATCH, p.hidden_size])?,
            mtp_seq_lens: a32("mtp b2 catch-up sequence lengths", &[BATCH])?,
            mtp_positions: a32("mtp b2 catch-up positions", &[BATCH])?,
            selected_states: a32("mtp b2 selected states", &[BATCH, state_elems])?,
            selected_conv: a16("mtp b2 selected conv", &[BATCH, conv_elems])?,
            selected_hidden: a16("mtp b2 selected hidden", &[BATCH, p.hidden_size])?,
            delta,
        });
        Ok(())
    }

    pub fn validate_native_mtp_target(&self) -> Result<()> {
        self.validate_hybrid_speculation_target()?;
        if !self.has_native_mtp() {
            return Err(ForgeError::Unsupported(
                "checkpoint nie ma obsługiwanego natywnego proposera MTP".into(),
            ));
        }
        Ok(())
    }

    /// Zwraca dostępny budget 0/2/3 po sprawdzeniu targetu, kontekstu i stron
    /// dla `fed` oraz draftu. Żądane K=3 może zostać przycięte do K=2.
    pub fn native_mtp_available_budget(&self, seq: &SeqKv, requested: usize) -> usize {
        if self.validate_native_mtp_target().is_err() || requested < 2 {
            return 0;
        }
        for budget in (2..=requested.min(3)).rev() {
            let Some(end) = seq
                .len
                .checked_add(1)
                .and_then(|length| length.checked_add(budget))
            else {
                continue;
            };
            if end > self.weights.descriptor.params.max_position_embeddings {
                continue;
            }
            let required_pages = end
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len());
            if required_pages <= self.available_pages() {
                return budget;
            }
        }
        0
    }

    fn verify_hybrid_greedy_draft_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        budget: usize,
        mtp_states: &mut [MtpDraftState; 2],
        mtp_kv: &mut KvCache,
        external_sources: [bool; 2],
    ) -> Result<MtpB2Verification> {
        if !matches!(budget, 2 | 3) {
            return Err(ForgeError::Unsupported(
                "verifier MTP B2 wymaga wspólnego K=2 lub K=3".into(),
            ));
        }
        let t = budget.checked_add(1).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie liczby kroków verifiera MTP B2".into())
        })?;
        let checked_elements = |name: &str, dimensions: &[usize]| {
            dimensions.iter().try_fold(1usize, |elements, &dimension| {
                elements.checked_mul(dimension).ok_or_else(|| {
                    ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {dimensions:?}"))
                })
            })
        };
        let total = checked_elements("mtp b2 total", &[2, t])?;
        self.validate_hybrid_speculation_target()?;
        self.ensure_prefill_bufs()?;
        self.ensure_verify_bufs(total)?;
        self.ensure_mtp_b2_bufs()?;
        for seq in seqs.iter_mut() {
            self.activate_hybrid_sequence(seq)?;
        }
        let leases = [
            seqs[0].hybrid_state.expect("lane0 ma lease"),
            seqs[1].hybrid_state.expect("lane1 ma lease"),
        ];
        let p = self.weights.descriptor.params.clone();
        let ssm = p.ssm.as_ref().expect("target B2 ma DeltaNet");
        let q_elements = checked_elements("mtp b2 q", &[total, p.n_heads, p.head_dim])?;
        let q_norm_rows = checked_elements("mtp b2 q norm", &[total, p.n_heads])?;
        let kv_norm_rows = checked_elements("mtp b2 kv norm", &[total, p.n_kv_heads])?;
        let delta_norm_rows = checked_elements("mtp b2 delta norm", &[total, ssm.n_v_heads()])?;
        let key_width = checked_elements("mtp b2 key width", &[ssm.d_state, ssm.n_group])?;
        let value_width = checked_elements("mtp b2 value width", &[ssm.d_state, ssm.n_v_heads()])?;
        let conv_width = key_width
            .checked_mul(2)
            .and_then(|key| key.checked_add(value_width))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie szerokości conv MTP B2".into()))?;
        let conv_elems = checked_elements(
            "mtp b2 conv state",
            &[
                conv_width,
                ssm.d_conv
                    .checked_sub(1)
                    .ok_or_else(|| ForgeError::Scheduler("MTP B2 wymaga d_conv > 0".into()))?,
            ],
        )?;
        let hidden_bytes = checked_elements("mtp b2 hidden bytes", &[p.hidden_size, 2])?;
        let bases = [seqs[0].len, seqs[1].len];
        let ends = [
            bases[0].checked_add(t).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji verifiera MTP B2 lane0".into())
            })?,
            bases[1].checked_add(t).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji verifiera MTP B2 lane1".into())
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
            .try_fold(0usize, |sum, (lane, seq)| {
                let pages = ends[lane]
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                sum.checked_add(pages).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie stron verifiera MTP B2".into())
                })
            })?;
        self.ensure_free_pages(required_pages);
        if required_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "verifier MTP B2 wymaga {required_pages} stron KV, dostępne {}",
                self.kv.free_page_count()
            )));
        }

        let mut snapshot_ready = false;
        let mut metadata_enqueued = false;
        let result = (|| {
            for seq in seqs.iter_mut() {
                for _ in 0..t {
                    self.kv.grow(seq)?;
                }
            }
            let page_table_elems =
                checked_elements("mtp b2 page tables", &[2, self.max_pages_per_seq])?;
            let mut page_tables = vec![-1i32; page_table_elems];
            for lane in 0..2 {
                let offset =
                    checked_elements("mtp b2 page table offset", &[lane, self.max_pages_per_seq])?;
                page_tables[offset..offset + seqs[lane].pages.len()]
                    .copy_from_slice(&seqs[lane].pages);
            }
            let pb = self.prefill_bufs.as_ref().expect("prefill gotowy");
            let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
            let mut metadata = Vec::with_capacity(2 + page_table_elems);
            metadata.extend([bases[0] as i32, bases[1] as i32]);
            metadata.extend_from_slice(&page_tables);
            write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_metadata)?;
            metadata_enqueued = true;
            self.device.copy(
                &b2.pinned_metadata,
                0,
                &b2.base_positions,
                0,
                8,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                8,
                &b2.page_tables,
                0,
                page_table_elems * 4,
                &self.stream,
            )?;
            self.kernels.mtp_pack_verify_inputs(
                &pb.ids,
                &pb.positions,
                &b2.visible_lens,
                &mtp_states[0].token_ids,
                &mtp_states[1].token_ids,
                &b2.base_positions,
                t,
                &self.stream,
            )?;
            let target_embedding = self
                .weights
                .mtp
                .as_ref()
                .and_then(|mtp| mtp.shares_target_embedding.then_some(&mtp.embedding))
                .ok_or_else(|| {
                    ForgeError::Unsupported("MTP B2 wymaga device-side target embeddingu".into())
                })?;
            match target_embedding {
                MtpEmbedding::Device(DevWeight::F16 { buf, rows, cols }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding F16 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q8_0 { buf, rows, cols }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding Q8_0 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q8_0_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.vocab_size,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q4K { buf, rows, cols }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding Q4_K ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q4_k_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
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
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding NVFP4 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_nvfp4_gguf_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.vocab_size,
                        p.hidden_size,
                        *output_scale,
                        &self.stream,
                    )?;
                }
                _ => {
                    return Err(ForgeError::Unsupported(
                        "target MTP B2 wymaga embeddingu F16, Q8_0, Q4_K lub GGUF NVFP4".into(),
                    ));
                }
            }
            if external_sources.into_iter().any(|external| external) {
                self.device.copy(
                    &pb.h,
                    0,
                    &b2.catchup_embeddings,
                    0,
                    total * p.hidden_size * 2,
                    &self.stream,
                )?;
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
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                total,
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
                            return Err(ForgeError::Unsupported(
                                "target MTP B2 wymaga rozdzielonych Q/K/V".into(),
                            ));
                        };
                        self.gemm(&b2.q_full, q, &pb.x, total, &self.stream)?;
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
                        self.gemm(&pb.k, k, &pb.x, total, &self.stream)?;
                        self.gemm(&pb.v, v, &pb.x, total, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &pb.k,
                                &pb.k,
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
                            &pb.positions,
                            total,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &pb.k,
                            &pb.positions,
                            total,
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
                            &pb.k,
                            &pb.v,
                            &b2.page_tables,
                            &b2.base_positions,
                            2,
                            t,
                            self.max_pages_per_seq,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        self.kernels.attn_verify_segmented_f16_hd256(
                            &pb.attn_out,
                            &b2.qc,
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.page_tables,
                            &b2.visible_lens,
                            2,
                            t,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            1.0 / (p.head_dim as f32).sqrt(),
                            &self.stream,
                        )?;
                        self.kernels.sigmoid_mul_f16(
                            &b2.gated,
                            &pb.attn_out,
                            &b2.gatec,
                            q_elements,
                            &self.stream,
                        )?;
                        self.gemm(&pb.o_out, &attention.attn_o, &b2.gated, total, &self.stream)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let cache = b2.delta[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma cache B2");
                        self.gemm(&b2.qkv_mixed, &delta.in_proj, &pb.x, total, &self.stream)?;
                        if let Some(cols) =
                            Self::delta_input_q8_cols(delta).filter(|_| matches!(total, 6 | 8))
                        {
                            let mut prepared =
                                self.kernels
                                    .prepare_q8_1(&pb.x, cols, total, &self.stream)?;
                            self.gemm_q8_prepared(&b2.z, &delta.gate_proj, &mut prepared, total)?;
                            self.gemm_q8_prepared(
                                &b2.alpha,
                                &delta.alpha_proj,
                                &mut prepared,
                                total,
                            )?;
                            self.gemm_q8_prepared(
                                &b2.beta_raw,
                                &delta.beta_proj,
                                &mut prepared,
                                total,
                            )?;
                        } else {
                            self.gemm(&b2.z, &delta.gate_proj, &pb.x, total, &self.stream)?;
                            self.gemm(&b2.alpha, &delta.alpha_proj, &pb.x, total, &self.stream)?;
                            self.gemm(&b2.beta_raw, &delta.beta_proj, &pb.x, total, &self.stream)?;
                        }
                        self.kernels.deltanet_prepare_segmented_f16(
                            &cache.q,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &cache.conv_checkpoints,
                            &cache.conv_initial,
                            &b2.qkv_mixed,
                            &delta.conv1d,
                            &b2.alpha,
                            &b2.beta_raw,
                            &delta.dt_bias,
                            &delta.a,
                            2,
                            t,
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
                                    &b2.selected_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    2,
                                    t,
                                    ssm.n_v_heads(),
                                    &self.stream,
                                )?
                            }
                            DeltaStateLayout::KeyValue => {
                                self.kernels.deltanet_gated_scan_segmented_shared_d128_f16(
                                    &b2.o,
                                    &b2.selected_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    2,
                                    t,
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?
                            }
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
                        self.gemm(&pb.o_out, &delta.out_proj, &b2.normed, total, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    total,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return Err(ForgeError::Unsupported("MTP B2 nie obsługuje MoE".into()));
                };
                self.ffn_dense_block(
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
                    total,
                    &self.stream,
                )?;
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
                    total,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
            }

            let vb = self.verify_bufs.as_ref().expect("verify gotowy");
            self.logits_gemm(&vb.logits, &pb.x, total, &self.stream)?;
            self.kernels.sample_batched_argmax_f32(
                &vb.ids,
                &vb.logits,
                total,
                p.vocab_size,
                &self.stream,
            )?;
            self.kernels.mtp_verify_decide_segmented(
                &b2.decisions,
                &vb.ids,
                &pb.ids,
                2,
                t,
                &self.stream,
            )?;
            self.kernels.mtp_select_row_segmented_f16(
                &b2.selected_hidden,
                &pb.x,
                &b2.decisions,
                2,
                t,
                p.hidden_size,
                &self.stream,
            )?;

            for (layer_index, cache) in b2.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                match self.delta_state_layout() {
                    DeltaStateLayout::ValueKey => {
                        self.kernels.deltanet_value_key_commit_recompute_f32(
                            &b2.selected_states,
                            &cache.state_initial,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.decisions,
                            2,
                            t,
                            ssm.n_v_heads(),
                            &self.stream,
                        )?
                    }
                    DeltaStateLayout::KeyValue => self
                        .kernels
                        .deltanet_commit_recompute_segmented_shared_d128_f32(
                            &b2.selected_states,
                            &cache.state_initial,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.decisions,
                            2,
                            t,
                            ssm.n_v_heads(),
                            ssm.d_state,
                            &self.stream,
                        )?,
                }
                self.kernels.mtp_select_row_segmented_f16(
                    &b2.selected_conv,
                    &cache.conv_checkpoints,
                    &b2.decisions,
                    2,
                    t,
                    conv_elems,
                    &self.stream,
                )?;
                for (lane, &lease) in leases.iter().enumerate() {
                    let (conv, state) = self
                        .hybrid_states
                        .as_ref()
                        .expect("model ma pulę hybrydową")
                        .state_buffers(lease, layer_index)?
                        .expect("warstwa DeltaNet ma stan");
                    self.device.copy(
                        &b2.selected_conv,
                        lane * conv.len(),
                        &conv,
                        0,
                        conv.len(),
                        &self.stream,
                    )?;
                    self.device.copy(
                        &b2.selected_states,
                        lane * state.len(),
                        &state,
                        0,
                        state.len(),
                        &self.stream,
                    )?;
                }
            }
            for (lane, state) in mtp_states.iter_mut().enumerate() {
                if !external_sources[lane] {
                    self.device.copy(
                        &b2.selected_hidden,
                        lane * hidden_bytes,
                        &state.recurrent_hidden,
                        0,
                        hidden_bytes,
                        &self.stream,
                    )?;
                }
            }
            if external_sources.into_iter().any(|external| external) {
                self.mtp_catchup_verified_prefix_b2(mtp_states, mtp_kv, t, external_sources)?;
            }
            self.device
                .copy(&b2.decisions, 0, &b2.pinned_decisions, 0, 16, &self.stream)?;
            for (lane, state) in mtp_states.iter().enumerate() {
                self.device.copy(
                    &state.token_ids,
                    0,
                    &b2.pinned_decisions,
                    16 + lane * 20,
                    20,
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            let decision_ptr = b2
                .pinned_decisions
                .host_ptr()
                .expect("decyzje B2 mają mapowanie") as *const i32;
            let mut results: [(Vec<u32>, usize, u32); 2] =
                std::array::from_fn(|_| (Vec::with_capacity(budget), 0, 0));
            for (lane, result) in results.iter_mut().enumerate() {
                let retained = unsafe { *decision_ptr.add(2 * lane) };
                let correction = unsafe { *decision_ptr.add(2 * lane + 1) };
                if retained <= 0
                    || retained as usize > t
                    || correction < 0
                    || correction as usize >= p.vocab_size
                {
                    return Err(ForgeError::Kernel(format!(
                        "decyzja MTP B2 lane {lane} poza zakresem"
                    )));
                }
                let ids = unsafe {
                    std::slice::from_raw_parts(decision_ptr.add(4 + lane * 5), budget + 1)
                };
                if ids.iter().any(|&id| id < 0 || id as usize >= p.vocab_size) {
                    return Err(ForgeError::Kernel(format!(
                        "draft MTP B2 lane {lane} poza zakresem"
                    )));
                }
                result.0.extend(ids[1..].iter().map(|&id| id as u32));
                result.1 = retained as usize - 1;
                result.2 = correction as u32;
            }
            let metadata_targets = validate_mtp_pair_metadata_commit(
                mtp_states,
                [results[0].1 + 1, results[1].1 + 1],
            )?;
            Ok((results, metadata_targets))
        })();

        match result {
            Ok((results, metadata_targets)) => {
                for lane in 0..2 {
                    self.kv
                        .rollback(seqs[lane], bases[lane] + results[lane].1 + 1);
                }
                self.pt_seq = 0;
                Ok((results, metadata_targets))
            }
            Err(error) => {
                for lane in 0..2 {
                    self.kv.rollback(seqs[lane], bases[lane]);
                }
                if snapshot_ready {
                    let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
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
                    self.device.synchronize()?;
                } else if metadata_enqueued {
                    self.device.synchronize()?;
                }
                self.pt_seq = 0;
                Err(error)
            }
        }
    }

    fn verify_hybrid_greedy_draft(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
        catchup_mtp: bool,
    ) -> Result<(usize, u32)> {
        if !matches!(draft.len(), 2 | 3) {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny obsługuje draft długości 2 lub 3".into(),
            ));
        }
        self.activate_hybrid_sequence(seq)?;
        let hybrid_slot = seq
            .hybrid_state
            .expect("aktywna sekwencja hybrydowa ma przypisany slot")
            .slot;
        let t = draft.len() + 1;
        self.validate_hybrid_speculation_target()?;
        self.ensure_prefill_bufs()?;
        self.ensure_verify_bufs(4)?;
        self.ensure_hybrid_verify_bufs(4)?;
        let p = self.weights.descriptor.params.clone();
        let base = seq.len;
        if base + t > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                base + t - 1,
                p.max_position_embeddings
            )));
        }
        self.ensure_free_pages(
            (base + t)
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len()),
        );
        let mut snapshot_ready = false;
        let result = (|| {
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            for (layer_index, cache) in hv.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                let state = self.active_ssm()[layer_index]
                    .as_ref()
                    .expect("warstwa DeltaNet ma stan");
                self.device.copy(
                    &state.state,
                    0,
                    &cache.state_initial,
                    0,
                    state.state.len(),
                    &self.stream,
                )?;
                self.device.copy(
                    &state.conv,
                    0,
                    &cache.conv_initial,
                    0,
                    state.conv.len(),
                    &self.stream,
                )?;
            }
            // Migawka i ewentualne odtworzenie idą TYM SAMYM strumieniem, więc
            // kolejność FIFO już gwarantuje, że restore zobaczy komplet kopii.
            // Pełny drain urządzenia na każdy krok weryfikacji kosztował 11%
            // bezczynności GPU w śladzie kroku MTP.
            snapshot_ready = true;
            for _ in 0..t {
                self.kv.grow(seq)?;
            }
            let mut page_table = vec![-1i32; self.max_pages_per_seq];
            page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.device
                .write(bytemuck::cast_slice(&page_table), &self.page_table_dev, 0)?;
            self.pt_seq = seq.id;
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("bufory prefill są gotowe");
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            let tokens: Vec<u32> = std::iter::once(fed).chain(draft.iter().copied()).collect();
            let ids: Vec<i32> = tokens.iter().map(|&id| id as i32).collect();
            let positions: Vec<i32> = (base..base + t).map(|pos| pos as i32).collect();
            let visible_lens: Vec<i32> = (base + 1..=base + t).map(|len| len as i32).collect();
            self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
            self.device
                .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;
            self.device
                .write(&(base as i32).to_le_bytes(), &hv.base_pos, 0)?;
            self.device
                .write(bytemuck::cast_slice(&visible_lens), &hv.visible_lens, 0)?;
            let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
            })?;
            let staging = hv.host_staging[0]
                .embedding
                .host_ptr()
                .expect("pinned embedding ma mapowanie hosta");
            for (row_index, &token) in tokens.iter().enumerate() {
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
                        staging.add(row_index * p.hidden_size * 2),
                        p.hidden_size * 2,
                    );
                }
            }
            self.device.copy(
                &hv.host_staging[0].embedding,
                0,
                &pb.h,
                0,
                t * p.hidden_size * 2,
                &self.stream,
            )?;
            let graph =
                if std::env::var("FORGE_HYBRID_VERIFY_GRAPH").is_ok_and(|value| value == "0") {
                    None
                } else {
                    self.hybrid_verify_graphs.get(&(hybrid_slot, t)).cloned()
                };
            if let Some(graph) = graph {
                self.device.launch_graph(&graph, &self.stream)?;
            } else {
                self.run_hybrid_verify_compute(t)?;
            }
            // Czekamy na TEN strumień, nie na całe urządzenie: `device.synchronize`
            // drenuje wszystkie strumienie i to on dawał 8,4 ms bezczynności GPU
            // na krok MTP (dwa drenaże: propose i weryfikacja).
            self.stream.synchronize()?;
            let decision =
                hv.pinned_decision
                    .host_ptr()
                    .expect("pinned decision ma mapowanie hosta") as *const i32;
            let retained = unsafe { *decision };
            let correction = unsafe { *decision.add(1) };
            if retained <= 0
                || retained as usize > t
                || correction < 0
                || correction as usize >= p.vocab_size
            {
                return Err(ForgeError::Kernel(
                    "decyzja MTP z GPU ma wartość poza zakresem".into(),
                ));
            }
            let accepted = retained as usize - 1;
            if catchup_mtp {
                self.mtp_catchup_verified_prefix(seq, accepted + 1, 0, None)?;
            }
            self.capture_hybrid_verify_graph_if_needed(hybrid_slot, t);
            Ok((accepted, correction as u32))
        })();
        match result {
            Ok((accepted, correction)) => {
                self.kv.rollback(seq, base + accepted + 1);
                self.pt_seq = 0;
                Ok((accepted, correction))
            }
            Err(error) => {
                self.kv.rollback(seq, base);
                self.pt_seq = 0;
                if snapshot_ready {
                    let restore = (|| {
                        let hv = self
                            .hybrid_verify_bufs
                            .as_ref()
                            .expect("bufory hybrid verify są gotowe");
                        for (layer_index, cache) in hv.delta.iter().enumerate() {
                            let Some(cache) = cache else { continue };
                            let state = self.active_ssm()[layer_index]
                                .as_ref()
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.state_initial,
                                0,
                                &state.state,
                                0,
                                state.state.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.conv_initial,
                                0,
                                &state.conv,
                                0,
                                state.conv.len(),
                                &self.stream,
                            )?;
                        }
                        self.device.synchronize()
                    })();
                    if let Err(restore_error) = restore {
                        return Err(ForgeError::Scheduler(format!(
                            "błąd verifiera MTP: {error}; błąd odtworzenia SSM: {restore_error}"
                        )));
                    }
                }
                Err(error)
            }
        }
    }

    /// Wykonuje jeden cykl natywnego MTP. Bieżące logity targetu przewidziały
    /// już `fed`, więc draft powstaje bez osobnego kroku targetu, a target
    /// zatwierdza `[fed, draft...]` przebiegiem T=3/4. Zwracany correction
    /// pozostaje tokenem do podania w następnym cyklu i nie jest jeszcze
    /// zapisany w stanie targetu.
    pub fn native_mtp_step(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        budget: usize,
    ) -> Result<(Vec<u32>, usize, u32)> {
        if budget != 2 && budget != 3 {
            return Err(ForgeError::Unsupported(
                "natywny MTP obsługuje budget 2 lub 3".into(),
            ));
        }
        self.validate_native_mtp_target()?;
        if self.native_mtp_available_budget(seq, budget) != budget {
            return Err(ForgeError::Scheduler(format!(
                "brak pojemności targetu dla MTP K={budget}"
            )));
        }
        let draft = self.mtp_propose_pending(seq, fed, budget)?;
        match self.verify_hybrid_greedy_draft(seq, fed, &draft, false) {
            Ok((accepted, correction)) => {
                let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
                let result = state
                    .commit_prefix(&mut mtp_kv, accepted + 1, &self.stream)
                    .and_then(|_| {
                        self.device.copy(
                            &self.bufs.x,
                            0,
                            &state.recurrent_hidden,
                            0,
                            state.recurrent_hidden.len(),
                            &self.stream,
                        )
                    })
                    .and_then(|_| self.device.synchronize());
                self.finish_mtp_runtime(lease, state, mtp_kv, result)?;
                Ok((draft, accepted, correction))
            }
            Err(error) => {
                if let Err(rollback_error) = self.rollback_mtp_pending(seq) {
                    return Err(ForgeError::Scheduler(format!(
                        "błąd verifiera MTP: {error}; błąd rollbacku draftu: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// Wykonuje wspólny cykl natywnego MTP dla dwóch sekwencji z tym samym K.
    pub fn native_mtp_step_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        budget: usize,
    ) -> Result<[(Vec<u32>, usize, u32); 2]> {
        self.native_mtp_routed_step_b2(seqs, fed, budget, [None, None])
    }

    /// Wykonuje wspólny verifier B2 dla dowolnej pary źródeł MTP/n-gram.
    pub fn native_mtp_routed_step_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        budget: usize,
        external_drafts: [Option<&[u32]>; 2],
    ) -> Result<[(Vec<u32>, usize, u32); 2]> {
        if !self.native_mtp_b2_capable([&*seqs[0], &*seqs[1]], budget) {
            return Err(ForgeError::Unsupported(
                "para nie spełnia kontraktu routed MTP B2".into(),
            ));
        }
        self.mtp_propose_pending_b2(seqs, fed, budget, external_drafts)?;
        let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let external_sources = external_drafts.map(|draft| draft.is_some());
        let result =
            match self.verify_hybrid_greedy_draft_b2(
                seqs,
                budget,
                &mut states,
                &mut mtp_kv,
                external_sources,
            ) {
                Ok((results, metadata_targets)) => {
                    apply_mtp_pair_metadata_commit(&mut states, &mut mtp_kv, metadata_targets);
                    Ok(results)
                }
                Err(error) => match rollback_mtp_pair(&mut states, &mut mtp_kv, &self.stream) {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(self
                        .poison_mtp_runtime(format!("błąd verifiera MTP B2: {error}; {rollback}"))),
                },
            };
        self.pt_seq = 0;
        self.finish_mtp_runtime_pair(leases, states, mtp_kv, result)
    }

    /// Weryfikuje dwa pełne drafty zewnętrznego proposera i dogania MTP na GPU.
    pub fn verify_greedy_draft_with_mtp_catchup_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        drafts: [&[u32]; 2],
    ) -> Result<[(usize, u32); 2]> {
        let budget = drafts[0].len();
        if drafts[1].len() != budget || !matches!(budget, 2 | 3) {
            return Err(ForgeError::Unsupported(
                "MTP+n-gram B2 wymaga dwóch draftów z tym samym K=2 lub K=3".into(),
            ));
        }
        self.native_mtp_routed_step_b2(seqs, fed, budget, [Some(drafts[0]), Some(drafts[1])])
            .map(|verified| {
                [
                    (verified[0].1, verified[0].2),
                    (verified[1].1, verified[1].2),
                ]
            })
    }

    /// Weryfikuje draft zewnętrznego proposera i dogania stan natywnego MTP.
    pub fn verify_greedy_draft_with_mtp_catchup(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        self.validate_native_mtp_target()?;
        self.verify_hybrid_greedy_draft(seq, fed, draft, true)
    }

    /// Verify one greedy speculative draft in a single forward (SPEC §6, linear
    /// path). Runs the model over `[fed, draft…]` as a mini-prefill chunk
    /// appended after the current position, greedy-argmaxes the logits at every
    /// query position, and accepts the longest draft prefix whose token equals
    /// the model's own argmax at the preceding position. The rejected draft
    /// positions' K/V are rolled back, leaving `fed` + the accepted drafts
    /// resident. Returns `(accepted, correction)`: the number of accepted draft
    /// tokens and the model's argmax token at the first unaccepted position
    /// (the correction when `accepted < draft.len()`, else the bonus token).
    /// Wywołujący musi wcześniej użyć `validate_speculation_target()` oraz
    /// próbkowania greedy, aby wynik był zgodny z dekodowaniem sekwencyjnym.
    pub fn verify_greedy_draft(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        debug_assert!(!draft.is_empty(), "verify called with an empty draft");
        debug_assert!(
            draft.len() <= MAX_SPEC_DRAFT,
            "draft exceeds MAX_SPEC_DRAFT"
        );
        if self.is_hybrid() {
            return self.verify_hybrid_greedy_draft(seq, fed, draft, false);
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let t = draft.len() + 1;
        self.ensure_verify_bufs(t)?;

        let base = seq.len;
        let recorded = RecordedTokens::of(seq);
        let mut batch = Vec::with_capacity(t);
        batch.push(fed);
        batch.extend_from_slice(draft);
        let result = (|| {
            self.prefill_forward(seq, &batch, true)?;

            let stream = &self.stream;
            let vb = self.verify_bufs.as_ref().expect("ensured above");
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("prefill_forward allocated");
            self.logits_gemm(&vb.logits, &pb.x, t, stream)?;
            self.kernels
                .sample_batched_argmax_f32(&vb.ids, &vb.logits, t, vocab, stream)?;
            self.device
                .copy(&vb.ids, 0, &vb.pinned_ids, 0, t * 4, stream)?;
            self.device.synchronize()?;

            let ptr = vb
                .pinned_ids
                .host_ptr()
                .expect("pinned buffer has host mapping") as *const i32;
            let argmax = unsafe { std::slice::from_raw_parts(ptr, t) };
            let mut accepted = 0usize;
            let mut correction = 0u32;
            for i in 0..t {
                let am = argmax[i] as u32;
                if i < draft.len() && am == draft[i] {
                    accepted += 1;
                } else {
                    correction = am;
                    break;
                }
            }
            Ok((accepted, correction))
        })();

        finish_greedy_verification(&mut self.kv, &mut self.pt_seq, seq, base, recorded, result)
    }

    /// Odtwarza stany MTP po target-only prefill B2 macierzowo, lane po lane.
    pub fn hybrid_prefill_mtp_catchup_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
    ) -> Result<()> {
        self.hybrid_prefill_mtp_catchup_b2_inner(seqs, tokens, reset, None, None)
    }

    pub(crate) fn hybrid_prefill_mtp_catchup_b2_inner(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        fail_after_lane: Option<usize>,
        fail_rollback_lane: Option<usize>,
    ) -> Result<()> {
        if let Some(pool) = self.hybrid_states.as_ref() {
            pool.ensure_healthy()?;
        }
        if !self.has_native_mtp() {
            return Ok(());
        }
        const STEPS: usize = 32;
        if tokens.iter().any(|lane| lane.len() != STEPS) {
            return Err(ForgeError::Scheduler(
                "catch-up MTP B2 wymaga dwóch segmentów T32".into(),
            ));
        }
        self.ensure_hybrid_bufs()?;
        self.ensure_prefill_bufs()?;
        self.ensure_hybrid_verify_bufs(4)?;
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        let saved_graphs = (
            std::mem::take(&mut self.hybrid_verify_graphs),
            std::mem::take(&mut self.hybrid_verify_graph_disabled),
        );
        let result = (|| {
            self.ensure_hybrid_verify_bufs(STEPS)?;
            let hidden = self.weights.descriptor.params.hidden_size;
            let hidden_bytes = hidden * 2;
            let direct_x = self
                .hybrid_prefill_b2_bufs
                .as_ref()
                .expect("target B2 przygotował scratch")
                .x
                .clone();
            for (lane, lane_tokens) in tokens.iter().enumerate() {
                let staging = &self
                    .hybrid_verify_bufs
                    .as_ref()
                    .expect("catch-up MTP ma scratch")
                    .host_staging[lane]
                    .embedding;
                let destination = staging
                    .host_ptr()
                    .expect("embedding catch-up ma mapowanie hosta");
                {
                    let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                        ForgeError::Unsupported(
                            "catch-up MTP B2 wymaga hostowego embeddingu".into(),
                        )
                    })?;
                    for (row, &token) in lane_tokens.iter().enumerate() {
                        let source = table
                            .get(token as usize * hidden..(token as usize + 1) * hidden)
                            .ok_or_else(|| {
                                ForgeError::Scheduler(format!(
                                    "token id {token} wykracza poza embedding catch-up"
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
                }
            }
            let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
            let mut pair_result = (|| {
                states[0].checkpoint(&self.stream)?;
                states[1].checkpoint(&self.stream)?;
                for (lane, state) in states.iter_mut().enumerate() {
                    if reset[lane] {
                        state.reset_pending(&mut mtp_kv, &self.stream)?;
                    }
                }
                for (lane, state) in states.iter_mut().enumerate() {
                    let prefill_x = self
                        .prefill_bufs
                        .as_ref()
                        .expect("catch-up MTP ma bufory prefill")
                        .x
                        .clone();
                    self.device.copy(
                        &direct_x,
                        lane * STEPS * hidden_bytes,
                        &prefill_x,
                        0,
                        STEPS * hidden_bytes,
                        &self.stream,
                    )?;
                    self.profile_catchup_start()?;
                    let execution = self.mtp_catchup_verified_prefix_pending(
                        state,
                        &mut mtp_kv,
                        STEPS,
                        lane,
                        None,
                    );
                    let profile_end = self.profile_catchup_end();
                    execution?;
                    profile_end?;
                    if fail_after_lane == Some(lane) {
                        return Err(ForgeError::Scheduler(format!(
                            "wymuszony błąd catch-up MTP po lane {lane}"
                        )));
                    }
                }
                states[0].validate_commit_catchup(STEPS)?;
                states[1].validate_commit_catchup(STEPS)?;
                self.device.synchronize()?;
                states[0].apply_commit_catchup();
                states[1].apply_commit_catchup();
                Ok(())
            })();
            if pair_result.is_err() {
                let rollback = rollback_mtp_pair_inner(
                    &mut states,
                    &mut mtp_kv,
                    &self.stream,
                    fail_rollback_lane,
                )
                .and_then(|_| self.device.synchronize());
                if let Err(rollback) = rollback {
                    let execution = pair_result.expect_err("catch-up pary zawiera błąd");
                    pair_result = Err(self.poison_mtp_runtime(format!(
                        "błąd catch-up MTP B2: {execution}; rollback pary nie powiódł się: {rollback}"
                    )));
                }
            }
            self.finish_mtp_runtime_pair(leases, states, mtp_kv, pair_result)
        })();
        restore_after(result, || {
            std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
            self.hybrid_verify_graphs = saved_graphs.0;
            self.hybrid_verify_graph_disabled = saved_graphs.1;
        })
    }

}
