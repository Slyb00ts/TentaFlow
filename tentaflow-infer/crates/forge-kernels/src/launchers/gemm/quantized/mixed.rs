// ===== File: mixed.rs — projekcje o roznych formatach w jednym uruchomieniu =====
use super::*;

const GEMV_MIXED_GROUP4: &str = "gemv_mixed_dp4a_group4_f16";

impl Kernels {
    /// Smallest committed MPAD bucket ≥ `n_tokens`, or `None` if `n_tokens`
    /// exceeds the largest committed ceiling (4096).
    pub(crate) fn q4k_native_mpad(n_tokens: usize) -> Option<usize> {
        [128usize, 256, 512, 1024, 2048, 4096]
            .into_iter()
            .find(|&m| m >= n_tokens)
    }

    /// Do czterech projekcji o RÓŻNYCH formatach, jednym uruchomieniem.
    ///
    /// `Q4_K_M` dobiera format PER TENSOR: `q`/`k` są w Q4_K, a `v` w Q6_K;
    /// wejściowa projekcja DeltaNet w Q6_K, a bramka w Q4_K. Grupowanie
    /// jednorodne omijało więc najliczniejsze trójki i czwórki projekcji.
    ///
    /// `projections` to `(wyjście, waga, wiersze, format)`, gdzie format wybiera
    /// `MixedQuant`. Wszystkie sloty muszą mieć tę samą liczbę kolumn — czytają
    /// tę samą aktywację.
    pub fn gemv_mixed_dp4a_group_f16(
        &self,
        projections: &[(&DevBuffer, &DevBuffer, usize, MixedQuant)],
        x: &DevBuffer,
        cols: usize,
        stream: &Stream,
    ) -> Result<bool> {
        if !(2..=4).contains(&projections.len()) || !self.artifacts.has(GEMV_MIXED_GROUP4) {
            return Ok(false);
        }
        // Wspólny prepass kwantyzacji wymaga wielokrotności najgrubszego bloku
        // spośród formatów w grupie.
        let step = projections
            .iter()
            .map(|&(_, _, _, q)| q.block())
            .max()
            .unwrap_or(256);
        Self::check_dp4a_cols(cols, step, "gemv_mixed_dp4a_group")?;
        let mut grid_x = 0u32;
        for &(_, _, rows, _) in projections {
            grid_x = grid_x
                .checked_add(u32::try_from(rows.div_ceil(8)).map_err(|_| {
                    ForgeError::Kernel("gemv mixed group: siatka przekracza u32".into())
                })?)
                .ok_or_else(|| {
                    ForgeError::Kernel("gemv mixed group: siatka przekracza u32".into())
                })?;
        }
        let mut args = LaunchArgs::new();
        for slot in 0..4 {
            match projections.get(slot) {
                Some(&(y, w, rows, quant)) => {
                    args = args.buf(y).buf(w).scalar(rows as i64).scalar(quant as i64);
                }
                // Nieużyty slot: zero wierszy nie tworzy bloku, więc wskaźniki
                // nie są dotykane.
                None => {
                    args = args
                        .buf(projections[0].0)
                        .buf(projections[0].1)
                        .scalar(0i64)
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
            .launch(self.artifacts.get(GEMV_MIXED_GROUP4)?, &cfg, &args, stream)?;
        Ok(true)
    }
}
