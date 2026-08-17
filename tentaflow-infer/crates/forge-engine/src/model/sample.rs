// ===== File: model/sample.rs — logity i sampling =====
use super::*;

impl Model {
    /// Logity z rezydualu leżącego na granicy etapu — dopełnienie
    /// `prefill_stage` na etapie OSTATNIM.
    ///
    /// Bierze wiersz `row` (zwykle ostatni token chunka), normalizuje go i
    /// przepuszcza przez głowę tą samą ścieżką co dekodowanie.
    pub fn stage_logits(&mut self, row: usize) -> Result<Vec<f32>> {
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let pb = self
            .prefill_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("bufory prefillu jeszcze nie istnieją".into()))?;
        self.device.copy(
            &pb.x,
            row * hidden * 2,
            &self.bufs.x,
            0,
            hidden * 2,
            &self.stream,
        )?;
        self.trace_f16("result_norm", &self.bufs.x, 0, hidden);
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        self.trace_f32("result_output", &self.bufs.logits, vocab);
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            &self.stream,
        )?;
        self.synchronize_kv_fatal("odczyt logitów etapu pipeline")?;
        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        Ok(unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec())
    }

    /// Wykonuje dense prefill i pozostawia logity ostatniego tokenu na urządzeniu.
    /// GPU sampling dołączony do tego samego streamu zapewnia kolejność bez
    /// pełnego odczytu słownika i pośredniej synchronizacji hosta.
    pub fn prefill_chunk_device_logits(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "device-only prefill chunk obsługuje wyłącznie model dense".into(),
            ));
        }
        if self.tp.is_some() {
            // Sekwencyjny prefill zostawia logity ostatniego tokenu w
            // `bufs.logits` — dokładnie tam, gdzie oczekuje ich sampling GPU.
            self.prefill_dense_split(seq, tokens, SplitPrefillLogits::Device)?;
            return Ok(());
        }
        self.refuse_split_prefill()?;
        self.profile_target_start()?;
        let t = self.prefill_forward(seq, tokens, false)?;
        let hidden = self.weights.descriptor.params.hidden_size;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("prefill_forward allocated");
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &self.bufs.x,
            0,
            hidden * 2,
            &self.stream,
        )?;
        self.trace_f16("result_norm", &self.bufs.x, 0, hidden);
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        self.trace_f32(
            "result_output",
            &self.bufs.logits,
            self.weights.descriptor.params.vocab_size,
        );
        self.profile_target_end()
    }

    /// Wykonuje równy dense prefill i pozostawia B wierszy logitów na GPU.
    pub fn prefill_batch_device_logits(
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
                model.prefill_forward_lanes(seqs, token_lanes, false, None)?;
                let hidden = model.weights.descriptor.params.hidden_size;
                let row_bytes = hidden * 2;
                let source = model
                    .prefill_bufs
                    .as_ref()
                    .expect("batch prefill ma bufory")
                    .x
                    .clone();
                let destination = model
                    .batch_bufs
                    .as_ref()
                    .expect("batch prefill ma batch scratch")
                    .x
                    .clone();
                for lane in 0..batch {
                    model.device.copy(
                        &source,
                        (lane * n_tokens + n_tokens - 1) * row_bytes,
                        &destination,
                        lane * row_bytes,
                        row_bytes,
                        &model.stream,
                    )?;
                }
                let logits = model
                    .batch_bufs
                    .as_ref()
                    .expect("batch prefill ma batch scratch")
                    .logits
                    .clone();
                model.logits_gemm(&logits, &destination, batch, &model.stream)
            },
            |model| model.synchronize_kv_fatal("rollback dense prefill"),
            |model, seqs, snapshots| {
                restore_prefill_seq_snapshots(&mut model.kv, seqs, snapshots);
                model.pt_seq = 0;
            },
        )
    }

    /// Próbkuje B wierszy dense prefill i odczytuje tylko token ID każdego lane.
    pub fn sample_prefill_batch_logits(
        &mut self,
        samplers: &mut [&mut GpuSampler],
    ) -> Result<Vec<u32>> {
        self.ensure_kv_reuse_healthy()?;
        let operation = (|| {
            let batch = samplers.len();
            if !matches!(batch, 4 | 8 | 16) {
                return Err(ForgeError::Scheduler(
                    "sampling prefill wymaga B4/B8/B16".into(),
                ));
            }
            let vocab = self.weights.descriptor.params.vocab_size;
            let params = samplers
                .iter_mut()
                .map(|sampler| sampler.batch_params(vocab))
                .collect::<Vec<_>>();
            let logits = self
                .batch_bufs
                .as_ref()
                .ok_or_else(|| ForgeError::Scheduler("brak logits batch prefill".into()))?
                .logits
                .clone();
            self.batch_sample_from(&logits, batch, &params)?;
            let buffers = self.batch_bufs.as_ref().expect("batch sampler ma bufory");
            let pinned_out = buffers.pinned_out.clone();
            self.device
                .copy(&buffers.out_ids, 0, &pinned_out, 0, batch * 4, &self.stream)?;
            Ok((pinned_out, batch, vocab))
        })();
        let (pinned_out, batch, vocab) =
            settle_kv_operation(operation, "sampling finalnego dense prefill", || {
                self.synchronize_kv_fatal("sampling finalnego dense prefill")
            })?;
        let output = pinned_out.host_ptr().expect("pinned output ma mapowanie") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(output, batch) };
        ids.iter()
            .enumerate()
            .map(|(lane, &id)| {
                if id < 0 || id as usize >= vocab {
                    Err(ForgeError::Kernel(format!(
                        "batch sampler prefill zwrócił token {id} poza słownikiem dla lane {lane}"
                    )))
                } else {
                    Ok(id as u32)
                }
            })
            .collect()
    }

    /// Whether requests with these sampling params can sample on the GPU:
    /// greedy always fits; a categorical draw needs a bounded top-k and a
    /// vocab within the kernel's merge capacity.
    pub fn gpu_sampling_supported(&self, params: &SamplingParams) -> bool {
        let vocab = self.weights.descriptor.params.vocab_size;
        GpuSampler::compatible(params)
            && (params.clone().sanitized().temperature <= 0.0
                || vocab <= forge_kernels::SAMPLE_MAX_VOCAB)
    }

    /// Sample from the logits currently resident in the device logits buffer
    /// (valid right after `step_launch`/`step`/`prefill_chunk` — before any
    /// other sequence runs). Launches ride the model stream, so this also
    /// works back-to-back with an un-synced `step_launch`.
    pub fn sample_last_logits(&mut self, sampler: &mut GpuSampler) -> Result<u32> {
        let p = &self.weights.descriptor.params;
        let b = &self.bufs;
        let sp = sampler.params().clone();

        let penalized = sampler.penalized();
        let penalty_counts = sampler.penalty_counts();
        if sp.has_penalties() && !penalized.is_empty() {
            if penalized.len() != penalty_counts.len()
                || penalized.len() * 4 > b.pinned_penalty.len()
            {
                return Err(ForgeError::Scheduler(format!(
                    "penalty histogram {} exceeds staging capacity",
                    penalized.len()
                )));
            }
            let ids_host = b
                .pinned_penalty
                .host_ptr()
                .expect("pinned buffer has host mapping");
            let counts_host = b
                .pinned_penalty_counts
                .host_ptr()
                .expect("pinned buffer has host mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    penalized.as_ptr() as *const u8,
                    ids_host,
                    penalized.len() * 4,
                );
                std::ptr::copy_nonoverlapping(
                    penalty_counts.as_ptr() as *const u8,
                    counts_host,
                    penalized.len() * 4,
                );
            }
            self.device.copy(
                &b.pinned_penalty,
                0,
                &b.penalty_ids,
                0,
                penalized.len() * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b.pinned_penalty_counts,
                0,
                &b.penalty_counts,
                0,
                penalized.len() * 4,
                &self.stream,
            )?;
            self.kernels.sample_penalize_histogram_f32(
                &b.logits,
                &b.penalty_ids,
                &b.penalty_counts,
                penalized.len(),
                p.vocab_size,
                sp.repetition_penalty,
                sp.frequency_penalty,
                sp.presence_penalty,
                &self.stream,
            )?;
        }

        if sp.temperature <= 0.0 {
            self.kernels.sample_argmax_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                &self.stream,
            )?;
        } else {
            let k = sp.top_k.min(p.vocab_size);
            self.kernels.sample_topk_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                k,
                1.0 / sp.temperature,
                sp.top_p,
                sp.min_p,
                sampler.seed(),
                sampler.next_step(),
                &self.stream,
            )?;
        }

        self.device
            .copy(&b.sample_out, 0, &b.pinned_sample, 0, 8, &self.stream)?;
        self.device.synchronize()?;

        let sp_host = b
            .pinned_sample
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const i32;
        let id = unsafe { *sp_host };
        if id < 0 || id as usize >= p.vocab_size {
            return Err(ForgeError::Kernel(format!(
                "GPU sampler returned out-of-range token {id}"
            )));
        }
        Ok(id as u32)
    }

    /// Odczytuje pełne logity pozostałe po ostatnim kroku ścieżki
    /// jednosekwencyjnej (`prefill_chunk` / `step_and_sample`). Symetryczne do
    /// `read_batch_logits` i służy temu samemu celowi: porównaniu numerycznemu
    /// obu ścieżek decode, które używają różnych kerneli.
    pub fn read_single_logits(&self) -> Result<Vec<f32>> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut logits = vec![0.0f32; vocab];
        self.device
            .read(&self.bufs.logits, 0, bytemuck::cast_slice_mut(&mut logits))?;
        Ok(logits)
    }

    /// Odczytuje pełne logity pozostałe po ostatnim dense batch decode.
    ///
    /// Metoda służy do audytu numerycznego. Bez tieringu wiersze zachowują
    /// kolejność lane'ów przekazaną do `batched_decode`.
    pub fn read_batch_logits(&self, batch: usize) -> Result<Vec<f32>> {
        let buffers = self
            .batch_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak buforów batch decode".into()))?;
        if batch == 0 || batch > buffers.cap {
            return Err(ForgeError::Scheduler(format!(
                "odczyt logitów wymaga batch 1..={}, otrzymano {batch}",
                buffers.cap
            )));
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut logits = vec![0.0f32; batch * vocab];
        self.device
            .read(&buffers.logits, 0, bytemuck::cast_slice_mut(&mut logits))?;
        Ok(logits)
    }

    /// GPU sampling over `b` contiguous live rows of device logits.
    pub(crate) fn batch_sample_from(
        &mut self,
        logits: &DevBuffer,
        b: usize,
        params: &[SeqSampleParams],
    ) -> Result<()> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        let stream = &self.stream;

        // Jedno uruchomienie kernela obsługuje wszystkie aktywne kary batcha.
        let any_penalty = params.iter().any(|p| !p.penalty_ids.is_empty());
        if any_penalty {
            let mut ids_flat: Vec<i32> = Vec::new();
            let mut counts_flat: Vec<i32> = Vec::new();
            let mut offsets: Vec<i32> = Vec::with_capacity(b + 1);
            let mut vals: Vec<f32> = Vec::with_capacity(b);
            let mut frequency: Vec<f32> = Vec::with_capacity(b);
            let mut presence: Vec<f32> = Vec::with_capacity(b);
            offsets.push(0);
            for p in params.iter() {
                if p.penalty_ids.len() != p.penalty_counts.len() {
                    return Err(ForgeError::Scheduler(
                        "penalty histogram ids/counts length mismatch".into(),
                    ));
                }
                ids_flat.extend_from_slice(&p.penalty_ids);
                counts_flat.extend_from_slice(&p.penalty_counts);
                offsets.push(ids_flat.len() as i32);
                vals.push(p.penalty);
                frequency.push(p.frequency_penalty);
                presence.push(p.presence_penalty);
            }
            if ids_flat.len() * 4 > bb.pinned_pen_ids.len() {
                return Err(ForgeError::Scheduler("penalty id staging overflow".into()));
            }
            Self::stage(
                &self.device,
                &bb.pinned_pen_ids,
                &bb.pen_ids,
                bytemuck::cast_slice(&ids_flat),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_counts,
                &bb.pen_counts,
                bytemuck::cast_slice(&counts_flat),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_offsets,
                &bb.pen_offsets,
                bytemuck::cast_slice(&offsets),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_vals,
                &bb.pen_vals,
                bytemuck::cast_slice(&vals),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_frequency,
                &bb.pen_frequency,
                bytemuck::cast_slice(&frequency),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_presence,
                &bb.pen_presence,
                bytemuck::cast_slice(&presence),
                stream,
            )?;
            self.kernels.sample_batched_penalize_f32(
                logits,
                vocab,
                &bb.pen_ids,
                &bb.pen_counts,
                &bb.pen_offsets,
                &bb.pen_vals,
                &bb.pen_frequency,
                &bb.pen_presence,
                b,
                stream,
            )?;
        }

        if params.iter().all(|p| p.greedy) {
            self.kernels
                .sample_batched_argmax_f32(&bb.out_ids, logits, b, vocab, stream)?;
            return Ok(());
        }

        // Mixed / sampled batch: per-seq top-k (k = 1 lanes reproduce argmax).
        let mut ks = Vec::with_capacity(b);
        let mut inv_t = Vec::with_capacity(b);
        let mut top_p = Vec::with_capacity(b);
        let mut min_p = Vec::with_capacity(b);
        let mut seed = Vec::with_capacity(b);
        let mut step = Vec::with_capacity(b);
        for p in params.iter() {
            ks.push(p.k);
            inv_t.push(p.inv_t);
            top_p.push(p.top_p);
            min_p.push(p.min_p);
            seed.push(p.seed);
            step.push(p.step);
        }
        // Params staged into one pinned block, then copied per array.
        let host = bb.pinned_samp.host_ptr().expect("pinned mapping");
        let mut off = 0usize;
        let put = |bytes: &[u8], off: &mut usize| unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host.add(*off), bytes.len());
            *off += bytes.len();
        };
        put(bytemuck::cast_slice(&ks), &mut off);
        put(bytemuck::cast_slice(&inv_t), &mut off);
        put(bytemuck::cast_slice(&top_p), &mut off);
        put(bytemuck::cast_slice(&min_p), &mut off);
        put(bytemuck::cast_slice(&seed), &mut off);
        put(bytemuck::cast_slice(&step), &mut off);
        let mut o = 0usize;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_k, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_inv_t, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_top_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_min_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_seed, 0, b * 8, stream)?;
        o += b * 8;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_step, 0, b * 8, stream)?;
        self.kernels.sample_batched_topk_f32(
            &bb.out_ids,
            logits,
            b,
            vocab,
            &bb.samp_k,
            &bb.samp_inv_t,
            &bb.samp_top_p,
            &bb.samp_min_p,
            &bb.samp_seed,
            &bb.samp_step,
            stream,
        )
    }
}
