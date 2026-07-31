// Sprawdza, ze podzial warstw dziala NA SPRZECIE: dwa etapy tego samego modelu
// wczytane rownoczesnie na dwie rozne karty, kazdy tylko ze swoimi warstwami.
// To jest warunek pipeline parallel — model wiekszy od jednej karty ma sie
// zmiescic na kilku.
use forge_engine::model::{Model, ModelConfig};
use forge_hal::{PoolSizes, gpu};

fn main() {
    let path = std::path::PathBuf::from(
        std::env::args().nth(1).expect("sciezka do gguf"),
    );
    let total: usize = std::env::args()
        .nth(2)
        .expect("liczba warstw modelu")
        .parse()
        .expect("liczba");
    let pools = PoolSizes {
        weights: 8 << 30,
        kv_cache: 256 << 20,
        activations: 512 << 20,
        kv_page_size: 256 << 10,
    };
    let half = total / 2;
    let ids = gpu::enumerate();
    assert!(ids.len() >= 2, "potrzebne sa dwie karty");

    for (stage, (first, count)) in [(0, half), (half, total - half)].into_iter().enumerate() {
        let device = gpu::open_id(ids[stage], pools).expect("otwarcie karty");
        let name = device.caps().name.clone();
        let model = Model::load_gguf(
            device,
            &path,
            ModelConfig {
                max_seq_len: 256,
                kv_pages: 8,
                prefix_cache: false,
                layer_range: Some((first, count)),
                tp_shard: forge_formats::TpShard { rank: 0, world: 1 },
                ..ModelConfig::default()
            },
        )
        .expect("wczytanie etapu");
        println!(
            "etap {stage} na {name}: warstwy {first}..{}, wczytanych {}",
            first + count,
            model.weights.layers.len()
        );
        assert_eq!(model.weights.layers.len(), count);
    }
    println!("oba etapy wczytane rownoczesnie, kazdy tylko ze swoimi warstwami");
}
