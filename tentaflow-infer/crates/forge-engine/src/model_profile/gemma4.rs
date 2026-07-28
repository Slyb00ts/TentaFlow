// ===== File: model_profile/gemma4.rs — Gemma 4 =====
//
// Okno kontekstu Gemmy 4 jest tak duże, że pule startowe liczone z pełnego
// kontekstu żądają ponad 100 GB VRAM i uruchomienie kończy się błędem pamięci.
// Domyślny kontekst 4096 sprawia, że model po prostu startuje.
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut gemma = ModelProfile::based_on_generic("gemma4");
    gemma.chat_template = true;
    gemma.default_ctx = Some(4096);
    gemma.stop = &["<end_of_turn>"];

    vec![ProfileEntry { arch: "gemma4", name_needle: "", profile: gemma }]
}
