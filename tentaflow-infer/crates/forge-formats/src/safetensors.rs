// ===== File: safetensors.rs — mmap safetensors loader (single-file + HF sharded index) =====

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use forge_types::{DType, ForgeError, Result};
use memmap2::Mmap;
use serde::Deserialize;

fn fmt_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Format(msg.into())
}

/// Safetensors headers are small JSON blobs; a multi-hundred-MB declared
/// header length is corrupt or hostile, so cap it before allocating/parsing.
const MAX_HEADER_LEN: u64 = 100 * 1024 * 1024;

fn dtype_from_st(s: &str) -> Result<DType> {
    Ok(match s {
        "F32" => DType::F32,
        "F16" => DType::F16,
        "BF16" => DType::BF16,
        "F8_E4M3" => DType::F8E4M3,
        "F8_E5M2" => DType::F8E5M2,
        "I8" => DType::I8,
        "U8" => DType::U8,
        "I32" => DType::I32,
        "I64" => DType::I64,
        other => {
            return Err(ForgeError::Unsupported(format!(
                "safetensors: unsupported dtype '{other}'"
            )))
        }
    })
}

/// One tensor entry with its absolute byte range in the file resolved and
/// bounds-checked at load time.
#[derive(Debug, Clone)]
pub struct StTensor {
    pub dtype: DType,
    pub shape: Vec<usize>,
    abs_start: usize,
    abs_end: usize,
}

impl StTensor {
    pub fn numel(&self) -> usize {
        self.shape.iter().product()
    }
}

#[derive(Deserialize)]
struct RawEntry {
    dtype: String,
    shape: Vec<u64>,
    data_offsets: [u64; 2],
}

/// A single mmapped .safetensors file.
pub struct SafeTensors {
    mmap: Mmap,
    tensors: HashMap<String, StTensor>,
    pub metadata: HashMap<String, String>,
}

impl SafeTensors {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())?;
        // SAFETY: read-only mapping; see gguf.rs for the accepted TOCTOU trade-off.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::parse(mmap)
    }

    fn parse(mmap: Mmap) -> Result<Self> {
        if mmap.len() < 8 {
            return Err(fmt_err(
                "safetensors: file shorter than header length field",
            ));
        }
        let header_len = u64::from_le_bytes(mmap[0..8].try_into().expect("8-byte slice"));
        if header_len > MAX_HEADER_LEN {
            return Err(fmt_err(format!(
                "safetensors: declared header length {header_len} exceeds cap"
            )));
        }
        let header_len = header_len as usize;
        let data_start = header_len
            .checked_add(8)
            .ok_or_else(|| fmt_err("safetensors: header length overflow"))?;
        if data_start > mmap.len() {
            return Err(fmt_err("safetensors: header extends past end of file"));
        }

        let header: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&mmap[8..data_start])
                .map_err(|e| fmt_err(format!("safetensors: bad header JSON: {e}")))?;
        let data_len = mmap.len() - data_start;

        let mut metadata = HashMap::new();
        let mut tensors = HashMap::new();
        for (name, value) in header {
            if name == "__metadata__" {
                if let serde_json::Value::Object(map) = value {
                    for (k, v) in map {
                        if let serde_json::Value::String(s) = v {
                            metadata.insert(k, s);
                        }
                    }
                }
                continue;
            }
            let entry: RawEntry = serde_json::from_value(value)
                .map_err(|e| fmt_err(format!("safetensors: bad entry for '{name}': {e}")))?;
            let dtype = dtype_from_st(&entry.dtype)?;
            let mut shape = Vec::with_capacity(entry.shape.len());
            let mut numel: u64 = 1;
            for &d in &entry.shape {
                numel = numel.checked_mul(d).ok_or_else(|| {
                    fmt_err(format!("safetensors: tensor '{name}' numel overflow"))
                })?;
                shape.push(
                    usize::try_from(d).map_err(|_| {
                        fmt_err(format!("safetensors: tensor '{name}' dim overflow"))
                    })?,
                );
            }
            let [start, end] = entry.data_offsets;
            if start > end || end > data_len as u64 {
                return Err(fmt_err(format!(
                    "safetensors: tensor '{name}' offsets [{start}, {end}] out of bounds (data {data_len})"
                )));
            }
            let expected = numel
                .checked_mul(dtype.size() as u64)
                .ok_or_else(|| fmt_err(format!("safetensors: tensor '{name}' size overflow")))?;
            if end - start != expected {
                return Err(fmt_err(format!(
                    "safetensors: tensor '{name}' byte span {} does not match shape ({expected} expected)",
                    end - start
                )));
            }
            tensors.insert(
                name,
                StTensor {
                    dtype,
                    shape,
                    abs_start: data_start + start as usize,
                    abs_end: data_start + end as usize,
                },
            );
        }
        Ok(SafeTensors {
            mmap,
            tensors,
            metadata,
        })
    }

    pub fn tensor(&self, name: &str) -> Option<&StTensor> {
        self.tensors.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tensors.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }

    /// Zero-copy view of a tensor's raw bytes.
    pub fn data(&self, name: &str) -> Result<&[u8]> {
        let t = self
            .tensors
            .get(name)
            .ok_or_else(|| fmt_err(format!("safetensors: no tensor named '{name}'")))?;
        Ok(&self.mmap[t.abs_start..t.abs_end])
    }
}

#[derive(Deserialize)]
struct ShardIndex {
    weight_map: HashMap<String, String>,
}

/// Multi-file model: resolves `model.safetensors.index.json` if present,
/// otherwise loads the single `model.safetensors`.
pub struct ShardedSafeTensors {
    shards: Vec<SafeTensors>,
    by_name: HashMap<String, usize>,
}

impl ShardedSafeTensors {
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let index_path = dir.join("model.safetensors.index.json");
        if index_path.is_file() {
            let index: ShardIndex = serde_json::from_slice(&std::fs::read(&index_path)?)
                .map_err(|e| fmt_err(format!("safetensors: bad index JSON: {e}")))?;
            let mut shard_paths: Vec<PathBuf> = Vec::new();
            let mut shard_ids: HashMap<PathBuf, usize> = HashMap::new();
            let mut by_name = HashMap::with_capacity(index.weight_map.len());
            for (tensor, file) in &index.weight_map {
                // Containment: shard references are bare filenames inside the
                // model dir; reject anything with path components.
                if file.contains('/') || file.contains('\\') || file.contains("..") {
                    return Err(fmt_err(format!(
                        "safetensors: index references suspicious shard path '{file}'"
                    )));
                }
                let path = dir.join(file);
                let id = *shard_ids.entry(path.clone()).or_insert_with(|| {
                    shard_paths.push(path);
                    shard_paths.len() - 1
                });
                by_name.insert(tensor.clone(), id);
            }
            let mut shards = Vec::with_capacity(shard_paths.len());
            for path in &shard_paths {
                shards.push(SafeTensors::open(path)?);
            }
            // Validate the index against the actual shard contents.
            for (tensor, &id) in &by_name {
                if shards[id].tensor(tensor).is_none() {
                    return Err(fmt_err(format!(
                        "safetensors: index lists '{tensor}' but shard does not contain it"
                    )));
                }
            }
            Ok(ShardedSafeTensors { shards, by_name })
        } else {
            let single = SafeTensors::open(dir.join("model.safetensors"))?;
            let by_name = single.names().map(|n| (n.to_string(), 0usize)).collect();
            Ok(ShardedSafeTensors {
                shards: vec![single],
                by_name,
            })
        }
    }

    pub fn tensor(&self, name: &str) -> Option<&StTensor> {
        self.by_name
            .get(name)
            .and_then(|&i| self.shards[i].tensor(name))
    }

    pub fn data(&self, name: &str) -> Result<&[u8]> {
        let &i = self
            .by_name
            .get(name)
            .ok_or_else(|| fmt_err(format!("safetensors: no tensor named '{name}'")))?;
        self.shards[i].data(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_st(dir: &Path, name: &str, header: &str, payload: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(&(header.len() as u64).to_le_bytes()).unwrap();
        f.write_all(header.as_bytes()).unwrap();
        f.write_all(payload).unwrap();
        path
    }

    #[test]
    fn parses_minimal_file_and_rejects_bad_offsets() {
        let dir = std::env::temp_dir().join("forge-formats-st-test");
        std::fs::create_dir_all(&dir).unwrap();
        let header = r#"{"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#;
        let path = write_st(&dir, "ok.safetensors", header, &[0u8; 16]);
        let st = SafeTensors::open(&path).unwrap();
        let t = st.tensor("w").unwrap();
        assert_eq!(t.dtype, DType::F32);
        assert_eq!(t.shape, vec![2, 2]);
        assert_eq!(st.data("w").unwrap().len(), 16);

        // span does not match shape
        let bad = r#"{"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,12]}}"#;
        let path = write_st(&dir, "bad.safetensors", bad, &[0u8; 16]);
        assert!(SafeTensors::open(&path).is_err());

        // offsets out of bounds
        let oob = r#"{"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#;
        let path = write_st(&dir, "oob.safetensors", oob, &[0u8; 4]);
        assert!(SafeTensors::open(&path).is_err());
    }

    #[test]
    fn header_length_cap() {
        let dir = std::env::temp_dir().join("forge-formats-st-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hugehdr.safetensors");
        let mut f = File::create(&path).unwrap();
        f.write_all(&u64::MAX.to_le_bytes()).unwrap();
        f.write_all(&[0u8; 32]).unwrap();
        assert!(SafeTensors::open(&path).is_err());
    }

    #[test]
    fn sharded_index_rejects_path_traversal() {
        let dir = std::env::temp_dir().join("forge-formats-st-shard-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("model.safetensors.index.json"),
            r#"{"weight_map":{"w":"../evil.safetensors"}}"#,
        )
        .unwrap();
        assert!(ShardedSafeTensors::load_dir(&dir).is_err());
    }
}
