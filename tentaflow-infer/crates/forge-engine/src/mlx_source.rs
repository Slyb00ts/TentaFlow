// ===== File: mlx_source.rs — MLX checkpoints as a source for the existing engine =====
//
// Nothing here is a new inference path. The engine takes weights through one
// interface, `TensorSource`, and this is one implementation of it: MLX affine
// 4-bit repacked into GGML Q4_1, which the CUDA and HIP kernels already
// compute. Everything downstream — scheduler, KV cache, speculation, tiering —
// is untouched and unaware the checkpoint came from MLX.

use forge_formats::safetensors::ShardedSafeTensors;
use forge_formats::{repack_affine_to_q4_1, MlxAffineTensor, MlxParams, MlxQuantConfig};
use forge_types::{DType, ForgeError, QuantKind, Result};
use half::{bf16, f16};

use crate::weights::{Fp8Host, NvFp4Host, TensorFetch, TensorSource};

/// Źródło tensorów dla checkpointów MLX.
///
/// Cała obsługa modeli MLX na CUDA i HIP sprowadza się do tego jednego typu.
/// Silnik ma już wszystko: dwadzieścia trzy kernele Q4_1, harmonogram, cache
/// KV, spekulację — i wczytuje wagi wyłącznie przez `TensorSource`. MLX affine
/// 4-bit liczy dokładnie to samo co Q4_1 (`q * skala + przesunięcie` na grupę
/// kolejnych wag), więc brakowało nie kernela, tylko przełożenia bitów.
///
/// Przepakowanie jest po stronie hosta, przy wczytywaniu, i jest sprawdzone co
/// do bitu wobec ścieżki MLX (`forge-formats`, `repack_affine_to_q4_1`).
pub(crate) struct MlxSource<'a> {
    st: &'a ShardedSafeTensors,
    cfg: MlxQuantConfig,
}

impl<'a> MlxSource<'a> {
    /// `None`, gdy katalog nie jest checkpointem MLX affine 4-bit — czyli gdy
    /// wagi ma wziąć zwykłe źródło safetensors.
    pub(crate) fn detect(config_text: &str, st: &'a ShardedSafeTensors) -> Option<Self> {
        let cfg = serde_json::from_str::<serde_json::Value>(config_text)
            .ok()
            .and_then(|v| MlxQuantConfig::from_config(&v).ok().flatten())
            .filter(|c| c.mode == forge_formats::MlxMode::Affine && c.bits == 4)?;
        Some(Self { st, cfg })
    }

    #[cfg(test)]
    fn new(st: &'a ShardedSafeTensors, cfg: MlxQuantConfig) -> Self {
        Self { st, cfg }
    }

    fn sibling(name: &str, suffix: &str) -> Option<String> {
        name.strip_suffix(".weight").map(|b| format!("{b}{suffix}"))
    }

    /// Bajty tensora jako `u16`, element po elemencie. `cast_slice` tu nie
    /// przejdzie: dane są odwzorowane z pliku i nie muszą być wyrównane.
    fn u16s(&self, name: &str) -> Result<Vec<u16>> {
        let raw = self.st.data(name)?;
        Ok(raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect())
    }

    fn u32s(&self, name: &str) -> Result<Vec<u32>> {
        let raw = self.st.data(name)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    /// `None`, gdy tensor nie jest kwantyzowaną trójką.
    fn fetch_quantized(&self, name: &str) -> Result<Option<TensorFetch>> {
        let (Some(scales_name), Some(biases_name)) = (
            Self::sibling(name, ".scales"),
            Self::sibling(name, ".biases"),
        ) else {
            return Ok(None);
        };
        let (Some(packed_t), Some(scales_t)) =
            (self.st.tensor(name), self.st.tensor(&scales_name))
        else {
            return Ok(None);
        };
        if self.st.tensor(&biases_name).is_none() {
            // Trójka niepełna to nie jest tensor niekwantyzowany, tylko zepsuty
            // checkpoint — cicha zamiana na zwykły odczyt dałaby śmieci.
            return Err(ForgeError::Format(format!(
                "MLX: {name} ma skale, ale nie ma przesunięć"
            )));
        }
        if packed_t.shape.len() != 2 {
            return Err(ForgeError::Format(format!(
                "MLX: {name} ma kształt {:?}, oczekiwano macierzy",
                packed_t.shape
            )));
        }
        let rows = packed_t.shape[0];
        let cols = packed_t.shape[1] * self.cfg.per_word();

        // Typ skal jest własnością KONWERTERA, nie formatu: mlx-lm zapisuje
        // bf16, mlx-whisper f16. Czytany jest więc z pliku, a nie zakładany —
        // pomyłka tutaj daje wynik bez podobieństwa do właściwego.
        let param_dtype = scales_t.dtype;
        let packed = self.u32s(name)?;
        let scales = self.u16s(&scales_name)?;
        let biases = self.u16s(&biases_name)?;
        let (sc_bf, bi_bf, sc_f, bi_f);
        let (scales, biases) = if param_dtype == DType::BF16 {
            sc_bf = scales.iter().map(|b| bf16::from_bits(*b)).collect::<Vec<_>>();
            bi_bf = biases.iter().map(|b| bf16::from_bits(*b)).collect::<Vec<_>>();
            (MlxParams::Bf16(&sc_bf), MlxParams::Bf16(&bi_bf))
        } else {
            sc_f = scales.iter().map(|b| f16::from_bits(*b)).collect::<Vec<_>>();
            bi_f = biases.iter().map(|b| f16::from_bits(*b)).collect::<Vec<_>>();
            (MlxParams::F16(&sc_f), MlxParams::F16(&bi_f))
        };
        let tensor = MlxAffineTensor {
            packed: &packed,
            scales,
            biases,
            rows,
            cols,
        };
        let blocks = repack_affine_to_q4_1(&tensor, &self.cfg)?;
        Ok(Some((blocks, DType::F16, QuantKind::Q4_1, vec![rows, cols])))
    }
}

impl TensorSource for MlxSource<'_> {
    fn byte_len(&self, name: &str) -> Option<usize> {
        let info = self.st.tensor(name)?;
        Some(info.numel() * info.dtype.size())
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch> {
        if let Some(found) = self.fetch_quantized(name)? {
            return Ok(found);
        }
        let t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("missing tensor {name}")))?;
        let data = self.st.data(name)?.to_vec();
        Ok((data, t.dtype, QuantKind::None, t.shape.clone()))
    }

    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>> {
        if self.st.tensor(name).is_none() {
            return Ok(None);
        }
        self.fetch(name).map(Some)
    }

    fn fetch_nvfp4(&self, _name: &str) -> Result<Option<NvFp4Host>> {
        Ok(None)
    }

    fn fetch_fp8(&self, _name: &str) -> Result<Option<Fp8Host>> {
        Ok(None)
    }
}


#[cfg(test)]
mod mlx_source_tests {
    use super::*;
    use forge_formats::dequantize_to_f32;
    use std::path::PathBuf;

    fn checkpoint() -> Option<PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)?
            .parent()?
            .join(".runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots");
        let snap = std::fs::read_dir(&dir).ok()?.flatten().next()?.path();
        snap.join("config.json").is_file().then_some(snap)
    }

    /// Tensor wzięty przez źródło MLX musi dać te same liczby, co ścieżka MLX.
    ///
    /// To jest bramka pod uruchamianie modeli MLX na CUDA i HIP. Silnik nie
    /// dostaje tu żadnego nowego kernela — dostaje wagi w Q4_1, który już umie
    /// liczyć — więc jedyne, co może pójść źle, to przełożenie bitów. Sprawdzane
    /// jest ono, a nie „czy się wczytało".
    #[test]
    fn mlx_source_hands_the_engine_the_same_numbers_mlx_would() {
        let Some(dir) = checkpoint() else {
            eprintln!("pomijam: brak checkpointu MLX");
            return;
        };
        let text = std::fs::read_to_string(dir.join("config.json")).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cfg = MlxQuantConfig::from_config(&raw)
            .unwrap()
            .expect("checkpoint deklaruje kwantyzację");
        let st = ShardedSafeTensors::load_dir(&dir).unwrap();
        let src = MlxSource::new(&st, cfg);

        // Dwa kształty: wąska projekcja uwagi i szeroka FFN — to one decydują
        // o liczbie grup na wiersz, czyli o tym, gdzie przełożenie może się
        // rozjechać.
        for name in [
            "model.layers.0.self_attn.q_proj.weight",
            "model.layers.0.mlp.down_proj.weight",
        ] {
            let (bytes, dtype, quant, dims) = src.fetch(name).unwrap();
            assert_eq!(quant, QuantKind::Q4_1, "{name}: inny format");
            assert_eq!(dtype, DType::F16);
            let (rows, cols) = (dims[0], dims[1]);

            let got = dequantize_to_f32(dtype, quant, &bytes, rows * cols).unwrap();

            // Wyrocznia: ta sama trójka, przeczytana ścieżką MLX.
            let packed: Vec<u32> = st
                .data(name)
                .unwrap()
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let base = name.strip_suffix(".weight").unwrap();
            let read16 = |n: String| -> Vec<u16> {
                st.data(&n)
                    .unwrap()
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect()
            };
            let sc = read16(format!("{base}.scales"));
            let bi = read16(format!("{base}.biases"));
            let sc: Vec<bf16> = sc.iter().map(|b| bf16::from_bits(*b)).collect();
            let bi: Vec<bf16> = bi.iter().map(|b| bf16::from_bits(*b)).collect();
            let tensor = MlxAffineTensor {
                packed: &packed,
                scales: MlxParams::Bf16(&sc),
                biases: MlxParams::Bf16(&bi),
                rows,
                cols,
            };
            let mut want = vec![0f32; rows * cols];
            forge_formats::dequantize_affine(&tensor, &cfg, &mut want).unwrap();

            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_eq!(
                    g.to_bits(),
                    w.to_bits(),
                    "{name}: element {i} — źródło MLX zmieniło wartość ({g} wobec {w})"
                );
            }
            eprintln!("{name}: [{rows} x {cols}] zgodne co do bitu");
        }
    }

    /// Tensor niekwantyzowany (norma) ma przejść nietknięty.
    #[test]
    fn plain_tensors_pass_through_unchanged() {
        let Some(dir) = checkpoint() else {
            eprintln!("pomijam: brak checkpointu MLX");
            return;
        };
        let text = std::fs::read_to_string(dir.join("config.json")).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cfg = MlxQuantConfig::from_config(&raw).unwrap().unwrap();
        let st = ShardedSafeTensors::load_dir(&dir).unwrap();
        let src = MlxSource::new(&st, cfg);

        let name = "model.layers.0.input_layernorm.weight";
        let (bytes, _, quant, dims) = src.fetch(name).unwrap();
        assert_eq!(quant, QuantKind::None, "norma nie jest kwantyzowana");
        assert_eq!(bytes, st.data(name).unwrap(), "bajty się zmieniły");
        assert_eq!(dims, st.tensor(name).unwrap().shape);
    }
}

