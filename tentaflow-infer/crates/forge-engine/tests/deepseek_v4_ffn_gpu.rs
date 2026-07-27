// ===== File: deepseek_v4_ffn_gpu.rs — SwiGLU eksperta DeepSeeka na GPU =====
//
// Sprawdza ścieżkę eksperta od wagi NVFP4 na dysku po wyjście: przepakowanie do
// układu jednobuforowego, GEMV, SwiGLU z niesymetrycznym obcięciem i projekcję
// wyjściową. Wzorzec pochodzi z `tools/deepseek_v4_oracle.py` policzonego na
// prawdziwym ekspercie 0 warstwy bez routingu haszowanego.
//
// Obcięcia SwiGLU są tu istotne: bramka ograniczana jest tylko od góry, a
// wejście obustronnie. Symetryczne obcięcie obu przechodzi bez błędu i psuje
// wynik tylko na skrajnych wartościach — czyli tam, gdzie średnia go ukryje.

use std::path::PathBuf;
use std::sync::Arc;

use forge_formats::nvfp4::{deepseek_expert_to_gguf, DeepseekNvFp4Names};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

struct Oracle {
    seqlen: usize,
    dim: usize,
    gate_layer: usize,
    gate_x: Vec<f32>,
    expert_out: Vec<f32>,
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
    let floats = |at: usize, n: usize| -> Vec<f32> {
        bytes[at..at + n * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let mut at =
        72 + (seqlen * dim + seqlen * 1024 + seqlen * n_heads * head_dim + seqlen * head_dim) * 4;
    let gate_x = floats(at, seqlen * dim);
    at += (seqlen * dim + seqlen * topk + seqlen * topk) * 4;
    let expert_out = floats(at, seqlen * dim);
    Some(Oracle {
        seqlen,
        dim,
        gate_layer,
        gate_x,
        expert_out,
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

/// Ekspert NVFP4 przez produkcyjne przepakowanie, wgrany na urządzenie.
fn upload_expert(
    dev: &dyn Device,
    st: &ShardedSafeTensors,
    base: &str,
) -> (DevBuffer, f32, usize, usize) {
    let names = DeepseekNvFp4Names::for_weight(&format!("{base}.weight")).unwrap();
    let info = st.tensor(&names.packed).expect(&names.packed);
    let gs = st.data(&names.global_scale).unwrap();
    let global = f32::from_le_bytes([gs[0], gs[1], gs[2], gs[3]]);
    let repacked = deepseek_expert_to_gguf(
        st.data(&names.packed).unwrap(),
        &info.shape,
        st.data(&names.scale).unwrap(),
        global,
    )
    .unwrap();
    let buf = dev
        .alloc(repacked.blocks.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&repacked.blocks, &buf, 0).unwrap();
    (buf, repacked.output_scale, repacked.rows, repacked.cols)
}

#[test]
fn expert_swiglu_matches_the_reference_on_gpu() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let base = format!("layers.{}.ffn.experts.0", oracle.gate_layer);

    let (w1, s1, inter, dim) = upload_expert(dev.as_ref(), &st, &format!("{base}.w1"));
    let (w3, s3, _, _) = upload_expert(dev.as_ref(), &st, &format!("{base}.w3"));
    let (w2, s2, out_dim, _) = upload_expert(dev.as_ref(), &st, &format!("{base}.w2"));
    assert_eq!(dim, oracle.dim);

    let x_host: Vec<f16> = oracle.gate_x.iter().map(|v| f16::from_f32(*v)).collect();
    let x = dev
        .alloc(x_host.len() * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(
        unsafe { std::slice::from_raw_parts(x_host.as_ptr() as *const u8, x_host.len() * 2) },
        &x,
        0,
    )
    .unwrap();
    let alloc = |n: usize| dev.alloc(n * 2, MemKind::Device, Pool::Activations).unwrap();
    let (gate, up, act, out) = (
        alloc(inter),
        alloc(inter),
        alloc(inter),
        alloc(oracle.seqlen * out_dim),
    );

    for token in 0..oracle.seqlen {
        let x_off = token * dim * 2;
        kernels
            .gemv_nvfp4_gguf_f16_at(&gate, 0, &w1, &x, x_off, inter, dim, s1, &stream)
            .unwrap();
        kernels
            .gemv_nvfp4_gguf_f16_at(&up, 0, &w3, &x, x_off, inter, dim, s3, &stream)
            .unwrap();
        kernels
            .swiglu_limit_f16(&act, &gate, &up, inter, 10.0, &stream)
            .unwrap();
        kernels
            .gemv_nvfp4_gguf_f16_at(
                &out,
                token * out_dim * 2,
                &w2,
                &act,
                0,
                out_dim,
                inter,
                s2,
                &stream,
            )
            .unwrap();
    }
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; oracle.seqlen * out_dim * 2];
    dev.read(&out, 0, &mut bytes).unwrap();
    let got: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();

    let num: f64 = got
        .iter()
        .zip(&oracle.expert_out)
        .map(|(g, w)| ((g - w) as f64).powi(2))
        .sum();
    let den: f64 = oracle.expert_out.iter().map(|w| (*w as f64).powi(2)).sum();
    let rel = (num / den.max(f64::MIN_POSITIVE)).sqrt();
    eprintln!("ekspert na GPU: względne L2 = {rel:.3e}");
    assert!(rel < 5e-2, "ekspert rozjeżdża się o {rel:.3e}");
    assert_eq!(
        got.iter().filter(|v| !v.is_finite()).count(),
        0,
        "wyjście zawiera wartości nieskończone lub NaN"
    );
}
