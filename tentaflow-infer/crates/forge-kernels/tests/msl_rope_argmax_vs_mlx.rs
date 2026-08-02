// ===== File: msl_rope_argmax_vs_mlx.rs — rotary embedding and greedy choice =====
//
// Two kernels whose failure modes are invisible in the shape of the output.
// RoPE has two conventions that differ only in which channels rotate together,
// and greedy choice has a tie rule that changes one token in a few thousand.
// Both are pinned against MLX here rather than against a reading of the docs.
//
// Fixture: tools/mlx-oracle/gen_rope_argmax.py
#![cfg(all(feature = "metal", target_os = "macos"))]

use std::collections::HashMap;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl;
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_rope_argmax.bin");

struct Fixture {
    heads: u32,
    dims: u32,
    pos: u32,
    argmax: u32,
    theta: f32,
    blobs: HashMap<String, Vec<u8>>,
}

fn load() -> Fixture {
    assert_eq!(&FIXTURE[0..4], b"RPAM", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let heads = u32_at(&mut pos);
    let dims = u32_at(&mut pos);
    let position = u32_at(&mut pos);
    let argmax = u32_at(&mut pos);
    let theta = f32::from_le_bytes(FIXTURE[pos..pos + 4].try_into().unwrap());
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
        heads,
        dims,
        pos: position,
        argmax,
        theta,
        blobs,
    }
}

fn f16_as_f64(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(2)
        .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f64())
        .collect()
}

fn f64s(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn rel_l2(got: &[f64], want: &[f64]) -> f64 {
    let (mut diff, mut norm) = (0f64, 0f64);
    for (g, v) in got.iter().zip(want) {
        diff += (g - v) * (g - v);
        norm += v * v;
    }
    (diff / norm.max(1e-300)).sqrt()
}

#[test]
fn rope_matches_mlx_on_the_half_split_convention() {
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let n = (f.heads * f.dims) as usize;
    let buf = dev.alloc(n * 2, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&f.blobs["q"], &buf, 0).unwrap();

    let module = dev
        .load_module(msl::ROPE_HALF_SPLIT_SOURCE.as_bytes())
        .unwrap();
    let kernel = module.kernel(msl::ROPE_HALF_SPLIT_NAME).unwrap();
    let stream = dev.create_stream().unwrap();

    let threads = 256u32;
    let args = LaunchArgs::new()
        .buf(&buf)
        .scalar(f.heads)
        .scalar(f.dims)
        .scalar(f.pos)
        .scalar(f.theta);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::rope_groups(f.heads, f.dims, threads), 1, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let mut raw = vec![0u8; n * 2];
    dev.read(&buf, 0, &mut raw).unwrap();
    let got = f16_as_f64(&raw);
    let truth = f64s(&f.blobs["rope_true"]);
    let mlx = f16_as_f64(&f.blobs["rope_mlx"]);

    let ours = rel_l2(&got, &truth);
    let theirs = rel_l2(&mlx, &truth);
    eprintln!("rope: kernel {ours:.3e}, MLX {theirs:.3e}");
    assert!(
        ours <= theirs * 1.5,
        "kernel odbiega od prawdy o {ours:.3e}, a MLX o {theirs:.3e}"
    );

    // Druga, niezależna kontrola: gdyby kernel obracał pary sąsiadujące
    // zamiast oddalonych o połowę wymiaru, kształt i norma by się zgadzały,
    // a wartości nie. Porównanie z MLX element po elemencie to wyklucza.
    let vs_mlx = rel_l2(&got, &mlx);
    assert!(
        vs_mlx < 1.0e-3,
        "kernel i MLX rozjeżdżają się o {vs_mlx:.3e} — to inna konwencja, \
         nie precyzja"
    );
}

#[test]
fn rope_with_the_wrong_pairing_is_visible() {
    // Kontrola samego porównania: obrót par sąsiadujących MUSI dać inny wynik,
    // inaczej test nie odróżnia dwóch konwencji i jego zieleń nic nie znaczy.
    let f = load();
    let q = f16_as_f64(&f.blobs["q"]);
    let truth = f64s(&f.blobs["rope_true"]);
    let (heads, dims) = (f.heads as usize, f.dims as usize);

    let mut traditional = q.clone();
    for h in 0..heads {
        for i in 0..dims / 2 {
            let freq = f.pos as f64 * (f.theta as f64).powf(-2.0 * i as f64 / dims as f64);
            let (c, s) = (freq.cos(), freq.sin());
            let (a, b) = (q[h * dims + 2 * i], q[h * dims + 2 * i + 1]);
            traditional[h * dims + 2 * i] = a * c - b * s;
            traditional[h * dims + 2 * i + 1] = a * s + b * c;
        }
    }
    let diff = rel_l2(&traditional, &truth);
    assert!(
        diff > 1.0e-2,
        "obie konwencje dały ten sam wynik ({diff:.3e}) — porównanie ich nie rozróżnia"
    );
}

#[test]
fn argmax_matches_mlx_including_the_tie_rule() {
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let logits = &f.blobs["logits"];
    let n = (logits.len() / 4) as u32;

    let input = dev
        .alloc(logits.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(logits, &input, 0).unwrap();
    let out = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();

    let module = dev.load_module(msl::ARGMAX_SOURCE.as_bytes()).unwrap();
    let kernel = module.kernel(msl::ARGMAX_NAME).unwrap();
    let stream = dev.create_stream().unwrap();

    let args = LaunchArgs::new().buf(&out).buf(&input).scalar(n);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (1, 1, 1),
            block: (msl::ARGMAX_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let mut raw = [0u8; 4];
    dev.read(&out, 0, &mut raw).unwrap();
    let got = u32::from_le_bytes(raw);

    // Fikstura ma DWA maksima o tej samej wartości. MLX bierze pierwsze,
    // i kernel musi też — inaczej model rozjeżdża się co kilka tysięcy tokenów
    // i nikt nie wie dlaczego.
    assert_eq!(
        got, f.argmax,
        "kernel wybrał {got}, MLX {} — sprawdź regułę remisu",
        f.argmax
    );
}
