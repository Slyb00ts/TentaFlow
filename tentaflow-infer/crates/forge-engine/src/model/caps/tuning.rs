// ===== File: model/tuning.rs — progi strojenia rozstrzygane kaskadą =====
use super::super::*;

fn is_bielik_q4k_identity(arch: &str, name: &str, tensor_parallel: bool, moe: bool) -> bool {
    arch == "llama" && name.to_ascii_lowercase().contains("bielik") && !tensor_parallel && !moe
}

/// Pokrętło, którego wartość zależy JEDNOCZEŚNIE od modelu i od karty.
///
/// Nie każda stała tutaj należy. Kształt kernela (liczba warpów, szerokość
/// kafla) jest własnością artefaktu — jest tyle wartości, ile zbudowanych
/// wariantów, więc wybiera go obecność artefaktu, nie tablica. Tutaj trafiają
/// wyłącznie PUNKTY PRZECIĘCIA dwóch implementacji, bo tam jedna krzywa kosztu
/// zależy od formatu wag, a druga od pasma i narzutu uruchomienia karty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Knob {
    /// Ile sekwencji musi dekodować naraz, zanim wspólny forward się opłaci.
    BatchMin,
    /// Najszersza grupa, jaką scheduler składa w jeden krok hybrydy.
    MaxDecodeGroup,
}

/// Klasa kształtu modelu — NIE nazwa checkpointu. Dwa różne checkpointy o tym
/// samym kształcie i kwantyzacji mają to samo optimum, a ten sam model w innej
/// kwantyzacji ma inne.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ModelClass {
    /// Hybryda z własnym wsadowym forwardem.
    HybridBatch,
    /// MoE z grupowaną dyspozycją ekspertów.
    MoeGrouped,
    /// Format z dokładnym kernelem małego batcha (NVFP4, K-kwanty, Q8_0).
    SmallBatch,
    /// Reszta: batch płaci pełny kafel tokenów niezależnie od szerokości.
    TokenTile,
}

/// Do jak wąskiej klasy sprzętu stosuje się wpis. Im węższa, tym wyższy
/// priorytet — `Card` bije `Arch`, `Arch` bije `Vendor`, `Vendor` bije `Any`.
#[derive(Clone, Copy)]
pub(crate) enum DeviceScope {
    Any,
    Vendor(forge_types::Vendor),
    Arch(&'static str),
    /// Fragment nazwy karty, np. "GB10" albo "4090".
    Card(&'static str),
}

impl DeviceScope {
    fn matches(&self, caps: &forge_types::DeviceCaps) -> bool {
        self.matches_parts(&caps.name, &caps.arch, caps.vendor)
    }

    fn matches_parts(&self, name: &str, arch: &str, vendor: forge_types::Vendor) -> bool {
        match self {
            DeviceScope::Any => true,
            DeviceScope::Vendor(want) => vendor == *want,
            DeviceScope::Arch(want) => arch == *want,
            DeviceScope::Card(fragment) => name.contains(fragment),
        }
    }

    fn specificity(&self) -> u8 {
        match self {
            DeviceScope::Any => 0,
            DeviceScope::Vendor(_) => 1,
            DeviceScope::Arch(_) => 2,
            DeviceScope::Card(_) => 3,
        }
    }
}

pub(crate) struct Rule {
    pub knob: Knob,
    pub model: Option<ModelClass>,
    pub device: DeviceScope,
    pub value: usize,
    /// Karta, na której tę liczbę ZMIERZONO. Wpis bez pomiaru nie należy tutaj.
    pub measured_on: &'static str,
}

/// Kolejność w tablicy nie ma znaczenia — rozstrzyga specyficzność.
///
/// Wpis dodaje się dopiero wtedy, gdy pomiar na danej karcie RÓŻNI SIĘ od tego,
/// co daje poziom wyżej. Wpisanie tej samej liczby ponownie tylko udaje wiedzę.
const RULES: &[Rule] = &[
    // Kernele małego batcha wygrywają od dwóch sekwencji: TPOT 11-14 ms wobec
    // 28-58 ms po kolei (serve p1024/o128, izolowane A/B 2026-07-24).
    Rule {
        knob: Knob::BatchMin,
        model: Some(ModelClass::SmallBatch),
        device: DeviceScope::Any,
        value: 2,
        measured_on: "RTX 4090",
    },
    Rule {
        knob: Knob::BatchMin,
        model: Some(ModelClass::HybridBatch),
        device: DeviceScope::Any,
        value: 2,
        measured_on: "GB10",
    },
    // Ten próg stał na 4, bo grupowany MoE płacił sortowanie po ekspertach i
    // odczyt routera na każdą warstwę — dwie linie tego nie pokrywały (73 tok/s
    // wsadowo wobec 87 po kolei). Dyspozycja adresowana na urządzeniu nie ma
    // ani sortowania, ani tego odczytu, więc dwie linie już się opłacają:
    // 109,8 wobec 85,8 po kolei. Qwen3-30B-A3B Q4_K_M, prompt 512.
    Rule {
        knob: Knob::BatchMin,
        model: Some(ModelClass::MoeGrouped),
        device: DeviceScope::Any,
        value: 2,
        measured_on: "GB10",
    },
    // Kafel tokenów jest płaski, więc amortyzuje się dopiero przy kilkunastu
    // sekwencjach (Mistral Q4_K C=4: 46 ms wsadowo wobec 26 ms po kolei).
    Rule {
        knob: Knob::BatchMin,
        model: Some(ModelClass::TokenTile),
        device: DeviceScope::Any,
        value: 12,
        measured_on: "RTX 4090",
    },
    // Każda szerokość 2..=16 trafia w strojony kernel NVFP4 GGUF; powyżej
    // dispatch przechodzi na kafel MMA bm32, którego ta ścieżka nie ma
    // zmierzonego. C=6 69,1 · C=10 67,8 · C=12 68,9 · C=16 71,3 tok/s.
    Rule {
        knob: Knob::MaxDecodeGroup,
        model: None,
        device: DeviceScope::Any,
        value: 16,
        measured_on: "RTX 4090",
    },
];

impl Model {
    pub fn set_gguf_identity(&mut self, name: &str) {
        self.q4k_decode_model = if is_bielik_q4k_identity(
            &self.weights.descriptor.arch,
            name,
            false,
            false,
        ) {
            forge_kernels::Q4kDecodeModelFamily::Bielik
        } else {
            forge_kernels::Q4kDecodeModelFamily::Dense
        };
    }

    pub(crate) fn q4k_decode_model_family(&self) -> forge_kernels::Q4kDecodeModelFamily {
        let tensor_parallel = self.tp.is_some() || self.tp_ffn.is_some();
        let moe = self
            .weights
            .layers
            .iter()
            .any(|layer| matches!(layer.ffn, crate::weights::LayerFfn::Moe(_)));
        if tensor_parallel || moe {
            return forge_kernels::Q4kDecodeModelFamily::Dense;
        }
        if self.weights.descriptor.arch == "qwen35" {
            return forge_kernels::Q4kDecodeModelFamily::Qwen35;
        }
        if matches!(self.q4k_decode_model, forge_kernels::Q4kDecodeModelFamily::Bielik) {
            return forge_kernels::Q4kDecodeModelFamily::Bielik;
        }
        match self.model_class() {
            ModelClass::HybridBatch => forge_kernels::Q4kDecodeModelFamily::Any,
            ModelClass::MoeGrouped | ModelClass::SmallBatch | ModelClass::TokenTile => {
                forge_kernels::Q4kDecodeModelFamily::Dense
            }
        }
    }

    fn model_class(&self) -> ModelClass {
        if self.hybrid_batch_capable() {
            return ModelClass::HybridBatch;
        }
        if self.moe_batch_capable() {
            return ModelClass::MoeGrouped;
        }
        if self.weights.small_batch_decode_capable() {
            return ModelClass::SmallBatch;
        }
        ModelClass::TokenTile
    }

    /// Wartość pokrętła dla TEGO modelu na TEJ karcie.
    ///
    /// Wygrywa wpis o najwęższym zasięgu sprzętowym; przy równym zasięgu wpis
    /// dopasowany do klasy modelu bije wpis ogólny. Nadpisanie operatora
    /// (`FORGE_BATCH_MIN`) obsługuje wołający — ono stoi ponad całą kaskadą.
    pub(crate) fn tuned(&self, knob: Knob) -> Option<usize> {
        let caps = self.device.caps();
        let class = self.model_class();
        RULES
            .iter()
            .filter(|rule| rule.knob == knob)
            .filter(|rule| rule.model.is_none_or(|scope| scope == class))
            .filter(|rule| rule.device.matches(caps))
            .max_by_key(|rule| (rule.device.specificity(), u8::from(rule.model.is_some())))
            .map(|rule| rule.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB10: (&str, &str, forge_types::Vendor) =
        ("NVIDIA GB10", "sm_121a", forge_types::Vendor::Nvidia);

    fn hits(scope: DeviceScope) -> bool {
        scope.matches_parts(GB10.0, GB10.1, GB10.2)
    }

    #[test]
    fn wezszy_zasieg_bije_szerszy() {
        assert!(hits(DeviceScope::Card("GB10")));
        assert!(hits(DeviceScope::Arch("sm_121a")));
        assert!(!hits(DeviceScope::Arch("sm_89")));
        assert!(hits(DeviceScope::Vendor(forge_types::Vendor::Nvidia)));
        assert!(!hits(DeviceScope::Vendor(forge_types::Vendor::Amd)));
        let order = [
            DeviceScope::Any.specificity(),
            DeviceScope::Vendor(forge_types::Vendor::Nvidia).specificity(),
            DeviceScope::Arch("sm_121a").specificity(),
            DeviceScope::Card("GB10").specificity(),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "kolejność: {order:?}"
        );
    }

    #[test]
    fn kazde_pokretlo_ma_wartosc_dla_kazdej_klasy_modelu() {
        for class in [
            ModelClass::HybridBatch,
            ModelClass::MoeGrouped,
            ModelClass::SmallBatch,
            ModelClass::TokenTile,
        ] {
            let hit = RULES
                .iter()
                .any(|r| r.knob == Knob::BatchMin && r.model == Some(class));
            assert!(hit, "{class:?} nie ma progu batcha");
        }
        assert!(RULES.iter().any(|r| r.knob == Knob::MaxDecodeGroup));
    }

    #[test]
    fn bielik_identity_accepts_only_the_exact_dense_single_gpu_route() {
        assert!(is_bielik_q4k_identity("llama", "Bielik-11B-v3.0", false, false));
    }

    #[test]
    fn bielik_identity_rejects_other_llama_and_parallel_or_moe_routes() {
        assert!(!is_bielik_q4k_identity("llama", "Mistral-11B", false, false));
        assert!(!is_bielik_q4k_identity("llama", "Bielik-11B", true, false));
        assert!(!is_bielik_q4k_identity("llama", "Bielik-11B", false, true));
        assert!(!is_bielik_q4k_identity("qwen35", "Bielik-11B", false, false));
    }

}
