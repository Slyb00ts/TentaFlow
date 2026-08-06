// ===== File: qwen3_moe.rs — a mixture-of-experts checkpoint, end to end =====
//
// Qwen3-30B-A3B is the first checkpoint on this path that is not a dense llama,
// and it brings three things at once: a query projection wider than the
// residual stream, a learned per-head norm on Q and K, and a routed FFN. Each
// has its own hermetic gate in `forge-kernels`; this file is the one that says
// they compose, on real weights, in the order a real model runs them.
//
// Two gates, and they answer different questions. The comparison against the
// host reference says the arithmetic is the same arithmetic. The English
// continuation says the result is a MODEL — two implementations of the same
// misunderstanding would agree with each other and still say nothing.

#[allow(dead_code)]
mod common;

use std::path::PathBuf;
use std::sync::Arc;

use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_model::dense::{Dense, Feed};

const SLOT: usize = 0;

fn checkpoint() -> Option<PathBuf> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../.runtime/models/qwen3-30b-a3b-gguf"
    ));
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
}

/// Pools sized for this checkpoint rather than by fraction.
///
/// 18.56 GB of weights on a part whose CUDA allocator reports about 20 GB free,
/// so the other two pools are what is left rather than what is comfortable.
fn device() -> Option<Arc<CudaDevice>> {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return None;
    }
    let pools = PoolSizes {
        weights: 18 << 30,
        kv_cache: 512 << 20,
        kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        activations: 320 << 20,
    };
    Some(CudaDevice::new(0, pools).expect("karta jest, a nie oddała pul"))
}

fn tokenizer(path: &PathBuf) -> forge_tokenize::Tokenizer {
    let gguf = forge_formats::Gguf::open(path).expect("otwarcie GGUF");
    let vocab = forge_tokenize::gguf_vocab(&gguf).expect("słownik z GGUF");
    forge_tokenize::Tokenizer::from_gguf_vocab(&vocab).expect("tokenizator")
}

/// The result has to be a LANGUAGE, and about the right thing.
///
/// Agreement with a reference says both compute the same numbers; it cannot say
/// those numbers are the model. This is the criterion that cannot be met by
/// accident: a factual continuation of a factual prompt, through the
/// checkpoint's own tokenizer.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu Qwen3-MoE"]
fn the_mixture_continues_a_factual_prompt() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3-MoE");
        return;
    };
    let Some(device) = device() else { return };
    let tok = tokenizer(&path);

    let t = std::time::Instant::now();
    let mut model = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie Qwen3-MoE na CUDA");
    eprintln!("wczytane w {:.1} s", t.elapsed().as_secs_f64());

    let shape = model.shape();
    assert_eq!(shape.layers, 48);
    assert_eq!(shape.hidden, 2048);
    // The three facts that make this checkpoint different from every dense one
    // already tested here.
    assert_eq!(shape.attn_width(), 4096, "Q szersze niż strumień");
    assert_ne!(shape.attn_width(), shape.hidden);

    let prompt = tok
        .encode("The capital of France is", true)
        .expect("tokenizacja");
    // Prefill throughput on a prompt long enough for the number to mean
    // something. Kept because its absence is what let a per-token loop sit in
    // this path unnoticed: "the mixture runs" was true and said nothing about
    // what it costs.
    let long: Vec<u32> = prompt.iter().cycle().take(256).copied().collect();
    let t = std::time::Instant::now();
    let _ = model.prefill(SLOT, &long).expect("prefill długi");
    let took = t.elapsed().as_secs_f64();
    eprintln!("prefill {} tokenów = {:.0} tok/s", long.len(), long.len() as f64 / took);
    model.reset(SLOT).expect("reset");
    let t = std::time::Instant::now();
    let out = model.generate(SLOT, &prompt, 20).expect("generacja");
    let text = tok.decode(&out, true).expect("dekodowanie");
    eprintln!(
        "{} tokenów w {:.2} s: {text:?}",
        out.len(),
        t.elapsed().as_secs_f64()
    );

    assert!(
        text.contains("Paris"),
        "kontynuacja nie zna odpowiedzi: {text:?}"
    );
    assert!(
        out.windows(4).any(|w| w.iter().any(|t| *t != w[0])),
        "kontynuacja to jeden powtarzany token: {out:?}"
    );
}

/// The same operations, computed twice by two executors sharing nothing below
/// the contract.
///
/// Slow, and the slowness is the routed FFN: the reference decodes every row of
/// every chosen expert in scalar f32, which is about 1.8 G multiply-accumulates
/// per token across 48 layers. The prompt is therefore as short as a prompt can
/// be and still exercise a tile.
///
/// It also settles the one question the hermetic gates deliberately avoid. The
/// router kernel reads f16 and the reference keeps the source's f32, so a
/// near-tie between two experts could be broken differently on the two sides.
/// If that happens the logits diverge far past this threshold and the test says
/// so, rather than the difference being absorbed into a loose bound.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu Qwen3-MoE; wzorzec liczy minutami na token"]
fn the_mixture_agrees_with_the_host_reference() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3-MoE");
        return;
    };
    let Some(device) = device() else { return };

    let mut gpu = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie na CUDA");
    let t = std::time::Instant::now();
    let mut cpu = Dense::load(&path, HostExec::new).expect("wczytanie na wzorcu");
    eprintln!("wzorzec wczytany w {:.1} s", t.elapsed().as_secs_f64());

    let prompt = [785u32, 6722, 315, 9625, 374];
    let t = std::time::Instant::now();
    let gpu_first = gpu.prefill(SLOT, &prompt).expect("prefill CUDA");
    eprintln!("CUDA: prefill w {:.2} s", t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let cpu_first = cpu.prefill(SLOT, &prompt).expect("prefill wzorca");
    eprintln!("wzorzec: prefill w {:.1} s", t.elapsed().as_secs_f64());
    compare("prefill", &gpu, &cpu);
    assert_eq!(gpu_first, cpu_first, "prefill wybrał inny token");

    let feed = [Feed {
        slot: SLOT,
        token: gpu_first,
    }];
    gpu.decode(&feed).expect("krok CUDA");
    cpu.decode(&feed).expect("krok wzorca");
    compare("krok", &gpu, &cpu);
}

fn compare(what: &str, gpu: &Dense<CudaExec>, cpu: &Dense<HostExec>) {
    let got = gpu.logits(0).expect("logity CUDA");
    let want = cpu.logits(0).expect("logity wzorca");
    // The same threshold the dense path is held to. A routing that picked a
    // different expert would land orders of magnitude outside it.
    common::agrees(what, &got, &want, 0.02);
}
