// ===== File: msl_qmv_vs_mlx.rs — the Metal dequant-GEMV against the MLX oracle =====
//
// The kernel that dominates a decode step, run on this machine's GPU and
// compared against MLX. The oracle is `mx.dequantize` composed with an f32
// matrix product — the mathematical definition of the operation, not a second
// implementation of the same kernel.
//
// Fixture: tools/mlx-oracle/gen_qmv.py
#![cfg(all(feature = "metal", target_os = "macos"))]

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl::{self, OutDtype, ScaleDtype};
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_qmv_bielik.bin");

struct Case {
    name: String,
    rows: u32,
    cols: u32,
    packed: Vec<u8>,
    scales: Vec<u8>,
    biases: Vec<u8>,
    x: Vec<u8>,
    /// Wynik ścieżki MLX w f32 (waga zaokrąglona do typu skal).
    y_mlx: Vec<f32>,
    /// Prawda w f64: `q * skala + przesunięcie` bez zaokrąglenia wagi.
    y_true: Vec<f64>,
}

fn load() -> (u32, u32, Vec<Case>) {
    assert_eq!(&FIXTURE[0..4], b"QMV1", "zły magic fikstury");
    let mut pos = 4usize;
    fn u32_at(pos: &mut usize) -> u32 {
        let v = u32::from_le_bytes(FIXTURE[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;
        v
    }
    fn blob(pos: &mut usize) -> Vec<u8> {
        let len = u32_at(pos) as usize;
        let out = FIXTURE[*pos..*pos + len].to_vec();
        *pos += len;
        out
    }
    assert_eq!(u32_at(&mut pos), 2, "wersja fikstury");
    let group = u32_at(&mut pos);
    let bits = u32_at(&mut pos);
    let count = u32_at(&mut pos);

    let mut cases = Vec::new();
    for _ in 0..count {
        let name_len = u32_at(&mut pos) as usize;
        let name = String::from_utf8(FIXTURE[pos..pos + name_len].to_vec()).unwrap();
        pos += name_len;
        let rows = u32_at(&mut pos);
        let cols = u32_at(&mut pos);
        let (packed, scales, biases, x, y, y64) = (
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
        );
        cases.push(Case {
            name,
            rows,
            cols,
            packed,
            scales,
            biases,
            x,
            y_mlx: y
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            y_true: y64
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        });
    }
    (group, bits, cases)
}

#[test]
fn dequant_gemv_matches_mlx_on_real_weights() {
    let (group, bits, cases) = load();
    assert_eq!(bits, 4, "ten kernel obsługuje wyłącznie 4 bity");

    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    // Bielik jest konwertowany przez mlx-lm, czyli skale w bf16.
    let scale_dtype = ScaleDtype::Bf16;
    let source = msl::qmv_affine_4bit_source(scale_dtype, OutDtype::F32);
    let module = dev.load_module(source.as_bytes()).unwrap();
    let kernel = module
        .kernel(&msl::qmv_affine_4bit_name(scale_dtype, OutDtype::F32))
        .unwrap();
    let stream = dev.create_stream().unwrap();

    for c in &cases {
        let upload = |bytes: &[u8]| {
            let buf = dev
                .alloc(bytes.len(), MemKind::Device, Pool::Weights)
                .unwrap();
            dev.write(bytes, &buf, 0).unwrap();
            buf
        };
        let packed = upload(&c.packed);
        let scales = upload(&c.scales);
        let biases = upload(&c.biases);
        let x = upload(&c.x);
        let y = dev
            .alloc(c.rows as usize * 4, MemKind::Device, Pool::Activations)
            .unwrap();

        let args = LaunchArgs::new()
            .buf(&y)
            .buf(&packed)
            .buf(&scales)
            .buf(&biases)
            .buf(&x)
            .scalar(c.rows)
            .scalar(c.cols)
            .scalar(group);
        let cfg = LaunchConfig {
            grid: (msl::qmv_affine_4bit_groups(c.rows), 1, 1),
            block: (msl::QMV_THREADS, 1, 1),
            shared_mem_bytes: 0,
        };
        dev.launch(&kernel, &cfg, &args, &stream).unwrap();
        stream.synchronize().unwrap();

        let mut bytes = vec![0u8; c.rows as usize * 4];
        dev.read(&y, 0, &mut bytes).unwrap();
        let got: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();

        // Progiem NIE jest odległość od wyniku MLX, tylko od prawdy: MLX
        // dekwantyzuje do typu skal, czyli tutaj zaokrągla wagę do ośmiu bitów
        // mantysy, a kernel liczy bez tego zaokrąglenia. Porównanie z MLX
        // mierzyłoby więc głównie stratę wyroczni. Wymagamy, żeby kernel był
        // wobec prawdy NIE GORSZY niż ścieżka MLX.
        let mlx_err = rel_l2_f64(&c.y_mlx.iter().map(|v| *v as f64).collect::<Vec<_>>(), &c.y_true);
        let kernel_err = rel_l2_f64(&got.iter().map(|v| *v as f64).collect::<Vec<_>>(), &c.y_true);
        let cos = cosine(&got, &c.y_mlx);

        assert!(
            cos > 0.9999,
            "{}: kosinus wobec MLX {cos:.7} — to nie jest precyzja, tylko inna \
             matematyka",
            c.name
        );
        assert!(
            kernel_err <= mlx_err,
            "{}: kernel odbiega od prawdy o {kernel_err:.3e}, a ścieżka MLX \
             o {mlx_err:.3e}",
            c.name
        );
        // Osobny bezwzględny sufit, żeby „nie gorzej niż MLX" nie stało się
        // zielone tylko dlatego, że wyrocznia jest nieprecyzyjna.
        assert!(
            kernel_err < 1.0e-5,
            "{}: kernel odbiega od prawdy o {kernel_err:.3e}",
            c.name
        );
        eprintln!(
            "{}: kernel wobec prawdy {kernel_err:.3e}, MLX wobec prawdy {mlx_err:.3e}, \
             kosinus wobec MLX {cos:.8}",
            c.name
        );
    }
}

#[test]
fn a_wrong_group_size_is_visible_in_the_result() {
    // Kontrola samego porównania. Rozmiar grupy nie zmienia kształtów, więc
    // pomyłka w nim nie wywali się na żadnej asercji rozmiaru — musi ją
    // zobaczyć porównanie liczbowe, inaczej zielony wynik nic nie znaczy.
    let (group, _bits, cases) = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let c = &cases[0];
    let source = msl::qmv_affine_4bit_source(ScaleDtype::Bf16, OutDtype::F32);
    let module = dev.load_module(source.as_bytes()).unwrap();
    let kernel = module
        .kernel(&msl::qmv_affine_4bit_name(ScaleDtype::Bf16, OutDtype::F32))
        .unwrap();
    let stream = dev.create_stream().unwrap();

    let upload = |bytes: &[u8]| {
        let buf = dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(bytes, &buf, 0).unwrap();
        buf
    };
    let (packed, scales, biases, x) = (
        upload(&c.packed),
        upload(&c.scales),
        upload(&c.biases),
        upload(&c.x),
    );
    let y = dev
        .alloc(c.rows as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    let args = LaunchArgs::new()
        .buf(&y)
        .buf(&packed)
        .buf(&scales)
        .buf(&biases)
        .buf(&x)
        .scalar(c.rows)
        .scalar(c.cols)
        .scalar(group * 2);
    let cfg = LaunchConfig {
        grid: (msl::qmv_affine_4bit_groups(c.rows), 1, 1),
        block: (msl::QMV_THREADS, 1, 1),
        shared_mem_bytes: 0,
    };
    dev.launch(&kernel, &cfg, &args, &stream).unwrap();
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; c.rows as usize * 4];
    dev.read(&y, 0, &mut bytes).unwrap();
    let got: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let (rel_l2, cos) = (
        rel_l2_f64(&got.iter().map(|v| *v as f64).collect::<Vec<_>>(), &c.y_true),
        cosine(&got, &c.y_mlx),
    );
    assert!(
        cos < 0.9999 || rel_l2 > 1.0e-3,
        "zła grupa dała kosinus {cos:.7} i rel_l2 {rel_l2:.3e} — porównanie \
         nie odróżnia wyników"
    );
}

fn rel_l2_f64(got: &[f64], want: &[f64]) -> f64 {
    let (mut diff, mut norm) = (0f64, 0f64);
    for (g, v) in got.iter().zip(want) {
        diff += (g - v) * (g - v);
        norm += v * v;
    }
    (diff / norm.max(1e-300)).sqrt()
}

/// Kosinus łapie to, czego norma nie: wynik przeskalowany o stałą albo liczony
/// z przestawionych danych.
fn cosine(got: &[f32], want: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (g, v) in got.iter().zip(want) {
        let (g, v) = (*g as f64, *v as f64);
        dot += g * v;
        na += g * g;
        nb += v * v;
    }
    dot / (na.sqrt() * nb.sqrt()).max(1e-300)
}
