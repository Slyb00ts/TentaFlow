// ===== File: model_profile/qwen3.rs — Qwen3 (gęsty i MoE) =====
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut qwen = ModelProfile::based_on_generic("qwen3");
    qwen.chat_template = true;
    qwen.stop = &["<|im_end|>"];

    let mut moe = qwen;
    moe.label = "qwen3moe";

    vec![
        ProfileEntry { arch: "qwen3", name_needle: "", profile: qwen },
        ProfileEntry { arch: "qwen3moe", name_needle: "", profile: moe },
    ]
}
