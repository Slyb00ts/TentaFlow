// ===== File: sample.rs — launchery samplingu: argmax, top-k, kary =====
use super::*;

impl Kernels {
    /// Batched greedy argmax over `logits` ([n_seqs, vocab] f32): one block per
    /// sequence, ties to the lowest id. `out_ids` receives n_seqs i32 token ids.
    pub fn sample_batched_argmax_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("argmax_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(logits)
            .scalar(vocab as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched categorical draw over `logits` ([n_seqs, vocab] f32) with
    /// per-seq params (k / inv_temp / top_p / min_p / seed / step arrays, each
    /// n_seqs long). `out_ids` receives n_seqs i32 token ids. `logits` is
    /// mutated (top-k masking) — valid because it is regenerated every step.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_topk_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        k_arr: &DevBuffer,
        inv_t_arr: &DevBuffer,
        top_p_arr: &DevBuffer,
        min_p_arr: &DevBuffer,
        seed_arr: &DevBuffer,
        step_arr: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_batched_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        // Two passes mirroring the fast single-row path: per-chunk partial
        // top-k lists (grid chunks × seqs, slices staged in shared memory),
        // then a per-sequence merge + sampling replay. The one-block-per-seq
        // k-rounds-over-vocab kernel this replaces cost ~10 ms at k=40 on a
        // 152k vocab.
        let chunk = SAMPLE_CHUNK;
        let n_blocks = vocab.div_ceil(chunk);
        if n_blocks > SAMPLE_MAX_VOCAB / SAMPLE_CHUNK {
            return Err(ForgeError::Unsupported(format!(
                "sample_batched_topk: vocab {vocab} needs {n_blocks} chunks over the cap"
            )));
        }
        let need_parts = n_seqs * SAMPLE_SCRATCH_PAIRS;
        let mut sc = self
            .sample_parts
            .lock()
            .map_err(|_| ForgeError::Kernel("sample parts scratch poisoned".into()))?;
        if sc.cap < need_parts {
            sc.vals = Some(self.device.alloc(
                need_parts * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.idx = Some(
                self.device
                    .alloc(need_parts * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap = need_parts;
        }
        let part_vals = sc.vals.as_ref().expect("parts allocated");
        let part_idx = sc.idx.as_ref().expect("parts allocated");

        let partial = self.artifacts.get("topk_batched_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, n_seqs as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(part_vals)
            .buf(part_idx)
            .buf(logits)
            .scalar(vocab as i64)
            .scalar(chunk as i64)
            .buf(k_arr);
        self.device.launch(partial, &cfg, &args, stream)?;

        let fin = self.artifacts.get("topk_batched_final_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(part_vals)
            .buf(part_idx)
            .scalar(n_blocks as i64)
            .scalar(vocab as i64)
            .buf(k_arr)
            .buf(inv_t_arr)
            .buf(top_p_arr)
            .buf(min_p_arr)
            .buf(seed_arr)
            .buf(step_arr);
        self.device.launch(fin, &cfg, &args, stream)
    }

    /// Batchowe kary in-place z histogramów unikalnych tokenów.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_penalize_f32(
        &self,
        logits: &DevBuffer,
        vocab: usize,
        ids: &DevBuffer,
        counts: &DevBuffer,
        offsets: &DevBuffer,
        penalties: &DevBuffer,
        frequency_penalties: &DevBuffer,
        presence_penalties: &DevBuffer,
        n_seqs: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .scalar(vocab as i64)
            .buf(ids)
            .buf(counts)
            .buf(offsets)
            .buf(penalties)
            .buf(frequency_penalties)
            .buf(presence_penalties);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// In-place repetition penalty over `n_ids` distinct token ids staged in
    /// `ids` (i32). Callers must deduplicate: the kernel applies the penalty
    /// once per listed id.
    pub fn sample_penalize_f32(
        &self,
        logits: &DevBuffer,
        ids: &DevBuffer,
        n_ids: usize,
        penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_f32")?;
        let cfg = LaunchConfig::linear(n_ids as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(ids)
            .scalar(n_ids as i64)
            .scalar(penalty);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Nakłada kary z kompaktowego histogramu i wybiera greedy w jednym launchu.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_penalized_argmax_f32(
        &self,
        out: &DevBuffer,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.validate_penalty_histogram(
            Some(out),
            logits,
            ids,
            counts,
            n_ids,
            vocab,
            repetition_penalty,
            frequency_penalty,
            presence_penalty,
        )?;
        let kernel = self.artifacts.get("penalized_argmax_f32")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(logits)
            .buf(ids)
            .buf(counts)
            .scalar(n_ids as i64)
            .scalar(vocab as i64)
            .scalar(repetition_penalty)
            .scalar(frequency_penalty)
            .scalar(presence_penalty);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Nakłada kary z histogramu unikalnych IDs przed równoległym samplingiem.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_penalize_histogram_f32(
        &self,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.validate_penalty_histogram(
            None,
            logits,
            ids,
            counts,
            n_ids,
            vocab,
            repetition_penalty,
            frequency_penalty,
            presence_penalty,
        )?;
        let kernel = self.artifacts.get("penalize_histogram_f32")?;
        let config = LaunchConfig::linear(n_ids as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(ids)
            .buf(counts)
            .scalar(n_ids as i64)
            .scalar(vocab as i64)
            .scalar(repetition_penalty)
            .scalar(frequency_penalty)
            .scalar(presence_penalty);
        self.device.launch(kernel, &config, &args, stream)
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_penalty_histogram(
        &self,
        out: Option<&DevBuffer>,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
    ) -> Result<()> {
        if n_ids == 0 || vocab == 0 || n_ids > vocab {
            return Err(ForgeError::Kernel(
                "fused sampling wymaga niepustego histogramu nie większego od słownika".into(),
            ));
        }
        let logits_bytes = checked_buffer_bytes("sampling logits", &[vocab], 4)?;
        let histogram_bytes = checked_buffer_bytes("sampling histogram", &[n_ids], 4)?;
        if out.is_some_and(|buffer| buffer.len() < 8)
            || logits.len() < logits_bytes
            || ids.len() < histogram_bytes
            || counts.len() < histogram_bytes
        {
            return Err(ForgeError::Kernel(
                "bufor fused sampling jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        if !repetition_penalty.is_finite()
            || repetition_penalty <= 0.0
            || !frequency_penalty.is_finite()
            || !presence_penalty.is_finite()
        {
            return Err(ForgeError::Kernel(
                "parametry kar fused sampling muszą być skończone".into(),
            ));
        }
        #[cfg(debug_assertions)]
        {
            // Kopie histogramu mogą oczekiwać na nieblokującym streamie modelu;
            // synchroniczny odczyt hostowy nie może ich wyprzedzić.
            self.device.synchronize()?;
            let mut host_ids = vec![0u8; histogram_bytes];
            let mut host_counts = vec![0u8; histogram_bytes];
            self.device.read(ids, 0, &mut host_ids)?;
            self.device.read(counts, 0, &mut host_counts)?;
            let mut unique = std::collections::HashSet::with_capacity(n_ids);
            for (id, count) in host_ids.chunks_exact(4).zip(host_counts.chunks_exact(4)) {
                let id = i32::from_le_bytes(id.try_into().expect("fragment i32"));
                let count = i32::from_le_bytes(count.try_into().expect("fragment i32"));
                if id < 0 || id as usize >= vocab || count <= 0 || !unique.insert(id) {
                    return Err(ForgeError::Kernel(
                        "histogram kar wymaga unikalnych IDs w zakresie vocab i dodatnich liczników"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Greedy argmax over f32 logits; the winning index lands in the first
    /// 4 bytes of `out` (i32) and its logprob slot (f32, 0 for greedy) in the
    /// next 4. Ties resolve to the lowest index like a sequential CPU scan.
    /// `scratch_vals`/`scratch_idx` hold the per-block partials
    /// (>= SAMPLE_SCRATCH_PAIRS entries each).
    pub fn sample_argmax_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        if n_blocks > SAMPLE_SCRATCH_PAIRS {
            return Err(ForgeError::Unsupported(format!(
                "sample_argmax: vocab {vocab} exceeds scratch capacity"
            )));
        }
        let kp = self.artifacts.get("argmax_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("argmax_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(n_blocks as i64);
        self.device.launch(kf, &cfg, &args, stream)
    }

    /// Categorical draw over f32 logits: top-k (k <= SAMPLE_MAX_TOPK)
    /// selection, temperature softmax, min-p floor, top-p cut, then a
    /// deterministic counter-hash draw on (seed, step). The sampled id (i32)
    /// lands in the first 4 bytes of `out`, its top-k-softmax logprob (f32)
    /// in the next 4.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_topk_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        k: usize,
        inv_t: f32,
        top_p: f32,
        min_p: f32,
        seed: u64,
        step: u64,
        stream: &Stream,
    ) -> Result<()> {
        if k == 0 || k > SAMPLE_MAX_TOPK {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: k {k} outside 1..={SAMPLE_MAX_TOPK}"
            )));
        }
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        let kp = self.artifacts.get("topk_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64)
            .scalar(k as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("topk_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar((n_blocks * k) as i64)
            .scalar(k as i64)
            .scalar(inv_t)
            .scalar(top_p)
            .scalar(min_p)
            .scalar(seed)
            .scalar(step);
        self.device.launch(kf, &cfg, &args, stream)
    }

}
