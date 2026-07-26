// ===== File: launchers.rs — typed launch wrappers over kernel artifacts =====
// Argument order and meaning must mirror the Mojo kernel signatures exactly
// (kernels/mojo/src/*.mojo). Mojo `Int` marshals as a 64-bit scalar slot,
// `Float32` as f32.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::OnceLock;

use forge_hal::{DevBuffer, Device, Event, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_types::{DType, ForgeError, MemKind, QuantKind, Result};

use crate::registry::KernelArtifacts;

const BLOCK: u32 = 256;
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
        if rows == 0
            || !rows.is_multiple_of(64)
            || cols == 0
            || !cols.is_multiple_of(128)
        {
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
    if source_end > physical_rows
        || output_bytes < required_output
        || input_bytes < required_input
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

/// Warps per block in attn_decode (must not exceed MAX_WARPS in attention.mojo).
const ATTN_BLOCK: u32 = 128;
const ATTN_HD256_BLOCK: u32 = 256;
const ATTN_HD256_SPLITS: usize = 8;
const ATTN_HD256_PARTIAL_STRIDE: usize = 260;

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
    Split8Hd256,
    Split8Hd512,
}

fn attn_decode_plan(
    head_dim: usize,
    vendor: forge_types::Vendor,
    warp_size: u32,
    max_threads: u32,
    split8_available: bool,
) -> Result<AttnDecodePlan> {
    match head_dim {
        64 => Ok(AttnDecodePlan::Generic("attn_decode_f16_hd64")),
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
fn validate_attn_prefill_segmented_f16(
    output_bytes: usize,
    q_bytes: usize,
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    page_table_bytes: usize,
    visible_bytes: usize,
    batch: usize,
    n_tokens: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    head_dim: usize,
    page_size: usize,
    max_pages: usize,
) -> Result<(u32, u32)> {
    if [
        batch, n_tokens, n_q_heads, n_kv_heads, head_dim, page_size, max_pages,
    ]
    .contains(&0)
    {
        return Err(ForgeError::Kernel(
            "segmentowana atencja verifiera wymaga niezerowych wymiarów".into(),
        ));
    }
    if !n_q_heads.is_multiple_of(n_kv_heads) {
        return Err(ForgeError::Kernel(
            "liczba głowic Q segmentowanej atencji musi być wielokrotnością głowic KV".into(),
        ));
    }
    if !matches!(head_dim, 128 | 256) {
        return Err(ForgeError::Kernel(format!(
            "segmentowana atencja obsługuje head_dim 128 albo 256, otrzymano {head_dim}"
        )));
    }
    let total = batch
        .checked_mul(n_tokens)
        .ok_or_else(|| ForgeError::Kernel("przepełnienie liczby tokenów atencji".into()))?;
    let query_bytes = checked_buffer_bytes(
        "segmentowana atencja query/output",
        &[total, n_q_heads, head_dim],
        2,
    )?;
    let required_page_table_bytes =
        checked_buffer_bytes("segmentowana atencja page tables", &[batch, max_pages], 4)?;
    let required_visible_bytes =
        checked_buffer_bytes("segmentowana atencja visible lengths", &[total], 4)?;
    let cache_page_bytes = checked_buffer_bytes(
        "segmentowana atencja strona KV",
        &[n_kv_heads, page_size, head_dim],
        2,
    )?;
    if output_bytes < query_bytes
        || q_bytes < query_bytes
        || page_table_bytes < required_page_table_bytes
        || visible_bytes < required_visible_bytes
        || k_cache_bytes < cache_page_bytes
        || v_cache_bytes < cache_page_bytes
        || k_cache_bytes != v_cache_bytes
        || !k_cache_bytes.is_multiple_of(cache_page_bytes)
    {
        return Err(ForgeError::Kernel(
            "segmentowana atencja verifiera ma za mały lub niezgodny bufor".into(),
        ));
    }
    let grid_x = u32::try_from(total)
        .map_err(|_| ForgeError::Kernel("liczba tokenów atencji przekracza u32".into()))?;
    let grid_y = u32::try_from(n_q_heads)
        .map_err(|_| ForgeError::Kernel("liczba głowic Q przekracza u32".into()))?;
    for (name, value) in [
        ("T", n_tokens),
        ("głowice Q", n_q_heads),
        ("głowice KV", n_kv_heads),
        ("rozmiar strony", page_size),
        ("liczba stron", max_pages),
    ] {
        i64::try_from(value)
            .map_err(|_| ForgeError::Kernel(format!("{name} atencji przekracza i64")))?;
    }
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
struct DotTile {
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
    warp_size: u32,
    max_threads: u32,
) -> Result<Nvfp4GgufDispatch> {
    if n_tokens < 2 {
        return Err(ForgeError::Kernel(
            "gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch tokenów".into(),
        ));
    }
    let (kernel, token_tile, row_tile, block_threads) = match n_tokens {
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
        _ => {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_f16: backend bez NVIDIA MMA nie obsługuje T={n_tokens} > 16"
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

fn raw_nvfp4_dp4a_supported(is_nvidia: bool, warp_size: u32) -> bool {
    is_nvidia && warp_size == 32
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

const HYBRID_PREFILL_T128_ARTIFACTS: [&str; 11] = [
    "deltanet_gated_scan_inplace_shared_d128_f16",
    "deltanet_gated_scan_inplace_dynamic_d128_f16",
    "gemm_nvfp4_gguf_f16_b2",
    "gemm_nvfp4_gguf_f16_b3_nvidia",
    "gemm_nvfp4_gguf_f16_b4_nvidia",
    "gemm_nvfp4_gguf_f16_b8_nvidia",
    "gemm_nvfp4_gguf_f16_b16",
    "gemm_nvfp4_gguf_mma_f16_bm32",
    "gemm_nvfp4_gguf_mma_f16_bm128",
    "gemm_nvfp4_gguf_mma_f16_bm128_bn32",
    "gemm_q8_0_i8mma_triplet_bm64",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DensePrefillLogitsKind {
    F16 { rows: usize, cols: usize },
    Q8_0 { rows: usize, cols: usize },
    NvFp4Gguf { rows: usize, cols: usize },
    /// Q4_K/Q6_K heads run the per-lane dp4a GEMV sweep inside `logits_gemm`
    /// (no batched GEMM-out-f32 kernel; one weight read per lane).
    Q4K { rows: usize, cols: usize },
    Q6K { rows: usize, cols: usize },
}

fn dense_prefill_backend_capable(
    vendor: forge_types::Vendor,
    warp_size: u32,
    max_threads_per_block: u32,
) -> bool {
    matches!(vendor, forge_types::Vendor::Nvidia) && warp_size == 32 && max_threads_per_block >= 256
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
    let attention = match head_dim {
        128 => "attn_prefill_fa_segmented_f16_hd128",
        256 => "attn_prefill_segmented_f16_hd256",
        _ => return false,
    };
    if !has(attention) {
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
            let kernel = match batch {
                4 => "gemm_q8_0_f16_exact_out_f32_b4",
                8 => "gemm_q8_0_f16_exact_out_f32_b8",
                16 => Kernels::q8_0_out_f32_kernel(rows, batch),
                _ => return false,
            };
            has(kernel)
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

fn has_hybrid_prefill_t128_artifacts(mut has: impl FnMut(&str) -> bool) -> bool {
    HYBRID_PREFILL_T128_ARTIFACTS.iter().all(|name| has(name))
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
    if !nvidia_warp32 {
        return 16;
    }
    if !has("gemm_nvfp4_gguf_mma_f16_bm32") || !has("gemm_q8_0_i8mma_triplet_bm64") {
        return 16;
    }
    if !has("deltanet_gated_scan_inplace_shared_d128_f16")
        || !has("gemm_nvfp4_gguf_mma_f16_bm128")
        || !has("gemm_nvfp4_gguf_mma_f16_bm128_bn32")
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
struct QkBatchScratch {
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

impl Kernels {
    /// Sprawdza pełny zestaw kerneli układu NVFP4 TileN128K64.
    pub fn supports_nvfp4_gguf_tile_n128_k64(&self) -> bool {
        let caps = self.device.caps();
        matches!(caps.vendor, forge_types::Vendor::Nvidia)
            && caps.warp_size == 32
            && caps.max_threads_per_block >= 256
            && has_nvfp4_gguf_tile_artifacts(|name| self.artifacts.has(name))
    }

    /// Sprawdza komplet artefaktów wymaganych przez ciągły prefill B2 T32.
    pub fn hybrid_prefill_b2_artifacts_capable(&self) -> bool {
        has_hybrid_prefill_b2_artifacts(|name| self.artifacts.has(name))
    }

    /// Sprawdza artefakty specjalizowanej ścieżki C1 T128 NVFP4.
    pub fn hybrid_prefill_t128_artifacts_capable(&self) -> bool {
        has_hybrid_prefill_t128_artifacts(|name| self.artifacts.has(name))
    }

    /// Zwraca największy chunk NVFP4 obsługiwany przez załadowane artefakty.
    pub fn hybrid_prefill_nvfp4_artifact_chunk_limit(&self) -> usize {
        let caps = self.device.caps();
        let nvidia_warp32 =
            matches!(caps.vendor, forge_types::Vendor::Nvidia) && caps.warp_size == 32;
        hybrid_prefill_nvfp4_artifact_chunk_limit(nvidia_warp32, |name| self.artifacts.has(name))
    }

    /// Sprawdza pełny backend i artefakty równego dense prefill.
    pub fn dense_prefill_batch_capable(
        &self,
        head_dim: usize,
        batch: usize,
        logits: DensePrefillLogitsKind,
    ) -> bool {
        let caps = self.device.caps();
        dense_prefill_backend_capable(caps.vendor, caps.warp_size, caps.max_threads_per_block)
            && dense_prefill_artifacts_capable(head_dim, batch, logits, |name| {
                self.artifacts.has(name)
            })
    }

    /// Wyznacza długość zaakceptowanego draftu i token korekty na GPU.
    pub fn mtp_verify_decide(
        &self,
        decision: &DevBuffer,
        predictions: &DevBuffer,
        input_ids: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_verify_decide")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(decision)
            .buf(predictions)
            .buf(input_ids)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Wyznacza acceptance i correction osobno dla każdego segmentu `[B,T]`.
    pub fn mtp_verify_decide_segmented(
        &self,
        decisions: &DevBuffer,
        predictions: &DevBuffer,
        input_ids: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_tokens < 2 {
            return Err(ForgeError::Kernel(format!(
                "mtp segmented decision wymaga B>0 i T>=2, otrzymano B={batch}, T={n_tokens}"
            )));
        }
        let kernel = self.artifacts.get("mtp_verify_decide_segmented")?;
        let config = LaunchConfig {
            grid: (batch as u32, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(decisions)
            .buf(predictions)
            .buf(input_ids)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F16 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f16(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f16")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F32 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f32(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f32")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje po jednym wierszu F16 wskazanym decyzją każdego segmentu.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_select_row_segmented_f16(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_rows: usize,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_rows == 0 || row_size == 0 {
            return Err(ForgeError::Kernel(
                "segmentowany wybór wiersza wymaga dodatnich wymiarów".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_select_row_segmented_f16")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), batch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decisions)
            .scalar(n_rows as i64)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    pub fn supports_fp8_modular_shape(&self, rows: usize, cols: usize) -> bool {
        self.artifacts.has(&format!("gemm_fp8_mod_{rows}_{cols}"))
    }

    pub fn supports_fp8_hybrid_packers(&self) -> bool {
        self.artifacts.has("pack_nvfp4_fp8") && self.artifacts.has("pack_f16_fp8")
    }

    /// Sprawdza komplet ręcznych artefaktów S0 N64/K128 przed aktywacją loadera.
    pub fn supports_nvfp4_ct_s0_n64k128_manual(&self) -> bool {
        let caps = self.device.caps();
        nvfp4_ct_s0_manual_capable(
            caps.vendor,
            &caps.arch,
            caps.warp_size,
            caps.max_threads_per_block,
            |name| self.artifacts.has(name),
        )
    }

    pub fn supports_fp8_logits(&self) -> bool {
        self.artifacts.has("gemv_fp8_out_f32_v2")
    }

    pub fn supports_attn_decode_gqa4_f16_hd128(&self) -> bool {
        self.artifacts.has("attn_decode_split_gqa4_f16_hd128")
            && self.artifacts.has("attn_decode_combine_gqa2_f16_hd128")
    }

    pub fn supports_deltanet_gated_scan_persistent_d128_f16(&self) -> bool {
        let caps = self.device.caps();
        caps.vendor == forge_types::Vendor::Nvidia
            && caps.warp_size == 32
            && caps.max_threads_per_block >= 64
            && self
                .artifacts
                .has("deltanet_gated_scan_persistent_d128_f16")
    }

    /// Wybiera układ ValueKey tylko wtedy, gdy cały zestaw operacji jest dostępny.
    pub fn preferred_delta_state_layout(&self, d_state: usize) -> DeltaStateLayout {
        let caps = self.device.caps();
        let complete = has_delta_value_key_artifacts(|name| self.artifacts.has(name));
        delta_state_layout_dispatch(
            d_state,
            caps.warp_size,
            caps.max_threads_per_block,
            complete,
        )
    }

    pub fn supports_deltanet_prepare_tiled_d128_c4_f16(&self) -> bool {
        let caps = self.device.caps();
        caps.max_threads_per_block >= 128
            && self.artifacts.has("deltanet_prepare_tiled_d128_c4_f16")
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

    /// out[row] = rmsnorm(x[row]) * weight, f16, one block per row.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `rmsnorm_f16` over a section of a fused buffer, addressed by byte offset
    /// (in/out share the slice). Used by the rot decode path to normalize the
    /// q/k slices of a fused qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16_at(
        &self,
        io: &DevBuffer,
        byte_off: usize,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(io, byte_off)?
            .buf_at(io, byte_off)?
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = rmsnorm(residual) * weight (fused, f16).
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = act(gate) * up nad n elementami f16 (bramkowany FFN).
    ///
    /// Nieliniowość jest parametrem, bo rodziny modeli różnią się nią przy
    /// identycznym kształcie warstwy: SwiGLU (`silu`) w llamie i qwenie, GeGLU
    /// z przybliżeniem tanh w rodzinie Gemma.
    pub fn glu_mul_f16(
        &self,
        act: forge_formats::FfnActivation,
        out: &DevBuffer,
        gate: &DevBuffer,
        up: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(Self::glu_kernel(act))?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(gate)
            .buf(up)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Nazwa kernela nieliniowości bramkowanego FFN.
    fn glu_kernel(act: forge_formats::FfnActivation) -> &'static str {
        match act {
            forge_formats::FfnActivation::SiLU => "silu_mul_f16",
            forge_formats::FfnActivation::GeLUTanh => "gelu_mul_f16",
        }
    }

    /// `glu_mul_f16` where gate and up are sections of one fused gate|up
    /// buffer, addressed by byte offsets.
    #[allow(clippy::too_many_arguments)]
    pub fn glu_mul_f16_at(
        &self,
        act: forge_formats::FfnActivation,
        out: &DevBuffer,
        gate_up: &DevBuffer,
        gate_byte_off: usize,
        up_byte_off: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(Self::glu_kernel(act))?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(gate_up, gate_byte_off)?
            .buf_at(gate_up, up_byte_off)?
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// buf *= factor w miejscu (skalowanie embeddingu w rodzinie Gemma).
    pub fn scale_f16(
        &self,
        buf: &DevBuffer,
        n: usize,
        factor: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("scale_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(buf).scalar(n as i64).scalar(factor);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// logits = cap * tanh(logits / cap) w miejscu (ograniczenie logitów Gemmy).
    /// `offset` liczony w elementach f32 — głowa batcha zapisuje kolejne lane'y
    /// do jednego bufora.
    pub fn softcap_f32(
        &self,
        logits: &DevBuffer,
        offset: usize,
        n: usize,
        cap: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("softcap_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(logits, offset * std::mem::size_of::<f32>())?
            .scalar(n as i64)
            .scalar(cap);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = a * sigmoid(gate) over n f16 elements (attention output gate).
    pub fn sigmoid_mul_f16(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        gate: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sigmoid_mul_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(gate).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// De-interleave a gated Q projection [n_heads, 2*head_dim] into query and
    /// gate halves (each [n_heads, head_dim]). `n = n_heads * head_dim`.
    pub fn deinterleave_gate_f16(
        &self,
        qc: &DevBuffer,
        gatec: &DevBuffer,
        q_full: &DevBuffer,
        head_dim: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deinterleave_gate_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(qc)
            .buf(gatec)
            .buf(q_full)
            .scalar(head_dim as i64)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// In-place neox RoPE over [n_tokens, n_heads, head_dim] f16.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        theta_base: f32,
        freq_factors: Option<&DevBuffer>,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(match freq_factors {
            // Rope proporcjonalne (warstwy globalne Gemmy) dzieli częstotliwość
            // każdej pary przez współczynnik z tensora `rope_freqs`.
            Some(_) => "rope_neox_ff_f16",
            None => "rope_neox_f16",
        })?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((head_dim as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let mut args = LaunchArgs::new()
            .buf(x_io)
            .buf(positions);
        if let Some(ff) = freq_factors {
            args = args.buf(ff);
        }
        let args = args
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `rope_neox_f16` over a section of a fused buffer, addressed by byte
    /// offset. Used by the rot decode path to rope the q/k slices of a fused
    /// qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16_at(
        &self,
        x_io: &DevBuffer,
        byte_off: usize,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        theta_base: f32,
        freq_factors: Option<&DevBuffer>,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(match freq_factors {
            // Rope proporcjonalne (warstwy globalne Gemmy) dzieli częstotliwość
            // każdej pary przez współczynnik z tensora `rope_freqs`.
            Some(_) => "rope_neox_ff_f16",
            None => "rope_neox_f16",
        })?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((head_dim as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let mut args = LaunchArgs::new()
            .buf_at(x_io, byte_off)?
            .buf(positions);
        if let Some(ff) = freq_factors {
            args = args.buf(ff);
        }
        let args = args
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Partial NEOX rotary: rotate only the first `n_rot` dims of each head
    /// (qwen35moe M-RoPE reduces to this for text positions). Layout matches
    /// `rope_neox_f16` ([n_tokens, n_heads, head_dim], in place).
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_partial_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        n_rot: usize,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rope_neox_partial_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((n_rot as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(x_io)
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(n_rot as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Depthwise causal conv (width `d_conv`) + SiLU, one DeltaNet decode step.
    /// `win_io` [conv_dim, d_conv-1] (oldest first) is advanced in place;
    /// `weight` is ggml ssm_conv1d {d_conv, conv_dim} flattened. Grid-stride
    /// over channels.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16(
        &self,
        out: &DevBuffer,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        weight: &DevBuffer,
        conv_dim: usize,
        d_conv: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_conv_silu_f16")?;
        let cfg = LaunchConfig {
            grid: ((conv_dim as u32).div_ceil(256).min(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(win_io)
            .buf(x_new)
            .buf(weight)
            .scalar(conv_dim as i64)
            .scalar(d_conv as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Jeden krok splotu dla wiersza macierzy batcha wskazanego offsetami.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16_at(
        &self,
        out: &DevBuffer,
        out_byte_off: usize,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        x_byte_off: usize,
        weight: &DevBuffer,
        conv_dim: usize,
        d_conv: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_conv_silu_f16")?;
        let cfg = LaunchConfig {
            grid: ((conv_dim as u32).div_ceil(256).min(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(out, out_byte_off)?
            .buf(win_io)
            .buf_at(x_new, x_byte_off)?
            .buf(weight)
            .scalar(conv_dim as i64)
            .scalar(d_conv as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head L2 normalization (out = x / sqrt(Σx² + eps)); one block per
    /// head, block covers `d_state`. Used on the DeltaNet conv q/k heads.
    pub fn l2norm_heads_f16(
        &self,
        out: &DevBuffer,
        x_in: &DevBuffer,
        n_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("l2norm_heads_f16")?;
        let cfg = LaunchConfig {
            grid: (n_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x_in)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// One Gated-DeltaNet recurrence step per value-head (grid = n_v_heads,
    /// block = d_state). `state_io` [n_v_heads, d_state, d_state] f32 is
    /// updated in place; q/k must already be L2-normed and repeated to
    /// n_v_heads. `g`/`beta` are the per-head log-decay / write gate (f32).
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_step_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, block_x) = validate_deltanet_gated_step_f16(
            out.len(),
            state_io.len(),
            q.len(),
            k.len(),
            v.len(),
            g.len(),
            beta.len(),
            n_v_heads,
            d_state,
            self.device.caps().max_threads_per_block,
        )?;
        let k_art = self.artifacts.get("deltanet_gated_step_f16")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Scala przygotowanie krótkiego przebiegu DeltaNet dla 2-4 tokenów.
    /// Stan okna wejściowego pozostaje niezmieniony, a checkpointy są token-major.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_checkpoints: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let kernel_name = match n_steps {
            2 => "deltanet_prepare_t2_f16",
            3 => "deltanet_prepare_t3_f16",
            4 => "deltanet_prepare_t4_f16",
            1.. => "deltanet_prepare_dynamic_f16",
            _ => return Err(ForgeError::Kernel("deltanet_prepare wymaga T > 0".into())),
        };
        let caps = self.device.caps();
        if n_k_heads == 0
            || n_v_heads == 0
            || !n_v_heads.is_multiple_of(n_k_heads)
            || d_state == 0
            || d_state.max(32) > caps.max_threads_per_block as usize
            || d_conv < 2
            || !eps.is_finite()
            || eps < 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "deltanet_prepare: niepoprawny kształt n_k={n_k_heads}, n_v={n_v_heads}, d_state={d_state}, d_conv={d_conv}, eps={eps}"
            )));
        }
        let key_heads = n_k_heads.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_heads = key_heads.checked_add(n_v_heads).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_dim = conv_heads
            .checked_mul(d_state)
            .ok_or_else(|| ForgeError::Kernel("deltanet_prepare: przepełnienie conv_dim".into()))?;
        let window = d_conv - 1;
        let vector_bytes = checked_buffer_bytes(
            "deltanet_prepare QKV output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let gate_f32_bytes =
            checked_buffer_bytes("deltanet_prepare gates output", &[n_steps, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("deltanet_prepare gates input", &[n_steps, n_v_heads], 2)?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_prepare conv checkpoints",
            &[n_steps, conv_dim, window],
            2,
        )?;
        let initial_bytes =
            checked_buffer_bytes("deltanet_prepare conv initial", &[conv_dim, window], 2)?;
        let mixed_bytes =
            checked_buffer_bytes("deltanet_prepare qkv mixed", &[n_steps, conv_dim], 2)?;
        let weight_bytes =
            checked_buffer_bytes("deltanet_prepare conv weight", &[conv_dim, d_conv], 2)?;
        let parameter_bytes = checked_buffer_bytes("deltanet_prepare parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_checkpoints.len() < checkpoint_bytes
            || conv_initial.len() < initial_bytes
            || qkv_mixed.len() < mixed_bytes
            || conv_weight.len() < weight_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_prepare: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x = u32::try_from(n_k_heads + n_v_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: liczba głów przekracza u32".into())
        })?;
        let block_x = u32::try_from(d_state.max(32)).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: rozmiar bloku przekracza u32".into())
        })?;
        let n_k_heads = i64::try_from(n_k_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_k_heads przekracza i64".into()))?;
        let n_v_heads = i64::try_from(n_v_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_v_heads przekracza i64".into()))?;
        let d_state = i64::try_from(d_state)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_state przekracza i64".into()))?;
        let d_conv = i64::try_from(d_conv)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_conv przekracza i64".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_checkpoints)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale);
        let args = if n_steps > 4 || n_steps == 1 {
            args.scalar(n_steps as i64)
        } else {
            args
        };
        let args = args
            .scalar(n_k_heads)
            .scalar(n_v_heads)
            .scalar(d_state)
            .scalar(d_conv)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje niezależne segmenty DeltaNet w układzie `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_segmented_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_checkpoints: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie DeltaNet wymaga B,T,d_state > 0".into(),
            ));
        }
        let kernel = self.artifacts.get("deltanet_prepare_segmented_f16")?;
        let config = LaunchConfig {
            grid: ((n_k_heads + n_v_heads) as u32, batch as u32, 1),
            block: (d_state.max(32) as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_checkpoints)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_steps as i64)
            .scalar(n_k_heads as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64)
            .scalar(d_conv as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje segmenty DeltaNet, zachowując tylko końcowe okno conv.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_segmented_final_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_final: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if [batch, n_steps, n_k_heads, n_v_heads, d_state, d_conv].contains(&0)
            || d_state > 1024
            || !n_v_heads.is_multiple_of(n_k_heads)
        {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie final wymaga poprawnych niezerowych wymiarów".into(),
            ));
        }
        let total = batch
            .checked_mul(n_steps)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie B×T DeltaNet final".into()))?;
        let conv_dim = n_k_heads
            .checked_mul(2)
            .and_then(|heads| heads.checked_add(n_v_heads))
            .and_then(|heads| heads.checked_mul(d_state))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv_dim DeltaNet final".into()))?;
        let value_dim = n_v_heads
            .checked_mul(d_state)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie value_dim DeltaNet final".into()))?;
        let conv_elems = conv_dim
            .checked_mul(d_conv - 1)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv state DeltaNet final".into()))?;
        let vector_bytes = checked_buffer_bytes("DeltaNet final vectors", &[total, value_dim], 2)?;
        let gate_f32_bytes = checked_buffer_bytes("DeltaNet final gates", &[total, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("DeltaNet final raw gates", &[total, n_v_heads], 2)?;
        let conv_bytes = checked_buffer_bytes("DeltaNet final conv", &[batch, conv_elems], 2)?;
        let mixed_bytes = checked_buffer_bytes("DeltaNet final mixed", &[total, conv_dim], 2)?;
        let conv_weight_bytes =
            checked_buffer_bytes("DeltaNet final conv weight", &[conv_dim, d_conv], 2)?;
        let parameter_bytes = checked_buffer_bytes("DeltaNet final parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_final.len() < conv_bytes
            || conv_initial.len() < conv_bytes
            || qkv_mixed.len() < mixed_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || conv_weight.len() < conv_weight_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie final ma za mały bufor".into(),
            ));
        }
        let grid_heads = n_k_heads
            .checked_add(n_v_heads)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie grid.x DeltaNet final".into()))?;
        let grid_x = u32::try_from(grid_heads)
            .map_err(|_| ForgeError::Kernel("DeltaNet final grid.x przekracza u32".into()))?;
        let grid_y = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("DeltaNet final grid.y przekracza u32".into()))?;
        let block_x = u32::try_from(d_state.max(32))
            .map_err(|_| ForgeError::Kernel("DeltaNet final block przekracza u32".into()))?;
        let kernel = self.artifacts.get("deltanet_prepare_segmented_final_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_final)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_steps as i64)
            .scalar(n_k_heads as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64)
            .scalar(d_conv as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje pojedynczy prefiks DeltaNet D128/C4 w kaflach czasu T32.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_tiled_d128_c4_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_final: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if [n_steps, n_k_heads, n_v_heads].contains(&0)
            || !n_v_heads.is_multiple_of(n_k_heads)
            || !eps.is_finite()
            || !self.supports_deltanet_prepare_tiled_d128_c4_f16()
        {
            return Err(ForgeError::Unsupported(
                "kafelkowane przygotowanie DeltaNet wymaga D128/C4 i poprawnych wymiarów".into(),
            ));
        }
        let conv_dim = n_k_heads
            .checked_mul(2)
            .and_then(|heads| heads.checked_add(n_v_heads))
            .and_then(|heads| heads.checked_mul(128))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv_dim DeltaNet tiled".into()))?;
        let value_dim = n_v_heads
            .checked_mul(128)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie value_dim DeltaNet tiled".into()))?;
        let vector_bytes =
            checked_buffer_bytes("DeltaNet tiled vectors", &[n_steps, value_dim], 2)?;
        let gate_f32_bytes =
            checked_buffer_bytes("DeltaNet tiled gates", &[n_steps, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("DeltaNet tiled raw gates", &[n_steps, n_v_heads], 2)?;
        let conv_state_bytes =
            checked_buffer_bytes("DeltaNet tiled conv state", &[conv_dim, 3], 2)?;
        let mixed_bytes = checked_buffer_bytes("DeltaNet tiled mixed", &[n_steps, conv_dim], 2)?;
        let conv_weight_bytes =
            checked_buffer_bytes("DeltaNet tiled conv weight", &[conv_dim, 4], 2)?;
        let parameter_bytes = checked_buffer_bytes("DeltaNet tiled parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_final.len() < conv_state_bytes
            || conv_initial.len() < conv_state_bytes
            || qkv_mixed.len() < mixed_bytes
            || conv_weight.len() < conv_weight_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "kafelkowane przygotowanie DeltaNet ma za mały bufor".into(),
            ));
        }
        let grid_heads = n_k_heads
            .checked_add(n_v_heads)
            .ok_or_else(|| ForgeError::Kernel("DeltaNet tiled grid.x przepełniony".into()))?;
        let grid_x = u32::try_from(grid_heads)
            .map_err(|_| ForgeError::Kernel("DeltaNet tiled grid.x przekracza u32".into()))?;
        let steps = u32::try_from(n_steps)
            .map_err(|_| ForgeError::Kernel("DeltaNet tiled T przekracza u32".into()))?;
        for value in [n_steps, n_k_heads, n_v_heads] {
            i64::try_from(value)
                .map_err(|_| ForgeError::Kernel("wymiar DeltaNet tiled przekracza i64".into()))?;
        }
        let kernel = self.artifacts.get("deltanet_prepare_tiled_d128_c4_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, steps.div_ceil(32), 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_final)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(i64::try_from(n_steps).expect("T sprawdzone przez validator"))
            .scalar(i64::try_from(n_k_heads).expect("głowice K sprawdzone przez validator"))
            .scalar(i64::try_from(n_v_heads).expect("głowice V sprawdzone przez validator"))
            .scalar(128i64)
            .scalar(4i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje niezależne stany D128 dla segmentów `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_segmented_d128_f16(
        &self,
        output: &DevBuffer,
        checkpoints: &DevBuffer,
        states: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "segmentowany skan DeltaNet wymaga B,T > 0 i d_state=128".into(),
            ));
        }
        let tile_width = 64usize.min(self.device.caps().max_threads_per_block as usize);
        let grid_x = n_v_heads
            .checked_mul(d_state.div_ceil(tile_width))
            .ok_or_else(|| {
                ForgeError::Kernel("przepełnienie siatki segmentowanego skanu DeltaNet".into())
            })?;
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_segmented_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid_x as u32, batch as u32, 1),
            block: (tile_width as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(checkpoints)
            .buf(states)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje segmenty D128, utrzymując stan warstwy w pamięci współdzielonej.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_segmented_shared_d128_f16(
        &self,
        output: &DevBuffer,
        final_states: &DevBuffer,
        states: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || n_v_heads == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "współdzielony skan segmentowany wymaga B,T,H > 0 i d_state=128".into(),
            ));
        }
        let vector_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany wektory",
            &[batch, n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany stany",
            &[batch, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany bramki",
            &[batch, n_steps, n_v_heads],
            4,
        )?;
        if output.len() < vector_bytes
            || final_states.len() < state_bytes
            || states.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "współdzielony skan segmentowany ma za mały bufor".into(),
            ));
        }
        let grid_x = n_v_heads.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie siatki współdzielonego skanu".into())
        })?;
        let grid_x = u32::try_from(grid_x).map_err(|_| {
            ForgeError::Kernel("siatka współdzielonego skanu przekracza u32".into())
        })?;
        let grid_y = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("batch współdzielonego skanu przekracza u32".into()))?;
        for (name, value) in [("T", n_steps), ("H", n_v_heads), ("D", d_state)] {
            i64::try_from(value).map_err(|_| {
                ForgeError::Kernel(format!("{name} współdzielonego skanu przekracza i64"))
            })?;
        }
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_segmented_shared_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(final_states)
            .buf(states)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Odtwarza wybrany prefiks segmentu bez pośrednich checkpointów w VRAM.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_recompute_segmented_shared_d128_f32(
        &self,
        states: &DevBuffer,
        initial_states: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        max_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || max_steps == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "commit segmentowany wymaga B,T > 0 i d_state=128".into(),
            ));
        }
        let grid_x = n_v_heads
            .checked_mul(2)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie siatki commitu DeltaNet".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_commit_recompute_segmented_shared_d128_f32")?;
        let config = LaunchConfig {
            grid: (grid_x as u32, batch as u32, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(states)
            .buf(initial_states)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .buf(decisions)
            .scalar(max_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zatwierdza po jednej decyzji segmentowej dla każdego lane DeltaNet.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_segmented_f32(
        &self,
        states: &DevBuffer,
        checkpoints: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let state_elements = n_v_heads
            .checked_mul(d_state)
            .and_then(|value| value.checked_mul(d_state))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie stanu DeltaNet".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_commit_checkpoint_segmented_f32")?;
        let config = LaunchConfig {
            grid: ((state_elements as u32).div_ceil(BLOCK), batch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(states)
            .buf(checkpoints)
            .buf(decisions)
            .scalar(state_elements as i64)
            .scalar(n_steps as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przyczynowy skan 2-4 kroków Gated-DeltaNet bez modyfikowania stanu
    /// wejściowego. Checkpointy mają układ [T, n_v_heads, d_state, d_state].
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_scan_f16_at(
            out,
            checkpoints,
            0,
            state_in,
            q,
            k,
            v,
            g,
            beta,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Przyczynowy skan Gated-DeltaNet zapisujący checkpointy od podanego
    /// przesunięcia bajtowego w większym buforze współdzielonym przez warstwy.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16_at(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("deltanet_gated_scan", checkpoint_byte_offset)?;
        let caps = self.device.caps();
        let dynamic_tiled =
            std::env::var("FORGE_DELTANET_SCAN_TILED").map_or(true, |value| value != "0");
        let tiled = d_state <= 128
            && (matches!(n_steps, 3 | 4) || (n_steps != 2 && dynamic_tiled))
            && caps.warp_size > 0
            && caps.warp_size <= caps.max_threads_per_block
            && caps.warp_size <= 128;
        let kernel_name = match (n_steps, tiled) {
            (2, _) => "deltanet_gated_scan_t2_f16",
            (3, true) => "deltanet_gated_scan_t3_d128_f16",
            (4, true) => "deltanet_gated_scan_t4_d128_f16",
            (3, false) => "deltanet_gated_scan_t3_f16",
            (4, false) => "deltanet_gated_scan_t4_f16",
            (1 | 5.., true) => "deltanet_gated_scan_dynamic_d128_f16",
            (1.., false) => "deltanet_gated_scan_dynamic_f16",
            _ => {
                return Err(ForgeError::Kernel(
                    "deltanet_gated_scan wymaga T > 0".into(),
                ))
            }
        };
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_gated_scan wymaga n_v_heads > 0 i 1 <= d_state <= 1024, otrzymano n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let output_bytes = checked_buffer_bytes(
            "deltanet_gated_scan output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "deltanet_gated_scan state",
            &[n_v_heads, d_state, d_state],
            4,
        )?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_gated_scan checkpoints",
            &[n_steps, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes =
            checked_buffer_bytes("deltanet_gated_scan gates", &[n_steps, n_v_heads], 4)?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_gated_scan: przepełnienie offsetu checkpointów".into())
            })?;
        if out.len() < output_bytes
            || checkpoints.len() < checkpoint_end
            || state_in.len() < state_bytes
            || q.len() < output_bytes
            || k.len() < output_bytes
            || v.len() < output_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_gated_scan: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let block_x = if tiled {
            caps.warp_size
        } else {
            u32::try_from(d_state).map_err(|_| {
                ForgeError::Kernel("deltanet_gated_scan: d_state przekracza u32".into())
            })?
        };
        let head_tiles = if tiled {
            d_state.div_ceil(block_x as usize)
        } else {
            1
        };
        let grid_heads = n_v_heads.checked_mul(head_tiles).ok_or_else(|| {
            ForgeError::Kernel("deltanet_gated_scan: przepełnienie liczby kafli".into())
        })?;
        let grid_x = u32::try_from(grid_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_gated_scan: liczba głów przekracza u32".into())
        })?;
        let k_art = self.artifacts.get(kernel_name)?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta);
        let args = if n_steps > 4 || n_steps == 1 {
            args.scalar(n_steps as i64)
        } else {
            args
        };
        let args = args.scalar(n_v_heads as i64).scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Wykonuje dynamiczny skan prefill bezpośrednio na stanie końcowym.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_inplace_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_scan_inplace_f16_at(
            out, state_io, q, k, v, g, beta, 0, n_steps, n_v_heads, d_state, stream,
        )
    }

    /// Wykonuje fragment skanu na wierszach większych macierzy token-major.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_inplace_f16_at(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        token_offset: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if n_steps == 0
            || n_v_heads == 0
            || d_state == 0
            || d_state > 128
            || caps.warp_size == 0
            || caps.warp_size > 128
            || caps.warp_size > caps.max_threads_per_block
        {
            return Err(ForgeError::Kernel(format!(
                "in-place DeltaNet wymaga T>0, heads>0, d_state<=128 i poprawnego warp, otrzymano T={n_steps}, heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let vector_bytes = checked_buffer_bytes(
            "in-place DeltaNet vectors",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes =
            checked_buffer_bytes("in-place DeltaNet state", &[n_v_heads, d_state, d_state], 4)?;
        let gate_bytes = checked_buffer_bytes("in-place DeltaNet gates", &[n_steps, n_v_heads], 4)?;
        let vector_byte_offset = checked_buffer_bytes(
            "in-place DeltaNet vector offset",
            &[token_offset, n_v_heads, d_state],
            2,
        )?;
        let gate_byte_offset = checked_buffer_bytes(
            "in-place DeltaNet gate offset",
            &[token_offset, n_v_heads],
            4,
        )?;
        let vector_end = vector_byte_offset
            .checked_add(vector_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("in-place DeltaNet: przepełnienie zakresu wektorów".into())
            })?;
        let gate_end = gate_byte_offset.checked_add(gate_bytes).ok_or_else(|| {
            ForgeError::Kernel("in-place DeltaNet: przepełnienie zakresu bramek".into())
        })?;
        if out.len() < vector_end
            || state_io.len() < state_bytes
            || q.len() < vector_end
            || k.len() < vector_end
            || v.len() < vector_end
            || g.len() < gate_end
            || beta.len() < gate_end
        {
            return Err(ForgeError::Kernel(
                "in-place DeltaNet: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let tile_width = (caps.warp_size as usize)
            .max(64)
            .min(d_state)
            .min(caps.max_threads_per_block as usize);
        let tiles = d_state.div_ceil(tile_width);
        let grid = n_v_heads
            .checked_mul(tiles)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("in-place DeltaNet: grid przekracza u32".into()))?;
        let kernel_name = if n_steps == 128 && d_state == 128 && tile_width == 64 {
            "deltanet_gated_scan_inplace_shared_d128_f16"
        } else {
            "deltanet_gated_scan_inplace_dynamic_d128_f16"
        };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (tile_width as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(out, vector_byte_offset)?
            .buf(state_io)
            .buf_at(q, vector_byte_offset)?
            .buf_at(k, vector_byte_offset)?
            .buf_at(v, vector_byte_offset)?
            .buf_at(g, gate_byte_offset)?
            .buf_at(beta, gate_byte_offset)?
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje stan ValueKey `[B,H,value,key]`, zapisując wyłącznie stan końcowy.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_inplace_f16(
        &self,
        out: &DevBuffer,
        state_out: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "skan ValueKey wymaga kompletnego backendu d128 i niezerowych wymiarów".into(),
            ));
        }
        let vector_bytes = checked_buffer_bytes(
            "ValueKey vectors",
            &[n_sequences, n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "ValueKey state",
            &[n_sequences, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes =
            checked_buffer_bytes("ValueKey gates", &[n_sequences, n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_out.len() < state_bytes
            || state_in.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "skan ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let block = caps.warp_size * 4;
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("siatka ValueKey przekracza u32".into()))?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch ValueKey przekracza u32".into()))?;
        let kernel = self.artifacts.get("deltanet_value_key_scan_inplace_f16")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_out)
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_sequences as i64)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje długi prefill ValueKey z dwiema kolumnami przypisanymi do warpa.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_persistent_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "persistent ValueKey wymaga kompletnego backendu d128".into(),
            ));
        }
        let vector_bytes =
            checked_buffer_bytes("persistent ValueKey vectors", &[n_steps, n_v_heads, 128], 2)?;
        let state_bytes =
            checked_buffer_bytes("persistent ValueKey state", &[n_v_heads, 128, 128], 4)?;
        let gate_bytes =
            checked_buffer_bytes("persistent ValueKey gates", &[n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_io.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "persistent ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ForgeError::Kernel("siatka persistent ValueKey przekracza u32".into())
            })?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_scan_persistent_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (caps.warp_size * 2, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje stan ValueKey i zapisuje checkpoint po każdym tokenie.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_checkpoints_f16(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_value_key_scan_checkpoints_f16_at(
            out,
            checkpoints,
            0,
            state_in,
            q,
            k,
            v,
            g,
            beta,
            n_sequences,
            n_steps,
            n_v_heads,
            stream,
        )
    }

    /// Zapisuje checkpointy ValueKey od przesunięcia w większym workspace.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_checkpoints_f16_at(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("checkpointy ValueKey", checkpoint_byte_offset)?;
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "checkpointy ValueKey wymagają kompletnego backendu d128".into(),
            ));
        }
        let state_bytes = checked_buffer_bytes(
            "ValueKey checkpoint state",
            &[n_sequences, n_v_heads, d_state, d_state],
            4,
        )?;
        let checkpoint_bytes = state_bytes
            .checked_mul(n_steps)
            .ok_or_else(|| ForgeError::Kernel("checkpointy ValueKey przekraczają usize".into()))?;
        let vector_bytes = checked_buffer_bytes(
            "ValueKey checkpoint vectors",
            &[n_sequences, n_steps, n_v_heads, d_state],
            2,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "ValueKey checkpoint gates",
            &[n_sequences, n_steps, n_v_heads],
            4,
        )?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("offset checkpointów ValueKey przepełnia usize".into())
            })?;
        if out.len() < vector_bytes
            || checkpoints.len() < checkpoint_end
            || state_in.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "skan checkpointów ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ForgeError::Kernel("siatka checkpointów ValueKey przekracza u32".into())
            })?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch checkpointów ValueKey przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_scan_checkpoints_f16")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (caps.warp_size * 4, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_sequences as i64)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Odtwarza na ValueKey zaakceptowany prefiks każdej sekwencji.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_commit_recompute_f32(
        &self,
        state_out: &DevBuffer,
        state_in: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        decisions: &DevBuffer,
        n_sequences: usize,
        max_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || max_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "recompute ValueKey wymaga kompletnego backendu d128".into(),
            ));
        }
        let state_bytes = checked_buffer_bytes(
            "ValueKey recompute state",
            &[n_sequences, n_v_heads, 128, 128],
            4,
        )?;
        let vector_bytes = checked_buffer_bytes(
            "ValueKey recompute vectors",
            &[n_sequences, max_steps, n_v_heads, 128],
            2,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "ValueKey recompute gates",
            &[n_sequences, max_steps, n_v_heads],
            4,
        )?;
        let decision_bytes =
            checked_buffer_bytes("ValueKey recompute decisions", &[n_sequences, 2], 4)?;
        if state_out.len() < state_bytes
            || state_in.len() < state_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
            || decisions.len() < decision_bytes
        {
            return Err(ForgeError::Kernel(
                "recompute ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("siatka recompute ValueKey przekracza u32".into()))?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch recompute ValueKey przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_commit_recompute_f32")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (caps.warp_size * 4, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(state_out)
            .buf(state_in)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .buf(decisions)
            .scalar(n_sequences as i64)
            .scalar(max_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Wykonuje pełny rejestrowy skan DeltaNet d128 jednym uruchomieniem.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_persistent_d128_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_steps == 0
            || n_v_heads == 0
            || !self.supports_deltanet_gated_scan_persistent_d128_f16()
        {
            return Err(ForgeError::Unsupported(
                "persistent DeltaNet wymaga T>0, heads>0 oraz NVIDIA warp32".into(),
            ));
        }
        let vector_bytes =
            checked_buffer_bytes("persistent DeltaNet vectors", &[n_steps, n_v_heads, 128], 2)?;
        let state_bytes =
            checked_buffer_bytes("persistent DeltaNet state", &[n_v_heads, 128, 128], 4)?;
        let gate_bytes =
            checked_buffer_bytes("persistent DeltaNet gates", &[n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_io.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "persistent DeltaNet: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("persistent DeltaNet: grid przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_persistent_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(128i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zatwierdza na GPU checkpoint wskazany przez urządzeniowy licznik i32.
    /// Wartość 0 pozostawia stan bez zmian, a wartości spoza [0, T] są ignorowane.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_commit_checkpoint_f32_at(
            state_out,
            checkpoints,
            0,
            accepted_index,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Zatwierdza checkpoint z fragmentu większego bufora zaczynającego się
    /// pod podanym przesunięciem bajtowym.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32_at(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("deltanet_commit_checkpoint", checkpoint_byte_offset)?;
        if n_steps == 0 {
            return Err(ForgeError::Kernel(
                "deltanet_commit_checkpoint wymaga T > 0".into(),
            ));
        }
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_commit_checkpoint: niepoprawny kształt n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let state_elements = n_v_heads
            .checked_mul(d_state)
            .and_then(|elements| elements.checked_mul(d_state))
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie stanu".into())
            })?;
        let state_bytes = state_elements.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie bajtów stanu".into())
        })?;
        let checkpoint_bytes = state_bytes.checked_mul(n_steps).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie checkpointów".into())
        })?;
        let state_elements_i64 = i64::try_from(state_elements).map_err(|_| {
            ForgeError::Kernel("deltanet_commit_checkpoint: liczba elementów przekracza i64".into())
        })?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel(
                    "deltanet_commit_checkpoint: przepełnienie offsetu checkpointów".into(),
                )
            })?;
        if state_out.len() < state_bytes
            || checkpoints.len() < checkpoint_end
            || accepted_index.len() < std::mem::size_of::<i32>()
        {
            return Err(ForgeError::Kernel(
                "deltanet_commit_checkpoint: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x =
            u32::try_from(state_elements.div_ceil(BLOCK as usize).min(65_535)).map_err(|_| {
                ForgeError::Kernel("deltanet_commit_checkpoint: siatka przekracza u32".into())
            })?;
        let k_art = self.artifacts.get("deltanet_commit_checkpoint_f32")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(state_out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(accepted_index)
            .scalar(state_elements_i64)
            .scalar(n_steps as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Output gated RMSNorm per value-head: out = rmsnorm(o, weight)·silu(z).
    /// One block per head, block covers `d_state`.
    #[allow(clippy::too_many_arguments)]
    /// `deltanet_gated_rmsnorm_f16` czytający bramkę `z` z przesunięcia lane'a.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_rmsnorm_f16_at(
        &self,
        out: &DevBuffer,
        o_in: &DevBuffer,
        z_in: &DevBuffer,
        z_byte_off: usize,
        weight: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_gated_rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (n_v_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(o_in)
            .buf_at(z_in, z_byte_off)?
            .buf(weight)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn deltanet_gated_rmsnorm_f16(
        &self,
        out: &DevBuffer,
        o_in: &DevBuffer,
        z_in: &DevBuffer,
        weight: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_gated_rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (n_v_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(o_in)
            .buf(z_in)
            .buf(weight)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head DeltaNet log-decay g = softplus(alpha + dt_bias)·a (f32 out).
    pub fn deltanet_log_decay_f32(
        &self,
        g_out: &DevBuffer,
        alpha_in: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_log_decay_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(g_out)
            .buf(alpha_in)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wariant batchowy pojedynczego wiersza z przesunięciem buforów wejścia
    /// i wyjścia; wektory parametrów warstwy zawsze zaczynają się od zera.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_log_decay_f32_at(
        &self,
        g_out: &DevBuffer,
        g_byte_off: usize,
        alpha_in: &DevBuffer,
        alpha_byte_off: usize,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_log_decay_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(g_out, g_byte_off)?
            .buf_at(alpha_in, alpha_byte_off)?
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head DeltaNet write gate beta = sigmoid(beta_proj) (f32 out).
    /// `deltanet_beta_sigmoid_f32` czytający wiersz `beta_byte_off` wejścia.
    /// Pozwala batchowemu decode wziąć swój lane wprost z projekcji policzonej
    /// dla całego batcha, bez kopii do jednotokenowego scratchu.
    pub fn deltanet_beta_sigmoid_f32_at(
        &self,
        beta_out: &DevBuffer,
        beta_in: &DevBuffer,
        beta_byte_off: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_beta_sigmoid_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(beta_out)
            .buf_at(beta_in, beta_byte_off)?
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn deltanet_beta_sigmoid_f32(
        &self,
        beta_out: &DevBuffer,
        beta_in: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_beta_sigmoid_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(beta_out)
            .buf(beta_in)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q8_0 blocks, x/y f16. One block per output row.
    pub fn gemv_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q8_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x, all f16. One block per output row.
    pub fn gemv_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new().buf(y).buf(w).buf(x).scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Mnoży y = W·x dla wag NVFP4 w układzie packed compressed-tensors.
    /// `inv_global_scale` jest odwrotnością `weight_global_scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4 requires cols % 16 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_nvfp4_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(packed)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Przepakowuje ograniczony chunk row-major do docelowego resident S0.
    #[allow(clippy::too_many_arguments)]
    pub fn repack_nvfp4_ct_s0_n64k128_into(
        &self,
        target: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        physical_rows: usize,
        cols: usize,
        source_rows: usize,
        target_row_offset: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 128
        {
            return Err(ForgeError::Unsupported(
                "repack NVFP4 CT wymaga NVIDIA warp32".into(),
            ));
        }
        let _ = validate_nvfp4_ct_repack_extents(
            target.len(),
            packed.len(),
            scales.len(),
            physical_rows,
            cols,
            source_rows,
            target_row_offset,
        )?;
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
        let target_end = target_row_offset.checked_add(source_rows).ok_or_else(|| {
            ForgeError::Kernel("repack NVFP4 CT: przepełnienie zakresu".into())
        })?;
        if target_end > physical_rows {
            return Err(ForgeError::Kernel(
                "repack NVFP4 CT: chunk wykracza poza resident".into(),
            ));
        }
        let target_bytes = checked_buffer_bytes(
            "repack NVFP4 CT target",
            &[physical_rows, cols],
            9,
        )? / 16;
        let packed_bytes =
            checked_buffer_bytes("repack NVFP4 CT packed", &[source_rows, cols], 1)? / 2;
        let scale_bytes =
            checked_buffer_bytes("repack NVFP4 CT scales", &[source_rows, cols], 1)? / 16;
        if target.len() != target_bytes
            || packed.len() < packed_bytes
            || scales.len() < scale_bytes
        {
            return Err(ForgeError::Kernel(
                "repack NVFP4 CT: niezgodny rozmiar bufora".into(),
            ));
        }
        let stages = source_rows
            .checked_div(64)
            .and_then(|tiles| tiles.checked_mul(cols / 128))
            .ok_or_else(|| ForgeError::Kernel("repack NVFP4 CT: przepełnienie siatki".into()))?;
        let grid_x = u32::try_from(stages)
            .map_err(|_| ForgeError::Kernel("repack NVFP4 CT: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get("repack_nvfp4_ct_s0_n64k128_into")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(target)
            .buf(packed)
            .buf(scales)
            .scalar(cols as i64)
            .scalar(source_rows as i64)
            .scalar(target_row_offset as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Mnoży wyrównane okno wierszy resident S0 przez pojedynczy wektor F16.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_ct_s0_n64k128_f16(
        &self,
        y: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        x: &DevBuffer,
        source_row_offset: usize,
        rows: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemv_nvfp4_ct_s0_n64k128_f16_at(
            y,
            0,
            weights,
            x,
            0,
            source_row_offset,
            rows,
            inv_global_scale,
            stream,
        )
    }

    /// Mnoży jeden wiersz batch z kontrolowanymi offsetami buforów F16.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_ct_s0_n64k128_f16_at(
        &self,
        y: &DevBuffer,
        y_byte_offset: usize,
        weights: Nvfp4CtS0View<'_>,
        x: &DevBuffer,
        x_byte_offset: usize,
        source_row_offset: usize,
        rows: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemv_batch_nvfp4_ct_s0_n64k128_f16_at(
            y,
            y_byte_offset,
            weights,
            x,
            x_byte_offset,
            source_row_offset,
            rows,
            1,
            inv_global_scale,
            stream,
        )
    }

    /// Mnoży M1..M16 z kolejnością arytmetyki zgodną z row-major GEMV.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_batch_nvfp4_ct_s0_n64k128_f16_at(
        &self,
        y: &DevBuffer,
        y_byte_offset: usize,
        weights: Nvfp4CtS0View<'_>,
        x: &DevBuffer,
        x_byte_offset: usize,
        source_row_offset: usize,
        rows: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
        {
            return Err(ForgeError::Unsupported(
                "decode NVFP4 CT wymaga NVIDIA warp32".into(),
            ));
        }
        let (kernel_name, bucket) = match n_tokens {
            1 => ("gemv_nvfp4_ct_s0_n64k128_f16", 1),
            2..=4 => ("gemv_batch_nvfp4_ct_s0_n64k128_f16_b4", 4),
            5..=8 => ("gemv_batch_nvfp4_ct_s0_n64k128_f16_b8", 8),
            9..=16 => ("gemv_batch_nvfp4_ct_s0_n64k128_f16_b16", 16),
            _ => {
                return Err(ForgeError::Unsupported(format!(
                    "decode NVFP4 CT obsługuje M1..M16, otrzymano M{n_tokens}"
                )))
            }
        };
        let output_bytes =
            checked_buffer_bytes("decode NVFP4 CT output", &[n_tokens, rows], 2)?;
        let input_bytes =
            checked_buffer_bytes("decode NVFP4 CT input", &[n_tokens, weights.cols], 2)?;
        let output_end = y_byte_offset.checked_add(output_bytes).ok_or_else(|| {
            ForgeError::Kernel("decode NVFP4 CT: przepełnienie offsetu wyjścia".into())
        })?;
        let input_end = x_byte_offset.checked_add(input_bytes).ok_or_else(|| {
            ForgeError::Kernel("decode NVFP4 CT: przepełnienie offsetu wejścia".into())
        })?;
        if output_end > y.len() || input_end > x.len() {
            return Err(ForgeError::Kernel(
                "decode NVFP4 CT: offset wykracza poza bufor".into(),
            ));
        }
        let _ = validate_nvfp4_ct_b1_extents(
            output_bytes,
            input_bytes,
            weights.rows,
            weights.cols,
            source_row_offset,
            rows,
            inv_global_scale,
        )?;
        if rows == 0
            || !rows.is_multiple_of(64)
            || !source_row_offset.is_multiple_of(64)
            || !inv_global_scale.is_finite()
        {
            return Err(ForgeError::Kernel(
                "decode NVFP4 CT wymaga wyrównanego okna N64 i skończonej skali".into(),
            ));
        }
        let source_end = source_row_offset.checked_add(rows).ok_or_else(|| {
            ForgeError::Kernel("decode NVFP4 CT: przepełnienie zakresu".into())
        })?;
        if source_end > weights.rows {
            return Err(ForgeError::Kernel(
                "decode NVFP4 CT: okno lub bufor nie pasuje do widoku".into(),
            ));
        }
        let grid_x = u32::try_from(rows.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("decode NVFP4 CT: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_byte_offset)?
            .buf(weights.buffer)
            .buf_at(x, x_byte_offset)?
            .scalar(weights.cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64)
            .scalar(source_row_offset as i64)
            .scalar(inv_global_scale);
        debug_assert!(n_tokens <= bucket);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Mnoży prefill F16 bezpośrednio z naturalnego układu S0.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_ct_s0_f16_at(
        &self,
        y: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        x: &DevBuffer,
        source_row_offset: usize,
        rows: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
        {
            return Err(ForgeError::Unsupported(
                "prefill NVFP4 CT wymaga NVIDIA warp32".into(),
            ));
        }
        if rows == 0
            || n_tokens == 0
            || !rows.is_multiple_of(64)
            || !source_row_offset.is_multiple_of(64)
            || !weights.cols.is_multiple_of(128)
            || !inv_global_scale.is_finite()
        {
            return Err(ForgeError::Kernel(
                "prefill NVFP4 CT wymaga okna N64, K128 i skończonej skali".into(),
            ));
        }
        let source_end = source_row_offset.checked_add(rows).ok_or_else(|| {
            ForgeError::Kernel("prefill NVFP4 CT: przepełnienie zakresu".into())
        })?;
        if source_end > weights.rows {
            return Err(ForgeError::Kernel(
                "prefill NVFP4 CT: okno wykracza poza widok wag".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("prefill NVFP4 CT output", &[n_tokens, rows], 2)?;
        let input_bytes =
            checked_buffer_bytes("prefill NVFP4 CT input", &[n_tokens, weights.cols], 2)?;
        if y.len() < output_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "prefill NVFP4 CT: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let (kernel_name, block, bm) = if rows >= 8192 && n_tokens <= 256 {
            ("gemm_nvfp4_ct_s0_f16_bm128", 256, 128)
        } else {
            ("gemm_nvfp4_ct_s0_f16_bm64", 128, 64)
        };
        let grid_x = u32::try_from(rows.div_ceil(64))
            .map_err(|_| ForgeError::Kernel("prefill NVFP4 CT: grid.x przekracza u32".into()))?;
        let grid_y = u32::try_from(n_tokens.div_ceil(bm))
            .map_err(|_| ForgeError::Kernel("prefill NVFP4 CT: grid.y przekracza u32".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights.buffer)
            .buf(x)
            .scalar(weights.cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64)
            .scalar(source_row_offset as i64)
            .scalar(inv_global_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Uruchamia projekcję logicznego M na fizycznym kaflu BM16 lub BM32.
    /// Wiersze aktywacji powyżej logicznego M są zerowane w kernelu, więc
    /// bufory muszą mieć pełną fizyczną pojemność kafla.
    pub fn gemm_nvfp4_ct_padded(
        &self,
        y: &DevBuffer,
        workspace: Option<&DevBuffer>,
        weights: Nvfp4CtS0View<'_>,
        x_padded: &DevBuffer,
        logical_m: usize,
        projection: Nvfp4CtProjection,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
        {
            return Err(ForgeError::Unsupported(
                "NVFP4 CT direct wymaga NVIDIA warp32 i bloku 256".into(),
            ));
        }
        if !inv_global_scale.is_finite() {
            return Err(ForgeError::Kernel(
                "NVFP4 CT direct wymaga skończonej skali".into(),
            ));
        }
        let physical_m = nvfp4_ct_physical_m(logical_m).ok_or_else(|| {
            ForgeError::Kernel(format!(
                "NVFP4 CT direct obsługuje logiczne M4/M8/M16/M24/M32; otrzymano M{logical_m}"
            ))
        })?;
        let kernel_name = projection.kernel_name(logical_m).ok_or_else(|| {
            ForgeError::Kernel(format!(
                "NVFP4 CT direct nie ma kernela dla M{logical_m}"
            ))
        })?;
        let (rows, cols, parts) = projection.dims();
        let (row_tile, block_threads) = projection.launch_shape(physical_m);
        let pipeline_stages = projection.pipeline_stages(physical_m);
        if !nvfp4_ct_split_pipeline_supported(cols / 128, parts, pipeline_stages) {
            return Err(ForgeError::Kernel(
                "NVFP4 CT direct: split-K jest krótszy od potoku cp.async".into(),
            ));
        }
        if weights.rows != rows || weights.cols != cols {
            return Err(ForgeError::Kernel(format!(
                "NVFP4 CT direct: widok {}x{} nie pasuje do projekcji {rows}x{cols}",
                weights.rows, weights.cols
            )));
        }
        let output_bytes = checked_buffer_bytes("NVFP4 CT direct output", &[physical_m, rows], 2)?;
        let input_bytes = checked_buffer_bytes("NVFP4 CT direct input", &[physical_m, cols], 2)?;
        if y.len() < output_bytes || x_padded.len() < input_bytes {
            return Err(ForgeError::Kernel(format!(
                "NVFP4 CT direct wymaga pełnych buforów wejścia i wyjścia M{physical_m}"
            )));
        }
        let target = if parts == 1 {
            if workspace.is_some() {
                return Err(ForgeError::Kernel(
                    "NVFP4 CT direct gate+up nie używa workspace".into(),
                ));
            }
            y
        } else {
            let workspace = workspace.ok_or_else(|| {
                ForgeError::Kernel("NVFP4 CT direct split-K wymaga workspace FP32".into())
            })?;
            let workspace_bytes =
                checked_buffer_bytes("NVFP4 CT direct workspace", &[parts, physical_m, rows], 4)?;
            if workspace.len() < workspace_bytes {
                return Err(ForgeError::Kernel(
                    "NVFP4 CT direct: workspace split-K jest za mały".into(),
                ));
            }
            workspace
        };
        let grid_x = rows
            .div_ceil(row_tile)
            .checked_mul(parts)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("NVFP4 CT direct: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(target)
            .buf(weights.buffer)
            .buf(x_padded)
            .scalar(inv_global_scale);
        self.device.launch(kernel, &config, &args, stream)?;
        if parts == 1 {
            return Ok(());
        }
        let reduce = self.artifacts.get("reduce_nvfp4_ct_bm16")?;
        let elements = rows.checked_mul(physical_m).ok_or_else(|| {
            ForgeError::Kernel("NVFP4 CT direct: liczba wyników przekracza usize".into())
        })?;
        let reduce_grid = u32::try_from(elements.div_ceil(BLOCK as usize))
            .map_err(|_| ForgeError::Kernel("NVFP4 CT direct: redukcja przekracza u32".into()))?;
        let reduce_config = LaunchConfig {
            grid: (reduce_grid, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let reduce_args = LaunchArgs::new()
            .buf(y)
            .buf(target)
            .scalar(rows as i64)
            .scalar(physical_m as i64)
            .scalar(parts as i64);
        self.device
            .launch(reduce, &reduce_config, &reduce_args, stream)
    }

    /// Mnożenie macierz-wektor bezpośrednio z bloków GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_f16 wymaga rows > 0 i cols % 64 == 0, otrzymano rows={rows}, cols={cols}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[rows], 2)?;
        let weight_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[cols], 2)?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("gemv_nvfp4_gguf_f16: siatka przekracza u32".into()))?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("gemv_nvfp4_gguf_f16")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols as i64)
            .scalar(output_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wykonuje pojedynczą projekcję F16 tą samą matematyką co NVIDIA B3/B4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_b1_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia) || caps.warp_size != 32 {
            return Err(ForgeError::Unsupported(
                "gemv_nvfp4_gguf_b1_f16 wymaga NVIDIA z warpem 32".into(),
            ));
        }
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_b1_f16 wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 output", &[rows], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 input", &[cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_b1_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("gemm_nvfp4_gguf_f16_b1_nvidia")?;
        let grid_x = u32::try_from(rows.div_ceil(2)).map_err(|_| {
            ForgeError::Kernel("gemv_nvfp4_gguf_b1_f16: siatka przekracza u32".into())
        })?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(1i64)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Liczy draftowe logity F32 bezpośrednio z bloków GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_out_f32(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_out_f32 wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_out_f32 output", &[rows], 4)?;
        let weight_bytes =
            checked_buffer_bytes("gemv_nvfp4_gguf_out_f32 weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_out_f32 input", &[cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let caps = self.device.caps();
        let nvidia = matches!(caps.vendor, forge_types::Vendor::Nvidia) && caps.warp_size == 32;
        let name = if nvidia {
            "gemm_nvfp4_gguf_out_f32_b1_nvidia"
        } else {
            "gemv_nvfp4_gguf_out_f32"
        };
        let grid_x = u32::try_from(if nvidia { rows.div_ceil(2) } else { rows }).map_err(|_| {
            ForgeError::Kernel("gemv_nvfp4_gguf_out_f32: siatka przekracza u32".into())
        })?;
        let kernel = self.artifacts.get(name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (if nvidia { 64 } else { 256 }, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols as i64);
        let args = if nvidia {
            args.scalar(rows as i64).scalar(1i64).scalar(output_scale)
        } else {
            args.scalar(output_scale)
        };
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kwantyzuje aktywację raz do Q8_1 i wykonuje grupę projekcji GGUF NVFP4
    /// przez dp4a. Q/K/V oraz gate/up mogą współdzielić ten sam prepass.
    pub fn gemv_nvfp4_gguf_q8_1_group_f16(
        &self,
        projections: &[Nvfp4GgufQ8Projection<'_>],
        x: &DevBuffer,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemv_nvfp4_gguf_q8_1_group_layout_f16(
            projections,
            x,
            cols,
            Nvfp4GgufLayout::RowMajor36,
            stream,
        )
    }

    /// Kwantyzuje aktywację raz i uruchamia decode zgodny z jawnym układem wag.
    pub fn gemv_nvfp4_gguf_q8_1_group_layout_f16(
        &self,
        projections: &[Nvfp4GgufQ8Projection<'_>],
        x: &DevBuffer,
        cols: usize,
        layout: Nvfp4GgufLayout,
        stream: &Stream,
    ) -> Result<()> {
        if projections.is_empty() || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_q8_1 wymaga projekcji i cols % 64 == 0, otrzymano projekcji={}, cols={cols}",
                projections.len()
            )));
        }
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_q8_1 input", &[cols], 2)?;
        if x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_q8_1: bufor wejścia jest za mały".into(),
            ));
        }
        for projection in projections {
            if projection.rows == 0 || !projection.output_scale.is_finite() {
                return Err(ForgeError::Kernel(
                    "gemv_nvfp4_gguf_q8_1 wymaga rows > 0 i skończonej skali".into(),
                ));
            }
            let output_bytes =
                checked_buffer_bytes("gemv_nvfp4_gguf_q8_1 output", &[projection.rows], 2)?;
            let weight_bytes = checked_buffer_bytes(
                "gemv_nvfp4_gguf_q8_1 weights",
                &[projection.rows, cols / 64],
                36,
            )?;
            if projection.output.len() < output_bytes || projection.weights.len() < weight_bytes {
                return Err(ForgeError::Kernel(
                    "gemv_nvfp4_gguf_q8_1: bufor projekcji jest za mały".into(),
                ));
            }
        }
        let caps = self.device.caps();
        let tile_layout = layout == Nvfp4GgufLayout::TileN128K64;
        if tile_layout
            && projections
                .iter()
                .any(|projection| !projection.rows.is_multiple_of(128))
        {
            return Err(ForgeError::Kernel(
                "decode NVFP4 TileN128K64 wymaga rows % 128 == 0".into(),
            ));
        }
        if tile_layout
            && (!raw_nvfp4_dp4a_supported(
                matches!(caps.vendor, forge_types::Vendor::Nvidia),
                caps.warp_size,
            ) || !self.artifacts.has("gemv_nvfp4_tile128_coop_q8_1_f16"))
        {
            return Err(ForgeError::Unsupported(
                "decode NVFP4 TileN128K64 wymaga NVIDIA warp32 i kernela tile128".into(),
            ));
        }
        if !tile_layout
            && !raw_nvfp4_dp4a_supported(
                matches!(caps.vendor, forge_types::Vendor::Nvidia),
                caps.warp_size,
            )
        {
            for projection in projections {
                self.gemv_nvfp4_gguf_f16(
                    projection.output,
                    projection.weights,
                    x,
                    projection.rows,
                    cols,
                    projection.output_scale,
                    stream,
                )?;
            }
            return Ok(());
        }
        let need_codes = cols;
        let need_blocks = cols / 32;
        let mut scratch = self.prequant.lock().expect("prequant scratch poisoned");
        if scratch.cap_codes < need_codes {
            scratch.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            scratch.cap_codes = need_codes;
        }
        if scratch.cap_blocks < need_blocks {
            scratch.xd = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.xsm = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.cap_blocks = need_blocks;
        }
        let xq = scratch.xq.as_ref().expect("xq zaalokowane");
        let xd = scratch.xd.as_ref().expect("xd zaalokowane");
        let xsm = scratch.xsm.as_ref().expect("xsm zaalokowane");
        let quant = self.artifacts.get("quantize_act_q8_1")?;
        let quant_cfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let quant_args = LaunchArgs::new()
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .buf(x)
            .scalar(cols as i64)
            .scalar(1i64);
        self.device.launch(quant, &quant_cfg, &quant_args, stream)?;

        let kernel_name = match layout {
            Nvfp4GgufLayout::RowMajor36 => "gemv_nvfp4_gguf_q8_1_f16",
            Nvfp4GgufLayout::TileN128K64 => "gemv_nvfp4_tile128_coop_q8_1_f16",
        };
        let kernel = self.artifacts.get(kernel_name)?;
        for projection in projections {
            let rows_per_block = if tile_layout { 4 } else { 8 };
            let grid_x = u32::try_from(projection.rows.div_ceil(rows_per_block)).map_err(|_| {
                ForgeError::Kernel("gemv_nvfp4_gguf_q8_1: siatka przekracza u32".into())
            })?;
            let config = LaunchConfig {
                grid: (grid_x, 1, 1),
                block: (if tile_layout { 128 } else { BLOCK }, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(projection.output)
                .buf(projection.weights)
                .buf(xq)
                .buf(xd)
                .scalar(cols as i64)
                .scalar(projection.rows as i64)
                .scalar(projection.output_scale);
            self.device.launch(kernel, &config, &args, stream)?;
        }
        Ok(())
    }

    /// Przepakowuje pełną macierz GGUF NVFP4 do układu TileN128K64 na GPU.
    pub fn repack_nvfp4_gguf_tile_n128_k64(
        &self,
        target: &DevBuffer,
        source: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
        {
            return Err(ForgeError::Unsupported(
                "repack NVFP4 TileN128K64 wymaga NVIDIA warp32".into(),
            ));
        }
        if rows == 0 || cols < 64 || !rows.is_multiple_of(128) || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "repack NVFP4 TileN128K64 wymaga rows % 128 == 0 i cols % 64 == 0; rows={rows}, cols={cols}"
            )));
        }
        let blocks_per_row = cols / 64;
        let bytes = checked_buffer_bytes("repack NVFP4 TileN128K64", &[rows, blocks_per_row], 36)?;
        if target.len() < bytes || source.len() < bytes {
            return Err(ForgeError::Kernel(
                "repack NVFP4 TileN128K64 ma za mały bufor".into(),
            ));
        }
        let stages = rows
            .checked_div(128)
            .and_then(|tiles| tiles.checked_mul(blocks_per_row))
            .and_then(|blocks| blocks.checked_mul(2))
            .ok_or_else(|| ForgeError::Kernel("repack NVFP4: przepełnienie siatki".into()))?;
        let grid_x = u32::try_from(stages)
            .map_err(|_| ForgeError::Kernel("repack NVFP4: siatka przekracza u32".into()))?;
        let blocks_per_row = i64::try_from(blocks_per_row)
            .map_err(|_| ForgeError::Kernel("repack NVFP4: K przekracza i64".into()))?;
        let kernel = self.artifacts.get("nvfp4_repack_tile128")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(target)
            .buf(source)
            .scalar(blocks_per_row);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kafelkowane mnożenie wielu tokenów bezpośrednio z bloków GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_nvfp4_gguf_layout_f16(
            y,
            weights,
            x,
            rows,
            cols,
            n_tokens,
            output_scale,
            Nvfp4GgufLayout::RowMajor36,
            stream,
        )
    }

    /// Kafelkowane mnożenie NVFP4 wybierane wyłącznie przez jawny układ wag.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_layout_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        output_scale: f32,
        layout: Nvfp4GgufLayout,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0
            || n_tokens == 0
            || cols < 64
            || !cols.is_multiple_of(64)
            || !output_scale.is_finite()
        {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_f16 wymaga rows > 0, cols % 64 == 0 i skończonej skali; otrzymano rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let caps = self.device.caps();
        let dispatch = nvfp4_gguf_layout_dispatch(
            layout,
            n_tokens,
            rows,
            cols,
            self.artifacts.has("gemm_nvfp4_gguf_mma_f16_bm128_prefetch"),
            self.artifacts
                .has("gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1"),
            self.artifacts.has("gemm_nvfp4_gguf_mma_f16_bm128_bn128"),
            self.artifacts.has("gemm_nvfp4_tile128_mma_f16_bm128_bn64"),
            self.artifacts.has("gemm_nvfp4_tile128_mma_f16_bm128_bn128"),
            matches!(caps.vendor, forge_types::Vendor::Nvidia),
            caps.warp_size,
            caps.max_threads_per_block,
        )?;
        let output_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_f16 output", &[n_tokens, rows], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_f16 weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemm_nvfp4_gguf_f16 input", &[n_tokens, cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(rows.div_ceil(dispatch.row_tile))
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: grid.x przekracza u32".into()))?;
        let grid_y = u32::try_from(n_tokens.div_ceil(dispatch.token_tile))
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: grid.y przekracza u32".into()))?;
        let rows = i64::try_from(rows)
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: rows przekracza i64".into()))?;
        let cols = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: cols przekracza i64".into()))?;
        let n_tokens = i64::try_from(n_tokens).map_err(|_| {
            ForgeError::Kernel("gemm_nvfp4_gguf_f16: liczba tokenów przekracza i64".into())
        })?;
        let kernel = self.artifacts.get(dispatch.kernel)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (dispatch.block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols)
            .scalar(rows)
            .scalar(n_tokens)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Liczy dwa wiersze logitów F32 bez dekwantyzacji głowy GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_out_f32_b2(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_nvfp4_gguf_out_f32_batch(y, weights, x, rows, cols, 2, output_scale, stream)
    }

    /// Liczy B4/B8/B16 wierszy logitów F32 jednym przebiegiem po wagach NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_out_f32_batch(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_out_f32_batch wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        if !matches!(n_tokens, 2 | 4 | 8 | 16) {
            return Err(ForgeError::Kernel(format!(
                "batch logits NVFP4 wymaga B=2/4/8/16, otrzymano {n_tokens}"
            )));
        }
        let output_bytes = checked_buffer_bytes("NVFP4 batch logits", &[n_tokens, rows], 4)?;
        let weight_bytes =
            checked_buffer_bytes("NVFP4 batch logits weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("NVFP4 batch logits input", &[n_tokens, cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_out_f32_batch: bufor jest za mały".into(),
            ));
        }
        let warp_size = self.device.caps().warp_size;
        if warp_size == 0 || warp_size > self.device.caps().max_threads_per_block {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_out_f32_batch: nieprawidłowy rozmiar wave".into(),
            ));
        }
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("NVFP4 B2 logits grid przekracza u32".into()))?;
        let rows = i64::try_from(rows)
            .map_err(|_| ForgeError::Kernel("NVFP4 B2 logits rows przekracza i64".into()))?;
        let cols = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("NVFP4 B2 logits cols przekracza i64".into()))?;
        let kernel = self
            .artifacts
            .get(&format!("gemm_nvfp4_gguf_out_f32_b{n_tokens}"))?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (warp_size, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols)
            .scalar(rows)
            .scalar(i64::try_from(n_tokens).expect("B logits NVFP4 jest małe"))
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Scalone przygotowanie wejścia MTP i projekcja Q8_0 z 2H do H.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_prepare_f16(
        &self,
        output: &DevBuffer,
        embedding_row: &DevBuffer,
        target_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        eh_proj: &DevBuffer,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if hidden_size == 0
            || hidden_size > 5120
            || !(2 * hidden_size).is_multiple_of(32)
            || !eps.is_finite()
            || eps <= 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "mtp_prepare_f16 wymaga 0 < H <= 5120, 2H % 32 == 0 i eps > 0; otrzymano H={hidden_size}, eps={eps}"
            )));
        }
        let output_bytes = checked_buffer_bytes("mtp_prepare_f16 output", &[hidden_size], 2)?;
        let vector_bytes = checked_buffer_bytes("mtp_prepare_f16 vector", &[hidden_size], 2)?;
        let projection_bytes = checked_buffer_bytes(
            "mtp_prepare_f16 eh_proj",
            &[hidden_size, (2 * hidden_size) / 32],
            34,
        )?;
        if output.len() < output_bytes
            || embedding_row.len() < vector_bytes
            || target_hidden.len() < vector_bytes
            || enorm.len() < vector_bytes
            || hnorm.len() < vector_bytes
            || eh_proj.len() < projection_bytes
        {
            return Err(ForgeError::Kernel(
                "mtp_prepare_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(hidden_size.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("mtp_prepare_f16: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get("mtp_prepare_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embedding_row)
            .buf(target_hidden)
            .buf(enorm)
            .buf(hnorm)
            .buf(eh_proj)
            .scalar(hidden_size as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Normalizuje batch embeddingów i przesuniętych hidden targetu przed eh_proj.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_norm_join_shifted_f16(
        &self,
        output: &DevBuffer,
        embeddings: &DevBuffer,
        target_hidden: &DevBuffer,
        initial_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        n_tokens: usize,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if n_tokens == 0 || hidden_size == 0 || !eps.is_finite() || eps <= 0.0 {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_f16 wymaga dodatnich wymiarów i eps".into(),
            ));
        }
        let rows = checked_buffer_bytes("mtp shifted rows", &[n_tokens, hidden_size], 2)?;
        let output_bytes =
            checked_buffer_bytes("mtp shifted output", &[n_tokens, 2, hidden_size], 2)?;
        let vector = checked_buffer_bytes("mtp shifted vector", &[hidden_size], 2)?;
        if output.len() < output_bytes
            || embeddings.len() < rows
            || target_hidden.len() < rows
            || initial_hidden.len() < vector
            || enorm.len() < vector
            || hnorm.len() < vector
        {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_norm_join_shifted_f16")?;
        let config = LaunchConfig {
            grid: (
                u32::try_from(n_tokens).map_err(|_| {
                    ForgeError::Kernel("mtp shifted: liczba tokenów przekracza u32".into())
                })?,
                1,
                1,
            ),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embeddings)
            .buf(target_hidden)
            .buf(initial_hidden)
            .buf(enorm)
            .buf(hnorm)
            .scalar(n_tokens as i64)
            .scalar(hidden_size as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Normalizuje `[B,T]` z osobnym początkowym hidden dla każdego lane.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_norm_join_shifted_segmented_f16(
        &self,
        output: &DevBuffer,
        embeddings: &DevBuffer,
        target_hidden: &DevBuffer,
        initial_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_tokens == 0 || hidden_size == 0 || !eps.is_finite() || eps <= 0.0 {
            return Err(ForgeError::Kernel(
                "segmentowany MTP join wymaga dodatnich wymiarów i eps".into(),
            ));
        }
        let total = batch.checked_mul(n_tokens).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie liczby tokenów segmentowanego MTP join".into())
        })?;
        let rows = checked_buffer_bytes("mtp segmented shifted rows", &[total, hidden_size], 2)?;
        let output_bytes =
            checked_buffer_bytes("mtp segmented shifted output", &[total, 2, hidden_size], 2)?;
        let initial_bytes =
            checked_buffer_bytes("mtp segmented shifted initial", &[batch, hidden_size], 2)?;
        let vector = checked_buffer_bytes("mtp segmented shifted vector", &[hidden_size], 2)?;
        if output.len() < output_bytes
            || embeddings.len() < rows
            || target_hidden.len() < rows
            || initial_hidden.len() < initial_bytes
            || enorm.len() < vector
            || hnorm.len() < vector
        {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_segmented_f16: bufor jest mniejszy od wymaganego kształtu"
                    .into(),
            ));
        }
        let grid = u32::try_from(total).map_err(|_| {
            ForgeError::Kernel("segmentowany MTP join przekracza siatkę u32".into())
        })?;
        let kernel = self.artifacts.get("mtp_norm_join_shifted_segmented_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embeddings)
            .buf(target_hidden)
            .buf(initial_hidden)
            .buf(enorm)
            .buf(hnorm)
            .scalar(i64::try_from(batch).map_err(|_| {
                ForgeError::Kernel("batch segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(i64::try_from(n_tokens).map_err(|_| {
                ForgeError::Kernel("T segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(i64::try_from(hidden_size).map_err(|_| {
                ForgeError::Kernel("hidden segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Projektuje złączony batch przez Q8_0 zgodnie z redukcją mtp_prepare.
    pub fn mtp_project_joined_q8_f16(
        &self,
        output: &DevBuffer,
        joined: &DevBuffer,
        weights: &DevBuffer,
        n_tokens: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let output_bytes = checked_buffer_bytes("mtp project output", &[n_tokens, hidden_size], 2)?;
        let joined_bytes =
            checked_buffer_bytes("mtp project joined", &[n_tokens, 2, hidden_size], 2)?;
        let weights_bytes = checked_buffer_bytes(
            "mtp project weights",
            &[hidden_size, (2 * hidden_size) / 32],
            34,
        )?;
        if n_tokens == 0
            || !(2 * hidden_size).is_multiple_of(32)
            || output.len() < output_bytes
            || joined.len() < joined_bytes
            || weights.len() < weights_bytes
        {
            return Err(ForgeError::Kernel(
                "mtp_project_joined_q8_f16: nieprawidłowy kształt".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_project_joined_q8_f16")?;
        let config = LaunchConfig {
            grid: ((hidden_size as u32).div_ceil(8), n_tokens as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(joined)
            .buf(weights)
            .scalar(hidden_size as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Ustawia metadane kroku MTP i opcjonalnie mapowanie nowej strony KV.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_stage_step(
        &self,
        position_out: &DevBuffer,
        seq_len_out: &DevBuffer,
        page_table: &DevBuffer,
        position: usize,
        seq_len: usize,
        logical_page: Option<usize>,
        physical_page: Option<i32>,
        stream: &Stream,
    ) -> Result<()> {
        if position_out.len() < 4 || seq_len_out.len() < 4 {
            return Err(ForgeError::Kernel(
                "mtp_stage_step wymaga 4-bajtowych buforów metadanych".into(),
            ));
        }
        let (logical_page, physical_page) = match (logical_page, physical_page) {
            (Some(logical), Some(physical)) if physical >= 0 => {
                let byte_end = logical
                    .checked_add(1)
                    .and_then(|entries| entries.checked_mul(4))
                    .ok_or_else(|| {
                        ForgeError::Kernel("mtp_stage_step: przepełnienie indeksu strony".into())
                    })?;
                if byte_end > page_table.len() {
                    return Err(ForgeError::Kernel(format!(
                        "mtp_stage_step: strona logiczna {logical} wykracza poza page table"
                    )));
                }
                (
                    i64::try_from(logical).map_err(|_| {
                        ForgeError::Kernel("mtp_stage_step: indeks strony przekracza i64".into())
                    })?,
                    i64::from(physical),
                )
            }
            (None, None) => (-1, -1),
            _ => {
                return Err(ForgeError::Kernel(
                    "mtp_stage_step wymaga kompletnej pary stron logiczna/fizyczna".into(),
                ));
            }
        };
        let position = i64::try_from(position)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: pozycja przekracza i64".into()))?;
        let seq_len = i64::try_from(seq_len)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: długość przekracza i64".into()))?;
        let kernel = self.artifacts.get("mtp_stage_step")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(position_out)
            .buf(seq_len_out)
            .buf(page_table)
            .scalar(position)
            .scalar(seq_len)
            .scalar(logical_page)
            .scalar(physical_page);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zapisuje końcowe metadane MTP dla niezależnych decyzji lane.
    pub fn mtp_commit_catchup_metadata_segmented(
        &self,
        seq_lens_out: &DevBuffer,
        positions_out: &DevBuffer,
        base_positions: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        stream: &Stream,
    ) -> Result<()> {
        let bytes = batch
            .checked_mul(4)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie metadanych catch-up MTP".into()))?;
        let decision_bytes = batch.checked_mul(8).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie decyzji metadanych catch-up MTP".into())
        })?;
        if batch == 0
            || seq_lens_out.len() < bytes
            || positions_out.len() < bytes
            || base_positions.len() < bytes
            || decisions.len() < decision_bytes
        {
            return Err(ForgeError::Kernel(
                "segmentowane metadane catch-up MTP mają nieprawidłowy kształt".into(),
            ));
        }
        let grid = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("batch metadanych MTP przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("mtp_commit_catchup_metadata_segmented")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(seq_lens_out)
            .buf(positions_out)
            .buf(base_positions)
            .buf(decisions);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje staged embedding row z dedykowanej tabeli F16 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_f16_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 {
            return Err(ForgeError::Kernel(
                "gather_f16_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("gather_f16_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gather_f16_row_f16 weights", &[vocab_size, hidden_size], 2)?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_f16_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_f16_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje staged embedding row z tied Q8_0 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_q8_0_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(32) {
            return Err(ForgeError::Kernel(
                "gather_q8_0_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("gather_q8_0_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_q8_0_row_f16 weights",
            &[vocab_size, hidden_size / 32],
            34,
        )?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_q8_0_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_q8_0_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje staged embedding row z tied GGUF NVFP4 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_nvfp4_gguf_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(64) {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gather_nvfp4_gguf_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_nvfp4_gguf_row_f16 weights",
            &[vocab_size, hidden_size / 64],
            36,
        )?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_nvfp4_gguf_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Pakuje GPU-resident drafty dwóch lane'ów oraz metadane target verifiera.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_pack_verify_inputs(
        &self,
        ids_out: &DevBuffer,
        positions_out: &DevBuffer,
        visible_out: &DevBuffer,
        lane0_ids: &DevBuffer,
        lane1_ids: &DevBuffer,
        base_positions: &DevBuffer,
        steps: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !(3..=4).contains(&steps) {
            return Err(ForgeError::Kernel(format!(
                "mtp_pack_verify_inputs wymaga T=3 lub T=4, otrzymano {steps}"
            )));
        }
        let total = steps.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("mtp_pack_verify_inputs: przepełnienie liczby ID".into())
        })?;
        let bytes = checked_buffer_bytes("mtp_pack_verify_inputs output", &[total], 4)?;
        if ids_out.len() < bytes
            || positions_out.len() < bytes
            || visible_out.len() < bytes
            || lane0_ids.len() < steps * 4
            || lane1_ids.len() < steps * 4
            || base_positions.len() < 8
        {
            return Err(ForgeError::Kernel(
                "mtp_pack_verify_inputs: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_pack_verify_inputs")?;
        let config = LaunchConfig::linear(total as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(ids_out)
            .buf(positions_out)
            .buf(visible_out)
            .buf(lane0_ids)
            .buf(lane1_ids)
            .buf(base_positions)
            .scalar(steps as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje batch wierszy target embeddingu Q8_0 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_q8_0_rows_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        ids: &DevBuffer,
        rows: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(32) {
            return Err(ForgeError::Kernel(
                "gather_q8_0_rows_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gather_q8_0_rows_f16 output", &[rows, hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_q8_0_rows_f16 weights",
            &[vocab_size, hidden_size / 32],
            34,
        )?;
        if output.len() < output_bytes || weights.len() < weight_bytes || ids.len() < rows * 4 {
            return Err(ForgeError::Kernel(
                "gather_q8_0_rows_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_q8_0_rows_f16")?;
        let config = LaunchConfig {
            grid: ((hidden_size as u32).div_ceil(BLOCK), rows as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(ids)
            .scalar(rows as i64)
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje batch wierszy target embeddingu GGUF NVFP4 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_nvfp4_gguf_rows_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        ids: &DevBuffer,
        rows: usize,
        vocab_size: usize,
        hidden_size: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(64) {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_rows_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gather_nvfp4_gguf_rows_f16 output", &[rows, hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_nvfp4_gguf_rows_f16 weights",
            &[vocab_size, hidden_size / 64],
            36,
        )?;
        if output.len() < output_bytes || weights.len() < weight_bytes || ids.len() < rows * 4 {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_rows_f16: zbyt mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let nvidia = matches!(caps.vendor, forge_types::Vendor::Nvidia) && caps.warp_size == 32;
        let (name, elements, block) = if nvidia {
            ("gather_nvfp4_gguf_rows_f16_nvidia", hidden_size / 2, 128u32)
        } else {
            ("gather_nvfp4_gguf_rows_f16", hidden_size, BLOCK)
        };
        let kernel = self.artifacts.get(name)?;
        let config = LaunchConfig {
            grid: ((elements as u32).div_ceil(block), rows as u32, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(ids)
            .scalar(rows as i64)
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// out[t] = table[ids[t]] — token embedding gather (f16 rows).
    pub fn gather_rows_f16(
        &self,
        out: &DevBuffer,
        table: &DevBuffer,
        ids: &DevBuffer,
        n_tokens: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let row_bytes = checked_buffer_bytes("gather_rows_f16 row", &[cols], 2)?;
        let output_bytes = checked_buffer_bytes("gather_rows_f16 output", &[n_tokens, cols], 2)?;
        let ids_bytes = checked_buffer_bytes("gather_rows_f16 ids", &[n_tokens], 4)?;
        if n_tokens == 0
            || cols == 0
            || table.is_empty()
            || !table.len().is_multiple_of(row_bytes)
            || out.len() < output_bytes
            || ids.len() < ids_bytes
        {
            return Err(ForgeError::Kernel(
                "gather_rows_f16: nieprawidłowy kształt lub zbyt mały bufor".into(),
            ));
        }
        let rows = table.len() / row_bytes;
        let k = self.artifacts.get("gather_rows_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK.min(cols as u32).max(32), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(table)
            .buf(ids)
            .scalar(rows as i64)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV, f16 weights → f32 logits.
    pub fn gemv_f16_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logity FP32 z wag E4M3 oraz jednej skali FP32 na wiersz.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_fp8_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_fp8_out_f32 wymaga cols % 256 == 0, otrzymano {cols}"
            )));
        }
        let output_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wyjścia".into())
        })?;
        let weight_bytes = rows.checked_mul(cols).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wag".into())
        })?;
        let input_bytes = cols.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wejścia".into())
        })?;
        let grid_x = u32::try_from(rows.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("gemv_fp8_out_f32: siatka przekracza u32".into()))?;
        if y_f32.len() < output_bytes
            || w.len() < weight_bytes
            || scales.len() < output_bytes
            || x.len() < input_bytes
        {
            return Err(ForgeError::Kernel(
                "gemv_fp8_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("gemv_fp8_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q8_0 weights (tied embeddings) → f32 logits.
    pub fn gemv_q8_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q8_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q8_0_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[row] = layernorm(x[row]) * weight + bias.
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = layernorm(residual) * weight + bias (fused).
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Elementwise GELU (exact erf) over n f16 elements.
    pub fn gelu_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gelu_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// 1-D conv (kernel 3, pad 1) with fused optional GELU.
    /// x: [in_ch, in_t]; weight: [out_ch, in_ch, 3]; out: [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_k3_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        in_ch: usize,
        out_ch: usize,
        in_t: usize,
        out_t: usize,
        stride: usize,
        apply_gelu: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_k3_f16")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(128), out_ch as u32, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_t as i64)
            .scalar(stride as i64)
            .scalar(if apply_gelu { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Full (non-paged) attention over contiguous K/V; causal optional.
    /// q/out: [n_q, n_q_heads, hd]; k/v: [n_kv, n_kv_heads, hd].
    #[allow(clippy::too_many_arguments)]
    pub fn attn_full_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_buf: &DevBuffer,
        v_buf: &DevBuffer,
        n_q: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        n_kv: usize,
        causal: bool,
        q_offset: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_full_f16_hd64",
            128 => "attn_full_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_full: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_q as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_buf)
            .buf(v_buf)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(n_kv as i64)
            .scalar(if causal { 1i64 } else { 0i64 })
            .scalar(q_offset as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_f16` reading x at `x_byte_off` and writing y at `y_byte_off`.
    /// Sequence-shaped callers (Whisper encoder) launch one GEMV per position
    /// over the same stream instead of staging per-position copies.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_at(
        &self,
        y: &DevBuffer,
        y_byte_off: usize,
        w: &DevBuffer,
        x: &DevBuffer,
        x_byte_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_byte_off)?
            .buf(w)
            .buf_at(x, x_byte_off)?
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_f16_bias` reading x at `x_byte_off` and writing y at `y_byte_off`.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_bias_at(
        &self,
        y: &DevBuffer,
        y_byte_off: usize,
        w: &DevBuffer,
        x: &DevBuffer,
        x_byte_off: usize,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_bias")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_byte_off)?
            .buf(w)
            .buf_at(x, x_byte_off)?
            .buf(bias)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// f16 GEMV with per-row bias: y = W·x + b.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_bias(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_bias")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .buf(bias)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Scatter the current token's K/V rows ([n_kv_heads, head_dim]) into the
    /// paged cache at position seq_len[0]-1 (device-resident addressing —
    /// CUDA-graph-replay safe).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_f16")?;
        let cfg = LaunchConfig {
            grid: (n_kv_heads as u32, 1, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Pick the prefill GEMM tile for a (rows, n_tokens) shape. The BM=64
    /// instantiation doubles the token-block count, which wins everywhere
    /// except very tall matrices at short chunks where the BM=128 grid is
    /// already saturated (measured on RTX 4090, kernels/mojo benches).
    fn gemm_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if rows >= 8192 && n_tokens <= 256 {
            ("", 256, 128)
        } else {
            ("_bm64", 128, 64)
        }
    }

    fn f16_out_f32_dispatch(
        rows: usize,
        n_tokens: usize,
        mut has: impl FnMut(&str) -> bool,
    ) -> (&'static str, u32, u32) {
        if (2..=4).contains(&n_tokens) && has("gemv_batch_f16_out_f32_b4") {
            ("gemv_batch_f16_out_f32_b4", 256, n_tokens as u32)
        } else if (5..=8).contains(&n_tokens) && has("gemv_batch_f16_out_f32_b8") {
            ("gemv_batch_f16_out_f32_b8", 256, n_tokens as u32)
        } else if n_tokens <= 32 && has("gemm_f16_out_f32_bm32") {
            ("gemm_f16_out_f32_bm32", 64, 32)
        } else {
            match Self::gemm_tile(rows, n_tokens) {
                ("", block, bm) => ("gemm_f16_out_f32", block, bm),
                ("_bm64", block, bm) => ("gemm_f16_out_f32_bm64", block, bm),
                _ => unreachable!("gemm_tile zwraca wyłącznie wspierane suffixy"),
            }
        }
    }

    fn q8_0_out_f32_kernel(rows: usize, n_tokens: usize) -> &'static str {
        match Self::gemm_tile(rows, n_tokens).0 {
            "" => "gemm_q8_0_out_f32",
            "_bm64" => "gemm_q8_0_out_f32_bm64",
            _ => unreachable!("gemm_tile zwraca wyłącznie wspierane suffixy"),
        }
    }

    /// Kafel dla małego batcha NVFP4. BM32 zachowuje ten sam łańcuch MMA,
    /// ale nie wykonuje pustej drugiej połowy kafla BM64.
    fn gemm_nvfp4_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if (2..=32).contains(&n_tokens) {
            ("_bm32", 64, 32)
        } else {
            Self::gemm_tile(rows, n_tokens)
        }
    }

    /// Y[t, row] = W·x[t] over Q8_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q8_0_f16_at(y, w_q8, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q8_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16_at(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q8_0_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over NVFP4 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_nvfp4_f16_at(
            y,
            packed,
            0,
            scales,
            0,
            x,
            rows,
            cols,
            n_tokens,
            inv_global_scale,
            stream,
        )
    }

    /// `gemm_nvfp4_f16` over a row window of a fused weight matrix; packed
    /// nibbles and FP8 block scales are separate streams, so the window needs
    /// a byte offset into each.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_f16_at(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        packed_byte_off: usize,
        scales: &DevBuffer,
        scales_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 16 || !cols.is_multiple_of(16) || n_tokens == 0 {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4 requires rows > 0, cols >= 16, cols % 16 == 0 and n_tokens > 0, got rows={rows}, cols={cols}, n_tokens={n_tokens}"
            )));
        }
        // Karty bez jednostki macierzowej: kafel int8 na `v_dot4_i32_i8`.
        // Wartości e2m1 są wielokrotnościami 0,5, więc podwojone są całkowite i
        // iloczyn wychodzi dokładnie — patrz `nvfp4_codes8`.
        if let Some(tile) = self.gemm_nvfp4_dot4_tile(rows, cols, n_tokens) {
            let (xq, xd, _) = self.prequant_q8_1(x, cols, n_tokens, stream)?;
            let k = self.artifacts.get(tile.name)?;
            let args = LaunchArgs::new()
                .buf(y)
                .buf_at(packed, packed_byte_off)?
                .buf_at(scales, scales_byte_off)?
                .buf(&xq)
                .buf(&xd)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(n_tokens as i64)
                .scalar(inv_global_scale);
            return self
                .device
                .launch(k, &tile.config(rows, n_tokens), &args, stream);
        }
        let (kernel_name, block, bm) = if (2..=4).contains(&n_tokens)
            && self.artifacts.has("gemv_batch_nvfp4_f16_b4")
        {
            ("gemv_batch_nvfp4_f16_b4".to_string(), 256, n_tokens as u32)
        } else if (5..=8).contains(&n_tokens) && self.artifacts.has("gemv_batch_nvfp4_f16_b8") {
            ("gemv_batch_nvfp4_f16_b8".to_string(), 256, n_tokens as u32)
        } else if (9..=16).contains(&n_tokens) && self.artifacts.has("gemv_batch_nvfp4_f16_b16") {
            ("gemv_batch_nvfp4_f16_b16".to_string(), 256, n_tokens as u32)
        } else {
            let (mut suffix, mut block, mut bm) = Self::gemm_nvfp4_tile(rows, n_tokens);
            if !self.artifacts.has(&format!("gemm_nvfp4_f16{suffix}")) {
                (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
            }
            (format!("gemm_nvfp4_f16{suffix}"), block, bm)
        };
        let k = self.artifacts.get(&kernel_name)?;
        let cfg = LaunchConfig {
            grid: if kernel_name.starts_with("gemv_batch_") {
                ((rows as u32).div_ceil(8), 1, 1)
            } else {
                (
                    (rows as u32).div_ceil(64),
                    (n_tokens as u32).div_ceil(bm),
                    1,
                )
            },
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(packed, packed_byte_off)?
            .buf_at(scales, scales_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t], all f16, row-major activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_f16` over a row window of a fused weight matrix. The kernel's
    /// 16-byte weight loads require `w_byte_off % 16 == 0`, which
    /// row-aligned offsets satisfy for any cols % 8 == 0.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        // The kernel consumes the reduction dim in vectors of 8; a tail would
        // be silently dropped, so reject it loudly instead.
        if !cols.is_multiple_of(8) {
            return Err(ForgeError::Kernel(format!(
                "gemm_f16 requires cols % 8 == 0, got {cols}"
            )));
        }
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        // Karty bez jednostki macierzowej nie mają rodziny `gemm_f16` (opartej
        // na mma/ldmatrix) i idą kaflem na `v_dot2_f32_f16`.
        if let Some(tile) = self.gemm_dot2_tile(rows, n_tokens) {
            if std::env::var("FORGE_TRACE_ROUTE").is_ok() {
                eprintln!("ROUTE dot2 {} rows={rows} cols={cols} T={n_tokens}", tile.name);
            }
            let k = self.artifacts.get(tile.name)?;
            return self
                .device
                .launch(k, &tile.config(rows, n_tokens), &args, stream);
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kafel `gemm_f16_dot2` dla kart bez jednostki macierzowej, albo `None` na
    /// NVIDII (tam właściwa jest rodzina mma; ten kernel jest tam zbudowany,
    /// ale degraduje do dwóch FMA i służy tylko do porównań).
    ///
    /// Wybór kafla wynika ze zmierzonych na gfx1030 przepustowości (patrz
    /// `docs/STATUS.md`), a przy małej liczbie tokenów schodzimy na węższy kafel,
    /// bo pełny byłby w większości odrzuconym obliczeniem.
    fn gemm_dot2_tile(&self, rows: usize, n_tokens: usize) -> Option<DotTile> {
        if self.device.caps().vendor == forge_types::Vendor::Nvidia {
            return None;
        }
        Some(if n_tokens <= 64 || rows < 128 {
            DotTile::new("gemm_f16_dot2_64x64", 64, 64, 4, 4)
        } else if n_tokens <= 128 {
            DotTile::new("gemm_f16_dot2_128x64", 128, 64, 8, 4)
        } else if n_tokens >= 256 && rows >= 2048 {
            DotTile::new("gemm_f16_dot2_256x64", 256, 64, 8, 8)
        } else {
            DotTile::new("gemm_f16_dot2_128x128", 128, 128, 8, 8)
        })
    }

    /// f16 GEMM emitting f32 outputs over a row window of `w` (batched logit
    /// head). Same grid/tiling as `gemm_f16_at`; the f32 store preserves the
    /// mma accumulator precision so batched logits match the single-row
    /// gemv_*_out_f32 path.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 8 || !cols.is_multiple_of(8) || n_tokens == 0 {
            return Err(ForgeError::Kernel(format!(
                "gemm_f16_out_f32 requires rows > 0, cols >= 8, cols % 8 == 0 and n_tokens > 0, got rows={rows}, cols={cols}, n_tokens={n_tokens}"
            )));
        }
        // Karty bez jednostki macierzowej: kafel f16 na `v_dot2_f32_f16` z
        // zapisem f32. Wyspecjalizowane gemv batchowe (do 8 tokenów) mają
        // pierwszeństwo, bo nie liczą odrzucanych wierszy.
        if self.device.caps().vendor != forge_types::Vendor::Nvidia
            && n_tokens > 8
            && self.artifacts.has("gemm_f16_dot2_out_f32_64x64")
        {
            let tile = DotTile::new("gemm_f16_dot2_out_f32_64x64", 64, 64, 4, 4);
            if std::env::var("FORGE_TRACE_ROUTE").is_ok() {
                eprintln!("ROUTE dot2_f32 rows={rows} cols={cols} T={n_tokens}");
            }
            let k = self.artifacts.get(tile.name)?;
            let args = LaunchArgs::new()
                .buf(y_f32)
                .buf_at(w, w_byte_off)?
                .buf(x)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(n_tokens as i64);
            return self
                .device
                .launch(k, &tile.config(rows, n_tokens), &args, stream);
        }
        let (kernel_name, block, bm) =
            Self::f16_out_f32_dispatch(rows, n_tokens, |name| self.artifacts.has(name));
        let k = self.artifacts.get(kernel_name)?;
        let cfg = LaunchConfig {
            grid: if kernel_name.starts_with("gemv_batch_") {
                ((rows as u32).div_ceil(8), 1, 1)
            } else {
                (
                    (rows as u32).div_ceil(64),
                    (n_tokens as u32).div_ceil(bm),
                    1,
                )
            },
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q8_0 GEMM emitting f32 outputs (batched logit head).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (_, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self
            .artifacts
            .get(Self::q8_0_out_f32_kernel(rows, n_tokens))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched greedy argmax over `logits` ([n_seqs, vocab] f32): one block per
    /// sequence, ties to the lowest id. `out_ids` receives n_seqs i32 token ids.
    pub fn sample_batched_argmax_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("argmax_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(logits)
            .scalar(vocab as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched categorical draw over `logits` ([n_seqs, vocab] f32) with
    /// per-seq params (k / inv_temp / top_p / min_p / seed / step arrays, each
    /// n_seqs long). `out_ids` receives n_seqs i32 token ids. `logits` is
    /// mutated (top-k masking) — valid because it is regenerated every step.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_topk_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        k_arr: &DevBuffer,
        inv_t_arr: &DevBuffer,
        top_p_arr: &DevBuffer,
        min_p_arr: &DevBuffer,
        seed_arr: &DevBuffer,
        step_arr: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_batched_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        // Two passes mirroring the fast single-row path: per-chunk partial
        // top-k lists (grid chunks × seqs, slices staged in shared memory),
        // then a per-sequence merge + sampling replay. The one-block-per-seq
        // k-rounds-over-vocab kernel this replaces cost ~10 ms at k=40 on a
        // 152k vocab.
        let chunk = SAMPLE_CHUNK;
        let n_blocks = vocab.div_ceil(chunk);
        if n_blocks > SAMPLE_MAX_VOCAB / SAMPLE_CHUNK {
            return Err(ForgeError::Unsupported(format!(
                "sample_batched_topk: vocab {vocab} needs {n_blocks} chunks over the cap"
            )));
        }
        let need_parts = n_seqs * SAMPLE_SCRATCH_PAIRS;
        let mut sc = self
            .sample_parts
            .lock()
            .map_err(|_| ForgeError::Kernel("sample parts scratch poisoned".into()))?;
        if sc.cap < need_parts {
            sc.vals = Some(
                self.device
                    .alloc(need_parts * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.idx = Some(
                self.device
                    .alloc(need_parts * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap = need_parts;
        }
        let part_vals = sc.vals.as_ref().expect("parts allocated");
        let part_idx = sc.idx.as_ref().expect("parts allocated");

        let partial = self.artifacts.get("topk_batched_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, n_seqs as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(part_vals)
            .buf(part_idx)
            .buf(logits)
            .scalar(vocab as i64)
            .scalar(chunk as i64)
            .buf(k_arr);
        self.device.launch(partial, &cfg, &args, stream)?;

        let fin = self.artifacts.get("topk_batched_final_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(part_vals)
            .buf(part_idx)
            .scalar(n_blocks as i64)
            .scalar(vocab as i64)
            .buf(k_arr)
            .buf(inv_t_arr)
            .buf(top_p_arr)
            .buf(min_p_arr)
            .buf(seed_arr)
            .buf(step_arr);
        self.device.launch(fin, &cfg, &args, stream)
    }

    /// Batchowe kary in-place z histogramów unikalnych tokenów.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_penalize_f32(
        &self,
        logits: &DevBuffer,
        vocab: usize,
        ids: &DevBuffer,
        counts: &DevBuffer,
        offsets: &DevBuffer,
        penalties: &DevBuffer,
        frequency_penalties: &DevBuffer,
        presence_penalties: &DevBuffer,
        n_seqs: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .scalar(vocab as i64)
            .buf(ids)
            .buf(counts)
            .buf(offsets)
            .buf(penalties)
            .buf(frequency_penalties)
            .buf(presence_penalties);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused decode QKV post-processing: optional per-head q/k RMSNorm, neox
    /// RoPE on q and k, and the paged-cache k/v append in ONE launch. q/k/v
    /// are [heads, head_dim] rows addressed by byte offsets (sections of a
    /// fused qkv buffer or separate buffers). Position and page id come from
    /// device buffers — CUDA-graph-replay safe. Bit-exact vs the separate
    /// rmsnorm/rope/kv_append chain (verified in test_kernels.mojo).
    #[allow(clippy::too_many_arguments)]
    pub fn qkv_post_f16(
        &self,
        q_io: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        positions: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        eps: f32,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        // One element per thread: block = head_dim (MAX_HEAD_DIM in
        // qkv_post.mojo bounds the shared staging array).
        if head_dim > 256 || !head_dim.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "qkv_post requires head_dim % 32 == 0 and head_dim <= 256, got {head_dim}"
            )));
        }
        let k = self.artifacts.get("qkv_post_f16")?;
        let cfg = LaunchConfig {
            grid: ((n_heads + n_kv_heads) as u32, 1, 1),
            block: (head_dim as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_io stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf_at(q_io, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_io))
            .buf(k_norm.unwrap_or(q_io))
            .buf(k_cache)
            .buf(v_cache)
            .buf(positions)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(head_dim as i64)
            .scalar(page_size as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kernel-name suffix for a KV cache element type (F16 canonical, FP8
    /// E4M3 per-value scale-free quantization).
    fn kv_suffix(kv_dtype: DType, what: &str) -> Result<&'static str> {
        match kv_dtype {
            DType::F16 => Ok("f16"),
            DType::F8E4M3 => Ok("fp8"),
            other => Err(ForgeError::Unsupported(format!(
                "{what}: no kernels for KV cache dtype {other}"
            ))),
        }
    }

    /// Scatter a prefill chunk's K/V rows ([n_tokens, n_kv_heads, head_dim])
    /// into the paged cache at positions base_pos..base_pos+n_tokens.
    /// `kv_dtype` selects the cache element type (f16 verbatim | fp8-e4m3
    /// per-value cast).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        kv_dtype: DType,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "kv_append_batch")?;
        let k = self.artifacts.get(&format!("kv_append_batch_{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Zapisuje K/V, odczytując pozycję bazową z bufora urządzenia.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_device_pos_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_batch_device_pos_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .buf(base_pos)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Zapisuje K/V dla spłaszczonych segmentów sequence-major `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_segmented_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        max_pages: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y, block) = validate_kv_append_batch_segmented_f16(
            k_cache.len(),
            v_cache.len(),
            k_in.len(),
            v_in.len(),
            page_tables.len(),
            base_positions.len(),
            batch,
            n_tokens,
            max_pages,
            n_kv_heads,
            page_size,
            head_dim,
            self.device.caps().max_threads_per_block,
        )?;
        let kernel = self.artifacts.get("kv_append_batch_segmented_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T append KV sprawdzone przez validator"))
            .scalar(
                i64::try_from(max_pages).expect("max_pages append KV sprawdzone przez validator"),
            )
            .scalar(
                i64::try_from(n_kv_heads)
                    .expect("liczba głów append KV sprawdzona przez validator"),
            )
            .scalar(
                i64::try_from(page_size)
                    .expect("rozmiar strony append KV sprawdzony przez validator"),
            )
            .scalar(
                i64::try_from(head_dim).expect("head_dim append KV sprawdzone przez validator"),
            );
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zapisuje K/V tylko dla prefiksu zatwierdzonego decyzją każdego lane.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_segmented_masked_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        max_pages: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y, block) = validate_kv_append_batch_segmented_masked_f16(
            k_cache.len(),
            v_cache.len(),
            k_in.len(),
            v_in.len(),
            page_tables.len(),
            base_positions.len(),
            decisions.len(),
            batch,
            n_tokens,
            max_pages,
            n_kv_heads,
            page_size,
            head_dim,
        )?;
        let kernel = self.artifacts.get("kv_append_batch_segmented_masked_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_tables)
            .buf(base_positions)
            .buf(decisions)
            .scalar(
                i64::try_from(n_tokens).map_err(|_| {
                    ForgeError::Kernel("T maskowanego append KV przekracza i64".into())
                })?,
            )
            .scalar(
                i64::try_from(max_pages)
                    .map_err(|_| ForgeError::Kernel("max_pages append KV przekracza i64".into()))?,
            )
            .scalar(
                i64::try_from(n_kv_heads).map_err(|_| {
                    ForgeError::Kernel("n_kv_heads append KV przekracza i64".into())
                })?,
            )
            .scalar(
                i64::try_from(page_size)
                    .map_err(|_| ForgeError::Kernel("page_size append KV przekracza i64".into()))?,
            )
            .scalar(
                i64::try_from(head_dim)
                    .map_err(|_| ForgeError::Kernel("head_dim append KV przekracza i64".into()))?,
            );
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Uruchamia przenośną atencję verifiera dla `[B,T]` i osobnych tablic KV.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_verify_segmented_f16_hd256(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        visible_lens: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.attn_prefill_segmented_f16(
            output,
            q,
            k_cache,
            v_cache,
            page_tables,
            visible_lens,
            batch,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            256,
            page_size,
            max_pages,
            scale,
            stream,
        )
    }

    /// Uruchamia causal prefill dla równych segmentów sequence-major `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_segmented_f16(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        visible_lens: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y) = validate_attn_prefill_segmented_f16(
            output.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_tables.len(),
            visible_lens.len(),
            batch,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            head_dim,
            page_size,
            max_pages,
        )?;
        let caps = self.device.caps();
        let warp32 = matches!(caps.vendor, forge_types::Vendor::Nvidia) && caps.warp_size == 32;
        let kernel_name = if warp32 {
            format!("attn_verify_segmented_f16_hd{head_dim}_warp32")
        } else {
            format!("attn_verify_segmented_f16_hd{head_dim}")
        };
        let kernel = self.artifacts.get(&kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (if warp32 { ATTN_BLOCK } else { 256 }, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(visible_lens)
            .scalar(i64::try_from(n_tokens).expect("T sprawdzone przez validator"))
            .scalar(i64::try_from(n_q_heads).expect("głowice Q sprawdzone przez validator"))
            .scalar(i64::try_from(n_kv_heads).expect("głowice KV sprawdzone przez validator"))
            .scalar(i64::try_from(page_size).expect("rozmiar strony sprawdzony przez validator"))
            .scalar(i64::try_from(max_pages).expect("liczba stron sprawdzona przez validator"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kafelkowa causal prefill attention dla równych segmentów `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_segmented_tiled_f16(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0
            || n_tokens == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || !n_q_heads.is_multiple_of(n_kv_heads)
            || !matches!(head_dim, 128 | 256)
            || page_size == 0
            || max_pages == 0
        {
            return Err(ForgeError::Kernel(
                "kafelkowa segmentowana atencja ma nieprawidłowy kształt".into(),
            ));
        }
        let total = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie segmentowanego prefill".into()))?;
        let query_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill query",
            &[total, n_q_heads, head_dim],
            2,
        )?;
        let page_table_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill page tables",
            &[batch, max_pages],
            4,
        )?;
        let base_bytes =
            checked_buffer_bytes("segmentowany tiled prefill base positions", &[batch], 4)?;
        let cache_page_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill cache page",
            &[n_kv_heads, page_size, head_dim],
            2,
        )?;
        if output.len() < query_bytes
            || q.len() < query_bytes
            || page_tables.len() < page_table_bytes
            || base_positions.len() < base_bytes
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || k_cache.len() != v_cache.len()
            || !k_cache.len().is_multiple_of(cache_page_bytes)
        {
            return Err(ForgeError::Kernel(
                "kafelkowa segmentowana atencja ma za mały lub niezgodny bufor".into(),
            ));
        }
        let tiles_per_sequence = n_tokens.div_ceil(16);
        let grid_x =
            u32::try_from(batch.checked_mul(tiles_per_sequence).ok_or_else(|| {
                ForgeError::Kernel("grid segmentowanego prefill overflow".into())
            })?)
            .map_err(|_| ForgeError::Kernel("grid segmentowanego prefill przekracza u32".into()))?;
        let grid_y = u32::try_from(n_q_heads)
            .map_err(|_| ForgeError::Kernel("głowice prefill przekraczają u32".into()))?;
        let kernel = self
            .artifacts
            .get(&format!("attn_prefill_segmented_f16_hd{head_dim}"))?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T segmentowanego prefill jest małe"))
            .scalar(i64::try_from(max_pages).expect("max_pages segmentowanego prefill jest małe"))
            .scalar(i64::try_from(n_q_heads).expect("Q heads segmentowanego prefill są małe"))
            .scalar(i64::try_from(n_kv_heads).expect("KV heads segmentowanego prefill są małe"))
            .scalar(i64::try_from(page_size).expect("page_size segmentowanego prefill jest małe"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Segmentowana FA korzystająca bez zmian z matematyki MMA ścieżki B1.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_fa_segmented_f16_hd128(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0
            || n_tokens == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || !n_q_heads.is_multiple_of(n_kv_heads)
            || page_size == 0
            || max_pages == 0
        {
            return Err(ForgeError::Kernel(
                "segmentowana FA HD128 ma nieprawidłowy kształt".into(),
            ));
        }
        let total = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie segmentowanej FA".into()))?;
        let query_bytes =
            checked_buffer_bytes("segmentowana FA query", &[total, n_q_heads, 128], 2)?;
        let page_table_bytes =
            checked_buffer_bytes("segmentowana FA page tables", &[batch, max_pages], 4)?;
        let base_bytes = checked_buffer_bytes("segmentowana FA base positions", &[batch], 4)?;
        let cache_page_bytes = checked_buffer_bytes(
            "segmentowana FA cache page",
            &[n_kv_heads, page_size, 128],
            2,
        )?;
        if output.len() < query_bytes
            || q.len() < query_bytes
            || page_tables.len() < page_table_bytes
            || base_positions.len() < base_bytes
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || k_cache.len() != v_cache.len()
            || !k_cache.len().is_multiple_of(cache_page_bytes)
        {
            return Err(ForgeError::Kernel(
                "segmentowana FA HD128 ma za mały lub niezgodny bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("attn_prefill_fa_segmented_f16_hd128")?;
        let config = LaunchConfig {
            grid: (
                u32::try_from(n_tokens.div_ceil(64))
                    .map_err(|_| ForgeError::Kernel("grid.x FA przekracza u32".into()))?,
                u32::try_from(n_q_heads)
                    .map_err(|_| ForgeError::Kernel("grid.y FA przekracza u32".into()))?,
                u32::try_from(batch)
                    .map_err(|_| ForgeError::Kernel("grid.z FA przekracza u32".into()))?,
            ),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T segmentowanej FA jest małe"))
            .scalar(i64::try_from(max_pages).expect("max_pages segmentowanej FA jest małe"))
            .scalar(i64::try_from(n_q_heads).expect("Q heads segmentowanej FA są małe"))
            .scalar(i64::try_from(n_kv_heads).expect("KV heads segmentowanej FA są małe"))
            .scalar(i64::try_from(page_size).expect("page_size segmentowanej FA jest małe"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Causal prefill attention over the paged cache. Query token t attends
    /// positions 0..base_pos+t, whose K/V must already be appended.
    /// `kv_dtype` selects the cache element type; the fp8 variant widens
    /// e4m3 rows to f16 in shared memory (exact), so its math matches the
    /// f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        kv_dtype: DType,
        scale: f32,
        // Okno przesuwne w tokenach; 0 = pełna uwaga przyczynowa.
        window: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Tensor-core flash-attention paths. Only the f16 cache with head_dim
        // 64/128 has an FA specialization; every other shape falls through to
        // the Mojo scalar kernel so nothing breaks.
        if kv_dtype == DType::F16 && (head_dim == 64 || head_dim == 128) {
            match self.attn {
                AttnBackend::Cuda => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, false,
                    );
                }
                AttnBackend::Mojo => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, true,
                    );
                }
                AttnBackend::Scalar => {}
            }
        }
        let suffix = Self::kv_suffix(kv_dtype, "attn_prefill")?;
        let name = match (head_dim, kv_dtype) {
            (64, _) => format!("attn_prefill_{suffix}_hd64"),
            (128, _) => format!("attn_prefill_{suffix}_hd128"),
            // Only the f16 cache has an hd256 specialization (qwen35moe
            // attention layers); fp8/rot hd256 is not compiled.
            (256, DType::F16) => format!("attn_prefill_{suffix}_hd256"),
            // 512: warstwy globalne Gemmy 4. Kafel pozycji jest tam o połowę
            // mniejszy (LDS), ale kontrakt uruchomienia jest ten sam.
            (512, DType::F16) => format!("attn_prefill_{suffix}_hd512"),
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
        // Kernel tiling contract (prefill.mojo QT): 16 queries per block,
        // block of 8 warps.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(16), n_q_heads as u32, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64)
            .scalar(window as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wykonuje prefill HD256 z pozycją bazową przechowywaną na urządzeniu.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_device_pos_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("attn_prefill_device_pos_f16_hd256")?;
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(16), n_q_heads as u32, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(base_pos)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Uruchamia Mojo Flash Attention HD256 ze zwalidowaną pozycją bazową.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_fa_mojo_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_position: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y) = validate_attn_prefill_fa_f16_hd256(
            out.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            base_position,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            page_size,
            scale,
        )?;
        if self.device.caps().max_threads_per_block < 128 {
            return Err(ForgeError::Unsupported(
                "flash attention prefill HD256 wymaga bloku 128 wątków".into(),
            ));
        }
        let experimental_bk32 =
            std::env::var("FORGE_HYBRID_FA_KEY_TILE").is_ok_and(|value| value == "32");
        let kernel_name =
            if experimental_bk32 && self.artifacts.has("attn_prefill_fa_mojo_f16_hd256_bk32") {
                "attn_prefill_fa_mojo_f16_hd256_bk32"
            } else if self.artifacts.has("attn_prefill_fa_mojo_f16_hd256_vtrans") {
                "attn_prefill_fa_mojo_f16_hd256_vtrans"
            } else {
                "attn_prefill_fa_mojo_f16_hd256"
            };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(
                i64::try_from(base_position).expect("pozycja bazowa sprawdzona przez validator"),
            )
            .scalar(i64::try_from(n_q_heads).expect("głowice Q sprawdzone przez validator"))
            .scalar(i64::try_from(n_kv_heads).expect("głowice KV sprawdzone przez validator"))
            .scalar(i64::try_from(page_size).expect("rozmiar strony sprawdzony przez validator"))
            .scalar(scale)
            .scalar(i64::try_from(n_tokens).expect("T sprawdzone przez validator"));
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Tensor-core causal flash-attention prefill. Same I/O contract as
    /// `attn_prefill` (f16 cache, paged KV, GQA, causal) but QK^T and P·V run as
    /// f16 mma with an online softmax kept in registers. Grid: (ceil(T/64),
    /// n_q_heads); one block of 4 warps owns 64 query rows of one head. `mojo`
    /// selects the portable Mojo kernel (`attn_prefill_fa_mma`,
    /// kernels/mojo/src/prefill.mojo) over the CUDA cubin
    /// (kernels/cuda/fattn_prefill.cu) — byte-identical tiling contract.
    #[allow(clippy::too_many_arguments)]
    fn attn_prefill_fa(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
        mojo: bool,
    ) -> Result<()> {
        let name = match (head_dim, mojo) {
            (64, false) => "attn_prefill_fa_f16_hd64",
            (128, false) => "attn_prefill_fa_f16_hd128",
            (64, true) => "attn_prefill_fa_mojo_f16_hd64",
            (128, true) => "attn_prefill_fa_mojo_f16_hd128",
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill_fa: head_dim {other} has no FA specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        // Kernel tiling contract (fattn_prefill.cu): BQ=64 queries per block,
        // 4 warps = 128 threads.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(64), n_q_heads as u32, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q4_K superblocks, x/y f16. Warp per row.
    pub fn gemv_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_K weights → f32 logits.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y_f32, y_off)?
            .buf(w_q4k)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q4_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_k_f16_at(y, w_q4k, 0, x, rows, cols, n_tokens, stream)
    }

    /// int8 TENSOR-CORE MMQ prefill GEMM over Q8_0 weights.
    /// Y[t, row] = W·x[t]; `w_byte_off` addresses the window's first block.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_i8mma_at(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_i8mma requires cols % 32 == 0, got {cols}"
            )));
        }
        self.gemm_i8mma_run(
            "gemm_q8_0_i8mma",
            false,
            y,
            w_q8,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Krótki GEMM Q8_0 x Q8_1 zapisujący pełne logity F32 dla weryfikatora.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_i8mma_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) || !(3..=4).contains(&n_tokens) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_i8mma_out_f32 wymaga cols % 32 == 0 i T=3/4, otrzymano cols={cols}, T={n_tokens}"
            )));
        }
        self.gemm_i8mma_run(
            "gemm_q8_0_i8mma",
            true,
            y_f32,
            w_q8,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Dokładny krótki GEMM Q8_0 x F16 zapisujący logity F32 bez requantyzacji X.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16_exact_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(32) || !(2..=8).contains(&n_tokens) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_f16_exact_out_f32 wymaga rows > 0, cols % 32 == 0 i T=2..8, otrzymano rows={rows}, cols={cols}, T={n_tokens}"
            )));
        }
        let output_bytes =
            checked_buffer_bytes("gemm_q8_0_f16_exact_out_f32 output", &[n_tokens, rows], 4)?;
        let weight_bytes = checked_buffer_bytes(
            "gemm_q8_0_f16_exact_out_f32 weights",
            &[rows, cols / 32],
            34,
        )?;
        let weight_end = w_byte_off.checked_add(weight_bytes).ok_or_else(|| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: przepełnienie zakresu wag".into())
        })?;
        let input_bytes =
            checked_buffer_bytes("gemm_q8_0_f16_exact_out_f32 input", &[n_tokens, cols], 2)?;
        if y_f32.len() < output_bytes || w_q8.len() < weight_end || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_q8_0_f16_exact_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let caps = self.device.caps();
        let rows_per_block = 8u32;
        let block_threads = caps.warp_size.checked_mul(rows_per_block).ok_or_else(|| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: przepełnienie rozmiaru bloku".into())
        })?;
        if block_threads > caps.max_threads_per_block {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_f16_exact_out_f32: blok {block_threads} przekracza limit urządzenia {}",
                caps.max_threads_per_block
            )));
        }
        let grid_x = u32::try_from(rows.div_ceil(rows_per_block as usize)).map_err(|_| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: siatka przekracza u32".into())
        })?;
        let kernel_name = match n_tokens {
            2 => "gemm_q8_0_f16_exact_out_f32_b2",
            3 => "gemm_q8_0_f16_exact_out_f32_b3",
            4 => "gemm_q8_0_f16_exact_out_f32_b4",
            5..=8 => "gemm_q8_0_f16_exact_out_f32_b8",
            _ => unreachable!(),
        };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// int8 TENSOR-CORE MMQ prefill GEMM over Q4_K weights.
    /// Y[t, row] = W·x[t]; `w_byte_off` addresses the window's first superblock.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_i8mma_at(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_k_i8mma requires cols % 256 == 0, got {cols}"
            )));
        }
        // Universal DEFAULT (all arches): the native-GGUF-layout Mojo int8 Q4_K
        // multistage GEMM (reads the raw `DevWeight::Q4K.buf` bytes in-kernel, NO
        // repack; bit-exact vs Q4_K MMQ by construction). Prefill-sized batches
        // whose (rows,cols) has a committed (N,K,MPAD) instance and T ≤ 4096. A
        // shape/token count with no bucket (or decode-sized n_tokens < 64) falls
        // through to the portable hand int8-MMQ tiles.
        if n_tokens >= 64
            && self.gemm_q4k_i8_native(y, w_q4k, w_byte_off, x, rows, cols, n_tokens, stream)?
        {
            return Ok(());
        }
        self.gemm_i8mma_run(
            "gemm_q4_k_i8mma",
            false,
            y,
            w_q4k,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Smallest committed MPAD bucket ≥ `n_tokens`, or `None` if `n_tokens`
    /// exceeds the largest committed ceiling (4096).
    fn q4k_native_mpad(n_tokens: usize) -> Option<usize> {
        [128usize, 256, 512, 1024, 2048, 4096]
            .into_iter()
            .find(|&m| m >= n_tokens)
    }

    /// Native-GGUF-layout Mojo int8 Q4_K multistage prefill GEMM (universal
    /// default). Zero-pads the f16 activation to the compile-time token ceiling
    /// MPAD (smallest bucket ≥ `n_tokens`), quantizes it to q8_1 over MPAD
    /// (block-major da/sa, stride MPAD), then runs the native GEMM reading the RAW
    /// `w_q4k` GGUF bytes at `w_byte_off` (144-byte block_q4_K de-interleaved
    /// in-kernel — TRUE 1× VRAM, no repacked weight/scale copy). The kernel guards
    /// stores by `m_real = n_tokens`, so the padded tail rows are computed but
    /// never written. Dynamic smem 53248 B (the >48 KB opt-in the HAL sets
    /// automatically). Returns `false` (caller falls back to the hand int8-MMQ
    /// tiles) when `(rows,cols)` has no committed instance or `n_tokens > 4096`.
    #[allow(clippy::too_many_arguments)]
    fn gemm_q4k_i8_native(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        let Some(mpad) = Self::q4k_native_mpad(n_tokens) else {
            return Ok(false);
        };
        let key = format!("gemm_q4k_i8_native_{rows}_{cols}_m{mpad}");
        let Ok(gk) = self.artifacts.get(&key) else {
            return Ok(false);
        };
        let qk = self.artifacts.get("quantize_act_q8_1")?;

        // Grow-only scratch: padded f16 activation [MPAD, cols], its int8 q8_1
        // codes [MPAD, cols] and block-major da/sa [cols/32, MPAD]. The padded
        // tail (rows n_tokens..MPAD) is allocated but never read for correctness
        // (its outputs are guarded off by m_real), so no zeroing is needed.
        let need_x = mpad * cols;
        let need_blocks = mpad * (cols / 32);
        let mut sc = self.q4k_native.lock().expect("q4k native scratch poisoned");
        if sc.cap_x < need_x {
            sc.xpad = Some(
                self.device
                    .alloc(need_x * 2, MemKind::Device, Pool::Activations)?,
            );
            sc.xq = Some(
                self.device
                    .alloc(need_x, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_x = need_x;
        }
        if sc.cap_blocks < need_blocks {
            sc.da = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.sa = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_blocks = need_blocks;
        }
        let xpad = sc.xpad.as_ref().expect("xpad allocated");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let da = sc.da.as_ref().expect("da allocated");
        let sa = sc.sa.as_ref().expect("sa allocated");

        // Copy the real activation [n_tokens, cols] f16 into the padded head.
        self.device
            .copy(x, 0, xpad, 0, n_tokens * cols * 2, stream)?;

        // q8_1 quant over the full MPAD ceiling → int8 codes + block-major da/sa
        // (stride MPAD, matching the native kernel's da[kb*MPAD + token] indexing).
        let qcfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(da)
            .buf(sa)
            .buf(xpad)
            .scalar(cols as i64)
            .scalar(mpad as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        // Native GEMM: grid (ceil(rows/128), MPAD/128); block 256; dynamic smem
        // 53248 B. Args mirror gemm_q4k_i8_native(y, a=xq, w, da, sa, m_real).
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(128), (mpad as u32) / 128, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 53248,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf_at(w_q4k, w_byte_off)?
            .buf(da)
            .buf(sa)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)?;
        Ok(true)
    }

    /// Pre-quantize the activation to q8_1 ONCE (`quantize_act_q8_1`) into the
    /// grow-only scratch, then run the int8-MMQ GEMM reading int8 X directly.
    /// This halves X read bandwidth and removes the redundant per-row-block
    /// requant the old in-kernel quant paid across the grid's `ceil(rows/64)`
    /// blocks. Both launches share one `stream`, so the GEMM sees the quantized
    /// X without an explicit sync.
    pub fn prepare_q8_1<'a>(
        &'a self,
        x: &DevBuffer,
        cols: usize,
        n_tokens: usize,
        stream: &'a Stream,
    ) -> Result<Q8ActPrepared<'a>> {
        if !(matches!(n_tokens, 6 | 8) || n_tokens >= 32) || cols == 0 || !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "prepare_q8_1 wymaga T=6/8 lub T>=32 i cols > 0 podzielnego przez 32, otrzymano T={n_tokens}, cols={cols}"
            )));
        }
        if n_tokens >= 32 {
            let caps = self.device.caps();
            if caps.vendor != forge_types::Vendor::Nvidia
                || caps.warp_size != 32
                || caps.max_threads_per_block < BLOCK
            {
                return Err(ForgeError::Unsupported(
                    "prepared Q8 T>=32 wymaga NVIDIA warp32 i bloku 256 wątków".into(),
                ));
            }
        }
        let input_bytes = checked_buffer_bytes("prepare_q8_1 input", &[n_tokens, cols], 2)?;
        if x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "prepare_q8_1: bufor wejścia jest za mały".into(),
            ));
        }
        let need_codes = checked_buffer_bytes("prepare_q8_1 codes", &[n_tokens, cols], 1)?;
        let scale_bytes = checked_buffer_bytes("prepare_q8_1 scales", &[n_tokens, cols / 32], 4)?;
        let need_blocks = scale_bytes / 4;
        let blocks_u32 = u32::try_from(need_blocks)
            .map_err(|_| ForgeError::Kernel("prepare_q8_1: liczba bloków przekracza u32".into()))?;
        let cols_i64 = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("prepare_q8_1: cols przekracza i64".into()))?;
        let n_tokens_i64 = i64::try_from(n_tokens)
            .map_err(|_| ForgeError::Kernel("prepare_q8_1: T przekracza i64".into()))?;
        let mut scratch = lock_prepared_q8_scratch(&self.prepared_q8)?;
        let grows = scratch.cap_codes < need_codes || scratch.cap_blocks < need_blocks;
        if let Some(ready) = scratch.ready.as_ref() {
            if grows {
                if let Err(error) = ready.synchronize() {
                    scratch.poisoned = true;
                    return Err(ForgeError::Kernel(format!(
                        "prepared Q8: synchronizacja przed zmianą pojemności nie powiodła się: {error}"
                    )));
                }
            } else {
                self.device.wait_event(stream, ready)?;
            }
        }
        if scratch.cap_codes < need_codes {
            scratch.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            scratch.cap_codes = need_codes;
        }
        if scratch.cap_blocks < need_blocks {
            scratch.xd = Some(self.device.alloc(
                scale_bytes,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.xsm = Some(self.device.alloc(
                scale_bytes,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.cap_blocks = need_blocks;
        }
        if scratch.ready.is_none() {
            scratch.ready = Some(self.device.create_event()?);
        }
        let qk = self.artifacts.get("quantize_act_q8_1")?;
        let qcfg = LaunchConfig::linear(blocks_u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(scratch.xq.as_ref().expect("xq allocated"))
            .buf(scratch.xd.as_ref().expect("xd allocated"))
            .buf(scratch.xsm.as_ref().expect("xsm allocated"))
            .buf(x)
            .scalar(cols_i64)
            .scalar(n_tokens_i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;
        mark_prepared_q8_ready(self.device.as_ref(), &mut scratch, stream)?;
        Ok(Q8ActPrepared {
            scratch,
            stream,
            cols,
            n_tokens,
            valid: true,
        })
    }

    /// Uruchamia Q8_0 GEMM na wcześniej przygotowanej aktywacji Q8_1.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_i8mma_prepared_at(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        prepared: &mut Q8ActPrepared<'_>,
        rows: usize,
        cols: usize,
        n_tokens: usize,
    ) -> Result<()> {
        if !prepared.valid {
            return Err(ForgeError::Kernel(
                "prepared Q8 handle jest nieważny po błędzie markera".into(),
            ));
        }
        if prepared.cols != cols
            || prepared.n_tokens != n_tokens
            || !(matches!(n_tokens, 6 | 8) || n_tokens >= 32)
            || rows == 0
        {
            return Err(ForgeError::Kernel(format!(
                "prepared Q8_0 wymaga zgodnych wymiarów T=6/8 lub T>=32 i rows > 0, otrzymano rows={rows}, cols={cols}, T={n_tokens}"
            )));
        }
        if n_tokens >= 32 {
            let caps = self.device.caps();
            if caps.vendor != forge_types::Vendor::Nvidia
                || caps.warp_size != 32
                || caps.max_threads_per_block < BLOCK
            {
                return Err(ForgeError::Unsupported(
                    "prepared Q8 T>=32 wymaga NVIDIA warp32 i bloku 256 wątków".into(),
                ));
            }
        }
        let output_bytes = checked_buffer_bytes("prepared Q8_0 output", &[n_tokens, rows], 2)?;
        let weight_bytes = checked_buffer_bytes("prepared Q8_0 weights", &[rows, cols / 32], 34)?;
        let weight_end = w_byte_off
            .checked_add(weight_bytes)
            .ok_or_else(|| ForgeError::Kernel("prepared Q8_0: przepełnienie wag".into()))?;
        if y.len() < output_bytes || w_q8.len() < weight_end {
            return Err(ForgeError::Kernel(
                "prepared Q8_0: bufor wyjścia lub wag jest za mały".into(),
            ));
        }
        let rows_u32 = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("prepared Q8_0: rows przekracza u32".into()))?;
        let cols_i64 = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("prepared Q8_0: cols przekracza i64".into()))?;
        let rows_i64 = i64::try_from(rows)
            .map_err(|_| ForgeError::Kernel("prepared Q8_0: rows przekracza i64".into()))?;
        let n_tokens_i64 = i64::try_from(n_tokens)
            .map_err(|_| ForgeError::Kernel("prepared Q8_0: T przekracza i64".into()))?;
        let (kernel, cfg, args) = if matches!(n_tokens, 6 | 8) {
            let caps = self.device.caps();
            let rows_per_block = 8u32;
            let block_threads = caps.warp_size.checked_mul(rows_per_block).ok_or_else(|| {
                ForgeError::Kernel("prepared Q8_0: przepełnienie rozmiaru bloku".into())
            })?;
            if block_threads > caps.max_threads_per_block {
                return Err(ForgeError::Kernel(format!(
                    "prepared Q8_0: blok {block_threads} przekracza limit urządzenia {}",
                    caps.max_threads_per_block
                )));
            }
            (
                self.artifacts.get("gemm_q8_0_i8mma_b8")?,
                LaunchConfig {
                    grid: (rows_u32.div_ceil(rows_per_block), 1, 1),
                    block: (block_threads, 1, 1),
                    shared_mem_bytes: 0,
                },
                LaunchArgs::new()
                    .buf(y)
                    .buf_at(w_q8, w_byte_off)?
                    .buf(prepared.scratch.xq.as_ref().expect("xq prepared"))
                    .buf(prepared.scratch.xd.as_ref().expect("xd prepared"))
                    .scalar(cols_i64)
                    .scalar(rows_i64)
                    .scalar(n_tokens_i64),
            )
        } else {
            let (suffix, bm, bn, threads) = Self::gemm_i8mma_tile(rows, n_tokens);
            (
                self.artifacts.get(&format!("gemm_q8_0_i8mma{suffix}"))?,
                LaunchConfig {
                    grid: (rows_u32.div_ceil(bn), (n_tokens as u32).div_ceil(bm), 1),
                    block: (threads, 1, 1),
                    shared_mem_bytes: 0,
                },
                LaunchArgs::new()
                    .buf(y)
                    .buf_at(w_q8, w_byte_off)?
                    .buf(prepared.scratch.xq.as_ref().expect("xq prepared"))
                    .buf(prepared.scratch.xd.as_ref().expect("xd prepared"))
                    .buf(prepared.scratch.xsm.as_ref().expect("xsm prepared"))
                    .scalar(cols_i64)
                    .scalar(rows_i64)
                    .scalar(n_tokens_i64),
            )
        };
        #[cfg(test)]
        PREPARED_Q8_GEMM_LAUNCHES.fetch_add(1, Ordering::SeqCst);
        self.device.launch(kernel, &cfg, &args, prepared.stream)?;
        if let Err(error) =
            mark_prepared_q8_ready(self.device.as_ref(), &mut prepared.scratch, prepared.stream)
        {
            prepared.valid = false;
            return Err(error);
        }
        Ok(())
    }

    /// Uruchamia trzy projekcje Q8_0 w jednym gridzie na wspólnej aktywacji Q8_1.
    pub fn gemm_q8_0_i8mma_prepared_triplet(
        &self,
        projections: &[Q8PreparedProjection<'_>; 3],
        prepared: &mut Q8ActPrepared<'_>,
        cols: usize,
        n_tokens: usize,
    ) -> Result<()> {
        if !prepared.valid {
            return Err(ForgeError::Kernel(
                "prepared Q8 handle jest nieważny po błędzie markera".into(),
            ));
        }
        if prepared.cols != cols || prepared.n_tokens != n_tokens || n_tokens < 32 {
            return Err(ForgeError::Kernel(format!(
                "fused prepared Q8 wymaga zgodnych wymiarów T>=32, otrzymano cols={cols}, T={n_tokens}"
            )));
        }
        let caps = self.device.caps();
        if caps.vendor != forge_types::Vendor::Nvidia
            || caps.warp_size != 32
            || caps.max_threads_per_block < BLOCK
        {
            return Err(ForgeError::Unsupported(
                "fused prepared Q8 wymaga NVIDIA warp32 i bloku 256 wątków".into(),
            ));
        }
        for projection in projections {
            if projection.rows == 0 {
                return Err(ForgeError::Kernel(
                    "fused prepared Q8 wymaga rows > 0 dla każdej projekcji".into(),
                ));
            }
            let output_bytes =
                checked_buffer_bytes("fused prepared Q8 output", &[n_tokens, projection.rows], 2)?;
            let weight_bytes = checked_buffer_bytes(
                "fused prepared Q8 weights",
                &[projection.rows, cols / 32],
                34,
            )?;
            let weight_end = projection
                .weight_byte_offset
                .checked_add(weight_bytes)
                .ok_or_else(|| ForgeError::Kernel("fused prepared Q8: przepełnienie wag".into()))?;
            if projection.output.len() < output_bytes || projection.weights.len() < weight_end {
                return Err(ForgeError::Kernel(
                    "fused prepared Q8: bufor wyjścia lub wag jest za mały".into(),
                ));
            }
        }
        let cols_i64 = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("fused prepared Q8: cols przekracza i64".into()))?;
        let n_tokens_i64 = i64::try_from(n_tokens)
            .map_err(|_| ForgeError::Kernel("fused prepared Q8: T przekracza i64".into()))?;
        let rows = projections
            .iter()
            .map(|projection| {
                i64::try_from(projection.rows).map_err(|_| {
                    ForgeError::Kernel("fused prepared Q8: rows przekracza i64".into())
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let (kernel_name, bm, bn, block) = if n_tokens >= 1024
            && caps.max_threads_per_block >= 512
            && self
                .artifacts
                .has("gemm_q8_0_i8mma_triplet_single_big_poststage")
        {
            (
                "gemm_q8_0_i8mma_triplet_single_big_poststage",
                128,
                128,
                512,
            )
        } else if n_tokens >= 1024
            && caps.max_threads_per_block >= 512
            && self.artifacts.has("gemm_q8_0_i8mma_triplet_single_big")
        {
            ("gemm_q8_0_i8mma_triplet_single_big", 128, 128, 512)
        } else if self.artifacts.has("gemm_q8_0_i8mma_triplet_single_bm64") {
            ("gemm_q8_0_i8mma_triplet_single_bm64", 64, 64, BLOCK)
        } else if n_tokens >= 256 {
            for projection in projections {
                self.gemm_q8_0_i8mma_prepared_at(
                    projection.output,
                    projection.weights,
                    projection.weight_byte_offset,
                    prepared,
                    projection.rows,
                    cols,
                    n_tokens,
                )?;
            }
            return Ok(());
        } else {
            ("gemm_q8_0_i8mma_triplet_bm64", 64, 64, BLOCK)
        };
        let row_blocks = projections.iter().try_fold(0u32, |sum, projection| {
            let rows = u32::try_from(projection.rows)
                .map_err(|_| ForgeError::Kernel("fused prepared Q8: rows przekracza u32".into()))?;
            sum.checked_add(rows.div_ceil(bn))
                .ok_or_else(|| ForgeError::Kernel("fused prepared Q8: grid przekracza u32".into()))
        })?;
        let kernel = self.artifacts.get(kernel_name)?;
        let cfg = LaunchConfig {
            grid: (row_blocks, (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(projections[0].output)
            .buf_at(projections[0].weights, projections[0].weight_byte_offset)?
            .scalar(rows[0])
            .buf(projections[1].output)
            .buf_at(projections[1].weights, projections[1].weight_byte_offset)?
            .scalar(rows[1])
            .buf(projections[2].output)
            .buf_at(projections[2].weights, projections[2].weight_byte_offset)?
            .scalar(rows[2])
            .buf(prepared.scratch.xq.as_ref().expect("xq prepared"))
            .buf(prepared.scratch.xd.as_ref().expect("xd prepared"))
            .buf(prepared.scratch.xsm.as_ref().expect("xsm prepared"))
            .scalar(cols_i64)
            .scalar(n_tokens_i64);
        #[cfg(test)]
        PREPARED_Q8_GEMM_LAUNCHES.fetch_add(1, Ordering::SeqCst);
        self.device.launch(kernel, &cfg, &args, prepared.stream)?;
        if let Err(error) =
            mark_prepared_q8_ready(self.device.as_ref(), &mut prepared.scratch, prepared.stream)
        {
            prepared.valid = false;
            return Err(error);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn gemm_i8mma_run(
        &self,
        kernel_base: &str,
        output_f32: bool,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Portable Mojo int8 tensor-core tiles (`.target sm_80`, JIT to any
        // sm_80+ part). This is the default Q4_K/Q6_K prefill GEMM on pre-Ada
        // GPUs and the Q8_0 prefill GEMM everywhere; on Ada the vendored MMQ
        // cubin intercepts Q4_K/Q6_K upstream (`gemm_q4_k_i8mma_at`).
        let (xq, xd, xsm) = self.prequant_q8_1(x, cols, n_tokens, stream)?;
        let (xq, xd, xsm) = (&xq, &xd, &xsm);

        if kernel_base == "gemm_q8_0_i8mma"
            && (2..=8).contains(&n_tokens)
            && (!output_f32 || n_tokens >= 3)
        {
            let caps = self.device.caps();
            let nvidia_dp4a = matches!(caps.vendor, forge_types::Vendor::Nvidia)
                && caps.warp_size == 32
                && matches!(n_tokens, 3 | 4);
            let rows_per_block = if nvidia_dp4a { 4 } else { 8 };
            let block_threads = caps.warp_size.checked_mul(rows_per_block).ok_or_else(|| {
                ForgeError::Kernel("gemm_q8_0 small: przepełnienie rozmiaru bloku".into())
            })?;
            if block_threads > caps.max_threads_per_block {
                return Err(ForgeError::Kernel(format!(
                    "gemm_q8_0 small: blok {block_threads} przekracza limit urządzenia {}",
                    caps.max_threads_per_block
                )));
            }
            let kernel_name = match (output_f32, n_tokens) {
                (false, 2) => "gemm_q8_0_i8mma_b2",
                (false, 3) if nvidia_dp4a => "gemm_q8_0_dp4a_b3_nvidia",
                (false, 4) if nvidia_dp4a => "gemm_q8_0_dp4a_b4_nvidia",
                (true, 3) if nvidia_dp4a => "gemm_q8_0_dp4a_out_f32_b3_nvidia",
                (true, 4) if nvidia_dp4a => "gemm_q8_0_dp4a_out_f32_b4_nvidia",
                (false, 3) => "gemm_q8_0_i8mma_b3",
                (false, 4) => "gemm_q8_0_i8mma_b4",
                (false, 5..=8) => "gemm_q8_0_i8mma_b8",
                (true, 3) => "gemm_q8_0_i8mma_out_f32_b3",
                (true, 4) => "gemm_q8_0_i8mma_out_f32_b4",
                _ => unreachable!(),
            };
            let kernel = self.artifacts.get(kernel_name)?;
            let cfg = LaunchConfig {
                grid: ((rows as u32).div_ceil(rows_per_block), 1, 1),
                block: (block_threads, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(y)
                .buf_at(w, w_byte_off)?
                .buf(xq)
                .buf(xd)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(n_tokens as i64);
            return self.device.launch(kernel, &cfg, &args, stream);
        }

        // Karty bez jednostki macierzowej: kafel int8 na `v_dot4_i32_i8`.
        if let Some(tile) = self.gemm_dot4_tile(kernel_base, output_f32, rows, n_tokens) {
            if std::env::var("FORGE_TRACE_ROUTE").is_ok() {
                eprintln!("ROUTE dot4 {} rows={rows} cols={cols} T={n_tokens}", tile.name);
            }
            let gk = self.artifacts.get(tile.name)?;
            let cfg = tile.config(rows, n_tokens);
            let args = LaunchArgs::new()
                .buf(y)
                .buf_at(w, w_byte_off)?
                .buf(xq)
                .buf(xd)
                .buf(xsm)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(n_tokens as i64);
            return self.device.launch(gk, &cfg, &args, stream);
        }

        let (suffix, bm, bn, threads) = Self::gemm_i8mma_tile(rows, n_tokens);
        let gk = self.artifacts.get(&format!("{kernel_base}{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(bn),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Kafel `gemm_*_dot4` dla kart bez jednostki macierzowej, albo `None` na
    /// NVIDII i dla formatów, których kafel int8 jeszcze nie obsługuje — wtedy
    /// wołający zgłosi brak kernela rodziny i8mma, co jest właściwym błędem.
    ///
    /// Kafel dobrany pomiarem na gfx1030 (4096x4096, T=1024): 128x128 daje
    /// 35 TOPS, 128x64 32, a 64x64 29 i jest potrzebny tylko dla wąskich
    /// kształtów, gdzie większy kafel liczy głównie odrzucane wiersze.
    /// Czy batchowe GEMM-y kwantyzuja aktywacje do int8 przed mnozeniem.
    /// Karty bez jednostki macierzowej licza kaflem `v_dot4_i32_i8`, wiec
    /// wynik zawiera blad kwantyzacji aktywacji — referencje testow musza go
    /// odtwarzac, inaczej mierza kwantyzacje, nie kernel.
    pub fn int8_batch_activations(&self) -> bool {
        self.device.caps().vendor != forge_types::Vendor::Nvidia
    }

    fn gemm_dot4_tile(
        &self,
        kernel_base: &str,
        output_f32: bool,
        rows: usize,
        n_tokens: usize,
    ) -> Option<DotTile> {
        if self.device.caps().vendor == forge_types::Vendor::Nvidia {
            return None;
        }
        let family = match kernel_base {
            "gemm_q8_0_i8mma" => "q8_0",
            "gemm_q4_k_i8mma" => "q4_k",
            "gemm_q6_k_i8mma" => "q6_k",
            "gemm_q4_0_i8mma" => "q4_0",
            _ => return None,
        };
        // Batchowa głowa logitów zapisuje f32 i pracuje na rozmiarze batcha
        // decode, więc ma tylko najmniejszy kafel.
        if output_f32 {
            return Some(DotTile::new(
                match family {
                    "q8_0" => "gemm_q8_0_dot4_out_f32_64x64",
                    "q4_k" => "gemm_q4_k_dot4_out_f32_64x64",
                    "q4_0" => "gemm_q4_0_dot4_out_f32_64x64",
                    _ => "gemm_q6_k_dot4_out_f32_64x64",
                },
                64,
                64,
                4,
                4,
            ));
        }
        Some(if n_tokens <= 64 || rows < 128 {
            DotTile::new(
                match family {
                    "q8_0" => "gemm_q8_0_dot4_64x64",
                    "q4_k" => "gemm_q4_k_dot4_64x64",
                    "q4_0" => "gemm_q4_0_dot4_64x64",
                    _ => "gemm_q6_k_dot4_64x64",
                },
                64,
                64,
                4,
                4,
            )
        } else if family == "q4_0" {
            DotTile::new("gemm_q4_0_dot4_128x128", 128, 128, 8, 4)
        } else if family == "q4_k" {
            // Formaty K rozpakowują wagi w LDS, więc płacą więcej za etap;
            // kafel 128x64 wyszedł szybszy (32 wobec 29 TOPS) niż 128x128.
            DotTile::new("gemm_q4_k_dot4_128x64", 128, 64, 8, 4)
        } else if family == "q6_k" {
            DotTile::new("gemm_q6_k_dot4_128x64", 128, 64, 8, 4)
        } else if n_tokens <= 128 {
            DotTile::new("gemm_q8_0_dot4_128x64", 128, 64, 8, 4)
        } else {
            DotTile::new("gemm_q8_0_dot4_128x128", 128, 128, 8, 4)
        })
    }

    /// Kwantyzuje aktywację `x[T, cols]` do q8_1 w wewnętrznym scratchu i
    /// zwraca `(kody int8, skale, skale*suma)`. Skale są blok-major `[K/32, T]`.
    ///
    /// Wspólne dla wszystkich kafli int8: rodziny i8mma oraz kafli dot na
    /// kartach bez jednostki macierzowej. Bufory rosną tylko w górę i żyją
    /// między wywołaniami, więc kolejne warstwy nie alokują.
    fn prequant_q8_1(
        &self,
        x: &DevBuffer,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<(DevBuffer, DevBuffer, DevBuffer)> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "prequant_q8_1 wymaga cols % 32 == 0, otrzymano {cols}"
            )));
        }
        let qk = self.artifacts.get("quantize_act_q8_1")?;
        let need_codes = n_tokens * cols;
        let need_blocks = n_tokens * (cols / 32);

        let mut sc = self.prequant.lock().expect("prequant scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_blocks < need_blocks {
            sc.xd = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.xsm = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.cap_blocks = need_blocks;
        }
        let xq = sc.xq.as_ref().expect("xq allocated").clone();
        let xd = sc.xd.as_ref().expect("xd allocated").clone();
        let xsm = sc.xsm.as_ref().expect("xsm allocated").clone();

        let qcfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(&xq)
            .buf(&xd)
            .buf(&xsm)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;
        Ok((xq, xd, xsm))
    }

    /// Kafel `gemm_nvfp4_dot4`, albo `None` na NVIDII i dla kształtów, które
    /// obsługują wyspecjalizowane gemv batchowe (do 16 tokenów) — tam kafel
    /// prefillowy liczyłby w większości odrzucane wiersze.
    fn gemm_nvfp4_dot4_tile(
        &self,
        rows: usize,
        cols: usize,
        n_tokens: usize,
    ) -> Option<DotTile> {
        if self.device.caps().vendor == forge_types::Vendor::Nvidia {
            return None;
        }
        // Kafel wnosi kolumny 32-blokami (tyle ma blok kwantyzacji aktywacji),
        // więc kształt niepodzielny przez 32 nie jest jego przypadkiem; taki
        // zgłosi brak kernela rodziny mma zamiast policzyć źle.
        if n_tokens <= 16 || !cols.is_multiple_of(32) {
            return None;
        }
        Some(if n_tokens <= 64 || rows < 128 {
            DotTile::new("gemm_nvfp4_dot4_64x64", 64, 64, 4, 4)
        } else {
            DotTile::new("gemm_nvfp4_dot4_128x64", 128, 64, 8, 4)
        })
    }

    /// Tile selection for the i8mma GEMM: `(suffix, BM, BN, block_threads)`.
    ///
    /// The `_big` variant (BM=128 x BN=128, 512-thread/16-warp block) doubles
    /// the rows-per-block so the activation X — re-read `ceil(rows/BN)` times —
    /// is fetched half as often, raising the mma:bytes-loaded ratio. It keeps
    /// the per-warp accumulator (and thus the 127-reg / 1-CTA-per-SM = 16-warp
    /// occupancy, matching the old 2x256-thread = 16-warp footprint) fixed by
    /// adding warps instead of n-tiles/warp. Bit-identical to the old BM=128
    /// kernel (integer mma is exact).
    ///
    /// The 512-thread block halves the block count of a given GEMM (BM=128 x
    /// BN=128 vs the 256-thread kernel's BM=128 x BN=64 at 2 CTAs/SM), so it
    /// only wins when the GEMM is big enough to keep the ~128 SMs busy at the
    /// coarser granularity. Two conditions must both hold:
    ///  * `n_tokens >= 1024` (a full `MAX_PREFILL_CHUNK`): at a 512-token chunk
    ///    the whole prefill is tiny and the coarse blocks underfill the SMs for
    ///    the small attention projections, regressing the Mistral 512 prefill
    ///    ~11%.
    ///  * `ceil(rows/128) * ceil(n_tokens/128) >= 256` (>= 2 full waves on the
    ///    128 SMs at 1 CTA/SM): small-model projections (Qwen3-0.6B rows<=3072)
    ///    make too few blocks and `_big` regresses that GEMM ~19%.
    ///
    /// Otherwise fall back to the committed 256-thread BM=128 (2 CTAs/SM) or
    /// BM=64 kernel. `_big` is bit-identical to BM=128 (integer mma), so this is
    /// a pure perf gate. Measured on the RTX 4090: Mistral-7B Q4_K 4096 prefill
    /// 2588 -> 2827 tok/s (+9%), 8192 2246 -> 2343 (+4%); Qwen3-0.6B and the 512
    /// prefill stay on the committed kernel (no regression).
    fn gemm_i8mma_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32, u32) {
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("_big", 128, 128, 512)
        } else if n_tokens >= 256 {
            ("", 128, 64, 256)
        } else {
            ("_bm64", 64, 64, 256)
        }
    }

    /// QServe W4A8 CTA config for `M` tokens and `K` cols, mirroring
    /// `gemm_forward_cuda`'s host dispatch. Returns
    /// `(registry_key, CTA_M, CTA_N, CTA_K, num_warps, dynamic_smem_bytes)`.
    fn w4a8_config(m: usize, k: usize) -> (&'static str, u32, u32, u32, u32, u32) {
        if m > 128 {
            ("w4a8_gemm_m128", 128, 64, 64, 4, 41472)
        } else if m == 128 {
            if k <= 4096 {
                ("w4a8_gemm_m64_ksm", 64, 64, 64, 4, 25088)
            } else {
                ("w4a8_gemm_m64_klg", 64, 64, 128, 8, 37248)
            }
        } else {
            ("w4a8_gemm_m32", 32, 64, 128, 4, 24960)
        }
    }

    /// W4A8 (int4-weight x int8-activation) prefill GEMM: `y[t,row] = W·x[t]`.
    /// Non-default (routed only under `FORGE_GEMM=w4a8`). Consumes activations
    /// ALREADY quantized to per-token int8 (`a_i8` + `ascales`); the weight
    /// buffers are QServe-packed (`forge_formats::w4a8`). `rows` (N) must be a
    /// multiple of 64 and `cols` (K) a multiple of 128 (the kernel's group).
    #[allow(clippy::too_many_arguments)]
    pub fn w4a8_gemm(
        &self,
        y: &DevBuffer,
        a_i8: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        ascales: &DevBuffer,
        n_tokens: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !rows.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires rows % 64 == 0, got {rows}"
            )));
        }
        if !cols.is_multiple_of(128) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires cols % 128 == 0, got {cols}"
            )));
        }
        let (key, cta_m, cta_n, cta_k, warps, smem) = Self::w4a8_config(n_tokens, cols);
        if !cols.is_multiple_of(cta_k as usize) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 config {key} needs cols % {cta_k} == 0, got {cols}"
            )));
        }
        let gk = self.artifacts.get(key)?;
        let num_blocks_n = (rows as u32) / cta_n;
        let num_blocks_m = (n_tokens as u32).div_ceil(cta_m);
        let log_tile = if num_blocks_m >= 6 {
            3
        } else if num_blocks_m >= 3 {
            2
        } else if num_blocks_m >= 2 {
            1
        } else {
            0
        };
        let tile_shift = 1u32 << log_tile;
        let cfg = LaunchConfig {
            grid: (
                num_blocks_n * tile_shift,
                num_blocks_m.div_ceil(tile_shift),
                1,
            ),
            block: (32, warps, 1),
            shared_mem_bytes: smem,
        };
        let args = LaunchArgs::new()
            .buf(a_i8)
            .buf(qweight)
            .buf(s2_zeros)
            .buf(s2_scales)
            .buf(wscales)
            .buf(ascales)
            .buf(y)
            .scalar(n_tokens as i64)
            .scalar(rows as i64)
            .scalar(cols as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Per-token int8 activation quant + W4A8 GEMM in one call: quantizes the
    /// f16 activation `x` [n_tokens, cols] to symmetric int8 codes + per-token
    /// f16 scale (QServe layout) into grow-only scratch, then runs the int4-
    /// weight x int8-activation GEMM. `y` is f16 [n_tokens, rows]. `inv_smooth`
    /// is the per-input-channel SmoothQuant reciprocal `1/s` (f16 [cols]);
    /// activations are multiplied by it before the int8 quant, matching the
    /// packed weight's per-column `s` scaling. Pass an all-ones buffer for the
    /// identity (no smoothing). Both launches share `stream` (no explicit sync).
    /// Non-default (FORGE_GEMM=w4a8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        inv_smooth: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let need_codes = n_tokens * cols;
        let mut sc = self.w4a8_act.lock().expect("w4a8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.a_i8 = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.ascales = Some(self.device.alloc(
                n_tokens * 2,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.cap_tokens = n_tokens;
        }
        let a_i8 = sc.a_i8.as_ref().expect("a_i8 allocated");
        let ascales = sc.ascales.as_ref().expect("ascales allocated");

        let qk = self.artifacts.get("w4a8_quant_act")?;
        let block = (cols as u32).clamp(32, 1024);
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(x)
            .buf(a_i8)
            .buf(ascales)
            .buf(inv_smooth)
            .scalar(n_tokens as i64)
            .scalar(cols as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        self.w4a8_gemm(
            y, a_i8, qweight, s2_zeros, s2_scales, wscales, ascales, n_tokens, rows, cols, stream,
        )
    }

    /// Tile selection for the fp8 GEMM: `(suffix, BM, BN, block_threads)`. The
    /// f32 mma accumulate is exact across tile shapes (bit-identical, like the
    /// integer i8mma), so this is a pure perf gate; mirrors `gemm_i8mma_tile`.
    fn gemm_fp8_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32, u32) {
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("_big", 128, 128, 512)
        } else if n_tokens >= 256 {
            ("", 128, 64, 256)
        } else {
            ("_bm64", 64, 64, 256)
        }
    }

    /// Per-token e4m3 activation quant + fp8 (e4m3-weight × e4m3-activation)
    /// prefill GEMM in one call: quantizes f16 `x` [n_tokens, cols] to e4m3
    /// codes + per-token f32 scale into grow-only scratch, then runs the fp8
    /// tensor-core GEMM. `w` is e4m3 bytes [rows, cols], `wscales` the per-row
    /// f32 scale [rows]. `y` is f16 [n_tokens, rows]. Both launches share
    /// `stream` (no explicit sync). `cols % 32 == 0`. Non-default
    /// (FORGE_GEMM=fp8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8 requires cols % 32 == 0, got {cols}"
            )));
        }
        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant: one block per token, block-wide absmax
        // reduction over K (block <= 1024 to fit the shared reduction array).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        let (suffix, bm, bn, threads) = Self::gemm_fp8_tile(rows, n_tokens);
        let gk = self.artifacts.get(&format!("gemm_fp8_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(bn),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(wscales)
            .buf(xq)
            .buf(xs)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Przepakowuje zakres wierszy rezydentnej macierzy NVFP4 do E4M3 na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn pack_nvfp4_fp8(
        &self,
        output: &DevBuffer,
        output_scales: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        cols: usize,
        source_row_offset: usize,
        rows: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 16 || !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "pack_nvfp4_fp8 wymaga rows > 0 oraz cols >= 16 podzielnego przez 16, otrzymano [{rows}, {cols}]"
            )));
        }
        let source_end = source_row_offset.checked_add(rows).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie zakresu wierszy".into())
        })?;
        let output_bytes = rows.checked_mul(cols).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru wyjścia".into())
        })?;
        let packed_bytes = source_end.checked_mul(cols / 2).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru packed".into())
        })?;
        let scale_bytes = source_end.checked_mul(cols / 16).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru scales".into())
        })?;
        let output_scale_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru skal wyjściowych".into())
        })?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack_nvfp4_fp8: siatka przekracza u32".into()))?;
        if output.len() < output_bytes
            || output_scales.len() < output_scale_bytes
            || packed.len() < packed_bytes
            || scales.len() < scale_bytes
        {
            return Err(ForgeError::Kernel(
                "pack_nvfp4_fp8: bufor jest mniejszy od żądanego zakresu".into(),
            ));
        }
        let kernel = self.artifacts.get("pack_nvfp4_fp8")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(output_scales)
            .buf(packed)
            .buf(scales)
            .scalar(cols as i64)
            .scalar(source_row_offset as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(kernel, &cfg, &args, stream)
    }

    /// Pakuje wyrównane okno resident S0 N64/K128 do E4M3 bez row-major.
    pub fn pack_nvfp4_ct_s0_fp8(
        &self,
        output: &DevBuffer,
        output_scales: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        source_row_offset: usize,
        rows: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0
            || !rows.is_multiple_of(64)
            || !source_row_offset.is_multiple_of(64)
            || !inv_global_scale.is_finite()
        {
            return Err(ForgeError::Kernel(
                "pack NVFP4 CT wymaga wyrównanego okna N64 i skończonej skali".into(),
            ));
        }
        let source_end = source_row_offset.checked_add(rows).ok_or_else(|| {
            ForgeError::Kernel("pack NVFP4 CT: przepełnienie zakresu wierszy".into())
        })?;
        if source_end > weights.rows {
            return Err(ForgeError::Kernel(
                "pack NVFP4 CT: okno wykracza poza resident".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("pack NVFP4 CT output", &[rows, weights.cols], 1)?;
        let scale_bytes = checked_buffer_bytes("pack NVFP4 CT scales", &[rows], 4)?;
        if output.len() < output_bytes || output_scales.len() < scale_bytes {
            return Err(ForgeError::Kernel(
                "pack NVFP4 CT: bufor wyjściowy jest za mały".into(),
            ));
        }
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack NVFP4 CT: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get("pack_nvfp4_ct_s0_fp8")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(output_scales)
            .buf(weights.buffer)
            .scalar(weights.cols as i64)
            .scalar(source_row_offset as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przepakowuje rezydentną macierz Q8_0 do bloków GGUF NVFP4 na GPU.
    pub fn pack_q8_0_nvfp4_gguf(
        &self,
        output: &DevBuffer,
        source: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "pack_q8_0_nvfp4_gguf wymaga rows > 0 i cols % 64 == 0, otrzymano [{rows}, {cols}]"
            )));
        }
        let blocks = rows.checked_mul(cols / 64).ok_or_else(|| {
            ForgeError::Kernel("pack_q8_0_nvfp4_gguf: przepełnienie liczby bloków".into())
        })?;
        let output_bytes = blocks.checked_mul(36).ok_or_else(|| {
            ForgeError::Kernel("pack_q8_0_nvfp4_gguf: przepełnienie wyjścia".into())
        })?;
        let source_bytes = rows
            .checked_mul(cols / 32)
            .and_then(|count| count.checked_mul(34))
            .ok_or_else(|| {
                ForgeError::Kernel("pack_q8_0_nvfp4_gguf: przepełnienie wejścia".into())
            })?;
        if output.len() < output_bytes || source.len() < source_bytes {
            return Err(ForgeError::Kernel(
                "pack_q8_0_nvfp4_gguf: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let (blocks_per_cta, block_threads) = q8_nvfp4_pack_launch(self.device.caps().warp_size);
        let grid_x = u32::try_from(blocks.div_ceil(blocks_per_cta)).map_err(|_| {
            ForgeError::Kernel("pack_q8_0_nvfp4_gguf: siatka przekracza u32".into())
        })?;
        let kernel = self.artifacts.get("pack_q8_0_nvfp4_gguf")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(source)
            .scalar(blocks as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przepakowuje rezydentną macierz F16 do E4M3 na GPU.
    pub fn pack_f16_fp8(
        &self,
        output: &DevBuffer,
        output_scales: &DevBuffer,
        source: &DevBuffer,
        cols: usize,
        rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols == 0 {
            return Err(ForgeError::Kernel(
                "pack_f16_fp8 wymaga niezerowego kształtu".into(),
            ));
        }
        let elements = rows
            .checked_mul(cols)
            .ok_or_else(|| ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru".into()))?;
        let source_bytes = elements.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru źródła".into())
        })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru skal".into())
        })?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack_f16_fp8: siatka przekracza u32".into()))?;
        if output.len() < elements
            || output_scales.len() < scale_bytes
            || source.len() < source_bytes
        {
            return Err(ForgeError::Kernel(
                "pack_f16_fp8: bufor jest mniejszy od żądanego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("pack_f16_fp8")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(output_scales)
            .buf(source)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(kernel, &cfg, &args, stream)
    }

    /// Per-token e4m3 activation quant + Modular's multistage cp.async fp8 GEMM
    /// (one kernel per (rows,cols); docs/CODEGEN_PROOF.md Finding G). Same fp8
    /// weight pack + activation quant as `gemm_fp8`, but the GEMM is the deeply
    /// pipelined `multistage_gemm_kernel` (dynamic-M wrapper) that runs at
    /// 260–313 TFLOPS on Ada — 1.3–1.5× the CUDA MMQ — with the per-token ×
    /// per-row scale + f16 downcast fused into its epilogue (no extra HBM pass).
    /// Grid (ceil(rows/128), ceil(n_tokens/128)); block 128; dynamic smem 65536
    /// (the >48 KB opt-in the HAL sets automatically). Non-default
    /// (`FORGE_GEMM=fp8mod`); errors if no committed PTX matches (rows,cols).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular requires cols % 64 == 0, got {cols}"
            )));
        }
        let caps = self.device.caps();
        let use_bn256 = fp8_modular_bn256_capable(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
            caps.max_shared_mem_per_block,
            rows,
            cols,
            n_tokens,
            |name| self.artifacts.has(name),
        );
        let base_kernel_name = format!("gemm_fp8_mod_{rows}_{cols}");
        let kernel_name = if use_bn256 {
            fp8_modular_bn256_kernel(rows, cols).expect("kształt sprawdzony przez capability")
        } else {
            base_kernel_name.as_str()
        };
        let gk = self
            .artifacts
            .get(kernel_name)
            .map_err(|_| {
                ForgeError::Kernel(format!(
                    "gemm_fp8_modular: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
                ))
            })?;

        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant → e4m3 codes + f32 scale (shared with the
        // hand fp8 path).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        // multistage GEMM: y = diag(xs)·(xq·wᵀ)·diag(ws), fused epilogue. Params
        // mirror gemm_fp8_mod(y, a=xq, b=w, xs, ws, m=n_tokens).
        let (row_tile, block_threads, shared_mem_bytes) = if use_bn256 {
            (256, 256, FP8_MODULAR_BN256_SMEM as u32)
        } else {
            (128, 128, 65_536)
        };
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(row_tile),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (block_threads, 1, 1),
            shared_mem_bytes,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Grow the shared fp8 activation scratch to hold `n_tokens × cols` e4m3
    /// codes + `n_tokens` f32 scales. Called by the fused rmsnorm→fp8 path
    /// (which fills it) and the prequant GEMM (which reads it).
    fn fp8_act_ensure(&self, need_codes: usize, n_tokens: usize) -> Result<()> {
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        Ok(())
    }

    /// Fused RMSNorm → shared fp8 activation: writes the f16 normed row to
    /// `out_f16` AND the per-token e4m3 codes + f32 scale into the shared fp8
    /// activation scratch, so the following q/k/v (or gate/up) projections read
    /// ONE quantized activation via `gemm_fp8_modular_prequant` instead of
    /// re-quantizing per projection. The fp8mod analog of a fused norm→quant for the fp8mod
    /// path. `cols` is the hidden size (the projection K).
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused residual-add + RMSNorm → shared fp8 activation: `residual_io += x`,
    /// normed row to `out_f16`, shared per-token e4m3 codes + scale to scratch.
    /// See `rmsnorm_fp8_shared`.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_residual_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_residual_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Modular multistage fp8 GEMM over an EXTERNALLY prequantized activation:
    /// reads the shared fp8 activation scratch (`xq`/`xs`) that the preceding
    /// fused rmsnorm→fp8 emitted — NO per-projection quantize pass. `cols` (the
    /// projection K) must match the fused norm's hidden size that filled the
    /// scratch. Otherwise identical to `gemm_fp8_modular`. (`FORGE_GEMM=fp8mod`).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular_prequant(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular_prequant requires cols % 64 == 0, got {cols}"
            )));
        }
        let caps = self.device.caps();
        let use_bn256 = fp8_modular_bn256_capable(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
            caps.max_shared_mem_per_block,
            rows,
            cols,
            n_tokens,
            |name| self.artifacts.has(name),
        );
        let base_kernel_name = format!("gemm_fp8_mod_{rows}_{cols}");
        let kernel_name = if use_bn256 {
            fp8_modular_bn256_kernel(rows, cols).expect("kształt sprawdzony przez capability")
        } else {
            base_kernel_name.as_str()
        };
        let gk = self
            .artifacts
            .get(kernel_name)
            .map_err(|_| {
                ForgeError::Kernel(format!(
                    "gemm_fp8_modular_prequant: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
                ))
            })?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < n_tokens * cols || sc.cap_tokens < n_tokens {
            return Err(ForgeError::Kernel(
                "gemm_fp8_modular_prequant: shared fp8 activation scratch not sized \
                 by a preceding rmsnorm_fp8_shared"
                    .into(),
            ));
        }
        let xq = sc.xq.as_ref().expect("xq filled by fused norm");
        let xs = sc.xs.as_ref().expect("xs filled by fused norm");
        let (row_tile, block_threads, shared_mem_bytes) = if use_bn256 {
            (256, 256, FP8_MODULAR_BN256_SMEM as u32)
        } else {
            (128, 128, 65_536)
        };
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(row_tile),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (block_threads, 1, 1),
            shared_mem_bytes,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// `gemm_q4_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first superblock of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q4k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q6_K superblocks, x/y f16. Warp per row.
    pub fn gemv_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_q6_k_f16` over a row window of `w_q6k` (`w_byte_off` addresses the
    /// window's first row). One block per 8 output rows — used for the routed
    /// MoE down-projection so a single-token expert GEMV saturates the SMs
    /// instead of a 64-token GEMM tile with one live column.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q6k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q6_K expert GEMV whose expert row window is read ON DEVICE
    /// from `ids[sel]` (no host readback). Writes the per-expert `[rows]` output
    /// at `y[0..]`; global weight row = `ids[sel] * rows_per_expert + local_row`,
    /// bit-identical to `gemv_q6_k_f16_at` at that expert's byte offset.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_f16_gidx(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
        rows_per_expert: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k_f16_gidx requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_f16_gidx")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64)
            .scalar(rows_per_expert as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q6_K weights → f32 logits.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y_f32, y_off)?
            .buf(w_q6k)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q6_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q6_k_f16_at(y, w_q6k, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q6_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first superblock of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q6_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        // Rodzina `gemm_q6_k_f16` dekwantyzuje wagi do f16 i mnoży na mma.
        // Karta bez jednostki macierzowej idzie zamiast tego kaflem int8, co
        // wymaga wcześniejszej kwantyzacji aktywacji — tym zajmuje się
        // `gemm_i8mma_run`, a właściwy kafel wybiera `gemm_dot4_tile`.
        if self
            .gemm_dot4_tile("gemm_q6_k_i8mma", false, rows, n_tokens)
            .is_some()
        {
            return self.gemm_i8mma_run(
                "gemm_q6_k_i8mma",
                false,
                y,
                w_q6k,
                w_byte_off,
                x,
                rows,
                cols,
                n_tokens,
                stream,
            );
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q6_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q6k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Paged flash-decode attention. Layouts documented in attention.mojo.
    /// Wartości długości sekwencji i fizyczne identyfikatory stron są
    /// przygotowywane oraz walidowane przez właściciela cache przed wywołaniem.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_f16(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        // Okno przesuwne w tokenach; 0 = pełny kontekst.
        window: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        // Wariant split8 nie ma jeszcze maskowania okna, więc przy oknie
        // schodzimy na ścieżkę generyczną, która je obsługuje.
        // Wariant dzielony maskuje już okno przesuwne, więc obowiązuje także
        // dla warstw okiennych (40 z 48 warstw Gemmy 4).
        let split_suffix = if head_dim == 512 { "hd512" } else { "hd256" };
        let split8_available = self
            .artifacts
            .has(&format!("attn_decode_split8_f16_{split_suffix}"))
            && self
                .artifacts
                .has(&format!("attn_decode_split8_combine_f16_{split_suffix}"));
        let plan = attn_decode_plan(
            head_dim,
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
            split8_available,
        )?;
        let (grid_x, grid_y) = validate_attn_decode_f16(
            out.len(),
            parts.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            seq_lens.len(),
            n_seqs,
            n_q_heads,
            n_kv_heads,
            head_dim,
            page_size,
            max_pages,
            scale,
            matches!(plan, AttnDecodePlan::Split8Hd256 | AttnDecodePlan::Split8Hd512),
        )?;
        if matches!(
            plan,
            AttnDecodePlan::Split8Hd256 | AttnDecodePlan::Split8Hd512
        ) {
            let partial =
                self.artifacts
                    .get(&format!("attn_decode_split8_f16_{split_suffix}"))?;
            let partial_config = LaunchConfig {
                grid: (grid_x, grid_y, ATTN_HD256_SPLITS as u32),
                block: (ATTN_HD256_BLOCK, 1, 1),
                shared_mem_bytes: 0,
            };
            let partial_args = LaunchArgs::new()
                .buf(parts)
                .buf(q)
                .buf(k_cache)
                .buf(v_cache)
                .buf(page_table)
                .buf(seq_lens)
                .scalar(n_q_heads as i64)
                .scalar(n_kv_heads as i64)
                .scalar(page_size as i64)
                .scalar(max_pages as i64)
                .scalar(scale)
                .scalar(window as i64);
            self.device
                .launch(partial, &partial_config, &partial_args, stream)?;

            let combine = self
                .artifacts
                .get(&format!("attn_decode_split8_combine_f16_{split_suffix}"))?;
            let combine_config = LaunchConfig {
                grid: (grid_x, grid_y, 1),
                block: (32, 1, 1),
                shared_mem_bytes: 0,
            };
            let combine_args = LaunchArgs::new()
                .buf(out)
                .buf(parts)
                .scalar(n_q_heads as i64);
            return self
                .device
                .launch(combine, &combine_config, &combine_args, stream);
        }
        let name = match plan {
            AttnDecodePlan::Generic(name) => name,
            AttnDecodePlan::Split8Hd256 | AttnDecodePlan::Split8Hd512 => {
                unreachable!("wariant dzielony zwraca wynik przed fallbackiem")
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale)
            .scalar(window as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Dokładny batch flash-decode korzystający ze wspólnej tablicy stron.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_batch_exact_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("attn_decode_batch_exact_f16_hd256")?;
        let config = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn attn_verify_split8_f16_hd256(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<bool> {
        let caps = self.device.caps();
        if !verify_attn_split8_enabled(std::env::var("FORGE_VERIFY_ATTN_SPLIT8").ok().as_deref())
            || !matches!(n_tokens, 3 | 4)
            || caps.vendor != forge_types::Vendor::Nvidia
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
            || caps.max_shared_mem_per_block < 33_024
        {
            return Ok(false);
        }
        let partial_name = format!("attn_verify_split8_f16_hd256_t{n_tokens}");
        let combine_name = "attn_verify_split8_combine_f16_hd256";
        if !self.artifacts.has(&partial_name) || !self.artifacts.has(combine_name) {
            return Ok(false);
        }
        let (grid_y, combine_grid_y) = validate_attn_verify_split8(
            out.len(),
            parts.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            seq_lens.len(),
            n_tokens,
            n_q_heads,
            n_kv_heads,
            page_size,
            max_pages,
            scale,
        )?;
        let partial = self.artifacts.get(&partial_name)?;
        let args = LaunchArgs::new()
            .buf(parts)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale);
        self.device.launch(
            partial,
            &LaunchConfig {
                grid: (1, grid_y, 8),
                block: (256, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            stream,
        )?;
        let combine = self.artifacts.get(combine_name)?;
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_tokens as i64)
            .scalar(n_q_heads as i64);
        self.device.launch(
            combine,
            &LaunchConfig {
                grid: (1, combine_grid_y, 1),
                block: (32, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            stream,
        )?;
        Ok(true)
    }

    /// Rows each warp computes in the norm-recomputing fused decode kernels.
    /// Fewer blocks means fewer redundant per-block norm recomputes (h32/h/
    /// norm-weight traffic), which pays off once the projection is tall
    /// enough to keep the GPU busy anyway; per-row math is unchanged.
    fn fused_rows_per_warp(rows: usize) -> usize {
        (rows / 2048).clamp(1, 8)
    }

    /// Guard shared by the norm-recomputing fused decode kernels: the normed
    /// x is staged in a MAX_HIDDEN-element shared array (decode_fused.mojo).
    fn check_fused_hidden(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > 8192 || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= 8192, got {cols}"
            )));
        }
        Ok(())
    }

    /// Fused rmsnorm-recompute + Q8_0 GEMV (decode). ss_from_h16 selects the
    /// sum-of-squares source: the f16 residual h (layer 0, straight from the
    /// embedding gather) or the unrounded f32 mirror h32 (later layers).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q8_0")?;
        let k = self.artifacts.get("gemv_norm_q8_0_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + NVFP4 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 16, "gemv_norm_nvfp4")?;
        let k = self.artifacts.get("gemv_norm_nvfp4_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(packed)
            .buf(scales)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + NVFP4 GEMV z naturalnego układu S0.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_nvfp4_ct_s0_f16(
        &self,
        y: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inv_global_scale: f32,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(weights.cols, 128, "gemv_norm_nvfp4_ct_s0")?;
        let k = self.artifacts.get("gemv_norm_nvfp4_ct_s0_f16")?;
        let rpw = Self::fused_rows_per_warp(weights.rows);
        let cfg = LaunchConfig {
            grid: ((weights.rows as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights.buffer)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(weights.cols as i64)
            .scalar(weights.rows as i64)
            .scalar(inv_global_scale)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + f16 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 8, "gemv_norm_f16")?;
        let k = self.artifacts.get("gemv_norm_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q4_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q4_k")?;
        let k = self.artifacts.get("gemv_norm_q4_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q6_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q6_k")?;
        let k = self.artifacts.get("gemv_norm_q6_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q8_0 GEMV + SiLU (decode FFN).
    /// `w_q8` is the fused gate|up matrix (rows 0..inter gate, inter..2*inter
    /// up); one launch writes act = silu(gate) * up.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q8_0_f16(
        &self,
        act: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q8_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q8_0_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up NVFP4 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_nvfp4_f16(
        &self,
        act: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        inv_global_scale: f32,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 16, "gemv_norm_silu_nvfp4")?;
        let k = self.artifacts.get("gemv_norm_silu_nvfp4_f16")?;
        let rpw = 3usize;
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(packed)
            .buf(scales)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(inv_global_scale)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up S0 GEMV + SiLU.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_nvfp4_ct_s0_f16(
        &self,
        act: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        inv_global_scale: f32,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(weights.cols, 128, "gemv_norm_silu_nvfp4_ct_s0")?;
        if weights.rows != inter * 2 {
            return Err(ForgeError::Kernel(
                "gemv_norm_silu NVFP4 CT wymaga pełnego gate|up".into(),
            ));
        }
        let k = self.artifacts.get("gemv_norm_silu_nvfp4_ct_s0_f16")?;
        let rpw = 3usize;
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(weights.buffer)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(weights.cols as i64)
            .scalar(inter as i64)
            .scalar(inv_global_scale)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up f16 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 8, "gemv_norm_silu_f16")?;
        let k = self.artifacts.get("gemv_norm_silu_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q4_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_k_f16(
        &self,
        act: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q4_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q6_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q6_k_f16(
        &self,
        act: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q6_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q6_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q8_0 GEMV + residual add: h += f16(W·x) with rmsnorm_residual_f16's
    /// rounding; the unrounded f32 sum lands in h32 for the next norm.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q8_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q8_0_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// NVFP4 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_nvfp4_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_nvfp4 requires cols % 16 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_nvfp4_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(packed)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// NVFP4 S0 GEMV + residual add.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_nvfp4_ct_s0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        weights: Nvfp4CtS0View<'_>,
        x: &DevBuffer,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !weights.cols.is_multiple_of(128) {
            return Err(ForgeError::Kernel(
                "gemv_residual NVFP4 CT wymaga K128".into(),
            ));
        }
        let k = self.artifacts.get("gemv_residual_nvfp4_ct_s0_f16")?;
        let cfg = LaunchConfig {
            grid: ((weights.rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(weights.buffer)
            .buf(x)
            .scalar(weights.cols as i64)
            .scalar(weights.rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// f16 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(8) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_f16 requires cols % 8 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K GEMV + residual add (see gemv_residual_q8_0_f16). The kernel
    /// stages per-32-column x sums in shared memory (Q4K_MAX_SEGS bounds
    /// cols at 32768).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q6_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q6_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q6_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Final decode norm from the (h f16, h32 f32) residual pair: out =
    /// rmsnorm(h) * weight with the sum-of-squares taken from h32.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_h32_f16(
        &self,
        out: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_h32_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(h)
            .buf(h32)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split-context flash-decode attention with the qkv_post stage fused in
    /// as a per-block prologue (q/k RMSNorm + RoPE + paged k/v append). q/k/v
    /// are sections of the raw QKV GEMV output addressed by byte offsets;
    /// rotated q lives only in shared memory (the q section is never written
    /// back). Unnormalized per-split partials land in `parts`
    /// ([n_seqs, n_q_heads, n_splits, head_dim + 2] f32) for
    /// attn_decode_combine_f16. n_splits == 1 is bit-exact vs attn_decode_f16.
    /// `kv_dtype` selects the cache element type: the fp8 variant appends
    /// e4m3(f16(rope(k)))/e4m3(v) and widens cache reads exactly, so its
    /// math matches the f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        kv_dtype: DType,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "attn_decode_split")?;
        let name = match head_dim {
            64 => format!("attn_decode_split_{suffix}_hd64"),
            128 => format!("attn_decode_split_{suffix}_hd128"),
            // 512: warstwy globalne Gemmy 4.
            512 => format!("attn_decode_split_{suffix}_hd512"),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_split: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_in stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_in))
            .buf(k_norm.unwrap_or(q_in))
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split attention F16 dla GQA 4:1, współdzielący odczyt K/V między głowicami Q.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split_gqa4_f16_hd128(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let expected_q_heads = n_kv_heads.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: liczba głowic przekracza zakres".into())
        })?;
        if n_seqs == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || n_q_heads != expected_q_heads
            || page_size == 0
            || max_pages == 0
            || n_splits == 0
        {
            return Err(ForgeError::Kernel(format!(
                "attn_decode_split_gqa4 wymaga niezerowych wymiarów i GQA 4:1, otrzymano seqs={n_seqs}, heads={n_q_heads}:{n_kv_heads}, page={page_size}, max_pages={max_pages}, splits={n_splits}"
            )));
        }
        if !q_byte_off.is_multiple_of(2)
            || !k_byte_off.is_multiple_of(2)
            || !v_byte_off.is_multiple_of(2)
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4 wymaga offsetów wyrównanych do F16".into(),
            ));
        }
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        let q_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 q", &[n_seqs, n_q_heads, 128], 2)?;
        let kv_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 kv", &[n_seqs, n_kv_heads, 128], 2)?;
        let cache_page_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 cache",
            &[n_kv_heads, page_size, 128],
            2,
        )?;
        let page_table_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 page_table", &[n_seqs, max_pages], 4)?;
        let metadata_bytes = checked_buffer_bytes("attn_decode_split_gqa4 metadata", &[n_seqs], 4)?;
        let q_end = q_byte_off.checked_add(q_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu Q".into())
        })?;
        let k_end = k_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu K".into())
        })?;
        let v_end = v_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu V".into())
        })?;
        if parts.len() < parts_bytes
            || q_in.len() < q_end
            || k_in.len() < k_end
            || v_in.len() < v_end
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || page_table.len() < page_table_bytes
            || seq_lens.len() < metadata_bytes
            || positions.len() < metadata_bytes
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza zakres siatki".into())
        })?;
        let grid_z = u32::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza zakres siatki".into())
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_kv_heads_i64 = i64::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza ABI Mojo".into())
        })?;
        let page_size_i64 = i64::try_from(page_size).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: page_size przekracza ABI Mojo".into())
        })?;
        let max_pages_i64 = i64::try_from(max_pages).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: max_pages przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_split_gqa4_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, grid_z),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_in)
            .buf(k_in)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads_i64)
            .scalar(n_kv_heads_i64)
            .scalar(page_size_i64)
            .scalar(max_pages_i64)
            .scalar(n_splits_i64)
            .scalar(0i64)
            .scalar(0i64)
            .scalar(eps)
            .scalar(theta_base)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_split_f16 partials into the final [n_seqs,
    /// n_q_heads, head_dim] f16 output (one warp per head, split order).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_f16(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        head_dim: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_combine_f16_hd64",
            128 => "attn_decode_combine_f16_hd128",
            512 => "attn_decode_combine_f16_hd512",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads as i64)
            .scalar(n_splits as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Laczy partiale GQA hd128, przetwarzajac dwie glowice Q w jednym CTA.
    pub fn attn_decode_combine_gqa2_f16_hd128(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_seqs == 0 || n_q_heads == 0 || n_splits == 0 {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2 wymaga niezerowych wymiarów".into(),
            ));
        }
        let out_bytes =
            checked_buffer_bytes("attn_decode_combine_gqa2 out", &[n_seqs, n_q_heads, 128], 2)?;
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_combine_gqa2 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        if out.len() < out_bytes || parts.len() < parts_bytes {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_q_heads.div_ceil(2)).map_err(|_| {
            ForgeError::Kernel(
                "attn_decode_combine_gqa2: n_q_heads przekracza zakres siatki".into(),
            )
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_combine_gqa2_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads_i64)
            .scalar(n_splits_i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Commit T tokens already resident in the paged f16 K/V cache
    /// (positions base_pos..base_pos+T) into the rotational low-bit store
    /// (rotquant.mojo: WHT rotate + 3/4-bit pack + per-(token,head) f16 scale).
    /// Grid (T, n_kv_heads); one thread per (token, head) vector.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot_from_cache(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot_from_cache", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rotate+quant+pack a batch of T linear (rope'd) K/V rows
    /// ([n_tokens, n_kv_heads, head_dim] f16) into the paged rotational store at
    /// the absolute positions in `positions` ([T] i32, one per token), writing
    /// the rotated f16 vectors into the residual ring at `pos % ring_slots` (the
    /// recent-window fidelity copy the decode attention reads directly). Reading
    /// the position from a device buffer keeps decode launches graph-capturable.
    /// Grid (T, n_kv_heads).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        k_in: &DevBuffer,
        k_in_byte_off: usize,
        v_in: &DevBuffer,
        v_in_byte_off: usize,
        page_table: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        ring_slots: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf_at(k_in, k_in_byte_off)?
            .buf_at(v_in, v_in_byte_off)?
            .buf(page_table)
            .buf(positions)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(ring_slots as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split-K rotational low-bit decode attention over the dual-region store:
    /// reads the residual f16 ring for the recent `ring_slots` positions (rotated
    /// f16, no unpack) and the packed 3/4-bit store for everything older. Rotates
    /// q once (block-cooperative WHT), scores in rotated space
    /// ((R·q)·k_rot = q·k), and writes each (seq, head, split) an UNNORMALIZED
    /// rotated partial to `parts` ([n_seqs, n_q_heads, n_splits, head_dim + 2]
    /// f32). `attn_decode_combine_rot` merges the splits and inverse-rotates.
    /// `ring_slots == 0` degrades to packed-only. Grid (n_seqs, n_q_heads,
    /// n_splits); block ATTN_BLOCK.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_rot(
        &self,
        parts: &DevBuffer,
        q: &DevBuffer,
        q_byte_off: usize,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        ring_slots: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_decode_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q, q_byte_off)?
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(ring_slots as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_rot's per-split rotated partials into the final
    /// [n_seqs, n_q_heads, head_dim] f16 output and inverse-rotate once per head
    /// (one warp per head, split order). Head_dim {64,128}.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_rot(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        head_dim: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_combine_rot_hd64",
            128 => "attn_decode_combine_rot_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine_rot: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads as i64)
            .scalar(n_splits as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rotational low-bit causal prefill attention over the packed store: query
    /// token t attends positions 0..base_pos+t. Packed-only (the residual ring's
    /// recent window would be overwritten within a chunk). Grid (T, n_q_heads),
    /// one warp per (token, head).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_rot(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_prefill_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kernel name for a rotational specialization: `<base>_hd{64,128}_b{3,4}`.
    fn rot_kernel_name(base: &str, head_dim: usize, bits: u8) -> Result<String> {
        if bits != 3 && bits != 4 {
            return Err(ForgeError::Unsupported(format!(
                "rotational KV supports 3 or 4 bits, got {bits}"
            )));
        }
        match head_dim {
            64 | 128 => Ok(format!("{base}_hd{head_dim}_b{bits}")),
            other => Err(ForgeError::Unsupported(format!(
                "rotational KV: head_dim {other} has no compiled specialization"
            ))),
        }
    }

    /// Column bound of the dp4a kernels that quantize x from global memory
    /// into shared int8 (plain + residual variants; X_MAX in decode_dp4a.mojo).
    pub const DP4A_MAX_COLS: usize = 16384;

    fn check_dp4a_cols(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > Self::DP4A_MAX_COLS || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= {}, got {cols}",
                Self::DP4A_MAX_COLS
            )));
        }
        Ok(())
    }

    /// Q8_0 GEMV with int8-quantized activations (q8_1) and dp4a dots.
    /// Not bit-exact vs gemv_q8_0_f16 (activation quantization rounding).
    pub fn gemv_q8_0_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_q8_0_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K GEMV with int8-quantized activations (q8_1) and dp4a dots.
    pub fn gemv_q4_k_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_q8_0_dp4a_f16` nad oknem wierszy `w_q8` (`w_byte_off` wskazuje
    /// pierwszy wiersz okna). Pozwala batchowej ścieżce dla JEDNEGO tokena
    /// uruchomić ten sam kernel co dekod jednosekwencyjny — kafel GEMM dopełniany
    /// do >=64 tokenów kwantyzuje aktywacje inaczej, co dawało trwałą różnicę
    /// logitów między ścieżkami.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q8_0_dp4a_f16_at(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_q8_0_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_q4_k_dp4a_f16` over a row window of `w_q4k` (`w_byte_off` addresses
    /// the window's first row). Used for the routed MoE gate/up projections so a
    /// single-token expert GEMV launches per-row blocks instead of a starved
    /// 64-token GEMM tile.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_f16_at(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q4k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q4_K expert GEMV whose expert row window is read ON DEVICE
    /// from `ids[sel]` (no host readback of the router selection). Writes the
    /// per-expert `[rows]` output at `y[0..]`; the global weight row is
    /// `ids[sel] * rows_per_expert + local_row`, so the result is bit-identical
    /// to `gemv_q4_k_dp4a_f16_at` at byte offset `ids[sel]*rows_per_expert`.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_f16_gidx(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
        rows_per_expert: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a_f16_gidx")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16_gidx")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64)
            .scalar(rows_per_expert as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K logit GEMV (f32 out) with dp4a dots.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_out_f32(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a_out_f32")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_out_f32")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y_f32, y_off)?
            .buf(w_q4k)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Quantize a small decode batch (T<=16) to q8_1 in the dedicated
    /// `qk_batch` scratch and return the guard. The scratch is always sized
    /// for the T=16 ceiling so buffer addresses stay stable once the decode
    /// graphs are captured (no events — all users share the model stream's
    /// ordering).
    fn qk_batch_quantize(
        &self,
        x: &DevBuffer,
        x_byte_off: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<std::sync::MutexGuard<'_, QkBatchScratch>> {
        let need_codes = checked_buffer_bytes("dp4a batch codes", &[16, cols], 1)?;
        let need_blocks = 16 * (cols / 32);
        let mut sc = self
            .qk_batch
            .lock()
            .map_err(|_| ForgeError::Kernel("dp4a batch scratch poisoned".into()))?;
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_blocks < need_blocks {
            sc.xd = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.xsm = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_blocks = need_blocks;
        }
        let qk = self.artifacts.get("quantize_act_q8_1")?;
        let quant_blocks = u32::try_from(n_tokens * (cols / 32))
            .map_err(|_| ForgeError::Kernel("dp4a batch: liczba bloków przekracza u32".into()))?;
        let qcfg = LaunchConfig::linear(quant_blocks, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(sc.xq.as_ref().expect("xq allocated"))
            .buf(sc.xd.as_ref().expect("xd allocated"))
            .buf(sc.xsm.as_ref().expect("xsm allocated"))
            .buf_at(x, x_byte_off)?
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;
        Ok(sc)
    }

    /// Weight-stationary small-batch dp4a GEMV for Q4_K/Q6_K batched decode
    /// (T = 2/4/8/16): quantizes the activation once (`prepare_q8_1`), then a
    /// single weight sweep serves every token. Returns `false` (caller keeps
    /// the token-tile GEMM path) when the shape, batch or device is
    /// unsupported or the kernels are absent.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_qk_dp4a_batch_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        q6: bool,
        stream: &Stream,
    ) -> Result<bool> {
        if !matches!(n_tokens, 2 | 4 | 8 | 16)
            || rows == 0
            || cols == 0
            || !cols.is_multiple_of(256)
        {
            return Ok(false);
        }
        let caps = self.device.caps();
        if caps.vendor != forge_types::Vendor::Nvidia || caps.warp_size != 32 {
            return Ok(false);
        }
        let name = if q6 {
            format!("gemv_q6_k_dp4a_batch_b{n_tokens}")
        } else {
            format!("gemv_q4_k_dp4a_batch_b{n_tokens}")
        };
        let Ok(gk) = self.artifacts.get(&name) else {
            return Ok(false);
        };
        let block_bytes = if q6 { 210 } else { 144 };
        let weight_bytes = checked_buffer_bytes("dp4a batch weights", &[rows, cols / 256], block_bytes)?;
        let weight_end = w_byte_off
            .checked_add(weight_bytes)
            .ok_or_else(|| ForgeError::Kernel("dp4a batch: przepełnienie wag".into()))?;
        let output_bytes = checked_buffer_bytes("dp4a batch output", &[n_tokens, rows], 2)?;
        if y.len() < output_bytes || w.len() < weight_end {
            return Err(ForgeError::Kernel(
                "dp4a batch: bufor wyjścia lub wag jest za mały".into(),
            ));
        }
        let sc = self.qk_batch_quantize(x, 0, cols, n_tokens, stream)?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(4), 1, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(sc.xq.as_ref().expect("xq allocated"))
            .buf(sc.xd.as_ref().expect("xd allocated"));
        if !q6 {
            args = args.buf(sc.xsm.as_ref().expect("xsm allocated"));
        }
        let args = args
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(&gk, &cfg, &args, stream)?;
        Ok(true)
    }

    /// Pack a resident GGUF projection (row window) straight to e4m3 fp8 on
    /// the GPU: one 256-thread block per output row computes the row absmax
    /// over on-the-fly dequantized values and encodes `x * 448/absmax`.
    /// Bit-identical to the CPU `pack_fp8_host` path (golden-gated). `fmt`:
    /// the GGUF quant of `w` — Q4_K, Q6_K or Q8_0.
    #[allow(clippy::too_many_arguments)]
    pub fn pack_gguf_fp8(
        &self,
        codes: &DevBuffer,
        scales: &DevBuffer,
        w: &DevBuffer,
        w_row_off: usize,
        rows: usize,
        cols: usize,
        fmt: QuantKind,
        stream: &Stream,
    ) -> Result<()> {
        let (name, blk_elems, blk_bytes) = match fmt {
            QuantKind::Q4K => ("pack_q4_k_fp8", 256, 144),
            QuantKind::Q6K => ("pack_q6_k_fp8", 256, 210),
            QuantKind::Q8_0 => ("pack_q8_0_fp8", 32, 34),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "pack_gguf_fp8: nieobsługiwany format {other:?}"
                )))
            }
        };
        if rows == 0 || cols == 0 || !cols.is_multiple_of(blk_elems) {
            return Err(ForgeError::Kernel(format!(
                "pack_gguf_fp8 wymaga rows > 0 i cols podzielnego przez {blk_elems}, otrzymano rows={rows}, cols={cols}"
            )));
        }
        let w_byte_off = w_row_off * (cols / blk_elems) * blk_bytes;
        let weight_bytes =
            checked_buffer_bytes("pack fp8 weights", &[rows, cols / blk_elems], blk_bytes)?;
        let weight_end = w_byte_off
            .checked_add(weight_bytes)
            .ok_or_else(|| ForgeError::Kernel("pack fp8: przepełnienie wag".into()))?;
        let code_bytes = checked_buffer_bytes("pack fp8 codes", &[rows], cols)?;
        if w.len() < weight_end || codes.len() < code_bytes || scales.len() < rows * 4 {
            return Err(ForgeError::Kernel(
                "pack fp8: bufor wag, kodów lub skal jest za mały".into(),
            ));
        }
        let gk = self.artifacts.get(name)?;
        let rows_u32 = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack fp8: rows przekracza u32".into()))?;
        let cfg = LaunchConfig {
            grid: (rows_u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(codes)
            .buf(scales)
            .buf_at(w, w_byte_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Weight-stationary small-batch Q8_0 GEMM (T = 2/4/8/16) over the shared
    /// q8_1 activation quant — same batched-decode contract as
    /// `gemm_qk_dp4a_batch_at` (returns `false` to keep the token-tile path).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_small_batch_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        if !matches!(n_tokens, 2 | 4 | 8 | 16)
            || rows == 0
            || cols == 0
            || !cols.is_multiple_of(32)
        {
            return Ok(false);
        }
        let caps = self.device.caps();
        if caps.vendor != forge_types::Vendor::Nvidia || caps.warp_size != 32 {
            return Ok(false);
        }
        let Ok(gk) = self.artifacts.get(&format!("gemm_q8_0_i8mma_b{n_tokens}")) else {
            return Ok(false);
        };
        let weight_bytes = checked_buffer_bytes("q8_0 batch weights", &[rows, cols / 32], 34)?;
        let weight_end = w_byte_off
            .checked_add(weight_bytes)
            .ok_or_else(|| ForgeError::Kernel("q8_0 batch: przepełnienie wag".into()))?;
        let output_bytes = checked_buffer_bytes("q8_0 batch output", &[n_tokens, rows], 2)?;
        if y.len() < output_bytes || w.len() < weight_end {
            return Err(ForgeError::Kernel(
                "q8_0 batch: bufor wyjścia lub wag jest za mały".into(),
            ));
        }
        let sc = self.qk_batch_quantize(x, 0, cols, n_tokens, stream)?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(sc.xq.as_ref().expect("xq allocated"))
            .buf(sc.xd.as_ref().expect("xd allocated"))
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(&gk, &cfg, &args, stream)?;
        Ok(true)
    }

    /// Fused rmsnorm-recompute + Q8_0 dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q8_0_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q8_0_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q4_K dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_k_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q4_k_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q6_K dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q6_k_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q6_k_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q8_0 dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q8_0_dp4a_f16(
        &self,
        act: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q8_0_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q4_K dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_k_dp4a_f16(
        &self,
        act: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_k_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q6_K dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q6_k_dp4a_f16(
        &self,
        act: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q6_k_dp4a_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q8_0 dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q8_0_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_residual_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q8_0_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q6_K dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q6_k_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_residual_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q6_k_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q6_K logit GEMV (f32 out) with dp4a dots.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_dp4a_out_f32(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q6_k_dp4a_out_f32")?;
        let k = self.artifacts.get("gemv_q6_k_dp4a_out_f32")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y_f32, y_off)?
            .buf(w_q6k)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_k_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_residual_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q4_k_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// In-place repetition penalty over `n_ids` distinct token ids staged in
    /// `ids` (i32). Callers must deduplicate: the kernel applies the penalty
    /// once per listed id.
    pub fn sample_penalize_f32(
        &self,
        logits: &DevBuffer,
        ids: &DevBuffer,
        n_ids: usize,
        penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_f32")?;
        let cfg = LaunchConfig::linear(n_ids as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(ids)
            .scalar(n_ids as i64)
            .scalar(penalty);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Nakłada kary z kompaktowego histogramu i wybiera greedy w jednym launchu.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_penalized_argmax_f32(
        &self,
        out: &DevBuffer,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.validate_penalty_histogram(
            Some(out),
            logits,
            ids,
            counts,
            n_ids,
            vocab,
            repetition_penalty,
            frequency_penalty,
            presence_penalty,
        )?;
        let kernel = self.artifacts.get("penalized_argmax_f32")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(logits)
            .buf(ids)
            .buf(counts)
            .scalar(n_ids as i64)
            .scalar(vocab as i64)
            .scalar(repetition_penalty)
            .scalar(frequency_penalty)
            .scalar(presence_penalty);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Nakłada kary z histogramu unikalnych IDs przed równoległym samplingiem.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_penalize_histogram_f32(
        &self,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.validate_penalty_histogram(
            None,
            logits,
            ids,
            counts,
            n_ids,
            vocab,
            repetition_penalty,
            frequency_penalty,
            presence_penalty,
        )?;
        let kernel = self.artifacts.get("penalize_histogram_f32")?;
        let config = LaunchConfig::linear(n_ids as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(ids)
            .buf(counts)
            .scalar(n_ids as i64)
            .scalar(vocab as i64)
            .scalar(repetition_penalty)
            .scalar(frequency_penalty)
            .scalar(presence_penalty);
        self.device.launch(kernel, &config, &args, stream)
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_penalty_histogram(
        &self,
        out: Option<&DevBuffer>,
        logits: &DevBuffer,
        ids: &DevBuffer,
        counts: &DevBuffer,
        n_ids: usize,
        vocab: usize,
        repetition_penalty: f32,
        frequency_penalty: f32,
        presence_penalty: f32,
    ) -> Result<()> {
        if n_ids == 0 || vocab == 0 || n_ids > vocab {
            return Err(ForgeError::Kernel(
                "fused sampling wymaga niepustego histogramu nie większego od słownika".into(),
            ));
        }
        let logits_bytes = checked_buffer_bytes("sampling logits", &[vocab], 4)?;
        let histogram_bytes = checked_buffer_bytes("sampling histogram", &[n_ids], 4)?;
        if out.is_some_and(|buffer| buffer.len() < 8)
            || logits.len() < logits_bytes
            || ids.len() < histogram_bytes
            || counts.len() < histogram_bytes
        {
            return Err(ForgeError::Kernel(
                "bufor fused sampling jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        if !repetition_penalty.is_finite()
            || repetition_penalty <= 0.0
            || !frequency_penalty.is_finite()
            || !presence_penalty.is_finite()
        {
            return Err(ForgeError::Kernel(
                "parametry kar fused sampling muszą być skończone".into(),
            ));
        }
        #[cfg(debug_assertions)]
        {
            // Kopie histogramu mogą oczekiwać na nieblokującym streamie modelu;
            // synchroniczny odczyt hostowy nie może ich wyprzedzić.
            self.device.synchronize()?;
            let mut host_ids = vec![0u8; histogram_bytes];
            let mut host_counts = vec![0u8; histogram_bytes];
            self.device.read(ids, 0, &mut host_ids)?;
            self.device.read(counts, 0, &mut host_counts)?;
            let mut unique = std::collections::HashSet::with_capacity(n_ids);
            for (id, count) in host_ids.chunks_exact(4).zip(host_counts.chunks_exact(4)) {
                let id = i32::from_le_bytes(id.try_into().expect("fragment i32"));
                let count = i32::from_le_bytes(count.try_into().expect("fragment i32"));
                if id < 0 || id as usize >= vocab || count <= 0 || !unique.insert(id) {
                    return Err(ForgeError::Kernel(
                        "histogram kar wymaga unikalnych IDs w zakresie vocab i dodatnich liczników"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Greedy argmax over f32 logits; the winning index lands in the first
    /// 4 bytes of `out` (i32) and its logprob slot (f32, 0 for greedy) in the
    /// next 4. Ties resolve to the lowest index like a sequential CPU scan.
    /// `scratch_vals`/`scratch_idx` hold the per-block partials
    /// (>= SAMPLE_SCRATCH_PAIRS entries each).
    pub fn sample_argmax_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        if n_blocks > SAMPLE_SCRATCH_PAIRS {
            return Err(ForgeError::Unsupported(format!(
                "sample_argmax: vocab {vocab} exceeds scratch capacity"
            )));
        }
        let kp = self.artifacts.get("argmax_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("argmax_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(n_blocks as i64);
        self.device.launch(kf, &cfg, &args, stream)
    }

    /// Categorical draw over f32 logits: top-k (k <= SAMPLE_MAX_TOPK)
    /// selection, temperature softmax, min-p floor, top-p cut, then a
    /// deterministic counter-hash draw on (seed, step). The sampled id (i32)
    /// lands in the first 4 bytes of `out`, its top-k-softmax logprob (f32)
    /// in the next 4.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_topk_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        k: usize,
        inv_t: f32,
        top_p: f32,
        min_p: f32,
        seed: u64,
        step: u64,
        stream: &Stream,
    ) -> Result<()> {
        if k == 0 || k > SAMPLE_MAX_TOPK {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: k {k} outside 1..={SAMPLE_MAX_TOPK}"
            )));
        }
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        let kp = self.artifacts.get("topk_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64)
            .scalar(k as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("topk_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar((n_blocks * k) as i64)
            .scalar(k as i64)
            .scalar(inv_t)
            .scalar(top_p)
            .scalar(min_p)
            .scalar(seed)
            .scalar(step);
        self.device.launch(kf, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q5_K blocks, x/y f16. Warp per row.
    pub fn gemv_q5_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_K weights → f32 logits.
    pub fn gemv_q5_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q5_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_k_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q5_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q5_k")?;
        let k = self.artifacts.get("gemv_norm_q5_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q5_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_k_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q5_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q5_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q3_K blocks, x/y f16. Warp per row.
    pub fn gemv_q3_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q3_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q3_K weights → f32 logits.
    pub fn gemv_q3_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q3_k_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q3_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q3_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q3_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q3_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q3_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q3_k_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q3_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q3_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q3_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q3_k")?;
        let k = self.artifacts.get("gemv_norm_q3_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q3_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q3_k_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q3_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q3_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q3_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q3_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q3_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q2_K blocks, x/y f16. Warp per row.
    pub fn gemv_q2_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q2_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q2_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q2_K weights → f32 logits.
    pub fn gemv_q2_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q2_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q2_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q2_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q2_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q2_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q2_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q2_k_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q2_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q2_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q2_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q2_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q2_k")?;
        let k = self.artifacts.get("gemv_norm_q2_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q2_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q2_k_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q2_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q2_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q2_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q2_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q2_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q2_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q4_0 blocks, x/y f16. Warp per row.
    pub fn gemv_q4_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_0 weights → f32 logits.
    pub fn gemv_q4_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_0_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q4_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_0_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q4_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_0_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        // Karty bez jednostki macierzowej: kafel int8 na `v_dot4_i32_i8`.
        // Rodzina `gemm_q4_0_f16` dekwantyzuje do f16 i mnozy na mma, wiec tam
        // jej nie ma. `gemm_i8mma_run` dokłada kwantyzację aktywacji.
        if self
            .gemm_dot4_tile("gemm_q4_0_i8mma", false, rows, n_tokens)
            .is_some()
        {
            return self.gemm_i8mma_run(
                "gemm_q4_0_i8mma",
                false,
                y,
                w,
                w_byte_off,
                x,
                rows,
                cols,
                n_tokens,
                stream,
            );
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_0_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q4_0 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q4_0")?;
        let k = self.artifacts.get("gemv_norm_q4_0_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q4_0 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_0_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q4_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_0_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_0 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_0_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q4_1 blocks, x/y f16. Warp per row.
    pub fn gemv_q4_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_1_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_1 weights → f32 logits.
    pub fn gemv_q4_1_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_1_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_1_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q4_1 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_1_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q4_1_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_1_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_1_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q4_1 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q4_1")?;
        let k = self.artifacts.get("gemv_norm_q4_1_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q4_1 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_1_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q4_1")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_1_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_1 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_1_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_1_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q5_0 blocks, x/y f16. Warp per row.
    pub fn gemv_q5_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_0 weights → f32 logits.
    pub fn gemv_q5_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_0_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q5_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_0_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_0_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_0_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q5_0 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q5_0")?;
        let k = self.artifacts.get("gemv_norm_q5_0_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q5_0 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_0_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q5_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_0_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q5_0 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_0_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q5_1 blocks, x/y f16. Warp per row.
    pub fn gemv_q5_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_1_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_1 weights → f32 logits.
    pub fn gemv_q5_1_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_1_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_1_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q5_1 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_1_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_1_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_1_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_1_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q5_1 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q5_1")?;
        let k = self.artifacts.get("gemv_norm_q5_1_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q5_1 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_1_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q5_1")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_1_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q5_1 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_1_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_1_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML IQ4_NL blocks, x/y f16. Warp per row.
    pub fn gemv_iq4_nl_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_nl_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ4_NL weights → f32 logits.
    pub fn gemv_iq4_nl_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_nl_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_nl_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ4_NL weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_nl_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq4_nl_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq4_nl_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_nl_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq4_nl_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ4_NL GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq4_nl_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_iq4_nl")?;
        let k = self.artifacts.get("gemv_norm_iq4_nl_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ4_NL GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq4_nl_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_iq4_nl")?;
        let k = self.artifacts.get("gemv_norm_silu_iq4_nl_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ4_NL GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq4_nl_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq4_nl_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML IQ4_XS blocks, x/y f16. Warp per row.
    pub fn gemv_iq4_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_xs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ4_XS weights → f32 logits.
    pub fn gemv_iq4_xs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_xs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_xs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ4_XS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq4_xs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq4_xs_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_xs_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq4_xs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ4_XS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq4_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq4_xs")?;
        let k = self.artifacts.get("gemv_norm_iq4_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ4_XS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq4_xs_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq4_xs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq4_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ4_XS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq4_xs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq4_xs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML MXFP4 blocks, x/y f16. Warp per row.
    pub fn gemv_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over MXFP4 weights → f32 logits.
    pub fn gemv_mxfp4_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_mxfp4_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over MXFP4 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_mxfp4_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_mxfp4_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self
            .artifacts
            .get(&format!("gemm_mxfp4_gguf_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + MXFP4 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_mxfp4_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up MXFP4 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_mxfp4_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_silu_mxfp4_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// MXFP4 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_mxfp4_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_mxfp4_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML IQ2_XS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_XS weights → f32 logits.
    pub fn gemv_iq2_xs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_XS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_xs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_xs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xs_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_xs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_XS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_xs")?;
        let k = self.artifacts.get("gemv_norm_iq2_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ2_XS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_xs_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_xs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_XS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_xs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_xs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ2_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_S weights → f32 logits.
    pub fn gemv_iq2_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_s_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_s")?;
        let k = self.artifacts.get("gemv_norm_iq2_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ2_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_s_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ3_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq3_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ3_S weights → f32 logits.
    pub fn gemv_iq3_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ3_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq3_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq3_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_s_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq3_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ3_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq3_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq3_s")?;
        let k = self.artifacts.get("gemv_norm_iq3_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ3_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq3_s_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq3_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq3_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ3_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq3_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq3_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML IQ2_XXS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xxs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_XXS weights → f32 logits.
    pub fn gemv_iq2_xxs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xxs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xxs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_XXS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_xxs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_xxs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xxs_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_xxs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_XXS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_xxs")?;
        let k = self.artifacts.get("gemv_norm_iq2_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ2_XXS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_xxs_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_xxs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_XXS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_xxs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_xxs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ3_XXS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq3_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_xxs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ3_XXS weights → f32 logits.
    pub fn gemv_iq3_xxs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_xxs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_xxs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ3_XXS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq3_xxs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq3_xxs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_xxs_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq3_xxs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ3_XXS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq3_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq3_xxs")?;
        let k = self.artifacts.get("gemv_norm_iq3_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ3_XXS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq3_xxs_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq3_xxs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq3_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ3_XXS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq3_xxs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq3_xxs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ1_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq1_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ1_S weights → f32 logits.
    pub fn gemv_iq1_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ1_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq1_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq1_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_s_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq1_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ1_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq1_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq1_s")?;
        let k = self.artifacts.get("gemv_norm_iq1_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ1_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq1_s_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq1_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq1_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ1_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq1_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq1_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ1_M superblocks, x/y f16. Warp per row.
    pub fn gemv_iq1_m_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_m_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ1_M weights → f32 logits.
    pub fn gemv_iq1_m_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_m_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_m_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ1_M weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_m_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq1_m_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq1_m_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_m_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq1_m_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ1_M GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq1_m_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq1_m")?;
        let k = self.artifacts.get("gemv_norm_iq1_m_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up IQ1_M GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq1_m_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq1_m")?;
        let k = self.artifacts.get("gemv_norm_silu_iq1_m_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ1_M GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq1_m_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq1_m_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// MoE router: for each of `n_tokens` rows of `x` (f16, [n_tokens, hidden])
    /// compute logits `x · gate_inp` over `n_expert` experts (f16 router,
    /// [n_expert, hidden]), softmax over all experts, then select the top-k.
    /// Writes `ids` ([n_tokens, top_k] i32) and `weights` ([n_tokens, top_k]
    /// f32). `norm_topk` renormalizes the selected weights to sum 1.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_router_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_inp: &DevBuffer,
        n_tokens: usize,
        hidden: usize,
        n_expert: usize,
        top_k: usize,
        norm_topk: bool,
        stream: &Stream,
    ) -> Result<()> {
        // Shared-memory staging caps (mirror MOE_MAX_* in moe.mojo).
        if hidden > 8192 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: hidden {hidden} exceeds kernel cap 8192"
            )));
        }
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: n_expert {n_expert} exceeds kernel cap 256"
            )));
        }
        let k = self.artifacts.get("moe_router_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(x)
            .buf(gate_inp)
            .scalar(hidden as i64)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(norm_topk as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fold one routed expert's f16 output into a token's FFN accumulator:
    /// `acc += scale * src` over `n` elements (or `acc = scale * src` when
    /// `init`). Both buffers are addressed by byte offset so a per-token row of
    /// a batched accumulator can be targeted.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        scale: f32,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .scalar(scale)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Like `moe_scale_add_f16` but the router weight is read ON DEVICE from
    /// `weights[sel]`, so no host readback of the routing weights is needed.
    /// For the shared expert, pass its device-resident sigmoid gate scale as
    /// `weights` with `sel = 0`.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_gidx_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        weights: &DevBuffer,
        sel: usize,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_gidx_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .buf(weights)
            .scalar(sel as i64)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out[0] = sigmoid(in[0])`: turns the shared-expert gate logit (f16) into
    /// a device-resident f32 scale so `moe_scale_add_gidx_f16` can fold the
    /// shared expert without a per-layer host round-trip.
    pub fn moe_sigmoid_f16_to_f32(
        &self,
        out: &DevBuffer,
        input: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_sigmoid_f16_to_f32")?;
        let cfg = LaunchConfig::linear(1, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(input);
        self.device.launch(k, &cfg, &args, stream)
    }

    // --- ONNX subset f32 ops (forge-onnx interpreter) -----------------------

    /// General 1-D convolution (group=1, dilation=1), all f32. `x` [in_ch, in_t],
    /// `w` [out_ch, in_ch, ksize], optional `bias` [out_ch], `out` [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        bias: Option<&DevBuffer>,
        in_ch: usize,
        in_t: usize,
        out_ch: usize,
        out_t: usize,
        ksize: usize,
        stride: usize,
        pad: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_f32")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(BLOCK), out_ch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent bias still needs a valid device pointer (never read); `out`
        // stands in, mirroring the qkv_post launcher convention.
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(w)
            .buf(bias.unwrap_or(out))
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_ch as i64)
            .scalar(out_t as i64)
            .scalar(ksize as i64)
            .scalar(stride as i64)
            .scalar(pad as i64)
            .scalar(if bias.is_some() { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = max(x, 0) over n f32 elements.
    pub fn relu_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("relu_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sigmoid(x) over n f32 elements.
    pub fn sigmoid_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sigmoid_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = a + b, same shape, n f32 elements (broadcasting done host-side).
    pub fn add_f32(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        b: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("add_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(b).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = x^e (elementwise, scalar exponent) over n f32 elements.
    pub fn pow_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        e: f32,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("pow_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(e).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sqrt(x) over n f32 elements.
    pub fn sqrt_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sqrt_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[o, i] = mean over the reduced axis of x viewed as [outer, axis, inner].
    pub fn reduce_mean_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        outer: usize,
        axis: usize,
        inner: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("reduce_mean_f32")?;
        let cfg = LaunchConfig::linear((outer * inner) as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .scalar(outer as i64)
            .scalar(axis as i64)
            .scalar(inner as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Single-direction, batch-1 ONNX LSTM (gate order i,o,f,c). Shapes are
    /// direction/batch-squeezed by the caller: `x` [seq, input], `w` [4h, input],
    /// `r` [4h, hidden], `b` [8h], `h0`/`c0` [hidden]; `y` [seq, hidden],
    /// `yh`/`yc` [hidden].
    #[allow(clippy::too_many_arguments)]
    pub fn lstm_f32(
        &self,
        y: &DevBuffer,
        yh: &DevBuffer,
        yc: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        r: &DevBuffer,
        b: &DevBuffer,
        h0: &DevBuffer,
        c0: &DevBuffer,
        seq: usize,
        input_size: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Shared recurrent state is sized for LSTM_MAX_HIDDEN = 512 in the kernel.
        if hidden > 512 {
            return Err(ForgeError::Kernel(format!(
                "lstm_f32: hidden {hidden} exceeds shared-state capacity (512)"
            )));
        }
        let k = self.artifacts.get("lstm_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (hidden as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(yh)
            .buf(yc)
            .buf(x)
            .buf(w)
            .buf(r)
            .buf(b)
            .buf(h0)
            .buf(c0)
            .scalar(seq as i64)
            .scalar(input_size as i64)
            .scalar(hidden as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}

#[cfg(test)]
mod nvfp4_gguf_dispatch_tests {
    use super::{
        attn_decode_plan, delta_state_layout_dispatch, dense_prefill_artifacts_capable,
        dense_prefill_backend_capable, ensure_prepared_q8_usable, has_delta_value_key_artifacts,
        has_hybrid_prefill_b2_artifacts, has_hybrid_prefill_t128_artifacts,
        has_nvfp4_gguf_tile_artifacts, hybrid_prefill_nvfp4_artifact_chunk_limit,
        lock_prepared_q8_scratch, fp8_modular_bn256_capable,
        nvfp4_gguf_dispatch as nvfp4_gguf_dispatch_impl,
        nvfp4_gguf_layout_dispatch, nvfp4_ct_s0_manual_capable,
        nvfp4_ct_split_pipeline_supported, q8_nvfp4_pack_launch,
        raw_nvfp4_dp4a_supported, resolve_prepared_q8_marker, validate_attn_decode_f16,
        validate_attn_prefill_fa_f16_hd256, validate_attn_verify_split8,
        validate_deltanet_gated_step_f16, validate_f32_byte_offset,
        validate_kv_append_batch_segmented_f16, validate_kv_append_batch_segmented_masked_f16,
        validate_nvfp4_ct_b1_extents, validate_nvfp4_ct_repack_extents,
        verify_attn_split8_enabled, AttnDecodePlan, DeltaStateLayout, DensePrefillLogitsKind,
        Kernels, Nvfp4GgufLayout, PrequantScratch, Q8PreparedProjection,
        HYBRID_PREFILL_B2_ARTIFACTS, HYBRID_PREFILL_T128_ARTIFACTS, PREPARED_Q8_GEMM_LAUNCHES,
        PREPARED_Q8_RECORD_FAILURES, PREPARED_Q8_SYNC_FAILURES, NVFP4_CT_S0_ARTIFACTS,
    };
    use forge_hal::cpu::CpuDevice;
    use forge_hal::cuda::{CudaDevice, PoolSizes};
    use forge_hal::{Device, Pool};
    use forge_types::{ForgeError, MemKind, Vendor};
    use half::f16;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

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
        assert!(!fp8_modular_bn256_capable(
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
            validate_nvfp4_ct_repack_extents(9216, 4096, 512, 128, 128, 64, 64)
                .unwrap(),
            1
        );
        assert!(
            validate_nvfp4_ct_repack_extents(9216 + 256, 4096, 512, 128, 128, 64, 64)
                .is_err()
        );
        assert!(
            validate_nvfp4_ct_repack_extents(9216, 4095, 512, 128, 128, 64, 64)
                .is_err()
        );
        assert!(
            validate_nvfp4_ct_repack_extents(
                9216,
                4096,
                512,
                usize::MAX - 63,
                128,
                64,
                usize::MAX - 63,
            )
            .is_err()
        );
    }

    #[test]
    fn nvfp4_ct_b1_waliduje_okno_i_overflow() {
        assert_eq!(
            validate_nvfp4_ct_b1_extents(128 + 256, 256, 128, 128, 64, 64, 1.0)
                .unwrap(),
            8
        );
        assert!(validate_nvfp4_ct_b1_extents(127, 256, 128, 128, 64, 64, 1.0).is_err());
        assert!(
            validate_nvfp4_ct_b1_extents(
                128,
                256,
                usize::MAX,
                128,
                usize::MAX - 63,
                64,
                1.0,
            )
            .is_err()
        );
        assert!(
            validate_nvfp4_ct_b1_extents(128, 256, 128, 128, 64, 64, f32::NAN).is_err()
        );
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

    #[test]
    fn split8_hd256_wymaga_nvidia_warp32_i_kompletu_artefaktow() {
        assert_eq!(
            attn_decode_plan(256, forge_types::Vendor::Nvidia, 32, 1024, true).unwrap(),
            AttnDecodePlan::Split8Hd256
        );
        for plan in [
            attn_decode_plan(256, forge_types::Vendor::Amd, 64, 1024, true).unwrap(),
            attn_decode_plan(256, forge_types::Vendor::Apple, 32, 1024, true).unwrap(),
            attn_decode_plan(256, forge_types::Vendor::Nvidia, 64, 1024, true).unwrap(),
            attn_decode_plan(256, forge_types::Vendor::Nvidia, 32, 128, true).unwrap(),
            attn_decode_plan(256, forge_types::Vendor::Nvidia, 32, 1024, false).unwrap(),
        ] {
            assert_eq!(plan, AttnDecodePlan::Generic("attn_decode_f16_hd256"));
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

    #[test]
    fn prefill_t128_wymaga_pelnego_zestawu_artefaktow() {
        assert!(has_hybrid_prefill_t128_artifacts(|_| true));
        for missing in HYBRID_PREFILL_T128_ARTIFACTS {
            assert!(!has_hybrid_prefill_t128_artifacts(|name| name != missing));
        }
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
        assert!(dense_prefill_backend_capable(
            forge_types::Vendor::Nvidia,
            32,
            1024
        ));
        assert!(!dense_prefill_backend_capable(
            forge_types::Vendor::Amd,
            64,
            1024
        ));
        assert!(!dense_prefill_backend_capable(
            forge_types::Vendor::Nvidia,
            64,
            1024
        ));
        assert!(!dense_prefill_backend_capable(
            forge_types::Vendor::Nvidia,
            32,
            128
        ));
        for (head_dim, attention) in [
            (128, "attn_prefill_fa_segmented_f16_hd128"),
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
            assert!(!dense_prefill_artifacts_capable(
                128,
                16,
                DensePrefillLogitsKind::Q8_0 { rows, cols: 4096 },
                |name| name != required,
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
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(false, |_| true),
            16
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
        assert_eq!(
            hybrid_prefill_nvfp4_artifact_chunk_limit(false, |name| {
                name != "gemm_nvfp4_gguf_f16_b3_nvidia"
            }),
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
        let device = match CudaDevice::new(
            0,
            PoolSizes {
                weights: 16 << 20,
                kv_cache: 4 << 20,
                activations: 64 << 20,
                kv_page_size: 256 << 10,
            },
        ) {
            Ok(device) => device,
            Err(error) => {
                eprintln!("pominięto parity prepared Q8 T32/T128 bez CUDA: {error}");
                return;
            }
        };
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
        let device = match CudaDevice::new(
            0,
            PoolSizes {
                weights: 16 << 20,
                kv_cache: 4 << 20,
                activations: 64 << 20,
                kv_page_size: 256 << 10,
            },
        ) {
            Ok(device) => device,
            Err(error) => {
                eprintln!("pominięto parity fused Q8 triplet bez CUDA: {error}");
                return;
            }
        };
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
        let device = match CudaDevice::new(
            0,
            PoolSizes {
                weights: 16 << 20,
                kv_cache: 4 << 20,
                activations: 16 << 20,
                kv_page_size: 256 << 10,
            },
        ) {
            Ok(device) => device,
            Err(error) => {
                eprintln!("pominięto test fault injection prepared Q8 bez CUDA: {error}");
                return;
            }
        };
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
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
        let _serialized = PREPARED_Q8_SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
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
        let large =
            nvfp4_gguf_dispatch_impl(2048, 5120, 6144, true, true, true, true, 32, 1024).unwrap();
        assert_eq!(large.kernel, "gemm_nvfp4_gguf_mma_f16_bm128_bn128");
        assert_eq!((large.row_tile, large.block_threads), (128, 256));

        let m128 =
            nvfp4_gguf_dispatch_impl(128, 5120, 5120, true, true, true, true, 32, 1024).unwrap();
        assert_eq!(m128.kernel, "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1");
        assert_eq!((m128.row_tile, m128.block_threads), (64, 256));

        let regression =
            nvfp4_gguf_dispatch_impl(128, 17408, 5120, true, true, true, true, 32, 1024).unwrap();
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
        assert!(raw_nvfp4_dp4a_supported(true, 32));
        assert!(!raw_nvfp4_dp4a_supported(true, 64));
        assert!(!raw_nvfp4_dp4a_supported(false, 32));
        assert!(!raw_nvfp4_dp4a_supported(false, 64));
    }

    #[test]
    fn packer_uzywa_logicznych_grup_dla_warp64() {
        assert_eq!(q8_nvfp4_pack_launch(32), (2, 256));
        assert_eq!(q8_nvfp4_pack_launch(64), (1, 64));
    }
}
