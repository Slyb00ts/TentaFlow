// ===== File: tensor.rs — host-resident runtime tensor for the ONNX executor =====
//
// Values flowing through the interpreter are host tensors: a dtype, a shape and
// a row-major little-endian byte buffer. Compute-heavy ops upload the relevant
// tensors to the GPU, run a Mojo kernel, and download the result back into a
// host tensor; shape/control ops read the bytes directly. Silero's tensors are
// tiny (a 512-sample frame), so the per-op host↔device staging is negligible.

use forge_types::{DType, ForgeError, Result};

use crate::proto::TensorProto;

#[derive(Debug, Clone)]
pub struct Tensor {
    pub dtype: DType,
    pub shape: Vec<usize>,
    pub data: Vec<u8>,
}

/// Map an ONNX TensorProto.data_type code to a FORGE DType.
pub fn onnx_dtype(code: i32) -> Result<DType> {
    Ok(match code {
        1 => DType::F32,
        2 => DType::U8,
        3 => DType::I8,
        4 => DType::U16,
        5 => DType::I16,
        6 => DType::I32,
        7 => DType::I64,
        9 => DType::Bool,
        10 => DType::F16,
        11 => DType::F64,
        12 => DType::U32,
        13 => DType::U64,
        other => {
            return Err(ForgeError::Unsupported(format!(
                "onnx tensor data_type {other} unsupported"
            )))
        }
    })
}

impl Tensor {
    pub fn new(dtype: DType, shape: Vec<usize>, data: Vec<u8>) -> Self {
        Self { dtype, shape, data }
    }

    pub fn numel(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn from_f32(shape: Vec<usize>, values: Vec<f32>) -> Self {
        let mut data = Vec::with_capacity(values.len() * 4);
        for v in &values {
            data.extend_from_slice(&v.to_le_bytes());
        }
        Self {
            dtype: DType::F32,
            shape,
            data,
        }
    }

    pub fn from_i64(shape: Vec<usize>, values: Vec<i64>) -> Self {
        let mut data = Vec::with_capacity(values.len() * 8);
        for v in &values {
            data.extend_from_slice(&v.to_le_bytes());
        }
        Self {
            dtype: DType::I64,
            shape,
            data,
        }
    }

    pub fn from_bool(shape: Vec<usize>, values: Vec<bool>) -> Self {
        Self {
            dtype: DType::Bool,
            shape,
            data: values.into_iter().map(|b| b as u8).collect(),
        }
    }

    pub fn scalar_i64(v: i64) -> Self {
        Self::from_i64(vec![], vec![v])
    }

    /// Interpret the buffer as f32 values, converting from f16/f64/int dtypes.
    pub fn to_f32_vec(&self) -> Result<Vec<f32>> {
        let n = self.numel();
        Ok(match self.dtype {
            DType::F32 => self
                .data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            DType::F16 => self
                .data
                .chunks_exact(2)
                .map(|c| half::f16::from_le_bytes(c.try_into().unwrap()).to_f32())
                .collect(),
            DType::F64 => self
                .data
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32)
                .collect(),
            DType::I64 => self.to_i64_vec()?.into_iter().map(|v| v as f32).collect(),
            DType::I32 => self
                .data
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as f32)
                .collect(),
            DType::Bool => self.data.iter().map(|&b| (b != 0) as u8 as f32).collect(),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "to_f32_vec: dtype {other} unsupported"
                )))
            }
        })
        .and_then(|v: Vec<f32>| {
            if v.len() == n {
                Ok(v)
            } else {
                Err(ForgeError::Format("tensor element count mismatch".into()))
            }
        })
    }

    /// Interpret the buffer as i64 values, converting from int/bool/float dtypes.
    pub fn to_i64_vec(&self) -> Result<Vec<i64>> {
        Ok(match self.dtype {
            DType::I64 => self
                .data
                .chunks_exact(8)
                .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            DType::I32 => self
                .data
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as i64)
                .collect(),
            DType::U8 => self.data.iter().map(|&b| b as i64).collect(),
            DType::Bool => self.data.iter().map(|&b| (b != 0) as i64).collect(),
            DType::F32 => self.to_f32_vec()?.into_iter().map(|v| v as i64).collect(),
            DType::F64 => self
                .data
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()) as i64)
                .collect(),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "to_i64_vec: dtype {other} unsupported"
                )))
            }
        })
    }

    pub fn to_bool_vec(&self) -> Result<Vec<bool>> {
        Ok(self.to_i64_vec()?.into_iter().map(|v| v != 0).collect())
    }

    /// Materialize an ONNX initializer/constant TensorProto into a host tensor.
    pub fn from_proto(t: &TensorProto) -> Result<Self> {
        let dtype = onnx_dtype(t.data_type)?;
        let shape: Vec<usize> = t
            .dims
            .iter()
            .map(|&d| {
                if d < 0 {
                    Err(ForgeError::Format("onnx: negative tensor dim".into()))
                } else {
                    Ok(d as usize)
                }
            })
            .collect::<Result<_>>()?;
        let numel: usize = shape.iter().product();

        // raw_data is the canonical little-endian payload; typed *_data fields
        // are the alternative for exporters that use them.
        if let Some(raw) = &t.raw_data {
            let expect = numel * dtype.size();
            if raw.len() != expect {
                return Err(ForgeError::Format(format!(
                    "onnx tensor {}: raw_data {} bytes, expected {expect}",
                    t.name,
                    raw.len()
                )));
            }
            return Ok(Self {
                dtype,
                shape,
                data: raw.clone(),
            });
        }

        let data = match dtype {
            DType::F32 => flatten(&t.float_data, numel, |v| v.to_le_bytes().to_vec(), &t.name)?,
            DType::F64 => flatten(&t.double_data, numel, |v| v.to_le_bytes().to_vec(), &t.name)?,
            DType::I64 => flatten(&t.int64_data, numel, |v| v.to_le_bytes().to_vec(), &t.name)?,
            DType::I32 | DType::I16 | DType::I8 => {
                flatten(&t.int32_data, numel, |v| v.to_le_bytes().to_vec(), &t.name)?
            }
            DType::Bool | DType::U8 => flatten(&t.int32_data, numel, |v| vec![v as u8], &t.name)?,
            DType::U64 | DType::U32 | DType::U16 => {
                flatten(&t.uint64_data, numel, |v| v.to_le_bytes().to_vec(), &t.name)?
            }
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "onnx tensor {}: dtype {other} without raw_data",
                    t.name
                )))
            }
        };
        Ok(Self { dtype, shape, data })
    }
}

fn flatten<T: Copy, F: Fn(T) -> Vec<u8>>(
    src: &[T],
    numel: usize,
    to_bytes: F,
    name: &str,
) -> Result<Vec<u8>> {
    if src.len() != numel {
        return Err(ForgeError::Format(format!(
            "onnx tensor {name}: {} typed values, expected {numel}",
            src.len()
        )));
    }
    Ok(src.iter().flat_map(|&v| to_bytes(v)).collect())
}
