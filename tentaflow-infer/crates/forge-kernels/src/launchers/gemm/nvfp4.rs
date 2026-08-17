// ===== File: gemm/nvfp4.rs — GEMM/GEMV na NVFP4 (compressed-tensors i GGUF) =====
use super::*;

/// GEMV NVFP4 z jedną falą na wiersz; `GEMV_WAVE_ROWS` w `src/nvfp4.mojo`.
const GEMV_NVFP4_WAVE: &str = "gemv_nvfp4_gguf_f16_wave";

/// Zlozone do czterech projekcji NVFP4 na wspolnej aktywacji Q8_1.
const GEMV_NVFP4_GROUP4: &str = "gemv_nvfp4_gguf_q8_1_group4_f16";

const GEMV_NVFP4_WAVE_ROWS: u32 = 8;

impl Kernels {
    /// Sprawdza pełny zestaw kerneli układu NVFP4 TileN128K64.
    pub fn supports_nvfp4_gguf_tile_n128_k64(&self) -> bool {
        let caps = self.device.caps();
        matches!(caps.vendor, forge_types::Vendor::Nvidia)
            && caps.warp_size == 32
            && caps.max_threads_per_block >= 256
            && has_nvfp4_gguf_tile_artifacts(|name| self.artifacts.has(name))
    }

    /// Zwraca największy chunk NVFP4 obsługiwany przez załadowane artefakty.
    pub fn hybrid_prefill_nvfp4_artifact_chunk_limit(&self) -> usize {
        let caps = self.device.caps();
        let nvidia_warp32 = forge_types::nvidia_warp32(caps.vendor, caps.warp_size);
        hybrid_prefill_nvfp4_artifact_chunk_limit(nvidia_warp32, |name| self.artifacts.has(name))
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
        let output_bytes = checked_buffer_bytes("decode NVFP4 CT output", &[n_tokens, rows], 2)?;
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
        let source_end = source_row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Kernel("decode NVFP4 CT: przepełnienie zakresu".into()))?;
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
        let source_end = source_row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Kernel("prefill NVFP4 CT: przepełnienie zakresu".into()))?;
        if source_end > weights.rows {
            return Err(ForgeError::Kernel(
                "prefill NVFP4 CT: okno wykracza poza widok wag".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("prefill NVFP4 CT output", &[n_tokens, rows], 2)?;
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
            ForgeError::Kernel(format!("NVFP4 CT direct nie ma kernela dla M{logical_m}"))
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
        // Wariant „fala na wiersz" zmierzył 493 GB/s wobec 133 GB/s wariantu
        // blokowego na kształcie 17408x5120 — wiersz ma kilka kilobajtów, więc
        // workgroup 256 wątków na wiersz płacił za redukcję przez cały blok
        // więcej, niż kosztowało samo liczenie.
        if self.artifacts.has(GEMV_NVFP4_WAVE) {
            let k = self.artifacts.get(GEMV_NVFP4_WAVE)?;
            let cfg = LaunchConfig {
                grid: (grid_x.div_ceil(GEMV_NVFP4_WAVE_ROWS), 1, 1),
                block: (GEMV_NVFP4_WAVE_ROWS * 32, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(y)
                .buf(weights)
                .buf(x)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(output_scale);
            return self.device.launch(k, &cfg, &args, stream);
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

    /// `gemv_nvfp4_gguf_f16` na wierszach wskazanych przesunięciami bajtowymi —
    /// ścieżka MoE liczy projekcje token po tokenie wewnątrz większych buforów.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_f16_at(
        &self,
        y: &DevBuffer,
        y_off: usize,
        weights: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_f16_at wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}"
            )));
        }
        let k = self.artifacts.get("gemv_nvfp4_gguf_f16")?;
        let cfg = LaunchConfig {
            grid: (
                u32::try_from(rows).map_err(|_| {
                    ForgeError::Kernel("gemv_nvfp4_gguf_f16_at: za dużo wierszy".into())
                })?,
                1,
                1,
            ),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_off)?
            .buf(weights)
            .buf_at(x, x_off)?
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
        if !forge_types::nvidia_warp32(caps.vendor, caps.warp_size) {
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
        let nvidia = forge_types::nvidia_warp32(caps.vendor, caps.warp_size);
        // Wariant przenośny liczy FALĘ NA WIERSZ, tak jak `gemv_q8_0_out_f32_v2`.
        // Poprzedni dawał JEDEN workgroup na wiersz słownika — dla głowy MTP to
        // 248320 grup roboczych i 111 GB/s, wobec 597 GB/s wariantu Q8_0.
        let name = if nvidia {
            "gemm_nvfp4_gguf_out_f32_b1_nvidia"
        } else {
            "gemv_nvfp4_gguf_out_f32_wave"
        };
        let grid_x = u32::try_from(if nvidia {
            rows.div_ceil(2)
        } else {
            rows.div_ceil(GEMV_WAVE_ROWS)
        })
        .map_err(|_| ForgeError::Kernel("gemv_nvfp4_gguf_out_f32: siatka przekracza u32".into()))?;
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
            .scalar(cols as i64)
            .scalar(rows as i64);
        let args = if nvidia {
            args.scalar(1i64).scalar(output_scale)
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
            && (!raw_nvfp4_dp4a_supported(caps.warp_size)
                || !self.artifacts.has("gemv_nvfp4_tile128_coop_q8_1_f16"))
        {
            return Err(ForgeError::Unsupported(
                "decode NVFP4 TileN128K64 wymaga NVIDIA warp32 i kernela tile128".into(),
            ));
        }
        if !tile_layout
            && !(raw_nvfp4_dp4a_supported(caps.warp_size)
                && self.artifacts.has("gemv_nvfp4_gguf_q8_1_f16")
                && self.artifacts.has("quantize_act_q8_1"))
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
        // Zlozenie do czterech projekcji w JEDNO uruchomienie: poza narzutem
        // uruchomienia liczy sie siatka — waska projekcja mierzy 425 GB/s wobec
        // 960 GB/s szerokiej, bo nie ma czym wypelnic karty. Razem daja siatke
        // o sumie wierszy.
        if !tile_layout
            && projections.len() > 1
            && projections.len() <= 4
            && self.artifacts.has(GEMV_NVFP4_GROUP4)
        {
            let kernel = self.artifacts.get(GEMV_NVFP4_GROUP4)?;
            let mut grid_x = 0u32;
            for projection in projections {
                grid_x = grid_x
                    .checked_add(u32::try_from(projection.rows.div_ceil(8)).map_err(|_| {
                        ForgeError::Kernel("gemv NVFP4 group4: siatka przekracza u32".into())
                    })?)
                    .ok_or_else(|| {
                        ForgeError::Kernel("gemv NVFP4 group4: siatka przekracza u32".into())
                    })?;
            }
            let mut args = LaunchArgs::new();
            for slot in 0..4 {
                match projections.get(slot) {
                    Some(projection) => {
                        args = args
                            .buf(projection.output)
                            .buf(projection.weights)
                            .scalar(projection.rows as i64)
                            .scalar(projection.output_scale);
                    }
                    // Nieuzyty slot: zerowa liczba wierszy nie tworzy zadnego
                    // bloku, wiec wskazniki nie sa dotykane.
                    None => {
                        args = args
                            .buf(projections[0].output)
                            .buf(projections[0].weights)
                            .scalar(0i64)
                            .scalar(0.0f32);
                    }
                }
            }
            let args = args.buf(xq).buf(xd).scalar(cols as i64);
            let config = LaunchConfig {
                grid: (grid_x, 1, 1),
                block: (BLOCK, 1, 1),
                shared_mem_bytes: 0,
            };
            return self.device.launch(kernel, &config, &args, stream);
        }
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

    /// `gemv_nvfp4_gguf_q8_1_f16` z wynikiem w f32 — dla sum CZĄSTKOWYCH.
    ///
    /// Podział kolumnowy między karty daje na każdej karcie fragment iloczynu,
    /// który dopiero po zsumowaniu jest wierszem wyniku. Wariant f16 gubiłby na
    /// tym bity przy każdej karcie, a wariant `..._out_f32_wave` liczy aktywację
    /// w f16 zamiast przez dp4a — czyli wolniej niż ścieżka jednokartowa, którą
    /// podział ma przyspieszać, a nie zastępować.
    pub fn gemv_nvfp4_gguf_q8_1_out_f32(
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
                "gemv_nvfp4_gguf_q8_1_out_f32 wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_q8_1_out_f32", &[rows], 4)?;
        let weight_bytes =
            checked_buffer_bytes("gemv_nvfp4_gguf_q8_1_out_f32", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_q8_1_out_f32", &[cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_q8_1_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        if !(raw_nvfp4_dp4a_supported(self.device.caps().warp_size)
            && self.artifacts.has("gemv_nvfp4_gguf_q8_1_out_f32")
            && self.artifacts.has("quantize_act_q8_1"))
        {
            return self.gemv_nvfp4_gguf_out_f32(y, weights, x, rows, cols, output_scale, stream);
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
        let quant_args = LaunchArgs::new()
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .buf(x)
            .scalar(cols as i64)
            .scalar(1i64);
        self.device.launch(
            quant,
            &LaunchConfig::linear(need_blocks as u32, BLOCK),
            &quant_args,
            stream,
        )?;
        let kernel = self.artifacts.get("gemv_nvfp4_gguf_q8_1_out_f32")?;
        let grid_x = u32::try_from(rows.div_ceil(8)).map_err(|_| {
            ForgeError::Kernel("gemv_nvfp4_gguf_q8_1_out_f32: siatka przekracza u32".into())
        })?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(xq)
            .buf(xd)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(output_scale);
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
            self.artifacts.has("gemm_nvfp4_gguf_wmma_f16_bm256")
                && self.artifacts.has("gemm_nvfp4_gguf_wmma_f16_bm32"),
            self.artifacts.has("gemm_nvfp4_gguf_wmma_f16_bm256_bn128")
                && self.artifacts.has("gemm_nvfp4_gguf_wmma_f16_bm512_bn128"),
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

    /// GEMM GGUF NVFP4 z wyjściem f32 dla dużego `T` (RDNA3+, WMMA).
    ///
    /// Istnieje po to, żeby macierz WIERSZOWO równoległa podziału na rangi mogła
    /// liczyć prefill przy pełnym `T`. Rodzina `gemm_nvfp4_gguf_out_f32_b*`
    /// przyjmuje wyłącznie `B = 2/4/8/16`, a `T = 16` leży POD progiem, przy
    /// którym wchodzi kafel macierzowy — zmierzone 37,25 s wobec 3,84 s przy
    /// T = 32 na prompcie 820 tokenów.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_wmma_out_f32(
        &self,
        y_f32: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0
            || n_tokens == 0
            || cols < 64
            || !cols.is_multiple_of(64)
            || !output_scale.is_finite()
        {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_wmma_out_f32 wymaga rows > 0, cols % 64 == 0 i skończonej skali; otrzymano rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let bn128 = self
            .artifacts
            .has("gemm_nvfp4_gguf_wmma_out_f32_bm256_bn128");
        let (kernel_name, token_tile, row_tile, block_threads) = if bn128 {
            ("gemm_nvfp4_gguf_wmma_out_f32_bm256_bn128", 256, 128, 256u32)
        } else {
            ("gemm_nvfp4_gguf_wmma_out_f32_bm256", 256, 64, 256u32)
        };
        if !self.artifacts.has(kernel_name) {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_wmma_out_f32: brak artefaktu dla tej architektury".into(),
            ));
        }
        if block_threads > self.device.caps().max_threads_per_block {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_wmma_out_f32: blok przekracza limit urządzenia".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_wmma_out_f32 output", &[n_tokens, rows], 4)?;
        let weight_bytes = checked_buffer_bytes(
            "gemm_nvfp4_gguf_wmma_out_f32 weights",
            &[rows, cols / 64],
            36,
        )?;
        let input_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_wmma_out_f32 input", &[n_tokens, cols], 2)?;
        if y_f32.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_wmma_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(rows.div_ceil(row_tile)).map_err(|_| {
            ForgeError::Kernel("gemm_nvfp4_gguf_wmma_out_f32: grid.x przekracza u32".into())
        })?;
        let grid_y = u32::try_from(n_tokens.div_ceil(token_tile)).map_err(|_| {
            ForgeError::Kernel("gemm_nvfp4_gguf_wmma_out_f32: grid.y przekracza u32".into())
        })?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(weights)
            .buf(x)
            .scalar(i64::try_from(cols).map_err(|_| {
                ForgeError::Kernel("gemm_nvfp4_gguf_wmma_out_f32: cols przekracza i64".into())
            })?)
            .scalar(i64::try_from(rows).map_err(|_| {
                ForgeError::Kernel("gemm_nvfp4_gguf_wmma_out_f32: rows przekracza i64".into())
            })?)
            .scalar(i64::try_from(n_tokens).map_err(|_| {
                ForgeError::Kernel("gemm_nvfp4_gguf_wmma_out_f32: T przekracza i64".into())
            })?)
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
        let nvidia = forge_types::nvidia_warp32(caps.vendor, caps.warp_size);
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

    /// Kafel dla małego batcha NVFP4. BM32 zachowuje ten sam łańcuch MMA,
    /// ale nie wykonuje pustej drugiej połowy kafla BM64.
    fn gemm_nvfp4_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if (2..=32).contains(&n_tokens) {
            ("_bm32", 64, 32)
        } else {
            Self::gemm_tile(rows, n_tokens)
        }
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

    /// Kafel `gemm_nvfp4_dot4`, albo `None` na NVIDII i dla kształtów, które
    /// obsługują wyspecjalizowane gemv batchowe (do 16 tokenów) — tam kafel
    /// prefillowy liczyłby w większości odrzucane wiersze.
    fn gemm_nvfp4_dot4_tile(&self, rows: usize, cols: usize, n_tokens: usize) -> Option<DotTile> {
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
}
