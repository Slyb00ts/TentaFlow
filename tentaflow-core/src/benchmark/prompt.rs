// ===== File: benchmark/prompt.rs — deterministic synthetic prompt generator sized in tokens =====

/// Short, common English words: with typical BPE vocabularies each word plus
/// its leading space encodes as roughly one token, so N words ≈ N tokens.
/// Real prompt_tokens are always taken from the API usage payload anyway.
const WORDS: &[&str] = &[
    "the", "river", "light", "stone", "wind", "forest", "night", "morning", "silver", "path",
    "mountain", "quiet", "voice", "shadow", "garden", "winter", "summer", "bridge", "harbor",
    "letter", "window", "candle", "music", "story", "journey", "island", "meadow", "thunder",
    "lantern", "compass", "orchard", "valley",
];

/// Builds a deterministic prompt of approximately `target_tokens` tokens.
/// The instruction header nudges the model to keep generating so runs actually
/// consume the `max_tokens` budget instead of stopping after one sentence.
pub fn synthetic_prompt(target_tokens: u32) -> String {
    let header = "Continue the following text with a long, detailed story. Do not stop early.\n";
    // The header itself costs a handful of tokens; keep the total close to target.
    let filler_words = target_tokens.saturating_sub(20).max(1) as usize;
    let mut prompt = String::with_capacity(header.len() + filler_words * 8);
    prompt.push_str(header);
    for i in 0..filler_words {
        prompt.push_str(WORDS[i % WORDS.len()]);
        prompt.push(if (i + 1) % 16 == 0 { '\n' } else { ' ' });
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_deterministic() {
        assert_eq!(synthetic_prompt(512), synthetic_prompt(512));
    }

    #[test]
    fn prompt_scales_with_target() {
        let small = synthetic_prompt(128);
        let large = synthetic_prompt(8192);
        assert!(large.len() > small.len() * 10);
        // Word count tracks the requested token budget (~1 token per word).
        let words = large.split_whitespace().count();
        assert!(words >= 8000 && words <= 8300, "words = {}", words);
    }
}
