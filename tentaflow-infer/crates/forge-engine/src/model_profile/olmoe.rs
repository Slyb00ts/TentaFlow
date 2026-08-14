// ===== File: model_profile/olmoe.rs — OLMoE =====
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut olmoe = ModelProfile::based_on_generic("olmoe");
    olmoe.chat_template = true;

    vec![ProfileEntry {
        arch: "olmoe",
        name_needle: "",
        profile: olmoe,
    }]
}
