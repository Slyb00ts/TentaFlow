// =============================================================================
// Plik: mtp.rs
// Opis: Typowane wagi MTP oraz niezależny stan KV i hidden dla draftu NextN.
// Przykład: MtpWeights::load(descriptor, params, loader, embedding, output)
// =============================================================================

use std::sync::Arc;

use forge_formats::{Hyperparams, MtpDescriptor, MtpWeightRole};
use forge_hal::{DevBuffer, Device, Pool, Stream, Event};
use forge_kernels::Kernels;
use forge_types::{ForgeError, MemKind, Result};

use crate::kv::{KvCache, KvConfig, SeqKv};
use crate::weights::{
    AttnWeights, DenseFfn, DevWeight, GateUpWeights, LayerFfn, LayerMixer, LayerWeights, QkvWeights,
};

/// Adapter źródła tensorów używany przez loader MTP niezależnie od GGUF i safetensors.
pub trait MtpTensorLoader {
    fn matrix(&mut self, name: &str, rows: usize, cols: usize) -> Result<DevWeight>;
    fn vector(&mut self, name: &str, len: usize) -> Result<DevBuffer>;
}

pub struct MtpLayerWeights {
    pub eh_proj: DevWeight,
    pub enorm: DevBuffer,
    pub hnorm: DevBuffer,
    pub shared_head_norm: DevBuffer,
    pub block: LayerWeights,
}

pub enum MtpEmbedding {
    Device(DevWeight),
    HostF16,
}

impl MtpEmbedding {
    pub fn mode(&self) -> &'static str {
        match self {
            MtpEmbedding::Device(_) => "device",
            MtpEmbedding::HostF16 => "host",
        }
    }
}

pub struct MtpWeights {
    pub descriptor: MtpDescriptor,
    pub shares_target_embedding: bool,
    /// Jeden staged wiersz embeddingu używany jako wejście kernela MTP.
    pub token_embedding: DevBuffer,
    pub embedding: MtpEmbedding,
    /// Waga współdzieląca alokacje LM headu targetu.
    pub output: DevWeight,
    pub layers: Vec<MtpLayerWeights>,
}

impl MtpWeights {
    pub fn runtime_supported(&self) -> bool {
        let [layer] = self.layers.as_slice() else {
            return false;
        };
        let LayerMixer::Attention(attention) = &layer.block.mixer else {
            return false;
        };
        let LayerFfn::Dense(ffn) = &layer.block.ffn else {
            return false;
        };
        matches!(&layer.eh_proj, DevWeight::Q8_0 { .. })
            && matches!(&attention.attn_qkv, QkvWeights::Split { .. })
            && matches!(&ffn.gate_up, GateUpWeights::Split { .. })
            && matches!(
                &self.embedding,
                MtpEmbedding::HostF16
                    | MtpEmbedding::Device(DevWeight::F16 { .. })
                    | MtpEmbedding::Device(DevWeight::Q8_0 { .. })
                    | MtpEmbedding::Device(DevWeight::NvFp4Gguf { .. })
            )
            && !matches!(
                &self.output,
                DevWeight::NvFp4 { .. } | DevWeight::NvFp4Gguf { .. }
            )
    }

    pub fn load(
        descriptor: &MtpDescriptor,
        params: &Hyperparams,
        loader: &mut dyn MtpTensorLoader,
        target_embedding: &DevBuffer,
        embedding: MtpEmbedding,
        target_output: &DevWeight,
    ) -> Result<Self> {
        if descriptor.block_count == 0 || descriptor.layers.len() != descriptor.block_count {
            return Err(ForgeError::Format(format!(
                "MTP: block_count={} nie odpowiada liczbie map warstw {}",
                descriptor.block_count,
                descriptor.layers.len()
            )));
        }
        let embedding_bytes = params
            .hidden_size
            .checked_mul(2)
            .ok_or_else(|| ForgeError::Format("MTP: przepełnienie rozmiaru embeddingu".into()))?;
        if target_embedding.len() < embedding_bytes {
            return Err(ForgeError::Format(format!(
                "MTP: staged embedding targetu ma {} bajtów, wymagano {embedding_bytes}",
                target_embedding.len()
            )));
        }
        if target_output.rows() != params.vocab_size || target_output.cols() != params.hidden_size {
            return Err(ForgeError::Format(format!(
                "MTP: LM head targetu ma kształt [{}, {}], wymagano [{}, {}]",
                target_output.rows(),
                target_output.cols(),
                params.vocab_size,
                params.hidden_size
            )));
        }
        let first_layer = descriptor.layers.first().ok_or_else(|| {
            ForgeError::Format("MTP: deskryptor nie zawiera mapy pierwszej warstwy".into())
        })?;
        let shares_target_embedding = !first_layer.contains_key(&MtpWeightRole::Embedding)
            && descriptor.share_target_embedding;
        let embedding = match first_layer.get(&MtpWeightRole::Embedding) {
            Some(name) => MtpEmbedding::Device(loader.matrix(
                name,
                params.vocab_size,
                params.hidden_size,
            )?),
            None if descriptor.share_target_embedding => embedding,
            None => {
                return Err(ForgeError::Format(
                    "MTP: brak dedykowanego embeddingu i jawnego fallbacku targetu".into(),
                ))
            }
        };
        let output = match first_layer.get(&MtpWeightRole::SharedHead) {
            Some(name) => loader.matrix(name, params.vocab_size, params.hidden_size)?,
            None if descriptor.share_target_output => share_weight(target_output),
            None => {
                return Err(ForgeError::Format(
                    "MTP: brak dedykowanego shared headu i jawnego fallbacku targetu".into(),
                ))
            }
        };
        if output.rows() != params.vocab_size || output.cols() != params.hidden_size {
            return Err(ForgeError::Format(format!(
                "MTP: shared head ma kształt [{}, {}], wymagano [{}, {}]",
                output.rows(),
                output.cols(),
                params.vocab_size,
                params.hidden_size
            )));
        }
        if let MtpEmbedding::Device(weight) = &embedding {
            if weight.rows() != params.vocab_size || weight.cols() != params.hidden_size {
                return Err(ForgeError::Format(format!(
                    "MTP: embedding targetu ma kształt [{}, {}], wymagano [{}, {}]",
                    weight.rows(),
                    weight.cols(),
                    params.vocab_size,
                    params.hidden_size
                )));
            }
        }

        let hidden = params.hidden_size;
        let q_dim = params.n_heads * params.head_dim;
        let q_rows = if params.attn_gated { 2 * q_dim } else { q_dim };
        let kv_dim = params.n_kv_heads * params.head_dim;
        let inter = params.intermediate_size;
        let mut layers = Vec::with_capacity(descriptor.block_count);
        for (index, names) in descriptor.layers.iter().enumerate() {
            let name = |role| {
                names.get(&role).map(String::as_str).ok_or_else(|| {
                    ForgeError::Format(format!("MTP: warstwa {index} nie ma roli {role:?}"))
                })
            };
            let q_norm = names
                .get(&MtpWeightRole::AttnQNorm)
                .map(|tensor| loader.vector(tensor, params.head_dim))
                .transpose()?;
            let k_norm = names
                .get(&MtpWeightRole::AttnKNorm)
                .map(|tensor| loader.vector(tensor, params.head_dim))
                .transpose()?;
            let block = LayerWeights {
                attn_norm: loader.vector(name(MtpWeightRole::AttnNorm)?, hidden)?,
                ffn_norm: loader.vector(name(MtpWeightRole::FfnNorm)?, hidden)?,
                mixer: LayerMixer::Attention(Box::new(AttnWeights {
                    q_norm,
                    k_norm,
                    attn_qkv: QkvWeights::Split {
                        q: loader.matrix(name(MtpWeightRole::AttnQ)?, q_rows, hidden)?,
                        k: loader.matrix(name(MtpWeightRole::AttnK)?, kv_dim, hidden)?,
                        v: loader.matrix(name(MtpWeightRole::AttnV)?, kv_dim, hidden)?,
                    },
                    attn_o: loader.matrix(name(MtpWeightRole::AttnO)?, hidden, q_dim)?,
                })),
                ffn: LayerFfn::Dense(DenseFfn {
                    gate_up: GateUpWeights::Split {
                        gate: loader.matrix(name(MtpWeightRole::FfnGate)?, inter, hidden)?,
                        up: loader.matrix(name(MtpWeightRole::FfnUp)?, inter, hidden)?,
                    },
                    down: loader.matrix(name(MtpWeightRole::FfnDown)?, hidden, inter)?,
                }),
            };
            layers.push(MtpLayerWeights {
                eh_proj: loader.matrix(name(MtpWeightRole::EhProj)?, hidden, 2 * hidden)?,
                enorm: loader.vector(name(MtpWeightRole::ENorm)?, hidden)?,
                hnorm: loader.vector(name(MtpWeightRole::HNorm)?, hidden)?,
                shared_head_norm: loader.vector(name(MtpWeightRole::SharedHeadNorm)?, hidden)?,
                block,
            });
        }

        Ok(Self {
            descriptor: descriptor.clone(),
            shares_target_embedding,
            token_embedding: target_embedding.clone(),
            embedding,
            output,
            layers,
        })
    }
}

fn share_weight(weight: &DevWeight) -> DevWeight {
    match weight {
        DevWeight::F16 { buf, rows, cols } => DevWeight::F16 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q8_0 { buf, rows, cols } => DevWeight::Q8_0 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q4K { buf, rows, cols } => DevWeight::Q4K {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q6K { buf, rows, cols } => DevWeight::Q6K {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q5K { buf, rows, cols } => DevWeight::Q5K {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q3K { buf, rows, cols } => DevWeight::Q3K {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q2K { buf, rows, cols } => DevWeight::Q2K {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q4_0 { buf, rows, cols } => DevWeight::Q4_0 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q4_1 { buf, rows, cols } => DevWeight::Q4_1 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q5_0 { buf, rows, cols } => DevWeight::Q5_0 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Q5_1 { buf, rows, cols } => DevWeight::Q5_1 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq4Nl { buf, rows, cols } => DevWeight::Iq4Nl {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq4Xs { buf, rows, cols } => DevWeight::Iq4Xs {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Mxfp4 { buf, rows, cols } => DevWeight::Mxfp4 {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq2Xs { buf, rows, cols } => DevWeight::Iq2Xs {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq2S { buf, rows, cols } => DevWeight::Iq2S {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq3S { buf, rows, cols } => DevWeight::Iq3S {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq2Xxs { buf, rows, cols } => DevWeight::Iq2Xxs {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq3Xxs { buf, rows, cols } => DevWeight::Iq3Xxs {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq1S { buf, rows, cols } => DevWeight::Iq1S {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::Iq1M { buf, rows, cols } => DevWeight::Iq1M {
            buf: buf.clone(),
            rows: *rows,
            cols: *cols,
        },
        DevWeight::NvFp4 {
            packed,
            scales,
            inv_global_scale,
            rows,
            cols,
        } => DevWeight::NvFp4 {
            packed: packed.clone(),
            scales: scales.clone(),
            inv_global_scale: *inv_global_scale,
            rows: *rows,
            cols: *cols,
        },
        DevWeight::NvFp4Gguf {
            buf,
            output_scale,
            rows,
            cols,
        } => DevWeight::NvFp4Gguf {
            buf: buf.clone(),
            output_scale: *output_scale,
            rows: *rows,
            cols: *cols,
        },
    }
}

/// Właściciel odrębnego KV MTP i hidden przenoszonego między krokami draftu.
pub struct MtpDraftState {
    device: Arc<dyn Device>,
    pub kv: KvCache,
    pub seq: SeqKv,
    pub recurrent_hidden: DevBuffer,
    pub catchup_hidden: DevBuffer,
    pub prepared_hidden: DevBuffer,
    pub logits: DevBuffer,
    pub page_table: DevBuffer,
    pub seq_len: DevBuffer,
    pub position: DevBuffer,
    pub token_ids: DevBuffer,
    pub pinned_token_ids: DevBuffer,
    pub pinned_scalar: DevBuffer,
    pub pinned_scalar_ready: Event,
    pub pinned_scalar_recorded: bool,
    checkpoint_hidden: DevBuffer,
    step_hidden: DevBuffer,
    checkpoint_len: Option<usize>,
    host_embedding_gathers: u64,
}

fn new_page_mapping(position: usize, page_size: usize, pages: &[i32]) -> Option<(usize, i32)> {
    position.is_multiple_of(page_size).then(|| {
        (
            pages.len() - 1,
            *pages.last().expect("grow dodał stronę na granicy"),
        )
    })
}

impl MtpDraftState {
    pub fn new(
        device: Arc<dyn Device>,
        kv_config: KvConfig,
        hidden_size: usize,
        vocab_size: usize,
    ) -> Result<Self> {
        if kv_config.n_layers != 1 {
            return Err(ForgeError::Format(format!(
                "MTP wymaga dokładnie jednej warstwy KV, otrzymano {}",
                kv_config.n_layers
            )));
        }
        let hidden_bytes = hidden_size
            .checked_mul(2)
            .ok_or_else(|| ForgeError::Format("MTP: przepełnienie rozmiaru hidden".into()))?;
        if hidden_bytes == 0 {
            return Err(ForgeError::Format(
                "MTP: hidden_size musi być dodatni".into(),
            ));
        }
        let logits_bytes = vocab_size
            .checked_mul(4)
            .ok_or_else(|| ForgeError::Format("MTP: przepełnienie rozmiaru logitów".into()))?;
        if logits_bytes == 0 {
            return Err(ForgeError::Format(
                "MTP: vocab_size musi być dodatni".into(),
            ));
        }
        let max_pages_per_seq = kv_config.max_pages_per_seq;
        let kv = KvCache::new(device.as_ref(), kv_config)?;
        let seq = kv.new_seq();
        let recurrent_hidden = device.alloc(hidden_bytes, MemKind::Device, Pool::Activations)?;
        let page_table = device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Activations)?;
        let seq_len = device.alloc(4, MemKind::Device, Pool::Activations)?;
        let position = device.alloc(4, MemKind::Device, Pool::Activations)?;
        device.write(&vec![0u8; hidden_bytes], &recurrent_hidden, 0)?;
        device.write(&vec![0xff; max_pages_per_seq * 4], &page_table, 0)?;
        device.write(&[0u8; 4], &seq_len, 0)?;
        device.write(&[0u8; 4], &position, 0)?;
        Ok(Self {
            recurrent_hidden,
            catchup_hidden: device.alloc(hidden_bytes, MemKind::Device, Pool::Activations)?,
            prepared_hidden: device.alloc(hidden_bytes, MemKind::Device, Pool::Activations)?,
            logits: device.alloc(logits_bytes, MemKind::Device, Pool::Activations)?,
            page_table,
            seq_len,
            position,
            token_ids: device.alloc(5 * 4, MemKind::Device, Pool::Activations)?,
            pinned_token_ids: device.alloc(5 * 4, MemKind::PinnedHost, Pool::Activations)?,
            pinned_scalar: device.alloc(4, MemKind::PinnedHost, Pool::Activations)?,
            pinned_scalar_ready: device.create_event()?,
            pinned_scalar_recorded: false,
            checkpoint_hidden: device.alloc(hidden_bytes, MemKind::Device, Pool::Activations)?,
            step_hidden: device.alloc(4 * hidden_bytes, MemKind::Device, Pool::Activations)?,
            device,
            kv,
            seq,
            checkpoint_len: None,
            host_embedding_gathers: 0,
        })
    }

    pub fn grow(&mut self) -> Result<()> {
        self.kv.grow(&mut self.seq)
    }

    pub fn checkpoint(&mut self, stream: &Stream) -> Result<()> {
        if self.checkpoint_len.is_some() {
            return Err(ForgeError::Scheduler(
                "MTP: poprzedni checkpoint nie został rozstrzygnięty".into(),
            ));
        }
        self.device.copy(
            &self.recurrent_hidden,
            0,
            &self.checkpoint_hidden,
            0,
            self.recurrent_hidden.len(),
            stream,
        )?;
        self.checkpoint_len = Some(self.seq.len);
        Ok(())
    }

    /// Zeruje carry MTP przed rozpoczęciem nowej sekwencji targetu.
    pub fn reset_hidden(&mut self) -> Result<()> {
        self.device.write(
            &vec![0u8; self.recurrent_hidden.len()],
            &self.recurrent_hidden,
            0,
        )?;
        self.device
            .write(&vec![0xff; self.page_table.len()], &self.page_table, 0)?;
        self.device.write(&[0u8; 4], &self.seq_len, 0)?;
        self.device.write(&[0u8; 4], &self.position, 0)
    }

    pub fn stage_step(&mut self, kernels: &Kernels, stream: &Stream) -> Result<usize> {
        let position = self.seq.len;
        self.grow()?;
        let mapping = new_page_mapping(position, self.kv.cfg.page_size, &self.seq.pages);
        kernels.mtp_stage_step(
            &self.position,
            &self.seq_len,
            &self.page_table,
            position,
            self.seq.len,
            mapping.map(|value| value.0),
            mapping.map(|value| value.1),
            stream,
        )?;
        Ok(position)
    }

    /// Rezerwuje ciąg pozycji MTP i aktualizuje jego tabelę stron jednym zapisem.
    pub fn stage_batch(&mut self, n_tokens: usize) -> Result<(usize, Vec<i32>, i32, i32)> {
        if n_tokens == 0 {
            return Err(ForgeError::Scheduler("MTP: pusty batch catch-up".into()));
        }
        let base = self.seq.len;
        for _ in 0..n_tokens {
            self.grow()?;
        }
        let mut page_table = vec![-1i32; self.kv.cfg.max_pages_per_seq];
        page_table[..self.seq.pages.len()].copy_from_slice(&self.seq.pages);
        Ok((
            base,
            page_table,
            self.seq.len as i32,
            (self.seq.len - 1) as i32,
        ))
    }

    pub fn rollback(&mut self, stream: &Stream) -> Result<()> {
        let len = self.checkpoint_len.ok_or_else(|| {
            ForgeError::Scheduler("MTP: rollback bez aktywnego checkpointu".into())
        })?;
        self.device.copy(
            &self.checkpoint_hidden,
            0,
            &self.recurrent_hidden,
            0,
            self.recurrent_hidden.len(),
            stream,
        )?;
        self.kv.rollback(&mut self.seq, len);
        self.checkpoint_len = None;
        Ok(())
    }

    pub fn save_step_hidden(&self, step: usize, stream: &Stream) -> Result<()> {
        if step >= 4 {
            return Err(ForgeError::Scheduler(format!(
                "MTP: indeks checkpointu hidden {step} wykracza poza pojemność"
            )));
        }
        self.device.copy(
            &self.recurrent_hidden,
            0,
            &self.step_hidden,
            step * self.recurrent_hidden.len(),
            self.recurrent_hidden.len(),
            stream,
        )
    }

    pub fn commit_prefix(&mut self, retained: usize, stream: &Stream) -> Result<()> {
        let base = self.checkpoint_len.ok_or_else(|| {
            ForgeError::Scheduler("MTP: commit bez aktywnego checkpointu".into())
        })?;
        if retained == 0 || retained > 4 || base + retained > self.seq.len {
            return Err(ForgeError::Scheduler(format!(
                "MTP: niepoprawna długość zatwierdzenia {retained} dla zakresu {}..{}",
                base,
                self.seq.len
            )));
        }
        self.device.copy(
            &self.step_hidden,
            (retained - 1) * self.recurrent_hidden.len(),
            &self.recurrent_hidden,
            0,
            self.recurrent_hidden.len(),
            stream,
        )?;
        self.kv.rollback(&mut self.seq, base + retained);
        self.checkpoint_len = None;
        Ok(())
    }

    /// Zatwierdza sekwencyjny catch-up wykonany od aktywnego checkpointu.
    pub fn commit_catchup(&mut self, retained: usize) -> Result<()> {
        let base = self.checkpoint_len.ok_or_else(|| {
            ForgeError::Scheduler("MTP: commit catch-up bez aktywnego checkpointu".into())
        })?;
        if retained == 0 || base + retained != self.seq.len {
            return Err(ForgeError::Scheduler(format!(
                "MTP: niepoprawna długość catch-up {retained} dla zakresu {base}..{}",
                self.seq.len
            )));
        }
        self.checkpoint_len = None;
        Ok(())
    }

    pub fn checkpoint_len(&self) -> Option<usize> {
        self.checkpoint_len
    }

    pub fn record_host_embedding_gather(&mut self) {
        self.host_embedding_gathers += 1;
    }

    pub fn host_embedding_gathers(&self) -> u64 {
        self.host_embedding_gathers
    }

    pub fn release(&mut self) {
        self.kv.release(&mut self.seq);
        self.checkpoint_len = None;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use forge_formats::{MoeParams, PoolingType};
    use forge_hal::cpu::CpuDevice;

    #[test]
    fn mapowanie_stron_powstaje_tylko_na_granicy() {
        assert_eq!(new_page_mapping(0, 2, &[7]), Some((0, 7)));
        assert_eq!(new_page_mapping(1, 2, &[7]), None);
        assert_eq!(new_page_mapping(2, 2, &[7, 11]), Some((1, 11)));
        assert_eq!(new_page_mapping(3, 2, &[7, 11]), None);
        assert_eq!(new_page_mapping(0, 4, &[5]), Some((0, 5)));
        assert_eq!(new_page_mapping(3, 4, &[5]), None);
        assert_eq!(new_page_mapping(4, 4, &[5, 9]), Some((1, 9)));
    }

    struct SyntheticLoader {
        device: Arc<dyn Device>,
        loaded: Vec<(String, usize, usize)>,
    }

    impl MtpTensorLoader for SyntheticLoader {
        fn matrix(&mut self, name: &str, rows: usize, cols: usize) -> Result<DevWeight> {
            self.loaded.push((name.to_string(), rows, cols));
            if name.contains("EhProj") {
                return Ok(DevWeight::Q8_0 {
                    buf: self.device.alloc(
                        rows * (cols / 32) * 34,
                        MemKind::Device,
                        Pool::Weights,
                    )?,
                    rows,
                    cols,
                });
            }
            Ok(DevWeight::F16 {
                buf: self
                    .device
                    .alloc(rows * cols * 2, MemKind::Device, Pool::Weights)?,
                rows,
                cols,
            })
        }

        fn vector(&mut self, name: &str, len: usize) -> Result<DevBuffer> {
            self.loaded.push((name.to_string(), 1, len));
            self.device.alloc(len * 2, MemKind::Device, Pool::Weights)
        }
    }

    fn params() -> Hyperparams {
        Hyperparams {
            block_count: 2,
            hidden_size: 128,
            n_heads: 4,
            n_kv_heads: 2,
            head_dim: 32,
            intermediate_size: 256,
            vocab_size: 64,
            rope_theta: 10_000.0,
            rms_norm_eps: 1e-6,
            max_position_embeddings: 1024,
            tie_word_embeddings: true,
            pooling_type: PoolingType::None,
            moe: None::<MoeParams>,
            qk_norm_over_hidden: false,
            ssm: None,
            rope_sections: None,
            full_attention_interval: 0,
            attn_gated: true,
        }
    }

    fn descriptor() -> MtpDescriptor {
        let mut layer = HashMap::new();
        for role in [
            MtpWeightRole::AttnK,
            MtpWeightRole::AttnKNorm,
            MtpWeightRole::AttnNorm,
            MtpWeightRole::AttnO,
            MtpWeightRole::AttnQ,
            MtpWeightRole::AttnQNorm,
            MtpWeightRole::AttnV,
            MtpWeightRole::FfnDown,
            MtpWeightRole::FfnGate,
            MtpWeightRole::FfnUp,
            MtpWeightRole::FfnNorm,
            MtpWeightRole::EhProj,
            MtpWeightRole::ENorm,
            MtpWeightRole::HNorm,
            MtpWeightRole::SharedHeadNorm,
        ] {
            layer.insert(role, format!("mtp.{role:?}"));
        }
        MtpDescriptor {
            first_block: 2,
            block_count: 1,
            layers: vec![layer],
            share_target_embedding: true,
            share_target_output: true,
        }
    }

    #[test]
    fn loader_wspoldzieli_target_i_waliduje_ksztalty() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let params = params();
        let embedding = device
            .alloc(
                params.vocab_size * params.hidden_size * 2,
                MemKind::Device,
                Pool::Weights,
            )
            .unwrap();
        let output_buffer = device
            .alloc(
                params.vocab_size * params.hidden_size * 2,
                MemKind::Device,
                Pool::Weights,
            )
            .unwrap();
        let output = DevWeight::F16 {
            buf: output_buffer.clone(),
            rows: params.vocab_size,
            cols: params.hidden_size,
        };
        let mut loader = SyntheticLoader {
            device,
            loaded: Vec::new(),
        };

        let weights = MtpWeights::load(
            &descriptor(),
            &params,
            &mut loader,
            &embedding,
            MtpEmbedding::Device(share_weight(&output)),
            &output,
        )
        .expect("załaduj syntetyczne MTP");
        assert_eq!(weights.layers.len(), 1);
        assert!(weights.shares_target_embedding);
        assert!(weights.runtime_supported());
        assert_eq!(weights.token_embedding.device_ptr(), embedding.device_ptr());
        let DevWeight::F16 { buf, .. } = &weights.output else {
            panic!("oczekiwano współdzielonego F16");
        };
        assert_eq!(buf.device_ptr(), output_buffer.device_ptr());
        assert!(loader.loaded.contains(&(
            "mtp.EhProj".into(),
            params.hidden_size,
            2 * params.hidden_size
        )));
        assert!(loader.loaded.contains(&(
            "mtp.AttnQ".into(),
            2 * params.n_heads * params.head_dim,
            params.hidden_size
        )));
    }

    #[test]
    fn loader_preferuje_dedykowany_embedding_i_shared_head() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let params = params();
        let embedding = device
            .alloc(params.hidden_size * 2, MemKind::Device, Pool::Weights)
            .unwrap();
        let output_buffer = device
            .alloc(
                params.vocab_size * params.hidden_size * 2,
                MemKind::Device,
                Pool::Weights,
            )
            .unwrap();
        let output = DevWeight::F16 {
            buf: output_buffer.clone(),
            rows: params.vocab_size,
            cols: params.hidden_size,
        };
        let mut descriptor = descriptor();
        descriptor.layers[0].insert(MtpWeightRole::Embedding, "mtp.embedding".into());
        descriptor.layers[0].insert(MtpWeightRole::SharedHead, "mtp.head".into());
        let mut loader = SyntheticLoader {
            device,
            loaded: Vec::new(),
        };

        let weights = MtpWeights::load(
            &descriptor,
            &params,
            &mut loader,
            &embedding,
            MtpEmbedding::HostF16,
            &output,
        )
        .expect("załaduj dedykowane MTP IO");
        assert!(!weights.shares_target_embedding);
        let MtpEmbedding::Device(DevWeight::F16 {
            buf: embedding_buf, ..
        }) = &weights.embedding
        else {
            panic!("oczekiwano dedykowanego embeddingu F16");
        };
        let DevWeight::F16 { buf: head_buf, .. } = &weights.output else {
            panic!("oczekiwano dedykowanego shared headu F16");
        };
        assert_ne!(embedding_buf.device_ptr(), embedding.device_ptr());
        assert_ne!(head_buf.device_ptr(), output_buffer.device_ptr());
        assert!(loader.loaded.contains(&(
            "mtp.embedding".into(),
            params.vocab_size,
            params.hidden_size
        )));
        assert!(loader.loaded.contains(&(
            "mtp.head".into(),
            params.vocab_size,
            params.hidden_size
        )));
    }

    #[test]
    fn loader_odrzuca_niejawny_fallback_targetu() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let params = params();
        let embedding = device
            .alloc(params.hidden_size * 2, MemKind::Device, Pool::Weights)
            .unwrap();
        let output = DevWeight::F16 {
            buf: device
                .alloc(
                    params.vocab_size * params.hidden_size * 2,
                    MemKind::Device,
                    Pool::Weights,
                )
                .unwrap(),
            rows: params.vocab_size,
            cols: params.hidden_size,
        };
        let mut descriptor = descriptor();
        descriptor.share_target_embedding = false;
        descriptor.share_target_output = false;
        let mut loader = SyntheticLoader {
            device,
            loaded: Vec::new(),
        };

        let Err(error) = MtpWeights::load(
            &descriptor,
            &params,
            &mut loader,
            &embedding,
            MtpEmbedding::HostF16,
            &output,
        ) else {
            panic!("oczekiwano odrzucenia niejawnego współdzielenia targetu");
        };
        assert!(error.to_string().contains("jawnego fallbacku"));
    }

    #[test]
    fn osobny_kv_przywraca_checkpoint_hidden_i_strony() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().unwrap();
        let config = KvConfig {
            n_layers: 1,
            n_kv_heads: 2,
            head_dim: 32,
            page_size: 2,
            n_pages: 4,
            max_pages_per_seq: 4,
            quant: crate::kv::KvQuant::F16,
        };
        let mut state = MtpDraftState::new(device.clone(), config, 4, 8).unwrap();
        for _ in 0..3 {
            state.grow().unwrap();
        }
        device
            .write(&[1, 2, 3, 4, 5, 6, 7, 8], &state.recurrent_hidden, 0)
            .unwrap();
        state.checkpoint(&stream).unwrap();
        for _ in 0..3 {
            state.grow().unwrap();
        }
        device.write(&[9; 8], &state.recurrent_hidden, 0).unwrap();
        assert_eq!(state.seq.len, 6);
        assert_eq!(state.kv.free_page_count(), 1);

        state.rollback(&stream).unwrap();
        assert_eq!(state.seq.len, 3);
        assert_eq!(state.seq.pages.len(), 2);
        assert_eq!(state.kv.free_page_count(), 2);
        assert_eq!(state.checkpoint_len(), None);
        let mut hidden = [0u8; 8];
        device
            .read(&state.recurrent_hidden, 0, &mut hidden)
            .unwrap();
        assert_eq!(hidden, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn commit_zachowuje_prefiks_dla_accepted_od_zera_do_trzech() {
        for budget in [2, 3] {
            for accepted in 0..=budget {
                let device: Arc<dyn Device> = CpuDevice::new();
                let stream = device.create_stream().unwrap();
                let config = KvConfig {
                    n_layers: 1,
                    n_kv_heads: 1,
                    head_dim: 32,
                    page_size: 2,
                    n_pages: 4,
                    max_pages_per_seq: 4,
                    quant: crate::kv::KvQuant::F16,
                };
                let mut state = MtpDraftState::new(device.clone(), config, 4, 8).unwrap();
                state.checkpoint(&stream).unwrap();
                for step in 0..=budget {
                    state.grow().unwrap();
                    device
                        .write(&[step as u8 + 1; 8], &state.recurrent_hidden, 0)
                        .unwrap();
                    state.save_step_hidden(step, &stream).unwrap();
                }

                state.commit_prefix(accepted + 1, &stream).unwrap();
                assert_eq!(state.seq.len, accepted + 1);
                assert_eq!(state.checkpoint_len(), None);
                let mut hidden = [0u8; 8];
                device
                    .read(&state.recurrent_hidden, 0, &mut hidden)
                    .unwrap();
                assert_eq!(hidden, [accepted as u8 + 1; 8]);

                state.release();
                assert_eq!(state.seq.len, 0);
                assert_eq!(state.kv.free_page_count(), 4);
            }
        }
    }

    #[test]
    fn commit_catchup_zachowuje_caly_dogoniony_prefiks() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().unwrap();
        let config = KvConfig {
            n_layers: 1,
            n_kv_heads: 1,
            head_dim: 32,
            page_size: 2,
            n_pages: 4,
            max_pages_per_seq: 4,
            quant: crate::kv::KvQuant::F16,
        };
        let mut state = MtpDraftState::new(device, config, 4, 8).unwrap();
        state.grow().unwrap();
        state.checkpoint(&stream).unwrap();
        state.grow().unwrap();
        state.grow().unwrap();

        assert!(state.commit_catchup(1).is_err());
        assert_eq!(state.checkpoint_len(), Some(1));
        state.commit_catchup(2).unwrap();
        assert_eq!(state.seq.len, 3);
        assert_eq!(state.checkpoint_len(), None);
    }

    #[test]
    fn stage_batch_obsluguje_kolejne_ogona_i_granice_stron() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().unwrap();
        let config = KvConfig {
            n_layers: 1,
            n_kv_heads: 1,
            head_dim: 32,
            page_size: 4,
            n_pages: 8,
            max_pages_per_seq: 8,
            quant: crate::kv::KvQuant::F16,
        };
        let mut state = MtpDraftState::new(device.clone(), config, 4, 8).unwrap();

        state.checkpoint(&stream).unwrap();
        let (base, _, seq_len, position) = state.stage_batch(3).unwrap();
        assert_eq!((base, seq_len, position), (0, 3, 2));
        state.commit_catchup(3).unwrap();
        state.checkpoint(&stream).unwrap();
        let (base, page_table, seq_len, position) = state.stage_batch(2).unwrap();
        assert_eq!((base, seq_len, position), (3, 5, 4));
        state.commit_catchup(2).unwrap();

        assert_eq!(&page_table[..state.seq.pages.len()], &state.seq.pages);
        assert_eq!(state.seq.pages.len(), 2);
        assert!(state.stage_batch(0).is_err());
    }

    #[test]
    fn nieudany_rollback_zachowuje_checkpoint_i_kv() {
        let device: Arc<dyn Device> = CpuDevice::new();
        let stream = device.create_stream().unwrap();
        let config = KvConfig {
            n_layers: 1,
            n_kv_heads: 1,
            head_dim: 32,
            page_size: 2,
            n_pages: 4,
            max_pages_per_seq: 4,
            quant: crate::kv::KvQuant::F16,
        };
        let mut state = MtpDraftState::new(device.clone(), config, 4, 8).unwrap();
        state.grow().unwrap();
        state.checkpoint(&stream).unwrap();
        state.grow().unwrap();
        let len = state.seq.len;
        let pages = state.seq.pages.clone();
        state.checkpoint_hidden = device
            .alloc(1, MemKind::Device, Pool::Activations)
            .unwrap();

        assert!(state.rollback(&stream).is_err());
        assert_eq!(state.seq.len, len);
        assert_eq!(state.seq.pages, pages);
        assert_eq!(state.checkpoint_len(), Some(1));
    }
}
