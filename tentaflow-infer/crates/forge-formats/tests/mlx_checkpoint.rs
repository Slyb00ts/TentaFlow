// ===== File: mlx_checkpoint.rs — every tensor of a real MLX checkpoint resolves =====
//
// The gate from PLAN_NAPRAWY §6.2: a model is added by an entry in the registry,
// and the entry is correct only when it resolves the file WITHOUT REMAINDER in
// both directions — no tensor the descriptor fails to claim, no role the file
// fails to provide. A partially resolved map does not fail loudly; it produces a
// model that loads and computes something else.
//
// Skips cleanly when the checkpoint is absent, like the other real-model tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use forge_formats::{
    map_checkpoint, split_component, HfConfig, MlxComponent, MlxMode, MlxQuantConfig,
    ModelDescriptor, SafeTensors,
};
use forge_types::DType;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below the workspace root")
        .parent()
        .expect("workspace root has a parent")
        .to_path_buf()
}

fn checkpoint() -> Option<PathBuf> {
    let dir = repo_root().join(
        ".runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots",
    );
    let snapshot = std::fs::read_dir(&dir).ok()?.flatten().next()?.path();
    snapshot.join("config.json").is_file().then_some(snapshot)
}

#[test]
fn component_split_is_reversible_and_leaves_plain_names_alone() {
    let (canon, comp) = split_component("model.layers.3.mlp.gate_proj.scales");
    assert_eq!(canon, "model.layers.3.mlp.gate_proj.weight");
    assert_eq!(comp, MlxComponent::Scales);

    let (canon, comp) = split_component("model.layers.3.mlp.gate_proj.biases");
    assert_eq!(canon, "model.layers.3.mlp.gate_proj.weight");
    assert_eq!(comp, MlxComponent::Biases);

    // A norm is stored as a single tensor and must pass through untouched.
    let (canon, comp) = split_component("model.layers.3.input_layernorm.weight");
    assert_eq!(canon, "model.layers.3.input_layernorm.weight");
    assert_eq!(comp, MlxComponent::Weight);
}

#[test]
fn bielik_mlx_checkpoint_resolves_without_remainder() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu MLX w .runtime/models");
        return;
    };

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    let quant = MlxQuantConfig::from_config(&config)
        .expect("blok quantization")
        .expect("checkpoint MLX deklaruje kwantyzację");
    assert_eq!(quant.mode, MlxMode::Affine);
    assert_eq!(quant.group_size, 64);
    assert_eq!(quant.bits, 4);
    assert_eq!(quant.per_word(), 8);

    let hf: HfConfig =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    let desc = ModelDescriptor::from_hf(&hf).expect("opis architektury z config.json");
    assert_eq!(desc.arch, "llama");
    assert_eq!(desc.layers.len(), 40);

    let st = SafeTensors::open(dir.join("model.safetensors")).expect("nagłówek safetensors");
    let layout = map_checkpoint(&desc, st.names());

    assert!(
        layout.is_complete(),
        "checkpoint nie rozwiązuje się bez reszty: {} nieznanych, {} brakujących, \
         {} niepełnych trójek\n  nieznane: {:?}\n  brakujące: {:?}\n  niepełne: {:?}",
        layout.unknown.len(),
        layout.missing.len(),
        layout.partial.len(),
        layout.unknown.iter().take(5).collect::<Vec<_>>(),
        layout.missing.iter().take(5).collect::<Vec<_>>(),
        layout.partial.iter().take(5).collect::<Vec<_>>(),
    );

    // 40 warstw × 7 skwantyzowanych projekcji + embedding + głowa.
    assert_eq!(layout.quantized.len(), 40 * 7 + 2, "skwantyzowane wagi");
    // 40 warstw × 2 normy + norma końcowa.
    assert_eq!(layout.plain.len(), 40 * 2 + 1, "tensory nieskwantyzowane");
    assert_eq!(layout.tensor_count(), st.len(), "suma musi objąć cały plik");
    assert_eq!(st.len(), 927);
}

#[test]
fn a_missing_scales_tensor_is_reported_not_ignored() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu MLX w .runtime/models");
        return;
    };
    let hf: HfConfig =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    let desc = ModelDescriptor::from_hf(&hf).unwrap();
    let st = SafeTensors::open(dir.join("model.safetensors")).unwrap();

    // Drop one scales tensor: an incomplete triple must surface as `partial`,
    // because decoding it would use whatever the neighbouring memory holds.
    let dropped = "model.layers.0.mlp.gate_proj.scales";
    let names: Vec<&str> = st.names().filter(|n| *n != dropped).collect();
    assert_eq!(
        names.len(),
        st.len() - 1,
        "tensor do usunięcia musi istnieć"
    );

    let layout = map_checkpoint(&desc, names.into_iter());
    assert!(!layout.is_complete());
    assert_eq!(layout.partial, vec!["model.layers.0.mlp.gate_proj.weight"]);
    assert!(layout.unknown.is_empty());
    assert!(layout.missing.is_empty());
}

fn whisper_checkpoint() -> Option<PathBuf> {
    // Cache pobierany przez ścieżkę MLX w drzewie głównym.
    let dir = dirs_data_local()?
        .join("tentaflow/models/mlx-whisper/mlx-community_whisper-large-v3-turbo-4bit");
    dir.join("model.safetensors").is_file().then_some(dir)
}

fn dirs_data_local() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
}

#[test]
fn whisper_mlx_checkpoint_has_the_expected_shape() {
    let Some(dir) = whisper_checkpoint() else {
        eprintln!("pomijam: brak checkpointu MLX Whisper");
        return;
    };

    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    let quant = MlxQuantConfig::from_config(&cfg).unwrap().unwrap();
    // Ten checkpoint NIE deklaruje pola `mode` — wartość domyślna musi być affine,
    // bo tak zapisywały wszystkie konwertery sprzed wprowadzenia tego pola.
    assert!(cfg["quantization"].get("mode").is_none());
    assert_eq!(quant.mode, MlxMode::Affine);
    assert_eq!((quant.group_size, quant.bits), (64, 4));

    let st = SafeTensors::open(dir.join("model.safetensors")).unwrap();
    assert_eq!(st.len(), 1053);

    let mut components: BTreeMap<String, [bool; 3]> = BTreeMap::new();
    for name in st.names() {
        let (canon, comp) = split_component(name);
        components.entry(canon).or_insert([false; 3])[comp as usize] = true;
    }

    let triples = components
        .values()
        .filter(|c| c[0] && c[1] && c[2])
        .count();
    assert_eq!(triples, 233, "wagi skwantyzowane");

    // Pułapka nazewnicza z realnego pliku: `attn.out.bias` to wektor przesunięcia
    // warstwy liniowej, a `attn.out.biases` to zera kwantyzacji. Różnią się jedną
    // literą i muszą trafić do dwóch różnych miejsc.
    assert!(components.contains_key("encoder.blocks.0.attn.out.bias"));
    assert_eq!(
        components["encoder.blocks.0.attn.out.bias"],
        [true, false, false],
        "bias warstwy liniowej nie może zostać uznany za składnik trójki"
    );
    assert_eq!(
        components["encoder.blocks.0.attn.out.weight"],
        [true, true, true]
    );

    // Sploty wejściowe zostają nieskwantyzowane — kwantyzacja MLX obejmuje
    // wyłącznie warstwy liniowe.
    for conv in ["encoder.conv1.weight", "encoder.conv2.weight"] {
        assert_eq!(components[conv], [true, false, false], "{conv}");
        assert_eq!(st.tensor(conv).unwrap().dtype, DType::F16);
    }

    // Nazewnictwo jest OpenAI-owe, nie HF-owe: to jest powód, dla którego
    // `forge-whisper` (który oczekuje `model.encoder.layers.N.self_attn.q_proj`)
    // jeszcze tego pliku nie wczyta.
    assert!(st.tensor("encoder.blocks.0.attn.query.weight").is_some());
    assert!(st
        .tensor("model.encoder.layers.0.self_attn.q_proj.weight")
        .is_none());
}

#[test]
fn qwen3_vl_moe_is_recognised_as_mlx_but_not_yet_mappable() {
    // Test graniczny: zapisuje, dokąd sięga dziś pokrycie i dlaczego dalej nie.
    // Warstwa formatu MLX radzi sobie z tym checkpointem — zatrzymuje się rejestr
    // architektur, bo modele wielomodalne trzymają konfigurację wieży tekstowej
    // w zagnieżdżonym `text_config`. Gdy ktoś to doda, ten test zacznie failować
    // i wtedy należy go zastąpić pełnym sprawdzeniem rozwiązywania ról.
    let dir = repo_root()
        .join(".runtime/models/models--mlx-community--Qwen3-VL-30B-A3B-Instruct-4bit/snapshots");
    let Some(snap) = std::fs::read_dir(&dir)
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .map(|e| e.path())
    else {
        eprintln!("pomijam: brak checkpointu Qwen3-VL w .runtime/models");
        return;
    };
    let raw = std::fs::read_to_string(snap.join("config.json")).unwrap();
    let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

    // To działa: blok kwantyzacji jest ten sam co w modelu gęstym.
    let quant = MlxQuantConfig::from_config(&cfg).unwrap().unwrap();
    assert_eq!(quant.mode, MlxMode::Affine);
    assert_eq!((quant.group_size, quant.bits), (64, 4));

    // To jeszcze nie: wymiary siedzą w `text_config`, nie na najwyższym poziomie.
    assert!(cfg.get("hidden_size").is_none());
    assert!(cfg["text_config"]["hidden_size"].is_number());
    assert!(
        serde_json::from_str::<HfConfig>(&raw).is_err(),
        "HfConfig zaczął parsować konfigurację zagnieżdżoną — zastąp ten test \
         pełnym sprawdzeniem rozwiązywania ról"
    );
}
