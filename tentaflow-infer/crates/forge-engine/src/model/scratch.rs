// ===== File: model/scratch.rs — bufory robocze i etapowanie wejsc =====
use super::*;

impl Model {
    /// Powyżej tylu wierszy projekcji opłaca się policzyć normę RAZ.
    ///
    /// Kernel scalony przelicza ją w KAŻDYM bloku, więc jego narzut rośnie z
    /// wysokością projekcji; policzenie normy osobno kosztuje jedno
    /// uruchomienie i zapis znormalizowanego wektora, niezależnie od
    /// wysokości. Zmierzone na GB10, Bielik-7B, tg32: projekcja QKV o 6144
    /// wierszach jest szybsza scalona (47,2 wobec 46,6 tok/s), a gate/up o
    /// 22528 wierszach szybsza z normą osobno (47,2 wobec 44,1). Prefill i tak
    /// zawsze liczy normę raz, więc to zbliża dekodowanie do niego, a nie
    /// oddala.
    const NORM_ONCE_ROWS: usize = 12288;

    pub(crate) fn norm_once_pays(rows: usize) -> bool {
        rows >= Self::NORM_ONCE_ROWS
    }

    /// Jedna projekcja dekodowania z normą wejścia, liczoną tam, gdzie taniej.
    pub(crate) fn decode_project_normed(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        norm_w: &DevBuffer,
        from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if Self::norm_once_pays(w.rows()) {
            self.decode_norm(norm_w, from_h16, eps, stream)?;
            return self.gemv(y, w, &self.bufs.x, stream);
        }
        self.gemv_norm(y, w, norm_w, from_h16, eps, stream)
    }

    /// Scalona para gate|up dekodowania wraz z bramką SwiGLU.
    pub(crate) fn decode_gate_up_fused(
        &self,
        w: &DevWeight,
        norm_w: &DevBuffer,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let inter = self.weights.descriptor.params.intermediate_size;
        if !Self::norm_once_pays(w.rows()) {
            return self.gemv_norm_silu(&b.act, w, norm_w, eps, stream);
        }
        self.decode_norm(norm_w, false, eps, stream)?;
        self.gemv_rows(&b.gate, w, &b.x, 0, inter, stream)?;
        self.gemv_rows(&b.up, w, &b.x, inter, inter, stream)?;
        self.kernels
            .glu_mul_f16(self.ffn_act(), &b.act, &b.gate, &b.up, inter, stream)
    }

    /// Znormalizowany strumień rezydualny dekodowania w `b.x`.
    ///
    /// `from_h16` mówi, że f32-owe lustro rezyduału jeszcze nie istnieje — tak
    /// jest wyłącznie przed pierwszą warstwą.
    pub(crate) fn decode_norm(
        &self,
        norm_w: &DevBuffer,
        from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let hidden = self.weights.descriptor.params.hidden_size;
        if from_h16 {
            self.kernels
                .rmsnorm_f16(&b.x, &b.h, norm_w, 1, hidden, eps, stream)
        } else {
            self.kernels
                .rmsnorm_h32_f16(&b.x, &b.h, &b.h32, norm_w, 1, hidden, eps, stream)
        }
    }

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
