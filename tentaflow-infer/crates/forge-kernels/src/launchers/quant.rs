// ===== File: quant.rs — pakowanie wag i kwantyzacja aktywacji =====
use super::*;

impl Kernels {
    /// Czy backend uciągnie kaflowe Q8_0 i8mma dla T>=32.
    ///
    /// Warianty batchowe (`_b2`..`_b16`) są przenośne, ale kafle dla T>=32 stoją
    /// na `ldmatrix`/`mma` NVIDII i na AMD ich po prostu nie ma. To jest jedno
    /// źródło tej odpowiedzi — pyta o nią i launcher, i dobór chunka prefillu,
    /// żeby silnik nie wybierał ścieżki, której backend nie uruchomi.
    pub fn prepared_q8_tiled_capable(&self) -> bool {
        let caps = self.device.caps();
        if caps.warp_size != 32 || caps.max_threads_per_block < BLOCK {
            return false;
        }
        matches!(caps.vendor, forge_types::Vendor::Nvidia)
            || self.artifacts.has("gemm_q8_0_wmma_64x128")
    }

    pub fn supports_fp8_hybrid_packers(&self) -> bool {
        self.artifacts.has("pack_nvfp4_fp8") && self.artifacts.has("pack_f16_fp8")
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
        let target_end = target_row_offset
            .checked_add(source_rows)
            .ok_or_else(|| ForgeError::Kernel("repack NVFP4 CT: przepełnienie zakresu".into()))?;
        if target_end > physical_rows {
            return Err(ForgeError::Kernel(
                "repack NVFP4 CT: chunk wykracza poza resident".into(),
            ));
        }
        let target_bytes =
            checked_buffer_bytes("repack NVFP4 CT target", &[physical_rows, cols], 9)? / 16;
        let packed_bytes =
            checked_buffer_bytes("repack NVFP4 CT packed", &[source_rows, cols], 1)? / 2;
        let scale_bytes =
            checked_buffer_bytes("repack NVFP4 CT scales", &[source_rows, cols], 1)? / 16;
        if target.len() != target_bytes || packed.len() < packed_bytes || scales.len() < scale_bytes
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

    /// Symulacja kwantyzacji aktywacji do FP8 (QAT), w miejscu.
    #[allow(clippy::too_many_arguments)]
    pub fn act_quant_fp8_f16(
        &self,
        buf: &DevBuffer,
        row_stride: usize,
        offset: usize,
        span: usize,
        block: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        if block == 0 || !span.is_multiple_of(block) {
            return Err(ForgeError::Kernel(format!(
                "act_quant_fp8: {span} nie dzieli się na grupy po {block}"
            )));
        }
        let k = self.artifacts.get("act_quant_fp8_f16")?;
        let total = n_rows * (span / block);
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(buf)
            .scalar(row_stride as i64)
            .scalar(offset as i64)
            .scalar(span as i64)
            .scalar(block as i64)
            .scalar(n_rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Symulacja kwantyzacji aktywacji do FP4 (QAT indeksera), w miejscu.
    pub fn act_quant_fp4_f16(
        &self,
        buf: &DevBuffer,
        row_stride: usize,
        span: usize,
        block: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        if block == 0 || !span.is_multiple_of(block) {
            return Err(ForgeError::Kernel(format!(
                "act_quant_fp4: {span} nie dzieli się na grupy po {block}"
            )));
        }
        let k = self.artifacts.get("act_quant_fp4_f16")?;
        let total = n_rows * (span / block);
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(buf)
            .scalar(row_stride as i64)
            .scalar(span as i64)
            .scalar(block as i64)
            .scalar(n_rows as i64);
        self.device.launch(k, &cfg, &args, stream)
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
        if n_tokens >= 32 && !self.prepared_q8_tiled_capable() {
            return Err(ForgeError::Unsupported(
                "prepared Q8 T>=32 wymaga NVIDIA warp32 i bloku 256 wątków".into(),
            ));
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

    /// Kwantyzuje aktywację `x[T, cols]` do q8_1 w wewnętrznym scratchu i
    /// zwraca `(kody int8, skale, skale*suma)`. Skale są blok-major `[K/32, T]`.
    ///
    /// Wspólne dla wszystkich kafli int8: rodziny i8mma oraz kafli dot na
    /// kartach bez jednostki macierzowej. Bufory rosną tylko w górę i żyją
    /// między wywołaniami, więc kolejne warstwy nie alokują.
    pub(crate) fn prequant_q8_1(
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
        let output_bytes = checked_buffer_bytes("pack NVFP4 CT output", &[rows, weights.cols], 1)?;
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

    /// Grow the shared fp8 activation scratch to hold `n_tokens × cols` e4m3
    /// codes + `n_tokens` f32 scales. Called by the fused rmsnorm→fp8 path
    /// (which fills it) and the prequant GEMM (which reads it).
    pub(crate) fn fp8_act_ensure(&self, need_codes: usize, n_tokens: usize) -> Result<()> {
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

    /// Quantize a small decode batch (T<=16) to q8_1 in the dedicated
    /// `qk_batch` scratch and return the guard. The scratch is always sized
    /// for the T=16 ceiling so buffer addresses stay stable once the decode
    /// graphs are captured (no events — all users share the model stream's
    /// ordering).
    pub(crate) fn qk_batch_quantize(
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
            sc.xsm = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
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
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let (name, blk_elems, blk_bytes) = match fmt {
            QuantKind::Q4K => ("pack_q4_k_fp8", 256, 144),
            QuantKind::Q6K => ("pack_q6_k_fp8", 256, 210),
            QuantKind::Q8_0 => ("pack_q8_0_fp8", 32, 34),
            QuantKind::NVFP4Gguf => ("pack_nvfp4_gguf_fp8", 64, 36),
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
            .scalar(rows as i64)
            .scalar(output_scale);
        self.device.launch(gk, &cfg, &args, stream)
    }

}
