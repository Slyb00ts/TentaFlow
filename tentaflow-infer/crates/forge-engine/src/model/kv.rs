// ===== File: model/kv.rs — strony KV, prefiks wspoldzielony, cykl zycia sekwencji =====
use super::*;

impl Model {
    /// Okno przesuwne uwagi dla warstwy `layer`; 0 = pełna uwaga przyczynowa.
    ///
    /// Architektury z naprzemienną geometrią (Gemma 4) mają okno tylko na
    /// części warstw; wzorzec jest już rozwinięty na wszystkie warstwy przez
    /// parser metadanych, więc tu wystarczy odczyt.
    pub(crate) fn attn_window(&self, layer: usize) -> usize {
        match &self.weights.descriptor.params.alt_attn {
            Some(alt) if alt.sliding.get(layer).copied().unwrap_or(false) => alt.window,
            _ => 0,
        }
    }

    pub(crate) fn target_kv_layer(&self, global_layer: usize) -> usize {
        self.kv
            .layer_index(global_layer)
            .expect("cache KV jest dostępny wyłącznie dla warstwy attention")
    }

    pub fn new_seq(&self) -> SeqKv {
        self.kv.new_seq()
    }

    pub fn ensure_kv_reuse_healthy(&self) -> Result<()> {
        self.kv_reuse_poison.ensure_healthy()
    }

    pub fn kv_reuse_poison_reason(&self) -> Option<&str> {
        self.kv_reuse_poison.reason()
    }

    pub(crate) fn synchronize_kv_fatal(&mut self, context: &str) -> Result<()> {
        let device = self.device.clone();
        fatal_kv_synchronize(&mut self.kv_reuse_poison, context, || device.synchronize())
    }

    pub fn release_seq(&mut self, seq: &mut SeqKv) {
        if self.kv_reuse_poison.is_poisoned() {
            let reason = self
                .kv_reuse_poison
                .reason()
                .expect("poison ma zapisany powód");
            tracing::error!(
                seq_id = seq.id,
                pages = seq.pages.len(),
                "sekwencja KV pozostaje w kwarantannie po fatalnym błędzie: {reason}"
            );
            return;
        }
        if let Some(lease) = seq.hybrid_state {
            let release = self
                .hybrid_states
                .as_mut()
                .expect("lease hybrydowy wymaga puli")
                .release(lease, &self.stream);
            match release {
                Ok(()) => seq.hybrid_state = None,
                Err(error) => {
                    tracing::error!("nie można bezpiecznie zwolnić stanu hybrydowego: {error}");
                }
            }
            // Zwolnienie MUSI objąć każdą rangę. Pule rang są identyczne i
            // wybierają slot tą samą kolejnością, więc pominięcie zwolnienia u
            // jednej z nich rozjeżdża listy wolnych slotów — druga sekwencja
            // dostaje wtedy inny slot na randze zerowej niż na pozostałych.
            for index in 0..self.tp_rank_count() {
                let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
                let stream = rank.stream.clone();
                if let Some(pool) = rank.hybrid_states.as_mut() {
                    if let Err(error) = pool.release(lease, &stream) {
                        tracing::error!(
                            "nie można zwolnić stanu hybrydowego rangi {}: {error}",
                            index + 1
                        );
                    }
                }
            }
        }
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        if self.prefix_cache.is_some() {
            self.finalize_prefix(seq);
        }
        self.kv.release(seq);
    }

    pub fn tier_enabled(&self) -> bool {
        self.tier.is_some()
    }

    /// Czy sekwencja ma zapamiętywać swoje tokeny.
    ///
    /// Tiering trzyma je pod recompute, prefiks pod darowiznę — a darowizna
    /// obejmuje też strony zapisane przez dekodowanie, więc token wygenerowany
    /// liczy się tak samo jak token promptu.
    pub(crate) fn records_tokens(&self) -> bool {
        self.tier.is_some() || self.prefix_cache.is_some()
    }

    pub(crate) fn record_token(&self, seq: &mut SeqKv, token: u32) {
        if self.records_tokens() {
            seq.tokens.push(token);
        }
    }

    /// Whether the radix prefix cache is active for this model.
    pub fn prefix_enabled(&self) -> bool {
        self.prefix_cache.is_some()
    }

    /// Longest cached-prefix length (tokens) servable for `prompt`, leaving at
    /// least one token to prefill (so the sequence still produces logits). Used
    /// by admission to project the reduced page demand; no state change.
    pub fn prefix_match_len(&self, prompt: &[u32]) -> usize {
        match &self.prefix_cache {
            Some(pc) if prompt.len() > self.kv.cfg.page_size => {
                pc.match_len(prompt, prompt.len() - 1, self.is_hybrid())
            }
            _ => 0,
        }
    }

    /// Borrow the longest cached prefix of `prompt` into `seq` (SPEC §5.2):
    /// shared pages are attached read-only, `seq.len`/`tokens`/`prefilled_len`
    /// advance to the shared boundary, and the divergent suffix is left to
    /// prefill. Returns the number of prompt tokens served from cache
    /// (`cache_read_tokens`). At least one token is always left to prefill.
    pub fn acquire_prefix(&mut self, seq: &mut SeqKv, prompt: &[u32]) -> usize {
        let ps = self.kv.cfg.page_size;
        let hybrid = self.is_hybrid();
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        if prompt.len() <= ps {
            return 0;
        }
        // Recurrent state only ever stands for a whole number of pages, so the
        // position this sequence will checkpoint at is fixed here, before a
        // single token is prefilled — and it holds whether or not the borrow
        // below finds anything.
        if hybrid {
            seq.state_target = prompt.len() / ps * ps;
        }
        let borrow = pc.acquire(prompt, prompt.len() - 1, hybrid);
        let shared = borrow.tokens;
        if shared == 0 {
            return 0;
        }
        seq.pages = borrow.pages;
        seq.shared_pages = seq.pages.len();
        seq.prefix_node = borrow.node;
        seq.state_restore = borrow.state;
        seq.len = shared;
        // Keep `tokens` page-aligned with `pages` so the completion-time
        // donation indexes shared + private pages uniformly. The borrowed pages
        // ARE the pages a prefill wrote, so the reused KV is bit-identical and
        // counts toward `prefilled_len`. The final logits are not: the divergent
        // tail is prefilled at a different token count than a cold run would
        // use, and the GEMM tile depends on that count. Greedy output can
        // therefore flip on near-tied logits depending on cache state — that is
        // why `forge bench` refuses to measure with the cache enabled.
        seq.tokens = prompt[..shared].to_vec();
        seq.prefilled_len = shared;
        // The single-stream decode path re-uploads the page table when a
        // different sequence's pages were resident; a borrow rewrites the table.
        self.pt_seq = 0;
        shared
    }

    /// Donate a completing sequence's freshly-prefilled complete pages back into
    /// the radix tree and release its borrow. Leading shared/donated pages are
    /// drained from `seq.pages` so the subsequent `kv.release` frees only the
    /// sequence's remaining private (partial + decode) pages.
    fn finalize_prefix(&mut self, seq: &mut SeqKv) {
        let ps = self.kv.cfg.page_size;
        let hybrid = self.is_hybrid();
        let mut states = std::mem::take(&mut seq.state_checkpoints);
        states.extend(seq.state_rolling.take());
        // A hybrid prefix is worth exactly as much as its checkpoint: pages past
        // it describe tokens no borrow can resume from, and pages donated
        // without one could never be borrowed at all. Both are simply not
        // offered, so the tree never holds KV that nothing can reach.
        // Prefiks hybrydy sięga dokładnie tak daleko, jak jej ostatni checkpoint:
        // strony za nim opisują tokeny, których wkładu rekurencyjnego nikt nie
        // zapisał, a strony bez checkpointu byłyby dla drzewa martwe.
        let last_state = states.iter().map(|&(pos, _)| pos).max().unwrap_or(0);
        let n_full = match hybrid {
            true => (last_state / ps).max(seq.shared_pages),
            // Gęsta ścieżka oddaje też strony zapisane przez dekodowanie:
            // odpowiedź poprzedniej tury jest prefiksem następnego promptu, a
            // bez niej każda tura czatu przelicza od nowa wszystko, co model
            // sam przed chwilą powiedział.
            false => seq.len / ps,
        };
        let node = seq.prefix_node.take();
        let donation = {
            let pc = self.prefix_cache.as_mut().expect("prefix path");
            let from = node.unwrap_or(crate::prefix::ROOT);
            let shared = node.map_or(0, |_| seq.shared_pages);
            let donation = (n_full > 0)
                .then(|| pc.donate(from, shared, n_full, &seq.tokens, &seq.pages, &states))
                .unwrap_or_else(|| crate::prefix::Donation {
                    dup_pages: Vec::new(),
                    consumed: 0,
                    dup_states: states.iter().map(|&(_, slot)| slot).collect(),
                });
            if let Some(node) = node {
                pc.release(node);
            }
            donation
        };
        for p in donation.dup_pages {
            self.kv.push_free(p);
        }
        self.return_state_slots(donation.dup_states);
        seq.pages.drain(0..donation.consumed.min(seq.pages.len()));
        seq.shared_pages = 0;
    }

    /// Reclaim up to `need` KV pages from the prefix cache (evicting refcount-0
    /// LRU prefixes) onto the free stack. No-op when the cache is inactive or
    /// already empty of evictable pages. Returns the number of pages freed.
    fn reclaim_prefix_pages(&mut self, need: usize) -> usize {
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        let freed = pc.evict(need);
        let n = freed.pages.len();
        for p in freed.pages {
            self.kv.push_free(p);
        }
        self.return_state_slots(freed.states);
        n
    }

    /// Ensure at least `need` free KV pages, evicting cached prefixes if the
    /// free stack is short. Called before prefill/decode growth so a cache hit
    /// never starves the pool.
    pub(crate) fn ensure_free_pages(&mut self, need: usize) {
        if self.prefix_cache.is_none() {
            return;
        }
        let free = self.kv.free_page_count();
        if free < need {
            self.reclaim_prefix_pages(need - free);
        }
    }

    /// Pages the engine can still hand out for a new request: the free stack
    /// plus everything the prefix cache can evict. Admission uses this so a
    /// reclaimable cache never blocks otherwise-fittable work.
    pub fn available_pages(&self) -> usize {
        self.kv.free_page_count()
            + self
                .prefix_cache
                .as_ref()
                .map(|pc| pc.evictable_pages())
                .unwrap_or(0)
    }

    /// Largest per-request KV demand (in pages) the engine can hold: the VRAM
    /// pool when tiering is off, the full context window when tiers extend it.
    pub fn max_request_pages(&self) -> usize {
        if self.tier.is_some() {
            self.max_pages_per_seq
        } else {
            self.kv.cfg.n_pages.min(self.max_pages_per_seq)
        }
    }

    /// Whether `seq`'s spilled pages can be restored without dropping the pool
    /// below the watermark reserve — restoring tighter than that would only
    /// thrash (the next step's capacity check would spill the pages again).
    pub(crate) fn tier_can_restore(&self, seq: &SeqKv) -> bool {
        let Some(tier) = &self.tier else { return false };
        seq.spilled_page_count() + tier.reserve_pages(self.kv.cfg.n_pages)
            <= self.kv.free_page_count()
    }

    /// Cross-sequence eviction (SPEC §5.4B): spill the globally coldest pages
    /// — across every provided sequence — until the pool can absorb
    /// `upcoming_pages` of growth plus the watermark reserve. Sequences with
    /// the largest spillable cold prefix donate first, so one long-context
    /// request no longer stalls behind neighbors' cold history. No-op with
    /// tiering off.
    pub fn tier_balance(&mut self, seqs: &mut [&mut SeqKv], upcoming_pages: usize) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let need = upcoming_pages + tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need {
            return Ok(());
        }
        let mut deficit = need - free;
        while deficit > 0 {
            let Some((idx, spillable)) = seqs
                .iter()
                .enumerate()
                .map(|(i, s)| (i, tier.spillable_pages(s)))
                .filter(|&(_, sp)| sp > 0)
                .max_by_key(|&(_, sp)| sp)
            else {
                break;
            };
            let take = deficit.min(spillable);
            let done = tier.spill(&mut self.kv, &mut *seqs[idx], take, &self.stream)?;
            if done == 0 {
                break;
            }
            self.pt_seq = 0;
            deficit = deficit.saturating_sub(done);
        }
        Ok(())
    }

    /// Spill this sequence's coldest pages until the pool can absorb
    /// `new_tokens` more tokens plus the watermark reserve. No-op with
    /// tiering off (the pool then errors on exhaustion, as before).
    pub(crate) fn tier_ensure_capacity(
        &mut self,
        seq: &mut SeqKv,
        new_tokens: usize,
    ) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let ps = self.kv.cfg.page_size;
        let need = (seq.len + new_tokens)
            .div_ceil(ps)
            .saturating_sub(seq.pages.len());
        let reserve = tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need + reserve {
            return Ok(());
        }
        let deficit = need + reserve - free;
        let spilled = tier.spill(&mut self.kv, seq, deficit, &self.stream)?;
        if spilled > 0 {
            self.pt_seq = 0;
        }
        Ok(())
    }

    /// Transfer-vs-recompute rule (SPEC §5.4): restore spilled chunks when the
    /// estimated transfer time beats re-prefilling the history. Recompute is
    /// only bit-identical for a purely prefilled history (decode writes its
    /// K/V through different kernels), so decode-extended sequences always
    /// transfer. Every decision is logged with the measured estimates.
    pub(crate) fn tier_restore_or_recompute(&mut self, seq: &mut SeqKv) -> Result<()> {
        let tier = self.tier.as_ref().expect("caller checked tiering");
        let (bytes, t_transfer) = tier.restore_cost(seq);
        let recompute_ok = seq.prefilled_len == seq.tokens.len() && !seq.tokens.is_empty();
        let t_recompute = tier.recompute_cost(seq.len);
        let use_recompute = recompute_ok && t_recompute < t_transfer;
        tracing::info!(
            "kv tier decision: seq {} transfer {:.1} MiB ≈ {:.1} ms vs recompute {} tok ≈ {:.1} ms → {}{}",
            seq.id,
            bytes as f64 / (1 << 20) as f64,
            t_transfer * 1e3,
            seq.len,
            t_recompute * 1e3,
            if use_recompute { "recompute" } else { "transfer" },
            if recompute_ok {
                ""
            } else {
                " (recompute ineligible: decode-written KV)"
            },
        );
        if use_recompute {
            self.recompute_seq(seq)
        } else {
            let tier = self.tier.as_mut().expect("checked above");
            tier.restore_all(&mut self.kv, seq, &self.stream)?;
            self.pt_seq = 0;
            Ok(())
        }
    }

    /// Rebuild `seq`'s KV from its retained tokens by re-prefilling from
    /// scratch, dropping all tier chunks first (recompute preemption).
    pub(crate) fn recompute_seq(&mut self, seq: &mut SeqKv) -> Result<()> {
        let toks = std::mem::take(&mut seq.tokens);
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        self.kv.release(seq);
        self.pt_seq = 0;
        if let (Some(pool), Some(lease)) = (&mut self.hybrid_states, seq.hybrid_state) {
            pool.reset(lease, &self.stream)?;
        }
        for chunk in toks.chunks(MAX_PREFILL_CHUNK) {
            if self.is_hybrid() {
                self.prefill_hybrid(seq, chunk)?;
            } else {
                self.prefill_forward(seq, chunk, true)?;
            }
        }
        Ok(())
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through every
    /// transformer block in one batched pass, appending K/V to `seq`. Leaves
    /// the final-norm hidden states for the chunk's `t` tokens in
    /// `prefill_bufs.x` as a `[t, hidden]` row-major f16 matrix and returns
    /// `t`. `wait_for_completion` opróżnia stream przed zwróceniem dla wywołań,
    /// które odczytują `x` na hoście. Operacje device-only mogą kontynuować na
    /// tym samym streamie bez pośredniej synchronizacji.
    /// Dzielniki częstotliwości rope dla warstwy `l` — tylko warstwy globalne
    /// architektur z naprzemienną uwagą (Gemma 4) i tylko gdy model niesie
    /// tensor `rope_freqs`.
    pub(crate) fn rope_freqs_at(
        &self,
        p: &forge_formats::Hyperparams,
        l: usize,
    ) -> Option<&DevBuffer> {
        if p.rope_proportional_at(l) {
            self.weights.rope_freqs.as_ref()
        } else {
            None
        }
    }

    /// Stage [token, pos, seq_len] in pinned memory and push them with async
    /// copies on the compute stream — pinned H2D avoids the pageable
    /// legacy-stream drain that plain write() must perform.
    /// Bierze kolejny slot pierscienia i CZEKA, az jego poprzednia kopia
    /// dotarla na urzadzenie. Bez tego host nadpisuje przypiete bajty, ktore
    /// czeka jeszcze zakolejkowana kopia.
    pub(crate) fn claim_staging_slot(&self) -> Result<usize> {
        let slot = self.staging_cursor.get() % STAGING_SLOTS;
        self.staging_events[slot].synchronize()?;
        self.staging_cursor.set(self.staging_cursor.get() + 1);
        Ok(slot)
    }

    /// Upload `seq`'s page table (pinned staging + async H2D) and mark it as
    /// the one resident in `page_table_dev`.
    pub(crate) fn upload_page_table(&mut self, seq: &SeqKv) -> Result<()> {
        let slot = self.claim_staging_slot()?;
        let pt_offset = slot * self.max_pages_per_seq * 4;
        let pt_host = self
            .bufs
            .pinned_pt
            .host_ptr()
            .expect("pinned buffer has host mapping");
        let mut pt = vec![-1i32; self.max_pages_per_seq];
        pt[..seq.pages.len()].copy_from_slice(&seq.pages);
        unsafe {
            std::ptr::copy_nonoverlapping(
                pt.as_ptr() as *const u8,
                pt_host.add(pt_offset),
                self.max_pages_per_seq * 4,
            );
        }
        self.device.copy(
            &self.bufs.pinned_pt,
            pt_offset,
            &self.page_table_dev,
            0,
            self.max_pages_per_seq * 4,
            &self.stream,
        )?;
        self.device
            .record_event(&self.staging_events[slot], &self.stream)?;
        self.pt_seq = seq.id;
        // Strony przydziela WYŁĄCZNIE ranga zerowa, a pozostałe indeksują nimi
        // swoje własne slaby. Dzięki temu listy wolnych stron nie mogą się
        // rozjechać: poza rangą zerową nikt niczego nie przydziela.
        for index in 0..self.tp_rank_count() {
            let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
            rank.upload_page_table(seq)?;
        }
        Ok(())
    }

    /// Smallest captured bucket >= `n`: a power of two, capped at `batch_cap`.
    /// A live batch replays the smallest bucket that holds it (dead lanes pad
    /// up to the bucket and are never sampled).
    pub(crate) fn bucket_for(&self, n: usize) -> usize {
        let mut s = 1;
        while s < n {
            s *= 2;
        }
        s.min(self.batch_cap).max(1)
    }
}
