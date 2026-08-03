// ===== File: mtp.rs — launchery MTP/NextN: propozycja, weryfikacja, catch-up =====
use super::*;

impl Kernels {
    /// Wyznacza długość zaakceptowanego draftu i token korekty na GPU.
    pub fn mtp_verify_decide(
        &self,
        decision: &DevBuffer,
        predictions: &DevBuffer,
        input_ids: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_verify_decide")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(decision)
            .buf(predictions)
            .buf(input_ids)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Wyznacza acceptance i correction osobno dla każdego segmentu `[B,T]`.
    pub fn mtp_verify_decide_segmented(
        &self,
        decisions: &DevBuffer,
        predictions: &DevBuffer,
        input_ids: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_tokens < 2 {
            return Err(ForgeError::Kernel(format!(
                "mtp segmented decision wymaga B>0 i T>=2, otrzymano B={batch}, T={n_tokens}"
            )));
        }
        let kernel = self.artifacts.get("mtp_verify_decide_segmented")?;
        let config = LaunchConfig {
            grid: (batch as u32, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(decisions)
            .buf(predictions)
            .buf(input_ids)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F16 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f16(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f16")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F32 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f32(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f32")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje po jednym wierszu F16 wskazanym decyzją każdego segmentu.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_select_row_segmented_f16(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_rows: usize,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_rows == 0 || row_size == 0 {
            return Err(ForgeError::Kernel(
                "segmentowany wybór wiersza wymaga dodatnich wymiarów".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_select_row_segmented_f16")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), batch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decisions)
            .scalar(n_rows as i64)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Scalone przygotowanie wejścia MTP i projekcja Q8_0 z 2H do H.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_prepare_f16(
        &self,
        output: &DevBuffer,
        embedding_row: &DevBuffer,
        target_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        eh_proj: &DevBuffer,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if hidden_size == 0
            || hidden_size > 5120
            || !(2 * hidden_size).is_multiple_of(32)
            || !eps.is_finite()
            || eps <= 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "mtp_prepare_f16 wymaga 0 < H <= 5120, 2H % 32 == 0 i eps > 0; otrzymano H={hidden_size}, eps={eps}"
            )));
        }
        let output_bytes = checked_buffer_bytes("mtp_prepare_f16 output", &[hidden_size], 2)?;
        let vector_bytes = checked_buffer_bytes("mtp_prepare_f16 vector", &[hidden_size], 2)?;
        let projection_bytes = checked_buffer_bytes(
            "mtp_prepare_f16 eh_proj",
            &[hidden_size, (2 * hidden_size) / 32],
            34,
        )?;
        if output.len() < output_bytes
            || embedding_row.len() < vector_bytes
            || target_hidden.len() < vector_bytes
            || enorm.len() < vector_bytes
            || hnorm.len() < vector_bytes
            || eh_proj.len() < projection_bytes
        {
            return Err(ForgeError::Kernel(
                "mtp_prepare_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(hidden_size.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("mtp_prepare_f16: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get("mtp_prepare_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embedding_row)
            .buf(target_hidden)
            .buf(enorm)
            .buf(hnorm)
            .buf(eh_proj)
            .scalar(hidden_size as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Normalizuje batch embeddingów i przesuniętych hidden targetu przed eh_proj.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_norm_join_shifted_f16(
        &self,
        output: &DevBuffer,
        embeddings: &DevBuffer,
        target_hidden: &DevBuffer,
        initial_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        n_tokens: usize,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if n_tokens == 0 || hidden_size == 0 || !eps.is_finite() || eps <= 0.0 {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_f16 wymaga dodatnich wymiarów i eps".into(),
            ));
        }
        let rows = checked_buffer_bytes("mtp shifted rows", &[n_tokens, hidden_size], 2)?;
        let output_bytes =
            checked_buffer_bytes("mtp shifted output", &[n_tokens, 2, hidden_size], 2)?;
        let vector = checked_buffer_bytes("mtp shifted vector", &[hidden_size], 2)?;
        if output.len() < output_bytes
            || embeddings.len() < rows
            || target_hidden.len() < rows
            || initial_hidden.len() < vector
            || enorm.len() < vector
            || hnorm.len() < vector
        {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_norm_join_shifted_f16")?;
        let config = LaunchConfig {
            grid: (
                u32::try_from(n_tokens).map_err(|_| {
                    ForgeError::Kernel("mtp shifted: liczba tokenów przekracza u32".into())
                })?,
                1,
                1,
            ),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embeddings)
            .buf(target_hidden)
            .buf(initial_hidden)
            .buf(enorm)
            .buf(hnorm)
            .scalar(n_tokens as i64)
            .scalar(hidden_size as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Normalizuje `[B,T]` z osobnym początkowym hidden dla każdego lane.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_norm_join_shifted_segmented_f16(
        &self,
        output: &DevBuffer,
        embeddings: &DevBuffer,
        target_hidden: &DevBuffer,
        initial_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_tokens == 0 || hidden_size == 0 || !eps.is_finite() || eps <= 0.0 {
            return Err(ForgeError::Kernel(
                "segmentowany MTP join wymaga dodatnich wymiarów i eps".into(),
            ));
        }
        let total = batch.checked_mul(n_tokens).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie liczby tokenów segmentowanego MTP join".into())
        })?;
        let rows = checked_buffer_bytes("mtp segmented shifted rows", &[total, hidden_size], 2)?;
        let output_bytes =
            checked_buffer_bytes("mtp segmented shifted output", &[total, 2, hidden_size], 2)?;
        let initial_bytes =
            checked_buffer_bytes("mtp segmented shifted initial", &[batch, hidden_size], 2)?;
        let vector = checked_buffer_bytes("mtp segmented shifted vector", &[hidden_size], 2)?;
        if output.len() < output_bytes
            || embeddings.len() < rows
            || target_hidden.len() < rows
            || initial_hidden.len() < initial_bytes
            || enorm.len() < vector
            || hnorm.len() < vector
        {
            return Err(ForgeError::Kernel(
                "mtp_norm_join_shifted_segmented_f16: bufor jest mniejszy od wymaganego kształtu"
                    .into(),
            ));
        }
        let grid = u32::try_from(total).map_err(|_| {
            ForgeError::Kernel("segmentowany MTP join przekracza siatkę u32".into())
        })?;
        let kernel = self.artifacts.get("mtp_norm_join_shifted_segmented_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embeddings)
            .buf(target_hidden)
            .buf(initial_hidden)
            .buf(enorm)
            .buf(hnorm)
            .scalar(i64::try_from(batch).map_err(|_| {
                ForgeError::Kernel("batch segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(i64::try_from(n_tokens).map_err(|_| {
                ForgeError::Kernel("T segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(i64::try_from(hidden_size).map_err(|_| {
                ForgeError::Kernel("hidden segmentowanego MTP join przekracza i64".into())
            })?)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Projektuje złączony batch przez Q8_0 zgodnie z redukcją mtp_prepare.
    pub fn mtp_project_joined_q8_f16(
        &self,
        output: &DevBuffer,
        joined: &DevBuffer,
        weights: &DevBuffer,
        n_tokens: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let output_bytes = checked_buffer_bytes("mtp project output", &[n_tokens, hidden_size], 2)?;
        let joined_bytes =
            checked_buffer_bytes("mtp project joined", &[n_tokens, 2, hidden_size], 2)?;
        let weights_bytes = checked_buffer_bytes(
            "mtp project weights",
            &[hidden_size, (2 * hidden_size) / 32],
            34,
        )?;
        if n_tokens == 0
            || !(2 * hidden_size).is_multiple_of(32)
            || output.len() < output_bytes
            || joined.len() < joined_bytes
            || weights.len() < weights_bytes
        {
            return Err(ForgeError::Kernel(
                "mtp_project_joined_q8_f16: nieprawidłowy kształt".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_project_joined_q8_f16")?;
        let config = LaunchConfig {
            grid: ((hidden_size as u32).div_ceil(8), n_tokens as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(joined)
            .buf(weights)
            .scalar(hidden_size as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Ustawia metadane kroku MTP i opcjonalnie mapowanie nowej strony KV.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_stage_step(
        &self,
        position_out: &DevBuffer,
        seq_len_out: &DevBuffer,
        page_table: &DevBuffer,
        position: usize,
        seq_len: usize,
        logical_page: Option<usize>,
        physical_page: Option<i32>,
        stream: &Stream,
    ) -> Result<()> {
        if position_out.len() < 4 || seq_len_out.len() < 4 {
            return Err(ForgeError::Kernel(
                "mtp_stage_step wymaga 4-bajtowych buforów metadanych".into(),
            ));
        }
        let (logical_page, physical_page) = match (logical_page, physical_page) {
            (Some(logical), Some(physical)) if physical >= 0 => {
                let byte_end = logical
                    .checked_add(1)
                    .and_then(|entries| entries.checked_mul(4))
                    .ok_or_else(|| {
                        ForgeError::Kernel("mtp_stage_step: przepełnienie indeksu strony".into())
                    })?;
                if byte_end > page_table.len() {
                    return Err(ForgeError::Kernel(format!(
                        "mtp_stage_step: strona logiczna {logical} wykracza poza page table"
                    )));
                }
                (
                    i64::try_from(logical).map_err(|_| {
                        ForgeError::Kernel("mtp_stage_step: indeks strony przekracza i64".into())
                    })?,
                    i64::from(physical),
                )
            }
            (None, None) => (-1, -1),
            _ => {
                return Err(ForgeError::Kernel(
                    "mtp_stage_step wymaga kompletnej pary stron logiczna/fizyczna".into(),
                ));
            }
        };
        let position = i64::try_from(position)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: pozycja przekracza i64".into()))?;
        let seq_len = i64::try_from(seq_len)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: długość przekracza i64".into()))?;
        let kernel = self.artifacts.get("mtp_stage_step")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(position_out)
            .buf(seq_len_out)
            .buf(page_table)
            .scalar(position)
            .scalar(seq_len)
            .scalar(logical_page)
            .scalar(physical_page);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zapisuje końcowe metadane MTP dla niezależnych decyzji lane.
    pub fn mtp_commit_catchup_metadata_segmented(
        &self,
        seq_lens_out: &DevBuffer,
        positions_out: &DevBuffer,
        base_positions: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        stream: &Stream,
    ) -> Result<()> {
        let bytes = batch
            .checked_mul(4)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie metadanych catch-up MTP".into()))?;
        let decision_bytes = batch.checked_mul(8).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie decyzji metadanych catch-up MTP".into())
        })?;
        if batch == 0
            || seq_lens_out.len() < bytes
            || positions_out.len() < bytes
            || base_positions.len() < bytes
            || decisions.len() < decision_bytes
        {
            return Err(ForgeError::Kernel(
                "segmentowane metadane catch-up MTP mają nieprawidłowy kształt".into(),
            ));
        }
        let grid = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("batch metadanych MTP przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("mtp_commit_catchup_metadata_segmented")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(seq_lens_out)
            .buf(positions_out)
            .buf(base_positions)
            .buf(decisions);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Pakuje GPU-resident drafty dwóch lane'ów oraz metadane target verifiera.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_pack_verify_inputs(
        &self,
        ids_out: &DevBuffer,
        positions_out: &DevBuffer,
        visible_out: &DevBuffer,
        lane0_ids: &DevBuffer,
        lane1_ids: &DevBuffer,
        base_positions: &DevBuffer,
        steps: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !(3..=4).contains(&steps) {
            return Err(ForgeError::Kernel(format!(
                "mtp_pack_verify_inputs wymaga T=3 lub T=4, otrzymano {steps}"
            )));
        }
        let total = steps.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("mtp_pack_verify_inputs: przepełnienie liczby ID".into())
        })?;
        let bytes = checked_buffer_bytes("mtp_pack_verify_inputs output", &[total], 4)?;
        if ids_out.len() < bytes
            || positions_out.len() < bytes
            || visible_out.len() < bytes
            || lane0_ids.len() < steps * 4
            || lane1_ids.len() < steps * 4
            || base_positions.len() < 8
        {
            return Err(ForgeError::Kernel(
                "mtp_pack_verify_inputs: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("mtp_pack_verify_inputs")?;
        let config = LaunchConfig::linear(total as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(ids_out)
            .buf(positions_out)
            .buf(visible_out)
            .buf(lane0_ids)
            .buf(lane1_ids)
            .buf(base_positions)
            .scalar(steps as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

}
