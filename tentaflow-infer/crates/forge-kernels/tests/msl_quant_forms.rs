// ===== File: msl_quant_forms.rs — every (width, group) through EVERY form =====
//
// The six-bit width had one gate: the vector form. That is the form a decode
// step takes, so a checkpoint carrying Q6_K weights decoded correctly and read
// as working — and a prompt long enough to reach the batched forms had nothing
// checking it at all.
//
// The group is the same kind of hole. MLX exports 4 bits over groups of 64,
// GGML Q4_K converts to 4/32 and Q6_K to 6/16, and Q4_K_M carries the last two
// IN THE SAME MODEL. The group is a divisor the addressing depends on, so a
// kernel tested on one pair says nothing about the others.
//
// So: every pair a real checkpoint produces, through every form that can serve
// it, against one CPU reference computed in f64 from the definition — not from
// another kernel.

#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use forge_hal::metal_device::MetalDevice;
use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_kernels::msl::{self, Bits, OutDtype, ScaleDtype};
use forge_types::MemKind;

/// Multiple of `QMG_BN`, so the matrix form accepts the shape at all.
const ROWS: usize = 128;
/// Multiple of `QMG_BK`, and of every group below.
const COLS: usize = 256;

/// Width and group of every affine weight this engine actually loads.
const PAIRS: &[(u32, usize)] = &[(4, 64), (4, 32), (6, 16)];

struct Weight {
    bits: u32,
    group: usize,
    codes: Vec<u8>,
    packed: Vec<u32>,
    high: Vec<u32>,
    scales: Vec<half::f16>,
    biases: Vec<half::f16>,
}

fn weight(bits: u32, group: usize) -> Weight {
    // Pełny zakres kodu tej szerokości. Przy sześciu bitach każda waga, której
    // starsze dwa bity są niezerowe, wyjdzie inna, jeśli kernel ich nie czyta.
    let span = if bits == 6 { 64 } else { 16 };
    let codes: Vec<u8> = (0..ROWS * COLS)
        .map(|i| ((i * 7 + 11) % span) as u8)
        .collect();
    let groups = ROWS * COLS / group;
    let mut packed = vec![0u32; ROWS * COLS / 8];
    let mut high = vec![0u32; ROWS * COLS / 16];
    for (i, &c) in codes.iter().enumerate() {
        packed[i / 8] |= u32::from(c & 0xF) << ((i % 8) * 4);
        high[i / 16] |= u32::from((c >> 4) & 0x3) << ((i % 16) * 2);
    }
    Weight {
        bits,
        group,
        codes,
        packed,
        high,
        scales: (0..groups)
            .map(|i| half::f16::from_f32(0.002 + (i % 5) as f32 * 0.0003))
            .collect(),
        biases: (0..groups)
            .map(|i| half::f16::from_f32(-0.03 - (i % 3) as f32 * 0.001))
            .collect(),
    }
}

fn activations(tokens: usize) -> Vec<half::f16> {
    (0..tokens * COLS)
        .map(|i| half::f16::from_f32((i % 11) as f32 * 0.05 - 0.25))
        .collect()
}

/// `out[token][row]`, from the definition, in f64.
fn reference(w: &Weight, x: &[half::f16], tokens: usize) -> Vec<f64> {
    let mut out = vec![0f64; tokens * ROWS];
    for t in 0..tokens {
        for r in 0..ROWS {
            let mut acc = 0f64;
            for c in 0..COLS {
                let g = r * (COLS / w.group) + c / w.group;
                let value = f64::from(w.codes[r * COLS + c]) * f64::from(w.scales[g])
                    + f64::from(w.biases[g]);
                acc += f64::from(x[t * COLS + c]) * value;
            }
            out[t * ROWS + r] = acc;
        }
    }
    out
}

struct Gpu {
    dev: std::sync::Arc<MetalDevice>,
    stream: Stream,
}

impl Gpu {
    fn up(&self, bytes: &[u8]) -> DevBuffer {
        let buf = self
            .dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        self.dev.write(bytes, &buf, 0).unwrap();
        buf
    }
}

#[derive(Clone, Copy)]
enum Form {
    Vector,
    Blocked,
    Matrix,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Form::Vector => "qmv",
            Form::Blocked => "qmm",
            Form::Matrix => "qmg",
        }
    }

    /// Tokens this form actually serves in the model, per the variant registry.
    fn tokens(self) -> usize {
        match self {
            Form::Vector => 1,
            Form::Blocked => 8,
            Form::Matrix => 64,
        }
    }

    fn source(self, bits: Bits) -> (String, String) {
        let (s, n) = match self {
            Form::Vector => (
                msl::qmv_affine_source as fn(Bits, ScaleDtype, OutDtype) -> String,
                msl::qmv_affine_name as fn(Bits, ScaleDtype, OutDtype) -> String,
            ),
            Form::Blocked => (
                msl::qmm_affine_source as fn(Bits, ScaleDtype, OutDtype) -> String,
                msl::qmm_affine_name as fn(Bits, ScaleDtype, OutDtype) -> String,
            ),
            Form::Matrix => (
                msl::qmg_affine_source as fn(Bits, ScaleDtype, OutDtype) -> String,
                msl::qmg_affine_name as fn(Bits, ScaleDtype, OutDtype) -> String,
            ),
        };
        (
            s(bits, ScaleDtype::F16, OutDtype::F32),
            n(bits, ScaleDtype::F16, OutDtype::F32),
        )
    }

    fn grid(self, tokens: u32) -> ((u32, u32), u32) {
        match self {
            Form::Vector => (
                (msl::qmv_affine_4bit_groups(ROWS as u32), 1),
                msl::QMV_THREADS,
            ),
            Form::Blocked => (
                msl::qmm_affine_4bit_groups(ROWS as u32, tokens),
                msl::QMM_THREADS,
            ),
            Form::Matrix => (
                msl::qmg_affine_4bit_groups(ROWS as u32, tokens),
                msl::QMG_THREADS,
            ),
        }
    }
}

fn run(gpu: &Gpu, w: &Weight, form: Form) -> Vec<f32> {
    let tokens = form.tokens();
    let x = activations(tokens);
    let bits = if w.bits == 6 { Bits::Six } else { Bits::Four };
    let (source, name) = form.source(bits);
    let module = gpu.dev.load_module(source.as_bytes()).unwrap();
    let kernel = module.kernel(&name).unwrap();
    let out = gpu
        .dev
        .alloc(tokens * ROWS * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    // Kolejność jak w `weight_args`: packed, scales, biases, x, a bufor
    // starszych bitów TYLKO przy sześciu — przy czterech kernel go nie
    // deklaruje, więc związanie go przesunęłoby wszystkie skalary o jeden.
    let mut args = LaunchArgs::new()
        .buf(&out)
        .buf(&gpu.up(bytemuck::cast_slice(&w.packed)))
        .buf(&gpu.up(bytemuck::cast_slice(&w.scales)))
        .buf(&gpu.up(bytemuck::cast_slice(&w.biases)))
        .buf(&gpu.up(bytemuck::cast_slice(&x)));
    if w.bits == 6 {
        args = args.buf(&gpu.up(bytemuck::cast_slice(&w.high)));
    }
    args = args
        .scalar(ROWS as u32)
        .scalar(COLS as u32)
        .scalar(w.group as u32);
    if !matches!(form, Form::Vector) {
        args = args.scalar(tokens as u32);
    }

    let (grid, threads) = form.grid(tokens as u32);
    gpu.dev
        .launch(
            &kernel,
            &LaunchConfig {
                grid: (grid.0, grid.1, 1),
                block: (threads, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &gpu.stream,
        )
        .unwrap();
    gpu.stream.synchronize().unwrap();

    let mut raw = vec![0u8; tokens * ROWS * 4];
    gpu.dev.read(&out, 0, &mut raw).unwrap();
    raw.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// Największa różnica, wyrażona w zakresie wyniku.
fn worst(got: &[f32], want: &[f64]) -> f64 {
    let span = want.iter().fold(0f64, |m, v| m.max(v.abs())).max(1e-30);
    got.iter()
        .zip(want)
        .fold(0f64, |m, (g, w)| m.max((f64::from(*g) - w).abs()))
        / span
}

#[test]
fn every_form_computes_every_width_and_group() {
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let stream = dev.create_stream().unwrap();
    let gpu = Gpu { dev, stream };

    for &(bits, group) in PAIRS {
        let w = weight(bits, group);
        for form in [Form::Vector, Form::Blocked, Form::Matrix] {
            let got = run(&gpu, &w, form);
            let want = reference(&w, &activations(form.tokens()), form.tokens());
            let err = worst(&got, &want);
            eprintln!(
                "{bits} bitów, grupa {group}, {}: {err:.3e} zakresu",
                form.label()
            );

            // Forma macierzowa zaokrągla mnożenie do half, więc jej próg jest
            // luźniejszy. Ale wszystkie trzy liczą TĘ SAMĄ wagę: pomylone
            // wyłuskanie kodu albo pomylona grupa dają błąd rzędu jedności, nie
            // tysięcznych. Ten próg rozstrzyga o adresowaniu, nie o precyzji.
            let limit = if matches!(form, Form::Matrix) {
                5e-3
            } else {
                1e-3
            };
            assert!(
                err <= limit,
                "{bits}/{group} przez {}: {err:.3e} zakresu",
                form.label()
            );
        }
    }
}
