// ===== File: checkpoint.rs — one way in, whatever the checkpoint is =====
//
// A model on disk is a directory of safetensors with a `config.json`, or a
// single `.gguf` file, or an MLX export that looks like the first but packs its
// weights like neither. Which one it is decides how to FIND a tensor. It does
// not decide what the tensor means, what shape the model has, or which machine
// will multiply it.
//
// So that decision is made once, here, and everything above sees the same two
// things: a `ModelDescriptor` that answers "which tensor plays which role", and
// a `TensorSource` that answers "give me its bytes". A model built on top of
// those never learns which format it was loaded from — which is the point, and
// the reason a second loader per platform is a bug rather than a feature.

use std::path::Path;

use forge_types::{ForgeError, Result};

use crate::arch::ModelDescriptor;
use crate::gguf::Gguf;
use crate::mlx_source::MlxSource;
use crate::safetensors::ShardedSafeTensors;
use crate::source::{GgufSource, StSource, TensorSource};
use crate::HfConfig;

/// An opened checkpoint: the descriptor, plus whatever backing store the format
/// needs kept alive for its tensors to be readable.
pub struct Checkpoint {
    descriptor: ModelDescriptor,
    store: Store,
}

enum Store {
    Gguf(Box<Gguf>),
    SafeTensors {
        st: Box<ShardedSafeTensors>,
        /// Kept because `MlxSource` is built from it on every borrow — the
        /// quantization config lives in the same JSON as the architecture.
        config: String,
    },
}

impl Checkpoint {
    /// Opens a directory (safetensors plus `config.json`) or a single `.gguf`.
    pub fn open(path: &Path) -> Result<Self> {
        if path.is_file() {
            let gguf = Gguf::open(path)?;
            let descriptor = ModelDescriptor::detect(&gguf)?;
            return Ok(Self {
                descriptor,
                store: Store::Gguf(Box::new(gguf)),
            });
        }
        let config = std::fs::read_to_string(path.join("config.json"))
            .map_err(|e| ForgeError::Format(format!("config.json: {e}")))?;
        let hf: HfConfig = serde_json::from_str(&config)
            .map_err(|e| ForgeError::Format(format!("config.json: {e}")))?;
        let descriptor = ModelDescriptor::from_hf(&hf)?;
        let st = ShardedSafeTensors::load_dir(path)?;
        Ok(Self {
            descriptor,
            store: Store::SafeTensors {
                st: Box::new(st),
                config,
            },
        })
    }

    pub fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    /// The reader for this checkpoint's tensors.
    ///
    /// Borrowed rather than stored, because a source is a thin view over the
    /// backing store — and because which source applies is a property of the
    /// store, not a choice the caller should be able to get wrong.
    pub fn source(&self) -> Box<dyn TensorSource + '_> {
        match &self.store {
            Store::Gguf(g) => Box::new(GgufSource(g)),
            Store::SafeTensors { st, config } => match MlxSource::detect(config, st) {
                Some(mlx) => Box::new(mlx),
                None => Box::new(StSource {
                    st,
                    scheme: None,
                    fp8: false,
                    deepseek_v4: false,
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_directory_fails_on_the_config_rather_than_later() {
        // Brak `config.json` musi się wywalić TU, a nie przy pierwszym tensorze,
        // bo wtedy komunikat mówiłby o wadze zamiast o katalogu.
        let Err(err) = Checkpoint::open(Path::new("/nonexistent-checkpoint-dir")) else {
            panic!("nieistniejący katalog nie może się otworzyć");
        };
        assert!(
            format!("{err}").contains("config.json"),
            "komunikat nie wskazuje na config.json: {err}"
        );
    }
}
