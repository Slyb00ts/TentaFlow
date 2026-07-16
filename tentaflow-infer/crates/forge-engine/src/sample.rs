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
    pub fn new(mut params: SamplingParams) -> Self {
        // Caller-supplied parameters are clamped into sane ranges so no
        // combination can empty the candidate set or inject NaN.
        if !params.temperature.is_finite() || params.temperature < 0.0 {
            params.temperature = 1.0;
        }
        if !params.top_p.is_finite() {
            params.top_p = 1.0;
        }
        params.top_p = params.top_p.clamp(0.0, 1.0);
        if params.top_p == 0.0 {
            params.top_p = 1.0;
        }
        if !params.min_p.is_finite() {
            params.min_p = 0.0;
        }
        params.min_p = params.min_p.clamp(0.0, 1.0);
        if !params.repetition_penalty.is_finite() || params.repetition_penalty <= 0.0 {
            params.repetition_penalty = 1.0;
        }
        let seed = params.seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
        });
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
