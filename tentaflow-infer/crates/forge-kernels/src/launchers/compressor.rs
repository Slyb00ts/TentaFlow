// ===== File: compressor.rs — launchery kompresora kontekstu =====
use super::*;

impl Kernels {
    /// `acc += src` w f32 — kodowanie pozycji kompresora jest w checkpoincie
    /// zapisane w tej precyzji.
    pub fn compressor_add_ape_f32(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("compressor_add_ape_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Redukcja kopii strumienia w głowie wyjściowej: sama sigmoida, bez
    /// Sinkhorna, z POJEDYNCZĄ skalą dla wszystkich kopii.
    #[allow(clippy::too_many_arguments)]
    pub fn hc_head_reduce_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        mix_fn: &DevBuffer,
        base: &DevBuffer,
        scale: &DevBuffer,
        dim: usize,
        hc: usize,
        n_tokens: usize,
        eps: f32,
        hc_eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if hc > 16 {
            return Err(ForgeError::Kernel(format!(
                "hc_head_reduce: {hc} kopii przekracza limit kernela 16"
            )));
        }
        let k = self.artifacts.get("hc_head_reduce_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(mix_fn)
            .buf(base)
            .buf(scale)
            .scalar(dim as i64)
            .scalar(hc as i64)
            .scalar(n_tokens as i64)
            .scalar(eps)
            .scalar(hc_eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Punktowanie pozycji przez indekser rzadkiej uwagi.
    #[allow(clippy::too_many_arguments)]
    pub fn index_score_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        kv: &DevBuffer,
        head_w: &DevBuffer,
        head_dim: usize,
        n_heads: usize,
        n_blocks: usize,
        n_tokens: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("index_score_f16")?;
        let total = n_tokens * n_blocks;
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(kv)
            .buf(head_w)
            .scalar(head_dim as i64)
            .scalar(n_heads as i64)
            .scalar(n_blocks as i64)
            .scalar(n_tokens as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wagi hyper-connections: redukcja, rozprowadzenie i macierz mieszająca po
    /// Sinkhornie. Jeden wątek na token — macierz ma `hc * hc` elementów.
    #[allow(clippy::too_many_arguments)]
    pub fn hc_sinkhorn_f32(
        &self,
        pre: &DevBuffer,
        post: &DevBuffer,
        comb: &DevBuffer,
        mixes: &DevBuffer,
        scale: &DevBuffer,
        base: &DevBuffer,
        hc: usize,
        iters: usize,
        eps: f32,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if hc == 0 || iters == 0 {
            return Err(ForgeError::Kernel(format!(
                "hc_sinkhorn: hc={hc}, iters={iters} muszą być dodatnie"
            )));
        }
        let k = self.artifacts.get("hc_sinkhorn_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(pre)
            .buf(post)
            .buf(comb)
            .buf(mixes)
            .buf(scale)
            .buf(base)
            .scalar(hc as i64)
            .scalar(iters as i64)
            .scalar(eps)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Redukcja `hc` kopii strumienia rezydualnego do jednej.
    #[allow(clippy::too_many_arguments)]
    pub fn hc_reduce_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        pre: &DevBuffer,
        dim: usize,
        hc: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("hc_reduce_f16")?;
        let total = n_tokens * dim;
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(pre)
            .scalar(dim as i64)
            .scalar(hc as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rozprowadzenie wyjścia bloku z powrotem na `hc` kopii.
    #[allow(clippy::too_many_arguments)]
    pub fn hc_expand_f16(
        &self,
        out: &DevBuffer,
        block_out: &DevBuffer,
        residual: &DevBuffer,
        post: &DevBuffer,
        comb: &DevBuffer,
        dim: usize,
        hc: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("hc_expand_f16")?;
        let total = n_tokens * hc * dim;
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(block_out)
            .buf(residual)
            .buf(post)
            .buf(comb)
            .scalar(dim as i64)
            .scalar(hc as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Bramkowany pooling kompresora KV. `slots` mapuje pozycję okna na wiersz
    /// źródłowy (`-1` = pozycja pusta), więc logika okien z zakładką siedzi w
    /// tablicy, a nie w kernelu.
    #[allow(clippy::too_many_arguments)]
    pub fn compressor_pool_f16(
        &self,
        out: &DevBuffer,
        kv_f32: &DevBuffer,
        score: &DevBuffer,
        slots: &DevBuffer,
        head_dim: usize,
        window: usize,
        n_blocks: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("compressor_pool_f16")?;
        let total = n_blocks * head_dim;
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(kv_f32)
            .buf(score)
            .buf(slots)
            .scalar(head_dim as i64)
            .scalar(window as i64)
            .scalar(n_blocks as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}
