// ===== File: qwen36_hybrid.rs — a hybrid mixture checkpoint, read before it is run =====
//
// Qwen3.6-35B-A3B is the second family this path meets and the first that is
// not one kind of layer repeated: three of every four blocks mix tokens with a
// recurrent Gated-DeltaNet scan and the fourth with output-gated attention,
// while EVERY block feeds a mixture of 256 experts plus a shared one.
//
// This file starts at the only question that can be answered before any of that
// computes: does the checkpoint DESCRIBE itself correctly, and does the model
// refuse the parts it cannot yet compute by name. Both matter. A descriptor
// that quietly reported forty attention layers would be computed happily, and
// the answer would be fluent, wrong text.

#[allow(dead_code)]
mod common;

use std::path::PathBuf;
use std::sync::Arc;

use forge_formats::checkpoint::Checkpoint;
use forge_formats::{LayerKind, WeightRole};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_model::dense::{Dense, Feed};

fn checkpoint() -> Option<PathBuf> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../.runtime/models/qwen36-35b-a3b-mxfp4-gguf"
    ));
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
}

/// The file's own account of itself, held against what it contains.
///
/// Worth a test of its own because this checkpoint could not be OPENED at all
/// until now: the descriptor refused every MoE hybrid carrying a speculation
/// head, which is a property of one runtime's MTP path and not of the file. The
/// forty trunk layers were unreachable collateral, and the branch that builds
/// the mixture head was dead code no test could have reached.
#[test]
#[ignore = "wymaga checkpointu Qwen3.6-MoE"]
fn the_hybrid_checkpoint_describes_itself() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3.6-MoE");
        return;
    };
    let ckpt = Checkpoint::open(&path).expect("otwarcie checkpointu hybrydy");
    let desc = ckpt.descriptor();
    let p = &desc.params;

    assert_eq!(desc.arch, "qwen35moe");
    // 41 blocks in the file, the last one a speculation head that the
    // autoregressive stack does not run.
    assert_eq!(desc.layers.len(), 40);
    assert_eq!(desc.layer_kinds.len(), 40);
    assert!(desc.mtp.is_some(), "głowa MTP ma być OPISANA, choć nieliczona");

    // Every fourth block mixes with attention, the rest with DeltaNet. Checked
    // as the whole sequence rather than as a count: thirty DeltaNet layers in
    // the wrong PLACES is the same count and a different model.
    let kinds: Vec<LayerKind> = desc.layer_kinds.clone();
    let expected: Vec<LayerKind> = (0..40)
        .map(|i| {
            if (i + 1) % 4 == 0 {
                LayerKind::Attention
            } else {
                LayerKind::DeltaNet
            }
        })
        .collect();
    assert_eq!(kinds, expected);

    assert_eq!(p.hidden_size, 2048);
    assert_eq!(p.n_heads, 16);
    assert_eq!(p.n_kv_heads, 2);
    // Attention heads are 256 wide, so Q is 4096 — twice the residual stream —
    // and the stored projection is twice THAT again, because it is gated.
    assert_eq!(p.head_dim, 256);
    assert!(p.attn_gated, "uwaga tej rodziny jest bramkowana");

    let moe = p.moe.as_ref().expect("mieszanka");
    assert_eq!(moe.n_experts, 256);
    assert_eq!(moe.n_experts_used, 8);
    assert_eq!(moe.moe_intermediate_size, 512);
    assert_eq!(
        moe.shared_intermediate_size, 512,
        "ekspert współdzielony liczy KAŻDY token"
    );

    let ssm = p.ssm.as_ref().expect("parametry DeltaNet");
    assert_eq!(ssm.d_conv, 4);
    assert_eq!(ssm.d_state, 128);
    // 16 key heads against 32 value heads: q and k are repeated four... two per
    // value head, which is why the recurrence cannot simply read them by index.
    assert_eq!(ssm.n_group, 16);
    assert_eq!(ssm.dt_rank, 32);
    assert_eq!(ssm.d_inner, 4096);

    // Partial rotary: the sections sum to half the rotated width, and only 64
    // of each 256-wide head turn. A full rotation here would be silent.
    let sections = p.rope_sections.expect("sekcje M-RoPE");
    assert_eq!(sections, [11, 11, 10, 0]);
    assert_eq!(sections.iter().sum::<u32>() * 2, 64);

    // Every trunk layer carries the mixture AND the shared expert; the roles a
    // layer carries are what the loader is held against.
    for (index, layer) in desc.layers.iter().enumerate() {
        for role in [
            WeightRole::FfnGateInp,
            WeightRole::FfnGateExps,
            WeightRole::FfnUpExps,
            WeightRole::FfnDownExps,
            WeightRole::FfnGateShExp,
            WeightRole::FfnUpShExp,
            WeightRole::FfnDownShExp,
            WeightRole::FfnGateInpShExp,
        ] {
            assert!(layer.contains_key(&role), "warstwa {index} bez {role:?}");
        }
    }
}

/// The whole checkpoint, on the card.
///
/// Everything 4b built meets here for the first time: thirty recurrent layers
/// interleaved with ten gated-attention ones, a mixture of 256 experts plus an
/// always-on one under every single block, and MXFP4 weights on a real file
/// rather than on masked bytes.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu Qwen3.6-MoE"]
fn the_hybrid_continues_a_factual_prompt() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3.6-MoE");
        return;
    };
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    }
    let free = CudaDevice::free_vram(0).expect("wolna pamięć");
    eprintln!("wolne VRAM: {:.1} GiB", free as f64 / (1u64 << 30) as f64);
    // 256 MiB of cache, which is the measurement rather than a round number:
    // a page costs its bytes in every ALLOCATED layer at once, so sizing the
    // cache by the layer count would make one page 20 MiB here and this pool
    // would hold fewer than one sequence's worth. Only ten of forty layers
    // attend, so a page costs 5 MiB and the pool holds four sequences.
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 24 << 30,
            kv_cache: 256 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            activations: 2 << 30,
        },
    )
    .expect("karta jest, a nie oddała pul");

    let gguf = forge_formats::Gguf::open(&path).expect("otwarcie GGUF");
    let vocab = forge_tokenize::gguf_vocab(&gguf).expect("słownik z GGUF");
    let tok = forge_tokenize::Tokenizer::from_gguf_vocab(&vocab).expect("tokenizator");

    let t = std::time::Instant::now();
    let mut model = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie hybrydy na CUDA");
    eprintln!("wczytane w {:.1} s", t.elapsed().as_secs_f64());

    let prompt = tok
        .encode("The capital of France is", true)
        .expect("tokenizacja");
    let t = std::time::Instant::now();
    let out = model.generate(0, &prompt, 20).expect("generacja");
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

    // Ten sam prompt, tylko DŁUGI, i kontynuowany — a nie odłożony.
    //
    // Mieszanka wybiera kernel po tym, ile wierszy wypada na eksperta, więc
    // krótki prompt nigdy nie wchodzi w kafel zgrupowany, który liczy prefill
    // szerokiego promptu. Poprzednia postać tego testu wgrywała 256 tokenów
    // WYŁĄCZNIE dla pomiaru przepustowości, po czym robiła `reset` i generowała
    // z sześciu — więc wyjście kafla nie było sprawdzane przez nic. Kernel
    // czytający trzy czwarte słów wagi spod przesuniętego adresu przechodził
    // wtedy CAŁY zestaw bramek, będąc o jedną czwartą szybszym; łapie go
    // dopiero to.
    model.reset(0).expect("reset");
    let long: Vec<u32> = prompt.iter().cycle().take(1024).copied().collect();
    let t = std::time::Instant::now();
    let wide = model.generate(0, &long, 20).expect("generacja z długiego");
    let took = t.elapsed().as_secs_f64();
    let wide_text = tok.decode(&wide, true).expect("dekodowanie długiego");
    eprintln!(
        "{} tokenów promptu w {took:.2} s: {wide_text:?}",
        long.len()
    );
    assert!(
        wide_text.contains("France") || wide_text.contains("Paris"),
        "kontynuacja długiego promptu zgubiła jego treść: {wide_text:?}"
    );
    assert!(
        wide.windows(4).any(|w| w.iter().any(|t| *t != w[0])),
        "kontynuacja długiego promptu to jeden powtarzany token: {wide:?}"
    );
}

/// The same forty layers, computed twice by executors sharing nothing below the
/// contract.
///
/// This is the check the English continuation cannot make. A recurrent state
/// that decays too fast, a convolution window that does not advance, an expert
/// chosen one apart — all of them still produce sentences, and this checkpoint's
/// greedy continuation of a factual prompt repeats itself either way.
///
/// Slow, and the slowness is thirty recurrent layers walked one token at a time
/// in scalar f32 with every MXFP4 row decoded on demand. The prompt is therefore
/// as short as it can be and still cross the width above which the executor
/// stops routing token by token and REORDERS the step so each expert multiplies
/// its own block of rows at once. That threshold matters most here: this is the
/// only checkpoint on this path with a shared expert, so the batched form of it
/// is compared against an oracle nowhere else.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu Qwen3.6-MoE; wzorzec liczy minutami na token"]
fn the_hybrid_agrees_with_the_host_reference() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3.6-MoE");
        return;
    };
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    }
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 24 << 30,
            kv_cache: 1 << 30,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            activations: 2 << 30,
        },
    )
    .expect("karta jest, a nie oddała pul");

    let mut gpu = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie na CUDA");
    let t = std::time::Instant::now();
    let mut cpu = Dense::load(&path, HostExec::new).expect("wczytanie na wzorcu");
    eprintln!("wzorzec wczytany w {:.1} s", t.elapsed().as_secs_f64());

    let unit = [785u32, 6722, 315, 9625, 374];
    let prompt: Vec<u32> = unit.iter().cycle().take(20).copied().collect();
    let t = std::time::Instant::now();
    let gpu_first = gpu.prefill(0, &prompt).expect("prefill CUDA");
    eprintln!("CUDA: prefill w {:.2} s", t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let cpu_first = cpu.prefill(0, &prompt).expect("prefill wzorca");
    eprintln!("wzorzec: prefill w {:.1} s", t.elapsed().as_secs_f64());

    common::agrees(
        "prefill",
        &gpu.logits(0).expect("logity CUDA"),
        &cpu.logits(0).expect("logity wzorca"),
        0.02,
    );
    assert_eq!(gpu_first, cpu_first, "prefill wybrał inny token");

    // The step is what says the STATE agrees, not just the arithmetic of one
    // token: it reads thirty state matrices the prefill left behind.
    let feed = [Feed {
        slot: 0,
        token: gpu_first,
    }];
    gpu.decode(&feed).expect("krok CUDA");
    cpu.decode(&feed).expect("krok wzorca");
    common::agrees(
        "krok",
        &gpu.logits(0).expect("logity CUDA"),
        &cpu.logits(0).expect("logity wzorca"),
        0.02,
    );

    // The same prompt one token at a time, which is below the threshold and so
    // routes the OTHER way — including a shared expert folded per token rather
    // than for the whole step. Two routes agreeing proves nothing by itself,
    // but the first has just been pinned to the f32 reference.
    gpu.reset(0).expect("reset");
    for &token in &prompt {
        gpu.decode(&[Feed { slot: 0, token }]).expect("krok");
    }
    gpu.decode(&feed).expect("krok po prompcie");
    common::agrees(
        "krok po kroku",
        &gpu.logits(0).expect("logity kroków"),
        &cpu.logits(0).expect("logity wzorca"),
        0.02,
    );
}
