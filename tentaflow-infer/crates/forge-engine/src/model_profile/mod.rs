// ===== File: model_profile/mod.rs — domyślne ustawienia per model =====
//
// Cel: `forge run <model> "<prompt>"` ma dawać SENSOWNY wynik bez flag. Bez tego
// domyślna temperatura 0,7 i wyłączony szablon czatu potrafią zamienić poprawny
// model w bełkot — i tak właśnie zmyliły pomiar tego modelu 2026-07-28.
//
// Każda rodzina modeli ma swój plik obok tego. Trzyma wyłącznie USTAWIENIA
// URUCHOMIENIOWE (sampling, szablon czatu, kontekst); mapa tensorów i kernele
// zostają tam, gdzie były — w `forge-formats/arch/*.ron` i w katalogu kerneli.
//
// Rozstrzyganie: najpierw dopasowanie po nazwie modelu (najbardziej
// szczegółowe), potem po architekturze GGUF, na końcu ostrożny profil ogólny.

mod bielik;
mod deepseek_v4;
mod gemma4;
mod llama;
mod olmoe;
mod qwen3;
mod qwen35;
mod muse_glimmer;

/// Czym model się przedstawia. Pochodzi z metadanych GGUF, a nie z nazwy pliku —
/// plik bywa przemianowany, metadane nie.
#[derive(Clone, Debug, Default)]
pub struct ModelIdentity {
    /// `general.architecture`.
    pub arch: String,
    /// `general.basename` albo `general.name`, małymi literami.
    pub name: String,
}

/// Ustawienia uruchomieniowe jednego modelu. Wszystko, co użytkownik mógłby
/// musieć podać z palca, żeby model w ogóle odpowiadał sensownie.
#[derive(Clone, Copy, Debug)]
pub struct ModelProfile {
    pub label: &'static str,

    // --- sampling ---
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: usize,
    pub min_p: f32,
    pub repetition_penalty: f32,

    // --- format promptu ---
    /// Czy owijać prompt w szablon czatu modelu. Modele instrukcyjne bez tego
    /// dostają surowy tekst i odpowiadają dopełnieniem, a nie odpowiedzią.
    pub chat_template: bool,
    /// Nadpisanie `tokenizer.ggml.add_bos_token`, gdy metadane modelu kłamią.
    /// `None` znaczy: wierzymy metadanym.
    pub add_bos: Option<bool>,
    /// Dodatkowe łańcuchy kończące generację, poza tokenami EOS.
    pub stop: &'static [&'static str],

    // --- wykonanie ---
    /// Kontekst, jeśli pełne okno modelu jest niepraktycznie duże (Gemma 4 ma
    /// tak duże, że pule startowe żądają ponad 100 GB VRAM).
    pub default_ctx: Option<usize>,
    /// Tryb cache'u KV: `f16` | `fp8` | `rot4` | `rot3`.
    pub kv_cache: &'static str,
    /// Dekodowanie spekulatywne: `off` | `ngram[:k]` | `mtp[:2|3]`.
    pub speculative: &'static str,
}

impl ModelProfile {
    /// Profil dla modelu, którego nie znamy: zachowawczo, ale z szablonem czatu,
    /// bo zdecydowana większość dzisiejszych modeli GGUF to modele instrukcyjne.
    pub const fn generic() -> Self {
        Self {
            label: "generic",
            temperature: 0.0,
            top_p: 1.0,
            top_k: 0,
            min_p: 0.0,
            repetition_penalty: 1.0,
            chat_template: true,
            add_bos: None,
            stop: &[],
            default_ctx: None,
            kv_cache: "f16",
            speculative: "off",
        }
    }

    /// Baza do budowania profili: pola, których model nie zmienia, zostają jak
    /// w `generic`, więc plik modelu wymienia TYLKO to, co jest u niego inne.
    pub const fn based_on_generic(label: &'static str) -> Self {
        let mut profile = Self::generic();
        profile.label = label;
        profile
    }
}

/// Wpis rejestru. `name_needle` puste oznacza dopasowanie po samej architekturze.
pub struct ProfileEntry {
    pub arch: &'static str,
    pub name_needle: &'static str,
    pub profile: ModelProfile,
}

fn registry() -> Vec<ProfileEntry> {
    let mut all = Vec::new();
    all.extend(bielik::entries());
    all.extend(gemma4::entries());
    all.extend(qwen3::entries());
    all.extend(qwen35::entries());
    all.extend(olmoe::entries());
    all.extend(deepseek_v4::entries());
    all.extend(muse_glimmer::entries());
    // Llama na końcu: jest najszerszy i nie może przykryć profili szczegółowych
    // dzielących z nim `gguf_arch` (Bielik, Mistral).
    all.extend(llama::entries());
    all
}

/// Profil dla podanej tożsamości. Wpis z dopasowaniem po nazwie wygrywa z
/// wpisem samej architektury, niezależnie od kolejności w rejestrze.
pub fn resolve(identity: &ModelIdentity) -> ModelProfile {
    let arch = identity.arch.to_ascii_lowercase();
    let name = identity.name.to_ascii_lowercase();
    let entries = registry();
    if let Some(hit) = entries
        .iter()
        .filter(|e| !e.name_needle.is_empty() && name.contains(e.name_needle))
        .find(|e| e.arch.is_empty() || e.arch == arch)
    {
        return hit.profile;
    }
    entries
        .iter()
        .find(|e| e.name_needle.is_empty() && e.arch == arch)
        .map(|e| e.profile)
        .unwrap_or_else(ModelProfile::generic)
}

/// Odczytuje tożsamość modelu z metadanych GGUF. Katalog safetensors nie nosi
/// tych pól, więc opada na nazwę katalogu — jedyne, czym się tam przedstawia.
pub fn identify(path: &std::path::Path) -> ModelIdentity {
    if path.is_dir() {
        return ModelIdentity {
            arch: String::new(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
    }
    let Ok(gguf) = forge_formats::Gguf::open(path) else {
        return ModelIdentity::default();
    };
    let name = gguf
        .get_str("general.basename")
        .or_else(|| gguf.get_str("general.name"))
        .unwrap_or_default();
    ModelIdentity {
        arch: gguf
            .get_str("general.architecture")
            .unwrap_or_default()
            .to_string(),
        name: name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(arch: &str, name: &str) -> ModelIdentity {
        ModelIdentity {
            arch: arch.into(),
            name: name.into(),
        }
    }

    #[test]
    fn nazwa_wygrywa_z_sama_architektura() {
        // Bielik i Mistral mają tę samą architekturę GGUF `llama`, a różne
        // profile — dopasowanie po nazwie musi je rozdzielić.
        let bielik = resolve(&id("llama", "minitron-Bielik-7B-v3.0"));
        let mistral = resolve(&id("llama", "Mistral-7B-Instruct-v0.3"));
        assert_eq!(bielik.label, "bielik");
        assert_eq!(mistral.label, "mistral");
    }

    #[test]
    fn sama_architektura_wystarczy_gdy_nazwa_nieznana() {
        assert_eq!(resolve(&id("llama", "cos-zupelnie-innego")).label, "llama");
        assert_eq!(resolve(&id("gemma4", "")).label, "gemma4");
    }

    #[test]
    fn nieznana_architektura_dostaje_profil_ogolny() {
        let p = resolve(&id("wynalazek-2030", "x"));
        assert_eq!(p.label, "generic");
        // Zachowawczo: greedy, żeby nieznany model nie wyglądał na zepsuty przez
        // losowanie.
        assert_eq!(p.temperature, 0.0);
    }

    #[test]
    fn wielkosc_liter_nie_ma_znaczenia() {
        assert_eq!(resolve(&id("LLaMa", "MINITRON-BIELIK-7B")).label, "bielik");
    }

    #[test]
    fn gemma_ogranicza_kontekst() {
        // Pełne okno Gemmy 4 każe pulom zażądać ponad 100 GB VRAM.
        assert!(resolve(&id("gemma4", "gemma-4-12B-it"))
            .default_ctx
            .is_some());
    }

    #[test]
    fn kazdy_wpis_ma_sensowny_sampling() {
        for entry in registry() {
            let p = entry.profile;
            assert!(p.temperature >= 0.0, "{}", p.label);
            assert!(p.top_p > 0.0 && p.top_p <= 1.0, "{}", p.label);
            assert!(p.repetition_penalty > 0.0, "{}", p.label);
            assert!(
                matches!(p.kv_cache, "f16" | "fp8" | "rot4" | "rot3"),
                "{} ma nieznany tryb kv: {}",
                p.label,
                p.kv_cache
            );
            assert!(!p.label.is_empty());
        }
    }

    #[test]
    fn muse_glimmer_uses_model_card_defaults() {
        let muse = resolve(&id("muse-glimmer", "Muse Glimmer Hf"));
        assert_eq!(muse.label, "muse_glimmer");
        assert_eq!(muse.temperature, 1.0);
        assert_eq!(muse.top_p, 0.95);
        assert_eq!(muse.top_k, 64);
        assert!(muse.chat_template);
    }
}
