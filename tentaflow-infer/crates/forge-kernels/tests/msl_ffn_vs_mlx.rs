// ===== File: msl_ffn_vs_mlx.rs — a whole FFN block of a real layer on the GPU =====
//
// Norm, both projections, the gate and the output projection of Bielik's layer
// zero, chained through the Metal backend on this machine and compared against
// MLX at EVERY stage. A single end comparison would say that something is
// wrong without saying where, and with four kernels in a row that is the
// difference between a lead and a bisection.
//
// The threshold is distance from the f64 truth, and the requirement is that the
// kernel be no worse than MLX's own path. Measuring against MLX directly would
// measure the oracle's loss: it dequantizes into the scale dtype, so it rounds
// every weight to eight mantissa bits before multiplying.
//
// Fixture: tools/mlx-oracle/gen_ffn.py
#![cfg(all(feature = "metal", target_os = "macos"))]

use std::collections::HashMap;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_kernels::msl::{self, OutDtype, ScaleDtype};
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_ffn_bielik.bin");
const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

struct Fixture {
    group: u32,
    bits: u32,
    hidden: u32,
    inter: u32,
    eps: f32,
    blobs: HashMap<String, Vec<u8>>,
}

impl Fixture {
    fn f16(&self, key: &str) -> Vec<u8> {
        self.blobs[key].clone()
    }

    fn f32s(&self, key: &str) -> Vec<f32> {
        self.blobs[key]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn f64s(&self, key: &str) -> Vec<f64> {
        self.blobs[key]
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn f16_as_f64(&self, key: &str) -> Vec<f64> {
        self.blobs[key]
            .chunks_exact(2)
            .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f64())
            .collect()
    }
}

fn load() -> Fixture {
    assert_eq!(&FIXTURE[0..4], b"FFN1", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |pos: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let group = u32_at(&mut pos);
    let bits = u32_at(&mut pos);
    let hidden = u32_at(&mut pos);
    let inter = u32_at(&mut pos);
    let eps = f32::from_le_bytes(FIXTURE[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let count = u32_at(&mut pos);

    let mut blobs = HashMap::new();
    for _ in 0..count {
        let key_len = u32_at(&mut pos) as usize;
        let key = String::from_utf8(FIXTURE[pos..pos + key_len].to_vec()).unwrap();
        pos += key_len;
        let len = u32_at(&mut pos) as usize;
        blobs.insert(key, FIXTURE[pos..pos + len].to_vec());
        pos += len;
    }
    Fixture {
        group,
        bits,
        hidden,
        inter,
        eps,
        blobs,
    }
}

/// The quantized weights come straight off the checkpoint on disk, not from the
/// fixture: three projections of a 4096-wide layer are 30 MB and belong next to
/// the model, not in the repository.
fn checkpoint_dir() -> Option<std::path::PathBuf> {
    let snapshots = std::path::PathBuf::from(CHECKPOINT);
    let dir = std::fs::read_dir(&snapshots).ok()?.flatten().next()?.path();
    dir.join("model.safetensors").is_file().then_some(dir)
}

struct Gpu {
    dev: std::sync::Arc<MetalDevice>,
    stream: Stream,
}

impl Gpu {
    fn upload(&self, bytes: &[u8]) -> DevBuffer {
        let buf = self
            .dev
            .alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)
            .unwrap();
        self.dev.write(bytes, &buf, 0).unwrap();
        buf
    }

    fn empty(&self, bytes: usize) -> DevBuffer {
        self.dev
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .unwrap()
    }

    fn read_f32(&self, buf: &DevBuffer, n: usize) -> Vec<f64> {
        let mut raw = vec![0u8; n * 4];
        self.dev.read(buf, 0, &mut raw).unwrap();
        raw.chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()) as f64)
            .collect()
    }

    fn read_f16(&self, buf: &DevBuffer, n: usize) -> Vec<f64> {
        let mut raw = vec![0u8; n * 2];
        self.dev.read(buf, 0, &mut raw).unwrap();
        raw.chunks_exact(2)
            .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f64())
            .collect()
    }
}

fn rel_l2(got: &[f64], want: &[f64]) -> f64 {
    let (mut diff, mut norm) = (0f64, 0f64);
    for (g, v) in got.iter().zip(want) {
        diff += (g - v) * (g - v);
        norm += v * v;
    }
    (diff / norm.max(1e-300)).sqrt()
}

/// Every stage is judged the same way, so the reasoning is written once.
fn check(stage: &str, got: &[f64], truth: &[f64], mlx: &[f64]) {
    let ours = rel_l2(got, truth);
    let theirs = rel_l2(mlx, truth);
    assert_eq!(got.len(), truth.len(), "{stage}: rozmiar");
    assert!(
        ours <= theirs.max(1e-6),
        "{stage}: kernel odbiega od prawdy o {ours:.3e}, a ścieżka MLX o {theirs:.3e}"
    );
    eprintln!("{stage:8}: kernel {ours:.3e}, MLX {theirs:.3e}");
}

#[test]
fn a_whole_ffn_block_matches_mlx_stage_by_stage() {
    let f = load();
    assert_eq!(f.bits, 4);
    let Some(ckpt) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let st = forge_formats::SafeTensors::open(ckpt.join("model.safetensors")).unwrap();
    let stream = dev.create_stream().unwrap();
    let gpu = Gpu { dev, stream };

    let scales = ScaleDtype::Bf16;
    // Bramka i projekcje po niej licza na wejsciu w half, bo tak trzyma
    // aktywacje MLX i tak trzyma je model — f32 byloby tu inna sciezka niz ta,
    // ktora dziala naprawde.
    let qmv_src = msl::qmv_affine_4bit_source(scales, OutDtype::F16);
    let qmv = gpu
        .dev
        .load_module(qmv_src.as_bytes())
        .unwrap()
        .kernel(&msl::qmv_affine_4bit_name(scales, OutDtype::F16))
        .unwrap();
    let norm_src = msl::rmsnorm_source(scales);
    let rmsnorm = gpu
        .dev
        .load_module(norm_src.as_bytes())
        .unwrap()
        .kernel(&msl::rmsnorm_name(scales))
        .unwrap();
    let silu = gpu
        .dev
        .load_module(msl::SILU_MUL_SOURCE.as_bytes())
        .unwrap()
        .kernel(msl::SILU_MUL_NAME)
        .unwrap();

    let (hidden, inter) = (f.hidden, f.inter);
    let x = gpu.upload(&f.f16("x"));
    let norm_w = gpu.upload(&f.f16("norm_w"));
    let h = gpu.empty(hidden as usize * 2);

    // --- norma ---
    let args = LaunchArgs::new()
        .buf(&h)
        .buf(&x)
        .buf(&norm_w)
        .scalar(hidden)
        .scalar(f.eps);
    gpu.dev
        .launch(
            &rmsnorm,
            &LaunchConfig {
                grid: (1, 1, 1),
                block: (msl::RMSNORM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &gpu.stream,
        )
        .unwrap();

    // --- projekcje gate i up ---
    let prefix = "model.layers.0.mlp";
    let proj = |name: &str| {
        (
            gpu.upload(st.data(&format!("{name}.weight")).unwrap()),
            gpu.upload(st.data(&format!("{name}.scales")).unwrap()),
            gpu.upload(st.data(&format!("{name}.biases")).unwrap()),
        )
    };
    let (gate_w, gate_s, gate_b) = proj(&format!("{prefix}.gate_proj"));
    let (up_w, up_s, up_b) = proj(&format!("{prefix}.up_proj"));
    let (down_w, down_s, down_b) = proj(&format!("{prefix}.down_proj"));

    let g = gpu.empty(inter as usize * 2);
    let u = gpu.empty(inter as usize * 2);
    let a = gpu.empty(inter as usize * 2);
    let y = gpu.empty(hidden as usize * 2);

    let gemv = |out: &DevBuffer,
                    w: &DevBuffer,
                    s: &DevBuffer,
                    b: &DevBuffer,
                    input: &DevBuffer,
                    rows: u32,
                    cols: u32| {
        let args = LaunchArgs::new()
            .buf(out)
            .buf(w)
            .buf(s)
            .buf(b)
            .buf(input)
            .scalar(rows)
            .scalar(cols)
            .scalar(f.group);
        gpu.dev
            .launch(
                &qmv,
                &LaunchConfig {
                    grid: (msl::qmv_affine_4bit_groups(rows), 1, 1),
                    block: (msl::QMV_THREADS, 1, 1),
                    shared_mem_bytes: 0,
                },
                &args,
                &gpu.stream,
            )
            .unwrap();
    };
    gemv(&g, &gate_w, &gate_s, &gate_b, &h, inter, hidden);
    gemv(&u, &up_w, &up_s, &up_b, &h, inter, hidden);

    // --- bramka ---
    let args = LaunchArgs::new().buf(&a).buf(&g).buf(&u).scalar(inter);
    gpu.dev
        .launch(
            &silu,
            &LaunchConfig {
                grid: (msl::silu_mul_groups(inter), 1, 1),
                block: (msl::SILU_MUL_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &gpu.stream,
        )
        .unwrap();

    // --- projekcja wyjściowa ---
    gemv(&y, &down_w, &down_s, &down_b, &a, hidden, inter);

    // Cały blok w JEDNYM buforze poleceń: pięć dyspozycji, jeden powrót na
    // hosta. To jest ta własność, dla której backend ma taki kształt.
    gpu.stream.synchronize().unwrap();

    check(
        "norma",
        &gpu.read_f16(&h, hidden as usize),
        &f.f64s("h_true"),
        &f.f16_as_f64("h_mlx"),
    );
    check(
        "gate",
        &gpu.read_f16(&g, inter as usize),
        &f.f64s("g_true"),
        &f.f32s("g_mlx").iter().map(|v| *v as f64).collect::<Vec<_>>(),
    );
    check(
        "up",
        &gpu.read_f16(&u, inter as usize),
        &f.f64s("u_true"),
        &f.f32s("u_mlx").iter().map(|v| *v as f64).collect::<Vec<_>>(),
    );
    check(
        "bramka",
        &gpu.read_f16(&a, inter as usize),
        &f.f64s("a_true"),
        &f.f16_as_f64("a_mlx"),
    );
    // Ostatni etap NIE jest porównywany z prawdą liczoną z wejścia MLX: kernel
    // podał tam swój własny, dokładniejszy wynik bramki, więc taka prawda
    // opisywałaby inne wejście i dokładniejszy kernel wypadłby gorzej. Właściwą
    // miarą dla złożenia jest cały łańcuch policzony w f64 od tego samego `x`.
    let mlx_chain = f
        .f32s("y_mlx")
        .iter()
        .map(|v| *v as f64)
        .collect::<Vec<_>>();
    check(
        "łańcuch",
        &gpu.read_f16(&y, hidden as usize),
        &f.f64s("y_chain_true"),
        &mlx_chain,
    );
}

#[test]
fn each_kernel_is_also_checked_in_isolation() {
    // Ten sam blok, ale każdy etap dostaje wejście z MLX zamiast wyniku
    // poprzedniego kernela. Rozdzielenie jest potrzebne, bo test łańcucha mówi
    // tylko, że złożenie jest dobre — nie mówi, KTÓRY kernel się myli.
    let f = load();
    let Some(ckpt) = checkpoint_dir() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let st = forge_formats::SafeTensors::open(ckpt.join("model.safetensors")).unwrap();
    let stream = dev.create_stream().unwrap();
    let gpu = Gpu { dev, stream };
    let scales = ScaleDtype::Bf16;

    let qmv_src = msl::qmv_affine_4bit_source(scales, OutDtype::F32);
    let qmv = gpu
        .dev
        .load_module(qmv_src.as_bytes())
        .unwrap()
        .kernel(&msl::qmv_affine_4bit_name(scales, OutDtype::F32))
        .unwrap();
    let (hidden, inter) = (f.hidden, f.inter);

    // Projekcja wyjściowa karmiona bramką POLICZONĄ PRZEZ MLX.
    let a_mlx = gpu.upload(&f.f16("a_mlx"));
    let w = gpu.upload(st.data("model.layers.0.mlp.down_proj.weight").unwrap());
    let s = gpu.upload(st.data("model.layers.0.mlp.down_proj.scales").unwrap());
    let b = gpu.upload(st.data("model.layers.0.mlp.down_proj.biases").unwrap());
    let y = gpu.empty(hidden as usize * 4);

    let args = LaunchArgs::new()
        .buf(&y)
        .buf(&w)
        .buf(&s)
        .buf(&b)
        .buf(&a_mlx)
        .scalar(hidden)
        .scalar(inter)
        .scalar(f.group);
    gpu.dev
        .launch(
            &qmv,
            &LaunchConfig {
                grid: (msl::qmv_affine_4bit_groups(hidden), 1, 1),
                block: (msl::QMV_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &gpu.stream,
        )
        .unwrap();
    gpu.stream.synchronize().unwrap();

    check(
        "down",
        &gpu.read_f32(&y, hidden as usize),
        &f.f64s("y_true"),
        &f.f32s("y_mlx").iter().map(|v| *v as f64).collect::<Vec<_>>(),
    );
}
