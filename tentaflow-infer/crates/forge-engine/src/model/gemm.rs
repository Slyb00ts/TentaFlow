// ===== File: model/gemm.rs — projekcje i mnozenia na poziomie modelu =====
use super::*;

impl Model {
    pub(crate) fn gemv(&self, y: &DevBuffer, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Q8_0/Q4_K take the int8-activation dp4a kernels (measured faster at
        // every decode shape); columns beyond the kernels' shared staging
        // bound keep the f16-x path. Q6_K stays on f16 x: its dot is already
        // bandwidth-bound and the dp4a variant's extra shared usage costs
        // occupancy (measured slower at the down-projection shape).
        match w {
            DevWeight::Fp8Row {
                buf,
                scales,
                rows,
                cols,
            } => self
                .kernels
                .gemv_fp8_row_f16(y, buf, scales, x, *rows, *cols, stream),
            DevWeight::F16 { buf, rows, cols } => {
                self.kernels.gemv_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q8_0_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q8_0_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q4_k_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            // Q6_K przez dp4a tam, gdzie się mieści w oknie aktywacji. Stary
            // komentarz odradzał tę ścieżkę, ale rozdzielony pomiar pokazał, że
            // przy NIEZMIENIONYM `X_MAX` jest szybsza: 28,2 -> 28,6 tok/s.
            // Wcześniejsza regresja pochodziła wyłącznie z podniesienia bufora
            // aktywacji w LDS, nie z samego dp4a.
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q6_k_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q6_k_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => {
                self.kernels.gemv_q5_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q3K { buf, rows, cols } => {
                self.kernels.gemv_q3_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q2K { buf, rows, cols } => {
                self.kernels.gemv_q2_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_0 { buf, rows, cols } => {
                self.kernels.gemv_q4_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_1 { buf, rows, cols } => {
                self.kernels.gemv_q4_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_0 { buf, rows, cols } => {
                self.kernels.gemv_q5_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_1 { buf, rows, cols } => {
                self.kernels.gemv_q5_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => {
                self.kernels.gemv_mxfp4_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => {
                self.kernels.gemv_iq2_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq3S { buf, rows, cols } => {
                self.kernels.gemv_iq3_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => {
                self.kernels.gemv_iq1_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq1M { buf, rows, cols } => {
                self.kernels.gemv_iq1_m_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => self.kernels.gemv_nvfp4_f16(
                    y,
                    packed,
                    scales,
                    x,
                    *rows,
                    *cols,
                    *inv_global_scale,
                    stream,
                ),
                NvFp4CtStorage::S0N64K128 { .. } => {
                    let window = w.nvfp4_ct_row_window(0, *rows)?;
                    let view =
                        Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
                    self.kernels.gemv_nvfp4_ct_s0_n64k128_f16(
                        y,
                        view,
                        x,
                        window.row_offset(),
                        window.rows(),
                        *inv_global_scale,
                        stream,
                    )
                }
            },
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } => {
                if *layout == Nvfp4GgufLayout::TileN128K64 {
                    self.kernels.gemv_nvfp4_gguf_q8_1_group_layout_f16(
                        &[Nvfp4GgufQ8Projection {
                            output: y,
                            weights: buf,
                            rows: *rows,
                            output_scale: *output_scale,
                        }],
                        x,
                        *cols,
                        *layout,
                        stream,
                    )
                } else if self.device.caps().vendor == Vendor::Nvidia {
                    self.kernels.gemv_nvfp4_gguf_b1_f16(
                        y,
                        buf,
                        x,
                        *rows,
                        *cols,
                        *output_scale,
                        stream,
                    )
                } else {
                    self.kernels.gemv_nvfp4_gguf_q8_1_group_f16(
                        &[Nvfp4GgufQ8Projection {
                            output: y,
                            weights: buf,
                            rows: *rows,
                            output_scale: *output_scale,
                        }],
                        x,
                        *cols,
                        stream,
                    )
                }
            }
        }
    }

    /// Wykonuje kilka pełnych projekcji GGUF NVFP4 ze wspólną kwantyzacją
    /// aktywacji Q8_1. Zwraca `false`, gdy choć jedna waga ma inny format.
    /// Wykonuje kilka projekcji Q8_0 jednym uruchomieniem. Zwraca `false`, gdy
    /// choć jedna waga ma inny format albo szerokości się różnią.
    pub(crate) fn gemv_q8_0_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::Q8_0 {
                buf,
                rows,
                cols: weight_cols,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Ok(false);
            }
            cols = Some(*weight_cols);
            group.push((output, buf, *rows));
        }
        let Some(cols) = cols else {
            return Ok(false);
        };
        self.kernels
            .gemv_q8_0_dp4a_group_f16(&group, x, cols, stream)
    }

    /// To samo dla Q4_K: `Q4_K_M` trzyma w tym formacie projekcje uwagi,
    /// wejściowe projekcje DeltaNet oraz `gate`/`up` FFN.
    pub(crate) fn gemv_q4_k_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::Q4K {
                buf,
                rows,
                cols: weight_cols,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Ok(false);
            }
            cols = Some(*weight_cols);
            group.push((output, buf, *rows));
        }
        let Some(cols) = cols else {
            return Ok(false);
        };
        if cols > Kernels::DP4A_MAX_COLS {
            return Ok(false);
        }
        self.kernels.gemv_q4_k_dp4a_group_f16(&group, x, cols, stream)
    }

    /// Grupa projekcji o RÓŻNYCH formatach — jedno uruchomienie.
    ///
    /// `Q4_K_M` dobiera format per tensor, więc jednorodne grupowanie omijało
    /// najliczniejsze trójki i czwórki: `q`/`k` w Q4_K obok `v` w Q6_K, oraz
    /// wejściową projekcję DeltaNet w Q6_K obok bramki w Q4_K.
    pub(crate) fn gemv_mixed_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let (buf, rows, weight_cols, quant) = match weight {
                DevWeight::Q4K { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q4K),
                DevWeight::Q6K { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q6K),
                DevWeight::Q8_0 { buf, rows, cols } => (buf, *rows, *cols, MixedQuant::Q8_0),
                _ => return Ok(false),
            };
            if cols.is_some_and(|value| value != weight_cols) {
                return Ok(false);
            }
            cols = Some(weight_cols);
            group.push((output, buf, rows, quant));
        }
        let Some(cols) = cols else { return Ok(false) };
        if cols > Kernels::DP4A_MAX_COLS {
            return Ok(false);
        }
        self.kernels
            .gemv_mixed_dp4a_group_f16(&group, x, cols, stream)
    }

    pub(crate) fn gemv_nvfp4_gguf_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut weight_layout = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols: weight_cols,
                layout,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Err(ForgeError::Format(
                    "projekcje NVFP4 współdzielące Q8_1 mają różne szerokości".into(),
                ));
            }
            if weight_layout.is_some_and(|value| value != *layout) {
                return Ok(false);
            }
            weight_layout = Some(*layout);
            cols = Some(*weight_cols);
            group.push(Nvfp4GgufQ8Projection {
                output,
                weights: buf,
                rows: *rows,
                output_scale: *output_scale,
            });
        }
        let Some(cols) = cols else { return Ok(false) };
        let layout = weight_layout.unwrap_or(Nvfp4GgufLayout::RowMajor36);
        if layout == Nvfp4GgufLayout::TileN128K64 {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_layout_f16(&group, x, cols, layout, stream)?;
        } else if self.device.caps().vendor == Vendor::Nvidia {
            for projection in group {
                self.kernels.gemv_nvfp4_gguf_b1_f16(
                    projection.output,
                    projection.weights,
                    x,
                    projection.rows,
                    cols,
                    projection.output_scale,
                    stream,
                )?;
            }
        } else {
            self.kernels
                .gemv_nvfp4_gguf_q8_1_group_f16(&group, x, cols, stream)?;
        }
        Ok(true)
    }



    /// GEMV + residual add into the decode residual pair (h, h32).
    pub(crate) fn gemv_residual(&self, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Same dp4a policy as `gemv`: Q8_0/Q4_K quantize x block-locally and
        // dot with dp4a (wins at every decode shape), Q6_K keeps the f16-x
        // kernel (already bandwidth-bound; dp4a's shared staging loses
        // occupancy at the wide down-projection).
        let b = &self.bufs;
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_residual_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q8_0_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q8_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                rows,
                cols,
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                    self.kernels.gemv_residual_nvfp4_f16(
                        &b.h,
                        &b.h32,
                        packed,
                        scales,
                        x,
                        *rows,
                        *cols,
                        *inv_global_scale,
                        stream,
                    )
                }
                NvFp4CtStorage::S0N64K128 { data } => {
                    let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                    self.kernels.gemv_residual_nvfp4_ct_s0_f16(
                        &b.h,
                        &b.h32,
                        view,
                        x,
                        *inv_global_scale,
                        stream,
                    )
                }
            },
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q4_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q4_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q6_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q6_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q3_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q2_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_nl_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_residual_mxfp4_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_m_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_residual nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    pub(crate) fn logits_gemv(&self, y_f32: &DevBuffer, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Głowa NIE korzysta z paczki `fp8_lm_head`, choć ta jest budowana razem
        // z paczkami FFN. e4m3 ma 3-bitową mantysę w warstwie, która wprost
        // wybiera token: użycie jej TYLKO tutaj dawało inny strumień greedy w
        // pojedynczym strumieniu niż w batchu (który liczy głowę w F16), czyli
        // jakość zależną od współbieżności. Paczka zostaje dla prefillu, gdzie
        // liczą się aktywacje, a nie wybór tokena.
        if let Some(tp) = &self.tp_ffn {
            if tp.forward_logits(stream, x, y_f32)? {
                return Ok(());
            }
        }
        self.logits_weight_gemv(y_f32, 0, x, 0, &self.weights.lm_head, stream)
    }

    pub(crate) fn logits_weight_gemv(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        x: &DevBuffer,
        x_off: usize,
        weight: &DevWeight,
        stream: &Stream,
    ) -> Result<()> {
        self.gemv_out_f32(y_f32, y_off, x, x_off, weight, stream)?;
        // Ograniczenie logitów (Gemma): cap * tanh(x / cap). Nakładane tutaj, bo
        // to jedyne wyjście głowy — sampling i logprob widzą już wartości po capie.
        let cap = self.weights.descriptor.params.final_logit_softcap;
        if cap > 0.0 {
            self.kernels
                .softcap_f32(y_f32, y_off, weight.rows(), cap, stream)?;
        }
        // Maska tokenów zabronionych: kopie stream-ordered z jednoelementowego
        // bufora -inf, więc mieszczą się w przechwytywanym grafie decode.
        if let Some(neg_inf) = &self.weights.neg_inf {
            for &id in &self.weights.descriptor.params.suppress_tokens {
                let slot = y_off + id as usize;
                if slot < y_off + weight.rows() {
                    self.device.copy(neg_inf, 0, y_f32, slot * 4, 4, stream)?;
                }
            }
        }
        Ok(())
    }

    /// Projekcja z wyjściem f32, bez obróbki właściwej głowie logitów.
    ///
    /// Ma dwóch wołających i oba potrzebują dokładnie tego samego: głowa logitów
    /// (która dokłada cap i maskę) oraz macierz WIERSZOWO równoległa podziału na
    /// rangi, której wynik jest sumą CZĄSTKOWĄ. Kontrakt liczbowy podziału
    /// wymaga, żeby ranga akumulowała w f32 i żeby zawężenie do f16 nastąpiło
    /// dopiero PO sumie — czyli dokładnie tego, co daje ta rodzina kerneli.
    pub(crate) fn gemv_out_f32(
        &self,
        y_f32: &DevBuffer,
        y_off: usize,
        x: &DevBuffer,
        x_off: usize,
        weight: &DevWeight,
        stream: &Stream,
    ) -> Result<()> {
        if (y_off != 0 || x_off != 0)
            && !matches!(weight, DevWeight::Q4K { .. } | DevWeight::Q6K { .. })
        {
            return Err(ForgeError::Unsupported(
                "gemv z wyjściem f32 i offsetem lane obsługuje tylko Q4_K/Q6_K".into(),
            ));
        }
        let out = match weight {
            // Wagi FP8 ze skalą wierszową mają wariant GEMV z wyjściem f16;
            // głowa logitów potrzebuje f32, więc dostanie własną ścieżkę razem
            // z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => {
                return Err(ForgeError::Unsupported(
                    "wagi FP8 ze skalą wierszową nie mają GEMV z wyjściem f32".into(),
                ))
            }
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_f16_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemv_q8_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q4_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q6_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q6_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_q5_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_q3_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_q2_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_q4_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_q4_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_q5_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_q5_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_mxfp4_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_iq2_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_iq3_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_iq1_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_iq1_m_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4 { .. } => Err(ForgeError::Unsupported(
                "NVFP4 compressed-tensors nie ma kernela GEMV z wyjściem f32".into(),
            )),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } => self.kernels.gemv_nvfp4_gguf_out_f32(
                if *layout == Nvfp4GgufLayout::RowMajor36 {
                    y_f32
                } else {
                    return Err(ForgeError::Unsupported(
                        "GEMV z wyjściem f32 nie obsługuje TileN128K64".into(),
                    ));
                },
                buf,
                x,
                *rows,
                *cols,
                *output_scale,
                stream,
            ),
        };
        out
    }

    pub(crate) fn gemm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_rows(y, w, x, n_tokens, 0, w.rows(), stream)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_nvfp4_ct_direct(
        &self,
        y_padded: &DevBuffer,
        workspace: &DevBuffer,
        weight: &DevWeight,
        x_padded: &DevBuffer,
        logical_m: usize,
        projection: Nvfp4CtProjection,
        stream: &Stream,
    ) -> Result<bool> {
        if nvfp4_ct_physical_m(logical_m).is_none()
            || Self::nvfp4_ct_projection(weight) != Some(projection)
        {
            return Ok(false);
        }
        let DevWeight::NvFp4 {
            inv_global_scale,
            rows,
            ..
        } = weight
        else {
            return Ok(false);
        };
        let window = weight.nvfp4_ct_row_window(0, *rows)?;
        let view = Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
        self.kernels.gemm_nvfp4_ct_padded(
            y_padded,
            if projection == Nvfp4CtProjection::GateUp {
                None
            } else {
                Some(workspace)
            },
            view,
            x_padded,
            logical_m,
            projection,
            *inv_global_scale,
            stream,
        )?;
        Ok(true)
    }

    pub(crate) fn gemm_q8_prepared(
        &self,
        y: &DevBuffer,
        weight: &DevWeight,
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        let DevWeight::Q8_0 {
            buf, rows, cols, ..
        } = weight
        else {
            return Err(ForgeError::Format(
                "przygotowana grupa DeltaNet wymaga wag Q8_0".into(),
            ));
        };
        self.kernels
            .gemm_q8_0_i8mma_prepared_at(y, buf, 0, prepared, *rows, *cols, n_tokens)
    }

    pub(crate) fn gemm_q8_prepared_triplet(
        &self,
        outputs: [&DevBuffer; 3],
        weights: [&DevWeight; 3],
        prepared: &mut Q8ActPrepared<'_>,
        n_tokens: usize,
    ) -> Result<()> {
        fn projection<'a>(
            output: &'a DevBuffer,
            weight: &'a DevWeight,
        ) -> Result<(Q8PreparedProjection<'a>, usize)> {
            let DevWeight::Q8_0 {
                buf, rows, cols, ..
            } = weight
            else {
                return Err(ForgeError::Format(
                    "fused grupa DeltaNet wymaga wag Q8_0".into(),
                ));
            };
            Ok((
                Q8PreparedProjection {
                    output,
                    weights: buf,
                    weight_byte_offset: 0,
                    rows: *rows,
                },
                *cols,
            ))
        }
        let (gate, cols) = projection(outputs[0], weights[0])?;
        let (alpha, alpha_cols) = projection(outputs[1], weights[1])?;
        let (beta, beta_cols) = projection(outputs[2], weights[2])?;
        if alpha_cols != cols || beta_cols != cols {
            return Err(ForgeError::Format(
                "fused grupa DeltaNet wymaga wspólnego rozmiaru wejścia".into(),
            ));
        }
        self.kernels.gemm_q8_0_i8mma_prepared_triplet(
            &[gate, alpha, beta],
            prepared,
            cols,
            n_tokens,
        )
    }

    /// W4A8 prefill projection GEMM (per-token int8 activation quant + int4xint8
    /// GEMM). Each W4A8 weight is a standalone logical matrix, so no windowing.
    /// Wykonuje projekcję wybraną wcześniej. `shared_act` mówi, że fuzowana
    /// norma wyemitowała już wspólną aktywację e4m3 i nie trzeba jej kwantyzować
    /// per projekcja.
    pub(crate) fn project(
        &self,
        y: &DevBuffer,
        p: &ProjectionPlan<'_>,
        x: &DevBuffer,
        rows: usize,
        shared_act: bool,
        stream: &Stream,
    ) -> Result<()> {
        match p {
            ProjectionPlan::W4A8(w) => self.gemm_w4a8(y, w, x, rows, stream),
            ProjectionPlan::Fp8(w) if shared_act => self.gemm_fp8_prequant(y, w, rows, stream),
            ProjectionPlan::Fp8(w) => self.gemm_fp8(y, w, x, rows, stream),
            ProjectionPlan::Rows { w, row_off, rows: n } => {
                self.gemm_rows(y, w, x, rows, *row_off, *n, stream)
            }
        }
    }

    /// Q/K/V warstwy `l` w jednej postaci, niezależnie od formatu i układu wag.
    pub(crate) fn qkv_projections<'w>(
        &'w self,
        layer: &'w LayerWeights,
        w4a8: Option<&'w W4A8Layer>,
        fp8: Option<&'w Fp8Layer>,
        fp8_ffn: Option<&'w Fp8FfnLayer>,
        q_rows: usize,
        kv_rows: usize,
    ) -> [ProjectionPlan<'w>; 3] {
        if let Some(wl) = w4a8 {
            return [
                ProjectionPlan::W4A8(&wl.q),
                ProjectionPlan::W4A8(&wl.k),
                ProjectionPlan::W4A8(&wl.v),
            ];
        }
        if let Some(fl) = fp8 {
            return [
                ProjectionPlan::Fp8(&fl.q),
                ProjectionPlan::Fp8(&fl.k),
                ProjectionPlan::Fp8(&fl.v),
            ];
        }
        if let Some(fl) = fp8_ffn {
            return [
                ProjectionPlan::Fp8(&fl.q),
                ProjectionPlan::Fp8(&fl.k),
                ProjectionPlan::Fp8(&fl.v),
            ];
        }
        match &layer.attn().attn_qkv {
            QkvWeights::Fused(w) => [
                ProjectionPlan::Rows { w, row_off: 0, rows: q_rows },
                ProjectionPlan::Rows { w, row_off: q_rows, rows: kv_rows },
                ProjectionPlan::Rows { w, row_off: q_rows + kv_rows, rows: kv_rows },
            ],
            QkvWeights::FusedQk { qk, v } => [
                ProjectionPlan::Rows { w: qk, row_off: 0, rows: q_rows },
                ProjectionPlan::Rows { w: qk, row_off: q_rows, rows: kv_rows },
                ProjectionPlan::Rows { w: v, row_off: 0, rows: v.rows() },
            ],
            QkvWeights::Split { q, k, v } => [
                ProjectionPlan::Rows { w: q, row_off: 0, rows: q.rows() },
                ProjectionPlan::Rows { w: k, row_off: 0, rows: k.rows() },
                ProjectionPlan::Rows { w: v, row_off: 0, rows: v.rows() },
            ],
        }
    }

    pub(crate) fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        w: &W4A8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels.gemm_w4a8(
            y,
            &w.qweight,
            &w.s2_zeros,
            &w.s2_scales,
            &w.s1_scales,
            &w.inv_smooth,
            x,
            w.rows,
            w.cols,
            n_tokens,
            stream,
        )
    }

    /// fp8 (e4m3) prefill projection GEMM (per-token e4m3 activation quant +
    /// e4m3×e4m3 tensor-core GEMM). Each fp8 weight is a standalone logical
    /// matrix, so no windowing. `FORGE_GEMM=fp8`.
    pub(crate) fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if self.weights.fp8_modular {
            return self.kernels.gemm_fp8_modular(
                y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
            );
        }
        self.kernels.gemm_fp8(
            y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
        )
    }

    /// fp8mod projection over the shared per-token e4m3 activation the preceding
    /// fused rmsnorm→fp8 emitted (q/k/v share one, gate/up share one) — no
    /// per-projection activation requant. `FORGE_GEMM=fp8mod` only.
    pub(crate) fn gemm_fp8_prequant(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels
            .gemm_fp8_modular_prequant(y, &w.qweight, &w.scales, w.rows, w.cols, n_tokens, stream)
    }

    /// Batched GEMM over a row window of `w`: y = W[row_off..row_off+n_rows]·x.
    /// Row offsets translate to per-format byte offsets into the weight (and,
    /// for NVFP4, scale) streams — this is how prefill reads the q/k/v and
    /// gate/up sections out of a fused matrix without storing them twice.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemm_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Przesunięcie wiersza podaje format z własnej geometrii bloku.
        // Miejsce wywołania przepisywało je wcześniej osobno dla każdego
        // z osiemnastu formatów, a rozmiar bloku i długość bloku w bajtach
        // są tą samą wiedzą, którą `QuantKind` już niesie.
        let row_bytes = || -> Result<usize> {
            w.row_offset_bytes(row_off).ok_or_else(|| {
                ForgeError::Unsupported(
                    "ten format nie adresuje wiersza jednym przesunięciem".into(),
                )
            })
        };
        match w {
            // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
            // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
            DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
            )),
            DevWeight::F16 { buf, cols, .. } => self.kernels.gemm_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            // Q8_0 / Q4_K prefill run the int8 TENSOR-CORE MMQ GEMM: activations
            // quantized to q8_1, weights kept as native codes, s8xs8->s32 mma
            // (m16n8k32) per 32-block, then per-block scale/min to f16. This is
            // the only path that beats the f16 tensor-core GEMM on Ada (2x MAC
            // throughput + zero dequant bandwidth). Decode still uses the dp4a
            // GEMV (see gemv). Marshalling the mma's 4x s32 output uses
            // inlined_assembly + _RegisterPackType (see kernels/mojo/MOJO_NOTES.md).
            DevWeight::Q8_0 { buf, cols, .. } => {
                let off = row_bytes()?;
                // Jeden token bierze ten sam dp4a GEMV co dekod jednosekwencyjny.
                // Kafel i8mma dopełnia do >=64 tokenów i kwantyzuje aktywacje
                // inaczej, więc ścieżka batchowa dla B=1 dawała trwale inne
                // logity niż serialna przy zerowym zysku wydajności.
                if n_tokens == 1 {
                    return self
                        .kernels
                        .gemv_q8_0_dp4a_f16_at(y, buf, off, x, n_rows, *cols, stream);
                }
                if self
                    .kernels
                    .gemm_q8_0_small_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q8_0_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            // Small decode batches (T=2/4/8/16) take the weight-stationary
            // dp4a GEMV: one weight sweep serves every token instead of the
            // >=64-token tile the GEMM kernels pad to.
            DevWeight::Q4K { buf, cols, .. } => {
                let off = row_bytes()?;
                if self
                    .kernels
                    .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, false, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q4_k_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            DevWeight::Q6K { buf, cols, .. } => {
                let off = row_bytes()?;
                if self
                    .kernels
                    .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, true, stream)?
                {
                    return Ok(());
                }
                self.kernels
                    .gemm_q6_k_f16_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
            }
            DevWeight::Q5K { buf, cols, .. } => self.kernels.gemm_q5_k_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q3K { buf, cols, .. } => self.kernels.gemm_q3_k_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q2K { buf, cols, .. } => self.kernels.gemm_q2_k_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_0 { buf, cols, .. } => self.kernels.gemm_q4_0_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_1 { buf, cols, .. } => self.kernels.gemm_q4_1_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_0 { buf, cols, .. } => self.kernels.gemm_q5_0_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_1 { buf, cols, .. } => self.kernels.gemm_q5_1_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Nl { buf, cols, .. } => self.kernels.gemm_iq4_nl_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Xs { buf, cols, .. } => self.kernels.gemm_iq4_xs_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Mxfp4 { buf, cols, .. } => self.kernels.gemm_mxfp4_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xs { buf, cols, .. } => self.kernels.gemm_iq2_xs_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2S { buf, cols, .. } => self.kernels.gemm_iq2_s_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3S { buf, cols, .. } => self.kernels.gemm_iq3_s_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, cols, .. } => self.kernels.gemm_iq2_xxs_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, cols, .. } => self.kernels.gemm_iq3_xxs_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1S { buf, cols, .. } => self.kernels.gemm_iq1_s_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1M { buf, cols, .. } => self.kernels.gemm_iq1_m_f16_at(
                y,
                buf,
                row_bytes()?,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::NvFp4 {
                storage,
                inv_global_scale,
                cols,
                ..
            } => match storage {
                NvFp4CtStorage::RowMajorE4M3 { packed, scales } => self.kernels.gemm_nvfp4_f16_at(
                    y,
                    packed,
                    row_off * (cols / 2),
                    scales,
                    row_off * (cols / 16),
                    x,
                    n_rows,
                    *cols,
                    n_tokens,
                    *inv_global_scale,
                    stream,
                ),
                NvFp4CtStorage::S0N64K128 { .. } => {
                    let window = w.nvfp4_ct_row_window(row_off, n_rows)?;
                    let view =
                        Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
                    if n_tokens <= 16 {
                        return self.kernels.gemv_batch_nvfp4_ct_s0_n64k128_f16_at(
                            y,
                            0,
                            view,
                            x,
                            0,
                            window.row_offset(),
                            window.rows(),
                            n_tokens,
                            *inv_global_scale,
                            stream,
                        );
                    }
                    self.kernels.gemm_nvfp4_ct_s0_f16_at(
                        y,
                        view,
                        x,
                        window.row_offset(),
                        window.rows(),
                        n_tokens,
                        *inv_global_scale,
                        stream,
                    )
                }
            },
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } if row_off == 0 && n_rows == *rows => self.kernels.gemm_nvfp4_gguf_layout_f16(
                y,
                buf,
                x,
                *rows,
                *cols,
                n_tokens,
                *output_scale,
                *layout,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "GGUF NVFP4 GEMM nie obsługuje okna wierszy".into(),
            )),
        }
    }

    /// Single-token GEMV over a row window of `w` (`y = W[row_off..+n_rows]·x`).
    /// The routed-MoE expert path uses this instead of the batched `gemm_rows`:
    /// a decode step feeds one token, and the GEMM tile (BM=64) then launches
    /// only `n_rows/64` blocks — far too few to saturate the SMs, so the GPU
    /// stays at idle clocks. The per-row GEMV kernels launch `n_rows/8` blocks
    /// (8 experts queued back-to-back per layer keep the device busy enough to
    /// boost). Formats without an offset GEMV variant fall back to the tile.
    pub(crate) fn gemv_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::Q4K { buf, cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_at(
                    y,
                    buf,
                    row_off * (cols / 256) * 144,
                    x,
                    n_rows,
                    *cols,
                    stream,
                )
            }
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemv_q6_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 210,
                x,
                n_rows,
                *cols,
                stream,
            ),
            _ => self.gemm_rows(y, w, x, 1, row_off, n_rows, stream),
        }
    }

    /// Single-token GEMV over the expert selected on-device by `ids[sel]`: the
    /// device analog of `gemv_rows`, resolving that expert's weight base from
    /// the stack's device-resident pointer table inside the kernel.
    /// Bit-identical to `gemv_rows(y, stack.expert(ids[sel]), x, 0, n_rows, ..)`.
    /// Only the quants `expert_stack_gidx` accepts reach here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gemv_rows_gidx(
        &self,
        y: &DevBuffer,
        stack: &ExpertStack,
        x: &DevBuffer,
        ids: &DevBuffer,
        sel: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match stack.representative() {
            DevWeight::Q4K { cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => self
                .kernels
                .gemv_q4_k_dp4a_f16_gidx(y, stack.table(), x, n_rows, *cols, ids, sel, stream),
            DevWeight::Q6K { cols, .. } => self.kernels.gemv_q6_k_f16_gidx(
                y,
                stack.table(),
                x,
                n_rows,
                *cols,
                ids,
                sel,
                stream,
            ),
            _ => Err(ForgeError::Unsupported(
                "gemv_rows_gidx called for a non-gidx expert quant".into(),
            )),
        }
    }

    /// Batchowa głowa logitów: y[b, vocab] f32 = lm_head · x[b, hidden].
    pub(crate) fn logits_gemm(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        match &self.weights.lm_head {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemm_f16_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } if (2..=8).contains(&n_tokens) => self
                .kernels
                .gemm_q8_0_f16_exact_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemm_q8_0_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout: Nvfp4GgufLayout::RowMajor36,
            } if matches!(n_tokens, 2 | 4 | 8 | 16) => self.kernels.gemm_nvfp4_gguf_out_f32_batch(
                y_f32,
                buf,
                x,
                *rows,
                *cols,
                n_tokens,
                *output_scale,
                stream,
            ),
            // Q6_K ma batchowy przemiat z wyjściem f32: jeden odczyt wag na
            // cały batch. Q4_K głowy go nie ma, więc zostaje przy przemiatnięciu
            // per token (patrz niżej).
            DevWeight::Q6K { buf, rows, cols }
                if self.kernels.gemv_q6_k_dp4a_batch_out_f32_at(
                    y_f32, buf, 0, x, *rows, *cols, n_tokens, stream,
                )? =>
            {
                Ok(())
            }
            // Pozostałe głowy K-kwantowe nie mają batchowego GEMM-a out-f32;
            // przemiat dp4a per token zostawia resztę kroku batchowego bez
            // zmian (głowa to jeden odczyt wag na token, stos warstw nadal jest
            // amortyzowany po batchu).
            w @ (DevWeight::Q4K { rows, cols, .. } | DevWeight::Q6K { rows, cols, .. }) => {
                for lane in 0..n_tokens {
                    self.logits_weight_gemv(
                        y_f32,
                        lane * *rows * 4,
                        x,
                        lane * *cols * 2,
                        w,
                        stream,
                    )?;
                }
                Ok(())
            }
            _ => Err(ForgeError::Unsupported(
                "batchowa głowa logitów nie obsługuje tego formatu ani szerokości".into(),
            )),
        }
    }

}
