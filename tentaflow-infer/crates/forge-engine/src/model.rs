// ===== File: model.rs — single-sequence forward pass (batched prefill + graphed decode) =====
// Decode runs one token per step through a captured CUDA graph; prefill runs
// whole prompt chunks through batched GEMM/attention kernels (same math, T
// tokens at once). The residual stream is carried through fused
// rmsnorm_residual chaining, so no standalone add kernel exists: each fusion
// adds the previous sublayer's output and produces the next sublayer's
// normed input.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use forge_formats::{LayerKind, PoolingType};
use forge_hal::DEVICE_ALLOC_ALIGN;
use forge_hal::{DevBuffer, Device, Event, ExecGraph, Pool, Stream};
pub use forge_kernels::Nvfp4GgufLayout;
use forge_kernels::{
    nvfp4_ct_physical_m, DeltaStateLayout, DensePrefillLogitsKind, Kernels, Nvfp4CtProjection,
    Nvfp4CtS0View, Nvfp4GgufQ8Projection, Q8ActPrepared, Q8PreparedProjection,
};
use forge_types::{DType, ForgeError, MemKind, QuantKind, Result, Vendor};
use half::f16;

use crate::kv::{HybridStateLease, KvCache, KvConfig, KvLayerMap, KvQuant, SeqKv};
use crate::expert_spill::ExpertSpill;
use crate::moe_residency::{
    ExpertStack, Migration, MoeLayerView, MoeResidencyState, Projection, ProjectionId,
    MOE_RESIDENCY_INTERVAL,
};
use crate::mtp::{MtpDraftState, MtpEmbedding};
use crate::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use crate::tier::{KvTierConfig, TierManager, STAGE_SLOTS};
use crate::weight_tier::{TieredWeightDevice, WeightResidency};
use crate::weights::{
    AttnWeights, CalibStats, DeltaNetWeights, DevWeight, Fp8Layer, Fp8Weight, GateUpWeights,
    LayerFfn, LayerMixer, ModelWeights, MoeFfn, NvFp4CtLayoutPolicy, NvFp4CtStorage, QkvWeights,
    W4A8Weight,
};

/// Largest token count `prefill_chunk` accepts per call; callers split longer
/// prompts. Bounds the persistent prefill scratch allocation.
pub const MAX_PREFILL_CHUNK: usize = 1024;
const HYBRID_PREFILL_PORTABLE_CHUNK: usize = 16;
const HYBRID_PREFILL_LEGACY_CHUNK: usize = 32;
const HYBRID_PREFILL_AUTO_CHUNK: usize = 128;
const HYBRID_PREFILL_ACTIVATION_RESERVE: usize = 64 * 1024 * 1024;
const HYBRID_LAYER_MAJOR_MAX_TOKENS: usize = 4096;
const HYBRID_HOST_STAGING_SLOTS: usize = 3;
const NVFP4_CT_QKV_ROWS: usize = 6144;
const NVFP4_CT_GATE_UP_ROWS: usize = 22528;
const NVFP4_CT_WORKSPACE_SPLITS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Nvfp4CtBufferPlan {
    physical_m: usize,
    matrix_cap: usize,
    qkv_elems: usize,
    gate_up_elems: usize,
    workspace_elems: usize,
}

fn nvfp4_ct_projection_for_shape(
    rows: usize,
    cols: usize,
) -> Option<Nvfp4CtProjection> {
    match (rows, cols) {
        (6144, 4096) => Some(Nvfp4CtProjection::Qkv),
        (4096, 4096) => Some(Nvfp4CtProjection::Output),
        (22528, 4096) => Some(Nvfp4CtProjection::GateUp),
        (4096, 11264) => Some(Nvfp4CtProjection::Down),
        _ => None,
    }
}

fn nvfp4_ct_dimensions_capable(
    hidden: usize,
    q_dim: usize,
    kv_dim: usize,
    inter: usize,
) -> bool {
    hidden == 4096 && q_dim == 4096 && kv_dim == 1024 && inter == 11264
}

/// Największy fizyczny kafel, jaki może wystąpić dla batcha do `cap` sekwencji.
/// Bufory segmentowane są stałe w instancji modelu, więc muszą pomieścić kafel
/// M32 zawsze, gdy `cap` w ogóle pozwala trafić w bucket 32.
fn nvfp4_ct_plan_physical_m(cap: usize) -> usize {
    if cap >= 24 {
        32
    } else {
        16
    }
}

fn nvfp4_ct_buffer_plan(
    cap: usize,
    model_capable: bool,
) -> Option<Nvfp4CtBufferPlan> {
    if !model_capable {
        return None;
    }
    let physical_m = nvfp4_ct_plan_physical_m(cap);
    Some(Nvfp4CtBufferPlan {
        physical_m,
        matrix_cap: cap.max(physical_m),
        qkv_elems: physical_m.checked_mul(NVFP4_CT_QKV_ROWS)?,
        gate_up_elems: physical_m.checked_mul(NVFP4_CT_GATE_UP_ROWS)?,
        workspace_elems: NVFP4_CT_WORKSPACE_SPLITS
            .checked_mul(physical_m)?
            .checked_mul(NVFP4_CT_QKV_ROWS)?,
    })
}

/// Wynik próby zbudowania rezydentnych paczek FP8. Rozdzielenie braku wsparcia
/// od zbyt małej puli wag pozwala wywołującemu zgłosić operatorowi konkretny,
/// naprawialny powód zamiast jednego zbiorczego "niedostępne".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fp8PackOutcome {
    Built,
    Unsupported,
    PoolShortfall { required: usize, available: usize },
}

impl Fp8PackOutcome {
    #[must_use]
    pub fn built(self) -> bool {
        self == Self::Built
    }
}

fn cleanup_after_error<T, F>(result: Result<T>, cleanup: F) -> Result<T>
where
    F: FnOnce(),
{
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            cleanup();
            Err(error)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HybridPrefillChunkConfig {
    Auto,
    Explicit(usize),
}

fn parse_hybrid_prefill_chunk_config(value: Option<&str>) -> Result<HybridPrefillChunkConfig> {
    let Some(value) = value else {
        return Ok(HybridPrefillChunkConfig::Auto);
    };
    if value.eq_ignore_ascii_case("auto") {
        return Ok(HybridPrefillChunkConfig::Auto);
    }
    let chunk = value.parse::<usize>().map_err(|_| {
        ForgeError::Unsupported(format!(
            "FORGE_HYBRID_PREFILL_CHUNK wymaga auto albo liczby całkowitej 3..={MAX_PREFILL_CHUNK}"
        ))
    })?;
    if !(3..=MAX_PREFILL_CHUNK).contains(&chunk) {
        return Err(ForgeError::Unsupported(format!(
            "FORGE_HYBRID_PREFILL_CHUNK={chunk} jest poza zakresem 3..={MAX_PREFILL_CHUNK}"
        )));
    }
    Ok(HybridPrefillChunkConfig::Explicit(chunk))
}

fn hybrid_layer_major_prefill_requested() -> bool {
    std::env::var("FORGE_HYBRID_LAYER_MAJOR_PREFILL").map_or(true, |value| value != "0")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HybridLayerMajorAttention {
    Exact,
    Prefill,
    Flash,
}

fn hybrid_layer_major_attention() -> Result<HybridLayerMajorAttention> {
    match std::env::var("FORGE_HYBRID_LAYER_MAJOR_ATTN")
        .ok()
        .as_deref()
    {
        None | Some("fa") => Ok(HybridLayerMajorAttention::Flash),
        Some("exact") => Ok(HybridLayerMajorAttention::Exact),
        Some("prefill") => Ok(HybridLayerMajorAttention::Prefill),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_HYBRID_LAYER_MAJOR_ATTN wymaga exact/prefill/fa, otrzymano {value}"
        ))),
    }
}

fn hybrid_layer_major_persistent_scan_requested() -> Result<bool> {
    match std::env::var("FORGE_HYBRID_LAYER_MAJOR_SCAN")
        .ok()
        .as_deref()
    {
        None | Some("auto") | Some("persistent") => Ok(true),
        Some("chunked") => Ok(false),
        Some(value) => Err(ForgeError::Scheduler(format!(
            "FORGE_HYBRID_LAYER_MAJOR_SCAN wymaga auto/persistent/chunked, otrzymano {value}"
        ))),
    }
}

fn hybrid_layer_major_tiled_prepare_requested() -> bool {
    std::env::var("FORGE_HYBRID_LAYER_MAJOR_DELTA_PREPARE")
        .map_or(true, |value| value != "segmented")
}

fn hybrid_prefill_chunk_config_for_model(
    is_hybrid: bool,
    value: Option<&str>,
) -> Result<HybridPrefillChunkConfig> {
    if is_hybrid {
        parse_hybrid_prefill_chunk_config(value)
    } else {
        Ok(HybridPrefillChunkConfig::Auto)
    }
}

fn resolve_hybrid_prefill_chunk_size(
    config: HybridPrefillChunkConfig,
    auto_extended_capable: bool,
    auto_t128_capable: bool,
    contains_nvfp4: bool,
    auto_chunk_limit: usize,
    nvfp4_chunk_limit: usize,
    prepared_q8_tiled_capable: bool,
) -> Result<usize> {
    if contains_nvfp4 && nvfp4_chunk_limit < 3 {
        return Err(ForgeError::Unsupported(
            "backend lub artefakty nie obsługują minimalnego wykonania T3 dla NVFP4".into(),
        ));
    }
    if let HybridPrefillChunkConfig::Explicit(chunk) = config {
        if chunk < 3 {
            return Err(ForgeError::Unsupported(
                "jawny chunk hybrydowego prefill musi mieć co najmniej T3".into(),
            ));
        }
        if contains_nvfp4 && chunk > nvfp4_chunk_limit {
            return Err(ForgeError::Unsupported(format!(
                "FORGE_HYBRID_PREFILL_CHUNK={chunk} przekracza limit NVFP4 backendu lub artefaktów {nvfp4_chunk_limit}"
            )));
        }
        if !contains_nvfp4 && chunk >= 32 && !prepared_q8_tiled_capable {
            return Err(ForgeError::Unsupported(format!(
                "FORGE_HYBRID_PREFILL_CHUNK={chunk} wymaga kaflowego Q8 i8mma, którego backend nie ma"
            )));
        }
        return Ok(chunk);
    }
    if !contains_nvfp4 {
        // Kwantyzacja aktywacji dla T>=32 wchodzi na kafle i8mma, których poza
        // NVIDIĄ nie ma — bez tego warunku Auto wybierało chunk, który zaraz
        // wywracał prefill na nieobsługiwanym kernelu.
        return Ok(if prepared_q8_tiled_capable {
            HYBRID_PREFILL_LEGACY_CHUNK
        } else {
            HYBRID_PREFILL_PORTABLE_CHUNK
        });
    }
    let policy_limit = if auto_extended_capable {
        auto_chunk_limit.min(if auto_t128_capable {
            HYBRID_PREFILL_AUTO_CHUNK
        } else {
            HYBRID_PREFILL_LEGACY_CHUNK
        })
    } else {
        auto_chunk_limit.min(HYBRID_PREFILL_PORTABLE_CHUNK)
    };
    [128, 32, 16, 8, 4, 3]
        .into_iter()
        .find(|&chunk| chunk <= policy_limit)
        .ok_or_else(|| {
            ForgeError::Unsupported(
                "backend, artefakty lub budżet nie obsługują minimalnego chunka Auto T3 NVFP4"
                    .into(),
            )
        })
}

/// Czy backend uciągnie rozszerzone chunki hybrydowego prefillu (T32/T128).
///
/// Warunkiem jest JEDNOSTKA MACIERZOWA i wave/warp 32 — ścieżka layer-major
/// stoi na kaflach 16x16x16, które na NVIDII daje `mma`, a na RDNA3+ WMMA.
/// Wcześniej stał tu warunek `vendor == Nvidia`, przez co każdy model qwen35 na
/// Radeonie liczył prefill chunkami po 16 tokenów, czyli czytał komplet wag
/// 64 razy na 1024-tokenowy prompt.
fn hybrid_prefill_t128_backend_capable(vendor: Vendor, warp_size: u32) -> bool {
    matches!(vendor, Vendor::Nvidia | Vendor::Amd) && warp_size == 32
}

fn hybrid_prefill_nvfp4_chunk_limit(
    vendor: Vendor,
    warp_size: u32,
    max_threads_per_block: u32,
) -> usize {
    if warp_size == 0 || warp_size > max_threads_per_block {
        return 0;
    }
    // Karty z jednostką macierzową i falą 32 dzielą tę samą politykę: sufit
    // wynika z rozmiaru bloku, a nie z producenta. Czy kafle T32/T128 REALNIE
    // istnieją, rozstrzyga osobno limit artefaktów.
    if matches!(vendor, Vendor::Nvidia | Vendor::Amd) && warp_size == 32 {
        return if max_threads_per_block >= 512 {
            MAX_PREFILL_CHUNK
        } else if max_threads_per_block >= 64 {
            8
        } else {
            2
        };
    }
    if warp_size
        .checked_mul(16)
        .is_some_and(|threads| threads <= max_threads_per_block)
    {
        16
    } else if warp_size
        .checked_mul(8)
        .is_some_and(|threads| threads <= max_threads_per_block)
    {
        8
    } else {
        4
    }
}

fn grow_prefill_lanes_transactional(
    kv: &mut KvCache,
    seqs: &mut [&mut SeqKv],
    n_tokens: usize,
) -> Result<()> {
    let old_lengths = seqs.iter().map(|seq| seq.len).collect::<Vec<_>>();
    for seq in seqs.iter_mut() {
        for _ in 0..n_tokens {
            if let Err(error) = kv.grow(seq) {
                for (seq, old_len) in seqs.iter_mut().zip(old_lengths) {
                    kv.rollback(seq, old_len);
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct PrefillSeqSnapshot {
    len: usize,
    tokens_len: usize,
    prefilled_len: usize,
}

#[derive(Default)]
struct KvReusePoison {
    reason: Option<String>,
}

impl KvReusePoison {
    fn poison(&mut self, reason: String) {
        if self.reason.is_none() {
            self.reason = Some(reason);
        }
    }

    fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn is_poisoned(&self) -> bool {
        self.reason.is_some()
    }

    fn ensure_healthy(&self) -> Result<()> {
        match self.reason() {
            Some(reason) => Err(ForgeError::Device(format!(
                "model zatrzymany po fatalnym błędzie synchronizacji KV: {reason}"
            ))),
            None => Ok(()),
        }
    }
}

fn fatal_kv_synchronize(
    poison: &mut KvReusePoison,
    context: &str,
    synchronize: impl FnOnce() -> Result<()>,
) -> Result<()> {
    poison.ensure_healthy()?;
    match synchronize() {
        Ok(()) => Ok(()),
        Err(error) => {
            poison.poison(format!("{context}: {error}"));
            Err(error)
        }
    }
}

fn settle_kv_operation<T>(
    operation: Result<T>,
    context: &str,
    synchronize: impl FnOnce() -> Result<()>,
) -> Result<T> {
    match synchronize() {
        Ok(()) => operation,
        Err(sync_error) => match operation {
            Ok(_) => Err(sync_error),
            Err(operation_error) => Err(ForgeError::Device(format!(
                "{context}: błąd operacji ({operation_error}); błąd synchronizacji ({sync_error})"
            ))),
        },
    }
}

fn restore_prefill_seq_snapshots(
    kv: &mut KvCache,
    seqs: &mut [&mut SeqKv],
    snapshots: &[PrefillSeqSnapshot],
) {
    for (seq, snapshot) in seqs.iter_mut().zip(snapshots) {
        kv.rollback(seq, snapshot.len);
        seq.tokens.truncate(snapshot.tokens_len);
        seq.prefilled_len = snapshot.prefilled_len;
    }
}

fn run_dense_prefill_transaction<C, T>(
    context: &mut C,
    seqs: &mut [&mut SeqKv],
    operation: impl FnOnce(&mut C, &mut [&mut SeqKv]) -> Result<T>,
    synchronize: impl FnOnce(&mut C) -> Result<()>,
    restore: impl FnOnce(&mut C, &mut [&mut SeqKv], &[PrefillSeqSnapshot]),
) -> Result<T> {
    let snapshots = seqs
        .iter()
        .map(|seq| PrefillSeqSnapshot {
            len: seq.len,
            tokens_len: seq.tokens.len(),
            prefilled_len: seq.prefilled_len,
        })
        .collect::<Vec<_>>();
    match operation(context, seqs) {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Err(sync_error) = synchronize(context) {
                return Err(ForgeError::Device(format!(
                    "dense prefill zakończył się błędem ({error}); synchronizacja przed rollbackiem także się nie udała ({sync_error})"
                )));
            }
            restore(context, seqs, &snapshots);
            Err(error)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HybridPrefillScratchShape {
    hidden: usize,
    q_dim: usize,
    kv_dim: usize,
    inter: usize,
    conv_dim: usize,
    value_dim: usize,
    n_v_heads: usize,
    d_state: usize,
    d_conv: usize,
    delta_layers: usize,
    max_pages_per_seq: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HybridPrefillScratchEstimate {
    device_bytes: usize,
    pinned_bytes: usize,
}

fn checked_scratch_bytes(name: &str, dimensions: &[usize], element_bytes: usize) -> Result<usize> {
    dimensions
        .iter()
        .try_fold(element_bytes, |bytes, &dimension| {
            bytes.checked_mul(dimension).ok_or_else(|| {
                ForgeError::Scheduler(format!("przepełnienie estymatora scratchu {name}"))
            })
        })
}

fn checked_scratch_sum(name: &str, values: impl IntoIterator<Item = usize>) -> Result<usize> {
    values.into_iter().try_fold(0usize, |total, value| {
        total.checked_add(value).ok_or_else(|| {
            ForgeError::Scheduler(format!("przepełnienie estymatora scratchu {name}"))
        })
    })
}

fn hybrid_verify_delta_scratch_instances(cap: usize, delta_layers: usize) -> usize {
    if cap > 4 {
        usize::from(delta_layers > 0)
    } else {
        delta_layers
    }
}

fn hybrid_verify_dedicated_z_bytes(shape: HybridPrefillScratchShape, cap: usize) -> Result<usize> {
    if cap > 4 {
        checked_scratch_bytes("z", &[cap, shape.value_dim], 2)
    } else {
        Ok(0)
    }
}

fn hybrid_verify_attention_parts_bytes(
    cap: usize,
    n_heads: usize,
    head_dim: usize,
) -> Result<usize> {
    let partial_stride = head_dim
        .checked_add(4)
        .ok_or_else(|| ForgeError::Scheduler("przepełnienie stride atencji verifiera".into()))?;
    checked_scratch_bytes(
        "części atencji verifiera",
        &[cap.min(4), n_heads, 8, partial_stride],
        4,
    )
}

fn hybrid_verify_scratch_estimate(
    shape: HybridPrefillScratchShape,
    cap: usize,
) -> Result<HybridPrefillScratchEstimate> {
    let q_full_cols = hybrid_q_full_cols(shape.q_dim, shape.conv_dim, shape.hidden);
    let wide_cols = shape.q_dim.max(shape.value_dim);
    let conv_elems = shape
        .conv_dim
        .checked_mul(shape.d_conv.saturating_sub(1))
        .ok_or_else(|| ForgeError::Scheduler("przepełnienie estymatora okna conv".into()))?;
    let state_elems = shape
        .n_v_heads
        .checked_mul(shape.d_state)
        .and_then(|value| value.checked_mul(shape.d_state))
        .ok_or_else(|| ForgeError::Scheduler("przepełnienie estymatora stanu DeltaNet".into()))?;
    let mut device = vec![
        4,
        checked_scratch_bytes("visible lengths", &[cap], 4)?,
        checked_scratch_bytes("q full", &[cap, q_full_cols], 2)?,
        checked_scratch_bytes("q, gate i gated", &[3, cap, wide_cols], 2)?,
        checked_scratch_bytes("alpha i beta raw", &[2, cap, shape.n_v_heads], 2)?,
        checked_scratch_bytes("g i beta", &[2, cap, shape.n_v_heads], 4)?,
        8,
    ];
    if cap > 4 {
        device.extend([
            hybrid_verify_dedicated_z_bytes(shape, cap)?,
            checked_scratch_bytes("recurrence output", &[cap, shape.value_dim], 2)?,
            4,
        ]);
    } else {
        device.extend([
            checked_scratch_bytes("mixed qkv", &[cap, shape.conv_dim], 2)?,
            checked_scratch_bytes("z, q32, k32 i v", &[4, cap, shape.value_dim], 2)?,
            checked_scratch_bytes("recurrence output i norm", &[2, cap, shape.value_dim], 2)?,
            checked_scratch_bytes("state checkpoints", &[cap, state_elems], 4)?,
            checked_scratch_bytes(
                "retained state checkpoints",
                &[shape.delta_layers, cap, state_elems],
                4,
            )?,
        ]);
    }
    device.push(checked_scratch_bytes(
        "warstwy DeltaNet",
        &[
            hybrid_verify_delta_scratch_instances(cap, shape.delta_layers),
            checked_scratch_sum(
                "warstwy DeltaNet",
                [
                    checked_scratch_bytes("conv initial", &[conv_elems], 2)?,
                    checked_scratch_bytes("conv checkpoints", &[cap, conv_elems], 2)?,
                    if cap > 4 {
                        4
                    } else {
                        checked_scratch_bytes("state initial", &[state_elems], 4)?
                    },
                ],
            )?,
        ],
        1,
    )?);
    let staging_slot = checked_scratch_sum(
        "staging hostowy",
        [
            checked_scratch_bytes("embedding staging", &[cap, shape.hidden], 2)?,
            checked_scratch_bytes("page table staging", &[2, shape.max_pages_per_seq], 4)?,
            checked_scratch_bytes("wektory staging", &[5, cap], 4)?,
            24,
        ],
    )?;
    Ok(HybridPrefillScratchEstimate {
        device_bytes: checked_scratch_sum("verifier device", device)?,
        pinned_bytes: checked_scratch_sum(
            "verifier pinned",
            [
                8,
                checked_scratch_bytes(
                    "potrójny staging",
                    &[HYBRID_HOST_STAGING_SLOTS, staging_slot],
                    1,
                )?,
            ],
        )?,
    })
}

fn hybrid_prefill_scratch_estimate(
    shape: HybridPrefillScratchShape,
    chunk: usize,
) -> Result<HybridPrefillScratchEstimate> {
    let prefill_device = checked_scratch_sum(
        "prefill device",
        [
            checked_scratch_bytes("prefill hidden", &[4, chunk, shape.hidden], 2)?,
            checked_scratch_bytes("prefill q", &[2, chunk, shape.q_dim], 2)?,
            checked_scratch_bytes("prefill kv", &[2, chunk, shape.kv_dim], 2)?,
            checked_scratch_bytes("prefill ffn", &[2, chunk, shape.inter], 2)?,
            checked_scratch_bytes("prefill metadata", &[2, chunk], 4)?,
        ],
    )?;
    let verifier = hybrid_verify_scratch_estimate(shape, 4)?;
    let prefill_verifier = hybrid_verify_scratch_estimate(shape, chunk.max(4))?;
    Ok(HybridPrefillScratchEstimate {
        device_bytes: checked_scratch_sum(
            "łączne bufory device",
            [
                prefill_device,
                verifier.device_bytes,
                prefill_verifier.device_bytes,
            ],
        )?,
        pinned_bytes: checked_scratch_sum(
            "łączne bufory pinned",
            [verifier.pinned_bytes, prefill_verifier.pinned_bytes],
        )?,
    })
}

fn hybrid_layer_major_scratch_estimate(
    shape: HybridPrefillScratchShape,
    tokens: usize,
) -> Result<usize> {
    if tokens == 0 || tokens > HYBRID_LAYER_MAJOR_MAX_TOKENS {
        return Err(ForgeError::Scheduler(format!(
            "layer-major prefill wymaga 1..={HYBRID_LAYER_MAJOR_MAX_TOKENS} tokenów"
        )));
    }
    let wide = shape.q_dim.max(shape.value_dim);
    let shared_projection = shape
        .q_dim
        .checked_mul(2)
        .map(|q| q.max(shape.conv_dim))
        .ok_or_else(|| ForgeError::Scheduler("przepełnienie projekcji layer-major".into()))?;
    if shape.inter < shared_projection || wide < shape.hidden || wide < shape.kv_dim {
        return Err(ForgeError::Unsupported(
            "kształt layer-major nie pozwala współdzielić buforów fazowych".into(),
        ));
    }
    let conv_elems = shape
        .conv_dim
        .checked_mul(shape.d_conv.saturating_sub(1))
        .ok_or_else(|| ForgeError::Scheduler("przepełnienie okna conv layer-major".into()))?;
    checked_scratch_sum(
        "arena layer-major",
        [
            checked_scratch_bytes("hidden layer-major", &[2, tokens, shape.hidden], 2)?,
            checked_scratch_bytes("v layer-major", &[tokens, shape.kv_dim], 2)?,
            checked_scratch_bytes("szerokie bufory layer-major", &[2, tokens, wide], 2)?,
            checked_scratch_bytes("z i o layer-major", &[2, tokens, shape.value_dim], 2)?,
            checked_scratch_bytes("ffn layer-major", &[2, tokens, shape.inter], 2)?,
            checked_scratch_bytes(
                "surowe bramki layer-major",
                &[2, tokens, shape.n_v_heads],
                2,
            )?,
            checked_scratch_bytes("bramki f32 layer-major", &[2, tokens, shape.n_v_heads], 4)?,
            checked_scratch_bytes("metadata layer-major", &[3, tokens], 4)?,
            checked_scratch_bytes("okna conv layer-major", &[2, conv_elems], 2)?,
            4,
        ],
    )
}

fn hybrid_prefill_activation_budget_capable(
    estimate: HybridPrefillScratchEstimate,
    available: Option<usize>,
) -> bool {
    estimate
        .device_bytes
        .checked_add(HYBRID_PREFILL_ACTIVATION_RESERVE)
        .zip(available)
        .is_some_and(|(required, available)| required <= available)
}

fn hybrid_prefill_profile_spans(prompt_tokens: usize, chunk_size: usize) -> usize {
    let full_outer = prompt_tokens / MAX_PREFILL_CHUNK;
    let remainder = prompt_tokens % MAX_PREFILL_CHUNK;
    full_outer * hybrid_prefill_inner_chunk_count(MAX_PREFILL_CHUNK, chunk_size)
        + hybrid_prefill_inner_chunk_count(remainder, chunk_size)
}

fn hybrid_prefill_inner_chunk_count(mut tokens: usize, chunk_size: usize) -> usize {
    let mut chunks = 0;
    while tokens > 0 {
        tokens -= hybrid_prefill_step_size(tokens, chunk_size);
        chunks += 1;
    }
    chunks
}

fn hybrid_prefill_step_size(remaining: usize, chunk_size: usize) -> usize {
    if remaining == chunk_size + 1 {
        chunk_size - 1
    } else {
        remaining.min(chunk_size)
    }
}

fn hybrid_q_full_cols(q_dim: usize, conv_dim: usize, hidden_size: usize) -> usize {
    (q_dim * 2).max(conv_dim).max(hidden_size * 2)
}

fn native_mtp_b2_device_embedding(mode: Option<&str>, shares_target_embedding: bool) -> bool {
    mode == Some("device") && shares_target_embedding
}

fn validate_mtp_routed_inputs(
    vocab: usize,
    fed: [u32; 2],
    k: usize,
    external_drafts: [Option<&[u32]>; 2],
) -> Result<[i32; 2]> {
    let [fed0, fed1] = fed.map(|token| {
        i32::try_from(token).map_err(|_| ForgeError::Format("fed routed MTP przekracza i32".into()))
    });
    let fed_i32 = [fed0?, fed1?];
    if external_drafts
        .iter()
        .flatten()
        .any(|draft| draft.len() != k)
    {
        return Err(ForgeError::Scheduler(
            "zewnętrzny draft routed MTP ma niezgodne K".into(),
        ));
    }
    if fed
        .iter()
        .copied()
        .chain(
            external_drafts
                .iter()
                .flatten()
                .flat_map(|draft| draft.iter().copied()),
        )
        .any(|token| token as usize >= vocab)
    {
        return Err(ForgeError::Scheduler(
            "token wejściowy pary routed MTP wykracza poza słownik".into(),
        ));
    }
    for token in external_drafts
        .iter()
        .flatten()
        .flat_map(|draft| draft.iter().copied())
    {
        i32::try_from(token)
            .map_err(|_| ForgeError::Format("draft routed MTP przekracza i32".into()))?;
    }
    Ok(fed_i32)
}

fn restore_after<T>(result: Result<T>, restore: impl FnOnce()) -> Result<T> {
    restore();
    result
}

fn validate_mtp_pair_metadata_commit(
    states: &[MtpDraftState; 2],
    retained: [usize; 2],
) -> Result<[usize; 2]> {
    Ok([
        states[0].validate_commit_prefix_metadata(retained[0])?,
        states[1].validate_commit_prefix_metadata(retained[1])?,
    ])
}

fn apply_mtp_pair_metadata_commit(
    states: &mut [MtpDraftState; 2],
    kv: &mut KvCache,
    targets: [usize; 2],
) {
    states[0].apply_commit_prefix_metadata(kv, targets[0]);
    states[1].apply_commit_prefix_metadata(kv, targets[1]);
}

fn rollback_mtp_pair(
    states: &mut [MtpDraftState; 2],
    kv: &mut KvCache,
    stream: &Stream,
) -> Result<()> {
    rollback_mtp_pair_inner(states, kv, stream, None)
}

fn rollback_mtp_pair_inner(
    states: &mut [MtpDraftState; 2],
    kv: &mut KvCache,
    stream: &Stream,
    fail_lane: Option<usize>,
) -> Result<()> {
    let first = if states[0].checkpoint_len().is_some() {
        if fail_lane == Some(0) {
            Err(ForgeError::Device(
                "MTP: wymuszony błąd rollbacku lane0".into(),
            ))
        } else {
            states[0].rollback(kv, stream)
        }
    } else {
        Ok(())
    };
    let second = if states[1].checkpoint_len().is_some() {
        if fail_lane == Some(1) {
            Err(ForgeError::Device(
                "MTP: wymuszony błąd rollbacku lane1".into(),
            ))
        } else {
            states[1].rollback(kv, stream)
        }
    } else {
        Ok(())
    };
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(ForgeError::Device(format!(
            "rollback MTP B2 lane0 nie powiódł się: {error}"
        ))),
        (Ok(()), Err(error)) => Err(ForgeError::Device(format!(
            "rollback MTP B2 lane1 nie powiódł się: {error}"
        ))),
        (Err(first), Err(second)) => Err(ForgeError::Device(format!(
            "rollback MTP B2 obu lane'ów nie powiódł się: lane0={first}; lane1={second}"
        ))),
    }
}

/// Fixed built-in calibration passage for W4A8 SmoothQuant. A few hundred
/// tokens of representative English prose plus a code snippet — enough to
/// exercise every linear's input-channel dynamic range so the migration scales
/// reflect real activation outliers. Embedded in-tree so calibration needs no
/// download and no original fp16 weights (SmoothQuant needs statistics only).
pub const W4A8_CALIB_TEXT: &str = "\
The quick brown fox jumps over the lazy dog. In the beginning the universe was \
created; this has made a lot of people very angry and been widely regarded as a \
bad move. Language models predict the next token given the preceding context, \
learning statistical regularities from vast corpora of natural text. Attention \
mechanisms let each position attend to every other position, weighting their \
contributions by learned compatibility scores. Quantization reduces the \
precision of weights and activations to shrink memory bandwidth and accelerate \
matrix multiplication on tensor cores, trading a small amount of accuracy for a \
large gain in throughput. The Eiffel Tower is located in Paris, the capital of \
France, and was completed in 1889 for the World's Fair. Water boils at one \
hundred degrees Celsius at sea level, and freezes at zero. Photosynthesis \
converts carbon dioxide and water into glucose and oxygen using energy from \
sunlight captured by chlorophyll in the chloroplasts of plant cells.\n\n\
fn fibonacci(n: u64) -> u64 {\n    let (mut a, mut b) = (0u64, 1u64);\n    \
for _ in 0..n {\n        let next = a + b;\n        a = b;\n        b = next;\n    \
}\n    a\n}\n\n\
def quicksort(xs):\n    if len(xs) <= 1:\n        return xs\n    pivot = xs[len(xs) // 2]\n    \
left = [x for x in xs if x < pivot]\n    mid = [x for x in xs if x == pivot]\n    \
right = [x for x in xs if x > pivot]\n    return quicksort(left) + mid + quicksort(right)\n\n\
SELECT id, name, SUM(amount) AS total FROM orders WHERE status = 'paid' GROUP BY id, name \
HAVING total > 1000 ORDER BY total DESC LIMIT 20;\n\n\
The mitochondria is the powerhouse of the cell. Supply and demand determine \
prices in a competitive market: when demand rises and supply stays fixed, the \
equilibrium price increases. Newton's second law states that force equals mass \
times acceleration. The derivative of sine is cosine, and the integral of one \
over x is the natural logarithm of x. Machine translation, summarization, and \
question answering are classic natural-language processing tasks that modern \
transformer architectures address with a single unified sequence-to-sequence \
formulation trained end to end on large and diverse multilingual datasets.";

/// Largest speculative draft (tokens) a single verification forward accepts
/// (SPEC §6). One verify runs `fed + draft` = up to `MAX_SPEC_DRAFT + 1` query
/// positions, bounding the [T, vocab] verify-logit scratch.
pub const MAX_SPEC_DRAFT: usize = 16;

/// Context splits for decode attention. Splitting shortens each warp's
/// sequential online-softmax chain by this factor (decode runs one block per
/// head — heavily latency-bound; 8 splits cut the attention kernel from
/// ~24 us to ~7 us per layer on RTX 4090), at the cost of a regrouped
/// softmax whose rounding differs slightly from the single-block order.
/// Measured drift vs splits=1 over 16 greedy steps: logit max-abs-diff
/// 0.087 (Bielik NVFP4) / 0.042 (Qwen3 Q8_0) with the argmax identical at
/// every step. 1 reproduces the unsplit arithmetic bit-exactly.
const ATTN_DECODE_SPLITS: usize = 8;

/// Decode rows riding a mixed prefill+decode forward: `b` sequences' input
/// token ids (their attention metadata lives in the batch buffers).
pub struct MixedDecodeRows {
    pub b: usize,
    pub ids: Vec<i32>,
}
const ATTN_DECODE_GQA_SPLITS: usize = 32;


/// Zrzut sumy i skrajnych wartości bufora f16, włączany `FORGE_LAYER_TRACE=1`.
/// Służy do porównania warstwa po warstwie z `llama-eval-callback`, który podaje
/// te same sumy po stronie llama.cpp. Każdy zrzut synchronizuje kartę, więc jest
/// bezużyteczny w pomiarach wydajności i domyślnie wyłączony.
fn layer_trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FORGE_LAYER_TRACE").is_ok_and(|v| v == "1"))
}

/// Coarse per-phase wall-clock attribution for `prefill_chunk`, enabled by
/// FORGE_PREFILL_TRACE=1. Every probe synchronizes the device, so absolute
/// numbers are pessimistic (no inter-kernel overlap) — use the ratios.
struct PrefillTrace {
    enabled: bool,
    names: Vec<&'static str>,
    totals: Vec<std::time::Duration>,
    last: std::time::Instant,
}

impl PrefillTrace {
    fn new() -> Self {
        Self {
            enabled: std::env::var("FORGE_PREFILL_TRACE").is_ok_and(|v| v == "1"),
            names: Vec::new(),
            totals: Vec::new(),
            last: std::time::Instant::now(),
        }
    }

    fn start(&mut self, device: &dyn Device) {
        if self.enabled {
            let _ = device.synchronize();
            self.last = std::time::Instant::now();
        }
    }

    fn mark(&mut self, device: &dyn Device, name: &'static str) {
        if !self.enabled {
            return;
        }
        let _ = device.synchronize();
        let now = std::time::Instant::now();
        let dt = now - self.last;
        self.last = now;
        match self.names.iter().position(|n| *n == name) {
            Some(i) => self.totals[i] += dt,
            None => {
                self.names.push(name);
                self.totals.push(dt);
            }
        }
    }

    fn report(&self, n_tokens: usize) {
        if !self.enabled {
            return;
        }
        let total: std::time::Duration = self.totals.iter().sum();
        eprintln!("prefill_chunk trace (T={n_tokens}, total {total:?}):");
        let mut order: Vec<usize> = (0..self.names.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.totals[i]));
        for i in order {
            eprintln!(
                "  {:<16} {:>10.3?} ({:>5.1}%)",
                self.names[i],
                self.totals[i],
                self.totals[i].as_secs_f64() / total.as_secs_f64() * 100.0
            );
        }
    }
}

pub struct ModelConfig {
    /// Budżet pamięci hosta (bajty) na wagi, które nie zmieszczą się w VRAM.
    /// GPU czyta je wprost przez PCIe, więc model większy od karty daje się
    /// uruchomić kosztem pasma. 0 wyłącza strumieniowanie: brak miejsca w VRAM
    /// jest wtedy błędem, jak przed wprowadzeniem tieringu.
    pub weight_host_budget: usize,
    /// Katalog na plik zrzutu wag ekspertów MoE. `None` wyłącza warstwę NVMe:
    /// model, który nie mieści się w VRAM i RAM, po prostu się nie załaduje.
    pub weight_spill_dir: Option<std::path::PathBuf>,
    pub kv_page_size: usize,
    pub kv_pages: usize,
    pub max_seq_len: usize,
    /// KV cache storage mode. F16 (default, bit-exact canonical path), Fp8
    /// (halves KV memory + bandwidth; per-value scale-free e4m3, fused decode
    /// only), or Rot{bits} (TurboQuant-class rotational 3/4-bit; single-stream
    /// decode path). Validated at load.
    pub kv_quant: KvQuant,
    /// KV tiering (SPEC §5.4B): spill cold pages to pinned RAM / NVMe and
    /// stream them back per layer, unlocking contexts beyond the VRAM pool.
    /// Off (default) = today's VRAM-only behavior; f16/fp8 caches only.
    pub kv_tier: KvTierConfig,
    /// Radix-tree prefix caching (SPEC §5.2): dedup shared KV prefixes across
    /// sequences so a request sharing a prefix skips re-prefilling it. `true`
    /// (default) engages only when it is a strict optimization — F16/Fp8 KV,
    /// no tiering, non-hybrid arch; otherwise silently inactive.
    pub prefix_cache: bool,
    /// Ładuje i uruchamia opcjonalną głowę MTP/NextN modelu.
    pub native_mtp: bool,
    /// Żądany układ GGUF NVFP4. Tryby spekulacyjne i batch muszą używać RowMajor36.
    pub nvfp4_gguf_layout: Nvfp4GgufLayout,
    /// Polityka resident dla checkpointów compressed-tensors NVFP4.
    pub nvfp4_ct_layout: NvFp4CtLayoutPolicy,
    /// Etap pipeline'u nie zaczynający się od warstwy zerowej dostaje stan
    /// ukryty z poprzedniej karty zamiast liczyć embedding. Wynika to wprost z
    /// `layer_range` i nie jest osobnym ustawieniem.
    ///
    /// Zakres warstw `(pierwsza, ile)` dla etapu pipeline'u. `None` ładuje cały
    /// model. Etap wczytuje WYŁĄCZNIE swoje warstwy, więc model większy od
    /// jednej karty mieści się na kilku.
    pub layer_range: Option<(usize, usize)>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            weight_host_budget: 0,
            weight_spill_dir: None,
            kv_page_size: 32,
            kv_pages: 512,
            max_seq_len: 8192,
            kv_quant: KvQuant::F16,
            kv_tier: KvTierConfig::default(),
            prefix_cache: true,
            native_mtp: false,
            nvfp4_gguf_layout: Nvfp4GgufLayout::RowMajor36,
            nvfp4_ct_layout: NvFp4CtLayoutPolicy::RowMajorE4M3,
            layer_range: None,
        }
    }
}

/// L2-normalize a vector in place. A zero vector is left unchanged (no NaNs
/// from dividing by a zero norm).
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        let inv = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

pub struct Model {
    /// Pierwsza warstwa TEGO etapu w numeracji całego modelu. Zero oznacza etap
    /// pierwszy, który liczy embedding; wyższa wartość — etap dostający stan
    /// ukryty z poprzedniej karty.
    pub stage_first_layer: usize,
    pub device: Arc<dyn Device>,
    pub kernels: Kernels,
    pub weights: ModelWeights,
    pub kv: KvCache,
    kv_reuse_poison: KvReusePoison,
    stream: Stream,
    /// Device-side page table + seq len for the active sequence (v0: one).
    page_table_dev: DevBuffer,
    seq_len_dev: DevBuffer,
    max_pages_per_seq: usize,
    bufs: DecodeBufs,
    /// Batched-prefill scratch; allocated lazily on the first prefill_chunk.
    prefill_bufs: Option<PrefillBufs>,
    /// Rozmiar wewnętrznego chunka hybrydowego prefill, wybrany raz przy ładowaniu.
    hybrid_prefill_chunk_size: usize,
    /// Speculative-verification logit scratch (SPEC §6): the [T, vocab] f32
    /// logits of one draft-verification forward. Allocated lazily on the first
    /// `verify_greedy_draft`; `None` until speculation runs.
    verify_bufs: Option<VerifyBufs>,
    /// Warstwowy verifier T=3/4 dla hybrydowego targetu MTP.
    hybrid_verify_bufs: Option<HybridVerifyBufs>,
    /// Segmentowany scratch native MTP dla dokładnie dwóch sekwencji.
    mtp_b2_bufs: Option<MtpB2Bufs>,
    /// Trwały scratch batched prefill, oddzielony od verifiera decode cap=4.
    hybrid_prefill_bufs: Option<HybridVerifyBufs>,
    /// Dedykowany scratch ciągłego prefill dokładnie B2×T32.
    hybrid_prefill_b2_bufs: Option<HybridPrefillB2Bufs>,
    /// Leniwa arena pełnego przebiegu layer-major, domyślnie nieaktywna.
    hybrid_layer_major_bufs: Option<HybridLayerMajorBufs>,
    /// Trwałe grafy stałej części verifiera MTP dla pary slotu i T.
    hybrid_verify_graphs: HashMap<(usize, usize), ExecGraph>,
    /// Nieudana próba przechwycenia wyłącza kolejne próby dla danej pary.
    hybrid_verify_graph_disabled: HashSet<(usize, usize)>,
    /// Captured decode step; replayed per token (inputs are device-resident).
    decode_graph: Option<ExecGraph>,
    /// Przechwycony krok dekodowania modelu hybrydowego (qwen35/DeltaNet).
    /// Wstawienie embeddingu zostaje poza grafem, bo jako jedyne zależy od
    /// `token_id`; reszta czyta pozycję i długość z buforów urządzenia.
    decode_hybrid_graph: Option<ExecGraph>,
    /// Captured non-hybrid MoE decode step (fully device-side grouped expert
    /// dispatch — no host readback), replayed per token like `decode_graph`.
    decode_moe_graph: Option<ExecGraph>,
    /// Captured rotational (rot4/rot3) decode step; replayed per token. The
    /// pack kernel reads the token position from `bufs.pos`, so the whole
    /// dual-region chain is position-independent and graph-capturable.
    decode_rot_graph: Option<ExecGraph>,
    /// Continuous-batching decode scratch (sized for `batch_cap` sequences),
    /// allocated on the first `ensure_batch`.
    batch_bufs: Option<BatchBufs>,
    /// Per-bucket captured batched forward+logits graphs (bucket = padded
    /// batch size). Replayed for any live batch that rounds up to the bucket.
    batch_graphs: HashMap<usize, ExecGraph>,
    /// Largest batch the scratch is provisioned for (0 until `ensure_batch`).
    batch_cap: usize,
    /// KV tier manager (SPEC §5.4B); `None` = tiering off (VRAM-only paging,
    /// bit-for-bit today's behavior).
    tier: Option<TierManager>,
    /// Streamed-attention staging: full-context K/V slabs for ONE layer plus
    /// an identity page table (staging page index == logical page index).
    tier_bufs: Option<TierBufs>,
    /// Sequence whose page table currently occupies `page_table_dev`
    /// (0 = none/stale). Spills, restores and batched growth invalidate it,
    /// forcing a re-upload on the next single-stream step.
    pt_seq: u64,
    /// MoE scratch; `Some` only for Mixture-of-Experts models.
    moe_bufs: Option<MoeBufs>,
    /// Rezydencja ekspertów; `Some` tylko dla modeli MoE.
    moe_residency: Option<MoeResidencyState>,
    /// Plik zrzutu wag ekspertów; `Some`, gdy skonfigurowano warstwę NVMe.
    expert_spill: Option<ExpertSpill>,
    /// Pula izolowanych stanów Gated-DeltaNet przypisanych do sekwencji.
    hybrid_states: Option<HybridStatePool>,
    /// Gated-attention + DeltaNet single-token scratch; allocated lazily for a
    /// hybrid model on the first hybrid forward.
    hybrid_bufs: Option<HybridBufs>,
    /// FORGE_HYBRID_DEBUG=1: dump per-layer residual-stream norms.
    hybrid_debug: bool,
    /// Radix-tree prefix cache (SPEC §5.2); `None` = inactive (disabled by
    /// config, or ineligible: tiering / rot / hybrid arch). When active, admitted
    /// sequences borrow shared prefix pages before prefill and donate their own
    /// prefilled pages on completion.
    prefix_cache: Option<crate::prefix::PrefixCache>,
    /// Active W4A8 SmoothQuant calibration accumulator: when `Some`, the dense
    /// prefill path records per-input-channel activation abs-max at the four
    /// linear-input points. Set only during `calibrate_w4a8`, `None` otherwise.
    calib: Option<CalibAccum>,
    /// Zdarzenia czasu GPU przygotowane wyłącznie przez `forge bench`.
    prefill_profiles: VecDeque<PrefillProfileRun>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PrefillProfile {
    pub target_gpu_ms: Option<f64>,
    pub mtp_catchup_gpu_ms: Option<f64>,
}

struct ProfileSpan {
    start: Event,
    end: Event,
}

struct PrefillProfileRun {
    target: Vec<ProfileSpan>,
    catchup: Vec<ProfileSpan>,
    target_cursor: usize,
    catchup_cursor: usize,
}

/// Running per-input-channel activation abs-max for the W4A8 SmoothQuant
/// calibration pass. One vector per layer per linear-input point; reduced from
/// the f16 activation buffers read back after each relevant kernel.
struct CalibAccum {
    attn_in: Vec<Vec<f32>>,
    attn_out: Vec<Vec<f32>>,
    ffn_in: Vec<Vec<f32>>,
    down_in: Vec<Vec<f32>>,
    /// Host staging for the largest captured buffer (grown as needed).
    scratch: Vec<u8>,
}

impl CalibAccum {
    fn new(n_layers: usize, hidden: usize, q_dim: usize, inter: usize) -> Self {
        Self {
            attn_in: vec![vec![0.0f32; hidden]; n_layers],
            attn_out: vec![vec![0.0f32; q_dim]; n_layers],
            ffn_in: vec![vec![0.0f32; hidden]; n_layers],
            down_in: vec![vec![0.0f32; inter]; n_layers],
            scratch: Vec::new(),
        }
    }

    /// Fold `t` rows of an f16 activation buffer into a per-channel abs-max acc.
    fn absorb(
        device: &dyn Device,
        buf: &DevBuffer,
        acc: &mut [f32],
        t: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<()> {
        let dim = acc.len();
        let bytes = t * dim * 2;
        if scratch.len() < bytes {
            scratch.resize(bytes, 0);
        }
        device.read(buf, 0, &mut scratch[..bytes])?;
        let rows: &[f16] = bytemuck::cast_slice(&scratch[..bytes]);
        for row in 0..t {
            let r = &rows[row * dim..row * dim + dim];
            for (a, h) in acc.iter_mut().zip(r) {
                *a = a.max(h.to_f32().abs());
            }
        }
        Ok(())
    }
}

/// One DeltaNet layer's resident recurrent state for the active sequence.
struct SsmState {
    /// Causal conv window `[conv_dim, d_conv-1]` f16 (oldest sample first).
    conv: DevBuffer,
    /// Recurrent state matrices `[n_v_heads, d_state, d_state]` f32.
    state: DevBuffer,
}

struct HybridStateSlot {
    layers: Vec<Option<SsmState>>,
    mtp: Option<MtpDraftState>,
    generation: u64,
    in_use: bool,
    ready: Event,
    ready_recorded: bool,
    initialized_generation: u64,
}

struct HybridStatePool {
    device: Arc<dyn Device>,
    layer_kinds: Vec<LayerKind>,
    layout: DeltaStateLayout,
    conv_bytes: usize,
    state_bytes: usize,
    zero_conv: DevBuffer,
    zero_state: DevBuffer,
    slots: Vec<HybridStateSlot>,
    free: Vec<usize>,
    active: Option<HybridStateLease>,
    poisoned: Option<String>,
    mtp_kv: Option<KvCache>,
    mtp_shape: Option<(usize, usize)>,
    quarantined_mtp_states: Vec<MtpDraftState>,
    quarantined_mtp_kv: Vec<KvCache>,
}

impl HybridStatePool {
    fn new(
        device: Arc<dyn Device>,
        layer_kinds: Vec<LayerKind>,
        layout: DeltaStateLayout,
        conv_bytes: usize,
        state_bytes: usize,
        mtp_config: Option<(KvConfig, usize, usize)>,
    ) -> Result<Self> {
        let zero_conv = device.alloc(conv_bytes, MemKind::PinnedHost, Pool::Activations)?;
        let zero_state = device.alloc(state_bytes, MemKind::PinnedHost, Pool::Activations)?;
        unsafe {
            std::ptr::write_bytes(
                zero_conv.host_ptr().expect("pinned host mapping"),
                0,
                conv_bytes,
            );
            std::ptr::write_bytes(
                zero_state.host_ptr().expect("pinned host mapping"),
                0,
                state_bytes,
            );
        }
        let (mtp_kv, mtp_shape) = match mtp_config {
            Some((config, hidden_size, vocab_size)) => (
                Some(KvCache::new(device.as_ref(), config)?),
                Some((hidden_size, vocab_size)),
            ),
            None => (None, None),
        };
        let mut pool = Self {
            device,
            layer_kinds,
            layout,
            conv_bytes,
            state_bytes,
            zero_conv,
            zero_state,
            slots: Vec::new(),
            free: Vec::new(),
            active: None,
            poisoned: None,
            mtp_kv,
            mtp_shape,
            quarantined_mtp_states: Vec::new(),
            quarantined_mtp_kv: Vec::new(),
        };
        pool.allocate_slot()?;
        Ok(pool)
    }

    fn build_slot(&self) -> Result<HybridStateSlot> {
        let mtp = match (&self.mtp_kv, self.mtp_shape) {
            (Some(kv), Some((hidden_size, vocab_size))) => Some(MtpDraftState::new(
                self.device.clone(),
                kv,
                hidden_size,
                vocab_size,
            )?),
            (None, None) => None,
            _ => {
                return Err(ForgeError::Scheduler(
                    "niespójna konfiguracja puli MTP".into(),
                ))
            }
        };
        let ready = self.device.create_event()?;
        let mut layers = Vec::with_capacity(self.layer_kinds.len());
        for kind in &self.layer_kinds {
            layers.push(match kind {
                LayerKind::DeltaNet => Some(SsmState {
                    conv: self
                        .device
                        .alloc(self.conv_bytes, MemKind::Device, Pool::Weights)?,
                    state: self
                        .device
                        .alloc(self.state_bytes, MemKind::Device, Pool::Weights)?,
                }),
                LayerKind::Attention => None,
            });
        }
        Ok(HybridStateSlot {
            layers,
            mtp,
            generation: 0,
            in_use: false,
            ready,
            ready_recorded: false,
            initialized_generation: 0,
        })
    }

    fn allocate_slot(&mut self) -> Result<usize> {
        let state = self.build_slot()?;
        let slot = self.slots.len();
        self.slots.push(state);
        self.free.push(slot);
        Ok(slot)
    }

    fn ensure_capacity(&mut self, slots: usize) -> Result<()> {
        self.ensure_healthy()?;
        let additional = slots.saturating_sub(self.slots.len());
        if additional == 0 {
            return Ok(());
        }
        let delta_layers = self
            .layer_kinds
            .iter()
            .filter(|kind| matches!(kind, LayerKind::DeltaNet))
            .count();
        let reserve = |bytes: usize| {
            bytes
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler("przepełnienie wyrównania alokacji".into()))
        };
        let weights_per_slot = reserve(self.conv_bytes)?
            .checked_add(reserve(self.state_bytes)?)
            .and_then(|bytes| bytes.checked_mul(delta_layers))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie rozmiaru slotu SSM".into()))?;
        let activations_per_slot = match (self.mtp_shape, self.mtp_kv.as_ref()) {
            (Some((hidden, vocab)), Some(kv)) => {
                let hidden_bytes = hidden.checked_mul(2).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie rozmiaru hidden MTP".into())
                })?;
                let step_hidden = hidden_bytes.checked_mul(4).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie checkpointów hidden MTP".into())
                })?;
                let logits = vocab
                    .checked_mul(4)
                    .ok_or_else(|| ForgeError::Scheduler("przepełnienie logitów MTP".into()))?;
                let page_table = kv.cfg.max_pages_per_seq.checked_mul(4).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie tabeli stron MTP".into())
                })?;
                [
                    hidden_bytes,
                    hidden_bytes,
                    hidden_bytes,
                    logits,
                    page_table,
                    4,
                    4,
                    20,
                    hidden_bytes,
                    step_hidden,
                ]
                .into_iter()
                .try_fold(0usize, |total, bytes| {
                    total.checked_add(reserve(bytes)?).ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie rozmiaru slotu MTP".into())
                    })
                })?
            }
            (None, None) => 0,
            _ => {
                return Err(ForgeError::Scheduler(
                    "niespójna konfiguracja puli MTP".into(),
                ))
            }
        };
        let required_weights = weights_per_slot
            .checked_mul(additional)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu puli SSM".into()))?;
        let required_activations = activations_per_slot
            .checked_mul(additional)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu puli MTP".into()))?;
        let available_weights = self.device.pool_available(Pool::Weights);
        let available_activations = self.device.pool_available(Pool::Activations);
        if available_weights.is_some_and(|available| required_weights > available)
            || available_activations.is_some_and(|available| required_activations > available)
        {
            return Err(ForgeError::Scheduler(format!(
                "preflight {slots} slotów hybrydowych wymaga {required_weights} B puli weights i {required_activations} B puli activations dla {additional} nowych slotów; dostępne odpowiednio {} B i {} B",
                available_weights.map_or_else(|| "nieznane".into(), |bytes| bytes.to_string()),
                available_activations
                    .map_or_else(|| "nieznane".into(), |bytes| bytes.to_string()),
            )));
        }
        let mut allocated = Vec::with_capacity(additional);
        for _ in 0..additional {
            allocated.push(self.build_slot().map_err(|error| {
                ForgeError::Scheduler(format!(
                    "preflight {slots} slotów hybrydowych nie zaalokował {additional} nowych slotów (weights {required_weights} B, activations {required_activations} B): {error}"
                ))
            })?);
        }
        let first = self.slots.len();
        self.slots.extend(allocated);
        self.free.extend(first..slots);
        Ok(())
    }

    fn acquire(&mut self) -> Result<HybridStateLease> {
        self.ensure_healthy()?;
        if self.free.is_empty() {
            self.allocate_slot()?;
        }
        let slot = self.free.pop().expect("wolny slot został przygotowany");
        let state = &mut self.slots[slot];
        state.generation = state.generation.checked_add(1).ok_or_else(|| {
            ForgeError::Scheduler("licznik generacji stanu hybrydowego został wyczerpany".into())
        })?;
        state.in_use = true;
        Ok(HybridStateLease {
            slot,
            generation: state.generation,
        })
    }

    fn validate(&self, lease: HybridStateLease) -> Result<()> {
        let Some(slot) = self.slots.get(lease.slot) else {
            return Err(ForgeError::Scheduler(
                "nieprawidłowy slot stanu hybrydowego".into(),
            ));
        };
        if !slot.in_use || slot.generation != lease.generation {
            return Err(ForgeError::Scheduler(
                "nieaktualny lease stanu hybrydowego".into(),
            ));
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<()> {
        match &self.poisoned {
            Some(reason) => Err(ForgeError::Device(format!(
                "pula stanów hybrydowych jest zatruta: {reason}"
            ))),
            None => Ok(()),
        }
    }

    fn poison(&mut self, reason: String) -> ForgeError {
        self.poisoned = Some(reason.clone());
        ForgeError::Device(format!("pula stanów hybrydowych została zatruta: {reason}"))
    }

    fn quarantine_mtp(
        &mut self,
        reason: String,
        states: impl IntoIterator<Item = MtpDraftState>,
        kv: KvCache,
    ) -> ForgeError {
        self.quarantined_mtp_states.extend(states);
        self.quarantined_mtp_kv.push(kv);
        self.poison(reason)
    }

    fn activate(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        if self.active == Some(lease) {
            return Ok(());
        }
        // Aktywne lease współdzielą jeden stream, więc ich praca jest już
        // uporządkowana; event jest potrzebny dopiero między generacjami slotu.
        let slot = &mut self.slots[lease.slot];
        if slot.ready_recorded {
            self.device.wait_event(stream, &slot.ready)?;
            slot.ready_recorded = false;
        }
        if slot.initialized_generation != lease.generation {
            for state in slot.layers.iter().flatten() {
                self.device
                    .copy(&self.zero_conv, 0, &state.conv, 0, self.conv_bytes, stream)?;
                self.device.copy(
                    &self.zero_state,
                    0,
                    &state.state,
                    0,
                    self.state_bytes,
                    stream,
                )?;
            }
            slot.initialized_generation = lease.generation;
        }
        self.active = Some(lease);
        Ok(())
    }

    fn release(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        let record_result = self
            .device
            .record_event(&self.slots[lease.slot].ready, stream);
        self.finish_release(lease, record_result, || stream.synchronize())
    }

    fn finish_release(
        &mut self,
        lease: HybridStateLease,
        record_result: Result<()>,
        synchronize: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let event_recorded = match record_result {
            Ok(()) => true,
            Err(record_error) => match synchronize() {
                Ok(()) => {
                    if let Err(zero_error) = self.zero_slot_synchronously(lease.slot) {
                        let reason = format!(
                            "record eventu nie powiódł się ({record_error}); po synchronizacji nie udało się wyzerować slotu ({zero_error})"
                        );
                        self.poisoned = Some(reason.clone());
                        return Err(ForgeError::Device(reason));
                    }
                    tracing::warn!(
                        "record eventu zwolnienia stanu hybrydowego nie powiódł się; stream został zsynchronizowany: {record_error}"
                    );
                    false
                }
                Err(sync_error) => {
                    let reason = format!(
                        "record eventu nie powiódł się ({record_error}); synchronizacja streamu także nie powiodła się ({sync_error})"
                    );
                    self.poisoned = Some(reason.clone());
                    return Err(ForgeError::Device(reason));
                }
            },
        };
        let slot = &mut self.slots[lease.slot];
        slot.ready_recorded = event_recorded;
        if self.active == Some(lease) {
            self.active = None;
        }
        slot.in_use = false;
        if let (Some(kv), Some(mtp)) = (&mut self.mtp_kv, &mut slot.mtp) {
            mtp.release(kv);
        }
        self.free.push(lease.slot);
        Ok(())
    }

    fn zero_slot_synchronously(&self, slot_index: usize) -> Result<()> {
        let zero_conv = unsafe {
            std::slice::from_raw_parts(
                self.zero_conv.host_ptr().expect("pinned host mapping"),
                self.conv_bytes,
            )
        };
        let zero_state = unsafe {
            std::slice::from_raw_parts(
                self.zero_state.host_ptr().expect("pinned host mapping"),
                self.state_bytes,
            )
        };
        for state in self.slots[slot_index].layers.iter().flatten() {
            self.device.write(zero_conv, &state.conv, 0)?;
            self.device.write(zero_state, &state.state, 0)?;
        }
        Ok(())
    }

    fn reset(&mut self, lease: HybridStateLease, stream: &Stream) -> Result<()> {
        self.activate(lease, stream)?;
        let slot = &mut self.slots[lease.slot];
        for state in slot.layers.iter().flatten() {
            self.device
                .copy(&self.zero_conv, 0, &state.conv, 0, self.conv_bytes, stream)?;
            self.device.copy(
                &self.zero_state,
                0,
                &state.state,
                0,
                self.state_bytes,
                stream,
            )?;
        }
        slot.initialized_generation = lease.generation;
        Ok(())
    }

    fn active_layers(&self) -> &[Option<SsmState>] {
        let active = self.active.expect("stan hybrydowy został aktywowany");
        &self.slots[active.slot].layers
    }

    fn layout(&self) -> DeltaStateLayout {
        self.layout
    }

    fn state_buffers(
        &self,
        lease: HybridStateLease,
        layer: usize,
    ) -> Result<Option<(DevBuffer, DevBuffer)>> {
        self.validate(lease)?;
        Ok(self.slots[lease.slot]
            .layers
            .get(layer)
            .ok_or_else(|| ForgeError::Scheduler("warstwa stanu hybrydowego poza zakresem".into()))?
            .as_ref()
            .map(|state| (state.conv.clone(), state.state.clone())))
    }

    fn has_mtp(&self) -> bool {
        self.mtp_kv.is_some() && self.mtp_shape.is_some()
    }

    fn take_mtp(&mut self, lease: HybridStateLease) -> Result<(MtpDraftState, KvCache)> {
        self.ensure_healthy()?;
        self.validate(lease)?;
        let kv = self.mtp_kv.take().ok_or_else(|| {
            ForgeError::Unsupported("współdzielony cache MTP nie został zaalokowany".into())
        })?;
        let state = match self.slots[lease.slot].mtp.take() {
            Some(state) => state,
            None => {
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "stan MTP aktywnej sekwencji jest już używany".into(),
                ));
            }
        };
        Ok((state, kv))
    }

    fn take_mtp_pair(
        &mut self,
        leases: [HybridStateLease; 2],
    ) -> Result<([MtpDraftState; 2], KvCache)> {
        self.ensure_healthy()?;
        if leases[0].slot == leases[1].slot {
            return Err(ForgeError::Scheduler(
                "para MTP wymaga dwóch różnych slotów".into(),
            ));
        }
        self.validate(leases[0])?;
        self.validate(leases[1])?;
        let kv = self.mtp_kv.take().ok_or_else(|| {
            ForgeError::Unsupported("współdzielony cache MTP nie został zaalokowany".into())
        })?;
        let first = match self.slots[leases[0].slot].mtp.take() {
            Some(state) => state,
            None => {
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "pierwszy stan pary MTP jest już używany".into(),
                ));
            }
        };
        let second = match self.slots[leases[1].slot].mtp.take() {
            Some(state) => state,
            None => {
                self.slots[leases[0].slot].mtp = Some(first);
                self.mtp_kv = Some(kv);
                return Err(ForgeError::Scheduler(
                    "drugi stan pary MTP jest już używany".into(),
                ));
            }
        };
        Ok(([first, second], kv))
    }

    fn restore_mtp(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
    ) -> Result<()> {
        let preflight = self
            .ensure_healthy()
            .and_then(|_| self.validate(lease))
            .and_then(|_| {
                if self.mtp_kv.is_some() || self.slots[lease.slot].mtp.is_some() {
                    Err(ForgeError::Scheduler(
                        "próba podwójnego przywrócenia stanu MTP".into(),
                    ))
                } else {
                    Ok(())
                }
            });
        if let Err(error) = preflight {
            let reason = format!("przywrócenie stanu MTP nie powiodło się: {error}");
            return Err(self.quarantine_mtp(reason, [state], kv));
        }
        self.mtp_kv = Some(kv);
        self.slots[lease.slot].mtp = Some(state);
        Ok(())
    }

    fn restore_mtp_pair(
        &mut self,
        leases: [HybridStateLease; 2],
        states: [MtpDraftState; 2],
        kv: KvCache,
    ) -> Result<()> {
        let preflight = self
            .ensure_healthy()
            .and_then(|_| {
                if leases[0].slot == leases[1].slot {
                    Err(ForgeError::Scheduler(
                        "para MTP wymaga dwóch różnych slotów".into(),
                    ))
                } else {
                    Ok(())
                }
            })
            .and_then(|_| self.validate(leases[0]))
            .and_then(|_| self.validate(leases[1]))
            .and_then(|_| {
                if self.mtp_kv.is_some()
                    || self.slots[leases[0].slot].mtp.is_some()
                    || self.slots[leases[1].slot].mtp.is_some()
                {
                    Err(ForgeError::Scheduler(
                        "próba podwójnego przywrócenia pary stanów MTP".into(),
                    ))
                } else {
                    Ok(())
                }
            });
        if let Err(error) = preflight {
            let reason = format!("przywrócenie pary stanów MTP nie powiodło się: {error}");
            return Err(self.quarantine_mtp(reason, states, kv));
        }
        let [first, second] = states;
        self.mtp_kv = Some(kv);
        self.slots[leases[0].slot].mtp = Some(first);
        self.slots[leases[1].slot].mtp = Some(second);
        Ok(())
    }

    fn mtp_host_embedding_gathers(&self) -> u64 {
        self.slots
            .iter()
            .filter_map(|slot| slot.mtp.as_ref())
            .map(MtpDraftState::host_embedding_gathers)
            .sum()
    }
}

/// Single-token scratch for the hybrid (gated-attention + DeltaNet) forward.
/// Buffers that exceed the standard decode scratch widths (the gated Q
/// projection is `2*n_heads*head_dim`, the conv stream `conv_dim`) live here.
struct HybridBufs {
    /// Liczba wierszy, na jaką starczy `batched_*`. Rośnie razem z batch cap.
    projection_rows: usize,
    /// Projekcje wejściowe DeltaNet policzone RAZ dla wszystkich lane'ów.
    /// Są bezstanowe, więc wiersze mogą pochodzić z różnych sekwencji, a jeden
    /// przebieg po wagach zastępuje jeden przebieg na lane (gate_proj to około
    /// 21 MB na warstwę — przy ośmiu lane'ach to była największa pozycja kroku).
    batched_qkv_mixed: DevBuffer,
    batched_z: DevBuffer,
    batched_alpha: DevBuffer,
    batched_beta_raw: DevBuffer,
    /// Gated Q projection output `[2*n_heads*head_dim]` f16.
    q_full: DevBuffer,
    /// De-interleaved query `[n_heads*head_dim]` f16.
    qc: DevBuffer,
    /// De-interleaved output gate `[n_heads*head_dim]` f16.
    gatec: DevBuffer,
    /// Gated attention output `[n_heads*head_dim]` f16 (attn ⊙ sigmoid(gate)).
    gated: DevBuffer,
    /// Conv + SiLU output `[conv_dim]` f16.
    conv_out: DevBuffer,
    /// Per-head-split conv q/k `[key_dim]` and their repeat to `[value_dim]`.
    q16: DevBuffer,
    k16: DevBuffer,
    q16src: DevBuffer,
    k16src: DevBuffer,
    q32: DevBuffer,
    k32: DevBuffer,
    /// Conv value heads `[value_dim]` f16.
    vtok: DevBuffer,
    /// Per-head log-decay `g` and write-gate `beta` `[n_v_heads]` f32.
    g: DevBuffer,
    beta_f: DevBuffer,
    /// DeltaNet recurrence output + gated-RMSNorm output `[value_dim]` f16.
    o: DevBuffer,
    normed: DevBuffer,
    /// Pinned-host staging for the per-token embedding row `[hidden]` f16, so
    /// the host gather lands via an async H2D on the compute stream (no
    /// per-token blocking legacy-stream drain).
    pinned_embed: DevBuffer,
}

/// Attention source for the shared decode chains: the paged VRAM cache (fast
/// path, graph-capturable) or the tier staging slabs carrying the sequence's
/// full context per layer (streamed path, never captured).
enum AttnSrc<'a> {
    Paged,
    Staged(&'a SeqKv),
}

/// VRAM staging for the streamed tier path (allocated only with tiering on).
/// Two slots ping-pong so the fused decode chain restores layer l+1 on the
/// tier's transfer stream while layer l's attention runs on the compute
/// stream; the synchronous paths (separate chain, prefill, rot, batched
/// streamed lanes) use slot 0 only.
struct TierBufs {
    slots: Vec<StageSlot>,
    identity_pt: DevBuffer,
    /// Bytes of one page of each staged region (KvConfig::tier_region_bytes
    /// order: K/V for f16/fp8; packed K/V + K/V scales for rot).
    region_bytes: Vec<usize>,
}

/// One staging generation: full-context slabs for one layer (one slab per
/// spillable region) plus the cross-stream handshake events.
struct StageSlot {
    stage: Vec<DevBuffer>,
    /// Recorded on the transfer stream when the slot's staging copies are
    /// enqueued; the compute stream waits on it before the attention launch.
    ready: Event,
    /// Recorded on the compute stream when the slot's slabs are no longer
    /// read; the transfer stream waits on it before restaging the slot.
    free: Event,
}

/// Persistent per-step activation buffers. Fixed addresses are what makes the
/// decode step CUDA-graph-replayable: only their contents change per token.
struct DecodeBufs {
    h: DevBuffer,
    /// Unrounded f32 mirror of the residual stream, written by the fused
    /// gemv_residual kernels; the norm-recomputing kernels take their
    /// sum-of-squares from it (rmsnorm_residual_f16's exact dataflow).
    h32: DevBuffer,
    x: DevBuffer,
    /// Fused q|k|v output ([q_dim + 2*kv_dim]); q starts at offset 0, so the
    /// attention kernel reads it directly. Split-layer fallbacks use q/k/v.
    qkv: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_out: DevBuffer,
    /// Split-attention partials: [n_heads, ATTN_DECODE_SPLITS, max_head_dim + 4]
    /// f32 (unnormalized acc + running max + running sum per split).
    attn_parts: DevBuffer,
    o_out: DevBuffer,
    /// Fused gate|up output ([2*inter]); split-layer fallbacks use gate/up.
    gate_up: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    logits: DevBuffer,
    ids: DevBuffer,
    pos: DevBuffer,
    /// Pinned-host staging: [token_id, pos, seq_len] i32 triple.
    pinned_in: DevBuffer,
    /// Pinned-host mirror of the page table (async H2D on page boundary).
    pinned_pt: DevBuffer,
    /// Pinned-host landing buffer for logits (avoids pageable D2H).
    pinned_logits: DevBuffer,
    /// Per-block partials of the sampling kernels ((f32, i32) pair arrays).
    sample_vals: DevBuffer,
    sample_idx: DevBuffer,
    /// Sampling result: [token_id i32, logprob f32].
    sample_out: DevBuffer,
    /// Pinned-host landing buffer for the 8-byte sampling result.
    pinned_sample: DevBuffer,
    /// Histogram kar samplingu przechowywany na urządzeniu.
    penalty_ids: DevBuffer,
    penalty_counts: DevBuffer,
    /// Przypięty bufor hosta do przygotowania histogramu kar.
    pinned_penalty: DevBuffer,
    pinned_penalty_counts: DevBuffer,
}

/// Persistent prefill scratch sized for MAX_PREFILL_CHUNK tokens. Activation
/// matrices are [T, cols] row-major; the batched GEMMs consume them directly
/// (token/column tails are clamped inside the kernels).
struct PrefillBufs {
    cap: usize,
    h: DevBuffer,
    x: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_out: DevBuffer,
    o_out: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
}

/// Speculative-verification scratch (SPEC §6): the [cap, vocab] f32 logits of
/// one draft-verification forward (one row per query position: the fed token
/// plus each draft token) plus the per-row greedy argmax, sized for `cap` =
/// MAX_SPEC_DRAFT + 1 positions. The argmax runs on the GPU so only `cap` token
/// ids cross PCIe, never the [cap, vocab] logits.
struct VerifyBufs {
    cap: usize,
    logits: DevBuffer,
    /// Per-row argmax token ids (i32, `cap` long), device-side.
    ids: DevBuffer,
    /// Pinned-host landing for `ids`.
    pinned_ids: DevBuffer,
}

/// Sposób przechowania danych potrzebnych do zatwierdzenia stanu DeltaNet.
enum DeltaVerifyCommit {
    InPlacePrefill,
    Retained {
        checkpoint_byte_offset: usize,
    },
    Recompute {
        q: DevBuffer,
        k: DevBuffer,
        v: DevBuffer,
        g: DevBuffer,
        beta: DevBuffer,
    },
}

/// Dane jednej warstwy DeltaNet potrzebne do zatwierdzenia prefiksu.
struct DeltaVerifyCache {
    commit: DeltaVerifyCommit,
    conv_initial: DevBuffer,
    conv_checkpoints: DevBuffer,
    state_initial: DevBuffer,
}

/// Bufory krótkiego, warstwowego przebiegu weryfikacyjnego modelu hybrydowego.
struct HybridVerifyBufs {
    cap: usize,
    base_pos: DevBuffer,
    visible_lens: DevBuffer,
    attn_parts: DevBuffer,
    q_full: DevBuffer,
    qc: DevBuffer,
    gatec: DevBuffer,
    gated: DevBuffer,
    qkv_mixed: DevBuffer,
    z: DevBuffer,
    q32: DevBuffer,
    k32: DevBuffer,
    vtok: DevBuffer,
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    g: DevBuffer,
    beta_f: DevBuffer,
    o: DevBuffer,
    normed: DevBuffer,
    state_checkpoints: DevBuffer,
    retained_state_checkpoints: Option<DevBuffer>,
    accepted: DevBuffer,
    pinned_decision: DevBuffer,
    host_staging: Vec<HybridHostStaging>,
    delta: Vec<Option<DeltaVerifyCache>>,
}

struct MtpB2DeltaCache {
    conv_initial: DevBuffer,
    conv_checkpoints: DevBuffer,
    state_initial: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    g: DevBuffer,
    beta: DevBuffer,
}

struct HybridPrefillB2DeltaCache {
    conv_initial: DevBuffer,
    state_initial: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    g: DevBuffer,
    beta: DevBuffer,
}

struct MtpB2Bufs {
    q_full: DevBuffer,
    qc: DevBuffer,
    gatec: DevBuffer,
    gated: DevBuffer,
    qkv_mixed: DevBuffer,
    z: DevBuffer,
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    o: DevBuffer,
    normed: DevBuffer,
    page_tables: DevBuffer,
    base_positions: DevBuffer,
    visible_lens: DevBuffer,
    decisions: DevBuffer,
    pinned_decisions: DevBuffer,
    pinned_metadata: DevBuffer,
    pinned_mtp_metadata: DevBuffer,
    catchup_embeddings: DevBuffer,
    mtp_initial_hidden: DevBuffer,
    mtp_seq_lens: DevBuffer,
    mtp_positions: DevBuffer,
    selected_states: DevBuffer,
    selected_conv: DevBuffer,
    selected_hidden: DevBuffer,
    delta: Vec<Option<MtpB2DeltaCache>>,
}

struct HybridPrefillB2Bufs {
    h: DevBuffer,
    x: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_out: DevBuffer,
    o_out: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    q_full: DevBuffer,
    qc: DevBuffer,
    gatec: DevBuffer,
    gated: DevBuffer,
    qkv_mixed: DevBuffer,
    z: DevBuffer,
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    o: DevBuffer,
    normed: DevBuffer,
    page_tables: DevBuffer,
    base_positions: DevBuffer,
    visible_lens: DevBuffer,
    decisions: DevBuffer,
    final_hidden: DevBuffer,
    logits: DevBuffer,
    pinned_metadata: DevBuffer,
    pinned_logits: DevBuffer,
    final_conv: DevBuffer,
    final_states: DevBuffer,
    delta: Vec<Option<HybridPrefillB2DeltaCache>>,
}

#[derive(Clone)]
struct HybridLayerMajorBufs {
    cap: usize,
    device_bytes: usize,
    h: DevBuffer,
    x: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    q_full: DevBuffer,
    qc: DevBuffer,
    gatec: DevBuffer,
    gated: DevBuffer,
    z: DevBuffer,
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    g: DevBuffer,
    beta: DevBuffer,
    o: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    mixer_out: DevBuffer,
    conv_initial: DevBuffer,
    conv_final: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    visible_lens: DevBuffer,
    base_pos: DevBuffer,
    host_staging: Vec<HybridLayerMajorHostStaging>,
}

#[derive(Clone)]
struct HybridLayerMajorHostStaging {
    embedding: DevBuffer,
    page_table: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    visible_lens: DevBuffer,
    base_pos: DevBuffer,
    seq_len: DevBuffer,
    position: DevBuffer,
    ready: Event,
}

struct HybridLayerMajorCheckpoint {
    base: usize,
    pages: Vec<i32>,
    tokens_len: usize,
    prefilled_len: usize,
    state_workspace: DevBuffer,
    conv_workspaces: Vec<Option<DevBuffer>>,
    state_bytes: usize,
    kv_byte_offset: usize,
    kv_page_bytes: usize,
    tail_page: Option<usize>,
}

type MtpB2Verification = ([(Vec<u32>, usize, u32); 2], [usize; 2]);

struct HybridHostStaging {
    embedding: DevBuffer,
    page_table: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    visible_lens: DevBuffer,
    base_pos: DevBuffer,
    accepted: DevBuffer,
    mtp_page_table: DevBuffer,
    mtp_positions: DevBuffer,
    mtp_visible_lens: DevBuffer,
    mtp_base_pos: DevBuffer,
    mtp_seq_len: DevBuffer,
    mtp_position: DevBuffer,
    ready: Event,
}

fn alloc_checked(
    device: &dyn Device,
    name: &str,
    dimensions: &[usize],
    element_bytes: usize,
    kind: MemKind,
) -> Result<DevBuffer> {
    let bytes = dimensions
        .iter()
        .try_fold(element_bytes, |size, &dimension| {
            size.checked_mul(dimension).ok_or_else(|| {
                ForgeError::Scheduler(format!(
                    "przepełnienie rozmiaru bufora {name}: {dimensions:?}"
                ))
            })
        })?;
    device.alloc(bytes, kind, Pool::Activations)
}

fn write_pinned(src: &[u8], dst: &DevBuffer) -> Result<()> {
    if src.len() > dst.len() {
        return Err(ForgeError::Scheduler(format!(
            "dane stagingu mają {} bajtów, bufor ma {}",
            src.len(),
            dst.len()
        )));
    }
    let host = dst
        .host_ptr()
        .ok_or_else(|| ForgeError::Device("bufor stagingu nie ma mapowania hosta".into()))?;
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), host, src.len());
    }
    Ok(())
}

#[cfg(test)]
fn native_mtp_greedy_decision(draft: &[u32], predictions: &[i32]) -> (usize, u32) {
    debug_assert_eq!(predictions.len(), draft.len() + 1);
    let mut accepted = 0usize;
    while accepted < draft.len() && predictions[accepted] as u32 == draft[accepted] {
        accepted += 1;
    }
    (accepted, predictions[accepted] as u32)
}

fn finish_greedy_verification(
    kv: &mut KvCache,
    page_table_seq: &mut u64,
    seq: &mut SeqKv,
    base: usize,
    result: Result<(usize, u32)>,
) -> Result<(usize, u32)> {
    let result = match result {
        Ok((accepted, correction)) => {
            let target_len = accepted
                .checked_add(1)
                .and_then(|retained| base.checked_add(retained));
            match target_len {
                Some(target_len) if target_len <= seq.len => {
                    kv.rollback(seq, target_len);
                    Ok((accepted, correction))
                }
                _ => {
                    if seq.len >= base {
                        kv.rollback(seq, base);
                    }
                    Err(ForgeError::Scheduler(
                        "invalid speculative verification rollback target".into(),
                    ))
                }
            }
        }
        Err(error) => {
            if seq.len >= base {
                kv.rollback(seq, base);
            }
            Err(error)
        }
    };
    *page_table_seq = 0;
    result
}

fn logical_kv_regions(
    pages: &[i32],
    seq_len: usize,
    page_size: usize,
    n_heads: usize,
    head_bytes: usize,
) -> Vec<(usize, usize)> {
    let page_head_bytes = page_size * head_bytes;
    let page_bytes = n_heads * page_head_bytes;
    let mut regions = Vec::with_capacity(pages.len() * n_heads);
    let mut remaining = seq_len;
    for &page in pages {
        if remaining == 0 {
            break;
        }
        let tokens = remaining.min(page_size);
        for head in 0..n_heads {
            regions.push((
                page as usize * page_bytes + head * page_head_bytes,
                tokens * head_bytes,
            ));
        }
        remaining -= tokens;
    }
    regions
}

/// Persistent continuous-batching decode scratch sized for `cap` sequences.
/// Activation matrices are `[cap, cols]` row-major (the batched GEMM/attention
/// kernels consume them directly, one row per active sequence). Per-step inputs
/// (ids/positions/seq_lens/page_table) and per-seq sampling params live in
/// device buffers refreshed by one async H2D per replay, so the forward+logits
/// path is CUDA-graph-replayable per batch-size bucket.
struct BatchBufs {
    cap: usize,
    h: DevBuffer,
    x: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_parts: DevBuffer,
    attn_out: DevBuffer,
    o_out: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    logits: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    seq_lens: DevBuffer,
    page_table: DevBuffer,
    /// Pinned staging: [ids | positions | seq_lens], i32, cap each.
    pinned_meta: DevBuffer,
    pinned_pt: DevBuffer,
    /// Pinned embeddingi modeli hybrydowych `[cap, hidden]` f16.
    pinned_embed: DevBuffer,
    /// Per-seq sampling params (device + pinned staging).
    samp_k: DevBuffer,
    samp_inv_t: DevBuffer,
    samp_top_p: DevBuffer,
    samp_min_p: DevBuffer,
    samp_seed: DevBuffer,
    samp_step: DevBuffer,
    pinned_samp: DevBuffer,
    /// Histogramy kar: płaskie IDs i liczniki, offsety oraz parametry sekwencji.
    pen_ids: DevBuffer,
    pen_counts: DevBuffer,
    pen_offsets: DevBuffer,
    pen_vals: DevBuffer,
    pen_frequency: DevBuffer,
    pen_presence: DevBuffer,
    pinned_pen_ids: DevBuffer,
    pinned_pen_counts: DevBuffer,
    pinned_pen_offsets: DevBuffer,
    pinned_pen_vals: DevBuffer,
    pinned_pen_frequency: DevBuffer,
    pinned_pen_presence: DevBuffer,
    out_ids: DevBuffer,
    pinned_out: DevBuffer,
    nvfp4_ct_qkv: Option<DevBuffer>,
    nvfp4_ct_gate_up: Option<DevBuffer>,
    nvfp4_ct_workspace: Option<DevBuffer>,
}

/// MoE scratch (allocated only for Mixture-of-Experts models). The router
/// output is sized for a full prefill chunk; decode uses the first row.
struct MoeBufs {
    /// Selected expert ids, i32 [MAX_PREFILL_CHUNK * top_k].
    ids: DevBuffer,
    /// Routing weights, f32 [MAX_PREFILL_CHUNK * top_k].
    weights: DevBuffer,
    pinned_ids: DevBuffer,
    pinned_weights: DevBuffer,
    /// One token's FFN-normed hidden, f16 [hidden] — prefill copies a row here
    /// so the per-expert GEMV reads a contiguous single-token activation.
    xrow: DevBuffer,
    /// One expert's down-projection output, f16 [hidden].
    tmp: DevBuffer,
    /// Pinned-host landing for the shared-expert gate logit (f16), read back in
    /// the same sync as the router top-k (fallback readback path only).
    pinned_shared: DevBuffer,
    /// Device-resident shared-expert sigmoid gate scale (f32, one element). The
    /// device dispatch path computes it on-GPU so folding the shared expert
    /// needs no host round-trip.
    shared_scale: DevBuffer,
}

pub(crate) fn hybrid_prefill_b2_backend_capable(vendor: Vendor, warp_size: u32) -> bool {
    vendor == Vendor::Nvidia && warp_size == 32
}

/// Normy „sandwich" rodziny Gemma: delta bloku (wyjście uwagi albo FFN) jest
/// normalizowana PRZED dodaniem do rezyduum. Architektury bez tych tensorów nie
/// wykonują tu żadnego kernela.


/// Domyka blok: normuje deltę „sandwich", dodaje ją do rezyduum, skaluje
/// strumień skalarem warstwy i liczy normę wejściową następnego bloku. Gdy model
/// nie ma norm sandwich, schodzi na samą fuzję rezyduum + norma, a skalę (jeśli
/// jest) nakłada osobno — tak działają architektury bez tych tensorów.
#[allow(clippy::too_many_arguments)]
fn close_block(
    kernels: &Kernels,
    delta_norm: Option<&DevBuffer>,
    layer_scale: Option<f32>,
    x_out: &DevBuffer,
    h: &DevBuffer,
    delta: &DevBuffer,
    next_norm: &DevBuffer,
    rows: usize,
    hidden: usize,
    eps: f32,
    stream: &Stream,
) -> Result<()> {
    match delta_norm {
        Some(dn) => kernels.rmsnorm_delta_residual_f16(
            x_out,
            h,
            delta,
            dn,
            next_norm,
            rows,
            hidden,
            eps,
            layer_scale.unwrap_or(1.0),
            stream,
        ),
        None => {
            kernels
                .rmsnorm_residual_f16(x_out, h, delta, next_norm, rows, hidden, eps, stream)?;
            layer_output_scale(kernels, layer_scale, h, rows * hidden, stream)
        }
    }
}

fn pre_residual_norm(
    kernels: &Kernels,
    norm: Option<&DevBuffer>,
    delta: &DevBuffer,
    rows: usize,
    hidden: usize,
    eps: f32,
    stream: &Stream,
) -> Result<()> {
    if let Some(w) = norm {
        kernels.rmsnorm_f16(delta, delta, w, rows, hidden, eps, stream)?;
    }
    Ok(())
}

/// Skalar mnożący wyjście CAŁEJ warstwy. Kolejna norma RMS jest niewrażliwa na
/// skalę, więc wystarczy przeskalować sam strumień rezydualny — po fuzji
/// `rmsnorm_residual`, nie przed nią.
fn layer_output_scale(
    kernels: &Kernels,
    scale: Option<f32>,
    h: &DevBuffer,
    n: usize,
    stream: &Stream,
) -> Result<()> {
    if let Some(f) = scale {
        kernels.scale_f16(h, n, f, stream)?;
    }
    Ok(())
}

impl Model {
    fn nvfp4_tile_requested(layout: Nvfp4GgufLayout, capable: bool) -> Result<bool> {
        match layout {
            Nvfp4GgufLayout::RowMajor36 => Ok(false),
            Nvfp4GgufLayout::TileN128K64 if capable => Ok(true),
            Nvfp4GgufLayout::TileN128K64 => Err(ForgeError::Unsupported(
                "TileN128K64 wymaga NVIDIA warp32 i pełnego zestawu artefaktów NVFP4".into(),
            )),
        }
    }

    fn validate_nvfp4_tile_repacked(requested: bool, repacked_weights: usize) -> Result<()> {
        if requested && repacked_weights == 0 {
            return Err(ForgeError::Unsupported(
                "TileN128K64 wymaga co najmniej jednej kwalifikującej się wagi GGUF NVFP4".into(),
            ));
        }
        Ok(())
    }

    /// Nieliniowość bramkowanego FFN tego modelu.
    fn ffn_act(&self) -> forge_formats::FfnActivation {
        self.weights.descriptor.params.ffn_activation
    }

    /// Okno przesuwne uwagi dla warstwy `layer`; 0 = pełna uwaga przyczynowa.
    ///
    /// Architektury z naprzemienną geometrią (Gemma 4) mają okno tylko na
    /// części warstw; wzorzec jest już rozwinięty na wszystkie warstwy przez
    /// parser metadanych, więc tu wystarczy odczyt.
    fn attn_window(&self, layer: usize) -> usize {
        match &self.weights.descriptor.params.alt_attn {
            Some(alt) if alt.sliding.get(layer).copied().unwrap_or(false) => alt.window,
            _ => 0,
        }
    }

    fn target_kv_layer(&self, global_layer: usize) -> usize {
        self.kv
            .layer_index(global_layer)
            .expect("cache KV jest dostępny wyłącznie dla warstwy attention")
    }

    /// Wypisuje, ile wag zostało w VRAM, a ile jest czytane z pamięci hosta
    /// przez PCIe — bez tego łatwo nie zauważyć, że model cicho zszedł na
    /// wolniejszą ścieżkę.
    fn report_residency(res: WeightResidency) {
        if res.host_bytes == 0 {
            return;
        }
        let gib = |b: usize| b as f64 / (1024.0 * 1024.0 * 1024.0);
        tracing::warn!(
            vram_gib = format!("{:.2}", gib(res.vram_bytes)),
            host_gib = format!("{:.2}", gib(res.host_bytes)),
            host_pct = format!("{:.1}", res.host_fraction() * 100.0),
            "część wag nie zmieściła się w VRAM i jest czytana z pamięci hosta przez PCIe"
        );
    }

    /// Rozkład ekspertów po warstwach pamięci, logowany po załadowaniu modelu.
    fn report_expert_residency(&self) {
        let Some((vram, host, nvme)) = self.moe_expert_residency() else {
            return;
        };
        if host == 0 && nvme == 0 {
            return;
        }
        tracing::info!(
            experts_vram = vram,
            experts_host = host,
            experts_nvme = nvme,
            "eksperci MoE rozłożeni między VRAM a pamięć hosta; rezydencja będzie przestawiana wg popularności"
        );
    }

    pub fn load_gguf(device: Arc<dyn Device>, path: &Path, cfg: ModelConfig) -> Result<Self> {
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;
        let target_tile = Self::nvfp4_tile_requested(
            cfg.nvfp4_gguf_layout,
            kernels.supports_nvfp4_gguf_tile_n128_k64(),
        )?;
        let repacked_weights = Cell::new(0);
        let tile_context = target_tile.then_some((&kernels, &stream, &repacked_weights));
        let sink: Arc<TieredWeightDevice> =
            Arc::new(TieredWeightDevice::new(device.clone(), cfg.weight_host_budget));
        let sink_dev: Arc<dyn Device> = sink.clone();
        let spill = Self::open_spill(&cfg, "gguf")?;
        let mut weights =
            ModelWeights::load_gguf(
                &sink_dev,
                path,
                cfg.native_mtp,
                tile_context,
                spill.as_ref(),
                cfg.weight_host_budget,
                cfg.layer_range,
            )?;
        Self::report_residency(sink.residency());
        Self::validate_nvfp4_tile_repacked(target_tile, repacked_weights.get())?;
        weights.nvfp4_repacked_weights = repacked_weights.get();
        Self::finish(sink_dev, weights, cfg, kernels, stream, spill)
    }

    pub fn load_safetensors_dir(
        device: Arc<dyn Device>,
        dir: &Path,
        cfg: ModelConfig,
    ) -> Result<Self> {
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;
        let target_tile = Self::nvfp4_tile_requested(
            cfg.nvfp4_gguf_layout,
            kernels.supports_nvfp4_gguf_tile_n128_k64(),
        )?;
        let repacked_weights = Cell::new(0);
        let tile_context = target_tile.then_some((&kernels, &stream, &repacked_weights));
        let sink: Arc<TieredWeightDevice> =
            Arc::new(TieredWeightDevice::new(device.clone(), cfg.weight_host_budget));
        let sink_dev: Arc<dyn Device> = sink.clone();
        let spill = Self::open_spill(&cfg, "safetensors")?;
        let mut weights =
            ModelWeights::load_safetensors_dir(
                &sink_dev,
                dir,
                cfg.native_mtp,
                tile_context,
                (&kernels, &stream, cfg.nvfp4_ct_layout),
                spill.as_ref(),
                cfg.weight_host_budget,
            )?;
        Self::report_residency(sink.residency());
        Self::validate_nvfp4_tile_repacked(target_tile, repacked_weights.get())?;
        weights.nvfp4_repacked_weights = repacked_weights.get();
        Self::finish(sink_dev, weights, cfg, kernels, stream, spill)
    }

    /// Otwiera plik zrzutu wag, gdy konfiguracja wskazała katalog.
    fn open_spill(cfg: &ModelConfig, tag: &str) -> Result<Option<ExpertSpill>> {
        let spill = cfg
            .weight_spill_dir
            .as_ref()
            .map(|dir| ExpertSpill::create(dir, tag))
            .transpose()?;
        if let Some(spill) = spill.as_ref() {
            tracing::info!(path = ?spill.path(), "otwarto plik zrzutu wag ekspertów");
        }
        Ok(spill)
    }

    pub fn nvfp4_gguf_layout_summary(&self) -> (Nvfp4GgufLayout, usize) {
        let count = self.weights.nvfp4_repacked_weights;
        let layout = if count == 0 {
            Nvfp4GgufLayout::RowMajor36
        } else {
            Nvfp4GgufLayout::TileN128K64
        };
        (layout, count)
    }

    fn finish(
        device: Arc<dyn Device>,
        mut weights: ModelWeights,
        cfg: ModelConfig,
        kernels: Kernels,
        stream: Stream,
        spill: Option<ExpertSpill>,
    ) -> Result<Self> {
        let p = weights.descriptor.params.clone();
        // head_dim 256 has an f16-only attention specialization (qwen35moe
        // gated attention layers); the hybrid arch always uses the f16 cache.
        // 512 to warstwy globalne rodziny Gemma 4 (16 głowic Q na jedną KV).
        if p.head_dim != 64 && p.head_dim != 128 && p.head_dim != 256 && p.head_dim != 512 {
            return Err(ForgeError::Unsupported(format!(
                "head_dim {} has no attention specialization",
                p.head_dim
            )));
        }
        if weights.is_moe() {
            // The routed decode path is a dedicated, non-graph-captured chain
            // over the f16 paged cache; low-bit KV modes and tiering are tracked
            // follow-ups (they need the fused decode kernels MoE bypasses).
            if !matches!(cfg.kv_quant, KvQuant::F16) {
                return Err(ForgeError::Unsupported(
                    "MoE models currently support only the f16 KV cache".into(),
                ));
            }
            // The hybrid `qwen35moe` arch (attention + Gated-DeltaNet MoE) DOES
            // tier: only its ~10 attention layers hold a paged KV cache, and
            // that cache spills/restores/streams through the same tier manager
            // as the dense path. The DeltaNet layers keep a small resident
            // recurrent state that is never paged. Non-hybrid MoE (OLMoE,
            // Qwen3-MoE) still lacks a staged-attention decode chain.
            let hybrid = weights.descriptor.params.ssm.is_some();
            if cfg.kv_tier.enabled() && !hybrid {
                return Err(ForgeError::Unsupported(
                    "non-hybrid MoE models do not support KV tiering yet".into(),
                ));
            }
        }
        match cfg.kv_quant {
            KvQuant::F16 => {}
            KvQuant::Fp8 => {
                // The non-fused decode chain (qkv_post + attn_decode) has no
                // fp8 cache kernels; fp8 decode goes through attn_decode_split
                // exclusively.
                if !Self::fused_decode_available(&weights, device.caps().vendor) {
                    return Err(ForgeError::Unsupported(
                        "kv_dtype fp8 requires the fused decode path; this model's weight \
                         formats fall back to the separate decode kernels"
                            .into(),
                    ));
                }
            }
            KvQuant::Rot { bits, .. } => {
                if bits != 3 && bits != 4 {
                    return Err(ForgeError::Unsupported(format!(
                        "rotational KV supports 3 or 4 bits, got {bits}"
                    )));
                }
                // Rot decode reads the packed store through attn_decode_rot;
                // prefill stays on the bit-exact f16 slab. Only head_dim 64/128
                // have compiled specializations (already checked above).
            }
        }
        let max_pages_per_seq = cfg.max_seq_len.div_ceil(cfg.kv_page_size);
        if weights.descriptor.layer_kinds.len() != p.block_count {
            return Err(ForgeError::Format(format!(
                "mapa typów warstw ma {} wpisów, oczekiwano {}",
                weights.descriptor.layer_kinds.len(),
                p.block_count
            )));
        }
        let kv_layer_map = KvLayerMap::from_attention_mask(
            weights
                .descriptor
                .layer_kinds
                .iter()
                .map(|kind| matches!(kind, forge_formats::LayerKind::Attention)),
        );
        let kv = KvCache::new_mapped(
            device.as_ref(),
            KvConfig {
                n_layers: kv_layer_map.kv_layers(),
                // Cache mieści NAJSZERSZĄ warstwę: przy naprzemiennej geometrii
                // (Gemma 4: 8x256 okienne, 1x512 globalne) każda warstwa adresuje
                // swój slab własnymi wymiarami i mieści się w tym zakresie, a
                // warstwy węższe używają początku każdej strony. Dla modeli
                // jednorodnych ta faktoryzacja daje dokładnie n_kv_heads.
                n_kv_heads: p.kv_cache_heads(),
                head_dim: p.kv_cache_head_dim(),
                page_size: cfg.kv_page_size,
                n_pages: cfg.kv_pages,
                max_pages_per_seq,
                quant: cfg.kv_quant,
            },
            kv_layer_map,
        )?;
        let draft_head_mode = std::env::var("FORGE_MTP_DRAFT_HEAD").unwrap_or_else(|_| "q8".into());
        match draft_head_mode.as_str() {
            "q8" => {}
            "nvfp4" => {
                let (source, rows, cols) = match weights.mtp.as_ref().map(|mtp| &mtp.output) {
                    Some(DevWeight::Q8_0 { buf, rows, cols }) => (buf.clone(), *rows, *cols),
                    Some(_) => {
                        return Err(ForgeError::Unsupported(
                            "FORGE_MTP_DRAFT_HEAD=nvfp4 wymaga headu źródłowego Q8_0".into(),
                        ));
                    }
                    None => {
                        return Err(ForgeError::Unsupported(
                            "FORGE_MTP_DRAFT_HEAD=nvfp4 wymaga modelu z MTP".into(),
                        ));
                    }
                };
                let bytes = rows
                    .checked_mul(cols / 64)
                    .and_then(|blocks| blocks.checked_mul(36))
                    .ok_or_else(|| {
                        ForgeError::Format("przepełnienie rozmiaru headu draftu NVFP4".into())
                    })?;
                if let Some(available) = device.pool_available(Pool::Weights) {
                    if bytes > available {
                        return Err(ForgeError::OutOfMemory {
                            requested: bytes,
                            available,
                        });
                    }
                }
                let packed = device.alloc(bytes, MemKind::Device, Pool::Weights)?;
                kernels.pack_q8_0_nvfp4_gguf(&packed, &source, rows, cols, &stream)?;
                device.synchronize()?;
                weights
                    .mtp
                    .as_mut()
                    .expect("sprawdzono head MTP")
                    .draft_output = Some(DevWeight::NvFp4Gguf {
                    buf: packed,
                    output_scale: 1.0,
                    rows,
                    cols,
                    layout: Nvfp4GgufLayout::RowMajor36,
                });
            }
            value => {
                return Err(ForgeError::Unsupported(format!(
                    "FORGE_MTP_DRAFT_HEAD={value}: oczekiwano q8 lub nvfp4"
                )));
            }
        }
        let page_table_dev = device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Weights)?;
        let seq_len_dev = device.alloc(4, MemKind::Device, Pool::Weights)?;
        let (tier, tier_bufs) = if cfg.kv_tier.enabled() {
            let region_bytes = kv.cfg.tier_region_bytes()?;
            let mut slots = Vec::with_capacity(STAGE_SLOTS);
            for _ in 0..STAGE_SLOTS {
                let stage = region_bytes
                    .iter()
                    .map(|&rb| {
                        let bytes = max_pages_per_seq.checked_mul(rb).ok_or_else(|| {
                            ForgeError::Scheduler("rozmiar staging tier KV przekracza usize".into())
                        })?;
                        device.alloc(bytes, MemKind::Device, Pool::Weights)
                    })
                    .collect::<Result<Vec<_>>>()?;
                slots.push(StageSlot {
                    stage,
                    ready: device.create_event()?,
                    free: device.create_event()?,
                });
            }
            let identity: Vec<i32> = (0..max_pages_per_seq as i32).collect();
            let identity_pt =
                device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Weights)?;
            device.write(bytemuck::cast_slice(&identity), &identity_pt, 0)?;
            // Tier only the attention layers: for a dense/rot model that is
            // every layer (`layer_kinds` is all-Attention, so behavior is
            // unchanged), for the hybrid arch it is the ~10 attention layers
            // (the DeltaNet layers keep a resident recurrent state, never paged).
            let tier_layers: Vec<usize> = weights
                .descriptor
                .layer_kinds
                .iter()
                .enumerate()
                .filter(|(_, k)| matches!(k, forge_formats::LayerKind::Attention))
                .map(|(i, _)| i)
                .collect();
            let tm = TierManager::new(
                cfg.kv_tier.clone(),
                device.clone(),
                tier_layers,
                region_bytes.clone(),
            )?;
            (
                Some(tm),
                Some(TierBufs {
                    slots,
                    identity_pt,
                    region_bytes,
                }),
            )
        } else {
            (None, None)
        };
        let hidden = p.hidden_size;
        // Przy naprzemiennej geometrii (Gemma 4) warstwy różnią się szerokością
        // projekcji — bufory muszą pomieścić najszerszą z nich.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let inter = p.intermediate_size;
        let attn_parts_bytes = p
            .n_heads
            .checked_mul(ATTN_DECODE_GQA_SPLITS)
            .and_then(|elements| elements.checked_mul(p.head_dim.checked_add(2)?))
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Format("przepełnienie bufora partiali attention GQA".into())
            })?;
        // Persistent decode scratch lives in the activation pool: it is the
        // pool provisioned for exactly this purpose, and nothing else uses it
        // on the LLM path anymore (the ring never needs to wrap).
        let alloc = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let bufs = DecodeBufs {
            h: alloc(hidden)?,
            h32: device.alloc(hidden * 4, MemKind::Device, Pool::Activations)?,
            x: alloc(hidden)?,
            qkv: alloc(q_dim + 2 * kv_dim)?,
            q: alloc(q_dim)?,
            k: alloc(kv_dim)?,
            v: alloc(kv_dim)?,
            attn_out: alloc(q_dim)?,
            attn_parts: device.alloc(attn_parts_bytes, MemKind::Device, Pool::Activations)?,
            o_out: alloc(hidden)?,
            gate_up: alloc(2 * inter)?,
            gate: alloc(inter)?,
            up: alloc(inter)?,
            act: alloc(inter)?,
            down: alloc(hidden)?,
            logits: device.alloc(p.vocab_size * 4, MemKind::Device, Pool::Activations)?,
            ids: device.alloc(4, MemKind::Device, Pool::Activations)?,
            pos: device.alloc(4, MemKind::Device, Pool::Activations)?,
            pinned_in: device.alloc(12, MemKind::PinnedHost, Pool::Activations)?,
            pinned_pt: device.alloc(
                max_pages_per_seq * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            pinned_logits: device.alloc(
                p.vocab_size * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            sample_vals: device.alloc(
                forge_kernels::SAMPLE_SCRATCH_PAIRS * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
            sample_idx: device.alloc(
                forge_kernels::SAMPLE_SCRATCH_PAIRS * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
            sample_out: device.alloc(8, MemKind::Device, Pool::Activations)?,
            pinned_sample: device.alloc(8, MemKind::PinnedHost, Pool::Activations)?,
            penalty_ids: device.alloc(cfg.max_seq_len * 4, MemKind::Device, Pool::Activations)?,
            penalty_counts: device.alloc(
                cfg.max_seq_len * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
            pinned_penalty: device.alloc(
                cfg.max_seq_len * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            pinned_penalty_counts: device.alloc(
                cfg.max_seq_len * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
        };
        let moe_bufs = match &weights.descriptor.params.moe {
            Some(m) => {
                let top_k = m.n_experts_used;
                let idw = MAX_PREFILL_CHUNK * top_k;
                Some(MoeBufs {
                    ids: device.alloc(idw * 4, MemKind::Device, Pool::Activations)?,
                    weights: device.alloc(idw * 4, MemKind::Device, Pool::Activations)?,
                    pinned_ids: device.alloc(idw * 4, MemKind::PinnedHost, Pool::Activations)?,
                    pinned_weights: device.alloc(
                        idw * 4,
                        MemKind::PinnedHost,
                        Pool::Activations,
                    )?,
                    xrow: device.alloc(hidden * 2, MemKind::Device, Pool::Activations)?,
                    tmp: device.alloc(hidden * 2, MemKind::Device, Pool::Activations)?,
                    pinned_shared: device.alloc(2, MemKind::PinnedHost, Pool::Activations)?,
                    shared_scale: {
                        // Seed 1.0 so a shared expert without a per-token gate
                        // (no shared_gate) folds in unscaled; the device sigmoid
                        // kernel overwrites this each layer when a gate exists.
                        let sc = device.alloc(4, MemKind::Device, Pool::Activations)?;
                        device.write(&1.0f32.to_le_bytes(), &sc, 0)?;
                        sc
                    },
                })
            }
            None => None,
        };

        // Rezydencja ma sens tylko wtedy, gdy jakikolwiek ekspert wylądował
        // poza VRAM — inaczej nie ma czego przenosić i każda runda byłaby
        // czystym kosztem.
        let moe_residency = {
            let views: Vec<MoeLayerView<'_>> = weights
                .layers
                .iter()
                .filter_map(|layer| match &layer.ffn {
                    LayerFfn::Moe(moe) => Some(MoeLayerView {
                        gate: &moe.gate_exps,
                        up: &moe.up_exps,
                        down: &moe.down_exps,
                    }),
                    _ => None,
                })
                .collect();
            MoeResidencyState::new(device.as_ref(), weights.layers.len(), &views)?
        };
        let mtp_config = weights.mtp.as_ref().map(|_| {
            (
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: p.n_kv_heads,
                    head_dim: p.head_dim,
                    page_size: cfg.kv_page_size,
                    n_pages: cfg.kv_pages,
                    max_pages_per_seq,
                    quant: KvQuant::F16,
                },
                hidden,
                p.vocab_size,
            )
        });
        // Pula stanów DeltaNet i MTP rośnie wraz z liczbą przeplatanych sekwencji.
        let hybrid_states = match &weights.descriptor.params.ssm {
            Some(sp) => {
                let conv_bytes = sp.conv_dim() * (sp.d_conv - 1) * 2;
                let state_bytes = sp.n_v_heads() * sp.d_state * sp.d_state * 4;
                let layout = kernels.preferred_delta_state_layout(sp.d_state);
                #[cfg(feature = "test-hooks")]
                let layout = if std::env::var_os("FORGE_TEST_FORCE_DELTA_KEY_VALUE").is_some() {
                    DeltaStateLayout::KeyValue
                } else {
                    layout
                };
                Some(HybridStatePool::new(
                    device.clone(),
                    weights.descriptor.layer_kinds.clone(),
                    layout,
                    conv_bytes,
                    state_bytes,
                    mtp_config,
                )?)
            }
            None => None,
        };
        // Prefix caching is a strict optimization: engage only where a borrowed
        // prefix page is byte-identical to a fresh prefill and never mutated.
        // That means the verbatim F16/Fp8 paged cache with no tiering (tiering
        // spills/rewrites pages), no rotational store (position-indexed residual
        // ring, not per-page) and no hybrid arch (recurrent SSM state is not in
        // KV pages). Otherwise the cache stays inactive and behavior is
        // bit-for-bit unchanged.
        let prefix_eligible = cfg.prefix_cache
            && !cfg.kv_tier.enabled()
            && matches!(cfg.kv_quant, KvQuant::F16 | KvQuant::Fp8)
            && weights.descriptor.params.ssm.is_none();
        let prefix_cache =
            prefix_eligible.then(|| crate::prefix::PrefixCache::new(cfg.kv_page_size));
        let mut model = Model {
            stage_first_layer: cfg.layer_range.map(|(first, _)| first).unwrap_or(0),
            device,
            kernels,
            weights,
            kv,
            kv_reuse_poison: KvReusePoison::default(),
            stream,
            page_table_dev,
            seq_len_dev,
            max_pages_per_seq,
            bufs,
            prefill_bufs: None,
            hybrid_prefill_chunk_size: HYBRID_PREFILL_PORTABLE_CHUNK,
            verify_bufs: None,
            hybrid_verify_bufs: None,
            mtp_b2_bufs: None,
            hybrid_prefill_bufs: None,
            hybrid_prefill_b2_bufs: None,
            hybrid_layer_major_bufs: None,
            hybrid_verify_graphs: HashMap::new(),
            hybrid_verify_graph_disabled: HashSet::new(),
            decode_graph: None,
            decode_hybrid_graph: None,
            decode_moe_graph: None,
            decode_rot_graph: None,
            batch_bufs: None,
            batch_graphs: HashMap::new(),
            batch_cap: 0,
            tier,
            tier_bufs,
            pt_seq: 0,
            moe_bufs,
            moe_residency,
            expert_spill: spill,
            hybrid_states,
            hybrid_bufs: None,
            hybrid_debug: std::env::var("FORGE_HYBRID_DEBUG").is_ok_and(|v| v == "1"),
            prefix_cache,
            calib: None,
            prefill_profiles: VecDeque::new(),
        };
        model.report_expert_residency();
        let configured = model
            .is_hybrid()
            .then(|| std::env::var("FORGE_HYBRID_PREFILL_CHUNK").ok())
            .flatten();
        let chunk_config =
            hybrid_prefill_chunk_config_for_model(model.is_hybrid(), configured.as_deref())?;
        let contains_nvfp4 = model.hybrid_prefill_contains_nvfp4();
        let extended_requested = contains_nvfp4
            && match chunk_config {
                HybridPrefillChunkConfig::Auto => {
                    model.hybrid_prefill_extended_structural_capable()
                        && model.kernels.hybrid_prefill_nvfp4_artifact_chunk_limit()
                            > HYBRID_PREFILL_PORTABLE_CHUNK
                }
                HybridPrefillChunkConfig::Explicit(chunk) => chunk > HYBRID_PREFILL_PORTABLE_CHUNK,
            };
        if extended_requested {
            let structurally_capable = match chunk_config {
                HybridPrefillChunkConfig::Auto => {
                    model.hybrid_prefill_extended_structural_capable()
                }
                HybridPrefillChunkConfig::Explicit(_) => {
                    model.hybrid_prefill_t128_structural_capable()
                }
            };
            if !structurally_capable {
                return Err(ForgeError::Unsupported(
                    "rozszerzony chunk NVFP4 wymaga kompletnej ścieżki qwen35 T128".into(),
                ));
            }
            model.ensure_hybrid_bufs()?;
        }
        let selected = model.resolve_hybrid_prefill_chunk_size(chunk_config)?;
        if contains_nvfp4 && selected > HYBRID_PREFILL_PORTABLE_CHUNK {
            if !model.hybrid_prefill_extended_budget_capable(selected) {
                return Err(ForgeError::Unsupported(format!(
                    "pula activations nie mieści scratchu NVFP4 T{selected} z rezerwą"
                )));
            }
            model.hybrid_prefill_chunk_size = selected;
            model.ensure_hybrid_prefill_capacity(selected)?;
        } else {
            model.hybrid_prefill_chunk_size = selected;
        }
        if model.is_hybrid() {
            tracing::info!(
                chunk = model.hybrid_prefill_chunk_size,
                "wybrano wewnętrzny chunk hybrydowego prefill"
            );
        }
        Ok(model)
    }

    /// Przygotowuje wszystkie eventy przed startem workera, aby ich alokacja
    /// nie wchodziła do TTFT mierzonego żądania.
    pub fn prepare_prefill_profiles(&mut self, prompt_tokens: usize, runs: usize) -> Result<()> {
        if prompt_tokens == 0 || runs == 0 {
            return Err(ForgeError::Scheduler(
                "profil prefill wymaga dodatniej liczby tokenów i przebiegów".into(),
            ));
        }
        if !self.prefill_profiles.is_empty() {
            return Err(ForgeError::Scheduler(
                "profil prefill został już przygotowany".into(),
            ));
        }
        let hybrid = self.is_hybrid();
        let layer_major_limit = self
            .hybrid_layer_major_prefill_limit()
            .filter(|_| prompt_tokens >= 32);
        if let Some(limit) = layer_major_limit {
            let arena_tokens = prompt_tokens.min(limit);
            self.ensure_hybrid_layer_major_bufs(arena_tokens)?;
            tracing::info!(
                tokens = arena_tokens,
                bytes = self
                    .hybrid_layer_major_bufs
                    .as_ref()
                    .expect("arena layer-major została zaalokowana")
                    .device_bytes,
                "zaalokowano prototypową arenę layer-major"
            );
        }
        let hybrid_batched = std::env::var("FORGE_HYBRID_BATCH_PREFILL")
            .map_or(true, |value| value != "0")
            && prompt_tokens > 1
            && self.validate_hybrid_speculation_target().is_ok();
        let target_spans = if let Some(limit) = layer_major_limit {
            prompt_tokens.div_ceil(limit)
        } else if hybrid_batched {
            hybrid_prefill_profile_spans(prompt_tokens, self.hybrid_prefill_chunk_size)
        } else if hybrid {
            prompt_tokens
        } else {
            prompt_tokens.div_ceil(MAX_PREFILL_CHUNK)
        };
        for _ in 0..runs {
            let mut target = Vec::with_capacity(target_spans);
            for _ in 0..target_spans {
                target.push(ProfileSpan {
                    start: self.device.create_timing_event()?,
                    end: self.device.create_timing_event()?,
                });
            }
            let catchup_spans = if layer_major_limit.is_some() || hybrid_batched {
                target_spans
            } else if hybrid {
                prompt_tokens
            } else {
                0
            };
            let mut catchup = Vec::with_capacity(catchup_spans);
            if hybrid {
                for _ in 0..catchup_spans {
                    catchup.push(ProfileSpan {
                        start: self.device.create_timing_event()?,
                        end: self.device.create_timing_event()?,
                    });
                }
            }
            self.prefill_profiles.push_back(PrefillProfileRun {
                target,
                catchup,
                target_cursor: 0,
                catchup_cursor: 0,
            });
        }
        Ok(())
    }

    pub fn take_prefill_profile(&mut self) -> Result<Option<PrefillProfile>> {
        let Some(run) = self.prefill_profiles.pop_front() else {
            return Ok(None);
        };
        if run.target_cursor != run.target.len() || run.catchup_cursor != run.catchup.len() {
            return Err(ForgeError::Scheduler(format!(
                "niepełny profil prefill: target {}/{}, MTP {}/{}",
                run.target_cursor,
                run.target.len(),
                run.catchup_cursor,
                run.catchup.len()
            )));
        }
        let target_gpu_ms = self.sum_profile_spans(&run.target)?;
        let mtp_catchup_gpu_ms = self.sum_profile_spans(&run.catchup)?;
        Ok(Some(PrefillProfile {
            target_gpu_ms,
            mtp_catchup_gpu_ms,
        }))
    }

    fn sum_profile_spans(&self, spans: &[ProfileSpan]) -> Result<Option<f64>> {
        if spans.is_empty() {
            return Ok(Some(0.0));
        }
        let mut total = 0.0;
        for span in spans {
            let Some(ms) = self.device.elapsed_event_ms(&span.start, &span.end)? else {
                return Ok(None);
            };
            total += f64::from(ms);
        }
        Ok(Some(total))
    }

    fn profile_target_start(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front() else {
            return Ok(());
        };
        let span = run.target.get(run.target_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil target prefill przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.start, &self.stream)
    }

    fn profile_target_end(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front_mut() else {
            return Ok(());
        };
        let span = run.target.get(run.target_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil target prefill przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.end, &self.stream)?;
        run.target_cursor += 1;
        Ok(())
    }

    fn profile_catchup_start(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front() else {
            return Ok(());
        };
        let span = run.catchup.get(run.catchup_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil MTP catch-up przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.start, &self.stream)
    }

    fn profile_catchup_end(&mut self) -> Result<()> {
        let Some(run) = self.prefill_profiles.front_mut() else {
            return Ok(());
        };
        let span = run.catchup.get(run.catchup_cursor).ok_or_else(|| {
            ForgeError::Scheduler("profil MTP catch-up przekroczył pojemność".into())
        })?;
        self.device.record_event(&span.end, &self.stream)?;
        run.catchup_cursor += 1;
        Ok(())
    }

    pub fn new_seq(&self) -> SeqKv {
        self.kv.new_seq()
    }

    pub fn ensure_kv_reuse_healthy(&self) -> Result<()> {
        self.kv_reuse_poison.ensure_healthy()
    }

    pub fn kv_reuse_poison_reason(&self) -> Option<&str> {
        self.kv_reuse_poison.reason()
    }

    fn synchronize_kv_fatal(&mut self, context: &str) -> Result<()> {
        let device = self.device.clone();
        fatal_kv_synchronize(&mut self.kv_reuse_poison, context, || device.synchronize())
    }

    /// Rezerwuje pamięć stanów targetu i MTP przed dopuszczeniem wielu requestów.
    pub fn preflight_hybrid_state_slots(&mut self, slots: usize) -> Result<()> {
        if slots == 0 {
            return Err(ForgeError::Scheduler(
                "preflight wymaga co najmniej jednego slotu".into(),
            ));
        }
        self.hybrid_states
            .as_mut()
            .ok_or_else(|| ForgeError::Unsupported("model nie jest hybrydowy".into()))?
            .ensure_capacity(slots)
    }

    pub fn release_seq(&mut self, seq: &mut SeqKv) {
        if self.kv_reuse_poison.is_poisoned() {
            let reason = self
                .kv_reuse_poison
                .reason()
                .expect("poison ma zapisany powód");
            tracing::error!(
                seq_id = seq.id,
                pages = seq.pages.len(),
                "sekwencja KV pozostaje w kwarantannie po fatalnym błędzie: {reason}"
            );
            return;
        }
        if let Some(lease) = seq.hybrid_state {
            let release = self
                .hybrid_states
                .as_mut()
                .expect("lease hybrydowy wymaga puli")
                .release(lease, &self.stream);
            match release {
                Ok(()) => seq.hybrid_state = None,
                Err(error) => {
                    tracing::error!("nie można bezpiecznie zwolnić stanu hybrydowego: {error}");
                }
            }
        }
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        if self.prefix_cache.is_some() {
            self.finalize_prefix(seq);
        }
        self.kv.release(seq);
    }

    fn activate_hybrid_sequence(&mut self, seq: &mut SeqKv) -> Result<()> {
        let pool = self.hybrid_states.as_mut().ok_or_else(|| {
            ForgeError::Scheduler("model hybrydowy nie ma puli stanów DeltaNet".into())
        })?;
        let lease = match seq.hybrid_state {
            Some(lease) => lease,
            None => {
                let lease = pool.acquire()?;
                seq.hybrid_state = Some(lease);
                lease
            }
        };
        pool.activate(lease, &self.stream)
    }

    fn active_ssm(&self) -> &[Option<SsmState>] {
        self.hybrid_states
            .as_ref()
            .expect("model hybrydowy ma pulę stanów")
            .active_layers()
    }

    fn delta_state_layout(&self) -> DeltaStateLayout {
        self.hybrid_states
            .as_ref()
            .expect("model hybrydowy ma pulę stanów")
            .layout()
    }

    fn take_mtp_runtime(
        &mut self,
        seq: &mut SeqKv,
    ) -> Result<(HybridStateLease, MtpDraftState, KvCache)> {
        self.activate_hybrid_sequence(seq)?;
        let lease = seq
            .hybrid_state
            .expect("aktywacja przydzieliła lease hybrydowy");
        let (state, kv) = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .take_mtp(lease)?;
        Ok((lease, state, kv))
    }

    fn take_mtp_runtime_pair(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
    ) -> Result<([HybridStateLease; 2], [MtpDraftState; 2], KvCache)> {
        self.activate_hybrid_sequence(seqs[0])?;
        let first = seqs[0]
            .hybrid_state
            .expect("aktywacja przydzieliła pierwszy lease hybrydowy");
        self.activate_hybrid_sequence(seqs[1])?;
        let second = seqs[1]
            .hybrid_state
            .expect("aktywacja przydzieliła drugi lease hybrydowy");
        let leases = [first, second];
        let (states, kv) = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .take_mtp_pair(leases)?;
        Ok((leases, states, kv))
    }

    fn restore_mtp_runtime(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
    ) -> Result<()> {
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .restore_mtp(lease, state, kv)
    }

    fn poison_mtp_runtime(&mut self, reason: String) -> ForgeError {
        self.hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .poison(reason)
    }

    fn finish_mtp_runtime<T>(
        &mut self,
        lease: HybridStateLease,
        state: MtpDraftState,
        kv: KvCache,
        result: Result<T>,
    ) -> Result<T> {
        match (result, self.restore_mtp_runtime(lease, state, kv)) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(error), Err(restore_error)) => Err(ForgeError::Scheduler(format!(
                "błąd wykonania MTP: {error}; błąd przywrócenia lease: {restore_error}"
            ))),
        }
    }

    fn finish_mtp_runtime_pair<T>(
        &mut self,
        leases: [HybridStateLease; 2],
        states: [MtpDraftState; 2],
        kv: KvCache,
        result: Result<T>,
    ) -> Result<T> {
        let restore = self
            .hybrid_states
            .as_mut()
            .expect("model hybrydowy ma pulę stanów")
            .restore_mtp_pair(leases, states, kv);
        match (result, restore) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(error), Err(restore_error)) => Err(ForgeError::Scheduler(format!(
                "błąd wykonania pary MTP: {error}; błąd przywrócenia lease: {restore_error}"
            ))),
        }
    }

    pub fn tier_enabled(&self) -> bool {
        self.tier.is_some()
    }

    /// Whether the radix prefix cache is active for this model.
    pub fn prefix_enabled(&self) -> bool {
        self.prefix_cache.is_some()
    }

    /// Longest cached-prefix length (tokens) servable for `prompt`, leaving at
    /// least one token to prefill (so the sequence still produces logits). Used
    /// by admission to project the reduced page demand; no state change.
    pub fn prefix_match_len(&self, prompt: &[u32]) -> usize {
        match &self.prefix_cache {
            Some(pc) if prompt.len() > self.kv.cfg.page_size => {
                pc.match_len(prompt, prompt.len() - 1)
            }
            _ => 0,
        }
    }

    /// Borrow the longest cached prefix of `prompt` into `seq` (SPEC §5.2):
    /// shared pages are attached read-only, `seq.len`/`tokens`/`prefilled_len`
    /// advance to the shared boundary, and the divergent suffix is left to
    /// prefill. Returns the number of prompt tokens served from cache
    /// (`cache_read_tokens`). At least one token is always left to prefill.
    pub fn acquire_prefix(&mut self, seq: &mut SeqKv, prompt: &[u32]) -> usize {
        let ps = self.kv.cfg.page_size;
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        if prompt.len() <= ps {
            return 0;
        }
        let (pages, node, shared) = pc.acquire(prompt, prompt.len() - 1);
        if shared == 0 {
            return 0;
        }
        seq.pages = pages;
        seq.shared_pages = seq.pages.len();
        seq.prefix_node = node;
        seq.len = shared;
        // Keep `tokens` page-aligned with `pages` so the completion-time
        // donation indexes shared + private pages uniformly. The borrowed pages
        // ARE the pages a prefill wrote, so the reused KV is bit-identical and
        // counts toward `prefilled_len`. The final logits are not: the divergent
        // tail is prefilled at a different token count than a cold run would
        // use, and the GEMM tile depends on that count. Greedy output can
        // therefore flip on near-tied logits depending on cache state — that is
        // why `forge bench` refuses to measure with the cache enabled.
        seq.tokens = prompt[..shared].to_vec();
        seq.prefilled_len = shared;
        // The single-stream decode path re-uploads the page table when a
        // different sequence's pages were resident; a borrow rewrites the table.
        self.pt_seq = 0;
        shared
    }

    /// Donate a completing sequence's freshly-prefilled complete pages back into
    /// the radix tree and release its borrow. Leading shared/donated pages are
    /// drained from `seq.pages` so the subsequent `kv.release` frees only the
    /// sequence's remaining private (partial + decode) pages.
    fn finalize_prefix(&mut self, seq: &mut SeqKv) {
        let ps = self.kv.cfg.page_size;
        let Some(node) = seq.prefix_node.take() else {
            // No borrow — but the sequence may still have prefilled a brand-new
            // prefix worth caching (cache miss). Donate from the root.
            let n_full = seq.prefilled_len / ps;
            if n_full == 0 {
                return;
            }
            let (dups, consumed) = {
                let pc = self.prefix_cache.as_mut().expect("prefix path");
                pc.donate(crate::prefix::ROOT, 0, n_full, &seq.tokens, &seq.pages)
            };
            for p in dups {
                self.kv.push_free(p);
            }
            seq.pages.drain(0..consumed.min(seq.pages.len()));
            seq.shared_pages = 0;
            return;
        };
        let n_full = seq.prefilled_len / ps;
        let (dups, consumed) = {
            let pc = self.prefix_cache.as_mut().expect("prefix path");
            let r = pc.donate(node, seq.shared_pages, n_full, &seq.tokens, &seq.pages);
            pc.release(node);
            r
        };
        for p in dups {
            self.kv.push_free(p);
        }
        seq.pages.drain(0..consumed.min(seq.pages.len()));
        seq.shared_pages = 0;
    }

    /// Reclaim up to `need` KV pages from the prefix cache (evicting refcount-0
    /// LRU prefixes) onto the free stack. No-op when the cache is inactive or
    /// already empty of evictable pages. Returns the number of pages freed.
    fn reclaim_prefix_pages(&mut self, need: usize) -> usize {
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        let freed = pc.evict(need);
        let n = freed.len();
        for p in freed {
            self.kv.push_free(p);
        }
        n
    }

    /// Ensure at least `need` free KV pages, evicting cached prefixes if the
    /// free stack is short. Called before prefill/decode growth so a cache hit
    /// never starves the pool.
    fn ensure_free_pages(&mut self, need: usize) {
        if self.prefix_cache.is_none() {
            return;
        }
        let free = self.kv.free_page_count();
        if free < need {
            self.reclaim_prefix_pages(need - free);
        }
    }

    /// Pages the engine can still hand out for a new request: the free stack
    /// plus everything the prefix cache can evict. Admission uses this so a
    /// reclaimable cache never blocks otherwise-fittable work.
    pub fn available_pages(&self) -> usize {
        self.kv.free_page_count()
            + self
                .prefix_cache
                .as_ref()
                .map(|pc| pc.evictable_pages())
                .unwrap_or(0)
    }

    /// Largest per-request KV demand (in pages) the engine can hold: the VRAM
    /// pool when tiering is off, the full context window when tiers extend it.
    pub fn max_request_pages(&self) -> usize {
        if self.tier.is_some() {
            self.max_pages_per_seq
        } else {
            self.kv.cfg.n_pages.min(self.max_pages_per_seq)
        }
    }

    /// Whether `seq`'s spilled pages can be restored without dropping the pool
    /// below the watermark reserve — restoring tighter than that would only
    /// thrash (the next step's capacity check would spill the pages again).
    fn tier_can_restore(&self, seq: &SeqKv) -> bool {
        let Some(tier) = &self.tier else { return false };
        seq.spilled_page_count() + tier.reserve_pages(self.kv.cfg.n_pages)
            <= self.kv.free_page_count()
    }

    /// Cross-sequence eviction (SPEC §5.4B): spill the globally coldest pages
    /// — across every provided sequence — until the pool can absorb
    /// `upcoming_pages` of growth plus the watermark reserve. Sequences with
    /// the largest spillable cold prefix donate first, so one long-context
    /// request no longer stalls behind neighbors' cold history. No-op with
    /// tiering off.
    pub fn tier_balance(&mut self, seqs: &mut [&mut SeqKv], upcoming_pages: usize) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let need = upcoming_pages + tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need {
            return Ok(());
        }
        let mut deficit = need - free;
        while deficit > 0 {
            let Some((idx, spillable)) = seqs
                .iter()
                .enumerate()
                .map(|(i, s)| (i, tier.spillable_pages(s)))
                .filter(|&(_, sp)| sp > 0)
                .max_by_key(|&(_, sp)| sp)
            else {
                break;
            };
            let take = deficit.min(spillable);
            let done = tier.spill(&mut self.kv, &mut *seqs[idx], take, &self.stream)?;
            if done == 0 {
                break;
            }
            self.pt_seq = 0;
            deficit = deficit.saturating_sub(done);
        }
        Ok(())
    }

    /// Spill this sequence's coldest pages until the pool can absorb
    /// `new_tokens` more tokens plus the watermark reserve. No-op with
    /// tiering off (the pool then errors on exhaustion, as before).
    fn tier_ensure_capacity(&mut self, seq: &mut SeqKv, new_tokens: usize) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let ps = self.kv.cfg.page_size;
        let need = (seq.len + new_tokens)
            .div_ceil(ps)
            .saturating_sub(seq.pages.len());
        let reserve = tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need + reserve {
            return Ok(());
        }
        let deficit = need + reserve - free;
        let spilled = tier.spill(&mut self.kv, seq, deficit, &self.stream)?;
        if spilled > 0 {
            self.pt_seq = 0;
        }
        Ok(())
    }

    /// Transfer-vs-recompute rule (SPEC §5.4): restore spilled chunks when the
    /// estimated transfer time beats re-prefilling the history. Recompute is
    /// only bit-identical for a purely prefilled history (decode writes its
    /// K/V through different kernels), so decode-extended sequences always
    /// transfer. Every decision is logged with the measured estimates.
    fn tier_restore_or_recompute(&mut self, seq: &mut SeqKv) -> Result<()> {
        let tier = self.tier.as_ref().expect("caller checked tiering");
        let (bytes, t_transfer) = tier.restore_cost(seq);
        let recompute_ok = seq.prefilled_len == seq.tokens.len() && !seq.tokens.is_empty();
        let t_recompute = tier.recompute_cost(seq.len);
        let use_recompute = recompute_ok && t_recompute < t_transfer;
        tracing::info!(
            "kv tier decision: seq {} transfer {:.1} MiB ≈ {:.1} ms vs recompute {} tok ≈ {:.1} ms → {}{}",
            seq.id,
            bytes as f64 / (1 << 20) as f64,
            t_transfer * 1e3,
            seq.len,
            t_recompute * 1e3,
            if use_recompute { "recompute" } else { "transfer" },
            if recompute_ok {
                ""
            } else {
                " (recompute ineligible: decode-written KV)"
            },
        );
        if use_recompute {
            self.recompute_seq(seq)
        } else {
            let tier = self.tier.as_mut().expect("checked above");
            tier.restore_all(&mut self.kv, seq, &self.stream)?;
            self.pt_seq = 0;
            Ok(())
        }
    }

    /// Rebuild `seq`'s KV from its retained tokens by re-prefilling from
    /// scratch, dropping all tier chunks first (recompute preemption).
    fn recompute_seq(&mut self, seq: &mut SeqKv) -> Result<()> {
        let toks = std::mem::take(&mut seq.tokens);
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        self.kv.release(seq);
        self.pt_seq = 0;
        if let (Some(pool), Some(lease)) = (&mut self.hybrid_states, seq.hybrid_state) {
            pool.reset(lease, &self.stream)?;
        }
        for chunk in toks.chunks(MAX_PREFILL_CHUNK) {
            if self.is_hybrid() {
                self.prefill_hybrid(seq, chunk)?;
            } else {
                self.prefill_forward(seq, chunk, true)?;
            }
        }
        Ok(())
    }

    fn gemv(&self, y: &DevBuffer, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Q8_0/Q4_K take the int8-activation dp4a kernels (measured faster at
        // every decode shape); columns beyond the kernels' shared staging
        // bound keep the f16-x path. Q6_K stays on f16 x: its dot is already
        // bandwidth-bound and the dp4a variant's extra shared usage costs
        // occupancy (measured slower at the down-projection shape).
        match w {
            DevWeight::Fp8Row {
                buf,
                scales,
                rows,
                cols,
            } => self
                .kernels
                .gemv_fp8_row_f16(y, buf, scales, x, *rows, *cols, stream),
            DevWeight::F16 { buf, rows, cols } => {
                self.kernels.gemv_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q8_0_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q8_0_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q4_k_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                self.kernels.gemv_q6_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5K { buf, rows, cols } => {
                self.kernels.gemv_q5_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q3K { buf, rows, cols } => {
                self.kernels.gemv_q3_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q2K { buf, rows, cols } => {
                self.kernels.gemv_q2_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_0 { buf, rows, cols } => {
                self.kernels.gemv_q4_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_1 { buf, rows, cols } => {
                self.kernels.gemv_q4_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_0 { buf, rows, cols } => {
                self.kernels.gemv_q5_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_1 { buf, rows, cols } => {
                self.kernels.gemv_q5_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => {
                self.kernels.gemv_mxfp4_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => {
                self.kernels.gemv_iq2_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq3S { buf, rows, cols } => {
                self.kernels.gemv_iq3_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => {
                self.kernels.gemv_iq1_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq1M { buf, rows, cols } => {
                self.kernels.gemv_iq1_m_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemv_nvfp4_f16(
                        y,
                        packed,
                        scales,
                        x,
                        *rows,
                        *cols,
                        *inv_global_scale,
                        stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { .. } => {
                    let window = w.nvfp4_ct_row_window(0, *rows)?;
                    let view = Nvfp4CtS0View::new(
                        window.data(),
                        window.physical_rows(),
                        window.cols(),
                    )?;
                    self.kernels.gemv_nvfp4_ct_s0_n64k128_f16(
                        y,
                        view,
                        x,
                        window.row_offset(),
                        window.rows(),
                        *inv_global_scale,
                        stream,
                    )
                }
            },
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } => {
                if *layout == Nvfp4GgufLayout::TileN128K64 {
                    self.kernels.gemv_nvfp4_gguf_q8_1_group_layout_f16(
                        &[Nvfp4GgufQ8Projection {
                            output: y,
                            weights: buf,
                            rows: *rows,
                            output_scale: *output_scale,
                        }],
                        x,
                        *cols,
                        *layout,
                        stream,
                    )
                } else if self.device.caps().vendor == Vendor::Nvidia {
                    self.kernels.gemv_nvfp4_gguf_b1_f16(
                        y,
                        buf,
                        x,
                        *rows,
                        *cols,
                        *output_scale,
                        stream,
                    )
                } else {
                    self.kernels.gemv_nvfp4_gguf_q8_1_group_f16(
                        &[Nvfp4GgufQ8Projection {
                            output: y,
                            weights: buf,
                            rows: *rows,
                            output_scale: *output_scale,
                        }],
                        x,
                        *cols,
                        stream,
                    )
                }
            }
        }
    }

    /// Wykonuje kilka pełnych projekcji GGUF NVFP4 ze wspólną kwantyzacją
    /// aktywacji Q8_1. Zwraca `false`, gdy choć jedna waga ma inny format.
    /// Wykonuje kilka projekcji Q8_0 jednym uruchomieniem. Zwraca `false`, gdy
    /// choć jedna waga ma inny format albo szerokości się różnią.
    fn gemv_q8_0_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::Q8_0 { buf, rows, cols: weight_cols } = weight else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Ok(false);
            }
            cols = Some(*weight_cols);
            group.push((output, buf, *rows));
        }
        let Some(cols) = cols else {
            return Ok(false);
        };
        self.kernels.gemv_q8_0_dp4a_group_f16(&group, x, cols, stream)
    }

    fn gemv_nvfp4_gguf_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut weight_layout = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols: weight_cols,
                layout,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Err(ForgeError::Format(
                    "projekcje NVFP4 współdzielące Q8_1 mają różne szerokości".into(),
                ));
            }
            if weight_layout.is_some_and(|value| value != *layout) {
                return Ok(false);
            }
            weight_layout = Some(*layout);
            cols = Some(*weight_cols);
            group.push(Nvfp4GgufQ8Projection {
                output,
                weights: buf,
                rows: *rows,
                output_scale: *output_scale,
            });
        }
        let Some(cols) = cols else { return Ok(false) };
        let layout = weight_layout.unwrap_or(Nvfp4GgufLayout::RowMajor36);
        if layout == Nvfp4GgufLayout::TileN128K64 {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_layout_f16(&group, x, cols, layout, stream)?;
        } else if self.device.caps().vendor == Vendor::Nvidia {
            for projection in group {
                self.kernels.gemv_nvfp4_gguf_b1_f16(
                    projection.output,
                    projection.weights,
                    x,
                    projection.rows,
                    cols,
                    projection.output_scale,
                    stream,
                )?;
            }
        } else {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_f16(&group, x, cols, stream)?;
        }
        Ok(true)
    }

    /// True when `w` can be consumed by the fused decode kernels
    /// (gemv_norm / gemv_norm_silu / gemv_residual format + column coverage).
    fn fused_decode_weight_ok(w: &DevWeight) -> bool {
        match w {
            DevWeight::Fp8Row { .. } => false,
            DevWeight::F16 { cols, .. } => cols.is_multiple_of(8),
            DevWeight::Q8_0 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::NvFp4 {
                storage: NvFp4CtStorage::RowMajorE4M3 { .. },
                cols,
                ..
            } => cols.is_multiple_of(16),
            DevWeight::NvFp4 {
                storage: NvFp4CtStorage::S0N64K128 { .. },
                cols,
                ..
            } => cols.is_multiple_of(128),
            DevWeight::NvFp4Gguf { .. } => false,
            // Q4_K stages per-32-column x sums in shared memory
            // (Q4K_MAX_SEGS in gemv2.mojo bounds cols at 32768).
            DevWeight::Q4K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q6K { cols, .. } => cols.is_multiple_of(256),
            // Q5_K shares Q4_K's 32-column x-sum staging bound; Q2_K stages
            // 16-column sums with the same 32768 ceiling.
            DevWeight::Q5K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q3K { cols, .. } => cols.is_multiple_of(256),
            DevWeight::Q2K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q4_0 { cols, .. }
            | DevWeight::Q4_1 { cols, .. }
            | DevWeight::Q5_0 { cols, .. }
            | DevWeight::Q5_1 { cols, .. }
            | DevWeight::Iq4Nl { cols, .. }
            | DevWeight::Mxfp4 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::Iq4Xs { cols, .. }
            | DevWeight::Iq2Xs { cols, .. }
            | DevWeight::Iq2S { cols, .. }
            | DevWeight::Iq3S { cols, .. }
            | DevWeight::Iq2Xxs { cols, .. }
            | DevWeight::Iq3Xxs { cols, .. }
            | DevWeight::Iq1S { cols, .. }
            | DevWeight::Iq1M { cols, .. } => cols.is_multiple_of(256),
        }
    }

    /// The fused decode step carries the residual stream as an (h, h32)
    /// pair with no standalone normed-x buffer and needs a hidden size that
    /// fits the kernels' shared-memory staging. QKV and gate/up may stay
    /// split (mixed formats, e.g. Q4_K q/k + Q6_K v, or Q5_K gate + Q6_K
    /// up): each projection then runs its own gemv_norm launch — same
    /// per-row math, only the norm recompute is repeated (gate/up adds an
    /// elementwise silu_mul). Anything else records the separate chain.
    fn fused_decode_supported(&self) -> bool {
        Self::fused_decode_available(&self.weights, self.device.caps().vendor)
    }

    fn fused_decode_available(weights: &ModelWeights, vendor: forge_types::Vendor) -> bool {
        let p = &weights.descriptor.params;
        // Kernele `gemv_norm_*` przeliczaja norme w KAZDEJ grupie roboczej i sa
        // strojone pod NVIDIA. Na gfx1030 profiler pokazal 182,95 us na wywolanie
        // dla projekcji FFN Mistrala (33 MB, czyli 181 GB/s), podczas gdy zwykly
        // GEMV na tej samej karcie robi 466 GB/s. Rozdzielenie normy i GEMV dalo
        // tam 67,2 -> 78,6 tok/s, a na Qwen3 286,6 -> 315,2. Dlatego poza NVIDIA
        // idzie sciezka rozdzielna.
        if vendor != forge_types::Vendor::Nvidia {
            return false;
        }
        if p.hidden_size > 8192 {
            return false;
        }
        // Naprzemienna geometria uwagi (Gemma 4: warstwy okienne 256/8 głowic i
        // globalne 512/1, dwie podstawy rope) nie da się wyrazić w fused
        // `qkv_post`, który zapieka jedną geometrię i jedną podstawę rope na całe
        // wywołanie. Takie modele idą ścieżką rozdzielną, liczącą wymiary per
        // warstwa.
        if p.alt_attn.is_some() {
            return false;
        }
        weights.layers.iter().all(|l| {
            // Routed MoE FFN has no fused single-GEMV decode kernel; MoE models
            // take the dedicated routed path (never this fused chain).
            let LayerFfn::Dense(dffn) = &l.ffn else {
                return false;
            };
            let qkv_ok = match &l.attn().attn_qkv {
                QkvWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                QkvWeights::FusedQk { qk, v } => {
                    Self::fused_decode_weight_ok(qk) && Self::fused_decode_weight_ok(v)
                }
                QkvWeights::Split { q, k, v } => {
                    Self::fused_decode_weight_ok(q)
                        && Self::fused_decode_weight_ok(k)
                        && Self::fused_decode_weight_ok(v)
                }
            };
            let gate_up_ok = match &dffn.gate_up {
                GateUpWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                // Mixed-format gate/up (e.g. Q5_K gate + Q6_K up) stays in
                // the fused chain: each projection runs its own gemv_norm
                // and a silu_mul combines them (see record_step_fused).
                GateUpWeights::Split { gate, up } => {
                    Self::fused_decode_weight_ok(gate) && Self::fused_decode_weight_ok(up)
                }
            };
            qkv_ok
                && gate_up_ok
                && Self::fused_decode_weight_ok(&l.attn().attn_o)
                && Self::fused_decode_weight_ok(&dffn.down)
        })
    }

    /// Fused rmsnorm-recompute + GEMV over the decode residual pair (h, h32).
    fn gemv_norm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        norm_w: &DevBuffer,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, rows, cols } => self.kernels.gemv_norm_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_q8_0_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemv_norm_nvfp4_f16(
                        y, packed, scales, &b.h, &b.h32, norm_w, *rows, *cols,
                        *inv_global_scale, ss_from_h16, eps, stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { data } => {
                    let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                    self.kernels.gemv_norm_nvfp4_ct_s0_f16(
                        y, view, &b.h, &b.h32, norm_w, *inv_global_scale,
                        ss_from_h16, eps, stream,
                    )
                }
            },
            DevWeight::Q4K { buf, rows, cols } => self.kernels.gemv_norm_q4_k_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_q6_k_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_q5_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_q3_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_q2_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_q4_0_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_q4_1_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_q5_0_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_q5_1_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_iq4_nl_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_iq4_xs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_mxfp4_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_iq2_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_iq3_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xxs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq3_xxs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_iq1_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_iq1_m_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_norm nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    /// Fused rmsnorm-recompute + gate|up GEMV + SiLU. `w` is the fused
    /// gate|up matrix; its row count is 2 * inter.
    fn gemv_norm_silu(
        &self,
        act: &DevBuffer,
        w: &DevWeight,
        norm_w: &DevBuffer,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, rows, cols } => self.kernels.gemv_norm_silu_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q8_0_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemv_norm_silu_nvfp4_f16(
                        act, packed, scales, &b.h, &b.h32, norm_w, rows / 2, *cols,
                        *inv_global_scale, eps, stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { data } => {
                    let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                    self.kernels.gemv_norm_silu_nvfp4_ct_s0_f16(
                        act, view, &b.h, &b.h32, norm_w, rows / 2,
                        *inv_global_scale, eps, stream,
                    )
                }
            },
            DevWeight::Q4K { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_k_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_silu_q6_k_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_silu_q3_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_silu_q2_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_0_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_1_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_0_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_1_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_nl_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_xs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_silu_mxfp4_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xxs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_xxs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_m_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_norm_silu nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    /// GEMV + residual add into the decode residual pair (h, h32).
    fn gemv_residual(&self, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Same dp4a policy as `gemv`: Q8_0/Q4_K quantize x block-locally and
        // dot with dp4a (wins at every decode shape), Q6_K keeps the f16-x
        // kernel (already bandwidth-bound; dp4a's shared staging loses
        // occupancy at the wide down-projection).
        let b = &self.bufs;
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_residual_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q8_0_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q8_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemv_residual_nvfp4_f16(
                        &b.h, &b.h32, packed, scales, x, *rows, *cols,
                        *inv_global_scale, stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { data } => {
                    let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                    self.kernels.gemv_residual_nvfp4_ct_s0_f16(
                        &b.h, &b.h32, view, x, *inv_global_scale, stream,
                    )
                }
            },
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q4_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q4_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q6_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q6_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q3_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q2_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_nl_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_residual_mxfp4_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_m_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_residual nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    fn logits_gemv(&self, y_f32: &DevBuffer, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Głowa NIE korzysta z paczki `fp8_lm_head`, choć ta jest budowana razem
        // z paczkami FFN. e4m3 ma 3-bitową mantysę w warstwie, która wprost
        // wybiera token: użycie jej TYLKO tutaj dawało inny strumień greedy w
        // pojedynczym strumieniu niż w batchu (który liczy głowę w F16), czyli
        // jakość zależną od współbieżności. Paczka zostaje dla prefillu, gdzie
        // liczą się aktywacje, a nie wybór tokena.
        self.logits_weight_gemv(y_f32, 0, x, 0, &self.weights.lm_head, stream)
    }

    fn logits_weight_gemv(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        x: &DevBuffer,
        x_off: usize,
        weight: &DevWeight,
        stream: &Stream,
    ) -> Result<()> {
        if (y_off != 0 || x_off != 0)
            && !matches!(weight, DevWeight::Q4K { .. } | DevWeight::Q6K { .. })
        {
            return Err(ForgeError::Unsupported(
                "gemv głowy logitów z offsetem lane obsługuje tylko Q4_K/Q6_K".into(),
            ));
        }
        let out = match weight {
            // Wagi FP8 ze skalą wierszową mają wariant GEMV z wyjściem f16;
            // głowa logitów potrzebuje f32, więc dostanie własną ścieżkę razem
            // z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => {
                return Err(ForgeError::Unsupported(
                    "głowa logitów nie obsługuje jeszcze wag FP8 ze skalą wierszową".into(),
                ))
            }
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_f16_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemv_q8_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q4_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q6_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q6_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_q5_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_q3_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_q2_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_q4_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_q4_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_q5_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_q5_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_mxfp4_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_iq2_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_iq3_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_iq1_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_iq1_m_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4 { .. } => Err(ForgeError::Unsupported(
                "NVFP4 lm_head has no f32-logit kernel yet".into(),
            )),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } => self.kernels.gemv_nvfp4_gguf_out_f32(
                if *layout == Nvfp4GgufLayout::RowMajor36 {
                    y_f32
                } else {
                    return Err(ForgeError::Unsupported(
                        "głowa logitów nie obsługuje TileN128K64".into(),
                    ));
                },
                buf,
                x,
                *rows,
                *cols,
                *output_scale,
                stream,
            ),
        };
        out?;
        // Ograniczenie logitów (Gemma): cap * tanh(x / cap). Nakładane tutaj, bo
        // to jedyne wyjście głowy — sampling i logprob widzą już wartości po capie.
        let cap = self.weights.descriptor.params.final_logit_softcap;
        if cap > 0.0 {
            self.kernels
                .softcap_f32(y_f32, y_off, weight.rows(), cap, stream)?;
        }
        // Maska tokenów zabronionych: kopie stream-ordered z jednoelementowego
        // bufora -inf, więc mieszczą się w przechwytywanym grafie decode.
        if let Some(neg_inf) = &self.weights.neg_inf {
            for &id in &self.weights.descriptor.params.suppress_tokens {
                let slot = y_off + id as usize;
                if slot < y_off + weight.rows() {
                    self.device.copy(neg_inf, 0, y_f32, slot * 4, 4, stream)?;
                }
            }
        }
        Ok(())
    }

    /// Batched GEMM over row-major activations x[t][col].
    fn gemm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_rows(y, w, x, n_tokens, 0, w.rows(), stream)
    }

    fn nvfp4_ct_projection(weight: &DevWeight) -> Option<Nvfp4CtProjection> {
        let DevWeight::NvFp4 {
            storage: NvFp4CtStorage::S0N64K128 { .. },
            rows,
            cols,
            ..
        } = weight else { return None };
        nvfp4_ct_projection_for_shape(*rows, *cols)
    }

    fn nvfp4_ct_model_capable(&self) -> bool {
        if !matches!(
            std::env::var("FORGE_NVFP4_CT_BM16").ok().as_deref(),
            None | Some("1")
        ) {
            return false;
        }
        let params = &self.weights.descriptor.params;
        let dimensions_capable = params
            .n_heads
            .checked_mul(params.head_dim)
            .zip(params.n_kv_heads.checked_mul(params.head_dim))
            .is_some_and(|(q_dim, kv_dim)| {
                nvfp4_ct_dimensions_capable(
                    params.hidden_size,
                    q_dim,
                    kv_dim,
                    params.intermediate_size,
                )
            });
        dimensions_capable
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && self.weights.layers.iter().all(|layer| {
                let QkvWeights::Fused(qkv) = &layer.attn().attn_qkv else {
                    return false;
                };
                let Ok(ffn) = layer.dense_ffn() else {
                    return false;
                };
                let GateUpWeights::Fused(gate_up) = &ffn.gate_up else {
                    return false;
                };
                Self::nvfp4_ct_projection(qkv) == Some(Nvfp4CtProjection::Qkv)
                    && Self::nvfp4_ct_projection(&layer.attn().attn_o)
                        == Some(Nvfp4CtProjection::Output)
                    && Self::nvfp4_ct_projection(gate_up)
                        == Some(Nvfp4CtProjection::GateUp)
                    && Self::nvfp4_ct_projection(&ffn.down)
                        == Some(Nvfp4CtProjection::Down)
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn gemm_nvfp4_ct_direct(
        &self,
        y_padded: &DevBuffer,
        workspace: &DevBuffer,
        weight: &DevWeight,
        x_padded: &DevBuffer,
        logical_m: usize,
        projection: Nvfp4CtProjection,
        stream: &Stream,
    ) -> Result<bool> {
        if nvfp4_ct_physical_m(logical_m).is_none()
            || Self::nvfp4_ct_projection(weight) != Some(projection)
        {
            return Ok(false);
        }
        let DevWeight::NvFp4 {
            inv_global_scale,
            rows,
            ..
        } = weight else { return Ok(false) };
        let window = weight.nvfp4_ct_row_window(0, *rows)?;
        let view =
            Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
        self.kernels.gemm_nvfp4_ct_padded(
            y_padded,
            if projection == Nvfp4CtProjection::GateUp {
                None
            } else {
                Some(workspace)
            },
            view,
            x_padded,
            logical_m,
            projection,
            *inv_global_scale,
            stream,
        )?;
        Ok(true)
    }

    fn delta_input_q8_cols(delta: &DeltaNetWeights) -> Option<usize> {
        let weights = [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj];
        let mut shared_cols = None;
        for weight in weights {
            let DevWeight::Q8_0 { cols, .. } = weight else {
                return None;
            };
            if shared_cols.is_some_and(|value| value != *cols) {
                return None;
            }
            shared_cols = Some(*cols);
        }
        shared_cols
    }

    fn gemm_q8_prepared(
        &self,
        y: &DevBuffer,
        weight: &DevWeight,
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        let DevWeight::Q8_0 {
            buf, rows, cols, ..
        } = weight
        else {
            return Err(ForgeError::Format(
                "przygotowana grupa DeltaNet wymaga wag Q8_0".into(),
            ));
        };
        self.kernels
            .gemm_q8_0_i8mma_prepared_at(y, buf, 0, prepared, *rows, *cols, n_tokens)
    }

    fn gemm_q8_prepared_triplet(
        &self,
        outputs: [&DevBuffer; 3],
        weights: [&DevWeight; 3],
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        fn projection<'a>(
            output: &'a DevBuffer,
            weight: &'a DevWeight,
        ) -> Result<(Q8PreparedProjection<'a>, usize)> {
            let DevWeight::Q8_0 {
                buf, rows, cols, ..
            } = weight
            else {
                return Err(ForgeError::Format(
                    "fused grupa DeltaNet wymaga wag Q8_0".into(),
                ));
            };
            Ok((
                Q8PreparedProjection {
                    output,
                    weights: buf,
                    weight_byte_offset: 0,
                    rows: *rows,
                },
                *cols,
            ))
        }
        let (gate, cols) = projection(outputs[0], weights[0])?;
        let (alpha, alpha_cols) = projection(outputs[1], weights[1])?;
        let (beta, beta_cols) = projection(outputs[2], weights[2])?;
        if alpha_cols != cols || beta_cols != cols {
            return Err(ForgeError::Format(
                "fused grupa DeltaNet wymaga wspólnego rozmiaru wejścia".into(),
            ));
        }
        self.kernels.gemm_q8_0_i8mma_prepared_triplet(
            &[gate, alpha, beta],
            prepared,
            cols,
            n_tokens,
        )
    }

    /// W4A8 prefill projection GEMM (per-token int8 activation quant + int4xint8
    /// GEMM). Each W4A8 weight is a standalone logical matrix, so no windowing.
    fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        w: &W4A8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels.gemm_w4a8(
            y,
            &w.qweight,
            &w.s2_zeros,
            &w.s2_scales,
            &w.s1_scales,
            &w.inv_smooth,
            x,
            w.rows,
            w.cols,
            n_tokens,
            stream,
        )
    }

    /// fp8 (e4m3) prefill projection GEMM (per-token e4m3 activation quant +
    /// e4m3×e4m3 tensor-core GEMM). Each fp8 weight is a standalone logical
    /// matrix, so no windowing. `FORGE_GEMM=fp8`.
    fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if self.weights.fp8_modular {
            return self.kernels.gemm_fp8_modular(
                y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
            );
        }
        self.kernels.gemm_fp8(
            y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
        )
    }

    /// fp8mod projection over the shared per-token e4m3 activation the preceding
    /// fused rmsnorm→fp8 emitted (q/k/v share one, gate/up share one) — no
    /// per-projection activation requant. `FORGE_GEMM=fp8mod` only.
    fn gemm_fp8_prequant(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels
            .gemm_fp8_modular_prequant(y, &w.qweight, &w.scales, w.rows, w.cols, n_tokens, stream)
    }

    /// Batched GEMM over a row window of `w`: y = W[row_off..row_off+n_rows]·x.
    /// Row offsets translate to per-format byte offsets into the weight (and,
    /// for NVFP4, scale) streams — this is how prefill reads the q/k/v and
    /// gate/up sections out of a fused matrix without storing them twice.
    #[allow(clippy::too_many_arguments)]
    fn gemm_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, cols, .. } => self.kernels.gemm_f16_at(
                y,
                buf,
                row_off * cols * 2,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            // Q8_0 / Q4_K prefill run the int8 TENSOR-CORE MMQ GEMM: activations
            // quantized to q8_1, weights kept as native codes, s8xs8->s32 mma
            // (m16n8k32) per 32-block, then per-block scale/min to f16. This is
            // the only path that beats the f16 tensor-core GEMM on Ada (2x MAC
            // throughput + zero dequant bandwidth). Decode still uses the dp4a
            // GEMV (see gemv). Marshalling the mma's 4x s32 output uses
            // inlined_assembly + _RegisterPackType (see kernels/mojo/MOJO_NOTES.md).
            DevWeight::Q8_0 { buf, cols, .. } => {
                let off = row_off * (cols / 32) * 34;
                // Jeden token bierze ten sam dp4a GEMV co dekod jednosekwencyjny.
                // Kafel i8mma dopełnia do >=64 tokenów i kwantyzuje aktywacje
                // inaczej, więc ścieżka batchowa dla B=1 dawała trwale inne
                // logity niż serialna przy zerowym zysku wydajności.
                if n_tokens == 1 {
                    return self.kernels.gemv_q8_0_dp4a_f16_at(
                        y, buf, off, x, n_rows, *cols, stream,
                    );
                }
                if self
                    .kernels
                    .gemm_q8_0_small_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q8_0_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            // Small decode batches (T=2/4/8/16) take the weight-stationary
            // dp4a GEMV: one weight sweep serves every token instead of the
            // >=64-token tile the GEMM kernels pad to.
            DevWeight::Q4K { buf, cols, .. } => {
                let off = row_off * (cols / 256) * 144;
                if self
                    .kernels
                    .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, false, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q4_k_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            DevWeight::Q6K { buf, cols, .. } => {
                let off = row_off * (cols / 256) * 210;
                if self
                    .kernels
                    .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, true, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q6_k_f16_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            DevWeight::Q5K { buf, cols, .. } => self.kernels.gemm_q5_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 176,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q3K { buf, cols, .. } => self.kernels.gemm_q3_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 110,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q2K { buf, cols, .. } => self.kernels.gemm_q2_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 84,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_0 { buf, cols, .. } => self.kernels.gemm_q4_0_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 18,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_1 { buf, cols, .. } => self.kernels.gemm_q4_1_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 20,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_0 { buf, cols, .. } => self.kernels.gemm_q5_0_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 22,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_1 { buf, cols, .. } => self.kernels.gemm_q5_1_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 24,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Nl { buf, cols, .. } => self.kernels.gemm_iq4_nl_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 18,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Xs { buf, cols, .. } => self.kernels.gemm_iq4_xs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 136,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Mxfp4 { buf, cols, .. } => self.kernels.gemm_mxfp4_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 17,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xs { buf, cols, .. } => self.kernels.gemm_iq2_xs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 74,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2S { buf, cols, .. } => self.kernels.gemm_iq2_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 82,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3S { buf, cols, .. } => self.kernels.gemm_iq3_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 110,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, cols, .. } => self.kernels.gemm_iq2_xxs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 66,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, cols, .. } => self.kernels.gemm_iq3_xxs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 98,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1S { buf, cols, .. } => self.kernels.gemm_iq1_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 50,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1M { buf, cols, .. } => self.kernels.gemm_iq1_m_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 56,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                cols,
                ..
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemm_nvfp4_f16_at(
                        y, packed, row_off * (cols / 2), scales,
                        row_off * (cols / 16), x, n_rows, *cols, n_tokens,
                        *inv_global_scale, stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { .. } => {
                    let window = w.nvfp4_ct_row_window(row_off, n_rows)?;
                    let view = Nvfp4CtS0View::new(
                        window.data(),
                        window.physical_rows(),
                        window.cols(),
                    )?;
                    if n_tokens <= 16 {
                        return self.kernels.gemv_batch_nvfp4_ct_s0_n64k128_f16_at(
                            y,
                            0,
                            view,
                            x,
                            0,
                            window.row_offset(),
                            window.rows(),
                            n_tokens,
                            *inv_global_scale,
                            stream,
                        );
                    }
                    self.kernels.gemm_nvfp4_ct_s0_f16_at(
                        y,
                        view,
                        x,
                        window.row_offset(),
                        window.rows(),
                        n_tokens,
                        *inv_global_scale,
                        stream,
                    )
                }
            },
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } if row_off == 0 && n_rows == *rows => self.kernels.gemm_nvfp4_gguf_layout_f16(
                y,
                buf,
                x,
                *rows,
                *cols,
                n_tokens,
                *output_scale,
                *layout,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "GGUF NVFP4 GEMM nie obsługuje okna wierszy".into(),
            )),
        }
    }

    /// Single-token GEMV over a row window of `w` (`y = W[row_off..+n_rows]·x`).
    /// The routed-MoE expert path uses this instead of the batched `gemm_rows`:
    /// a decode step feeds one token, and the GEMM tile (BM=64) then launches
    /// only `n_rows/64` blocks — far too few to saturate the SMs, so the GPU
    /// stays at idle clocks. The per-row GEMV kernels launch `n_rows/8` blocks
    /// (8 experts queued back-to-back per layer keep the device busy enough to
    /// boost). Formats without an offset GEMV variant fall back to the tile.
    fn gemv_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::Q4K { buf, cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_at(
                    y,
                    buf,
                    row_off * (cols / 256) * 144,
                    x,
                    n_rows,
                    *cols,
                    stream,
                )
            }
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemv_q6_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 210,
                x,
                n_rows,
                *cols,
                stream,
            ),
            _ => self.gemm_rows(y, w, x, 1, row_off, n_rows, stream),
        }
    }

    /// Whether `stack` is a routed-expert stack the device-side grouped dispatch
    /// can index without a host readback: the dp4a Q4_K path (cols within the
    /// dp4a bound) and the warp-per-row Q6_K path have `_gidx` kernels that read
    /// the expert selection on-device. Other quants keep the host-readback loop.
    fn expert_stack_gidx(stack: &ExpertStack) -> bool {
        match stack.representative() {
            DevWeight::Q4K { cols, .. } => *cols <= Kernels::DP4A_MAX_COLS,
            DevWeight::Q6K { .. } => true,
            _ => false,
        }
    }

    /// True when every routed-expert projection of `moe` supports the
    /// device-indexed dispatch (so the whole layer runs with zero host readback).
    /// Warstwa ze stronicowanym ekspertem NIE kwalifikuje się: kernel `_gidx`
    /// nie ma jak zaadresować bloku, który leży na dysku, a stwierdzić tego
    /// przed uruchomieniem można tylko po odczycie wyboru routera na hoście.
    fn moe_gidx_capable(moe: &MoeFfn) -> bool {
        [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .all(|stack| Self::expert_stack_gidx(stack) && stack.fully_resident())
    }

    /// Single-token GEMV over the expert selected on-device by `ids[sel]`: the
    /// device analog of `gemv_rows`, resolving that expert's weight base from
    /// the stack's device-resident pointer table inside the kernel.
    /// Bit-identical to `gemv_rows(y, stack.expert(ids[sel]), x, 0, n_rows, ..)`.
    /// Only the quants `expert_stack_gidx` accepts reach here.
    #[allow(clippy::too_many_arguments)]
    fn gemv_rows_gidx(
        &self,
        y: &DevBuffer,
        stack: &ExpertStack,
        x: &DevBuffer,
        ids: &DevBuffer,
        sel: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match stack.representative() {
            DevWeight::Q4K { cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_gidx(
                    y,
                    stack.table(),
                    x,
                    n_rows,
                    *cols,
                    ids,
                    sel,
                    stream,
                )
            }
            DevWeight::Q6K { cols, .. } => self.kernels.gemv_q6_k_f16_gidx(
                y,
                stack.table(),
                x,
                n_rows,
                *cols,
                ids,
                sel,
                stream,
            ),
            _ => Err(ForgeError::Unsupported(
                "gemv_rows_gidx called for a non-gidx expert quant".into(),
            )),
        }
    }

    fn ensure_prefill_bufs(&mut self) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let inter = p.intermediate_size;
        let t_max = if self.is_hybrid() {
            self.hybrid_prefill_chunk_size.max(4)
        } else {
            MAX_PREFILL_CHUNK
        };
        if self
            .prefill_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= t_max)
        {
            return Ok(());
        }
        let alloc = |elems: usize| {
            self.device
                .alloc(elems * 2, MemKind::Device, Pool::Activations)
        };
        let gate = alloc(t_max * inter)?;
        let act = if self.is_hybrid() {
            gate.clone()
        } else {
            alloc(t_max * inter)?
        };
        self.prefill_bufs = Some(PrefillBufs {
            cap: t_max,
            h: alloc(t_max * hidden)?,
            x: alloc(t_max * hidden)?,
            q: alloc(t_max * q_dim)?,
            k: alloc(t_max * kv_dim)?,
            v: alloc(t_max * kv_dim)?,
            attn_out: alloc(t_max * q_dim)?,
            o_out: alloc(t_max * hidden)?,
            gate,
            up: alloc(t_max * inter)?,
            act,
            down: alloc(t_max * hidden)?,
            ids: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
            positions: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
        });
        Ok(())
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through every
    /// transformer block in one batched pass, appending K/V to `seq`. Leaves
    /// the final-norm hidden states for the chunk's `t` tokens in
    /// `prefill_bufs.x` as a `[t, hidden]` row-major f16 matrix and returns
    /// `t`. `wait_for_completion` opróżnia stream przed zwróceniem dla wywołań,
    /// które odczytują `x` na hoście. Operacje device-only mogą kontynuować na
    /// tym samym streamie bez pośredniej synchronizacji.
    /// Dzielniki częstotliwości rope dla warstwy `l` — tylko warstwy globalne
    /// architektur z naprzemienną uwagą (Gemma 4) i tylko gdy model niesie
    /// tensor `rope_freqs`.
    fn rope_freqs_at(&self, p: &forge_formats::Hyperparams, l: usize) -> Option<&DevBuffer> {
        if p.rope_proportional_at(l) {
            self.weights.rope_freqs.as_ref()
        } else {
            None
        }
    }

    fn prefill_forward(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        wait_for_completion: bool,
    ) -> Result<usize> {
        self.prefill_forward_lanes(&mut [seq], &[tokens], wait_for_completion, None)
    }

    fn prefill_forward_lanes(
        &mut self,
        seqs: &mut [&mut SeqKv],
        token_lanes: &[&[u32]],
        wait_for_completion: bool,
        mixed_decode: Option<&MixedDecodeRows>,
    ) -> Result<usize> {
        self.ensure_kv_reuse_healthy()?;
        let p = self.weights.descriptor.params.clone();
        let batch = seqs.len();
        if batch == 0 || token_lanes.len() != batch {
            return Err(ForgeError::Scheduler(
                "prefill wymaga tej samej liczby sekwencji i lane'ów tokenów".into(),
            ));
        }
        let n_tokens = token_lanes[0].len();
        if n_tokens == 0 {
            return Err(ForgeError::Scheduler("empty prefill chunk".into()));
        }
        if token_lanes.iter().any(|tokens| tokens.len() != n_tokens) {
            return Err(ForgeError::Scheduler(
                "batch prefill wymaga równej liczby tokenów w każdym lane".into(),
            ));
        }
        let t = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie batch prefill".into()))?;
        // Mixed step: `db` decode rows ride the chunk's GEMMs/norms at row
        // offset `t`. They stay RAW through the chunk's qk-norm/rope/append —
        // `attn_decode_split` folds its own norm + RoPE + paged append. The
        // caller uploaded their ids into `MixedDecodeRows` and their attention
        // metadata (page tables, seq_lens, positions) into the batch buffers.
        let db = mixed_decode.map_or(0, |m| m.b);
        if db > 0
            && (batch != 1
                || self.calib.is_some()
                || self.tier.is_some()
                || self.kv.cfg.quant.is_rot()
                || self.weights.is_moe())
        {
            return Err(ForgeError::Unsupported(
                "mixed prefill+decode wymaga pojedynczego gęstego lane bez rot/tier/kalibracji".into(),
            ));
        }
        let rows = t + db;
        if rows > MAX_PREFILL_CHUNK {
            return Err(ForgeError::Scheduler(format!(
                "prefill chunk {rows} exceeds MAX_PREFILL_CHUNK {MAX_PREFILL_CHUNK}"
            )));
        }
        if batch > 1 && !self.dense_prefill_batch_capable(batch, n_tokens) {
            return Err(ForgeError::Unsupported(
                "batch prefill nie spełnia pełnego kontraktu backendu, modelu i artefaktów".into(),
            ));
        }
        let base_pos = seqs[0].len;
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        let mut total_new_pages = 0usize;
        for seq in seqs.iter_mut() {
            let new_len = seq.len.checked_add(n_tokens).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie długości sekwencji prefill".into())
            })?;
            if new_len > p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    new_len - 1,
                    p.max_position_embeddings
                )));
            }
            self.tier_ensure_capacity(seq, n_tokens)?;
            let required_pages = new_len.div_ceil(self.kv.cfg.page_size);
            if required_pages > self.kv.cfg.max_pages_per_seq {
                return Err(ForgeError::Scheduler(format!(
                    "sequence requires {required_pages} KV pages, limit is {}",
                    self.kv.cfg.max_pages_per_seq
                )));
            }
            total_new_pages = total_new_pages
                .checked_add(required_pages.saturating_sub(seq.pages.len()))
                .ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie liczby stron batch prefill".into())
                })?;
        }
        self.ensure_free_pages(total_new_pages);
        if total_new_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "batch prefill wymaga {total_new_pages} stron KV, dostępnych jest {}",
                self.kv.free_page_count()
            )));
        }
        self.ensure_prefill_bufs()?;
        if batch > 1 {
            self.ensure_batch(batch)?;
        }
        grow_prefill_lanes_transactional(&mut self.kv, seqs, n_tokens)?;
        if self.tier.is_some() || self.prefix_cache.is_some() {
            for (seq, tokens) in seqs.iter_mut().zip(token_lanes.iter()) {
                if seq.tokens.len() == seq.prefilled_len {
                    seq.prefilled_len += n_tokens;
                }
                seq.tokens.extend_from_slice(tokens);
            }
        }

        let streamed = batch == 1 && !seqs[0].spilled.is_empty();
        if streamed {
            self.tier
                .as_mut()
                .expect("spilled pages imply tiering")
                .prepare_streaming(seqs[0])?;
        }
        let mut page_tables = vec![-1i32; batch * self.max_pages_per_seq];
        let mut base_positions = Vec::with_capacity(batch);
        let mut ids = Vec::with_capacity(t);
        let mut positions = Vec::with_capacity(t);
        for (lane, (seq, tokens)) in seqs.iter().zip(token_lanes.iter()).enumerate() {
            let table = &mut page_tables
                [lane * self.max_pages_per_seq..(lane + 1) * self.max_pages_per_seq];
            table[..seq.pages.len()].copy_from_slice(&seq.pages);
            base_positions.push(seq.len as i32 - n_tokens as i32);
            ids.extend(tokens.iter().map(|&id| id as i32));
            positions.extend((seq.len - n_tokens..seq.len).map(|position| position as i32));
        }
        let segmented = if batch == 1 {
            self.device
                .write(bytemuck::cast_slice(&page_tables), &self.page_table_dev, 0)?;
            self.pt_seq = seqs[0].id;
            None
        } else {
            let bb = self.batch_bufs.as_ref().expect("batch prefill ma bufory");
            self.device
                .write(bytemuck::cast_slice(&page_tables), &bb.page_table, 0)?;
            self.device
                .write(bytemuck::cast_slice(&base_positions), &bb.seq_lens, 0)?;
            self.pt_seq = 0;
            Some((bb.page_table.clone(), bb.seq_lens.clone()))
        };
        let mut ids = ids;
        if let Some(m) = mixed_decode {
            ids.extend_from_slice(&m.ids);
        }
        let pb = self.prefill_bufs.as_ref().expect("allocated above");
        self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
        self.device
            .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;

        // W4A8 SmoothQuant calibration: pull the accumulator out of `self` so
        // the per-layer captures can borrow it mutably alongside the immutable
        // `pb`/`device` borrows. Restored before return; `None` in normal runs.
        let mut calib = self.calib.take();

        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;

        // fp8mod fused-norm path: the Modular fp8 GEMM shares ONE per-token e4m3
        // activation across q/k/v (and across gate/up) by folding the activation
        // quant into the preceding RMSNorm. Only when the fp8 packs are loaded, the
        // Modular kernel is selected, and no W4A8 calibration is capturing mid-layer
        // f16 activations.
        //
        // Q4_K prefill takes the plain rmsnorm (f16) → per-projection
        // `quantize_act_q8_1` → native int8 GEMM sequence (`gemm_q4_k_i8mma_at`
        // routes to `gemm_q4k_i8_native`). The old fused rmsnorm→`block_q8_1_mmq`
        // path was an MMQ-only perf optimization (one shared DS4 activation across
        // q/k/v & gate/up); it was retired with the CUDA MMQ kernel. The
        // shared-activation reuse can be re-added on top of the native kernel later.
        let fp8mod_fuse = self.weights.fp8.is_some() && self.weights.fp8_modular && calib.is_none();
        let fp8mod_ffn_fuse =
            self.weights.fp8_ffn.is_some() && self.weights.fp8_modular && calib.is_none();

        let mut trace = PrefillTrace::new();
        trace.start(self.device.as_ref());

        // Etap pipeline'u, który nie zaczyna się od warstwy zerowej, dostaje
        // strumień rezydualny z poprzedniej karty — `pb.h` jest już wypełniony,
        // więc embeddingu się nie liczy. Granicą etapu jest właśnie `pb.h`, a
        // nie znormalizowane `pb.x`: następny etap normalizuje po swojemu.
        if self.stage_first_layer == 0 {
            kernels.gather_rows_f16(
                &pb.h,
                &self.weights.token_embd_f16,
                &pb.ids,
                rows,
                hidden,
                stream,
            )?;
        }
        // Rodzina Gemma mnoży embedding przez sqrt(hidden). Norma RMS jest na
        // to niewrażliwa, ale strumień rezydualny już nie — bez tego wyjście
        // jest ciche. Skalowanie dotyczy WYŁĄCZNIE świeżo pobranego embeddingu:
        // etap dalszy dostaje już przeskalowany rezydual i drugie mnożenie
        // rozjechałoby stan.
        if self.stage_first_layer == 0 {
            if let Some(factor) = p.embd_scale {
                kernels.scale_f16(&pb.h, rows * hidden, factor, stream)?;
            }
        }
        self.trace_f16("stage_in", &pb.h, (rows - 1) * hidden * 2, hidden);
        // Layer 0's attn-norm feeds the q/k/v projections.
        if fp8mod_fuse {
            kernels.rmsnorm_fp8_shared(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                rows,
                hidden,
                eps,
                stream,
            )?;
        } else {
            kernels.rmsnorm_f16(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                rows,
                hidden,
                eps,
                stream,
            )?;
        }
        self.trace_f16("embd", &pb.h, (rows - 1) * hidden * 2, hidden);
        self.trace_f16("attn_norm-0", &pb.x, (rows - 1) * hidden * 2, hidden);
        trace.mark(self.device.as_ref(), "embed");

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            // Geometria per warstwa: przy naprzemiennej uwadze szerokości
            // projekcji i offsety sekcji scalonego q|k|v różnią się między
            // warstwami, więc muszą być liczone tutaj, a nie raz na model.
            let head_dim = p.head_dim_at(l);
            let n_kv_heads = p.n_kv_heads_at(l);
            let scale = p.attn_scale_at(l);
            let q_dim = p.n_heads * head_dim;
            let kv_dim = n_kv_heads * head_dim;
            let layer = &self.weights.layers[l];

            // W4A8 prefill (non-default): each projection is its own logical
            // pack, so q/k/v are three standalone GEMMs. The Q4_K weights stay
            // loaded for decode + the logit head.
            let w4a8_layer = self.weights.w4a8.as_ref().map(|v| &v[l]);
            let fp8_layer = self.weights.fp8.as_ref().map(|v| &v[l]);
            let fp8_ffn_layer = self.weights.fp8_ffn.as_ref().map(|v| &v[l]);

            // Calibration capture 1/4: q/k/v input (attn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.attn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            // Prefill outputs must stay [T, dim] contiguous per projection
            // (attention/rope/append index (t*heads+h)*head_dim), so a fused
            // matrix is consumed as three row-window GEMMs into separate
            // buffers — same weight bytes, no second copy in VRAM.
            if let Some(wl) = w4a8_layer {
                self.gemm_w4a8(&pb.q, &wl.q, &pb.x, rows, stream)?;
                self.gemm_w4a8(&pb.k, &wl.k, &pb.x, rows, stream)?;
                self.gemm_w4a8(&pb.v, &wl.v, &pb.x, rows, stream)?;
            } else if let Some(fl) = fp8_layer {
                if fp8mod_fuse {
                    // q/k/v read the shared per-token e4m3 activation the attn-norm
                    // already emitted — no per-projection requant.
                    self.gemm_fp8_prequant(&pb.q, &fl.q, rows, stream)?;
                    self.gemm_fp8_prequant(&pb.k, &fl.k, rows, stream)?;
                    self.gemm_fp8_prequant(&pb.v, &fl.v, rows, stream)?;
                } else {
                    self.gemm_fp8(&pb.q, &fl.q, &pb.x, rows, stream)?;
                    self.gemm_fp8(&pb.k, &fl.k, &pb.x, rows, stream)?;
                    self.gemm_fp8(&pb.v, &fl.v, &pb.x, rows, stream)?;
                }
            } else if let Some(fl) = fp8_ffn_layer {
                self.gemm_fp8(&pb.q, &fl.q, &pb.x, rows, stream)?;
                match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemm_rows(&pb.k, w, &pb.x, rows, q_dim, kv_dim, stream)?;
                    }
                    QkvWeights::FusedQk { qk, .. } => {
                        self.gemm_rows(&pb.k, qk, &pb.x, rows, q_dim, kv_dim, stream)?;
                    }
                    QkvWeights::Split { k, .. } => {
                        self.gemm(&pb.k, k, &pb.x, rows, stream)?;
                    }
                }
                match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemm_rows(
                            &pb.v,
                            w,
                            &pb.x,
                            rows,
                            q_dim + kv_dim,
                            kv_dim,
                            stream,
                        )?;
                    }
                    QkvWeights::FusedQk { v, .. } => {
                        self.gemm(&pb.v, v, &pb.x, rows, stream)?;
                    }
                    QkvWeights::Split { v, .. } => {
                        self.gemm(&pb.v, v, &pb.x, rows, stream)?;
                    }
                }
            } else {
                match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemm_rows(&pb.q, w, &pb.x, rows, 0, q_dim, stream)?;
                        self.gemm_rows(&pb.k, w, &pb.x, rows, q_dim, kv_dim, stream)?;
                        self.gemm_rows(&pb.v, w, &pb.x, rows, q_dim + kv_dim, kv_dim, stream)?;
                    }
                    QkvWeights::FusedQk { qk, v } => {
                        self.gemm_rows(&pb.q, qk, &pb.x, rows, 0, q_dim, stream)?;
                        self.gemm_rows(&pb.k, qk, &pb.x, rows, q_dim, kv_dim, stream)?;
                        self.gemm(&pb.v, v, &pb.x, rows, stream)?;
                    }
                    QkvWeights::Split { q, k, v } => {
                        self.gemm(&pb.q, q, &pb.x, rows, stream)?;
                        self.gemm(&pb.k, k, &pb.x, rows, stream)?;
                        self.gemm(&pb.v, v, &pb.x, rows, stream)?;
                    }
                }
            }
            if l == 0 {
                self.trace_f16("Qcur-0", &pb.q, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("Kcur-0", &pb.k, (rows - 1) * kv_dim * 2, kv_dim);
            }
            trace.mark(self.device.as_ref(), "gemm_qkv");

            // QK-norm granularity: OLMoE normalizes the whole q/k projection
            // once per token (rows = t), Qwen3 normalizes per head (rows =
            // t*n_heads). Dense non-OLMoE arches keep the per-head form
            // bit-for-bit (qk_norm_over_hidden == false).
            let attn_w = layer.attn();
            match (
                attn_w.q_norm.as_ref(),
                attn_w.k_norm.as_ref(),
                attn_w.v_norm.as_ref(),
            ) {
                // Wszystkie trzy normy per głowica: jedno uruchomienie zamiast
                // trzech (rodzina Gemma).
                (Some(qn), Some(kn), Some(vn)) if !p.qk_norm_over_hidden => {
                    kernels.rmsnorm_qkv_f16(
                        &pb.q,
                        &pb.k,
                        &pb.v,
                        qn,
                        kn,
                        vn,
                        t * p.n_heads,
                        t * n_kv_heads,
                        head_dim,
                        eps,
                        stream,
                    )?;
                }
                _ => {
                    if let Some(qn) = attn_w.q_norm.as_ref() {
                        if p.qk_norm_over_hidden {
                            kernels.rmsnorm_f16(&pb.q, &pb.q, qn, t, q_dim, eps, stream)?;
                        } else {
                            kernels
                                .rmsnorm_f16(&pb.q, &pb.q, qn, t * p.n_heads, head_dim, eps, stream)?;
                        }
                    }
                    if let Some(kn) = attn_w.k_norm.as_ref() {
                        if p.qk_norm_over_hidden {
                            kernels.rmsnorm_f16(&pb.k, &pb.k, kn, t, kv_dim, eps, stream)?;
                        } else {
                            kernels.rmsnorm_f16(
                                &pb.k,
                                &pb.k,
                                kn,
                                t * n_kv_heads,
                                head_dim,
                                eps,
                                stream,
                            )?;
                        }
                    }
                    if let Some(vn) = attn_w.v_norm.as_ref() {
                        kernels.rmsnorm_f16(
                            &pb.v,
                            &pb.v,
                            vn,
                            t * n_kv_heads,
                            head_dim,
                            eps,
                            stream,
                        )?;
                    }
                }
            }

            kernels.rope_neox_f16(
                &pb.q,
                &pb.positions,
                t,
                p.n_heads,
                head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            kernels.rope_neox_f16(
                &pb.k,
                &pb.positions,
                t,
                n_kv_heads,
                head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            if l == 0 {
                self.trace_f16("Qrope-0", &pb.q, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("Krope-0", &pb.k, (rows - 1) * kv_dim * 2, kv_dim);
            }
            trace.mark(self.device.as_ref(), "norm_rope");

            if let KvQuant::Rot { bits, .. } = self.kv.cfg.quant {
                // Rot: rotate+quant the chunk's rope'd K/V (linear pb.k/pb.v)
                // straight into the full-history packed store + residual ring —
                // no f16 slab. Packing must land before the attention launch,
                // which reads the packed store causally.
                let ring_slots = self
                    .kv
                    .cfg
                    .quant
                    .ring_slots()
                    .expect("rot mode has ring_slots");
                kernels.kv_pack_rot(
                    &self.kv.k_packed[self.target_kv_layer(l)],
                    &self.kv.v_packed[self.target_kv_layer(l)],
                    &self.kv.k_scale[self.target_kv_layer(l)],
                    &self.kv.v_scale[self.target_kv_layer(l)],
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &pb.k,
                    0,
                    &pb.v,
                    0,
                    &self.page_table_dev,
                    &pb.positions,
                    t,
                    n_kv_heads,
                    self.kv.cfg.page_size,
                    head_dim,
                    ring_slots,
                    bits,
                    stream,
                )?;
                trace.mark(self.device.as_ref(), "kv_pack_rot");
                if streamed {
                    // The chunk's packed K/V just landed in resident tail
                    // pages; staging pulls the full logical history (spilled
                    // chunks + resident pages) so the causal attention sees
                    // every position through the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seqs[0], l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                } else {
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k_packed[self.target_kv_layer(l)],
                        &self.kv.v_packed[self.target_kv_layer(l)],
                        &self.kv.k_scale[self.target_kv_layer(l)],
                        &self.kv.v_scale[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            } else {
                // Causal attention reads the chunk's own K/V from the cache, so
                // the batch append must land before the attention launch.
                if let Some((page_tables, base_positions)) = &segmented {
                    kernels.kv_append_batch_segmented_f16(
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &pb.k,
                        &pb.v,
                        page_tables,
                        base_positions,
                        batch,
                        n_tokens,
                        self.max_pages_per_seq,
                        n_kv_heads,
                        self.kv.cfg.page_size,
                        head_dim,
                        stream,
                    )?;
                } else {
                    kernels.kv_append_batch(
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &pb.k,
                        &pb.v,
                        &self.page_table_dev,
                        base_pos,
                        t,
                        n_kv_heads,
                        self.kv.cfg.page_size,
                        head_dim,
                        self.kv.cfg.dtype(),
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "kv_append");
                if streamed {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seqs[0], l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        self.attn_window(l),
                        stream,
                    )?;
                } else if let Some((page_tables, base_positions)) = &segmented {
                    if head_dim == 128 {
                        kernels.attn_prefill_fa_segmented_f16_hd128(
                            &pb.attn_out,
                            &pb.q,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            page_tables,
                            base_positions,
                            batch,
                            n_tokens,
                            p.n_heads,
                            n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_prefill_segmented_tiled_f16(
                            &pb.attn_out,
                            &pb.q,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            page_tables,
                            base_positions,
                            batch,
                            n_tokens,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    }
                } else {
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        self.attn_window(l),
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            }

            if let Some(m) = mixed_decode {
                // Decode rows: RAW q/k/v at row offset `t` — the fused split
                // kernel applies qk-norm + RoPE, appends each row's K/V to its
                // own sequence and attends over its pages (batch metadata
                // uploaded by `mixed_prefill_decode_step`).
                let bb = self.batch_bufs.as_ref().expect("mixed step ma batch bufory");
                kernels.attn_decode_split(
                    &bb.attn_parts,
                    &pb.q,
                    t * q_dim * 2,
                    &pb.k,
                    t * kv_dim * 2,
                    &pb.v,
                    t * kv_dim * 2,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &bb.page_table,
                    &bb.seq_lens,
                    &bb.positions,
                    m.b,
                    p.n_heads,
                    n_kv_heads,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &bb.attn_out,
                    &bb.attn_parts,
                    m.b,
                    p.n_heads,
                    head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
                self.device.copy(
                    &bb.attn_out,
                    0,
                    &pb.attn_out,
                    t * q_dim * 2,
                    m.b * q_dim * 2,
                    stream,
                )?;
            }

            // Calibration capture 2/4: o_proj input (attention output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.attn_out,
                    &mut cal.attn_out[l],
                    t,
                    &mut cal.scratch,
                )?;
            }
            if let Some(wl) = w4a8_layer {
                self.gemm_w4a8(&pb.o_out, &wl.attn_o, &pb.attn_out, rows, stream)?;
            } else if let Some(fl) = fp8_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, rows, stream)?;
            } else if let Some(fl) = fp8_ffn_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, rows, stream)?;
            } else {
                self.gemm(&pb.o_out, &layer.attn().attn_o, &pb.attn_out, rows, stream)?;
            }
            if l == 0 {
                self.trace_f16("attn_out-0", &pb.attn_out, (rows - 1) * q_dim * 2, q_dim);
                self.trace_f16("kqv_out-0", &pb.o_out, (rows - 1) * hidden * 2, hidden);
            }
            trace.mark(self.device.as_ref(), "gemm_o");
            let fp8mod_fuse_gateup =
                (fp8mod_fuse || fp8mod_ffn_fuse) && matches!(layer.ffn, LayerFfn::Dense(_));
            if fp8mod_fuse_gateup {
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            } else {
                close_block(
                    kernels,
                    layer.post_attn_norm.as_ref(),
                    None,
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res");

            // Calibration capture 3/4: gate/up input (ffn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.ffn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            match &layer.ffn {
                LayerFfn::Dense(dffn) => {
                    if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.gate, &wl.gate, &pb.x, rows, stream)?;
                        self.gemm_w4a8(&pb.up, &wl.up, &pb.x, rows, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        if fp8mod_fuse {
                            self.gemm_fp8_prequant(&pb.gate, &fl.gate, rows, stream)?;
                            self.gemm_fp8_prequant(&pb.up, &fl.up, rows, stream)?;
                        } else {
                            self.gemm_fp8(&pb.gate, &fl.gate, &pb.x, rows, stream)?;
                            self.gemm_fp8(&pb.up, &fl.up, &pb.x, rows, stream)?;
                        }
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8_prequant(&pb.gate, &fl.gate, rows, stream)?;
                        self.gemm_fp8_prequant(&pb.up, &fl.up, rows, stream)?;
                    } else {
                        match &dffn.gate_up {
                            GateUpWeights::Fused(w) => {
                                self.gemm_rows(&pb.gate, w, &pb.x, rows, 0, inter, stream)?;
                                self.gemm_rows(&pb.up, w, &pb.x, rows, inter, inter, stream)?;
                            }
                            GateUpWeights::Split { gate, up } => {
                                self.gemm(&pb.gate, gate, &pb.x, rows, stream)?;
                                self.gemm(&pb.up, up, &pb.x, rows, stream)?;
                            }
                        }
                    }
                    trace.mark(self.device.as_ref(), "gemm_gateup");
                    kernels.glu_mul_f16(self.ffn_act(), &pb.act, &pb.gate, &pb.up, rows * inter, stream)?;
                    trace.mark(self.device.as_ref(), "silu");
                    // Calibration capture 4/4: down_proj input (SwiGLU output).
                    if let Some(cal) = calib.as_mut() {
                        self.device.synchronize()?;
                        CalibAccum::absorb(
                            self.device.as_ref(),
                            &pb.act,
                            &mut cal.down_in[l],
                            t,
                            &mut cal.scratch,
                        )?;
                    }
                    if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.down, &wl.down, &pb.act, rows, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, rows, stream)?;
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, rows, stream)?;
                    } else {
                        self.gemm(&pb.down, &dffn.down, &pb.act, rows, stream)?;
                    }
                    trace.mark(self.device.as_ref(), "gemm_down");
                }
                LayerFfn::Moe(moe) => {
                    // Per-token routed experts written into pb.down [t, hidden].
                    self.moe_prefill_ffn(moe, l, t, hidden, stream)?;
                    trace.mark(self.device.as_ref(), "moe_ffn");
                }
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            // This norm feeds the NEXT layer's q/k/v (or, for the last layer, the
            // logit head — never fused, keeps the f16 hidden state).
            if l + 1 < n_layers && fp8mod_fuse {
                pre_residual_norm(
                    kernels,
                    layer.post_ffw_norm.as_ref(),
                    &pb.down,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x, &pb.h, &pb.down, next_norm, rows, hidden, eps, stream,
                )?;
                layer_output_scale(
                    kernels,
                    layer.layer_output_scale,
                    &pb.h,
                    rows * hidden,
                    stream,
                )?;
            } else {
                close_block(
                    kernels,
                    layer.post_ffw_norm.as_ref(),
                    layer.layer_output_scale,
                    &pb.x,
                    &pb.h,
                    &pb.down,
                    next_norm,
                    rows,
                    hidden,
                    eps,
                    stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res2");
            self.trace_f16(&format!("l_out-{l}"), &pb.h, (rows - 1) * hidden * 2, hidden);
        }

        if wait_for_completion || tier_t0.is_some() {
            self.synchronize_kv_fatal("dense prefill forward")?;
        }
        if let (Some(tier), Some(t0)) = (&self.tier, tier_t0) {
            // Measured prefill rate feeds the transfer-vs-recompute estimate.
            tier.note_prefill(t, t0.elapsed().as_secs_f64());
        }
        trace.report(t);
        self.calib = calib;
        Ok(t)
    }

    /// Build the W4A8 SmoothQuant packs from a one-time calibration pass over
    /// `calib_tokens` (a fixed built-in passage tokenized by the caller). Runs
    /// the coherent Q4_K prefill path collecting per-input-channel activation
    /// abs-max at the four linear inputs, then repacks every dense projection
    /// from the resident GGUF weights with per-channel migration folded into the
    /// weight (and the reciprocal into the GEMM's activation quantizer). Must be
    /// called once after load when `FORGE_GEMM=w4a8`; before it runs `w4a8` is
    /// `None` and prefill stays on the Q4_K path.
    pub fn calibrate_w4a8(&mut self, path: &Path, calib_tokens: &[u32]) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "W4A8 calibration supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        if calib_tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty W4A8 calibration input".into()));
        }
        let p = &self.weights.descriptor.params;
        let n_layers = self.weights.layers.len();
        let (hidden, q_dim, inter) = (p.hidden_size, p.max_q_dim(), p.intermediate_size);
        // Default is the identity requant (no SmoothQuant): measured best on the
        // Q4_K→W4A8 path, where the two-level requant error dominates and
        // migrating activation outliers only inflates the weights (see
        // docs/BENCH_COMPARISON.md). `FORGE_W4A8_ALPHA=<0..1>` opts into
        // SmoothQuant and triggers the calibration forward.
        let alpha = std::env::var("FORGE_W4A8_ALPHA")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(-1.0);

        // Ensure the Q4_K path runs during calibration (packs not built yet).
        self.weights.w4a8 = None;
        let stats = if alpha >= 0.0 {
            self.calib = Some(CalibAccum::new(n_layers, hidden, q_dim, inter));
            let mut seq = self.new_seq();
            let mut res = Ok(());
            for chunk in calib_tokens.chunks(MAX_PREFILL_CHUNK) {
                if let Err(e) = self.prefill_forward(&mut seq, chunk, true) {
                    res = Err(e);
                    break;
                }
            }
            self.release_seq(&mut seq);
            res?;
            let acc = self.calib.take().expect("calib accumulator set above");
            CalibStats {
                attn_in: acc.attn_in,
                attn_out: acc.attn_out,
                ffn_in: acc.ffn_in,
                down_in: acc.down_in,
                alpha,
            }
        } else {
            // Identity: smoothing_scale ignores the (unused) stats and returns 1.
            CalibStats {
                attn_in: vec![vec![0.0; hidden]; n_layers],
                attn_out: vec![vec![0.0; q_dim]; n_layers],
                ffn_in: vec![vec![0.0; hidden]; n_layers],
                down_in: vec![vec![0.0; inter]; n_layers],
                alpha,
            }
        };
        let layers = self
            .weights
            .rebuild_w4a8_smoothed(self.device.as_ref(), path, &stats)?;
        self.weights.w4a8 = Some(layers);
        Ok(())
    }

    /// Build the fp8 (e4m3) prefill packs from the resident GGUF weights. No
    /// calibration pass is needed (e4m3's exponent captures the per-row range),
    /// so this just dequantizes every dense projection and repacks it to e4m3
    /// with a per-row scale. Must be called once after load when
    /// `FORGE_GEMM=fp8`; before it runs `fp8` is `None` and prefill stays on the
    /// resident (Q4_K MMQ) path.
    pub fn build_fp8(&mut self, path: &Path) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "fp8 prefill supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        self.weights.fp8 = None;
        if !self.build_fp8_gpu()? {
            let layers = self.weights.rebuild_fp8(self.device.as_ref(), path)?;
            self.weights.fp8 = Some(layers);
        }
        self.weights.fp8_modular = crate::weights::fp8_modular_enabled();
        Ok(())
    }

    /// Pack one resident projection (row window) to e4m3 on the GPU.
    fn pack_fp8_gpu_window(
        &self,
        buf: &DevBuffer,
        quant: QuantKind,
        row_off: usize,
        rows: usize,
        cols: usize,
    ) -> Result<Fp8Weight> {
        let qweight = self
            .device
            .alloc(rows * cols, MemKind::Device, Pool::Weights)?;
        let scales = self.device.alloc(rows * 4, MemKind::Device, Pool::Weights)?;
        self.kernels
            .pack_gguf_fp8(&qweight, &scales, buf, row_off, rows, cols, quant, &self.stream)?;
        Ok(Fp8Weight {
            qweight,
            scales,
            rows,
            cols,
        })
    }

    /// Build the fp8 packs from the RESIDENT GGUF weights on the GPU (no disk
    /// re-read, no CPU dequant). Returns `Ok(false)` when any projection is
    /// not Q4_K/Q6_K/Q8_0 — the caller falls back to the CPU rebuild.
    pub fn build_fp8_gpu(&mut self) -> Result<bool> {
        fn packable(w: &DevWeight) -> Option<(&DevBuffer, usize, usize, QuantKind)> {
            match w {
                DevWeight::Q4K { buf, rows, cols } => Some((buf, *rows, *cols, QuantKind::Q4K)),
                DevWeight::Q6K { buf, rows, cols } => Some((buf, *rows, *cols, QuantKind::Q6K)),
                DevWeight::Q8_0 { buf, rows, cols } => Some((buf, *rows, *cols, QuantKind::Q8_0)),
                _ => None,
            }
        }
        let pack_full = |w: &DevWeight| -> Result<Option<Fp8Weight>> {
            let Some((buf, rows, cols, quant)) = packable(w) else {
                return Ok(None);
            };
            self.pack_fp8_gpu_window(buf, quant, 0, rows, cols).map(Some)
        };
        let pack_window = |w: &DevWeight, row_off: usize, rows: usize| -> Result<Option<Fp8Weight>> {
            let Some((buf, _, cols, quant)) = packable(w) else {
                return Ok(None);
            };
            self.pack_fp8_gpu_window(buf, quant, row_off, rows, cols).map(Some)
        };
        // Nothing is allocated before every projection passes the format
        // check, so a refusal leaves the weights pool untouched.
        for layer in &self.weights.layers {
            let LayerMixer::Attention(a) = &layer.mixer else {
                return Ok(false);
            };
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(false);
            };
            let mut ws: Vec<&DevWeight> = vec![&a.attn_o, &ffn.down];
            match &a.attn_qkv {
                QkvWeights::Split { q, k, v } => ws.extend([q, k, v]),
                QkvWeights::FusedQk { qk, v } => ws.extend([qk, v]),
                QkvWeights::Fused(qkv) => ws.push(qkv),
            }
            match &ffn.gate_up {
                GateUpWeights::Split { gate, up } => ws.extend([gate, up]),
                GateUpWeights::Fused(gu) => ws.push(gu),
            }
            if ws.into_iter().any(|w| packable(w).is_none()) {
                return Ok(false);
            }
        }
        let p = &self.weights.descriptor.params;
        let q_rows = p.n_heads * p.head_dim;
        let kv_rows = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let mut layers = Vec::with_capacity(self.weights.layers.len());
        for layer in &self.weights.layers {
            let LayerMixer::Attention(a) = &layer.mixer else {
                return Ok(false);
            };
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(false);
            };
            let (q, k, v) = match &a.attn_qkv {
                QkvWeights::Split { q, k, v } => (pack_full(q)?, pack_full(k)?, pack_full(v)?),
                QkvWeights::FusedQk { qk, v } => (
                    pack_window(qk, 0, q_rows)?,
                    pack_window(qk, q_rows, kv_rows)?,
                    pack_full(v)?,
                ),
                QkvWeights::Fused(qkv) => (
                    pack_window(qkv, 0, q_rows)?,
                    pack_window(qkv, q_rows, kv_rows)?,
                    pack_window(qkv, q_rows + kv_rows, kv_rows)?,
                ),
            };
            let (gate, up) = match &ffn.gate_up {
                GateUpWeights::Split { gate, up } => (pack_full(gate)?, pack_full(up)?),
                GateUpWeights::Fused(gu) => {
                    (pack_window(gu, 0, inter)?, pack_window(gu, inter, inter)?)
                }
            };
            let attn_o = pack_full(&a.attn_o)?;
            let down = pack_full(&ffn.down)?;
            match (q, k, v, attn_o, gate, up, down) {
                (Some(q), Some(k), Some(v), Some(attn_o), Some(gate), Some(up), Some(down)) => {
                    layers.push(Fp8Layer {
                        q,
                        k,
                        v,
                        attn_o,
                        gate,
                        up,
                        down,
                    });
                }
                _ => return Ok(false),
            }
        }
        self.stream.synchronize()?;
        self.weights.fp8 = Some(layers);
        self.weights.fp8_modular = true;
        Ok(true)
    }

    /// Auto-enable the Modular fp8 prefill for a dense GGUF model when the
    /// device has native fp8 tensor cores, every projection shape has a
    /// committed `gemm_fp8_mod_{rows}_{cols}` instance and the weights pool
    /// holds the e4m3 packs. Returns `Ok(false)` (prefill stays on the native
    /// GGUF path) when any gate fails; nothing is allocated before all gates
    /// pass, so a refusal leaves the model untouched.
    pub fn build_fp8_modular_auto(&mut self, path: &Path) -> Result<Fp8PackOutcome> {
        if self.is_hybrid() || self.weights.is_moe() || !self.device.caps().fp8_native {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let q_rows = p.n_heads * p.head_dim;
        let kv_rows = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        // (rows, cols, count) per layer: q, k, v, o, gate, up, down.
        let shapes = [
            (q_rows, hidden, 1usize),
            (kv_rows, hidden, 2),
            (hidden, q_rows, 1),
            (inter, hidden, 2),
            (hidden, inter, 1),
        ];
        let arts = self.kernels.artifacts();
        if shapes
            .iter()
            .any(|(rows, cols, _)| !arts.has(&format!("gemm_fp8_mod_{rows}_{cols}")))
        {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let per_layer: usize = shapes
            .iter()
            .map(|(rows, cols, n)| n * (rows * cols + rows * 4))
            .sum();
        let required = per_layer * self.weights.descriptor.params.block_count;
        let available = self.device.pool_available(Pool::Weights).unwrap_or(0);
        tracing::info!(required, available, "preflight paczek fp8mod dla GGUF");
        if required > available {
            return Ok(Fp8PackOutcome::PoolShortfall {
                required,
                available,
            });
        }
        self.weights.fp8 = None;
        if self.build_fp8_gpu()? {
            return Ok(Fp8PackOutcome::Built);
        }
        let layers = self.weights.rebuild_fp8(self.device.as_ref(), path)?;
        self.weights.fp8 = Some(layers);
        self.weights.fp8_modular = true;
        Ok(Fp8PackOutcome::Built)
    }

    fn pack_nvfp4_rows(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Result<Fp8Weight> {
        let DevWeight::NvFp4 {
            storage,
            inv_global_scale,
            rows: source_rows,
            cols,
        } = weight
        else {
            return Err(ForgeError::Unsupported(
                "fp8mod-ffn wymaga rezydentnych wag NVFP4".into(),
            ));
        };
        let row_end = row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Format("przepełnienie zakresu wierszy FP8".into()))?;
        if row_end > *source_rows {
            return Err(ForgeError::Format(format!(
                "zakres wierszy FP8 {}..{} przekracza {source_rows}",
                row_offset, row_end
            )));
        }
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let output_scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        let launch_result = match storage {
            NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                self.kernels.pack_nvfp4_fp8(
                    &qweight,
                    &output_scales,
                    packed,
                    scales,
                    *cols,
                    row_offset,
                    rows,
                    *inv_global_scale,
                    &self.stream,
                )
            }
            NvFp4CtStorage::S0N64K128 { .. } => {
                let window = weight.nvfp4_ct_row_window(row_offset, rows)?;
                let view = Nvfp4CtS0View::new(
                    window.data(),
                    window.physical_rows(),
                    window.cols(),
                )?;
                self.kernels.pack_nvfp4_ct_s0_fp8(
                    &qweight,
                    &output_scales,
                    view,
                    window.row_offset(),
                    window.rows(),
                    *inv_global_scale,
                    &self.stream,
                )
            }
        };
        cleanup_after_error(launch_result, || {
            let _ = self.stream.synchronize();
        })?;
        Ok(Fp8Weight {
            qweight,
            scales: output_scales,
            rows,
            cols: *cols,
        })
    }

    fn pack_f16_weight(&self, weight: &DevWeight) -> Result<Fp8Weight> {
        let DevWeight::F16 { buf, rows, cols } = weight else {
            return Err(ForgeError::Unsupported(
                "przepakowanie lm_head FP8 wymaga źródła F16".into(),
            ));
        };
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        let launch_result =
            self.kernels
                .pack_f16_fp8(&qweight, &scales, buf, *cols, *rows, &self.stream);
        cleanup_after_error(launch_result, || {
            let _ = self.stream.synchronize();
        })?;
        Ok(Fp8Weight {
            qweight,
            scales,
            rows: *rows,
            cols: *cols,
        })
    }

    fn fp8_pack_allocation_bytes(rows: usize, cols: usize) -> Option<usize> {
        let weight = rows
            .checked_mul(cols)?
            .max(1)
            .checked_next_multiple_of(256)?;
        let scales = rows.checked_mul(4)?.max(1).checked_next_multiple_of(256)?;
        weight.checked_add(scales)
    }

    fn preflight_nvfp4_pack(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Option<usize> {
        let DevWeight::NvFp4 {
            storage,
            rows: source_rows,
            cols,
            ..
        } = weight else { return None };
        let row_end = row_offset.checked_add(rows)?;
        if rows == 0
            || row_end > *source_rows
            || !self.kernels.supports_fp8_modular_shape(rows, *cols)
        {
            return None;
        }
        match storage {
            NvFp4CtStorage::RowMajorE4M3 { .. } if !cols.is_multiple_of(16) => return None,
            NvFp4CtStorage::RowMajorE4M3 { .. } => {}
            NvFp4CtStorage::S0N64K128 { .. } => {
                weight.nvfp4_ct_row_window(row_offset, rows).ok()?;
            }
        }
        Self::fp8_pack_allocation_bytes(rows, *cols)
    }

    fn fp8_build_step<T>(&self, result: Result<T>) -> Result<T> {
        cleanup_after_error(result, || {
            let _ = self.stream.synchronize();
        })
    }

    /// Buduje na GPU opt-in paczki FP8 dla Q/O oraz projekcji FFN checkpointu NVFP4.
    pub fn build_fp8_ffn(&mut self) -> Result<Fp8PackOutcome> {
        if self.weights.fp8_ffn.is_some() {
            return Ok(Fp8PackOutcome::Built);
        }
        if !self.device.caps().fp8_native
            || !self.kernels.supports_fp8_hybrid_packers()
            || !self.kernels.supports_fp8_logits()
        {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        if self.is_hybrid() || self.weights.is_moe() {
            return Ok(Fp8PackOutcome::Unsupported);
        }
        let mut required_bytes = 0usize;
        let mut add_required = |bytes: Option<usize>| -> Option<()> {
            required_bytes = required_bytes.checked_add(bytes?)?;
            Some(())
        };
        let params = &self.weights.descriptor.params;
        let Some(q_rows) = params.n_heads.checked_mul(params.head_dim) else {
            return Ok(Fp8PackOutcome::Unsupported);
        };
        for layer in &self.weights.layers {
            let q_source = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => weight,
                QkvWeights::Split { q, .. } => q,
            };
            if add_required(self.preflight_nvfp4_pack(q_source, 0, q_rows)).is_none()
                || add_required(self.preflight_nvfp4_pack(
                    &layer.attn().attn_o,
                    0,
                    layer.attn().attn_o.rows(),
                ))
                .is_none()
            {
                return Ok(Fp8PackOutcome::Unsupported);
            }
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(Fp8PackOutcome::Unsupported);
            };
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    if weight.rows() % 2 != 0 {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                    let rows = weight.rows() / 2;
                    if add_required(self.preflight_nvfp4_pack(weight, 0, rows)).is_none()
                        || add_required(self.preflight_nvfp4_pack(weight, rows, rows)).is_none()
                    {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                }
                GateUpWeights::Split { gate, up } => {
                    if add_required(self.preflight_nvfp4_pack(gate, 0, gate.rows())).is_none()
                        || add_required(self.preflight_nvfp4_pack(up, 0, up.rows())).is_none()
                    {
                        return Ok(Fp8PackOutcome::Unsupported);
                    }
                }
            }
            if add_required(self.preflight_nvfp4_pack(&ffn.down, 0, ffn.down.rows())).is_none() {
                return Ok(Fp8PackOutcome::Unsupported);
            }
        }
        let fp8_head_supported = match &self.weights.lm_head {
            DevWeight::F16 { rows, cols, .. } => {
                if !cols.is_multiple_of(256) {
                    false
                } else if add_required(Self::fp8_pack_allocation_bytes(*rows, *cols)).is_none() {
                    return Ok(Fp8PackOutcome::Unsupported);
                } else {
                    true
                }
            }
            _ => false,
        };
        let Some(available) = self.device.pool_available(Pool::Weights) else {
            return Ok(Fp8PackOutcome::Unsupported);
        };
        tracing::info!(
            required_bytes,
            available_bytes = available,
            "preflight rezydentnych paczek FP8"
        );
        if required_bytes > available {
            return Ok(Fp8PackOutcome::PoolShortfall {
                required: required_bytes,
                available,
            });
        }

        self.device.synchronize()?;
        let mut layers = Vec::with_capacity(self.weights.layers.len());
        for layer in &self.weights.layers {
            let q = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => {
                    self.fp8_build_step(self.pack_nvfp4_rows(weight, 0, q_rows))?
                }
                QkvWeights::Split { q, .. } => {
                    self.fp8_build_step(self.pack_nvfp4_rows(q, 0, q.rows()))?
                }
            };
            let attn_o = self.fp8_build_step(self.pack_nvfp4_rows(
                &layer.attn().attn_o,
                0,
                layer.attn().attn_o.rows(),
            ))?;
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                unreachable!("modele MoE zostały odrzucone przed przepakowaniem")
            };
            let (gate, up) = match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    let rows = weight.rows() / 2;
                    (
                        self.fp8_build_step(self.pack_nvfp4_rows(weight, 0, rows))?,
                        self.fp8_build_step(self.pack_nvfp4_rows(weight, rows, rows))?,
                    )
                }
                GateUpWeights::Split { gate, up } => (
                    self.fp8_build_step(self.pack_nvfp4_rows(gate, 0, gate.rows()))?,
                    self.fp8_build_step(self.pack_nvfp4_rows(up, 0, up.rows()))?,
                ),
            };
            let down =
                self.fp8_build_step(self.pack_nvfp4_rows(&ffn.down, 0, ffn.down.rows()))?;
            layers.push(crate::weights::Fp8FfnLayer {
                q,
                attn_o,
                gate,
                up,
                down,
            });
        }
        let fp8_lm_head = match (&self.weights.lm_head, fp8_head_supported) {
            (DevWeight::F16 { .. }, true) => Some(
                self.fp8_build_step(self.pack_f16_weight(&self.weights.lm_head))?,
            ),
            _ => None,
        };
        cleanup_after_error(self.stream.synchronize(), || {
            let _ = self.device.synchronize();
        })?;
        tracing::info!(
            resident_pack_bytes = required_bytes,
            layer_count = layers.len(),
            kv_packs = 0,
            available_after_bytes = self.device.pool_available(Pool::Weights),
            "paczki FP8 gotowe do publikacji"
        );
        self.weights.fp8_lm_head = fp8_lm_head;
        self.weights.fp8_ffn = Some(layers);
        self.weights.fp8_modular = crate::weights::fp8_ffn_modular_enabled();
        self.decode_graph = None;
        self.decode_hybrid_graph = None;
        self.decode_moe_graph = None;
        self.decode_rot_graph = None;
        self.batch_graphs.clear();
        Ok(Fp8PackOutcome::Built)
    }

    /// Mean next-token negative log-likelihood over `tokens` (natural log),
    /// i.e. `ln(perplexity)`. Runs the forward pass through whatever prefill
    /// GEMM is active (W4A8 when calibrated, else Q4_K), then maps every
    /// position's final-norm hidden state through the lm_head and scores the
    /// actual next token with a numerically stable log-softmax. Used by the
    /// W4A8 quality gate to compare requant paths on a fixed held-out passage.
    pub fn perplexity(&mut self, tokens: &[u32]) -> Result<(f64, usize)> {
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "perplexity harness supports dense models only".into(),
            ));
        }
        if tokens.len() < 2 {
            return Err(ForgeError::Scheduler("perplexity needs >= 2 tokens".into()));
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut seq = self.new_seq();
        let mut nll_sum = 0.0f64;
        let mut count = 0usize;
        let mut result = Ok(());
        'outer: for (ci, chunk) in tokens.chunks(MAX_PREFILL_CHUNK).enumerate() {
            let base = ci * MAX_PREFILL_CHUNK;
            let t = match self.prefill_forward(&mut seq, chunk, true) {
                Ok(t) => t,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            for i in 0..t {
                let global = base + i;
                if global + 1 >= tokens.len() {
                    break 'outer;
                }
                let next = tokens[global + 1] as usize;
                let stream = &self.stream;
                let pb = self
                    .prefill_bufs
                    .as_ref()
                    .expect("prefill_forward allocated");
                if let Err(e) = self
                    .device
                    .copy(&pb.x, i * hidden * 2, &self.bufs.x, 0, hidden * 2, stream)
                    .and_then(|_| self.logits_gemv(&self.bufs.logits, &self.bufs.x, stream))
                    .and_then(|_| {
                        self.device.copy(
                            &self.bufs.logits,
                            0,
                            &self.bufs.pinned_logits,
                            0,
                            vocab * 4,
                            stream,
                        )
                    })
                    .and_then(|_| self.device.synchronize())
                {
                    result = Err(e);
                    break 'outer;
                }
                let lp =
                    self.bufs
                        .pinned_logits
                        .host_ptr()
                        .expect("pinned buffer has host mapping") as *const f32;
                let logits = unsafe { std::slice::from_raw_parts(lp, vocab) };
                // logsumexp for a stable NLL = logsumexp(logits) - logits[next].
                let maxv = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let mut sum = 0.0f64;
                for &v in logits {
                    sum += ((v - maxv) as f64).exp();
                }
                let logz = (maxv as f64) + sum.ln();
                nll_sum += logz - logits[next] as f64;
                count += 1;
            }
        }
        self.release_seq(&mut seq);
        result?;
        Ok((nll_sum / count.max(1) as f64, count))
    }

    /// Strumień rezydualny prefillu — granica etapu pipeline'u.
    ///
    /// Etap oddaje TO, a nie znormalizowane `x`: następny etap normalizuje po
    /// swojemu swoją warstwą zerową, więc między kartami wędruje wyłącznie
    /// rezydual. Bufor istnieje dopiero po pierwszym prefillu.
    pub fn stage_hidden(&self) -> Result<&DevBuffer> {
        self.prefill_bufs
            .as_ref()
            .map(|pb| &pb.h)
            .ok_or_else(|| ForgeError::Scheduler("bufory prefillu jeszcze nie istnieją".into()))
    }

    /// Przygotowuje bufory prefillu bez liczenia, żeby etap NIE pierwszy miał
    /// gdzie przyjąć stan z poprzedniej karty.
    pub fn ensure_stage_buffers(&mut self) -> Result<()> {
        self.ensure_prefill_bufs()
    }

    /// Przepuszcza chunk przez warstwy TEGO etapu i zatrzymuje się na granicy —
    /// bez głowy logitów.
    ///
    /// Etap zerowy pobiera embedding sam; etap dalszy oczekuje, że wołający
    /// wpisał już rezydual poprzedniej karty do `stage_hidden`. Tokeny podaje
    /// się każdemu etapowi, bo poza embeddingiem wyznaczają pozycje RoPE i
    /// dopisanie do KV. Zwraca liczbę wierszy chunka.
    pub fn prefill_stage(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<usize> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "etap pipeline'u obsługuje na razie wyłącznie model dense".into(),
            ));
        }
        self.prefill_forward(seq, tokens, true)
    }

    /// Logity z rezydualu leżącego na granicy etapu — dopełnienie
    /// `prefill_stage` na etapie OSTATNIM.
    ///
    /// Bierze wiersz `row` (zwykle ostatni token chunka), normalizuje go i
    /// przepuszcza przez głowę tą samą ścieżką co dekodowanie.
    pub fn stage_logits(&mut self, row: usize) -> Result<Vec<f32>> {
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let pb = self
            .prefill_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("bufory prefillu jeszcze nie istnieją".into()))?;
        self.device
            .copy(&pb.x, row * hidden * 2, &self.bufs.x, 0, hidden * 2, &self.stream)?;
        self.trace_f16("result_norm", &self.bufs.x, 0, hidden);
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        self.trace_f32("result_output", &self.bufs.logits, vocab);
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            &self.stream,
        )?;
        self.synchronize_kv_fatal("odczyt logitów etapu pipeline")?;
        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        Ok(unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec())
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through the model in one
    /// batched pass, appending to `seq`, and return the last token's logits.
    /// Not graph-captured: T varies per call and prefill launches are large
    /// enough that launch overhead is immaterial.
    pub fn prefill_chunk(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return self.prefill_hybrid(seq, tokens);
        }
        self.profile_target_start()?;
        let t = self.prefill_forward(seq, tokens, true)?;
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("prefill_forward allocated");
        // Only the last token's logits matter; route its hidden state through
        // the decode logits path (same GEMV + pinned landing).
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &self.bufs.x,
            0,
            hidden * 2,
            &self.stream,
        )?;
        self.trace_f16("result_norm", &self.bufs.x, 0, hidden);
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        self.trace_f32("result_output", &self.bufs.logits, vocab);
        self.profile_target_end()?;
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            &self.stream,
        )?;
        self.synchronize_kv_fatal("odczyt logitów dense prefill")?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
        Ok(logits)
    }

    /// Wykonuje dense prefill i pozostawia logity ostatniego tokenu na urządzeniu.
    /// GPU sampling dołączony do tego samego streamu zapewnia kolejność bez
    /// pełnego odczytu słownika i pośredniej synchronizacji hosta.
    pub fn prefill_chunk_device_logits(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "device-only prefill chunk obsługuje wyłącznie model dense".into(),
            ));
        }
        self.profile_target_start()?;
        let t = self.prefill_forward(seq, tokens, false)?;
        let hidden = self.weights.descriptor.params.hidden_size;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("prefill_forward allocated");
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &self.bufs.x,
            0,
            hidden * 2,
            &self.stream,
        )?;
        self.trace_f16("result_norm", &self.bufs.x, 0, hidden);
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        self.trace_f32(
            "result_output",
            &self.bufs.logits,
            self.weights.descriptor.params.vocab_size,
        );
        self.profile_target_end()
    }

    /// Wykonuje pośredni dense prefill bez głowy logits i opróżnia stream przed
    /// ponownym użyciem współdzielonych buforów przez następny chunk.
    pub fn prefill_chunk_device_sync(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "device-only prefill chunk obsługuje wyłącznie model dense".into(),
            ));
        }
        self.profile_target_start()?;
        self.prefill_forward(seq, tokens, true)?;
        self.profile_target_end()
    }

    /// Sprawdza pełny kontrakt równego dense prefill dla kubełka B4/B8/B16.
    pub fn dense_prefill_batch_capable(&self, batch: usize, n_tokens: usize) -> bool {
        let logits = match &self.weights.lm_head {
            DevWeight::F16 { rows, cols, .. } => DensePrefillLogitsKind::F16 {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q8_0 { rows, cols, .. } => DensePrefillLogitsKind::Q8_0 {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::NvFp4Gguf {
                layout: Nvfp4GgufLayout::RowMajor36,
                rows,
                cols,
                ..
            } => DensePrefillLogitsKind::NvFp4Gguf {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q4K { rows, cols, .. } => DensePrefillLogitsKind::Q4K {
                rows: *rows,
                cols: *cols,
            },
            DevWeight::Q6K { rows, cols, .. } => DensePrefillLogitsKind::Q6K {
                rows: *rows,
                cols: *cols,
            },
            _ => return false,
        };
        let head_dim = self.weights.descriptor.params.head_dim;
        matches!(batch, 4 | 8 | 16)
            && n_tokens > 0
            && batch
                .checked_mul(n_tokens)
                .is_some_and(|total| total <= MAX_PREFILL_CHUNK)
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && self.tier.is_none()
            && self.kv.cfg.dtype() == DType::F16
            && self
                .kernels
                .dense_prefill_batch_capable(head_dim, batch, logits)
    }

    /// Sprawdza wszystkie kubełki wymagane przez wymuszony rollout schedulera.
    pub fn dense_prefill_rollout_capable(&self) -> bool {
        [4usize, 8, 16]
            .into_iter()
            .all(|batch| self.dense_prefill_batch_capable(batch, 1))
    }

    /// Wykonuje równy dense prefill i pozostawia B wierszy logitów na GPU.
    pub fn prefill_batch_device_logits(
        &mut self,
        seqs: &mut [&mut SeqKv],
        token_lanes: &[&[u32]],
    ) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        let batch = seqs.len();
        let n_tokens = token_lanes.first().map_or(0, |tokens| tokens.len());
        if !self.dense_prefill_batch_capable(batch, n_tokens) {
            return Err(ForgeError::Unsupported(
                "batch prefill nie spełnia kontraktu B4/B8/B16".into(),
            ));
        }
        run_dense_prefill_transaction(
            self,
            seqs,
            |model, seqs| {
                model.prefill_forward_lanes(seqs, token_lanes, false, None)?;
                let hidden = model.weights.descriptor.params.hidden_size;
                let row_bytes = hidden * 2;
                let source = model
                    .prefill_bufs
                    .as_ref()
                    .expect("batch prefill ma bufory")
                    .x
                    .clone();
                let destination = model
                    .batch_bufs
                    .as_ref()
                    .expect("batch prefill ma batch scratch")
                    .x
                    .clone();
                for lane in 0..batch {
                    model.device.copy(
                        &source,
                        (lane * n_tokens + n_tokens - 1) * row_bytes,
                        &destination,
                        lane * row_bytes,
                        row_bytes,
                        &model.stream,
                    )?;
                }
                let logits = model
                    .batch_bufs
                    .as_ref()
                    .expect("batch prefill ma batch scratch")
                    .logits
                    .clone();
                model.logits_gemm(&logits, &destination, batch, &model.stream)
            },
            |model| model.synchronize_kv_fatal("rollback dense prefill"),
            |model, seqs, snapshots| {
                restore_prefill_seq_snapshots(&mut model.kv, seqs, snapshots);
                model.pt_seq = 0;
            },
        )
    }

    /// Wykonuje równy pośredni chunk bez głowy logitów i opróżnia stream.
    pub fn prefill_batch_device_sync(
        &mut self,
        seqs: &mut [&mut SeqKv],
        token_lanes: &[&[u32]],
    ) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        let batch = seqs.len();
        let n_tokens = token_lanes.first().map_or(0, |tokens| tokens.len());
        if !self.dense_prefill_batch_capable(batch, n_tokens) {
            return Err(ForgeError::Unsupported(
                "batch prefill nie spełnia kontraktu B4/B8/B16".into(),
            ));
        }
        run_dense_prefill_transaction(
            self,
            seqs,
            |model, seqs| {
                model.prefill_forward_lanes(seqs, token_lanes, true, None)?;
                Ok(())
            },
            |model| model.synchronize_kv_fatal("rollback dense prefill"),
            |model, seqs, snapshots| {
                restore_prefill_seq_snapshots(&mut model.kv, seqs, snapshots);
                model.pt_seq = 0;
            },
        )
    }

    /// Próbkuje B wierszy dense prefill i odczytuje tylko token ID każdego lane.
    pub fn sample_prefill_batch_logits(
        &mut self,
        samplers: &mut [&mut GpuSampler],
    ) -> Result<Vec<u32>> {
        self.ensure_kv_reuse_healthy()?;
        let operation = (|| {
            let batch = samplers.len();
            if !matches!(batch, 4 | 8 | 16) {
                return Err(ForgeError::Scheduler(
                    "sampling prefill wymaga B4/B8/B16".into(),
                ));
            }
            let vocab = self.weights.descriptor.params.vocab_size;
            let params = samplers
                .iter_mut()
                .map(|sampler| sampler.batch_params(vocab))
                .collect::<Vec<_>>();
            let logits = self
                .batch_bufs
                .as_ref()
                .ok_or_else(|| ForgeError::Scheduler("brak logits batch prefill".into()))?
                .logits
                .clone();
            self.batch_sample_from(&logits, batch, &params)?;
            let buffers = self.batch_bufs.as_ref().expect("batch sampler ma bufory");
            let pinned_out = buffers.pinned_out.clone();
            self.device
                .copy(&buffers.out_ids, 0, &pinned_out, 0, batch * 4, &self.stream)?;
            Ok((pinned_out, batch, vocab))
        })();
        let (pinned_out, batch, vocab) =
            settle_kv_operation(operation, "sampling finalnego dense prefill", || {
                self.synchronize_kv_fatal("sampling finalnego dense prefill")
            })?;
        let output = pinned_out.host_ptr().expect("pinned output ma mapowanie") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(output, batch) };
        ids.iter()
            .enumerate()
            .map(|(lane, &id)| {
                if id < 0 || id as usize >= vocab {
                    Err(ForgeError::Kernel(format!(
                        "batch sampler prefill zwrócił token {id} poza słownikiem dla lane {lane}"
                    )))
                } else {
                    Ok(id as u32)
                }
            })
            .collect()
    }

    /// Pooling declared by this model's metadata, falling back to `Mean` for
    /// models that declare none (the neutral choice for a generative model
    /// asked to produce an embedding).
    pub fn embedding_pooling(&self) -> PoolingType {
        match self.weights.descriptor.params.pooling_type {
            PoolingType::None => PoolingType::Mean,
            other => other,
        }
    }

    /// Encode `tokens` into a single sentence embedding. Runs the causal
    /// forward pass over the whole sequence (chunked at MAX_PREFILL_CHUNK,
    /// appending to a private KV sequence so later chunks attend to earlier
    /// ones), pools the final-norm hidden states with `pooling`, and — when
    /// `normalize` — L2-normalizes the result.
    ///
    /// The v0 arches (qwen3/llama/mistral) are decoder transformers, so the
    /// pass is causal: `Last` pooling reads the final token (which has
    /// attended to the whole sequence) and `Cls` the first. A bidirectional
    /// encoder arch would need the non-causal attention path (`attn_full`,
    /// already used by the Whisper encoder) wired in behind an arch flag; no
    /// such arch is in the registry yet, so that path is intentionally absent.
    pub fn embed(
        &mut self,
        tokens: &[u32],
        pooling: PoolingType,
        normalize: bool,
    ) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty embedding input".into()));
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        let mut seq = self.new_seq();
        let out = self.embed_pooled(&mut seq, tokens, pooling, hidden);
        self.release_seq(&mut seq);
        let mut v = out?;
        if normalize {
            l2_normalize(&mut v);
        }
        Ok(v)
    }

    /// Forward + pool over a freshly grown sequence. Split out so `embed` can
    /// always release the sequence even on a mid-chunk error.
    fn embed_pooled(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        pooling: PoolingType,
        hidden: usize,
    ) -> Result<Vec<f32>> {
        let mut sum = vec![0f32; hidden];
        let mut last = vec![0f32; hidden];
        let mut cls: Option<Vec<f32>> = None;
        let mut total: usize = 0;
        let mut scratch = vec![0u8; MAX_PREFILL_CHUNK * hidden * 2];
        for chunk in tokens.chunks(MAX_PREFILL_CHUNK) {
            let t = self.prefill_forward(seq, chunk, true)?;
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("prefill_forward allocated");
            let bytes = &mut scratch[..t * hidden * 2];
            self.device.read(&pb.x, 0, bytes)?;
            let rows: &[f16] = bytemuck::cast_slice(bytes);
            if cls.is_none() {
                cls = Some(rows[..hidden].iter().map(|h| h.to_f32()).collect());
            }
            for ti in 0..t {
                let row = &rows[ti * hidden..(ti + 1) * hidden];
                match pooling {
                    PoolingType::Mean | PoolingType::None => {
                        for (s, h) in sum.iter_mut().zip(row) {
                            *s += h.to_f32();
                        }
                    }
                    PoolingType::Last => {
                        for (dst, h) in last.iter_mut().zip(row) {
                            *dst = h.to_f32();
                        }
                    }
                    PoolingType::Cls => {}
                }
            }
            total += t;
        }
        Ok(match pooling {
            PoolingType::Mean | PoolingType::None => {
                let inv = 1.0 / total as f32;
                sum.iter().map(|s| s * inv).collect()
            }
            PoolingType::Last => last,
            PoolingType::Cls => cls.expect("at least one chunk processed"),
        })
    }

    /// Run one token through the model, appending to `seq`, and return the
    /// f32 logits for the next-token distribution.
    pub fn step(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<Vec<f32>> {
        let vocab = self.weights.descriptor.params.vocab_size;
        self.step_launch(seq, token_id)?;
        // Land logits in pinned memory on the same stream, then one sync.
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();

        Ok(logits)
    }

    /// Enqueue one decode step (graph replay) on the model stream WITHOUT
    /// downloading logits or synchronizing. The next-token logits are left
    /// in the device logits buffer for either the pinned D2H (`step`) or the
    /// on-GPU sampler (`step_and_sample`).
    fn step_launch(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
        self.ensure_kv_reuse_healthy()?;
        self.tick_moe_residency()?;
        let p = self.weights.descriptor.params.clone();
        let pos = seq.len;

        if pos >= p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {pos} exceeds model context {}",
                p.max_position_embeddings
            )));
        }
        if self.is_hybrid() {
            self.activate_hybrid_sequence(seq)?;
        }

        self.tier_ensure_capacity(seq, 1)?;
        if self.tier.is_some() {
            if !seq.spilled.is_empty() {
                if self.tier_can_restore(seq) {
                    // The whole sequence fits again with the watermark reserve
                    // intact: bring it back and take the graphed fast path.
                    self.tier_restore_or_recompute(seq)?;
                } else {
                    return self.step_streamed(seq, token_id);
                }
            }
            seq.tokens.push(token_id);
        }

        let page_boundary = seq.len.is_multiple_of(self.kv.cfg.page_size);
        if page_boundary {
            // A new page is about to be allocated; reclaim a cached prefix page
            // if the free stack is empty so decode growth never starves behind
            // the prefix cache (no-op when the cache is inactive/empty).
            self.ensure_free_pages(1);
        }
        self.kv.grow(seq)?;
        self.upload_decode_inputs(token_id, pos)?;

        // The page table changes when a page is appended — and goes stale when
        // another sequence used the single-stream path, or batched growth /
        // tier restores rewrote this sequence's pages.
        if page_boundary || self.pt_seq != seq.id {
            self.upload_page_table(seq)?;
        }

        // Dekodowanie hybrydowe (uwaga + DeltaNet): rekurencyjny skan po
        // rezydentnym stanie SSM. Wstawienie embeddingu zależy od `token_id` i
        // zostaje poza grafem; reszta kroku czyta pozycję i długość sekwencji z
        // buforów urządzenia, więc jest przechwytywalna i odtwarzana bez
        // uruchamiania ~1200 kerneli po kolei — profil pokazał, że te przerwy
        // między uruchomieniami to 15,8% czasu fazy liczenia.
        if self.is_hybrid() {
            self.ensure_hybrid_bufs()?;
            self.stage_hybrid_embedding(token_id)?;
            if self.decode_hybrid_graph.is_none() {
                let graph = self.capture_hybrid_step()?;
                self.decode_hybrid_graph = Some(graph);
            }
            let graph = self
                .decode_hybrid_graph
                .as_ref()
                .expect("captured above")
                .clone();
            return self.device.launch_graph(&graph, &self.stream);
        }

        // Routed MoE decode: the device-side grouped expert dispatch keeps the
        // router selection on-device (no host readback), so a fully-gidx model
        // records into a replayable graph like the dense path. A model with a
        // fallback expert quant (e.g. Q8_0) still reads back per layer and runs
        // the explicit chain each step.
        if self.weights.is_moe() {
            if self.moe_fully_gidx() {
                if self.decode_moe_graph.is_none() {
                    let graph = self.capture_step_moe()?;
                    self.decode_moe_graph = Some(graph);
                }
                let graph = self
                    .decode_moe_graph
                    .as_ref()
                    .expect("captured above")
                    .clone();
                return self.device.launch_graph(&graph, &self.stream);
            }
            return self.run_step_moe();
        }

        // Rot decode commits the current token into the packed store + ring and
        // reads it back through the split-K attn_decode_rot. The pack kernel
        // takes the token position from `bufs.pos` (device-resident), so the
        // chain is position-independent and captured once like the f16 path.
        if self.kv.cfg.quant.is_rot() {
            if self.decode_rot_graph.is_none() {
                let graph = self.capture_decode_rot()?;
                self.decode_rot_graph = Some(graph);
            }
            let graph = self
                .decode_rot_graph
                .as_ref()
                .expect("captured above")
                .clone();
            return self.device.launch_graph(&graph, &self.stream);
        }

        if self.decode_graph.is_none() {
            let graph = self.capture_step()?;
            self.decode_graph = Some(graph);
        }
        let graph = self.decode_graph.as_ref().expect("captured above").clone();
        self.device.launch_graph(&graph, &self.stream)
    }

    /// Stage [token, pos, seq_len] in pinned memory and push them with async
    /// copies on the compute stream — pinned H2D avoids the pageable
    /// legacy-stream drain that plain write() must perform.
    fn upload_decode_inputs(&self, token_id: u32, pos: usize) -> Result<()> {
        let host = self
            .bufs
            .pinned_in
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            let vals = [token_id as i32, pos as i32, (pos + 1) as i32];
            std::ptr::copy_nonoverlapping(vals.as_ptr() as *const u8, host, 12);
        }
        self.device
            .copy(&self.bufs.pinned_in, 0, &self.bufs.ids, 0, 4, &self.stream)?;
        self.device
            .copy(&self.bufs.pinned_in, 4, &self.bufs.pos, 0, 4, &self.stream)?;
        self.device.copy(
            &self.bufs.pinned_in,
            8,
            &self.seq_len_dev,
            0,
            4,
            &self.stream,
        )?;
        Ok(())
    }

    /// Upload `seq`'s page table (pinned staging + async H2D) and mark it as
    /// the one resident in `page_table_dev`.
    fn upload_page_table(&mut self, seq: &SeqKv) -> Result<()> {
        let pt_host = self
            .bufs
            .pinned_pt
            .host_ptr()
            .expect("pinned buffer has host mapping");
        let mut pt = vec![-1i32; self.max_pages_per_seq];
        pt[..seq.pages.len()].copy_from_slice(&seq.pages);
        unsafe {
            std::ptr::copy_nonoverlapping(
                pt.as_ptr() as *const u8,
                pt_host,
                self.max_pages_per_seq * 4,
            );
        }
        self.device.copy(
            &self.bufs.pinned_pt,
            0,
            &self.page_table_dev,
            0,
            self.max_pages_per_seq * 4,
            &self.stream,
        )?;
        self.pt_seq = seq.id;
        Ok(())
    }

    /// One decode step for a sequence whose spilled KV cannot be restored into
    /// VRAM: the canonical paged slabs keep the resident tail while each
    /// layer's attention runs over the staging slabs holding the FULL context
    /// for that layer (spilled chunks streamed in from RAM/NVMe, resident
    /// pages copied D2D). Never graph-captured; the kernels and their order
    /// match the resident chains exactly, so greedy tokens are bit-identical
    /// to an untiered run.
    fn step_streamed(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
        let pos = seq.len;
        seq.tokens.push(token_id);
        self.kv.grow(seq)?;
        self.upload_decode_inputs(token_id, pos)?;
        self.tier
            .as_mut()
            .expect("streamed path requires tiering")
            .prepare_streaming(seq)?;
        // Hybrid decode: the per-token recurrent forward runs each attention
        // layer over the tier staging slabs (its full context), while the
        // resident DeltaNet state advances untouched. kv_append needs the
        // device page table for the resident tail write.
        if self.is_hybrid() {
            self.ensure_hybrid_bufs()?;
            self.upload_page_table(seq)?;
            return self.hybrid_forward_token(token_id, true, AttnSrc::Staged(seq));
        }
        if self.kv.cfg.quant.is_rot() {
            // kv_pack_rot commits the token into the canonical packed store
            // through the device page table (tail pages are resident), so the
            // per-layer staging picks it up like the separate f16 chain.
            self.upload_page_table(seq)?;
            self.run_step_rot(AttnSrc::Staged(seq))
        } else if self.fused_decode_supported() {
            self.run_step_fused(AttnSrc::Staged(seq))
        } else {
            // The separate chain's qkv_post / kv_append write the new token
            // into the canonical paged slab through the device page table.
            self.upload_page_table(seq)?;
            self.run_step_separate(AttnSrc::Staged(seq))
        }
    }

    /// Capture the rotational decode step into a replayable graph. The recorded
    /// launches read all per-step inputs (token id, position, seq len, page
    /// table) from device buffers refreshed before each replay, so one capture
    /// serves every token.
    fn capture_decode_rot(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_rot(AttnSrc::Paged);
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step for the rotational KV modes. Mirrors the non-fused
    /// decode chain (explicit rmsnorm → qkv → norm/rope) but commits the
    /// appended token into the packed low-bit store + residual ring and reads
    /// it back through the split-K attn_decode_rot / attn_decode_combine_rot
    /// pair (rotate q once, score in rotated space, inverse-rotate the V
    /// accumulator). The pack kernel takes the position from `bufs.pos`, so the
    /// paged variant records cleanly into a CUDA graph. `src` selects the
    /// attention's store: the paged packed regions (captured) or the tier
    /// staging slabs carrying the sequence's full packed context per layer
    /// (streamed path, never captured; the residual ring is a global overlay
    /// and always reads in place).
    fn run_step_rot(&self, src: AttnSrc) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = p.attn_scale_at(0);
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;
        let bits = self.kv.cfg.quant.bits().expect("rot mode has bits");

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        let ring_slots = self
            .kv
            .cfg
            .quant
            .ring_slots()
            .expect("rot mode has ring_slots");
        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            // Gemma 4 zmienia geometrię głowic między warstwami okiennymi a globalnymi.
            let head_dim = p.head_dim_at(l);
            let n_kv_heads = p.n_kv_heads_at(l);
            let layer = &self.weights.layers[l];
            // Produce the rope'd q (attention query) plus the rope'd K/V as
            // LINEAR buffers so the pack kernel rotates them into the packed
            // store + residual ring. No paged f16 append (there is no f16 slab).
            // Returned tuple: (q_buf, q_off, k_src, k_off, v_src, v_off).
            let (q_buf, q_off, k_src, k_off, v_src, v_off): (
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
            ) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemv(&b.qkv, w, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels
                            .rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv,
                            k_byte_off,
                            kn,
                            n_kv_heads,
                            head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemv(&b.qkv, qk, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels
                            .rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv,
                            k_byte_off,
                            kn,
                            n_kv_heads,
                            head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.v, 0)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16(
                            &b.k,
                            &b.k,
                            kn,
                            n_kv_heads,
                            head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16(
                        &b.q,
                        &b.pos,
                        1,
                        p.n_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    kernels.rope_neox_f16(
                        &b.k,
                        &b.pos,
                        1,
                        n_kv_heads,
                        head_dim,
                        p.rope_theta_at(l),
                        self.rope_freqs_at(&p, l),
                        stream,
                    )?;
                    if let Some(vn) = &layer.attn().v_norm {
                        kernels.rmsnorm_f16(&b.v, &b.v, vn, n_kv_heads, head_dim, eps, stream)?;
                    }
                    (&b.q, 0, &b.k, 0, &b.v, 0)
                }
            };

            // Rotate+quant the token into the packed store + residual ring, then
            // attend over the dual region (ring for the recent window, packed
            // for older). q_buf's q head occupies head_dim*n_heads at q_off.
            kernels.kv_pack_rot(
                &self.kv.k_packed[self.target_kv_layer(l)],
                &self.kv.v_packed[self.target_kv_layer(l)],
                &self.kv.k_scale[self.target_kv_layer(l)],
                &self.kv.v_scale[self.target_kv_layer(l)],
                &self.kv.k[self.target_kv_layer(l)],
                &self.kv.v[self.target_kv_layer(l)],
                k_src,
                k_off,
                v_src,
                v_off,
                &self.page_table_dev,
                &self.bufs.pos,
                1,
                n_kv_heads,
                self.kv.cfg.page_size,
                head_dim,
                ring_slots,
                bits,
                stream,
            )?;
            match &src {
                AttnSrc::Paged => {
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &self.kv.k_packed[self.target_kv_layer(l)],
                        &self.kv.v_packed[self.target_kv_layer(l)],
                        &self.kv.k_scale[self.target_kv_layer(l)],
                        &self.kv.v_scale[self.target_kv_layer(l)],
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &self.page_table_dev,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                AttnSrc::Staged(seq) => {
                    // The pack above landed this token in the canonical packed
                    // store's resident tail page; staging materializes the full
                    // packed history (spilled chunks + resident pages) for this
                    // layer behind the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &self.kv.k[self.target_kv_layer(l)],
                        &self.kv.v[self.target_kv_layer(l)],
                        &tb.identity_pt,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        n_kv_heads,
                        head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
            }
            kernels.attn_decode_combine_rot(
                &b.attn_out,
                &b.attn_parts,
                1,
                p.n_heads,
                head_dim,
                ATTN_DECODE_SPLITS,
                stream,
            )?;

            self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
            close_block(
                kernels,
                layer.post_attn_norm.as_ref(),
                None,
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv(&b.gate_up, w, &b.x, stream)?;
                    kernels.glu_mul_f16_at(self.ffn_act(), &b.act, &b.gate_up, 0, inter * 2, inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv(&b.gate, gate, &b.x, stream)?;
                    self.gemv(&b.up, up, &b.x, stream)?;
                    kernels.glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
                }
            }
            self.gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            close_block(
                kernels,
                layer.post_ffw_norm.as_ref(),
                layer.layer_output_scale,
                &b.x,
                &b.h,
                &b.down,
                next_norm,
                1,
                hidden,
                eps,
                stream,
            )?;
        }

        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Whether requests with these sampling params can sample on the GPU:
    /// greedy always fits; a categorical draw needs a bounded top-k and a
    /// vocab within the kernel's merge capacity.
    pub fn gpu_sampling_supported(&self, params: &SamplingParams) -> bool {
        let vocab = self.weights.descriptor.params.vocab_size;
        GpuSampler::compatible(params)
            && (params.clone().sanitized().temperature <= 0.0
                || vocab <= forge_kernels::SAMPLE_MAX_VOCAB)
    }

    /// Run one token through the model and sample its successor on the GPU;
    /// only the 8-byte result crosses PCIe instead of the vocab-sized logits.
    pub fn step_and_sample(
        &mut self,
        seq: &mut SeqKv,
        token_id: u32,
        sampler: &mut GpuSampler,
    ) -> Result<u32> {
        self.step_launch(seq, token_id)?;
        self.sample_last_logits(sampler)
    }

    /// Sample from the logits currently resident in the device logits buffer
    /// (valid right after `step_launch`/`step`/`prefill_chunk` — before any
    /// other sequence runs). Launches ride the model stream, so this also
    /// works back-to-back with an un-synced `step_launch`.
    pub fn sample_last_logits(&mut self, sampler: &mut GpuSampler) -> Result<u32> {
        let p = &self.weights.descriptor.params;
        let b = &self.bufs;
        let sp = sampler.params().clone();

        let penalized = sampler.penalized();
        let penalty_counts = sampler.penalty_counts();
        if sp.has_penalties() && !penalized.is_empty() {
            if penalized.len() != penalty_counts.len()
                || penalized.len() * 4 > b.pinned_penalty.len()
            {
                return Err(ForgeError::Scheduler(format!(
                    "penalty histogram {} exceeds staging capacity",
                    penalized.len()
                )));
            }
            let ids_host = b
                .pinned_penalty
                .host_ptr()
                .expect("pinned buffer has host mapping");
            let counts_host = b
                .pinned_penalty_counts
                .host_ptr()
                .expect("pinned buffer has host mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    penalized.as_ptr() as *const u8,
                    ids_host,
                    penalized.len() * 4,
                );
                std::ptr::copy_nonoverlapping(
                    penalty_counts.as_ptr() as *const u8,
                    counts_host,
                    penalized.len() * 4,
                );
            }
            self.device.copy(
                &b.pinned_penalty,
                0,
                &b.penalty_ids,
                0,
                penalized.len() * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b.pinned_penalty_counts,
                0,
                &b.penalty_counts,
                0,
                penalized.len() * 4,
                &self.stream,
            )?;
            self.kernels.sample_penalize_histogram_f32(
                &b.logits,
                &b.penalty_ids,
                &b.penalty_counts,
                penalized.len(),
                p.vocab_size,
                sp.repetition_penalty,
                sp.frequency_penalty,
                sp.presence_penalty,
                &self.stream,
            )?;
        }

        if sp.temperature <= 0.0 {
            self.kernels.sample_argmax_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                &self.stream,
            )?;
        } else {
            let k = sp.top_k.min(p.vocab_size);
            self.kernels.sample_topk_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                k,
                1.0 / sp.temperature,
                sp.top_p,
                sp.min_p,
                sampler.seed(),
                sampler.next_step(),
                &self.stream,
            )?;
        }

        self.device
            .copy(&b.sample_out, 0, &b.pinned_sample, 0, 8, &self.stream)?;
        self.device.synchronize()?;

        let sp_host = b
            .pinned_sample
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const i32;
        let id = unsafe { *sp_host };
        if id < 0 || id as usize >= p.vocab_size {
            return Err(ForgeError::Kernel(format!(
                "GPU sampler returned out-of-range token {id}"
            )));
        }
        Ok(id as u32)
    }

    /// Zwraca, czy checkpoint zawiera kompletny blok NextN gotowy do smoke MTP.
    pub fn has_native_mtp(&self) -> bool {
        self.weights
            .mtp
            .as_ref()
            .is_some_and(|mtp| mtp.runtime_supported())
            && self
                .hybrid_states
                .as_ref()
                .is_some_and(HybridStatePool::has_mtp)
    }

    pub fn mtp_host_embedding_gathers(&self) -> u64 {
        self.hybrid_states
            .as_ref()
            .map_or(0, HybridStatePool::mtp_host_embedding_gathers)
    }

    pub fn mtp_embedding_mode(&self) -> Option<&'static str> {
        self.weights
            .mtp
            .as_ref()
            .map(|weights| weights.embedding.mode())
    }

    /// Sprawdza niemutujący kontrakt pierwszego pionu native MTP B2.
    pub fn native_mtp_b2_capable(&self, seqs: [&SeqKv; 2], budget: usize) -> bool {
        matches!(budget, 2 | 3)
            && self.mtp_ngram_b2_model_capable()
            && seqs
                .iter()
                .all(|seq| self.native_mtp_available_budget(seq, budget) == budget)
    }

    /// Sprawdza strukturalny kontrakt wspólnego target verifiera N/N B2.
    pub fn mtp_ngram_b2_model_capable(&self) -> bool {
        self.validate_native_mtp_target().is_ok()
            && self.hybrid_batch_weights_capable()
            && matches!(self.kv.cfg.quant, KvQuant::F16)
            && self.tier.is_none()
            && self.prefix_cache.is_none()
            && native_mtp_b2_device_embedding(
                self.mtp_embedding_mode(),
                self.weights
                    .mtp
                    .as_ref()
                    .is_some_and(|mtp| mtp.shares_target_embedding),
            )
    }

    /// Sprawdza kontrakt wspólnego target verifiera dla dwóch pełnych draftów n-gram.
    pub fn mtp_ngram_b2_capable(&self, seqs: [&SeqKv; 2], budget: usize) -> bool {
        self.native_mtp_b2_capable(seqs, budget)
    }

    fn mtp_upload_scalar(
        &self,
        state: &mut MtpDraftState,
        value: i32,
        dst: &DevBuffer,
        dst_offset: usize,
    ) -> Result<()> {
        if state.pinned_scalar_recorded {
            state.pinned_scalar_ready.synchronize()?;
        }
        write_pinned(&value.to_le_bytes(), &state.pinned_scalar)?;
        self.device
            .copy(&state.pinned_scalar, 0, dst, dst_offset, 4, &self.stream)?;
        self.device
            .record_event(&state.pinned_scalar_ready, &self.stream)?;
        state.pinned_scalar_recorded = true;
        Ok(())
    }

    fn mtp_propose_pending(&mut self, seq: &mut SeqKv, fed: u32, k: usize) -> Result<Vec<u32>> {
        if k != 2 && k != 3 {
            return Err(ForgeError::Unsupported(
                "MTP propose_k obsługuje wyłącznie K=2 lub K=3".into(),
            ));
        }
        if !self.has_native_mtp() {
            return Err(ForgeError::Unsupported(
                "checkpoint MTP nie spełnia ograniczeń natywnego runtime".into(),
            ));
        }
        self.ensure_hybrid_bufs()?;
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut checkpoint_attempted = false;
        let mut result: Result<Vec<u32>> = (|| {
            if fed as usize >= self.weights.descriptor.params.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token wejściowy MTP {fed} wykracza poza słownik"
                )));
            }
            checkpoint_attempted = true;
            state.checkpoint(&self.stream)?;
            let initial_ids = [fed as i32, 0, 0, 0, 0];
            write_pinned(bytemuck::cast_slice(&initial_ids), &state.pinned_token_ids)?;
            self.device.copy(
                &state.pinned_token_ids,
                0,
                &state.token_ids,
                0,
                initial_ids.len() * 4,
                &self.stream,
            )?;
            self.device.copy(
                &state.token_ids,
                0,
                &self.bufs.sample_out,
                0,
                4,
                &self.stream,
            )?;

            for step in 0..=k {
                self.mtp_gather_embedding(&mut state, step)?;
                state.stage_step(&mut mtp_kv, &self.kernels, &self.stream)?;
                self.mtp_forward_one(&mut state, &mtp_kv, step < k)?;
                state.save_step_hidden(step, &self.stream)?;
                if step == k {
                    continue;
                }
                self.kernels.sample_argmax_f32(
                    &self.bufs.sample_out,
                    &self.bufs.sample_vals,
                    &self.bufs.sample_idx,
                    &state.logits,
                    self.weights.descriptor.params.vocab_size,
                    &self.stream,
                )?;
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.token_ids,
                    (step + 1) * 4,
                    4,
                    &self.stream,
                )?;
            }
            self.device.copy(
                &state.token_ids,
                0,
                &state.pinned_token_ids,
                0,
                5 * 4,
                &self.stream,
            )?;
            self.device.synchronize()?;
            let host = state
                .pinned_token_ids
                .host_ptr()
                .expect("pinned token IDs mają mapowanie hosta");
            let ids = unsafe { std::slice::from_raw_parts(host as *const i32, k + 1) };
            let gather_status = unsafe { *(host as *const i32).add(4) };
            if gather_status != 0 {
                return Err(ForgeError::Kernel(
                    "MTP GPU gather odrzucił token poza zakresem słownika".into(),
                ));
            }
            Ok(ids[1..].iter().map(|&id| id as u32).collect())
        })();
        if result.is_err() {
            if state.checkpoint_len().is_some() {
                if let Err(rollback) = state.rollback(&mut mtp_kv, &self.stream) {
                    let execution = result.expect_err("wynik propose zawiera błąd");
                    result = Err(self.poison_mtp_runtime(format!(
                        "błąd propose MTP: {execution}; rollback nie powiódł się: {rollback}"
                    )));
                }
            } else if checkpoint_attempted {
                let execution = result.expect_err("wynik propose zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd propose MTP przed utworzeniem checkpointu: {execution}"
                )));
            }
        }
        self.pt_seq = 0;
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_propose_pending_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        k: usize,
        external_drafts: [Option<&[u32]>; 2],
    ) -> Result<()> {
        if !self.native_mtp_b2_capable([&*seqs[0], &*seqs[1]], k) {
            return Err(ForgeError::Unsupported(
                "para nie spełnia kontraktu native MTP B2".into(),
            ));
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let fed_i32 = validate_mtp_routed_inputs(vocab, fed, k, external_drafts)?;
        self.ensure_hybrid_bufs()?;
        let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let mut checkpoint_attempted = false;
        let mut result: Result<()> = (|| {
            let mut required_pages = 0usize;
            for state in &states {
                let end = state.seq.len.checked_add(k + 1).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie długości draftu MTP B2".into())
                })?;
                let end_pages = end.div_ceil(mtp_kv.cfg.page_size);
                if end_pages > mtp_kv.cfg.max_pages_per_seq {
                    return Err(ForgeError::Scheduler(
                        "draft MTP B2 przekracza limit stron sekwencji".into(),
                    ));
                }
                required_pages = required_pages
                    .checked_add(end_pages.saturating_sub(state.seq.pages.len()))
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie rezerwacji KV MTP B2".into())
                    })?;
            }
            if required_pages > mtp_kv.free_page_count() {
                return Err(ForgeError::Scheduler(format!(
                    "draft MTP B2 wymaga {required_pages} stron, dostępne {}",
                    mtp_kv.free_page_count()
                )));
            }

            checkpoint_attempted = true;
            for (lane, state) in states.iter_mut().enumerate() {
                state.checkpoint(&self.stream)?;
                let mut initial_ids = [fed_i32[lane], 0, 0, 0, 0];
                if let Some(draft) = external_drafts[lane] {
                    for (index, &token) in draft.iter().enumerate() {
                        initial_ids[index + 1] = i32::try_from(token).map_err(|_| {
                            ForgeError::Format("draft routed MTP przekracza i32".into())
                        })?;
                    }
                }
                write_pinned(bytemuck::cast_slice(&initial_ids), &state.pinned_token_ids)?;
                self.device.copy(
                    &state.pinned_token_ids,
                    0,
                    &state.token_ids,
                    0,
                    initial_ids.len() * 4,
                    &self.stream,
                )?;
            }

            for step in 0..=k {
                for (lane, state) in states.iter_mut().enumerate() {
                    if external_drafts[lane].is_some() {
                        continue;
                    }
                    self.device.copy(
                        &state.token_ids,
                        step * 4,
                        &self.bufs.sample_out,
                        0,
                        4,
                        &self.stream,
                    )?;
                    self.mtp_gather_embedding(state, step)?;
                    state.stage_step(&mut mtp_kv, &self.kernels, &self.stream)?;
                    self.mtp_forward_one(state, &mtp_kv, step < k)?;
                    state.save_step_hidden(step, &self.stream)?;
                    if step < k {
                        self.kernels.sample_argmax_f32(
                            &self.bufs.sample_out,
                            &self.bufs.sample_vals,
                            &self.bufs.sample_idx,
                            &state.logits,
                            vocab,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &self.bufs.sample_out,
                            0,
                            &state.token_ids,
                            (step + 1) * 4,
                            4,
                            &self.stream,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let checkpoints_complete = states.iter().all(|state| state.checkpoint_len().is_some());
            if let Err(rollback) = rollback_mtp_pair(&mut states, &mut mtp_kv, &self.stream) {
                let execution = result.expect_err("wynik propose B2 zawiera błąd");
                result = Err(self
                    .poison_mtp_runtime(format!("błąd propose MTP B2: {execution}; {rollback}")));
            } else if checkpoint_attempted && !checkpoints_complete {
                let execution = result.expect_err("wynik propose B2 zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd propose MTP B2 przed utworzeniem obu checkpointów: {execution}"
                )));
            }
        }
        self.pt_seq = 0;
        self.finish_mtp_runtime_pair(leases, states, mtp_kv, result)
    }

    /// Buduje dwa drafty K=2/3 w kolejności per krok i odtwarza oba stany MTP.
    pub fn mtp_propose_k_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        k: usize,
    ) -> Result<[Vec<u32>; 2]> {
        self.mtp_propose_pending_b2(seqs, fed, k, [None, None])?;
        let (leases, states, mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let readback = (|| {
            for state in &states {
                self.device.copy(
                    &state.token_ids,
                    0,
                    &state.pinned_token_ids,
                    0,
                    5 * 4,
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            let mut drafts = [Vec::with_capacity(k), Vec::with_capacity(k)];
            for (lane, state) in states.iter().enumerate() {
                let host = state
                    .pinned_token_ids
                    .host_ptr()
                    .expect("pinned token IDs mają mapowanie hosta");
                let ids = unsafe { std::slice::from_raw_parts(host as *const i32, k + 1) };
                if ids
                    .iter()
                    .any(|&id| id < 0 || id as usize >= self.weights.descriptor.params.vocab_size)
                {
                    return Err(ForgeError::Kernel(format!(
                        "MTP GPU gather lane {lane} odrzucił token poza słownikiem"
                    )));
                }
                drafts[lane].extend(ids[1..].iter().map(|&id| id as u32));
            }
            Ok(drafts)
        })();
        let drafts = self.finish_mtp_runtime_pair(leases, states, mtp_kv, readback)?;
        let first = self.rollback_mtp_pending(seqs[0]);
        let second = self.rollback_mtp_pending(seqs[1]);
        match (first, second) {
            (Ok(()), Ok(())) => Ok(drafts),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(first), Err(second)) => Err(ForgeError::Scheduler(format!(
                "rollback obu lane'ów MTP B2 nie powiódł się: lane0={first}; lane1={second}"
            ))),
        }
    }

    /// Buduje liniowy draft K=2/3 poza normalnym server flow i nie zmienia
    /// trwałego stanu KV/hidden bloku MTP.
    pub fn mtp_propose_k(&mut self, seq: &mut SeqKv, fed: u32, k: usize) -> Result<Vec<u32>> {
        let draft = self.mtp_propose_pending(seq, fed, k)?;
        self.rollback_mtp_pending(seq)?;
        Ok(draft)
    }

    fn rollback_mtp_pending(&mut self, seq: &mut SeqKv) -> Result<()> {
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = state
            .rollback(&mut mtp_kv, &self.stream)
            .and_then(|_| self.device.synchronize());
        if let Err(rollback) = &result {
            result =
                Err(self
                    .poison_mtp_runtime(format!("rollback stanu MTP nie powiódł się: {rollback}")));
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn reset_mtp_runtime(&mut self, seq: &mut SeqKv) -> Result<()> {
        if !self.has_native_mtp() {
            return Ok(());
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let result = state.reset(&mut mtp_kv, &self.stream);
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_token(&mut self, seq: &mut SeqKv, token: u32) -> Result<()> {
        if !self.has_native_mtp() {
            return Ok(());
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let result = self.mtp_catchup_token_pending(&mut state, &mut mtp_kv, token);
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_token_pending(
        &mut self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        token: u32,
    ) -> Result<()> {
        self.device.copy(
            &self.bufs.x,
            0,
            &state.catchup_hidden,
            0,
            state.catchup_hidden.len(),
            &self.stream,
        )?;
        let sample_out = self.bufs.sample_out.clone();
        self.mtp_upload_scalar(state, token as i32, &sample_out, 0)?;
        self.mtp_gather_embedding(state, 0)?;
        state.stage_step(mtp_kv, &self.kernels, &self.stream)?;
        self.mtp_forward_one(state, mtp_kv, false)?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            state.recurrent_hidden.len(),
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &self.bufs.x,
            0,
            state.catchup_hidden.len(),
            &self.stream,
        )
    }

    fn mtp_catchup_batch_host(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        t: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("batchowy catch-up MTP wymaga jednej warstwy".into())
        })?;
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrydowego catch-up są gotowe");
        let stream = &self.stream;
        let (base, page_table, seq_len, position) = state.stage_batch(mtp_kv, t)?;
        let positions: Vec<i32> = (base..base + t).map(|value| value as i32).collect();
        let visible: Vec<i32> = (base + 1..=base + t).map(|value| value as i32).collect();
        let host_staging = &hv.host_staging[staging_slot];
        write_pinned(
            bytemuck::cast_slice(&page_table),
            &host_staging.mtp_page_table,
        )?;
        write_pinned(
            bytemuck::cast_slice(&positions),
            &host_staging.mtp_positions,
        )?;
        write_pinned(
            bytemuck::cast_slice(&visible),
            &host_staging.mtp_visible_lens,
        )?;
        write_pinned(&(base as i32).to_le_bytes(), &host_staging.mtp_base_pos)?;
        write_pinned(&seq_len.to_le_bytes(), &host_staging.mtp_seq_len)?;
        write_pinned(&position.to_le_bytes(), &host_staging.mtp_position)?;
        self.device.copy(
            &host_staging.mtp_page_table,
            0,
            &state.page_table,
            0,
            page_table.len() * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.mtp_seq_len, 0, &state.seq_len, 0, 4, stream)?;
        self.device
            .copy(&host_staging.mtp_position, 0, &state.position, 0, 4, stream)?;
        self.device.copy(
            &host_staging.mtp_positions,
            0,
            &pb.positions,
            0,
            t * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.mtp_base_pos, 0, &hv.base_pos, 0, 4, stream)?;
        self.device.copy(
            &host_staging.mtp_visible_lens,
            0,
            &hv.visible_lens,
            0,
            t * 4,
            stream,
        )?;
        self.device
            .copy(&host_staging.embedding, 0, &pb.h, 0, t * hidden * 2, stream)?;
        if let Some(event) = staging_ready {
            self.device.record_event(event, stream)?;
        }
        self.kernels.mtp_norm_join_shifted_f16(
            &hv.q_full,
            &pb.h,
            &pb.x,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            t,
            hidden,
            p.rms_norm_eps,
            stream,
        )?;
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &state.catchup_hidden,
            0,
            hidden * 2,
            stream,
        )?;
        self.kernels
            .mtp_project_joined_q8_f16(&pb.h, &hv.q_full, eh_proj, t, hidden, stream)?;
        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &layer.block.attn_norm,
            t,
            hidden,
            p.rms_norm_eps,
            stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q: _, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&pb.k, k, &pb.x, t, stream)?;
        self.gemm(&pb.v, v, &pb.x, t, stream)?;
        if let Some(norm) = &attention.k_norm {
            self.kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                t * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        let n_rot = self.hybrid_n_rot();
        self.kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.kv_append_batch_device_pos_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &pb.k,
            &pb.v,
            &state.page_table,
            &hv.base_pos,
            t,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            hidden * 2,
            stream,
        )
    }

    /// Dogania stan MTP po zaakceptowanym prefiksie targetu bez liczenia logits.
    fn mtp_catchup_verified_prefix(
        &mut self,
        seq: &mut SeqKv,
        retained: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        if retained == 0 {
            return Err(ForgeError::Scheduler(
                "catch-up MTP wymaga co najmniej tokenu fed".into(),
            ));
        }
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = (|| {
            state.checkpoint(&self.stream)?;
            self.mtp_catchup_verified_prefix_pending(
                &mut state,
                &mut mtp_kv,
                retained,
                staging_slot,
                staging_ready,
            )?;
            state.commit_catchup(retained)?;
            Ok(())
        })();
        if result.is_err() && state.checkpoint_len().is_some() {
            let rollback = state
                .rollback(&mut mtp_kv, &self.stream)
                .and_then(|_| self.device.synchronize());
            if let Err(rollback) = rollback {
                let execution = result.expect_err("wynik catch-up zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd catch-up MTP: {execution}; rollback nie powiódł się: {rollback}"
                )));
            }
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_verified_prefix_pending(
        &mut self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        retained: usize,
        staging_slot: usize,
        staging_ready: Option<&Event>,
    ) -> Result<()> {
        if retained == 0 {
            return Err(ForgeError::Scheduler(
                "catch-up MTP wymaga co najmniej tokenu fed".into(),
            ));
        }
        let hidden_bytes = self.weights.descriptor.params.hidden_size * 2;
        if retained > 1
            && self
                .weights
                .mtp
                .as_ref()
                .is_some_and(|mtp| mtp.shares_target_embedding)
        {
            self.mtp_catchup_batch_host(state, mtp_kv, retained, staging_slot, staging_ready)?;
            return self.device.copy(
                &state.catchup_hidden,
                0,
                &self.bufs.x,
                0,
                hidden_bytes,
                &self.stream,
            );
        }
        for row in 0..retained {
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("bufory prefill są gotowe");
            self.device.copy(
                &pb.x,
                row * hidden_bytes,
                &state.catchup_hidden,
                0,
                hidden_bytes,
                &self.stream,
            )?;
            self.device
                .copy(&pb.ids, row * 4, &self.bufs.sample_out, 0, 4, &self.stream)?;
            self.mtp_gather_embedding(state, row)?;
            state.stage_step(mtp_kv, &self.kernels, &self.stream)?;
            self.mtp_forward_one(state, mtp_kv, false)?;
            self.device.copy(
                &state.catchup_hidden,
                0,
                &state.recurrent_hidden,
                0,
                state.recurrent_hidden.len(),
                &self.stream,
            )?;
        }
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.device.copy(
            &pb.x,
            (retained - 1) * hidden_bytes,
            &self.bufs.x,
            0,
            hidden_bytes,
            &self.stream,
        )
    }

    fn mtp_catchup_layer_major_prefix(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        target_hidden: &DevBuffer,
        fail_after_pending: bool,
        fail_after_commit: bool,
    ) -> Result<()> {
        if !self.has_native_mtp() {
            self.profile_catchup_end()?;
            return self.device.synchronize();
        }
        let hidden_bytes = self.weights.descriptor.params.hidden_size * 2;
        let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
        let mut result = (|| {
            state.checkpoint(&self.stream)?;
            let batch_capable = self.weights.mtp.as_ref().is_some_and(|mtp| {
                mtp.shares_target_embedding
                    && mtp.layers.first().is_some_and(|layer| {
                        matches!(layer.eh_proj, DevWeight::Q8_0 { .. })
                            && matches!(layer.block.attn().attn_qkv, QkvWeights::Split { .. })
                    })
            });
            if batch_capable {
                self.mtp_catchup_layer_major_batch_pending(
                    &mut state,
                    &mut mtp_kv,
                    tokens,
                    target_hidden,
                )?;
            } else {
                for (row, &token) in tokens.iter().enumerate() {
                    self.device.copy(
                        target_hidden,
                        row * hidden_bytes,
                        &self.bufs.x,
                        0,
                        hidden_bytes,
                        &self.stream,
                    )?;
                    self.mtp_catchup_token_pending(&mut state, &mut mtp_kv, token)?;
                }
            }
            if fail_after_pending {
                return Err(ForgeError::Scheduler(
                    "wymuszony błąd layer-major catch-up MTP".into(),
                ));
            }
            state.validate_commit_catchup(tokens.len())?;
            self.profile_catchup_end()?;
            self.device.synchronize()?;
            if fail_after_commit {
                return Err(ForgeError::Scheduler(
                    "wymuszony błąd layer-major po commit MTP".into(),
                ));
            }
            state.apply_commit_catchup();
            Ok(())
        })();
        if result.is_err() && state.checkpoint_len().is_some() {
            let rollback = state
                .rollback(&mut mtp_kv, &self.stream)
                .and_then(|_| self.device.synchronize());
            if let Err(rollback) = rollback {
                let execution = result.expect_err("wynik catch-up zawiera błąd");
                result = Err(self.poison_mtp_runtime(format!(
                    "błąd layer-major catch-up MTP: {execution}; rollback nie powiódł się: {rollback}"
                )));
            }
        }
        self.finish_mtp_runtime(lease, state, mtp_kv, result)
    }

    fn mtp_catchup_layer_major_batch_pending(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &mut KvCache,
        tokens: &[u32],
        target_hidden: &DevBuffer,
    ) -> Result<()> {
        let arena = self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major jest gotowa");
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("batchowy catch-up MTP wymaga jednej warstwy".into())
        })?;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let QkvWeights::Split { q: _, k, v } = &layer.block.attn().attn_qkv else {
            return Err(ForgeError::Unsupported(
                "batchowy catch-up MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        let p = &self.weights.descriptor.params;
        let hidden_bytes = p.hidden_size * 2;
        let t = tokens.len();
        let table = self
            .weights
            .token_embd_host
            .as_ref()
            .expect("współdzielony embedding targetu jest dostępny");
        let (base, page_table, seq_len, position) = state.stage_batch(mtp_kv, t)?;
        let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
        for (chunk_index, chunk) in tokens.chunks(128).enumerate() {
            let offset = chunk_index * 128;
            let slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
            let host = &arena.host_staging[slot];
            host.ready.synchronize()?;
            let positions: Vec<i32> = (base + offset..base + offset + chunk.len())
                .map(|value| value as i32)
                .collect();
            write_pinned(bytemuck::cast_slice(&positions), &host.positions)?;
            let destination = host
                .embedding
                .host_ptr()
                .expect("pinned embedding ma mapowanie hosta");
            for (row, &token) in chunk.iter().enumerate() {
                let source =
                    &table[token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source.as_ptr() as *const u8,
                        destination.add(row * hidden_bytes),
                        hidden_bytes,
                    );
                }
            }
            if chunk_index == 0 {
                write_pinned(bytemuck::cast_slice(&page_table), &host.page_table)?;
                write_pinned(&(base as i32).to_le_bytes(), &host.base_pos)?;
                write_pinned(&seq_len.to_le_bytes(), &host.seq_len)?;
                write_pinned(&position.to_le_bytes(), &host.position)?;
                self.device.copy(
                    &host.page_table,
                    0,
                    &state.page_table,
                    0,
                    page_table.len() * 4,
                    &self.stream,
                )?;
                self.device
                    .copy(&host.seq_len, 0, &state.seq_len, 0, 4, &self.stream)?;
                self.device
                    .copy(&host.position, 0, &state.position, 0, 4, &self.stream)?;
                self.device
                    .copy(&host.base_pos, 0, &arena.base_pos, 0, 4, &self.stream)?;
            }
            self.device.copy(
                &host.embedding,
                0,
                &arena.h,
                offset * hidden_bytes,
                chunk.len() * hidden_bytes,
                &self.stream,
            )?;
            self.device.copy(
                &host.positions,
                0,
                &arena.positions,
                offset * 4,
                chunk.len() * 4,
                &self.stream,
            )?;
            self.device.record_event(&host.ready, &self.stream)?;
            staging_recorded[slot] = true;
        }
        debug_assert!(staging_recorded.into_iter().any(|recorded| recorded));
        self.kernels.mtp_norm_join_shifted_f16(
            &arena.q_full,
            &arena.h,
            target_hidden,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.device.copy(
            target_hidden,
            (t - 1) * hidden_bytes,
            &state.catchup_hidden,
            0,
            hidden_bytes,
            &self.stream,
        )?;
        self.kernels.mtp_project_joined_q8_f16(
            &arena.h,
            &arena.q_full,
            eh_proj,
            t,
            p.hidden_size,
            &self.stream,
        )?;
        self.kernels.rmsnorm_f16(
            &arena.x,
            &arena.h,
            &layer.block.attn_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.gemm(&arena.k, k, &arena.x, t, &self.stream)?;
        self.gemm(&arena.v, v, &arena.x, t, &self.stream)?;
        let attention = layer.block.attn();
        if let Some(norm) = &attention.k_norm {
            self.kernels.rmsnorm_f16(
                &arena.k,
                &arena.k,
                norm,
                t * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                &self.stream,
            )?;
        }
        self.kernels.rope_neox_partial_f16(
            &arena.k,
            &arena.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            self.hybrid_n_rot(),
            p.rope_theta,
            &self.stream,
        )?;
        self.kernels.kv_append_batch_device_pos_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &arena.k,
            &arena.v,
            &state.page_table,
            &arena.base_pos,
            t,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &state.recurrent_hidden,
            0,
            hidden_bytes,
            &self.stream,
        )?;
        self.device.copy(
            &state.catchup_hidden,
            0,
            &self.bufs.x,
            0,
            hidden_bytes,
            &self.stream,
        )
    }

    fn mtp_catchup_verified_prefix_b2(
        &self,
        states: &mut [MtpDraftState; 2],
        mtp_kv: &mut KvCache,
        t: usize,
        external_sources: [bool; 2],
    ) -> Result<()> {
        let mtp = self.weights.mtp.as_ref().expect("stan MTP ma wagi");
        let layer = mtp.layers.first().ok_or_else(|| {
            ForgeError::Unsupported("segmentowany catch-up MTP wymaga jednej warstwy".into())
        })?;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "segmentowany catch-up MTP wymaga eh_proj Q8_0".into(),
            ));
        };
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let total = 2usize
            .checked_mul(t)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie catch-up MTP B2".into()))?;
        let mut bases = [0i32; 2];
        let mut page_tables = vec![-1i32; 2 * self.max_pages_per_seq];
        for (lane, state) in states.iter_mut().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &state.recurrent_hidden,
                0,
                &self
                    .mtp_b2_bufs
                    .as_ref()
                    .expect("MTP B2 gotowy")
                    .mtp_initial_hidden,
                lane * hidden * 2,
                hidden * 2,
                &self.stream,
            )?;
            let (base, table, _, _) = state.stage_batch(mtp_kv, t)?;
            bases[lane] = i32::try_from(base).map_err(|_| {
                ForgeError::Scheduler("pozycja bazowa catch-up MTP przekracza i32".into())
            })?;
            let offset = lane * self.max_pages_per_seq;
            page_tables[offset..offset + table.len()].copy_from_slice(&table);
        }
        let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
        let mut metadata = Vec::with_capacity(2 + page_tables.len());
        metadata.extend_from_slice(&bases);
        metadata.extend_from_slice(&page_tables);
        write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_mtp_metadata)?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            0,
            &b2.base_positions,
            0,
            8,
            &self.stream,
        )?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            8,
            &b2.page_tables,
            0,
            page_tables.len() * 4,
            &self.stream,
        )?;
        for (lane, state) in states.iter().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &b2.page_tables,
                lane * self.max_pages_per_seq * 4,
                &state.page_table,
                0,
                self.max_pages_per_seq * 4,
                &self.stream,
            )?;
        }

        let pb = self.prefill_bufs.as_ref().expect("prefill gotowy");
        self.kernels.mtp_pack_verify_inputs(
            &pb.ids,
            &pb.positions,
            &b2.visible_lens,
            &states[0].token_ids,
            &states[1].token_ids,
            &b2.base_positions,
            t,
            &self.stream,
        )?;
        let masked_bases = [
            if external_sources[0] { bases[0] } else { -1 },
            if external_sources[1] { bases[1] } else { -1 },
        ];
        write_pinned(bytemuck::cast_slice(&masked_bases), &b2.pinned_mtp_metadata)?;
        self.device.copy(
            &b2.pinned_mtp_metadata,
            0,
            &b2.base_positions,
            0,
            8,
            &self.stream,
        )?;
        self.kernels.mtp_norm_join_shifted_segmented_f16(
            &b2.q_full,
            &b2.catchup_embeddings,
            &pb.x,
            &b2.mtp_initial_hidden,
            &layer.enorm,
            &layer.hnorm,
            2,
            t,
            hidden,
            p.rms_norm_eps,
            &self.stream,
        )?;
        self.kernels.mtp_project_joined_q8_f16(
            &pb.h,
            &b2.q_full,
            eh_proj,
            total,
            hidden,
            &self.stream,
        )?;
        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &layer.block.attn_norm,
            total,
            hidden,
            p.rms_norm_eps,
            &self.stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q: _, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&pb.k, k, &pb.x, total, &self.stream)?;
        self.gemm(&pb.v, v, &pb.x, total, &self.stream)?;
        if let Some(norm) = &attention.k_norm {
            self.kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                total * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                &self.stream,
            )?;
        }
        self.kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            total,
            p.n_kv_heads,
            p.head_dim,
            self.hybrid_n_rot(),
            p.rope_theta,
            &self.stream,
        )?;
        self.kernels.kv_append_batch_segmented_masked_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &pb.k,
            &pb.v,
            &b2.page_tables,
            &b2.base_positions,
            &b2.decisions,
            2,
            t,
            self.max_pages_per_seq,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            p.head_dim,
            &self.stream,
        )?;
        self.kernels.mtp_commit_catchup_metadata_segmented(
            &b2.mtp_seq_lens,
            &b2.mtp_positions,
            &b2.base_positions,
            &b2.decisions,
            2,
            &self.stream,
        )?;
        for (lane, state) in states.iter().enumerate() {
            if !external_sources[lane] {
                continue;
            }
            self.device.copy(
                &b2.mtp_seq_lens,
                lane * 4,
                &state.seq_len,
                0,
                4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.mtp_positions,
                lane * 4,
                &state.position,
                0,
                4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.selected_hidden,
                lane * hidden * 2,
                &state.recurrent_hidden,
                0,
                hidden * 2,
                &self.stream,
            )?;
        }
        Ok(())
    }

    fn mtp_gather_embedding(&self, state: &mut MtpDraftState, token_index: usize) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        let p = &self.weights.descriptor.params;
        match &mtp.embedding {
            MtpEmbedding::Device(DevWeight::F16 { buf, .. }) => self.kernels.gather_f16_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::Q8_0 { buf, .. }) => self.kernels.gather_q8_0_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                layout: Nvfp4GgufLayout::RowMajor36,
                ..
            }) => self.kernels.gather_nvfp4_gguf_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                *output_scale,
                &self.stream,
            ),
            MtpEmbedding::Device(_) => Err(ForgeError::Unsupported(
                "GPU MTP wymaga embeddingu F16, Q8_0 lub GGUF NVFP4".into(),
            )),
            MtpEmbedding::HostF16 => {
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.pinned_token_ids,
                    token_index * 4,
                    4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let ids = state
                    .pinned_token_ids
                    .host_ptr()
                    .expect("pinned token IDs mają mapowanie hosta")
                    as *const i32;
                let token_id = unsafe { *ids.add(token_index) };
                if token_id < 0 || token_id as usize >= p.vocab_size {
                    return Err(ForgeError::Kernel(format!(
                        "MTP argmax zwrócił token poza zakresem: {token_id}"
                    )));
                }
                let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                    ForgeError::Unsupported("brak hostowego embeddingu dla MTP low-memory".into())
                })?;
                let base = token_id as usize * p.hidden_size;
                let row = table.get(base..base + p.hidden_size).ok_or_else(|| {
                    ForgeError::Format("wiersz embeddingu MTP wykracza poza tabelę".into())
                })?;
                let staging = self
                    .hybrid_bufs
                    .as_ref()
                    .expect("bufory hybrid zaalokowane")
                    .pinned_embed
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        row.as_ptr() as *const u8,
                        staging,
                        p.hidden_size * 2,
                    );
                }
                self.device.copy(
                    &self
                        .hybrid_bufs
                        .as_ref()
                        .expect("bufory hybrid zaalokowane")
                        .pinned_embed,
                    0,
                    &mtp.token_embedding,
                    0,
                    p.hidden_size * 2,
                    &self.stream,
                )?;
                state.record_host_embedding_gather();
                Ok(())
            }
        }
    }

    fn mtp_forward_one(
        &self,
        state: &mut MtpDraftState,
        mtp_kv: &KvCache,
        want_logits: bool,
    ) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        if mtp.layers.len() != 1 {
            return Err(ForgeError::Unsupported(format!(
                "runtime MTP obsługuje jeden blok, otrzymano {}",
                mtp.layers.len()
            )));
        }
        let layer = &mtp.layers[0];
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let head_dim = p.head_dim;
        let q_dim = p.n_heads * head_dim;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let b = &self.bufs;
        let hb = self
            .hybrid_bufs
            .as_ref()
            .expect("bufory hybrid zaalokowane");
        let stream = &self.stream;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "mtp_prepare wymaga eh_proj w Q8_0".into(),
            ));
        };
        self.kernels.mtp_prepare_f16(
            &state.prepared_hidden,
            &mtp.token_embedding,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            eh_proj,
            hidden,
            eps,
            stream,
        )?;
        self.kernels.rmsnorm_f16(
            &b.x,
            &state.prepared_hidden,
            &layer.block.attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        let qkv_grouped =
            self.gemv_nvfp4_gguf_group(&[(&hb.q_full, q), (&b.k, k), (&b.v, v)], &b.x, stream)?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, q, &b.x, stream)?;
            self.gemv(&b.k, k, &b.x, stream)?;
            self.gemv(&b.v, v, &b.x, stream)?;
        }
        self.kernels
            .deinterleave_gate_f16(&hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream)?;
        if let Some(norm) = &attention.q_norm {
            self.kernels
                .rmsnorm_f16(&hb.qc, &hb.qc, norm, p.n_heads, head_dim, eps, stream)?;
        }
        if let Some(norm) = &attention.k_norm {
            self.kernels
                .rmsnorm_f16(&b.k, &b.k, norm, p.n_kv_heads, head_dim, eps, stream)?;
        }
        let n_rot = self.hybrid_n_rot();
        self.kernels.rope_neox_partial_f16(
            &hb.qc,
            &state.position,
            1,
            p.n_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.rope_neox_partial_f16(
            &b.k,
            &state.position,
            1,
            p.n_kv_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.kv_append_f16(
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &b.k,
            &b.v,
            &state.page_table,
            &state.seq_len,
            p.n_kv_heads,
            mtp_kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        self.kernels.attn_decode_f16(
            &b.attn_out,
            &b.attn_parts,
            &hb.qc,
            &mtp_kv.k[0],
            &mtp_kv.v[0],
            &state.page_table,
            &state.seq_len,
            1,
            p.n_heads,
            p.n_kv_heads,
            head_dim,
            mtp_kv.cfg.page_size,
            mtp_kv.cfg.max_pages_per_seq,
            1.0 / (head_dim as f32).sqrt(),
            // Głowa MTP pracuje na pełnym kontekście swojej sekwencji.
            0,
            stream,
        )?;
        self.kernels
            .sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.gemv(&b.o_out, &attention.attn_o, &hb.gated, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.o_out,
            &layer.block.ffn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let ffn = layer.block.dense_ffn()?;
        let GateUpWeights::Split { gate, up } = &ffn.gate_up else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych gate/up".into(),
            ));
        };
        self.gemv(&b.gate, gate, &b.x, stream)?;
        self.gemv(&b.up, up, &b.x, stream)?;
        self.kernels
            .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
        self.gemv(&b.down, &ffn.down, &b.act, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.down,
            &layer.shared_head_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.device.copy(
            &b.x,
            0,
            &state.recurrent_hidden,
            0,
            state.recurrent_hidden.len(),
            stream,
        )?;
        if want_logits {
            let output = mtp.draft_output.as_ref().unwrap_or(&mtp.output);
            self.logits_weight_gemv(&state.logits, 0, &b.x, 0, output, stream)?;
        }
        Ok(())
    }

    /// Sprawdza wspólne ograniczenia verifiera spekulacyjnego dla targetu.
    pub fn validate_speculation_target(&self, draft_tokens: usize) -> Result<()> {
        if self.is_hybrid() {
            if !matches!(draft_tokens, 2 | 3) {
                return Err(ForgeError::Unsupported(
                    "hybrydowy verifier spekulacyjny wymaga budżetu 2 lub 3".into(),
                ));
            }
            return self.validate_hybrid_speculation_target();
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support routed MoE models".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 KV cache".into(),
            ));
        }
        if self.tier.is_some() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support KV tiering".into(),
            ));
        }
        if self.prefix_cache.is_some() {
            return Err(ForgeError::Unsupported(
                "speculative verification requires the prefix cache to be disabled".into(),
            ));
        }
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. } | DevWeight::Q8_0 { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 or Q8_0 language-model head".into(),
            ));
        }
        Ok(())
    }

    /// Sprawdza target hybrydowy niezależnie od źródła tokenów draftu.
    pub fn validate_hybrid_speculation_target(&self) -> Result<()> {
        if !self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga hybrydowego targetu".into(),
            ));
        }
        if self.weights.descriptor.params.head_dim != 256 {
            return Err(ForgeError::Unsupported(format!(
                "hybrydowy verifier spekulacyjny wymaga head_dim=256, otrzymano {}",
                self.weights.descriptor.params.head_dim
            )));
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny nie obsługuje jeszcze targetu MoE".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga cache KV F16".into(),
            ));
        }
        if self.tier.is_some() || self.prefix_cache.is_some() {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny wymaga wyłączonego tieringu i prefix cache".into(),
            ));
        }
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. } | DevWeight::Q8_0 { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "batchowy head hybrydowego targetu wymaga F16 lub Q8_0".into(),
            ));
        }
        Ok(())
    }

    /// Zapewnia scratch logitów weryfikatora dla `cap` pozycji.
    fn ensure_verify_bufs(&mut self, cap: usize) -> Result<()> {
        if self.verify_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        self.verify_bufs = Some(VerifyBufs {
            cap,
            logits: self
                .device
                .alloc(cap * vocab * 4, MemKind::Device, Pool::Activations)?,
            ids: self
                .device
                .alloc(cap * 4, MemKind::Device, Pool::Activations)?,
            pinned_ids: self
                .device
                .alloc(cap * 4, MemKind::PinnedHost, Pool::Activations)?,
        });
        Ok(())
    }

    fn ensure_hybrid_verify_bufs(&mut self, cap: usize) -> Result<()> {
        if self
            .hybrid_verify_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= cap)
        {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p
            .ssm
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("target MTP nie jest hybrydowy".into()))?;
        let q_dim = p.n_heads * p.head_dim;
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let n_v = ssm.n_v_heads();
        let conv_elems = conv_dim
            .checked_mul(ssm.d_conv - 1)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie okna conv verifiera MTP".into()))?;
        let state_elems = n_v
            .checked_mul(ssm.d_state)
            .and_then(|value| value.checked_mul(ssm.d_state))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie stanu verifiera MTP".into()))?;
        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let base_pos = a32("mtp base position", &[1])?;
        let visible_lens = a32("mtp visible lengths", &[cap])?;
        let attn_parts = device.alloc(
            hybrid_verify_attention_parts_bytes(cap, p.n_heads, p.head_dim)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let q_full = a16(
            "mtp q full",
            &[cap, hybrid_q_full_cols(q_dim, conv_dim, p.hidden_size)],
        )?;
        let qc = a16("mtp q", &[cap, q_dim.max(value_dim)])?;
        let gatec = a16("mtp q gate", &[cap, q_dim.max(value_dim)])?;
        let gated = a16("mtp gated attention", &[cap, q_dim.max(value_dim)])?;
        let qkv_mixed = if cap > 4 {
            q_full.clone()
        } else {
            a16("mtp mixed qkv", &[cap, conv_dim])?
        };
        let z = a16("mtp z", &[cap, value_dim])?;
        let q32 = if cap > 4 {
            qc.clone()
        } else {
            a16("mtp q32", &[cap, value_dim])?
        };
        let k32 = if cap > 4 {
            gatec.clone()
        } else {
            a16("mtp k32", &[cap, value_dim])?
        };
        let vtok = if cap > 4 {
            gated.clone()
        } else {
            a16("mtp v", &[cap, value_dim])?
        };
        let alpha = a16("mtp alpha", &[cap, n_v])?;
        let beta_raw = a16("mtp beta raw", &[cap, n_v])?;
        let g = a32("mtp g", &[cap, n_v])?;
        let beta_f = a32("mtp beta", &[cap, n_v])?;
        let o = a16("mtp recurrence output", &[cap, value_dim])?;
        let normed = if cap > 4 {
            o.clone()
        } else {
            a16("mtp recurrence norm", &[cap, value_dim])?
        };
        let state_checkpoints = if cap > 4 {
            a32("mtp state checkpoints", &[1])?
        } else {
            a32("mtp state checkpoints", &[cap, state_elems])?
        };
        let accepted = a32("mtp accepted", &[2])?;
        let pinned_decision = alloc_checked(
            device.as_ref(),
            "mtp pinned decision",
            &[2],
            4,
            MemKind::PinnedHost,
        )?;
        let pinned = |name: &str, dims: &[usize], element_bytes: usize| {
            alloc_checked(
                device.as_ref(),
                name,
                dims,
                element_bytes,
                MemKind::PinnedHost,
            )
        };
        let host_staging = (0..HYBRID_HOST_STAGING_SLOTS)
            .map(|_| {
                Ok(HybridHostStaging {
                    embedding: pinned("mtp pinned embedding", &[cap, p.hidden_size], 2)?,
                    page_table: pinned("mtp pinned page table", &[self.max_pages_per_seq], 4)?,
                    ids: pinned("mtp pinned ids", &[cap], 4)?,
                    positions: pinned("mtp pinned positions", &[cap], 4)?,
                    visible_lens: pinned("mtp pinned visible lengths", &[cap], 4)?,
                    base_pos: pinned("mtp pinned base position", &[1], 4)?,
                    accepted: pinned("mtp pinned accepted", &[2], 4)?,
                    mtp_page_table: pinned(
                        "mtp pinned catch-up page table",
                        &[self.max_pages_per_seq],
                        4,
                    )?,
                    mtp_positions: pinned("mtp pinned catch-up positions", &[cap], 4)?,
                    mtp_visible_lens: pinned("mtp pinned catch-up visible lengths", &[cap], 4)?,
                    mtp_base_pos: pinned("mtp pinned catch-up base position", &[1], 4)?,
                    mtp_seq_len: pinned("mtp pinned catch-up sequence length", &[1], 4)?,
                    mtp_position: pinned("mtp pinned catch-up position", &[1], 4)?,
                    ready: device.create_event()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let has_delta = self
            .weights
            .layers
            .iter()
            .any(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)));
        let shared_delta_base = if cap > 4 && has_delta {
            Some((
                a16("mtp współdzielony conv initial", &[conv_elems])?,
                a16("mtp współdzielone conv checkpoints", &[cap, conv_elems])?,
                a32("mtp współdzielony state initial", &[1])?,
            ))
        } else {
            None
        };
        let delta_base = self
            .weights
            .layers
            .iter()
            .map(|layer| match &layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::DeltaNet(_) => {
                    let buffers = if let Some((conv_initial, conv_checkpoints, state_initial)) =
                        shared_delta_base.as_ref()
                    {
                        (
                            conv_initial.clone(),
                            conv_checkpoints.clone(),
                            state_initial.clone(),
                        )
                    } else {
                        (
                            a16("mtp conv initial", &[conv_elems])?,
                            a16("mtp conv checkpoints", &[cap, conv_elems])?,
                            a32("mtp state initial", &[state_elems])?,
                        )
                    };
                    Ok(Some(buffers))
                }
                LayerMixer::Attention(_) => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        let checkpoint_stride = cap
            .checked_mul(state_elems)
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie offsetu checkpointów MTP".into())
            })?;
        let retained_state_checkpoints = if cap <= 4 {
            Some(a32(
                "mtp retained state checkpoints",
                &[
                    self.weights
                        .layers
                        .iter()
                        .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
                        .count(),
                    cap,
                    state_elems,
                ],
            )?)
        } else {
            None
        };
        let retain_checkpoints = retained_state_checkpoints.is_some();
        let mut delta_index = 0usize;
        let delta = delta_base
            .into_iter()
            .map(|base| match base {
                Some((conv_initial, conv_checkpoints, state_initial)) => {
                    let checkpoint_byte_offset =
                        delta_index.checked_mul(checkpoint_stride).ok_or_else(|| {
                            ForgeError::Scheduler(
                                "przepełnienie offsetu warstwy DeltaNet MTP".into(),
                            )
                        })?;
                    delta_index += 1;
                    let commit = if cap > 4 {
                        DeltaVerifyCommit::InPlacePrefill
                    } else if retain_checkpoints {
                        DeltaVerifyCommit::Retained {
                            checkpoint_byte_offset,
                        }
                    } else {
                        DeltaVerifyCommit::Recompute {
                            q: a16("mtp delta q", &[cap, value_dim])?,
                            k: a16("mtp delta k", &[cap, value_dim])?,
                            v: a16("mtp delta v", &[cap, value_dim])?,
                            g: a32("mtp delta g", &[cap, n_v])?,
                            beta: a32("mtp delta beta", &[cap, n_v])?,
                        }
                    };
                    Ok(Some(DeltaVerifyCache {
                        commit,
                        conv_initial,
                        conv_checkpoints,
                        state_initial,
                    }))
                }
                None => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_verify_graphs.clear();
        self.hybrid_verify_graph_disabled.clear();
        self.hybrid_verify_bufs = Some(HybridVerifyBufs {
            cap,
            base_pos,
            visible_lens,
            attn_parts,
            q_full,
            qc,
            gatec,
            gated,
            qkv_mixed,
            z,
            q32,
            k32,
            vtok,
            alpha,
            beta_raw,
            g,
            beta_f,
            o,
            normed,
            state_checkpoints,
            retained_state_checkpoints,
            accepted,
            pinned_decision,
            host_staging,
            delta,
        });
        Ok(())
    }

    fn ensure_mtp_b2_bufs(&mut self) -> Result<()> {
        if self.mtp_b2_bufs.is_some() {
            return Ok(());
        }
        const BATCH: usize = 2;
        const STEPS: usize = 4;
        let checked_mul = |name: &str, left: usize, right: usize| {
            left.checked_mul(right).ok_or_else(|| {
                ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {left} * {right}"))
            })
        };
        let checked_add = |name: &str, left: usize, right: usize| {
            left.checked_add(right).ok_or_else(|| {
                ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {left} + {right}"))
            })
        };
        let total = checked_mul("mtp b2 total", BATCH, STEPS)?;
        let p = &self.weights.descriptor.params;
        let ssm = p
            .ssm
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("target MTP B2 nie jest hybrydowy".into()))?;
        if ssm.d_state != 128 {
            return Err(ForgeError::Unsupported(format!(
                "target MTP B2 wymaga d_state=128, otrzymano {}",
                ssm.d_state
            )));
        }
        let q_dim = checked_mul("mtp b2 q", p.n_heads, p.head_dim)?;
        let key_dim = checked_mul("mtp b2 key", ssm.d_state, ssm.n_group)?;
        let n_v = ssm.n_v_heads();
        let value_dim = checked_mul("mtp b2 value", ssm.d_state, n_v)?;
        let doubled_key = checked_mul("mtp b2 doubled key", key_dim, 2)?;
        let conv_dim = checked_add("mtp b2 conv", doubled_key, value_dim)?;
        let conv_history = ssm
            .d_conv
            .checked_sub(1)
            .ok_or_else(|| ForgeError::Scheduler("MTP B2 wymaga d_conv > 0".into()))?;
        let conv_elems = checked_mul("mtp b2 conv history", conv_dim, conv_history)?;
        let state_head = checked_mul("mtp b2 state head", ssm.d_state, ssm.d_state)?;
        let state_elems = checked_mul("mtp b2 state", n_v, state_head)?;
        let doubled_q = checked_mul("mtp b2 doubled q", q_dim, 2)?;
        let doubled_hidden = checked_mul("mtp b2 doubled hidden", p.hidden_size, 2)?;
        let q_full_cols = doubled_q.max(conv_dim).max(doubled_hidden);
        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let delta = self
            .weights
            .layers
            .iter()
            .map(|layer| match layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(_) => Ok(None),
                LayerMixer::DeltaNet(_) => Ok(Some(MtpB2DeltaCache {
                    conv_initial: a16("mtp b2 conv initial", &[BATCH, conv_elems])?,
                    conv_checkpoints: a16("mtp b2 conv checkpoints", &[BATCH, STEPS, conv_elems])?,
                    state_initial: a32("mtp b2 state initial", &[BATCH, state_elems])?,
                    q: a16("mtp b2 delta q", &[total, value_dim])?,
                    k: a16("mtp b2 delta k", &[total, value_dim])?,
                    v: a16("mtp b2 delta v", &[total, value_dim])?,
                    g: a32("mtp b2 delta g", &[total, n_v])?,
                    beta: a32("mtp b2 delta beta", &[total, n_v])?,
                })),
            })
            .collect::<Result<Vec<_>>>()?;
        self.mtp_b2_bufs = Some(MtpB2Bufs {
            q_full: a16("mtp b2 q full", &[total, q_full_cols])?,
            qc: a16("mtp b2 q", &[total, q_dim.max(value_dim)])?,
            gatec: a16("mtp b2 q gate", &[total, q_dim.max(value_dim)])?,
            gated: a16("mtp b2 gated", &[total, q_dim.max(value_dim)])?,
            qkv_mixed: a16("mtp b2 qkv mixed", &[total, conv_dim])?,
            z: a16("mtp b2 z", &[total, value_dim])?,
            alpha: a16("mtp b2 alpha", &[total, n_v])?,
            beta_raw: a16("mtp b2 beta raw", &[total, n_v])?,
            o: a16("mtp b2 recurrence output", &[total, value_dim])?,
            normed: a16("mtp b2 recurrence norm", &[total, value_dim])?,
            page_tables: a32("mtp b2 page tables", &[BATCH, self.max_pages_per_seq])?,
            base_positions: a32("mtp b2 base positions", &[BATCH])?,
            visible_lens: a32("mtp b2 visible lengths", &[total])?,
            decisions: a32("mtp b2 decisions", &[BATCH, 2])?,
            pinned_decisions: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned decisions",
                &[BATCH * 2 + BATCH * 5],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_metadata: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned metadata",
                &[BATCH + BATCH * self.max_pages_per_seq],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_mtp_metadata: alloc_checked(
                device.as_ref(),
                "mtp b2 pinned catch-up metadata",
                &[BATCH + BATCH * self.max_pages_per_seq],
                4,
                MemKind::PinnedHost,
            )?,
            catchup_embeddings: a16("mtp b2 catch-up embeddings", &[total, p.hidden_size])?,
            mtp_initial_hidden: a16("mtp b2 catch-up initial hidden", &[BATCH, p.hidden_size])?,
            mtp_seq_lens: a32("mtp b2 catch-up sequence lengths", &[BATCH])?,
            mtp_positions: a32("mtp b2 catch-up positions", &[BATCH])?,
            selected_states: a32("mtp b2 selected states", &[BATCH, state_elems])?,
            selected_conv: a16("mtp b2 selected conv", &[BATCH, conv_elems])?,
            selected_hidden: a16("mtp b2 selected hidden", &[BATCH, p.hidden_size])?,
            delta,
        });
        Ok(())
    }

    fn ensure_hybrid_prefill_b2_bufs(&mut self) -> Result<()> {
        if self.hybrid_prefill_b2_bufs.is_some() {
            return Ok(());
        }
        const BATCH: usize = 2;
        const STEPS: usize = 32;
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("prefill B2 T32 wymaga targetu hybrydowego".into())
        })?;
        let elements = |name: &str, dimensions: &[usize]| {
            dimensions.iter().try_fold(1usize, |total, &dimension| {
                total.checked_mul(dimension).ok_or_else(|| {
                    ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {dimensions:?}"))
                })
            })
        };
        let bytes = |name: &str, dimensions: &[usize], element_bytes: usize| {
            elements(name, dimensions)?
                .checked_mul(element_bytes)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie bufora {name}")))
        };
        let total = elements("prefill B2 total", &[BATCH, STEPS])?;
        let q_dim = elements("prefill B2 q", &[p.n_heads, p.head_dim])?;
        let kv_dim = elements("prefill B2 kv", &[p.n_kv_heads, p.head_dim])?;
        let key_dim = elements("prefill B2 key", &[ssm.d_state, ssm.n_group])?;
        let n_v = ssm.n_v_heads();
        let value_dim = elements("prefill B2 value", &[ssm.d_state, n_v])?;
        let conv_dim = key_dim
            .checked_mul(2)
            .and_then(|value| value.checked_add(value_dim))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie conv prefill B2".into()))?;
        let conv_history = ssm
            .d_conv
            .checked_sub(1)
            .ok_or_else(|| ForgeError::Scheduler("prefill B2 wymaga d_conv > 0".into()))?;
        let conv_elems = elements("prefill B2 conv state", &[conv_dim, conv_history])?;
        let state_elems = elements("prefill B2 state", &[n_v, ssm.d_state, ssm.d_state])?;
        let q_full_cols = hybrid_q_full_cols(q_dim, conv_dim, p.hidden_size);
        let page_table_elems =
            elements("prefill B2 page tables", &[BATCH, self.max_pages_per_seq])?;
        let metadata_elems = elements("prefill B2 metadata rows", &[3, total])?
            .checked_add(BATCH)
            .and_then(|value| value.checked_add(page_table_elems))
            .and_then(|value| value.checked_add(BATCH * 2))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie metadata prefill B2".into()))?;
        let mut required = 0usize;
        let mut reserve = |name: &str, dimensions: &[usize], element_bytes: usize| -> Result<()> {
            let allocation = bytes(name, dimensions, element_bytes)?
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie alokacji {name}")))?;
            required = required.checked_add(allocation).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie preflightu prefill B2".into())
            })?;
            Ok(())
        };
        for (name, rows, cols, element_bytes) in [
            ("h", total, p.hidden_size, 2),
            ("x", total, p.hidden_size, 2),
            ("k", total, kv_dim, 2),
            ("v", total, kv_dim, 2),
            ("attn", total, q_dim, 2),
            ("o_out", total, p.hidden_size, 2),
            ("gate", total, p.intermediate_size, 2),
            ("up", total, p.intermediate_size, 2),
            ("down", total, p.hidden_size, 2),
            ("q_full", total, q_full_cols, 2),
            ("qc", total, q_dim.max(value_dim), 2),
            ("gatec", total, q_dim.max(value_dim), 2),
            ("gated", total, q_dim.max(value_dim), 2),
            ("qkv_mixed", total, conv_dim, 2),
            ("z", total, value_dim, 2),
            ("alpha", total, n_v, 2),
            ("beta_raw", total, n_v, 2),
            ("recurrence", total, value_dim, 2),
            ("normed", total, value_dim, 2),
        ] {
            reserve(name, &[rows, cols], element_bytes)?;
        }
        reserve("ids", &[total], 4)?;
        reserve("positions", &[total], 4)?;
        reserve("page tables", &[page_table_elems], 4)?;
        reserve("base positions", &[BATCH], 4)?;
        reserve("visible lengths", &[total], 4)?;
        reserve("decisions", &[BATCH, 2], 4)?;
        reserve("final hidden", &[BATCH, p.hidden_size], 2)?;
        reserve("logits", &[BATCH, p.vocab_size], 4)?;
        reserve("pinned metadata", &[metadata_elems], 4)?;
        reserve("pinned logits", &[BATCH, p.vocab_size], 4)?;
        reserve("final conv", &[BATCH, conv_elems], 2)?;
        reserve("final states", &[BATCH, state_elems], 4)?;
        let delta_layers = self
            .weights
            .layers
            .iter()
            .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
            .count();
        let mut per_delta = 0usize;
        for (name, dimensions, element_bytes) in [
            ("conv initial", vec![BATCH, conv_elems], 2),
            ("state initial", vec![BATCH, state_elems], 4),
            ("delta q", vec![total, value_dim], 2),
            ("delta k", vec![total, value_dim], 2),
            ("delta v", vec![total, value_dim], 2),
            ("delta g", vec![total, n_v], 4),
            ("delta beta", vec![total, n_v], 4),
        ] {
            let allocation = bytes(name, &dimensions, element_bytes)?
                .max(1)
                .checked_next_multiple_of(DEVICE_ALLOC_ALIGN)
                .ok_or_else(|| ForgeError::Scheduler(format!("przepełnienie alokacji {name}")))?;
            per_delta = per_delta.checked_add(allocation).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie scratchu warstwy Delta prefill B2".into())
            })?;
        }
        required = per_delta
            .checked_mul(delta_layers)
            .and_then(|delta| required.checked_add(delta))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie preflightu prefill B2".into()))?;
        if self
            .device
            .pool_available(Pool::Activations)
            .is_some_and(|available| required > available)
        {
            return Err(ForgeError::OutOfMemory {
                requested: required,
                available: self.device.pool_available(Pool::Activations).unwrap_or(0),
            });
        }

        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let gate = a16("prefill B2 gate", &[total, p.intermediate_size])?;
        let delta = self
            .weights
            .layers
            .iter()
            .map(|layer| match layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(_) => Ok(None),
                LayerMixer::DeltaNet(_) => Ok(Some(HybridPrefillB2DeltaCache {
                    conv_initial: a16("prefill B2 conv initial", &[BATCH, conv_elems])?,
                    state_initial: a32("prefill B2 state initial", &[BATCH, state_elems])?,
                    q: a16("prefill B2 delta q", &[total, value_dim])?,
                    k: a16("prefill B2 delta k", &[total, value_dim])?,
                    v: a16("prefill B2 delta v", &[total, value_dim])?,
                    g: a32("prefill B2 delta g", &[total, n_v])?,
                    beta: a32("prefill B2 delta beta", &[total, n_v])?,
                })),
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_prefill_b2_bufs = Some(HybridPrefillB2Bufs {
            h: a16("prefill B2 h", &[total, p.hidden_size])?,
            x: a16("prefill B2 x", &[total, p.hidden_size])?,
            k: a16("prefill B2 k", &[total, kv_dim])?,
            v: a16("prefill B2 v", &[total, kv_dim])?,
            attn_out: a16("prefill B2 attention", &[total, q_dim])?,
            o_out: a16("prefill B2 mixer output", &[total, p.hidden_size])?,
            gate: gate.clone(),
            up: a16("prefill B2 up", &[total, p.intermediate_size])?,
            act: gate,
            down: a16("prefill B2 down", &[total, p.hidden_size])?,
            ids: a32("prefill B2 ids", &[total])?,
            positions: a32("prefill B2 positions", &[total])?,
            q_full: a16("prefill B2 q full", &[total, q_full_cols])?,
            qc: a16("prefill B2 qc", &[total, q_dim.max(value_dim)])?,
            gatec: a16("prefill B2 gatec", &[total, q_dim.max(value_dim)])?,
            gated: a16("prefill B2 gated", &[total, q_dim.max(value_dim)])?,
            qkv_mixed: a16("prefill B2 qkv mixed", &[total, conv_dim])?,
            z: a16("prefill B2 z", &[total, value_dim])?,
            alpha: a16("prefill B2 alpha", &[total, n_v])?,
            beta_raw: a16("prefill B2 beta raw", &[total, n_v])?,
            o: a16("prefill B2 recurrence", &[total, value_dim])?,
            normed: a16("prefill B2 recurrence norm", &[total, value_dim])?,
            page_tables: a32("prefill B2 page tables", &[page_table_elems])?,
            base_positions: a32("prefill B2 base positions", &[BATCH])?,
            visible_lens: a32("prefill B2 visible lengths", &[total])?,
            decisions: a32("prefill B2 decisions", &[BATCH, 2])?,
            final_hidden: a16("prefill B2 final hidden", &[BATCH, p.hidden_size])?,
            logits: a32("prefill B2 logits", &[BATCH, p.vocab_size])?,
            pinned_metadata: alloc_checked(
                device.as_ref(),
                "prefill B2 pinned metadata",
                &[metadata_elems],
                4,
                MemKind::PinnedHost,
            )?,
            pinned_logits: alloc_checked(
                device.as_ref(),
                "prefill B2 pinned logits",
                &[BATCH, p.vocab_size],
                4,
                MemKind::PinnedHost,
            )?,
            final_conv: a16("prefill B2 final conv", &[BATCH, conv_elems])?,
            final_states: a32("prefill B2 final states", &[BATCH, state_elems])?,
            delta,
        });
        Ok(())
    }

    pub fn validate_native_mtp_target(&self) -> Result<()> {
        self.validate_hybrid_speculation_target()?;
        if !self.has_native_mtp() {
            return Err(ForgeError::Unsupported(
                "checkpoint nie ma obsługiwanego natywnego proposera MTP".into(),
            ));
        }
        Ok(())
    }

    /// Zwraca dostępny budget 0/2/3 po sprawdzeniu targetu, kontekstu i stron
    /// dla `fed` oraz draftu. Żądane K=3 może zostać przycięte do K=2.
    pub fn native_mtp_available_budget(&self, seq: &SeqKv, requested: usize) -> usize {
        if self.validate_native_mtp_target().is_err() || requested < 2 {
            return 0;
        }
        for budget in (2..=requested.min(3)).rev() {
            let Some(end) = seq
                .len
                .checked_add(1)
                .and_then(|length| length.checked_add(budget))
            else {
                continue;
            };
            if end > self.weights.descriptor.params.max_position_embeddings {
                continue;
            }
            let required_pages = end
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len());
            if required_pages <= self.available_pages() {
                return budget;
            }
        }
        0
    }

    fn hybrid_verify_delta_layer(
        &self,
        layer_index: usize,
        delta: &DeltaNetWeights,
        t: usize,
        inplace_prefill: bool,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let d_state = ssm.d_state;
        let conv_elems = conv_dim * (ssm.d_conv - 1);
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let cache = hv.delta[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma cache verifiera");
        let state = self.active_ssm()[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");

        self.gemm(&hv.qkv_mixed, &delta.in_proj, &pb.x, t, stream)?;
        let mut prepared = Self::delta_input_q8_cols(delta)
            .filter(|_| matches!(t, 6 | 8 | 32 | 128))
            .map(|cols| self.kernels.prepare_q8_1(&pb.x, cols, t, stream))
            .transpose()?;
        let fused_q8_triplet = prepared.is_some() && matches!(t, 32 | 128);
        if let Some(prepared) = prepared.as_mut() {
            if fused_q8_triplet {
                self.gemm_q8_prepared_triplet(
                    [&hv.z, &hv.alpha, &hv.beta_raw],
                    [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj],
                    prepared,
                    t,
                )?;
            } else if !inplace_prefill {
                self.gemm_q8_prepared(&hv.z, &delta.gate_proj, prepared, t)?;
            }
            if !fused_q8_triplet {
                self.gemm_q8_prepared(&hv.alpha, &delta.alpha_proj, prepared, t)?;
                self.gemm_q8_prepared(&hv.beta_raw, &delta.beta_proj, prepared, t)?;
            }
        } else {
            if !inplace_prefill {
                self.gemm(&hv.z, &delta.gate_proj, &pb.x, t, stream)?;
            }
            self.gemm(&hv.alpha, &delta.alpha_proj, &pb.x, t, stream)?;
            self.gemm(&hv.beta_raw, &delta.beta_proj, &pb.x, t, stream)?;
        }

        self.device.copy(
            &state.conv,
            0,
            &cache.conv_initial,
            0,
            conv_elems * 2,
            stream,
        )?;
        kernels.deltanet_prepare_f16(
            &hv.q32,
            &hv.k32,
            &hv.vtok,
            &hv.g,
            &hv.beta_f,
            &cache.conv_checkpoints,
            &cache.conv_initial,
            &hv.qkv_mixed,
            &delta.conv1d,
            &hv.alpha,
            &hv.beta_raw,
            &delta.dt_bias,
            &delta.a,
            t,
            n_k,
            n_v,
            d_state,
            ssm.d_conv,
            p.rms_norm_eps,
            stream,
        )?;
        if inplace_prefill && !fused_q8_triplet {
            if let Some(prepared) = prepared.as_mut() {
                self.gemm_q8_prepared(&hv.z, &delta.gate_proj, prepared, t)?;
            } else {
                self.gemm(&hv.z, &delta.gate_proj, &pb.x, t, stream)?;
            }
        }
        drop(prepared);
        if inplace_prefill {
            match self.delta_state_layout() {
                DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_inplace_f16(
                    &hv.o,
                    &state.state,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    1,
                    t,
                    n_v,
                    stream,
                )?,
                DeltaStateLayout::KeyValue => kernels.deltanet_gated_scan_inplace_f16(
                    &hv.o,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    t,
                    n_v,
                    d_state,
                    stream,
                )?,
            }
        } else {
            let (state_checkpoints, checkpoint_byte_offset) = match &cache.commit {
                DeltaVerifyCommit::InPlacePrefill => {
                    return Err(ForgeError::Scheduler(
                        "scratch prefill wymaga skanu DeltaNet in-place".into(),
                    ));
                }
                DeltaVerifyCommit::Retained {
                    checkpoint_byte_offset,
                } => (
                    hv.retained_state_checkpoints
                        .as_ref()
                        .expect("retained checkpointy DeltaNet są zaalokowane"),
                    *checkpoint_byte_offset,
                ),
                DeltaVerifyCommit::Recompute { .. } => (&hv.state_checkpoints, 0),
            };
            match self.delta_state_layout() {
                DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_checkpoints_f16_at(
                    &hv.o,
                    state_checkpoints,
                    checkpoint_byte_offset,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    1,
                    t,
                    n_v,
                    stream,
                )?,
                DeltaStateLayout::KeyValue => kernels.deltanet_gated_scan_f16_at(
                    &hv.o,
                    state_checkpoints,
                    checkpoint_byte_offset,
                    &state.state,
                    &hv.q32,
                    &hv.k32,
                    &hv.vtok,
                    &hv.g,
                    &hv.beta_f,
                    t,
                    n_v,
                    d_state,
                    stream,
                )?,
            }
            if let DeltaVerifyCommit::Recompute { q, k, v, g, beta } = &cache.commit {
                self.device
                    .copy(&hv.q32, 0, q, 0, t * value_dim * 2, stream)?;
                self.device
                    .copy(&hv.k32, 0, k, 0, t * value_dim * 2, stream)?;
                self.device
                    .copy(&hv.vtok, 0, v, 0, t * value_dim * 2, stream)?;
                self.device.copy(&hv.g, 0, g, 0, t * n_v * 4, stream)?;
                self.device
                    .copy(&hv.beta_f, 0, beta, 0, t * n_v * 4, stream)?;
            }
        }
        kernels.deltanet_gated_rmsnorm_f16(
            &hv.normed,
            &hv.o,
            &hv.z,
            &delta.ssm_norm,
            t * n_v,
            d_state,
            p.rms_norm_eps,
            stream,
        )?;
        self.gemm(&pb.o_out, &delta.out_proj, &hv.normed, t, stream)
    }

    fn hybrid_verify_attention_layer(
        &self,
        layer_index: usize,
        attention: &AttnWeights,
        t: usize,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&hv.q_full, q, &pb.x, t, stream)?;
        kernels.deinterleave_gate_f16(
            &hv.qc,
            &hv.gatec,
            &hv.q_full,
            p.head_dim,
            t * q_dim,
            stream,
        )?;
        if let Some(norm) = &attention.q_norm {
            kernels.rmsnorm_f16(
                &hv.qc,
                &hv.qc,
                norm,
                t * p.n_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        self.gemm(&pb.k, k, &pb.x, t, stream)?;
        self.gemm(&pb.v, v, &pb.x, t, stream)?;
        if let Some(norm) = &attention.k_norm {
            kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                t * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        let n_rot = self.hybrid_n_rot();
        kernels.rope_neox_partial_f16(
            &hv.qc,
            &pb.positions,
            t,
            p.n_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.kv_append_batch_device_pos_f16(
            &self.kv.k[self.target_kv_layer(layer_index)],
            &self.kv.v[self.target_kv_layer(layer_index)],
            &pb.k,
            &pb.v,
            &self.page_table_dev,
            &hv.base_pos,
            t,
            p.n_kv_heads,
            self.kv.cfg.page_size,
            p.head_dim,
            stream,
        )?;
        if self.kernels.attn_verify_split8_f16_hd256(
            &pb.attn_out,
            &hv.attn_parts,
            &hv.qc,
            &self.kv.k[self.target_kv_layer(layer_index)],
            &self.kv.v[self.target_kv_layer(layer_index)],
            &self.page_table_dev,
            &hv.visible_lens,
            t,
            p.n_heads,
            p.n_kv_heads,
            self.kv.cfg.page_size,
            self.max_pages_per_seq,
            1.0 / (p.head_dim as f32).sqrt(),
            stream,
        )? {
        } else if self.device.caps().vendor == Vendor::Nvidia {
            kernels.attn_decode_batch_exact_f16_hd256(
                &pb.attn_out,
                &hv.qc,
                &self.kv.k[self.target_kv_layer(layer_index)],
                &self.kv.v[self.target_kv_layer(layer_index)],
                &self.page_table_dev,
                &hv.visible_lens,
                t,
                p.n_heads,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                1.0 / (p.head_dim as f32).sqrt(),
                stream,
            )?;
        } else {
            kernels.attn_prefill_device_pos_f16_hd256(
                &pb.attn_out,
                &hv.qc,
                &self.kv.k[self.target_kv_layer(layer_index)],
                &self.kv.v[self.target_kv_layer(layer_index)],
                &self.page_table_dev,
                &hv.base_pos,
                t,
                p.n_heads,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                1.0 / (p.head_dim as f32).sqrt(),
                stream,
            )?;
        }
        kernels.sigmoid_mul_f16(&hv.gated, &pb.attn_out, &hv.gatec, t * q_dim, stream)?;
        debug_assert_eq!(attention.attn_o.cols(), q_dim);
        debug_assert!(pb.k.len() >= t * kv_dim * 2);
        self.gemm(&pb.o_out, &attention.attn_o, &hv.gated, t, stream)
    }

    /// Zatwierdza na GPU stan odpowiadający zaakceptowanemu prefiksowi.
    fn run_hybrid_verify_postlude(&self, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        let vb = self.verify_bufs.as_ref().expect("bufory verify są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        self.kernels
            .mtp_verify_decide(&hv.accepted, &vb.ids, &pb.ids, t, &self.stream)?;
        self.kernels.mtp_select_row_f16(
            &self.bufs.h,
            &pb.h,
            &hv.accepted,
            p.hidden_size,
            &self.stream,
        )?;
        self.kernels.mtp_select_row_f16(
            &self.bufs.x,
            &pb.x,
            &hv.accepted,
            p.hidden_size,
            &self.stream,
        )?;
        self.kernels.mtp_select_row_f32(
            &self.bufs.logits,
            &vb.logits,
            &hv.accepted,
            p.vocab_size,
            &self.stream,
        )?;

        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let conv_elems = ssm.conv_dim() * (ssm.d_conv - 1);
        for (layer_index, cache) in hv.delta.iter().enumerate() {
            let Some(cache) = cache else { continue };
            let state = self.active_ssm()[layer_index]
                .as_ref()
                .expect("warstwa DeltaNet ma stan");
            match &cache.commit {
                DeltaVerifyCommit::InPlacePrefill => {
                    return Err(ForgeError::Scheduler(
                        "scratch prefill nie może zatwierdzać verifiera MTP".into(),
                    ));
                }
                DeltaVerifyCommit::Retained {
                    checkpoint_byte_offset,
                } => self.kernels.deltanet_commit_checkpoint_f32_at(
                    &state.state,
                    hv.retained_state_checkpoints
                        .as_ref()
                        .expect("retained checkpointy DeltaNet są zaalokowane"),
                    *checkpoint_byte_offset,
                    &hv.accepted,
                    t,
                    ssm.n_v_heads(),
                    ssm.d_state,
                    &self.stream,
                )?,
                DeltaVerifyCommit::Recompute { q, k, v, g, beta } => {
                    match self.delta_state_layout() {
                        DeltaStateLayout::ValueKey => {
                            self.kernels.deltanet_value_key_commit_recompute_f32(
                                &state.state,
                                &state.state,
                                k,
                                v,
                                g,
                                beta,
                                &hv.accepted,
                                1,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?
                        }
                        DeltaStateLayout::KeyValue => {
                            self.kernels.deltanet_gated_scan_f16(
                                &hv.o,
                                &hv.state_checkpoints,
                                &state.state,
                                q,
                                k,
                                v,
                                g,
                                beta,
                                t,
                                ssm.n_v_heads(),
                                ssm.d_state,
                                &self.stream,
                            )?;
                            self.kernels.deltanet_commit_checkpoint_f32(
                                &state.state,
                                &hv.state_checkpoints,
                                &hv.accepted,
                                t,
                                ssm.n_v_heads(),
                                ssm.d_state,
                                &self.stream,
                            )?;
                        }
                    }
                }
            }
            self.kernels.mtp_select_row_f16(
                &state.conv,
                &cache.conv_checkpoints,
                &hv.accepted,
                conv_elems,
                &self.stream,
            )?;
        }
        self.device
            .copy(&hv.accepted, 0, &hv.pinned_decision, 0, 8, &self.stream)
    }

    fn commit_hybrid_prefill_delta_layer(&self, layer_index: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrydowego prefill są gotowe");
        let cache = hv.delta[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma cache skanu");
        let state = self.active_ssm()[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        self.kernels.mtp_select_row_f16(
            &state.conv,
            &cache.conv_checkpoints,
            &hv.accepted,
            ssm.conv_dim() * (ssm.d_conv - 1),
            &self.stream,
        )
    }

    /// Uruchamia wspólny batched forward hybrydowego targetu.
    fn run_hybrid_batch_layers(&self, t: usize, commit_prefill: bool) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.kernels.rmsnorm_f16(
            &pb.x,
            &pb.h,
            &self.weights.layers[0].attn_norm,
            t,
            p.hidden_size,
            p.rms_norm_eps,
            &self.stream,
        )?;

        for (layer_index, layer) in self.weights.layers.iter().enumerate() {
            match &layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(attention) => {
                    self.hybrid_verify_attention_layer(layer_index, attention, t)?
                }
                LayerMixer::DeltaNet(delta) => {
                    self.hybrid_verify_delta_layer(layer_index, delta, t, commit_prefill)?;
                    if commit_prefill {
                        self.commit_hybrid_prefill_delta_layer(layer_index)?;
                    }
                }
            }
            self.kernels.rmsnorm_residual_f16(
                &pb.x,
                &pb.h,
                &pb.o_out,
                &layer.ffn_norm,
                t,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Err(ForgeError::Unsupported(
                    "hybrydowy verifier MTP nie obsługuje jeszcze targetu MoE".into(),
                ));
            };
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    self.gemm_rows(
                        &pb.gate,
                        weight,
                        &pb.x,
                        t,
                        0,
                        p.intermediate_size,
                        &self.stream,
                    )?;
                    self.gemm_rows(
                        &pb.up,
                        weight,
                        &pb.x,
                        t,
                        p.intermediate_size,
                        p.intermediate_size,
                        &self.stream,
                    )?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&pb.gate, gate, &pb.x, t, &self.stream)?;
                    self.gemm(&pb.up, up, &pb.x, t, &self.stream)?;
                }
            }
            self.kernels.glu_mul_f16(self.ffn_act(), 
                &pb.act,
                &pb.gate,
                &pb.up,
                t * p.intermediate_size,
                &self.stream,
            )?;
            self.gemm(&pb.down, &ffn.down, &pb.act, t, &self.stream)?;
            let next_norm = if layer_index + 1 < self.weights.layers.len() {
                &self.weights.layers[layer_index + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            self.kernels.rmsnorm_residual_f16(
                &pb.x,
                &pb.h,
                &pb.down,
                next_norm,
                t,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
        }

        Ok(())
    }

    /// Uruchamia stałą część verifiera hybrydowego bez synchronizacji z hostem.
    fn run_hybrid_verify_compute(&self, t: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe");
        self.run_hybrid_batch_layers(t, false)?;

        let vb = self.verify_bufs.as_ref().expect("bufory verify są gotowe");
        self.logits_gemm(&vb.logits, &pb.x, t, &self.stream)?;
        self.kernels.sample_batched_argmax_f32(
            &vb.ids,
            &vb.logits,
            t,
            p.vocab_size,
            &self.stream,
        )?;
        self.run_hybrid_verify_postlude(t)
    }

    /// Przechwytuje rozgrzany łańcuch verifiera dla stałego T.
    fn capture_hybrid_verify_compute(&self, t: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.run_hybrid_verify_compute(t) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(error) => {
                let _ = self.device.end_capture(&self.stream);
                Err(error)
            }
        }
    }

    /// Po pierwszym wykonaniu eager zapisuje graf właściwy dla slotu i T.
    fn capture_hybrid_verify_graph_if_needed(&mut self, slot: usize, t: usize) {
        if std::env::var("FORGE_HYBRID_VERIFY_GRAPH").is_ok_and(|value| value == "0") {
            return;
        }
        if !self.device.caps().supports_graph_capture {
            return;
        }
        if !matches!(t, 3 | 4) {
            return;
        }
        let key = (slot, t);
        if self.hybrid_verify_graphs.contains_key(&key)
            || self.hybrid_verify_graph_disabled.contains(&key)
        {
            return;
        }
        match self.capture_hybrid_verify_compute(t) {
            Ok(captured) => {
                self.hybrid_verify_graphs.insert(key, captured);
            }
            Err(error) => {
                tracing::warn!(
                    "wyłączono capture grafu hybrid verifier slot={slot} T={t}: {error}"
                );
                self.hybrid_verify_graph_disabled.insert(key);
            }
        }
    }

    fn verify_hybrid_greedy_draft_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        budget: usize,
        mtp_states: &mut [MtpDraftState; 2],
        mtp_kv: &mut KvCache,
        external_sources: [bool; 2],
    ) -> Result<MtpB2Verification> {
        if !matches!(budget, 2 | 3) {
            return Err(ForgeError::Unsupported(
                "verifier MTP B2 wymaga wspólnego K=2 lub K=3".into(),
            ));
        }
        let t = budget.checked_add(1).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie liczby kroków verifiera MTP B2".into())
        })?;
        let checked_elements = |name: &str, dimensions: &[usize]| {
            dimensions.iter().try_fold(1usize, |elements, &dimension| {
                elements.checked_mul(dimension).ok_or_else(|| {
                    ForgeError::Scheduler(format!("przepełnienie wymiaru {name}: {dimensions:?}"))
                })
            })
        };
        let total = checked_elements("mtp b2 total", &[2, t])?;
        self.validate_hybrid_speculation_target()?;
        self.ensure_prefill_bufs()?;
        self.ensure_verify_bufs(total)?;
        self.ensure_mtp_b2_bufs()?;
        for seq in seqs.iter_mut() {
            self.activate_hybrid_sequence(seq)?;
        }
        let leases = [
            seqs[0].hybrid_state.expect("lane0 ma lease"),
            seqs[1].hybrid_state.expect("lane1 ma lease"),
        ];
        let p = self.weights.descriptor.params.clone();
        let ssm = p.ssm.as_ref().expect("target B2 ma DeltaNet");
        let q_elements = checked_elements("mtp b2 q", &[total, p.n_heads, p.head_dim])?;
        let q_norm_rows = checked_elements("mtp b2 q norm", &[total, p.n_heads])?;
        let kv_norm_rows = checked_elements("mtp b2 kv norm", &[total, p.n_kv_heads])?;
        let delta_norm_rows = checked_elements("mtp b2 delta norm", &[total, ssm.n_v_heads()])?;
        let ffn_rows = checked_elements("mtp b2 ffn", &[total, p.intermediate_size])?;
        let key_width = checked_elements("mtp b2 key width", &[ssm.d_state, ssm.n_group])?;
        let value_width = checked_elements("mtp b2 value width", &[ssm.d_state, ssm.n_v_heads()])?;
        let conv_width = key_width
            .checked_mul(2)
            .and_then(|key| key.checked_add(value_width))
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie szerokości conv MTP B2".into()))?;
        let conv_elems = checked_elements(
            "mtp b2 conv state",
            &[
                conv_width,
                ssm.d_conv
                    .checked_sub(1)
                    .ok_or_else(|| ForgeError::Scheduler("MTP B2 wymaga d_conv > 0".into()))?,
            ],
        )?;
        let hidden_bytes = checked_elements("mtp b2 hidden bytes", &[p.hidden_size, 2])?;
        let bases = [seqs[0].len, seqs[1].len];
        let ends = [
            bases[0].checked_add(t).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji verifiera MTP B2 lane0".into())
            })?,
            bases[1].checked_add(t).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji verifiera MTP B2 lane1".into())
            })?,
        ];
        for end in ends {
            if end > p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    end - 1,
                    p.max_position_embeddings
                )));
            }
        }
        let required_pages = seqs
            .iter()
            .enumerate()
            .try_fold(0usize, |sum, (lane, seq)| {
                let pages = ends[lane]
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                sum.checked_add(pages).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie stron verifiera MTP B2".into())
                })
            })?;
        self.ensure_free_pages(required_pages);
        if required_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "verifier MTP B2 wymaga {required_pages} stron KV, dostępne {}",
                self.kv.free_page_count()
            )));
        }

        let mut snapshot_ready = false;
        let mut metadata_enqueued = false;
        let result = (|| {
            for seq in seqs.iter_mut() {
                for _ in 0..t {
                    self.kv.grow(seq)?;
                }
            }
            let page_table_elems =
                checked_elements("mtp b2 page tables", &[2, self.max_pages_per_seq])?;
            let mut page_tables = vec![-1i32; page_table_elems];
            for lane in 0..2 {
                let offset =
                    checked_elements("mtp b2 page table offset", &[lane, self.max_pages_per_seq])?;
                page_tables[offset..offset + seqs[lane].pages.len()]
                    .copy_from_slice(&seqs[lane].pages);
            }
            let pb = self.prefill_bufs.as_ref().expect("prefill gotowy");
            let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
            let mut metadata = Vec::with_capacity(2 + page_table_elems);
            metadata.extend([bases[0] as i32, bases[1] as i32]);
            metadata.extend_from_slice(&page_tables);
            write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_metadata)?;
            metadata_enqueued = true;
            self.device.copy(
                &b2.pinned_metadata,
                0,
                &b2.base_positions,
                0,
                8,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                8,
                &b2.page_tables,
                0,
                page_table_elems * 4,
                &self.stream,
            )?;
            self.kernels.mtp_pack_verify_inputs(
                &pb.ids,
                &pb.positions,
                &b2.visible_lens,
                &mtp_states[0].token_ids,
                &mtp_states[1].token_ids,
                &b2.base_positions,
                t,
                &self.stream,
            )?;
            let target_embedding = self
                .weights
                .mtp
                .as_ref()
                .and_then(|mtp| mtp.shares_target_embedding.then_some(&mtp.embedding))
                .ok_or_else(|| {
                    ForgeError::Unsupported("MTP B2 wymaga device-side target embeddingu".into())
                })?;
            match target_embedding {
                MtpEmbedding::Device(DevWeight::F16 { buf, rows, cols }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding F16 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q8_0 { buf, rows, cols }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding Q8_0 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q8_0_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.vocab_size,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                    buf,
                    output_scale,
                    rows,
                    cols,
                    layout: Nvfp4GgufLayout::RowMajor36,
                }) => {
                    if *rows != p.vocab_size || *cols != p.hidden_size {
                        return Err(ForgeError::Format(
                            "target embedding NVFP4 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_nvfp4_gguf_rows_f16(
                        &pb.h,
                        buf,
                        &pb.ids,
                        total,
                        p.vocab_size,
                        p.hidden_size,
                        *output_scale,
                        &self.stream,
                    )?;
                }
                _ => {
                    return Err(ForgeError::Unsupported(
                        "target MTP B2 wymaga embeddingu F16, Q8_0 lub GGUF NVFP4".into(),
                    ));
                }
            }
            if external_sources.into_iter().any(|external| external) {
                self.device.copy(
                    &pb.h,
                    0,
                    &b2.catchup_embeddings,
                    0,
                    total * p.hidden_size * 2,
                    &self.stream,
                )?;
            }

            for (layer_index, cache) in b2.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                for (lane, &lease) in leases.iter().enumerate() {
                    let (conv, state) = self
                        .hybrid_states
                        .as_ref()
                        .expect("model ma pulę hybrydową")
                        .state_buffers(lease, layer_index)?
                        .expect("warstwa DeltaNet ma stan");
                    self.device.copy(
                        &conv,
                        0,
                        &cache.conv_initial,
                        lane * conv.len(),
                        conv.len(),
                        &self.stream,
                    )?;
                    self.device.copy(
                        &state,
                        0,
                        &cache.state_initial,
                        lane * state.len(),
                        state.len(),
                        &self.stream,
                    )?;
                }
            }
            snapshot_ready = true;

            self.kernels.rmsnorm_f16(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                total,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
                            return Err(ForgeError::Unsupported(
                                "target MTP B2 wymaga rozdzielonych Q/K/V".into(),
                            ));
                        };
                        self.gemm(&b2.q_full, q, &pb.x, total, &self.stream)?;
                        self.kernels.deinterleave_gate_f16(
                            &b2.qc,
                            &b2.gatec,
                            &b2.q_full,
                            p.head_dim,
                            q_elements,
                            &self.stream,
                        )?;
                        if let Some(norm) = &attention.q_norm {
                            self.kernels.rmsnorm_f16(
                                &b2.qc,
                                &b2.qc,
                                norm,
                                q_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        self.gemm(&pb.k, k, &pb.x, total, &self.stream)?;
                        self.gemm(&pb.v, v, &pb.x, total, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &pb.k,
                                &pb.k,
                                norm,
                                kv_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        let n_rot = self.hybrid_n_rot();
                        self.kernels.rope_neox_partial_f16(
                            &b2.qc,
                            &pb.positions,
                            total,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &pb.k,
                            &pb.positions,
                            total,
                            p.n_kv_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        let kv_layer = self.target_kv_layer(layer_index);
                        self.kernels.kv_append_batch_segmented_f16(
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &pb.k,
                            &pb.v,
                            &b2.page_tables,
                            &b2.base_positions,
                            2,
                            t,
                            self.max_pages_per_seq,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        self.kernels.attn_verify_segmented_f16_hd256(
                            &pb.attn_out,
                            &b2.qc,
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.page_tables,
                            &b2.visible_lens,
                            2,
                            t,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            1.0 / (p.head_dim as f32).sqrt(),
                            &self.stream,
                        )?;
                        self.kernels.sigmoid_mul_f16(
                            &b2.gated,
                            &pb.attn_out,
                            &b2.gatec,
                            q_elements,
                            &self.stream,
                        )?;
                        self.gemm(&pb.o_out, &attention.attn_o, &b2.gated, total, &self.stream)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let cache = b2.delta[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma cache B2");
                        self.gemm(&b2.qkv_mixed, &delta.in_proj, &pb.x, total, &self.stream)?;
                        if let Some(cols) =
                            Self::delta_input_q8_cols(delta).filter(|_| matches!(total, 6 | 8))
                        {
                            let mut prepared =
                                self.kernels
                                    .prepare_q8_1(&pb.x, cols, total, &self.stream)?;
                            self.gemm_q8_prepared(&b2.z, &delta.gate_proj, &mut prepared, total)?;
                            self.gemm_q8_prepared(
                                &b2.alpha,
                                &delta.alpha_proj,
                                &mut prepared,
                                total,
                            )?;
                            self.gemm_q8_prepared(
                                &b2.beta_raw,
                                &delta.beta_proj,
                                &mut prepared,
                                total,
                            )?;
                        } else {
                            self.gemm(&b2.z, &delta.gate_proj, &pb.x, total, &self.stream)?;
                            self.gemm(&b2.alpha, &delta.alpha_proj, &pb.x, total, &self.stream)?;
                            self.gemm(&b2.beta_raw, &delta.beta_proj, &pb.x, total, &self.stream)?;
                        }
                        self.kernels.deltanet_prepare_segmented_f16(
                            &cache.q,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &cache.conv_checkpoints,
                            &cache.conv_initial,
                            &b2.qkv_mixed,
                            &delta.conv1d,
                            &b2.alpha,
                            &b2.beta_raw,
                            &delta.dt_bias,
                            &delta.a,
                            2,
                            t,
                            ssm.n_k_heads(),
                            ssm.n_v_heads(),
                            ssm.d_state,
                            ssm.d_conv,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        match self.delta_state_layout() {
                            DeltaStateLayout::ValueKey => {
                                self.kernels.deltanet_value_key_scan_inplace_f16(
                                    &b2.o,
                                    &b2.selected_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    2,
                                    t,
                                    ssm.n_v_heads(),
                                    &self.stream,
                                )?
                            }
                            DeltaStateLayout::KeyValue => {
                                self.kernels.deltanet_gated_scan_segmented_shared_d128_f16(
                                    &b2.o,
                                    &b2.selected_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    2,
                                    t,
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?
                            }
                        }
                        self.kernels.deltanet_gated_rmsnorm_f16(
                            &b2.normed,
                            &b2.o,
                            &b2.z,
                            &delta.ssm_norm,
                            delta_norm_rows,
                            ssm.d_state,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        self.gemm(&pb.o_out, &delta.out_proj, &b2.normed, total, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    total,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return Err(ForgeError::Unsupported("MTP B2 nie obsługuje MoE".into()));
                };
                let GateUpWeights::Split { gate, up } = &ffn.gate_up else {
                    return Err(ForgeError::Unsupported(
                        "MTP B2 wymaga rozdzielonych gate/up".into(),
                    ));
                };
                self.gemm(&pb.gate, gate, &pb.x, total, &self.stream)?;
                self.gemm(&pb.up, up, &pb.x, total, &self.stream)?;
                self.kernels
                    .glu_mul_f16(self.ffn_act(), &pb.act, &pb.gate, &pb.up, ffn_rows, &self.stream)?;
                self.gemm(&pb.down, &ffn.down, &pb.act, total, &self.stream)?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.down,
                    next_norm,
                    total,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
            }

            let vb = self.verify_bufs.as_ref().expect("verify gotowy");
            self.logits_gemm(&vb.logits, &pb.x, total, &self.stream)?;
            self.kernels.sample_batched_argmax_f32(
                &vb.ids,
                &vb.logits,
                total,
                p.vocab_size,
                &self.stream,
            )?;
            self.kernels.mtp_verify_decide_segmented(
                &b2.decisions,
                &vb.ids,
                &pb.ids,
                2,
                t,
                &self.stream,
            )?;
            self.kernels.mtp_select_row_segmented_f16(
                &b2.selected_hidden,
                &pb.x,
                &b2.decisions,
                2,
                t,
                p.hidden_size,
                &self.stream,
            )?;

            for (layer_index, cache) in b2.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                match self.delta_state_layout() {
                    DeltaStateLayout::ValueKey => {
                        self.kernels.deltanet_value_key_commit_recompute_f32(
                            &b2.selected_states,
                            &cache.state_initial,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.decisions,
                            2,
                            t,
                            ssm.n_v_heads(),
                            &self.stream,
                        )?
                    }
                    DeltaStateLayout::KeyValue => self
                        .kernels
                        .deltanet_commit_recompute_segmented_shared_d128_f32(
                            &b2.selected_states,
                            &cache.state_initial,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.decisions,
                            2,
                            t,
                            ssm.n_v_heads(),
                            ssm.d_state,
                            &self.stream,
                        )?,
                }
                self.kernels.mtp_select_row_segmented_f16(
                    &b2.selected_conv,
                    &cache.conv_checkpoints,
                    &b2.decisions,
                    2,
                    t,
                    conv_elems,
                    &self.stream,
                )?;
                for (lane, &lease) in leases.iter().enumerate() {
                    let (conv, state) = self
                        .hybrid_states
                        .as_ref()
                        .expect("model ma pulę hybrydową")
                        .state_buffers(lease, layer_index)?
                        .expect("warstwa DeltaNet ma stan");
                    self.device.copy(
                        &b2.selected_conv,
                        lane * conv.len(),
                        &conv,
                        0,
                        conv.len(),
                        &self.stream,
                    )?;
                    self.device.copy(
                        &b2.selected_states,
                        lane * state.len(),
                        &state,
                        0,
                        state.len(),
                        &self.stream,
                    )?;
                }
            }
            for (lane, state) in mtp_states.iter_mut().enumerate() {
                if !external_sources[lane] {
                    self.device.copy(
                        &b2.selected_hidden,
                        lane * hidden_bytes,
                        &state.recurrent_hidden,
                        0,
                        hidden_bytes,
                        &self.stream,
                    )?;
                }
            }
            if external_sources.into_iter().any(|external| external) {
                self.mtp_catchup_verified_prefix_b2(mtp_states, mtp_kv, t, external_sources)?;
            }
            self.device
                .copy(&b2.decisions, 0, &b2.pinned_decisions, 0, 16, &self.stream)?;
            for (lane, state) in mtp_states.iter().enumerate() {
                self.device.copy(
                    &state.token_ids,
                    0,
                    &b2.pinned_decisions,
                    16 + lane * 20,
                    20,
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            let decision_ptr = b2
                .pinned_decisions
                .host_ptr()
                .expect("decyzje B2 mają mapowanie") as *const i32;
            let mut results: [(Vec<u32>, usize, u32); 2] =
                std::array::from_fn(|_| (Vec::with_capacity(budget), 0, 0));
            for (lane, result) in results.iter_mut().enumerate() {
                let retained = unsafe { *decision_ptr.add(2 * lane) };
                let correction = unsafe { *decision_ptr.add(2 * lane + 1) };
                if retained <= 0
                    || retained as usize > t
                    || correction < 0
                    || correction as usize >= p.vocab_size
                {
                    return Err(ForgeError::Kernel(format!(
                        "decyzja MTP B2 lane {lane} poza zakresem"
                    )));
                }
                let ids = unsafe {
                    std::slice::from_raw_parts(decision_ptr.add(4 + lane * 5), budget + 1)
                };
                if ids.iter().any(|&id| id < 0 || id as usize >= p.vocab_size) {
                    return Err(ForgeError::Kernel(format!(
                        "draft MTP B2 lane {lane} poza zakresem"
                    )));
                }
                result.0.extend(ids[1..].iter().map(|&id| id as u32));
                result.1 = retained as usize - 1;
                result.2 = correction as u32;
            }
            let metadata_targets = validate_mtp_pair_metadata_commit(
                mtp_states,
                [results[0].1 + 1, results[1].1 + 1],
            )?;
            Ok((results, metadata_targets))
        })();

        match result {
            Ok((results, metadata_targets)) => {
                for lane in 0..2 {
                    self.kv
                        .rollback(seqs[lane], bases[lane] + results[lane].1 + 1);
                }
                self.pt_seq = 0;
                Ok((results, metadata_targets))
            }
            Err(error) => {
                for lane in 0..2 {
                    self.kv.rollback(seqs[lane], bases[lane]);
                }
                if snapshot_ready {
                    let b2 = self.mtp_b2_bufs.as_ref().expect("MTP B2 gotowy");
                    for (layer_index, cache) in b2.delta.iter().enumerate() {
                        let Some(cache) = cache else { continue };
                        for (lane, &lease) in leases.iter().enumerate() {
                            let (conv, state) = self
                                .hybrid_states
                                .as_ref()
                                .expect("model ma pulę hybrydową")
                                .state_buffers(lease, layer_index)?
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.conv_initial,
                                lane * conv.len(),
                                &conv,
                                0,
                                conv.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.state_initial,
                                lane * state.len(),
                                &state,
                                0,
                                state.len(),
                                &self.stream,
                            )?;
                        }
                    }
                    self.device.synchronize()?;
                } else if metadata_enqueued {
                    self.device.synchronize()?;
                }
                self.pt_seq = 0;
                Err(error)
            }
        }
    }

    fn verify_hybrid_greedy_draft(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
        catchup_mtp: bool,
    ) -> Result<(usize, u32)> {
        if !matches!(draft.len(), 2 | 3) {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier spekulacyjny obsługuje draft długości 2 lub 3".into(),
            ));
        }
        self.activate_hybrid_sequence(seq)?;
        let hybrid_slot = seq
            .hybrid_state
            .expect("aktywna sekwencja hybrydowa ma przypisany slot")
            .slot;
        let t = draft.len() + 1;
        self.validate_hybrid_speculation_target()?;
        self.ensure_prefill_bufs()?;
        self.ensure_verify_bufs(4)?;
        self.ensure_hybrid_verify_bufs(4)?;
        let p = self.weights.descriptor.params.clone();
        let base = seq.len;
        if base + t > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                base + t - 1,
                p.max_position_embeddings
            )));
        }
        self.ensure_free_pages(
            (base + t)
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len()),
        );
        let mut snapshot_ready = false;
        let result = (|| {
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            for (layer_index, cache) in hv.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                let state = self.active_ssm()[layer_index]
                    .as_ref()
                    .expect("warstwa DeltaNet ma stan");
                self.device.copy(
                    &state.state,
                    0,
                    &cache.state_initial,
                    0,
                    state.state.len(),
                    &self.stream,
                )?;
                self.device.copy(
                    &state.conv,
                    0,
                    &cache.conv_initial,
                    0,
                    state.conv.len(),
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            snapshot_ready = true;
            for _ in 0..t {
                self.kv.grow(seq)?;
            }
            let mut page_table = vec![-1i32; self.max_pages_per_seq];
            page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.device
                .write(bytemuck::cast_slice(&page_table), &self.page_table_dev, 0)?;
            self.pt_seq = seq.id;
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("bufory prefill są gotowe");
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            let tokens: Vec<u32> = std::iter::once(fed).chain(draft.iter().copied()).collect();
            let ids: Vec<i32> = tokens.iter().map(|&id| id as i32).collect();
            let positions: Vec<i32> = (base..base + t).map(|pos| pos as i32).collect();
            let visible_lens: Vec<i32> = (base + 1..=base + t).map(|len| len as i32).collect();
            self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
            self.device
                .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;
            self.device
                .write(&(base as i32).to_le_bytes(), &hv.base_pos, 0)?;
            self.device
                .write(bytemuck::cast_slice(&visible_lens), &hv.visible_lens, 0)?;
            let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
            })?;
            let staging = hv.host_staging[0]
                .embedding
                .host_ptr()
                .expect("pinned embedding ma mapowanie hosta");
            for (row_index, &token) in tokens.iter().enumerate() {
                let source = table
                    .get(token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size)
                    .ok_or_else(|| {
                        ForgeError::Scheduler(format!(
                            "token id {token} wykracza poza embedding targetu"
                        ))
                    })?;
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source.as_ptr() as *const u8,
                        staging.add(row_index * p.hidden_size * 2),
                        p.hidden_size * 2,
                    );
                }
            }
            self.device.copy(
                &hv.host_staging[0].embedding,
                0,
                &pb.h,
                0,
                t * p.hidden_size * 2,
                &self.stream,
            )?;
            let graph =
                if std::env::var("FORGE_HYBRID_VERIFY_GRAPH").is_ok_and(|value| value == "0") {
                    None
                } else {
                    self.hybrid_verify_graphs.get(&(hybrid_slot, t)).cloned()
                };
            if let Some(graph) = graph {
                self.device.launch_graph(&graph, &self.stream)?;
            } else {
                self.run_hybrid_verify_compute(t)?;
            }
            self.device.synchronize()?;
            let decision =
                hv.pinned_decision
                    .host_ptr()
                    .expect("pinned decision ma mapowanie hosta") as *const i32;
            let retained = unsafe { *decision };
            let correction = unsafe { *decision.add(1) };
            if retained <= 0
                || retained as usize > t
                || correction < 0
                || correction as usize >= p.vocab_size
            {
                return Err(ForgeError::Kernel(
                    "decyzja MTP z GPU ma wartość poza zakresem".into(),
                ));
            }
            let accepted = retained as usize - 1;
            if catchup_mtp {
                self.mtp_catchup_verified_prefix(seq, accepted + 1, 0, None)?;
            }
            self.capture_hybrid_verify_graph_if_needed(hybrid_slot, t);
            Ok((accepted, correction as u32))
        })();
        match result {
            Ok((accepted, correction)) => {
                self.kv.rollback(seq, base + accepted + 1);
                self.pt_seq = 0;
                Ok((accepted, correction))
            }
            Err(error) => {
                self.kv.rollback(seq, base);
                self.pt_seq = 0;
                if snapshot_ready {
                    let restore = (|| {
                        let hv = self
                            .hybrid_verify_bufs
                            .as_ref()
                            .expect("bufory hybrid verify są gotowe");
                        for (layer_index, cache) in hv.delta.iter().enumerate() {
                            let Some(cache) = cache else { continue };
                            let state = self.active_ssm()[layer_index]
                                .as_ref()
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.state_initial,
                                0,
                                &state.state,
                                0,
                                state.state.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.conv_initial,
                                0,
                                &state.conv,
                                0,
                                state.conv.len(),
                                &self.stream,
                            )?;
                        }
                        self.device.synchronize()
                    })();
                    if let Err(restore_error) = restore {
                        return Err(ForgeError::Scheduler(format!(
                            "błąd verifiera MTP: {error}; błąd odtworzenia SSM: {restore_error}"
                        )));
                    }
                }
                Err(error)
            }
        }
    }

    /// Wykonuje jeden cykl natywnego MTP. Bieżące logity targetu przewidziały
    /// już `fed`, więc draft powstaje bez osobnego kroku targetu, a target
    /// zatwierdza `[fed, draft...]` przebiegiem T=3/4. Zwracany correction
    /// pozostaje tokenem do podania w następnym cyklu i nie jest jeszcze
    /// zapisany w stanie targetu.
    pub fn native_mtp_step(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        budget: usize,
    ) -> Result<(Vec<u32>, usize, u32)> {
        if budget != 2 && budget != 3 {
            return Err(ForgeError::Unsupported(
                "natywny MTP obsługuje budget 2 lub 3".into(),
            ));
        }
        self.validate_native_mtp_target()?;
        if self.native_mtp_available_budget(seq, budget) != budget {
            return Err(ForgeError::Scheduler(format!(
                "brak pojemności targetu dla MTP K={budget}"
            )));
        }
        let draft = self.mtp_propose_pending(seq, fed, budget)?;
        match self.verify_hybrid_greedy_draft(seq, fed, &draft, false) {
            Ok((accepted, correction)) => {
                let (lease, mut state, mut mtp_kv) = self.take_mtp_runtime(seq)?;
                let result = state
                    .commit_prefix(&mut mtp_kv, accepted + 1, &self.stream)
                    .and_then(|_| {
                        self.device.copy(
                            &self.bufs.x,
                            0,
                            &state.recurrent_hidden,
                            0,
                            state.recurrent_hidden.len(),
                            &self.stream,
                        )
                    })
                    .and_then(|_| self.device.synchronize());
                self.finish_mtp_runtime(lease, state, mtp_kv, result)?;
                Ok((draft, accepted, correction))
            }
            Err(error) => {
                if let Err(rollback_error) = self.rollback_mtp_pending(seq) {
                    return Err(ForgeError::Scheduler(format!(
                        "błąd verifiera MTP: {error}; błąd rollbacku draftu: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// Wykonuje wspólny cykl natywnego MTP dla dwóch sekwencji z tym samym K.
    pub fn native_mtp_step_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        budget: usize,
    ) -> Result<[(Vec<u32>, usize, u32); 2]> {
        self.native_mtp_routed_step_b2(seqs, fed, budget, [None, None])
    }

    /// Wykonuje wspólny verifier B2 dla dowolnej pary źródeł MTP/n-gram.
    pub fn native_mtp_routed_step_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        budget: usize,
        external_drafts: [Option<&[u32]>; 2],
    ) -> Result<[(Vec<u32>, usize, u32); 2]> {
        if !self.native_mtp_b2_capable([&*seqs[0], &*seqs[1]], budget) {
            return Err(ForgeError::Unsupported(
                "para nie spełnia kontraktu routed MTP B2".into(),
            ));
        }
        self.mtp_propose_pending_b2(seqs, fed, budget, external_drafts)?;
        let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
        let external_sources = external_drafts.map(|draft| draft.is_some());
        let result =
            match self.verify_hybrid_greedy_draft_b2(
                seqs,
                budget,
                &mut states,
                &mut mtp_kv,
                external_sources,
            ) {
                Ok((results, metadata_targets)) => {
                    apply_mtp_pair_metadata_commit(&mut states, &mut mtp_kv, metadata_targets);
                    Ok(results)
                }
                Err(error) => match rollback_mtp_pair(&mut states, &mut mtp_kv, &self.stream) {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(self
                        .poison_mtp_runtime(format!("błąd verifiera MTP B2: {error}; {rollback}"))),
                },
            };
        self.pt_seq = 0;
        self.finish_mtp_runtime_pair(leases, states, mtp_kv, result)
    }

    /// Weryfikuje dwa pełne drafty zewnętrznego proposera i dogania MTP na GPU.
    pub fn verify_greedy_draft_with_mtp_catchup_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        fed: [u32; 2],
        drafts: [&[u32]; 2],
    ) -> Result<[(usize, u32); 2]> {
        let budget = drafts[0].len();
        if drafts[1].len() != budget || !matches!(budget, 2 | 3) {
            return Err(ForgeError::Unsupported(
                "MTP+n-gram B2 wymaga dwóch draftów z tym samym K=2 lub K=3".into(),
            ));
        }
        self.native_mtp_routed_step_b2(seqs, fed, budget, [Some(drafts[0]), Some(drafts[1])])
            .map(|verified| {
                [
                    (verified[0].1, verified[0].2),
                    (verified[1].1, verified[1].2),
                ]
            })
    }

    /// Weryfikuje draft zewnętrznego proposera i dogania stan natywnego MTP.
    pub fn verify_greedy_draft_with_mtp_catchup(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        self.validate_native_mtp_target()?;
        self.verify_hybrid_greedy_draft(seq, fed, draft, true)
    }

    /// Verify one greedy speculative draft in a single forward (SPEC §6, linear
    /// path). Runs the model over `[fed, draft…]` as a mini-prefill chunk
    /// appended after the current position, greedy-argmaxes the logits at every
    /// query position, and accepts the longest draft prefix whose token equals
    /// the model's own argmax at the preceding position. The rejected draft
    /// positions' K/V are rolled back, leaving `fed` + the accepted drafts
    /// resident. Returns `(accepted, correction)`: the number of accepted draft
    /// tokens and the model's argmax token at the first unaccepted position
    /// (the correction when `accepted < draft.len()`, else the bonus token).
    /// Wywołujący musi wcześniej użyć `validate_speculation_target()` oraz
    /// próbkowania greedy, aby wynik był zgodny z dekodowaniem sekwencyjnym.
    pub fn verify_greedy_draft(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        debug_assert!(!draft.is_empty(), "verify called with an empty draft");
        debug_assert!(
            draft.len() <= MAX_SPEC_DRAFT,
            "draft exceeds MAX_SPEC_DRAFT"
        );
        if self.is_hybrid() {
            return self.verify_hybrid_greedy_draft(seq, fed, draft, false);
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let t = draft.len() + 1;
        self.ensure_verify_bufs(t)?;

        let base = seq.len;
        let mut batch = Vec::with_capacity(t);
        batch.push(fed);
        batch.extend_from_slice(draft);
        let result = (|| {
            self.prefill_forward(seq, &batch, true)?;

            let stream = &self.stream;
            let vb = self.verify_bufs.as_ref().expect("ensured above");
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("prefill_forward allocated");
            self.logits_gemm(&vb.logits, &pb.x, t, stream)?;
            self.kernels
                .sample_batched_argmax_f32(&vb.ids, &vb.logits, t, vocab, stream)?;
            self.device
                .copy(&vb.ids, 0, &vb.pinned_ids, 0, t * 4, stream)?;
            self.device.synchronize()?;

            let ptr = vb
                .pinned_ids
                .host_ptr()
                .expect("pinned buffer has host mapping") as *const i32;
            let argmax = unsafe { std::slice::from_raw_parts(ptr, t) };
            let mut accepted = 0usize;
            let mut correction = 0u32;
            for i in 0..t {
                let am = argmax[i] as u32;
                if i < draft.len() && am == draft[i] {
                    accepted += 1;
                } else {
                    correction = am;
                    break;
                }
            }
            Ok((accepted, correction))
        })();

        finish_greedy_verification(&mut self.kv, &mut self.pt_seq, seq, base, result)
    }

    /// Record every launch of one decode step into a replayable graph.
    /// Stream capture does not execute the work, so buffer contents during
    /// capture are irrelevant — only addresses and launch geometry matter.
    fn capture_hybrid_step(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.hybrid_forward_staged(true, AttnSrc::Paged) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                // Przerywamy przechwytywanie, żeby strumień był dalej zdatny.
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    fn capture_step(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = if self.fused_decode_supported() {
            self.run_step_fused(AttnSrc::Paged)
        } else {
            self.run_step_separate(AttnSrc::Paged)
        };
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                // Abort the capture so the stream is usable again.
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step's worth of expert-residency upkeep.
    ///
    /// The router already tallies expert selections on-device for free, so the
    /// only cost here is the periodic round: read the tallies, refresh the
    /// popularity estimate, and move a bounded number of experts between VRAM
    /// and host memory. Rounds are rare and capped because the migration itself
    /// moves whole expert blocks — spending more on shuffling than the better
    /// placement returns would defeat the point.
    ///
    /// A model whose experts all fit in VRAM never has a host-resident expert,
    /// so `plan` returns nothing and this degenerates to a counter read.
    fn tick_moe_residency(&mut self) -> Result<()> {
        let Some(policy) = self.moe_residency.as_mut() else {
            return Ok(());
        };
        policy.tokens_since_round += 1;
        if policy.tokens_since_round < MOE_RESIDENCY_INTERVAL {
            return Ok(());
        }
        policy.tokens_since_round = 0;
        self.rebalance_moe_residency()
    }

    /// Ilu ekspertów siedzi w VRAM, a ilu w pamięci hosta, w całym modelu.
    /// `None` dla modeli bez routowanych ekspertów.
    pub fn moe_expert_residency(&self) -> Option<(usize, usize, usize)> {
        let mut vram = 0usize;
        let mut host = 0usize;
        let mut nvme = 0usize;
        let mut any = false;
        for layer in &self.weights.layers {
            let LayerFfn::Moe(moe) = &layer.ffn else {
                continue;
            };
            any = true;
            for stack in [&moe.gate_exps, &moe.up_exps, &moe.down_exps] {
                let (v, h, n) = stack.tier_counts();
                vram += v;
                host += h;
                nvme += n;
            }
        }
        any.then_some((vram, host, nvme))
    }

    fn rebalance_moe_residency(&mut self) -> Result<()> {
        // Tallies are written by kernels still queued on the model stream.
        self.stream.synchronize()?;
        let mut planned: Vec<Migration> = Vec::new();
        {
            let state = self
                .moe_residency
                .as_mut()
                .expect("residency state present for MoE models");
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                let LayerFfn::Moe(moe) = &layer.ffn else {
                    continue;
                };
                let counts = moe.usage.take(self.device.as_ref())?;
                state.policy.observe(layer_index, &counts);
                for (projection, stack) in [
                    (Projection::Gate, &moe.gate_exps),
                    (Projection::Up, &moe.up_exps),
                    (Projection::Down, &moe.down_exps),
                ] {
                    planned.extend(state.policy.candidates(
                        ProjectionId {
                            layer: layer_index,
                            projection,
                        },
                        stack,
                    ));
                }
            }
        }
        let planned = self
            .moe_residency
            .as_ref()
            .expect("residency state present")
            .policy
            .select_round(planned);
        if planned.is_empty() {
            return Ok(());
        }
        // The captured decode graph reads expert bases from the device-resident
        // pointer table, so a migration needs no re-capture — the table update
        // is picked up by the next replay.
        tracing::debug!(
            migrations = planned.len(),
            "runda rezydencji ekspertów: przenoszę do VRAM"
        );
        for migration in planned {
            let scratch = self
                .moe_residency
                .as_ref()
                .expect("residency state present")
                .scratch
                .clone();
            let LayerFfn::Moe(moe) = &self.weights.layers[migration.target.layer].ffn else {
                continue;
            };
            let stack = match migration.target.projection {
                Projection::Gate => &moe.gate_exps,
                Projection::Up => &moe.up_exps,
                Projection::Down => &moe.down_exps,
            };
            stack.promote_to_vram(
                self.device.as_ref(),
                migration.promote,
                migration.demote,
                &scratch,
                &self.stream,
            )?;
        }
        Ok(())
    }

    /// Whether every routed-MoE layer supports the device-side grouped dispatch
    /// (no host readback anywhere in the forward), so `run_step_moe` records
    /// cleanly into a replayable graph. False if any layer has a fallback quant
    /// (e.g. Q8_0 experts) that still needs a per-layer router readback.
    fn moe_fully_gidx(&self) -> bool {
        self.weights.layers.iter().all(|l| match &l.ffn {
            LayerFfn::Moe(moe) => Self::moe_gidx_capable(moe),
            LayerFfn::Dense(_) => true,
        })
    }

    /// Record the non-hybrid MoE decode step into a replayable graph. Only valid
    /// when `moe_fully_gidx()`: the expert dispatch reads the router selection on
    /// device (no readback), and all per-token inputs (token id, position, page
    /// table, seq len) come from device buffers refreshed before each replay.
    fn capture_step_moe(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_moe();
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step of the non-fused (separate-kernel) chain: explicit
    /// rmsnorm → qkv GEMVs → qkv_post (norm/rope/paged append) → attention →
    /// ffn. `src` selects the attention's K/V source: the paged cache
    /// (recorded into the replayable graph) or the tier staging slabs holding
    /// the sequence's full context per layer (streamed path, never captured).
    fn run_step_separate(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;

        {
            kernels.gather_rows_f16(
                &b.h,
                &self.weights.token_embd_f16,
                &b.ids,
                1,
                hidden,
                stream,
            )?;
            // Skalowanie embeddingu (rodzina Gemma) — jak w prefillu.
            if let Some(factor) = p.embd_scale {
                kernels.scale_f16(&b.h, hidden, factor, stream)?;
            }
            kernels.rmsnorm_f16(
                &b.x,
                &b.h,
                &self.weights.layers[0].attn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            let n_layers = self.weights.layers.len();
            for l in 0..n_layers {
                let layer = &self.weights.layers[l];
                // Geometria bywa różna per warstwa (Gemma 4), więc szerokości i
                // offsety sekcji scalonego q|k|v muszą być liczone W PĘTLI —
                // policzone raz dla całego modelu wskazywały poza bufor warstwy.
                let head_dim = p.head_dim_at(l);
                let n_kv_heads = p.n_kv_heads_at(l);
                let scale = p.attn_scale_at(l);
                let q_dim = p.n_heads * head_dim;
                let kv_dim = p.n_kv_heads_at(l) * head_dim;
                // Byte offsets of the K and V sections inside the fused q|k|v
                // decode buffer (q occupies rows 0..q_dim, so its offset is 0).
                let k_byte_off = q_dim * 2;
                let v_byte_off = (q_dim + kv_dim) * 2;

                // Fused layers project q|k|v with ONE GEMV into one buffer,
                // then qkv_post fuses the whole q/k-norm + RoPE + kv-append
                // stretch into a second single launch (sections resolved via
                // host-computed byte offsets; rotated K lands directly in the
                // cache, so the K section of b.qkv is left un-rotated —
                // nothing reads it after this point).
                let q_buf = match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemv(&b.qkv, w, &b.x, stream)?;
                        kernels.qkv_post_f16(
                            &b.qkv,
                            0,
                            &b.qkv,
                            k_byte_off,
                            &b.qkv,
                            v_byte_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &b.pos,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            eps,
                            p.rope_theta_at(l),
                            stream,
                        )?;
                        &b.qkv
                    }
                    QkvWeights::FusedQk { qk, v } => {
                        // q|k land at the front of b.qkv (same section
                        // offsets as the fully fused layout); v is projected
                        // into its own buffer and handed to qkv_post by
                        // pointer.
                        self.gemv(&b.qkv, qk, &b.x, stream)?;
                        self.gemv(&b.v, v, &b.x, stream)?;
                        kernels.qkv_post_f16(
                            &b.qkv,
                            0,
                            &b.qkv,
                            k_byte_off,
                            &b.v,
                            0,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &b.pos,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            eps,
                            p.rope_theta_at(l),
                            stream,
                        )?;
                        &b.qkv
                    }
                    QkvWeights::Split { q, k, v } => {
                        self.gemv(&b.q, q, &b.x, stream)?;
                        self.gemv(&b.k, k, &b.x, stream)?;
                        self.gemv(&b.v, v, &b.x, stream)?;
                        let aw = layer.attn();
                        match (aw.q_norm.as_ref(), aw.k_norm.as_ref(), aw.v_norm.as_ref()) {
                            (Some(qn), Some(kn), Some(vn)) => {
                                kernels.rmsnorm_qkv_f16(
                                    &b.q,
                                    &b.k,
                                    &b.v,
                                    qn,
                                    kn,
                                    vn,
                                    p.n_heads,
                                    n_kv_heads,
                                    head_dim,
                                    eps,
                                    stream,
                                )?;
                            }
                            _ => {
                                if let Some(qn) = aw.q_norm.as_ref() {
                                    kernels.rmsnorm_f16(
                                        &b.q, &b.q, qn, p.n_heads, head_dim, eps, stream,
                                    )?;
                                }
                                if let Some(kn) = aw.k_norm.as_ref() {
                                    kernels.rmsnorm_f16(
                                        &b.k, &b.k, kn, n_kv_heads, head_dim, eps, stream,
                                    )?;
                                }
                                if let Some(vn) = aw.v_norm.as_ref() {
                                    kernels.rmsnorm_f16(
                                        &b.v, &b.v, vn, n_kv_heads, head_dim, eps, stream,
                                    )?;
                                }
                            }
                        }
                        kernels.rope_neox_f16(
                            &b.q,
                            &b.pos,
                            1,
                            p.n_heads,
                            head_dim,
                            p.rope_theta_at(l),
                            self.rope_freqs_at(&p, l),
                            stream,
                        )?;
                        kernels.rope_neox_f16(
                            &b.k,
                            &b.pos,
                            1,
                            p.n_kv_heads_at(l),
                            head_dim,
                            p.rope_theta_at(l),
                            self.rope_freqs_at(&p, l),
                            stream,
                        )?;
                        kernels.kv_append_f16(
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &b.k,
                            &b.v,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            n_kv_heads,
                            self.kv.cfg.page_size,
                            head_dim,
                            stream,
                        )?;
                        &b.q
                    }
                };

                match &src {
                    AttnSrc::Paged => {
                        kernels.attn_decode_f16(
                            &b.attn_out,
                            &b.attn_parts,
                            q_buf,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            1,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            self.attn_window(l),
                            stream,
                        )?;
                    }
                    AttnSrc::Staged(seq) => {
                        // qkv_post / kv_append above already committed the new
                        // token to the canonical paged slab; staging picks it
                        // up through the resident-page D2D copies.
                        let tier = self
                            .tier
                            .as_ref()
                            .expect("staged attention requires tiering");
                        let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                        let slot = &tb.slots[0];
                        tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                        kernels.attn_decode_f16(
                            &b.attn_out,
                            &b.attn_parts,
                            q_buf,
                            &slot.stage[0],
                            &slot.stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            1,
                            p.n_heads,
                            n_kv_heads,
                            head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            self.attn_window(l),
                            stream,
                        )?;
                    }
                }

                self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
                close_block(
                    kernels,
                    layer.post_attn_norm.as_ref(),
                    None,
                    &b.x,
                    &b.h,
                    &b.o_out,
                    &layer.ffn_norm,
                    1,
                    hidden,
                    eps,
                    stream,
                )?;

                match &layer.dense_ffn()?.gate_up {
                    GateUpWeights::Fused(w) => {
                        self.gemv(&b.gate_up, w, &b.x, stream)?;
                        kernels.glu_mul_f16_at(self.ffn_act(), &b.act, &b.gate_up, 0, inter * 2, inter, stream)?;
                    }
                    GateUpWeights::Split { gate, up } => {
                        self.gemv(&b.gate, gate, &b.x, stream)?;
                        self.gemv(&b.up, up, &b.x, stream)?;
                        kernels.glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
                    }
                }
                self.gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;

                let next_norm = if l + 1 < n_layers {
                    &self.weights.layers[l + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                close_block(
                    kernels,
                    layer.post_ffw_norm.as_ref(),
                    layer.layer_output_scale,
                    &b.x,
                    &b.h,
                    &b.down,
                    next_norm,
                    1,
                    hidden,
                    eps,
                    stream,
                )?;
            }

            self.logits_gemv(&b.logits, &b.x, stream)
        }
    }

    /// One decode step for a Mixture-of-Experts model (single token, paged f16
    /// cache). Attention mirrors the explicit separate chain but applies the
    /// model's QK-norm granularity (per-head for Qwen3-MoE, whole-vector for
    /// OLMoE); the FFN is replaced by `moe_decode_ffn`. Graph-captured when the
    /// model is fully gidx-capable (the routed experts are dispatched entirely
    /// on device); a fallback expert quant falls back to per-step launches.
    fn run_step_moe(&self) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = p.attn_scale_at(0);
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Project q/k/v into the separate b.q/b.k/b.v buffers regardless of
            // weight fusion (a fused matrix is read as three row-window GEMVs).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemm_rows(&b.q, w, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, w, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemm_rows(&b.v, w, &b.x, 1, q_dim + kv_dim, kv_dim, stream)?;
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&b.q, qk, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, qk, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
            }

            if let Some(qn) = &layer.attn().q_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, 1, q_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                }
            }
            if let Some(kn) = &layer.attn().k_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, 1, kv_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, p.n_kv_heads, p.head_dim, eps, stream)?;
                }
            }
            kernels.rope_neox_f16(
                &b.q,
                &b.pos,
                1,
                p.n_heads,
                p.head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            kernels.rope_neox_f16(
                &b.k,
                &b.pos,
                1,
                p.n_kv_heads,
                p.head_dim,
                p.rope_theta_at(l),
                self.rope_freqs_at(&p, l),
                stream,
            )?;
            kernels.kv_append_f16(
                &self.kv.k[self.target_kv_layer(l)],
                &self.kv.v[self.target_kv_layer(l)],
                &b.k,
                &b.v,
                &self.page_table_dev,
                &self.seq_len_dev,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                p.head_dim,
                stream,
            )?;
            kernels.attn_decode_f16(
                &b.attn_out,
                &b.attn_parts,
                &b.q,
                &self.kv.k[self.target_kv_layer(l)],
                &self.kv.v[self.target_kv_layer(l)],
                &self.page_table_dev,
                &self.seq_len_dev,
                1,
                p.n_heads,
                p.n_kv_heads,
                p.head_dim,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                scale,
                self.attn_window(l),
                stream,
            )?;

            self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
            close_block(
                kernels,
                layer.post_attn_norm.as_ref(),
                None,
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            match &layer.ffn {
                LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, l, hidden, stream)?,
                LayerFfn::Dense(_) => {
                    return Err(ForgeError::Unsupported(
                        "dense layer inside a MoE forward pass".into(),
                    ))
                }
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            close_block(
                kernels,
                layer.post_ffw_norm.as_ref(),
                layer.layer_output_scale,
                &b.x,
                &b.h,
                &b.down,
                next_norm,
                1,
                hidden,
                eps,
                stream,
            )?;
        }

        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Apply the routed experts for one token: `b.x` holds the FFN-normed
    /// input, `b.down` receives the weighted sum of the selected experts'
    /// SwiGLU outputs (plus the shared expert if present). The top-k experts
    /// are read back to the host to index the stacked expert weights.
    fn moe_decode_ffn(
        &self,
        moe: &MoeFfn,
        layer: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;
        let DevWeight::F16 {
            buf: router_buf, ..
        } = &moe.router
        else {
            return Err(ForgeError::Unsupported("MoE router must be f16".into()));
        };

        // Device-side grouped dispatch: the router's selected ids/weights stay
        // ON the device and drive the expert GEMVs + accumulate through the
        // `_gidx` kernels, so the whole per-layer FFN runs as queued stream work
        // with ZERO host readback / synchronize. The expert count `k` is a fixed
        // model constant (not data-dependent), so the launch sequence is static.
        if Self::moe_gidx_capable(moe) {
            if let Some(sg) = &moe.shared_gate {
                self.gemv(&mb.tmp, sg, &b.x, stream)?;
                self.kernels
                    .moe_sigmoid_f16_to_f32(&mb.shared_scale, &mb.tmp, stream)?;
            }
            self.kernels.moe_router_f16(
                &mb.ids,
                &mb.weights,
                &b.x,
                router_buf,
                moe.usage.counts(),
                1,
                hidden,
                moe.n_experts,
                k,
                moe.norm_topk,
                stream,
            )?;
            return self
                .moe_experts_accumulate_device(moe, &b.x, &b.down, 0, inter, hidden, k, stream);
        }

        // Fallback (expert quant without a `_gidx` kernel, e.g. Q8_0 down
        // projections): route on device but read the top-k selection back to
        // the host to launch the byte-offset expert GEMVs — one sync per layer.
        // Enqueue the shared-expert gate GEMV (when the arch has one) BEFORE the
        // router readback so its logit rides the SAME single sync as the top-k,
        // rather than forcing a second per-layer host round-trip.
        if let Some(sg) = &moe.shared_gate {
            self.gemv(&mb.tmp, sg, &b.x, stream)?;
            self.device
                .copy(&mb.tmp, 0, &mb.pinned_shared, 0, 2, stream)?;
        }
        self.kernels.moe_router_f16(
            &mb.ids,
            &mb.weights,
            &b.x,
            router_buf,
            moe.usage.counts(),
            1,
            hidden,
            moe.n_experts,
            k,
            moe.norm_topk,
            stream,
        )?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                k,
            )
        };
        // Per-token sigmoid gate for the shared expert (`ffn_gate_inp_shexp · x`);
        // 1.0 when the arch declares no shared-expert gate (OLMoE / Qwen3-MoE).
        let shared_scale = if moe.shared_gate.is_some() {
            let sp = mb.pinned_shared.host_ptr().expect("pinned host mapping");
            let bytes = unsafe { *(sp as *const [u8; 2]) };
            let logit = f16::from_le_bytes(bytes).to_f32();
            1.0 / (1.0 + (-logit).exp())
        } else {
            1.0
        };
        self.fault_in_experts(moe, layer, ids)?;
        self.moe_experts_accumulate(
            moe,
            &b.x,
            &b.down,
            0,
            inter,
            hidden,
            ids,
            weights,
            shared_scale,
            stream,
        )
    }

    /// Device-side grouped expert dispatch for a single decode token: identical
    /// SwiGLU math to `moe_experts_accumulate`, but every routed expert's row
    /// window and routing weight are read ON DEVICE from `mb.ids`/`mb.weights`
    /// through the `_gidx` kernels — no host readback, no `synchronize`. The
    /// loop over `k` is over a fixed model constant, so the launch sequence is
    /// static and stream-ordered. The shared expert (row offset 0, host-known)
    /// reuses the ordinary GEMVs and folds in with the device-resident sigmoid
    /// gate scale. Bit-identical to the readback path for the routed experts;
    /// the only difference is the shared-gate sigmoid is computed on-GPU.
    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate_device(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        k: usize,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        for j in 0..k {
            self.gemv_rows_gidx(&b.gate, &moe.gate_exps, x_in, &mb.ids, j, inter, stream)?;
            self.gemv_rows_gidx(&b.up, &moe.up_exps, x_in, &mb.ids, j, inter, stream)?;
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
            self.gemv_rows_gidx(&mb.tmp, &moe.down_exps, &b.act, &mb.ids, j, hidden, stream)?;
            self.kernels.moe_scale_add_gidx_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                &mb.weights,
                j,
                j == 0,
                stream,
            )?;
        }
        if let Some(sh) = &moe.shared {
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            // mb.shared_scale holds this layer's device sigmoid gate scale when
            // the arch has a shared gate; for a gate-less shared expert it stays
            // at the 1.0 seeded once at load, so no per-layer write is needed.
            self.kernels.moe_scale_add_gidx_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                &mb.shared_scale,
                0,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Run each selected expert's SwiGLU over the single-token activation
    /// `x_in` (contiguous [hidden] at offset 0) and accumulate
    /// `weight * expert_out` into `out` at byte offset `out_off`. Reuses the
    /// quant GEMV machinery indexed by expert row-offset; the shared expert (if
    /// any) is folded in last. Scratch (`b.gate/up/act`, `mb.tmp`) is
    /// single-token sized, so this serves both the decode and prefill loops.
    /// Ściąga z dysku każdego wybranego eksperta, który nie jest rezydentny.
    ///
    /// Cały komplet warstwy idzie jednym zgłoszeniem: chybienia trzech
    /// projekcji są znane naraz, a NVMe oddaje pełną przepustowość dopiero przy
    /// głębokiej kolejce — po kolei płaciłoby się sumę opóźnień zamiast
    /// najdłuższego z nich.
    /// Zbiór różnych ekspertów wybranych w całym kawałku prefillu.
    fn chunk_expert_union(&self, ids: &[i32]) -> Vec<i32> {
        let mut union: Vec<i32> = ids.to_vec();
        union.sort_unstable();
        union.dedup();
        union
    }

    /// Czy komplet `count` ekspertów zmieści się naraz w slotach hosta każdej
    /// projekcji warstwy.
    fn expert_union_fits(&self, moe: &MoeFfn, count: usize) -> bool {
        [&moe.gate_exps, &moe.up_exps, &moe.down_exps]
            .into_iter()
            .all(|stack| stack.fully_resident() || stack.host_slots() >= count)
    }

    fn fault_in_experts(&self, moe: &MoeFfn, layer: usize, ids: &[i32]) -> Result<()> {
        let Some(spill) = self.expert_spill.as_ref() else {
            return Ok(());
        };
        let wanted: Vec<usize> = ids.iter().map(|&e| e as usize).collect();
        // Bez zebranych liczników popularność jest zerowa i ofiarą pada
        // dowolny slot — to poprawne, tylko nieoptymalne przez pierwszą rundę.
        let empty = Vec::new();
        let popularity = self
            .moe_residency
            .as_ref()
            .map(|state| state.policy.popularity(layer))
            .unwrap_or(&empty);
        for stack in [&moe.gate_exps, &moe.up_exps, &moe.down_exps] {
            if stack.fully_resident() {
                continue;
            }
            stack.fault_in(self.device.as_ref(), spill, &wanted, popularity)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        ids: &[i32],
        weights: &[f32],
        shared_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        // A single-token GEMV over an expert = a row window of the stacked
        // expert matrix, i.e. gemm_rows at the expert row-offset (rows-per-
        // expert = inter for gate/up, hidden for down).
        for (j, (&e, &wt)) in ids.iter().zip(weights).enumerate() {
            let e = e as usize;
            if e >= moe.n_experts {
                return Err(ForgeError::Kernel(format!(
                    "router selected out-of-range expert {e}"
                )));
            }
            self.gemv_rows(&b.gate, moe.gate_exps.expert(e)?, x_in, 0, inter, stream)?;
            self.gemv_rows(&b.up, moe.up_exps.expert(e)?, x_in, 0, inter, stream)?;
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
            self.gemv_rows(&mb.tmp, moe.down_exps.expert(e)?, &b.act, 0, hidden, stream)?;
            self.kernels
                .moe_scale_add_f16(out, out_off, &mb.tmp, 0, hidden, wt, j == 0, stream)?;
        }
        // Shared always-on expert: a dense SwiGLU added on top, scaled by the
        // per-token sigmoid gate (`shared_scale`; 1.0 when the arch has no
        // shared-expert gate).
        if let Some(sh) = &moe.shared {
            // Shared expert down is [hidden, shared_inter], so cols = its width.
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            self.kernels.moe_scale_add_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                shared_scale,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Routed experts for a prefill chunk: route all `t` tokens at once, then
    /// apply each token's top-k experts, writing `[t, hidden]` into `pb.down`.
    /// Correctness-first per-token loop (grouped-GEMM permute/unpermute is a
    /// tracked perf follow-up); the router readback is one sync per layer.
    fn moe_prefill_ffn(
        &self,
        moe: &MoeFfn,
        layer: usize,
        t: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let pb = self.prefill_bufs.as_ref().expect("prefill bufs allocated");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;
        let DevWeight::F16 {
            buf: router_buf, ..
        } = &moe.router
        else {
            return Err(ForgeError::Unsupported("MoE router must be f16".into()));
        };
        self.kernels.moe_router_f16(
            &mb.ids,
            &mb.weights,
            &pb.x,
            router_buf,
            moe.usage.counts(),
            t,
            hidden,
            moe.n_experts,
            k,
            moe.norm_topk,
            stream,
        )?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, t * k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, t * k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                t * k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                t * k,
            )
        };
        // Prefill dotyka wielu tokenów, a te trafiają w mocno zachodzące zbiory
        // ekspertów. Ściągnięcie sumy całego kawałka jednym zgłoszeniem zamienia
        // `t` rund odczytu na jedną; gdy suma nie mieści się w slotach, zostaje
        // stronicowanie per token — wtedy i tak trzeba by ich wypierać nawzajem.
        let chunk_union = self.chunk_expert_union(ids);
        let union_fits = self.expert_union_fits(moe, chunk_union.len());
        if union_fits {
            self.fault_in_experts(moe, layer, &chunk_union)?;
        }
        for ti in 0..t {
            // Copy this token's normed hidden into a contiguous scratch row so
            // the single-token expert GEMVs read from offset 0.
            self.device
                .copy(&pb.x, ti * hidden * 2, &mb.xrow, 0, hidden * 2, stream)?;
            if !union_fits {
                // Stronicowanie nadpisuje przypiętą pamięć slotu z hosta, a
                // eksperci poprzednich tokenów mogą być jeszcze czytani przez
                // kernele w locie — bez tej bariery byłby to wyścig o wagi.
                stream.synchronize()?;
                self.fault_in_experts(moe, layer, &ids[ti * k..(ti + 1) * k])?;
            }
            self.moe_experts_accumulate(
                moe,
                &mb.xrow,
                &pb.down,
                ti * hidden * 2,
                inter,
                hidden,
                &ids[ti * k..(ti + 1) * k],
                &weights[ti * k..(ti + 1) * k],
                1.0,
                stream,
            )?;
        }
        Ok(())
    }

    /// Whether this is the hybrid attention/Gated-DeltaNet MoE arch (qwen35moe).
    pub fn is_hybrid(&self) -> bool {
        self.weights.descriptor.params.ssm.is_some()
    }

    fn hybrid_prefill_contains_nvfp4(&self) -> bool {
        self.weights.layers.iter().any(|layer| {
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return false;
            };
            let gate_up = match &ffn.gate_up {
                GateUpWeights::Fused(weight) => matches!(weight, DevWeight::NvFp4Gguf { .. }),
                GateUpWeights::Split { gate, up } => {
                    matches!(gate, DevWeight::NvFp4Gguf { .. })
                        || matches!(up, DevWeight::NvFp4Gguf { .. })
                }
            };
            gate_up || matches!(ffn.down, DevWeight::NvFp4Gguf { .. })
        })
    }

    fn hybrid_prefill_scratch_shape(&self) -> Option<HybridPrefillScratchShape> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref()?;
        Some(HybridPrefillScratchShape {
            hidden: p.hidden_size,
            q_dim: p.n_heads.checked_mul(p.head_dim)?,
            kv_dim: p.n_kv_heads.checked_mul(p.head_dim)?,
            inter: p.intermediate_size,
            conv_dim: ssm.conv_dim(),
            value_dim: ssm.value_dim(),
            n_v_heads: ssm.n_v_heads(),
            d_state: ssm.d_state,
            d_conv: ssm.d_conv,
            delta_layers: self
                .weights
                .layers
                .iter()
                .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
                .count(),
            max_pages_per_seq: self.max_pages_per_seq,
        })
    }

    fn hybrid_prefill_extended_structural_capable(&self) -> bool {
        let caps = self.device.caps();
        hybrid_prefill_t128_backend_capable(caps.vendor, caps.warp_size)
            && caps.max_threads_per_block >= 512
            && self.weights.descriptor.arch == "qwen35"
            && self.weights.token_embd_host.is_some()
            && self.validate_hybrid_speculation_target().is_ok()
            && self
                .weights
                .descriptor
                .params
                .ssm
                .as_ref()
                .is_some_and(|ssm| ssm.d_state == 128)
            && self.weights.layers.iter().all(|layer| {
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return false;
                };
                let gate_up = match &ffn.gate_up {
                    GateUpWeights::Fused(weight) => {
                        matches!(weight, DevWeight::NvFp4Gguf { .. })
                    }
                    GateUpWeights::Split { gate, up } => {
                        matches!(gate, DevWeight::NvFp4Gguf { .. })
                            && matches!(up, DevWeight::NvFp4Gguf { .. })
                    }
                };
                gate_up && matches!(ffn.down, DevWeight::NvFp4Gguf { .. })
            })
    }

    fn hybrid_prefill_t128_structural_capable(&self) -> bool {
        self.hybrid_prefill_extended_structural_capable()
            && self.kernels.hybrid_prefill_t128_artifacts_capable()
    }

    /// Wariant uwagi dla layer-major, z zejściem na `Exact` gdy backend nie ma
    /// flash-attention.
    ///
    /// Domyślne `auto` wybiera Mojo FA HD256, ale ta rodzina stoi na `mma` i
    /// istnieje wyłącznie na NVIDII. Bez tego zejścia cała ścieżka layer-major
    /// przewracała się na `kernel not loaded` dopiero przy pierwszym żądaniu.
    /// Jawne `FORGE_HYBRID_LAYER_MAJOR_ATTN=fa` nadal jest błędem, jeśli
    /// artefaktu nie ma — prośba o konkretny wariant ma nie schodzić po cichu.
    fn hybrid_layer_major_attention_backend(&self) -> Result<HybridLayerMajorAttention> {
        let requested = hybrid_layer_major_attention()?;
        if requested == HybridLayerMajorAttention::Flash
            && std::env::var("FORGE_HYBRID_LAYER_MAJOR_ATTN").is_err()
            && !self.kernels.has_artifact("attn_prefill_fa_mojo_f16_hd256")
        {
            return Ok(HybridLayerMajorAttention::Exact);
        }
        Ok(requested)
    }

    fn hybrid_layer_major_route_capable(&self) -> bool {
        hybrid_layer_major_prefill_requested()
            && self.hybrid_prefill_t128_structural_capable()
            && self.hybrid_layer_major_attention_backend().is_ok()
            && hybrid_layer_major_persistent_scan_requested().is_ok()
    }

    fn hybrid_prefill_extended_budget_capable(&self, chunk: usize) -> bool {
        let Some(shape) = self.hybrid_prefill_scratch_shape() else {
            return false;
        };
        let Ok(estimate) = hybrid_prefill_scratch_estimate(shape, chunk) else {
            return false;
        };
        hybrid_prefill_activation_budget_capable(
            estimate,
            self.device.pool_available(Pool::Activations),
        )
    }

    fn resolve_hybrid_prefill_chunk_size(&self, config: HybridPrefillChunkConfig) -> Result<usize> {
        if !self.is_hybrid() {
            return Ok(HYBRID_PREFILL_PORTABLE_CHUNK);
        }
        let caps = self.device.caps();
        let nvfp4_chunk_limit = hybrid_prefill_nvfp4_chunk_limit(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
        );
        let artifact_chunk_limit = self.kernels.hybrid_prefill_nvfp4_artifact_chunk_limit();
        let extended_capable = self.hybrid_prefill_extended_structural_capable();
        let executable_chunk_limit = nvfp4_chunk_limit.min(artifact_chunk_limit);
        let supported_limit = executable_chunk_limit.min(HYBRID_PREFILL_AUTO_CHUNK);
        let budget_chunk_limit = if extended_capable && supported_limit > 16 {
            [128, 32]
                .into_iter()
                .find(|&chunk| {
                    chunk <= supported_limit && self.hybrid_prefill_extended_budget_capable(chunk)
                })
                .unwrap_or(HYBRID_PREFILL_PORTABLE_CHUNK)
        } else {
            supported_limit.min(HYBRID_PREFILL_PORTABLE_CHUNK)
        };
        let auto_chunk_limit = supported_limit.min(budget_chunk_limit);
        resolve_hybrid_prefill_chunk_size(
            config,
            extended_capable,
            self.hybrid_prefill_t128_structural_capable()
                && auto_chunk_limit >= HYBRID_PREFILL_AUTO_CHUNK,
            self.hybrid_prefill_contains_nvfp4(),
            auto_chunk_limit,
            executable_chunk_limit,
            self.kernels.prepared_q8_tiled_capable(),
        )
    }

    fn ensure_hybrid_prefill_capacity(&mut self, cap: usize) -> Result<()> {
        self.ensure_prefill_bufs()?;
        self.ensure_hybrid_verify_bufs(4)?;
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        let result = self.ensure_hybrid_verify_bufs(cap.max(4));
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        result
    }

    fn ensure_hybrid_layer_major_bufs(&mut self, cap: usize) -> Result<()> {
        if self
            .hybrid_layer_major_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= cap)
        {
            return Ok(());
        }
        if !self.hybrid_prefill_extended_structural_capable() {
            return Err(ForgeError::Unsupported(
                "arena layer-major wymaga zweryfikowanego targetu hybrydowego NVIDIA".into(),
            ));
        }
        let shape = self.hybrid_prefill_scratch_shape().ok_or_else(|| {
            ForgeError::Unsupported("arena layer-major wymaga parametrów SSM".into())
        })?;
        let device_bytes = hybrid_layer_major_scratch_estimate(shape, cap)?;
        let required = device_bytes
            .checked_add(HYBRID_PREFILL_ACTIVATION_RESERVE)
            .ok_or_else(|| ForgeError::Scheduler("przepełnienie budżetu layer-major".into()))?;
        let available = self
            .device
            .pool_available(Pool::Activations)
            .ok_or_else(|| {
                ForgeError::Unsupported("backend nie raportuje budżetu areny layer-major".into())
            })?;
        let reclaimable = self
            .hybrid_layer_major_bufs
            .as_ref()
            .map_or(0, |bufs| bufs.device_bytes);
        let effective_available = available.checked_add(reclaimable).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie dostępnego budżetu layer-major".into())
        })?;
        if required > effective_available {
            return Err(ForgeError::Unsupported(format!(
                "arena layer-major wymaga {required} bajtów, dostępne {effective_available}"
            )));
        }
        drop(self.hybrid_layer_major_bufs.take());

        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let pinned = |name: &str, dims: &[usize], element_bytes: usize| {
            alloc_checked(
                device.as_ref(),
                name,
                dims,
                element_bytes,
                MemKind::PinnedHost,
            )
        };
        let shared_projection_cols = shape
            .q_dim
            .checked_mul(2)
            .map(|q| q.max(shape.conv_dim))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie projekcji areny layer-major".into())
            })?;
        let wide = shape.q_dim.max(shape.value_dim);
        if shape.inter < shared_projection_cols || wide < shape.hidden || wide < shape.kv_dim {
            return Err(ForgeError::Unsupported(
                "kształt areny layer-major nie pozwala współdzielić buforów fazowych".into(),
            ));
        }
        let conv_elems = shape
            .conv_dim
            .checked_mul(shape.d_conv - 1)
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie okna conv areny layer-major".into())
            })?;
        let h = a16("layer-major h", &[cap, shape.hidden])?;
        let x = a16("layer-major x", &[cap, shape.hidden])?;
        let v = a16("layer-major v", &[cap, shape.kv_dim])?;
        let gatec = a16("layer-major gatec i mixer", &[cap, wide])?;
        let gated = a16("layer-major gated i k", &[cap, wide])?;
        let z = a16("layer-major z", &[cap, shape.value_dim])?;
        let alpha = a16("layer-major alpha", &[cap, shape.n_v_heads])?;
        let beta_raw = a16("layer-major beta raw", &[cap, shape.n_v_heads])?;
        let g = a32("layer-major g", &[cap, shape.n_v_heads])?;
        let beta = a32("layer-major beta", &[cap, shape.n_v_heads])?;
        let o = a16("layer-major o i normed", &[cap, shape.value_dim])?;
        let gate = a16("layer-major q full, gate i act", &[cap, shape.inter])?;
        let up = a16("layer-major qc i up", &[cap, shape.inter])?;
        let q_full = gate.clone();
        let qc = up.clone();
        let k = gated.clone();
        let mixer_out = gatec.clone();
        let conv_initial = a16("layer-major conv initial", &[conv_elems])?;
        let conv_final = a16("layer-major conv final", &[conv_elems])?;
        let host_staging = (0..HYBRID_HOST_STAGING_SLOTS)
            .map(|_| {
                Ok(HybridLayerMajorHostStaging {
                    embedding: pinned("layer-major pinned embedding", &[128, shape.hidden], 2)?,
                    page_table: pinned(
                        "layer-major pinned page table",
                        &[shape.max_pages_per_seq],
                        4,
                    )?,
                    ids: pinned("layer-major pinned ids", &[128], 4)?,
                    positions: pinned("layer-major pinned positions", &[128], 4)?,
                    visible_lens: pinned("layer-major pinned visible lengths", &[128], 4)?,
                    base_pos: pinned("layer-major pinned base position", &[1], 4)?,
                    seq_len: pinned("layer-major pinned sequence length", &[1], 4)?,
                    position: pinned("layer-major pinned position", &[1], 4)?,
                    ready: device.create_event()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_layer_major_bufs = Some(HybridLayerMajorBufs {
            cap,
            device_bytes,
            h,
            x,
            k,
            v,
            q_full,
            qc,
            gatec,
            gated,
            z,
            alpha,
            beta_raw,
            g,
            beta,
            o,
            gate,
            up,
            mixer_out,
            conv_initial,
            conv_final,
            ids: a32("layer-major ids", &[cap])?,
            positions: a32("layer-major positions", &[cap])?,
            visible_lens: a32("layer-major visible lengths", &[cap])?,
            base_pos: a32("layer-major base position", &[1])?,
            host_staging,
        });
        Ok(())
    }

    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_layer_major_arena_bytes(&mut self, cap: usize) -> Result<usize> {
        self.ensure_hybrid_layer_major_bufs(cap)?;
        Ok(self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major została zaalokowana")
            .device_bytes)
    }

    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_layer_major_rollback(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        fail_after_layer: Option<usize>,
        fail_mtp_catchup: bool,
        fail_after_mtp_commit: bool,
    ) -> Result<Vec<f32>> {
        self.activate_hybrid_sequence(seq)?;
        self.prefill_hybrid_layer_major_inner(
            seq,
            tokens,
            fail_after_layer,
            fail_mtp_catchup,
            fail_after_mtp_commit,
        )
    }

    fn hybrid_batch_weights_capable(&self) -> bool {
        fn full_rows(weight: &DevWeight) -> bool {
            matches!(
                weight,
                DevWeight::F16 { .. }
                    | DevWeight::Q8_0 { .. }
                    | DevWeight::Q4K { .. }
                    | DevWeight::Q6K { .. }
                    | DevWeight::Q5K { .. }
                    | DevWeight::Q3K { .. }
                    | DevWeight::Q2K { .. }
                    | DevWeight::Q4_0 { .. }
                    | DevWeight::Q4_1 { .. }
                    | DevWeight::Q5_0 { .. }
                    | DevWeight::Q5_1 { .. }
                    | DevWeight::Iq4Nl { .. }
                    | DevWeight::Iq4Xs { .. }
                    | DevWeight::Mxfp4 { .. }
                    | DevWeight::Iq2Xs { .. }
                    | DevWeight::Iq2S { .. }
                    | DevWeight::Iq3S { .. }
                    | DevWeight::Iq2Xxs { .. }
                    | DevWeight::Iq3Xxs { .. }
                    | DevWeight::Iq1S { .. }
                    | DevWeight::Iq1M { .. }
                    | DevWeight::NvFp4 {
                        storage: NvFp4CtStorage::RowMajorE4M3 { .. },
                        ..
                    }
                    | DevWeight::NvFp4Gguf { .. }
            )
        }

        fn window_rows(weight: &DevWeight) -> bool {
            full_rows(weight) && !matches!(weight, DevWeight::NvFp4Gguf { .. })
        }

        self.is_hybrid()
            && self.tier.is_none()
            && matches!(
                self.weights.lm_head,
                DevWeight::F16 { .. } | DevWeight::Q8_0 { .. } | DevWeight::NvFp4Gguf { .. }
            )
            && self.weights.layers.iter().all(|layer| {
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return false;
                };
                let gate_up = match &ffn.gate_up {
                    GateUpWeights::Fused(weight) => window_rows(weight),
                    GateUpWeights::Split { gate, up } => full_rows(gate) && full_rows(up),
                };
                gate_up && full_rows(&ffn.down)
            })
    }

    /// Sprawdza pełny, niemutujący kontrakt batchowanego targetu hybrydowego.
    pub fn hybrid_batch_capable(&self) -> bool {
        self.weights.token_embd_host.is_some() && self.hybrid_batch_weights_capable()
    }

    /// Sprawdza semantyczny kontrakt kerneli eksperymentalnego prefill B2 T32.
    pub fn hybrid_prefill_b2_capable(&self, chunk_tokens: usize) -> bool {
        let Some(ssm) = self.weights.descriptor.params.ssm.as_ref() else {
            return false;
        };
        let device_embedding = self.weights.mtp.as_ref().is_some_and(|mtp| {
            mtp.shares_target_embedding
                && matches!(
                    mtp.embedding,
                    MtpEmbedding::Device(DevWeight::F16 { .. })
                        | MtpEmbedding::Device(DevWeight::Q8_0 { .. })
                        | MtpEmbedding::Device(DevWeight::NvFp4Gguf { .. })
                )
        });
        let split_layout = self.weights.layers.iter().all(|layer| {
            let attention_ok = match &layer.mixer {
                // DeepSeek V4 ma własną ścieżkę; hybrydowe zdolności go nie dotyczą.
                LayerMixer::DeepseekAttention(_) => false,
                LayerMixer::Attention(attention) => {
                    matches!(attention.attn_qkv, QkvWeights::Split { .. })
                }
                LayerMixer::DeltaNet(_) => true,
            };
            let ffn_ok = match &layer.ffn {
                LayerFfn::Dense(ffn) => matches!(ffn.gate_up, GateUpWeights::Split { .. }),
                LayerFfn::Moe(_) => false,
            };
            attention_ok && ffn_ok
        });
        hybrid_prefill_b2_backend_capable(self.device.caps().vendor, self.device.caps().warp_size)
            && self.kernels.hybrid_prefill_b2_artifacts_capable()
            && chunk_tokens == 32
            && self.weights.descriptor.params.head_dim == 256
            && ssm.d_state == 128
            && ssm.d_conv > 0
            && !self.weights.is_moe()
            && matches!(self.kv.cfg.quant, KvQuant::F16)
            && self.tier.is_none()
            && self.prefix_cache.is_none()
            && device_embedding
            && split_layout
            && self.hybrid_batch_weights_capable()
    }

    pub fn hybrid_layer_major_prefill_limit(&self) -> Option<usize> {
        if !self.hybrid_layer_major_route_capable() {
            return None;
        }
        let shape = self.hybrid_prefill_scratch_shape()?;
        let available = self.device.pool_available(Pool::Activations)?.checked_add(
            self.hybrid_layer_major_bufs
                .as_ref()
                .map_or(0, |bufs| bufs.device_bytes),
        )?;
        let budget_limit = [4096, 2048, 1024, 512, 128, 32]
            .into_iter()
            .find(|&tokens| {
                hybrid_layer_major_scratch_estimate(shape, tokens)
                    .ok()
                    .and_then(|bytes| bytes.checked_add(HYBRID_PREFILL_ACTIVATION_RESERVE))
                    .is_some_and(|required| required <= available)
            });
        budget_limit
            .into_iter()
            .chain(self.hybrid_layer_major_bufs.as_ref().map(|bufs| bufs.cap))
            .max()
    }

    /// Wykonuje atomowy target-only prefill dwóch segmentów T32 bez catch-up MTP.
    pub fn hybrid_prefill_b2_t32(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<[Vec<f32>; 2]> {
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, None, true)
    }

    /// Wykonuje target prefill B2, pozostawiając oba wiersze logits na urządzeniu.
    pub fn hybrid_prefill_b2_t32_device(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<()> {
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, None, false)
            .map(drop)
    }

    /// Mierzy zdarzeniami urządzenia pojedynczy serialny chunk prefill.
    #[doc(hidden)]
    pub fn debug_prefill_chunk_gpu_ms(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
    ) -> Result<(Vec<f32>, f32)> {
        let start = self.device.create_timing_event()?;
        let end = self.device.create_timing_event()?;
        self.device.record_event(&start, &self.stream)?;
        let logits = self.prefill_chunk(seq, tokens)?;
        self.device.record_event(&end, &self.stream)?;
        self.device.synchronize()?;
        let elapsed = self.device.elapsed_event_ms(&start, &end)?.ok_or_else(|| {
            ForgeError::Unsupported("urządzenie nie obsługuje zdarzeń czasowych".into())
        })?;
        Ok((logits, elapsed))
    }

    /// Mierzy zdarzeniami urządzenia bezpośredni prefill B2 T32.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_t32_gpu_ms(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
    ) -> Result<([Vec<f32>; 2], f32)> {
        let start = self.device.create_timing_event()?;
        let end = self.device.create_timing_event()?;
        self.device.record_event(&start, &self.stream)?;
        let logits = self.hybrid_prefill_b2_t32(seqs, tokens)?;
        self.device.record_event(&end, &self.stream)?;
        self.device.synchronize()?;
        let elapsed = self.device.elapsed_event_ms(&start, &end)?.ok_or_else(|| {
            ForgeError::Unsupported("urządzenie nie obsługuje zdarzeń czasowych".into())
        })?;
        Ok((logits, elapsed))
    }

    /// Zwraca sumę logicznych rozmiarów dedykowanego scratchu prefill B2.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_scratch_bytes(&self) -> usize {
        let Some(bufs) = self.hybrid_prefill_b2_bufs.as_ref() else {
            return 0;
        };
        let fixed = [
            &bufs.h,
            &bufs.x,
            &bufs.k,
            &bufs.v,
            &bufs.attn_out,
            &bufs.o_out,
            &bufs.gate,
            &bufs.up,
            &bufs.act,
            &bufs.down,
            &bufs.ids,
            &bufs.positions,
            &bufs.q_full,
            &bufs.qc,
            &bufs.gatec,
            &bufs.gated,
            &bufs.qkv_mixed,
            &bufs.z,
            &bufs.alpha,
            &bufs.beta_raw,
            &bufs.o,
            &bufs.normed,
            &bufs.page_tables,
            &bufs.base_positions,
            &bufs.visible_lens,
            &bufs.decisions,
            &bufs.final_hidden,
            &bufs.logits,
            &bufs.pinned_metadata,
            &bufs.pinned_logits,
            &bufs.final_conv,
            &bufs.final_states,
        ]
        .into_iter()
        .map(DevBuffer::len)
        .sum::<usize>();
        fixed
            + bufs
                .delta
                .iter()
                .flatten()
                .map(|cache| {
                    cache.conv_initial.len()
                        + cache.state_initial.len()
                        + cache.q.len()
                        + cache.k.len()
                        + cache.v.len()
                        + cache.g.len()
                        + cache.beta.len()
                })
                .sum::<usize>()
    }

    /// Sprawdza osobny bufor `z` i potrójną projekcję scratchu prefill.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn debug_hybrid_prefill_triplet_contract(&mut self, cap: usize) -> Result<()> {
        if !matches!(cap, 32 | 128) {
            return Err(ForgeError::Scheduler(
                "test kontraktu triplet wymaga cap 32 lub 128".into(),
            ));
        }
        self.ensure_hybrid_prefill_capacity(cap)?;
        let (gate_w, gate_rows, alpha_w, alpha_rows, beta_w, beta_rows, cols) = self
            .weights
            .layers
            .iter()
            .find_map(|layer| {
                let LayerMixer::DeltaNet(delta) = &layer.mixer else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: gate,
                    rows: gate_rows,
                    cols,
                } = &delta.gate_proj
                else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: alpha,
                    rows: alpha_rows,
                    cols: alpha_cols,
                } = &delta.alpha_proj
                else {
                    return None;
                };
                let DevWeight::Q8_0 {
                    buf: beta,
                    rows: beta_rows,
                    cols: beta_cols,
                } = &delta.beta_proj
                else {
                    return None;
                };
                (*alpha_cols == *cols && *beta_cols == *cols).then(|| {
                    (
                        gate.clone(),
                        *gate_rows,
                        alpha.clone(),
                        *alpha_rows,
                        beta.clone(),
                        *beta_rows,
                        *cols,
                    )
                })
            })
            .ok_or_else(|| ForgeError::Unsupported("brak grupy DeltaNet Q8_0".into()))?;
        let pb_x = self
            .prefill_bufs
            .as_ref()
            .expect("bufory prefill są gotowe")
            .x
            .clone();
        let hv = self
            .hybrid_prefill_bufs
            .as_ref()
            .expect("scratch prefill hybrid jest gotowy");
        let qkv = hv.qkv_mixed.clone();
        let z = hv.z.clone();
        let alpha = hv.alpha.clone();
        let beta = hv.beta_raw.clone();
        if qkv.device_ptr() == z.device_ptr() {
            return Err(ForgeError::Scheduler(
                "scratch cap>4 aliasuje z z wejściem mixed qkv".into(),
            ));
        }
        let input_bytes = checked_scratch_bytes("test input triplet", &[cap, cols], 2)?;
        let host_x = (0..input_bytes / 2)
            .map(|index| f16::from_f32((index as f32 % 31.0 - 15.0) / 8.0))
            .collect::<Vec<_>>();
        self.device
            .write(bytemuck::cast_slice::<f16, u8>(&host_x), &pb_x, 0)?;
        let qkv_pattern = (0..qkv.len())
            .map(|index| ((index * 37 + 11) & 0xff) as u8)
            .collect::<Vec<_>>();
        self.device.write(&qkv_pattern, &qkv, 0)?;
        let baseline_z = self.device.alloc(
            checked_scratch_bytes("test baseline z", &[cap, gate_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let baseline_alpha = self.device.alloc(
            checked_scratch_bytes("test baseline alpha", &[cap, alpha_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let baseline_beta = self.device.alloc(
            checked_scratch_bytes("test baseline beta", &[cap, beta_rows], 2)?,
            MemKind::Device,
            Pool::Activations,
        )?;
        let mut baseline = self.kernels.prepare_q8_1(&pb_x, cols, cap, &self.stream)?;
        for (output, weights, rows) in [
            (&baseline_z, &gate_w, gate_rows),
            (&baseline_alpha, &alpha_w, alpha_rows),
            (&baseline_beta, &beta_w, beta_rows),
        ] {
            self.kernels.gemm_q8_0_i8mma_prepared_at(
                output,
                weights,
                0,
                &mut baseline,
                rows,
                cols,
                cap,
            )?;
        }
        drop(baseline);
        let mut fused = self.kernels.prepare_q8_1(&pb_x, cols, cap, &self.stream)?;
        self.kernels.gemm_q8_0_i8mma_prepared_triplet(
            &[
                Q8PreparedProjection {
                    output: &z,
                    weights: &gate_w,
                    weight_byte_offset: 0,
                    rows: gate_rows,
                },
                Q8PreparedProjection {
                    output: &alpha,
                    weights: &alpha_w,
                    weight_byte_offset: 0,
                    rows: alpha_rows,
                },
                Q8PreparedProjection {
                    output: &beta,
                    weights: &beta_w,
                    weight_byte_offset: 0,
                    rows: beta_rows,
                },
            ],
            &mut fused,
            cols,
            cap,
        )?;
        drop(fused);
        self.device.synchronize()?;
        let mut qkv_after = vec![0u8; qkv.len()];
        self.device.read(&qkv, 0, &mut qkv_after)?;
        if qkv_after != qkv_pattern {
            return Err(ForgeError::Kernel(
                "triplet nadpisał wejście mixed qkv przed deltanet_prepare".into(),
            ));
        }
        for (name, actual, expected) in [
            ("z", &z, &baseline_z),
            ("alpha", &alpha, &baseline_alpha),
            ("beta", &beta, &baseline_beta),
        ] {
            let mut actual_bytes = vec![0u8; expected.len()];
            let mut expected_bytes = vec![0u8; expected.len()];
            self.device.read(actual, 0, &mut actual_bytes)?;
            self.device.read(expected, 0, &mut expected_bytes)?;
            if actual_bytes != expected_bytes {
                return Err(ForgeError::Kernel(format!(
                    "fused triplet różni się bitowo dla projekcji {name}"
                )));
            }
        }
        Ok(())
    }

    /// Odtwarza stany MTP po target-only prefill B2 macierzowo, lane po lane.
    pub fn hybrid_prefill_mtp_catchup_b2(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
    ) -> Result<()> {
        self.hybrid_prefill_mtp_catchup_b2_inner(seqs, tokens, reset, None, None)
    }

    /// Wymusza błąd po wykonaniu wskazanego lane transakcji catch-up MTP.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_mtp_catchup_b2_rollback(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        failed_lane: usize,
    ) -> Result<()> {
        if failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test rollbacku catch-up MTP wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_mtp_catchup_b2_inner(seqs, tokens, reset, Some(failed_lane), None)
    }

    /// Wymusza błąd lane oraz następującego po nim rollbacku pary MTP.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_mtp_catchup_b2_rollback_failure(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        failed_lane: usize,
        rollback_failed_lane: usize,
    ) -> Result<()> {
        if failed_lane >= 2 || rollback_failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test błędu rollbacku catch-up MTP wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_mtp_catchup_b2_inner(
            seqs,
            tokens,
            reset,
            Some(failed_lane),
            Some(rollback_failed_lane),
        )
    }

    fn hybrid_prefill_mtp_catchup_b2_inner(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        reset: [bool; 2],
        fail_after_lane: Option<usize>,
        fail_rollback_lane: Option<usize>,
    ) -> Result<()> {
        if let Some(pool) = self.hybrid_states.as_ref() {
            pool.ensure_healthy()?;
        }
        if !self.has_native_mtp() {
            return Ok(());
        }
        const STEPS: usize = 32;
        if tokens.iter().any(|lane| lane.len() != STEPS) {
            return Err(ForgeError::Scheduler(
                "catch-up MTP B2 wymaga dwóch segmentów T32".into(),
            ));
        }
        self.ensure_hybrid_bufs()?;
        self.ensure_prefill_bufs()?;
        self.ensure_hybrid_verify_bufs(4)?;
        std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
        let saved_graphs = (
            std::mem::take(&mut self.hybrid_verify_graphs),
            std::mem::take(&mut self.hybrid_verify_graph_disabled),
        );
        let result = (|| {
            self.ensure_hybrid_verify_bufs(STEPS)?;
            let hidden = self.weights.descriptor.params.hidden_size;
            let hidden_bytes = hidden * 2;
            let direct_x = self
                .hybrid_prefill_b2_bufs
                .as_ref()
                .expect("target B2 przygotował scratch")
                .x
                .clone();
            for (lane, lane_tokens) in tokens.iter().enumerate() {
                let staging = &self
                    .hybrid_verify_bufs
                    .as_ref()
                    .expect("catch-up MTP ma scratch")
                    .host_staging[lane]
                    .embedding;
                let destination = staging
                    .host_ptr()
                    .expect("embedding catch-up ma mapowanie hosta");
                {
                    let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                        ForgeError::Unsupported(
                            "catch-up MTP B2 wymaga hostowego embeddingu".into(),
                        )
                    })?;
                    for (row, &token) in lane_tokens.iter().enumerate() {
                        let source = table
                            .get(token as usize * hidden..(token as usize + 1) * hidden)
                            .ok_or_else(|| {
                                ForgeError::Scheduler(format!(
                                    "token id {token} wykracza poza embedding catch-up"
                                ))
                            })?;
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                source.as_ptr() as *const u8,
                                destination.add(row * hidden_bytes),
                                hidden_bytes,
                            );
                        }
                    }
                }
            }
            let (leases, mut states, mut mtp_kv) = self.take_mtp_runtime_pair(seqs)?;
            let mut pair_result = (|| {
                states[0].checkpoint(&self.stream)?;
                states[1].checkpoint(&self.stream)?;
                for (lane, state) in states.iter_mut().enumerate() {
                    if reset[lane] {
                        state.reset_pending(&mut mtp_kv, &self.stream)?;
                    }
                }
                for (lane, state) in states.iter_mut().enumerate() {
                    let prefill_x = self
                        .prefill_bufs
                        .as_ref()
                        .expect("catch-up MTP ma bufory prefill")
                        .x
                        .clone();
                    self.device.copy(
                        &direct_x,
                        lane * STEPS * hidden_bytes,
                        &prefill_x,
                        0,
                        STEPS * hidden_bytes,
                        &self.stream,
                    )?;
                    self.profile_catchup_start()?;
                    let execution = self.mtp_catchup_verified_prefix_pending(
                        state,
                        &mut mtp_kv,
                        STEPS,
                        lane,
                        None,
                    );
                    let profile_end = self.profile_catchup_end();
                    execution?;
                    profile_end?;
                    if fail_after_lane == Some(lane) {
                        return Err(ForgeError::Scheduler(format!(
                            "wymuszony błąd catch-up MTP po lane {lane}"
                        )));
                    }
                }
                states[0].validate_commit_catchup(STEPS)?;
                states[1].validate_commit_catchup(STEPS)?;
                self.device.synchronize()?;
                states[0].apply_commit_catchup();
                states[1].apply_commit_catchup();
                Ok(())
            })();
            if pair_result.is_err() {
                let rollback = rollback_mtp_pair_inner(
                    &mut states,
                    &mut mtp_kv,
                    &self.stream,
                    fail_rollback_lane,
                )
                .and_then(|_| self.device.synchronize());
                if let Err(rollback) = rollback {
                    let execution = pair_result.expect_err("catch-up pary zawiera błąd");
                    pair_result = Err(self.poison_mtp_runtime(format!(
                        "błąd catch-up MTP B2: {execution}; rollback pary nie powiódł się: {rollback}"
                    )));
                }
            }
            self.finish_mtp_runtime_pair(leases, states, mtp_kv, pair_result)
        })();
        restore_after(result, || {
            std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
            self.hybrid_verify_graphs = saved_graphs.0;
            self.hybrid_verify_graph_disabled = saved_graphs.1;
        })
    }

    /// Próbkuje wskazany wiersz logits ostatniego prefill B2 na GPU.
    pub fn sample_hybrid_prefill_b2_logits(
        &mut self,
        lane: usize,
        sampler: &mut GpuSampler,
    ) -> Result<u32> {
        if lane >= 2 {
            return Err(ForgeError::Scheduler(
                "logity prefill B2 wymagają lane 0 lub 1".into(),
            ));
        }
        let logits = &self
            .hybrid_prefill_b2_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak logits prefill B2".into()))?
            .logits;
        let bytes = self.weights.descriptor.params.vocab_size * 4;
        self.device.copy(
            logits,
            lane * bytes,
            &self.bufs.logits,
            0,
            bytes,
            &self.stream,
        )?;
        self.sample_last_logits(sampler)
    }

    /// Próbkuje oba wiersze prefill B2 na GPU i odczytuje tylko dwa ID.
    pub fn sample_hybrid_prefill_b2_logits_batched(
        &mut self,
        samplers: &mut [&mut GpuSampler; 2],
    ) -> Result<[u32; 2]> {
        const BATCH: usize = 2;
        self.ensure_batch(BATCH)?;
        let vocab = self.weights.descriptor.params.vocab_size;
        let [first, second] = samplers;
        let params = [first.batch_params(vocab), second.batch_params(vocab)];
        let logits = self
            .hybrid_prefill_b2_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak logits prefill B2".into()))?
            .logits
            .clone();
        self.batch_sample_from(&logits, BATCH, &params)?;
        let batch = self.batch_bufs.as_ref().expect("batch sampler ma bufory");
        self.device.copy(
            &batch.out_ids,
            0,
            &batch.pinned_out,
            0,
            BATCH * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;
        let output = batch
            .pinned_out
            .host_ptr()
            .expect("pinned output ma mapowanie") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(output, BATCH) };
        let mut result = [0u32; BATCH];
        for lane in 0..BATCH {
            let id = ids[lane];
            if id < 0 || id as usize >= vocab {
                return Err(ForgeError::Kernel(format!(
                    "batch sampler prefill zwrócił token {id} poza słownikiem dla lane {lane}"
                )));
            }
            result[lane] = id as u32;
        }
        Ok(result)
    }

    /// Wymusza błąd po pierwszym zapisie stanu na potrzeby testu rollbacku.
    #[doc(hidden)]
    pub fn debug_hybrid_prefill_b2_t32_rollback(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        failed_lane: usize,
    ) -> Result<[Vec<f32>; 2]> {
        if failed_lane >= 2 {
            return Err(ForgeError::Scheduler(
                "test rollbacku prefill B2 wymaga lane 0 lub 1".into(),
            ));
        }
        self.hybrid_prefill_b2_t32_inner(seqs, tokens, Some(failed_lane), true)
    }

    fn hybrid_prefill_b2_t32_inner(
        &mut self,
        seqs: &mut [&mut SeqKv; 2],
        tokens: [&[u32]; 2],
        fail_after_state_commit: Option<usize>,
        read_host_logits: bool,
    ) -> Result<[Vec<f32>; 2]> {
        const BATCH: usize = 2;
        const STEPS: usize = 32;
        const TOTAL: usize = BATCH * STEPS;
        if tokens.iter().any(|lane| lane.len() != STEPS) {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 wymaga dokładnie 32 tokenów w każdym lane".into(),
            ));
        }
        if !self.hybrid_prefill_b2_capable(STEPS) {
            return Err(ForgeError::Unsupported(
                "model nie spełnia semantycznego kontraktu prefill B2 T32".into(),
            ));
        }
        if seqs[0].id == seqs[1].id {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 wymaga dwóch różnych sekwencji".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        if tokens
            .iter()
            .flat_map(|lane| lane.iter())
            .any(|&token| token as usize >= p.vocab_size)
        {
            return Err(ForgeError::Scheduler(
                "prefill B2 T32 otrzymał token poza słownikiem".into(),
            ));
        }
        let bases = [seqs[0].len, seqs[1].len];
        let ends = [
            bases[0].checked_add(STEPS).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji prefill B2 T32 lane0".into())
            })?,
            bases[1].checked_add(STEPS).ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie pozycji prefill B2 T32 lane1".into())
            })?,
        ];
        for end in ends {
            if end > p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    end - 1,
                    p.max_position_embeddings
                )));
            }
        }
        let required_pages = seqs
            .iter()
            .enumerate()
            .try_fold(0usize, |total, (lane, seq)| {
                let pages = ends[lane]
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                total.checked_add(pages).ok_or_else(|| {
                    ForgeError::Scheduler("przepełnienie stron prefill B2 T32".into())
                })
            })?;
        self.ensure_free_pages(required_pages);
        if required_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "prefill B2 T32 wymaga {required_pages} stron KV, dostępne {}",
                self.kv.free_page_count()
            )));
        }
        self.preflight_hybrid_state_slots(2)?;
        self.ensure_hybrid_prefill_b2_bufs()?;
        for seq in seqs.iter_mut() {
            self.activate_hybrid_sequence(seq)?;
        }
        let leases = [
            seqs[0].hybrid_state.expect("lane0 ma lease"),
            seqs[1].hybrid_state.expect("lane1 ma lease"),
        ];
        let ssm = p.ssm.as_ref().expect("prefill B2 ma parametry SSM");
        let q_elements = TOTAL * p.n_heads * p.head_dim;
        let q_norm_rows = TOTAL * p.n_heads;
        let kv_norm_rows = TOTAL * p.n_kv_heads;
        let delta_norm_rows = TOTAL * ssm.n_v_heads();
        let ffn_rows = TOTAL * p.intermediate_size;
        let state_bytes = ssm.n_v_heads() * ssm.d_state * ssm.d_state * 4;
        let mut snapshot_ready = false;
        let mut work_enqueued = false;
        let result = (|| {
            for seq in seqs.iter_mut() {
                for _ in 0..STEPS {
                    self.kv.grow(seq)?;
                }
            }
            let page_table_elems = BATCH * self.max_pages_per_seq;
            let mut metadata = Vec::with_capacity(3 * TOTAL + BATCH + page_table_elems + BATCH * 2);
            metadata.extend(
                tokens[0]
                    .iter()
                    .chain(tokens[1].iter())
                    .map(|&token| token as i32),
            );
            for lane in 0..BATCH {
                metadata.extend((bases[lane]..ends[lane]).map(|position| position as i32));
            }
            for lane in 0..BATCH {
                metadata.extend((bases[lane] + 1..=ends[lane]).map(|visible| visible as i32));
            }
            metadata.extend(bases.map(|base| base as i32));
            let page_table_offset = metadata.len();
            for seq in seqs.iter() {
                metadata.extend(seq.pages.iter().copied());
                metadata.resize(
                    metadata.len() + self.max_pages_per_seq - seq.pages.len(),
                    -1,
                );
            }
            metadata.extend([STEPS as i32, 0, STEPS as i32, 0]);
            let b2 = self
                .hybrid_prefill_b2_bufs
                .as_ref()
                .expect("scratch prefill B2 jest gotowy");
            write_pinned(bytemuck::cast_slice(&metadata), &b2.pinned_metadata)?;
            work_enqueued = true;
            let rows_bytes = TOTAL * 4;
            self.device
                .copy(&b2.pinned_metadata, 0, &b2.ids, 0, rows_bytes, &self.stream)?;
            self.device.copy(
                &b2.pinned_metadata,
                rows_bytes,
                &b2.positions,
                0,
                rows_bytes,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                2 * rows_bytes,
                &b2.visible_lens,
                0,
                rows_bytes,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                3 * rows_bytes,
                &b2.base_positions,
                0,
                BATCH * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                page_table_offset * 4,
                &b2.page_tables,
                0,
                page_table_elems * 4,
                &self.stream,
            )?;
            self.device.copy(
                &b2.pinned_metadata,
                (page_table_offset + page_table_elems) * 4,
                &b2.decisions,
                0,
                BATCH * 2 * 4,
                &self.stream,
            )?;

            let embedding = self
                .weights
                .mtp
                .as_ref()
                .and_then(|mtp| mtp.shares_target_embedding.then_some(&mtp.embedding))
                .expect("capability sprawdziło device embedding");
            match embedding {
                MtpEmbedding::Device(DevWeight::F16 { buf, rows, cols }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding F16 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::Q8_0 { buf, rows, cols }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding Q8_0 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_q8_0_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.vocab_size,
                        p.hidden_size,
                        &self.stream,
                    )?;
                }
                MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                    buf,
                    output_scale,
                    rows,
                    cols,
                    layout: Nvfp4GgufLayout::RowMajor36,
                }) => {
                    if (*rows, *cols) != (p.vocab_size, p.hidden_size) {
                        return Err(ForgeError::Format(
                            "embedding NVFP4 prefill B2 ma niezgodny kształt".into(),
                        ));
                    }
                    self.kernels.gather_nvfp4_gguf_rows_f16(
                        &b2.h,
                        buf,
                        &b2.ids,
                        TOTAL,
                        p.vocab_size,
                        p.hidden_size,
                        *output_scale,
                        &self.stream,
                    )?;
                }
                _ => unreachable!("capability ogranicza format embeddingu"),
            }

            for (layer_index, cache) in b2.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                for (lane, &lease) in leases.iter().enumerate() {
                    let (conv, state) = self
                        .hybrid_states
                        .as_ref()
                        .expect("model ma pulę hybrydową")
                        .state_buffers(lease, layer_index)?
                        .expect("warstwa DeltaNet ma stan");
                    self.device.copy(
                        &conv,
                        0,
                        &cache.conv_initial,
                        lane * conv.len(),
                        conv.len(),
                        &self.stream,
                    )?;
                    self.device.copy(
                        &state,
                        0,
                        &cache.state_initial,
                        lane * state.len(),
                        state.len(),
                        &self.stream,
                    )?;
                }
            }
            snapshot_ready = true;
            self.kernels.rmsnorm_f16(
                &b2.x,
                &b2.h,
                &self.weights.layers[0].attn_norm,
                TOTAL,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
                            unreachable!("capability wymaga rozdzielonych Q/K/V")
                        };
                        self.gemm(&b2.q_full, q, &b2.x, TOTAL, &self.stream)?;
                        self.kernels.deinterleave_gate_f16(
                            &b2.qc,
                            &b2.gatec,
                            &b2.q_full,
                            p.head_dim,
                            q_elements,
                            &self.stream,
                        )?;
                        if let Some(norm) = &attention.q_norm {
                            self.kernels.rmsnorm_f16(
                                &b2.qc,
                                &b2.qc,
                                norm,
                                q_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        self.gemm(&b2.k, k, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.v, v, &b2.x, TOTAL, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &b2.k,
                                &b2.k,
                                norm,
                                kv_norm_rows,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        let n_rot = self.hybrid_n_rot();
                        self.kernels.rope_neox_partial_f16(
                            &b2.qc,
                            &b2.positions,
                            TOTAL,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &b2.k,
                            &b2.positions,
                            TOTAL,
                            p.n_kv_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        let kv_layer = self.target_kv_layer(layer_index);
                        self.kernels.kv_append_batch_segmented_f16(
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.k,
                            &b2.v,
                            &b2.page_tables,
                            &b2.base_positions,
                            BATCH,
                            STEPS,
                            self.max_pages_per_seq,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        self.kernels.attn_verify_segmented_f16_hd256(
                            &b2.attn_out,
                            &b2.qc,
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &b2.page_tables,
                            &b2.visible_lens,
                            BATCH,
                            STEPS,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            1.0 / (p.head_dim as f32).sqrt(),
                            &self.stream,
                        )?;
                        self.kernels.sigmoid_mul_f16(
                            &b2.gated,
                            &b2.attn_out,
                            &b2.gatec,
                            q_elements,
                            &self.stream,
                        )?;
                        self.gemm(&b2.o_out, &attention.attn_o, &b2.gated, TOTAL, &self.stream)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let cache = b2.delta[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma scratch B2");
                        self.gemm(&b2.qkv_mixed, &delta.in_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.z, &delta.gate_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.alpha, &delta.alpha_proj, &b2.x, TOTAL, &self.stream)?;
                        self.gemm(&b2.beta_raw, &delta.beta_proj, &b2.x, TOTAL, &self.stream)?;
                        self.kernels.deltanet_prepare_segmented_final_f16(
                            &cache.q,
                            &cache.k,
                            &cache.v,
                            &cache.g,
                            &cache.beta,
                            &b2.final_conv,
                            &cache.conv_initial,
                            &b2.qkv_mixed,
                            &delta.conv1d,
                            &b2.alpha,
                            &b2.beta_raw,
                            &delta.dt_bias,
                            &delta.a,
                            BATCH,
                            STEPS,
                            ssm.n_k_heads(),
                            ssm.n_v_heads(),
                            ssm.d_state,
                            ssm.d_conv,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        match self.delta_state_layout() {
                            DeltaStateLayout::ValueKey => {
                                self.kernels.deltanet_value_key_scan_inplace_f16(
                                    &b2.o,
                                    &b2.final_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    BATCH,
                                    STEPS,
                                    ssm.n_v_heads(),
                                    &self.stream,
                                )?
                            }
                            DeltaStateLayout::KeyValue => {
                                self.kernels.deltanet_gated_scan_segmented_shared_d128_f16(
                                    &b2.o,
                                    &b2.final_states,
                                    &cache.state_initial,
                                    &cache.q,
                                    &cache.k,
                                    &cache.v,
                                    &cache.g,
                                    &cache.beta,
                                    BATCH,
                                    STEPS,
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?
                            }
                        }
                        for (lane, &lease) in leases.iter().enumerate() {
                            let (conv, state) = self
                                .hybrid_states
                                .as_ref()
                                .expect("model ma pulę hybrydową")
                                .state_buffers(lease, layer_index)?
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &b2.final_conv,
                                lane * conv.len(),
                                &conv,
                                0,
                                conv.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &b2.final_states,
                                lane * state_bytes,
                                &state,
                                0,
                                state.len(),
                                &self.stream,
                            )?;
                        }
                        if let Some(lane) = fail_after_state_commit {
                            return Err(ForgeError::Scheduler(format!(
                                "wymuszony błąd rollbacku prefill B2 lane {lane}"
                            )));
                        }
                        self.kernels.deltanet_gated_rmsnorm_f16(
                            &b2.normed,
                            &b2.o,
                            &b2.z,
                            &delta.ssm_norm,
                            delta_norm_rows,
                            ssm.d_state,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        self.gemm(&b2.o_out, &delta.out_proj, &b2.normed, TOTAL, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &b2.x,
                    &b2.h,
                    &b2.o_out,
                    &layer.ffn_norm,
                    TOTAL,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    unreachable!("capability odrzuca MoE")
                };
                let GateUpWeights::Split { gate, up } = &ffn.gate_up else {
                    unreachable!("capability wymaga rozdzielonych gate/up")
                };
                self.gemm(&b2.gate, gate, &b2.x, TOTAL, &self.stream)?;
                self.gemm(&b2.up, up, &b2.x, TOTAL, &self.stream)?;
                self.kernels
                    .glu_mul_f16(self.ffn_act(), &b2.act, &b2.gate, &b2.up, ffn_rows, &self.stream)?;
                self.gemm(&b2.down, &ffn.down, &b2.act, TOTAL, &self.stream)?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &b2.x,
                    &b2.h,
                    &b2.down,
                    next_norm,
                    TOTAL,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
            }
            self.kernels.mtp_select_row_segmented_f16(
                &b2.final_hidden,
                &b2.x,
                &b2.decisions,
                BATCH,
                STEPS,
                p.hidden_size,
                &self.stream,
            )?;
            self.logits_gemm(&b2.logits, &b2.final_hidden, BATCH, &self.stream)?;
            if read_host_logits {
                self.device.copy(
                    &b2.logits,
                    0,
                    &b2.pinned_logits,
                    0,
                    BATCH * p.vocab_size * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let logits = b2
                    .pinned_logits
                    .host_ptr()
                    .expect("logity B2 mają mapowanie") as *const f32;
                Ok(std::array::from_fn(|lane| unsafe {
                    std::slice::from_raw_parts(logits.add(lane * p.vocab_size), p.vocab_size)
                        .to_vec()
                }))
            } else {
                Ok(std::array::from_fn(|_| Vec::new()))
            }
        })();

        self.pt_seq = 0;
        if let Err(error) = result {
            for lane in 0..BATCH {
                self.kv.rollback(seqs[lane], bases[lane]);
            }
            if snapshot_ready {
                let b2 = self
                    .hybrid_prefill_b2_bufs
                    .as_ref()
                    .expect("scratch prefill B2 jest gotowy");
                let rollback = (|| {
                    for (layer_index, cache) in b2.delta.iter().enumerate() {
                        let Some(cache) = cache else { continue };
                        for (lane, &lease) in leases.iter().enumerate() {
                            let (conv, state) = self
                                .hybrid_states
                                .as_ref()
                                .expect("model ma pulę hybrydową")
                                .state_buffers(lease, layer_index)?
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.conv_initial,
                                lane * conv.len(),
                                &conv,
                                0,
                                conv.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.state_initial,
                                lane * state.len(),
                                &state,
                                0,
                                state.len(),
                                &self.stream,
                            )?;
                        }
                    }
                    self.device.synchronize()
                })();
                if let Err(rollback) = rollback {
                    return Err(self
                        .hybrid_states
                        .as_mut()
                        .expect("model ma pulę hybrydową")
                        .poison(format!(
                            "błąd prefill B2 T32: {error}; rollback stanów nie powiódł się: {rollback}"
                        )));
                }
            } else if work_enqueued {
                if let Err(sync) = self.device.synchronize() {
                    return Err(self
                        .hybrid_states
                        .as_mut()
                        .expect("model ma pulę hybrydową")
                        .poison(format!(
                            "błąd prefill B2 T32: {error}; synchronizacja rollbacku nie powiodła się: {sync}"
                        )));
                }
            }
            return Err(error);
        }
        result
    }

    /// Zrzuca stan hybrydowy do diagnostyki zgodności batch kontra serial.
    pub fn debug_hybrid_state_snapshot(&self) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.device.synchronize()?;
        let mut snapshot = Vec::new();
        for (name, buffer, element_bytes) in
            [("h", &self.bufs.h, 2usize), ("x", &self.bufs.x, 2usize)]
        {
            let mut bytes = vec![0u8; buffer.len()];
            self.device.read(buffer, 0, &mut bytes)?;
            snapshot.push((name.into(), element_bytes, bytes));
        }
        for (layer, state) in self.active_ssm().iter().enumerate() {
            let Some(state) = state else { continue };
            for (kind, buffer, element_bytes) in
                [("conv", &state.conv, 2usize), ("ssm", &state.state, 4usize)]
            {
                let mut bytes = vec![0u8; buffer.len()];
                self.device.read(buffer, 0, &mut bytes)?;
                snapshot.push((format!("layer.{layer}.{kind}"), element_bytes, bytes));
            }
        }
        Ok(snapshot)
    }

    /// Zrzuca logiczny KV i stany DeltaNet jednej sekwencji do testów parytetu.
    pub fn debug_hybrid_sequence_snapshot(
        &mut self,
        seq: &mut SeqKv,
    ) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.activate_hybrid_sequence(seq)?;
        self.device.synchronize()?;
        let lease = seq
            .hybrid_state
            .expect("aktywna sekwencja hybrydowa ma lease");
        let mut snapshot = Vec::new();
        for layer_index in 0..self.weights.layers.len() {
            if let Some((conv, state)) = self
                .hybrid_states
                .as_ref()
                .expect("model ma pulę hybrydową")
                .state_buffers(lease, layer_index)?
            {
                for (kind, buffer, element_bytes) in
                    [("conv", conv, 2usize), ("state", state, 4usize)]
                {
                    let mut data = vec![0u8; buffer.len()];
                    self.device.read(&buffer, 0, &mut data)?;
                    snapshot.push((format!("layer.{layer_index}.{kind}"), element_bytes, data));
                }
            }
        }
        let page_bytes = self.kv.cfg.n_kv_heads * self.kv.cfg.page_size * self.kv.cfg.head_dim * 2;
        for layer_index in 0..self.weights.layers.len() {
            let LayerMixer::Attention(_) = self.weights.layers[layer_index].mixer else {
                continue;
            };
            let kv_layer = self.target_kv_layer(layer_index);
            for (kind, slab) in [("k", &self.kv.k[kv_layer]), ("v", &self.kv.v[kv_layer])] {
                let mut data = vec![0u8; seq.pages.len() * page_bytes];
                for (logical, &physical) in seq.pages.iter().enumerate() {
                    if physical < 0 {
                        return Err(ForgeError::Scheduler(
                            "snapshot prefill B2 nie obsługuje stron spilled".into(),
                        ));
                    }
                    self.device.read(
                        slab,
                        physical as usize * page_bytes,
                        &mut data[logical * page_bytes..(logical + 1) * page_bytes],
                    )?;
                }
                snapshot.push((format!("layer.{layer_index}.kv.{kind}"), 2, data));
            }
        }
        snapshot.push(("seq.len".into(), 1, seq.len.to_le_bytes().to_vec()));
        snapshot.push((
            "seq.tokens".into(),
            4,
            bytemuck::cast_slice(&seq.tokens).to_vec(),
        ));
        Ok(snapshot)
    }

    /// Wymusza produkcyjną odbudowę KV i stanu hybrydowego z historii promptu.
    #[doc(hidden)]
    #[cfg(feature = "test-hooks")]
    pub fn debug_recompute_seq(&mut self, seq: &mut SeqKv) -> Result<()> {
        if self.tier.is_none() {
            return Err(ForgeError::Scheduler(
                "test recompute wymaga włączonego tieringu".into(),
            ));
        }
        self.recompute_seq(seq)
    }

    /// Zrzuca carry oraz aktywny prefiks KV MTP w kolejności logicznych stron.
    pub fn debug_mtp_state_snapshot(&self, seq: &SeqKv) -> Result<Vec<(String, usize, Vec<u8>)>> {
        self.device.synchronize()?;
        let lease = seq
            .hybrid_state
            .ok_or_else(|| ForgeError::Unsupported("sekwencja nie ma lease stanu MTP".into()))?;
        let pool = self
            .hybrid_states
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("model nie ma aktywnego stanu MTP".into()))?;
        pool.validate(lease)?;
        let state = pool.slots[lease.slot].mtp.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("stan MTP sekwencji jest aktualnie używany".into())
        })?;
        let mtp_kv = pool
            .mtp_kv
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("cache MTP jest aktualnie używany".into()))?;
        let mut hidden = vec![0u8; state.recurrent_hidden.len()];
        self.device.read(&state.recurrent_hidden, 0, &mut hidden)?;
        let mut snapshot = vec![("mtp.hidden".into(), 2, hidden)];
        for (name, buffer) in [
            ("mtp.page_table", &state.page_table),
            ("mtp.seq_len", &state.seq_len),
            ("mtp.position", &state.position),
        ] {
            let mut bytes = vec![0u8; buffer.len()];
            self.device.read(buffer, 0, &mut bytes)?;
            snapshot.push((name.into(), 4, bytes));
        }
        let head_bytes = mtp_kv.cfg.head_dim * 2;
        for (name, buffers) in [("mtp.k", &mtp_kv.k), ("mtp.v", &mtp_kv.v)] {
            let mut bytes = Vec::with_capacity(state.seq.len * mtp_kv.cfg.n_kv_heads * head_bytes);
            for (offset, length) in logical_kv_regions(
                &state.seq.pages,
                state.seq.len,
                mtp_kv.cfg.page_size,
                mtp_kv.cfg.n_kv_heads,
                head_bytes,
            ) {
                let mut chunk = vec![0u8; length];
                self.device.read(&buffers[0], offset, &mut chunk)?;
                bytes.extend_from_slice(&chunk);
            }
            snapshot.push((name.into(), 2usize, bytes));
        }
        snapshot.push(("mtp.len".into(), 1, state.seq.len.to_le_bytes().to_vec()));
        Ok(snapshot)
    }

    /// Wykonuje pojedynczy referencyjny catch-up MTP po kroku targetu.
    pub fn debug_mtp_catchup_token(&mut self, seq: &mut SeqKv, token: u32) -> Result<()> {
        self.mtp_catchup_token(seq, token)
    }

    /// NEOX partial-rotary width for the hybrid attention layers: M-RoPE over
    /// text positions rotates the first `2*Σ sections` dims of each head.
    fn hybrid_n_rot(&self) -> usize {
        let p = &self.weights.descriptor.params;
        p.rope_sections
            .map(|s| s.iter().sum::<u32>() as usize * 2)
            .unwrap_or(p.head_dim)
    }

    /// Allocate the hybrid single-token scratch (gated-attention de-interleave +
    /// DeltaNet conv/recurrence buffers) on first use.
    fn ensure_hybrid_bufs(&mut self) -> Result<()> {
        let rows = self.batch_cap.max(1);
        if self
            .hybrid_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.projection_rows >= rows)
        {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.clone().expect("hybrid model has ssm params");
        let q_dim = p.n_heads * p.head_dim;
        let q_full = q_dim * 2;
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let key_dim = ssm.key_dim();
        let nv = ssm.n_v_heads();
        let device = self.device.clone();
        let a16 = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let a32 = |elems: usize| device.alloc(elems * 4, MemKind::Device, Pool::Activations);
        self.hybrid_bufs = Some(HybridBufs {
            projection_rows: rows,
            batched_qkv_mixed: a16(rows * conv_dim)?,
            batched_z: a16(rows * value_dim)?,
            batched_alpha: a16(rows * nv)?,
            batched_beta_raw: a16(rows * nv)?,
            q_full: a16(q_full)?,
            qc: a16(q_dim)?,
            gatec: a16(q_dim)?,
            gated: a16(q_dim)?,
            conv_out: a16(conv_dim)?,
            q16: a16(key_dim)?,
            k16: a16(key_dim)?,
            q16src: a16(key_dim)?,
            k16src: a16(key_dim)?,
            q32: a16(value_dim)?,
            k32: a16(value_dim)?,
            vtok: a16(value_dim)?,
            g: a32(nv)?,
            beta_f: a32(nv)?,
            o: a16(value_dim)?,
            normed: a16(value_dim)?,
            pinned_embed: device.alloc(
                p.hidden_size * 2,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
        });
        Ok(())
    }

    /// One token through the hybrid (gated-attention / Gated-DeltaNet + MoE)
    /// stack. Mirrors `run_step_moe`'s residual/norm skeleton, dispatching the
    /// token mixer by layer kind and folding in the gated shared expert. Inputs
    /// (`b.ids`/`b.pos`/`seq_len_dev`/page table) must be uploaded by the
    /// caller; the next-token logits land in `b.logits` when `want_logits`.
    /// Wstawia wiersz embeddingu tego tokena do bufora rezydualnego.
    ///
    /// Tablica embeddingu mieszka w RAM hosta (VRAM zostaje na wagi), więc
    /// wiersz idzie przez pamięć przypiętą i asynchroniczne H2D na strumieniu
    /// obliczeniowym. Kolejność strumienia serializuje to za ogonem poprzedniego
    /// tokena, więc nie trzeba blokującej synchronizacji.
    ///
    /// WYDZIELONE Z `hybrid_forward_token`, bo to JEDYNY krok kroku dekodowania
    /// zależny od `token_id` po stronie hosta — reszta czyta pozycję i długość
    /// sekwencji z buforów urządzenia. Dzięki temu reszta daje się przechwycić
    /// w graf i odtwarzać bez kosztu uruchamiania kerneli po kolei.
    fn stage_hybrid_embedding(&self, token_id: u32) -> Result<()> {
        let hidden = self.weights.descriptor.params.hidden_size;
        let host = self
            .weights
            .token_embd_host
            .as_ref()
            .expect("hybrid model has host embedding");
        let base = token_id as usize * hidden;
        let row = host.get(base..base + hidden).ok_or_else(|| {
            ForgeError::Scheduler(format!("token id {token_id} out of embedding range"))
        })?;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let dst = hb
            .pinned_embed
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(row.as_ptr() as *const u8, dst, hidden * 2);
        }
        self.device
            .copy(&hb.pinned_embed, 0, &self.bufs.h, 0, hidden * 2, &self.stream)
    }

    fn hybrid_forward_token(&self, token_id: u32, want_logits: bool, src: AttnSrc) -> Result<()> {
        self.stage_hybrid_embedding(token_id)?;
        self.hybrid_forward_staged(want_logits, src)
    }

    /// Krok hybrydowy OD wstawionego embeddingu — bez niczego zależnego od
    /// `token_id`, więc nadaje się do przechwycenia w graf.
    fn hybrid_forward_staged(&self, want_logits: bool, src: AttnSrc) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let n_layers = self.weights.layers.len();
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            match &layer.mixer {
                LayerMixer::DeepseekAttention(_) => {
                    unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                }
                LayerMixer::Attention(a) => self.hybrid_attn_mixer(l, a, &src)?,
                LayerMixer::DeltaNet(d) => {
                    self.hybrid_delta_projections(d, &b.x, 1)?;
                    self.hybrid_delta_mixer(l, d, 0)?;
                }
            }
            // Residual add (mixer output) + post-attention norm for the FFN.
            kernels.rmsnorm_residual_f16(
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;
            match &layer.ffn {
                LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, l, hidden, stream)?,
                LayerFfn::Dense(ffn) => {
                    match &ffn.gate_up {
                        GateUpWeights::Fused(weight) => {
                            self.gemv(&b.gate_up, weight, &b.x, stream)?;
                            kernels.glu_mul_f16_at(self.ffn_act(), 
                                &b.act,
                                &b.gate_up,
                                0,
                                inter * 2,
                                inter,
                                stream,
                            )?;
                        }
                        GateUpWeights::Split { gate, up } => {
                            // Obie projekcje czytają TĘ SAMĄ znormalizowaną
                            // aktywację, więc idą jednym uruchomieniem ze
                            // wspólną kwantyzacją zamiast dwoma.
                            if !self.gemv_nvfp4_gguf_group(
                                &[(&b.gate, gate), (&b.up, up)],
                                &b.x,
                                stream,
                            )? {
                                self.gemv(&b.gate, gate, &b.x, stream)?;
                                self.gemv(&b.up, up, &b.x, stream)?;
                            }
                            kernels.glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)?;
                        }
                    }
                    self.gemv(&b.down, &ffn.down, &b.act, stream)?;
                }
            }
            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;

            if self.hybrid_debug {
                self.device.synchronize()?;
                let mut hb = vec![0u8; hidden * 2];
                self.device.read(&b.h, 0, &mut hb)?;
                let hf: &[f16] = bytemuck::cast_slice(&hb);
                let norm: f32 = hf.iter().map(|v| v.to_f32().powi(2)).sum::<f32>().sqrt();
                let kind = if matches!(layer.mixer, LayerMixer::DeltaNet(_)) {
                    "delta"
                } else {
                    "attn"
                };
                eprintln!("  layer {l:2} [{kind}] ||h|| = {norm:.4}");
            }
        }

        if want_logits {
            self.logits_gemv(&b.logits, &b.x, stream)?;
        }
        Ok(())
    }

    /// Gated softmax-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. The Q
    /// projection is gated (`[q, gate]` interleaved per head), so q/gate are
    /// de-interleaved, per-head QK-norm + partial RoPE applied, causal decode
    /// attention run, then `out = attn ⊙ sigmoid(gate)` before the O projection.
    fn hybrid_attn_mixer(&self, l: usize, a: &AttnWeights, src: &AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let head_dim = p.head_dim;
        let n_heads = p.n_heads;
        let n_kv = p.n_kv_heads;
        let q_dim = n_heads * head_dim;
        let eps = p.rms_norm_eps;
        let theta = p.rope_theta;
        let n_rot = self.hybrid_n_rot();
        let scale = p.attn_scale_at(l);
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let (wq, wk, wv) = match &a.attn_qkv {
            QkvWeights::Split { q, k, v } => (q, k, v),
            _ => {
                return Err(ForgeError::Unsupported(
                    "hybrid attention expects split q/k/v weights".into(),
                ))
            }
        };
        // Gated Q projection [2*q_dim], then de-interleave per head: q at
        // h*2*head_dim, gate at h*2*head_dim + head_dim.
        let qkv_grouped =
            self.gemv_nvfp4_gguf_group(&[(&hb.q_full, wq), (&b.k, wk), (&b.v, wv)], &b.x, stream)?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, wq, &b.x, stream)?;
            self.gemv(&b.k, wk, &b.x, stream)?;
            self.gemv(&b.v, wv, &b.x, stream)?;
        }
        kernels.deinterleave_gate_f16(&hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream)?;
        if let Some(qn) = &a.q_norm {
            kernels.rmsnorm_f16(&hb.qc, &hb.qc, qn, n_heads, head_dim, eps, stream)?;
        }
        if let Some(kn) = &a.k_norm {
            kernels.rmsnorm_f16(&b.k, &b.k, kn, n_kv, head_dim, eps, stream)?;
        }
        kernels
            .rope_neox_partial_f16(&hb.qc, &b.pos, 1, n_heads, head_dim, n_rot, theta, stream)?;
        kernels.rope_neox_partial_f16(&b.k, &b.pos, 1, n_kv, head_dim, n_rot, theta, stream)?;
        kernels.kv_append_f16(
            &self.kv.k[self.target_kv_layer(l)],
            &self.kv.v[self.target_kv_layer(l)],
            &b.k,
            &b.v,
            &self.page_table_dev,
            &self.seq_len_dev,
            n_kv,
            self.kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        match src {
            AttnSrc::Paged => {
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    &hb.qc,
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
            AttnSrc::Staged(seq) => {
                // Spilled sequence: kv_append above committed this token into
                // the resident tail of the canonical paged slab; staging then
                // materializes the FULL context for this attention layer (cold
                // pages streamed from RAM/NVMe, resident pages copied D2D) and
                // attention runs over it via the identity page table. Same
                // kernel + order as the paged path, so greedy tokens are
                // bit-identical to an untiered run.
                let tier = self
                    .tier
                    .as_ref()
                    .expect("staged attention requires tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    &hb.qc,
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    self.attn_window(l),
                    stream,
                )?;
            }
        }
        // Output gate: out = attn ⊙ sigmoid(gate), applied on-device so the
        // whole mixer stays on the compute stream (no per-layer host sync).
        kernels.sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.gemv(&b.o_out, &a.attn_o, &hb.gated, stream)?;
        Ok(())
    }

    /// Gated-DeltaNet linear-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. Advances
    /// this layer's resident conv window + recurrent state by one token.
    /// Liczy cztery projekcje wejściowe DeltaNet dla `n_rows` wierszy `x` naraz.
    /// Są bezstanowe, więc wiersze mogą należeć do różnych sekwencji; jeden
    /// przebieg po wagach zastępuje `n_rows` przebiegów per lane. Trafia w tę
    /// samą rodzinę weight-stationary dp4a (`gemm_q8_0_i8mma_b*`) co ścieżka
    /// jednolane'owa, więc numeryka się nie zmienia.
    fn hybrid_delta_projections(
        &self,
        d: &DeltaNetWeights,
        x: &DevBuffer,
        n_rows: usize,
    ) -> Result<()> {
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        if n_rows > hb.projection_rows {
            return Err(ForgeError::Scheduler(format!(
                "projekcje DeltaNet: {n_rows} wierszy przekracza pojemność {}",
                hb.projection_rows
            )));
        }
        let projections = [
            (&hb.batched_qkv_mixed, &d.in_proj),
            (&hb.batched_z, &d.gate_proj),
            (&hb.batched_alpha, &d.alpha_proj),
            (&hb.batched_beta_raw, &d.beta_proj),
        ];
        // Cztery projekcje czytają TEN SAM znormalizowany `x`, ale NIE MAJĄ tego
        // samego formatu: `in_proj` jest NVFP4, a bramka, alfa i beta Q8_0.
        // Dlatego grupujemy PER FORMAT — jednorodna próba na całej czwórce
        // odpadała i wszystkie cztery szły osobno. Osobno każda ma za małą
        // siatkę, żeby wypełnić kartę.
        if n_rows == 1 {
            let mut nvfp4: Vec<(&DevBuffer, &DevWeight)> = Vec::new();
            let mut q8: Vec<(&DevBuffer, &DevWeight)> = Vec::new();
            for &(y, w) in &projections {
                match w {
                    DevWeight::NvFp4Gguf { .. } => nvfp4.push((y, w)),
                    DevWeight::Q8_0 { .. } => q8.push((y, w)),
                    _ => {
                        nvfp4.clear();
                        q8.clear();
                        break;
                    }
                }
            }
            if nvfp4.len() + q8.len() == projections.len() {
                for (subset, is_nvfp4) in [(&nvfp4, true), (&q8, false)] {
                    if subset.is_empty() {
                        continue;
                    }
                    let fused = if is_nvfp4 {
                        self.gemv_nvfp4_gguf_group(subset, x, &self.stream)?
                    } else {
                        self.gemv_q8_0_group(subset, x, &self.stream)?
                    };
                    if !fused {
                        for &(y, w) in subset.iter() {
                            self.hybrid_project(y, w, x, 1)?;
                        }
                    }
                }
                return Ok(());
            }
        }
        for (y, w) in projections {
            self.hybrid_project(y, w, x, n_rows)?;
        }
        Ok(())
    }

    /// Jedna projekcja hybrydy: `gemv` dla pojedynczego wiersza, batchowy `gemm`
    /// dla wielu. Rozgałęzienie jest konieczne, bo ścieżka GEMM dla wag NVFP4
    /// GGUF odrzuca jeden token (`gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch`).
    fn hybrid_project(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_rows: usize,
    ) -> Result<()> {
        if n_rows == 1 {
            return self.gemv(y, w, x, &self.stream);
        }
        self.gemm(y, w, x, n_rows, &self.stream)
    }

    fn hybrid_delta_mixer(&self, l: usize, d: &DeltaNetWeights, lane: usize) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrid has ssm params");
        let eps = p.rms_norm_eps;
        let conv_dim = ssm.conv_dim();
        let d_conv = ssm.d_conv;
        let key_dim = ssm.key_dim();
        let value_dim = ssm.value_dim();
        let d_state = ssm.d_state;
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let rep = n_v / n_k;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let st = self.active_ssm()[l]
            .as_ref()
            .expect("DeltaNet layer has ssm state");

        // Projekcje wejściowe policzył `hybrid_delta_projections` dla wszystkich
        // lane'ów naraz. Konsumenci czytają swój wiersz przez przesunięcie
        // bajtowe, więc nie ma kopii do jednotokenowego scratchu.
        let qkv_off = lane * conv_dim * 2;
        let z_off = lane * value_dim * 2;
        let head_off = lane * n_v * 2;

        // Causal depthwise conv + SiLU (advances the conv window in place).
        kernels.deltanet_conv_silu_f16_at(
            &hb.conv_out,
            0,
            &st.conv,
            &hb.batched_qkv_mixed,
            qkv_off,
            &d.conv1d,
            conv_dim,
            d_conv,
            stream,
        )?;
        // Split conv output into q/k (key_dim each) and v (value_dim).
        self.device
            .copy(&hb.conv_out, 0, &hb.q16src, 0, key_dim * 2, stream)?;
        self.device.copy(
            &hb.conv_out,
            key_dim * 2,
            &hb.k16src,
            0,
            key_dim * 2,
            stream,
        )?;
        self.device.copy(
            &hb.conv_out,
            2 * key_dim * 2,
            &hb.vtok,
            0,
            value_dim * 2,
            stream,
        )?;
        // Per-head L2 norm on the key-head q/k (n_k heads over d_state).
        kernels.l2norm_heads_f16(&hb.q16, &hb.q16src, n_k, d_state, eps, stream)?;
        kernels.l2norm_heads_f16(&hb.k16, &hb.k16src, n_k, d_state, eps, stream)?;
        // Format GGUF przestawia tensory strony V do układu kafelkowego, więc
        // każda głowa V używa głowy K o indeksie `head % n_k`.
        let key_bytes = n_k * d_state * 2;
        for r in 0..rep {
            self.device
                .copy(&hb.q16, 0, &hb.q32, r * key_bytes, key_bytes, stream)?;
            self.device
                .copy(&hb.k16, 0, &hb.k32, r * key_bytes, key_bytes, stream)?;
        }
        // Per-head log-decay g = softplus(alpha + dt_bias)·a and beta gate.
        kernels.deltanet_log_decay_f32_at(
            &hb.g,
            0,
            &hb.batched_alpha,
            head_off,
            &d.dt_bias,
            &d.a,
            n_v,
            stream,
        )?;
        kernels.deltanet_beta_sigmoid_f32_at(
            &hb.beta_f,
            &hb.batched_beta_raw,
            head_off,
            n_v,
            stream,
        )?;
        // Rank-1 gated-delta recurrence (advances the state matrix in place).
        match self.delta_state_layout() {
            DeltaStateLayout::ValueKey => kernels.deltanet_value_key_scan_inplace_f16(
                &hb.o, &st.state, &st.state, &hb.q32, &hb.k32, &hb.vtok, &hb.g, &hb.beta_f, 1, 1,
                n_v, stream,
            )?,
            DeltaStateLayout::KeyValue => kernels.deltanet_gated_step_f16(
                &hb.o, &st.state, &hb.q32, &hb.k32, &hb.vtok, &hb.g, &hb.beta_f, n_v, d_state,
                stream,
            )?,
        }
        // Output gated RMSNorm then the value-dim → hidden out projection.
        kernels.deltanet_gated_rmsnorm_f16_at(
            &hb.normed,
            &hb.o,
            &hb.batched_z,
            z_off,
            &d.ssm_norm,
            n_v,
            d_state,
            eps,
            stream,
        )?;
        self.gemv(&b.o_out, &d.out_proj, &hb.normed, stream)?;
        Ok(())
    }

    /// Prefill a prompt chunk for the hybrid arch as a sequential per-token
    /// recurrent scan (the DeltaNet state carries token-to-token). Returns the
    /// last token's next-token logits. Tier-aware: each token first spills the
    /// coldest attention KV if the hot pool is full, so a long prompt beyond the
    /// VRAM pool prefills by streaming older attention KV back per layer while
    /// the resident DeltaNet state advances untouched.
    fn prefill_hybrid(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.activate_hybrid_sequence(seq)?;
        if tokens.len() >= 32 && self.hybrid_layer_major_route_capable() {
            return self.prefill_hybrid_layer_major(seq, tokens);
        }
        let batched_enabled =
            std::env::var("FORGE_HYBRID_BATCH_PREFILL").map_or(true, |value| value != "0");
        if batched_enabled && tokens.len() > 1 && self.validate_hybrid_speculation_target().is_ok()
        {
            return self.prefill_hybrid_batched(seq, tokens);
        }
        self.ensure_hybrid_bufs()?;
        let p = self.weights.descriptor.params.clone();
        let vocab = p.vocab_size;
        let page_size = self.kv.cfg.page_size;
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        let mut last_logits = Vec::new();
        for (i, &tok) in tokens.iter().enumerate() {
            let pos = seq.len;
            if pos == 0 {
                self.reset_mtp_runtime(seq)?;
            }
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            // Free VRAM pages for this token before growing; may spill the
            // coldest attention KV to RAM/NVMe. Retain the tokens (recompute
            // path) and track the still-purely-prefilled prefix.
            self.tier_ensure_capacity(seq, 1)?;
            if self.tier.is_some() {
                if seq.tokens.len() == seq.prefilled_len {
                    seq.prefilled_len += 1;
                }
                seq.tokens.push(tok);
            }
            let staged = self.tier.is_some() && !seq.spilled.is_empty();
            let page_boundary = seq.len.is_multiple_of(page_size);
            self.kv.grow(seq)?;
            self.upload_decode_inputs(tok, pos)?;
            let want = i + 1 == tokens.len();
            self.profile_target_start()?;
            if staged {
                self.tier
                    .as_mut()
                    .expect("staged implies tiering")
                    .prepare_streaming(seq)?;
                self.upload_page_table(seq)?;
                self.hybrid_forward_token(tok, want, AttnSrc::Staged(seq))?;
            } else {
                if page_boundary || self.pt_seq != seq.id {
                    self.upload_page_table(seq)?;
                }
                self.hybrid_forward_token(tok, want, AttnSrc::Paged)?;
            }
            self.profile_target_end()?;
            self.profile_catchup_start()?;
            self.mtp_catchup_token(seq, tok)?;
            self.profile_catchup_end()?;
            if want {
                self.device.copy(
                    &self.bufs.logits,
                    0,
                    &self.bufs.pinned_logits,
                    0,
                    vocab * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let lp =
                    self.bufs
                        .pinned_logits
                        .host_ptr()
                        .expect("pinned buffer has host mapping") as *const f32;
                last_logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
            }
        }
        // Feed the measured prefill rate into the tier's transfer-vs-recompute
        // estimate (bit-identical recompute is eligible only for prefill KV).
        if let (Some(t0), Some(tier)) = (tier_t0, self.tier.as_ref()) {
            if !tokens.is_empty() {
                tier.note_prefill(tokens.len(), t0.elapsed().as_secs_f64());
            }
        }
        Ok(last_logits)
    }

    fn checkpoint_hybrid_layer_major(&self, seq: &SeqKv) -> Result<HybridLayerMajorCheckpoint> {
        let verifier = self.hybrid_verify_bufs.as_ref().ok_or_else(|| {
            ForgeError::Scheduler("layer-major nie ma workspace verifiera do rollbacku".into())
        })?;
        let state_workspace = verifier
            .retained_state_checkpoints
            .as_ref()
            .ok_or_else(|| {
                ForgeError::Scheduler("layer-major nie ma retained checkpointów stanu".into())
            })?
            .clone();
        let conv_workspaces = verifier
            .delta
            .iter()
            .map(|cache| cache.as_ref().map(|cache| cache.conv_initial.clone()))
            .collect::<Vec<_>>();
        let state_bytes = self
            .active_ssm()
            .iter()
            .flatten()
            .next()
            .ok_or_else(|| ForgeError::Scheduler("layer-major nie ma stanu DeltaNet".into()))?
            .state
            .len();
        let delta_layers = self.active_ssm().iter().flatten().count();
        let kv_byte_offset = state_bytes.checked_mul(delta_layers).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie checkpointu stanów layer-major".into())
        })?;
        let kv_page_bytes = checked_scratch_bytes(
            "checkpoint strony KV layer-major",
            &[
                self.kv.cfg.n_kv_heads,
                self.kv.cfg.page_size,
                self.kv.cfg.head_dim,
            ],
            2,
        )?;
        let tail_page = if seq.len > 0 && !seq.len.is_multiple_of(self.kv.cfg.page_size) {
            let physical = *seq
                .pages
                .last()
                .ok_or_else(|| ForgeError::Scheduler("częściowy ogon KV nie ma strony".into()))?;
            Some(usize::try_from(physical).map_err(|_| {
                ForgeError::Unsupported("layer-major nie obsługuje spilled ogona KV".into())
            })?)
        } else {
            None
        };
        let kv_checkpoint_bytes = if tail_page.is_some() {
            checked_scratch_bytes(
                "checkpoint ogona KV layer-major",
                &[2, self.kv.k.len(), kv_page_bytes],
                1,
            )?
        } else {
            0
        };
        let required = kv_byte_offset
            .checked_add(kv_checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie workspace rollbacku layer-major".into())
            })?;
        if state_workspace.len() < required {
            return Err(ForgeError::Scheduler(format!(
                "retained checkpointy mają {} bajtów, rollback layer-major wymaga {required}",
                state_workspace.len()
            )));
        }
        let mut delta_index = 0usize;
        for (layer_index, state) in self.active_ssm().iter().enumerate() {
            let Some(state) = state else { continue };
            self.device.copy(
                &state.state,
                0,
                &state_workspace,
                delta_index * state_bytes,
                state_bytes,
                &self.stream,
            )?;
            let conv_workspace = conv_workspaces[layer_index].as_ref().ok_or_else(|| {
                ForgeError::Scheduler("warstwa DeltaNet nie ma checkpointu conv".into())
            })?;
            self.device.copy(
                &state.conv,
                0,
                conv_workspace,
                0,
                state.conv.len(),
                &self.stream,
            )?;
            delta_index += 1;
        }
        if let Some(physical) = tail_page {
            let source_offset = physical
                .checked_mul(kv_page_bytes)
                .ok_or_else(|| ForgeError::Scheduler("przepełnienie offsetu strony KV".into()))?;
            for layer in 0..self.kv.k.len() {
                for (kind, slab) in [&self.kv.k[layer], &self.kv.v[layer]]
                    .into_iter()
                    .enumerate()
                {
                    let destination_offset = kv_byte_offset + (2 * layer + kind) * kv_page_bytes;
                    self.device.copy(
                        slab,
                        source_offset,
                        &state_workspace,
                        destination_offset,
                        kv_page_bytes,
                        &self.stream,
                    )?;
                }
            }
        }
        Ok(HybridLayerMajorCheckpoint {
            base: seq.len,
            pages: seq.pages.clone(),
            tokens_len: seq.tokens.len(),
            prefilled_len: seq.prefilled_len,
            state_workspace,
            conv_workspaces,
            state_bytes,
            kv_byte_offset,
            kv_page_bytes,
            tail_page,
        })
    }

    fn rollback_hybrid_layer_major(
        &mut self,
        seq: &mut SeqKv,
        checkpoint: &HybridLayerMajorCheckpoint,
    ) -> Result<()> {
        let restore_result = (|| -> Result<()> {
            let mut delta_index = 0usize;
            for (layer_index, state) in self.active_ssm().iter().enumerate() {
                let Some(state) = state else { continue };
                self.device.copy(
                    &checkpoint.state_workspace,
                    delta_index * checkpoint.state_bytes,
                    &state.state,
                    0,
                    checkpoint.state_bytes,
                    &self.stream,
                )?;
                let conv_workspace = checkpoint.conv_workspaces[layer_index]
                    .as_ref()
                    .ok_or_else(|| {
                        ForgeError::Scheduler("rollback nie ma checkpointu conv".into())
                    })?;
                self.device.copy(
                    conv_workspace,
                    0,
                    &state.conv,
                    0,
                    state.conv.len(),
                    &self.stream,
                )?;
                delta_index += 1;
            }
            if let Some(physical) = checkpoint.tail_page {
                let destination_offset = physical
                    .checked_mul(checkpoint.kv_page_bytes)
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie offsetu rollbacku KV".into())
                    })?;
                for layer in 0..self.kv.k.len() {
                    for (kind, slab) in [&self.kv.k[layer], &self.kv.v[layer]]
                        .into_iter()
                        .enumerate()
                    {
                        let source_offset = checkpoint.kv_byte_offset
                            + (2 * layer + kind) * checkpoint.kv_page_bytes;
                        self.device.copy(
                            &checkpoint.state_workspace,
                            source_offset,
                            slab,
                            destination_offset,
                            checkpoint.kv_page_bytes,
                            &self.stream,
                        )?;
                    }
                }
            }
            self.device.synchronize()?;
            Ok(())
        })();
        self.kv.rollback(seq, checkpoint.base);
        seq.tokens.truncate(checkpoint.tokens_len);
        seq.prefilled_len = checkpoint.prefilled_len;
        self.pt_seq = 0;
        if seq.pages != checkpoint.pages {
            return Err(ForgeError::Scheduler(
                "rollback layer-major nie odtworzył mapy stron".into(),
            ));
        }
        restore_result
    }

    /// Wykonuje prefill hybrydowego targetu w macierzowych chunkach i zatwierdza
    /// ostatni checkpoint rekurencji po każdym chunku.
    fn prefill_hybrid_layer_major(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.prefill_hybrid_layer_major_inner(seq, tokens, None, false, false)
    }

    fn prefill_hybrid_layer_major_inner(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        fail_after_layer: Option<usize>,
        fail_mtp_catchup: bool,
        fail_after_mtp_commit: bool,
    ) -> Result<Vec<f32>> {
        if tokens.is_empty() || tokens.len() > HYBRID_LAYER_MAJOR_MAX_TOKENS {
            return Err(ForgeError::Scheduler(
                "layer-major prefill otrzymał nieobsługiwaną długość".into(),
            ));
        }
        if seq.len + tokens.len() > self.weights.descriptor.params.max_position_embeddings {
            return Err(ForgeError::Scheduler(
                "layer-major prefill przekracza kontekst".into(),
            ));
        }
        {
            let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
            })?;
            for &token in tokens {
                let end = (token as usize + 1)
                    .checked_mul(self.weights.descriptor.params.hidden_size)
                    .ok_or_else(|| {
                        ForgeError::Scheduler("przepełnienie indeksu embeddingu".into())
                    })?;
                if end > table.len() {
                    return Err(ForgeError::Scheduler(format!(
                        "token id {token} wykracza poza embedding targetu"
                    )));
                }
            }
        }
        self.ensure_hybrid_verify_bufs(4)?;
        self.ensure_hybrid_layer_major_bufs(tokens.len())?;
        if seq.len == 0 {
            self.reset_mtp_runtime(seq)?;
        }
        let arena = self
            .hybrid_layer_major_bufs
            .as_ref()
            .expect("arena layer-major jest gotowa")
            .clone();
        let p = self.weights.descriptor.params.clone();
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let t = tokens.len();
        let base = seq.len;
        let attention_backend = self.hybrid_layer_major_attention_backend()?;
        let persistent_scan = hybrid_layer_major_persistent_scan_requested()?
            && t > 128
            && self
                .kernels
                .supports_deltanet_gated_scan_persistent_d128_f16();
        let checkpoint = self.checkpoint_hybrid_layer_major(seq)?;
        let result = (|| {
            let new_pages = (base + t)
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len());
            self.ensure_free_pages(new_pages);
            for _ in 0..t {
                self.kv.grow(seq)?;
            }
            let mut page_table = vec![-1i32; self.max_pages_per_seq];
            page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.pt_seq = seq.id;

            let table = self
                .weights
                .token_embd_host
                .as_ref()
                .expect("embedding targetu sprawdzono przed mutacją KV");
            let hidden_bytes = p.hidden_size * 2;
            let staging = &arena.host_staging;
            let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
            for (chunk_index, chunk) in tokens.chunks(128).enumerate() {
                let offset = chunk_index * 128;
                let slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
                let host = &staging[slot];
                if staging_recorded[slot] {
                    host.ready.synchronize()?;
                }
                let ids: Vec<i32> = chunk.iter().map(|&id| id as i32).collect();
                let positions: Vec<i32> = (base + offset..base + offset + chunk.len())
                    .map(|position| position as i32)
                    .collect();
                let visible_lens: Vec<i32> = (base + offset + 1..=base + offset + chunk.len())
                    .map(|len| len as i32)
                    .collect();
                write_pinned(bytemuck::cast_slice(&ids), &host.ids)?;
                write_pinned(bytemuck::cast_slice(&positions), &host.positions)?;
                write_pinned(bytemuck::cast_slice(&visible_lens), &host.visible_lens)?;
                let destination = host
                    .embedding
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                for (row, &token) in chunk.iter().enumerate() {
                    let source = table
                        .get(token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size)
                        .ok_or_else(|| {
                            ForgeError::Scheduler(format!(
                                "token id {token} wykracza poza embedding targetu"
                            ))
                        })?;
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            source.as_ptr() as *const u8,
                            destination.add(row * hidden_bytes),
                            hidden_bytes,
                        );
                    }
                }
                if chunk_index == 0 {
                    write_pinned(bytemuck::cast_slice(&page_table), &host.page_table)?;
                    write_pinned(&(base as i32).to_le_bytes(), &host.base_pos)?;
                    self.device.copy(
                        &host.page_table,
                        0,
                        &self.page_table_dev,
                        0,
                        page_table.len() * 4,
                        &self.stream,
                    )?;
                    self.device
                        .copy(&host.base_pos, 0, &arena.base_pos, 0, 4, &self.stream)?;
                }
                self.device.copy(
                    &host.embedding,
                    0,
                    &arena.h,
                    offset * hidden_bytes,
                    chunk.len() * hidden_bytes,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.ids,
                    0,
                    &arena.ids,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.positions,
                    0,
                    &arena.positions,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.copy(
                    &host.visible_lens,
                    0,
                    &arena.visible_lens,
                    offset * 4,
                    chunk.len() * 4,
                    &self.stream,
                )?;
                self.device.record_event(&host.ready, &self.stream)?;
                staging_recorded[slot] = true;
            }

            self.profile_target_start()?;
            self.kernels.rmsnorm_f16(
                &arena.x,
                &arena.h,
                &self.weights.layers[0].attn_norm,
                t,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;
            let q_dim = p.n_heads * p.head_dim;
            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
                            return Err(ForgeError::Unsupported(
                                "layer-major wymaga rozdzielonych Q/K/V".into(),
                            ));
                        };
                        self.gemm(&arena.q_full, q, &arena.x, t, &self.stream)?;
                        self.kernels.deinterleave_gate_f16(
                            &arena.qc,
                            &arena.gatec,
                            &arena.q_full,
                            p.head_dim,
                            t * q_dim,
                            &self.stream,
                        )?;
                        if let Some(norm) = &attention.q_norm {
                            self.kernels.rmsnorm_f16(
                                &arena.qc,
                                &arena.qc,
                                norm,
                                t * p.n_heads,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        self.gemm(&arena.k, k, &arena.x, t, &self.stream)?;
                        self.gemm(&arena.v, v, &arena.x, t, &self.stream)?;
                        if let Some(norm) = &attention.k_norm {
                            self.kernels.rmsnorm_f16(
                                &arena.k,
                                &arena.k,
                                norm,
                                t * p.n_kv_heads,
                                p.head_dim,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        let n_rot = self.hybrid_n_rot();
                        self.kernels.rope_neox_partial_f16(
                            &arena.qc,
                            &arena.positions,
                            t,
                            p.n_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        self.kernels.rope_neox_partial_f16(
                            &arena.k,
                            &arena.positions,
                            t,
                            p.n_kv_heads,
                            p.head_dim,
                            n_rot,
                            p.rope_theta,
                            &self.stream,
                        )?;
                        let kv_layer = self.target_kv_layer(layer_index);
                        self.kernels.kv_append_batch_device_pos_f16(
                            &self.kv.k[kv_layer],
                            &self.kv.v[kv_layer],
                            &arena.k,
                            &arena.v,
                            &self.page_table_dev,
                            &arena.base_pos,
                            t,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            &self.stream,
                        )?;
                        match attention_backend {
                            HybridLayerMajorAttention::Exact => {
                                self.kernels.attn_decode_batch_exact_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    &arena.visible_lens,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    self.max_pages_per_seq,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                            HybridLayerMajorAttention::Prefill => {
                                self.kernels.attn_prefill_device_pos_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    &arena.base_pos,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                            HybridLayerMajorAttention::Flash => {
                                self.kernels.attn_prefill_fa_mojo_f16_hd256(
                                    &arena.q_full,
                                    &arena.qc,
                                    &self.kv.k[kv_layer],
                                    &self.kv.v[kv_layer],
                                    &self.page_table_dev,
                                    base,
                                    t,
                                    p.n_heads,
                                    p.n_kv_heads,
                                    self.kv.cfg.page_size,
                                    1.0 / (p.head_dim as f32).sqrt(),
                                    &self.stream,
                                )?;
                            }
                        }
                        self.kernels.sigmoid_mul_f16(
                            &arena.gated,
                            &arena.q_full,
                            &arena.gatec,
                            t * q_dim,
                            &self.stream,
                        )?;
                        self.gemm(
                            &arena.mixer_out,
                            &attention.attn_o,
                            &arena.gated,
                            t,
                            &self.stream,
                        )?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        let state = self.active_ssm()[layer_index]
                            .as_ref()
                            .expect("warstwa DeltaNet ma stan");
                        self.gemm(&arena.q_full, &delta.in_proj, &arena.x, t, &self.stream)?;
                        if let Some(cols) = Self::delta_input_q8_cols(delta) {
                            let mut prepared =
                                self.kernels.prepare_q8_1(&arena.x, cols, t, &self.stream)?;
                            self.gemm_q8_prepared_triplet(
                                [&arena.z, &arena.alpha, &arena.beta_raw],
                                [&delta.gate_proj, &delta.alpha_proj, &delta.beta_proj],
                                &mut prepared,
                                t,
                            )?;
                        } else {
                            self.gemm(&arena.z, &delta.gate_proj, &arena.x, t, &self.stream)?;
                            self.gemm(&arena.alpha, &delta.alpha_proj, &arena.x, t, &self.stream)?;
                            self.gemm(
                                &arena.beta_raw,
                                &delta.beta_proj,
                                &arena.x,
                                t,
                                &self.stream,
                            )?;
                        }
                        self.device.copy(
                            &state.conv,
                            0,
                            &arena.conv_initial,
                            0,
                            state.conv.len(),
                            &self.stream,
                        )?;
                        if hybrid_layer_major_tiled_prepare_requested()
                            && ssm.d_state == 128
                            && ssm.d_conv == 4
                            && self.kernels.supports_deltanet_prepare_tiled_d128_c4_f16()
                        {
                            self.kernels.deltanet_prepare_tiled_d128_c4_f16(
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                &arena.conv_final,
                                &arena.conv_initial,
                                &arena.q_full,
                                &delta.conv1d,
                                &arena.alpha,
                                &arena.beta_raw,
                                &delta.dt_bias,
                                &delta.a,
                                t,
                                ssm.n_k_heads(),
                                ssm.n_v_heads(),
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        } else {
                            self.kernels.deltanet_prepare_segmented_final_f16(
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                &arena.conv_final,
                                &arena.conv_initial,
                                &arena.q_full,
                                &delta.conv1d,
                                &arena.alpha,
                                &arena.beta_raw,
                                &delta.dt_bias,
                                &delta.a,
                                1,
                                t,
                                ssm.n_k_heads(),
                                ssm.n_v_heads(),
                                ssm.d_state,
                                ssm.d_conv,
                                p.rms_norm_eps,
                                &self.stream,
                            )?;
                        }
                        if self.delta_state_layout() == DeltaStateLayout::ValueKey {
                            self.kernels.deltanet_value_key_scan_persistent_f16(
                                &arena.o,
                                &state.state,
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?;
                        } else if persistent_scan {
                            self.kernels.deltanet_gated_scan_persistent_d128_f16(
                                &arena.o,
                                &state.state,
                                &arena.qc,
                                &arena.gatec,
                                &arena.gated,
                                &arena.g,
                                &arena.beta,
                                t,
                                ssm.n_v_heads(),
                                &self.stream,
                            )?;
                        } else {
                            for token_offset in (0..t).step_by(128) {
                                self.kernels.deltanet_gated_scan_inplace_f16_at(
                                    &arena.o,
                                    &state.state,
                                    &arena.qc,
                                    &arena.gatec,
                                    &arena.gated,
                                    &arena.g,
                                    &arena.beta,
                                    token_offset,
                                    (t - token_offset).min(128),
                                    ssm.n_v_heads(),
                                    ssm.d_state,
                                    &self.stream,
                                )?;
                            }
                        }
                        self.device.copy(
                            &arena.conv_final,
                            0,
                            &state.conv,
                            0,
                            state.conv.len(),
                            &self.stream,
                        )?;
                        self.kernels.deltanet_gated_rmsnorm_f16(
                            &arena.o,
                            &arena.o,
                            &arena.z,
                            &delta.ssm_norm,
                            t * ssm.n_v_heads(),
                            ssm.d_state,
                            p.rms_norm_eps,
                            &self.stream,
                        )?;
                        self.gemm(&arena.mixer_out, &delta.out_proj, &arena.o, t, &self.stream)?;
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &arena.x,
                    &arena.h,
                    &arena.mixer_out,
                    &layer.ffn_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return Err(ForgeError::Unsupported(
                        "layer-major nie obsługuje targetu MoE".into(),
                    ));
                };
                match &ffn.gate_up {
                    GateUpWeights::Fused(weight) => {
                        self.gemm_rows(
                            &arena.gate,
                            weight,
                            &arena.x,
                            t,
                            0,
                            p.intermediate_size,
                            &self.stream,
                        )?;
                        self.gemm_rows(
                            &arena.up,
                            weight,
                            &arena.x,
                            t,
                            p.intermediate_size,
                            p.intermediate_size,
                            &self.stream,
                        )?;
                    }
                    GateUpWeights::Split { gate, up } => {
                        self.gemm(&arena.gate, gate, &arena.x, t, &self.stream)?;
                        self.gemm(&arena.up, up, &arena.x, t, &self.stream)?;
                    }
                }
                self.kernels.glu_mul_f16(self.ffn_act(), 
                    &arena.gate,
                    &arena.gate,
                    &arena.up,
                    t * p.intermediate_size,
                    &self.stream,
                )?;
                self.gemm(&arena.mixer_out, &ffn.down, &arena.gate, t, &self.stream)?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &arena.x,
                    &arena.h,
                    &arena.mixer_out,
                    next_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                if fail_after_layer == Some(layer_index) {
                    return Err(ForgeError::Scheduler(format!(
                        "wymuszony błąd layer-major po warstwie {layer_index}"
                    )));
                }
            }
            self.device.copy(
                &arena.x,
                (t - 1) * hidden_bytes,
                &self.bufs.x,
                0,
                hidden_bytes,
                &self.stream,
            )?;
            self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
            self.device.copy(
                &self.bufs.logits,
                0,
                &self.bufs.pinned_logits,
                0,
                p.vocab_size * 4,
                &self.stream,
            )?;
            self.profile_target_end()?;
            self.profile_catchup_start()?;
            self.mtp_catchup_layer_major_prefix(
                seq,
                tokens,
                &arena.x,
                fail_mtp_catchup,
                fail_after_mtp_commit,
            )?;
            let logits =
                self.bufs
                    .pinned_logits
                    .host_ptr()
                    .expect("pinned logits mają mapowanie hosta") as *const f32;
            Ok(unsafe { std::slice::from_raw_parts(logits, p.vocab_size) }.to_vec())
        })();
        if let Err(error) = result {
            return match self.rollback_hybrid_layer_major(seq, &checkpoint) {
                Ok(()) => Err(error),
                Err(rollback) => Err(self
                    .hybrid_states
                    .as_mut()
                    .expect("model hybrydowy ma pulę stanów")
                    .poison(format!(
                        "błąd layer-major: {error}; rollback nie powiódł się: {rollback}"
                    ))),
            };
        }
        result
    }

    fn prefill_hybrid_batched(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty prefill chunk".into()));
        }
        let p = self.weights.descriptor.params.clone();
        if seq.len + tokens.len() > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                seq.len + tokens.len() - 1,
                p.max_position_embeddings
            )));
        }
        if seq.len == 0 {
            self.reset_mtp_runtime(seq)?;
        }
        self.ensure_hybrid_bufs()?;
        self.ensure_prefill_bufs()?;
        let chunk_size = self.hybrid_prefill_chunk_size;
        let prefill_cap = tokens.len().min(chunk_size);
        let saved_graphs = if prefill_cap > 4 {
            self.ensure_hybrid_verify_bufs(4)?;
            std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
            Some((
                std::mem::take(&mut self.hybrid_verify_graphs),
                std::mem::take(&mut self.hybrid_verify_graph_disabled),
            ))
        } else {
            None
        };
        let result = (|| {
            self.ensure_hybrid_verify_bufs(prefill_cap)?;

            let hidden_bytes = p.hidden_size * 2;
            let mut last_logits = Vec::new();
            let mut staging_recorded = [false; HYBRID_HOST_STAGING_SLOTS];
            let mut offset = 0usize;
            let mut chunk_index = 0usize;
            while offset < tokens.len() {
                let remaining = tokens.len() - offset;
                let t = hybrid_prefill_step_size(remaining, chunk_size);
                let chunk = &tokens[offset..offset + t];
                offset += t;
                let t = chunk.len();
                let base = seq.len;
                let new_pages = (base + t)
                    .div_ceil(self.kv.cfg.page_size)
                    .saturating_sub(seq.pages.len());
                self.ensure_free_pages(new_pages);
                for _ in 0..t {
                    self.kv.grow(seq)?;
                }

                let mut page_table = vec![-1i32; self.max_pages_per_seq];
                page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
                self.pt_seq = seq.id;

                let ids: Vec<i32> = chunk.iter().map(|&id| id as i32).collect();
                let positions: Vec<i32> =
                    (base..base + t).map(|position| position as i32).collect();
                let visible_lens: Vec<i32> = (base + 1..=base + t).map(|len| len as i32).collect();
                let pb = self
                    .prefill_bufs
                    .as_ref()
                    .expect("bufory prefill są gotowe");
                let hv = self
                    .hybrid_verify_bufs
                    .as_ref()
                    .expect("bufory hybrydowego prefill są gotowe");
                let staging_slot = chunk_index % HYBRID_HOST_STAGING_SLOTS;
                let host_staging = &hv.host_staging[staging_slot];
                let staging_ready = host_staging.ready.clone();
                if staging_recorded[staging_slot] {
                    staging_ready.synchronize()?;
                }
                write_pinned(bytemuck::cast_slice(&page_table), &host_staging.page_table)?;
                write_pinned(bytemuck::cast_slice(&ids), &host_staging.ids)?;
                write_pinned(bytemuck::cast_slice(&positions), &host_staging.positions)?;
                write_pinned(
                    bytemuck::cast_slice(&visible_lens),
                    &host_staging.visible_lens,
                )?;
                write_pinned(&(base as i32).to_le_bytes(), &host_staging.base_pos)?;
                write_pinned(&(t as i32).to_le_bytes(), &host_staging.accepted)?;
                let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                    ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
                })?;
                let staging_buffer = &host_staging.embedding;
                let staging = staging_buffer
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                for (row_index, &token) in chunk.iter().enumerate() {
                    let source = table
                        .get(token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size)
                        .ok_or_else(|| {
                            ForgeError::Scheduler(format!(
                                "token id {token} wykracza poza embedding targetu"
                            ))
                        })?;
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            source.as_ptr() as *const u8,
                            staging.add(row_index * hidden_bytes),
                            hidden_bytes,
                        );
                    }
                }
                self.device.copy(
                    &host_staging.page_table,
                    0,
                    &self.page_table_dev,
                    0,
                    page_table.len() * 4,
                    &self.stream,
                )?;
                self.device
                    .copy(&host_staging.ids, 0, &pb.ids, 0, t * 4, &self.stream)?;
                self.device.copy(
                    &host_staging.positions,
                    0,
                    &pb.positions,
                    0,
                    t * 4,
                    &self.stream,
                )?;
                self.device
                    .copy(&host_staging.base_pos, 0, &hv.base_pos, 0, 4, &self.stream)?;
                self.device.copy(
                    &host_staging.visible_lens,
                    0,
                    &hv.visible_lens,
                    0,
                    t * 4,
                    &self.stream,
                )?;
                self.device
                    .copy(&host_staging.accepted, 0, &hv.accepted, 0, 4, &self.stream)?;
                self.device
                    .copy(staging_buffer, 0, &pb.h, 0, t * hidden_bytes, &self.stream)?;
                self.device.record_event(&staging_ready, &self.stream)?;
                staging_recorded[staging_slot] = true;

                self.profile_target_start()?;
                self.run_hybrid_batch_layers(t, true)?;
                let pb = self
                    .prefill_bufs
                    .as_ref()
                    .expect("bufory prefill są gotowe");
                self.device.copy(
                    &pb.x,
                    (t - 1) * hidden_bytes,
                    &self.bufs.x,
                    0,
                    hidden_bytes,
                    &self.stream,
                )?;
                if offset == tokens.len() {
                    self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
                    self.device.copy(
                        &self.bufs.logits,
                        0,
                        &self.bufs.pinned_logits,
                        0,
                        p.vocab_size * 4,
                        &self.stream,
                    )?;
                }
                self.profile_target_end()?;

                self.profile_catchup_start()?;
                if self.has_native_mtp() {
                    self.mtp_catchup_verified_prefix(seq, t, staging_slot, Some(&staging_ready))?;
                }
                self.profile_catchup_end()?;
                chunk_index += 1;
            }
            self.device.synchronize()?;
            let logits = self
                .bufs
                .pinned_logits
                .host_ptr()
                .expect("pinned buffer has host mapping") as *const f32;
            last_logits
                .extend_from_slice(unsafe { std::slice::from_raw_parts(logits, p.vocab_size) });
            Ok(last_logits)
        })();
        if result.is_err() {
            let _ = self.stream.synchronize();
        }
        restore_after(result, || {
            if let Some((graphs, disabled)) = saved_graphs {
                // Verifier decode zachowuje własne bufory cap=4 i przechwycone grafy.
                std::mem::swap(&mut self.hybrid_verify_bufs, &mut self.hybrid_prefill_bufs);
                self.hybrid_verify_graphs = graphs;
                self.hybrid_verify_graph_disabled = disabled;
            }
        })
    }

    /// Fused decode step: six launches per layer instead of nine. The
    /// residual stream is carried as the (h f16, h32 f32) pair — every
    /// norm-consuming kernel recomputes the RMSNorm per block from that pair
    /// (bit-identical to the separate rmsnorm kernels, see decode_fused.mojo)
    /// and attn_decode_split folds the whole qkv_post stage into the
    /// attention prologue (the split/combine pair fills the GPU where one
    /// block per head could not). Layer 0 sums squares from h directly (h32
    /// is only materialized by the first gemv_residual of the step).
    ///
    /// `src` selects the attention's K/V home: the paged cache (recorded into
    /// the replayable decode graph) or the tier staging slabs carrying the
    /// sequence's full context per layer (streamed path, never captured). On
    /// the staged path attn_decode_split appends the new token INTO staging
    /// and the tail page is mirrored back to the canonical paged slab.
    fn trace_f32(&self, label: &str, buf: &DevBuffer, len: usize) {
        if !layer_trace_enabled() {
            return;
        }
        let _ = self.stream.synchronize();
        let mut bytes = vec![0u8; len * 4];
        if self.device.read(buf, 0, &mut bytes).is_err() {
            return;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let sum: f64 = values.iter().map(|v| *v as f64).sum();
        let (best, top) = values
            .iter()
            .enumerate()
            .fold((0usize, f32::NEG_INFINITY), |acc, (i, v)| {
                if *v > acc.1 { (i, *v) } else { acc }
            });
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*b].total_cmp(&values[*a]));
        let head: Vec<String> = order
            .iter()
            .take(5)
            .map(|i| format!("{i}={:.4}", values[*i]))
            .collect();
        eprintln!(
            "TRACE {label}: suma {sum:.6} max id {best} = {top:.4} | top5 {} | id19887={:.4} id415={:.4}",
            head.join(" "),
            values.get(19887).copied().unwrap_or(f32::NAN),
            values.get(415).copied().unwrap_or(f32::NAN)
        );
    }

    fn trace_f16(&self, label: &str, buf: &DevBuffer, byte_offset: usize, len: usize) {
        if !layer_trace_enabled() {
            return;
        }
        let _ = self.stream.synchronize();
        let mut bytes = vec![0u8; len * 2];
        if self.device.read(buf, byte_offset, &mut bytes).is_err() {
            return;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect();
        let sum: f32 = values.iter().sum();
        eprintln!(
            "TRACE {label}: [{:.4}, {:.4}, {:.4} ... {:.4}] suma {sum:.6}",
            values[0],
            values[1],
            values[2],
            values[len - 1]
        );
    }

    fn run_step_fused(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = p.attn_scale_at(0);
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;

        self.trace_f16("embd", &b.h, 0, hidden);

        let n_layers = self.weights.layers.len();
        if let AttnSrc::Staged(seq) = &src {
            // Ping-pong staging: layer l+1 restores on the tier's transfer
            // stream while layer l computes. Both slots start "free" relative
            // to any prior compute work, and slot 0 prestages layer 0.
            let tier = self
                .tier
                .as_ref()
                .expect("staged attention requires tiering");
            let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
            let xfer = tier.xfer_stream();
            for slot in &tb.slots {
                self.device.record_event(&slot.free, stream)?;
            }
            self.device.wait_event(xfer, &tb.slots[0].free)?;
            tier.stage_layer(&self.kv, seq, 0, &tb.slots[0].stage, 0, xfer)?;
            self.device.record_event(&tb.slots[0].ready, xfer)?;
        }
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Fused QKV projects with one gemv_norm into the fused buffer;
            // split layers (mixed formats) run one gemv_norm per projection —
            // per-row math is identical, only the block-level norm recompute
            // repeats. Both feed attn_decode_split via buffer + byte offset.
            let (q_buf, q_off, k_buf, k_off, v_buf, v_off) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w_qkv) => {
                    self.gemv_norm(&b.qkv, w_qkv, &layer.attn_norm, l == 0, eps, stream)?;
                    if l == 0 {
                        self.trace_f16("Qcur-0", &b.qkv, 0, q_dim);
                    }
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    // The fused q|k rows land at the front of b.qkv, exactly
                    // where the Fused layout puts them; v goes to its own
                    // buffer.
                    self.gemv_norm(&b.qkv, qk, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.v, 0usize)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv_norm(&b.q, q, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.k, k, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.q, 0usize, &b.k, 0usize, &b.v, 0usize)
                }
            };
            let gqa_q_heads = p.n_kv_heads.checked_mul(4);
            let use_gqa = std::env::var("FORGE_ATTN_GQA").ok().as_deref() != Some("0")
                && self.device.caps().vendor == Vendor::Nvidia
                && kernels.supports_attn_decode_gqa4_f16_hd128()
                && self.kv.cfg.dtype() == forge_types::DType::F16
                && p.head_dim == 128
                && gqa_q_heads == Some(p.n_heads)
                && layer.attn().q_norm.is_none()
                && layer.attn().k_norm.is_none();
            let attn_splits = if use_gqa {
                ATTN_DECODE_GQA_SPLITS
            } else {
                ATTN_DECODE_SPLITS
            };
            match &src {
                AttnSrc::Paged => {
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[self.target_kv_layer(l)],
                            &self.kv.v[self.target_kv_layer(l)],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
                AttnSrc::Staged(seq) => {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let xfer = tier.xfer_stream();
                    let s = l % STAGE_SLOTS;
                    // Prestage the NEXT layer into the other slot on the
                    // transfer stream while this layer computes.
                    if l + 1 < n_layers {
                        let ns = (l + 1) % STAGE_SLOTS;
                        self.device.wait_event(xfer, &tb.slots[ns].free)?;
                        tier.stage_layer(&self.kv, seq, l + 1, &tb.slots[ns].stage, ns, xfer)?;
                        self.device.record_event(&tb.slots[ns].ready, xfer)?;
                    }
                    self.device.wait_event(stream, &tb.slots[s].ready)?;
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
            }
            if use_gqa {
                kernels.attn_decode_combine_gqa2_f16_hd128(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    attn_splits,
                    stream,
                )?;
            } else {
                kernels.attn_decode_combine_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    attn_splits,
                    stream,
                )?;
            }
            if let AttnSrc::Staged(seq) = &src {
                // The kernel appended this token's rope'd K/V into the staging
                // tail page; mirror that page back into the canonical paged
                // cache so future steps (and spills) see it, then mark the
                // slot free for the transfer stream to restage.
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let s = l % STAGE_SLOTS;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &tb.slots[s].stage[0],
                    lp * rb,
                    &self.kv.k[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &tb.slots[s].stage[1],
                    lp * rb,
                    &self.kv.v[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.record_event(&tb.slots[s].free, stream)?;
            }
            self.gemv_residual(&layer.attn().attn_o, &b.attn_out, stream)?;
            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_norm_silu(&b.act, w, &layer.ffn_norm, eps, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    // Mixed-format gate/up: two gemv_norm launches (same
                    // per-row math as the fused silu kernels, the norm
                    // recompute repeats) + the elementwise SwiGLU combine.
                    // Rounding matches gemv_norm_silu: both projections are
                    // stored as f16 before silu_mul reads them.
                    self.gemv_norm(&b.gate, gate, &layer.ffn_norm, false, eps, stream)?;
                    self.gemv_norm(&b.up, up, &layer.ffn_norm, false, eps, stream)?;
                    kernels.glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, p.intermediate_size, stream)?;
                }
            }
            self.gemv_residual(&layer.dense_ffn()?.down, &b.act, stream)?;
        }

        kernels.rmsnorm_h32_f16(
            &b.x,
            &b.h,
            &b.h32,
            &self.weights.output_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Batchowa głowa logitów: y[b, vocab] f32 = lm_head · x[b, hidden].
    fn logits_gemm(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        match &self.weights.lm_head {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemm_f16_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } if (2..=8).contains(&n_tokens) => self
                .kernels
                .gemm_q8_0_f16_exact_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemm_q8_0_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout: Nvfp4GgufLayout::RowMajor36,
            } if matches!(n_tokens, 2 | 4 | 8 | 16) => self.kernels.gemm_nvfp4_gguf_out_f32_batch(
                y_f32,
                buf,
                x,
                *rows,
                *cols,
                n_tokens,
                *output_scale,
                stream,
            ),
            // Q4_K/Q6_K heads have no batched GEMM-out-f32 kernel; a per-lane
            // dp4a GEMV sweep keeps the rest of the batched decode step intact
            // (the head is one weight read per lane, the layer stack stays
            // amortized across the batch).
            w @ (DevWeight::Q4K { rows, cols, .. } | DevWeight::Q6K { rows, cols, .. }) => {
                for lane in 0..n_tokens {
                    self.logits_weight_gemv(
                        y_f32,
                        lane * *rows * 4,
                        x,
                        lane * *cols * 2,
                        w,
                        stream,
                    )?;
                }
                Ok(())
            }
            _ => Err(ForgeError::Unsupported(
                "batchowa głowa logitów nie obsługuje tego formatu ani szerokości".into(),
            )),
        }
    }

    /// Smallest captured bucket >= `n`: a power of two, capped at `batch_cap`.
    /// A live batch replays the smallest bucket that holds it (dead lanes pad
    /// up to the bucket and are never sampled).
    fn bucket_for(&self, n: usize) -> usize {
        let mut s = 1;
        while s < n {
            s *= 2;
        }
        s.min(self.batch_cap).max(1)
    }

    /// Provision the continuous-batching decode scratch for up to `cap`
    /// sequences. Idempotent; a larger `cap` than a previous call reallocates.
    pub fn ensure_batch(&mut self, cap: usize) -> Result<()> {
        let cap = cap.max(1);
        if self.batch_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let nvfp4_ct_plan =
            nvfp4_ct_buffer_plan(cap, self.nvfp4_ct_model_capable());
        let matrix_cap = nvfp4_ct_plan.map_or(cap, |plan| plan.matrix_cap);
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let inter = p.intermediate_size;
        let vocab = p.vocab_size;
        let mpp = self.max_pages_per_seq;
        let max_seq = self.kv.cfg.max_pages_per_seq * self.kv.cfg.page_size;
        let dev = &self.device;
        let f16 = |elems: usize| dev.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let f32b = |elems: usize| dev.alloc(elems * 4, MemKind::Device, Pool::Activations);
        let pin = |bytes: usize| dev.alloc(bytes, MemKind::PinnedHost, Pool::Activations);
        self.batch_bufs = Some(BatchBufs {
            cap,
            h: f16(matrix_cap * hidden)?,
            x: f16(matrix_cap * hidden)?,
            q: f16(matrix_cap * q_dim)?,
            k: f16(matrix_cap * kv_dim)?,
            v: f16(matrix_cap * kv_dim)?,
            attn_parts: f32b(matrix_cap * p.n_heads * ATTN_DECODE_SPLITS * (p.max_head_dim() + 4))?,
            attn_out: f16(matrix_cap * q_dim)?,
            o_out: f16(matrix_cap * hidden)?,
            gate: f16(matrix_cap * inter)?,
            up: f16(matrix_cap * inter)?,
            act: f16(matrix_cap * inter)?,
            down: f16(matrix_cap * hidden)?,
            logits: f32b(matrix_cap * vocab)?,
            ids: f32b(cap)?,
            positions: f32b(cap)?,
            seq_lens: f32b(cap)?,
            page_table: f32b(cap * mpp)?,
            pinned_meta: pin(cap * 3 * 4)?,
            pinned_pt: pin(cap * mpp * 4)?,
            pinned_embed: pin(cap * hidden * 2)?,
            samp_k: f32b(cap)?,
            samp_inv_t: f32b(cap)?,
            samp_top_p: f32b(cap)?,
            samp_min_p: f32b(cap)?,
            samp_seed: dev.alloc(cap * 8, MemKind::Device, Pool::Activations)?,
            samp_step: dev.alloc(cap * 8, MemKind::Device, Pool::Activations)?,
            pinned_samp: pin(cap * (4 * 4 + 2 * 8))?,
            pen_ids: f32b(cap * max_seq)?,
            pen_counts: f32b(cap * max_seq)?,
            pen_offsets: f32b(cap + 1)?,
            pen_vals: f32b(cap)?,
            pen_frequency: f32b(cap)?,
            pen_presence: f32b(cap)?,
            pinned_pen_ids: pin(cap * max_seq * 4)?,
            pinned_pen_counts: pin(cap * max_seq * 4)?,
            pinned_pen_offsets: pin((cap + 1) * 4)?,
            pinned_pen_vals: pin(cap * 4)?,
            pinned_pen_frequency: pin(cap * 4)?,
            pinned_pen_presence: pin(cap * 4)?,
            out_ids: f32b(cap)?,
            pinned_out: pin(cap * 4)?,
            nvfp4_ct_qkv: nvfp4_ct_plan
                .map(|plan| f16(plan.qkv_elems))
                .transpose()?,
            nvfp4_ct_gate_up: nvfp4_ct_plan
                .map(|plan| f16(plan.gate_up_elems))
                .transpose()?,
            nvfp4_ct_workspace: nvfp4_ct_plan
                .map(|plan| f32b(plan.workspace_elems))
                .transpose()?,
        });
        // Fresh scratch invalidates any graph captured against the old buffers.
        self.batch_graphs.clear();
        self.batch_cap = cap;
        Ok(())
    }

    /// Wykonuje batch targetu hybrydowego ze wspólnymi GEMM FFN i głowy logits.
    /// Mixery (attention i DeltaNet) idą lane po lane, bo pula stanów aktywuje
    /// jeden lease naraz, a ich scratch jest jednotokenowy; batchują się norm,
    /// FFN i głowa logitów, czyli cała część ważona wagami. Stan każdego lane'a
    /// jest osobny i porządkowany na jednym streamie.
    fn record_hybrid_batch_forward(
        &mut self,
        seqs: &mut [&mut SeqKv],
        tokens: &[u32],
    ) -> Result<()> {
        let n = seqs.len();
        if n == 0 || tokens.len() != n {
            return Err(ForgeError::Unsupported(
                "hybrydowy batch targetu wymaga niepustego batcha i tokenu na lane".into(),
            ));
        }
        if n > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "hybrydowy batch {n} przekracza zarezerwowaną pojemność {}",
                self.batch_cap
            )));
        }
        self.ensure_hybrid_bufs()?;
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let mpp = self.max_pages_per_seq;
        let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
            ForgeError::Unsupported("target hybrydowy nie ma hostowego embeddingu".into())
        })?;
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let staging = bb
            .pinned_embed
            .host_ptr()
            .expect("pinned embedding ma mapowanie hosta");
        for (lane, &token) in tokens.iter().enumerate() {
            let row = table
                .get(token as usize * hidden..(token as usize + 1) * hidden)
                .ok_or_else(|| {
                    ForgeError::Scheduler(format!(
                        "token id {token} wykracza poza embedding targetu"
                    ))
                })?;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.as_ptr() as *const u8,
                    staging.add(lane * hidden * 2),
                    hidden * 2,
                );
            }
        }
        self.device
            .copy(&bb.pinned_embed, 0, &bb.h, 0, n * hidden * 2, &self.stream)?;
        self.kernels.rmsnorm_f16(
            &bb.x,
            &bb.h,
            &self.weights.layers[0].attn_norm,
            n,
            hidden,
            eps,
            &self.stream,
        )?;

        for layer_index in 0..self.weights.layers.len() {
            // Projekcje DeltaNet są bezstanowe, więc lecą raz dla całego batcha
            // z `bb.x` — jeden przebieg po wagach zamiast jednego na lane.
            if let LayerMixer::DeltaNet(delta) = &self.weights.layers[layer_index].mixer {
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.hybrid_delta_projections(delta, &bb.x, n)?;
            }
            for (lane, seq) in seqs.iter_mut().enumerate() {
                self.activate_hybrid_sequence(seq)?;
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.device.copy(
                    &bb.x,
                    lane * hidden * 2,
                    &self.bufs.x,
                    0,
                    hidden * 2,
                    &self.stream,
                )?;
                match &self.weights.layers[layer_index].mixer {
                    LayerMixer::DeepseekAttention(_) => {
                        unreachable!("ścieżka hybrydowa trafiła na warstwę DeepSeeka V4")
                    }
                    LayerMixer::Attention(attention) => {
                        self.device.copy(
                            &bb.positions,
                            lane * 4,
                            &self.bufs.pos,
                            0,
                            4,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &bb.seq_lens,
                            lane * 4,
                            &self.seq_len_dev,
                            0,
                            4,
                            &self.stream,
                        )?;
                        self.device.copy(
                            &bb.page_table,
                            lane * mpp * 4,
                            &self.page_table_dev,
                            0,
                            mpp * 4,
                            &self.stream,
                        )?;
                        self.hybrid_attn_mixer(layer_index, attention, &AttnSrc::Paged)?;
                    }
                    LayerMixer::DeltaNet(delta) => {
                        self.hybrid_delta_mixer(layer_index, delta, lane)?;
                    }
                }
                let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
                self.device.copy(
                    &self.bufs.o_out,
                    0,
                    &bb.o_out,
                    lane * hidden * 2,
                    hidden * 2,
                    &self.stream,
                )?;
            }

            let layer = &self.weights.layers[layer_index];
            let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
            self.kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.o_out,
                &layer.ffn_norm,
                n,
                hidden,
                eps,
                &self.stream,
            )?;
            let ffn = layer.dense_ffn()?;
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    self.gemm_rows(&bb.gate, weight, &bb.x, n, 0, inter, &self.stream)?;
                    self.gemm_rows(&bb.up, weight, &bb.x, n, inter, inter, &self.stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&bb.gate, gate, &bb.x, n, &self.stream)?;
                    self.gemm(&bb.up, up, &bb.x, n, &self.stream)?;
                }
            }
            self.kernels
                .glu_mul_f16(self.ffn_act(), &bb.act, &bb.gate, &bb.up, n * inter, &self.stream)?;
            self.gemm(&bb.down, &ffn.down, &bb.act, n, &self.stream)?;
            let next_norm = if layer_index + 1 < self.weights.layers.len() {
                &self.weights.layers[layer_index + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            self.kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.down,
                next_norm,
                n,
                hidden,
                eps,
                &self.stream,
            )?;
        }
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        self.logits_gemm(&bb.logits, &bb.x, n, &self.stream)
    }

    /// Record one batched forward + logit head over `n` rows into the model
    /// stream (no sampling — that runs param-dependent, outside the graph).
    /// Mirrors the prefill dataflow (rmsnorm rows=n, batched GEMM projections,
    /// row-batched silu/residual) but swaps causal prefill attention for the
    /// per-sequence paged flash-decode. Lanes `0..resident` attend through
    /// their page tables in one launch; `streamed` lanes (packed at the tail
    /// of the batch: spilled KV that exceeds free VRAM) attend one at a time
    /// over the tier staging slabs holding their full context per layer. A
    /// batch with streamed lanes is never graph-captured; pure-resident
    /// buckets stay captured (`streamed` empty, `resident == n`).
    fn record_batch_forward(
        &self,
        n: usize,
        resident: usize,
        streamed: &[(usize, &SeqKv)],
    ) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let eps = p.rms_norm_eps;
        let scale = p.attn_scale_at(0);
        let kernels = &self.kernels;
        let stream = &self.stream;
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let n_layers = self.weights.layers.len();

        kernels.gather_rows_f16(
            &bb.h,
            &self.weights.token_embd_f16,
            &bb.ids,
            n,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &bb.x,
            &bb.h,
            &self.weights.layers[0].attn_norm,
            n,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            let mut segmented_qkv = false;
            // Raw q/k/v projections (no norm/rope here — attn_decode_split folds
            // the q/k-norm + RoPE + paged append into its per-seq prologue).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    if let (Some(qkv), Some(workspace)) =
                        (&bb.nvfp4_ct_qkv, &bb.nvfp4_ct_workspace)
                    {
                        segmented_qkv = self.gemm_nvfp4_ct_direct(
                            qkv,
                            workspace,
                            w,
                            &bb.x,
                            n,
                            Nvfp4CtProjection::Qkv,
                            stream,
                        )?;
                    }
                    if !segmented_qkv {
                        self.gemm_rows(&bb.q, w, &bb.x, n, 0, q_dim, stream)?;
                        self.gemm_rows(&bb.k, w, &bb.x, n, q_dim, kv_dim, stream)?;
                        self.gemm_rows(&bb.v, w, &bb.x, n, q_dim + kv_dim, kv_dim, stream)?;
                    }
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&bb.q, qk, &bb.x, n, 0, q_dim, stream)?;
                    self.gemm_rows(&bb.k, qk, &bb.x, n, q_dim, kv_dim, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemm(&bb.q, q, &bb.x, n, stream)?;
                    self.gemm(&bb.k, k, &bb.x, n, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
            }
            let (q_input, q_offset, k_input, k_offset, v_input, v_offset) =
                if segmented_qkv {
                    let qkv = bb
                        .nvfp4_ct_qkv
                        .as_ref()
                        .expect("segmentowany QKV wymaga bufora padded");
                    let physical_m = nvfp4_ct_physical_m(n)
                        .expect("segmentowany QKV ma fizyczny kafel");
                    (
                        qkv,
                        0,
                        qkv,
                        physical_m * q_dim * 2,
                        qkv,
                        physical_m * (q_dim + kv_dim) * 2,
                    )
                } else {
                    (&bb.q, 0, &bb.k, 0, &bb.v, 0)
                };
            if resident > 0 {
                kernels.attn_decode_split(
                    &bb.attn_parts,
                    q_input,
                    q_offset,
                    k_input,
                    k_offset,
                    v_input,
                    v_offset,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[self.target_kv_layer(l)],
                    &self.kv.v[self.target_kv_layer(l)],
                    &bb.page_table,
                    &bb.seq_lens,
                    &bb.positions,
                    resident,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &bb.attn_out,
                    &bb.attn_parts,
                    resident,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
            }
            for &(lane, seq) in streamed {
                // Lane-scalar pos/len land in the single-seq buffers the
                // n_seqs=1 launch reads at index 0; the lane's q/k/v rows are
                // addressed by byte offset. The attention appends the token
                // into staging and the tail page mirrors back to the canonical
                // slab, exactly like the single-stream staged step. All copies
                // and launches ride the compute stream, so slab reuse across
                // lanes is stream-ordered.
                let tier = self.tier.as_ref().expect("streamed lanes require tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                let db = &self.bufs;
                self.device
                    .copy(&bb.positions, lane * 4, &db.pos, 0, 4, stream)?;
                self.device
                    .copy(&bb.seq_lens, lane * 4, &self.seq_len_dev, 0, 4, stream)?;
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_split(
                    &db.attn_parts,
                    q_input,
                    q_offset + lane * q_dim * 2,
                    k_input,
                    k_offset + lane * kv_dim * 2,
                    v_input,
                    v_offset + lane * kv_dim * 2,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    &db.pos,
                    1,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &db.attn_out,
                    &db.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
                self.device.copy(
                    &db.attn_out,
                    0,
                    &bb.attn_out,
                    lane * q_dim * 2,
                    q_dim * 2,
                    stream,
                )?;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &slot.stage[0],
                    lp * rb,
                    &self.kv.k[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &slot.stage[1],
                    lp * rb,
                    &self.kv.v[self.target_kv_layer(l)],
                    phys * rb,
                    rb,
                    stream,
                )?;
            }
            let mut specialized_o = false;
            if let Some(workspace) = &bb.nvfp4_ct_workspace {
                specialized_o = self.gemm_nvfp4_ct_direct(
                    &bb.o_out,
                    workspace,
                    &layer.attn().attn_o,
                    &bb.attn_out,
                    n,
                    Nvfp4CtProjection::Output,
                    stream,
                )?;
            }
            if !specialized_o {
                self.gemm(&bb.o_out, &layer.attn().attn_o, &bb.attn_out, n, stream)?;
            }
            kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.o_out,
                &layer.ffn_norm,
                n,
                hidden,
                eps,
                stream,
            )?;

            let mut segmented_gate_up = false;
            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    if let (Some(gate_up), Some(workspace)) =
                        (&bb.nvfp4_ct_gate_up, &bb.nvfp4_ct_workspace)
                    {
                        segmented_gate_up = self.gemm_nvfp4_ct_direct(
                            gate_up,
                            workspace,
                            w,
                            &bb.x,
                            n,
                            Nvfp4CtProjection::GateUp,
                            stream,
                        )?;
                    }
                    if !segmented_gate_up {
                        self.gemm_rows(&bb.gate, w, &bb.x, n, 0, inter, stream)?;
                        self.gemm_rows(&bb.up, w, &bb.x, n, inter, inter, stream)?;
                    }
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&bb.gate, gate, &bb.x, n, stream)?;
                    self.gemm(&bb.up, up, &bb.x, n, stream)?;
                }
            }
            if segmented_gate_up {
                let physical_m = nvfp4_ct_physical_m(n)
                    .expect("segmentowany GateUp ma fizyczny kafel");
                kernels.glu_mul_f16_at(self.ffn_act(), 
                    &bb.act,
                    bb.nvfp4_ct_gate_up
                        .as_ref()
                        .expect("segmentowany GateUp wymaga bufora padded"),
                    0,
                    physical_m * inter * 2,
                    n * inter,
                    stream,
                )?;
            } else {
                kernels.glu_mul_f16(self.ffn_act(), &bb.act, &bb.gate, &bb.up, n * inter, stream)?;
            }
            let down = &layer.dense_ffn()?.down;
            let mut specialized_down = false;
            if let Some(workspace) = &bb.nvfp4_ct_workspace {
                specialized_down = self.gemm_nvfp4_ct_direct(
                    &bb.down,
                    workspace,
                    down,
                    &bb.act,
                    n,
                    Nvfp4CtProjection::Down,
                    stream,
                )?;
            }
            if !specialized_down {
                self.gemm(&bb.down, down, &bb.act, n, stream)?;
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels
                .rmsnorm_residual_f16(&bb.x, &bb.h, &bb.down, next_norm, n, hidden, eps, stream)?;
        }

        self.logits_gemm(&bb.logits, &bb.x, n, stream)
    }

    /// Capture `record_batch_forward(bucket)` into a replayable graph.
    fn capture_batch_forward(&self, bucket: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.record_batch_forward(bucket, bucket, &[]) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// Run one batched decode step: advance every sequence in `seqs` by its
    /// input token in `tokens`, sampling each successor on the GPU with its own
    /// params. Returns the `B` next-token ids. The forward+logit head replays a
    /// per-bucket CUDA graph (dead lanes padded to the bucket, never sampled);
    /// sampling runs after the replay so per-seq params (and the greedy/top-k
    /// mix) need no re-capture.
    pub fn batched_decode(
        &mut self,
        seqs: &mut [&mut SeqKv],
        tokens: &[u32],
        params: &[SeqSampleParams],
    ) -> Result<Vec<u32>> {
        self.ensure_kv_reuse_healthy()?;
        let b = seqs.len();
        if b == 0 {
            return Ok(Vec::new());
        }
        if tokens.len() != b || params.len() != b {
            return Err(ForgeError::Scheduler(
                "batched_decode: seqs/tokens/params length mismatch".into(),
            ));
        }
        if self.is_hybrid() && !self.hybrid_batch_capable() {
            return Err(ForgeError::Unsupported(
                "hybrydowy batch nie spełnia kontraktu modelu lub pamięci KV".into(),
            ));
        }
        // Rot modes commit each appended token into the packed low-bit store on
        // the single-stream decode path only; the batched path would append to
        // the f16 slab without packing, leaving the packed store stale. Refuse
        // rather than read a stale store. (Batched rot decode is a follow-up.)
        if self.kv.cfg.quant.is_rot() {
            return Err(ForgeError::Unsupported(
                "rotational KV (rot4/rot3) supports single-stream decode only; \
                 disable batching for this model"
                    .into(),
            ));
        }
        // MoE routing chooses experts per token from a host readback, so the
        // batched forward cannot be graph-captured; MoE decodes one sequence at
        // a time (batched grouped-GEMM MoE is a tracked follow-up).
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "MoE models support single-stream decode only; disable batching".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        for (seq, &token) in seqs.iter().zip(tokens) {
            if seq.len >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    seq.len, p.max_position_embeddings
                )));
            }
            if token as usize >= p.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token id {token} exceeds model vocabulary {}",
                    p.vocab_size
                )));
            }
        }
        let growth_pages = self.kv.batch_growth_pages(seqs.iter().map(|seq| &**seq))?;
        // Batched growth appends pages without refreshing the single-stream
        // page table; invalidate it so the next single-stream step re-uploads.
        self.pt_seq = 0;
        if self.tier.is_some() {
            // Spilled sequences that fit back into free pages are restored
            // (plain fits-check, no reserve: restoring beats streaming when
            // possible); the rest stay streamed and join the batch through
            // the tier staging attention. The balance pass then guarantees a
            // free page per lane's potential boundary growth, spilling the
            // globally coldest prefixes — after it, lane residency is fixed.
            for seq in seqs.iter_mut() {
                if !seq.spilled.is_empty() && seq.spilled_page_count() <= self.kv.free_page_count()
                {
                    self.tier_restore_or_recompute(seq)?;
                }
            }
            self.tier_balance(seqs, b)?;
        }
        self.ensure_batch(b)?;
        // Streamed lanes (spilled KV) pack at the tail of the lane order: the
        // batch-wide paged attention launch covers exactly the leading
        // resident lanes, and each streamed lane attends over the staging
        // slabs. A mixed batch runs uncaptured at its exact size; a
        // pure-resident batch replays the per-bucket graph (dead lanes
        // padded).
        let mut order: Vec<usize> = (0..b).collect();
        order.sort_by_key(|&i| !seqs[i].spilled.is_empty());
        let resident = seqs.iter().filter(|s| s.spilled.is_empty()).count();
        let mixed = resident < b;
        let bucket = if mixed { b } else { self.bucket_for(b) };
        if b > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "batch {b} exceeds provisioned cap {}",
                self.batch_cap
            )));
        }

        // Reclaim cached prefix pages if the free stack cannot cover a boundary
        // page for every lane (no-op when the prefix cache is inactive/empty).
        self.ensure_free_pages(growth_pages);
        if growth_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "batch KV growth needs {growth_pages} pages, cache has {} free",
                self.kv.free_page_count()
            )));
        }
        if self.tier.is_some() {
            for (seq, &tok) in seqs.iter_mut().zip(tokens) {
                seq.tokens.push(tok);
            }
        }

        // Grow each sequence by one token and gather its position/page table
        // in lane order. Streamed lanes' page tables keep -1 for spilled
        // pages; only the identity-table staging path reads their context.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; bucket * 3]; // [ids | positions | seq_lens]
        let mut pt = vec![-1i32; bucket * mpp];
        for (lane, &i) in order.iter().enumerate() {
            let seq = &mut *seqs[i];
            let pos = seq.len;
            self.kv.grow(seq)?;
            meta[lane] = tokens[i] as i32;
            meta[bucket + lane] = pos as i32;
            meta[2 * bucket + lane] = (pos + 1) as i32;
            pt[lane * mpp..lane * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        // Dead lanes replay sequence 0's inputs so they compute harmlessly
        // (captured path only; the mixed path runs at its exact size).
        if !mixed {
            let lane0_pt: Vec<i32> = pt[..mpp].to_vec();
            for i in b..bucket {
                meta[i] = meta[0];
                meta[bucket + i] = meta[bucket];
                meta[2 * bucket + i] = meta[2 * bucket];
                pt[i * mpp..i * mpp + mpp].copy_from_slice(&lane0_pt);
            }
        }

        let bb = self.batch_bufs.as_ref().expect("provisioned above");
        // Upload meta (ids/positions/seq_lens) and the page table via pinned H2D.
        let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, bucket * 3 * 4);
        }
        self.device
            .copy(&bb.pinned_meta, 0, &bb.ids, 0, bucket * 4, &self.stream)?;
        self.device.copy(
            &bb.pinned_meta,
            bucket * 4,
            &bb.positions,
            0,
            bucket * 4,
            &self.stream,
        )?;
        self.device.copy(
            &bb.pinned_meta,
            2 * bucket * 4,
            &bb.seq_lens,
            0,
            bucket * 4,
            &self.stream,
        )?;
        let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, bucket * mpp * 4);
        }
        self.device.copy(
            &bb.pinned_pt,
            0,
            &bb.page_table,
            0,
            bucket * mpp * 4,
            &self.stream,
        )?;

        if self.is_hybrid() {
            if mixed {
                return Err(ForgeError::Unsupported(
                    "hybrydowy batch B2 nie obsługuje tieringu KV".into(),
                ));
            }
            self.record_hybrid_batch_forward(seqs, tokens)?;
            self.pt_seq = 0;
        } else if mixed {
            let tier = self.tier.as_mut().expect("mixed batch requires tiering");
            for &i in &order[resident..] {
                tier.prepare_streaming(seqs[i])?;
            }
            let streamed: Vec<(usize, &SeqKv)> = order[resident..]
                .iter()
                .enumerate()
                .map(|(j, &i)| (resident + j, &*seqs[i]))
                .collect();
            self.record_batch_forward(b, resident, &streamed)?;
        } else {
            // Replay the bucket's forward+logits graph (capture on first use).
            if !self.batch_graphs.contains_key(&bucket) {
                let g = self.capture_batch_forward(bucket)?;
                self.batch_graphs.insert(bucket, g);
            }
            let graph = self.batch_graphs.get(&bucket).expect("captured").clone();
            self.device.launch_graph(&graph, &self.stream)?;
        }

        // Sample the B live rows on the GPU (outside the graph so the per-seq
        // param mix is free), in lane order. Greedy-only batches take the
        // argmax fast path.
        let lane_params: Vec<SeqSampleParams> = order.iter().map(|&i| params[i].clone()).collect();
        let logits = self
            .batch_bufs
            .as_ref()
            .expect("provisioned")
            .logits
            .clone();
        self.batch_sample_from(&logits, b, &lane_params)?;

        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device
            .copy(&bb.out_ids, 0, &bb.pinned_out, 0, b * 4, &self.stream)?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(op, b) };
        let mut out = vec![0u32; b];
        for (lane, &i) in order.iter().enumerate() {
            let id = ids[lane];
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "batched sampler returned out-of-range token {id} for seq {i}"
                )));
            }
            out[i] = id as u32;
        }
        Ok(out)
    }

    /// Whether one scheduler iteration may fold `b` decode rows into a dense
    /// prefill chunk forward (`mixed_prefill_decode_step`).
    pub fn mixed_step_capable(&self, b: usize) -> bool {
        // The head runs at b(+1) rows — an arbitrary count, so it must be a
        // format whose batched logits path takes any row count (F16/Q8_0
        // GEMMs, Q4_K/Q6_K per-lane GEMV). NvFp4Gguf heads only have
        // power-of-two batch kernels and keep the two-phase iteration.
        let head_ok = matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. }
                | DevWeight::Q8_0 { .. }
                | DevWeight::Q4K { .. }
                | DevWeight::Q6K { .. }
        );
        b > 0
            && b <= self.batch_cap.max(1)
            && head_ok
            && !self.is_hybrid()
            && !self.weights.is_moe()
            && !self.kv.cfg.quant.is_rot()
            && self.tier.is_none()
            && self.calib.is_none()
    }

    /// One mixed step: the `b` decode sequences' tokens ride the prefill
    /// chunk's GEMMs/norms as extra rows (decode attention runs through the
    /// fused split kernel over the batch metadata), so a long prompt no longer
    /// stalls decode. Returns the decode sequences' next tokens and — when the
    /// chunk completes the prompt — the prefill sequence's first token.
    #[allow(clippy::too_many_arguments)]
    pub fn mixed_prefill_decode_step(
        &mut self,
        decode_seqs: &mut [&mut SeqKv],
        decode_tokens: &[u32],
        decode_params: &[SeqSampleParams],
        prefill_seq: &mut SeqKv,
        chunk: &[u32],
        final_params: Option<SeqSampleParams>,
    ) -> Result<(Vec<u32>, Option<u32>)> {
        self.ensure_kv_reuse_healthy()?;
        let b = decode_seqs.len();
        if !self.mixed_step_capable(b) || chunk.is_empty() {
            return Err(ForgeError::Unsupported(
                "mixed step nie spełnia kontraktu modelu".into(),
            ));
        }
        if decode_tokens.len() != b || decode_params.len() != b {
            return Err(ForgeError::Scheduler(
                "mixed step: seqs/tokens/params length mismatch".into(),
            ));
        }
        let p = self.weights.descriptor.params.clone();
        for (seq, &token) in decode_seqs.iter().zip(decode_tokens) {
            if seq.len >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {} exceeds model context {}",
                    seq.len, p.max_position_embeddings
                )));
            }
            if token as usize >= p.vocab_size {
                return Err(ForgeError::Scheduler(format!(
                    "token id {token} exceeds model vocabulary {}",
                    p.vocab_size
                )));
            }
            if !seq.spilled.is_empty() {
                return Err(ForgeError::Unsupported(
                    "mixed step nie obsługuje spillowanych sekwencji".into(),
                ));
            }
        }
        let growth_pages = self
            .kv
            .batch_growth_pages(decode_seqs.iter().map(|seq| &**seq))?;
        self.pt_seq = 0;
        self.ensure_batch(b)?;
        self.ensure_free_pages(growth_pages);
        if growth_pages > self.kv.free_page_count() {
            return Err(ForgeError::Scheduler(format!(
                "mixed step: wzrost KV wymaga {growth_pages} stron, wolnych {}",
                self.kv.free_page_count()
            )));
        }

        // Grow each decode sequence by one token; upload ids into the mixed
        // rows and positions/seq_lens/page tables into the batch buffers the
        // fused decode attention reads.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; b * 2]; // [positions | seq_lens]
        let mut pt = vec![-1i32; b * mpp];
        let mut ids = Vec::with_capacity(b);
        for (lane, seq) in decode_seqs.iter_mut().enumerate() {
            let pos = seq.len;
            self.kv.grow(seq)?;
            ids.push(decode_tokens[lane] as i32);
            meta[lane] = pos as i32;
            meta[b + lane] = (pos + 1) as i32;
            pt[lane * mpp..lane * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        {
            // Pinned H2D like `batched_decode` — pageable `device.write`
            // staggers the stream with synchronous staging copies.
            let bb = self.batch_bufs.as_ref().expect("provisioned above");
            let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, b * 2 * 4);
            }
            self.device
                .copy(&bb.pinned_meta, 0, &bb.positions, 0, b * 4, &self.stream)?;
            self.device
                .copy(&bb.pinned_meta, b * 4, &bb.seq_lens, 0, b * 4, &self.stream)?;
            let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, b * mpp * 4);
            }
            self.device
                .copy(&bb.pinned_pt, 0, &bb.page_table, 0, b * mpp * 4, &self.stream)?;
        }

        let mixed = MixedDecodeRows { b, ids };
        let t = self.prefill_forward_lanes(
            &mut [prefill_seq],
            &[chunk],
            false,
            Some(&mixed),
        )?;

        // Logits: decode rows [t..t+b] (+ the chunk's last row when the prompt
        // completes) copied into the batch scratch, one GEMM, batched sampling.
        let hidden = p.hidden_size;
        let row_bytes = hidden * 2;
        let sample_rows = b + usize::from(final_params.is_some());
        {
            let pb = self.prefill_bufs.as_ref().expect("prefill bufs live");
            let bb = self.batch_bufs.as_ref().expect("provisioned above");
            self.device.copy(
                &pb.x,
                t * row_bytes,
                &bb.x,
                0,
                b * row_bytes,
                &self.stream,
            )?;
            if final_params.is_some() {
                self.device.copy(
                    &pb.x,
                    (t - 1) * row_bytes,
                    &bb.x,
                    b * row_bytes,
                    row_bytes,
                    &self.stream,
                )?;
            }
            let logits = bb.logits.clone();
            self.logits_gemm(&logits, &bb.x, sample_rows, &self.stream)?;
        }
        let mut lane_params: Vec<SeqSampleParams> = decode_params.to_vec();
        if let Some(fp) = final_params {
            lane_params.push(fp);
        }
        let logits = self
            .batch_bufs
            .as_ref()
            .expect("provisioned")
            .logits
            .clone();
        self.batch_sample_from(&logits, sample_rows, &lane_params)?;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device.copy(
            &bb.out_ids,
            0,
            &bb.pinned_out,
            0,
            sample_rows * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let raw = unsafe { std::slice::from_raw_parts(op, sample_rows) };
        let mut out = Vec::with_capacity(b);
        for (lane, &id) in raw.iter().take(b).enumerate() {
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "mixed sampler returned out-of-range token {id} for lane {lane}"
                )));
            }
            out.push(id as u32);
        }
        let final_id = if sample_rows > b {
            let id = raw[b];
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "mixed sampler returned out-of-range prompt token {id}"
                )));
            }
            Some(id as u32)
        } else {
            None
        };
        Ok((out, final_id))
    }

    /// Odczytuje pełne logity pozostałe po ostatnim kroku ścieżki
    /// jednosekwencyjnej (`prefill_chunk` / `step_and_sample`). Symetryczne do
    /// `read_batch_logits` i służy temu samemu celowi: porównaniu numerycznemu
    /// obu ścieżek decode, które używają różnych kerneli.
    pub fn read_single_logits(&self) -> Result<Vec<f32>> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut logits = vec![0.0f32; vocab];
        self.device
            .read(&self.bufs.logits, 0, bytemuck::cast_slice_mut(&mut logits))?;
        Ok(logits)
    }

    /// Odczytuje pełne logity pozostałe po ostatnim dense batch decode.
    ///
    /// Metoda służy do audytu numerycznego. Bez tieringu wiersze zachowują
    /// kolejność lane'ów przekazaną do `batched_decode`.
    pub fn read_batch_logits(&self, batch: usize) -> Result<Vec<f32>> {
        let buffers = self
            .batch_bufs
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("brak buforów batch decode".into()))?;
        if batch == 0 || batch > buffers.cap {
            return Err(ForgeError::Scheduler(format!(
                "odczyt logitów wymaga batch 1..={}, otrzymano {batch}",
                buffers.cap
            )));
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut logits = vec![0.0f32; batch * vocab];
        self.device
            .read(&buffers.logits, 0, bytemuck::cast_slice_mut(&mut logits))?;
        Ok(logits)
    }

    /// GPU sampling over `b` contiguous live rows of device logits.
    fn batch_sample_from(
        &mut self,
        logits: &DevBuffer,
        b: usize,
        params: &[SeqSampleParams],
    ) -> Result<()> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        let stream = &self.stream;

        // Jedno uruchomienie kernela obsługuje wszystkie aktywne kary batcha.
        let any_penalty = params.iter().any(|p| !p.penalty_ids.is_empty());
        if any_penalty {
            let mut ids_flat: Vec<i32> = Vec::new();
            let mut counts_flat: Vec<i32> = Vec::new();
            let mut offsets: Vec<i32> = Vec::with_capacity(b + 1);
            let mut vals: Vec<f32> = Vec::with_capacity(b);
            let mut frequency: Vec<f32> = Vec::with_capacity(b);
            let mut presence: Vec<f32> = Vec::with_capacity(b);
            offsets.push(0);
            for p in params.iter() {
                if p.penalty_ids.len() != p.penalty_counts.len() {
                    return Err(ForgeError::Scheduler(
                        "penalty histogram ids/counts length mismatch".into(),
                    ));
                }
                ids_flat.extend_from_slice(&p.penalty_ids);
                counts_flat.extend_from_slice(&p.penalty_counts);
                offsets.push(ids_flat.len() as i32);
                vals.push(p.penalty);
                frequency.push(p.frequency_penalty);
                presence.push(p.presence_penalty);
            }
            if ids_flat.len() * 4 > bb.pinned_pen_ids.len() {
                return Err(ForgeError::Scheduler("penalty id staging overflow".into()));
            }
            Self::stage(
                &self.device,
                &bb.pinned_pen_ids,
                &bb.pen_ids,
                bytemuck::cast_slice(&ids_flat),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_counts,
                &bb.pen_counts,
                bytemuck::cast_slice(&counts_flat),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_offsets,
                &bb.pen_offsets,
                bytemuck::cast_slice(&offsets),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_vals,
                &bb.pen_vals,
                bytemuck::cast_slice(&vals),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_frequency,
                &bb.pen_frequency,
                bytemuck::cast_slice(&frequency),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_presence,
                &bb.pen_presence,
                bytemuck::cast_slice(&presence),
                stream,
            )?;
            self.kernels.sample_batched_penalize_f32(
                logits,
                vocab,
                &bb.pen_ids,
                &bb.pen_counts,
                &bb.pen_offsets,
                &bb.pen_vals,
                &bb.pen_frequency,
                &bb.pen_presence,
                b,
                stream,
            )?;
        }

        if params.iter().all(|p| p.greedy) {
            self.kernels
                .sample_batched_argmax_f32(&bb.out_ids, logits, b, vocab, stream)?;
            return Ok(());
        }

        // Mixed / sampled batch: per-seq top-k (k = 1 lanes reproduce argmax).
        let mut ks = Vec::with_capacity(b);
        let mut inv_t = Vec::with_capacity(b);
        let mut top_p = Vec::with_capacity(b);
        let mut min_p = Vec::with_capacity(b);
        let mut seed = Vec::with_capacity(b);
        let mut step = Vec::with_capacity(b);
        for p in params.iter() {
            ks.push(p.k);
            inv_t.push(p.inv_t);
            top_p.push(p.top_p);
            min_p.push(p.min_p);
            seed.push(p.seed);
            step.push(p.step);
        }
        // Params staged into one pinned block, then copied per array.
        let host = bb.pinned_samp.host_ptr().expect("pinned mapping");
        let mut off = 0usize;
        let put = |bytes: &[u8], off: &mut usize| unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host.add(*off), bytes.len());
            *off += bytes.len();
        };
        put(bytemuck::cast_slice(&ks), &mut off);
        put(bytemuck::cast_slice(&inv_t), &mut off);
        put(bytemuck::cast_slice(&top_p), &mut off);
        put(bytemuck::cast_slice(&min_p), &mut off);
        put(bytemuck::cast_slice(&seed), &mut off);
        put(bytemuck::cast_slice(&step), &mut off);
        let mut o = 0usize;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_k, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_inv_t, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_top_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_min_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_seed, 0, b * 8, stream)?;
        o += b * 8;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_step, 0, b * 8, stream)?;
        self.kernels.sample_batched_topk_f32(
            &bb.out_ids,
            logits,
            b,
            vocab,
            &bb.samp_k,
            &bb.samp_inv_t,
            &bb.samp_top_p,
            &bb.samp_min_p,
            &bb.samp_seed,
            &bb.samp_step,
            stream,
        )
    }

    /// Copy `bytes` into a pinned staging buffer and enqueue the H2D to its
    /// device buffer on `stream`.
    fn stage(
        device: &Arc<dyn Device>,
        pinned: &DevBuffer,
        dev: &DevBuffer,
        bytes: &[u8],
        stream: &Stream,
    ) -> Result<()> {
        let host = pinned.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host, bytes.len());
        }
        device.copy(pinned, 0, dev, 0, bytes.len(), stream)
    }
}

#[cfg(test)]
mod verify_rollback_tests {
    use super::{
        apply_mtp_pair_metadata_commit, fatal_kv_synchronize, finish_greedy_verification,
        cleanup_after_error,
        grow_prefill_lanes_transactional, hybrid_layer_major_scratch_estimate,
        hybrid_prefill_activation_budget_capable, hybrid_prefill_chunk_config_for_model,
        hybrid_prefill_inner_chunk_count, hybrid_prefill_nvfp4_chunk_limit,
        hybrid_prefill_profile_spans, hybrid_prefill_scratch_estimate, hybrid_prefill_step_size,
        hybrid_prefill_t128_backend_capable, hybrid_q_full_cols,
        hybrid_verify_attention_parts_bytes, hybrid_verify_dedicated_z_bytes,
        hybrid_verify_delta_scratch_instances, hybrid_verify_scratch_estimate, logical_kv_regions,
        native_mtp_b2_device_embedding, native_mtp_greedy_decision,
        nvfp4_ct_buffer_plan, nvfp4_ct_dimensions_capable,
        nvfp4_ct_physical_m, nvfp4_ct_plan_physical_m, nvfp4_ct_projection_for_shape,
        parse_hybrid_prefill_chunk_config, resolve_hybrid_prefill_chunk_size, restore_after,
        restore_prefill_seq_snapshots, rollback_mtp_pair, run_dense_prefill_transaction,
        settle_kv_operation, validate_mtp_pair_metadata_commit, validate_mtp_routed_inputs,
        DeltaStateLayout, ForgeError, HybridPrefillChunkConfig, HybridPrefillScratchShape,
        HybridStatePool, KvCache, KvConfig, KvQuant, KvReusePoison, LayerKind, Model,
        Nvfp4CtProjection, Nvfp4GgufLayout, Result, SeqKv, Vendor,
    };
    use forge_hal::cpu::CpuDevice;
    use forge_hal::Device;
    use std::sync::Arc;

    #[test]
    fn cleanup_buildera_fp8_zachowuje_pierwotny_blad() {
        let mut cleanup_called = false;
        let result: Result<()> = cleanup_after_error(
            Err(ForgeError::Kernel("pierwotny błąd pakowania".into())),
            || cleanup_called = true,
        );
        assert!(cleanup_called);
        assert!(matches!(
            result,
            Err(ForgeError::Kernel(message)) if message == "pierwotny błąd pakowania"
        ));
    }

    #[test]
    fn nvfp4_ct_rozpoznaje_tylko_dokladne_ksztalty_bielika() {
        assert_eq!(
            nvfp4_ct_projection_for_shape(6144, 4096),
            Some(Nvfp4CtProjection::Qkv)
        );
        assert_eq!(
            nvfp4_ct_projection_for_shape(4096, 4096),
            Some(Nvfp4CtProjection::Output)
        );
        assert_eq!(
            nvfp4_ct_projection_for_shape(22528, 4096),
            Some(Nvfp4CtProjection::GateUp)
        );
        assert_eq!(
            nvfp4_ct_projection_for_shape(4096, 11264),
            Some(Nvfp4CtProjection::Down)
        );
        for shape in [
            (6143, 4096),
            (6144, 4095),
            (4096, 6144),
            (11264, 4096),
            (22528, 4095),
        ] {
            assert_eq!(nvfp4_ct_projection_for_shape(shape.0, shape.1), None);
        }
    }

    #[test]
    fn nvfp4_ct_wybiera_kafel_bm16_lub_bm32_tylko_dla_zmierzonych_m() {
        for logical_m in [4, 8, 16] {
            assert_eq!(nvfp4_ct_physical_m(logical_m), Some(16));
        }
        for logical_m in [24, 32] {
            assert_eq!(nvfp4_ct_physical_m(logical_m), Some(32));
        }
        for logical_m in [0, 1, 2, 3, 5, 15, 17, 23, 25, 31, 33, 64] {
            assert_eq!(nvfp4_ct_physical_m(logical_m), None);
        }
    }

    #[test]
    fn nvfp4_ct_wymaga_dokladnego_podzialu_qkv() {
        assert!(nvfp4_ct_dimensions_capable(
            4096, 4096, 1024, 11264
        ));
        assert!(!nvfp4_ct_dimensions_capable(
            4096, 5120, 512, 11264
        ));
        assert!(!nvfp4_ct_dimensions_capable(
            4096, 4096, 1024, 14336
        ));
    }

    #[test]
    fn nvfp4_ct_planuje_bufory_pod_najwiekszy_osiagalny_kafel() {
        assert_eq!(nvfp4_ct_buffer_plan(4, false), None);
        let plan = nvfp4_ct_buffer_plan(4, true).unwrap();
        assert_eq!(plan.physical_m, 16);
        assert_eq!(plan.matrix_cap, 16);
        assert_eq!(plan.qkv_elems, 16 * 6144);
        assert_eq!(plan.gate_up_elems, 16 * 22528);
        assert_eq!(plan.workspace_elems, 4 * 16 * 6144);
        // Kafel M32 jest osiągalny dopiero od cap 24; niżej bufory zostają M16.
        assert_eq!(nvfp4_ct_plan_physical_m(23), 16);
        assert_eq!(nvfp4_ct_plan_physical_m(24), 32);
        let wide = nvfp4_ct_buffer_plan(32, true).unwrap();
        assert_eq!(wide.physical_m, 32);
        assert_eq!(wide.matrix_cap, 32);
        assert_eq!(wide.qkv_elems, 32 * 6144);
        assert_eq!(wide.gate_up_elems, 32 * 22528);
        assert_eq!(wide.workspace_elems, 4 * 32 * 6144);
    }

    #[test]
    fn fed_routed_mtp_ponad_i32_konczy_sie_przed_mutacja() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (states, kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");
        let lengths = [states[0].seq.len, states[1].seq.len];
        let checkpoints = [states[0].checkpoint_len(), states[1].checkpoint_len()];
        let free_pages = kv.free_page_count();
        let result = validate_mtp_routed_inputs(usize::MAX, [u32::MAX, 7], 2, [None, None]);
        assert!(matches!(result, Err(ForgeError::Format(_))));
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );
        assert_eq!(kv.free_page_count(), free_pages);
        pool.restore_mtp_pair(leases, states, kv)
            .expect("niezmieniona para powinna wrócić do puli");
    }

    #[test]
    fn jawny_tile_nvfp4_nie_ma_cichego_fallbacku() {
        assert!(!Model::nvfp4_tile_requested(Nvfp4GgufLayout::RowMajor36, false).unwrap());
        assert!(Model::nvfp4_tile_requested(Nvfp4GgufLayout::TileN128K64, true).unwrap());
        assert!(Model::nvfp4_tile_requested(Nvfp4GgufLayout::TileN128K64, false).is_err());
        assert!(Model::validate_nvfp4_tile_repacked(true, 0).is_err());
        assert!(Model::validate_nvfp4_tile_repacked(true, 1).is_ok());
        assert!(Model::validate_nvfp4_tile_repacked(false, 0).is_ok());
    }

    #[test]
    fn pula_stanow_izoluje_przeplatane_sekwencje_i_zeruje_reuzyty_slot() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device.clone(),
            vec![LayerKind::DeltaNet, LayerKind::Attention],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
        )
        .expect("pula powinna powstać");
        let first = pool.acquire().expect("pierwszy lease powinien powstać");
        let second = pool.acquire().expect("drugi lease powinien powstać");
        assert_ne!(first.slot, second.slot);

        pool.activate(first, &stream)
            .expect("pierwszy stan powinien się aktywować");
        let first_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .write(&[7; 16], &first_state.state, 0)
            .expect("zapis pierwszego stanu powinien się udać");

        pool.activate(second, &stream)
            .expect("drugi stan powinien się aktywować");
        let second_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        let mut bytes = [0xff; 16];
        device
            .read(&second_state.state, 0, &mut bytes)
            .expect("odczyt drugiego stanu powinien się udać");
        assert_eq!(bytes, [0; 16]);
        device
            .write(&[9; 16], &second_state.state, 0)
            .expect("zapis drugiego stanu powinien się udać");

        pool.activate(first, &stream)
            .expect("pierwszy stan powinien wrócić");
        let first_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .read(&first_state.state, 0, &mut bytes)
            .expect("odczyt pierwszego stanu powinien się udać");
        assert_eq!(bytes, [7; 16]);

        pool.release(first, &stream)
            .expect("pierwszy lease powinien się zwolnić");
        let reused = pool.acquire().expect("slot powinien wrócić do puli");
        assert_eq!(reused.slot, first.slot);
        assert!(reused.generation > first.generation);
        pool.activate(reused, &stream)
            .expect("ponownie użyty slot powinien się aktywować");
        let reused_state = pool.active_layers()[0]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");
        device
            .read(&reused_state.state, 0, &mut bytes)
            .expect("odczyt ponownie użytego stanu powinien się udać");
        assert_eq!(bytes, [0; 16]);
        assert!(pool.release(first, &stream).is_err());
    }

    #[test]
    fn wspoldzielony_cache_mtp_obsluguje_cancel_release_i_reuse() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mtp_config = KvConfig {
            n_layers: 1,
            n_kv_heads: 1,
            head_dim: 8,
            page_size: 2,
            n_pages: 8,
            max_pages_per_seq: 8,
            quant: KvQuant::F16,
        };
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            Some((mtp_config, 4, 8)),
        )
        .expect("pula z MTP powinna powstać");
        let first = pool.acquire().expect("pierwszy lease powinien powstać");
        let second = pool.acquire().expect("drugi lease powinien powstać");

        pool.activate(first, &stream)
            .expect("pierwszy lease powinien się aktywować");
        let (mut first_state, mut kv) = pool
            .take_mtp(first)
            .expect("stan MTP powinien być dostępny");
        first_state
            .grow(&mut kv)
            .expect("pierwsza strona powinna powstać");
        first_state
            .grow(&mut kv)
            .expect("pierwsza strona powinna się wypełnić");
        let first_pages = first_state.seq.pages.clone();
        pool.restore_mtp(first, first_state, kv)
            .expect("pierwszy stan powinien wrócić do slotu");

        pool.activate(second, &stream)
            .expect("drugi lease powinien się aktywować");
        let (mut second_state, mut kv) = pool
            .take_mtp(second)
            .expect("stan MTP powinien być dostępny");
        second_state
            .grow(&mut kv)
            .expect("druga strona powinna powstać");
        assert!(!first_pages.contains(&second_state.seq.pages[0]));
        second_state
            .checkpoint(&stream)
            .expect("checkpoint powinien powstać");
        second_state
            .grow(&mut kv)
            .expect("draft powinien zająć kolejną pozycję");
        second_state
            .rollback(&mut kv, &stream)
            .expect("cancel powinien odtworzyć długość bazową");
        assert_eq!(second_state.seq.len, 1);
        pool.restore_mtp(second, second_state, kv)
            .expect("drugi stan powinien wrócić do slotu");

        pool.release(first, &stream)
            .expect("pierwszy lease powinien się zwolnić");
        let reused = pool.acquire().expect("zwolniony slot powinien wrócić");
        assert_eq!(reused.slot, first.slot);
        assert!(reused.generation > first.generation);
        pool.activate(reused, &stream)
            .expect("ponownie użyty slot powinien się aktywować");
        let (reused_state, kv) = pool
            .take_mtp(reused)
            .expect("stan MTP powinien być dostępny");
        assert_eq!(reused_state.seq.len, 0);
        assert!(reused_state.seq.pages.is_empty());
        assert_eq!(kv.free_page_count(), 7);
        pool.restore_mtp(reused, reused_state, kv)
            .expect("stan po reuse powinien wrócić do slotu");

        pool.release(second, &stream)
            .expect("drugi lease powinien się zwolnić");
        pool.release(reused, &stream)
            .expect("ponownie użyty lease powinien się zwolnić");
        assert_eq!(
            pool.mtp_kv
                .as_ref()
                .expect("cache MTP powinien istnieć")
                .free_page_count(),
            8
        );
    }

    fn testowa_pula_mtp(device: Arc<dyn Device>) -> HybridStatePool {
        HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            Some((
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: 1,
                    head_dim: 8,
                    page_size: 2,
                    n_pages: 8,
                    max_pages_per_seq: 8,
                    quant: KvQuant::F16,
                },
                4,
                8,
            )),
        )
        .expect("testowa pula MTP powinna powstać")
    }

    #[test]
    fn prewalidacja_commitu_pary_mtp_nie_mutuje_zadnej_lane() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (mut states, mut kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");
        for state in &mut states {
            state
                .checkpoint(&stream)
                .expect("checkpoint powinien powstać");
            state.grow(&mut kv).expect("krok draftu powinien powstać");
        }
        let lengths = [states[0].seq.len, states[1].seq.len];
        let checkpoints = [states[0].checkpoint_len(), states[1].checkpoint_len()];

        assert!(validate_mtp_pair_metadata_commit(&states, [0, 1]).is_err());
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );
        assert!(validate_mtp_pair_metadata_commit(&states, [1, 0]).is_err());
        assert_eq!([states[0].seq.len, states[1].seq.len], lengths);
        assert_eq!(
            [states[0].checkpoint_len(), states[1].checkpoint_len()],
            checkpoints
        );

        let targets = validate_mtp_pair_metadata_commit(&states, [1, 1])
            .expect("poprawny commit obu lane'ów powinien się udać");
        apply_mtp_pair_metadata_commit(&mut states, &mut kv, targets);
        pool.restore_mtp_pair(leases, states, kv)
            .expect("para po commicie powinna wrócić do puli");
    }

    #[test]
    fn blad_restore_pary_kwarantannuje_stany_i_cache() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let mut pool = testowa_pula_mtp(device);
        let leases = [
            pool.acquire().expect("lane0 powinien powstać"),
            pool.acquire().expect("lane1 powinien powstać"),
        ];
        let (states, kv) = pool
            .take_mtp_pair(leases)
            .expect("para powinna być dostępna");

        assert!(pool
            .restore_mtp_pair([leases[0], leases[0]], states, kv)
            .is_err());
        assert!(pool.poisoned.is_some());
        assert_eq!(pool.quarantined_mtp_states.len(), 2);
        assert_eq!(pool.quarantined_mtp_kv.len(), 1);
        assert!(pool.mtp_kv.is_none());
        assert!(pool.acquire().is_err());
    }

    #[test]
    fn blad_rollback_lane_zatruwa_cala_pare() {
        for failed_lane in 0..2 {
            let device: Arc<dyn Device> = CpuDevice::new();
            let stream = device.create_stream().expect("stream CPU powinien powstać");
            let mut pool = testowa_pula_mtp(device.clone());
            let leases = [
                pool.acquire().expect("lane0 powinien powstać"),
                pool.acquire().expect("lane1 powinien powstać"),
            ];
            let (mut states, mut kv) = pool
                .take_mtp_pair(leases)
                .expect("para powinna być dostępna");
            for state in &mut states {
                state
                    .checkpoint(&stream)
                    .expect("checkpoint powinien powstać");
                state.grow(&mut kv).expect("krok draftu powinien powstać");
            }
            states[failed_lane].inject_rollback_failure();

            let rollback = rollback_mtp_pair(&mut states, &mut kv, &stream)
                .expect_err("rollback wskazanego lane powinien się nie udać");
            assert!(rollback.to_string().contains(&format!("lane{failed_lane}")));
            pool.poison(format!("wymuszony błąd propose: {rollback}"));
            assert!(pool.restore_mtp_pair(leases, states, kv).is_err());
            assert!(pool.poisoned.is_some());
            assert_eq!(pool.quarantined_mtp_states.len(), 2);
            assert_eq!(pool.quarantined_mtp_kv.len(), 1);
            assert!(pool.take_mtp_pair(leases).is_err());
        }
    }

    #[test]
    fn blad_checkpointu_propose_lane_zatruwa_cala_pare() {
        for failed_lane in 0..2 {
            let device: Arc<dyn Device> = CpuDevice::new();
            let stream = device.create_stream().expect("stream CPU powinien powstać");
            let mut pool = testowa_pula_mtp(device);
            let leases = [
                pool.acquire().expect("lane0 powinien powstać"),
                pool.acquire().expect("lane1 powinien powstać"),
            ];
            let (mut states, mut kv) = pool
                .take_mtp_pair(leases)
                .expect("para powinna być dostępna");
            states[failed_lane].inject_checkpoint_failure();

            let propose = (|| {
                states[0].checkpoint(&stream)?;
                states[1].checkpoint(&stream)
            })();
            let propose_error = propose.expect_err("checkpoint wskazanego lane powinien zawieść");
            let checkpoints_complete = states.iter().all(|state| state.checkpoint_len().is_some());
            rollback_mtp_pair(&mut states, &mut kv, &stream)
                .expect("utworzony checkpoint drugiego lane powinien się cofnąć");
            assert!(!checkpoints_complete);
            pool.poison(format!(
                "błąd propose przed utworzeniem obu checkpointów: {propose_error}"
            ));
            assert!(pool.restore_mtp_pair(leases, states, kv).is_err());
            assert_eq!(pool.quarantined_mtp_states.len(), 2);
            assert_eq!(pool.quarantined_mtp_kv.len(), 1);
            assert!(pool.acquire().is_err());
        }
    }

    #[test]
    fn blad_eventu_z_udanym_sync_nie_powoduje_wzrostu_puli() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
        )
        .expect("pula powinna powstać");

        for _ in 0..64 {
            let lease = pool.acquire().expect("slot powinien wrócić do puli");
            pool.activate(lease, &stream)
                .expect("slot powinien się aktywować");
            let state = pool.active_layers()[0]
                .as_ref()
                .expect("warstwa DeltaNet ma stan");
            pool.device
                .write(&[7; 16], &state.state, 0)
                .expect("zapis stanu powinien się udać");
            pool.finish_release(
                lease,
                Err(ForgeError::Device("wymuszony błąd eventu".into())),
                || Ok(()),
            )
            .expect("synchronizacja powinna bezpiecznie odzyskać slot");
            let mut bytes = [0xff; 16];
            pool.device
                .read(
                    &pool.slots[lease.slot].layers[0]
                        .as_ref()
                        .expect("warstwa DeltaNet ma stan")
                        .state,
                    0,
                    &mut bytes,
                )
                .expect("odczyt stanu powinien się udać");
            assert_eq!(bytes, [0; 16]);
        }

        assert_eq!(pool.slots.len(), 1);
        assert_eq!(pool.free, vec![0]);
        assert!(pool.poisoned.is_none());
    }

    #[test]
    fn podwojny_blad_zwolnienia_zatruwa_pule_i_blokuje_alokacje() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().expect("stream CPU powinien powstać");
        let mut pool = HybridStatePool::new(
            device,
            vec![LayerKind::DeltaNet],
            DeltaStateLayout::KeyValue,
            8,
            16,
            None,
        )
        .expect("pula powinna powstać");
        let lease = pool.acquire().expect("lease powinien powstać");
        pool.activate(lease, &stream)
            .expect("slot powinien się aktywować");

        let result = pool.finish_release(
            lease,
            Err(ForgeError::Device("wymuszony błąd eventu".into())),
            || Err(ForgeError::Device("wymuszony błąd synchronizacji".into())),
        );

        assert!(result.is_err());
        assert!(pool.poisoned.is_some());
        assert!(pool.slots[lease.slot].in_use);
        assert!(pool.free.is_empty());
        assert!(pool.acquire().is_err());
        assert_eq!(pool.slots.len(), 1);
    }

    #[test]
    fn logical_kv_regions_zachowuja_layout_i_czesciowa_strone() {
        let regions = logical_kv_regions(&[3, 1, 7], 5, 4, 2, 6);
        assert_eq!(regions, vec![(144, 24), (168, 24), (48, 6), (72, 6)]);
        assert_eq!(regions.iter().map(|(_, len)| len).sum::<usize>(), 5 * 2 * 6);
    }

    fn fail_after_kv_growth(
        kv: &mut KvCache,
        page_table_seq: &mut u64,
        seq: &mut SeqKv,
    ) -> Result<()> {
        let base = seq.len;
        let result: Result<(usize, u32)> = (|| {
            for _ in 0..6 {
                kv.grow(seq)?;
            }
            Err(ForgeError::Kernel("wymuszony błąd weryfikacji".into()))
        })();
        finish_greedy_verification(kv, page_table_seq, seq, base, result).map(|_| ())
    }

    #[test]
    fn error_after_kv_growth_rolls_back_every_page() {
        let device = CpuDevice::new();
        let mut kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 8,
                page_size: 4,
                n_pages: 8,
                max_pages_per_seq: 8,
                quant: KvQuant::F16,
            },
        )
        .expect("cache KV powinien powstać");
        let mut seq = kv.new_seq();
        for _ in 0..3 {
            kv.grow(&mut seq)
                .expect("wzrost sekwencji powinien się udać");
        }
        let base_pages = seq.pages.len();
        let base_free_pages = kv.free_page_count();
        let mut page_table_seq = 17;

        assert!(fail_after_kv_growth(&mut kv, &mut page_table_seq, &mut seq).is_err());
        assert_eq!(seq.len, 3);
        assert_eq!(seq.pages.len(), base_pages);
        assert_eq!(kv.free_page_count(), base_free_pages);
        assert_eq!(page_table_seq, 0);
    }

    #[test]
    fn batch_prefill_nie_zostawia_czesciowego_kv_po_bledzie_wzrostu() {
        let device = CpuDevice::new();
        let mut kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 8,
                page_size: 4,
                n_pages: 1,
                max_pages_per_seq: 1,
                quant: KvQuant::F16,
            },
        )
        .expect("cache KV powinien powstać");
        let mut first = kv.new_seq();
        let mut second = kv.new_seq();
        let free_before = kv.free_page_count();
        let result = grow_prefill_lanes_transactional(&mut kv, &mut [&mut first, &mut second], 1);

        assert!(result.is_err());
        assert_eq!(first.len, 0);
        assert_eq!(second.len, 0);
        assert!(first.pages.is_empty());
        assert!(second.pages.is_empty());
        assert_eq!(kv.free_page_count(), free_before);
    }

    #[test]
    fn transakcja_publicznego_prefill_przywraca_kv_i_metadane_po_bledzie() {
        struct TestContext {
            device: Arc<dyn Device>,
            kv: KvCache,
        }

        let device: Arc<dyn Device> = CpuDevice::new();
        let mut context = TestContext {
            kv: KvCache::new(
                device.as_ref(),
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: 1,
                    head_dim: 8,
                    page_size: 1,
                    n_pages: 4,
                    max_pages_per_seq: 4,
                    quant: KvQuant::F16,
                },
            )
            .expect("cache KV powinien powstać"),
            device,
        };
        let mut first = context.kv.new_seq();
        let mut second = context.kv.new_seq();
        for (seq, token) in [(&mut first, 11), (&mut second, 22)] {
            context.kv.grow(seq).expect("bazowy wzrost KV");
            seq.tokens.push(token);
            seq.prefilled_len = 1;
        }
        let pages_before = [first.pages.clone(), second.pages.clone()];
        let free_before = context.kv.free_page_count();
        let result: Result<()> = run_dense_prefill_transaction(
            &mut context,
            &mut [&mut first, &mut second],
            |context, seqs| {
                grow_prefill_lanes_transactional(&mut context.kv, seqs, 1)?;
                for (seq, token) in seqs.iter_mut().zip([33, 44]) {
                    seq.tokens.push(token);
                    seq.prefilled_len += 1;
                }
                Err(ForgeError::Kernel(
                    "wstrzyknięty błąd po wzroście KV".into(),
                ))
            },
            |context| context.device.synchronize(),
            |context, seqs, snapshots| {
                restore_prefill_seq_snapshots(&mut context.kv, seqs, snapshots);
            },
        );

        assert!(result.is_err());
        assert_eq!([first.len, second.len], [1, 1]);
        assert_eq!([first.pages.clone(), second.pages.clone()], pages_before);
        assert_eq!(context.kv.free_page_count(), free_before);
        assert_eq!(first.tokens, [11]);
        assert_eq!(second.tokens, [22]);
        assert_eq!([first.prefilled_len, second.prefilled_len], [1, 1]);
    }

    #[test]
    fn transakcja_prefill_nie_zwalnia_stron_po_bledzie_synchronizacji() {
        struct TestContext {
            kv: KvCache,
            restore_called: bool,
            poison: KvReusePoison,
        }

        let device = CpuDevice::new();
        let mut context = TestContext {
            kv: KvCache::new(
                device.as_ref(),
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: 1,
                    head_dim: 8,
                    page_size: 1,
                    n_pages: 1,
                    max_pages_per_seq: 1,
                    quant: KvQuant::F16,
                },
            )
            .expect("cache KV powinien powstać"),
            restore_called: false,
            poison: KvReusePoison::default(),
        };
        let mut seq = context.kv.new_seq();
        let result: Result<()> = run_dense_prefill_transaction(
            &mut context,
            &mut [&mut seq],
            |context, seqs| {
                context.kv.grow(seqs[0])?;
                Err(ForgeError::Kernel("pierwotny błąd kernela".into()))
            },
            |context| {
                fatal_kv_synchronize(&mut context.poison, "rollback dense prefill", || {
                    Err(ForgeError::Device("błąd synchronizacji".into()))
                })
            },
            |context, seqs, snapshots| {
                context.restore_called = true;
                restore_prefill_seq_snapshots(&mut context.kv, seqs, snapshots);
            },
        );

        let message = result
            .expect_err("transakcja powinna zwrócić błąd")
            .to_string();
        assert!(message.contains("pierwotny błąd kernela"));
        assert!(message.contains("błąd synchronizacji"));
        assert!(!context.restore_called);
        assert!(context.poison.ensure_healthy().is_err());
        assert_eq!(seq.len, 1);
        assert_eq!(seq.pages.len(), 1);
        assert_eq!(context.kv.free_page_count(), 0);
        if !context.poison.is_poisoned() {
            context.kv.release(&mut seq);
        }
        assert_eq!(seq.pages.len(), 1);
        assert_eq!(context.kv.free_page_count(), 0);
    }

    #[test]
    fn finalny_sampling_prefill_zatruwa_reuse_przed_cleanupem() {
        let device = CpuDevice::new();
        let mut kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 8,
                page_size: 1,
                n_pages: 1,
                max_pages_per_seq: 1,
                quant: KvQuant::F16,
            },
        )
        .expect("cache KV powinien powstać");
        let mut seq = kv.new_seq();
        kv.grow(&mut seq).expect("strona KV powinna powstać");
        let mut poison = KvReusePoison::default();

        let result: Result<()> = settle_kv_operation(
            Err(ForgeError::Kernel(
                "wstrzyknięty błąd launch samplera".into(),
            )),
            "sampling finalnego dense prefill",
            || {
                fatal_kv_synchronize(&mut poison, "sampling finalnego dense prefill", || {
                    Err(ForgeError::Device("wstrzyknięty błąd D2H sync".into()))
                })
            },
        );

        let message = result
            .expect_err("operacja i sync powinny zawieść")
            .to_string();
        assert!(message.contains("wstrzyknięty błąd launch samplera"));
        assert!(message.contains("wstrzyknięty błąd D2H sync"));
        assert!(poison
            .reason()
            .is_some_and(|reason| reason.contains("sampling finalnego dense prefill")));
        if !poison.is_poisoned() {
            kv.release(&mut seq);
        }
        assert_eq!(seq.pages.len(), 1);
        assert_eq!(kv.free_page_count(), 0);

        let synchronize_called = std::cell::Cell::new(false);
        let retry = fatal_kv_synchronize(&mut poison, "kolejny prefill", || {
            synchronize_called.set(true);
            Ok(())
        });
        assert!(retry.is_err());
        assert!(!synchronize_called.get());
    }

    #[test]
    fn blad_operacji_prefill_po_udanym_settle_pozwala_zwolnic_kv() {
        let device = CpuDevice::new();
        let mut kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 8,
                page_size: 1,
                n_pages: 1,
                max_pages_per_seq: 1,
                quant: KvQuant::F16,
            },
        )
        .expect("cache KV powinien powstać");
        let mut seq = kv.new_seq();
        kv.grow(&mut seq).expect("strona KV powinna powstać");
        let mut poison = KvReusePoison::default();

        let result: Result<()> = settle_kv_operation(
            Err(ForgeError::Kernel(
                "wstrzyknięty błąd launch samplera".into(),
            )),
            "sampling finalnego dense prefill",
            || fatal_kv_synchronize(&mut poison, "sampling finalnego dense prefill", || Ok(())),
        );

        let message = result
            .expect_err("błąd operacji powinien zostać zachowany")
            .to_string();
        assert!(message.contains("wstrzyknięty błąd launch samplera"));
        assert!(!poison.is_poisoned());
        if !poison.is_poisoned() {
            kv.release(&mut seq);
        }
        assert!(seq.pages.is_empty());
        assert_eq!(kv.free_page_count(), 1);
    }

    #[test]
    fn native_mtp_acceptance_excludes_fed_and_uses_bonus_row() {
        assert_eq!(
            native_mtp_greedy_decision(&[11, 12, 13], &[11, 12, 13, 14]),
            (3, 14)
        );
        assert_eq!(
            native_mtp_greedy_decision(&[11, 12, 13], &[10, 99, 99, 99]),
            (0, 10)
        );
        assert_eq!(
            native_mtp_greedy_decision(&[11, 12, 13], &[11, 20, 99, 99]),
            (1, 20)
        );
    }

    #[test]
    fn native_mtp_b2_wymaga_wspoldzielonego_device_embeddingu() {
        assert!(native_mtp_b2_device_embedding(Some("device"), true));
        assert!(!native_mtp_b2_device_embedding(Some("device"), false));
        assert!(!native_mtp_b2_device_embedding(Some("host"), true));
        assert!(!native_mtp_b2_device_embedding(None, true));
    }

    #[test]
    fn profil_prefill_sumuje_wewnetrzne_chunki_kazdego_outer_chunku() {
        assert_eq!(hybrid_prefill_profile_spans(1, 128), 1);
        assert_eq!(hybrid_prefill_profile_spans(1024, 128), 8);
        assert_eq!(hybrid_prefill_profile_spans(1025, 128), 9);
        assert_eq!(hybrid_prefill_profile_spans(1153, 128), 10);
        assert_eq!(hybrid_prefill_profile_spans(2048, 128), 16);
        assert_eq!(hybrid_prefill_profile_spans(4, 3), 2);
        assert_eq!(hybrid_prefill_profile_spans(5, 3), 2);
    }

    #[test]
    fn auto_prefill_wybiera_t128_tylko_dla_pelnego_gate() {
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                true,
                true,
                true,
                128,
                1024,
                true,
            )
            .unwrap(),
            128
        );
    }

    #[test]
    fn auto_prefill_nvidia_po_braku_budzetu_t128_wybiera_t32() {
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                true,
                false,
                true,
                32,
                1024,
                true,
            )
            .unwrap(),
            32
        );
    }

    #[test]
    fn auto_prefill_backend_przenosny_wybiera_t16() {
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                false,
                false,
                true,
                16,
                16,
                true,
            )
            .unwrap(),
            16
        );
    }

    #[test]
    fn auto_prefill_respektuje_limit_artefaktow() {
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                true,
                false,
                true,
                32,
                1024,
                true,
            )
            .unwrap(),
            32
        );
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                true,
                false,
                true,
                16,
                1024,
                true,
            )
            .unwrap(),
            16
        );
        for chunk in [8, 4, 3] {
            assert_eq!(
                resolve_hybrid_prefill_chunk_size(
                    HybridPrefillChunkConfig::Auto,
                    false,
                    false,
                    true,
                    chunk,
                    1024,
                    true,
                )
                .unwrap(),
                chunk
            );
        }
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Auto,
            false,
            false,
            true,
            2,
            1024,
            true,
        )
        .is_err());
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                false,
                false,
                false,
                0,
                0,
                true,
            )
            .unwrap(),
            32
        );
        assert!(hybrid_prefill_t128_backend_capable(Vendor::Nvidia, 32));
        assert!(!hybrid_prefill_t128_backend_capable(Vendor::Nvidia, 64));
        // RDNA3 ma WMMA, więc AMD z falą 32 jest tu zdolne tak samo jak NVIDIA;
        // fala 64 (CDNA, stare GCN) nie — kafel zakłada 32 linie.
        assert!(hybrid_prefill_t128_backend_capable(Vendor::Amd, 32));
        assert!(!hybrid_prefill_t128_backend_capable(Vendor::Amd, 64));
        assert!(!hybrid_prefill_t128_backend_capable(Vendor::Apple, 32));
        assert!(!hybrid_prefill_t128_backend_capable(Vendor::Cpu, 32));
        assert_eq!(
            hybrid_prefill_nvfp4_chunk_limit(Vendor::Nvidia, 32, 1024),
            1024
        );
        assert_eq!(hybrid_prefill_nvfp4_chunk_limit(Vendor::Nvidia, 32, 256), 8);
        assert_eq!(hybrid_prefill_nvfp4_chunk_limit(Vendor::Amd, 64, 1024), 16);
        // AMD z falą 32 dostaje tę samą politykę co NVIDIA — o realnej
        // dostępności kafli rozstrzyga limit artefaktów, nie producent.
        assert_eq!(
            hybrid_prefill_nvfp4_chunk_limit(Vendor::Amd, 32, 1024),
            crate::model::MAX_PREFILL_CHUNK
        );
        assert_eq!(hybrid_prefill_nvfp4_chunk_limit(Vendor::Apple, 32, 256), 8);
        assert_eq!(hybrid_prefill_nvfp4_chunk_limit(Vendor::Cpu, 1, 1), 4);
        assert_eq!(hybrid_prefill_nvfp4_chunk_limit(Vendor::Cpu, 32, 1), 0);
    }

    /// Hybryda BEZ NVFP4 dostawała chunk 32 niezależnie od backendu, a
    /// kwantyzacja aktywacji dla T>=32 wchodzi na kafle i8mma, których poza
    /// NVIDIĄ nie ma — prefill wywracał się dopiero przy pierwszym żądaniu.
    #[test]
    fn auto_prefill_bez_nvfp4_schodzi_do_t16_gdy_backend_nie_ma_kafli_q8() {
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Auto,
                false,
                false,
                false,
                0,
                0,
                false,
            )
            .unwrap(),
            crate::model::HYBRID_PREFILL_PORTABLE_CHUNK
        );
        // Jawne żądanie T>=32 na takim backendzie ma paść na starcie, a nie po
        // wczytaniu modelu.
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Explicit(32),
            false,
            false,
            false,
            0,
            0,
            false,
        )
        .is_err());
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Explicit(16),
                false,
                false,
                false,
                0,
                0,
                false,
            )
            .unwrap(),
            16
        );
    }

    #[test]
    fn jawny_chunk_prefill_zachowuje_wartosc_lub_konczy_startup_bledem() {
        for chunk in [3, 16, 64, 128, 1024] {
            assert_eq!(
                resolve_hybrid_prefill_chunk_size(
                    HybridPrefillChunkConfig::Explicit(chunk),
                    false,
                    false,
                    true,
                    0,
                    1024,
                    true,
                )
                .unwrap(),
                chunk
            );
        }
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Explicit(64),
            false,
            false,
            true,
            0,
            16,
            true,
        )
        .is_err());
        assert_eq!(
            resolve_hybrid_prefill_chunk_size(
                HybridPrefillChunkConfig::Explicit(16),
                false,
                false,
                true,
                0,
                16,
                true,
            )
            .unwrap(),
            16
        );
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Explicit(4),
            false,
            false,
            true,
            0,
            3,
            true,
        )
        .is_err());
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Explicit(2),
            false,
            false,
            true,
            0,
            2,
            true,
        )
        .is_err());
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Explicit(2),
            false,
            false,
            true,
            0,
            3,
            true,
        )
        .is_err());
        assert!(resolve_hybrid_prefill_chunk_size(
            HybridPrefillChunkConfig::Auto,
            false,
            false,
            true,
            0,
            0,
            true,
        )
        .is_err());
        assert_eq!(
            parse_hybrid_prefill_chunk_config(None).unwrap(),
            HybridPrefillChunkConfig::Auto
        );
        assert_eq!(
            parse_hybrid_prefill_chunk_config(Some("auto")).unwrap(),
            HybridPrefillChunkConfig::Auto
        );
        assert_eq!(
            parse_hybrid_prefill_chunk_config(Some("128")).unwrap(),
            HybridPrefillChunkConfig::Explicit(128)
        );
        assert!(parse_hybrid_prefill_chunk_config(Some("1")).is_err());
        assert!(parse_hybrid_prefill_chunk_config(Some("2")).is_err());
        assert!(parse_hybrid_prefill_chunk_config(Some("1025")).is_err());
        assert!(parse_hybrid_prefill_chunk_config(Some("invalid")).is_err());
    }

    #[test]
    fn konfiguracja_chunka_dotyczy_tylko_modelu_hybrydowego() {
        assert_eq!(
            hybrid_prefill_chunk_config_for_model(false, Some("invalid")).unwrap(),
            HybridPrefillChunkConfig::Auto
        );
        assert!(hybrid_prefill_chunk_config_for_model(true, Some("invalid")).is_err());
    }

    #[test]
    fn chunk_prefill_omija_jednoelementowy_ogon() {
        assert_eq!(hybrid_prefill_step_size(129, 128), 127);
        assert_eq!(hybrid_prefill_step_size(4, 3), 2);
        assert_eq!(hybrid_prefill_step_size(3, 3), 3);
        assert_eq!(hybrid_prefill_step_size(2, 3), 2);
        assert_eq!(hybrid_prefill_step_size(1, 3), 1);
        assert_eq!(hybrid_prefill_step_size(2, 128), 2);
        assert_eq!(hybrid_prefill_step_size(128, 128), 128);

        for chunk_size in [3, 4, 8, 16, 32, 128] {
            for prompt_tokens in 2..=257 {
                let mut remaining = prompt_tokens;
                let mut chunks = 0;
                while remaining > 0 {
                    let step = hybrid_prefill_step_size(remaining, chunk_size);
                    assert!(step >= 2, "T1 dla promptu {prompt_tokens} i T{chunk_size}");
                    assert!(step <= chunk_size);
                    remaining -= step;
                    chunks += 1;
                }
                assert_eq!(
                    chunks,
                    hybrid_prefill_inner_chunk_count(prompt_tokens, chunk_size)
                );
            }
        }
    }

    fn testowy_ksztalt_scratchu() -> HybridPrefillScratchShape {
        HybridPrefillScratchShape {
            hidden: 5120,
            q_dim: 6144,
            kv_dim: 1024,
            inter: 17408,
            conv_dim: 6144,
            value_dim: 4096,
            n_v_heads: 16,
            d_state: 128,
            d_conv: 4,
            delta_layers: 48,
            max_pages_per_seq: 256,
        }
    }

    #[test]
    fn scratch_atencji_verifiera_nie_rosnie_powyzej_t4() {
        let expected = 4 * 24 * 8 * 260 * 4;
        assert_eq!(
            hybrid_verify_attention_parts_bytes(4, 24, 256).unwrap(),
            expected
        );
        assert_eq!(
            hybrid_verify_attention_parts_bytes(128, 24, 256).unwrap(),
            expected
        );
        assert!(hybrid_verify_attention_parts_bytes(4, usize::MAX, 256).is_err());
    }

    #[test]
    fn estimator_scratchu_uwzglednia_cap4_cap128_i_staging() {
        let t16 = hybrid_prefill_scratch_estimate(testowy_ksztalt_scratchu(), 16).unwrap();
        let t128 = hybrid_prefill_scratch_estimate(testowy_ksztalt_scratchu(), 128).unwrap();
        assert!(t128.device_bytes > t16.device_bytes);
        assert!(t128.pinned_bytes > t16.pinned_bytes);
        assert!(t128.device_bytes > 64 * 1024 * 1024);
        assert!(t128.pinned_bytes > 3 * 128 * 5120 * 2);
    }

    #[test]
    fn arena_layer_major_ma_staly_scratch_conv_i_limit_p4096() {
        let shape = testowy_ksztalt_scratchu();
        let p2048 = hybrid_layer_major_scratch_estimate(shape, 2048).unwrap();
        let p4096 = hybrid_layer_major_scratch_estimate(shape, 4096).unwrap();
        assert!(p2048 > 240 * 1024 * 1024);
        assert!(p2048 < 300 * 1024 * 1024);
        assert!(p4096 > 500 * 1024 * 1024);
        assert!(p4096 < 600 * 1024 * 1024);
        assert!(p4096 < p2048 * 2);
        assert!(hybrid_layer_major_scratch_estimate(shape, 0).is_err());
        assert!(hybrid_layer_major_scratch_estimate(shape, 4097).is_err());
    }

    #[test]
    fn cap128_wspoldzieli_scratch_conv_miedzy_warstwami() {
        assert_eq!(hybrid_verify_delta_scratch_instances(4, 48), 48);
        assert_eq!(hybrid_verify_delta_scratch_instances(128, 48), 1);
        assert_eq!(hybrid_verify_delta_scratch_instances(128, 0), 0);

        let estimate = hybrid_verify_scratch_estimate(testowy_ksztalt_scratchu(), 128).unwrap();
        let total = estimate.device_bytes + estimate.pinned_bytes;
        assert!(
            total >= 16 * 1024 * 1024,
            "scratch cap128 ma {total} bajtów"
        );
        assert!(
            total <= 20 * 1024 * 1024,
            "scratch cap128 ma {total} bajtów"
        );
    }

    #[test]
    fn cap128_budzetuje_osobny_bufor_z() {
        let shape = testowy_ksztalt_scratchu();
        assert_eq!(hybrid_verify_dedicated_z_bytes(shape, 4).unwrap(), 0);
        assert_eq!(
            hybrid_verify_dedicated_z_bytes(shape, 128).unwrap(),
            128 * shape.value_dim * 2
        );
    }

    #[test]
    fn auto_t128_wymaga_pelnego_budzetu_z_rezerwa() {
        let estimate = hybrid_prefill_scratch_estimate(testowy_ksztalt_scratchu(), 128).unwrap();
        let required = estimate.device_bytes + super::HYBRID_PREFILL_ACTIVATION_RESERVE;
        assert!(hybrid_prefill_activation_budget_capable(
            estimate,
            Some(required)
        ));
        assert!(!hybrid_prefill_activation_budget_capable(
            estimate,
            Some(required - 1)
        ));
        assert!(!hybrid_prefill_activation_budget_capable(estimate, None));
    }

    #[test]
    fn estimator_scratchu_odrzuca_przepelnienie() {
        let mut shape = testowy_ksztalt_scratchu();
        shape.hidden = usize::MAX;
        assert!(hybrid_prefill_scratch_estimate(shape, 128).is_err());
    }

    #[test]
    fn scratch_join_mtp_miesci_dwa_wektory_hidden() {
        assert_eq!(hybrid_q_full_cols(8, 12, 10), 20);
        assert_eq!(hybrid_q_full_cols(16, 12, 10), 32);
        assert_eq!(hybrid_q_full_cols(8, 40, 10), 40);
    }

    #[test]
    fn restore_wykonuje_sie_takze_po_bledzie() {
        let mut restored = false;
        let result: Result<()> = restore_after(
            Err(ForgeError::Kernel("wymuszony błąd prefill".into())),
            || restored = true,
        );
        assert!(result.is_err());
        assert!(restored);
    }
}
