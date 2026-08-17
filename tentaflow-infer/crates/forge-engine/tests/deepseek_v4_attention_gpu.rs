// ===== File: deepseek_v4_attention_gpu.rs — pełna ścieżka uwagi na GPU =====
//
// To jest test złożenia, nie pojedynczych kroków. Każdy kernel ma już swój test
// złoty, a każdy fragment matematyki referencję przypiętą do implementacji
// autorów modelu — tu sprawdzamy KOLEJNOŚĆ, bufory i przepływ danych między
// nimi, czyli miejsce, gdzie te pierwsze dwa poziomy niczego nie złapią.
//
// Wejście i oczekiwane wyjście pochodzą z `tools/deepseek_v4_oracle.py`
// policzonego na prawdziwych wagach warstwy 2 — warstwie o stopniu kompresji 4,
// czyli z kompresorem I indekserem, najbogatszym wariancie tej architektury.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::deepseek::{attention_prefill, DeepseekAttnBufs, DeepseekAttnShape};
use forge_engine::weights::load_deepseek_layer_for_test;
use forge_formats::HfConfig;
use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const LAYER: usize = 2;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json")
        .is_file()
        .then_some(dir)
}

/// Zrzut oracle'a: interesują nas `x` (wejście warstwy) i `attn_full` (wyjście
/// całej uwagi). Pozostałe pola przeskakujemy, znając ich rozmiary z nagłówka.
struct Oracle {
    seqlen: usize,
    dim: usize,
    x: Vec<f32>,
    attn_full: Vec<f32>,
}

fn load_oracle() -> Option<Oracle> {
    let bytes = std::fs::read(std::env::var("FORGE_DEEPSEEK_V4_ORACLE").ok()?).ok()?;
    let head: Vec<i32> = bytes[..72]
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let (seqlen, dim, n_heads, head_dim) = (
        head[0] as usize,
        head[1] as usize,
        head[2] as usize,
        head[3] as usize,
    );
    let (topk, o_groups, ratio) = (head[6] as usize, head[8] as usize, head[10] as usize);
    let (index_head_dim, n_topk, hc) = (head[12] as usize, head[14] as usize, head[15] as usize);
    let (decode_steps, vocab) = (head[16] as usize, head[17] as usize);
    let _ = o_groups;

    let floats = |at: usize, n: usize| -> Vec<f32> {
        bytes[at..at + n * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let mut at = 72;
    let x = floats(at, seqlen * dim);
    // Kolejność zapisu w oracle'u; ostatnie pole to pełne wyjście uwagi.
    for n in [
        seqlen * dim,
        seqlen * 1024,
        seqlen * n_heads * head_dim,
        seqlen * head_dim,
        seqlen * dim,
        seqlen * topk,
        seqlen * topk,
        seqlen * dim,
        seqlen * n_heads * head_dim,
        seqlen * dim,
        seqlen / ratio * head_dim,
        seqlen / ratio * index_head_dim,
        seqlen * (seqlen / ratio),
        seqlen * n_topk,
        seqlen * n_heads * head_dim,
        seqlen * hc * dim,
        seqlen * dim,
        seqlen * dim,
        seqlen * hc * dim,
        decode_steps * dim,
        head_dim,
        seqlen * dim,
        vocab,
        seqlen,
        seqlen * topk,
        seqlen * topk,
    ] {
        at += n * 4;
    }
    let attn_full = floats(at, seqlen * dim);
    Some(Oracle {
        seqlen,
        dim,
        x,
        attn_full,
    })
}

fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 3 << 30,
            kv_cache: 16 << 20,
            activations: 512 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .expect("GPU wymagane")
}

fn upload_f16(dev: &dyn Device, values: &[f32]) -> DevBuffer {
    let host: Vec<f16> = values.iter().map(|v| f16::from_f32(*v)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

/// Częstotliwości rope z interpolacją YaRN — warstwa 2 ma kompresję, więc używa
/// bazy 160000 i skalowania.
fn rope_freqs(dim: usize, base: f32, original_seq_len: usize, factor: f32) -> Vec<f32> {
    let mut freqs: Vec<f32> = (0..dim / 2)
        .map(|i| 1.0 / base.powf(2.0 * i as f32 / dim as f32))
        .collect();
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

#[test]
fn attention_prefill_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let config = HfConfig::load(dir.join("config.json")).unwrap();
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let weights = load_deepseek_layer_for_test(dev.as_ref(), &dir, LAYER).unwrap();

    let shape = DeepseekAttnShape {
        hidden: config.hidden_size,
        n_heads: config.num_attention_heads,
        head_dim: config.head_dim.unwrap(),
        rope_head_dim: config.qk_rope_head_dim.unwrap(),
        q_lora_rank: config.q_lora_rank.unwrap(),
        o_groups: config.o_groups.unwrap(),
        o_lora_rank: config.o_lora_rank.unwrap(),
        window_size: config.sliding_window.unwrap(),
        compress_ratio: config.compress_ratios[LAYER],
        index_n_heads: config.index_n_heads.unwrap(),
        index_head_dim: config.index_head_dim.unwrap(),
        index_topk: config.index_topk.unwrap(),
        eps: config.rms_norm_eps,
    };
    let freqs = rope_freqs(shape.rope_head_dim, 160_000.0, 65_536, 16.0);
    let bufs = DeepseekAttnBufs::new(dev.as_ref(), &shape, oracle.seqlen, &freqs).unwrap();

    let x = upload_f16(dev.as_ref(), &oracle.x);
    let out = dev
        .alloc(
            oracle.seqlen * oracle.dim * 2,
            MemKind::Device,
            Pool::Activations,
        )
        .unwrap();
    attention_prefill(
        &kernels,
        dev.as_ref(),
        &weights,
        &shape,
        &bufs,
        &x,
        &out,
        oracle.seqlen,
        &stream,
    )
    .expect("prefill uwagi");
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; oracle.seqlen * oracle.dim * 2];
    dev.read(&out, 0, &mut bytes).unwrap();
    let got: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();

    // NaN pojawiłby się przy pierwszym przelaniu zakresu w kompresorze, a
    // uśrednione L2 potrafi go przemilczeć.
    assert_eq!(
        got.iter().filter(|v| !v.is_finite()).count(),
        0,
        "wyjście zawiera wartości nieskończone lub NaN"
    );
    let num: f64 = got
        .iter()
        .zip(&oracle.attn_full)
        .map(|(g, w)| ((g - w) as f64).powi(2))
        .sum();
    let den: f64 = oracle.attn_full.iter().map(|w| (*w as f64).powi(2)).sum();
    let rel = (num / den.max(f64::MIN_POSITIVE)).sqrt();
    eprintln!("uwaga DeepSeeka na GPU: względne L2 = {rel:.3e}");
    // Próg mieści akumulację w f16 na całej ścieżce (referencja liczy w f32),
    // ale nie zmieściłby pomylonej kolejności kroków ani złego bufora.
    assert!(rel < 5e-2, "ścieżka uwagi rozjeżdża się o {rel:.3e}");
    assert!(
        oracle.attn_full.iter().any(|v| v.abs() > 1e-3),
        "referencja jest zerowa, test nic nie dowodzi"
    );
}
