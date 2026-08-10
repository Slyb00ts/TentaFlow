// ===== File: model_profile/bielik.rs — Bielik (SpeakLeash) =====
//
// Bielik dzieli `gguf_arch = "llama"` z Mistralem, ale ma własny szablon czatu
// (ChatML) i deklaruje `add_bos_token = false`. Pomiar z 2026-07-28 pokazał, że
// bez BOS pierwszy token jest inny niż w llama.cpp, dlatego BOS wymuszamy.
//
// UWAGA: ten model ma otwarty błąd poprawności w FORGE (bełkot przy greedy,
// podczas gdy llama.cpp z tego samego pliku odpowiada poprawnie). Profil ustawia
// mu warunki startowe, ale sam błędu NIE naprawia.
use super::{ModelProfile, ProfileEntry};

pub fn entries() -> Vec<ProfileEntry> {
    let mut bielik = ModelProfile::based_on_generic("bielik");
    bielik.chat_template = true;
    bielik.add_bos = Some(true);
    bielik.stop = &["<|im_end|>"];

    vec![ProfileEntry {
        arch: "llama",
        name_needle: "bielik",
        profile: bielik,
    }]
}
