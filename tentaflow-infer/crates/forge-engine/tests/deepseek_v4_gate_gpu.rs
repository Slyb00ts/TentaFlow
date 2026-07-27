// ===== File: deepseek_v4_gate_gpu.rs — bramka MoE DeepSeeka V4 na GPU =====
//
// Bramka ma trzy szczegóły, z których każdy przy pomyłce pozwala modelowi dalej
// generować tekst — tylko gorszy, bez jednego komunikatu:
//
//  1. bias wchodzi WYŁĄCZNIE do rankingu top-k, a nie do wag,
//  2. wynik to `sqrt(softplus(logit))`, nie softmax ani sigmoid,
//  3. wagi są normalizowane do sumy 1 i dopiero potem mnożone przez `route_scale`.
//
// Wzorzec pochodzi z `tools/deepseek_v4_oracle.py` policzonego na prawdziwych
// wagach warstwy bez routingu haszowanego.

use std::path::PathBuf;
use std::sync::Arc;

use forge_formats::safetensors::ShardedSafeTensors;
use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{DType, MemKind};
use half::f16;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

/// Ze zrzutu oracle'a bierzemy wejście bramki oraz oczekiwane indeksy i wagi.
struct Oracle {
    seqlen: usize,
    dim: usize,
    topk: usize,
    gate_layer: usize,
    gate_x: Vec<f32>,
    indices: Vec<i32>,
    weights: Vec<f32>,
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
    let (gate_layer, topk) = (head[5] as usize, head[6] as usize);
    let mut at = 72 + (seqlen * dim + seqlen * 1024 + seqlen * n_heads * head_dim + seqlen * head_dim) * 4;
    let gate_x: Vec<f32> = bytes[at..at + seqlen * dim * 4]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    at += seqlen * dim * 4;
    let indices: Vec<i32> = bytes[at..at + seqlen * topk * 4]
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    at += seqlen * topk * 4;
    let weights: Vec<f32> = bytes[at..at + seqlen * topk * 4]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Some(Oracle {
        seqlen,
        dim,
        topk,
        gate_layer,
        gate_x,
        indices,
        weights,
    })
}

fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 512 << 20,
            kv_cache: 16 << 20,
            activations: 128 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .expect("GPU wymagane")
}

fn upload(dev: &dyn Device, bytes: &[u8]) -> DevBuffer {
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn load_f32(st: &ShardedSafeTensors, name: &str) -> Vec<f32> {
    let info = st.tensor(name).expect(name);
    let data = st.data(name).unwrap();
    match info.dtype {
        DType::BF16 => data
            .chunks_exact(2)
            .map(|c| f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16))
            .collect(),
        DType::F32 => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        other => panic!("{name}: nieoczekiwany typ {other:?}"),
    }
}

#[test]
fn gate_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let g = format!("layers.{}.ffn.gate", oracle.gate_layer);
    let gate_w = load_f32(&st, &format!("{g}.weight"));
    let bias = load_f32(&st, &format!("{g}.bias"));
    let n_expert = bias.len();

    let gate_host: Vec<f16> = gate_w.iter().map(|v| f16::from_f32(*v)).collect();
    let gate_buf = upload(dev.as_ref(), unsafe {
        std::slice::from_raw_parts(gate_host.as_ptr() as *const u8, gate_host.len() * 2)
    });
    let bias_buf = upload(dev.as_ref(), unsafe {
        std::slice::from_raw_parts(bias.as_ptr() as *const u8, bias.len() * 4)
    });
    let x_host: Vec<f16> = oracle.gate_x.iter().map(|v| f16::from_f32(*v)).collect();
    let x_buf = upload(dev.as_ref(), unsafe {
        std::slice::from_raw_parts(x_host.as_ptr() as *const u8, x_host.len() * 2)
    });

    let ids = dev
        .alloc(oracle.seqlen * oracle.topk * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let weights = dev
        .alloc(oracle.seqlen * oracle.topk * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .moe_gate_sqrtsoftplus_f16(
            &ids,
            &weights,
            &x_buf,
            &gate_buf,
            &bias_buf,
            oracle.seqlen,
            oracle.dim,
            n_expert,
            oracle.topk,
            1.5,
            &stream,
        )
        .unwrap();
    stream.synchronize().unwrap();

    let mut idb = vec![0u8; oracle.seqlen * oracle.topk * 4];
    dev.read(&ids, 0, &mut idb).unwrap();
    let got_ids: Vec<i32> = idb
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut wb = vec![0u8; oracle.seqlen * oracle.topk * 4];
    dev.read(&weights, 0, &mut wb).unwrap();
    let got_w: Vec<f32> = wb
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    for token in 0..oracle.seqlen {
        let span = token * oracle.topk..(token + 1) * oracle.topk;
        let mut got = got_ids[span.clone()].to_vec();
        let mut want = oracle.indices[span.clone()].to_vec();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "token {token}: inny zestaw ekspertów");

        // Wagi porównujemy po ekspercie, bo kolejność wyboru może się różnić
        // przy remisach, a suma i przypisanie — nie.
        for (j, expert) in got_ids[span.clone()].iter().enumerate() {
            let at = oracle.indices[span.clone()]
                .iter()
                .position(|e| e == expert)
                .expect("ekspert z GPU jest w referencji");
            let diff = (got_w[span.start + j] - oracle.weights[span.start + at]).abs();
            assert!(
                diff < 2e-3,
                "token {token}, ekspert {expert}: waga {} zamiast {}",
                got_w[span.start + j],
                oracle.weights[span.start + at]
            );
        }
        let sum: f32 = got_w[span.clone()].iter().sum();
        assert!(
            (sum - 1.5).abs() < 5e-3,
            "token {token}: wagi sumują się do {sum}, a nie do route_scale"
        );
    }
}
