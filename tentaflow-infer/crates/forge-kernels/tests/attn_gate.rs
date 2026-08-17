// ===== File: attn_gate.rs — the two operations Qwen3.6 attention adds =====
//
// A partial rotary and an output gate. Neither fails loudly: a rotary that
// turns the wrong dimensions at plausible angles answers about another
// position, and a gate applied to the wrong buffer scales the question instead
// of the answer. Both read as fluent text.
//
// The trick that makes this hermetic is that neither test builds a reference
// input. Each reads the slot the executor itself filled, applies the formula to
// THAT, and compares against what the operation left behind — so the two
// executors are held to one written-out definition without either of them
// having to agree with the other about how a matrix multiplies.

use std::sync::Arc;

use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, Step, WeightStore,
};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_types::{DType, DenseShape, QuantKind};

const HIDDEN: usize = 256;
const HEADS: usize = 4;
const HEAD_DIM: usize = 64;
/// A quarter of every head turns, which is the shape of the thing: Qwen3.6
/// rotates 64 of 256. Equal to `HEAD_DIM` this file would prove nothing that
/// the existing rotary tests do not.
const ROT: usize = 16;
const WIDTH: usize = HEADS * HEAD_DIM;
const POS: u32 = 37;

fn shape() -> DenseShape {
    DenseShape {
        hidden: HIDDEN as u32,
        layers: 1,
        heads: HEADS as u32,
        kv_heads: 1,
        head_dim: HEAD_DIM as u32,
        inter: WIDTH as u32,
        vocab: HIDDEN as u32,
        eps: 1e-5,
        rope_theta: 10_000.0,
        rope_rot: ROT as u32,
    }
}

fn spec() -> ExecSpec {
    ExecSpec {
        shape: shape(),
        attends: vec![true].into(),
        quant_params: DType::F16,
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

fn matrix(rows: usize, cols: usize, seed: u64) -> Vec<u8> {
    let mut next = noise(seed);
    f16_bytes(&(0..rows * cols).map(|_| next()).collect::<Vec<_>>())
}

fn put<E: WeightStore>(
    exec: &mut E,
    codes: Vec<u8>,
    rows: usize,
    cols: usize,
) -> forge_graph::WeightId {
    exec.put_quant(QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes,
            scales: None,
            global: None,
        },
        quant: QuantKind::None,
        layout: Layout::Blocks,
        dtype: DType::F16,
        rows,
        cols,
    }))
    .expect("waga")
}

fn device() -> Option<Arc<CudaDevice>> {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return None;
    }
    Some(
        CudaDevice::new(
            0,
            PoolSizes {
                weights: 16 << 20,
                kv_cache: 16 << 20,
                kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
                activations: 64 << 20,
            },
        )
        .expect("karta jest, a nie oddała pul"),
    )
}

/// Relative to the spread, so f16 rounding is measured against the size of the
/// numbers rather than against zero.
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

/// The partial rotary, written out: dimension `j` pairs with `j + rot/2` at
/// `theta^(-2j/rot)`, and everything from `rot` up is left exactly alone.
fn rotated(before: &[f32]) -> Vec<f32> {
    let mut out = before.to_vec();
    let half = ROT / 2;
    for h in 0..HEADS {
        let base = h * HEAD_DIM;
        for j in 0..half {
            let freq = POS as f32 * 10_000f32.powf(-2.0 * j as f32 / ROT as f32);
            let (sin, cos) = freq.sin_cos();
            let (a, b) = (before[base + j], before[base + half + j]);
            out[base + j] = a * cos - b * sin;
            out[base + half + j] = a * sin + b * cos;
        }
    }
    out
}

fn run_rope<E: Executor + WeightStore>(exec: &mut E) -> (Vec<f32>, Vec<f32>) {
    let table = put(exec, matrix(HIDDEN, HIDDEN, 0x1234), HIDDEN, HIDDEN);
    let w = put(exec, matrix(WIDTH, HIDDEN, 0x5678), WIDTH, HIDDEN);
    let step = Step::single(0, POS, 1).expect("krok");
    exec.run(&Op::Embed {
        table,
        tokens: vec![7],
        step: step.clone(),
    })
    .expect("osadzenie");
    exec.run(&Op::MatMul {
        out: Act::Query,
        w,
        x: Act::Hidden,
        step: step.clone(),
    })
    .expect("projekcja");
    let before = exec.read(Act::Query, WIDTH).expect("odczyt przed");
    exec.run(&Op::Rope {
        act: Act::Query,
        heads: HEADS as u32,
        step,
    })
    .expect("obrót");
    let after = exec.read(Act::Query, WIDTH).expect("odczyt po");
    (before, after)
}

#[test]
#[ignore = "wymaga karty NVIDIA"]
fn a_partial_rotary_turns_only_its_own_dimensions() {
    let Some(device) = device() else { return };

    for (who, (before, after)) in [
        (
            "wzorzec",
            run_rope(&mut HostExec::new(spec()).expect("wzorzec")),
        ),
        (
            "CUDA",
            run_rope(&mut CudaExec::new(device as Arc<_>, spec()).expect("wykonawca CUDA")),
        ),
    ] {
        // The projection has to have produced something, or every assertion
        // below would hold on zeros.
        let peak = before.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 1e-3, "{who}: projekcja dała same zera");

        let want = rotated(&before);
        let err = spread_error(&after, &want);
        eprintln!("{who}: {:.3}% rozpiętości", err * 100.0);
        assert!(err < 0.02, "{who}: {:.3}% rozpiętości", err * 100.0);

        // The dimensions past `rot` must be untouched EXACTLY, not nearly: a
        // full rotary would move them, and a rounding cannot.
        for h in 0..HEADS {
            for j in ROT..HEAD_DIM {
                let i = h * HEAD_DIM + j;
                assert_eq!(
                    after[i], before[i],
                    "{who}: wymiar {j} głowicy {h} obrócony"
                );
            }
        }
        // And the ones below it must have MOVED, or the test would pass for a
        // rotary that did nothing at all.
        let turned = (0..HEADS)
            .flat_map(|h| (0..ROT).map(move |j| h * HEAD_DIM + j))
            .filter(|&i| (after[i] - before[i]).abs() > 1e-3)
            .count();
        assert!(
            turned > HEADS * ROT / 2,
            "{who}: obrócono {turned} wymiarów"
        );
    }
}

fn gate_body<E: Executor + WeightStore>(exec: &mut E) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let table = put(exec, matrix(HIDDEN, HIDDEN, 0x1234), HIDDEN, HIDDEN);
    let wa = put(exec, matrix(WIDTH, HIDDEN, 0x9abc), WIDTH, HIDDEN);
    // Scaled so the sigmoid spans a wide band while SATURATING NOWHERE.
    // Either extreme would let a step function pass for a sigmoid, and a
    // constant would pass for a gate that varies.
    let wg: Vec<u8> = {
        let mut next = noise(0xdef0);
        f16_bytes(
            &(0..WIDTH * HIDDEN)
                .map(|_| next() * 0.2)
                .collect::<Vec<_>>(),
        )
    };
    let wg = put(exec, wg, WIDTH, HIDDEN);
    let step = Step::single(0, 0, 1).expect("krok");
    exec.run(&Op::Embed {
        table,
        tokens: vec![11],
        step: step.clone(),
    })
    .expect("osadzenie");
    for (out, w) in [(Act::Attn, wa), (Act::AttnGate, wg)] {
        exec.run(&Op::MatMul {
            out,
            w,
            x: Act::Hidden,
            step: step.clone(),
        })
        .expect("projekcja");
    }
    let attn = exec.read(Act::Attn, WIDTH).expect("odczyt uwagi");
    let gate = exec.read(Act::AttnGate, WIDTH).expect("odczyt bramki");
    exec.run(&Op::SigmoidMul {
        act: Act::Attn,
        gate: Act::AttnGate,
        step,
    })
    .expect("bramka");
    let out = exec.read(Act::Attn, WIDTH).expect("odczyt wyjścia");
    (attn, gate, out)
}

#[test]
#[ignore = "wymaga karty NVIDIA"]
fn the_output_gate_scales_the_answer_by_its_sigmoid() {
    let Some(device) = device() else { return };

    let mut host = HostExec::new(spec()).expect("wzorzec");
    let mut cuda = CudaExec::new(device as Arc<_>, spec()).expect("wykonawca CUDA");
    let runs = [
        ("wzorzec", gate_body(&mut host)),
        ("CUDA", gate_body(&mut cuda)),
    ];
    for (who, (attn, gate, out)) in runs {
        let want: Vec<f32> = attn
            .iter()
            .zip(&gate)
            .map(|(a, g)| a / (1.0 + (-g).exp()))
            .collect();
        let err = spread_error(&out, &want);
        // The gate has to actually vary, or `out == attn` would also pass.
        let (lo, hi) = gate
            .iter()
            .map(|g| 1.0 / (1.0 + (-g).exp()))
            .fold((f32::MAX, f32::MIN), |(lo, hi), v: f32| {
                (lo.min(v), hi.max(v))
            });
        eprintln!(
            "{who}: {:.3}% rozpiętości, bramka {lo:.3}..{hi:.3}",
            err * 100.0
        );
        assert!(hi - lo > 0.3, "{who}: bramka nie zmienia się przez wiersz");
        assert!(
            lo > 0.02 && hi < 0.98,
            "{who}: bramka nasyca się w {lo:.3}..{hi:.3} — funkcja schodkowa \
             przeszłaby ten test"
        );
        assert!(
            spread_error(&attn, &want) > 0.1,
            "{who}: bramka niczego nie zmieniła"
        );
        assert!(err < 0.02, "{who}: {:.3}% rozpiętości", err * 100.0);
    }
}
