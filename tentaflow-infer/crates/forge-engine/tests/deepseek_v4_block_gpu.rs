// ===== File: deepseek_v4_block_gpu.rs — hyper-connections bloku na GPU =====
//
// Blok DeepSeeka V4 nie ma zwykłego rezyduum: strumień to `hc_mult` kopii stanu.
// Wejście podbloku powstaje przez ważoną redukcję tych kopii, a wyjście wraca
// przez macierz mieszającą po Sinkhornie.
//
// Test sprawdza obie strony na prawdziwych wagach warstwy 2 wobec
// `tools/deepseek_v4_oracle.py`. Kluczowy jest przy tym szczegół, którego test
// pojedynczego kernela nie widzi: `hc_leave` MUSI użyć tych samych `post` i
// `comb`, które policzył `hc_enter`. Policzenie ich ponownie na zmienionym
// stanie daje wartości tego samego rzędu i inny model.

use std::path::PathBuf;
use std::sync::Arc;

use forge_engine::deepseek::{hc_enter, hc_leave, HyperConnectionBufs};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{DType, MemKind};
use forge_engine::weights::HyperConnectionWeights;
use half::f16;

const LAYER: usize = 2;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

struct Oracle {
    seqlen: usize,
    dim: usize,
    hc: usize,
    hc_in: Vec<f32>,
    attn_full: Vec<f32>,
    normed_attn: Vec<f32>,
    block_state: Vec<f32>,
}

fn load_oracle() -> Option<Oracle> {
    let bytes = std::fs::read(std::env::var("FORGE_DEEPSEEK_V4_ORACLE").ok()?).ok()?;
    let h: Vec<i32> = bytes[..72]
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let (seqlen, dim, n_heads, head_dim) = (h[0] as usize, h[1] as usize, h[2] as usize, h[3] as usize);
    let (topk, ratio) = (h[6] as usize, h[10] as usize);
    let (index_head_dim, n_topk, hc) = (h[12] as usize, h[14] as usize, h[15] as usize);
    let (decode_steps, vocab) = (h[16] as usize, h[17] as usize);
    let floats = |at: usize, n: usize| -> Vec<f32> {
        bytes[at..at + n * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let mut at = 72;
    let sizes = [
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
    ];
    for n in sizes {
        at += n * 4;
    }
    let hc_in = floats(at, seqlen * hc * dim);
    at += (seqlen * hc * dim + seqlen * dim + seqlen * dim + seqlen * hc * dim
        + decode_steps * dim
        + head_dim
        + seqlen * dim
        + vocab
        + seqlen
        + seqlen * topk
        + seqlen * topk)
        * 4;
    let attn_full = floats(at, seqlen * dim);
    at += seqlen * dim * 4;
    let normed_attn = floats(at, seqlen * dim);
    at += seqlen * dim * 4;
    let block_state = floats(at, seqlen * hc * dim);
    Some(Oracle {
        seqlen,
        dim,
        hc,
        hc_in,
        attn_full,
        normed_attn,
        block_state,
    })
}

fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 512 << 20,
            kv_cache: 16 << 20,
            activations: 256 << 20,
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

fn upload_f32_named(dev: &dyn Device, st: &ShardedSafeTensors, name: &str) -> DevBuffer {
    let info = st.tensor(name).expect(name);
    let data = st.data(name).unwrap();
    let values: Vec<f32> = match info.dtype {
        DType::F32 => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        DType::BF16 => data
            .chunks_exact(2)
            .map(|c| f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16))
            .collect(),
        other => panic!("{name}: nieoczekiwany typ {other:?}"),
    };
    let bytes =
        unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, values.len() * 4) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

fn relative_l2(got: &[f32], want: &[f32]) -> f32 {
    let num: f64 = got
        .iter()
        .zip(want)
        .map(|(g, w)| ((g - w) as f64).powi(2))
        .sum();
    let den: f64 = want.iter().map(|w| (*w as f64).powi(2)).sum();
    (num / den.max(f64::MIN_POSITIVE)).sqrt() as f32
}

#[test]
fn hyper_connections_wrap_the_attention_subblock() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let hc_weights = HyperConnectionWeights {
        mix_fn: upload_f32_named(dev.as_ref(), &st, &format!("layers.{LAYER}.hc_attn_fn")),
        base: upload_f32_named(dev.as_ref(), &st, &format!("layers.{LAYER}.hc_attn_base")),
        scale: upload_f32_named(dev.as_ref(), &st, &format!("layers.{LAYER}.hc_attn_scale")),
    };
    let attn_norm = upload_f32_named(dev.as_ref(), &st, &format!("layers.{LAYER}.attn_norm.weight"));
    // Norma RMS czyta wagę w f16, tak jak pozostałe warstwy silnika.
    let attn_norm_f16 = {
        let mut bytes = vec![0u8; oracle.dim * 4];
        dev.read(&attn_norm, 0, &mut bytes).unwrap();
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        upload_f16(dev.as_ref(), &values)
    };

    let bufs =
        HyperConnectionBufs::new(dev.as_ref(), oracle.dim, oracle.hc, oracle.seqlen).unwrap();
    let residual = upload_f16(dev.as_ref(), &oracle.hc_in);

    hc_enter(
        &kernels,
        &hc_weights,
        &attn_norm_f16,
        &bufs,
        &residual,
        oracle.dim,
        oracle.seqlen,
        20,
        1e-6,
        1e-6,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let normed = download_f16(dev.as_ref(), bufs.normed(), oracle.seqlen * oracle.dim);
    let enter_err = relative_l2(&normed, &oracle.normed_attn);
    eprintln!("hc_enter: względne L2 = {enter_err:.3e}");
    assert!(enter_err < 1e-2, "wejście podbloku rozjeżdża się o {enter_err:.3e}");

    // Wyjście podbloku to zwalidowane wcześniej wyjście uwagi.
    let block_out = upload_f16(dev.as_ref(), &oracle.attn_full);
    let out = dev
        .alloc(
            oracle.seqlen * oracle.hc * oracle.dim * 2,
            MemKind::Device,
            Pool::Activations,
        )
        .unwrap();
    hc_leave(
        &kernels,
        &bufs,
        &block_out,
        &residual,
        &out,
        oracle.dim,
        oracle.seqlen,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let expanded = download_f16(dev.as_ref(), &out, oracle.seqlen * oracle.hc * oracle.dim);
    let leave_err = relative_l2(&expanded, &oracle.block_state);
    eprintln!("hc_leave: względne L2 = {leave_err:.3e}");
    assert!(leave_err < 1e-2, "wyjście bloku rozjeżdża się o {leave_err:.3e}");
    assert_eq!(
        expanded.iter().filter(|v| !v.is_finite()).count(),
        0,
        "stan bloku zawiera wartości nieskończone lub NaN"
    );
}
