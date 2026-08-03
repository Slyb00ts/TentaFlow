// ===== File: model/tp.rs — podzial tensorowy miedzy rangami =====
use super::*;

impl Model {
    /// Odmawia wszystkiego, czego sterownik podziału jeszcze nie prowadzi.
    ///
    /// Ścieżka, której podział nie obejmuje, nie liczy się źle w sposób widoczny:
    /// ranga po prostu użyłaby pociętej macierzy bez redukcji i wyprodukowała
    /// tekst-śmieć bez błędu i bez asercji. Dlatego to jest twarda odmowa przy
    /// starcie, a nie ostrzeżenie.
    pub(crate) fn tp_refuse_uncovered(&self, cfg: &ModelConfig) -> Result<()> {
        let refuse = |what: &str| -> Result<()> {
            Err(ForgeError::Unsupported(format!(
                "podział na rangi nie obejmuje jeszcze: {what}"
            )))
        };
        if self.weights.is_moe() {
            return refuse("warstw MoE");
        }
        if cfg.native_mtp {
            return refuse("natywnej głowy MTP/NextN");
        }
        if cfg.kv_tier.enabled() {
            return refuse("tieringu KV");
        }
        Ok(())
    }

    /// Macierz WIERSZOWO równoległa dla `n_tokens` wierszy naraz.
    ///
    /// `T` jest tu PARAMETREM, a nie osobną architekturą: dekodowanie
    /// (`T = 1`), weryfikacja draftu (`T = 3/4`) i prefill (`T` = chunk) idą tą
    /// samą drogą i przez te same dwa punkty redukcji.
    pub(crate) fn row_parallel_gemm(
        &self,
        dst: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Jedna karta liczy DOKŁADNIE to co przedtem — także dla `T = 1`, które w
        // ścieżce batchowej idzie kaflem GEMM, a nie GEMV. Sprowadzenie go tutaj
        // do GEMV zmieniło rodzinę kerneli i wygenerowany tekst.
        let Some(partial) = &self.tp_partial else {
            return self.gemm(dst, w, x, n_tokens, stream);
        };
        if n_tokens == 1 {
            return self.gemv_out_f32(partial, 0, x, 0, w, stream);
        }
        match w {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemm_f16_out_f32_at(partial, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemm_q8_0_out_f32_at(partial, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
                layout,
            } => {
                if *layout != Nvfp4GgufLayout::RowMajor36 {
                    return Err(ForgeError::Unsupported(
                        "suma cząstkowa GGUF NVFP4 wymaga układu RowMajor36".into(),
                    ));
                }
                // B = 2/4/8/16 to rodzina małych batchy, ważona per token.
                // Powyżej idzie kafel macierzowy z epilogiem f32 — ten sam
                // przepływ co wariant f16, tylko bez zawężenia wyniku.
                match n_tokens {
                    2 | 4 | 8 | 16 => self.kernels.gemm_nvfp4_gguf_out_f32_batch(
                        partial,
                        buf,
                        x,
                        *rows,
                        *cols,
                        n_tokens,
                        *output_scale,
                        stream,
                    ),
                    _ => self.kernels.gemm_nvfp4_gguf_wmma_out_f32(
                        partial,
                        buf,
                        x,
                        *rows,
                        *cols,
                        n_tokens,
                        *output_scale,
                        stream,
                    ),
                }
            }
            other => Err(ForgeError::Unsupported(format!(
                "podział nie ma GEMM z wyjściem f32 dla formatu {:?} przy T={n_tokens}",
                std::mem::discriminant(other)
            ))),
        }
    }

    /// Macierz WIERSZOWO równoległa: `attn_output`, `ssm_out` i `ffn_down`.
    ///
    /// Na jednej karcie to zwykła projekcja do bufora f16. Gdy model jest rangą
    /// podziału, ta sama macierz ma podzielone WEJŚCIE, więc jej wynik to suma
    /// cząstkowa — idzie w f32 do bufora rangi i czeka na redukcję, która dopiero
    /// zapisze `dst`. To są jedyne trzy miejsca, w których cokolwiek przechodzi
    /// między kartami.
    pub(crate) fn row_parallel_gemv(
        &self,
        dst: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        match &self.tp_partial {
            Some(partial) => self.gemv_out_f32(partial, 0, x, 0, w, stream),
            None => self.gemv(dst, w, x, stream),
        }
    }

    /// Enqueue one decode step (graph replay) on the model stream WITHOUT
    /// downloading logits or synchronizing. The next-token logits are left
    /// in the device logits buffer for either the pinned D2H (`step`) or the
    /// on-GPU sampler (`step_and_sample`).
    /// Czy model nadaje się do rozłożenia FFN na karty.
    ///
    /// Wymagania są wymaganiami ŚCIEŻKI, nie zachcianką: podział działa na
    /// gęstym FFN liczonym jawnym łańcuchem dekodowania, więc MoE, model
    /// hybrydowy, obrotowy cache i tiering odpadają — każde z nich ma własną
    /// pętlę warstw, w której tego FFN po prostu nie ma.
    pub fn tp_ffn_capable(&self) -> Result<()> {
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "podział FFN na karty obejmuje modele gęste".into(),
            ));
        }
        // Model hybrydowy MA gęsty FFN w każdej warstwie — inny jest tylko mikser
        // przed nim. Podział czyta wagi z pliku po rolach, więc jedynym realnym
        // wymaganiem jest rozdzielone `gate`/`up`: scalona macierz nie ma jak
        // dostać granicy wiersza wspólnej z podziałem kolumn `down`.
        for (index, layer) in self.weights.layers.iter().enumerate() {
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Err(ForgeError::Unsupported(format!(
                    "warstwa {index} nie ma gęstego FFN do podziału"
                )));
            };
            if matches!(ffn.gate_up, GateUpWeights::Fused(_)) {
                return Err(ForgeError::Unsupported(format!(
                    "warstwa {index} ma scalone gate/up, podział wymaga rozdzielonych"
                )));
            }
        }
        if self.kv.cfg.quant.is_rot() {
            return Err(ForgeError::Unsupported(
                "obrotowy cache KV idzie osobnym łańcuchem dekodowania".into(),
            ));
        }
        if self.tier.is_some() {
            return Err(ForgeError::Unsupported(
                "tiering KV wyklucza podział FFN na karty".into(),
            ));
        }
        Ok(())
    }

    /// Rozkłada FFN modelu na `extra` dodatkowych kart obok tej, na której model
    /// już stoi.
    ///
    /// Wagi FFN są czytane z pliku PONOWNIE i cięte planem ze zmierzonej mocy
    /// kart. Prefill nadal liczy je macierzowo na jednej karcie, więc jego kopia
    /// zostaje — podział obejmuje dekodowanie, gdzie FFN jest ograniczony pasmem
    /// i druga karta faktycznie coś wnosi.
    pub fn enable_tp_ffn(
        &mut self,
        path: &Path,
        extra: &[forge_hal::gpu::DeviceId],
        pools: forge_hal::PoolSizes,
        layer_range: Option<(usize, usize)>,
        forced: Option<&[usize]>,
    ) -> Result<()> {
        if extra.is_empty() {
            return Err(ForgeError::Scheduler(
                "podział FFN wymaga co najmniej jednej dodatkowej karty".into(),
            ));
        }
        self.tp_ffn_capable()?;
        // Kalibracja musi iść formatem, którym model faktycznie liczy — stosunek
        // mocy kart zależy od niego i przy różnych architekturach potrafi się
        // odwrócić.
        let quant = match &self.weights.layers[0].ffn {
            LayerFfn::Dense(ffn) => match &ffn.gate_up {
                GateUpWeights::Split { gate, .. } => gate.split_quant(),
                GateUpWeights::Fused(_) => None,
            },
            LayerFfn::Moe(_) => None,
        }
        .ok_or_else(|| {
            ForgeError::Unsupported("format wag FFN nie ma ścieżki podziału na karty".into())
        })?;
        let cluster = crate::cluster::Cluster::attach(self.device.clone(), extra, pools)?;
        let mut caps = cluster.calibrate(quant)?;
        let layers =
            crate::weights::load_ffn_shards_gguf(path, &cluster, &caps, layer_range, forced)?;
        if layers.len() != self.weights.layers.len() {
            return Err(ForgeError::Format(format!(
                "podział objął {} warstw, model ma {}",
                layers.len(),
                self.weights.layers.len()
            )));
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        let mut tp = crate::tensor_parallel::TpDecode::new(cluster, layers, hidden)?;
        // Każdy kolejny podział planuje pojemność z ODŚWIEŻONEGO stanu pul —
        // inaczej trzy niezależne plany obiecałyby sobie nawzajem to samo
        // miejsce na karcie modelu.
        tp.refresh_free(&mut caps);
        // Dwie duże projekcje wejściowe DeltaNet: razem 16,5% odczytu na token.
        tp.attach_delta_projections(&caps, &crate::weights::load_delta_projection_source(path)?)?;
        tp.refresh_free(&mut caps);
        // Głowa logitów to jedna macierz czytana raz na token — na tym modelu
        // 8% całego odczytu, czyli więcej niż wszystkie projekcje uwagi razem.
        // Dzieli się po wierszach słownika, więc wynik jest bitowo zgodny z
        // jednokartowym.
        if let Some((data, vocab, cols, quant)) = crate::weights::load_lm_head_shard_source(path)? {
            if cols == hidden && vocab == self.weights.descriptor.params.vocab_size {
                tp.attach_lm_head(&caps, &data, vocab, cols, quant)?;
            }
        }
        // Weryfikacja draftu MTP przepuszcza przez warstwę cały draft naraz;
        // kernele sum cząstkowych f32 obsługują do 16 tokenów.
        tp.attach_batch(16, hidden)?;
        self.tp_ffn = Some(tp);
        // Krok dekodowania przestaje być jednokartowy, więc przechwycone grafy
        // przestają go opisywać — dotyczy to obu ścieżek, gęstej i hybrydowej.
        self.decode_graph = None;
        self.decode_hybrid_graph = None;
        self.hybrid_verify_graphs.clear();
        Ok(())
    }

    pub fn tp_ffn(&self) -> Option<&crate::tensor_parallel::TpDecode> {
        self.tp_ffn.as_ref()
    }

    /// Chunk batchowy rozłożony na rangi.
    ///
    /// Ten sam kształt co dekodowanie — trzy części i dwie redukcje — tylko `T`
    /// jest większe. To jest cała różnica między prefillem a dekodowaniem pod
    /// podziałem i dlatego prefill nie potrzebuje własnej architektury.
    pub(crate) fn run_hybrid_batch_layers_tp(&self, t: usize, commit_prefill: bool) -> Result<()> {
        for member in self.tp_ranks() {
            member.hybrid_batch_entry_norm(t)?;
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        if t > MAX_SPLIT_PREFILL_CHUNK {
            return Err(ForgeError::Unsupported(format!(
                "chunk {t} przekracza bufor sumy cząstkowej ({MAX_SPLIT_PREFILL_CHUNK})"
            )));
        }
        for layer_index in 0..self.weights.layers.len() {
            for member in self.tp_members() {
                member.hybrid_batch_mixer(layer_index, t, commit_prefill)?;
            }
            self.tp_all_reduce(Self::prefill_o_out, t * hidden)?;
            for member in self.tp_members() {
                member.hybrid_batch_ffn(layer_index, t)?;
            }
            self.tp_all_reduce(Self::prefill_down, t * hidden)?;
            for member in self.tp_members() {
                member.hybrid_batch_residual(layer_index, t)?;
            }
        }
        Ok(())
    }

    /// Krok hybrydowy OD wstawionego embeddingu — bez niczego zależnego od
    /// `token_id`, więc nadaje się do przechwycenia w graf.
    /// Rangi POZA zerową. Pusto, gdy model liczy jedna karta.
    pub(crate) fn tp_ranks(&self) -> impl Iterator<Item = &Model> {
        self.tp
            .as_ref()
            .map(|tp| tp.ranks.as_slice())
            .unwrap_or_default()
            .iter()
    }

    pub(crate) fn tp_rank_count(&self) -> usize {
        self.tp.as_ref().map_or(0, |tp| tp.ranks.len())
    }

    /// Rangi podziału w kolejności numerów: zerowa, potem reszta.
    fn tp_members(&self) -> impl Iterator<Item = &Model> {
        std::iter::once(self).chain(
            self.tp
                .as_ref()
                .map(|tp| tp.ranks.as_slice())
                .unwrap_or_default()
                .iter(),
        )
    }

    /// Domyka jeden punkt redukcji: sumy cząstkowe rang wchodzą, a każda ranga
    /// wychodzi z pełnym wektorem we wskazanym buforze f16.
    ///
    /// Obie rangi potrzebują wyniku, bo każda liczy własny `rmsnorm_residual` na
    /// własnym strumieniu rezydualnym — a ten jest replikowany za darmo, skoro
    /// wagi norm ładują się jako `Replicated`.
    fn tp_all_reduce(&self, out_of: fn(&Model) -> &DevBuffer, elems: usize) -> Result<()> {
        let tp = self
            .tp
            .as_ref()
            .ok_or_else(|| ForgeError::Scheduler("redukcja bez aktywnego podziału".into()))?;
        let members: Vec<&Model> = self.tp_members().collect();
        let ranks: Vec<crate::cluster::ReduceRank<'_>> = members
            .iter()
            .enumerate()
            .map(|(index, member)| crate::cluster::ReduceRank {
                device: member.device.as_ref(),
                stream: &member.stream,
                kernels: &member.kernels,
                done: &tp.events[index],
                read_done: &tp.read_events[index],
                part: member.tp_partial.as_ref(),
            })
            .collect();
        let out: Vec<&DevBuffer> = members.iter().map(|m| out_of(m)).collect();
        let acc: Vec<&DevBuffer> = tp.acc.iter().collect();
        crate::cluster::all_reduce_f16(&ranks, &acc, &out, elems)
    }

    /// Krok modelu hybrydowego rozłożony na rangi.
    ///
    /// To jest CAŁY sterownik podziału: ta sama warstwa wykonuje się na każdej
    /// randze na jej fragmencie wag, a między częściami stoją dokładnie dwie
    /// redukcje — po projekcji wyjściowej miksera i po `down` FFN. Prefill,
    /// dekodowanie i każda inna ścieżka przechodząca przez warstwę dostają
    /// podział z tego jednego miejsca, bo `T` jest parametrem, a nie osobną
    /// architekturą.
    pub(crate) fn hybrid_forward_staged_tp(&self, want_logits: bool) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let n_layers = self.weights.layers.len();
        for member in self.tp_members() {
            member.kernels.rmsnorm_f16(
                &member.bufs.x,
                &member.bufs.h,
                &member.weights.layers[0].attn_norm,
                1,
                hidden,
                eps,
                &member.stream,
            )?;
        }
        for l in 0..n_layers {
            for member in self.tp_members() {
                member.hybrid_decode_mixer(l, &AttnSrc::Paged)?;
            }
            self.tp_all_reduce(Self::decode_o_out, hidden)?;
            for member in self.tp_members() {
                member.hybrid_decode_ffn(l)?;
            }
            self.tp_all_reduce(Self::decode_down, hidden)?;
            for member in self.tp_members() {
                member.hybrid_decode_residual(l)?;
            }
        }
        if want_logits {
            // Głowa logitów jest replikowana, więc liczy ją ranga zerowa na
            // swoim (już pełnym) strumieniu rezydualnym.
            self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        }
        Ok(())
    }

    /// Krok modelu gęstego rozłożony na rangi.
    ///
    /// Ten sam kształt co hybrydowy — trzy części warstwy i dwie redukcje między
    /// nimi. Różni się wyłącznie tym, którą warstwę woła, bo mikser gęsty i
    /// hybrydowy mają inne wnętrze, a ten sam punkt cięcia.
    pub(crate) fn dense_forward_staged_tp(&self, src: &AttnSrc, want_logits: bool) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let n_layers = self.weights.layers.len();
        for member in self.tp_members() {
            member.kernels.gather_rows_f16(
                &member.bufs.h,
                &member.weights.token_embd_f16,
                &member.bufs.ids,
                1,
                hidden,
                &member.stream,
            )?;
            if let Some(factor) = p.embd_scale {
                member
                    .kernels
                    .scale_f16(&member.bufs.h, hidden, factor, &member.stream)?;
            }
            member.kernels.rmsnorm_f16(
                &member.bufs.x,
                &member.bufs.h,
                &member.weights.layers[0].attn_norm,
                1,
                hidden,
                eps,
                &member.stream,
            )?;
        }
        for l in 0..n_layers {
            for member in self.tp_members() {
                member.dense_decode_mixer(l, src)?;
            }
            self.tp_all_reduce(Self::decode_o_out, hidden)?;
            for member in self.tp_members() {
                member.dense_decode_ffn(l)?;
            }
            self.tp_all_reduce(Self::decode_down, hidden)?;
            for member in self.tp_members() {
                member.dense_decode_residual(l)?;
            }
        }
        if want_logits {
            self.logits_gemv(&self.bufs.logits, &self.bufs.x, &self.stream)?;
        }
        Ok(())
    }

}
