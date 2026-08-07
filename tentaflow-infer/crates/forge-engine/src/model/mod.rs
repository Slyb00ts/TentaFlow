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
    MixedQuant,
    nvfp4_ct_physical_m, DeltaStateLayout, DensePrefillLogitsKind, Kernels, Nvfp4CtProjection,
    Nvfp4CtS0View, Nvfp4GgufQ8Projection, Q8ActPrepared, Q8PreparedProjection,
};

use forge_types::{DType, ForgeError, MemKind, QuantKind, Result, Vendor};

use half::f16;

use crate::expert_spill::ExpertSpill;

use crate::kv::{HybridStateLease, KvCache, KvConfig, KvLayerMap, KvQuant, SeqKv};

use crate::moe_residency::{
    ExpertStack, Migration, MoeLayerView, MoeResidencyState, Projection, ProjectionId,
    MOE_RESIDENCY_INTERVAL,
};

use crate::mtp::{MtpDraftState, MtpEmbedding};

use crate::sample::{GpuSampler, SamplingParams, SeqSampleParams};

use crate::tier::{KvTierConfig, TierManager, STAGE_SLOTS};

use crate::weight_tier::{TieredWeightDevice, WeightResidency};

use crate::weights::{
    AttnWeights, CalibStats, DeltaNetWeights, DevWeight, Fp8Layer, Fp8Weight,
    GateUpWeights, LayerFfn, LayerMixer, ModelWeights, MoeFfn, NvFp4CtLayoutPolicy,
    NvFp4CtStorage, QkvWeights, W4A8Weight, W4A8Layer, LayerWeights, Fp8FfnLayer,
};

/// Largest token count `prefill_chunk` accepts per call; callers split longer
/// prompts. Bounds the persistent prefill scratch allocation.
pub const MAX_PREFILL_CHUNK: usize = 1024;

/// Liczba slotow przypietego bufora posredniego dla wejsc jednego tokenu.
///
/// Host wyprzedza GPU: kopie z przypietego bufora sa ASYNCHRONICZNE, wiec
/// nadpisanie go dla kolejnego tokenu, zanim poprzednia kopia sie wykonala,
/// wysyla na urzadzenie dane z PRZYSZLOSCI. Objawialo sie to `seq_len`
/// wyprzedzajacym tablice stron: kernel czytal wpis `-1` i siegal poza pule KV
/// (blad pamieci GPU w prefillu hybrydowym, tylko przy prompcie dluzszym niz
/// jedna strona — dekodowanie synchronizuje sie co token po logity, wiec tam
/// wyscig nie wystepowal).
const STAGING_SLOTS: usize = 64;

const STAGING_IN_BYTES: usize = 12;

const HYBRID_PREFILL_PORTABLE_CHUNK: usize = 16;

/// Najszerszy chunk prefillu, jaki podział na rangi wykona w jednym przebiegu.
/// Wyznacza rozmiar bufora sumy cząstkowej każdej rangi.
const MAX_SPLIT_PREFILL_CHUNK: usize = 256;

const HYBRID_PREFILL_LEGACY_CHUNK: usize = 32;

const HYBRID_PREFILL_AUTO_CHUNK: usize = 128;

/// Kolejnosc prob dla chunka bez NVFP4 — od najwiekszego, bo wiekszy wygrywa
/// az do nasycenia (patrz pomiar w `resolve_hybrid_prefill_chunk_size`).
const HYBRID_PREFILL_LADDER: [usize; 5] = [1024, 512, 256, 128, 32];

const HYBRID_PREFILL_ACTIVATION_RESERVE: usize = 64 * 1024 * 1024;

const HYBRID_LAYER_MAJOR_MAX_TOKENS: usize = 4096;

/// Bufory jednego wywołania gęstego bloku FFN.
///
/// `act` wolno wskazywać ten sam bufor co `gate` — ścieżka layer-major prefillu
/// liczy bramkowanie w miejscu. `gate_up` jest potrzebne wyłącznie przy scalonej
/// macierzy `gate_up` i jednym tokenie.
pub(crate) struct FfnBlockBufs<'a> {
    x: &'a DevBuffer,
    gate: &'a DevBuffer,
    up: &'a DevBuffer,
    act: &'a DevBuffer,
    out: &'a DevBuffer,
    gate_up: Option<&'a DevBuffer>,
}

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

fn nvfp4_ct_projection_for_shape(rows: usize, cols: usize) -> Option<Nvfp4CtProjection> {
    match (rows, cols) {
        (6144, 4096) => Some(Nvfp4CtProjection::Qkv),
        (4096, 4096) => Some(Nvfp4CtProjection::Output),
        (22528, 4096) => Some(Nvfp4CtProjection::GateUp),
        (4096, 11264) => Some(Nvfp4CtProjection::Down),
        _ => None,
    }
}

fn nvfp4_ct_dimensions_capable(hidden: usize, q_dim: usize, kv_dim: usize, inter: usize) -> bool {
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

fn nvfp4_ct_buffer_plan(cap: usize, model_capable: bool) -> Option<Nvfp4CtBufferPlan> {
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
pub(crate) enum HybridPrefillChunkConfig {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HybridLayerMajorAttention {
    Exact,
    Prefill,
    Flash,
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
    legacy_chunk_limit: usize,
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
        if !prepared_q8_tiled_capable {
            return Ok(HYBRID_PREFILL_PORTABLE_CHUNK);
        }
        // Dotad zwracalo się tu SZTYWNE 32, choc wieksze chunki dzialaja i sa
        // wyraznie szybsze. Qwen3.6-27B Q4_K_M, prefill 2048 na RX 7900 XT:
        // T32 198,3 tok/s, T128 440,4, T256 677,7, T512 720,5, T1024 726,3.
        // Granice stawia budzet puli aktywacji, nie format wag.
        return Ok(HYBRID_PREFILL_LADDER
            .into_iter()
            .find(|&chunk| chunk <= legacy_chunk_limit)
            .unwrap_or(HYBRID_PREFILL_LEGACY_CHUNK));
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
    forge_types::matrix_warp32(vendor, warp_size)
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
    if forge_types::matrix_warp32(vendor, warp_size) {
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
pub(crate) struct HybridPrefillScratchShape {
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

#[derive(Clone)]
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
    /// Przydział rangi w podziale tensor-parallel. `world = 1` (domyślnie)
    /// ładuje pełny model; przy większym świecie ranga wczytuje WYŁĄCZNIE swój
    /// fragment każdej macierzy i widzi model o podzielonych liczbach głowic.
    pub tp_shard: forge_formats::TpShard,
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
            tp_shard: forge_formats::TpShard { rank: 0, world: 1 },
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

/// Podział SPMD: pozostałe rangi i to, czego wymaga redukcja między nimi.
///
/// Ranga to pełny `Model` zbudowany na swojej karcie z deskryptora o
/// PODZIELONYCH liczbach głowic i wymiarze pośrednim. Dzięki temu nie ma zakresu
/// głowic do przewlekania przez silnik: ranga po prostu widzi mniejszy model, a
/// pętla warstw, KV, stan DeltaNet i bufory aktywacji są te, które `Model` już
/// umie sobie zbudować.
/// Czego sekwencyjny prefill pod podziałem ma dostarczyć na końcu chunka.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitPrefillLogits {
    /// Chunk pośredni — głowa logitów w ogóle się nie liczy.
    None,
    /// Logity zostają w `bufs.logits` dla samplingu GPU.
    Device,
    /// Logity wracają na hosta.
    Host,
}

struct TpSpmd {
    /// Rangi 1..world. Ranga zerowa to model, który to pole trzyma.
    ranks: Vec<Model>,
    /// Zdarzenie każdej rangi (zerowej włącznie): suma cząstkowa jest zapisana.
    events: Vec<Event>,
    /// Zdarzenie każdej rangi: skończyła czytać cudze sumy cząstkowe.
    read_events: Vec<Event>,
    /// Akumulator f32 KAŻDEJ rangi, na jej własnej karcie. Redukcja jest
    /// symetryczna — każda ranga sumuje u siebie — więc nie ma jednej karty
    /// zbierającej ani bufora na przywożone cząstki.
    acc: Vec<DevBuffer>,
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
    /// FFN rozłożony na karty. `None` = cały model liczy jedna karta.
    tp_ffn: Option<crate::tensor_parallel::TpDecode>,
    /// Pozostałe rangi podziału SPMD. `Some` wyłącznie na randze zerowej, która
    /// jest jedynym wejściem do modelu i prowadzi pętlę warstw za wszystkie.
    tp: Option<Box<TpSpmd>>,
    /// Suma cząstkowa tej rangi w f32, `[hidden]`. `Some` wyłącznie wtedy, gdy
    /// model jest fragmentem podziału tensor-parallel (`world > 1`).
    ///
    /// Obecność tego bufora JEST przełącznikiem: macierz wierszowo równoległa
    /// (`attn_output`, `ssm_out`, `ffn_down`) pisze do niego w f32 zamiast do
    /// swojego bufora f16, bo jej wynik to dopiero połowa sumy. Zawężenie do f16
    /// robi redukcja, po zsumowaniu — jedno zaokrąglenie, tak jak na jednej
    /// karcie.
    tp_partial: Option<DevBuffer>,
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
    /// Pierscien slotow przypietego bufora posredniego i zdarzenie na kazdy z
    /// nich. Slot wolno nadpisac dopiero, gdy jego kopia dotarla na urzadzenie.
    staging_cursor: Cell<usize>,
    staging_events: Vec<Event>,
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
pub(crate) struct SsmState {
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
pub(crate) enum AttnSrc<'a> {
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

pub(crate) struct HybridLayerMajorCheckpoint {
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
    /// Router logits, f32 [MAX_PREFILL_CHUNK * n_experts]. The projection that
    /// fills it is an ordinary multiply and runs as one, so the selection that
    /// follows is the only part that is genuinely one block per token.
    logits: DevBuffer,
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
    forge_types::nvidia_warp32(vendor, warp_size)
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
            kernels.rmsnorm_residual_f16(x_out, h, delta, next_norm, rows, hidden, eps, stream)?;
            layer_output_scale(kernels, layer_scale, h, rows * hidden, stream)
        }
    }
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

/// Jedna projekcja: CO pomnożyć i CZYM, rozstrzygnięte raz zamiast przy każdym
/// wywołaniu.
///
/// Wcześniej każde miejsce wywołania rozpisywało iloczyn dwóch wymiarów —
/// formatu wag (W4A8 / pełny FP8 / hybryda FP8 / natywny) i układu QKV
/// (`Fused` / `FusedQk` / `Split`) — czyli dwanaście kombinacji, w kilku
/// miejscach naraz. Stąd dwie usterki z jednego dnia: podział szerokiego N
/// trafił w jeden z dwóch punktów wejścia GEMM, a przeniesienie K/V na FP8
/// wymagało zmian w trzech miejscach z czterech potrzebnych.
pub(crate) enum ProjectionPlan<'w> {
    W4A8(&'w W4A8Weight),
    Fp8(&'w Fp8Weight),
    /// Okno wierszy macierzy natywnej. Obsługuje `Split` (offset 0, całe
    /// wiersze) i oba warianty fused bez osobnych gałęzi.
    Rows {
        w: &'w DevWeight,
        row_off: usize,
        rows: usize,
    },
}


mod arch;
mod debug;
mod gemm;
mod graph;
mod kv;
mod loader;
mod mtp;
mod quant_dispatch;
mod sample;
mod scratch;
mod tp;

impl Model {
    pub(crate) fn finish(
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
        // `auto` przepakowuje head MTP do NVFP4 WYŁĄCZNIE na propozycje draftu:
        // czyta o połowę mniej bajtów na token draftu, a target weryfikuje na
        // oryginalnym Q8_0, więc wyjście zostaje bez zmian. Zmierzone na R9700,
        // prompt 4672 + odpowiedź 512: 13,36 -> 13,18 s przy akceptacji 2,13 ->
        // 2,15/krok. Kosztuje kopię headu w puli wag, więc gdy jej nie ma albo
        // head źródłowy nie jest Q8_0, zostaje Q8_0. Jawne `nvfp4` nie schodzi
        // po cichu — prośba o konkretny wariant ma być błędem, jeśli się nie da.
        let draft_head_mode =
            std::env::var("FORGE_MTP_DRAFT_HEAD").unwrap_or_else(|_| "auto".into());
        let strict_nvfp4 = draft_head_mode == "nvfp4";
        match draft_head_mode.as_str() {
            "q8" => {}
            "nvfp4" | "auto" => {
                let source = match weights.mtp.as_ref().map(|mtp| &mtp.output) {
                    Some(DevWeight::Q8_0 { buf, rows, cols }) => Some((buf.clone(), *rows, *cols)),
                    Some(_) if strict_nvfp4 => {
                        return Err(ForgeError::Unsupported(
                            "FORGE_MTP_DRAFT_HEAD=nvfp4 wymaga headu źródłowego Q8_0".into(),
                        ));
                    }
                    None if strict_nvfp4 => {
                        return Err(ForgeError::Unsupported(
                            "FORGE_MTP_DRAFT_HEAD=nvfp4 wymaga modelu z MTP".into(),
                        ));
                    }
                    _ => None,
                };
                // Blok etykietowany zamiast wczesnego `return`: jesteśmy w środku
                // konstruktora, model jeszcze nie istnieje.
                'pack: {
                let Some((source, rows, cols)) = source else {
                    break 'pack;
                };
                let bytes = rows
                    .checked_mul(cols / 64)
                    .and_then(|blocks| blocks.checked_mul(36))
                    .ok_or_else(|| {
                        ForgeError::Format("przepełnienie rozmiaru headu draftu NVFP4".into())
                    })?;
                if let Some(available) = device.pool_available(Pool::Weights) {
                    if bytes > available {
                        if strict_nvfp4 {
                            return Err(ForgeError::OutOfMemory {
                                requested: bytes,
                                available,
                            });
                        }
                        tracing::info!(
                            "head draftu MTP zostaje Q8_0: brak {bytes} B w puli wag \
                             (dostępne {available} B)"
                        );
                        break 'pack;
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
            }
            value => {
                return Err(ForgeError::Unsupported(format!(
                    "FORGE_MTP_DRAFT_HEAD={value}: oczekiwano auto, q8 lub nvfp4"
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
        // Krok partiala to `head_dim + 4` (wektor, maksimum, mianownik i wyrównanie),
        // a nie `+ 2` — przy 32 partycjach ta różnica wychodzi poza bufor.
        let attn_parts_bytes = p
            .n_heads
            .checked_mul(ATTN_DECODE_GQA_SPLITS)
            .and_then(|elements| elements.checked_mul(p.head_dim.checked_add(4)?))
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Format("przepełnienie bufora partiali attention GQA".into())
            })?;
        // Persistent decode scratch lives in the activation pool: it is the
        // pool provisioned for exactly this purpose, and nothing else uses it
        // on the LLM path anymore (the ring never needs to wrap).
        let alloc = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let inter = p.intermediate_size;
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
            pinned_in: device.alloc(
                STAGING_SLOTS * STAGING_IN_BYTES,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            pinned_pt: device.alloc(
                STAGING_SLOTS * max_pages_per_seq * 4,
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
                    logits: device.alloc(
                        MAX_PREFILL_CHUNK * m.n_experts * 4,
                        MemKind::Device,
                        Pool::Activations,
                    )?,
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
        let staging_events = (0..STAGING_SLOTS)
            .map(|_| device.create_event())
            .collect::<Result<Vec<_>>>()?;
        // Suma cząstkowa musi pomieścić NAJSZERSZY chunk, jaki podział wykona:
        // prefill liczy `T` wierszy naraz i każdy z nich ma własną sumę.
        let tp_partial = match cfg.tp_shard.world {
            1 => None,
            _ => Some(device.alloc(
                MAX_SPLIT_PREFILL_CHUNK * hidden * 4,
                MemKind::Device,
                Pool::Activations,
            )?),
        };
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
            tp_ffn: None,
            tp: None,
            tp_partial,
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
            staging_cursor: Cell::new(0),
            staging_events,
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

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through the model in one
    /// batched pass, appending to `seq`, and return the last token's logits.
    /// Not graph-captured: T varies per call and prefill launches are large
    /// enough that launch overhead is immaterial.
    pub fn prefill_chunk(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.ensure_kv_reuse_healthy()?;
        if self.is_hybrid() {
            return self.prefill_hybrid(seq, tokens);
        }
        if self.tp.is_some() {
            return self.prefill_dense_split(seq, tokens, SplitPrefillLogits::Host);
        }
        self.refuse_split_prefill()?;
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

    /// One decode step for a sequence whose spilled KV cannot be restored into
    /// VRAM: the canonical paged slabs keep the resident tail while each
    /// layer's attention runs over the staging slabs holding the FULL context
    /// for that layer (spilled chunks streamed in from RAM/NVMe, resident
    /// pages copied D2D). Never graph-captured; the kernels and their order
    /// match the resident chains exactly, so greedy tokens are bit-identical
    /// to an untiered run.
    pub(crate) fn step_streamed(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
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

    /// Provision the continuous-batching decode scratch for up to `cap`
    /// sequences. Idempotent; a larger `cap` than a previous call reallocates.
    pub fn ensure_batch(&mut self, cap: usize) -> Result<()> {
        let cap = cap.max(1);
        if self.batch_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let nvfp4_ct_plan = nvfp4_ct_buffer_plan(cap, self.nvfp4_ct_model_capable());
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
            nvfp4_ct_qkv: nvfp4_ct_plan.map(|plan| f16(plan.qkv_elems)).transpose()?,
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

}


#[cfg(test)]
mod verify_rollback_tests {
    use super::{
        apply_mtp_pair_metadata_commit, cleanup_after_error, fatal_kv_synchronize,
        finish_greedy_verification, grow_prefill_lanes_transactional,
        hybrid_layer_major_scratch_estimate, hybrid_prefill_activation_budget_capable,
        hybrid_prefill_chunk_config_for_model, hybrid_prefill_inner_chunk_count,
        hybrid_prefill_nvfp4_chunk_limit, hybrid_prefill_profile_spans,
        hybrid_prefill_scratch_estimate, hybrid_prefill_step_size,
        hybrid_prefill_t128_backend_capable, hybrid_q_full_cols,
        hybrid_verify_attention_parts_bytes, hybrid_verify_dedicated_z_bytes,
        hybrid_verify_delta_scratch_instances, hybrid_verify_scratch_estimate, logical_kv_regions,
        native_mtp_b2_device_embedding, native_mtp_greedy_decision, nvfp4_ct_buffer_plan,
        nvfp4_ct_dimensions_capable, nvfp4_ct_physical_m, nvfp4_ct_plan_physical_m,
        nvfp4_ct_projection_for_shape, parse_hybrid_prefill_chunk_config,
        resolve_hybrid_prefill_chunk_size, restore_after, restore_prefill_seq_snapshots,
        rollback_mtp_pair, run_dense_prefill_transaction, settle_kv_operation,
        validate_mtp_pair_metadata_commit, validate_mtp_routed_inputs, DeltaStateLayout,
        ForgeError, HybridPrefillChunkConfig, HybridPrefillScratchShape, HybridStatePool, KvCache,
        KvConfig, KvQuant, KvReusePoison, LayerKind, Model, Nvfp4CtProjection, Nvfp4GgufLayout,
        Result, SeqKv, Vendor,
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
        assert!(nvfp4_ct_dimensions_capable(4096, 4096, 1024, 11264));
        assert!(!nvfp4_ct_dimensions_capable(4096, 5120, 512, 11264));
        assert!(!nvfp4_ct_dimensions_capable(4096, 4096, 1024, 14336));
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                    super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                    super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
                super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
            super::HYBRID_PREFILL_LEGACY_CHUNK,
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
