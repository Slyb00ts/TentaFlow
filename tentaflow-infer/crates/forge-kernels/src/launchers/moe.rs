// ===== File: moe.rs — launchery MoE: router, bramka, redukcja ekspertow =====
use super::*;

impl Kernels {
    /// Bramka MoE DeepSeeka V4. Bias wchodzi WYŁĄCZNIE do rankingu top-k; wagi
    /// biorą się z wyniku bez niego, są normalizowane do sumy 1 i mnożone przez
    /// `route_scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_gate_sqrtsoftplus_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_w: &DevBuffer,
        bias: &DevBuffer,
        n_tokens: usize,
        hidden: usize,
        n_expert: usize,
        top_k: usize,
        route_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_gate: {n_expert} ekspertów przekracza limit kernela 256"
            )));
        }
        let k = self.artifacts.get("moe_gate_sqrtsoftplus_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(x)
            .buf(gate_w)
            .buf(bias)
            .scalar(hidden as i64)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(route_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// MoE router: for each of `n_tokens` rows of `x` (f16, [n_tokens, hidden])
    /// compute logits `x · gate_inp` over `n_expert` experts (f16 router,
    /// [n_expert, hidden]), softmax over all experts, then select the top-k.
    /// Writes `ids` ([n_tokens, top_k] i32) and `weights` ([n_tokens, top_k]
    /// f32). `norm_topk` renormalizes the selected weights to sum 1.
    #[allow(clippy::too_many_arguments)]
    /// Softmax i top-k z GOTOWYCH logitow routera.
    ///
    /// Projekcja routera jest zwyklym GEMV i liczy ja GEMV; tutaj zostaje
    /// wylacznie wybor, ktory naprawde jest jednym blokiem na token.
    /// `moe_router_f16` robi jedno i drugie naraz, co przy generacji sciska
    /// caly milion bajtow wagi routera przez jeden multiprocesor.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_topk_f32(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        logits: &DevBuffer,
        counts: &DevBuffer,
        n_tokens: usize,
        n_expert: usize,
        top_k: usize,
        norm_topk: bool,
        stream: &Stream,
    ) -> Result<()> {
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_topk: n_expert {n_expert} przekracza limit kernela 256"
            )));
        }
        if top_k == 0 || top_k > n_expert {
            return Err(ForgeError::Kernel(format!(
                "moe_topk: top_k {top_k} poza zakresem dla {n_expert} ekspertow"
            )));
        }
        if ids.len() < n_tokens * top_k * 4
            || weights.len() < n_tokens * top_k * 4
            || logits.len() < n_tokens * n_expert * 4
            || counts.len() < n_expert * 4
        {
            return Err(ForgeError::Kernel(
                "moe_topk: bufor jest mniejszy od ksztaltu".into(),
            ));
        }
        let k = self.artifacts.get("moe_topk_f32")?;
        // JEDNA FALA na token: kernel trzyma ekspertów w rejestrach linii i
        // redukuje przetasowaniem, więc szerszy blok tylko by próżnował.
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(logits)
            .buf(counts)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(i64::from(norm_topk));
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn moe_router_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_inp: &DevBuffer,
        counts: &DevBuffer,
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
            .buf(counts)
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

    /// `out[i] = sigmoid(in[i])` over `n` shared-expert gate logits: turns them
    /// (f16, from the gate projection) into device-resident f32 scales, so
    /// folding the shared expert costs no per-layer host round-trip. One logit
    /// per token, and the whole step at once when its projection ran as one
    /// matrix.
    pub fn moe_sigmoid_f16_to_f32(
        &self,
        out: &DevBuffer,
        input: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_sigmoid_f16_to_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(input).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out[t] = Σ_j weights[t·top_k+j] · src[slots[t·top_k+j]]`, one block per
    /// token: the inverse of the gather that groups a step's selections by
    /// expert.
    ///
    /// The sum walks `j` in the order the router chose, as the per-token route
    /// did, but keeps it in f32 across all `top_k` and rounds to f16 once —
    /// where folding expert by expert rounded after each one. Toward the f32
    /// reference, not away from it.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_combine_f16(
        &self,
        out: &DevBuffer,
        src: &DevBuffer,
        slots: &DevBuffer,
        weights: &DevBuffer,
        tokens: usize,
        cols: usize,
        top_k: usize,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let selections = checked_buffer_bytes("moe_combine selections", &[tokens, top_k], 4)?;
        let out_bytes = checked_buffer_bytes("moe_combine output", &[tokens, cols], 2)?;
        if tokens == 0
            || cols == 0
            || top_k == 0
            || out.len() < out_bytes
            || slots.len() < selections
            || weights.len() < selections
        {
            return Err(ForgeError::Kernel(
                "moe_combine_f16: nieprawidłowy kształt lub zbyt mały bufor".into(),
            ));
        }
        let k = self.artifacts.get("moe_combine_f16")?;
        let cfg = LaunchConfig {
            grid: (tokens as u32, 1, 1),
            block: (BLOCK.min(cols as u32).max(32), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(src)
            .buf(slots)
            .buf(weights)
            .scalar(cols as i64)
            .scalar(top_k as i64)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Every expert's slice of one projection, in ONE launch.
    ///
    /// Replaces a loop that launched a GEMM per expert. Each of those covered
    /// about a dozen blocks — a card with dozens of multiprocessors ran them one
    /// after another and sat mostly idle — where this spans every expert's tiles
    /// at once. The tile arrays say, per block, which expert it reads and which
    /// rows of the grouped activation it owns; they are built where the grouping
    /// is decided, because that is where the row counts are known.
    ///
    /// `quant` picks the kernel exactly as the ungrouped table does, and the
    /// tile it runs is the same tile — grouping chooses `(row0, t0)` per block
    /// instead of per launch, and changes no arithmetic.
    ///
    /// NOT every format wants this, and the two answers are opposite enough
    /// that only measurement settles it. On GB10 at a 512-token prompt the one
    /// grid is worth 7,6× for the int8 tiles (Qwen3-30B: 1473 against 194 tok/s
    /// launching the same kernel tile by tile). MXFP4 was measured at 1,3×
    /// AGAINST grouping — for a single decode step, where a grid touching every
    /// expert loses more in locality than it gains in occupancy. A prompt is
    /// the opposite shape and the same tile wins there.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_grouped_experts(
        &self,
        quant: QuantKind,
        y: &DevBuffer,
        table: &DevBuffer,
        x: &DevBuffer,
        tiles: GroupedTiles<'_>,
        rows: usize,
        cols: usize,
        selections: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Four bits on BOTH sides: the block-scaled unit's operands are e2m1
        // with a ue8m0 scale, so the activation goes through the same form as
        // the weight. That is the arithmetic, not a shortcut around it.
        if quant == QuantKind::MXFP4 {
            let (xq, xs) = self.prequant_mxf4(x, cols, selections, stream)?;
            return self
                .gemm_mxf4_grouped(y, table, &xq, &xs, tiles, rows, cols, selections, stream);
        }
        let (name, block, bn, _) = self.grouped_variant(quant)?;
        let kernel = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (rows.div_ceil(bn) as u32, tiles.count as u32, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        // Q6_K reads f16 activations; the int8 tiles read the q8_1 form, which
        // is quantized ONCE for the whole grouped block rather than once per
        // expert — that repetition was its own line in the profile.
        let int8 = matches!(quant, QuantKind::Q4K | QuantKind::Q8_0);
        let args = if !int8 {
            LaunchArgs::new().buf(y).buf(table).buf(x)
        } else {
            let (xq, xd, xsm) = self.prequant_q8_1(x, cols, selections, stream)?;
            LaunchArgs::new()
                .buf(y)
                .buf(table)
                .buf(&xq)
                .buf(&xd)
                .buf(&xsm)
        };
        let args = args
            .buf(tiles.expert)
            .buf(tiles.first)
            .buf(tiles.end)
            .scalar(cols as i64)
            .scalar(rows as i64);
        // The int8 tiles read per-block activation scales laid out block-major,
        // so they need the length of the WHOLE grouped activation as a stride —
        // the tile's own end is a separate bound and travels in `tiles`.
        let args = if int8 {
            args.scalar(selections as i64)
        } else {
            args
        };
        self.device.launch(kernel, &cfg, &args, stream)
    }

    /// Every selection of a routed step, in ONE launch.
    ///
    /// The decode counterpart of `gemm_grouped_experts`, and the same lesson:
    /// the per-selection route launched `3·top_k` kernels a layer and each
    /// covered a handful of blocks, so they queued behind one another over a
    /// mostly idle card. Here `block_idx.y` IS the selection — it picks the
    /// expert out of `ids`, the token row that feeds it, and the slice of `y`
    /// it writes.
    ///
    /// `share` is how many consecutive selections read the same input row:
    /// `top_k` for the projections fed by the token's hidden state, 1 for the
    /// one fed by the feed-forward half, where each selection already has its
    /// own row.
    ///
    /// Not the grouped GEMM, deliberately. At this width every expert is chosen
    /// once or twice, so grouping would sort a handful of rows and then hand
    /// them to a tile built for hundreds; these are the kernels written for a
    /// single row, launched once instead of `top_k` times.
    #[allow(clippy::too_many_arguments)]
    /// Routed-MoE Q4_K expert GEMV whose expert is selected ON DEVICE from
    /// `ids[sel]` (no host readback of the router selection). `w_table` is a
    /// device array of per-expert weight base pointers, so experts may sit in
    /// different memory tiers and move independently. Writes the per-expert
    /// `[rows]` output at `y[0..]`, bit-identical to `gemv_q4_k_dp4a_f16_at`
    /// launched against that expert's own block.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_f16_gidx(
        &self,
        y: &DevBuffer,
        w_table: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
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
            .buf(w_table)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q6_K expert GEMV on the integer path.
    ///
    /// The six-bit twin of `gemv_q4_k_dp4a_f16_gidx`, and it exists because
    /// Q4_K_M splits ONE expert between the two: six bits on `ffn_down`, four
    /// on gate and up. Without it half a mixture's down projections took the
    /// f16 route, which dequantizes a superblock before multiplying — measured
    /// 126 GB/s against 179 for the four-bit half of the same shape.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_dp4a_f16_gidx(
        &self,
        y: &DevBuffer,
        w_table: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q6_k_dp4a_f16_gidx")?;
        let k = self.artifacts.get("gemv_q6_k_dp4a_f16_gidx")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_table)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q6_K expert GEMV whose expert is selected ON DEVICE from
    /// `ids[sel]` (no host readback). `w_table` is a device array of per-expert
    /// weight base pointers, so experts may sit in different memory tiers and
    /// move independently. Bit-identical to `gemv_q6_k_f16_at` launched against
    /// that expert's own block.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_f16_gidx(
        &self,
        y: &DevBuffer,
        w_table: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
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
            .buf(w_table)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Gate, up and the gate function of every selection, in ONE launch.
    ///
    /// `false` when this format has no such kernel, and the caller then runs
    /// the two projections and the elementwise gate separately — the same
    /// answer for three times the launches and twice the activation staging.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_silu_gidx_batch(
        &self,
        quant: QuantKind,
        act: &DevBuffer,
        table_gate: &DevBuffer,
        table_up: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        selections: usize,
        share: usize,
        stream: &Stream,
    ) -> Result<bool> {
        const ROWS_PER_BLOCK: u32 = 8;
        if cols > Self::DP4A_MAX_COLS {
            return Ok(false);
        }
        let name = match quant {
            QuantKind::Q4K => "gemv_silu_q4_k_dp4a_f16_gidx_batch",
            QuantKind::Q6K => "gemv_silu_q6_k_dp4a_f16_gidx_batch",
            _ => return Ok(false),
        };
        let Ok(k) = self.artifacts.get(name) else {
            return Ok(false);
        };
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(ROWS_PER_BLOCK), selections as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(table_gate)
            .buf(table_up)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(share as i64);
        self.device.launch(k, &cfg, &args, stream)?;
        Ok(true)
    }

    pub fn gemv_gidx_batch(
        &self,
        quant: QuantKind,
        y: &DevBuffer,
        table: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        selections: usize,
        share: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Rows one block covers, mirroring the single-selection launchers these
        // wrap — the batched kernel only adds a grid dimension.
        const ROWS_PER_BLOCK: u32 = 8;
        let name = match quant {
            QuantKind::Q4K => "gemv_q4_k_dp4a_f16_gidx_batch",
            // The integer route stages the activation in shared memory, so it
            // is the wide step that cannot take it — not the format.
            QuantKind::Q6K if cols <= Self::DP4A_MAX_COLS => "gemv_q6_k_dp4a_f16_gidx_batch",
            QuantKind::Q6K => "gemv_q6_k_f16_gidx_batch",
            QuantKind::MXFP4 => "gemv_mxfp4_f16_gidx_batch",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{other:?}: stos ekspertów nie ma wsadowego kernela adresowanego na urządzeniu"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(ROWS_PER_BLOCK), selections as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(table)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .buf(ids)
            .scalar(share as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}

/// Which expert each block of a grouped launch belongs to, and which rows of
/// the grouped activation it owns.
///
/// One entry per tile of `BM` rows. Carried as a struct because the three
/// arrays are meaningless apart — a mismatch between them multiplies one
/// expert's weights by another expert's tokens, which answers fluently.
pub struct GroupedTiles<'a> {
    pub expert: &'a DevBuffer,
    pub first: &'a DevBuffer,
    pub end: &'a DevBuffer,
    pub count: usize,
}

/// Rows of the grouped activation one tile of a grouped launch covers.
///
/// The value the int8 and Q6_K tiles use, and the WIDEST any tile uses — so it
/// is the right size for the tile table, and the wrong stride to build it with
/// unless every projection of the layer is that wide. See `grouped_tile_rows`.
pub const GROUPED_TILE_ROWS: usize = 64;

/// The narrowest tile any format has, and therefore the bound on how many tiles
/// a grouped launch can need.
pub const GROUPED_TILE_ROWS_MIN: usize = 16;

impl Kernels {
    /// Kernel, block width, output rows per block and TOKENS per tile for a
    /// grouped stack of this format.
    pub(crate) fn grouped_variant(
        &self,
        quant: QuantKind,
    ) -> Result<(&'static str, u32, usize, usize)> {
        Ok(match quant {
            QuantKind::MXFP4 => {
                let (name, tokens, threads) = self.mxf4_grouped_variant();
                (name, threads, 128, tokens)
            }
            QuantKind::Q4K => ("gemm_q4_k_i8mma_grouped", 256, 64, 64),
            QuantKind::Q8_0 => ("gemm_q8_0_i8mma_grouped", 256, 64, 64),
            QuantKind::Q6K => ("gemm_q6_k_f16_grouped", 128, 64, 64),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{other:?}: stos ekspertów nie ma zgrupowanego GEMM-u"
                )))
            }
        })
    }

    /// Whether a stack of this format multiplies as ONE grid over every expert
    /// through `gemm_grouped_experts`.
    ///
    /// MXFP4 answers on the block-scaled matrix unit, which needs its
    /// quantizer as well as its tile — hence a capability of its own.
    pub fn supports_grouped_experts(&self, quant: QuantKind) -> bool {
        if quant == QuantKind::MXFP4 {
            return self.supports_mxf4_grouped();
        }
        matches!(quant, QuantKind::Q4K | QuantKind::Q8_0 | QuantKind::Q6K)
            && self
                .grouped_variant(quant)
                .is_ok_and(|(name, ..)| self.artifacts.has(name))
    }

    /// Rows of one expert's block that this format's grouped tile computes in a
    /// single launch.
    ///
    /// A tile does NOT loop over tokens: it covers its own width from
    /// `tile_first` and stops. So the tile table has to be built with exactly
    /// this stride — built wider, the rows past it belong to no launch and are
    /// folded into their tokens as whatever the scratch last held.
    pub fn grouped_tile_rows(&self, quant: QuantKind) -> usize {
        self.grouped_variant(quant)
            .map(|(_, _, _, tokens)| tokens)
            .unwrap_or(GROUPED_TILE_ROWS)
    }
}
