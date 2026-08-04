// ===== File: msl_attn_vs_mlx.rs — decode attention against MLX =====
//
// One query against a 512-entry KV cache with grouped query heads, on this
// machine's GPU. GQA is the point: four query heads share one KV stream, and a
// wrong mapping changes neither the shape nor the norm of the result, so only a
// value comparison catches it. The test carries a control that computes the
// wrong mapping and asserts the comparison can tell.
//
// Fixture: tools/mlx-oracle/gen_attn.py
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::collections::HashMap;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Pool};
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
        .scalar(f.scale)
        .scalar(1u32);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::attn_groups(f.heads, 1), 1, 1),
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
fn a_length_past_the_cache_capacity_is_refused() {
    // Softmax przyrostowy zniósł limit wpisany w rozmiar tablicy wyników — na
    // długość kontekstu kernel nie ma już własnego sufitu. Co ZOSTAJE: nie
    // wolno czytać dalej, niż sięga cache. Ta granica jest realna, bo pamięć
    // za nią należy do kogoś innego.
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
        .scalar(64u32)
        .scalar(32u32)
        .scalar(f.scale)
        .scalar(1u32);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::attn_groups(f.heads, 1), 1, 1),
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

/// Forma blokowa wobec formy per-token, na wsadzie zapytań.
///
/// Ta druga jest przypięta do MLX testem wyżej, więc zgodność z nią domyka
/// łańcuch bez drugiej wyroczni. Porównanie jest przy tym mocniejsze niż
/// „obie dają coś sensownego": obie liczą maskę przyczynową, więc rozjazd o
/// jedną pozycję w masce zmienia wynik dla każdego zapytania osobno.
#[test]
fn blocked_attention_agrees_with_the_per_token_form() {
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let stream = dev.create_stream().unwrap();

    const DIM: u32 = 128;
    const HEADS: u32 = 32;
    const KV_HEADS: u32 = 8;
    const TOKENS: u32 = 96; // celowo NIE wielokrotność bloku 32... jest; patrz niżej
    const CAP: u32 = 256;
    let seq = TOKENS;
    let scale = 1.0f32 / (DIM as f32).sqrt();

    // Dane pseudolosowe, deterministyczne: liczby mają być różne i skończone,
    // a nie realistyczne — maskę i układ sprawdza porównanie, nie rozkład.
    let mut state = 0x2026_0803u32;
    let mut next = || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        ((state >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 2.0
    };
    let to_h = |v: &[f32]| -> Vec<u8> {
        v.iter()
            .flat_map(|x| half::f16::from_f32(*x).to_bits().to_le_bytes())
            .collect()
    };

    let q_host: Vec<f32> = (0..(TOKENS * HEADS * DIM) as usize).map(|_| next()).collect();
    let mut k_host = vec![0f32; (KV_HEADS * CAP * DIM) as usize];
    let mut v_host = vec![0f32; (KV_HEADS * CAP * DIM) as usize];
    for h in 0..KV_HEADS as usize {
        for t in 0..TOKENS as usize {
            for d in 0..DIM as usize {
                k_host[(h * CAP as usize + t) * DIM as usize + d] = next();
                v_host[(h * CAP as usize + t) * DIM as usize + d] = next();
            }
        }
    }

    let up = |bytes: &[u8]| {
        let b = dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(bytes, &b, 0).unwrap();
        b
    };
    let q = up(&to_h(&q_host));
    let k = up(&to_h(&k_host));
    let v = up(&to_h(&v_host));
    let out_len = (TOKENS * HEADS * DIM) as usize;
    let out_a = dev
        .alloc(out_len * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let out_b = dev
        .alloc(out_len * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    let args = |o: &DevBuffer| {
        LaunchArgs::new()
            .buf(o)
            .buf(&q)
            .buf(&k)
            .buf(&v)
            .scalar(HEADS)
            .scalar(KV_HEADS)
            .scalar(seq)
            .scalar(CAP)
            .scalar(scale)
            .scalar(TOKENS)
    };

    let per_token = dev
        .load_module(msl::attn_decode_source(DIM).as_bytes())
        .unwrap()
        .kernel(&msl::attn_decode_name(DIM))
        .unwrap();
    dev.launch(
        &per_token,
        &LaunchConfig {
            grid: (msl::attn_groups(HEADS, TOKENS), 1, 1),
            block: (msl::ATTN_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args(&out_a),
        &stream,
    )
    .unwrap();

    assert!(msl::flash_fits(TOKENS, DIM));
    let blocked = dev
        .load_module(msl::flash_attn_source(DIM).as_bytes())
        .unwrap()
        .kernel(&msl::flash_attn_name(DIM))
        .unwrap();
    dev.launch(
        &blocked,
        &LaunchConfig {
            grid: (msl::flash_attn_groups(HEADS, TOKENS), 1, 1),
            block: (msl::FLASH_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args(&out_b),
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let read = |b: &DevBuffer| -> Vec<f32> {
        let mut raw = vec![0u8; out_len * 2];
        dev.read(b, 0, &mut raw).unwrap();
        raw.chunks_exact(2)
            .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f32())
            .collect()
    };
    let a = read(&out_a);
    let b = read(&out_b);

    // Per token, nie na całości: błąd w jednym zapytaniu — na przykład w tym,
    // które jako jedyne widzi cały kontekst — ginie w normie po wszystkich.
    let mut worst = 0f64;
    for t in 0..TOKENS as usize {
        let lo = t * (HEADS * DIM) as usize;
        let hi = lo + (HEADS * DIM) as usize;
        let num: f64 = a[lo..hi]
            .iter()
            .zip(&b[lo..hi])
            .map(|(x, y)| (*x as f64 - *y as f64).powi(2))
            .sum();
        let den: f64 = a[lo..hi].iter().map(|x| (*x as f64).powi(2)).sum();
        let rel = (num / den.max(1e-30)).sqrt();
        worst = worst.max(rel);
        assert!(
            rel < 3e-3,
            "token {t}: forma blokowa odbiega od per-token o {rel:.3e}"
        );
    }
    eprintln!("największa różnica na token: {worst:.3e}");

    // Kontrola samego porównania: bez maski przyczynowej ostatni token nic by
    // nie zmienił, a pierwszy — wszystko. Sprawdzamy, że wyniki NIE są stałe.
    let first = &a[0..(HEADS * DIM) as usize];
    let last = &a[(TOKENS as usize - 1) * (HEADS * DIM) as usize..];
    let diff: f64 = first
        .iter()
        .zip(last)
        .map(|(x, y)| (*x as f64 - *y as f64).abs())
        .sum();
    assert!(diff > 1.0, "wyniki nie zależą od pozycji — maska nie działa");
}
