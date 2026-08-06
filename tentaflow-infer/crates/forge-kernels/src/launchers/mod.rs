// ===== File: launchers.rs — typed launch wrappers over kernel artifacts =====
// Argument order and meaning must mirror the Mojo kernel signatures exactly
// (kernels/mojo/src/*.mojo). Mojo `Int` marshals as a 64-bit scalar slot,
// `Float32` as f32.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use std::sync::OnceLock;

use std::sync::{Arc, Mutex};

use forge_hal::{DevBuffer, Device, Event, LaunchArgs, LaunchConfig, Pool, Stream};

use forge_types::{DType, ForgeError, MemKind, QuantKind, Result};

use crate::registry::KernelArtifacts;

const BLOCK: u32 = 256;

/// Wierszy na blok w rodzinie `gemv_nvfp4_gguf_*_wave` (osiem fal po 32 linie).
const GEMV_WAVE_ROWS: usize = 8;

/// Format wagi w grupie mieszanej. Wartości muszą odpowiadać `_dot_mixed_i8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixedQuant {
    Q4K = 0,
    Q6K = 1,
    Q8_0 = 2,
}

impl MixedQuant {
    /// Liczba wartości w bloku kwantyzacji tego formatu.
    fn block(self) -> usize {
        match self {
            MixedQuant::Q4K | MixedQuant::Q6K => 256,
            MixedQuant::Q8_0 => 32,
        }
    }
}

const FP8_MODULAR_BN256_SMEM: usize = 98_304;

const FP8_MODULAR_BN256_TOKENS: usize = 1024;

static FP8_MODULAR_BN256_ENABLED: OnceLock<bool> = OnceLock::new();

fn fp8_modular_bn256_enabled() -> bool {
    *FP8_MODULAR_BN256_ENABLED.get_or_init(|| {
        matches!(
            std::env::var("FORGE_FP8_BN256").ok().as_deref(),
            None | Some("auto") | Some("1")
        )
    })
}

fn fp8_modular_bn256_kernel(rows: usize, cols: usize) -> Option<&'static str> {
    match (rows, cols) {
        (4096, 4096) => Some("gemm_fp8_mod_4096_4096_bn256"),
        (11264, 4096) => Some("gemm_fp8_mod_11264_4096_bn256"),
        // Projekcja `down`: BN=256 daje tu nieporównanie więcej niż gdzie
        // indziej. Sweep na GB10 przy M=1024: 1471.9 -> 867.0 us, czyli
        // 64 -> 109 TFLOPS. q/o i gate/up zyskują po ~4%, więc to ten kształt
        // decydował o prefillu, a akurat jego wariantu brakowało.
        (4096, 11264) => Some("gemm_fp8_mod_4096_11264_bn256"),
        (4096, 14336) => Some("gemm_fp8_mod_4096_14336_bn256"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn fp8_modular_bn256_capable(
    vendor: forge_types::Vendor,
    warp_size: u32,
    max_threads_per_block: u32,
    max_shared_mem_per_block: usize,
    rows: usize,
    cols: usize,
    n_tokens: usize,
    mut has: impl FnMut(&str) -> bool,
) -> bool {
    let Some(kernel) = fp8_modular_bn256_kernel(rows, cols) else {
        return false;
    };
    fp8_modular_bn256_enabled()
        && vendor == forge_types::Vendor::Nvidia
        && warp_size == 32
        && max_threads_per_block >= 256
        && max_shared_mem_per_block >= FP8_MODULAR_BN256_SMEM
        && n_tokens == FP8_MODULAR_BN256_TOKENS
        && has(kernel)
}

/// Jedna projekcja surowego GGUF NVFP4 korzystająca ze wspólnej aktywacji Q8_1.
pub struct Nvfp4GgufQ8Projection<'a> {
    pub output: &'a DevBuffer,
    pub weights: &'a DevBuffer,
    pub rows: usize,
    pub output_scale: f32,
}

/// Fizyczny układ bajtów macierzy GGUF NVFP4 na urządzeniu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nvfp4GgufLayout {
    RowMajor36,
    TileN128K64,
}

/// Widok pełnej macierzy NVFP4 w naturalnym układzie S0 N64/K128.
#[derive(Clone, Copy)]
pub struct Nvfp4CtS0View<'a> {
    buffer: &'a DevBuffer,
    rows: usize,
    cols: usize,
}

impl<'a> Nvfp4CtS0View<'a> {
    pub fn new(buffer: &'a DevBuffer, rows: usize, cols: usize) -> Result<Self> {
        if rows == 0 || !rows.is_multiple_of(64) || cols == 0 || !cols.is_multiple_of(128) {
            return Err(ForgeError::Kernel(format!(
                "NVFP4 S0 N64/K128 wymaga rows % 64 == 0 i cols % 128 == 0; rows={rows}, cols={cols}"
            )));
        }
        let bytes = checked_buffer_bytes("NVFP4 S0 N64/K128", &[rows, cols], 9)?
            .checked_div(16)
            .ok_or_else(|| ForgeError::Kernel("NVFP4 S0: niepoprawny rozmiar".into()))?;
        if buffer.len() != bytes {
            return Err(ForgeError::Kernel(format!(
                "NVFP4 S0 N64/K128 wymaga dokładnie {bytes} bajtów, otrzymano {}",
                buffer.len()
            )));
        }
        Ok(Self { buffer, rows, cols })
    }
}

/// Zmierzona specjalizacja projekcji BM16/BM32 modelu 4096/11264.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nvfp4CtProjection {
    Qkv,
    Output,
    GateUp,
    Down,
}

/// Fizyczny kafel M obsługujący dane logiczne M: 4/8/16 na BM16, 24/32 na BM32.
/// Inne rozmiary nie mają wyspecjalizowanego kernela i wracają na ścieżkę ogólną.
#[must_use]
pub fn nvfp4_ct_physical_m(logical_m: usize) -> Option<usize> {
    match logical_m {
        4 | 8 | 16 => Some(16),
        24 | 32 => Some(32),
        _ => None,
    }
}

impl Nvfp4CtProjection {
    /// Zwraca (n_rows, n_cols, części split-K) projekcji.
    fn dims(self) -> (usize, usize, usize) {
        match self {
            Self::Qkv => (6144, 4096, 3),
            Self::Output => (4096, 4096, 4),
            Self::GateUp => (22528, 4096, 1),
            Self::Down => (4096, 11264, 4),
        }
    }

    /// Zwraca (kafel N, wątki bloku) dla danego kafla fizycznego M.
    fn launch_shape(self, physical_m: usize) -> (usize, u32) {
        if physical_m == 32 || self == Self::Down {
            (64, 128)
        } else {
            (128, 256)
        }
    }

    /// Głębokość potoku cp.async, którą musi pomieścić każda część split-K.
    fn pipeline_stages(self, physical_m: usize) -> usize {
        if physical_m == 32 || self == Self::GateUp {
            3
        } else {
            4
        }
    }

    fn kernel_name(self, logical_m: usize) -> Option<&'static str> {
        match (self, logical_m) {
            (Self::Qkv, 4) => Some("gemm_nvfp4_ct_bm16_qkv_m4"),
            (Self::Qkv, 8) => Some("gemm_nvfp4_ct_bm16_qkv_m8"),
            (Self::Qkv, 16) => Some("gemm_nvfp4_ct_bm16_qkv_m16"),
            (Self::Qkv, 24) => Some("gemm_nvfp4_ct_bm32_qkv_m24"),
            (Self::Qkv, 32) => Some("gemm_nvfp4_ct_bm32_qkv_m32"),
            (Self::Output, 4) => Some("gemm_nvfp4_ct_bm16_o_m4"),
            (Self::Output, 8) => Some("gemm_nvfp4_ct_bm16_o_m8"),
            (Self::Output, 16) => Some("gemm_nvfp4_ct_bm16_o_m16"),
            (Self::Output, 24) => Some("gemm_nvfp4_ct_bm32_o_m24"),
            (Self::Output, 32) => Some("gemm_nvfp4_ct_bm32_o_m32"),
            (Self::GateUp, 4) => Some("gemm_nvfp4_ct_bm16_gateup_m4"),
            (Self::GateUp, 8) => Some("gemm_nvfp4_ct_bm16_gateup_m8"),
            (Self::GateUp, 16) => Some("gemm_nvfp4_ct_bm16_gateup_m16"),
            (Self::GateUp, 24) => Some("gemm_nvfp4_ct_bm32_gateup_m24"),
            (Self::GateUp, 32) => Some("gemm_nvfp4_ct_bm32_gateup_m32"),
            (Self::Down, 4) => Some("gemm_nvfp4_ct_bm16_down_m4"),
            (Self::Down, 8) => Some("gemm_nvfp4_ct_bm16_down_m8"),
            (Self::Down, 16) => Some("gemm_nvfp4_ct_bm16_down_m16"),
            (Self::Down, 24) => Some("gemm_nvfp4_ct_bm32_down_m24"),
            (Self::Down, 32) => Some("gemm_nvfp4_ct_bm32_down_m32"),
            _ => None,
        }
    }
}

const NVFP4_CT_S0_ARTIFACTS: [&str; 32] = [
    "repack_nvfp4_ct_s0_n64k128_into",
    "gemv_nvfp4_ct_s0_n64k128_f16",
    "gemv_batch_nvfp4_ct_s0_n64k128_f16_b4",
    "gemv_batch_nvfp4_ct_s0_n64k128_f16_b8",
    "gemv_batch_nvfp4_ct_s0_n64k128_f16_b16",
    "gemm_nvfp4_ct_s0_f16_bm64",
    "gemm_nvfp4_ct_s0_f16_bm128",
    "gemv_norm_nvfp4_ct_s0_f16",
    "gemv_norm_silu_nvfp4_ct_s0_f16",
    "gemv_residual_nvfp4_ct_s0_f16",
    "gemm_nvfp4_ct_bm16_qkv_m4",
    "gemm_nvfp4_ct_bm16_qkv_m8",
    "gemm_nvfp4_ct_bm16_qkv_m16",
    "gemm_nvfp4_ct_bm16_o_m4",
    "gemm_nvfp4_ct_bm16_o_m8",
    "gemm_nvfp4_ct_bm16_o_m16",
    "gemm_nvfp4_ct_bm16_gateup_m4",
    "gemm_nvfp4_ct_bm16_gateup_m8",
    "gemm_nvfp4_ct_bm16_gateup_m16",
    "gemm_nvfp4_ct_bm16_down_m4",
    "gemm_nvfp4_ct_bm16_down_m8",
    "gemm_nvfp4_ct_bm16_down_m16",
    "gemm_nvfp4_ct_bm32_qkv_m24",
    "gemm_nvfp4_ct_bm32_qkv_m32",
    "gemm_nvfp4_ct_bm32_o_m24",
    "gemm_nvfp4_ct_bm32_o_m32",
    "gemm_nvfp4_ct_bm32_gateup_m24",
    "gemm_nvfp4_ct_bm32_gateup_m32",
    "gemm_nvfp4_ct_bm32_down_m24",
    "gemm_nvfp4_ct_bm32_down_m32",
    "reduce_nvfp4_ct_bm16",
    "pack_nvfp4_ct_s0_fp8",
];

fn nvfp4_ct_split_pipeline_supported(
    total_stages: usize,
    parts: usize,
    pipeline_stages: usize,
) -> bool {
    if total_stages == 0 || parts == 0 || pipeline_stages == 0 {
        return false;
    }
    let span = total_stages.div_ceil(parts);
    let Some(last_start) = (parts - 1).checked_mul(span) else {
        return false;
    };
    last_start < total_stages && total_stages - last_start >= pipeline_stages
}

fn nvfp4_ct_s0_manual_capable(
    vendor: forge_types::Vendor,
    arch: &str,
    warp_size: u32,
    max_threads_per_block: u32,
    mut has: impl FnMut(&str) -> bool,
) -> bool {
    let sm = arch
        .strip_prefix("sm_")
        .and_then(|value| value.parse::<u32>().ok());
    vendor == forge_types::Vendor::Nvidia
        && sm.is_some_and(|value| value >= 80)
        && warp_size == 32
        && max_threads_per_block >= 256
        && NVFP4_CT_S0_ARTIFACTS.iter().all(|name| has(name))
}

#[allow(clippy::too_many_arguments)]
fn validate_nvfp4_ct_repack_extents(
    target_bytes: usize,
    packed_bytes: usize,
    scale_bytes: usize,
    physical_rows: usize,
    cols: usize,
    source_rows: usize,
    target_row_offset: usize,
) -> Result<u32> {
    if physical_rows == 0
        || !physical_rows.is_multiple_of(64)
        || source_rows == 0
        || !source_rows.is_multiple_of(64)
        || !target_row_offset.is_multiple_of(64)
        || cols == 0
        || !cols.is_multiple_of(128)
    {
        return Err(ForgeError::Kernel(
            "repack NVFP4 CT wymaga pełnych kafli N64/K128".into(),
        ));
    }
    let target_end = target_row_offset
        .checked_add(source_rows)
        .ok_or_else(|| ForgeError::Kernel("repack NVFP4 CT: przepełnienie zakresu".into()))?;
    if target_end > physical_rows {
        return Err(ForgeError::Kernel(
            "repack NVFP4 CT: chunk wykracza poza resident".into(),
        ));
    }
    let required_target =
        checked_buffer_bytes("repack NVFP4 CT target", &[physical_rows, cols], 9)? / 16;
    let required_packed =
        checked_buffer_bytes("repack NVFP4 CT packed", &[source_rows, cols], 1)? / 2;
    let required_scales =
        checked_buffer_bytes("repack NVFP4 CT scales", &[source_rows, cols], 1)? / 16;
    if target_bytes != required_target
        || packed_bytes < required_packed
        || scale_bytes < required_scales
    {
        return Err(ForgeError::Kernel(
            "repack NVFP4 CT: niezgodny rozmiar bufora".into(),
        ));
    }
    let stages = (source_rows / 64)
        .checked_mul(cols / 128)
        .ok_or_else(|| ForgeError::Kernel("repack NVFP4 CT: przepełnienie siatki".into()))?;
    u32::try_from(stages)
        .map_err(|_| ForgeError::Kernel("repack NVFP4 CT: siatka przekracza u32".into()))
}

#[allow(clippy::too_many_arguments)]
fn validate_nvfp4_ct_b1_extents(
    output_bytes: usize,
    input_bytes: usize,
    physical_rows: usize,
    cols: usize,
    source_row_offset: usize,
    rows: usize,
    inv_global_scale: f32,
) -> Result<u32> {
    if rows == 0
        || !rows.is_multiple_of(64)
        || !source_row_offset.is_multiple_of(64)
        || !inv_global_scale.is_finite()
    {
        return Err(ForgeError::Kernel(
            "decode NVFP4 CT wymaga wyrównanego okna N64 i skończonej skali".into(),
        ));
    }
    let source_end = source_row_offset
        .checked_add(rows)
        .ok_or_else(|| ForgeError::Kernel("decode NVFP4 CT: przepełnienie zakresu".into()))?;
    let required_output = checked_buffer_bytes("decode NVFP4 CT output", &[rows], 2)?;
    let required_input = checked_buffer_bytes("decode NVFP4 CT input", &[cols], 2)?;
    if source_end > physical_rows || output_bytes < required_output || input_bytes < required_input
    {
        return Err(ForgeError::Kernel(
            "decode NVFP4 CT: okno lub bufor nie pasuje do widoku".into(),
        ));
    }
    u32::try_from(rows.div_ceil(8))
        .map_err(|_| ForgeError::Kernel("decode NVFP4 CT: siatka przekracza u32".into()))
}

/// Fizyczny układ macierzy stanu Gated-DeltaNet na urządzeniu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaStateLayout {
    KeyValue,
    ValueKey,
}

fn delta_state_layout_dispatch(
    d_state: usize,
    warp_size: u32,
    max_threads_per_block: u32,
    complete_artifacts: bool,
) -> DeltaStateLayout {
    if d_state == 128
        && warp_size > 0
        && 128u32.is_multiple_of(warp_size)
        && warp_size
            .checked_mul(4)
            .is_some_and(|threads| threads <= max_threads_per_block)
        && complete_artifacts
    {
        DeltaStateLayout::ValueKey
    } else {
        DeltaStateLayout::KeyValue
    }
}

fn has_delta_value_key_artifacts(mut has: impl FnMut(&str) -> bool) -> bool {
    [
        "deltanet_value_key_scan_inplace_f16",
        "deltanet_value_key_scan_checkpoints_f16",
        "deltanet_value_key_commit_recompute_f32",
        "deltanet_value_key_scan_persistent_f16",
    ]
    .into_iter()
    .all(&mut has)
}

fn validate_f32_byte_offset(name: &str, byte_offset: usize) -> Result<()> {
    if !byte_offset.is_multiple_of(std::mem::align_of::<f32>()) {
        return Err(ForgeError::Kernel(format!(
            "{name}: offset {byte_offset} nie jest wyrównany do f32"
        )));
    }
    Ok(())
}

/// Jedna projekcja Q8_0 korzystająca ze wspólnej przygotowanej aktywacji Q8_1.
pub struct Q8PreparedProjection<'a> {
    pub output: &'a DevBuffer,
    pub weights: &'a DevBuffer,
    pub weight_byte_offset: usize,
    pub rows: usize,
}

const ATTN_HD256_BLOCK: u32 = 256;

const ATTN_HD256_SPLITS: usize = 8;

fn verify_attn_split8_enabled(value: Option<&str>) -> bool {
    matches!(value, None | Some("auto") | Some("1"))
}

#[allow(clippy::too_many_arguments)]
fn validate_attn_verify_split8(
    output_bytes: usize,
    parts_bytes: usize,
    q_bytes: usize,
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    page_table_bytes: usize,
    seq_lens_bytes: usize,
    n_tokens: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    page_size: usize,
    max_pages: usize,
    scale: f32,
) -> Result<(u32, u32)> {
    if !matches!(n_tokens, 3 | 4)
        || n_q_heads == 0
        || n_kv_heads == 0
        || !n_q_heads.is_multiple_of(n_kv_heads)
        || page_size == 0
        || max_pages == 0
        || !scale.is_finite()
    {
        return Err(ForgeError::Kernel(
            "niepoprawny kształt split8 verifiera".into(),
        ));
    }
    let checked_bytes = |dims: &[usize], element_bytes: usize| {
        dims.iter()
            .try_fold(element_bytes, |value, &dim| value.checked_mul(dim))
    };
    let needed_out = checked_bytes(&[n_tokens, n_q_heads, 256], 2);
    let needed_parts = checked_bytes(&[n_tokens, n_q_heads, 8, 260], 4);
    let needed_cache = checked_bytes(&[max_pages, n_kv_heads, page_size, 256], 2);
    let needed_pages = max_pages.checked_mul(4);
    let needed_lens = n_tokens.checked_mul(4);
    if needed_out.is_none_or(|v| output_bytes < v || q_bytes < v)
        || needed_parts.is_none_or(|v| parts_bytes < v)
        || needed_cache.is_none_or(|v| k_cache_bytes < v || v_cache_bytes < v)
        || needed_pages.is_none_or(|v| page_table_bytes < v)
        || needed_lens.is_none_or(|v| seq_lens_bytes < v)
    {
        return Err(ForgeError::Kernel(
            "niepoprawny extent split8 verifiera".into(),
        ));
    }
    let grid_y = u32::try_from(n_q_heads)
        .map_err(|_| ForgeError::Kernel("liczba głów split8 przekracza u32".into()))?;
    let combine_grid_y = n_tokens
        .checked_mul(n_q_heads)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| ForgeError::Kernel("grid combine split8 przekracza u32".into()))?;
    Ok((grid_y, combine_grid_y))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttnDecodePlan {
    Generic(&'static str),
    Split8Hd64,
    Split8Hd128,
    Split8Hd256,
    Split8Hd512,
}

fn attn_decode_plan(
    head_dim: usize,
    warp_size: u32,
    max_threads: u32,
    split8_available: bool,
) -> Result<AttnDecodePlan> {
    match head_dim {
        64 if warp_size == 32 && max_threads >= ATTN_HD256_BLOCK && split8_available => {
            Ok(AttnDecodePlan::Split8Hd64)
        }
        64 => Ok(AttnDecodePlan::Generic("attn_decode_f16_hd64")),
        128 if warp_size == 32 && max_threads >= ATTN_HD256_BLOCK && split8_available => {
            Ok(AttnDecodePlan::Split8Hd128)
        }
        128 => Ok(AttnDecodePlan::Generic("attn_decode_f16_hd128")),
        // Podział kontekstu jest jedynym sposobem, żeby dekodowanie JEDNEJ
        // sekwencji wysyciło pamięć: wariant generyczny ma siatkę
        // (sekwencje, głowice), czyli 16 grup roboczych na 80 CU, i profiler
        // pokazał na nim 50 GB/s przy 397 GB/s na wagach. Warunek to fala 32
        // (obie rodziny kart) i skompilowane artefakty.
        256 if warp_size == 32 && max_threads >= ATTN_HD256_BLOCK && split8_available => {
            Ok(AttnDecodePlan::Split8Hd256)
        }
        256 => Ok(AttnDecodePlan::Generic("attn_decode_f16_hd256")),
        512 if warp_size == 32 && max_threads >= ATTN_HD256_BLOCK && split8_available => {
            Ok(AttnDecodePlan::Split8Hd512)
        }
        512 => Ok(AttnDecodePlan::Generic("attn_decode_f16_hd512")),
        other => Err(ForgeError::Unsupported(format!(
            "attn_decode: head_dim {other} has no compiled specialization"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_attn_decode_f16(
    output_bytes: usize,
    parts_bytes: usize,
    q_bytes: usize,
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    page_table_bytes: usize,
    seq_lens_bytes: usize,
    n_seqs: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    head_dim: usize,
    page_size: usize,
    max_pages: usize,
    scale: f32,
    split8: bool,
) -> Result<(u32, u32)> {
    if [
        n_seqs, n_q_heads, n_kv_heads, head_dim, page_size, max_pages,
    ]
    .contains(&0)
        || !scale.is_finite()
    {
        return Err(ForgeError::Kernel(
            "attention decode wymaga niezerowych wymiarów i skończonej skali".into(),
        ));
    }
    if !n_q_heads.is_multiple_of(n_kv_heads) {
        return Err(ForgeError::Kernel(
            "liczba głowic Q attention decode musi być wielokrotnością głowic KV".into(),
        ));
    }
    let vectors = checked_buffer_bytes(
        "attention decode Q/output",
        &[n_seqs, n_q_heads, head_dim],
        2,
    )?;
    let page_table = checked_buffer_bytes("attention decode page table", &[n_seqs, max_pages], 4)?;
    let seq_lens = checked_buffer_bytes("attention decode seq_lens", &[n_seqs], 4)?;
    let cache_page = checked_buffer_bytes(
        "attention decode strona KV",
        &[n_kv_heads, page_size, head_dim],
        2,
    )?;
    let _context_capacity = max_pages.checked_mul(page_size).ok_or_else(|| {
        ForgeError::Kernel("attention decode: przepełnienie pojemności kontekstu".into())
    })?;
    if output_bytes < vectors
        || q_bytes < vectors
        || page_table_bytes < page_table
        || seq_lens_bytes < seq_lens
        || k_cache_bytes < cache_page
        || v_cache_bytes < cache_page
        || k_cache_bytes != v_cache_bytes
        || !k_cache_bytes.is_multiple_of(cache_page)
    {
        return Err(ForgeError::Kernel(
            "attention decode ma za mały lub niezgodny bufor".into(),
        ));
    }
    if split8 {
        // Krok partycji to head_dim + 4 f32 (wektor, maksimum, mianownik, pad).
        let required_parts = checked_buffer_bytes(
            "attention decode scratch dzielony",
            &[n_seqs, n_q_heads, ATTN_HD256_SPLITS, head_dim + 4],
            4,
        )?;
        if parts_bytes < required_parts {
            return Err(ForgeError::Kernel(format!(
                "scratch dzielonej attention ma {parts_bytes} B, wymagane {required_parts} B"
            )));
        }
    }
    for (name, value) in [
        ("liczba sekwencji", n_seqs),
        ("liczba głowic Q", n_q_heads),
        ("liczba głowic KV", n_kv_heads),
        ("head_dim", head_dim),
        ("rozmiar strony", page_size),
        ("maksymalna liczba stron", max_pages),
    ] {
        i64::try_from(value)
            .map_err(|_| ForgeError::Kernel(format!("attention decode: {name} przekracza i64")))?;
    }
    let grid_x = u32::try_from(n_seqs)
        .map_err(|_| ForgeError::Kernel("attention decode: grid.x przekracza u32".into()))?;
    let grid_y = u32::try_from(n_q_heads)
        .map_err(|_| ForgeError::Kernel("attention decode: grid.y przekracza u32".into()))?;
    Ok((grid_x, grid_y))
}

#[allow(clippy::too_many_arguments)]
fn validate_attn_prefill_fa_f16_hd256(
    output_bytes: usize,
    q_bytes: usize,
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    page_table_bytes: usize,
    base_position: usize,
    n_tokens: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    page_size: usize,
    scale: f32,
) -> Result<(u32, u32)> {
    if [n_tokens, n_q_heads, n_kv_heads, page_size].contains(&0) || !scale.is_finite() {
        return Err(ForgeError::Kernel(
            "flash attention prefill wymaga poprawnych niezerowych wymiarów i skali".into(),
        ));
    }
    if !n_q_heads.is_multiple_of(n_kv_heads) {
        return Err(ForgeError::Kernel(
            "liczba głowic Q flash attention musi być wielokrotnością głowic KV".into(),
        ));
    }
    let query_bytes = checked_buffer_bytes(
        "flash attention query/output",
        &[n_tokens, n_q_heads, 256],
        2,
    )?;
    let cache_page_bytes = checked_buffer_bytes(
        "flash attention strona KV",
        &[n_kv_heads, page_size, 256],
        2,
    )?;
    let cache_pages = k_cache_bytes / cache_page_bytes;
    let page_table_entries = page_table_bytes / 4;
    let end_position = base_position.checked_add(n_tokens).ok_or_else(|| {
        ForgeError::Kernel("flash attention prefill: przepełnienie base + T".into())
    })?;
    let required_pages = end_position.div_ceil(page_size);
    if output_bytes < query_bytes
        || q_bytes < query_bytes
        || k_cache_bytes < cache_page_bytes
        || v_cache_bytes < cache_page_bytes
        || k_cache_bytes != v_cache_bytes
        || !k_cache_bytes.is_multiple_of(cache_page_bytes)
        || page_table_bytes < 4
        || !page_table_bytes.is_multiple_of(4)
        || required_pages > page_table_entries
        || required_pages > cache_pages
    {
        return Err(ForgeError::Kernel(
            "flash attention prefill ma za mały lub niezgodny bufor".into(),
        ));
    }
    for (name, value) in [
        ("T", n_tokens),
        ("głowice Q", n_q_heads),
        ("głowice KV", n_kv_heads),
        ("rozmiar strony", page_size),
        ("pozycja bazowa", base_position),
        ("pozycja końcowa", end_position),
    ] {
        i64::try_from(value)
            .map_err(|_| ForgeError::Kernel(format!("{name} flash attention przekracza i64")))?;
    }
    let tokens = u32::try_from(n_tokens)
        .map_err(|_| ForgeError::Kernel("liczba tokenów flash attention przekracza u32".into()))?;
    let heads = u32::try_from(n_q_heads)
        .map_err(|_| ForgeError::Kernel("liczba głowic flash attention przekracza u32".into()))?;
    Ok((tokens.div_ceil(64), heads))
}

/// Per-block logits slice of the sampling kernels (SAMPLE_CHUNK in
/// sampling.mojo — staged in shared memory by topk_partial_f32).
const SAMPLE_CHUNK: usize = 4096;

/// Largest top_k the GPU draw supports (MAX_TOPK in sampling.mojo).
pub const SAMPLE_MAX_TOPK: usize = 64;

/// Largest vocab the GPU top-k draw supports (MAX_SAMPLE_BLOCKS * CHUNK).
pub const SAMPLE_MAX_VOCAB: usize = 64 * SAMPLE_CHUNK;

/// Scratch capacity in (f32, i32) pairs both sampling paths share
/// (top-k: MAX_SAMPLE_BLOCKS * MAX_TOPK partials; argmax: one per block).
pub const SAMPLE_SCRATCH_PAIRS: usize = 64 * SAMPLE_MAX_TOPK;

fn checked_buffer_bytes(name: &str, dimensions: &[usize], element_bytes: usize) -> Result<usize> {
    dimensions
        .iter()
        .try_fold(element_bytes, |bytes, dimension| {
            bytes.checked_mul(*dimension).ok_or_else(|| {
                ForgeError::Kernel(format!(
                    "{name}: przepełnienie rozmiaru bufora dla wymiarów {dimensions:?}"
                ))
            })
        })
}

#[allow(clippy::too_many_arguments)]
fn validate_deltanet_gated_step_f16(
    out_bytes: usize,
    state_bytes: usize,
    q_bytes: usize,
    k_bytes: usize,
    v_bytes: usize,
    g_bytes: usize,
    beta_bytes: usize,
    n_v_heads: usize,
    d_state: usize,
    max_threads_per_block: u32,
) -> Result<(u32, u32)> {
    if n_v_heads == 0 || d_state == 0 {
        return Err(ForgeError::Kernel(
            "deltanet_gated_step wymaga niezerowych wymiarów".into(),
        ));
    }
    if d_state > 1024 || d_state > max_threads_per_block as usize {
        return Err(ForgeError::Kernel(format!(
            "deltanet_gated_step: d_state {d_state} przekracza limit bloku {}",
            max_threads_per_block.min(1024)
        )));
    }
    let grid_x = u32::try_from(n_v_heads).map_err(|_| {
        ForgeError::Kernel("deltanet_gated_step: liczba głów przekracza u32".into())
    })?;
    let block_x = u32::try_from(d_state).map_err(|_| {
        ForgeError::Kernel("deltanet_gated_step: rozmiar bloku przekracza u32".into())
    })?;
    let vector_bytes =
        checked_buffer_bytes("deltanet_gated_step vectors", &[n_v_heads, d_state], 2)?;
    let state_required = checked_buffer_bytes(
        "deltanet_gated_step state",
        &[n_v_heads, d_state, d_state],
        4,
    )?;
    let gate_bytes = checked_buffer_bytes("deltanet_gated_step gates", &[n_v_heads], 4)?;
    if out_bytes < vector_bytes
        || state_bytes < state_required
        || q_bytes < vector_bytes
        || k_bytes < vector_bytes
        || v_bytes < vector_bytes
        || g_bytes < gate_bytes
        || beta_bytes < gate_bytes
    {
        return Err(ForgeError::Kernel(
            "deltanet_gated_step: co najmniej jeden bufor jest za mały".into(),
        ));
    }
    Ok((grid_x, block_x))
}

#[allow(clippy::too_many_arguments)]
fn validate_kv_append_batch_segmented_masked_f16(
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    k_input_bytes: usize,
    v_input_bytes: usize,
    page_table_bytes: usize,
    base_position_bytes: usize,
    decision_bytes: usize,
    batch: usize,
    n_tokens: usize,
    max_pages: usize,
    n_kv_heads: usize,
    page_size: usize,
    head_dim: usize,
) -> Result<(u32, u32, u32)> {
    if [batch, n_tokens, max_pages, n_kv_heads, page_size, head_dim].contains(&0) {
        return Err(ForgeError::Kernel(
            "maskowany segmentowany append KV wymaga niezerowych wymiarów".into(),
        ));
    }
    let total = batch.checked_mul(n_tokens).ok_or_else(|| {
        ForgeError::Kernel("przepełnienie liczby tokenów maskowanego append KV".into())
    })?;
    let input_bytes = checked_buffer_bytes(
        "maskowany segmentowany append KV input",
        &[total, n_kv_heads, head_dim],
        2,
    )?;
    let cache_page_bytes = checked_buffer_bytes(
        "maskowany segmentowany append KV cache page",
        &[n_kv_heads, page_size, head_dim],
        2,
    )?;
    let required_page_table_bytes = checked_buffer_bytes(
        "maskowany segmentowany append KV page table",
        &[batch, max_pages],
        4,
    )?;
    let required_base_bytes = checked_buffer_bytes(
        "maskowany segmentowany append KV base positions",
        &[batch],
        4,
    )?;
    let required_decision_bytes =
        checked_buffer_bytes("maskowany segmentowany append KV decisions", &[batch, 2], 4)?;
    if k_input_bytes < input_bytes
        || v_input_bytes < input_bytes
        || page_table_bytes < required_page_table_bytes
        || base_position_bytes < required_base_bytes
        || decision_bytes < required_decision_bytes
        || k_cache_bytes != v_cache_bytes
        || k_cache_bytes < cache_page_bytes
        || !k_cache_bytes.is_multiple_of(cache_page_bytes)
    {
        return Err(ForgeError::Kernel(
            "maskowany segmentowany append KV ma bufor mniejszy lub niezgodny z układem F16".into(),
        ));
    }
    for (name, value) in [
        ("T", n_tokens),
        ("max_pages", max_pages),
        ("n_kv_heads", n_kv_heads),
        ("page_size", page_size),
        ("head_dim", head_dim),
    ] {
        i64::try_from(value).map_err(|_| {
            ForgeError::Kernel(format!(
                "{name} maskowanego segmentowanego append KV przekracza i64"
            ))
        })?;
    }
    let grid_x = u32::try_from(total).map_err(|_| {
        ForgeError::Kernel("liczba tokenów maskowanego append KV przekracza u32".into())
    })?;
    let grid_y = u32::try_from(n_kv_heads).map_err(|_| {
        ForgeError::Kernel("liczba głów maskowanego append KV przekracza u32".into())
    })?;
    let block = u32::try_from(head_dim)
        .map_err(|_| ForgeError::Kernel("head_dim append KV przekracza u32".into()))?
        .clamp(32, 256);
    Ok((grid_x, grid_y, block))
}

#[allow(clippy::too_many_arguments)]
fn validate_kv_append_batch_segmented_f16(
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    k_input_bytes: usize,
    v_input_bytes: usize,
    page_table_bytes: usize,
    base_position_bytes: usize,
    batch: usize,
    n_tokens: usize,
    max_pages: usize,
    n_kv_heads: usize,
    page_size: usize,
    head_dim: usize,
    max_threads_per_block: u32,
) -> Result<(u32, u32, u32)> {
    if [batch, n_tokens, max_pages, n_kv_heads, page_size, head_dim].contains(&0) {
        return Err(ForgeError::Kernel(
            "segmentowany append KV wymaga niezerowych wymiarów".into(),
        ));
    }
    let total = batch.checked_mul(n_tokens).ok_or_else(|| {
        ForgeError::Kernel("przepełnienie liczby tokenów segmentowanego append KV".into())
    })?;
    let input_bytes = checked_buffer_bytes(
        "segmentowany append KV input",
        &[total, n_kv_heads, head_dim],
        2,
    )?;
    let cache_page_bytes = checked_buffer_bytes(
        "segmentowany append KV cache page",
        &[n_kv_heads, page_size, head_dim],
        2,
    )?;
    let required_page_table_bytes =
        checked_buffer_bytes("segmentowany append KV page table", &[batch, max_pages], 4)?;
    let required_base_bytes =
        checked_buffer_bytes("segmentowany append KV base positions", &[batch], 4)?;
    if k_input_bytes < input_bytes
        || v_input_bytes < input_bytes
        || page_table_bytes < required_page_table_bytes
        || base_position_bytes < required_base_bytes
        || k_cache_bytes != v_cache_bytes
        || k_cache_bytes < cache_page_bytes
        || !k_cache_bytes.is_multiple_of(cache_page_bytes)
    {
        return Err(ForgeError::Kernel(
            "segmentowany append KV ma bufor mniejszy lub niezgodny z układem F16".into(),
        ));
    }
    for (name, value) in [
        ("T", n_tokens),
        ("max_pages", max_pages),
        ("n_kv_heads", n_kv_heads),
        ("page_size", page_size),
        ("head_dim", head_dim),
    ] {
        i64::try_from(value).map_err(|_| {
            ForgeError::Kernel(format!("{name} segmentowanego append KV przekracza i64"))
        })?;
    }
    let grid_x = u32::try_from(total).map_err(|_| {
        ForgeError::Kernel("liczba tokenów segmentowanego append KV przekracza u32".into())
    })?;
    let grid_y = u32::try_from(n_kv_heads).map_err(|_| {
        ForgeError::Kernel("liczba głów segmentowanego append KV przekracza u32".into())
    })?;
    let block = u32::try_from(head_dim)
        .map_err(|_| ForgeError::Kernel("head_dim append KV przekracza u32".into()))?
        .clamp(32, 256);
    if block > max_threads_per_block {
        return Err(ForgeError::Kernel(format!(
            "blok segmentowanego append KV {block} przekracza limit urządzenia {max_threads_per_block}"
        )));
    }
    Ok((grid_x, grid_y, block))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Nvfp4GgufDispatch {
    kernel: &'static str,
    token_tile: usize,
    row_tile: usize,
    block_threads: u32,
}

/// Kafel GEMM na instrukcjach dot (karty bez jednostki macierzowej).
///
/// Liczba wątków bloku NIE jest tu podawana ręcznie, tylko wyliczana z wymiarów
/// kafla — kernel dzieli kafel BM x BN na wątki po TM x TN wyników, więc każda
/// inna wartość wysyła nadmiarowe wątki poza kafel w LDS. Wpisywana ręcznie
/// rozjechała się z instancją kernela i dawała niedeterministyczne wyniki, które
/// wyszły dopiero na bramce powtarzalności tokenów w `forge bench`.
#[derive(Clone, Copy)]
pub(crate) struct DotTile {
    name: &'static str,
    bm: u32,
    bn: u32,
    tm: u32,
    tn: u32,
}

impl DotTile {
    const fn new(name: &'static str, bm: u32, bn: u32, tm: u32, tn: u32) -> Self {
        Self {
            name,
            bm,
            bn,
            tm,
            tn,
        }
    }

    fn config(&self, rows: usize, n_tokens: usize) -> LaunchConfig {
        LaunchConfig {
            grid: (
                (rows as u32).div_ceil(self.bn),
                (n_tokens as u32).div_ceil(self.bm),
                1,
            ),
            block: ((self.bm / self.tm) * (self.bn / self.tn), 1, 1),
            shared_mem_bytes: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn nvfp4_gguf_dispatch(
    n_tokens: usize,
    n_rows: usize,
    n_cols: usize,
    prefetch_available: bool,
    sync1_available: bool,
    bn128_available: bool,
    is_nvidia: bool,
    wmma_available: bool,
    wmma_bn128_available: bool,
    int8_batch_available: bool,
    warp_size: u32,
    max_threads: u32,
) -> Result<Nvfp4GgufDispatch> {
    if n_tokens < 2 {
        return Err(ForgeError::Kernel(
            "gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch tokenów".into(),
        ));
    }
    let (kernel, token_tile, row_tile, block_threads) = match n_tokens {
        // Weryfikacja MTP liczy 2-4 tokeny naraz. Rodzina `b*` ma właściwą
        // strukturę (fala na wiersz), ale idzie ścieżką f16 — przy T=4 mierzyła
        // 152 us wobec 63 us GEMV-a int8 na tym samym kształcie, mimo że czyta
        // TE SAME wagi. Wariant int8 dekoduje wagę raz na falę i używa jej dla
        // wszystkich tokenów.
        2 if int8_batch_available => ("gemv_nvfp4_gguf_q8_1_b2_f16", 2, 8, Some(BLOCK)),
        3..=4 if int8_batch_available => ("gemv_nvfp4_gguf_q8_1_b4_f16", 4, 8, Some(BLOCK)),
        5..=8 if int8_batch_available => ("gemv_nvfp4_gguf_q8_1_b8_f16", 8, 8, Some(BLOCK)),
        2 => ("gemm_nvfp4_gguf_f16_b2", 2, 1, Some(warp_size)),
        3 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b3_nvidia",
            3,
            2,
            warp_size.checked_mul(2),
        ),
        4 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b4_nvidia",
            4,
            2,
            warp_size.checked_mul(2),
        ),
        3 => ("gemm_nvfp4_gguf_f16_b3", 3, 1, Some(warp_size)),
        4 => ("gemm_nvfp4_gguf_f16_b4", 4, 1, Some(warp_size)),
        5..=8 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b8_nvidia",
            8,
            2,
            warp_size.checked_mul(2),
        ),
        5..=8 => ("gemm_nvfp4_gguf_f16_b8", 8, 1, warp_size.checked_mul(8)),
        9..=16 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b16_nvidia",
            16,
            2,
            warp_size.checked_mul(2),
        ),
        9..=16 => ("gemm_nvfp4_gguf_f16_b16", 16, 1, warp_size.checked_mul(16)),
        17..=32 if is_nvidia && warp_size == 32 => {
            ("gemm_nvfp4_gguf_mma_f16_bm32", 32, 64, Some(64))
        }
        128 if n_rows == 1024 && is_nvidia && warp_size == 32 => {
            ("gemm_nvfp4_gguf_mma_f16_bm128_bn32", 128, 32, Some(128))
        }
        _ if n_tokens >= 256
            && n_tokens.is_multiple_of(128)
            && bn128_available
            && is_nvidia
            && warp_size == 32 =>
        {
            ("gemm_nvfp4_gguf_mma_f16_bm128_bn128", 128, 128, Some(256))
        }
        128 if sync1_available
            && !(n_rows == 17408 && n_cols == 5120)
            && is_nvidia
            && warp_size == 32 =>
        {
            (
                "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1",
                128,
                64,
                Some(256),
            )
        }
        _ if n_tokens.is_multiple_of(128) && prefetch_available && is_nvidia && warp_size == 32 => {
            ("gemm_nvfp4_gguf_mma_f16_bm128_prefetch", 128, 64, Some(256))
        }
        _ if is_nvidia && warp_size == 32 => ("gemm_nvfp4_gguf_mma_f16_bm128", 128, 64, Some(256)),
        // Kafle WMMA (RDNA3+) liczą tę samą matematykę co kafle `mma`. Nie
        // powielamy tu strojenia NVIDII (sync1/prefetch/bn128) — te warianty
        // istnieją tylko w rodzinie `mma`, a ich odpowiedniki trzeba osobno
        // zmierzyć, zanim zaczną cokolwiek wybierać.
        //
        // Podział na BM32 i BM256 jest ZMIERZONY, nie domyślny: wymuszenie
        // BM32 na prefillu 2048 tokenów Qwen3.6-27B NVFP4 dało 305,9 tok/s
        // wobec 826,2 dla BM256. Duże BM amortyzuje dekwantyzację — każdy
        // rozpakowany element wagi jest reużyty przez BM wierszy tokenów.
        17..=32 if wmma_available => ("gemm_nvfp4_gguf_wmma_f16_bm32", 32, 64, Some(128)),
        // BN=128 czyta aktywacje `rows / 128` razy zamiast `rows / 64`; na
        // każdym zmierzonym kształcie 27B wygrywa (liczby w nagłówku kernela).
        // BM=512 potrzebuje T >= 512, żeby mieć czym wypełnić kafel.
        _ if wmma_bn128_available && n_tokens >= 512 => {
            ("gemm_nvfp4_gguf_wmma_f16_bm512_bn128", 512, 128, Some(512))
        }
        _ if wmma_bn128_available => ("gemm_nvfp4_gguf_wmma_f16_bm256_bn128", 256, 128, Some(256)),
        _ if wmma_available => ("gemm_nvfp4_gguf_wmma_f16_bm256", 256, 64, Some(256)),
        _ => {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_f16: backend bez jednostki macierzowej nie obsługuje T={n_tokens} > 16"
            )));
        }
    };
    let block_threads = block_threads.ok_or_else(|| {
        ForgeError::Kernel("gemm_nvfp4_gguf_f16: przepełnienie rozmiaru bloku".into())
    })?;
    if block_threads == 0 || block_threads > max_threads {
        return Err(ForgeError::Kernel(format!(
            "gemm_nvfp4_gguf_f16: blok {block_threads} przekracza limit urządzenia {max_threads}"
        )));
    }
    Ok(Nvfp4GgufDispatch {
        kernel,
        token_tile,
        row_tile,
        block_threads,
    })
}

#[allow(clippy::too_many_arguments)]
fn nvfp4_gguf_layout_dispatch(
    layout: Nvfp4GgufLayout,
    n_tokens: usize,
    n_rows: usize,
    n_cols: usize,
    prefetch_available: bool,
    sync1_available: bool,
    bn128_available: bool,
    tile_bn64_available: bool,
    tile_bn128_available: bool,
    is_nvidia: bool,
    wmma_available: bool,
    wmma_bn128_available: bool,
    int8_batch_available: bool,
    warp_size: u32,
    max_threads: u32,
) -> Result<Nvfp4GgufDispatch> {
    match layout {
        Nvfp4GgufLayout::RowMajor36 => nvfp4_gguf_dispatch(
            n_tokens,
            n_rows,
            n_cols,
            prefetch_available,
            sync1_available,
            bn128_available,
            is_nvidia,
            wmma_available,
            wmma_bn128_available,
            int8_batch_available,
            warp_size,
            max_threads,
        ),
        Nvfp4GgufLayout::TileN128K64 => {
            if n_tokens < 2 {
                return Err(ForgeError::Kernel(
                    "gemm NVFP4 TileN128K64 wymaga co najmniej dwóch tokenów".into(),
                ));
            }
            if !is_nvidia || warp_size != 32 || max_threads < 256 || !tile_bn64_available {
                return Err(ForgeError::Unsupported(
                    "układ NVFP4 TileN128K64 wymaga NVIDIA warp32 i kernela BN64".into(),
                ));
            }
            if n_rows == 0 || !n_rows.is_multiple_of(128) || !n_cols.is_multiple_of(64) {
                return Err(ForgeError::Kernel(format!(
                    "układ NVFP4 TileN128K64 wymaga rows % 128 == 0 i cols % 64 == 0; rows={n_rows}, cols={n_cols}"
                )));
            }
            let bn128 = n_tokens >= 256 && tile_bn128_available;
            Ok(Nvfp4GgufDispatch {
                kernel: if bn128 {
                    "gemm_nvfp4_tile128_mma_f16_bm128_bn128"
                } else {
                    "gemm_nvfp4_tile128_mma_f16_bm128_bn64"
                },
                token_tile: 128,
                row_tile: if bn128 { 128 } else { 64 },
                block_threads: 256,
            })
        }
    }
}

/// Czy backend uciągnie decode NVFP4 przez całkowitoliczbowy iloczyn dp4a.
///
/// Warunkiem jest fala 32 i instrukcja iloczynu czterech bajtów — `dp4a` na
/// NVIDII, `v_dot4_i32_i8` na RDNA2 i `v_dot4_i32_iu8` na RDNA3+, wszystkie
/// pod jednym helperem `dot4_i8`. Stał tu warunek `is_nvidia`, przez co AMD
/// schodziło na wariant f16, który w izolacji mierzy 486 GB/s wobec 993 GB/s
/// ścieżki int8 — konwersje f16->f32 w pętli, a nie pamięć, były wąskim gardłem.
fn raw_nvfp4_dp4a_supported(warp_size: u32) -> bool {
    warp_size == 32
}

fn has_nvfp4_gguf_tile_artifacts(mut has: impl FnMut(&str) -> bool) -> bool {
    [
        "quantize_act_q8_1",
        "nvfp4_repack_tile128",
        "gemv_nvfp4_tile128_coop_q8_1_f16",
        "gemm_nvfp4_tile128_mma_f16_bm128_bn64",
        "gemm_nvfp4_tile128_mma_f16_bm128_bn128",
    ]
    .iter()
    .all(|name| has(name))
}

fn q8_nvfp4_pack_launch(warp_size: u32) -> (usize, u32) {
    if warp_size == 32 {
        (2, 256)
    } else {
        (1, 64)
    }
}

const HYBRID_PREFILL_B2_ARTIFACTS: [&str; 4] = [
    "deltanet_prepare_segmented_final_f16",
    "deltanet_gated_scan_segmented_shared_d128_f16",
    "gemm_nvfp4_gguf_out_f32_b2",
    "gemm_q8_0_f16_exact_out_f32_b2",
];

/// Rdzeń wspólny obu rodzinom: skany DeltaNet i batchowe GEMM-y NVFP4.
const HYBRID_PREFILL_T128_SHARED: [&str; 4] = [
    "deltanet_gated_scan_inplace_shared_d128_f16",
    "deltanet_gated_scan_inplace_dynamic_d128_f16",
    "gemm_nvfp4_gguf_f16_b2",
    "gemm_nvfp4_gguf_f16_b16",
];

/// Kafle na jednostce macierzowej. Ta sama matematyka, dwie rodziny instrukcji:
/// `mma`/`ldmatrix` na NVIDII i WMMA na RDNA3+. Rozdzielone, bo to JEDYNA
/// różnica między backendami w tej ścieżce — reszta kontraktu jest identyczna.
const HYBRID_PREFILL_T128_MATRIX_NVIDIA: [&str; 6] = [
    "gemm_nvfp4_gguf_f16_b3_nvidia",
    "gemm_nvfp4_gguf_f16_b4_nvidia",
    "gemm_nvfp4_gguf_f16_b8_nvidia",
    "gemm_nvfp4_gguf_mma_f16_bm32",
    "gemm_nvfp4_gguf_mma_f16_bm128",
    "gemm_nvfp4_gguf_mma_f16_bm128_bn32",
];

const HYBRID_PREFILL_T128_MATRIX_AMD: [&str; 5] = [
    "gemm_nvfp4_gguf_f16_b3",
    "gemm_nvfp4_gguf_f16_b4",
    "gemm_nvfp4_gguf_f16_b8",
    "gemm_nvfp4_gguf_wmma_f16_bm32",
    "gemm_nvfp4_gguf_wmma_f16_bm256",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DensePrefillLogitsKind {
    F16 {
        rows: usize,
        cols: usize,
    },
    Q8_0 {
        rows: usize,
        cols: usize,
    },
    NvFp4Gguf {
        rows: usize,
        cols: usize,
    },
    /// Q4_K/Q6_K heads run the per-lane dp4a GEMV sweep inside `logits_gemm`
    /// (no batched GEMM-out-f32 kernel; one weight read per lane).
    Q4K {
        rows: usize,
        cols: usize,
    },
    Q6K {
        rows: usize,
        cols: usize,
    },
}

/// Wymagania SPRZĘTOWE równego dense prefillu, bez pytania o producenta.
///
/// Kernele segmentowane zakładają falę 32 wątków i blok 256 — to jest cały
/// kontrakt. Wcześniej stał tu warunek „tylko NVIDIA", który wyłączał tę
/// ścieżkę na Radeonach mimo obecności wszystkich potrzebnych artefaktów;
/// resztę i tak sprawdza `dense_prefill_artifacts_capable`.
fn dense_prefill_backend_capable(warp_size: u32, max_threads_per_block: u32) -> bool {
    warp_size == 32 && max_threads_per_block >= 256
}

fn dense_prefill_artifacts_capable(
    head_dim: usize,
    batch: usize,
    logits: DensePrefillLogitsKind,
    mut has: impl FnMut(&str) -> bool,
) -> bool {
    if !matches!(batch, 4 | 8 | 16)
        || !has("kv_append_batch_segmented_f16")
        || !has("argmax_batched_f32")
        || !has("topk_batched_partial_f32")
        || !has("topk_batched_final_f32")
        || !has("penalize_batched_f32")
    {
        return false;
    }
    // HD128 ma dwa równoważne kernele: FA (mma NVIDII) i przenośny kafel
    // segmentowany. Wystarczy jeden z nich — wybór należy do miejsca wywołania.
    let attention = match head_dim {
        128 => {
            has("attn_prefill_fa_segmented_f16_hd128") || has("attn_prefill_segmented_f16_hd128")
        }
        256 => has("attn_prefill_segmented_f16_hd256"),
        _ => false,
    };
    if !attention {
        return false;
    }
    match logits {
        DensePrefillLogitsKind::F16 { rows, cols } => {
            if rows == 0 || cols < 8 || !cols.is_multiple_of(8) {
                return false;
            }
            let kernel = Kernels::f16_out_f32_dispatch(rows, batch, |name| has(name)).0;
            has(kernel)
        }
        DensePrefillLogitsKind::Q8_0 { rows, cols } => {
            if rows == 0 || !cols.is_multiple_of(32) {
                return false;
            }
            match batch {
                4 => has("gemm_q8_0_f16_exact_out_f32_b4"),
                8 => has("gemm_q8_0_f16_exact_out_f32_b8"),
                // B16 idzie przez `gemm_q8_0_out_f32_at`, które na kartach bez
                // `mma` NVIDII wybiera kafel WMMA albo `dot4`. Zdolność ma więc
                // każda z tych trzech dróg.
                16 => {
                    has(Kernels::q8_0_out_f32_kernel(rows, batch))
                        || has("gemm_q8_0_wmma_out_f32_16x64")
                        || has("gemm_q8_0_dot4_out_f32_64x64")
                }
                _ => false,
            }
        }
        DensePrefillLogitsKind::NvFp4Gguf { rows, cols } => {
            if rows == 0 || !cols.is_multiple_of(64) {
                return false;
            }
            let kernel = match batch {
                4 => "gemm_nvfp4_gguf_out_f32_b4",
                8 => "gemm_nvfp4_gguf_out_f32_b8",
                16 => "gemm_nvfp4_gguf_out_f32_b16",
                _ => return false,
            };
            has(kernel)
        }
        DensePrefillLogitsKind::Q4K { rows, cols } => {
            rows > 0
                && cols.is_multiple_of(256)
                && has(if cols <= Kernels::DP4A_MAX_COLS {
                    "gemv_q4_k_dp4a_out_f32"
                } else {
                    "gemv_q4_k_out_f32_v2"
                })
        }
        DensePrefillLogitsKind::Q6K { rows, cols } => {
            rows > 0
                && cols.is_multiple_of(256)
                && has(if cols <= Kernels::DP4A_MAX_COLS {
                    "gemv_q6_k_dp4a_out_f32"
                } else {
                    "gemv_q6_k_out_f32_v2"
                })
        }
    }
}

fn has_hybrid_prefill_b2_artifacts(mut has: impl FnMut(&str) -> bool) -> bool {
    HYBRID_PREFILL_B2_ARTIFACTS.iter().all(|name| has(name))
}

fn has_hybrid_prefill_t128_artifacts(nvidia: bool, mut has: impl FnMut(&str) -> bool) -> bool {
    let matrix: &[&str] = if nvidia {
        &HYBRID_PREFILL_T128_MATRIX_NVIDIA
    } else {
        &HYBRID_PREFILL_T128_MATRIX_AMD
    };
    let triplet = if nvidia {
        "gemm_q8_0_i8mma_triplet_bm64"
    } else {
        "gemm_q8_0_wmma_triplet_bm64"
    };
    HYBRID_PREFILL_T128_SHARED
        .iter()
        .chain(matrix.iter())
        .all(|name| has(name))
        && has(triplet)
}

fn hybrid_prefill_nvfp4_artifact_chunk_limit(
    nvidia_warp32: bool,
    mut has: impl FnMut(&str) -> bool,
) -> usize {
    let variant = |generic, nvidia| if nvidia_warp32 { nvidia } else { generic };
    if !has("deltanet_gated_scan_inplace_dynamic_d128_f16")
        || !has("gemm_nvfp4_gguf_f16_b2")
        || !has(variant(
            "gemm_nvfp4_gguf_f16_b3",
            "gemm_nvfp4_gguf_f16_b3_nvidia",
        ))
    {
        return 0;
    }
    if !has(variant(
        "gemm_nvfp4_gguf_f16_b4",
        "gemm_nvfp4_gguf_f16_b4_nvidia",
    )) {
        return 3;
    }
    if !has(variant(
        "gemm_nvfp4_gguf_f16_b8",
        "gemm_nvfp4_gguf_f16_b8_nvidia",
    )) {
        return 4;
    }
    if !has(variant(
        "gemm_nvfp4_gguf_f16_b16",
        "gemm_nvfp4_gguf_f16_b16_nvidia",
    )) {
        return 8;
    }
    let matrix_bm32 = variant(
        "gemm_nvfp4_gguf_wmma_f16_bm32",
        "gemm_nvfp4_gguf_mma_f16_bm32",
    );
    let triplet = variant(
        "gemm_q8_0_wmma_triplet_bm64",
        "gemm_q8_0_i8mma_triplet_bm64",
    );
    if !has(matrix_bm32) || !has(triplet) {
        return 16;
    }
    if !has("deltanet_gated_scan_inplace_shared_d128_f16")
        || !has(variant(
            "gemm_nvfp4_gguf_wmma_f16_bm256",
            "gemm_nvfp4_gguf_mma_f16_bm128",
        ))
        || (nvidia_warp32 && !has("gemm_nvfp4_gguf_mma_f16_bm128_bn32"))
    {
        return 32;
    }
    128
}

pub struct Kernels {
    device: Arc<dyn Device>,
    artifacts: KernelArtifacts,
    /// Codebook grid tables for the IQ formats, uploaded once at load
    /// (ggml iq2xs/iq2s/iq3s grids + ksigns; kernels take them as device
    /// pointers — the constant-table trick llama.cpp's CUDA kernels use).
    iq_tables: IqTables,
    /// Grow-only q8_1 scratch for the i8mma prefill GEMM: the activation tile is
    /// quantized ONCE (`quantize_act_q8_1`) into `xq` (int8 [T,K]) + `xd`/`xsm`
    /// (f32 [T,K/32]) here, then every weight-row block reads int8 X directly
    /// instead of re-quantizing f16 X per block. Sized to the largest (T*K) seen.
    prequant: Mutex<PrequantScratch>,
    /// Scratch grupy prepared-Q8 ma osobny cykl życia i zależność między streamami.
    prepared_q8: Mutex<PrequantScratch>,
    /// Grow-only per-token int8 activation scratch for the W4A8 GEMM: `x` is
    /// quantized ONCE into `a_i8` (int8 [T,K]) + `ascales` (f16 [T]) by
    /// `w4a8_quant_act`, then `w4a8_gemm` reads them directly. Non-default path
    /// (FORGE_GEMM=w4a8); separate from the q8_1 `prequant` (different layout).
    w4a8_act: Mutex<W4A8ActScratch>,
    /// Grow-only per-token e4m3 activation scratch for the fp8 prefill GEMM
    /// (FORGE_GEMM=fp8).
    fp8_act: Mutex<Fp8ActScratch>,
    /// Grow-only scratch for the native-GGUF-layout Mojo int8 Q4_K prefill GEMM
    /// (`gemm_q4k_i8_native`): the MPAD-padded f16 activation, its int8 q8_1 codes
    /// and block-major da/sa scales. Separate layout from `prequant` (padded to
    /// the compile-time token ceiling MPAD, not the real token count).
    q4k_native: Mutex<Q4kNativeScratch>,
    /// (`gemm_qk_dp4a_batch_at`): dedicated small-batch decode quant scratch.
    qk_batch: Mutex<QkBatchScratch>,
    /// (`sample_batched_topk_f32`): two-pass batched top-k parts scratch.
    sample_parts: Mutex<SamplePartsScratch>,
    /// Backend attention dla prefill F16 hd64/hd128. Domyślnie wybiera kernel
    /// Mojo skompilowany obecnie do PTX; obsługa AMDGPU i Metal wymaga osobnych
    /// backendów HAL i artefaktów. `FORGE_ATTN=fa` wybiera cubin tylko wtedy,
    /// gdy jest dostępny dla bieżącej architektury NVIDIA.
    attn: AttnBackend,
}

/// Dense prefill attention routing (FORGE_ATTN).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttnBackend {
    /// Scalar/SIMD online-softmax Mojo kernel (`attn_prefill`).
    Scalar,
    /// Tensor-core flash-attention CUDA cubin (`fattn_prefill.cu`).
    Cuda,
    /// Tensor-core flash-attention Mojo kernel (`attn_prefill_fa_mma`).
    Mojo,
}

/// Device-resident q8_1 activation scratch shared by the i8mma GEMM launches.
#[derive(Default)]
struct PrequantScratch {
    xq: Option<DevBuffer>,
    xd: Option<DevBuffer>,
    xsm: Option<DevBuffer>,
    /// Current int8-code capacity (elements) of `xq`.
    cap_codes: usize,
    /// Current f32 capacity (elements) of `xd`/`xsm`.
    cap_blocks: usize,
    /// Marker ostatniego użycia buforów przez asynchroniczny stream.
    ready: Option<Event>,
    /// Błąd synchronizacji zabrania ponownego użycia niepewnych buforów.
    poisoned: bool,
}

fn ensure_prepared_q8_usable(scratch: &PrequantScratch) -> Result<()> {
    if scratch.poisoned {
        return Err(ForgeError::Kernel(
            "prepared Q8 scratch jest zatruty po błędzie synchronizacji".into(),
        ));
    }
    Ok(())
}

fn lock_prepared_q8_scratch(
    scratch: &Mutex<PrequantScratch>,
) -> Result<std::sync::MutexGuard<'_, PrequantScratch>> {
    match scratch.lock() {
        Ok(guard) => {
            ensure_prepared_q8_usable(&guard)?;
            Ok(guard)
        }
        Err(mut error) => {
            error.get_mut().poisoned = true;
            Err(ForgeError::Kernel(
                "prepared Q8 scratch jest zatruty przez panic podczas użycia".into(),
            ))
        }
    }
}

fn resolve_prepared_q8_marker(
    scratch: &mut PrequantScratch,
    record_result: Result<()>,
    synchronize: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let Err(record_error) = record_result else {
        return Ok(());
    };
    match synchronize() {
        Ok(()) => {
            scratch.ready = None;
            Err(record_error)
        }
        Err(sync_error) => {
            scratch.ready = None;
            scratch.poisoned = true;
            Err(ForgeError::Kernel(format!(
                "prepared Q8: zapis markera nie powiódł się: {record_error}; synchronizacja awaryjna nie powiodła się: {sync_error}"
            )))
        }
    }
}

fn mark_prepared_q8_ready(
    device: &dyn Device,
    scratch: &mut PrequantScratch,
    stream: &Stream,
) -> Result<()> {
    let ready =
        scratch.ready.as_ref().cloned().ok_or_else(|| {
            ForgeError::Kernel("prepared Q8: brak eventu gotowości scratch".into())
        })?;
    #[cfg(test)]
    let record_result = if take_prepared_q8_fault(&PREPARED_Q8_RECORD_FAILURES) {
        Err(ForgeError::Kernel(
            "wstrzyknięty błąd record prepared Q8".into(),
        ))
    } else {
        device.record_event(&ready, stream)
    };
    #[cfg(not(test))]
    let record_result = device.record_event(&ready, stream);
    resolve_prepared_q8_marker(scratch, record_result, || {
        #[cfg(test)]
        if take_prepared_q8_fault(&PREPARED_Q8_SYNC_FAILURES) {
            return Err(ForgeError::Device(
                "wstrzyknięty błąd sync prepared Q8".into(),
            ));
        }
        stream.synchronize()
    })
}

/// Krótkotrwały widok aktywacji Q8_1 utrzymujący scratch do końca grupy GEMM.
pub struct Q8ActPrepared<'a> {
    scratch: std::sync::MutexGuard<'a, PrequantScratch>,
    stream: &'a Stream,
    cols: usize,
    n_tokens: usize,
    valid: bool,
}

#[cfg(test)]
static PREPARED_Q8_RECORD_FAILURES: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
static PREPARED_Q8_SYNC_FAILURES: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
static PREPARED_Q8_GEMM_LAUNCHES: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
fn take_prepared_q8_fault(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_sub(1)
        })
        .is_ok()
}

/// Device-resident per-token int8 activation scratch for the W4A8 GEMM.
#[derive(Default)]
struct W4A8ActScratch {
    a_i8: Option<DevBuffer>,
    ascales: Option<DevBuffer>,
    /// Current int8-code capacity (elements) of `a_i8`.
    cap_codes: usize,
    /// Current token capacity of `ascales`.
    cap_tokens: usize,
}

/// Device-resident per-token e4m3 activation scratch for the fp8 GEMM: `x` is
/// quantized ONCE into `xq` (e4m3 bytes [T,K]) + `xs` (f32 per-token scale [T])
/// by `quantize_act_fp8`, then `gemm_fp8` reads them directly. Non-default path
/// (FORGE_GEMM=fp8); separate layout from the q8_1 `prequant`.
#[derive(Default)]
struct Fp8ActScratch {
    xq: Option<DevBuffer>,
    xs: Option<DevBuffer>,
    /// Current e4m3-code capacity (elements) of `xq`.
    cap_codes: usize,
    /// Current token capacity of `xs`.
    cap_tokens: usize,
}

/// Device-resident scratch for the native-GGUF-layout int8 Q4_K prefill GEMM.
/// All buffers are sized to the padded token ceiling MPAD (grow-only).
#[derive(Default)]
struct Q4kNativeScratch {
    /// MPAD-padded f16 activation [MPAD, cols] (the real rows in the head, the
    /// tail allocated but never stored back).
    xpad: Option<DevBuffer>,
    /// int8 q8_1 codes [MPAD, cols].
    xq: Option<DevBuffer>,
    /// Block-major per-32 activation scale d [cols/32, MPAD].
    da: Option<DevBuffer>,
    /// Block-major per-32 activation sum d·Σcodes [cols/32, MPAD].
    sa: Option<DevBuffer>,
    /// Current f16/int8 element capacity of `xpad`/`xq` (MPAD·cols).
    cap_x: usize,
    /// Current f32 element capacity of `da`/`sa` ((cols/32)·MPAD).
    cap_blocks: usize,
}

/// Grow-only scratch for the small-batch dp4a GEMV (`gemm_qk_dp4a_batch_at`):
/// q8_1 codes + block-major scales/sums, always allocated for the full T=16
/// batch ceiling so buffer addresses stay stable once the decode graphs are
/// captured (no events — all users share the model stream's ordering).
#[derive(Default)]
pub(crate) struct QkBatchScratch {
    /// int8 q8_1 codes [16, cols].
    xq: Option<DevBuffer>,
    /// Block-major per-32 activation scale d [cols/32, 16].
    xd: Option<DevBuffer>,
    /// Block-major per-32 activation sum d·Σcodes [cols/32, 16].
    xsm: Option<DevBuffer>,
    /// Current int8 code capacity (16·cols).
    cap_codes: usize,
    /// Current f32 element capacity of `xd`/`xsm` ((cols/32)·16).
    cap_blocks: usize,
}

/// Grow-only parts scratch for the two-pass batched top-k sampler
/// ([n_seqs × SAMPLE_SCRATCH_PAIRS] value/id pairs). Sampling runs outside the
/// decode graphs, so lazy growth is capture-safe.
#[derive(Default)]
struct SamplePartsScratch {
    vals: Option<DevBuffer>,
    idx: Option<DevBuffer>,
    cap: usize,
}

/// Device-resident ggml codebook tables (LE bytes of the u64/u32 grids).
struct IqTables {
    iq2xs_grid: DevBuffer,
    iq2s_grid: DevBuffer,
    iq3s_grid: DevBuffer,
    iq2xxs_grid: DevBuffer,
    iq3xxs_grid: DevBuffer,
    iq1s_grid: DevBuffer,
    ksigns: DevBuffer,
}

impl IqTables {
    fn upload(device: &dyn Device) -> Result<Self> {
        use forge_formats::iq_tables::{
            IQ1S_GRID, IQ2S_GRID, IQ2XS_GRID, IQ2XXS_GRID, IQ3S_GRID, IQ3XXS_GRID, KSIGNS_IQ2XS,
        };
        let up = |bytes: &[u8]| -> Result<DevBuffer> {
            let buf = device.alloc(
                bytes.len(),
                forge_types::MemKind::Device,
                forge_hal::Pool::Weights,
            )?;
            device.write(bytes, &buf, 0)?;
            Ok(buf)
        };
        let u64s = |t: &[u64]| -> Vec<u8> { t.iter().flat_map(|v| v.to_le_bytes()).collect() };
        let u32s = |t: &[u32]| -> Vec<u8> { t.iter().flat_map(|v| v.to_le_bytes()).collect() };
        Ok(Self {
            iq2xs_grid: up(&u64s(&IQ2XS_GRID))?,
            iq2s_grid: up(&u64s(&IQ2S_GRID))?,
            iq3s_grid: up(&u32s(&IQ3S_GRID))?,
            iq2xxs_grid: up(&u64s(&IQ2XXS_GRID))?,
            iq3xxs_grid: up(&u32s(&IQ3XXS_GRID))?,
            iq1s_grid: up(&u64s(&IQ1S_GRID))?,
            ksigns: up(&KSIGNS_IQ2XS)?,
        })
    }
}


mod attention;
mod compressor;
mod deltanet;
mod elementwise;
mod gemm;
pub mod moe;
mod mtp;
mod norm;
mod quant;
mod sample;

impl Kernels {
    /// Czy dany artefakt jest załadowany. Silnik pyta o to, gdy musi wybrać
    /// wariant ścieżki zależny od rodziny kart.
    pub fn has_artifact(&self, name: &str) -> bool {
        self.artifacts.has(name)
    }

    pub fn load(device: Arc<dyn Device>) -> Result<Self> {
        let artifacts = KernelArtifacts::load(device.as_ref())?;
        let iq_tables = IqTables::upload(device.as_ref())?;
        let cuda_attn_available =
            artifacts.has("attn_prefill_fa_f16_hd64") && artifacts.has("attn_prefill_fa_f16_hd128");
        // Flash-attention w Mojo stoi na `mma`, więc na architekturze bez
        // jednostki macierzowej nie ma go w katalogu. Domyślny wybór pyta o
        // artefakt, a nie o producenta — brak kernela oznacza ścieżkę skalarną,
        // która jest przenośna i bitowo referencyjna.
        let mojo_attn_available = artifacts.has("attn_prefill_fa_mojo_f16_hd64")
            && artifacts.has("attn_prefill_fa_mojo_f16_hd128");
        Ok(Self {
            device,
            artifacts,
            iq_tables,
            prequant: Mutex::new(PrequantScratch::default()),
            prepared_q8: Mutex::new(PrequantScratch::default()),
            w4a8_act: Mutex::new(W4A8ActScratch::default()),
            fp8_act: Mutex::new(Fp8ActScratch::default()),
            q4k_native: Mutex::new(Q4kNativeScratch::default()),
            qk_batch: Mutex::new(QkBatchScratch::default()),
            sample_parts: Mutex::new(SamplePartsScratch::default()),
            attn: match std::env::var("FORGE_ATTN").ok().as_deref() {
                Some("scalar") => AttnBackend::Scalar,
                Some("fa") | Some("cuda") if cuda_attn_available => AttnBackend::Cuda,
                _ if mojo_attn_available => AttnBackend::Mojo,
                _ => AttnBackend::Scalar,
            },
        })
    }

    pub fn artifacts(&self) -> &KernelArtifacts {
        &self.artifacts
    }

}


#[cfg(test)]
mod nvfp4_gguf_dispatch_tests {
    use super::{
        attn_decode_plan, delta_state_layout_dispatch, dense_prefill_artifacts_capable,
        dense_prefill_backend_capable, ensure_prepared_q8_usable, fp8_modular_bn256_capable,
        has_delta_value_key_artifacts, has_hybrid_prefill_b2_artifacts,
        has_hybrid_prefill_t128_artifacts, has_nvfp4_gguf_tile_artifacts,
        hybrid_prefill_nvfp4_artifact_chunk_limit, lock_prepared_q8_scratch,
        nvfp4_ct_s0_manual_capable, nvfp4_ct_split_pipeline_supported,
        nvfp4_gguf_dispatch as nvfp4_gguf_dispatch_impl, nvfp4_gguf_layout_dispatch,
        q8_nvfp4_pack_launch, raw_nvfp4_dp4a_supported, resolve_prepared_q8_marker,
        validate_attn_decode_f16, validate_attn_prefill_fa_f16_hd256, validate_attn_verify_split8,
        validate_deltanet_gated_step_f16, validate_f32_byte_offset,
        validate_kv_append_batch_segmented_f16, validate_kv_append_batch_segmented_masked_f16,
        validate_nvfp4_ct_b1_extents, validate_nvfp4_ct_repack_extents, verify_attn_split8_enabled,
        AttnDecodePlan, DeltaStateLayout, DensePrefillLogitsKind, Kernels, Nvfp4GgufLayout,
        PrequantScratch, Q8PreparedProjection, HYBRID_PREFILL_B2_ARTIFACTS,
        HYBRID_PREFILL_T128_MATRIX_AMD, HYBRID_PREFILL_T128_MATRIX_NVIDIA,
        HYBRID_PREFILL_T128_SHARED, NVFP4_CT_S0_ARTIFACTS, PREPARED_Q8_GEMM_LAUNCHES,
        PREPARED_Q8_RECORD_FAILURES, PREPARED_Q8_SYNC_FAILURES,
    };
    use forge_hal::cpu::CpuDevice;
    use forge_hal::cuda::{CudaDevice, PoolSizes};
    use forge_hal::{Device, Pool};
    use forge_types::{ForgeError, MemKind, Vendor};
    use half::f16;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    /// Urzadzenie testowe albo `None`, gdy maszyna nie ma sterownika CUDA.
    ///
    /// `catch_unwind`, nie sam wariant `Err`: bez biblioteki sterownika — czyli
    /// na kazdym Macu — cudarc panikuje przy jej leniwym ladowaniu, wiec samo
    /// dopasowanie `Err` nigdy nie dochodzilo do skutku i caly zestaw testow
    /// byl czerwony zamiast cichy.
    fn test_device(activations: usize) -> Option<std::sync::Arc<CudaDevice>> {
        let pools = PoolSizes {
            weights: 16 << 20,
            kv_cache: 4 << 20,
            activations,
            kv_page_size: 256 << 10,
        };
        match std::panic::catch_unwind(|| CudaDevice::new(0, pools)) {
            Ok(Ok(device)) => Some(device),
            Ok(Err(error)) => {
                eprintln!("pominieto: brak uzytecznego urzadzenia CUDA ({error})");
                None
            }
            Err(_) => {
                eprintln!("pominieto: brak sterownika CUDA na tej maszynie");
                None
            }
        }
    }
    #[test]
    fn fp8_bn256_wymaga_pelnego_m1024_i_zasobow() {
        assert!(fp8_modular_bn256_capable(
            Vendor::Nvidia,
            32,
            1024,
            100 * 1024,
            11264,
            4096,
            1024,
            |_| true,
        ));
        for tokens in [128, 256, 512, 640, 768, 896] {
            assert!(!fp8_modular_bn256_capable(
                Vendor::Nvidia,
                32,
                1024,
                100 * 1024,
                11264,
                4096,
                tokens,
                |_| true,
            ));
        }
        assert!(!fp8_modular_bn256_capable(
            Vendor::Amd,
            64,
            1024,
            128 * 1024,
            11264,
            4096,
            1024,
            |_| true,
        ));
        assert!(!fp8_modular_bn256_capable(
            Vendor::Nvidia,
            32,
            1024,
            64 * 1024,
            11264,
            4096,
            1024,
            |_| true,
        ));
        // Kształt spoza tablicy BN=256 (projekcja K/V) nie ma wariantu i musi
        // zostać odrzucony. `down` (4096, 11264) świadomie NIE jest już takim
        // kształtem — to jemu BN=256 daje najwięcej.
        assert!(!fp8_modular_bn256_capable(
            Vendor::Nvidia,
            32,
            1024,
            100 * 1024,
            1024,
            4096,
            1024,
            |_| true,
        ));
        assert!(fp8_modular_bn256_capable(
            Vendor::Nvidia,
            32,
            1024,
            100 * 1024,
            4096,
            11264,
            1024,
            |_| true,
        ));
        assert!(!fp8_modular_bn256_capable(
            Vendor::Nvidia,
            32,
            1024,
            100 * 1024,
            4096,
            4096,
            1024,
            |_| false,
        ));
    }

    #[test]
    fn split8_verifiera_ma_jawny_kill_switch() {
        assert!(verify_attn_split8_enabled(None));
        assert!(verify_attn_split8_enabled(Some("auto")));
        assert!(verify_attn_split8_enabled(Some("1")));
        assert!(!verify_attn_split8_enabled(Some("0")));
        assert!(!verify_attn_split8_enabled(Some("false")));
        assert!(!verify_attn_split8_enabled(Some("")));
    }

    #[test]
    fn nvfp4_ct_wymaga_sm80_warp32_i_pelnego_zestawu() {
        assert!(nvfp4_ct_s0_manual_capable(
            Vendor::Nvidia,
            "sm_80",
            32,
            256,
            |_| true,
        ));
        for missing in NVFP4_CT_S0_ARTIFACTS {
            assert!(!nvfp4_ct_s0_manual_capable(
                Vendor::Nvidia,
                "sm_89",
                32,
                256,
                |name| name != missing,
            ));
        }
        assert!(!nvfp4_ct_s0_manual_capable(
            Vendor::Nvidia,
            "sm_75",
            32,
            1024,
            |_| true,
        ));
        assert!(!nvfp4_ct_s0_manual_capable(
            Vendor::Amd,
            "gfx942",
            64,
            1024,
            |_| true,
        ));
        assert!(!nvfp4_ct_s0_manual_capable(
            Vendor::Nvidia,
            "sm_90",
            64,
            1024,
            |_| true,
        ));
    }

    #[test]
    fn nvfp4_ct_odrzuca_split_k_krotszy_od_potokowania() {
        for split_stages in 0..4 {
            assert!(!nvfp4_ct_split_pipeline_supported(split_stages, 1, 4));
        }
        assert!(!nvfp4_ct_split_pipeline_supported(13, 4, 3));
        assert!(nvfp4_ct_split_pipeline_supported(8, 1, 4));
        assert!(nvfp4_ct_split_pipeline_supported(32, 3, 4));
        assert!(nvfp4_ct_split_pipeline_supported(32, 4, 4));
        assert!(nvfp4_ct_split_pipeline_supported(32, 1, 3));
        assert!(nvfp4_ct_split_pipeline_supported(88, 4, 4));
        assert!(!nvfp4_ct_split_pipeline_supported(32, 9, 4));
    }

    #[test]
    fn nvfp4_ct_repack_waliduje_extenty_i_overflow() {
        assert_eq!(
            validate_nvfp4_ct_repack_extents(9216, 4096, 512, 128, 128, 64, 64).unwrap(),
            1
        );
        assert!(validate_nvfp4_ct_repack_extents(9216 + 256, 4096, 512, 128, 128, 64, 64).is_err());
        assert!(validate_nvfp4_ct_repack_extents(9216, 4095, 512, 128, 128, 64, 64).is_err());
        assert!(validate_nvfp4_ct_repack_extents(
            9216,
            4096,
            512,
            usize::MAX - 63,
            128,
            64,
            usize::MAX - 63,
        )
        .is_err());
    }

    #[test]
    fn nvfp4_ct_b1_waliduje_okno_i_overflow() {
        assert_eq!(
            validate_nvfp4_ct_b1_extents(128 + 256, 256, 128, 128, 64, 64, 1.0).unwrap(),
            8
        );
        assert!(validate_nvfp4_ct_b1_extents(127, 256, 128, 128, 64, 64, 1.0).is_err());
        assert!(
            validate_nvfp4_ct_b1_extents(128, 256, usize::MAX, 128, usize::MAX - 63, 64, 1.0,)
                .is_err()
        );
        assert!(validate_nvfp4_ct_b1_extents(128, 256, 128, 128, 64, 64, f32::NAN).is_err());
    }

    #[test]
    fn split8_verifiera_waliduje_wszystkie_extenty_i_overflow() {
        let valid = [
            3 * 24 * 256 * 2,
            3 * 24 * 8 * 260 * 4,
            3 * 24 * 256 * 2,
            136 * 4 * 32 * 256 * 2,
            136 * 4 * 32 * 256 * 2,
            136 * 4,
            3 * 4,
        ];
        let check = |sizes: [usize; 7], tokens, heads, kv_heads, page, pages, scale| {
            validate_attn_verify_split8(
                sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6], tokens,
                heads, kv_heads, page, pages, scale,
            )
        };
        assert_eq!(check(valid, 3, 24, 4, 32, 136, 0.0625).unwrap(), (24, 72));
        for index in 0..valid.len() {
            let mut undersized = valid;
            undersized[index] -= 1;
            assert!(check(undersized, 3, 24, 4, 32, 136, 0.0625).is_err());
        }
        assert!(check(valid, 2, 24, 4, 32, 136, 0.0625).is_err());
        assert!(check(valid, 3, 0, 4, 32, 136, 0.0625).is_err());
        assert!(check(valid, 3, 24, 5, 32, 136, 0.0625).is_err());
        assert!(check(valid, 3, 24, 4, 0, 136, 0.0625).is_err());
        assert!(check(valid, 3, 24, 4, 32, 0, 0.0625).is_err());
        assert!(check(valid, 3, 24, 4, 32, 136, f32::NAN).is_err());
        assert!(check([usize::MAX; 7], 4, usize::MAX, 1, 1, 1, 1.0).is_err());
    }

    fn nvfp4_gguf_dispatch(
        n_tokens: usize,
        n_rows: usize,
        prefetch_available: bool,
        is_nvidia: bool,
        warp_size: u32,
        max_threads: u32,
    ) -> forge_types::Result<super::Nvfp4GgufDispatch> {
        nvfp4_gguf_dispatch_impl(
            n_tokens,
            n_rows,
            5120,
            prefetch_available,
            false,
            false,
            is_nvidia,
            false,
            false,
            false,
            warp_size,
            max_threads,
        )
    }

    #[test]
    fn waliduje_kontrakt_flash_attention_prefill_hd256() {
        let query = 64 * 8 * 256 * 2;
        let cache = 2 * 256 * 256 * 2;
        assert_eq!(
            validate_attn_prefill_fa_f16_hd256(
                query, query, cache, cache, 4, 0, 64, 8, 2, 256, 0.0625,
            )
            .unwrap(),
            (1, 8)
        );
        assert!(validate_attn_prefill_fa_f16_hd256(
            query, query, cache, cache, 4, 0, 64, 7, 2, 256, 0.0625,
        )
        .is_err());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query - 2,
            query,
            cache,
            cache,
            4,
            0,
            64,
            8,
            2,
            256,
            0.0625,
        )
        .is_err());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query,
            query,
            cache,
            cache,
            4,
            0,
            64,
            8,
            2,
            256,
            f32::NAN,
        )
        .is_err());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query,
            query,
            cache,
            cache,
            4,
            usize::MAX,
            64,
            8,
            2,
            256,
            0.0625,
        )
        .is_err());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query, query, cache, cache, 4, 256, 64, 8, 2, 256, 0.0625,
        )
        .is_err());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query, query, cache, cache, 8, 0, 64, 8, 2, 256, 0.0625,
        )
        .is_ok());
        assert!(validate_attn_prefill_fa_f16_hd256(
            query,
            query,
            cache / 2,
            cache / 2,
            8,
            256,
            64,
            8,
            2,
            256,
            0.0625,
        )
        .is_err());
    }

    /// Wariant dzielony obowiązuje na KAŻDEJ karcie z falą 32 — przy jednej
    /// sekwencji tylko on wysyca pamięć (siatka generycznego to
    /// `sekwencje * głowice`, czyli 16 grup roboczych). Odpada przy fali 64,
    /// za małym bloku i braku artefaktów.
    #[test]
    fn wariant_dzielony_wymaga_fali_32_i_kompletu_artefaktow() {
        for (head_dim, expected) in [
            (64usize, AttnDecodePlan::Split8Hd64),
            (128, AttnDecodePlan::Split8Hd128),
            (256, AttnDecodePlan::Split8Hd256),
            (512, AttnDecodePlan::Split8Hd512),
        ] {
            assert_eq!(
                attn_decode_plan(head_dim, 32, 1024, true).unwrap(),
                expected
            );
        }
        for (head_dim, generic) in [
            (64usize, "attn_decode_f16_hd64"),
            (128, "attn_decode_f16_hd128"),
            (256, "attn_decode_f16_hd256"),
            (512, "attn_decode_f16_hd512"),
        ] {
            for plan in [
                attn_decode_plan(head_dim, 64, 1024, true).unwrap(),
                attn_decode_plan(head_dim, 32, 128, true).unwrap(),
                attn_decode_plan(head_dim, 32, 1024, false).unwrap(),
            ] {
                assert_eq!(plan, AttnDecodePlan::Generic(generic));
            }
        }
    }

    #[test]
    fn waliduje_pelny_kontrakt_attention_decode() {
        let vectors = 2 * 4 * 256 * 2;
        let cache = 2 * 32 * 256 * 2;
        let parts = 2 * 4 * 8 * 260 * 4;
        assert_eq!(
            validate_attn_decode_f16(
                vectors,
                parts,
                vectors,
                cache,
                cache,
                2 * 8 * 4,
                2 * 4,
                2,
                4,
                2,
                256,
                32,
                8,
                0.0625,
                true,
            )
            .unwrap(),
            (2, 4)
        );
        assert!(validate_attn_decode_f16(
            vectors,
            parts - 4,
            vectors,
            cache,
            cache,
            2 * 8 * 4,
            2 * 4,
            2,
            4,
            2,
            256,
            32,
            8,
            0.0625,
            true,
        )
        .is_err());
        assert!(validate_attn_decode_f16(
            vectors,
            parts,
            vectors,
            cache,
            cache,
            2 * 8 * 4,
            2 * 4,
            2,
            3,
            2,
            256,
            32,
            8,
            0.0625,
            true,
        )
        .is_err());
        assert!(validate_attn_decode_f16(
            vectors,
            parts,
            vectors,
            cache,
            cache,
            2 * 8 * 4,
            2 * 4,
            0,
            4,
            2,
            256,
            32,
            8,
            0.0625,
            true,
        )
        .is_err());
    }

    #[test]
    fn waliduje_pelny_kontrakt_kroku_deltanet() {
        let vectors = 48 * 128 * 2;
        let state = 48 * 128 * 128 * 4;
        let gates = 48 * 4;
        assert_eq!(
            validate_deltanet_gated_step_f16(
                vectors, state, vectors, vectors, vectors, gates, gates, 48, 128, 1024,
            )
            .unwrap(),
            (48, 128)
        );
        for invalid in [
            validate_deltanet_gated_step_f16(
                vectors, state, vectors, vectors, vectors, gates, gates, 0, 128, 1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors, state, vectors, vectors, vectors, gates, gates, 48, 0, 1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors, state, vectors, vectors, vectors, gates, gates, 48, 128, 64,
            ),
            validate_deltanet_gated_step_f16(
                vectors, state, vectors, vectors, vectors, gates, gates, 48, 1025, 2048,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors,
                vectors,
                vectors,
                gates,
                gates,
                u32::MAX as usize + 1,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors - 1,
                state,
                vectors,
                vectors,
                vectors,
                gates,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state - 1,
                vectors,
                vectors,
                vectors,
                gates,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors - 1,
                vectors,
                vectors,
                gates,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors,
                vectors - 1,
                vectors,
                gates,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors,
                vectors,
                vectors - 1,
                gates,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors,
                vectors,
                vectors,
                gates - 1,
                gates,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                vectors,
                state,
                vectors,
                vectors,
                vectors,
                gates,
                gates - 1,
                48,
                128,
                1024,
            ),
            validate_deltanet_gated_step_f16(
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                usize::MAX,
                128,
                1024,
            ),
        ] {
            assert!(invalid.is_err());
        }
    }

    #[test]
    fn prefill_b2_wymaga_pelnego_zestawu_artefaktow() {
        assert!(has_hybrid_prefill_b2_artifacts(|_| true));
        for missing in HYBRID_PREFILL_B2_ARTIFACTS {
            assert!(!has_hybrid_prefill_b2_artifacts(|name| name != missing));
        }
    }

    /// Obie rodziny mają PEŁNY, ale RÓŻNY zestaw kafli macierzowych: NVIDIA na
    /// `mma`/`ldmatrix`, AMD na WMMA. Test pilnuje jednego i drugiego, a przy
    /// okazji tego, że zestawy się nie mieszają — kernel NVIDII nie może
    /// wystarczyć AMD ani odwrotnie.
    #[test]
    fn prefill_t128_wymaga_pelnego_zestawu_artefaktow_swojej_rodziny() {
        for nvidia in [true, false] {
            assert!(
                has_hybrid_prefill_t128_artifacts(nvidia, |_| true),
                "{nvidia}"
            );
            let matrix: &[&str] = if nvidia {
                &HYBRID_PREFILL_T128_MATRIX_NVIDIA
            } else {
                &HYBRID_PREFILL_T128_MATRIX_AMD
            };
            let triplet = if nvidia {
                "gemm_q8_0_i8mma_triplet_bm64"
            } else {
                "gemm_q8_0_wmma_triplet_bm64"
            };
            for missing in HYBRID_PREFILL_T128_SHARED
                .iter()
                .chain(matrix.iter())
                .chain(std::iter::once(&triplet))
            {
                assert!(
                    !has_hybrid_prefill_t128_artifacts(nvidia, |name| name != *missing),
                    "{nvidia} {missing}"
                );
            }
        }
        // Sam zestaw NVIDII nie wystarcza AMD i odwrotnie.
        let nvidia_only = |name: &str| {
            HYBRID_PREFILL_T128_SHARED.contains(&name)
                || HYBRID_PREFILL_T128_MATRIX_NVIDIA.contains(&name)
                || name == "gemm_q8_0_i8mma_triplet_bm64"
        };
        assert!(has_hybrid_prefill_t128_artifacts(true, nvidia_only));
        assert!(!has_hybrid_prefill_t128_artifacts(false, nvidia_only));
    }

    #[test]
    fn dense_prefill_wymaga_artefaktow_konkretnego_hd_formatu_i_batcha() {
        assert_eq!(
            Kernels::f16_out_f32_dispatch(32_000, 4, |_| true).0,
            "gemv_batch_f16_out_f32_b4"
        );
        assert_eq!(
            Kernels::f16_out_f32_dispatch(32_000, 16, |_| true).0,
            "gemm_f16_out_f32_bm32"
        );
        assert_eq!(
            Kernels::f16_out_f32_dispatch(32_000, 16, |_| false).0,
            "gemm_f16_out_f32"
        );
        // Fala 32 i blok co najmniej 256 — u obu producentow.
        assert!(dense_prefill_backend_capable(32, 1024));
        // Fala 64 (CDNA, stare GCN) nie spelnia kontraktu kerneli.
        assert!(!dense_prefill_backend_capable(64, 1024));
        assert!(!dense_prefill_backend_capable(32, 128));
        // HD128 ma dwa rownowazne kernele uwagi, wiec brak jednego z nich NIE
        // wylacza dense prefillu — dopiero brak obu.
        for one_missing in [
            "attn_prefill_fa_segmented_f16_hd128",
            "attn_prefill_segmented_f16_hd128",
        ] {
            assert!(dense_prefill_artifacts_capable(
                128,
                16,
                DensePrefillLogitsKind::NvFp4Gguf {
                    rows: 32_000,
                    cols: 4096,
                },
                |name| name != one_missing,
            ));
        }
        assert!(!dense_prefill_artifacts_capable(
            128,
            16,
            DensePrefillLogitsKind::NvFp4Gguf {
                rows: 32_000,
                cols: 4096,
            },
            |name| !name.starts_with("attn_prefill"),
        ));
        for (head_dim, attention) in [
            (128, "kv_append_batch_segmented_f16"),
            (256, "attn_prefill_segmented_f16_hd256"),
        ] {
            assert!(dense_prefill_artifacts_capable(
                head_dim,
                16,
                DensePrefillLogitsKind::NvFp4Gguf {
                    rows: 32_000,
                    cols: 4096,
                },
                |_| true,
            ));
            for missing in [
                "kv_append_batch_segmented_f16",
                attention,
                "argmax_batched_f32",
                "topk_batched_partial_f32",
                "topk_batched_final_f32",
                "penalize_batched_f32",
                "gemm_nvfp4_gguf_out_f32_b16",
            ] {
                assert!(
                    !dense_prefill_artifacts_capable(
                        head_dim,
                        16,
                        DensePrefillLogitsKind::NvFp4Gguf {
                            rows: 32_000,
                            cols: 4096,
                        },
                        |name| name != missing,
                    ),
                    "brak {missing} powinien wyłączyć dense prefill"
                );
            }
        }
        assert!(dense_prefill_artifacts_capable(
            128,
            4,
            DensePrefillLogitsKind::Q8_0 {
                rows: 32_000,
                cols: 4096,
            },
            |_| true,
        ));
        assert!(!dense_prefill_artifacts_capable(
            128,
            4,
            DensePrefillLogitsKind::Q8_0 {
                rows: 32_000,
                cols: 4096,
            },
            |name| name != "gemm_q8_0_f16_exact_out_f32_b4",
        ));
        for (rows, required) in [
            (8191, "gemm_q8_0_out_f32_bm64"),
            (8192, "gemm_q8_0_out_f32"),
        ] {
            assert!(dense_prefill_artifacts_capable(
                128,
                16,
                DensePrefillLogitsKind::Q8_0 { rows, cols: 4096 },
                |_| true,
            ));
            // Brak kernela NVIDII zostawia jeszcze kafle AMD, wiec zdolnosc
            // znika dopiero, gdy nie ma zadnej z trzech drog.
            assert!(dense_prefill_artifacts_capable(
                128,
                16,
                DensePrefillLogitsKind::Q8_0 { rows, cols: 4096 },
                |name| name != required,
            ));
            assert!(!dense_prefill_artifacts_capable(
                128,
                16,
                DensePrefillLogitsKind::Q8_0 { rows, cols: 4096 },
                |name| !name.starts_with("gemm_q8_0_"),
            ));
        }
        assert!(!dense_prefill_artifacts_capable(
            64,
            4,
            DensePrefillLogitsKind::F16 {
                rows: 32_000,
                cols: 4096,
            },
            |_| true,
        ));
    }

    #[test]
    fn limit_artefaktow_prefill_nvidia_sprawdza_kazdy_poziom_dispatchera() {
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(true, |_| true),
            128
        );
        for missing in [
            "deltanet_gated_scan_inplace_dynamic_d128_f16",
            "gemm_nvfp4_gguf_f16_b2",
            "gemm_nvfp4_gguf_f16_b3_nvidia",
        ] {
            assert_eq!(
                hybrid_prefill_nvfp4_artifact_chunk_limit(true, |name| name != missing),
                0,
                "brak {missing} powinien wyłączyć automatyczny NVFP4 prefill"
            );
        }
        for (missing, expected) in [
            ("gemm_nvfp4_gguf_f16_b4_nvidia", 3),
            ("gemm_nvfp4_gguf_f16_b8_nvidia", 4),
            ("gemm_nvfp4_gguf_f16_b16_nvidia", 8),
            ("gemm_nvfp4_gguf_mma_f16_bm32", 16),
            ("gemm_q8_0_i8mma_triplet_bm64", 16),
            ("deltanet_gated_scan_inplace_shared_d128_f16", 32),
            ("gemm_nvfp4_gguf_mma_f16_bm128", 32),
            ("gemm_nvfp4_gguf_mma_f16_bm128_bn32", 32),
        ] {
            assert_eq!(
                hybrid_prefill_nvfp4_artifact_chunk_limit(true, |name| name != missing),
                expected,
                "brak {missing} powinien ograniczyć chunk do T{expected}"
            );
        }
    }

    #[test]
    fn limit_artefaktow_prefill_przenosny_uzywa_wariantow_generic() {
        // Z kompletem kafli WMMA backend przenośny sięga tego samego T128 co
        // NVIDIA — wcześniej był tu twardy sufit 16, bo kafle macierzowe
        // istniały wyłącznie w wariancie `mma`.
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(false, |_| true),
            128
        );
        for missing in [
            "deltanet_gated_scan_inplace_dynamic_d128_f16",
            "gemm_nvfp4_gguf_f16_b2",
            "gemm_nvfp4_gguf_f16_b3",
        ] {
            assert_eq!(
                hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| name != missing),
                0
            );
        }
        for (missing, expected) in [
            ("gemm_nvfp4_gguf_f16_b4", 3),
            ("gemm_nvfp4_gguf_f16_b8", 4),
            ("gemm_nvfp4_gguf_f16_b16", 8),
        ] {
            assert_eq!(
                hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| name != missing),
                expected
            );
        }
        // Wariant `_nvidia` nie jest dla tego backendu — jego brak niczego nie
        // zmienia.
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| {
                name != "gemm_nvfp4_gguf_f16_b3_nvidia"
            }),
            128
        );
        // Brak kafli WMMA cofa backend przenośny dokładnie tak, jak brak kafli
        // `mma` cofa NVIDIĘ.
        for (missing, expected) in [
            ("gemm_nvfp4_gguf_wmma_f16_bm32", 16),
            ("gemm_q8_0_wmma_triplet_bm64", 16),
            ("gemm_nvfp4_gguf_wmma_f16_bm256", 32),
        ] {
            assert_eq!(
                hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| name != missing),
                expected,
                "{missing}"
            );
        }
        // Kafle `mma` NVIDII nie zastępują WMMA.
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| !name.contains("wmma")),
            16
        );
    }

    #[test]
    fn maskowany_append_kv_odrzuca_wymiary_i_bufory_przed_launch() {
        let valid = [256usize, 256, 128, 128, 16, 8, 16, 2, 2, 2, 2, 2, 8];
        let validate = |values: [usize; 13]| {
            validate_kv_append_batch_segmented_masked_f16(
                values[0], values[1], values[2], values[3], values[4], values[5], values[6],
                values[7], values[8], values[9], values[10], values[11], values[12],
            )
        };
        assert_eq!(validate(valid).unwrap(), (4, 2, 32));
        assert_eq!(
            validate_kv_append_batch_segmented_f16(
                64, 64, 128, 128, 2048, 8, 2, 2, 256, 2, 2, 8, 256,
            )
            .unwrap(),
            (4, 2, 32)
        );

        for dimension in 7..13 {
            let mut values = valid;
            values[dimension] = 0;
            assert!(
                validate(values).is_err(),
                "wymiar {dimension} powinien być odrzucony"
            );
        }
        for buffer in 0..7 {
            let mut values = valid;
            values[buffer] = values[buffer].saturating_sub(1);
            assert!(
                validate(values).is_err(),
                "bufor {buffer} powinien być odrzucony"
            );
        }
        let mut different_cache = valid;
        different_cache[1] += 256;
        assert!(validate(different_cache).is_err());
        let mut misaligned_cache = valid;
        misaligned_cache[0] += 2;
        misaligned_cache[1] += 2;
        assert!(validate(misaligned_cache).is_err());
        let mut overflow = valid;
        overflow[7] = usize::MAX;
        assert!(validate(overflow).is_err());
    }

    /// Scratch `prepare_q8_1` jest współdzielony i grow-only, więc testy, które
    /// przez niego przechodzą, nie mogą lecieć równolegle — bez tej serializacji
    /// nadpisywały sobie skwantyzowane aktywacje i wywracały kontrolę canary
    /// (reprodukowalne: równolegle 40/42, z `--test-threads=1` 42/42).
    static PREPARED_Q8_SCRATCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn segmentowany_append_kv_odrzuca_extenty_grid_i_limit_urzadzenia() {
        let valid = [256usize, 256, 128, 128, 16, 8, 2, 2, 2, 2, 2, 8, 256];
        let validate = |values: [usize; 13]| {
            validate_kv_append_batch_segmented_f16(
                values[0],
                values[1],
                values[2],
                values[3],
                values[4],
                values[5],
                values[6],
                values[7],
                values[8],
                values[9],
                values[10],
                values[11],
                u32::try_from(values[12]).unwrap_or(0),
            )
        };
        assert_eq!(validate(valid).unwrap(), (4, 2, 32));

        for dimension in 6..12 {
            let mut values = valid;
            values[dimension] = 0;
            assert!(
                validate(values).is_err(),
                "wymiar {dimension} powinien być odrzucony"
            );
        }
        for buffer in 0..6 {
            let mut values = valid;
            values[buffer] = values[buffer].saturating_sub(1);
            assert!(
                validate(values).is_err(),
                "bufor {buffer} powinien być odrzucony"
            );
        }
        let mut different_cache = valid;
        different_cache[1] += 256;
        assert!(validate(different_cache).is_err());
        let mut misaligned_cache = valid;
        misaligned_cache[0] += 2;
        misaligned_cache[1] += 2;
        assert!(validate(misaligned_cache).is_err());
        let mut grid_overflow = valid;
        grid_overflow[6] = usize::MAX;
        assert!(validate(grid_overflow).is_err());
        let mut block_too_large = valid;
        block_too_large[11] = 128;
        block_too_large[12] = 64;
        assert!(validate(block_too_large).is_err());
    }

    #[test]
    fn prepared_q8_t32_t128_jest_zgodny_z_baseline_i_chroni_canary() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `catch_unwind`, nie sam wariant `Err`: na maszynie bez biblioteki
        // sterownika CUDA — czyli na kazdym Macu — cudarc panikuje przy jej
        // leniwym ladowaniu, wiec ponizsze pomijanie nigdy nie dochodzilo do
        // skutku i caly zestaw testow byl czerwony zamiast cichy.
        let Some(device) = test_device(64 << 20) else { return };
        let kernels = Kernels::load(device.clone()).unwrap();
        let stream = device.create_stream().unwrap();
        let rows = 65usize;
        let cols = 64usize;
        let weight_offset = 68usize;
        let mut host_weights = vec![0xa5; weight_offset];
        for block_index in 0..rows * (cols / 32) {
            host_weights.extend(f16::from_f32(0.0078125).to_bits().to_le_bytes());
            host_weights.extend((0..32).map(|byte| ((block_index * 17 + byte * 13) & 0xff) as u8));
        }
        let weights = device
            .alloc(host_weights.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        device.write(&host_weights, &weights, 0).unwrap();

        for tokens in [32usize, 128] {
            let host_x = (0..tokens * cols)
                .map(|index| f16::from_f32((index as f32 % 29.0 - 14.0) / 8.0))
                .collect::<Vec<_>>();
            let x = device
                .alloc(host_x.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&host_x), &x, 0)
                .unwrap();
            let output_bytes = tokens * rows * 2;
            let canary_bytes = 64usize;
            let baseline = device
                .alloc(
                    output_bytes + canary_bytes,
                    MemKind::Device,
                    Pool::Activations,
                )
                .unwrap();
            let prepared_output = device
                .alloc(
                    output_bytes + canary_bytes,
                    MemKind::Device,
                    Pool::Activations,
                )
                .unwrap();
            let initialized = vec![0xa5; output_bytes + canary_bytes];
            device.write(&initialized, &baseline, 0).unwrap();
            device.write(&initialized, &prepared_output, 0).unwrap();
            kernels
                .gemm_q8_0_i8mma_at(
                    &baseline,
                    &weights,
                    weight_offset,
                    &x,
                    rows,
                    cols,
                    tokens,
                    &stream,
                )
                .unwrap();
            let mut prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
            kernels
                .gemm_q8_0_i8mma_prepared_at(
                    &prepared_output,
                    &weights,
                    weight_offset,
                    &mut prepared,
                    rows,
                    cols,
                    tokens,
                )
                .unwrap();
            drop(prepared);
            stream.synchronize().unwrap();
            let mut baseline_bytes = vec![0u8; output_bytes + canary_bytes];
            let mut prepared_bytes = vec![0u8; output_bytes + canary_bytes];
            device.read(&baseline, 0, &mut baseline_bytes).unwrap();
            device
                .read(&prepared_output, 0, &mut prepared_bytes)
                .unwrap();
            assert_eq!(
                prepared_bytes[..output_bytes],
                baseline_bytes[..output_bytes]
            );
            assert_eq!(prepared_bytes[output_bytes..], vec![0xa5; canary_bytes]);
            assert_eq!(baseline_bytes[output_bytes..], vec![0xa5; canary_bytes]);
        }
    }

    #[test]
    fn fused_q8_triplet_t32_t128_jest_bitowo_zgodny_i_chroni_canary() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `catch_unwind`, nie sam wariant `Err`: na maszynie bez biblioteki
        // sterownika CUDA — czyli na kazdym Macu — cudarc panikuje przy jej
        // leniwym ladowaniu, wiec ponizsze pomijanie nigdy nie dochodzilo do
        // skutku i caly zestaw testow byl czerwony zamiast cichy.
        let Some(device) = test_device(64 << 20) else { return };
        let kernels = Kernels::load(device.clone()).unwrap();
        let stream = device.create_stream().unwrap();
        let rows = [65usize, 17, 9];
        let offsets = [68usize, 102, 136];
        let cols = 64usize;
        let canary_bytes = 64usize;
        let mut weights = Vec::new();
        for projection in 0..3 {
            let mut host = vec![0xa5; offsets[projection]];
            for block_index in 0..rows[projection] * (cols / 32) {
                host.extend(
                    f16::from_f32(0.00390625 * (projection + 1) as f32)
                        .to_bits()
                        .to_le_bytes(),
                );
                host.extend(
                    (0..32).map(|byte| {
                        ((block_index * 17 + byte * 13 + projection * 29) & 0xff) as u8
                    }),
                );
            }
            let buffer = device
                .alloc(host.len(), MemKind::Device, Pool::Weights)
                .unwrap();
            device.write(&host, &buffer, 0).unwrap();
            weights.push(buffer);
        }

        for tokens in [32usize, 128] {
            let host_x = (0..tokens * cols)
                .map(|index| f16::from_f32((index as f32 % 31.0 - 15.0) / 8.0))
                .collect::<Vec<_>>();
            let x = device
                .alloc(host_x.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&host_x), &x, 0)
                .unwrap();
            let mut baseline_outputs = Vec::new();
            let mut fused_outputs = Vec::new();
            for projection_rows in rows {
                let output_bytes = tokens * projection_rows * 2;
                let initialized = vec![0xa5; output_bytes + canary_bytes];
                let baseline = device
                    .alloc(initialized.len(), MemKind::Device, Pool::Activations)
                    .unwrap();
                let fused = device
                    .alloc(initialized.len(), MemKind::Device, Pool::Activations)
                    .unwrap();
                device.write(&initialized, &baseline, 0).unwrap();
                device.write(&initialized, &fused, 0).unwrap();
                baseline_outputs.push(baseline);
                fused_outputs.push(fused);
            }
            let mut baseline_prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
            for projection in 0..3 {
                kernels
                    .gemm_q8_0_i8mma_prepared_at(
                        &baseline_outputs[projection],
                        &weights[projection],
                        offsets[projection],
                        &mut baseline_prepared,
                        rows[projection],
                        cols,
                        tokens,
                    )
                    .unwrap();
            }
            drop(baseline_prepared);
            let mut fused_prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
            kernels
                .gemm_q8_0_i8mma_prepared_triplet(
                    &[
                        Q8PreparedProjection {
                            output: &fused_outputs[0],
                            weights: &weights[0],
                            weight_byte_offset: offsets[0],
                            rows: rows[0],
                        },
                        Q8PreparedProjection {
                            output: &fused_outputs[1],
                            weights: &weights[1],
                            weight_byte_offset: offsets[1],
                            rows: rows[1],
                        },
                        Q8PreparedProjection {
                            output: &fused_outputs[2],
                            weights: &weights[2],
                            weight_byte_offset: offsets[2],
                            rows: rows[2],
                        },
                    ],
                    &mut fused_prepared,
                    cols,
                    tokens,
                )
                .unwrap();
            drop(fused_prepared);
            stream.synchronize().unwrap();
            for projection in 0..3 {
                let output_bytes = tokens * rows[projection] * 2;
                let mut baseline = vec![0u8; output_bytes + canary_bytes];
                let mut fused = vec![0u8; output_bytes + canary_bytes];
                device
                    .read(&baseline_outputs[projection], 0, &mut baseline)
                    .unwrap();
                device
                    .read(&fused_outputs[projection], 0, &mut fused)
                    .unwrap();
                assert_eq!(fused[..output_bytes], baseline[..output_bytes]);
                assert_eq!(baseline[output_bytes..], vec![0xa5; canary_bytes]);
                assert_eq!(fused[output_bytes..], vec![0xa5; canary_bytes]);
            }
        }
    }

    #[test]
    fn publiczny_flow_uniewaznia_handle_po_bledzie_record() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(device) = test_device(16 << 20) else { return };
        let kernels = Kernels::load(device.clone()).unwrap();
        let stream = device.create_stream().unwrap();
        let tokens = 6usize;
        let rows = 16usize;
        let cols = 32usize;
        let weight_offset = 68usize;
        let host_x = (0..tokens * cols)
            .map(|index| f16::from_f32((index as f32 % 29.0 - 14.0) / 8.0))
            .collect::<Vec<_>>();
        let x = device
            .alloc(host_x.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice::<f16, u8>(&host_x), &x, 0)
            .unwrap();
        let mut host_weights = vec![0xa5; weight_offset];
        for block_index in 0..rows {
            host_weights.extend(f16::from_f32(0.0078125).to_bits().to_le_bytes());
            host_weights.extend((0..32).map(|byte| ((block_index * 17 + byte * 13) & 0xff) as u8));
        }
        let weights = device
            .alloc(host_weights.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        device.write(&host_weights, &weights, 0).unwrap();
        let baseline = device
            .alloc(tokens * rows * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        kernels
            .gemm_q8_0_i8mma_at(
                &baseline,
                &weights,
                weight_offset,
                &x,
                rows,
                cols,
                tokens,
                &stream,
            )
            .unwrap();
        stream.synchronize().unwrap();
        let mut baseline_bytes = vec![0u8; tokens * rows * 2];
        device.read(&baseline, 0, &mut baseline_bytes).unwrap();

        PREPARED_Q8_RECORD_FAILURES.store(0, Ordering::SeqCst);
        PREPARED_Q8_SYNC_FAILURES.store(0, Ordering::SeqCst);
        PREPARED_Q8_GEMM_LAUNCHES.store(0, Ordering::SeqCst);
        let failed_output = device
            .alloc(tokens * rows * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        let mut prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
        PREPARED_Q8_RECORD_FAILURES.store(1, Ordering::SeqCst);
        assert!(kernels
            .gemm_q8_0_i8mma_prepared_at(
                &failed_output,
                &weights,
                weight_offset,
                &mut prepared,
                rows,
                cols,
                tokens,
            )
            .is_err());
        assert_eq!(PREPARED_Q8_GEMM_LAUNCHES.load(Ordering::SeqCst), 1);
        assert!(kernels
            .gemm_q8_0_i8mma_prepared_at(
                &failed_output,
                &weights,
                weight_offset,
                &mut prepared,
                rows,
                cols,
                tokens,
            )
            .is_err());
        assert_eq!(PREPARED_Q8_GEMM_LAUNCHES.load(Ordering::SeqCst), 1);
        drop(prepared);

        let recovered_output = device
            .alloc(tokens * rows * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        let mut recovered = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
        kernels
            .gemm_q8_0_i8mma_prepared_at(
                &recovered_output,
                &weights,
                weight_offset,
                &mut recovered,
                rows,
                cols,
                tokens,
            )
            .unwrap();
        stream.synchronize().unwrap();
        let mut recovered_bytes = vec![0u8; tokens * rows * 2];
        device
            .read(&recovered_output, 0, &mut recovered_bytes)
            .unwrap();
        assert_eq!(recovered_bytes, baseline_bytes);
        drop(recovered);

        PREPARED_Q8_GEMM_LAUNCHES.store(0, Ordering::SeqCst);
        let poisoned_output = device
            .alloc(tokens * rows * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        let mut poisoned = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
        PREPARED_Q8_RECORD_FAILURES.store(1, Ordering::SeqCst);
        PREPARED_Q8_SYNC_FAILURES.store(1, Ordering::SeqCst);
        assert!(kernels
            .gemm_q8_0_i8mma_prepared_at(
                &poisoned_output,
                &weights,
                weight_offset,
                &mut poisoned,
                rows,
                cols,
                tokens,
            )
            .is_err());
        assert_eq!(PREPARED_Q8_GEMM_LAUNCHES.load(Ordering::SeqCst), 1);
        assert!(kernels
            .gemm_q8_0_i8mma_prepared_at(
                &poisoned_output,
                &weights,
                weight_offset,
                &mut poisoned,
                rows,
                cols,
                tokens,
            )
            .is_err());
        assert_eq!(PREPARED_Q8_GEMM_LAUNCHES.load(Ordering::SeqCst), 1);
        drop(poisoned);
        stream.synchronize().unwrap();
        assert!(kernels.prepare_q8_1(&x, cols, tokens, &stream).is_err());
        PREPARED_Q8_RECORD_FAILURES.store(0, Ordering::SeqCst);
        PREPARED_Q8_SYNC_FAILURES.store(0, Ordering::SeqCst);
        PREPARED_Q8_GEMM_LAUNCHES.store(0, Ordering::SeqCst);
    }

    #[test]
    fn blad_record_z_udanym_sync_resetuje_marker_bez_zatrucia() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut scratch = PrequantScratch {
            ready: Some(device.create_event().unwrap()),
            ..PrequantScratch::default()
        };
        let error = resolve_prepared_q8_marker(
            &mut scratch,
            Err(ForgeError::Kernel("wstrzyknięty błąd record".into())),
            || Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("wstrzyknięty błąd record"));
        assert!(scratch.ready.is_none());
        assert!(!scratch.poisoned);
        assert!(ensure_prepared_q8_usable(&scratch).is_ok());
        assert!(lock_prepared_q8_scratch(&Mutex::new(scratch)).is_ok());
    }

    #[test]
    fn blad_record_i_sync_zatruwa_scratch_i_blokuje_nastepne_prepare() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut scratch = PrequantScratch {
            ready: Some(device.create_event().unwrap()),
            ..PrequantScratch::default()
        };
        let error = resolve_prepared_q8_marker(
            &mut scratch,
            Err(ForgeError::Kernel("wstrzyknięty błąd record".into())),
            || Err(ForgeError::Device("wstrzyknięty błąd sync".into())),
        )
        .unwrap_err();

        assert!(error.to_string().contains("wstrzyknięty błąd record"));
        assert!(error.to_string().contains("wstrzyknięty błąd sync"));
        assert!(scratch.ready.is_none());
        assert!(scratch.poisoned);
        assert!(ensure_prepared_q8_usable(&scratch).is_err());
        assert!(lock_prepared_q8_scratch(&Mutex::new(scratch)).is_err());
    }

    #[test]
    fn zatruty_mutex_zwraca_blad_bez_odzyskania_scratch() {
        let _serialized = PREPARED_Q8_SCRATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scratch = Arc::new(Mutex::new(PrequantScratch::default()));
        let panicking = scratch.clone();
        assert!(std::thread::spawn(move || {
            let _guard = panicking.lock().unwrap();
            panic!("wstrzyknięty panic pod blokadą");
        })
        .join()
        .is_err());

        assert!(lock_prepared_q8_scratch(&scratch).is_err());
        let poisoned = match scratch.lock() {
            Ok(_) => panic!("mutex powinien pozostać zatruty"),
            Err(error) => error.into_inner().poisoned,
        };
        assert!(poisoned);
    }

    #[test]
    fn wybiera_dokladne_buckety_weryfikatora() {
        for (tokens, expected, block) in [
            (2, "gemm_nvfp4_gguf_f16_b2", 32),
            (3, "gemm_nvfp4_gguf_f16_b3_nvidia", 64),
            (4, "gemm_nvfp4_gguf_f16_b4_nvidia", 64),
            (5, "gemm_nvfp4_gguf_f16_b8_nvidia", 64),
            (8, "gemm_nvfp4_gguf_f16_b8_nvidia", 64),
            (9, "gemm_nvfp4_gguf_f16_b16_nvidia", 64),
            (16, "gemm_nvfp4_gguf_f16_b16_nvidia", 64),
        ] {
            let dispatch = nvfp4_gguf_dispatch(tokens, 5120, false, true, 32, 1024).unwrap();
            assert_eq!(dispatch.kernel, expected);
            assert_eq!(dispatch.block_threads, block);
        }
        for tokens in 2..=4 {
            let dispatch = nvfp4_gguf_dispatch(tokens, 5120, false, false, 64, 1024).unwrap();
            assert_eq!(dispatch.block_threads, 64);
        }
        assert_eq!(
            nvfp4_gguf_dispatch(3, 5120, false, false, 64, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_f16_b3"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(4, 5120, false, false, 64, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_f16_b4"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(8, 5120, false, false, 64, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_f16_b8"
        );
    }

    #[test]
    fn wybiera_mma_tylko_dla_nvidia() {
        assert_eq!(
            nvfp4_gguf_dispatch(17, 5120, false, true, 32, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_mma_f16_bm32"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(128, 5120, false, true, 32, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_mma_f16_bm128"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(128, 1024, true, true, 32, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_mma_f16_bm128_bn32"
        );
        for rows in [5120, 10240, 12288, 17408] {
            assert_eq!(
                nvfp4_gguf_dispatch(128, rows, true, true, 32, 1024)
                    .unwrap()
                    .kernel,
                "gemm_nvfp4_gguf_mma_f16_bm128_prefetch"
            );
        }
        for tokens in [256, 2048, 4096] {
            assert_eq!(
                nvfp4_gguf_dispatch(tokens, 5120, true, true, 32, 1024)
                    .unwrap()
                    .kernel,
                "gemm_nvfp4_gguf_mma_f16_bm128_prefetch"
            );
        }
        assert_eq!(
            nvfp4_gguf_dispatch(128, 5120, false, true, 32, 1024)
                .unwrap()
                .kernel,
            "gemm_nvfp4_gguf_mma_f16_bm128"
        );
        assert!(nvfp4_gguf_dispatch(17, 5120, false, false, 64, 1024).is_err());
        assert!(nvfp4_gguf_dispatch(17, 5120, false, true, 64, 1024).is_err());
    }

    #[test]
    fn wybiera_bn128_i_sync1_z_regresyjnym_wyjatkiem() {
        let large = nvfp4_gguf_dispatch_impl(
            2048, 5120, 6144, true, true, true, true, false, false, false, 32, 1024,
        )
        .unwrap();
        assert_eq!(large.kernel, "gemm_nvfp4_gguf_mma_f16_bm128_bn128");
        assert_eq!((large.row_tile, large.block_threads), (128, 256));

        let m128 = nvfp4_gguf_dispatch_impl(
            128, 5120, 5120, true, true, true, true, false, false, false, 32, 1024,
        )
        .unwrap();
        assert_eq!(m128.kernel, "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1");
        assert_eq!((m128.row_tile, m128.block_threads), (64, 256));

        let regression = nvfp4_gguf_dispatch_impl(
            128, 17408, 5120, true, true, true, true, false, false, false, 32, 1024,
        )
        .unwrap();
        assert_eq!(regression.kernel, "gemm_nvfp4_gguf_mma_f16_bm128_prefetch");
    }

    #[test]
    fn wybiera_kafelkowany_nvfp4_tylko_dla_pelnego_kontraktu() {
        let bn64 = nvfp4_gguf_layout_dispatch(
            Nvfp4GgufLayout::TileN128K64,
            128,
            5120,
            5120,
            true,
            true,
            true,
            true,
            true,
            true,
            false,
            false,
            false,
            32,
            1024,
        )
        .unwrap();
        assert_eq!(
            (
                bn64.kernel,
                bn64.token_tile,
                bn64.row_tile,
                bn64.block_threads
            ),
            ("gemm_nvfp4_tile128_mma_f16_bm128_bn64", 128, 64, 256)
        );

        let bn128 = nvfp4_gguf_layout_dispatch(
            Nvfp4GgufLayout::TileN128K64,
            2048,
            5120,
            5120,
            true,
            true,
            true,
            true,
            true,
            true,
            false,
            false,
            false,
            32,
            1024,
        )
        .unwrap();
        assert_eq!(
            (bn128.kernel, bn128.row_tile),
            ("gemm_nvfp4_tile128_mma_f16_bm128_bn128", 128)
        );

        let fallback = nvfp4_gguf_layout_dispatch(
            Nvfp4GgufLayout::TileN128K64,
            2048,
            5120,
            5120,
            true,
            true,
            true,
            true,
            false,
            true,
            false,
            false,
            false,
            32,
            1024,
        )
        .unwrap();
        assert_eq!(
            (fallback.kernel, fallback.row_tile),
            ("gemm_nvfp4_tile128_mma_f16_bm128_bn64", 64)
        );

        for result in [
            nvfp4_gguf_layout_dispatch(
                Nvfp4GgufLayout::TileN128K64,
                1,
                5120,
                5120,
                true,
                true,
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                32,
                1024,
            ),
            nvfp4_gguf_layout_dispatch(
                Nvfp4GgufLayout::TileN128K64,
                128,
                5121,
                5120,
                true,
                true,
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                32,
                1024,
            ),
            nvfp4_gguf_layout_dispatch(
                Nvfp4GgufLayout::TileN128K64,
                128,
                5120,
                5120,
                true,
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                false,
                32,
                1024,
            ),
        ] {
            assert!(result.is_err());
        }
    }

    #[test]
    fn capability_tile_nvfp4_wymaga_kwantyzacji_i_wszystkich_kerneli() {
        let required = [
            "quantize_act_q8_1",
            "nvfp4_repack_tile128",
            "gemv_nvfp4_tile128_coop_q8_1_f16",
            "gemm_nvfp4_tile128_mma_f16_bm128_bn64",
            "gemm_nvfp4_tile128_mma_f16_bm128_bn128",
        ];
        assert!(has_nvfp4_gguf_tile_artifacts(|_| true));
        for missing in required {
            assert!(!has_nvfp4_gguf_tile_artifacts(|name| name != missing));
        }
    }

    #[test]
    fn layout_value_key_wymaga_pelnego_backendu_i_poprawnej_geometrii() {
        assert_eq!(
            delta_state_layout_dispatch(128, 32, 1024, true),
            DeltaStateLayout::ValueKey
        );
        for fallback in [
            delta_state_layout_dispatch(64, 32, 1024, true),
            delta_state_layout_dispatch(128, 0, 1024, true),
            delta_state_layout_dispatch(128, 48, 1024, true),
            delta_state_layout_dispatch(128, 64, 128, true),
            delta_state_layout_dispatch(128, 32, 1024, false),
        ] {
            assert_eq!(fallback, DeltaStateLayout::KeyValue);
        }
        let required = [
            "deltanet_value_key_scan_inplace_f16",
            "deltanet_value_key_scan_checkpoints_f16",
            "deltanet_value_key_commit_recompute_f32",
            "deltanet_value_key_scan_persistent_f16",
        ];
        assert!(has_delta_value_key_artifacts(|_| true));
        for missing in required {
            assert!(!has_delta_value_key_artifacts(|name| name != missing));
        }
    }

    #[test]
    fn offset_checkpointow_f32_musi_byc_wyrownany() {
        assert!(validate_f32_byte_offset("test", 0).is_ok());
        assert!(validate_f32_byte_offset("test", 4).is_ok());
        for offset in [1, 2, 3, 5] {
            assert!(validate_f32_byte_offset("test", offset).is_err());
        }
    }

    #[test]
    fn odrzuca_nieprawidlowy_rozmiar_bloku() {
        assert!(nvfp4_gguf_dispatch(1, 5120, false, true, 32, 1024).is_err());
        assert!(nvfp4_gguf_dispatch(16, 5120, false, false, 64, 512).is_err());
        assert!(nvfp4_gguf_dispatch(3, 5120, false, false, 0, 1024).is_err());
    }

    #[test]
    fn dp4a_wymaga_nvidia_i_warp32() {
        // Decyduje FALA, nie producent: `dot4_i8` ma instrukcje sprzetowa na
        // obu rodzinach, a fala 64 (CDNA, stare GCN) nie pasuje do kafla.
        assert!(raw_nvfp4_dp4a_supported(32));
        assert!(!raw_nvfp4_dp4a_supported(64));
    }

    #[test]
    fn packer_uzywa_logicznych_grup_dla_warp64() {
        assert_eq!(q8_nvfp4_pack_launch(32), (2, 256));
        assert_eq!(q8_nvfp4_pack_launch(64), (1, 64));
    }
}
