// ===== File: tp_shard_load.rs — loader podziału tensor-parallel na realnym checkpoincie =====
// Plan cięcia jest zabramkowany testami jednostkowymi (`forge-formats`), ale one
// sprawdzają arytmetykę zakresów, a nie to, co loader FAKTYCZNIE wgrał na kartę.
// Ten test ładuje rangę modelu hybrydowego i porównuje kształty każdej dzielonej
// macierzy z kontraktem `Hyperparams::shard`.
//
// Dlaczego to jest osobna bramka: podział, który się nie dzieli, nie rzuca
// wyjątku — daje model liczący co innego. Jedyne miejsce, w którym da się to
// złapać tanio, to kształt tuż po załadowaniu.
//
// Model wskazuje `FORGE_TP_TEST_MODEL` (hybrydowy GGUF, np. Qwen3.6-27B).
// Bez tej zmiennej albo bez karty test pomija się czysto.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::weights::LayerMixer;
use forge_formats::TpShard;
use forge_hal::{gpu, Device, PoolSizes};

const WORLD: usize = 2;

fn load_rank(rank: usize) -> Option<Model> {
    let path = PathBuf::from(std::env::var("FORGE_TP_TEST_MODEL").ok()?);
    if !path.is_file() {
        eprintln!("pomijam: brak modelu {}", path.display());
        return None;
    }
    let device = match gpu::open(
        0,
        PoolSizes {
            weights: 24 << 30,
            kv_cache: 2 << 30,
            activations: 2 << 30,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("pomijam: brak karty: {e}");
            return None;
        }
    };
    let dev: Arc<dyn Device> = device;
    let cfg = ModelConfig {
        kv_pages: 64,
        max_seq_len: 512,
        tp_shard: TpShard::new(rank, WORLD).expect("ranga w zasięgu"),
        ..ModelConfig::default()
    };
    Some(Model::load_gguf(dev, &path, cfg).expect("ranga wczytana"))
}

#[test]
#[ignore = "wymaga karty i FORGE_TP_TEST_MODEL"]
fn ranga_wczytuje_swoj_fragment_kazdej_dzielonej_macierzy() {
    let Some(model) = load_rank(1) else { return };
    let p = &model.weights.descriptor.params;
    let ssm = p.ssm.as_ref().expect("model hybrydowy ma parametry SSM");

    // Deskryptor rangi: cała reszta silnika ma widzieć mniejszy model.
    assert_eq!(p.n_heads % p.n_kv_heads, 0, "grupa GQA rozjechała się");
    assert_eq!(
        ssm.n_v_heads() % ssm.n_k_heads(),
        0,
        "mapowanie GQA DeltaNet"
    );
    assert!(p.intermediate_size.is_multiple_of(256));

    let key_dim = ssm.key_dim();
    let value_dim = ssm.value_dim();
    let conv_dim = ssm.conv_dim();
    let mut delta_layers = 0usize;
    let mut attn_layers = 0usize;

    for (index, layer) in model.weights.layers.iter().enumerate() {
        match &layer.mixer {
            LayerMixer::DeltaNet(d) => {
                delta_layers += 1;
                // Wejściowa projekcja niesie q|k|v jedno za drugim — wiersze
                // rangi muszą sumować się DOKŁADNIE do jej lokalnego conv_dim,
                // inaczej mikser czytałby poza własnym fragmentem.
                assert_eq!(d.in_proj.rows(), conv_dim, "warstwa {index} in_proj");
                assert_eq!(d.gate_proj.rows(), value_dim, "warstwa {index} gate_proj");
                assert_eq!(
                    d.alpha_proj.rows(),
                    ssm.n_v_heads(),
                    "warstwa {index} alpha"
                );
                assert_eq!(d.beta_proj.rows(), ssm.n_v_heads(), "warstwa {index} beta");
                // Projekcja wyjściowa jest WIERSZOWO równoległa: dzieli się po
                // kolumnach, więc liczba wierszy zostaje pełna, a wejściem jest
                // wymiar głowic V tej rangi.
                assert_eq!(d.out_proj.rows(), p.hidden_size, "warstwa {index} ssm_out");
            }
            LayerMixer::Attention(a) => {
                attn_layers += 1;
                let forge_engine::weights::QkvWeights::Split { q, k, v } = &a.attn_qkv else {
                    panic!("warstwa {index}: hybryda ma rozdzielone q/k/v");
                };
                // Projekcja Q niesie na głowicę DWA bloki head_dim, bo druga
                // połowa to bramka wyjścia.
                let gated = if p.attn_gated { 2 } else { 1 };
                assert_eq!(
                    q.rows(),
                    p.n_heads * p.head_dim * gated,
                    "warstwa {index} q"
                );
                assert_eq!(k.rows(), p.n_kv_heads * p.head_dim, "warstwa {index} k");
                assert_eq!(v.rows(), p.n_kv_heads * p.head_dim, "warstwa {index} v");
                assert_eq!(a.attn_o.rows(), p.hidden_size, "warstwa {index} attn_o");
            }
            LayerMixer::DeepseekAttention(_) => panic!("warstwa {index}: nie ta architektura"),
        }
        let forge_engine::weights::LayerFfn::Dense(ffn) = &layer.ffn else {
            panic!("warstwa {index}: podział opisany dla gęstego FFN");
        };
        let forge_engine::weights::GateUpWeights::Split { gate, up } = &ffn.gate_up else {
            panic!("warstwa {index}: hybryda ma rozdzielone gate/up");
        };
        assert_eq!(gate.rows(), p.intermediate_size, "warstwa {index} ffn_gate");
        assert_eq!(up.rows(), p.intermediate_size, "warstwa {index} ffn_up");
        assert_eq!(ffn.down.rows(), p.hidden_size, "warstwa {index} ffn_down");
    }

    assert!(
        delta_layers > 0 && attn_layers > 0,
        "hybryda ma oba rodzaje warstw"
    );
    assert!(key_dim > 0 && conv_dim == 2 * key_dim + value_dim);
}
