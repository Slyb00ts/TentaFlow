// ===== File: format_table.rs — every row of the format table, against the CPU reference =====
//
// The executor's format table dispatches twenty-two quantizations, but until
// now four of them had ever been RUN: Q4_K and Q6_K through a Q4_K_M
// checkpoint, Q8_0 through its own, NVFP4 through a safetensors export. The
// other eighteen were wired to a kernel name and never asked to compute
// anything, which is the weakest possible state for this particular table —
// a row pointing at the wrong kernel produces numbers, not an error, and the
// two formats it confuses often have the same block size.
//
// Gating them on checkpoints does not scale: it would mean one download per
// format and a quantizer we do not ship. So the gate is hermetic instead. For
// each format the SAME BYTES are multiplied twice — once by the kernel the
// table selects, once by `forge_formats::dequantize_to_f32`, which is the CPU
// decoder for every one of these formats and shares no code with the Mojo
// side. Agreement is then evidence about the row rather than about the bytes.
//
// Everything goes through the public contract — `WeightStore::put_quant`,
// `Op`, `Executor::read` — so the test also holds the upload path, the block
// geometry check and the scalar handling, not just the kernel name.
//
// LIMITATION, stated rather than hidden: the code bytes are masked to six bits
// (see `blocks`). Every one of these layouts stores an f16 or f8 scale
// somewhere inside the block, and unrestricted random bytes put infinities and
// NaNs there for most of them, which makes a numeric comparison meaningless
// for reasons that have nothing to do with the row under test. Six bits keeps
// every scale field finite in every layout without needing to know where each
// layout puts it. The cost is that the top two bits of each code byte are
// never exercised here; the four formats with real checkpoints cover full-range
// codes in `forge-model/tests/cuda_vs_reference.rs`.

use std::sync::Arc;

use forge_formats::dequantize_to_f32;
use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, Step, WeightId,
    WeightStore,
};
use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::CudaExec;
use forge_types::{DType, DenseShape, QuantKind};

/// Square on purpose: the same weight can serve as a projection and as the
/// output head, so one upload gates two of the three table families.
const WIDTH: usize = 256;

/// Tokens in the batched pass. Above the vector form and outside the widths
/// 2/4/8/16 the Q4_K batch kernel accepts, so it is the tile that runs.
const TILE_TOKENS: u32 = 32;

/// Every format the table claims, including the two written out by hand and
/// the one that takes a tensor scalar.
///
/// The list is spelled out rather than derived, because deriving it from the
/// table would make the test agree with the table by construction — including
/// about a format the table forgot.
const FORMATS: &[QuantKind] = &[
    QuantKind::None,
    QuantKind::Q4_0,
    QuantKind::Q4_1,
    QuantKind::Q5_0,
    QuantKind::Q5_1,
    QuantKind::Q8_0,
    QuantKind::Q2K,
    QuantKind::Q3K,
    QuantKind::Q4K,
    QuantKind::Q5K,
    QuantKind::Q6K,
    QuantKind::IQ1S,
    QuantKind::IQ1M,
    QuantKind::IQ2XXS,
    QuantKind::IQ2XS,
    QuantKind::IQ2S,
    QuantKind::IQ3XXS,
    QuantKind::IQ3S,
    QuantKind::IQ4NL,
    QuantKind::IQ4XS,
    QuantKind::MXFP4,
    QuantKind::NVFP4Gguf,
];

/// The scalar the NVFP4 kernels multiply by. Not 1.0, so a row that drops it
/// is off by a factor rather than exactly right.
const TENSOR_SCALE: f32 = 0.37;

fn shape() -> DenseShape {
    DenseShape {
        hidden: WIDTH as u32,
        layers: 1,
        heads: 2,
        kv_heads: 1,
        head_dim: (WIDTH / 2) as u32,
        inter: WIDTH as u32,
        vocab: WIDTH as u32,
        eps: 1e-5,
        rope_theta: 10_000.0,
        rope_rot: (WIDTH / 2) as u32,
    }
}

/// The card, or nothing — and "busy" is not "absent".
fn device() -> Option<Arc<CudaDevice>> {
    if CudaDevice::free_vram(0).is_err() {
        eprintln!("pomijam: brak urządzenia CUDA");
        return None;
    }
    // The activation pool holds one executor's scratch per format in the table,
    // all of them at once, so it grows with the slot COUNT rather than with the
    // work. Adding the attention gate cost 256 KB per executor at this width
    // and this is the pool that felt it.
    let pools = PoolSizes {
        weights: 64 << 20,
        kv_cache: 32 << 20,
        kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        activations: 128 << 20,
    };
    Some(CudaDevice::new(0, pools).expect("karta jest, a nie oddała pul"))
}

/// A deterministic byte stream. Not `rand`, so a failure is reproducible by
/// running the test again rather than by recording a seed.
fn noise(seed: u64) -> impl FnMut() -> u8 {
    let mut state = seed | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    }
}

/// Packed bytes for a `WIDTH * WIDTH` weight in `quant`.
///
/// The six-bit mask is the limitation described at the top of this file: it
/// keeps every scale field of every layout finite without knowing where each
/// layout puts it.
fn blocks(quant: QuantKind, seed: u64) -> Vec<u8> {
    let numel = WIDTH * WIDTH;
    let mut next = noise(seed);
    if quant == QuantKind::None {
        // Unquantized IS a format, and its dtype is that format. Values in a
        // small range so the products stay in f16's comfortable middle.
        return (0..numel)
            .flat_map(|_| {
                let v = (next() as f32 / 255.0 - 0.5) * 0.5;
                half::f16::from_f32(v).to_le_bytes()
            })
            .collect();
    }
    let block_bytes = quant.block_bytes();
    let bytes = numel / quant.block_elems() * block_bytes;
    let mut out: Vec<u8> = (0..bytes).map(|_| next() & 0x3f).collect();
    if quant == QuantKind::MXFP4 {
        // The one layout whose scale is a bare exponent rather than an f16:
        // MXFP4 puts an E8M0 in byte zero, where 127 means 2^0. Six bits of it
        // means 2^-64, so the mask that keeps every other format finite turns
        // this one into zeros — which `assert_nontrivial` says out loud rather
        // than passing. A narrow band around 127 keeps the block varied and
        // the magnitude ordinary.
        for block in out.chunks_exact_mut(block_bytes) {
            block[0] = 125 + (next() % 5);
        }
    }
    out
}

/// The reference matrix: the CPU decoder's reading of exactly these bytes.
fn reference_weight(quant: QuantKind, bytes: &[u8]) -> Vec<f32> {
    let dtype = if quant == QuantKind::None {
        DType::F16
    } else {
        DType::U8
    };
    let mut w = dequantize_to_f32(dtype, quant, bytes, WIDTH * WIDTH)
        .unwrap_or_else(|e| panic!("{quant:?}: wzorzec CPU nie przeczytał własnych bajtów: {e}"));
    if quant == QuantKind::NVFP4Gguf {
        // The block holds only the four-bit exponent per sixteen values; the
        // rest of the range is this one multiplier, and the kernels take it as
        // an argument rather than reading it from the block.
        for v in &mut w {
            *v *= TENSOR_SCALE;
        }
    }
    w
}

/// An embedding table in f16 — the only way to place chosen activations into
/// `Act::Hidden` through the contract, since no operation writes them directly.
///
/// `scale` exists because these formats do not agree on magnitude. A masked
/// byte stream decodes to values around 1 in Q8_0 and to values in the
/// thousands in IQ4_XS, whose block scale is an f16 multiplied by a six-bit
/// per-group factor. The tile kernel accumulates in f16, so the second case
/// overflows to infinity at 256 columns — which is a fact about the magnitude
/// of the test's own weights, not about the row. Scaling the activation so the
/// products land near one keeps the comparison about the format.
fn embedding(seed: u64, scale: f32) -> (Vec<u8>, Vec<f32>) {
    let mut next = noise(seed);
    let values: Vec<f32> = (0..WIDTH * WIDTH)
        .map(|_| (next() as f32 / 255.0 - 0.5) * 2.0 * scale)
        .collect();
    let bytes = values
        .iter()
        .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
        .collect();
    // What the kernels will actually see, after f16 rounding — comparing
    // against the unrounded values would charge the row for the table's error.
    let seen = values
        .iter()
        .map(|&v| half::f16::from_f32(v).to_f32())
        .collect();
    (bytes, seen)
}

fn put(exec: &mut CudaExec, quant: QuantKind, bytes: Vec<u8>) -> WeightId {
    let global = (quant == QuantKind::NVFP4Gguf).then_some(TENSOR_SCALE);
    let dtype = if quant == QuantKind::None {
        DType::F16
    } else {
        DType::U8
    };
    exec.put_quant(QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes: bytes,
            scales: None,
            global,
        },
        quant,
        layout: Layout::Blocks,
        dtype,
        rows: WIDTH,
        cols: WIDTH,
    }))
    .unwrap_or_else(|e| panic!("{quant:?}: wykonawca nie przyjął wagi: {e}"))
}

/// `row = weight * x`, in f32, from the CPU decoder's matrix.
fn expected(w: &[f32], x: &[f32]) -> Vec<f32> {
    (0..WIDTH)
        .map(|r| (0..WIDTH).map(|c| w[r * WIDTH + c] * x[c]).sum())
        .collect()
}

/// Error as a fraction of what the reference row spans.
///
/// Absolute error says nothing without the scale of the numbers, and these
/// formats span three orders of magnitude between IQ1_S and Q8_0.
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

/// The reference matrix has to be worth multiplying by.
///
/// A masked byte stream that a format decodes to all zeros would make every
/// comparison below pass while proving nothing, and that is a plausible
/// outcome for a format whose codes index a grid.
fn assert_nontrivial(quant: QuantKind, w: &[f32]) {
    let (lo, hi) = w
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(
        w.iter().all(|v| v.is_finite()),
        "{quant:?}: wzorzec CPU dał wartość nieskończoną — maska bajtów nie wystarcza dla tego układu"
    );
    assert!(
        hi - lo > 1e-6,
        "{quant:?}: wzorzec CPU dał macierz stałą ({lo}..{hi}) — ten test by nic nie sprawdził"
    );
}

/// Every row of the table computes the format it names.
///
/// Three passes per format, because the table has three families and a row can
/// be right in one and wrong in another — which is exactly the failure that
/// put this table here: Q4_K used to reach a different kernel through the
/// decode path than through the batch one.
#[test]
#[ignore = "wymaga karty NVIDIA"]
fn every_format_agrees_with_the_cpu_reference() {
    let Some(device) = device() else { return };

    let mut failures = Vec::new();

    for (i, &quant) in FORMATS.iter().enumerate() {
        let bytes = blocks(quant, 0x1234_5678 + i as u64 * 0x9E37);
        let w = reference_weight(quant, &bytes);
        assert_nontrivial(quant, &w);

        // Random signs make the sum of `WIDTH` products grow like its square
        // root, so this puts the reference output near one whatever magnitude
        // the format's blocks decoded to.
        let peak = w.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let (table_bytes, table) = embedding(
            0x9E37_79B9_7F4A_7C15,
            1.0 / (peak * (WIDTH as f32).sqrt()),
        );

        let mut exec = CudaExec::new(
            device.clone() as Arc<_>,
            ExecSpec {
                shape: shape(),
                ssm: None,
                attends: vec![true].into(),
                quant_params: DType::F16,
                norm_weights: DType::F32,
            },
        )
        .expect("wykonawca");
        let embed = put(&mut exec, QuantKind::None, table_bytes);
        let weight = put(&mut exec, quant, bytes);

        // Vector form: one row of activations, which is the decode path.
        let token = 7u32;
        let step = Step::single(0, 0, 1).expect("krok");
        exec.run(&Op::Embed {
            table: embed,
            tokens: vec![token],
            step: step.clone(),
        })
        .expect("osadzenie");
        exec.run(&Op::MatMul {
            out: Act::Query,
            w: weight,
            x: Act::Hidden,
            step: step.clone(),
        })
        .unwrap_or_else(|e| panic!("{quant:?}: GEMV odmówił: {e}"));
        let x = &table[token as usize * WIDTH..(token as usize + 1) * WIDTH];
        let want = expected(&w, x);
        let got = exec.read(Act::Query, WIDTH).expect("odczyt");
        let gemv = spread_error(&got, &want);

        // The output head, which is the third family and writes f32.
        exec.run(&Op::LogitsOfLast {
            w: weight,
            x: Act::Hidden,
            step: step.clone(),
        })
        .unwrap_or_else(|e| panic!("{quant:?}: głowa odmówiła: {e}"));
        let head = spread_error(&exec.read(Act::Logits, WIDTH).expect("logity"), &want);

        // Tile form: many rows at once, a different kernel over the same bytes.
        let tokens: Vec<u32> = (0..TILE_TOKENS).map(|t| (t * 3 + 1) % WIDTH as u32).collect();
        let tile_step = Step::single(1, 0, TILE_TOKENS).expect("krok kafla");
        exec.run(&Op::Embed {
            table: embed,
            tokens: tokens.clone(),
            step: tile_step.clone(),
        })
        .expect("osadzenie kafla");
        exec.run(&Op::MatMul {
            out: Act::Query,
            w: weight,
            x: Act::Hidden,
            step: tile_step,
        })
        .unwrap_or_else(|e| panic!("{quant:?}: GEMM odmówił: {e}"));
        let tiled = exec.read(Act::Query, WIDTH * TILE_TOKENS as usize).expect("odczyt kafla");
        let gemm = tokens
            .iter()
            .enumerate()
            .map(|(row, &t)| {
                let x = &table[t as usize * WIDTH..(t as usize + 1) * WIDTH];
                spread_error(&tiled[row * WIDTH..(row + 1) * WIDTH], &expected(&w, x))
            })
            .fold(0.0f32, f32::max);

        eprintln!(
            "{quant:?}: GEMV {:.3}%, głowa {:.3}%, GEMM {:.3}%",
            gemv * 100.0,
            head * 100.0,
            gemm * 100.0
        );
        // Loose enough for two roundings of the same formula — the kernels
        // accumulate in f16 or int8 where the reference stays in f32 — and far
        // tighter than a row reaching the wrong format, which misreads the
        // block layout and lands orders of magnitude away.
        for (family, err) in [("GEMV", gemv), ("głowa", head), ("GEMM", gemm)] {
            if !(err < 0.02) {
                failures.push(format!("{quant:?} {family}: {:.3}% rozpiętości", err * 100.0));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} z {} przebiegów rozjechało się z wzorcem:\n  {}",
        failures.len(),
        FORMATS.len() * 3,
        failures.join("\n  ")
    );
}
