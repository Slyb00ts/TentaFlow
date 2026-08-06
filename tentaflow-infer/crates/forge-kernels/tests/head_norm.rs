// ===== File: head_norm.rs — the per-head norm, on both executors =====
//
// `Op::HeadNorm` is the normalization the Qwen3 family applies to Q and K
// before RoPE: RMS over `head_dim` with a learned weight, one row per HEAD
// rather than per token. Bielik does not have it and llama does not have it,
// so no checkpoint in this repository exercises it yet — which is the state
// this suite refuses to leave a table row in.
//
// The gate is therefore hermetic and doubled. The same operations run on the
// CUDA executor and on the host reference, and BOTH are held against an f32
// computation written out in the test. Comparing the two executors to each
// other would only prove they agree; comparing both to a third statement of
// the formula is what says they are right.
//
// The property that matters is the row split. A per-head norm computed over
// the whole activation instead of over each head produces numbers of the right
// magnitude in the right places — fluent, wrong text — and only the arithmetic
// says otherwise, which is why the widths here are chosen so the two readings
// differ.

use std::sync::Arc;

use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, Step, WeightId,
    WeightStore,
};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_types::{DType, DenseShape, QuantKind};

const HEADS: usize = 4;
const HEAD_DIM: usize = 64;
/// Query is `HEADS * HEAD_DIM` wide, and the residual stream is the same here
/// only to keep the embedding table square — the split under test is the one
/// inside Q.
const WIDTH: usize = HEADS * HEAD_DIM;
const EPS: f32 = 1e-5;

fn shape() -> DenseShape {
    DenseShape {
        hidden: WIDTH as u32,
        layers: 1,
        heads: HEADS as u32,
        kv_heads: 1,
        head_dim: HEAD_DIM as u32,
        inter: WIDTH as u32,
        vocab: WIDTH as u32,
        eps: EPS,
        rope_theta: 10_000.0,
        rope_rot: HEAD_DIM as u32,
    }
}

fn spec() -> ExecSpec {
    ExecSpec {
        shape: shape(),
        attends: vec![true].into(),
        quant_params: DType::F16,
        // GGUF keeps normalization weights in f32 while the kernels read f16,
        // so this is the conversion the real path takes.
        norm_weights: DType::F32,
        ssm: None,
    }
}

fn noise(seed: u64) -> impl FnMut() -> f32 {
    let mut state = seed | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 24) as u8 as f32 / 255.0 - 0.5) * 2.0
    }
}

fn f16_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
        .collect()
}

/// What the executors will actually see, after f16 rounding.
fn seen(values: &[f32]) -> Vec<f32> {
    values
        .iter()
        .map(|&v| half::f16::from_f32(v).to_f32())
        .collect()
}

fn plain_weight(values: &[f32]) -> QuantWeight {
    QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes: f16_bytes(values),
            scales: None,
            global: None,
        },
        quant: QuantKind::None,
        layout: Layout::Blocks,
        dtype: DType::F16,
        rows: WIDTH,
        cols: WIDTH,
    })
}

/// The formula, written out: RMS over each head's own `HEAD_DIM` values.
fn expected(q: &[f32], weight: &[f32]) -> Vec<f32> {
    let mut out = q.to_vec();
    for head in out.chunks_exact_mut(HEAD_DIM) {
        let mean = head.iter().map(|v| v * v).sum::<f32>() / HEAD_DIM as f32;
        let scale = 1.0 / (mean + EPS).sqrt();
        for (i, v) in head.iter_mut().enumerate() {
            *v = *v * scale * weight[i];
        }
    }
    out
}

/// Runs embed → project → head-norm and returns Q, on whichever executor.
fn run<E: Executor + WeightStore>(
    exec: &mut E,
    table: &[f32],
    proj: &[f32],
    norm: &[f32],
    token: u32,
) -> Vec<f32> {
    let embed = exec.put_quant(plain_weight(table)).expect("tablica");
    let w = exec.put_quant(plain_weight(proj)).expect("projekcja");
    let norm_id: WeightId = exec
        .put_plain(norm.iter().flat_map(|v| v.to_le_bytes()).collect())
        .expect("waga normy");

    let step = Step::single(0, 0, 1).expect("krok");
    exec.run(&Op::Embed {
        table: embed,
        tokens: vec![token],
        step: step.clone(),
    })
    .expect("osadzenie");
    exec.run(&Op::MatMul {
        out: Act::Query,
        w,
        x: Act::Hidden,
        step: step.clone(),
    })
    .expect("projekcja Q");
    let before = exec.read(Act::Query, WIDTH).expect("Q przed normą");
    exec.run(&Op::HeadNorm {
        act: Act::Query,
        w: norm_id,
        heads: HEADS as u32,
        step,
    })
    .expect("norma głowic");
    let after = exec.read(Act::Query, WIDTH).expect("Q po normie");
    assert_ne!(before, after, "norma nic nie zmieniła");
    after
}

fn spread_error(got: &[f32], want: &[f32]) -> f32 {
    let (lo, hi) = want
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let spread = (hi - lo).max(f32::MIN_POSITIVE);
    got.iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max)
        / spread
}

#[test]
#[ignore = "wymaga karty NVIDIA"]
fn the_per_head_norm_matches_the_formula_on_both_executors() {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    }
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 16 << 20,
            kv_cache: 16 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            activations: 16 << 20,
        },
    )
    .expect("karta jest, a nie oddała pul");

    let mut next = noise(0x5DEE_CE66_D1CE_4A11);
    let table: Vec<f32> = (0..WIDTH * WIDTH).map(|_| next()).collect();
    // Every head gets its OWN magnitude, and that is what makes this test
    // decide anything. With identically distributed heads each head's RMS
    // lands on the global one, so normalizing per head and normalizing over
    // the whole activation agree to within a few percent — and a test built on
    // such data passes just as happily for an executor that ignores `heads`.
    // Real heads do not share a scale either.
    let proj: Vec<f32> = (0..WIDTH * WIDTH)
        .map(|i| next() * 0.1 * (1 << (i / WIDTH / HEAD_DIM)) as f32)
        .collect();
    // Not all ones: a weight of ones would make a dropped multiply invisible.
    let norm: Vec<f32> = (0..HEAD_DIM).map(|_| 0.5 + next()).collect();

    let token = 3u32;
    let mut host = HostExec::new(spec()).expect("wzorzec");
    let host_q = run(&mut host, &table, &proj, &norm, token);

    let mut cuda = CudaExec::new(device as Arc<_>, spec()).expect("wykonawca CUDA");
    let cuda_q = run(&mut cuda, &table, &proj, &norm, token);

    // The reference is built from what the executors were given, rounded the
    // way they round it, so the comparison is about the norm and not about f16.
    let weight = seen(&norm);
    let x = seen(&table[token as usize * WIDTH..(token as usize + 1) * WIDTH]);
    let rounded_proj = seen(&proj);
    let projected: Vec<f32> = (0..WIDTH)
        .map(|r| {
            let row = &rounded_proj[r * WIDTH..(r + 1) * WIDTH];
            row.iter().zip(&x).map(|(w, v)| w * v).sum()
        })
        .collect();
    let want = expected(&projected, &weight);

    let host_err = spread_error(&host_q, &want);
    let cuda_err = spread_error(&cuda_q, &want);
    eprintln!("wzorzec {:.4}%, CUDA {:.4}%", host_err * 100.0, cuda_err * 100.0);
    assert!(host_err < 0.01, "wzorzec: {:.4}%", host_err * 100.0);
    assert!(cuda_err < 0.02, "CUDA: {:.4}%", cuda_err * 100.0);

    // The split under test: normalizing the whole activation instead of each
    // head separately must NOT produce this answer, or the test would pass for
    // an executor that ignored `heads`.
    let mean = projected.iter().map(|v| v * v).sum::<f32>() / WIDTH as f32;
    let flat: Vec<f32> = projected
        .iter()
        .enumerate()
        .map(|(i, v)| v / (mean + EPS).sqrt() * weight[i % HEAD_DIM])
        .collect();
    assert!(
        spread_error(&flat, &want) > 0.05,
        "norma po całej aktywacji daje to samo co po głowicach — test nie rozstrzyga"
    );
}
