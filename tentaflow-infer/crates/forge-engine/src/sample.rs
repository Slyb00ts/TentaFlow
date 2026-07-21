// ===== File: sample.rs — CPU sampling over f32 logits =====
// v0 samples on the host (one small vector per step); the GPU sampling kernel
// replaces this on the scheduler chunk without changing the interface.

use forge_types::{ForgeError, Result};

#[derive(Debug, Clone)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_k: usize,
    pub top_p: f32,
    pub min_p: f32,
    pub repetition_penalty: f32,
    pub seed: Option<u64>,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_k: 40,
            top_p: 0.95,
            min_p: 0.0,
            repetition_penalty: 1.0,
            seed: None,
        }
    }
}

impl SamplingParams {
    /// Clamp caller-supplied parameters into sane ranges so no combination
    /// can empty the candidate set or inject NaN. Both the CPU and GPU
    /// samplers run on the sanitized form.
    pub fn sanitized(mut self) -> Self {
        if !self.temperature.is_finite() || self.temperature < 0.0 {
            self.temperature = 1.0;
        }
        if !self.top_p.is_finite() {
            self.top_p = 1.0;
        }
        self.top_p = self.top_p.clamp(0.0, 1.0);
        if self.top_p == 0.0 {
            self.top_p = 1.0;
        }
        if !self.min_p.is_finite() {
            self.min_p = 0.0;
        }
        self.min_p = self.min_p.clamp(0.0, 1.0);
        if !self.repetition_penalty.is_finite() || self.repetition_penalty <= 0.0 {
            self.repetition_penalty = 1.0;
        }
        self
    }

    /// The seed both samplers stream from: caller-provided or time-derived.
    pub fn resolve_seed(&self) -> u64 {
        self.seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
        })
    }
}

/// Per-token log-probability report (SPEC §8.1.2 `logprobs`/`top_logprobs`):
/// the sampled token's id + log-probability, plus the top-N alternatives at
/// that position (id, log-probability), ordered most-probable first. Computed
/// on the host from the full logits the CPU sampler already holds.
#[derive(Debug, Clone)]
pub struct TokenLogprob {
    pub token: u32,
    pub logprob: f32,
    pub top: Vec<(u32, f32)>,
}

/// Add caller-supplied `logit_bias` (`{token_id: bias}`, bias in [-100, 100];
/// ±100 ≈ hard force/ban) to the raw logits before selection. Out-of-range ids
/// are ignored. Applied only on the CPU sampler path (full host logits).
pub fn apply_logit_bias(logits: &mut [f32], bias: &[(u32, f32)]) {
    for &(id, b) in bias {
        if let Some(l) = logits.get_mut(id as usize) {
            *l += b;
        }
    }
}

/// Suppress every end-of-sequence id (force its logit to -inf) so the sequence
/// cannot terminate before it has produced `min_tokens` tokens (SPEC §8.1.2
/// `min_tokens`). No-op once the floor is reached.
pub fn suppress_eos(logits: &mut [f32], eos_ids: &[u32], generated: usize, min_tokens: usize) {
    if generated >= min_tokens {
        return;
    }
    for &e in eos_ids {
        if let Some(l) = logits.get_mut(e as usize) {
            *l = f32::NEG_INFINITY;
        }
    }
}

/// Log-softmax the full logits and report the `sampled` token's log-probability
/// plus the `top_n` most-probable tokens (each as `(id, logprob)`), ordered
/// most-probable first. Numerically stable via the max-shifted log-sum-exp; a
/// -inf logit (e.g. grammar/`min_tokens`-masked) maps to a -inf log-probability.
pub fn compute_logprob(logits: &[f32], sampled: u32, top_n: usize) -> TokenLogprob {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = if max.is_finite() {
        logits.iter().map(|&l| (l - max).exp()).sum()
    } else {
        0.0
    };
    // log-sum-exp; guards the degenerate all -inf case (sum == 0 → lse -inf).
    let lse = if sum > 0.0 { max + sum.ln() } else { f32::NEG_INFINITY };
    let lp = |l: f32| l - lse;

    let sampled_lp = logits
        .get(sampled as usize)
        .map(|&l| lp(l))
        .unwrap_or(f32::NEG_INFINITY);

    let mut top = Vec::new();
    if top_n > 0 && !logits.is_empty() {
        let mut idx: Vec<(u32, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, &l)| (i as u32, l))
            .collect();
        let k = top_n.min(idx.len());
        idx.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
        idx.truncate(k);
        idx.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        top = idx.into_iter().map(|(i, l)| (i, lp(l))).collect();
    }
    TokenLogprob {
        token: sampled,
        logprob: sampled_lp,
        top,
    }
}

/// xorshift64* — deterministic per-request stream, no rand dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545F4914F6CDD1D);
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }
}

pub struct Sampler {
    params: SamplingParams,
    rng: Rng,
}

impl Sampler {
    pub fn new(params: SamplingParams) -> Self {
        let params = params.sanitized();
        let seed = params.resolve_seed();
        Sampler {
            params,
            rng: Rng::new(seed),
        }
    }

    /// Pick the next token. `recent` feeds the repetition penalty.
    pub fn sample(&mut self, logits: &[f32], recent: &[u32]) -> Result<u32> {
        if logits.is_empty() {
            return Err(ForgeError::Scheduler("empty logits".into()));
        }
        let p = &self.params;

        let mut logits = logits.to_vec();
        if p.repetition_penalty != 1.0 {
            // Penalize each distinct token once — repeated occurrences must
            // not compound the penalty exponentially.
            let distinct: std::collections::HashSet<u32> = recent.iter().copied().collect();
            for t in distinct {
                if let Some(l) = logits.get_mut(t as usize) {
                    *l = if *l > 0.0 {
                        *l / p.repetition_penalty
                    } else {
                        *l * p.repetition_penalty
                    };
                }
            }
        }

        // Greedy path: temperature 0 means argmax, no randomness.
        if p.temperature <= 0.0 {
            let (best, _) = logits
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .expect("non-empty");
            return Ok(best as u32);
        }

        let inv_t = 1.0 / p.temperature;
        let mut candidates: Vec<(u32, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, &l)| (i as u32, l * inv_t))
            .collect();
        candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

        if p.top_k > 0 && p.top_k < candidates.len() {
            candidates.truncate(p.top_k);
        }

        // Softmax over the surviving candidates (max-shifted for stability).
        let m = candidates[0].1;
        let mut total = 0.0f32;
        for c in candidates.iter_mut() {
            c.1 = (c.1 - m).exp();
            total += c.1;
        }
        for c in candidates.iter_mut() {
            c.1 /= total;
        }

        if p.min_p > 0.0 {
            let floor = p.min_p * candidates[0].1;
            candidates.retain(|c| c.1 >= floor);
            if candidates.is_empty() {
                // Numerical edge (NaN probabilities): fall back to greedy.
                return Ok(logits
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map(|(i, _)| i as u32)
                    .unwrap_or(0));
            }
        }

        if p.top_p < 1.0 {
            let mut cum = 0.0;
            let mut cut = candidates.len();
            for (i, c) in candidates.iter().enumerate() {
                cum += c.1;
                if cum >= p.top_p {
                    cut = i + 1;
                    break;
                }
            }
            candidates.truncate(cut);
        }

        let total: f32 = candidates.iter().map(|c| c.1).sum();
        let mut r = self.rng.next_f32() * total;
        for c in &candidates {
            r -= c.1;
            if r <= 0.0 {
                return Ok(c.0);
            }
        }
        Ok(candidates.last().expect("non-empty").0)
    }
}

/// Per-sequence state for on-GPU sampling: the sanitized params, the
/// deterministic (seed, step) counter the draw kernel hashes, and the
/// distinct-token list feeding the repetition-penalty kernel. The logits
/// themselves never leave the device — `Model::sample_last_logits` reads
/// back only the 8-byte result.
pub struct GpuSampler {
    params: SamplingParams,
    seed: u64,
    step: u64,
    /// Distinct generated token ids, i32 for direct device upload. Only
    /// maintained when the penalty is active.
    penalized: Vec<i32>,
    seen: std::collections::HashSet<u32>,
}

impl GpuSampler {
    /// Whether `params` (sanitized) fall inside what the GPU kernels
    /// implement: greedy always; a categorical draw only for
    /// 1..=SAMPLE_MAX_TOPK top-k (the kernel merges per-block top-k lists,
    /// so an unbounded candidate set has no GPU form).
    pub fn compatible(params: &SamplingParams) -> bool {
        let p = params.clone().sanitized();
        p.temperature <= 0.0 || (p.top_k >= 1 && p.top_k <= forge_kernels::SAMPLE_MAX_TOPK)
    }

    pub fn new(params: SamplingParams) -> Self {
        let params = params.sanitized();
        let seed = params.resolve_seed();
        GpuSampler {
            params,
            seed,
            step: 0,
            penalized: Vec::new(),
            seen: std::collections::HashSet::new(),
        }
    }

    /// Record a generated token for the repetition penalty (distinct ids
    /// only — the penalty must not compound across repeats).
    pub fn note_token(&mut self, id: u32) {
        if self.params.repetition_penalty != 1.0 && self.seen.insert(id) {
            self.penalized.push(id as i32);
        }
    }

    pub fn params(&self) -> &SamplingParams {
        &self.params
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Distinct penalized ids, empty when the penalty is inactive.
    pub fn penalized(&self) -> &[i32] {
        &self.penalized
    }

    /// Current step counter; advances once per drawn token.
    pub fn next_step(&mut self) -> u64 {
        let s = self.step;
        self.step += 1;
        s
    }

    /// Snapshot this sequence's params for one batched decode step, advancing
    /// the deterministic draw counter exactly like `sample_last_logits`.
    /// Greedy (temperature <= 0) collapses to `k = 1`, which the batched
    /// top-k kernel resolves to the argmax (ties to the lowest id) — bit-exact
    /// against the single-row greedy sampler.
    pub fn batch_params(&mut self, vocab: usize) -> SeqSampleParams {
        let p = &self.params;
        let greedy = p.temperature <= 0.0;
        let step = self.step;
        self.step += 1;
        SeqSampleParams {
            greedy,
            k: if greedy {
                1
            } else {
                p.top_k.clamp(1, vocab) as i32
            },
            inv_t: if greedy { 1.0 } else { 1.0 / p.temperature },
            top_p: p.top_p,
            min_p: p.min_p,
            seed: self.seed,
            step,
            penalty: p.repetition_penalty,
            penalty_ids: if p.repetition_penalty != 1.0 {
                self.penalized.clone()
            } else {
                Vec::new()
            },
        }
    }
}

/// One sequence's sampling parameters for a batched decode step, flattened for
/// upload into the per-seq GPU sampler param arrays.
#[derive(Debug, Clone)]
pub struct SeqSampleParams {
    pub greedy: bool,
    pub k: i32,
    pub inv_t: f32,
    pub top_p: f32,
    pub min_p: f32,
    pub seed: u64,
    pub step: u64,
    pub penalty: f32,
    pub penalty_ids: Vec<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argmax(logits: &[f32]) -> u32 {
        logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i as u32)
            .unwrap()
    }

    #[test]
    fn logit_bias_forces_and_bans_tokens() {
        let base = [1.0f32, 2.0, 0.5, 3.0];
        // Greedy pick without bias is token 3.
        assert_eq!(argmax(&base), 3);

        // A +100 bias forces an otherwise-unlikely token.
        let mut l = base;
        apply_logit_bias(&mut l, &[(2, 100.0)]);
        assert_eq!(argmax(&l), 2);

        // A -100 bias bans the natural argmax; the runner-up wins.
        let mut l = base;
        apply_logit_bias(&mut l, &[(3, -100.0)]);
        assert_eq!(argmax(&l), 1);

        // Out-of-range ids are ignored (no panic, no effect).
        let mut l = base;
        apply_logit_bias(&mut l, &[(999, 100.0)]);
        assert_eq!(l, base);
    }

    #[test]
    fn min_tokens_suppresses_eos_until_floor() {
        let eos = [2u32, 3];
        // Below the floor, every eos logit is forced to -inf.
        let mut l = [1.0f32, 0.0, 5.0, 4.0];
        suppress_eos(&mut l, &eos, 1, 20);
        assert_eq!(l[2], f32::NEG_INFINITY);
        assert_eq!(l[3], f32::NEG_INFINITY);
        // Non-eos logits are untouched, so the argmax is now a non-eos token.
        assert_eq!(argmax(&l), 0);

        // At/above the floor it is a no-op.
        let mut l = [1.0f32, 0.0, 5.0, 4.0];
        suppress_eos(&mut l, &eos, 20, 20);
        assert_eq!(argmax(&l), 2);
    }

    #[test]
    fn logprob_is_log_softmax_and_top1_equals_argmax() {
        let logits = [2.0f32, 1.0, 0.0, -1.0];
        let sampled = argmax(&logits); // 0
        let lp = compute_logprob(&logits, sampled, 3);
        assert_eq!(lp.token, 0);

        // Reference log-softmax of the sampled token.
        let max = 2.0f32;
        let sum: f32 = logits.iter().map(|&l| (l - max).exp()).sum();
        let lse = max + sum.ln();
        assert!((lp.logprob - (logits[0] - lse)).abs() < 1e-5);

        // All log-probabilities are <= 0 and the exp-sum over the full vocab is 1.
        assert!(lp.logprob <= 0.0);
        let total: f32 = logits.iter().map(|&l| (l - lse).exp()).sum();
        assert!((total - 1.0).abs() < 1e-4);

        // top is ordered most-probable first and its head equals the argmax.
        assert_eq!(lp.top.len(), 3);
        assert_eq!(lp.top[0].0, 0);
        assert!(lp.top[0].1 >= lp.top[1].1);
        assert!(lp.top[1].1 >= lp.top[2].1);
        // The sampled token's logprob matches its entry in the top list.
        assert!((lp.top[0].1 - lp.logprob).abs() < 1e-6);
    }

    #[test]
    fn logprob_zero_top_still_reports_sampled() {
        let logits = [0.5f32, 2.5, 1.0];
        let lp = compute_logprob(&logits, 1, 0);
        assert_eq!(lp.token, 1);
        assert!(lp.top.is_empty());
        assert!(lp.logprob <= 0.0);
    }
}
