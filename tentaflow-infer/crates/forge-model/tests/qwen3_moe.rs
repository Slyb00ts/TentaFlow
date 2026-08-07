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
///
/// The prompt is 20 tokens and that number is load-bearing: above a threshold
/// the executor stops routing token by token and REORDERS the step so each
/// expert multiplies its own block of rows at once. A five-token prompt would
/// leave that route uncompared against anything — which is how it could ship
/// wrong and still produce Polish.
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

    let unit = [785u32, 6722, 315, 9625, 374];
    let prompt: Vec<u32> = unit.iter().cycle().take(20).copied().collect();
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

    // The same prompt, one token at a time: below the threshold every step
    // routes token by token, so this is the OTHER route through the same
    // weights. Comparing the two routes to each other says nothing on its own —
    // two copies of one mistake agree — but the grouped side has just been
    // pinned to the f32 reference above, so agreeing with it pins this one too.
    gpu.reset(SLOT).expect("reset");
    for &token in &prompt {
        gpu.decode(&[Feed { slot: SLOT, token }]).expect("krok");
    }
    gpu.decode(&feed).expect("krok po prompcie");
    compare("krok po kroku", &gpu, &cpu);
}

fn compare(what: &str, gpu: &Dense<CudaExec>, cpu: &Dense<HostExec>) {
    let got = gpu.logits(0).expect("logity CUDA");
    let want = cpu.logits(0).expect("logity wzorca");
    // The same threshold the dense path is held to. A routing that picked a
    // different expert would land orders of magnitude outside it.
    common::agrees(what, &got, &want, 0.02);
}

/// A step recorded at one width, after the buffers it names were replaced.
///
/// The executor records a decode step once and replays it, which makes the
/// recording a set of ADDRESSES. The mixture's scratch is sized for the widest
/// step seen so far and REPLACED when a wider one arrives, so a long prompt
/// that follows a short conversation frees exactly the buffers the recorded
/// decode step launches over.
///
/// Held by the RECORDING COUNT rather than by tokens, deliberately. The
/// released region stays mapped, so a stale recording reads and writes its own
/// now-unowned scratch and keeps answering correctly — right up until that
/// memory is handed to something else. A token comparison would pass today and
/// say nothing about the invariant that has to hold.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu Qwen3-MoE"]
fn a_recorded_step_does_not_outlive_the_buffers_it_names() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3-MoE");
        return;
    };
    let Some(device) = device() else { return };
    let mut model = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie Qwen3-MoE na CUDA");

    let prompt = vec![1u32, 991, 302, 15, 4234];
    // Long enough that a step is recorded rather than only warmed up.
    model.reset(SLOT).expect("reset");
    let before = model.generate(SLOT, &prompt, 8).expect("przebieg samotny");
    assert_eq!(
        model.exec_mut().recorded_steps(),
        1,
        "krok dekodowania nie został w ogóle nagrany"
    );

    // Wider than anything the short prompt asked for, so the mixture sizes its
    // scratch again: 64 tokens is 512 selections against the 40 of five.
    let long: Vec<u32> = prompt.iter().cycle().take(64).copied().collect();
    model.reset(1).expect("reset drugiego slotu");
    model.prefill(1, &long).expect("długi prefill");
    assert_eq!(
        model.exec_mut().recorded_steps(),
        0,
        "nagranie przeżyło bufory, które nazywa"
    );

    model.reset(SLOT).expect("reset");
    let after = model.generate(SLOT, &prompt, 8).expect("przebieg samotny");
    assert_eq!(
        before, after,
        "ten sam prompt po szerszym kroku poszedł inaczej: {before:?} wobec {after:?}"
    );
}
