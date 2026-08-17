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

use forge_formats::nvfp4::{deepseek_expert_to_gguf, deepseek_fp8_to_row_scaled, f8e4m3_to_f32};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_types::DType;

const LAYER: usize = 2;

fn checkpoint_dir() -> Option<PathBuf> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    dir.join("model.safetensors.index.json")
        .is_file()
        .then_some(dir)
}

/// Kursor po zrzucie: pola idą po sobie w kolejności zapisu.
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn floats(&mut self, n: usize) -> Vec<f32> {
        let out = self.bytes[self.offset..self.offset + n * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        self.offset += n * 4;
        out
    }

    fn ints(&mut self, n: usize) -> Vec<i32> {
        let out = self.bytes[self.offset..self.offset + n * 4]
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        self.offset += n * 4;
        out
    }
}

/// Zrzut aktywacji referencyjnych: nagłówek, potem ścieżka uwagi i ścieżka MoE.
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
    gate_layer: usize,
    topk: usize,
    n_experts: usize,
    gate_x: Vec<f32>,
    indices: Vec<i32>,
    gate_weights: Vec<f32>,
    expert_out: Vec<f32>,
    o_groups: usize,
    o_lora_rank: usize,
    attn_in: Vec<f32>,
    attn_out: Vec<f32>,
    ratio: usize,
    compressed: Vec<f32>,
    index_n_heads: usize,
    index_head_dim: usize,
    index_kv: Vec<f32>,
    index_score: Vec<f32>,
    n_topk: usize,
    topk_idxs: Vec<i32>,
    sparse_out: Vec<f32>,
    hc: usize,
    hc_in: Vec<f32>,
    hc_block: Vec<f32>,
    hc_reduced: Vec<f32>,
    hc_expanded: Vec<f32>,
    decode_steps: usize,
    decode_x: Vec<f32>,
    decode_compressed: Vec<f32>,
    vocab: usize,
    head_reduced: Vec<f32>,
    logits: Vec<f32>,
    token_ids: Vec<i32>,
    hash_indices: Vec<i32>,
    hash_weights: Vec<f32>,
}

fn load_oracle() -> Option<Oracle> {
    let path = std::env::var("FORGE_DEEPSEEK_V4_ORACLE").ok()?;
    let bytes = std::fs::read(path).ok()?;
    let head: Vec<i32> = bytes[..72]
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
    let (gate_layer, topk, n_experts) = (head[5] as usize, head[6] as usize, head[7] as usize);
    let (o_groups, o_lora_rank) = (head[8] as usize, head[9] as usize);
    let ratio = head[10] as usize;
    let (index_n_heads, index_head_dim) = (head[11] as usize, head[12] as usize);
    let n_topk = head[14] as usize;
    let hc = head[15] as usize;
    let decode_steps = head[16] as usize;
    let vocab = head[17] as usize;
    let mut cursor = Cursor {
        bytes: &bytes,
        offset: 72,
    };
    let x = cursor.floats(seqlen * dim);
    let qr = cursor.floats(seqlen * 1024);
    let q = cursor.floats(seqlen * n_heads * head_dim);
    let kv = cursor.floats(seqlen * head_dim);
    let gate_x = cursor.floats(seqlen * dim);
    let indices = cursor.ints(seqlen * topk);
    let gate_weights = cursor.floats(seqlen * topk);
    let expert_out = cursor.floats(seqlen * dim);
    let attn_in = cursor.floats(seqlen * n_heads * head_dim);
    let attn_out = cursor.floats(seqlen * dim);
    let compressed = cursor.floats(seqlen / ratio * head_dim);
    let index_kv = cursor.floats(seqlen / ratio * index_head_dim);
    let index_score = cursor.floats(seqlen * (seqlen / ratio));
    let topk_idxs = cursor.ints(seqlen * n_topk);
    let sparse_out = cursor.floats(seqlen * n_heads * head_dim);
    let hc_in = cursor.floats(seqlen * hc * dim);
    let hc_block = cursor.floats(seqlen * dim);
    let hc_reduced = cursor.floats(seqlen * dim);
    let hc_expanded = cursor.floats(seqlen * hc * dim);
    let decode_x = cursor.floats(decode_steps * dim);
    let decode_compressed = cursor.floats(head_dim);
    let head_reduced = cursor.floats(seqlen * dim);
    let logits = cursor.floats(vocab);
    let token_ids = cursor.ints(seqlen);
    let hash_indices = cursor.ints(seqlen * topk);
    let hash_weights = cursor.floats(seqlen * topk);
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
        gate_layer,
        topk,
        n_experts,
        gate_x,
        indices,
        gate_weights,
        expert_out,
        o_groups,
        o_lora_rank,
        attn_in,
        attn_out,
        ratio,
        compressed,
        index_n_heads,
        index_head_dim,
        index_kv,
        index_score,
        n_topk,
        topk_idxs,
        sparse_out,
        hc,
        hc_in,
        hc_block,
        hc_reduced,
        hc_expanded,
        decode_steps,
        decode_x,
        decode_compressed,
        vocab,
        head_reduced,
        logits,
        token_ids,
        hash_indices,
        hash_weights,
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
    x.iter().zip(weight).map(|(v, w)| w * (v * inv)).collect()
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

/// Ekspert NVFP4 rozwinięty przez PRODUKCYJNE przepakowanie do układu
/// jednobuforowego, tak jak zobaczy go kernel.
fn load_expert(st: &ShardedSafeTensors, base: &str) -> (Vec<f32>, usize, usize) {
    let names =
        forge_formats::nvfp4::DeepseekNvFp4Names::for_weight(&format!("{base}.weight")).unwrap();
    let info = st.tensor(&names.packed).expect(&names.packed);
    let gs = st.data(&names.global_scale).unwrap();
    let global = f32::from_le_bytes([gs[0], gs[1], gs[2], gs[3]]);
    let repacked = deepseek_expert_to_gguf(
        st.data(&names.packed).unwrap(),
        &info.shape,
        st.data(&names.scale).unwrap(),
        global,
    )
    .unwrap();
    let decoded = forge_formats::dequant::dequantize_to_f32(
        DType::U8,
        forge_types::QuantKind::NVFP4Gguf,
        &repacked.blocks,
        repacked.rows * repacked.cols,
    )
    .unwrap();
    let scaled: Vec<f32> = decoded.iter().map(|v| v * repacked.output_scale).collect();
    (scaled, repacked.rows, repacked.cols)
}

/// Bramka MoE ma trzy szczegóły, które przy pomyłce nie krzyczą, tylko zmieniają
/// wybór ekspertów albo ich wagi:
///
///  1. bias wchodzi WYŁĄCZNIE do wyboru top-k; wagi bierze się z wyników BEZ niego,
///  2. wynik to `sqrt(softplus(logit))`, nie softmax ani sigmoid,
///  3. wagi są normalizowane do sumy 1, a dopiero potem mnożone przez `route_scale`.
#[test]
fn moe_gate_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let g = format!("layers.{}.ffn.gate", oracle.gate_layer);
    let weight = load_vector(&st, &format!("{g}.weight"));
    let bias = load_vector(&st, &format!("{g}.bias"));
    let route_scale = 1.5f32;

    for token in 0..oracle.seqlen {
        let x = &oracle.gate_x[token * oracle.dim..(token + 1) * oracle.dim];
        let scores: Vec<f32> = (0..oracle.n_experts)
            .map(|e| {
                let logit: f32 = (0..oracle.dim)
                    .map(|c| weight[e * oracle.dim + c] * x[c])
                    .sum();
                // softplus stabilne numerycznie, potem pierwiastek.
                let softplus = if logit > 20.0 {
                    logit
                } else {
                    logit.exp().ln_1p()
                };
                softplus.sqrt()
            })
            .collect();

        // Wybór po wynikach Z biasem, wagi z wyników BEZ biasu.
        let mut ranked: Vec<usize> = (0..oracle.n_experts).collect();
        ranked.sort_by(|a, b| (scores[*b] + bias[*b]).total_cmp(&(scores[*a] + bias[*a])));
        let chosen = &ranked[..oracle.topk];
        let want_idx = &oracle.indices[token * oracle.topk..(token + 1) * oracle.topk];
        let mut got: Vec<i32> = chosen.iter().map(|e| *e as i32).collect();
        let mut want = want_idx.to_vec();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "token {token}: inny zestaw ekspertów");

        // Wagi liczone w kolejności, w jakiej wybrał je oracle.
        let raw: Vec<f32> = want_idx.iter().map(|e| scores[*e as usize]).collect();
        let sum: f32 = raw.iter().sum();
        let want_w = &oracle.gate_weights[token * oracle.topk..(token + 1) * oracle.topk];
        for (j, r) in raw.iter().enumerate() {
            let got_w = r / sum * route_scale;
            assert!(
                (got_w - want_w[j]).abs() < 1e-4,
                "token {token}, pozycja {j}: waga {got_w} zamiast {}",
                want_w[j]
            );
        }
    }
}

/// SwiGLU eksperta obcina bramkę TYLKO od góry, a wejście obustronnie — i robi
/// to PRZED mnożeniem. Test liczy jednego prawdziwego eksperta NVFP4 od wagi po
/// wyjście, więc waliduje też przepakowanie.
#[test]
fn expert_swiglu_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let base = format!("layers.{}.ffn.experts.0", oracle.gate_layer);
    let (w1, inter, dim) = load_expert(&st, &format!("{base}.w1"));
    let (w3, _, _) = load_expert(&st, &format!("{base}.w3"));
    let (w2, out_dim, _) = load_expert(&st, &format!("{base}.w2"));
    let limit = 10.0f32;

    let mut got = Vec::with_capacity(oracle.expert_out.len());
    for token in 0..oracle.seqlen {
        let x = &oracle.gate_x[token * dim..(token + 1) * dim];
        let gate = project(&w1, x, inter, dim);
        let up = project(&w3, x, inter, dim);
        let act: Vec<f32> = gate
            .iter()
            .zip(&up)
            .map(|(g, u)| {
                let g = g.min(limit);
                let u = u.clamp(-limit, limit);
                (g / (1.0 + (-g).exp())) * u
            })
            .collect();
        got.extend_from_slice(&project(&w2, &act, out_dim, inter));
    }

    let err = relative_l2(&got, &oracle.expert_out);
    eprintln!("ekspert SwiGLU: względne L2 = {err:.3e}");
    assert!(err < 1e-4, "ekspert rozjeżdża się o {err:.3e}");
}

/// Ścieżka wyjścia uwagi. Dwie rzeczy odróżniają ją od zwykłej projekcji O:
///
///  1. na ostatnich 64 wymiarach głowicy nakładane jest rope ODWROTNE (sprzężenie
///     tego samego obrotu) — pomyłka w znaku daje wynik wyglądający sensownie,
///  2. wyjście jest dzielone na 8 grup, każda mnożona przez WŁASNY blok `wo_a`,
///     a dopiero złączenie wchodzi w `wo_b`. Potraktowanie `wo_a` jako jednej
///     macierzy daje poprawny kształt i błędne liczby.
#[test]
fn attention_output_path_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let p = format!("layers.{LAYER}.attn");
    let (wo_a, _, _) = load_row_scaled(&st, &format!("{p}.wo_a.weight"));
    let (wo_b, out_rows, out_cols) = load_row_scaled(&st, &format!("{p}.wo_b.weight"));
    let freqs = rope_freqs(oracle.rope_head_dim, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let nope = oracle.head_dim - oracle.rope_head_dim;
    let per_group = oracle.n_heads * oracle.head_dim / oracle.o_groups;

    let mut got = Vec::with_capacity(oracle.attn_out.len());
    for pos in 0..oracle.seqlen {
        let span = oracle.n_heads * oracle.head_dim;
        let mut o = oracle.attn_in[pos * span..(pos + 1) * span].to_vec();
        for head in 0..oracle.n_heads {
            let slot = &mut o[head * oracle.head_dim..(head + 1) * oracle.head_dim];
            apply_inverse_rope(&mut slot[nope..], &freqs, pos);
        }
        // Każda grupa ma własny blok wo_a o kształcie [o_lora_rank, per_group].
        let mut lora = vec![0f32; oracle.o_groups * oracle.o_lora_rank];
        for group in 0..oracle.o_groups {
            let block =
                &wo_a[group * oracle.o_lora_rank * per_group..][..oracle.o_lora_rank * per_group];
            let chunk = &o[group * per_group..(group + 1) * per_group];
            let out = project(block, chunk, oracle.o_lora_rank, per_group);
            lora[group * oracle.o_lora_rank..(group + 1) * oracle.o_lora_rank]
                .copy_from_slice(&out);
        }
        got.extend_from_slice(&project(&wo_b, &lora, out_rows, out_cols));
    }

    let err = relative_l2(&got, &oracle.attn_out);
    eprintln!("wyjście uwagi: względne L2 = {err:.3e}");
    assert!(err < 1e-5, "ścieżka wyjścia rozjeżdża się o {err:.3e}");
}

/// Rope odwrotne to sprzężenie obrotu: ten sam kąt, przeciwny znak sinusa.
fn apply_inverse_rope(slice: &mut [f32], freqs: &[f32], pos: usize) {
    for (i, freq) in freqs.iter().enumerate() {
        let angle = pos as f32 * freq;
        let (sin, cos) = angle.sin_cos();
        let (a, b) = (slice[2 * i], slice[2 * i + 1]);
        slice[2 * i] = a * cos + b * sin;
        slice[2 * i + 1] = -a * sin + b * cos;
    }
}

/// Symulacja kwantyzacji aktywacji do FP8 z zaokrągleniem skali do potęgi
/// dwójki — model był tak trenowany (QAT), więc pominięcie tego kroku zmienia
/// wartości wchodzące do cache'u KV.
fn act_quant_inplace(values: &mut [f32], block: usize) {
    for group in values.chunks_mut(block) {
        let amax = group.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-4);
        let scale = (amax / 448.0).log2().ceil().exp2();
        for v in group.iter_mut() {
            *v = f8e4m3_round(*v / scale) * scale;
        }
    }
}

/// Zaokrągla do najbliższej wartości E4M3 (do najbliższej parzystej przy remisie).
fn f8e4m3_round(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let clamped = v.clamp(-448.0, 448.0);
    let mut best = 0f32;
    let mut best_err = f32::INFINITY;
    for code in 0u16..256 {
        let candidate = f8e4m3_to_f32(code as u8);
        if !candidate.is_finite() {
            continue;
        }
        let err = (candidate - clamped).abs();
        if err < best_err || (err == best_err && candidate.to_bits() % 2 == 0) {
            best_err = err;
            best = candidate;
        }
    }
    best
}

/// Kompresor strumienia KV. Dla `ratio == 4` okna są Z ZAKŁADKĄ: projekcje dają
/// dwa razy szerszy wektor, którego pierwsza połowa opisuje okno przesunięte o
/// jeden blok wstecz. Potraktowanie go jako zwykłego poolingu daje poprawny
/// kształt i błędne wartości.
#[test]
fn kv_compressor_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let c = format!("layers.{LAYER}.attn.compressor");
    let wkv = load_vector(&st, &format!("{c}.wkv.weight"));
    let wgate = load_vector(&st, &format!("{c}.wgate.weight"));
    let ape = load_vector(&st, &format!("{c}.ape"));
    let norm_w = load_vector(&st, &format!("{c}.norm.weight"));
    let eps = 1e-6f32;

    let ratio = oracle.ratio;
    let overlap = ratio == 4;
    let d = oracle.head_dim;
    let wide = if overlap { 2 * d } else { d };
    let blocks = oracle.seqlen / ratio;
    let freqs = rope_freqs(oracle.rope_head_dim, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let nope = d - oracle.rope_head_dim;

    // Projekcje per token: wartości i wyniki bramki, plus kodowanie pozycji
    // wewnątrz okna.
    let mut kv = vec![0f32; oracle.seqlen * wide];
    let mut score = vec![0f32; oracle.seqlen * wide];
    for t in 0..oracle.seqlen {
        let x = &oracle.x[t * oracle.dim..(t + 1) * oracle.dim];
        kv[t * wide..(t + 1) * wide].copy_from_slice(&project(&wkv, x, wide, oracle.dim));
        let mut s = project(&wgate, x, wide, oracle.dim);
        let slot = t % ratio;
        for (i, v) in s.iter_mut().enumerate() {
            *v += ape[slot * wide + i];
        }
        score[t * wide..(t + 1) * wide].copy_from_slice(&s);
    }

    let mut got = Vec::with_capacity(blocks * d);
    for block in 0..blocks {
        // Okno: przy zakładce pierwsze `ratio` pozycji bierze DRUGĄ połowę
        // wymiarów poprzedniego bloku, a dalsze — pierwszą połowę bieżącego.
        let window = if overlap { 2 * ratio } else { ratio };
        let mut win_kv = vec![0f32; window * d];
        let mut win_sc = vec![f32::NEG_INFINITY; window * d];
        for slot in 0..window {
            let (token, half) = if !overlap {
                (block * ratio + slot, 0)
            } else if slot < ratio {
                if block == 0 {
                    continue;
                }
                ((block - 1) * ratio + slot, 0)
            } else {
                (block * ratio + slot - ratio, 1)
            };
            let src = token * wide + half * d;
            win_kv[slot * d..(slot + 1) * d].copy_from_slice(&kv[src..src + d]);
            win_sc[slot * d..(slot + 1) * d].copy_from_slice(&score[src..src + d]);
        }
        // Softmax po oknie, osobno dla każdego wymiaru.
        let mut pooled = vec![0f32; d];
        for dim_i in 0..d {
            let mut max = f32::NEG_INFINITY;
            for slot in 0..window {
                max = max.max(win_sc[slot * d + dim_i]);
            }
            let mut denom = 0f32;
            for slot in 0..window {
                denom += (win_sc[slot * d + dim_i] - max).exp();
            }
            let mut acc = 0f32;
            for slot in 0..window {
                let w = (win_sc[slot * d + dim_i] - max).exp() / denom;
                acc += w * win_kv[slot * d + dim_i];
            }
            pooled[dim_i] = acc;
        }
        let mut normed = rms_norm(&pooled, &norm_w, eps);
        apply_rope(&mut normed[nope..], &freqs, block * ratio);
        act_quant_inplace(&mut normed[..nope], 64);
        got.extend_from_slice(&normed);
    }

    let err = relative_l2(&got, &oracle.compressed);
    eprintln!("kompresor KV: względne L2 = {err:.3e}");
    assert!(err < 1e-4, "kompresor rozjeżdża się o {err:.3e}");
}

/// Transformata Walsha-Hadamarda po ostatnim wymiarze, znormalizowana `1/sqrt(n)`.
fn hadamard(values: &mut [f32]) {
    let n = values.len();
    assert!(n.is_power_of_two());
    let mut step = 1;
    while step < n {
        for base in (0..n).step_by(2 * step) {
            for i in 0..step {
                let (a, b) = (values[base + i], values[base + step + i]);
                values[base + i] = a + b;
                values[base + step + i] = a - b;
            }
        }
        step *= 2;
    }
    // Rotacja liczy się w f32, ale wynik wraca do bf16 — referencja trzyma ten
    // tensor w bf16, a zaokrąglenie potrafi przesunąć maksimum grupy przez
    // granicę potęgi dwójki i zmienić skalę całej grupy przy kwantyzacji FP4.
    let scale = (n as f32).sqrt().recip();
    values.iter_mut().for_each(|v| *v = to_bf16(*v * scale));
}

/// Kwantyzacja aktywacji do FP4 (E2M1) ze skalą zaokrągloną do potęgi dwójki.
fn fp4_act_quant_inplace(values: &mut [f32], block: usize) {
    const CODEBOOK: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
    for group in values.chunks_mut(block) {
        let amax = group
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()))
            .max(6.0 * (2f32).powi(-126));
        let scale = (amax / 6.0).log2().ceil().exp2();
        for v in group.iter_mut() {
            let scaled = (*v / scale).clamp(-6.0, 6.0);
            let sign = if scaled < 0.0 { -1.0 } else { 1.0 };
            let mag = scaled.abs();
            let nearest = CODEBOOK
                .iter()
                .copied()
                .min_by(|a, b| (a - mag).abs().total_cmp(&(b - mag).abs()))
                .unwrap();
            *v = sign * nearest * scale;
        }
    }
}

fn to_bf16(v: f32) -> f32 {
    let bits = v.to_bits();
    let round = ((bits >> 16) & 1) + 0x7FFF;
    f32::from_bits((bits + round) & 0xFFFF_0000)
}

/// Indekser rzadkiej uwagi. Ma własny kompresor — z rotacją Hadamarda i
/// kwantyzacją FP4 zamiast FP8 — a wynik punktowania to `relu(q·k)` ważone per
/// głowica i zsumowane. Pominięcie rotacji albo kwantyzacji daje wartości tego
/// samego rzędu, ale inny wybór pozycji.
#[test]
fn sparse_indexer_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let ix = format!("layers.{LAYER}.attn.indexer");
    let (wq_b, q_out, q_in) = load_row_scaled(&st, &format!("{ix}.wq_b.weight"));
    let wproj = load_vector(&st, &format!("{ix}.weights_proj.weight"));
    let wkv = load_vector(&st, &format!("{ix}.compressor.wkv.weight"));
    let wgate = load_vector(&st, &format!("{ix}.compressor.wgate.weight"));
    let ape = load_vector(&st, &format!("{ix}.compressor.ape"));
    let norm_w = load_vector(&st, &format!("{ix}.compressor.norm.weight"));
    let eps = 1e-6f32;

    let ratio = oracle.ratio;
    let d = oracle.index_head_dim;
    let wide = 2 * d;
    let blocks = oracle.seqlen / ratio;
    let freqs = rope_freqs(oracle.rope_head_dim, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let nope = d - oracle.rope_head_dim;

    // Kompresor indeksera.
    let mut kv = vec![0f32; oracle.seqlen * wide];
    let mut score = vec![0f32; oracle.seqlen * wide];
    for t in 0..oracle.seqlen {
        let x = &oracle.x[t * oracle.dim..(t + 1) * oracle.dim];
        kv[t * wide..(t + 1) * wide].copy_from_slice(&project(&wkv, x, wide, oracle.dim));
        let mut s = project(&wgate, x, wide, oracle.dim);
        for (i, v) in s.iter_mut().enumerate() {
            *v += ape[(t % ratio) * wide + i];
        }
        score[t * wide..(t + 1) * wide].copy_from_slice(&s);
    }
    let mut index_kv = Vec::with_capacity(blocks * d);
    for block in 0..blocks {
        let window = 2 * ratio;
        let mut win_kv = vec![0f32; window * d];
        let mut win_sc = vec![f32::NEG_INFINITY; window * d];
        for slot in 0..window {
            let (token, half) = if slot < ratio {
                if block == 0 {
                    continue;
                }
                ((block - 1) * ratio + slot, 0)
            } else {
                (block * ratio + slot - ratio, 1)
            };
            let src = token * wide + half * d;
            win_kv[slot * d..(slot + 1) * d].copy_from_slice(&kv[src..src + d]);
            win_sc[slot * d..(slot + 1) * d].copy_from_slice(&score[src..src + d]);
        }
        let mut pooled = vec![0f32; d];
        for dim_i in 0..d {
            let mut max = f32::NEG_INFINITY;
            for slot in 0..window {
                max = max.max(win_sc[slot * d + dim_i]);
            }
            let mut denom = 0f32;
            let mut acc = 0f32;
            for slot in 0..window {
                let w = (win_sc[slot * d + dim_i] - max).exp();
                denom += w;
                acc += w * win_kv[slot * d + dim_i];
            }
            pooled[dim_i] = acc / denom;
        }
        let mut normed = rms_norm(&pooled, &norm_w, eps);
        apply_rope(&mut normed[nope..], &freqs, block * ratio);
        normed.iter_mut().for_each(|v| *v = to_bf16(*v));
        hadamard(&mut normed);
        fp4_act_quant_inplace(&mut normed, 32);
        index_kv.extend_from_slice(&normed);
    }
    let kv_err = relative_l2(&index_kv, &oracle.index_kv);
    eprintln!("kompresor indeksera: względne L2 = {kv_err:.3e}");
    assert!(
        kv_err < 1e-3,
        "kompresor indeksera rozjeżdża się o {kv_err:.3e}"
    );

    // Zapytania indeksera i punktowanie pozycji.
    let softmax_scale = (d as f32).sqrt().recip();
    let head_weight_scale = softmax_scale * (oracle.index_n_heads as f32).sqrt().recip();
    let mut got_score = Vec::with_capacity(oracle.seqlen * blocks);
    for t in 0..oracle.seqlen {
        let qr = &oracle.qr[t * q_in..(t + 1) * q_in];
        let mut q = project(&wq_b, qr, q_out, q_in);
        for head in 0..oracle.index_n_heads {
            let slot = &mut q[head * d..(head + 1) * d];
            apply_rope(&mut slot[nope..], &freqs, t);
            slot.iter_mut().for_each(|v| *v = to_bf16(*v));
            hadamard(slot);
            fp4_act_quant_inplace(slot, 32);
        }
        let x = &oracle.x[t * oracle.dim..(t + 1) * oracle.dim];
        let head_w = project(&wproj, x, oracle.index_n_heads, oracle.dim);
        for block in 0..blocks {
            let key = &index_kv[block * d..(block + 1) * d];
            let mut acc = 0f32;
            for head in 0..oracle.index_n_heads {
                let dot: f32 = q[head * d..(head + 1) * d]
                    .iter()
                    .zip(key)
                    .map(|(a, b)| a * b)
                    .sum();
                acc += dot.max(0.0) * (head_w[head] * head_weight_scale);
            }
            got_score.push(acc);
        }
    }
    let err = relative_l2(&got_score, &oracle.index_score);
    eprintln!("punktowanie indeksera: względne L2 = {err:.3e}");
    assert!(err < 1e-3, "indekser rozjeżdża się o {err:.3e}");
}

/// Rzadka uwaga po zebranych indeksach. Dwa szczegóły, które przy pomyłce dają
/// wynik wyglądający poprawnie:
///
///  1. indeks `-1` oznacza pozycję zamaskowaną — jej wynik to `-inf`, a wektor
///     wartości zero; potraktowanie go jako zwykłego indeksu czyta cudzy wiersz,
///  2. kotwica (`attn_sink`) wchodzi WYŁĄCZNIE do mianownika softmaxu, jako
///     dodatkowy logit o zerowym wektorze wartości — nie do licznika.
#[test]
fn sparse_attention_matches_the_reference_implementation() {
    let Some(oracle) = load_oracle() else {
        eprintln!("pomijam: brak zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let Some(dir) = checkpoint_dir() else { return };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let sink = load_vector(&st, &format!("layers.{LAYER}.attn.attn_sink"));

    let d = oracle.head_dim;
    let scale = (d as f32).sqrt().recip();
    // Bufor KV prefillu: najpierw KV tokenów okna, potem wpisy skompresowane.
    let mut kv_full = oracle.kv.clone();
    kv_full.extend_from_slice(&oracle.compressed);

    let mut got = vec![0f32; oracle.sparse_out.len()];
    for t in 0..oracle.seqlen {
        let idxs = &oracle.topk_idxs[t * oracle.n_topk..(t + 1) * oracle.n_topk];
        let valid: Vec<usize> = idxs
            .iter()
            .filter(|i| **i >= 0)
            .map(|i| *i as usize)
            .collect();
        for head in 0..oracle.n_heads {
            let q = &oracle.q[(t * oracle.n_heads + head) * d..][..d];
            let scores: Vec<f32> = valid
                .iter()
                .map(|k| {
                    let key = &kv_full[k * d..(k + 1) * d];
                    q.iter().zip(key).map(|(a, b)| a * b).sum::<f32>() * scale
                })
                .collect();
            let max = scores.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
            let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
            // Kotwica: dodatkowy logit w mianowniku, bez wkładu do licznika.
            let denom: f32 = exps.iter().sum::<f32>() + (sink[head] - max).exp();
            let out = &mut got[(t * oracle.n_heads + head) * d..][..d];
            for (w, k) in exps.iter().zip(&valid) {
                let key = &kv_full[k * d..(k + 1) * d];
                for (o, v) in out.iter_mut().zip(key) {
                    *o += w * v;
                }
            }
            out.iter_mut().for_each(|v| *v /= denom);
        }
    }

    let err = relative_l2(&got, &oracle.sparse_out);
    eprintln!("rzadka uwaga: względne L2 = {err:.3e}");
    assert!(err < 1e-5, "rzadka uwaga rozjeżdża się o {err:.3e}");
}

/// Konstrukcja indeksów prefillu: okno przesuwne jest przyczynowe, a wpis
/// skompresowany `n` staje się widoczny dopiero dla tokenów od `(n+1)*ratio`.
/// Pozycje niedostępne są oznaczane `-1`, nie pomijane.
#[test]
fn prefill_index_construction_matches_the_reference() {
    let Some(oracle) = load_oracle() else {
        eprintln!("pomijam: brak zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let window = oracle.seqlen.min(128);
    let blocks = oracle.seqlen / oracle.ratio;
    for t in 0..oracle.seqlen {
        let row = &oracle.topk_idxs[t * oracle.n_topk..(t + 1) * oracle.n_topk];
        for (slot, want) in row.iter().enumerate().take(window) {
            let first = t.saturating_sub(127);
            let idx = first + slot;
            let expected = if idx > t { -1 } else { idx as i32 };
            assert_eq!(*want, expected, "token {t}, okno {slot}");
        }
        for block in 0..blocks {
            let want = row[window + block];
            let visible = block < (t + 1) / oracle.ratio;
            let expected = if visible {
                (oracle.seqlen + block) as i32
            } else {
                -1
            };
            assert_eq!(want, expected, "token {t}, blok skompresowany {block}");
        }
    }
}

/// Hyper-connections: strumień rezydualny to `hc_mult` kopii stanu, a nie jedna.
/// Blok najpierw redukuje je do jednej ważoną sumą, a po policzeniu uwagi/FFN
/// rozprowadza wynik z powrotem, mieszając kopie macierzą po Sinkhornie.
///
/// Sinkhorn ma tu nieoczywistą kolejność: po softmaksie po wierszach idzie
/// najpierw normalizacja po KOLUMNACH, i dopiero potem `iters - 1` pełnych par
/// wiersz+kolumna. Rozpoczęcie od wierszy daje inną macierz.
#[test]
fn hyper_connections_match_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let hc_fn = load_vector(&st, &format!("layers.{LAYER}.hc_attn_fn"));
    let hc_base = load_vector(&st, &format!("layers.{LAYER}.hc_attn_base"));
    let hc_scale = load_vector(&st, &format!("layers.{LAYER}.hc_attn_scale"));
    let (hc, dim, eps, hc_eps) = (oracle.hc, oracle.dim, 1e-6f32, 1e-6f32);
    let mix_hc = (2 + hc) * hc;
    let wide = hc * dim;

    let mut reduced = Vec::with_capacity(oracle.hc_reduced.len());
    let mut expanded = vec![0f32; oracle.hc_expanded.len()];
    for t in 0..oracle.seqlen {
        let x = &oracle.hc_in[t * wide..(t + 1) * wide];
        let mean = x.iter().map(|v| v * v).sum::<f32>() / wide as f32;
        let rsqrt = (mean + eps).sqrt().recip();
        let mixes: Vec<f32> = (0..mix_hc)
            .map(|m| (0..wide).map(|c| hc_fn[m * wide + c] * x[c]).sum::<f32>() * rsqrt)
            .collect();

        let sigmoid = |v: f32| 1.0 / (1.0 + (-v).exp());
        let pre: Vec<f32> = (0..hc)
            .map(|j| sigmoid(mixes[j] * hc_scale[0] + hc_base[j]) + hc_eps)
            .collect();
        let post: Vec<f32> = (0..hc)
            .map(|j| 2.0 * sigmoid(mixes[j + hc] * hc_scale[1] + hc_base[j + hc]))
            .collect();
        let mut comb = vec![0f32; hc * hc];
        for j in 0..hc {
            for k in 0..hc {
                let idx = j * hc + k + 2 * hc;
                comb[j * hc + k] = mixes[idx] * hc_scale[2] + hc_base[idx];
            }
        }
        // Softmax po wierszach, potem NAJPIERW kolumny, dopiero potem pary.
        for j in 0..hc {
            let row = &mut comb[j * hc..(j + 1) * hc];
            let max = row.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
            let mut sum = 0f32;
            row.iter_mut().for_each(|v| {
                *v = (*v - max).exp();
                sum += *v;
            });
            row.iter_mut().for_each(|v| *v = *v / sum + hc_eps);
        }
        let normalize_cols = |comb: &mut Vec<f32>| {
            for k in 0..hc {
                let sum: f32 = (0..hc).map(|j| comb[j * hc + k]).sum();
                for j in 0..hc {
                    comb[j * hc + k] /= sum + hc_eps;
                }
            }
        };
        normalize_cols(&mut comb);
        for _ in 0..19 {
            for j in 0..hc {
                let sum: f32 = comb[j * hc..(j + 1) * hc].iter().sum();
                comb[j * hc..(j + 1) * hc]
                    .iter_mut()
                    .for_each(|v| *v /= sum + hc_eps);
            }
            normalize_cols(&mut comb);
        }

        // Redukcja HC kopii do jednej.
        let mut y = vec![0f32; dim];
        for copy in 0..hc {
            let src = &x[copy * dim..(copy + 1) * dim];
            for (o, v) in y.iter_mut().zip(src) {
                *o += pre[copy] * v;
            }
        }
        reduced.extend_from_slice(&y);

        // Rozprowadzenie wyjścia bloku z powrotem na HC kopii.
        let blk = &oracle.hc_block[t * dim..(t + 1) * dim];
        for out_copy in 0..hc {
            let dst = &mut expanded[(t * hc + out_copy) * dim..][..dim];
            for (o, v) in dst.iter_mut().zip(blk) {
                *o = post[out_copy] * v;
            }
            for in_copy in 0..hc {
                let src = &x[in_copy * dim..(in_copy + 1) * dim];
                let w = comb[in_copy * hc + out_copy];
                for (o, v) in dst.iter_mut().zip(src) {
                    *o += w * v;
                }
            }
        }
    }

    let pre_err = relative_l2(&reduced, &oracle.hc_reduced);
    let post_err = relative_l2(&expanded, &oracle.hc_expanded);
    eprintln!("hc_pre {pre_err:.3e}, hc_post {post_err:.3e}");
    assert!(pre_err < 1e-5, "redukcja HC rozjeżdża się o {pre_err:.3e}");
    assert!(
        post_err < 1e-5,
        "rozprowadzenie HC rozjeżdża się o {post_err:.3e}"
    );
}

/// Ścieżka DEKODOWANIA kompresora: token po tokenie, ze stanem okna między
/// krokami. Wpis skompresowany powstaje dopiero co `ratio` tokenów, a przy
/// zakładce stan przesuwa się o okno — sloty `[0, ratio)` trzymają poprzednie
/// okno, `[ratio, 2*ratio)` bieżące. Rope bierze pozycję POCZĄTKU okna, nie
/// tokenu, który je domknął.
#[test]
fn kv_compressor_decode_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let c = format!("layers.{LAYER}.attn.compressor");
    let wkv = load_vector(&st, &format!("{c}.wkv.weight"));
    let wgate = load_vector(&st, &format!("{c}.wgate.weight"));
    let ape = load_vector(&st, &format!("{c}.ape"));
    let norm_w = load_vector(&st, &format!("{c}.norm.weight"));
    let eps = 1e-6f32;

    let ratio = oracle.ratio;
    let d = oracle.head_dim;
    let wide = 2 * d;
    let freqs = rope_freqs(oracle.rope_head_dim, 160_000.0, 65_536, 16.0, 32.0, 1.0);
    let nope = d - oracle.rope_head_dim;

    // Stan po prefillu: sloty [0, ratio) to ostatnie okno promptu.
    let mut kv_state = vec![0f32; 2 * ratio * wide];
    let mut sc_state = vec![f32::NEG_INFINITY; 2 * ratio * wide];
    for slot in 0..ratio {
        let token = oracle.seqlen - ratio + slot;
        let x = &oracle.x[token * oracle.dim..(token + 1) * oracle.dim];
        kv_state[slot * wide..(slot + 1) * wide]
            .copy_from_slice(&project(&wkv, x, wide, oracle.dim));
        let mut s = project(&wgate, x, wide, oracle.dim);
        for (i, v) in s.iter_mut().enumerate() {
            *v += ape[slot * wide + i];
        }
        sc_state[slot * wide..(slot + 1) * wide].copy_from_slice(&s);
    }

    let mut produced: Option<Vec<f32>> = None;
    for step in 0..oracle.decode_steps {
        let pos = oracle.seqlen + step;
        let x = &oracle.decode_x[step * oracle.dim..(step + 1) * oracle.dim];
        let slot = ratio + pos % ratio;
        kv_state[slot * wide..(slot + 1) * wide]
            .copy_from_slice(&project(&wkv, x, wide, oracle.dim));
        let mut s = project(&wgate, x, wide, oracle.dim);
        for (i, v) in s.iter_mut().enumerate() {
            *v += ape[(pos % ratio) * wide + i];
        }
        sc_state[slot * wide..(slot + 1) * wide].copy_from_slice(&s);
        if (pos + 1) % ratio != 0 {
            continue;
        }

        // Okno: pierwsza połowa wymiarów poprzedniego okna, druga bieżącego.
        let window = 2 * ratio;
        let mut win_kv = vec![0f32; window * d];
        let mut win_sc = vec![f32::NEG_INFINITY; window * d];
        for w in 0..window {
            let (src_slot, half) = if w < ratio { (w, 0) } else { (w, 1) };
            let src = src_slot * wide + half * d;
            win_kv[w * d..(w + 1) * d].copy_from_slice(&kv_state[src..src + d]);
            win_sc[w * d..(w + 1) * d].copy_from_slice(&sc_state[src..src + d]);
        }
        let mut pooled = vec![0f32; d];
        for dim_i in 0..d {
            let mut max = f32::NEG_INFINITY;
            for w in 0..window {
                max = max.max(win_sc[w * d + dim_i]);
            }
            let mut denom = 0f32;
            let mut acc = 0f32;
            for w in 0..window {
                let weight = (win_sc[w * d + dim_i] - max).exp();
                denom += weight;
                acc += weight * win_kv[w * d + dim_i];
            }
            pooled[dim_i] = acc / denom;
        }
        // Stan przesuwa się o okno.
        kv_state.copy_within(ratio * wide..2 * ratio * wide, 0);
        sc_state.copy_within(ratio * wide..2 * ratio * wide, 0);

        let mut normed = rms_norm(&pooled, &norm_w, eps);
        apply_rope(&mut normed[nope..], &freqs, pos + 1 - ratio);
        act_quant_inplace(&mut normed[..nope], 64);
        produced = Some(normed);
    }

    let got = produced.expect("cztery kroki przy ratio 4 domykają jedno okno");
    let err = relative_l2(&got, &oracle.decode_compressed);
    eprintln!("kompresor (dekodowanie): względne L2 = {err:.3e}");
    assert!(
        err < 1e-4,
        "dekodowanie kompresora rozjeżdża się o {err:.3e}"
    );
}

/// Głowa wyjściowa. Redukcja kopii HC jest tu PROSTSZA niż w bloku — sama
/// sigmoida, bez Sinkhorna i bez macierzy mieszającej. Logity liczone są tylko
/// dla ostatniej pozycji.
#[test]
fn output_head_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let hc_fn = load_vector(&st, "hc_head_fn");
    let hc_base = load_vector(&st, "hc_head_base");
    let hc_scale = load_vector(&st, "hc_head_scale");
    let norm_w = load_vector(&st, "norm.weight");
    let lm_head = load_vector(&st, "head.weight");
    let (hc, dim, eps, hc_eps) = (oracle.hc, oracle.dim, 1e-6f32, 1e-6f32);
    let wide = hc * dim;

    let mut reduced = Vec::with_capacity(oracle.head_reduced.len());
    for t in 0..oracle.seqlen {
        let x = &oracle.hc_in[t * wide..(t + 1) * wide];
        let mean = x.iter().map(|v| v * v).sum::<f32>() / wide as f32;
        let rsqrt = (mean + eps).sqrt().recip();
        let mut y = vec![0f32; dim];
        for copy in 0..hc {
            let mix: f32 = (0..wide)
                .map(|c| hc_fn[copy * wide + c] * x[c])
                .sum::<f32>()
                * rsqrt;
            // W głowie skala jest jedna dla wszystkich kopii (w bloku są trzy
            // osobne), a przesunięcie zostaje per kopia.
            let pre = 1.0 / (1.0 + (-(mix * hc_scale[0] + hc_base[copy])).exp()) + hc_eps;
            let src = &x[copy * dim..(copy + 1) * dim];
            for (o, v) in y.iter_mut().zip(src) {
                *o += pre * v;
            }
        }
        reduced.extend_from_slice(&y);
    }
    let reduce_err = relative_l2(&reduced, &oracle.head_reduced);
    eprintln!("głowa: redukcja HC = {reduce_err:.3e}");
    assert!(
        reduce_err < 1e-5,
        "redukcja HC głowy rozjeżdża się o {reduce_err:.3e}"
    );

    // Logity tylko dla ostatniej pozycji.
    let last = &reduced[(oracle.seqlen - 1) * dim..];
    let normed = rms_norm(last, &norm_w, eps);
    let got = project(&lm_head, &normed, oracle.vocab, dim);
    let err = relative_l2(&got, &oracle.logits);
    eprintln!("głowa: logity = {err:.3e}");
    assert!(err < 1e-5, "logity rozjeżdżają się o {err:.3e}");
}

/// Trzy pierwsze warstwy routują przez tablicę `token -> eksperci`, a nie przez
/// wynik bramki. Wynik nadal jest liczony — bierze się z niego WAGI dla
/// wybranych ekspertów, tylko wybór pochodzi z tablicy. Pominięcie tego
/// rozróżnienia daje poprawne wagi przy błędnych ekspertach.
#[test]
fn hash_routing_matches_the_reference_implementation() {
    let (Some(dir), Some(oracle)) = (checkpoint_dir(), load_oracle()) else {
        eprintln!("pomijam: brak checkpointu lub zrzutu (FORGE_DEEPSEEK_V4_ORACLE)");
        return;
    };
    let st = ShardedSafeTensors::load_dir(&dir).unwrap();
    let g = format!("layers.{LAYER}.ffn.gate");
    let weight = load_vector(&st, &format!("{g}.weight"));
    let table_info = st
        .tensor(&format!("{g}.tid2eid"))
        .expect("warstwa haszowana");
    let table_bytes = st.data(&format!("{g}.tid2eid")).unwrap();
    let per_token = table_info.shape[1];
    assert_eq!(
        per_token, oracle.topk,
        "tablica ma inną liczbę ekspertów niż top-k"
    );
    let route_scale = 1.5f32;

    for t in 0..oracle.seqlen {
        let token = oracle.token_ids[t] as usize;
        // Wybór: prosto z tablicy, indeksowanej identyfikatorem tokenu.
        let want_idx = &oracle.hash_indices[t * per_token..(t + 1) * per_token];
        for (j, want) in want_idx.iter().enumerate() {
            let at = (token * per_token + j) * 8;
            let got = i64::from_le_bytes(table_bytes[at..at + 8].try_into().unwrap());
            assert_eq!(got as i32, *want, "token {token}, pozycja {j}");
        }

        // Wagi: nadal z wyniku bramki, znormalizowane i przeskalowane.
        let x = &oracle.gate_x[t * oracle.dim..(t + 1) * oracle.dim];
        let raw: Vec<f32> = want_idx
            .iter()
            .map(|e| {
                let e = *e as usize;
                let logit: f32 = (0..oracle.dim)
                    .map(|c| weight[e * oracle.dim + c] * x[c])
                    .sum();
                let softplus = if logit > 20.0 {
                    logit
                } else {
                    logit.exp().ln_1p()
                };
                softplus.sqrt()
            })
            .collect();
        let sum: f32 = raw.iter().sum();
        let want_w = &oracle.hash_weights[t * per_token..(t + 1) * per_token];
        for (j, r) in raw.iter().enumerate() {
            let got = r / sum * route_scale;
            assert!(
                (got - want_w[j]).abs() < 1e-4,
                "token {t}, pozycja {j}: waga {got} zamiast {}",
                want_w[j]
            );
        }
    }
}
