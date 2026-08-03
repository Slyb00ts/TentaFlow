// ===== File: model/scratch.rs — bufory robocze i etapowanie wejsc =====
use super::*;

impl Model {
    pub(crate) fn ensure_prefill_bufs(&mut self) -> Result<()> {
        for index in 0..self.tp_rank_count() {
            let rank = &mut self.tp.as_mut().expect("podział sprawdzony").ranks[index];
            rank.ensure_prefill_bufs()?;
        }
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        // Bufory muszą pomieścić najszerszą warstwę modelu — przy
        // naprzemiennej geometrii warstwy różnią się szerokością projekcji.
        let q_dim = p.max_q_dim();
        let kv_dim = p.max_kv_dim();
        let inter = p.intermediate_size;
        let t_max = if self.is_hybrid() {
            self.hybrid_prefill_chunk_size.max(4)
        } else {
            MAX_PREFILL_CHUNK
        };
        if self
            .prefill_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= t_max)
        {
            return Ok(());
        }
        let alloc = |elems: usize| {
            self.device
                .alloc(elems * 2, MemKind::Device, Pool::Activations)
        };
        let gate = alloc(t_max * inter)?;
        let act = if self.is_hybrid() {
            gate.clone()
        } else {
            alloc(t_max * inter)?
        };
        self.prefill_bufs = Some(PrefillBufs {
            cap: t_max,
            h: alloc(t_max * hidden)?,
            x: alloc(t_max * hidden)?,
            q: alloc(t_max * q_dim)?,
            k: alloc(t_max * kv_dim)?,
            v: alloc(t_max * kv_dim)?,
            attn_out: alloc(t_max * q_dim)?,
            o_out: alloc(t_max * hidden)?,
            gate,
            up: alloc(t_max * inter)?,
            act,
            down: alloc(t_max * hidden)?,
            ids: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
            positions: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
        });
        Ok(())
    }

    /// Strumień rezydualny prefillu — granica etapu pipeline'u.
    ///
    /// Etap oddaje TO, a nie znormalizowane `x`: następny etap normalizuje po
    /// swojemu swoją warstwą zerową, więc między kartami wędruje wyłącznie
    /// rezydual. Bufor istnieje dopiero po pierwszym prefillu.
    pub fn stage_hidden(&self) -> Result<&DevBuffer> {
        self.prefill_bufs
            .as_ref()
            .map(|pb| &pb.h)
            .ok_or_else(|| ForgeError::Scheduler("bufory prefillu jeszcze nie istnieją".into()))
    }

    /// Przygotowuje bufory prefillu bez liczenia, żeby etap NIE pierwszy miał
    /// gdzie przyjąć stan z poprzedniej karty.
    pub fn ensure_stage_buffers(&mut self) -> Result<()> {
        self.ensure_prefill_bufs()
    }

    /// Zapewnia scratch logitów weryfikatora dla `cap` pozycji.
    pub(crate) fn ensure_verify_bufs(&mut self, cap: usize) -> Result<()> {
        if self.verify_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        self.verify_bufs = Some(VerifyBufs {
            cap,
            logits: self
                .device
                .alloc(cap * vocab * 4, MemKind::Device, Pool::Activations)?,
            ids: self
                .device
                .alloc(cap * 4, MemKind::Device, Pool::Activations)?,
            pinned_ids: self
                .device
                .alloc(cap * 4, MemKind::PinnedHost, Pool::Activations)?,
        });
        Ok(())
    }

    /// Copy `bytes` into a pinned staging buffer and enqueue the H2D to its
    /// device buffer on `stream`.
    pub(crate) fn stage(
        device: &Arc<dyn Device>,
        pinned: &DevBuffer,
        dev: &DevBuffer,
        bytes: &[u8],
        stream: &Stream,
    ) -> Result<()> {
        let host = pinned.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host, bytes.len());
        }
        device.copy(pinned, 0, dev, 0, bytes.len(), stream)
    }

}
