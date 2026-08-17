// ===== File: deepseek_v4_layer_load.rs — wczytanie warstwy DeepSeeka V4 na GPU =====
//
// Sprawdza to, czego nie sprawdzi żaden test na kształtach z pliku: że wagi
// PRZECHODZĄ przez produkcyjne konwersje kwantyzacji i lądują na urządzeniu z
// właściwą geometrią, a obecność kompresora i indeksera zgadza się ze stopniem
// kompresji warstwy.
//
// Wymaga rozpakowanego checkpointu; bez niego test jest pomijany, bo 157 GB nie
// wjeżdża do repozytorium.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::weights::{load_deepseek_layer_for_test, DevWeight};
use forge_formats::HfConfig;
use forge_hal::cuda::PoolSizes;
use forge_hal::Device;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json")
        .is_file()
        .then_some(dir)
}

fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 3 << 30,
            kv_cache: 16 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .expect("GPU wymagane")
}

/// Warstwa 2 ma stopień kompresji 4, więc ma i kompresor, i indekser — to
/// najbogatszy wariant tej architektury.
#[test]
fn loads_layer_with_compressor_and_indexer() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let config = HfConfig::load(dir.join("config.json")).unwrap();
    let dev = device();
    let attn = load_deepseek_layer_for_test(dev.as_ref(), &dir, 2).expect("wczytanie warstwy 2");

    let hidden = config.hidden_size;
    let q_rank = config.q_lora_rank.unwrap();
    let head_dim = config.head_dim.unwrap();
    let n_heads = config.num_attention_heads;
    let o_groups = config.o_groups.unwrap();
    let o_rank = config.o_lora_rank.unwrap();

    assert_eq!((attn.wq_a.rows(), attn.wq_a.cols()), (q_rank, hidden));
    assert_eq!(
        (attn.wq_b.rows(), attn.wq_b.cols()),
        (n_heads * head_dim, q_rank)
    );
    // KV to POJEDYNCZA głowica — nie GQA.
    assert_eq!((attn.wkv.rows(), attn.wkv.cols()), (head_dim, hidden));
    assert_eq!(
        (attn.wo_a.rows(), attn.wo_a.cols()),
        (o_groups * o_rank, n_heads * head_dim / o_groups)
    );
    assert_eq!(
        (attn.wo_b.rows(), attn.wo_b.cols()),
        (hidden, o_groups * o_rank)
    );

    // Wagi FP8 muszą wyjść ze ścieżki ze skalą wierszową, a nie jako f16 —
    // inaczej model urósłby o 5,5 GiB.
    for (name, weight) in [
        ("wq_a", &attn.wq_a),
        ("wq_b", &attn.wq_b),
        ("wkv", &attn.wkv),
        ("wo_b", &attn.wo_b),
    ] {
        assert!(
            matches!(weight, DevWeight::Fp8Row { .. }),
            "{name} nie przeszło konwersji na skalę wierszową"
        );
    }

    let compressor = attn.compressor.as_ref().expect("warstwa 2 ma kompresor");
    // Stopień 4 oznacza okna z zakładką, więc projekcje są dwa razy szersze.
    assert_eq!(compressor.wkv.rows(), 2 * head_dim);
    assert_eq!(compressor.wgate.rows(), 2 * head_dim);
    assert_eq!(compressor.ape.len(), 4 * 2 * head_dim * 4);

    let indexer = attn.indexer.as_ref().expect("warstwa 2 ma indekser");
    let index_dim = config.index_head_dim.unwrap();
    let index_heads = config.index_n_heads.unwrap();
    assert_eq!(
        (indexer.wq_b.rows(), indexer.wq_b.cols()),
        (index_heads * index_dim, q_rank)
    );
    assert_eq!(indexer.weights_proj.rows(), index_heads);
    assert_eq!(indexer.compressor.wkv.rows(), 2 * index_dim);
}

/// Warstwa 3 ma stopień 128: kompresor bez zakładki i BEZ indeksera.
#[test]
fn layer_without_indexer_loads_only_its_compressor() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let config = HfConfig::load(dir.join("config.json")).unwrap();
    let dev = device();
    let attn = load_deepseek_layer_for_test(dev.as_ref(), &dir, 3).expect("wczytanie warstwy 3");
    let head_dim = config.head_dim.unwrap();

    let compressor = attn.compressor.as_ref().expect("warstwa 3 ma kompresor");
    // Brak zakładki: projekcja ma szerokość jednej głowicy.
    assert_eq!(compressor.wkv.rows(), head_dim);
    assert!(
        attn.indexer.is_none(),
        "warstwa o stopniu 128 nie powinna mieć indeksera"
    );
}

/// Warstwy 0 i 1 mają stopień 0 — sama uwaga okienna, bez kompresji.
#[test]
fn layer_without_compression_has_neither() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let dev = device();
    let attn = load_deepseek_layer_for_test(dev.as_ref(), &dir, 0).expect("wczytanie warstwy 0");
    assert!(attn.compressor.is_none());
    assert!(attn.indexer.is_none());
}
