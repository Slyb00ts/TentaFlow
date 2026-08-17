// ===== File: model_profile/qwen35.rs — Qwen3.5/3.6 (hybryda DeltaNet) =====
//
// Jedyna rodzina, dla której natywne MTP jest zweryfikowane wykonawczo, więc
// spekulacja jest tu domyślnie włączona — na innych modelach nadal `off`.
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut qwen35 = ModelProfile::based_on_generic("qwen35");
    qwen35.chat_template = true;
    qwen35.stop = &["<|im_end|>"];
    qwen35.speculative = "mtp:3";

    let mut moe = qwen35;
    moe.label = "qwen35moe";

    vec![
        ProfileEntry {
            arch: "qwen35",
            name_needle: "",
            profile: qwen35,
        },
        ProfileEntry {
            arch: "qwen35moe",
            name_needle: "",
            profile: moe,
        },
    ]
}
