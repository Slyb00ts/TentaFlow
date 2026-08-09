// ===== File: model/caps.rs — co ten model wolno uruchomić na tym urządzeniu =====
use super::*;
pub(crate) mod tuning;

impl Model {
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
    pub(crate) fn fused_decode_supported(&self) -> bool {
        // Łańcuch scalony liczy `attn_o` i `ffn_down` przez `gemv_residual`,
        // czyli dokłada rezyduum W TYM SAMYM kernelu co projekcję. Pod podziałem
        // wynik tej projekcji jest dopiero sumą CZĄSTKOWĄ, więc rezyduum wolno
        // dodać dopiero po redukcji — ranga idzie łańcuchem rozdzielonym, który
        // ma te dwa kroki osobno.
        self.tp_partial.is_none()
            && Self::fused_decode_available(&self.weights, self.device.caps().vendor)
    }
    pub(crate) fn fused_decode_available(weights: &ModelWeights, vendor: forge_types::Vendor) -> bool {
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
            // Pytanie pada przy wyborze puli KV, więc musi mieć odpowiedź dla
            // KAŻDEGO modelu: mikser rekurencyjny nie ma odpowiednika w tym
            // gęstym łańcuchu, a hybryda i tak dispatchuje po `mixer`.
            let LayerMixer::Attention(attn) = &l.mixer else {
                return false;
            };
            let qkv_ok = match &attn.attn_qkv {
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
                && Self::fused_decode_weight_ok(&attn.attn_o)
                && Self::fused_decode_weight_ok(&dffn.down)
        })
    }
}

/// Odmowy, które muszą paść PRZED alokacją czegokolwiek.
///
/// Wszystkie czytają wyłącznie opis modelu, konfigurację i urządzenie, więc
/// stoją w jednym miejscu zamiast być rozsiane po `finish` — a `finish` woła je
/// zanim weźmie choć jedną stronę pamięci.
pub(crate) fn admit(
    device: &dyn Device,
    weights: &ModelWeights,
    cfg: &ModelConfig,
) -> Result<()> {
    let p = &weights.descriptor.params;
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
            // `attn_prefill_fp8` istnieje dla head_dim 64/128, nie dla 256
            // hybrydy — bez tej odmowy jej prefill cicho schodzi na wariant
            // token-po-tokenie. Dalej: łańcuch rozdzielony (qkv_post +
            // attn_decode) też nie ma kerneli fp8.
            if weights.descriptor.params.ssm.is_some() {
                return Err(ForgeError::Unsupported(
                    "hybrid models support only the f16 KV cache: their attention and \
                     prefill kernels have no fp8 variant"
                        .into(),
                ));
            }
            if !Model::fused_decode_available(&weights, device.caps().vendor) {
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
    Ok(())
}

/// Ile linii naraz łączy batchowany target hybrydowy.
///
/// B3 i B4 składają się z par plus seryjny ogon, więc szerokość, na którą
/// kontrakt musi odpowiadać, jest jedna.
pub(crate) const HYBRID_BATCH_LANES: usize = 2;

/// Czy `logits_gemm` policzy tę głowę wsadowo dla `n_tokens` naraz.
///
/// Q6_K idzie batchowym przemiatem dp4a z wyjściem f32, Q4_K przemiatem per
/// token. Bez tych dwóch KAŻDY GGUF Q4_K_M odpadał, bo llama.cpp z konwencji
/// daje im właśnie głowę Q6_K. NVFP4 nie ma przemiatnięcia per token, którym
/// K-kwanty domykają resztę — jego kernel batchowy ma stałe szerokości, więc
/// pytanie o format bez szerokości nie ma tu sensownej odpowiedzi.
/// Szerokość, na której liczymy głowę logitów dla kroku o `n_tokens` liniach.
///
/// Wsadowe przemiaty głowy istnieją dla 2, 4 i 8. Krok o siedmiu liniach nie
/// trafiał w żaden i spadał na pętlę per linia, czyli PEŁNY odczyt wag głowy
/// razy liczba linii — na Qwen3-30B-A3B Q4_K_M to 255 MiB siedem razy zamiast
/// raz. Głowa jest ograniczona odczytem wag, więc policzenie kilku wierszy
/// więcej kosztuje tyle co nic; wiersze ponad `n_tokens` niosą nieużywane
/// aktywacje, a ich logitów nikt nie czyta.
impl Model {
    /// Odtwarza nagrany forward+logity dla tej szerokości batcha, nagrywając go
    /// przy pierwszym użyciu. Jedno miejsce dla ścieżki gęstej i routowanej.
    pub(crate) fn replay_batch_graph(&mut self, bucket: usize) -> Result<()> {
        if !self.batch_graphs.contains_key(&bucket) {
            let g = self.capture_batch_forward(bucket)?;
            self.batch_graphs.insert(bucket, g);
        }
        let graph = self.batch_graphs.get(&bucket).expect("captured").clone();
        self.device.launch_graph(&graph, &self.stream)
    }
}

impl Model {
    /// Czy uwaga dekodująca może pójść kernelem dzielącym jeden odczyt K/V
    /// między cztery głowice Q. Liczy się KROTNOŚĆ czwórki, nie dokładnie 4:1:
    /// przy GQA 8:1 dwie grupy dzielą strumień i czytają go dwa razy zamiast
    /// ośmiu. To jedyny człon kroku, który rośnie wprost z liczbą linii, więc
    /// nadmiarowość akurat tutaj kosztuje najwięcej przy równoległości.
    pub(crate) fn attn_gqa_shared(&self) -> bool {
        let p = &self.weights.descriptor.params;
        let heads_per_kv = p.n_heads.checked_div(p.n_kv_heads).unwrap_or(0);
        self.device.caps().vendor == forge_types::Vendor::Nvidia
            && self.kernels.supports_attn_decode_gqa4_f16_hd128()
            && self.kv.cfg.dtype() == forge_types::DType::F16
            && p.head_dim == 128
            && p.n_heads == heads_per_kv * p.n_kv_heads
            && heads_per_kv >= 4
            && heads_per_kv.is_multiple_of(4)
    }
}

pub(crate) fn head_batch_width(n_tokens: usize) -> usize {
    match n_tokens {
        3 => 4,
        5..=7 => 8,
        other => other,
    }
}

pub(crate) fn batched_head_supported(head: &DevWeight, n_tokens: usize) -> Result<()> {
    let supported = match head {
        DevWeight::F16 { .. }
        | DevWeight::Q8_0 { .. }
        | DevWeight::Q4K { .. }
        | DevWeight::Q6K { .. } => true,
        DevWeight::NvFp4Gguf {
            layout: Nvfp4GgufLayout::RowMajor36,
            ..
        } => matches!(n_tokens, 2 | 4 | 8 | 16),
        _ => false,
    };
    match supported {
        true => Ok(()),
        false => Err(ForgeError::Unsupported(format!(
            "batchowa głowa nie liczy tego formatu dla {n_tokens} tokenów naraz"
        ))),
    }
}

pub(crate) fn native_mtp_b2_device_embedding(mode: Option<&str>, shares_target_embedding: bool) -> bool {
    mode == Some("device") && shares_target_embedding
}

/// Szerokość jednej hybrydowej grupy decode. Każda szerokość 2..=16 trafia w
/// strojony kernel NVFP4 GGUF (b2/b3/b4 dokładnie, 5..8 przez b8, 9..16 przez
/// b16 — ten ostatni dopiero od dodania wariantu `_nvidia`; wcześniej szerokości
/// 9..16 spadały na przenośny kernel i grupa 10 dawała 37,5 tok/s wobec 67,8 po
/// naprawie). Powyżej 16 dispatch przechodzi na kafel MMA bm32, którego ta
/// ścieżka nie ma zmierzonego, więc tam jest granica grupy.
/// Zmierzone na ThinkingCap-Qwen3.6-27B (RTX 4090, prompt 85, out 128):
/// C=6 69,1 · C=10 67,8 · C=12 68,9 · C=16 71,3 tok/s.
impl Model {
    pub(crate) fn hybrid_group_size(&self, pending: usize) -> usize {
        let cap = self
            .tuned(tuning::Knob::MaxDecodeGroup)
            .expect("szerokość grupy ma wartość dla każdej karty");
        pending.min(cap)
    }
}

impl Model {
    /// Whether this model's shape admits a batched decode forward at all.
    ///
    /// Three independent contracts, each with its own reason to refuse:
    /// the hybrid needs exact small-batch kernels and resident KV; rot KV packs
    /// appended tokens only on the single-stream path, so a batch would leave
    /// the packed store stale; and a routed layer batches only through the
    /// grouped dispatch — without it every lane re-reads its experts anyway,
    /// which is what the serial path already does.
    pub(crate) fn batched_decode_admits(&self) -> Result<()> {
        if self.is_hybrid() && !self.hybrid_batch_capable() {
            return Err(ForgeError::Unsupported(
                "hybrydowy batch nie spełnia kontraktu modelu lub pamięci KV".into(),
            ));
        }
        if self.kv.cfg.quant.is_rot() {
            return Err(ForgeError::Unsupported(
                "rotational KV (rot4/rot3) supports single-stream decode only; \
                 disable batching for this model"
                    .into(),
            ));
        }
        if self.weights.is_moe() && !self.moe_batch_capable() {
            return Err(ForgeError::Unsupported(
                "ten model MoE nie ma grupowanej ścieżki ekspertów; batch wyłączony".into(),
            ));
        }
        Ok(())
    }
}

impl Model {
    /// Decode concurrency at which this model's batched forward starts to win.
    /// Resolved through the tuning cascade — the crossover moves with BOTH the
    /// model's kernel family and the card's bandwidth-to-launch ratio.
    pub(crate) fn batch_min_default(&self) -> usize {
        self.tuned(tuning::Knob::BatchMin)
            .expect("każda klasa modelu ma próg batcha")
    }
}

impl Model {
    /// Scratch a decode batch of `cap` lanes needs before its forward runs.
    ///
    /// Beyond the batch buffers themselves, a grouped MoE borrows the prefill
    /// scratch's gate/up rows for the shared expert — and the first decode can
    /// come before any prefill has allocated them.
    pub(crate) fn ensure_batch_scratch(&mut self, cap: usize) -> Result<()> {
        // Głowa liczy się na `head_batch_width`, która bywa szersza niż krok.
        self.ensure_batch(head_batch_width(cap))?;
        if self.weights.is_moe() {
            self.ensure_prefill_bufs()?;
        }
        Ok(())
    }
}
