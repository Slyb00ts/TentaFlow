// ===== File: dtype.rs — scalar element types used across HAL, formats and kernels =====

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DType {
    F32,
    F16,
    BF16,
    F8E4M3,
    F8E5M2,
    F64,
    I64,
    I32,
    I16,
    I8,
    U8,
    U16,
    U32,
    U64,
    Bool,
}

impl DType {
    /// Size in bytes of one element. Sub-byte quantized formats are described
    /// by `QuantKind` block layouts, not `DType`.
    pub const fn size(self) -> usize {
        match self {
            DType::F32 | DType::I32 | DType::U32 => 4,
            DType::F16 | DType::BF16 | DType::I16 | DType::U16 => 2,
            DType::F8E4M3 | DType::F8E5M2 | DType::I8 | DType::U8 | DType::Bool => 1,
            DType::F64 | DType::I64 | DType::U64 => 8,
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(
            self,
            DType::F32 | DType::F16 | DType::BF16 | DType::F8E4M3 | DType::F8E5M2
        )
    }
}

impl std::fmt::Display for DType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DType::F32 => "f32",
            DType::F16 => "f16",
            DType::BF16 => "bf16",
            DType::F8E4M3 => "f8e4m3",
            DType::F8E5M2 => "f8e5m2",
            DType::F64 => "f64",
            DType::I64 => "i64",
            DType::I32 => "i32",
            DType::I16 => "i16",
            DType::I8 => "i8",
            DType::U8 => "u8",
            DType::U16 => "u16",
            DType::U32 => "u32",
            DType::U64 => "u64",
            DType::Bool => "bool",
        };
        f.write_str(s)
    }
}
