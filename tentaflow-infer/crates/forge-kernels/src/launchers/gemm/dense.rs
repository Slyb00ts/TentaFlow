// ===== File: gemm/dense.rs — GEMM/GEMV na gestych f16 =====
use super::*;

impl Kernels {
    /// Sprawdza komplet artefaktów wymaganych przez ciągły prefill B2 T32.
    pub fn hybrid_prefill_b2_artifacts_capable(&self) -> bool {
        has_hybrid_prefill_b2_artifacts(|name| self.artifacts.has(name))
    }

    /// Sprawdza artefakty specjalizowanej ścieżki C1 T128 NVFP4.
    pub fn hybrid_prefill_t128_artifacts_capable(&self) -> bool {
        let nvidia = matches!(self.device.caps().vendor, forge_types::Vendor::Nvidia);
        has_hybrid_prefill_t128_artifacts(nvidia, |name| self.artifacts.has(name))
    }

    /// Sprawdza pełny backend i artefakty równego dense prefill.
    pub fn dense_prefill_batch_capable(
        &self,
        head_dim: usize,
        batch: usize,
        logits: DensePrefillLogitsKind,
    ) -> bool {
        let caps = self.device.caps();
        dense_prefill_backend_capable(caps.warp_size, caps.max_threads_per_block)
            && dense_prefill_artifacts_capable(head_dim, batch, logits, |name| {
                self.artifacts.has(name)
            })
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

    /// `gemv_f16_out_f32` na wierszach wskazanych przesunięciami bajtowymi.
    /// Kompresor liczy projekcje w f32, bo w f16 wyniki bramki się przelewają.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_out_f32_at(
        &self,
        y: &DevBuffer,
        y_off: usize,
        w: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
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
            .buf_at(y, y_off)?
            .buf(w)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
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

    /// Pick the prefill GEMM tile for a (rows, n_tokens) shape. The BM=64
    /// instantiation doubles the token-block count, which wins everywhere
    /// except very tall matrices at short chunks where the BM=128 grid is
    /// already saturated (measured on RTX 4090, kernels/mojo benches).
    pub(crate) fn gemm_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if rows >= 8192 && n_tokens <= 256 {
            ("", 256, 128)
        } else {
            ("_bm64", 128, 64)
        }
    }

    pub(crate) fn f16_out_f32_dispatch(
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
                eprintln!(
                    "ROUTE dot2 {} rows={rows} cols={cols} T={n_tokens}",
                    tile.name
                );
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

    /// Przenośny kafel rejestrowy dla kart bez jednostki macierzowej. Jest
    /// OSTATNIĄ deską ratunku: wołany dopiero wtedy, gdy dla danego formatu nie
    /// ma ani wariantu macierzowego, ani szybkiej ścieżki `dot4`. Wyprzedzenie
    /// nim `dot4` kosztowało na RX 6900 XT trzynastokrotny spadek prefillu.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_kblock_portable(
        &self,
        family: &str,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        const BM: usize = 32;
        const BN: usize = 64;
        let name = format!("{family}_tile_f16_bm32");
        if !self.artifacts.has(&name) {
            return Ok(false);
        }
        let kernel = self.artifacts.get(&name)?;
        let config = LaunchConfig {
            grid: (rows.div_ceil(BN) as u32, n_tokens.div_ceil(BM) as u32, 1),
            block: (((BM / 4) * (BN / 4)) as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)?;
        Ok(true)
    }

    /// Prefillowy GEMM k-kwantów na jednostkach macierzowych RDNA3
    /// (WMMA 16x16x16), czytający surowe superbloki GGUF. `family` to przedrostek
    /// artefaktu, np. `gemm_q4_k_wmma_f16`. Zwraca `false`, gdy architektura nie
    /// ma takiego artefaktu — wtedy wołający zostaje na swojej dotychczasowej
    /// ścieżce.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_kblock_wmma(
        &self,
        family: &str,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        // NAZWA KAFLA NIESIE JEGO GEOMETRIĘ. Obraz HSACO jest związany z
        // architekturą, więc zestawy już zbudowane (gfx1100) mają pod `_bm256`
        // kafel BN=64. Gdyby nowa geometria weszła pod starą nazwą, launcher
        // liczyłby siatkę z BN=128 dla kernela kafelkującego po 64 i po cichu
        // pomijałby połowę wierszy.
        //
        // BN=128 wygrywa na KAŻDYM zmierzonym kształcie prefillu 27B, bo
        // aktywacje są czytane `rows / 128` razy zamiast `rows / 64`; liczby są
        // w nagłówkach kerneli. BM=512 potrzebuje T >= 512, żeby mieć czym
        // wypełnić kafel.
        //
        // WĄSKIE WYJŚCIE MA WŁASNĄ REGUŁĘ. Kafel prefillowy pokrywa 128 wierszy,
        // więc projekcja o 48 wierszach (bramki `ssm_alpha`/`ssm_beta`) dostaje
        // JEDEN blok w osi wierszy — przy BM=512 daje to dwa bloki robocze na
        // karcie o 64 CU. Zmierzone: 222 us na wywołanie, 96 wywołań na prefill
        // 27B, czyli 21 ms zmarnowane na 2,6% prefillu. Mały kafel ma tam 32
        // bloki zamiast dwóch.
        let has = |s: &str| self.artifacts.has(&format!("{family}{s}"));
        let narrow = rows <= 64;
        let (suffix, bm, bn, block) = if n_tokens <= 32 || narrow {
            ("_bm32", 32usize, 64usize, 128u32)
        } else if n_tokens >= 512 && has("_bm512_bn128") {
            ("_bm512_bn128", 512usize, 128usize, 512u32)
        } else if has("_bm256_bn128") {
            ("_bm256_bn128", 256usize, 128usize, 256u32)
        } else {
            ("_bm256", 256usize, 64usize, 256u32)
        };
        let name = format!("{family}{suffix}");
        if !self.artifacts.has(&name) {
            return Ok(false);
        }
        let kernel = self.artifacts.get(&name)?;
        let config = LaunchConfig {
            grid: (rows.div_ceil(bn) as u32, n_tokens.div_ceil(bm) as u32, 1),
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
        self.device.launch(kernel, &config, &args, stream)?;
        Ok(true)
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
    pub(crate) fn gemm_q4k_i8_native(
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_i8mma_run(
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
                eprintln!(
                    "ROUTE dot4 {} rows={rows} cols={cols} T={n_tokens}",
                    tile.name
                );
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
    pub(crate) fn gemm_dot4_tile(
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
        // Karty z jednostką macierzową liczą Q8_0 na WMMA — na RDNA3 instrukcje
        // dot idą o połowę wolniej niż na RDNA2 (43 wobec 97 TOPS), a WMMA daje
        // 98, więc kafel dot byłby tam regresją względem STARSZEJ karty.
        // Obecność artefaktów jest jednocześnie testem architektury: zasięg
        // `amd:gfx11+` wpuszcza je wyłącznie do katalogów RDNA3 i nowszych.
        //
        // NA RDNA4 PRÓG MIĘDZY KAFLAMI JEST INNY. Zmierzone na Radeon AI PRO
        // R9700 (`bench_gemm_wmma_vs_dot4`, rows=cols=4096, TOPS):
        //
        //   T=512   wmma 64x128 45 | wmma 16x64 20 | dot4 31
        //   T=1024  wmma 64x128 48 | wmma 16x64 18 | dot4 34
        //   T=2048  wmma 64x128 42 | wmma 16x64 21 | dot4 39
        //
        // Kafel BM=64 przegrywał z dot4 na WYSOKICH macierzach (gate/up,
        // rows=11264): ruch wag to `(T/BM) * rows * cols` bajtów, więc mniejsze
        // BM znaczy dwa razy więcej ponownych odczytów 49 MB wag. Kafel BM=128
        // to naprawia i wygrywa na KAŻDYM zmierzonym kształcie prefillu
        // Bielika 7B (T=1024, TOPS na R9700):
        //
        //   rows=4096  cols=4096  : 128x128 42 | 64x128 39 | dot4 29
        //   rows=1024  cols=4096  : 128x128 30 | 64x128 29 | dot4 26
        //   rows=11264 cols=4096  : 128x128 48 | 64x128 27 | dot4 39
        //   rows=4096  cols=11264 : 128x128 53 | 64x128 45 | dot4 44
        //
        // Próg jest na KSZTAŁCIE, nie na modelu — ale kształty bierze się z
        // modelu, więc geometria spoza zmierzonego zakresu wymaga własnego
        // pomiaru, a nie założenia, że „już dobrane".
        let rdna4 = self.device.caps().arch.starts_with("gfx12");
        let tall_tile = rdna4
            && n_tokens >= 128
            && rows >= 1024
            && self.artifacts.has("gemm_q8_0_wmma_128x128");
        let big_tile = n_tokens >= 512 && rows >= 2048;
        if family == "q8_0" && tall_tile {
            if output_f32 {
                return Some(DotTile::new("gemm_q8_0_wmma_out_f32_16x64", 16, 64, 1, 8));
            }
            return Some(DotTile::new("gemm_q8_0_wmma_128x128", 128, 128, 8, 8));
        }
        if family == "q8_0" && (!rdna4 || big_tile) && self.artifacts.has("gemm_q8_0_wmma_64x128") {
            // Kafel 16x64 (BM=16, BN=64, 128 wątków) wobec 64x128 (BM=64,
            // BN=128, 128 wątków). Próg z pomiaru A/B na 7900 XT: duży kafel ma
            // lepsze reużycie danych, ale przy krótkim prompcie albo wąskiej
            // projekcji nie ma czym wypełnić fal i przegrywa z małym.
            if output_f32 {
                return Some(DotTile::new("gemm_q8_0_wmma_out_f32_16x64", 16, 64, 1, 8));
            }
            return Some(if big_tile {
                DotTile::new("gemm_q8_0_wmma_64x128", 64, 128, 8, 8)
            } else {
                DotTile::new("gemm_q8_0_wmma_16x64", 16, 64, 1, 8)
            });
        }
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
    pub(crate) fn gemm_i8mma_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32, u32) {
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("_big", 128, 128, 512)
        } else if n_tokens >= 256 {
            ("", 128, 64, 256)
        } else {
            ("_bm64", 64, 64, 256)
        }
    }

    /// Rows each warp computes in the norm-recomputing fused decode kernels.
    /// Fewer blocks means fewer redundant per-block norm recomputes (h32/h/
    /// norm-weight traffic), which pays off once the projection is tall
    /// enough to keep the GPU busy anyway; per-row math is unchanged.
    pub(crate) fn fused_rows_per_warp(rows: usize) -> usize {
        (rows / 2048).clamp(1, 8)
    }

    /// Guard shared by the norm-recomputing fused decode kernels: the normed
    /// x is staged in a MAX_HIDDEN-element shared array (decode_fused.mojo).
    pub(crate) fn check_fused_hidden(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > 8192 || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= 8192, got {cols}"
            )));
        }
        Ok(())
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

    /// Column bound of the dp4a kernels that quantize x from global memory
    /// into shared int8 (plain + residual variants; X_MAX in decode_dp4a.mojo).
    pub const DP4A_MAX_COLS: usize = 16384;

    pub(crate) fn check_dp4a_cols(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > Self::DP4A_MAX_COLS || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= {}, got {cols}",
                Self::DP4A_MAX_COLS
            )));
        }
        Ok(())
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
        // Kontraktem tych kerneli jest fala 32 i instrukcja dot4 na int8 —
        // ma ja tak samo RDNA (`v_dot4_i32_i8`). Obecnosc artefaktu sprawdza
        // sie ponizej, wiec warunek producenta tylko wylaczal karty, dla
        // ktorych kernel jest zbudowany.
        if self.device.caps().warp_size != 32 {
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
        let weight_bytes =
            checked_buffer_bytes("dp4a batch weights", &[rows, cols / 256], block_bytes)?;
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

}
