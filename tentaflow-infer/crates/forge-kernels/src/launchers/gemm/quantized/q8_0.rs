// ===== File: q8_0.rs — GGUF Q8_0 wraz ze sciezkami DP4A i i8mma =====
use super::*;

/// Złożone do czterech projekcji Q8_0 na wspólnej aktywacji.
const GEMV_Q8_GROUP4: &str = "gemv_q8_0_dp4a_group4_f16";

impl Kernels {
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

    pub(crate) fn q8_0_out_f32_kernel(rows: usize, n_tokens: usize) -> &'static str {
        match Self::gemm_tile(rows, n_tokens).0 {
            "" => "gemm_q8_0_out_f32",
            "_bm64" => "gemm_q8_0_out_f32_bm64",
            _ => unreachable!("gemm_tile zwraca wyłącznie wspierane suffixy"),
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
        // `gemm_q8_0_out_f32` jest kernelem NVIDII (mma m16n8k16). Karty bez
        // tej instrukcji liczą tę samą głowę kaflem WMMA albo `dot4`, który
        // wybiera architektura — dlatego wejście jest jedno, a rozgałęzienie
        // siedzi tutaj, nie u wołającego.
        if self.device.caps().vendor != forge_types::Vendor::Nvidia {
            return self.gemm_i8mma_run(
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
            );
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
        if n_tokens >= 32 && !self.prepared_q8_tiled_capable() {
            return Err(ForgeError::Unsupported(
                "prepared Q8 T>=32 wymaga NVIDIA warp32 i bloku 256 wątków".into(),
            ));
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
        } else if !matches!(self.device.caps().vendor, forge_types::Vendor::Nvidia) {
            // RDNA3+: ten sam kafel co w pojedynczym GEMM Q8_0, tylko karmiony
            // wcześniej przygotowaną aktywacją.
            (
                self.artifacts.get("gemm_q8_0_wmma_64x128")?,
                LaunchConfig {
                    grid: (rows_u32.div_ceil(128), (n_tokens as u32).div_ceil(64), 1),
                    block: (128, 1, 1),
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
        } else {
            let (gk, bm, bn, threads) = self.gemm_i8mma_tile("gemm_q8_0_i8mma", rows, n_tokens)?;
            (
                gk,
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
        if !self.prepared_q8_tiled_capable() {
            return Err(ForgeError::Unsupported(
                "fused prepared Q8 wymaga jednostki macierzowej, fali 32 i bloku 256 wątków".into(),
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
        } else if self.artifacts.has("gemm_q8_0_wmma_triplet_bm64") {
            // RDNA3+: jeden kafel triplety, bez wariantów strojonych pod NVIDIĘ.
            ("gemm_q8_0_wmma_triplet_bm64", 64, 64, 128)
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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

    /// Jak `gemv_q8_0_dp4a_f16`, ale wynik w f32.
    ///
    /// Tensor parallel potrzebuje sum cząstkowych w f32, a jednocześnie musi
    /// liczyć DOKŁADNIE tym samym kernelem co ścieżka jednokartowa — inaczej
    /// podział zmienia nie tylko rozkład pracy, ale i wynik.
    pub fn gemv_q8_0_dp4a_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_q8_0_dp4a_out_f32")?;
        let k = self.artifacts.get("gemv_q8_0_dp4a_out_f32")?;
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

    /// Do czterech projekcji Q8_0 na WSPÓLNEJ aktywacji, jednym uruchomieniem.
    ///
    /// Zwraca `false`, gdy artefaktu nie ma albo grupa jest poza zakresem 2..4 —
    /// wywołujący wraca wtedy do pojedynczych uruchomień.
    pub fn gemv_q8_0_dp4a_group_f16(
        &self,
        projections: &[(&DevBuffer, &DevBuffer, usize)],
        x: &DevBuffer,
        cols: usize,
        stream: &Stream,
    ) -> Result<bool> {
        if !(2..=4).contains(&projections.len()) || !self.artifacts.has(GEMV_Q8_GROUP4) {
            return Ok(false);
        }
        Self::check_dp4a_cols(cols, 32, "gemv_q8_0_dp4a_group")?;
        let mut grid_x = 0u32;
        for &(_, _, rows) in projections {
            grid_x = grid_x
                .checked_add(u32::try_from(rows.div_ceil(8)).map_err(|_| {
                    ForgeError::Kernel("gemv Q8_0 group: siatka przekracza u32".into())
                })?)
                .ok_or_else(|| {
                    ForgeError::Kernel("gemv Q8_0 group: siatka przekracza u32".into())
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
            .launch(self.artifacts.get(GEMV_Q8_GROUP4)?, &cfg, &args, stream)?;
        Ok(true)
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
        if !matches!(n_tokens, 2 | 4 | 8 | 16) || rows == 0 || cols == 0 || !cols.is_multiple_of(32)
        {
            return Ok(false);
        }
        let caps = self.device.caps();
        if !forge_types::nvidia_warp32(caps.vendor, caps.warp_size) {
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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

}
