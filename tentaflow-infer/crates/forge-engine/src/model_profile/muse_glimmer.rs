// ===== File: muse_glimmer.rs — Muse Glimmer runtime defaults =====

use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut profile = ModelProfile::based_on_generic("muse_glimmer");
    profile.temperature = 1.0;
    profile.top_p = 0.95;
    profile.top_k = 64;
    profile.chat_template = true;
    profile.stop = &["<|im_end|>"];

    vec![ProfileEntry {
        arch: "muse-glimmer",
        name_needle: "",
        profile,
    }]
}
