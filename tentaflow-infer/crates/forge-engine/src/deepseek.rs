// ===== File: deepseek.rs — przebieg w przód uwagi DeepSeeka V4 =====
//
// Składa kernele w ścieżkę jednej warstwy uwagi. Każdy krok ma referencję
// przypiętą do implementacji autorów modelu (`forge-formats/tests/
// deepseek_v4_attention.rs`), a same kernele — testy złote na GPU
// (`forge-kernels/tests/deepseek_activations.rs`). Ten moduł odpowiada za
// KOLEJNOŚĆ i za bufory, bo to w nich kryją się pomyłki, których żaden test
// pojedynczego kernela nie złapie.
//
// Zakres: prefill jednej sekwencji. Dekodowanie wymaga stanu okna kompresora
// między tokenami i wejdzie osobno.
//
// Wybór top-k indeksera liczony jest NA HOŚCIE: wyniki punktowania wracają z
// urządzenia, są sortowane i wysyłane z powrotem jako lista indeksów. Jest to
// poprawne i kosztuje jedną synchronizację na warstwę; przeniesienie selekcji
// na GPU to osobna praca, która niczego powyżej nie zmieni.

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_kernels::Kernels;
use forge_types::{ForgeError, MemKind, Result};
use half::f16;

use crate::moe_residency::ExpertStack;
use crate::weights::{CompressorWeights, DeepseekAttnWeights, DevWeight, MoeFfn};

/// Geometria warstwy potrzebna przebiegowi. W tej architekturze różni się
/// między warstwami: stopień kompresji i obecność indeksera zależą od numeru.
#[derive(Clone, Copy, Debug)]
pub struct DeepseekAttnShape {
    pub hidden: usize,
    pub n_heads: usize,
    pub head_dim: usize,
    pub rope_head_dim: usize,
    pub q_lora_rank: usize,
    pub o_groups: usize,
    pub o_lora_rank: usize,
    pub window_size: usize,
    /// 0 = warstwa bez kompresora strumienia KV.
    pub compress_ratio: usize,
    pub index_n_heads: usize,
    pub index_head_dim: usize,
    pub index_topk: usize,
    pub eps: f32,
}

impl DeepseekAttnShape {
    /// Wymiary głowicy NIE przechodzące przez rope.
    fn nope(&self) -> usize {
        self.head_dim - self.rope_head_dim
    }

    /// Okna z zakładką występują wyłącznie przy stopniu 4; wtedy projekcje
    /// kompresora są dwa razy szersze, bo pierwsza połowa wymiarów opisuje okno
    /// przesunięte o blok wstecz.
    fn overlap(&self) -> bool {
        self.compress_ratio == 4
    }

    fn compressor_window(&self) -> usize {
        if self.overlap() {
            2 * self.compress_ratio
        } else {
            self.compress_ratio.max(1)
        }
    }
}

/// Bufory robocze przebiegu, alokowane raz na warstwę o danym kształcie.
pub struct DeepseekAttnBufs {
    qr: DevBuffer,
    q: DevBuffer,
    /// Strumień KV: najpierw `tokens` wpisów okna, potem wpisy skompresowane —
    /// rzadka uwaga indeksuje jeden ciągły bufor.
    kv_all: DevBuffer,
    comp_proj: DevBuffer,
    comp_score: DevBuffer,
    comp_slots: DevBuffer,
    index_q: DevBuffer,
    index_kv: DevBuffer,
    index_proj: DevBuffer,
    index_score: DevBuffer,
    index_head_w: DevBuffer,
    attn_out: DevBuffer,
    lora: DevBuffer,
    idxs: DevBuffer,
    freqs: DevBuffer,
    max_tokens: usize,
    blocks: usize,
}

impl DeepseekAttnBufs {
    /// Strumień KV warstwy: najpierw wpisy okna, potem skompresowane. Dekodowanie
    /// musi go zachować między tokenami, więc jest częścią kontraktu.
    pub fn kv_stream(&self) -> &DevBuffer {
        &self.kv_all
    }

    /// `freqs` to częstotliwości rope tej warstwy (z YaRN albo bez, zależnie od
    /// stopnia kompresji) — wyliczane raz przy budowie modelu.
    pub fn new(
        device: &dyn Device,
        shape: &DeepseekAttnShape,
        max_tokens: usize,
        freqs: &[f32],
    ) -> Result<Self> {
        if freqs.len() != shape.rope_head_dim / 2 {
            return Err(ForgeError::Format(format!(
                "rope: {} częstotliwości przy wymiarze {}",
                freqs.len(),
                shape.rope_head_dim
            )));
        }
        let f16b = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let f32b = |elems: usize| device.alloc(elems * 4, MemKind::Device, Pool::Activations);
        let ratio = shape.compress_ratio.max(1);
        let blocks = max_tokens / ratio;
        let wide = if shape.overlap() {
            2 * shape.head_dim
        } else {
            shape.head_dim
        };
        let index_wide = 2 * shape.index_head_dim;
        let freq_buf = f32b(freqs.len())?;
        device.write(bytemuck::cast_slice(freqs), &freq_buf, 0)?;
        Ok(Self {
            qr: f16b(max_tokens * shape.q_lora_rank)?,
            q: f16b(max_tokens * shape.n_heads * shape.head_dim)?,
            kv_all: f16b((max_tokens + blocks.max(1)) * shape.head_dim)?,
            // Kompresor liczy w f32: w f16 wyniki bramki się przelewają i
            // softmax po oknie daje NaN zamiast rozkładu.
            comp_proj: f32b(max_tokens.max(1) * wide)?,
            comp_score: f32b(max_tokens.max(1) * wide)?,
            comp_slots: f32b(blocks.max(1) * shape.compressor_window())?,
            index_q: f16b(max_tokens * shape.index_n_heads * shape.index_head_dim)?,
            index_kv: f16b(blocks.max(1) * shape.index_head_dim)?,
            index_proj: f32b(max_tokens.max(1) * index_wide)?,
            index_score: f16b(max_tokens * blocks.max(1))?,
            index_head_w: f16b(max_tokens * shape.index_n_heads.max(1))?,
            attn_out: f16b(max_tokens * shape.n_heads * shape.head_dim)?,
            lora: f16b(max_tokens * shape.o_groups * shape.o_lora_rank)?,
            idxs: f32b(max_tokens * (max_tokens + blocks.max(1)))?,
            freqs: freq_buf,
            max_tokens,
            blocks,
        })
    }
}

/// GEMV pojedynczego wiersza na wadze dowolnego wspieranego formatu.
fn gemv_row(
    kernels: &Kernels,
    y: &DevBuffer,
    y_off: usize,
    w: &DevWeight,
    x: &DevBuffer,
    x_off: usize,
    stream: &Stream,
) -> Result<()> {
    match w {
        DevWeight::Fp8Row {
            buf,
            scales,
            rows,
            cols,
        } => kernels.gemv_fp8_row_f16_at(y, y_off, buf, scales, x, x_off, *rows, *cols, stream),
        DevWeight::F16 { buf, rows, cols } => {
            kernels.gemv_f16_at(y, y_off, buf, x, x_off, *rows, *cols, stream)
        }
        DevWeight::NvFp4Gguf {
            buf,
            output_scale,
            rows,
            cols,
            ..
        } => kernels.gemv_nvfp4_gguf_f16_at(
            y,
            y_off,
            buf,
            x,
            x_off,
            *rows,
            *cols,
            *output_scale,
            stream,
        ),
        other => Err(ForgeError::Unsupported(format!(
            "ścieżka DeepSeeka nie obsługuje wagi {:?}",
            std::mem::discriminant(other)
        ))),
    }
}

/// GEMV pojedynczego wiersza z wynikiem w f32 — ścieżka kompresora.
fn gemv_row_f32(
    kernels: &Kernels,
    y: &DevBuffer,
    y_off: usize,
    w: &DevWeight,
    x: &DevBuffer,
    x_off: usize,
    stream: &Stream,
) -> Result<()> {
    match w {
        DevWeight::F16 { buf, rows, cols } => {
            kernels.gemv_f16_out_f32_at(y, y_off, buf, x, x_off, *rows, *cols, stream)
        }
        other => Err(ForgeError::Unsupported(format!(
            "kompresor nie obsługuje wagi {:?}",
            std::mem::discriminant(other)
        ))),
    }
}

/// Buduje tablicę slotów kompresora: dla każdego bloku i pozycji okna wskazuje
/// wiersz projekcji i połowę wymiarów, z której ma czytać. Wartość ujemna to
/// pozycja pusta (blok zerowy przy oknach z zakładką nie ma poprzednika).
///
/// Cała logika zakładki siedzi tutaj — kernel poolingu nic o niej nie wie, a
/// wariant bez zakładki jest tym samym kernelem z inną tablicą.
fn compressor_slots(
    shape: &DeepseekAttnShape,
    blocks: usize,
    head_dim: usize,
    wide: usize,
) -> Vec<i32> {
    let ratio = shape.compress_ratio;
    let window = shape.compressor_window();
    let mut slots = vec![-1i32; blocks * window];
    for block in 0..blocks {
        for w in 0..window {
            let (token, half) = if !shape.overlap() {
                (block * ratio + w, 0usize)
            } else if w < ratio {
                if block == 0 {
                    continue;
                }
                ((block - 1) * ratio + w, 0)
            } else {
                (block * ratio + w - ratio, 1)
            };
            // Kernel czyta wiersze o szerokości `head_dim` TEGO kompresora —
            // indekser ma własną, mniejszą niż uwaga. Połowa wymiarów to
            // przesunięcie o pół wiersza projekcji.
            let stride = wide / head_dim;
            slots[block * window + w] = (token * stride + half) as i32;
        }
    }
    slots
}

/// Wspólny odcinek obu kompresorów: pooling, norma i rope. `quantize` domyka go
/// tak, jak wymaga wariant (FP8 dla strumienia KV, Hadamard + FP4 dla indeksera).
#[allow(clippy::too_many_arguments)]
fn compress_stream(
    kernels: &Kernels,
    weights: &CompressorWeights,
    shape: &DeepseekAttnShape,
    proj: &DevBuffer,
    score: &DevBuffer,
    slots: &DevBuffer,
    out: &DevBuffer,
    freqs: &DevBuffer,
    head_dim: usize,
    wide: usize,
    x: &DevBuffer,
    tokens: usize,
    blocks: usize,
    stream: &Stream,
) -> Result<()> {
    for token in 0..tokens {
        gemv_row_f32(
            kernels,
            proj,
            token * wide * 4,
            &weights.wkv,
            x,
            token * shape.hidden * 2,
            stream,
        )?;
        gemv_row_f32(
            kernels,
            score,
            token * wide * 4,
            &weights.wgate,
            x,
            token * shape.hidden * 2,
            stream,
        )?;
        // Kodowanie pozycji wewnątrz okna zależy od miejsca tokenu w oknie.
        // Kodowanie pozycji wewnątrz okna dochodzi do wyników bramki.
        kernels.compressor_add_ape_f32(
            score,
            token * wide * 4,
            &weights.ape,
            (token % shape.compress_ratio) * wide * 4,
            wide,
            stream,
        )?;
    }
    kernels.compressor_pool_f16(
        out,
        proj,
        score,
        slots,
        head_dim,
        shape.compressor_window(),
        blocks,
        stream,
    )?;
    kernels.rmsnorm_f16(out, out, &weights.norm, blocks, head_dim, shape.eps, stream)?;
    // Rope bierze pozycję POCZĄTKU okna, nie tokenu, który je domknął.
    kernels.rope_interleaved_f16(
        out,
        freqs,
        head_dim,
        head_dim - shape.rope_head_dim,
        shape.rope_head_dim,
        blocks,
        0,
        shape.compress_ratio,
        false,
        stream,
    )?;
    Ok(())
}

/// Indeksy prefillu: okno przesuwne (przyczynowe) plus widoczne wpisy
/// skompresowane, wybrane przez indekser albo — gdy warstwa go nie ma — wszystkie
/// dostępne. Pozycja niedostępna to `-1`, nie pominięcie.
fn prefill_indices(
    shape: &DeepseekAttnShape,
    tokens: usize,
    blocks: usize,
    selected: Option<&[Vec<usize>]>,
) -> Vec<i32> {
    let window = tokens.min(shape.window_size);
    let width = window + blocks;
    let mut idxs = vec![-1i32; tokens * width];
    for token in 0..tokens {
        let first = token.saturating_sub(shape.window_size - 1);
        for slot in 0..window {
            let at = first + slot;
            if at <= token {
                idxs[token * width + slot] = at as i32;
            }
        }
        let visible = if shape.compress_ratio == 0 {
            0
        } else {
            (token + 1) / shape.compress_ratio
        };
        match selected {
            Some(rows) => {
                for (slot, block) in rows[token].iter().enumerate() {
                    if *block < visible {
                        idxs[token * width + window + slot] = (tokens + block) as i32;
                    }
                }
            }
            None => {
                for block in 0..visible.min(blocks) {
                    idxs[token * width + window + block] = (tokens + block) as i32;
                }
            }
        }
    }
    idxs
}

/// Prefill uwagi jednej warstwy: z `x` na `out`, oba `[tokens, hidden]` w f16.
#[allow(clippy::too_many_arguments)]
pub fn attention_prefill(
    kernels: &Kernels,
    device: &dyn Device,
    weights: &DeepseekAttnWeights,
    shape: &DeepseekAttnShape,
    bufs: &DeepseekAttnBufs,
    x: &DevBuffer,
    out: &DevBuffer,
    tokens: usize,
    stream: &Stream,
) -> Result<()> {
    if tokens > bufs.max_tokens {
        return Err(ForgeError::Kernel(format!(
            "prefill {tokens} tokenów przekracza pojemność buforów {}",
            bufs.max_tokens
        )));
    }
    let ratio = shape.compress_ratio;
    let blocks = if ratio == 0 { 0 } else { tokens / ratio };
    if blocks > bufs.blocks {
        return Err(ForgeError::Kernel(format!(
            "{blocks} bloków kompresji przekracza pojemność {}",
            bufs.blocks
        )));
    }

    // --- Q ---
    for token in 0..tokens {
        gemv_row(
            kernels,
            &bufs.qr,
            token * shape.q_lora_rank * 2,
            &weights.wq_a,
            x,
            token * shape.hidden * 2,
            stream,
        )?;
    }
    kernels.rmsnorm_f16(
        &bufs.qr,
        &bufs.qr,
        &weights.q_norm,
        tokens,
        shape.q_lora_rank,
        shape.eps,
        stream,
    )?;
    let q_width = shape.n_heads * shape.head_dim;
    for token in 0..tokens {
        gemv_row(
            kernels,
            &bufs.q,
            token * q_width * 2,
            &weights.wq_b,
            &bufs.qr,
            token * shape.q_lora_rank * 2,
            stream,
        )?;
    }
    // Druga norma RMS: per głowica i BEZ wagi.
    kernels.rmsnorm_head_f16(&bufs.q, shape.head_dim, tokens * shape.n_heads, shape.eps, stream)?;
    for token in 0..tokens {
        kernels.rope_interleaved_f16(
            &bufs.q,
            &bufs.freqs,
            shape.head_dim,
            token * q_width + shape.nope(),
            shape.rope_head_dim,
            shape.n_heads,
            token,
            0,
            false,
            stream,
        )?;
    }

    // --- KV okna ---
    for token in 0..tokens {
        gemv_row(
            kernels,
            &bufs.kv_all,
            token * shape.head_dim * 2,
            &weights.wkv,
            x,
            token * shape.hidden * 2,
            stream,
        )?;
    }
    kernels.rmsnorm_f16(
        &bufs.kv_all,
        &bufs.kv_all,
        &weights.kv_norm,
        tokens,
        shape.head_dim,
        shape.eps,
        stream,
    )?;
    kernels.rope_interleaved_f16(
        &bufs.kv_all,
        &bufs.freqs,
        shape.head_dim,
        shape.nope(),
        shape.rope_head_dim,
        tokens,
        0,
        1,
        false,
        stream,
    )?;
    // Kwantyzacja QAT obejmuje wymiary BEZ rope; ogon rope zostaje w f16.
    kernels.act_quant_fp8_f16(&bufs.kv_all, shape.head_dim, 0, shape.nope(), 64, tokens, stream)?;

    // --- strumień skompresowany ---
    if let Some(compressor) = weights.compressor.as_ref() {
        let wide = if shape.overlap() {
            2 * shape.head_dim
        } else {
            shape.head_dim
        };
        let slots = compressor_slots(shape, blocks, shape.head_dim, wide);
        device.write(bytemuck::cast_slice(&slots), &bufs.comp_slots, 0)?;
        let compressed = device.sub_buffer(
            &bufs.kv_all,
            tokens * shape.head_dim * 2,
            blocks * shape.head_dim * 2,
        )?;
        compress_stream(
            kernels,
            compressor,
            shape,
            &bufs.comp_proj,
            &bufs.comp_score,
            &bufs.comp_slots,
            &compressed,
            &bufs.freqs,
            shape.head_dim,
            wide,
            x,
            tokens,
            blocks,
            stream,
        )?;
        kernels.act_quant_fp8_f16(
            &compressed,
            shape.head_dim,
            0,
            shape.nope(),
            64,
            blocks,
            stream,
        )?;
    }

    // --- wybór pozycji ---
    let selected = match weights.indexer.as_ref() {
        Some(indexer) => Some(index_topk(
            kernels, device, indexer, shape, bufs, x, tokens, blocks, stream,
        )?),
        None => None,
    };
    let idxs = prefill_indices(shape, tokens, blocks, selected.as_deref());
    device.write(bytemuck::cast_slice(&idxs), &bufs.idxs, 0)?;
    let width = tokens.min(shape.window_size) + blocks;

    // --- rzadka uwaga ---
    let scale = (shape.head_dim as f32).sqrt().recip();
    for token in 0..tokens {
        let q_row = device.sub_buffer(&bufs.q, token * q_width * 2, q_width * 2)?;
        let out_row = device.sub_buffer(&bufs.attn_out, token * q_width * 2, q_width * 2)?;
        let idx_row = device.sub_buffer(&bufs.idxs, token * width * 4, width * 4)?;
        kernels.sparse_attn_f16(
            &out_row,
            &q_row,
            &bufs.kv_all,
            &weights.attn_sink,
            &idx_row,
            shape.head_dim,
            shape.n_heads,
            width,
            scale,
            stream,
        )?;
    }

    // --- wyjście: rope ODWROTNE, potem grupowana LoRA ---
    for token in 0..tokens {
        kernels.rope_interleaved_f16(
            &bufs.attn_out,
            &bufs.freqs,
            shape.head_dim,
            token * q_width + shape.nope(),
            shape.rope_head_dim,
            shape.n_heads,
            token,
            0,
            true,
            stream,
        )?;
    }
    let per_group = q_width / shape.o_groups;
    for token in 0..tokens {
        for group in 0..shape.o_groups {
            // Każda grupa mnoży się przez WŁASNY blok `wo_a`; potraktowanie go
            // jako jednej macierzy daje poprawny kształt i błędne liczby.
            let block = weight_row_window(
                device,
                &weights.wo_a,
                group * shape.o_lora_rank,
                shape.o_lora_rank,
            )?;
            gemv_row(
                kernels,
                &bufs.lora,
                (token * shape.o_groups + group) * shape.o_lora_rank * 2,
                &block,
                &bufs.attn_out,
                (token * shape.o_groups + group) * per_group * 2,
                stream,
            )?;
        }
        gemv_row(
            kernels,
            out,
            token * shape.hidden * 2,
            &weights.wo_b,
            &bufs.lora,
            token * shape.o_groups * shape.o_lora_rank * 2,
            stream,
        )?;
    }
    Ok(())
}

/// Okno wierszy wagi jako samodzielna waga — `wo_a` trzyma bloki grup jeden po
/// drugim, a każdy jest osobną macierzą.
fn weight_row_window(
    device: &dyn Device,
    weight: &DevWeight,
    row_off: usize,
    rows: usize,
) -> Result<DevWeight> {
    match weight {
        DevWeight::Fp8Row {
            buf,
            scales,
            cols,
            rows: total,
        } => {
            if row_off + rows > *total {
                return Err(ForgeError::Kernel("okno wierszy poza wagą".into()));
            }
            Ok(DevWeight::Fp8Row {
                buf: device.sub_buffer(buf, row_off * cols, rows * cols)?,
                scales: device.sub_buffer(scales, row_off * 4, rows * 4)?,
                rows,
                cols: *cols,
            })
        }
        other => Err(ForgeError::Unsupported(format!(
            "okno wierszy nie obsługuje {:?}",
            std::mem::discriminant(other)
        ))),
    }
}

/// Punktowanie i wybór pozycji przez indekser. Zwraca dla każdego tokenu listę
/// bloków skompresowanych w kolejności malejącego wyniku.
#[allow(clippy::too_many_arguments)]
fn index_topk(
    kernels: &Kernels,
    device: &dyn Device,
    indexer: &crate::weights::IndexerWeights,
    shape: &DeepseekAttnShape,
    bufs: &DeepseekAttnBufs,
    x: &DevBuffer,
    tokens: usize,
    blocks: usize,
    stream: &Stream,
) -> Result<Vec<Vec<usize>>> {
    let d = shape.index_head_dim;
    let wide = 2 * d;
    let slots = compressor_slots(shape, blocks, d, wide);
    device.write(bytemuck::cast_slice(&slots), &bufs.comp_slots, 0)?;
    compress_stream(
        kernels,
        &indexer.compressor,
        shape,
        &bufs.index_proj,
        &bufs.comp_score,
        &bufs.comp_slots,
        &bufs.index_kv,
        &bufs.freqs,
        d,
        wide,
        x,
        tokens,
        blocks,
        stream,
    )?;
    // Kompresor indeksera domyka się rotacją i kwantyzacją FP4, nie FP8.
    kernels.hadamard_bf16_f16(&bufs.index_kv, d, blocks, stream)?;
    kernels.act_quant_fp4_f16(&bufs.index_kv, d, d, 32, blocks, stream)?;

    let q_width = shape.index_n_heads * d;
    for token in 0..tokens {
        gemv_row(
            kernels,
            &bufs.index_q,
            token * q_width * 2,
            &indexer.wq_b,
            &bufs.qr,
            token * shape.q_lora_rank * 2,
            stream,
        )?;
        kernels.rope_interleaved_f16(
            &bufs.index_q,
            &bufs.freqs,
            d,
            token * q_width + d - shape.rope_head_dim,
            shape.rope_head_dim,
            shape.index_n_heads,
            token,
            0,
            false,
            stream,
        )?;
        gemv_row(
            kernels,
            &bufs.index_head_w,
            token * shape.index_n_heads * 2,
            &indexer.weights_proj,
            x,
            token * shape.hidden * 2,
            stream,
        )?;
    }
    kernels.hadamard_bf16_f16(&bufs.index_q, d, tokens * shape.index_n_heads, stream)?;
    kernels.act_quant_fp4_f16(&bufs.index_q, d, d, 32, tokens * shape.index_n_heads, stream)?;
    kernels.index_score_f16(
        &bufs.index_score,
        &bufs.index_q,
        &bufs.index_kv,
        &bufs.index_head_w,
        d,
        shape.index_n_heads,
        blocks,
        tokens,
        (d as f32).sqrt().recip() * (shape.index_n_heads as f32).sqrt().recip(),
        stream,
    )?;

    // Selekcja na hoście: jedna synchronizacja na warstwę.
    stream.synchronize()?;
    let mut bytes = vec![0u8; tokens * blocks * 2];
    device.read(&bufs.index_score, 0, &mut bytes)?;
    let scores: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();
    let keep = shape.index_topk.min(blocks);
    Ok((0..tokens)
        .map(|token| {
            let mut order: Vec<usize> = (0..blocks).collect();
            order.sort_by(|a, b| {
                scores[token * blocks + *b].total_cmp(&scores[token * blocks + *a])
            });
            order.truncate(keep);
            order
        })
        .collect())
}

/// Geometria warstwy FFN.
#[derive(Clone, Copy, Debug)]
pub struct DeepseekFfnShape {
    pub hidden: usize,
    pub moe_inter: usize,
    pub n_experts_used: usize,
    pub route_scale: f32,
    /// Górne (a dla wejścia obustronne) obcięcie SwiGLU. Ta architektura zawsze
    /// je deklaruje, więc brak wartości dodatniej oznacza błędną konfigurację.
    pub swiglu_limit: f32,
}

/// Bufory ścieżki FFN.
pub struct DeepseekFfnBufs {
    ids: DevBuffer,
    weights: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    expert_out: DevBuffer,
    max_tokens: usize,
}

impl DeepseekFfnBufs {
    pub fn new(
        device: &dyn Device,
        shape: &DeepseekFfnShape,
        max_tokens: usize,
    ) -> Result<Self> {
        let f16b = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let f32b = |elems: usize| device.alloc(elems * 4, MemKind::Device, Pool::Activations);
        Ok(Self {
            ids: f32b(max_tokens * shape.n_experts_used)?,
            weights: f32b(max_tokens * shape.n_experts_used)?,
            gate: f16b(shape.moe_inter)?,
            up: f16b(shape.moe_inter)?,
            act: f16b(shape.moe_inter)?,
            expert_out: f16b(shape.hidden)?,
            max_tokens,
        })
    }
}

/// Jeden ekspert SwiGLU na jednym tokenie, dodany do akumulatora z wagą routingu.
#[allow(clippy::too_many_arguments)]
fn run_expert(
    kernels: &Kernels,
    gate_w: &DevWeight,
    up_w: &DevWeight,
    down_w: &DevWeight,
    shape: &DeepseekFfnShape,
    bufs: &DeepseekFfnBufs,
    x: &DevBuffer,
    x_off: usize,
    out: &DevBuffer,
    out_off: usize,
    weight: f32,
    init: bool,
    stream: &Stream,
) -> Result<()> {
    gemv_row(kernels, &bufs.gate, 0, gate_w, x, x_off, stream)?;
    gemv_row(kernels, &bufs.up, 0, up_w, x, x_off, stream)?;
    kernels.swiglu_limit_f16(
        &bufs.act,
        &bufs.gate,
        &bufs.up,
        shape.moe_inter,
        shape.swiglu_limit,
        stream,
    )?;
    gemv_row(kernels, &bufs.expert_out, 0, down_w, &bufs.act, 0, stream)?;
    // Waga routingu wchodzi PRZED zsumowaniem z pozostałymi ekspertami.
    kernels.moe_scale_add_f16(
        out,
        out_off,
        &bufs.expert_out,
        0,
        shape.hidden,
        weight,
        init,
        stream,
    )
}

/// Prefill FFN jednej warstwy: routowani eksperci plus zawsze aktywny ekspert
/// dzielony.
///
/// Wybór ekspertów wraca na hosta, bo dla każdego tokenu uruchamiamy inne wagi.
/// Ścieżka bez odczytu wstecznego (jak w innych modelach MoE tego silnika)
/// wymaga wszystkich ekspertów rezydentnych i adresowalnych tablicą wskaźników;
/// przy 256 ekspertach na warstwę i 148 GiB wag jest to inna klasa problemu i
/// wejdzie razem z rezydencją.
#[allow(clippy::too_many_arguments)]
pub fn ffn_prefill(
    kernels: &Kernels,
    device: &dyn Device,
    moe: &MoeFfn,
    shape: &DeepseekFfnShape,
    bufs: &DeepseekFfnBufs,
    x: &DevBuffer,
    out: &DevBuffer,
    tokens: usize,
    token_ids: Option<&[u32]>,
    stream: &Stream,
) -> Result<()> {
    if tokens > bufs.max_tokens {
        return Err(ForgeError::Kernel(format!(
            "FFN {tokens} tokenów przekracza pojemność buforów {}",
            bufs.max_tokens
        )));
    }
    if !(shape.swiglu_limit > 0.0) {
        return Err(ForgeError::Format(format!(
            "swiglu_limit = {} — DeepSeek V4 zawsze deklaruje dodatnie obcięcie",
            shape.swiglu_limit
        )));
    }
    let DevWeight::F16 { buf: gate_w, .. } = &moe.router else {
        return Err(ForgeError::Unsupported("router MoE musi być f16".into()));
    };
    let top_k = shape.n_experts_used;

    // Wagi routingu liczy bramka nawet w warstwach haszowanych — z tablicy
    // pochodzi WYŁĄCZNIE wybór ekspertów.
    let bias = moe.gate_bias.as_ref().ok_or_else(|| {
        ForgeError::Unsupported("bramka DeepSeeka wymaga biasu routera".into())
    })?;
    kernels.moe_gate_sqrtsoftplus_f16(
        &bufs.ids,
        &bufs.weights,
        x,
        gate_w,
        bias,
        tokens,
        shape.hidden,
        moe.n_experts,
        top_k,
        shape.route_scale,
        stream,
    )?;
    stream.synchronize()?;

    let mut id_bytes = vec![0u8; tokens * top_k * 4];
    device.read(&bufs.ids, 0, &mut id_bytes)?;
    let mut ids: Vec<i32> = id_bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if let (Some(table), Some(tokens_ids)) = (moe.tid2eid.as_ref(), token_ids) {
        ids = hash_route(device, table, tokens_ids, top_k)?;
    }
    let mut weight_bytes = vec![0u8; tokens * top_k * 4];
    device.read(&bufs.weights, 0, &mut weight_bytes)?;
    let route_w: Vec<f32> = weight_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    for token in 0..tokens {
        let x_off = token * shape.hidden * 2;
        let out_off = token * shape.hidden * 2;
        for slot in 0..top_k {
            let expert = ids[token * top_k + slot] as usize;
            run_expert(
                kernels,
                expert_weight(&moe.gate_exps, expert)?,
                expert_weight(&moe.up_exps, expert)?,
                expert_weight(&moe.down_exps, expert)?,
                shape,
                bufs,
                x,
                x_off,
                out,
                out_off,
                route_w[token * top_k + slot],
                slot == 0,
                stream,
            )?;
        }
        // Ekspert dzielony jest zawsze aktywny i wchodzi bez wagi routingu.
        if let Some(shared) = moe.shared.as_ref() {
            let (gate_w, up_w) = match &shared.gate_up {
                crate::weights::GateUpWeights::Split { gate, up } => (gate, up),
                crate::weights::GateUpWeights::Fused(_) => {
                    return Err(ForgeError::Unsupported(
                        "ekspert dzielony DeepSeeka ma rozdzielone gate/up".into(),
                    ))
                }
            };
            run_expert(
                kernels, gate_w, up_w, &shared.down, shape, bufs, x, x_off, out, out_off, 1.0,
                false, stream,
            )?;
        }
    }
    Ok(())
}

/// Wybór ekspertów z tablicy `token -> eksperci` warstw haszowanych.
fn hash_route(
    device: &dyn Device,
    table: &DevBuffer,
    token_ids: &[u32],
    top_k: usize,
) -> Result<Vec<i32>> {
    let mut out = Vec::with_capacity(token_ids.len() * top_k);
    let mut row = vec![0u8; top_k * 8];
    for id in token_ids {
        device.read(table, *id as usize * top_k * 8, &mut row)?;
        for entry in row.chunks_exact(8) {
            out.push(i64::from_le_bytes(entry.try_into().expect("osiem bajtów")) as i32);
        }
    }
    Ok(out)
}

fn expert_weight(stack: &ExpertStack, expert: usize) -> Result<&DevWeight> {
    stack.expert(expert)
}
