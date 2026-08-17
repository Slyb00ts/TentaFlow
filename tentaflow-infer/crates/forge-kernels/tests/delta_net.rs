// ===== File: delta_net.rs — the recurrent mixer, against the formula =====
//
// `Op::DeltaNet` is one operation covering nine: four projections, a causal
// convolution with SiLU, a per-head L2 normalization, the repeat that lets four
// value heads read two key heads, the per-head decay and write gate, the rank-1
// fold into a state matrix, a gated normalization and the output projection.
//
// None of it fails loudly, and two of its mistakes are invisible for one token
// and only wrong from the second one on: a convolution window that does not
// advance, and a state matrix that is rebuilt instead of carried. So this file
// runs a SEQUENCE and checks three separate things — that the arithmetic
// matches a formula written out here, that the second token's answer depends on
// the first, and that a lane restarting at position zero forgets.
//
// The reference is spelled out longhand rather than calling
// `forge-formats::deltanet`, which is what the host executor composes. Those
// primitives have their own tests; what is untested until here is the ORDER
// they go in, and a reference sharing the composition could not see it.

use std::sync::Arc;

use forge_graph::{
    Act, DeltaWeights, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, SsmShape,
    Step, WeightId, WeightStore,
};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_types::{DType, DenseShape, QuantKind};

const HIDDEN: usize = 64;
const K_HEADS: usize = 2;
const V_HEADS: usize = 4;
const D_STATE: usize = 32;
const D_CONV: usize = 4;
const KEY: usize = K_HEADS * D_STATE;
const VALUE: usize = V_HEADS * D_STATE;
const MIXED: usize = 2 * KEY + VALUE;
const EPS: f32 = 1e-5;

fn ssm() -> SsmShape {
    SsmShape {
        d_conv: D_CONV as u32,
        d_state: D_STATE as u32,
        k_heads: K_HEADS as u32,
        v_heads: V_HEADS as u32,
    }
}

fn spec() -> ExecSpec {
    ExecSpec {
        shape: DenseShape {
            hidden: HIDDEN as u32,
            layers: 1,
            heads: 2,
            kv_heads: 1,
            head_dim: (HIDDEN / 2) as u32,
            inter: HIDDEN as u32,
            vocab: HIDDEN as u32,
            eps: EPS,
            rope_theta: 10_000.0,
            rope_rot: (HIDDEN / 2) as u32,
        },
        ssm: Some(ssm()),
        attends: vec![true].into(),
        quant_params: DType::F16,
        norm_weights: DType::F32,
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

fn draw(n: usize, seed: u64, scale: f32) -> Vec<f32> {
    let mut next = noise(seed);
    (0..n).map(|_| next() * scale).collect()
}

/// What the executors see after the f16 rounding both of them apply on upload.
fn seen(values: &[f32]) -> Vec<f32> {
    values
        .iter()
        .map(|&v| half::f16::from_f32(v).to_f32())
        .collect()
}

fn f16_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
        .collect()
}

struct Fixture {
    table: Vec<f32>,
    qkv: Vec<f32>,
    gate: Vec<f32>,
    conv: Vec<f32>,
    alpha: Vec<f32>,
    beta: Vec<f32>,
    dt_bias: Vec<f32>,
    /// `a = -exp(A_log)`, so the decay is a decay.
    a: Vec<f32>,
    norm: Vec<f32>,
    out: Vec<f32>,
}

fn fixture() -> Fixture {
    Fixture {
        // Small, because the fold compounds: every token multiplies the state
        // by a decay and adds a rank-one term, and an activation of the usual
        // size drives it past f16 within a few steps.
        table: seen(&draw(HIDDEN * HIDDEN, 0x1111, 0.05)),
        qkv: seen(&draw(MIXED * HIDDEN, 0x2222, 0.1)),
        gate: seen(&draw(VALUE * HIDDEN, 0x3333, 0.1)),
        conv: seen(&draw(MIXED * D_CONV, 0x4444, 0.5)),
        alpha: seen(&draw(V_HEADS * HIDDEN, 0x5555, 0.1)),
        beta: seen(&draw(V_HEADS * HIDDEN, 0x6666, 0.1)),
        dt_bias: seen(&draw(V_HEADS, 0x7777, 1.0)),
        a: seen(
            &draw(V_HEADS, 0x8888, 1.0)
                .iter()
                .map(|v| -(v.abs().exp()))
                .collect::<Vec<_>>(),
        ),
        norm: seen(&draw(D_STATE, 0x9999, 1.0)),
        out: seen(&draw(HIDDEN * VALUE, 0xaaaa, 0.1)),
    }
}

/// The state one sequence carries between tokens.
struct Carried {
    window: Vec<f32>,
    matrix: Vec<f32>,
}

impl Carried {
    fn new() -> Self {
        Self {
            window: vec![0.0; MIXED * (D_CONV - 1)],
            matrix: vec![0.0; V_HEADS * D_STATE * D_STATE],
        }
    }
}

fn row_times(m: &[f32], rows: usize, x: &[f32]) -> Vec<f32> {
    (0..rows)
        .map(|r| {
            m[r * x.len()..(r + 1) * x.len()]
                .iter()
                .zip(x)
                .map(|(a, b)| a * b)
                .sum()
        })
        .collect()
}

/// One token of the whole layer, written out.
fn expected(f: &Fixture, carried: &mut Carried, token: u32) -> Vec<f32> {
    let x = &f.table[token as usize * HIDDEN..(token as usize + 1) * HIDDEN];
    let mixed = row_times(&f.qkv, MIXED, x);
    let z = row_times(&f.gate, VALUE, x);
    let alpha = row_times(&f.alpha, V_HEADS, x);
    let beta_raw = row_times(&f.beta, V_HEADS, x);

    // Causal convolution over each channel's own window, newest tap last, then
    // SiLU. The window advances afterwards.
    let win = D_CONV - 1;
    let mut conv = vec![0.0f32; MIXED];
    for c in 0..MIXED {
        let taps = &f.conv[c * D_CONV..(c + 1) * D_CONV];
        let hist = &carried.window[c * win..(c + 1) * win];
        let mut acc = taps[win] * mixed[c];
        for j in 0..win {
            acc += taps[j] * hist[j];
        }
        conv[c] = acc / (1.0 + (-acc).exp());
    }
    for c in 0..MIXED {
        let hist = &mut carried.window[c * win..(c + 1) * win];
        for j in 0..win - 1 {
            hist[j] = hist[j + 1];
        }
        hist[win - 1] = mixed[c];
    }

    // Query, key and value lie end to end. q and k are normalized per KEY head
    // and value head h then reads key head h % k_heads.
    let mut q = conv[..KEY].to_vec();
    let mut k = conv[KEY..2 * KEY].to_vec();
    let v = &conv[2 * KEY..];
    for head in 0..K_HEADS {
        for part in [&mut q, &mut k] {
            let span = &mut part[head * D_STATE..(head + 1) * D_STATE];
            let inv = 1.0 / (span.iter().map(|a| a * a).sum::<f32>() + EPS).sqrt();
            for e in span.iter_mut() {
                *e *= inv;
            }
        }
    }

    let mut answer = vec![0.0f32; VALUE];
    for head in 0..V_HEADS {
        let from = (head % K_HEADS) * D_STATE;
        let softplus = {
            let t = alpha[head] + f.dt_bias[head];
            if t > 20.0 {
                t
            } else {
                (1.0 + t.exp()).ln()
            }
        };
        let decay = (softplus * f.a[head]).exp();
        let gate = 1.0 / (1.0 + (-beta_raw[head]).exp());
        let s = &mut carried.matrix[head * D_STATE * D_STATE..][..D_STATE * D_STATE];
        for e in s.iter_mut() {
            *e *= decay;
        }
        let mut predicted = vec![0.0f32; D_STATE];
        for i in 0..D_STATE {
            for j in 0..D_STATE {
                predicted[j] += k[from + i] * s[i * D_STATE + j];
            }
        }
        let delta: Vec<f32> = (0..D_STATE)
            .map(|j| gate * (v[head * D_STATE + j] - predicted[j]))
            .collect();
        for i in 0..D_STATE {
            for j in 0..D_STATE {
                s[i * D_STATE + j] += k[from + i] * delta[j];
            }
        }
        let inv_sqrt = 1.0 / (D_STATE as f32).sqrt();
        for i in 0..D_STATE {
            let qi = q[from + i] * inv_sqrt;
            for j in 0..D_STATE {
                answer[head * D_STATE + j] += qi * s[i * D_STATE + j];
            }
        }
    }

    // Gated normalization per value head, then out.
    let mut normed = vec![0.0f32; VALUE];
    for head in 0..V_HEADS {
        let span = head * D_STATE..(head + 1) * D_STATE;
        let o = &answer[span.clone()];
        let ss = o.iter().map(|a| a * a).sum::<f32>() / D_STATE as f32;
        let inv = 1.0 / (ss + EPS).sqrt();
        for j in 0..D_STATE {
            let zj = z[span.start + j];
            normed[span.start + j] = o[j] * inv * f.norm[j] * (zj / (1.0 + (-zj).exp()));
        }
    }
    row_times(&f.out, HIDDEN, &normed)
}

fn put<E: WeightStore>(exec: &mut E, values: &[f32], rows: usize, cols: usize) -> WeightId {
    exec.put_quant(QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes: f16_bytes(values),
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

/// f32 bytes, because that is what `norm_weights` in the spec declares the
/// source keeps them in — the executors narrow on upload, and handing them
/// halves would have them read pairs of them as single numbers.
fn plain<E: WeightStore>(exec: &mut E, values: &[f32]) -> WeightId {
    exec.put_plain(values.iter().flat_map(|v| v.to_le_bytes()).collect())
        .expect("waga zwykła")
}

/// Runs `tokens` through the layer, one step each, and returns every answer.
fn run<E: Executor + WeightStore>(
    exec: &mut E,
    f: &Fixture,
    tokens: &[u32],
    from: u32,
) -> Vec<Vec<f32>> {
    let table = put(exec, &f.table, HIDDEN, HIDDEN);
    let w = DeltaWeights {
        qkv: put(exec, &f.qkv, MIXED, HIDDEN),
        gate: put(exec, &f.gate, VALUE, HIDDEN),
        conv: plain(exec, &f.conv),
        alpha: put(exec, &f.alpha, V_HEADS, HIDDEN),
        beta: put(exec, &f.beta, V_HEADS, HIDDEN),
        dt_bias: plain(exec, &f.dt_bias),
        a: plain(exec, &f.a),
        norm: plain(exec, &f.norm),
        out: put(exec, &f.out, HIDDEN, VALUE),
    };
    tokens
        .iter()
        .enumerate()
        .map(|(i, token)| {
            let step = Step::single(0, from + i as u32, 1).expect("krok");
            exec.run(&Op::Embed {
                table,
                tokens: vec![*token],
                step: step.clone(),
            })
            .expect("osadzenie");
            exec.run(&Op::DeltaNet {
                out: Act::Proj,
                x: Act::Hidden,
                layer: 0,
                w,
                step,
            })
            .expect("mikser rekurencyjny");
            exec.read(Act::Proj, HIDDEN).expect("odczyt")
        })
        .collect()
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

const PROMPT: [u32; 4] = [3, 11, 7, 3];

#[test]
#[ignore = "wymaga karty NVIDIA"]
fn the_recurrent_mixer_matches_the_formula_and_carries_its_state() {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    }
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 32 << 20,
            kv_cache: 16 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
            activations: 64 << 20,
        },
    )
    .expect("karta jest, a nie oddała pul");

    let f = fixture();
    let mut carried = Carried::new();
    let want: Vec<Vec<f32>> = PROMPT
        .iter()
        .map(|t| expected(&f, &mut carried, *t))
        .collect();

    let peak = want[3].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 1e-4, "wzorzec dał same zera");
    assert!(peak < 1e4, "wzorzec sięga {peak}, poza zakres f16");

    // The FIRST and the LAST token are the same id. Their answers must differ,
    // or nothing below could tell a carried state from a recomputed one.
    let moved = spread_error(&want[0], &want[3]);
    assert!(
        moved > 0.2,
        "ten sam token dał ten sam wynik ({:.3}%) — fikstura nie niesie stanu",
        moved * 100.0
    );

    for (who, got) in [
        (
            "wzorzec",
            run(&mut HostExec::new(spec()).expect("wzorzec"), &f, &PROMPT, 0),
        ),
        (
            "CUDA",
            run(
                &mut CudaExec::new(device.clone() as Arc<_>, spec()).expect("wykonawca CUDA"),
                &f,
                &PROMPT,
                0,
            ),
        ),
    ] {
        for (i, (got, want)) in got.iter().zip(&want).enumerate() {
            let err = spread_error(got, want);
            eprintln!("{who} token {i}: {:.3}% rozpiętości", err * 100.0);
            assert!(err < 0.03, "{who} token {i}: {:.3}%", err * 100.0);
        }
    }

    // A lane starting at position zero forgets. Fed the same prompt again from
    // the front, the same executor must reproduce it exactly — a state left
    // half-full would be fluent and different.
    let mut exec = CudaExec::new(device as Arc<_>, spec()).expect("wykonawca CUDA");
    let first = run(&mut exec, &f, &PROMPT, 0);
    let again = run(&mut exec, &f, &PROMPT, 0);
    for (i, (a, b)) in first.iter().zip(&again).enumerate() {
        assert_eq!(a, b, "token {i} po restarcie od pozycji zero");
    }
}
