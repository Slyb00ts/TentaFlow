// ===== File: i_quants.rs — GGUF i-kwantyzacje: IQ1..IQ4 =====
use super::*;

impl Kernels {
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
        let rpw = Self::fused_rows_per_warp();
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
}
