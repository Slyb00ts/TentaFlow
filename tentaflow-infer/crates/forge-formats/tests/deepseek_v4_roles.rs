// ===== File: deepseek_v4_roles.rs — mapa ról DeepSeek V4 wobec prawdziwego checkpointu =====
//
// Test sprawdza jedno, ale za to bezlitośnie: czy opis architektury pokrywa
// KAŻDY tensor checkpointu i czy każda nazwa, po którą sięgnie loader, w tym
// checkpoincie istnieje. Literówka w szablonie albo przeoczona rola to w tej
// architekturze najłatwiejszy możliwy błąd — 135 tysięcy tensorów i
// dwadzieścia kilka nowych ról nie da się sprawdzić wzrokiem.
//
// Wymaga rozpakowanego checkpointu; bez niego test jest pomijany z komunikatem,
// bo 157 GB nie wjeżdża do repozytorium.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use forge_formats::{HfConfig, ModelDescriptor, WeightRole};

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

/// Nazwy tensorów z indeksu safetensors, bez sufiksów skal — te są własnością
/// warstwy kwantyzacji, nie mapy ról.
fn checkpoint_tensors(dir: &PathBuf) -> HashSet<String> {
    let text = std::fs::read_to_string(dir.join("model.safetensors.index.json")).unwrap();
    let index: serde_json::Value = serde_json::from_str(&text).unwrap();
    index["weight_map"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// Sufiksy towarzyszące wadze kwantyzowanej; opis modelu wskazuje samą wagę.
const QUANT_SUFFIXES: &[&str] = &[
    ".scale",
    ".weight_scale",
    ".weight_scale_2",
    ".input_scale",
];

fn is_quant_sidecar(name: &str) -> bool {
    QUANT_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

fn descriptor(dir: &PathBuf) -> ModelDescriptor {
    let config = HfConfig::load(dir.join("config.json")).expect("config.json");
    let mut desc = ModelDescriptor::from_hf(&config).expect("descriptor DeepSeek V4");
    // Ścieżka HF deklaruje wszystkie role każdej warstwy; kompresor, indekser i
    // tablica routingu istnieją tylko w części z nich.
    let present = checkpoint_tensors(dir);
    desc.prune_absent_optional(|name| present.contains(name));
    desc
}

/// Rozwija nazwy wszystkich ról opisu na konkretne tensory checkpointu.
fn expected_names(desc: &ModelDescriptor, n_experts: usize) -> HashMap<String, WeightRole> {
    let mut out = HashMap::new();
    for (role, name) in &desc.globals {
        out.insert(name.clone(), *role);
    }
    for layer in &desc.layers {
        for (role, template) in layer {
            if template.contains("{expert}") {
                for expert in 0..n_experts {
                    out.insert(
                        template.replace("{expert}", &expert.to_string()),
                        *role,
                    );
                }
            } else {
                out.insert(template.clone(), *role);
            }
        }
    }
    out
}

#[test]
fn every_role_resolves_to_a_real_tensor() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let desc = descriptor(&dir);
    let moe = desc.params.moe.as_ref().expect("DeepSeek V4 jest modelem MoE");
    let present = checkpoint_tensors(&dir);
    let expected = expected_names(&desc, moe.n_experts);

    let missing: Vec<&String> = expected
        .keys()
        .filter(|name| !present.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "opis wskazuje {} tensorów, których nie ma w checkpoincie, np. {:?}",
        missing.len(),
        &missing[..missing.len().min(10)]
    );
}

#[test]
fn every_checkpoint_tensor_has_a_role() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let desc = descriptor(&dir);
    let moe = desc.params.moe.as_ref().expect("DeepSeek V4 jest modelem MoE");
    let expected = expected_names(&desc, moe.n_experts);
    let present = checkpoint_tensors(&dir);

    // Głowa MTP jest osobnym blokiem spekulacyjnym i nie należy do pnia — jej
    // tensory zostaną zmapowane razem z obsługą MTP, nie tutaj.
    let unclaimed: Vec<&String> = present
        .iter()
        .filter(|name| !name.starts_with("mtp."))
        .filter(|name| !is_quant_sidecar(name))
        .filter(|name| !expected.contains_key(*name))
        .collect();
    assert!(
        unclaimed.is_empty(),
        "{} tensorów pnia nie ma przypisanej roli, np. {:?}",
        unclaimed.len(),
        &unclaimed[..unclaimed.len().min(20)]
    );
}

/// Kompresor i indekser są opcjonalne i ich rozkład po warstwach wynika z
/// `compress_ratios` — ten test pilnuje, że opis zgadza się z konfiguracją, a
/// nie tylko z tym, co akurat leży na dysku.
#[test]
fn compressor_and_indexer_follow_compress_ratios() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let config = HfConfig::load(dir.join("config.json")).expect("config.json");
    let present = checkpoint_tensors(&dir);
    let ratios = &config.compress_ratios;
    assert!(
        ratios.len() >= config.num_hidden_layers,
        "compress_ratios ({}) nie pokrywa {} warstw",
        ratios.len(),
        config.num_hidden_layers
    );

    for layer in 0..config.num_hidden_layers {
        let ratio = ratios[layer];
        let has_compressor = present.contains(&format!("layers.{layer}.attn.compressor.wkv.weight"));
        assert_eq!(
            has_compressor,
            ratio != 0,
            "warstwa {layer}: compress_ratio={ratio}, a kompresor {}",
            if has_compressor { "jest" } else { "go nie ma" }
        );
        let has_indexer =
            present.contains(&format!("layers.{layer}.attn.indexer.weights_proj.weight"));
        assert_eq!(
            has_indexer,
            ratio == 4,
            "warstwa {layer}: compress_ratio={ratio}, a indekser {}",
            if has_indexer { "jest" } else { "go nie ma" }
        );
    }
}
