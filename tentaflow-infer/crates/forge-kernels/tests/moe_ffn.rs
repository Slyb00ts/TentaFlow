// ===== File: moe_ffn.rs — the mixture block, against the formula =====
//
// `Op::MoeFfn` is one operation covering five: the router multiply, the softmax
// over all experts, the top-k selection with its renormalization, the SwiGLU of
// each chosen expert over its own window of the stack, and the weighted sum of
// their outputs.
//
// That is a lot to get subtly wrong, and none of it fails loudly. Route to the
// wrong expert and the answer is another expert's opinion, fluently expressed.
// Select before the softmax instead of after and the weights are wrong while
// the choice is right. Fold the leading stack dimension the wrong way and every
// expert reads its neighbour's rows.
//
// So the gate is hermetic and triple: the CUDA executor, the host reference and
// an f32 computation written out here, on dimensions small enough to hold in
// one's head. Comparing the two executors alone would only prove they agree.

use std::sync::Arc;

use forge_formats::dequantize_to_f32;
use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, Shared, Step, WeightId,
    WeightStore,
};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_types::{DType, DenseShape, QuantKind};

const HIDDEN: usize = 256;
const INTER: usize = 512;
const EXPERTS: usize = 4;
const TOP_K: usize = 2;
/// Deliberately NOT `INTER`. The always-on expert states its own width through
/// its stacks, and the shape's `inter` only bounds the scratch — a path reading
/// the width from the shape would address twice every row here. Still a
/// multiple of 256, because that is the block a Q6_K row is read in.
const SH_INTER: usize = 256;

/// Gate and up are Q4_K, down is Q6_K — the exact pairing Qwen3-MoE ships and
/// the only one the device-indexed kernels cover.
const GATE_QUANT: QuantKind = QuantKind::Q4K;
const DOWN_QUANT: QuantKind = QuantKind::Q6K;

fn shape() -> DenseShape {
    DenseShape {
        hidden: HIDDEN as u32,
        layers: 1,
        heads: 2,
        kv_heads: 1,
        head_dim: (HIDDEN / 2) as u32,
        inter: INTER as u32,
        vocab: HIDDEN as u32,
        eps: 1e-5,
        rope_theta: 10_000.0,
        rope_rot: (HIDDEN / 2) as u32,
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

fn noise(seed: u64) -> impl FnMut() -> u8 {
    let mut state = seed | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    }
}

/// Blocks of a quantized stack, masked the way `format_table.rs` masks them so
/// every layout's scale field stays finite.
fn blocks(quant: QuantKind, rows: usize, cols: usize, seed: u64) -> Vec<u8> {
    let mut next = noise(seed);
    let bytes = rows * cols / quant.block_elems() * quant.block_bytes();
    (0..bytes).map(|_| next() & 0x3f).collect()
}

fn f16_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
        .collect()
}

fn seen(values: &[f32]) -> Vec<f32> {
    values
        .iter()
        .map(|&v| half::f16::from_f32(v).to_f32())
        .collect()
}

fn packed(codes: Vec<u8>, quant: QuantKind, dtype: DType, rows: usize, cols: usize) -> QuantWeight {
    QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes,
            scales: None,
            global: None,
        },
        quant,
        layout: Layout::Blocks,
        dtype,
        rows,
        cols,
    })
}

struct Fixture {
    router_bytes: Vec<u8>,
    gate: Vec<u8>,
    up: Vec<u8>,
    down: Vec<u8>,
    sh_gate: Vec<u8>,
    sh_up: Vec<u8>,
    sh_down: Vec<u8>,
    sh_router_bytes: Vec<u8>,
    table_bytes: Vec<u8>,
    /// What the executors see after f16 rounding.
    table: Vec<f32>,
    router: Vec<f32>,
    sh_router: Vec<f32>,
}

fn fixture() -> Fixture {
    let mut next = noise(0x2545_F491_4F6C_DD1D);
    // Small on purpose. Three multiplies compound — gate and up are linear in
    // x, the SwiGLU is quadratic in it, the down projection makes it cubic — so
    // an activation of the usual size drives the block's output past what f16
    // holds and the device path returns infinities. That is the magnitude of
    // the fixture, not of the executor, and `expected` asserts the range so it
    // cannot drift back.
    let table: Vec<f32> = (0..HIDDEN * HIDDEN)
        .map(|_| (next() as f32 / 255.0 - 0.5) * 0.002)
        .collect();
    // Router logits well separated on purpose. The executors do not agree on
    // its precision — the kernel wants f16 and the reference keeps f32 — so a
    // near-tie would flip WHICH expert computes, and this test is about the
    // block rather than about that. The flip itself is asserted against on the
    // real checkpoint, where the logits are what they are.
    let router: Vec<f32> = (0..EXPERTS * HIDDEN)
        .map(|i| {
            let bias = (i / HIDDEN) as f32 * 0.35;
            (next() as f32 / 255.0 - 0.5) * 0.2 + bias
        })
        .collect();
    // The shared expert's gate is ONE row and its logit is a per-token number,
    // so it is scaled to land the sigmoid decisively between 0 and 1. A gate
    // saturated either way would let a path that ignores it pass.
    let sh_router: Vec<f32> = (0..HIDDEN)
        .map(|_| (next() as f32 / 255.0 - 0.35) * 150.0)
        .collect();
    Fixture {
        router_bytes: f16_bytes(&router),
        gate: blocks(GATE_QUANT, EXPERTS * INTER, HIDDEN, 0x1111),
        up: blocks(GATE_QUANT, EXPERTS * INTER, HIDDEN, 0x2222),
        down: blocks(DOWN_QUANT, EXPERTS * HIDDEN, INTER, 0x3333),
        sh_gate: blocks(GATE_QUANT, SH_INTER, HIDDEN, 0x4444),
        sh_up: blocks(GATE_QUANT, SH_INTER, HIDDEN, 0x5555),
        sh_down: blocks(DOWN_QUANT, HIDDEN, SH_INTER, 0x6666),
        sh_router_bytes: f16_bytes(&sh_router),
        table_bytes: f16_bytes(&table),
        table: seen(&table),
        router: seen(&router),
        sh_router: seen(&sh_router),
    }
}

/// The per-token gate of the shared expert, as the formula states it.
fn shared_gate(f: &Fixture, x: &[f32]) -> f32 {
    let logit: f32 = f.sh_router.iter().zip(x).map(|(w, v)| w * v).sum();
    1.0 / (1.0 + (-logit).exp())
}

/// The formula, spelled out over the decoded stacks.
///
/// `shared` chooses whether the always-on expert contributes, and `gated`
/// whether its per-token sigmoid is applied — the second is the trap: dropping
/// it means weight 1.0, which is a different model that still answers.
fn expected(f: &Fixture, x: &[f32], shared: bool, gated: bool) -> Vec<f32> {
    let decode = |bytes: &[u8], quant: QuantKind, rows: usize, cols: usize| {
        dequantize_to_f32(DType::U8, quant, bytes, rows * cols).expect("dekoder wzorca")
    };
    let gate = decode(&f.gate, GATE_QUANT, EXPERTS * INTER, HIDDEN);
    let up = decode(&f.up, GATE_QUANT, EXPERTS * INTER, HIDDEN);
    let down = decode(&f.down, DOWN_QUANT, EXPERTS * HIDDEN, INTER);

    let logits: Vec<f32> = (0..EXPERTS)
        .map(|e| {
            f.router[e * HIDDEN..(e + 1) * HIDDEN]
                .iter()
                .zip(x)
                .map(|(w, v)| w * v)
                .sum()
        })
        .collect();
    let peak = logits.iter().copied().fold(f32::MIN, f32::max);
    let mut probs: Vec<f32> = logits.iter().map(|l| (l - peak).exp()).collect();
    let total: f32 = probs.iter().sum();
    for p in probs.iter_mut() {
        *p /= total;
    }
    let mut order: Vec<usize> = (0..EXPERTS).collect();
    order.sort_by(|a, b| probs[*b].partial_cmp(&probs[*a]).unwrap().then(a.cmp(b)));
    let chosen = &order[..TOP_K];
    let renorm = 1.0 / chosen.iter().map(|e| probs[*e]).sum::<f32>();

    let mut acc = vec![0.0f32; HIDDEN];
    for &e in chosen {
        let mut activated = vec![0.0f32; INTER];
        for (i, a) in activated.iter_mut().enumerate() {
            let r = (e * INTER + i) * HIDDEN;
            let g: f32 = gate[r..r + HIDDEN].iter().zip(x).map(|(w, v)| w * v).sum();
            let u: f32 = up[r..r + HIDDEN].iter().zip(x).map(|(w, v)| w * v).sum();
            *a = g / (1.0 + (-g).exp()) * u;
        }
        let weight = probs[e] * renorm;
        for (h, out) in acc.iter_mut().enumerate() {
            let r = (e * HIDDEN + h) * INTER;
            let v: f32 = down[r..r + INTER]
                .iter()
                .zip(&activated)
                .map(|(w, z)| w * z)
                .sum();
            *out += weight * v;
        }
    }
    if !shared {
        return acc;
    }
    // ON TOP of the routed sum, over its OWN width, with a per-token gate
    // rather than a routing weight.
    let sh_gate = decode(&f.sh_gate, GATE_QUANT, SH_INTER, HIDDEN);
    let sh_up = decode(&f.sh_up, GATE_QUANT, SH_INTER, HIDDEN);
    let sh_down = decode(&f.sh_down, DOWN_QUANT, HIDDEN, SH_INTER);
    let mut activated = vec![0.0f32; SH_INTER];
    for (i, a) in activated.iter_mut().enumerate() {
        let r = i * HIDDEN;
        let g: f32 = sh_gate[r..r + HIDDEN].iter().zip(x).map(|(w, v)| w * v).sum();
        let u: f32 = sh_up[r..r + HIDDEN].iter().zip(x).map(|(w, v)| w * v).sum();
        *a = g / (1.0 + (-g).exp()) * u;
    }
    let weight = if gated { shared_gate(f, x) } else { 1.0 };
    for (h, out) in acc.iter_mut().enumerate() {
        let r = h * SH_INTER;
        let v: f32 = sh_down[r..r + SH_INTER]
            .iter()
            .zip(&activated)
            .map(|(w, z)| w * z)
            .sum();
        *out += weight * v;
    }
    acc
}

fn run<E: Executor + WeightStore>(exec: &mut E, f: &Fixture, token: u32, shared: bool) -> Vec<f32> {
    let embed = exec
        .put_quant(packed(
            f.table_bytes.clone(),
            QuantKind::None,
            DType::F16,
            HIDDEN,
            HIDDEN,
        ))
        .expect("tablica");
    let router: WeightId = exec
        .put_quant(packed(
            f.router_bytes.clone(),
            QuantKind::None,
            DType::F16,
            EXPERTS,
            HIDDEN,
        ))
        .expect("router");
    let gate = exec
        .put_quant(packed(
            f.gate.clone(),
            GATE_QUANT,
            DType::U8,
            EXPERTS * INTER,
            HIDDEN,
        ))
        .expect("gate");
    let up = exec
        .put_quant(packed(
            f.up.clone(),
            GATE_QUANT,
            DType::U8,
            EXPERTS * INTER,
            HIDDEN,
        ))
        .expect("up");
    let down = exec
        .put_quant(packed(
            f.down.clone(),
            DOWN_QUANT,
            DType::U8,
            EXPERTS * HIDDEN,
            INTER,
        ))
        .expect("down");
    let shared = shared.then(|| Shared {
        gate: exec
            .put_quant(packed(f.sh_gate.clone(), GATE_QUANT, DType::U8, SH_INTER, HIDDEN))
            .expect("gate współdzielony"),
        up: exec
            .put_quant(packed(f.sh_up.clone(), GATE_QUANT, DType::U8, SH_INTER, HIDDEN))
            .expect("up współdzielony"),
        down: exec
            .put_quant(packed(f.sh_down.clone(), DOWN_QUANT, DType::U8, HIDDEN, SH_INTER))
            .expect("down współdzielony"),
        router: exec
            .put_quant(packed(
                f.sh_router_bytes.clone(),
                QuantKind::None,
                DType::F16,
                1,
                HIDDEN,
            ))
            .expect("bramka współdzielona"),
    });

    let step = Step::single(0, 0, 1).expect("krok");
    exec.run(&Op::Embed {
        table: embed,
        tokens: vec![token],
        step: step.clone(),
    })
    .expect("osadzenie");
    exec.run(&Op::MoeFfn {
        out: Act::Proj,
        x: Act::Hidden,
        router,
        gate,
        up,
        down,
        experts: EXPERTS as u32,
        top_k: TOP_K as u32,
        norm_topk: true,
        shared,
        step,
    })
    .expect("mieszanka");
    exec.read(Act::Proj, HIDDEN).expect("odczyt")
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
fn the_mixture_block_matches_the_formula_on_both_executors() {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    }
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 16 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            activations: 64 << 20,
        },
    )
    .expect("karta jest, a nie oddała pul");

    let f = fixture();
    let token = 5u32;
    let x = &f.table[token as usize * HIDDEN..(token as usize + 1) * HIDDEN];
    let want = expected(&f, x, true, true);
    let peak = want.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(
        peak > 1e-3,
        "wzorzec dał same zera — ten test by nic nie sprawdził"
    );
    // f16 stops at 65504, and the device path carries this slot in f16.
    assert!(
        peak < 1e4,
        "wzorzec sięga {peak}, czyli poza zakres f16 — mierzyłoby się przepełnienie fikstury"
    );

    // Three things this fixture must be able to TELL APART, checked before
    // anything computes. Without them the comparison below would pass for a
    // path that skipped the shared expert, or applied it ungated — both of
    // which answer fluently.
    let routed_only = expected(&f, x, false, true);
    let ungated = expected(&f, x, true, false);
    let gate = shared_gate(&f, x);
    assert!(
        (0.15..0.85).contains(&gate),
        "bramka {gate} jest nasycona, więc jej pominięcie byłoby nierozróżnialne"
    );
    assert!(
        spread_error(&routed_only, &want) > 0.2,
        "ekspert współdzielony nie zmienia wyniku tej fikstury"
    );
    assert!(
        spread_error(&ungated, &want) > 0.2,
        "bramka nie zmienia wyniku tej fikstury"
    );

    let mut host = HostExec::new(spec()).expect("wzorzec");
    let host_out = run(&mut host, &f, token, true);
    let host_err = spread_error(&host_out, &want);

    let mut cuda = CudaExec::new(device as Arc<_>, spec()).expect("wykonawca CUDA");
    let cuda_out = run(&mut cuda, &f, token, true);
    let cuda_err = spread_error(&cuda_out, &want);

    eprintln!(
        "wzorzec {:.3}%, CUDA {:.3}% (bramka {gate:.3})",
        host_err * 100.0,
        cuda_err * 100.0
    );
    assert!(host_err < 0.01, "wzorzec: {:.3}%", host_err * 100.0);
    assert!(cuda_err < 0.03, "CUDA: {:.3}%", cuda_err * 100.0);

    // And the mixture WITHOUT a shared expert still computes — the same op,
    // the same weights, the branch a checkpoint like Qwen3-30B-A3B takes.
    let bare = run(&mut host, &f, token, false);
    assert!(
        spread_error(&bare, &routed_only) < 0.01,
        "mieszanka bez eksperta współdzielonego: {:.3}%",
        spread_error(&bare, &routed_only) * 100.0
    );
}
