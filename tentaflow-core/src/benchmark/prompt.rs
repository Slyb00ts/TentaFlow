// ===== File: benchmark/prompt.rs — per-request unique synthetic prompt generator sized in tokens =====

/// Short, common English words: with typical BPE vocabularies each word plus
/// its leading space encodes as roughly one token, so N words ≈ N tokens.
/// Real prompt_tokens are always taken from the API usage payload anyway.
const WORDS: &[&str] = &[
    "the", "river", "light", "stone", "wind", "forest", "night", "morning", "silver", "path",
    "mountain", "quiet", "voice", "shadow", "garden", "winter", "summer", "bridge", "harbor",
    "letter", "window", "candle", "music", "story", "journey", "island", "meadow", "thunder",
    "lantern", "compass", "orchard", "valley",
];

/// Builds a prompt of approximately `target_tokens` tokens whose body is unique
/// per `salt`. WHY: benchmarking an LLM server (llama.cpp/vLLM) with a repeated
/// prompt lets the server restore its prefill KV-cache checkpoint, so TTFT and
/// prefill_tps would measure a cached prefill instead of the real one. Injecting
/// a salt-dependent marker right after the fixed header, and picking every body
/// word from `(i, salt)`, makes the token sequence diverge from the second token
/// on — the server's prefix-cache cannot reuse a common prefix and we measure the
/// real prefill.
pub fn synthetic_prompt(target_tokens: u32, salt: u64) -> String {
    let header = "Continue the following text with a long, detailed story. Do not stop early.\n";
    // Unique marker right after the header so divergence starts at an early token;
    // the fixed header's cache is a handful of tokens and negligible.
    let marker = format!("Reference {salt}.\n");
    // The header + marker cost a handful of tokens; keep the total close to target.
    let filler_words = target_tokens.saturating_sub(20).max(1) as usize;
    let mut prompt = String::with_capacity(header.len() + marker.len() + filler_words * 8);
    prompt.push_str(header);
    prompt.push_str(&marker);
    let salt = salt as usize;
    for i in 0..filler_words {
        let idx = i.wrapping_mul(31).wrapping_add(salt) % WORDS.len();
        prompt.push_str(WORDS[idx]);
        prompt.push(if (i + 1) % 16 == 0 { '\n' } else { ' ' });
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_deterministic() {
        // Same salt reproduces the exact same prompt (needed for stable warmup).
        assert_eq!(synthetic_prompt(512, 7), synthetic_prompt(512, 7));
    }

    #[test]
    fn different_salt_differs_early() {
        let a = synthetic_prompt(512, 1);
        let b = synthetic_prompt(512, 2);
        assert_ne!(a, b);
        // Divergence must appear within the first ~30 chars after the header so the
        // server's prefix-cache cannot latch onto a shared prefix.
        let header =
            "Continue the following text with a long, detailed story. Do not stop early.\n";
        let a_tail = &a[header.len()..];
        let b_tail = &b[header.len()..];
        let window = 30.min(a_tail.len()).min(b_tail.len());
        assert_ne!(
            &a_tail[..window],
            &b_tail[..window],
            "prompts share the first {window} chars after the header"
        );
    }

    #[test]
    fn prompt_scales_with_target() {
        let small = synthetic_prompt(128, 0);
        let large = synthetic_prompt(8192, 0);
        assert!(large.len() > small.len() * 10);
        // Word count tracks the requested token budget (~1 token per word); the
        // marker adds one extra whitespace-separated token ("Reference").
        let words = large.split_whitespace().count();
        assert!(words >= 8000 && words <= 8300, "words = {}", words);
    }
}
