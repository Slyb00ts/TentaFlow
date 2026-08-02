// ===== File: msl_embed_vs_mlx.rs — quantized embedding lookup and residual add =====
//
// The embedding lookup is the only read in a decode step indexed by a token
// rather than sequential, so a wrong row offset yields a correctly shaped
// vector holding a different word. The fixture therefore carries six tokens
// from different parts of the table, all verified non-zero: this vocabulary has
// 140 zeroed rows, and on a zero row a wrong offset also produces zeros.
//
// Fixture: tools/mlx-oracle/gen_embed.py
#![cfg(all(feature = "metal", target_os = "macos"))]

use std::collections::HashMap;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl::{self, ScaleDtype};
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_embed_bielik.bin");

struct Fixture {
    group: u32,
    hidden: u32,
    tokens: Vec<u32>,
    blobs: HashMap<String, Vec<u8>>,
}

fn load() -> Fixture {
    assert_eq!(&FIXTURE[0..4], b"EMB1", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let group = u32_at(&mut pos);
    let bits = u32_at(&mut pos);
    assert_eq!(bits, 4);
    let hidden = u32_at(&mut pos);
    let token_count = u32_at(&mut pos);
    let tokens: Vec<u32> = (0..token_count).map(|_| u32_at(&mut pos)).collect();
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
        hidden,
        tokens,
        blobs,
    }
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
fn embedding_lookup_matches_mlx_for_every_token() {
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let scales = ScaleDtype::Bf16;
    let source = msl::embed_gather_source(scales);
    let kernel = dev
        .load_module(source.as_bytes())
        .unwrap()
        .kernel(&msl::embed_gather_name(scales))
        .unwrap();
    let stream = dev.create_stream().unwrap();
    let out = dev
        .alloc(f.hidden as usize * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    for &token in &f.tokens {
        let upload = |key: String| {
            let bytes = &f.blobs[&key];
            let buf = dev
                .alloc(bytes.len(), MemKind::Device, Pool::Weights)
                .unwrap();
            dev.write(bytes, &buf, 0).unwrap();
            buf
        };
        // Fikstura trzyma pojedynczy wiersz, więc token wewnątrz kernela to 0 —
        // sam offset wierszowy sprawdza osobny test niżej, na sklejonej tabeli.
        let packed = upload(format!("packed_{token}"));
        let s = upload(format!("scales_{token}"));
        let b = upload(format!("biases_{token}"));

        let args = LaunchArgs::new()
            .buf(&out)
            .buf(&packed)
            .buf(&s)
            .buf(&b)
            .scalar(0u32)
            .scalar(f.hidden)
            .scalar(f.group);
        dev.launch(
            &kernel,
            &LaunchConfig {
                grid: (msl::elementwise_groups(f.hidden), 1, 1),
                block: (msl::ELEMENTWISE_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &stream,
        )
        .unwrap();
        stream.synchronize().unwrap();

        let mut raw = vec![0u8; f.hidden as usize * 2];
        dev.read(&out, 0, &mut raw).unwrap();
        let got: Vec<f64> = raw
            .chunks_exact(2)
            .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f64())
            .collect();
        let truth = f64s(&f.blobs[&format!("true_{token}")]);

        let err = rel_l2(&got, &truth);
        // Wyjście jest w f16, więc próg to rozdzielczość tego typu, nie f32.
        assert!(err < 1.0e-3, "token {token}: rel_l2 {err:.3e}");
        assert!(
            got.iter().any(|v| v.abs() > 1e-4),
            "token {token}: sam zera — na zerach ten test nic nie sprawdza"
        );
    }
}

#[test]
fn the_row_offset_is_applied_to_scales_as_well_as_to_bits() {
    // Skleja dwa wiersze w jedną tabelę i pobiera DRUGI. Kernel, który
    // przesuwa tylko upakowane bity, a skale czyta od zera, daje wektor
    // poprawnego kształtu zbudowany z dwóch różnych tokenów naraz.
    let f = load();
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let (first, second) = (f.tokens[0], f.tokens[3]);
    let mut packed = f.blobs[&format!("packed_{first}")].clone();
    packed.extend_from_slice(&f.blobs[&format!("packed_{second}")]);
    let mut scales = f.blobs[&format!("scales_{first}")].clone();
    scales.extend_from_slice(&f.blobs[&format!("scales_{second}")]);
    let mut biases = f.blobs[&format!("biases_{first}")].clone();
    biases.extend_from_slice(&f.blobs[&format!("biases_{second}")]);

    let upload = |bytes: &[u8]| {
        let buf = dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(bytes, &buf, 0).unwrap();
        buf
    };
    let scale_dtype = ScaleDtype::Bf16;
    let source = msl::embed_gather_source(scale_dtype);
    let kernel = dev
        .load_module(source.as_bytes())
        .unwrap()
        .kernel(&msl::embed_gather_name(scale_dtype))
        .unwrap();
    let stream = dev.create_stream().unwrap();
    let out = dev
        .alloc(f.hidden as usize * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    let args = LaunchArgs::new()
        .buf(&out)
        .buf(&upload(&packed))
        .buf(&upload(&scales))
        .buf(&upload(&biases))
        .scalar(1u32)
        .scalar(f.hidden)
        .scalar(f.group);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::elementwise_groups(f.hidden), 1, 1),
            block: (msl::ELEMENTWISE_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let mut raw = vec![0u8; f.hidden as usize * 2];
    dev.read(&out, 0, &mut raw).unwrap();
    let got: Vec<f64> = raw
        .chunks_exact(2)
        .map(|c| half::f16::from_bits(u16::from_le_bytes(c.try_into().unwrap())).to_f64())
        .collect();

    let want = f64s(&f.blobs[&format!("true_{second}")]);
    let other = f64s(&f.blobs[&format!("true_{first}")]);
    assert!(rel_l2(&got, &want) < 1.0e-3, "pobrano nie ten wiersz");
    assert!(
        rel_l2(&got, &other) > 1.0e-1,
        "oba wiersze są zbyt podobne, żeby ten test cokolwiek rozstrzygał"
    );
}

#[test]
fn residual_add_sums_in_f32() {
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let kernel = dev
        .load_module(msl::RESIDUAL_ADD_SOURCE.as_bytes())
        .unwrap()
        .kernel(msl::RESIDUAL_ADD_NAME)
        .unwrap();
    let stream = dev.create_stream().unwrap();

    // Duża aktywacja i mała poprawka: dodawanie w f16 gubi drugi składnik,
    // a to on niesie informację przez czterdzieści warstw.
    let n = 64usize;
    let a: Vec<half::f16> = (0..n).map(|_| half::f16::from_f32(1024.0)).collect();
    let b: Vec<f32> = (0..n).map(|i| 0.5 + i as f32 * 0.25).collect();

    let a_bytes: Vec<u8> = a.iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
    let b_bytes: Vec<u8> = b.iter().flat_map(|v| v.to_le_bytes()).collect();
    let upload = |bytes: &[u8]| {
        let buf = dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(bytes, &buf, 0).unwrap();
        buf
    };
    let out = dev.alloc(n * 2, MemKind::Device, Pool::Activations).unwrap();

    let args = LaunchArgs::new()
        .buf(&out)
        .buf(&upload(&a_bytes))
        .buf(&upload(&b_bytes))
        .scalar(n as u32);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::elementwise_groups(n as u32), 1, 1),
            block: (msl::ELEMENTWISE_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .unwrap();
    stream.synchronize().unwrap();

    let mut raw = vec![0u8; n * 2];
    dev.read(&out, 0, &mut raw).unwrap();
    for (i, chunk) in raw.chunks_exact(2).enumerate() {
        let got = half::f16::from_bits(u16::from_le_bytes(chunk.try_into().unwrap())).to_f32();
        let want = half::f16::from_f32(1024.0 + b[i]).to_f32();
        assert_eq!(got, want, "element {i}");
    }
}
