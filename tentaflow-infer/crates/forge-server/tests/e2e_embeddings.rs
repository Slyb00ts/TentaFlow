// ===== File: e2e_embeddings.rs — GPU embedding sanity for an embedding GGUF =====
// Loads an embedding model (default: the jina-embeddings-v5 qwen3 GGUF),
// embeds a few texts, and asserts the vectors are finite, unit-norm, and
// semantically ordered (a topically similar pair scores higher by cosine than
// a dissimilar one, in Polish and English). Ignored by default (needs a CUDA
// device + the model); run with:
//   FORGE_EMBED_TEST_MODEL=/path/model.gguf \
//   cargo test -p forge-server --test e2e_embeddings -- --ignored --nocapture

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_formats::{Gguf, PoolingType};
use forge_hal::gpu;
use forge_hal::Device;
use forge_server::source::{
    load_tokenizer_gguf, read_descriptor, resolve_normalize, resolve_pooling,
};

fn model_path() -> PathBuf {
    std::env::var("FORGE_EMBED_TEST_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.push("../../../.runtime/models/model.gguf");
            p
        })
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

#[test]
#[ignore = "requires a CUDA device and an embedding GGUF model"]
fn embedding_sanity_cosine_ordering() {
    let path = model_path();
    assert!(path.is_file(), "model not found: {}", path.display());

    // These qwen3 embedding models are decoder-causal with last-token pooling,
    // so pooling operates on the final token's hidden state after it has
    // attended to the whole (causal) sequence — no bidirectional attention.
    let dev: Arc<dyn Device> = gpu::open_default_pools(0).expect("cuda device");

    let _desc = read_descriptor(&path).expect("descriptor");
    let gguf = Gguf::open(&path).expect("open gguf");
    let bundle = load_tokenizer_gguf(&gguf).expect("tokenizer");
    drop(gguf);
    let mut model = Model::load_gguf(dev, &path, ModelConfig::default()).expect("load model");

    let pooling = match resolve_pooling(&path, &model.weights.descriptor) {
        PoolingType::None => PoolingType::Mean,
        other => other,
    };
    let normalize = resolve_normalize(&path);
    let dim = model.weights.descriptor.params.hidden_size;
    eprintln!(
        "model: {} | arch={} | dim={dim} | pooling={pooling:?} | normalize={normalize} | decoder-causal-pooled",
        path.display(),
        model.weights.descriptor.arch,
    );

    let mut embed = |text: &str| -> Vec<f32> {
        let ids = bundle.tokenizer.encode(text, true).expect("encode");
        let v = model.embed(&ids, pooling, normalize).expect("embed");
        assert_eq!(v.len(), dim, "vector width");
        assert!(
            v.iter().all(|x| x.is_finite()),
            "non-finite component in '{text}'"
        );
        if normalize {
            let l2 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (l2 - 1.0).abs() < 1e-3,
                "L2 norm {l2} not ~1.0 for '{text}'"
            );
        }
        v
    };

    // English: an animal/pet pair vs. an unrelated finance sentence.
    let en_cat = embed("A cat is a small domesticated feline animal kept as a pet.");
    let en_dog = embed("A dog is a loyal four-legged animal that people keep as a pet.");
    let en_fin = embed("The central bank raised interest rates to fight inflation.");
    // Polish equivalents.
    let pl_kot = embed("Kot to małe udomowione zwierzę trzymane w domu.");
    let pl_pies = embed("Pies to wierne czworonożne zwierzę domowe.");
    let pl_fin = embed("Bank centralny podniósł stopy procentowe z powodu inflacji.");

    let en_sim = cosine(&en_cat, &en_dog);
    let en_dis = cosine(&en_cat, &en_fin);
    let pl_sim = cosine(&pl_kot, &pl_pies);
    let pl_dis = cosine(&pl_kot, &pl_fin);
    eprintln!("EN cos(cat,dog)={en_sim:.4}  cos(cat,finance)={en_dis:.4}");
    eprintln!("PL cos(kot,pies)={pl_sim:.4}  cos(kot,finanse)={pl_dis:.4}");

    assert!(
        en_sim > en_dis,
        "expected cat~dog ({en_sim:.4}) > cat~finance ({en_dis:.4})"
    );
    assert!(
        pl_sim > pl_dis,
        "expected kot~pies ({pl_sim:.4}) > kot~finanse ({pl_dis:.4})"
    );
}
