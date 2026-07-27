// ===== File: deepseek_v4_model_gpu.rs — złożenie przebiegu modelu na GPU =====
//
// Sprawdza to, czego nie widzą testy pojedynczych bloków: embedding rozdzielony
// na kopie strumienia, przejście przez warstwy z aktualizacją stanu, głowa z
// własną redukcją i logity ostatniej pozycji.
//
// Model jest obcięty do kilku warstw, bo pełny ma 157 GB — ale jest to poprawny
// model o mniejszej liczbie warstw, więc ścieżka wykonania jest ta sama.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::deepseek::{
    model_prefill, DeepseekAttnShape, DeepseekFfnShape, DeepseekModelBufs, DeepseekModelShape,
    HyperConnectionConfig,
};
use forge_engine::weights::load_deepseek_prefix_for_test;
use forge_formats::HfConfig;
use forge_hal::cuda::PoolSizes;
use forge_hal::Device;
use forge_kernels::Kernels;

const LAYERS: usize = 2;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 10 << 30,
            kv_cache: 16 << 20,
            activations: 1 << 30,
            kv_page_size: 256 << 10,
        },
    )
    .expect("GPU wymagane")
}

/// Częstotliwości rope warstwy: z YaRN i bazą kompresji, gdy warstwa kompresuje
/// KV; inaczej czyste rope z bazą podstawową.
fn rope_freqs(dim: usize, base: f32, original_seq_len: usize, factor: f32) -> Vec<f32> {
    let mut freqs: Vec<f32> = (0..dim / 2)
        .map(|i| 1.0 / base.powf(2.0 * i as f32 / dim as f32))
        .collect();
    if original_seq_len == 0 {
        return freqs;
    }
    let correction = |rot: f32| -> f32 {
        dim as f32 * (original_seq_len as f32 / (rot * 2.0 * std::f32::consts::PI)).ln()
            / (2.0 * base.ln())
    };
    let low = correction(32.0).floor().max(0.0);
    let high = correction(1.0).ceil().min(dim as f32 - 1.0);
    for (i, freq) in freqs.iter_mut().enumerate() {
        let ramp = ((i as f32 - low) / (high - low)).clamp(0.0, 1.0);
        let smooth = 1.0 - ramp;
        *freq = *freq / factor * (1.0 - smooth) + *freq * smooth;
    }
    freqs
}

fn model_shape(config: &HfConfig, layers: usize) -> DeepseekModelShape {
    let rope_dim = config.qk_rope_head_dim.unwrap();
    let mut attn = Vec::with_capacity(layers);
    let mut rope = Vec::with_capacity(layers);
    for layer in 0..layers {
        let ratio = config.compress_ratios[layer];
        attn.push(DeepseekAttnShape {
            hidden: config.hidden_size,
            n_heads: config.num_attention_heads,
            head_dim: config.head_dim.unwrap(),
            rope_head_dim: rope_dim,
            q_lora_rank: config.q_lora_rank.unwrap(),
            o_groups: config.o_groups.unwrap(),
            o_lora_rank: config.o_lora_rank.unwrap(),
            window_size: config.sliding_window.unwrap(),
            compress_ratio: ratio,
            index_n_heads: config.index_n_heads.unwrap(),
            index_head_dim: config.index_head_dim.unwrap(),
            index_topk: config.index_topk.unwrap(),
            eps: config.rms_norm_eps,
        });
        rope.push(if ratio == 0 {
            rope_freqs(rope_dim, config.rope_theta, 0, 1.0)
        } else {
            rope_freqs(rope_dim, config.compress_rope_theta.unwrap(), 65_536, 16.0)
        });
    }
    DeepseekModelShape {
        hidden: config.hidden_size,
        vocab: config.vocab_size,
        n_experts: config.n_routed_experts.unwrap(),
        hc: HyperConnectionConfig {
            hc: config.hc_mult.unwrap_or(4),
            sinkhorn_iters: config.hc_sinkhorn_iters.unwrap_or(20),
            eps: config.rms_norm_eps,
            hc_eps: config.hc_eps.unwrap_or(1e-6),
        },
        ffn: DeepseekFfnShape {
            hidden: config.hidden_size,
            moe_inter: config.moe_intermediate_size.unwrap(),
            n_experts_used: config.num_experts_per_tok.unwrap(),
            route_scale: config.routed_scaling_factor.unwrap_or(1.0),
            swiglu_limit: config.swiglu_limit.unwrap_or(0.0),
        },
        attn,
        rope,
    }
}

#[test]
fn model_prefill_produces_finite_logits() {
    let Some(dir) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    let config = HfConfig::load(dir.join("config.json")).unwrap();
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let weights = load_deepseek_prefix_for_test(dev.as_ref(), &dir, LAYERS, 4 << 30, None)
        .expect("wczytanie obciętego modelu");
    let shape = model_shape(&config, LAYERS);
    let tokens: Vec<u32> = vec![0, 17, 342, 9001, 5, 88, 1234, 7];
    let mut bufs = DeepseekModelBufs::new(dev.as_ref(), &shape, tokens.len()).unwrap();

    model_prefill(
        &kernels,
        dev.as_ref(),
        &weights,
        &shape,
        &mut bufs,
        &tokens,
        &stream,
    )
    .expect("prefill modelu");
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; shape.vocab * 4];
    dev.read(bufs.logits(), 0, &mut bytes).unwrap();
    let logits: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    assert_eq!(
        logits.iter().filter(|v| !v.is_finite()).count(),
        0,
        "logity zawierają wartości nieskończone lub NaN"
    );
    // Rozkład zdegenerowany (same zera, albo jedna wartość) oznacza, że coś po
    // drodze wyzerowało stan — a to przeszłoby test na samą skończoność.
    let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min = logits.iter().cloned().fold(f32::INFINITY, f32::min);
    assert!(
        max - min > 1.0,
        "logity są zdegenerowane: zakres {min}..{max}"
    );
    let best = logits
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, v)| (i, *v))
        .unwrap();
    eprintln!(
        "logity: zakres {min:.3}..{max:.3}, argmax {} ({:.3})",
        best.0, best.1
    );
}
