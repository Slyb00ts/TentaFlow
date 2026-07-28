// ===== File: model_profile/deepseek_v4.rs — DeepSeek V4 =====
//
// Model nie mieści się w VRAM pojedynczej karty i jest stronicowany z NVMe,
// więc kontekst startowy trzymamy krótki — inaczej sama pula KV zjada budżet.
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut deepseek = ModelProfile::based_on_generic("deepseek_v4");
    deepseek.chat_template = true;
    deepseek.default_ctx = Some(4096);

    vec![ProfileEntry { arch: "deepseek_v4", name_needle: "", profile: deepseek }]
}
