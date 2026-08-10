// ===== File: model_profile/llama.rs — rodzina Llama i Mistral =====
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut mistral = ModelProfile::based_on_generic("mistral");
    mistral.chat_template = true;

    let mut llama = ModelProfile::based_on_generic("llama");
    llama.chat_template = true;

    vec![
        ProfileEntry {
            arch: "llama",
            name_needle: "mistral",
            profile: mistral,
        },
        ProfileEntry {
            arch: "llama",
            name_needle: "",
            profile: llama,
        },
    ]
}
