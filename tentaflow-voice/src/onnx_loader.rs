// =============================================================================
// Plik: onnx_loader.rs
// Opis: Parser plikow .onnx — wyciaga surowe tensory (wagi) po nazwach.
//       Nie parsuje grafu operacji — tylko tensory z `model.graph.initializer`.
//       Forward pass robimy recznie per-model.
// =============================================================================

use crate::error::{VoiceError, VoiceResult};
use crate::generated::{tensor_proto::DataType, GraphProto, ModelProto};
use prost::Message;
use std::collections::HashMap;
use std::path::Path;

/// Tensor reprezentacja — shape + dane f32 (po dekonwertowaniu z raw ONNX)
#[derive(Debug, Clone)]
pub struct Tensor {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    /// Liczba elementow (iloczyn shape)
    pub fn num_elements(&self) -> usize {
        self.shape.iter().product()
    }

    /// Sprawdza czy shape pasuje do oczekiwanego
    pub fn expect_shape(&self, expected: &[usize]) -> VoiceResult<()> {
        if self.shape != expected {
            return Err(VoiceError::ShapeMismatch {
                name: self.name.clone(),
                expected: expected.to_vec(),
                actual: self.shape.clone(),
            });
        }
        Ok(())
    }
}

/// Kontener wag modelu ONNX — mapa nazwa → Tensor
pub struct OnnxWeights {
    tensors: HashMap<String, Tensor>,
}

impl OnnxWeights {
    /// Laduje model .onnx z pliku, wyciaga wszystkie tensory z `graph.initializer`.
    pub fn load<P: AsRef<Path>>(path: P) -> VoiceResult<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::load_from_bytes(&bytes)
    }

    /// Laduje z surowych bajtow protobuf
    pub fn load_from_bytes(bytes: &[u8]) -> VoiceResult<Self> {
        let model = ModelProto::decode(bytes)?;
        let graph = model
            .graph
            .ok_or_else(|| VoiceError::OnnxParse("brak graph w ModelProto".into()))?;

        let mut tensors = HashMap::new();
        extract_graph_tensors(&graph, &mut tensors, "")?;

        tracing::info!("ONNX model loaded: {} tensorow", tensors.len());
        Ok(Self { tensors })
    }

    /// Pobiera tensor po nazwie
    pub fn get(&self, name: &str) -> VoiceResult<&Tensor> {
        self.tensors
            .get(name)
            .ok_or_else(|| VoiceError::MissingTensor(name.to_string()))
    }

    /// Pobiera tensor po jednej z alternatywnych nazw (ONNX exporters zmieniaja
    /// convention, wiec warto probowac kilka wariantow)
    pub fn get_any(&self, names: &[&str]) -> VoiceResult<&Tensor> {
        for name in names {
            if let Some(t) = self.tensors.get(*name) {
                return Ok(t);
            }
        }
        Err(VoiceError::MissingTensor(format!(
            "zadna z nazw: {:?}",
            names
        )))
    }

    /// Lista wszystkich nazw tensorow (przydatne do debugowania)
    pub fn names(&self) -> Vec<&str> {
        self.tensors.keys().map(|s| s.as_str()).collect()
    }

    /// Liczba zaladowanych tensorow
    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }
}

/// Rekurencyjnie wyciaga tensory z grafu (i sub-grafow w If/Loop/Scan nodes).
/// `prefix` pozwala unikalnie nazwac tensory z sub-grafow (np. "if_then/conv1").
fn extract_graph_tensors(
    graph: &GraphProto,
    tensors: &mut HashMap<String, Tensor>,
    prefix: &str,
) -> VoiceResult<()> {
    // Initializers (standardowy format)
    for initializer in &graph.initializer {
        let name = if prefix.is_empty() {
            initializer.name.clone()
        } else {
            format!("{}/{}", prefix, initializer.name)
        };
        let shape: Vec<usize> = initializer.dims.iter().map(|&d| d as usize).collect();
        let data = decode_tensor_data(initializer)?;
        tensors.insert(
            name.clone(),
            Tensor { name, shape, data },
        );
    }

    // Constant nodes + rekurencyjna obsluga sub-grafow
    for node in &graph.node {
        // Constant node → tensor
        if node.op_type == "Constant" {
            if let Some(output_name) = node.output.first() {
                for attr in &node.attribute {
                    if attr.name == "value" {
                        if let Some(ref tp) = attr.t {
                            let full_name = if prefix.is_empty() {
                                output_name.clone()
                            } else {
                                format!("{}/{}", prefix, output_name)
                            };
                            let shape: Vec<usize> = tp.dims.iter().map(|&d| d as usize).collect();
                            let data = decode_tensor_data(tp)?;
                            tensors.insert(
                                full_name.clone(),
                                Tensor { name: full_name, shape, data },
                            );
                        }
                    }
                }
            }
        }

        // Sub-grafy: If ma 'then_branch' i 'else_branch', Loop ma 'body', Scan ma 'body'
        for attr in &node.attribute {
            if let Some(ref subgraph) = attr.g {
                let sub_prefix = if prefix.is_empty() {
                    format!("{}_{}", node.name, attr.name)
                } else {
                    format!("{}/{}_{}", prefix, node.name, attr.name)
                };
                extract_graph_tensors(subgraph, tensors, &sub_prefix)?;
            }
        }
    }

    Ok(())
}

/// Konwertuje dane tensora ONNX na Vec<f32>, obslugujac rozne reprezentacje:
/// - `float_data` (jeden z pól TensorProto) — bezposrednio f32
/// - `raw_data` — bajty little-endian w zaleznosci od `data_type`
fn decode_tensor_data(tensor: &crate::generated::TensorProto) -> VoiceResult<Vec<f32>> {
    let dtype = DataType::try_from(tensor.data_type).unwrap_or(DataType::Undefined);

    // Preferuj float_data jesli wypelnione (male modele)
    if !tensor.float_data.is_empty() {
        return Ok(tensor.float_data.clone());
    }

    // raw_data — bajty w formacie dtype
    let raw = &tensor.raw_data;
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    match dtype {
        DataType::Float => {
            // f32 LE
            if raw.len() % 4 != 0 {
                return Err(VoiceError::OnnxParse(format!(
                    "raw_data f32 ma niepoprawna dlugosc {}",
                    raw.len()
                )));
            }
            let mut out = Vec::with_capacity(raw.len() / 4);
            for chunk in raw.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            Ok(out)
        }
        DataType::Float16 => {
            // f16 LE → f32
            if raw.len() % 2 != 0 {
                return Err(VoiceError::OnnxParse("raw_data f16 ma nieparzysta dlugosc".into()));
            }
            let mut out = Vec::with_capacity(raw.len() / 2);
            for chunk in raw.chunks_exact(2) {
                let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
                out.push(f16_to_f32(bits));
            }
            Ok(out)
        }
        DataType::Int32 => {
            if raw.len() % 4 != 0 {
                return Err(VoiceError::OnnxParse("raw_data i32 niepoprawna".into()));
            }
            let mut out = Vec::with_capacity(raw.len() / 4);
            for chunk in raw.chunks_exact(4) {
                let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(v as f32);
            }
            Ok(out)
        }
        DataType::Int64 => {
            if raw.len() % 8 != 0 {
                return Err(VoiceError::OnnxParse("raw_data i64 niepoprawna".into()));
            }
            let mut out = Vec::with_capacity(raw.len() / 8);
            for chunk in raw.chunks_exact(8) {
                let v = i64::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ]);
                out.push(v as f32);
            }
            Ok(out)
        }
        _ => Err(VoiceError::UnsupportedDtype(format!(
            "{:?} (data_type={})",
            dtype, tensor.data_type
        ))),
    }
}

/// Konwersja IEEE 754 binary16 (half precision) na f32
#[inline]
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 0x1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;

    let f32_bits = if exp == 0 {
        if mantissa == 0 {
            sign << 31
        } else {
            // Denormalized: normalizuj
            let mut m = mantissa;
            let mut e: i32 = -14;
            while (m & 0x400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            (sign << 31) | (((e + 127) as u32) << 23) | (m << 13)
        }
    } else if exp == 0x1f {
        // Inf / NaN
        (sign << 31) | (0xff << 23) | (mantissa << 13)
    } else {
        (sign << 31) | (((exp - 15 + 127) as u32) << 23) | (mantissa << 13)
    };

    f32::from_bits(f32_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_to_f32_basic() {
        // 0x3C00 = 1.0 w f16
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        // 0xC000 = -2.0
        assert_eq!(f16_to_f32(0xC000), -2.0);
        // 0 = 0.0
        assert_eq!(f16_to_f32(0x0000), 0.0);
    }
}
