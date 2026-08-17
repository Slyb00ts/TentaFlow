// ===== File: k_gather.rs — reading single rows out of k-quantized tables =====
//
// An embedding table is k-quantized like any other weight, but nothing about
// reading ONE of its rows is a matrix product: no activation, no accumulation,
// no tile. It sits apart from the GEMM launchers for that reason.

use super::*;

impl Kernels {
    /// Dekwantyzuje staged embedding row z tied Q4_K według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_q4_k_row_f16(
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
        if vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(256) {
            return Err(ForgeError::Kernel(
                "gather_q4_k_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("gather_q4_k_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_q4_k_row_f16 weights",
            &[vocab_size, hidden_size / 256],
            144,
        )?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_q4_k_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_q4_k_row_f16")?;
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

    /// Dekwantyzuje batch wierszy target embeddingu Q4_K według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_q4_k_rows_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        ids: &DevBuffer,
        rows: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(256) {
            return Err(ForgeError::Kernel(
                "gather_q4_k_rows_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gather_q4_k_rows_f16 output", &[rows, hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_q4_k_rows_f16 weights",
            &[vocab_size, hidden_size / 256],
            144,
        )?;
        if output.len() < output_bytes || weights.len() < weight_bytes || ids.len() < rows * 4 {
            return Err(ForgeError::Kernel(
                "gather_q4_k_rows_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_q4_k_rows_f16")?;
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
}
