// ===== File: deepseek_v4_attention.rs — ścieżka Q/KV uwagi wobec oracle'a =====
//
// Test przypina semantykę miksera DeepSeeka V4, zanim powstaną kernele GPU.
// Wzorzec pochodzi z implementacji referencyjnej modelu (`inference/model.py`),
// policzonej na PRAWDZIWYCH wagach i zrzuconej do pliku; tutaj odtwarzamy tę
// samą matematykę i porównujemy.
//
// Cztery rzeczy, w których łatwo się pomylić, a które nie krzyczą przy pomyłce:
//
//  1. rope obraca pary SĄSIADUJĄCE (2i, 2i+1), a nie połówki wektora — układ
//     NeoX, którego używa reszta FORGE, dałby inne, ale wciąż „sensowne" liczby,
//  2. rope obejmuje tylko OSTATNIE 64 wymiary głowicy z 512,
//  3. Q dostaje DRUGĄ normalizację RMS, per głowica i BEZ wagi, już po projekcji,
//  4. warstwy z kompresją KV używają YaRN i bazy 160000, a pozostałe czystego
//     rope z bazą 10000.
//
// Przy okazji jest to walidacja od końca do końca konwersji wag FP8 na skalę
// wierszową: projekcje liczone są na wagach przepuszczonych przez tę ścieżkę.

use std::path::PathBuf;

use forge_formats::nvfp4::{deepseek_fp8_to_row_scaled, f8e4m3_to_f32, f8e8m0_to_f32};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_types::DType;

const LAYER: usize = 2;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json").is_file().then_some(dir)
}

/// Zrzut aktywacji referencyjnych: nagłówek z pięcioma i32, potem x, qr, q, kv.
struct Oracle {
    seqlen: usize,
    dim: usize,
    n_heads: usize,
    head_dim: usize,
    rope_head_dim: usize,
    x: Vec<f32>,
    qr: Vec<f32>,
    q: Vec<f32>,
    kv: Vec<f32>,
}

fn load_oracle() -> Option<Oracle> {
    let path = std::env::var("FORGE_DEEPSEEK_V4_ORACLE").ok()?;
    let bytes = std::fs::read(path).ok()?;
    let head: Vec<i32> = bytes[..20]
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let (seqlen, dim, n_heads, head_dim, rope_head_dim) = (
        head[0] as usize,
        head[1] as usize,
        head[2] as usize,
        head[3] as usize,
        head[4] as usize,
    );
    let mut offset = 20;
    let mut take = |n: usize| {
        let slice: Vec<f32> = bytes[offset..offset + n * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        offset += n * 4;
        slice
    };
    let x = take(seqlen * dim);
    let qr = take(seqlen * 1024);
    let q = take(seqlen * n_heads * head_dim);
    let kv = take(seqlen * head_dim);
    Some(Oracle {
        seqlen,
        dim,
        n_heads,
        head_dim,
        rope_head_dim,
        x,
        qr,
        q,
        kv,
    })
}

/// Waga FP8 wczytana przez PRODUKCYJNĄ konwersję na skalę wierszową i rozwinięta
/// do f32 — tak, jak zobaczy ją kernel.
fn load_row_scaled(st: &ShardedSafeTensors, name: &str) -> (Vec<f32>, usize, usize) {
    let info = st.tensor(name).expect(name);
    let (rows, cols) = (info.shape[0], info.shape[1]);
    let scale_name = format!("{}.scale", name.strip_suffix(".weight").unwrap());
    let scale_info = st.tensor(&scale_name).expect(&scale_name);
    let tile = cols / scale_info.shape[1];
    let (bytes, row_scales) = deepseek_fp8_to_row_scaled(
        st.data(name).unwrap(),
        st.data(&scale_name).unwrap(),
        rows,
        cols,
        tile,
    )
    .unwrap();
    let mut out = vec![0f32; rows * cols];
    for row in 0..rows {
        for col in 0..cols {
            out[row * cols + col] = f8e4m3_to_f32(bytes[row * cols + col]) * row_scales[row];
        }
    }
    (out, rows, cols)
}

fn load_vector(st: &ShardedSafeTensors, name: &str) -> Vec<f32> {
    let info = st.tensor(name).expect(name);
    let data = st.data(name).unwrap();
    match info.dtype {
        DType::BF16 => data
            .chunks_exact(2)
            .map(|c| f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16))
            .collect(),
        DType::F32 => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        other => panic!("{name}: nieoczekiwany typ {other:?}"),
    }
}

/// `y[r] = sum_c w[r, c] * x[c]`, akumulacja w f32 jak w referencji.
fn project(w: &[f32], x: &[f32], rows: usize, cols: usize) -> Vec<f32> {
    (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| w[row * cols + col] * x[col])
                .sum::<f32>()
        })
        .collect()
}

fn rms_norm(x: &[f32], weight: &[f32], eps: f32) -> Vec<f32> {
    let mean = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let inv = (mean + eps).sqrt().recip();
    x.iter()
        .zip(weight)
        .map(|(v, w)| w * (v * inv))
        .collect()
}

/// Częstotliwości rope z interpolacją YaRN — `original_seq_len == 0` wyłącza ją.
fn rope_freqs(
    dim: usize,
    base: f32,
    original_seq_len: usize,
    factor: f32,
    beta_fast: f32,
    beta_slow: f32,
) -> Vec<f32> {
    let mut freqs: Vec<f32> = (0..dim / 2)
        .map(|i| 1.0 / base.powf(2.0 * i as f32 / dim as f32))
        .collect();
    if original_seq_len == 0 {
        return freqs;
    }
    let correction_dim = |rotations: f32| -> f32 {
        dim as f32 * (original_seq_len as f32 / (rotations * 2.0 * std::f32::consts::PI)).ln()
            / (2.0 * base.ln())
    };
    let low = correction_dim(beta_fast).floor().max(0.0);
    let high = correction_dim(beta_slow).ceil().min(dim as f32 - 1.0);
    for (i, freq) in freqs.iter_mut().enumerate() {
        let ramp = if (high - low).abs() < f32::EPSILON {
            ((i as f32 - low) / 0.001).clamp(0.0, 1.0)
        } else {
            ((i as f32 - low) / (high - low)).clamp(0.0, 1.0)
        };
        let smooth = 1.0 - ramp;
        *freq = *freq / factor * (1.0 - smooth) + *freq * smooth;
    }
    freqs
}

/// Obraca pary SĄSIADUJĄCE (2i, 2i+1) — nie połówki wektora.
fn apply_rope(slice: &mut [f32], freqs: &[f32], pos: usize) {
    for (i, freq) in freqs.iter().enumerate() {
        let angle = pos as f32 * freq;
        let (sin, cos) = angle.sin_cos();
        let (a, b) = (slice[2 * i], slice[2 * i + 1]);
        slice[2 * i] = a * cos - b * sin;
        slice[2 * i + 1] = a * sin + b * cos;
    }
}

fn relative_l2(got: &[f32], want: &[f32]) -> f32 {
    let num: f64 = got
        .iter()
        .zip(want)
        .map(|(g, w)| ((g - w) as f64).powi(2))
        .sum();
    let den: f64 = want.iter().map(|w| (*w as f64).powi(2)).sum();
    (num / den.max(f64::MIN_POSITIVE)).sqrt() as f32
}

#[test]
fn q_and_kv_path_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let p = format!("layers.{LAYER}.attn");
    let (wq_a, q_rank, _) = load_row_scaled(&st, &format!("{p}.wq_a.weight"));
    let (wq_b, q_out, _) = load_row_scaled(&st, &format!("{p}.wq_b.weight"));
    let (wkv, kv_out, _) = load_row_scaled(&st, &format!("{p}.wkv.weight"));
    let q_norm_w = load_vector(&st, &format!("{p}.q_norm.weight"));
    let kv_norm_w = load_vector(&st, &format!("{p}.kv_norm.weight"));
    let eps = 1e-6f32;

    // Warstwa 2 ma kompresję KV (ratio 4), więc rope idzie z YaRN i bazą 160000.
    let freqs = rope_freqs(oracle.rope_head_dim, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let nope = oracle.head_dim - oracle.rope_head_dim;

    let mut qr_all = Vec::with_capacity(oracle.seqlen * q_rank);
    let mut q_all = Vec::with_capacity(oracle.q.len());
    let mut kv_all = Vec::with_capacity(oracle.kv.len());
    for pos in 0..oracle.seqlen {
        let x = &oracle.x[pos * oracle.dim..(pos + 1) * oracle.dim];

        let qr = rms_norm(&project(&wq_a, x, q_rank, oracle.dim), &q_norm_w, eps);
        let mut q = project(&wq_b, &qr, q_out, q_rank);
        for head in 0..oracle.n_heads {
            let slot = &mut q[head * oracle.head_dim..(head + 1) * oracle.head_dim];
            // Druga normalizacja RMS: per głowica i BEZ wagi.
            let mean = slot.iter().map(|v| v * v).sum::<f32>() / slot.len() as f32;
            let inv = (mean + eps).sqrt().recip();
            slot.iter_mut().for_each(|v| *v *= inv);
            apply_rope(&mut slot[nope..], &freqs, pos);
        }

        let mut kv = rms_norm(&project(&wkv, x, kv_out, oracle.dim), &kv_norm_w, eps);
        apply_rope(&mut kv[nope..], &freqs, pos);

        qr_all.extend_from_slice(&qr);
        q_all.extend_from_slice(&q);
        kv_all.extend_from_slice(&kv);
    }

    let qr_err = relative_l2(&qr_all, &oracle.qr);
    let q_err = relative_l2(&q_all, &oracle.q);
    let kv_err = relative_l2(&kv_all, &oracle.kv);
    eprintln!("qr {qr_err:.3e}, q {q_err:.3e}, kv {kv_err:.3e}");
    // Próg mieści rozjazd sumowania f32 i stratę konwersji wag na skalę
    // wierszową (zmierzoną osobno na 4-12e-7), ale nie zmieściłby złego układu
    // rope, pominiętej normalizacji ani błędnej bazy.
    assert!(qr_err < 1e-5, "zejście LoRA Q rozjeżdża się o {qr_err:.3e}");
    assert!(q_err < 1e-5, "ścieżka Q rozjeżdża się o {q_err:.3e}");
    assert!(kv_err < 1e-5, "ścieżka KV rozjeżdża się o {kv_err:.3e}");
}

/// Dowód, że próg testu ma moc rozdzielczą: układ NeoX (obrót połówek wektora)
/// zamiast par sąsiadujących musi go przekroczyć.
#[test]
fn neox_rope_layout_would_be_caught() {
    let freqs = rope_freqs(64, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let base: Vec<f32> = (0..64).map(|i| ((i * 37 % 19) as f32) - 9.0).collect();

    let mut paired = base.clone();
    apply_rope(&mut paired, &freqs, 5);

    // Wariant NeoX: para to (i, i + dim/2).
    let mut neox = base.clone();
    for (i, freq) in freqs.iter().enumerate() {
        let angle = 5.0 * freq;
        let (sin, cos) = angle.sin_cos();
        let (a, b) = (base[i], base[i + 32]);
        neox[i] = a * cos - b * sin;
        neox[i + 32] = a * sin + b * cos;
    }
    assert!(
        relative_l2(&neox, &paired) > 1e-2,
        "oba układy rope dają ten sam wynik — test nie odróżniłby pomyłki"
    );
}
