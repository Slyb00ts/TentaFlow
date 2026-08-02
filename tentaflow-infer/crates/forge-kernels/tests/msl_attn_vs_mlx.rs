// ===== File: msl_attn_vs_mlx.rs — decode attention against MLX =====
//
// One query against a 512-entry KV cache with grouped query heads, on this
// machine's GPU. GQA is the point: four query heads share one KV stream, and a
// wrong mapping changes neither the shape nor the norm of the result, so only a
// value comparison catches it. The test carries a control that computes the
// wrong mapping and asserts the comparison can tell.
//
// Fixture: tools/mlx-oracle/gen_attn.py
#![cfg(all(feature = "metal", target_os = "macos"))]

use std::collections::HashMap;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl;
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_attn_bielik.bin");

struct Fixture {
    heads: u32,
    kv_heads: u32,
    dim: u32,
    seq: u32,
    scale: f32,
    blobs: HashMap<String, Vec<u8>>,
}

fn load() -> Fixture {
    assert_eq!(&FIXTURE[0..4], b"ATT1", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let heads = u32_at(&mut pos);
    let kv_heads = u32_at(&mut pos);
    let dim = u32_at(&mut pos);
    let seq = u32_at(&mut pos);
    let scale = f32::from_le_bytes(FIXTURE[pos..pos + 4].try_into().unwrap());
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
        kv_heads,
        dim,
        seq,
        scale,
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
fn decode_attention_matches_mlx_with_grouped_heads() {
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let upload = |bytes: &[u8]| {
        let buf = dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(bytes, &buf, 0).unwrap();
        buf
    };
    let q = upload(&f.blobs["q"]);
    let k = upload(&f.blobs["k"]);
    let v = upload(&f.blobs["v"]);
    let out = dev
        .alloc((f.heads * f.dim) as usize * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    let source = msl::attn_decode_source(f.dim);
    let kernel = dev
        .load_module(source.as_bytes())
        .unwrap()
        .kernel(&msl::attn_decode_name(f.dim))
        .unwrap();
    let stream = dev.create_stream().unwrap();

    let args = LaunchArgs::new()
        .buf(&out)
        .buf(&q)
        .buf(&k)
        .buf(&v)
        .scalar(f.heads)
        .scalar(f.kv_heads)
        .scalar(f.seq)
        .scalar(f.seq)
        .scalar(f.scale);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (f.heads, 1, 1),
            block: (msl::ATTN_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let mut raw = vec![0u8; (f.heads * f.dim) as usize * 2];
    dev.read(&out, 0, &mut raw).unwrap();
    let got = f16_as_f64(&raw);
    let truth = f64s(&f.blobs["out_true"]);
    let mlx = f16_as_f64(&f.blobs["out_mlx"]);

    let ours = rel_l2(&got, &truth);
    let theirs = rel_l2(&mlx, &truth);
    eprintln!("uwaga: kernel {ours:.3e}, MLX {theirs:.3e}");
    assert!(
        ours <= theirs * 1.5,
        "kernel odbiega od prawdy o {ours:.3e}, a MLX o {theirs:.3e}"
    );

    // Osobno per głowica: średnia po wszystkich mogłaby ukryć jedną głowicę
    // czytającą cudzy strumień KV, bo pozostałe trzydzieści jeden by ją zalało.
    for h in 0..f.heads as usize {
        let lo = h * f.dim as usize;
        let hi = lo + f.dim as usize;
        let err = rel_l2(&got[lo..hi], &truth[lo..hi]);
        assert!(err < 5.0e-3, "głowica {h}: {err:.3e}");
    }
}

#[test]
fn a_wrong_query_to_kv_mapping_is_visible() {
    // Kontrola samego porównania. Mapa 4:1 to jedyne miejsce, w którym błąd nie
    // zmienia ani kształtu, ani rzędu wielkości wyniku — jeśli test tego nie
    // odróżnia, jego zieleń nie znaczy nic.
    let f = load();
    let truth = f64s(&f.blobs["out_true"]);
    let (heads, dim) = (f.heads as usize, f.dim as usize);

    let q = f16_as_f64(&f.blobs["q"]);
    let k = f16_as_f64(&f.blobs["k"]);
    let v = f16_as_f64(&f.blobs["v"]);
    let seq = f.seq as usize;

    // Zła mapa: `head % kv_heads` zamiast `head / per_kv`. Kształt ten sam,
    // rozkład wartości podobny, treść inna.
    let mut wrong = vec![0f64; heads * dim];
    for h in 0..heads {
        let kv = h % f.kv_heads as usize;
        let mut scores = vec![0f64; seq];
        for (j, score) in scores.iter_mut().enumerate() {
            let mut acc = 0.0;
            for c in 0..dim {
                acc += q[h * dim + c] * k[(kv * seq + j) * dim + c];
            }
            *score = acc * f.scale as f64;
        }
        let m = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mut total = 0.0;
        for s in scores.iter_mut() {
            *s = (*s - m).exp();
            total += *s;
        }
        for c in 0..dim {
            let mut acc = 0.0;
            for (j, s) in scores.iter().enumerate() {
                acc += s * v[(kv * seq + j) * dim + c];
            }
            wrong[h * dim + c] = acc / total;
        }
    }

    let err = rel_l2(&wrong, &truth);
    assert!(
        err > 1.0e-1,
        "zła mapa głowic dała {err:.3e} — porównanie jej nie odróżnia; \
         przy 4:1 pierwsze cztery głowice trafiają w to samo KV, więc kontrola \
         musi patrzeć na pozostałe"
    );
}

#[test]
fn a_cache_longer_than_the_declared_bound_is_refused() {
    // Limit siedzi w pamięci grupy roboczej, więc nie da się go przekroczyć
    // „trochę". Kernel ma odmówić, a nie czytać poza tablicę.
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let source = msl::attn_decode_source(f.dim);
    let kernel = dev
        .load_module(source.as_bytes())
        .unwrap()
        .kernel(&msl::attn_decode_name(f.dim))
        .unwrap();
    let stream = dev.create_stream().unwrap();

    let q = dev
        .alloc((f.heads * f.dim) as usize * 2, MemKind::Device, Pool::Weights)
        .unwrap();
    let out = dev
        .alloc((f.heads * f.dim) as usize * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&vec![0u8; (f.heads * f.dim) as usize * 2], &out, 0)
        .unwrap();

    let args = LaunchArgs::new()
        .buf(&out)
        .buf(&q)
        .buf(&q)
        .buf(&q)
        .scalar(f.heads)
        .scalar(f.kv_heads)
        .scalar(msl::ATTN_MAX_SEQ + 1)
        .scalar(msl::ATTN_MAX_SEQ + 1)
        .scalar(f.scale);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (f.heads, 1, 1),
            block: (msl::ATTN_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    // Odmowa znaczy: wyjście nietknięte, nie śmieci.
    let mut raw = vec![0xFFu8; (f.heads * f.dim) as usize * 2];
    dev.read(&out, 0, &mut raw).unwrap();
    assert!(
        raw.iter().all(|b| *b == 0),
        "kernel powinien odmówić i nie dotknąć wyjścia"
    );
}
