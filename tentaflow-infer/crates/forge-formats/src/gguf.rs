// ===== File: gguf.rs — GGUF v2/v3 parser: mmap, typed metadata, zero-copy tensor views =====

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use forge_types::{DType, ForgeError, QuantKind, Result};
use memmap2::Mmap;

/// GGUF metadata array nesting is bounded so a malicious file cannot drive the
/// recursive-descent parser into stack exhaustion.
const MAX_ARRAY_DEPTH: usize = 8;
const GGUF_MAGIC: &[u8; 4] = b"GGUF";

fn fmt_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Format(msg.into())
}

/// Fully typed GGUF metadata value (all 13 wire types, arrays may nest).
#[derive(Debug, Clone, PartialEq)]
pub enum MetaValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    F32(f32),
    Bool(bool),
    String(String),
    Array(Vec<MetaValue>),
    U64(u64),
    I64(i64),
    F64(f64),
}

impl MetaValue {
    /// Widening read of any integer value that fits in u64 (writers are
    /// inconsistent about which integer width they emit for counts).
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            MetaValue::U8(v) => Some(v as u64),
            MetaValue::U16(v) => Some(v as u64),
            MetaValue::U32(v) => Some(v as u64),
            MetaValue::U64(v) => Some(v),
            MetaValue::I8(v) => u64::try_from(v).ok(),
            MetaValue::I16(v) => u64::try_from(v).ok(),
            MetaValue::I32(v) => u64::try_from(v).ok(),
            MetaValue::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            MetaValue::F32(v) => Some(v),
            MetaValue::F64(v) => Some(v as f32),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            MetaValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            MetaValue::Bool(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[MetaValue]> {
        match self {
            MetaValue::Array(v) => Some(v),
            _ => None,
        }
    }
}

/// One entry of the GGUF tensor table with its absolute byte range resolved.
#[derive(Debug, Clone)]
pub struct GgufTensor {
    pub name: String,
    /// Dims in GGUF order: dims[0] is the innermost (contiguous) dimension.
    pub dims: Vec<u64>,
    pub ggml_type: u32,
    pub dtype: DType,
    pub quant: QuantKind,
    /// Offset relative to the start of the data section (as stored on disk).
    pub offset: u64,
    /// Absolute byte range inside the file, fully bounds-checked at parse time.
    pub abs_start: usize,
    pub size_bytes: usize,
}

impl GgufTensor {
    pub fn numel(&self) -> u64 {
        self.dims.iter().product()
    }
}

/// Map a raw GGML type id to FORGE (DType, QuantKind).
///
/// Numbering verified against the vendored llama.cpp checkout pinned by the
/// TentaFlow native-libs scripts (`ggml/include/ggml.h`, enum ggml_type).
/// Quantized formats carry raw block bytes, so their element view is `U8`.
pub fn ggml_type_to_forge(id: u32) -> Result<(DType, QuantKind)> {
    let q = |k: QuantKind| Ok((DType::U8, k));
    match id {
        0 => Ok((DType::F32, QuantKind::None)),
        1 => Ok((DType::F16, QuantKind::None)),
        2 => q(QuantKind::Q4_0),
        3 => q(QuantKind::Q4_1),
        6 => q(QuantKind::Q5_0),
        7 => q(QuantKind::Q5_1),
        8 => q(QuantKind::Q8_0),
        9 => q(QuantKind::Q8_1),
        10 => q(QuantKind::Q2K),
        11 => q(QuantKind::Q3K),
        12 => q(QuantKind::Q4K),
        13 => q(QuantKind::Q5K),
        14 => q(QuantKind::Q6K),
        15 => q(QuantKind::Q8K),
        16 => q(QuantKind::IQ2XXS),
        17 => q(QuantKind::IQ2XS),
        18 => q(QuantKind::IQ3XXS),
        19 => q(QuantKind::IQ1S),
        20 => q(QuantKind::IQ4NL),
        21 => q(QuantKind::IQ3S),
        22 => q(QuantKind::IQ2S),
        23 => q(QuantKind::IQ4XS),
        24 => Ok((DType::I8, QuantKind::None)),
        26 => Ok((DType::I32, QuantKind::None)),
        27 => Ok((DType::I64, QuantKind::None)),
        29 => q(QuantKind::IQ1M),
        30 => Ok((DType::BF16, QuantKind::None)),
        39 => q(QuantKind::MXFP4),
        40 => q(QuantKind::NVFP4Gguf),
        // 25=I16, 28=F64, 34/35=TQ*, 41=Q1_0.
        other => Err(ForgeError::Unsupported(format!(
            "ggml tensor type id {other} is not supported"
        ))),
    }
}

/// Byte cursor over the mmap with checked reads only.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| fmt_err("gguf: offset overflow"))?;
        if end > self.data.len() {
            return Err(fmt_err(format!(
                "gguf: truncated file: need {} bytes at offset {}, file has {}",
                n,
                self.pos,
                self.data.len()
            )));
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u64()?;
        let len = usize::try_from(len).map_err(|_| fmt_err("gguf: string length overflow"))?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| fmt_err("gguf: string is not valid UTF-8"))
    }

    fn value(&mut self, type_id: u32, depth: usize) -> Result<MetaValue> {
        Ok(match type_id {
            0 => MetaValue::U8(self.u8()?),
            1 => MetaValue::I8(self.u8()? as i8),
            2 => MetaValue::U16(self.u16()?),
            3 => MetaValue::I16(self.u16()? as i16),
            4 => MetaValue::U32(self.u32()?),
            5 => MetaValue::I32(self.u32()? as i32),
            6 => MetaValue::F32(self.f32()?),
            7 => MetaValue::Bool(match self.u8()? {
                0 => false,
                1 => true,
                other => return Err(fmt_err(format!("gguf: invalid bool byte {other}"))),
            }),
            8 => MetaValue::String(self.string()?),
            9 => {
                if depth >= MAX_ARRAY_DEPTH {
                    return Err(fmt_err("gguf: metadata array nesting too deep"));
                }
                let elem_type = self.u32()?;
                let n = self.u64()?;
                let n = usize::try_from(n).map_err(|_| fmt_err("gguf: array length overflow"))?;
                // Every element occupies at least one byte on the wire, so a
                // declared count larger than the remaining bytes is corrupt —
                // reject before allocating.
                if n > self.data.len().saturating_sub(self.pos) {
                    return Err(fmt_err(format!(
                        "gguf: array of {n} elements exceeds remaining file size"
                    )));
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.value(elem_type, depth + 1)?);
                }
                MetaValue::Array(items)
            }
            10 => MetaValue::U64(self.u64()?),
            11 => MetaValue::I64(self.u64()? as i64),
            12 => MetaValue::F64(self.f64()?),
            other => return Err(fmt_err(format!("gguf: unknown metadata type {other}"))),
        })
    }
}

/// Parsed GGUF file. Owns the mmap; tensor data accessors hand out zero-copy
/// slices into it.
pub struct Gguf {
    mmap: Mmap,
    pub version: u32,
    pub alignment: u64,
    metadata: HashMap<String, MetaValue>,
    tensors: Vec<GgufTensor>,
    by_name: HashMap<String, usize>,
    pub data_start: usize,
}

impl Gguf {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())?;
        // SAFETY: read-only mapping of a regular file; TOCTOU truncation by an
        // external writer would fault, which is the standard mmap trade-off
        // accepted for model loading (same as llama.cpp / safetensors crates).
        let mmap = unsafe { Mmap::map(&file)? };
        Self::parse(mmap)
    }

    fn parse(mmap: Mmap) -> Result<Self> {
        let mut cur = Cursor::new(&mmap[..]);

        if cur.take(4)? != GGUF_MAGIC {
            return Err(fmt_err("gguf: bad magic (not a GGUF file)"));
        }
        let version = cur.u32()?;
        if version != 2 && version != 3 {
            return Err(fmt_err(format!(
                "gguf: unsupported version {version} (only v2/v3)"
            )));
        }

        let tensor_count = cur.u64()?;
        let kv_count = cur.u64()?;
        let tensor_count =
            usize::try_from(tensor_count).map_err(|_| fmt_err("gguf: tensor count overflow"))?;
        let kv_count = usize::try_from(kv_count).map_err(|_| fmt_err("gguf: kv count overflow"))?;
        // Each tensor entry / kv entry is at least ~20 bytes on the wire;
        // reject absurd counts before reserving memory for them.
        let remaining = mmap.len().saturating_sub(cur.pos);
        if tensor_count > remaining / 20 || kv_count > remaining {
            return Err(fmt_err("gguf: declared entry counts exceed file size"));
        }

        let mut metadata = HashMap::with_capacity(kv_count);
        for _ in 0..kv_count {
            let key = cur.string()?;
            if metadata.contains_key(&key) {
                return Err(fmt_err(format!("gguf: duplicate metadata key '{key}'")));
            }
            let type_id = cur.u32()?;
            let value = cur.value(type_id, 0)?;
            metadata.insert(key, value);
        }

        let alignment = metadata
            .get("general.alignment")
            .and_then(MetaValue::as_u64)
            .unwrap_or(32);
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(fmt_err(format!("gguf: invalid alignment {alignment}")));
        }

        struct RawTensor {
            name: String,
            dims: Vec<u64>,
            ggml_type: u32,
            offset: u64,
        }
        let mut raw = Vec::with_capacity(tensor_count);
        for _ in 0..tensor_count {
            let name = cur.string()?;
            let n_dims = cur.u32()?;
            if n_dims == 0 || n_dims > 4 {
                return Err(fmt_err(format!(
                    "gguf: tensor '{name}' has invalid n_dims {n_dims}"
                )));
            }
            let mut dims = Vec::with_capacity(n_dims as usize);
            for _ in 0..n_dims {
                dims.push(cur.u64()?);
            }
            let ggml_type = cur.u32()?;
            let offset = cur.u64()?;
            raw.push(RawTensor {
                name,
                dims,
                ggml_type,
                offset,
            });
        }

        // Data section begins at the header end rounded up to the alignment.
        let header_end = cur.pos as u64;
        let data_start = header_end
            .checked_add(alignment - 1)
            .ok_or_else(|| fmt_err("gguf: data offset overflow"))?
            & !(alignment - 1);
        let data_start =
            usize::try_from(data_start).map_err(|_| fmt_err("gguf: data offset overflow"))?;
        if data_start > mmap.len() {
            return Err(fmt_err("gguf: data section starts past end of file"));
        }
        let data_len = mmap.len() - data_start;

        let mut tensors = Vec::with_capacity(raw.len());
        let mut by_name = HashMap::with_capacity(raw.len());
        for t in raw {
            if t.offset % alignment != 0 {
                return Err(fmt_err(format!(
                    "gguf: tensor '{}' offset {} is not aligned to {alignment} bytes",
                    t.name, t.offset
                )));
            }
            let (dtype, quant) = ggml_type_to_forge(t.ggml_type)?;
            let mut numel: u64 = 1;
            for &d in &t.dims {
                numel = numel
                    .checked_mul(d)
                    .ok_or_else(|| fmt_err(format!("gguf: tensor '{}' numel overflow", t.name)))?;
            }
            let size_bytes = tensor_byte_size(&t.name, dtype, quant, numel, t.dims[0])?;
            let offset = checked_tensor_extent(&t.name, t.offset, size_bytes, data_len)?;
            let idx = tensors.len();
            if by_name.insert(t.name.clone(), idx).is_some() {
                return Err(fmt_err(format!("gguf: duplicate tensor name '{}'", t.name)));
            }
            tensors.push(GgufTensor {
                name: t.name,
                dims: t.dims,
                ggml_type: t.ggml_type,
                dtype,
                quant,
                offset: t.offset,
                abs_start: data_start + offset,
                size_bytes,
            });
        }
        validate_tensor_ranges(&tensors)?;

        Ok(Gguf {
            mmap,
            version,
            alignment,
            metadata,
            tensors,
            by_name,
            data_start,
        })
    }

    pub fn tensors(&self) -> &[GgufTensor] {
        &self.tensors
    }

    pub fn tensor(&self, name: &str) -> Option<&GgufTensor> {
        self.by_name.get(name).map(|&i| &self.tensors[i])
    }

    /// Zero-copy view of a tensor's raw bytes inside the mmap.
    pub fn tensor_data(&self, name: &str) -> Result<&[u8]> {
        let t = self
            .tensor(name)
            .ok_or_else(|| fmt_err(format!("gguf: no tensor named '{name}'")))?;
        // Range was validated against the file length during parse.
        Ok(&self.mmap[t.abs_start..t.abs_start + t.size_bytes])
    }

    pub fn metadata(&self) -> &HashMap<String, MetaValue> {
        &self.metadata
    }

    pub fn get(&self, key: &str) -> Option<&MetaValue> {
        self.metadata.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(MetaValue::as_str)
    }

    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key)
            .and_then(MetaValue::as_u64)
            .and_then(|v| u32::try_from(v).ok())
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(MetaValue::as_u64)
    }

    pub fn get_f32(&self, key: &str) -> Option<f32> {
        self.get(key).and_then(MetaValue::as_f32)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(MetaValue::as_bool)
    }

    pub fn get_array(&self, key: &str) -> Option<&[MetaValue]> {
        self.get(key).and_then(MetaValue::as_array)
    }

    pub fn get_str_array(&self, key: &str) -> Option<Vec<&str>> {
        self.get_array(key)?.iter().map(MetaValue::as_str).collect()
    }
}

/// Byte size of a tensor given its element count, with block-divisibility
/// enforced on the innermost dim exactly like ggml does.
fn tensor_byte_size(
    name: &str,
    dtype: DType,
    quant: QuantKind,
    numel: u64,
    inner_dim: u64,
) -> Result<usize> {
    let bytes = if quant == QuantKind::None {
        numel
            .checked_mul(dtype.size() as u64)
            .ok_or_else(|| fmt_err(format!("gguf: tensor '{name}' size overflow")))?
    } else {
        let be = quant.block_elems() as u64;
        let bb = quant.block_bytes() as u64;
        if inner_dim % be != 0 {
            return Err(fmt_err(format!(
                "gguf: tensor '{name}' inner dim {inner_dim} not divisible by block size {be}"
            )));
        }
        (numel / be)
            .checked_mul(bb)
            .ok_or_else(|| fmt_err(format!("gguf: tensor '{name}' size overflow")))?
    };
    usize::try_from(bytes).map_err(|_| fmt_err(format!("gguf: tensor '{name}' size overflow")))
}

fn checked_tensor_extent(
    name: &str,
    offset: u64,
    size_bytes: usize,
    data_len: usize,
) -> Result<usize> {
    let offset = usize::try_from(offset)
        .map_err(|_| fmt_err(format!("gguf: tensor '{name}' offset overflow")))?;
    let end = offset
        .checked_add(size_bytes)
        .ok_or_else(|| fmt_err(format!("gguf: tensor '{name}' extent overflow")))?;
    if end > data_len {
        return Err(fmt_err(format!(
            "gguf: tensor '{name}' [{offset}..{end}] exceeds data section of {data_len} bytes"
        )));
    }
    Ok(offset)
}

fn validate_tensor_ranges(tensors: &[GgufTensor]) -> Result<()> {
    let mut ranges: Vec<_> = tensors
        .iter()
        .map(|tensor| {
            (
                tensor.offset,
                tensor.offset + tensor.size_bytes as u64,
                &tensor.name,
            )
        })
        .collect();
    ranges.sort_unstable_by_key(|range| range.0);

    let mut previous_end = 0;
    let mut previous_name: Option<&str> = None;
    for (start, end, name) in ranges {
        if start < previous_end {
            return Err(fmt_err(format!(
                "gguf: tensor '{name}' overlaps tensor '{}'",
                previous_name.unwrap_or("<unknown>")
            )));
        }
        previous_end = end;
        previous_name = Some(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u64).to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }

    fn open_test_gguf(metadata: &[(&str, u8)], tensor_offsets: &[u64]) -> Result<Gguf> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(GGUF_MAGIC);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&(tensor_offsets.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
        for &(key, value) in metadata {
            write_string(&mut bytes, key);
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.push(value);
        }
        for (index, &offset) in tensor_offsets.iter().enumerate() {
            write_string(&mut bytes, &format!("tensor.{index}"));
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&16u64.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
        }
        bytes.resize((bytes.len() + 31) & !31, 0);
        let data_len = tensor_offsets
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(64) as usize;
        bytes.resize(bytes.len() + data_len, 0);

        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&bytes)?;
        Gguf::open(file.path())
    }

    #[test]
    fn ggml_type_table_matches_ggml_h() {
        assert_eq!(
            ggml_type_to_forge(0).unwrap(),
            (DType::F32, QuantKind::None)
        );
        assert_eq!(
            ggml_type_to_forge(1).unwrap(),
            (DType::F16, QuantKind::None)
        );
        assert_eq!(ggml_type_to_forge(8).unwrap().1, QuantKind::Q8_0);
        assert_eq!(ggml_type_to_forge(12).unwrap().1, QuantKind::Q4K);
        assert_eq!(ggml_type_to_forge(14).unwrap().1, QuantKind::Q6K);
        assert_eq!(ggml_type_to_forge(20).unwrap().1, QuantKind::IQ4NL);
        assert_eq!(ggml_type_to_forge(23).unwrap().1, QuantKind::IQ4XS);
        assert_eq!(
            ggml_type_to_forge(30).unwrap(),
            (DType::BF16, QuantKind::None)
        );
        assert_eq!(ggml_type_to_forge(39).unwrap().1, QuantKind::MXFP4);
        assert_eq!(ggml_type_to_forge(40).unwrap().1, QuantKind::NVFP4Gguf);
        assert!(ggml_type_to_forge(4).is_err()); // removed q4_2
        assert!(ggml_type_to_forge(28).is_err()); // f64 unsupported
        assert!(ggml_type_to_forge(42).is_err());
    }

    #[test]
    fn rejects_truncated_and_bad_magic() {
        // Cursor-level checks stand in for full-file fuzzing here.
        let mut cur = Cursor::new(b"GGU");
        assert!(cur.take(4).is_err());
        let mut cur = Cursor::new(&[9u8, 0, 0, 0]); // array type with no payload
        assert!(cur.value(9, 0).is_err());
    }

    #[test]
    fn array_count_larger_than_file_is_rejected() {
        // type=array(elem u8), declared 2^32 elements but no bytes follow.
        let mut buf = vec![];
        buf.extend_from_slice(&0u32.to_le_bytes()); // elem type u8
        buf.extend_from_slice(&(u32::MAX as u64).to_le_bytes());
        let mut cur = Cursor::new(&buf);
        assert!(cur.value(9, 0).is_err());
    }

    #[test]
    fn rejects_duplicate_metadata_keys() {
        let error = open_test_gguf(&[("key", 1), ("key", 2)], &[])
            .err()
            .expect("odrzuć zduplikowany klucz metadanych");
        assert!(error.to_string().contains("duplicate metadata key 'key'"));
    }

    #[test]
    fn rejects_misaligned_tensor_offset() {
        let error = open_test_gguf(&[], &[1])
            .err()
            .expect("odrzuć niewyrównany offset tensora");
        assert!(error.to_string().contains("is not aligned"));
    }

    #[test]
    fn rejects_overlapping_tensor_ranges() {
        let error = open_test_gguf(&[], &[0, 32])
            .err()
            .expect("odrzuć nakładające się tensory");
        assert!(error.to_string().contains("overlaps tensor"));
    }

    #[test]
    fn accepts_aligned_non_overlapping_tensor_ranges() {
        assert!(open_test_gguf(&[], &[0, 64]).is_ok());
    }

    #[test]
    fn nvfp4_gguf_size_is_checked() {
        assert_eq!(
            tensor_byte_size("weight", DType::U8, QuantKind::NVFP4Gguf, 128, 64).unwrap(),
            72
        );
        assert!(tensor_byte_size("weight", DType::U8, QuantKind::NVFP4Gguf, 63, 63,).is_err());
        assert_eq!(checked_tensor_extent("weight", 32, 36, 68).unwrap(), 32);
        assert!(checked_tensor_extent("weight", u64::MAX, 36, usize::MAX).is_err());
        assert!(checked_tensor_extent("weight", 64, 36, 99).is_err());
    }
}
