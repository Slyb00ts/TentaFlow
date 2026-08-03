// ===== File: mxfp4.rs — GGUF MXFP4 =====
use super::*;

impl Kernels {
    /// y = W·x with W in GGML MXFP4 blocks, x/y f16. Warp per row.
    pub fn gemv_mxfp4_f16(
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
                "gemv_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_f16_v2")?;
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

    /// Logit GEMV over MXFP4 weights → f32 logits.
    pub fn gemv_mxfp4_out_f32(
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
                "gemv_mxfp4_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over MXFP4 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_mxfp4_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_mxfp4_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16_at(
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
                "gemm_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self
            .artifacts
            .get(&format!("gemm_mxfp4_gguf_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + MXFP4 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_mxfp4_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_mxfp4_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up MXFP4 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_mxfp4_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_silu_mxfp4_f16")?;
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

    /// MXFP4 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_mxfp4_f16(
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
                "gemv_residual_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_mxfp4_f16")?;
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
