// ===== File: k_quants.rs — GGUF k-kwantyzacje: Q2_K..Q6_K =====
use super::*;

/// Siatka trwałego GEMV dekodowania. Wąskie macierze (`ssm_out`, `attn_output`
/// — 640 kafli) kończą się niepełną ostatnią falą grup roboczych, a blok, który
/// przechodzi po kaflach krokiem siatki, staje w jednej fali i kwantyzuje
/// aktywację raz zamiast raz na kafel. Zmierzone na R9700 jako optimum sweepu.
const PERSIST_GRID: u32 = 384;

const GEMV_Q4K_GROUP4: &str = "gemv_q4_k_dp4a_group4_f16";

impl Kernels {
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
        // RDNA3 i nowsze: kafel WMMA czytajacy surowe superbloki Q4_K. Bez niego
        // Q4_K schodzil na `dot4`, zostawiajac jednostke macierzowa bezczynna —
        // zmierzone, ze wlasnie o to rozbijal sie prefill na 7900 XT.
        if self.gemm_q4_k_i8wmma(y, w_q4k, w_byte_off, x, rows, cols, n_tokens, stream)? {
            return Ok(());
        }
        if self.gemm_kblock_wmma(
            "gemm_q4_k_wmma_f16",
            y,
            w_q4k,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
        }
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

    /// Prefillowy GEMM Q4_K na CAŁKOWITOLICZBOWEJ jednostce macierzowej RDNA3.
    /// RDNA3 liczy int8 dwa razy szybciej niż f16, więc ten wariant wyprzedza
    /// kafel f16 tam, gdzie oba są dostępne. Aktywacje przechodzą przez
    /// `quantize_act_q8_1`, którego suma bloku pokrywa człon minimum Q4_K.
    #[allow(clippy::too_many_arguments)]
    fn gemm_q4_k_i8wmma(
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
        const BN: usize = 64;
        let (name, bm, block) = if n_tokens <= 32 {
            ("gemm_q4_k_i8wmma_f16_bm32", 32usize, 128u32)
        } else {
            ("gemm_q4_k_i8wmma_f16_bm256", 256usize, 256u32)
        };
        // ZMIERZONE: ten wariant jest na razie 3,3x WOLNIEJSZY od kafla f16
        // (735 wobec 2419 tok/s, Bielik Q4_K, RX 7900 XT). Sama jednostka int8
        // jest na RDNA3 dwa razy szybsza, ale ten kernel nie stronicuje wag
        // przez LDS i ładuje skale aktywacji osobno dla każdego pola
        // akumulatora, co zjada całą przewagę. Zostaje za flagą do dalszej
        // pracy — domyślnie NIE jest używany.
        if !std::env::var("FORGE_Q4K_INT8_WMMA").is_ok_and(|v| v == "1") {
            return Ok(false);
        }
        if !self.artifacts.has(name) || !cols.is_multiple_of(256) {
            return Ok(false);
        }
        let kernel = self.artifacts.get(name)?;
        let (xq, xd, xsm) = self.prequant_q8_1(x, cols, n_tokens, stream)?;
        let config = LaunchConfig {
            grid: (rows.div_ceil(BN) as u32, n_tokens.div_ceil(bm) as u32, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&xq)
            .buf(&xd)
            .buf(&xsm)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)?;
        Ok(true)
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
        // Kafel WMMA czytający surowe superbloki Q6_K. Q6_K był jedynym
        // K-kwantem bez wariantu macierzowego i schodził na `dot4`: profil
        // prefillu 27B Q4_K_M na R9700 pokazał 25,4% czasu w `ffn_down`,
        // 4,66 ms wobec 1,43 ms typowej projekcji Q4_K o tej samej liczbie
        // mnożeń.
        if self.gemm_kblock_wmma(
            "gemm_q6_k_wmma_f16",
            y,
            w_q6k,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
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

    /// Do czterech projekcji Q4_K na WSPÓLNEJ aktywacji, jednym uruchomieniem.
    ///
    /// `Q4_K_M` trzyma w tym formacie projekcje uwagi, wejściowe projekcje
    /// DeltaNet oraz `gate`/`up` FFN — wszystkie czytają ten sam znormalizowany
    /// `x`. Osobno każda ma za wąską siatkę, żeby wypełnić kartę; złożone razem
    /// dają siatkę o sumie wierszy. `false` znaczy „nie ma tej ścieżki, licz
    /// projekcje osobno".
    pub fn gemv_q4_k_dp4a_group_f16(
        &self,
        projections: &[(&DevBuffer, &DevBuffer, usize)],
        x: &DevBuffer,
        cols: usize,
        stream: &Stream,
    ) -> Result<bool> {
        if !(2..=4).contains(&projections.len()) || !self.artifacts.has(GEMV_Q4K_GROUP4) {
            return Ok(false);
        }
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a_group")?;
        let mut grid_x = 0u32;
        for &(_, _, rows) in projections {
            grid_x = grid_x
                .checked_add(u32::try_from(rows.div_ceil(8)).map_err(|_| {
                    ForgeError::Kernel("gemv Q4_K group: siatka przekracza u32".into())
                })?)
                .ok_or_else(|| {
                    ForgeError::Kernel("gemv Q4_K group: siatka przekracza u32".into())
                })?;
        }
        let mut args = LaunchArgs::new();
        for slot in 0..4 {
            match projections.get(slot) {
                Some(&(y, w, rows)) => {
                    args = args.buf(y).buf(w).scalar(rows as i64);
                }
                // Nieużyty slot: zero wierszy nie tworzy bloku, więc wskaźniki
                // nie są dotykane.
                None => {
                    args = args
                        .buf(projections[0].0)
                        .buf(projections[0].1)
                        .scalar(0i64);
                }
            }
        }
        let args = args.buf(x).scalar(cols as i64);
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        self.device
            .launch(self.artifacts.get(GEMV_Q4K_GROUP4)?, &cfg, &args, stream)?;
        Ok(true)
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
        let tiles = (rows as u32).div_ceil(8);
        // Powyżej ~2048 kafli kernel trwa dość długo, żeby rozbieg pamięci się
        // zamortyzował, a sweep pokazał tam remis — zysk jest wyłącznie na
        // wąskich macierzach, więc tylko one schodzą na siatkę trwałą.
        let persist = tiles > PERSIST_GRID
            && tiles <= 2048
            && self.artifacts.get("gemv_q4_k_dp4a_persist_f16").is_ok();
        // Narrow staging when the activation fits it: same arithmetic, a
        // quarter of the shared memory, so an SM can hold more of these blocks.
        let k = match persist {
            true => match (cols <= 4096)
                .then(|| self.artifacts.get("gemv_q4_k_dp4a_persist_x4k_f16").ok())
                .flatten()
            {
                Some(n) => n,
                None => self.artifacts.get("gemv_q4_k_dp4a_persist_f16")?,
            },
            false => self.artifacts.get("gemv_q4_k_dp4a_f16")?,
        };
        let cfg = LaunchConfig {
            grid: (if persist { PERSIST_GRID } else { tiles }, 1, 1),
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

    /// Batchowa głowa logitów Q6_K z wyjściem f32: ten sam przemiat wag co
    /// `gemm_qk_dp4a_batch_at`, ale zapis w f32, którego wymaga sampling
    /// weryfikatora. Bez tego wariantu weryfikacja MTP przy głowie Q6_K czyta
    /// CAŁĄ głowę raz na token draftu (1,27 GiB x T) zamiast raz na cykl.
    /// Zwraca `false`, gdy kształt, szerokość batcha albo artefakt nie pasują —
    /// wołający zostaje przy przemiatnięciu per token.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_dp4a_batch_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        if !matches!(n_tokens, 2 | 4 | 8) || rows == 0 || cols == 0 || !cols.is_multiple_of(256) {
            return Ok(false);
        }
        if self.device.caps().warp_size != 32 {
            return Ok(false);
        }
        let name = format!("gemv_q6_k_dp4a_batch_out_f32_b{n_tokens}");
        let Ok(kernel) = self.artifacts.get(&name) else {
            return Ok(false);
        };
        let weight_bytes =
            checked_buffer_bytes("q6_k batch head weights", &[rows, cols / 256], 210)?;
        let weight_end = w_byte_off
            .checked_add(weight_bytes)
            .ok_or_else(|| ForgeError::Kernel("q6_k batch head: przepełnienie wag".into()))?;
        let output_bytes = checked_buffer_bytes("q6_k batch head output", &[n_tokens, rows], 4)?;
        if y_f32.len() < output_bytes || w.len() < weight_end {
            return Err(ForgeError::Kernel(
                "q6_k batch head: bufor wyjścia lub wag jest za mały".into(),
            ));
        }
        let sc = self.qk_batch_quantize(x, 0, cols, n_tokens, stream)?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(4), 1, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w, w_byte_off)?
            .buf(sc.xq.as_ref().expect("xq allocated"))
            .buf(sc.xd.as_ref().expect("xd allocated"))
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(&kernel, &cfg, &args, stream)?;
        Ok(true)
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
    /// Q6_K GEMV z aktywacją int8 (q8_1) i iloczynami dp4a, wyjście f16.
    pub fn gemv_q6_k_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_q6_k_dp4a_f16")?;
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
        // RDNA3 i nowsze: kafel WMMA czytający surowe superbloki Q5_K. Rodzina
        // `gemm_q5_k_f16{suffix}` wymaga fragmentu `mma`, którego AMD nie ma —
        // bez tej gałęzi Q5_K nie uruchamiał się tam w ogóle.
        if self.gemm_kblock_wmma(
            "gemm_q5_k_wmma_f16",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
        }
        if self.gemm_kblock_portable(
            "gemm_q5_k",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
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
        if self.gemm_kblock_wmma(
            "gemm_q3_k_wmma_f16",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
        }
        if self.gemm_kblock_portable(
            "gemm_q3_k",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
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
        if self.gemm_kblock_wmma(
            "gemm_q2_k_wmma_f16",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
        }
        if self.gemm_kblock_portable(
            "gemm_q2_k",
            y,
            w,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )? {
            return Ok(());
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

}
