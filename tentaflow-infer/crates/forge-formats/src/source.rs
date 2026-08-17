// ===== File: source.rs — where weights come from, before any device sees them =====
//
// A checkpoint is a FILE LAYOUT, not a platform. GGUF, safetensors, NVFP4 and
// MLX differ in how they name tensors, how they pack them and which quirks they
// carry — and none of that has anything to do with whether the result will be
// multiplied by CUDA or by Metal. So the contract that turns a file into host
// bytes lives HERE, next to the parsers, and not inside one backend's loader.
//
// That placement is the point. It used to live inside forge-engine, which meant
// the Metal path could not reach it and grew its own ad-hoc reader for exactly
// one format. Two readers of the same files is how one of them silently misses
// a quirk the other handles — the Q/K row order below being the obvious one.

use half::f16;

use forge_types::{DType, ForgeError, QuantKind, Result};

use crate::gguf::Gguf;
use crate::nvfp4::{self, NvFp4Scheme, NvFp4TensorNames};
use crate::safetensors::ShardedSafeTensors;

/// Source-agnostic host-side tensor fetch: (bytes, dtype, quant, dims).
pub type TensorFetch = (Vec<u8>, DType, QuantKind, Vec<usize>);

pub trait TensorSource {
    /// Czy zrodlo trzyma wiersze Q/K w ORYGINALNEJ, przeplatanej kolejnosci
    /// rodziny Llama. GGUF tak; HF permutuje je juz przy konwersji, zeby moc
    /// liczyc rotacja NeoX. Kernele RoPE silnika sa NeoX, wiec przestawiac
    /// wiersze wolno TYLKO dla zrodel, ktore tego nie zrobily — inaczej
    /// permutacja zostaje nalozona dwa razy i model generuje smieci.
    fn stores_original_rope_order(&self) -> bool {
        false
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch>;
    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>>;
    /// NVFP4 triple fetch; None when the tensor is not NVFP4-packed.
    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>>;
    /// compressed-tensors FP8 ("float-quantized"): f8e4m3 weight + sibling
    /// `<base>.weight_scale` (per-channel or per-tensor). None when absent.
    fn fetch_fp8(&self, name: &str) -> Result<Option<Fp8Host>>;
    /// Wagi w układach swoistych dla DeepSeeka V4. `None` dla pozostałych
    /// źródeł i tensorów.
    fn fetch_deepseek(&self, _name: &str) -> Result<Option<HostWeight>> {
        Ok(None)
    }
    /// Wagi 4-bitowe afiniczne w postaci trzech tablic, gdy źródło potrafi je
    /// tak oddać BEZ konwersji. `None` znaczy „zapytaj przez `fetch` i przepisz
    /// sam" — a nie „ten tensor nie jest afiniczny".
    ///
    /// Istnieje, bo MLX trzyma dokładnie tę postać natywnie, ze skalami w bf16.
    /// Przepuszczanie go przez format pośredni po to, żeby wszystko wyglądało
    /// jednakowo, zwężało skale do f16 i gubiło te najmniejsze.
    fn fetch_affine(&self, _name: &str) -> Result<Option<crate::affine::AffineTriple>> {
        Ok(None)
    }
    /// Rozmiar tensora na dysku, bez jego wczytywania. `None`, gdy źródło nie
    /// potrafi go podać — wtedy budżet rezydencji ekspertów jest nieznany.
    fn byte_len(&self, name: &str) -> Option<usize>;
}

pub struct Fp8Host {
    pub weight: Vec<u8>,
    /// One scale per output row, or a single tensor-wide scale.
    pub scales: Vec<f32>,
    pub rows: usize,
    pub cols: usize,
}

pub struct NvFp4Host {
    pub packed: Vec<u8>,
    pub scales: Vec<u8>,
    pub global_scale: f32,
    pub rows: usize,
    pub cols: usize,
}

pub struct GgufSource<'a>(pub &'a Gguf);

impl TensorSource for GgufSource<'_> {
    fn stores_original_rope_order(&self) -> bool {
        true
    }

    fn byte_len(&self, name: &str) -> Option<usize> {
        self.0.tensor(name).map(|t| t.size_bytes)
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch> {
        let t = self
            .0
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("missing tensor {name}")))?;
        let data = self.0.tensor_data(name)?.to_vec();
        // GGUF dims are innermost-first; matrices arrive as [cols, rows].
        let mut dims: Vec<usize> = t.dims.iter().map(|&d| d as usize).collect();
        dims.reverse();
        Ok((data, t.dtype, t.quant, dims))
    }

    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>> {
        if self.0.tensor(name).is_none() {
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

pub struct StSource<'a> {
    pub st: &'a ShardedSafeTensors,
    pub scheme: Option<NvFp4Scheme>,
    /// compressed-tensors "float-quantized" (FP8 weights + scale siblings).
    pub fp8: bool,
    /// Checkpoint DeepSeeka V4: eksperci NVFP4 pod nazwą `weight` ze skalą
    /// globalną `weight_scale_2` (MNOŻĄCĄ), a pozostałe wagi FP8 ze skalą
    /// kafelkową w siostrzanym `.scale`. Oba układy różnią się od
    /// compressed-tensors na tyle, że wspólna ścieżka dałaby ciche śmieci.
    pub deepseek_v4: bool,
}

/// Wczytuje wagę DeepSeeka V4 w postaci, którą kernel przyjmie wprost.
/// `None`, gdy tensor nie pasuje do żadnego z dwóch układów tego checkpointu.
fn fetch_deepseek_weight(st: &ShardedSafeTensors, name: &str) -> Result<Option<HostWeight>> {
    let Some(info) = st.tensor(name) else {
        return Ok(None);
    };
    if info.shape.len() != 2 {
        return Ok(None);
    }
    let base = name.strip_suffix(".weight").unwrap_or(name);

    // DeepSeek publikuje ten model w dwóch kwantyzacjach FP4 i rozstrzyga o tym
    // NAZWA skali, nie typ pakietu: NVFP4 ma `.weight_scale` (UE4M3 co 16) oraz
    // `.weight_scale_2`, MXFP4 ma `.scale` (E8M0 co 32). Kody e2m1 są w obu te
    // same, więc MXFP4 przeliczamy na układ NVFP4 i dalej idzie jedna ścieżka
    // kerneli — przeliczenie jest dokładne, bo E8M0 to czysta potęga dwójki.
    if matches!(info.dtype, DType::U8 | DType::I8) {
        let mx = crate::mxfp4::DeepseekMxFp4Names::for_weight(name)?;
        if st.tensor(&mx.scale).is_some() {
            let packed = st.data(&mx.packed)?;
            let scales = st.data(&mx.scale)?;
            let repacked =
                crate::mxfp4::deepseek_expert_mxfp4_to_gguf(packed, &info.shape, scales)?;
            return Ok(Some(HostWeight::NvFp4Gguf {
                data: repacked.blocks,
                output_scale: repacked.output_scale,
                rows: repacked.rows,
                cols: repacked.cols,
            }));
        }
        let names = nvfp4::DeepseekNvFp4Names::for_weight(name)?;
        let scales = st.data(&names.scale)?;
        let global_bytes = st.data(&names.global_scale)?;
        if global_bytes.len() != 4 {
            return Err(ForgeError::Format(format!(
                "{}: oczekiwano skalarnej skali f32",
                names.global_scale
            )));
        }
        let global = f32::from_le_bytes([
            global_bytes[0],
            global_bytes[1],
            global_bytes[2],
            global_bytes[3],
        ]);
        let packed = st.data(&names.packed)?;
        let repacked = nvfp4::deepseek_expert_to_gguf(packed, &info.shape, scales, global)?;
        return Ok(Some(HostWeight::NvFp4Gguf {
            data: repacked.blocks,
            output_scale: repacked.output_scale,
            rows: repacked.rows,
            cols: repacked.cols,
        }));
    }

    // Waga FP8 z kafelkową skalą E8M0: skala idzie na wiersze, a różnica
    // wykładników wtapia się w bajty E4M3. Zmierzony błąd wyjścia projekcji to
    // 4,7e-7 przy jednym bajcie na wagę — wobec 5,4e-3 dla przekwantyzowania na
    // Q8_0 i 13,7 GiB dla materializacji do f16.
    if info.dtype == DType::F8E4M3 {
        let scale_name = format!("{base}.scale");
        let Some(scale_info) = st.tensor(&scale_name) else {
            return Ok(None);
        };
        let (rows, cols) = (info.shape[0], info.shape[1]);
        if scale_info.shape.len() != 2 || scale_info.shape[1] == 0 {
            return Err(ForgeError::Format(format!(
                "{scale_name}: oczekiwano dwuwymiarowej skali kafelkowej"
            )));
        }
        let tile = cols / scale_info.shape[1];
        if tile == 0 || rows.div_ceil(tile) != scale_info.shape[0] {
            return Err(ForgeError::Format(format!(
                "{scale_name}: kafel {tile} nie zgadza się z kształtem {:?}",
                scale_info.shape
            )));
        }
        let (data, scales) = nvfp4::deepseek_fp8_to_row_scaled(
            st.data(name)?,
            st.data(&scale_name)?,
            rows,
            cols,
            tile,
        )?;
        return Ok(Some(HostWeight::Fp8Row {
            data,
            scales,
            rows,
            cols,
        }));
    }

    Ok(None)
}

impl TensorSource for StSource<'_> {
    fn byte_len(&self, name: &str) -> Option<usize> {
        let info = self.st.tensor(name)?;
        Some(info.numel() * info.dtype.size())
    }

    fn fetch_deepseek(&self, name: &str) -> Result<Option<HostWeight>> {
        if !self.deepseek_v4 {
            return Ok(None);
        }
        fetch_deepseek_weight(self.st, name)
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch> {
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

    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>> {
        let Some(scheme) = &self.scheme else {
            return Ok(None);
        };
        let names = NvFp4TensorNames::for_weight(name)?;
        let Some(packed_t) = self.st.tensor(&names.packed) else {
            return Ok(None);
        };
        if scheme.group_size != 16 {
            return Err(ForgeError::Unsupported(format!(
                "nvfp4 group_size {} (kernel supports 16)",
                scheme.group_size
            )));
        }
        let rows = packed_t.shape[0];
        let cols = packed_t.shape[1] * 2;
        let packed = self.st.data(&names.packed)?.to_vec();
        let scales = self.st.data(&names.scale)?.to_vec();
        let gs_bytes = self.st.data(&names.global_scale)?;
        if gs_bytes.len() != 4 {
            return Err(ForgeError::Format(format!(
                "{}: expected one f32",
                names.global_scale
            )));
        }
        let global_scale = f32::from_le_bytes([gs_bytes[0], gs_bytes[1], gs_bytes[2], gs_bytes[3]]);
        Ok(Some(NvFp4Host {
            packed,
            scales,
            global_scale,
            rows,
            cols,
        }))
    }

    fn fetch_fp8(&self, name: &str) -> Result<Option<Fp8Host>> {
        if !self.fp8 {
            return Ok(None);
        }
        let Some(t) = self.st.tensor(name) else {
            return Ok(None);
        };
        if t.dtype != DType::F8E4M3 || t.shape.len() != 2 {
            return Ok(None);
        }
        let base = name.strip_suffix(".weight").unwrap_or(name);
        let scale_name = format!("{base}.weight_scale");
        let Some(scale_t) = self.st.tensor(&scale_name) else {
            return Err(ForgeError::Format(format!(
                "{name}: fp8 weight without {scale_name}"
            )));
        };
        let (rows, cols) = (t.shape[0], t.shape[1]);
        let scale_n = scale_t.numel();
        if scale_n != rows && scale_n != 1 {
            return Err(ForgeError::Format(format!(
                "{scale_name}: {scale_n} scales for {rows} rows (expect per-channel or per-tensor)"
            )));
        }
        let scale_bytes = self.st.data(&scale_name)?;
        let scales: Vec<f32> = match scale_t.dtype {
            DType::F32 => scale_bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            DType::BF16 => scale_bytes
                .chunks_exact(2)
                .map(|c| f32::from_bits(u32::from(u16::from_le_bytes([c[0], c[1]])) << 16))
                .collect(),
            DType::F16 => scale_bytes
                .chunks_exact(2)
                .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
                .collect(),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{scale_name}: scale dtype {other}"
                )))
            }
        };
        Ok(Some(Fp8Host {
            weight: self.st.data(name)?.to_vec(),
            scales,
            rows,
            cols,
        }))
    }
}

/// A weight matrix still on the host, in the exact byte layout the fused
/// kernels consume. Kept host-side long enough to row-concatenate sibling
/// projections (QKV, gate/up) before the single upload.
pub enum HostWeight {
    F16 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q8_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q6K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q3K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q2K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4_1 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5_1 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq4Nl {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq4Xs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Mxfp4 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2Xs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq3S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2Xxs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq3Xxs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq1S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq1M {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    NvFp4 {
        names: Vec<String>,
        packed: Vec<u8>,
        scales: Vec<u8>,
        global_scale: f32,
        rows: usize,
        cols: usize,
    },
    NvFp4Gguf {
        data: Vec<u8>,
        output_scale: f32,
        rows: usize,
        cols: usize,
    },
    /// Wagi FP8 E4M3 z jedną skalą na wiersz. DeepSeek V4 trzyma na dysku skalę
    /// kafelkową; loader przenosi ją na wiersze, wtapiając różnicę wykładników
    /// w same bajty (patrz `nvfp4::deepseek_fp8_to_row_scaled`).
    Fp8Row {
        data: Vec<u8>,
        scales: Vec<f32>,
        rows: usize,
        cols: usize,
    },
}

impl HostWeight {
    pub fn mtp_device_bytes(&self) -> Option<usize> {
        match self {
            HostWeight::Q8_0 { data, .. }
            | HostWeight::Q4K { data, .. }
            | HostWeight::NvFp4Gguf { data, .. } => Some(data.len()),
            _ => None,
        }
    }

    pub fn rows(&self) -> usize {
        match self {
            HostWeight::F16 { rows, .. }
            | HostWeight::Q8_0 { rows, .. }
            | HostWeight::Q4K { rows, .. }
            | HostWeight::Q6K { rows, .. }
            | HostWeight::Q5K { rows, .. }
            | HostWeight::Q3K { rows, .. }
            | HostWeight::Q2K { rows, .. }
            | HostWeight::Q4_0 { rows, .. }
            | HostWeight::Q4_1 { rows, .. }
            | HostWeight::Q5_0 { rows, .. }
            | HostWeight::Q5_1 { rows, .. }
            | HostWeight::Iq4Nl { rows, .. }
            | HostWeight::Iq4Xs { rows, .. }
            | HostWeight::Mxfp4 { rows, .. }
            | HostWeight::Iq2Xs { rows, .. }
            | HostWeight::Iq2S { rows, .. }
            | HostWeight::Iq3S { rows, .. }
            | HostWeight::Iq2Xxs { rows, .. }
            | HostWeight::Iq3Xxs { rows, .. }
            | HostWeight::Iq1S { rows, .. }
            | HostWeight::Iq1M { rows, .. }
            | HostWeight::NvFp4 { rows, .. }
            | HostWeight::NvFp4Gguf { rows, .. }
            | HostWeight::Fp8Row { rows, .. } => *rows,
        }
    }

    pub fn cols(&self) -> usize {
        match self {
            HostWeight::F16 { cols, .. }
            | HostWeight::Q8_0 { cols, .. }
            | HostWeight::Q4K { cols, .. }
            | HostWeight::Q6K { cols, .. }
            | HostWeight::Q5K { cols, .. }
            | HostWeight::Q3K { cols, .. }
            | HostWeight::Q2K { cols, .. }
            | HostWeight::Q4_0 { cols, .. }
            | HostWeight::Q4_1 { cols, .. }
            | HostWeight::Q5_0 { cols, .. }
            | HostWeight::Q5_1 { cols, .. }
            | HostWeight::Iq4Nl { cols, .. }
            | HostWeight::Iq4Xs { cols, .. }
            | HostWeight::Mxfp4 { cols, .. }
            | HostWeight::Iq2Xs { cols, .. }
            | HostWeight::Iq2S { cols, .. }
            | HostWeight::Iq3S { cols, .. }
            | HostWeight::Iq2Xxs { cols, .. }
            | HostWeight::Iq3Xxs { cols, .. }
            | HostWeight::Iq1S { cols, .. }
            | HostWeight::Iq1M { cols, .. }
            | HostWeight::NvFp4 { cols, .. }
            | HostWeight::NvFp4Gguf { cols, .. }
            | HostWeight::Fp8Row { cols, .. } => *cols,
        }
    }
}

impl HostWeight {
    /// Bufory tego formatu ułożone WIERSZOWO, wraz z krokiem wiersza w bajtach.
    ///
    /// Każdy format odpowiada tu za siebie, zamiast być wyliczany w cudzym
    /// `match` razem z dwudziestoma innymi. Tamten kształt kosztował: NVFP4
    /// compressed-tensors wpadał w gałąź „nieobsługiwane", bo trzyma wartości i
    /// skale w DWÓCH buforach i nie pasował do wzorca „jeden bufor".
    ///
    /// Zwraca liczbę wierszy i listę widoków; operacja wierszowa (dziś
    /// permutacja RoPE) przechodzi po wszystkich, każdy ze swoim krokiem.
    pub fn row_views_mut(&mut self) -> Result<(usize, Vec<(&mut Vec<u8>, usize)>)> {
        // Formaty jednobuforowe: krok wiersza wynika z rozmiaru bufora.
        macro_rules! single {
            ($data:expr, $rows:expr) => {{
                let rows = $rows;
                let data = $data;
                if rows == 0 || !data.len().is_multiple_of(rows) {
                    return Err(ForgeError::Format(
                        "rozmiar macierzy nie dzieli się na równe wiersze".into(),
                    ));
                }
                let row_bytes = data.len() / rows;
                Ok((rows, vec![(data, row_bytes)]))
            }};
        }
        match self {
            HostWeight::F16 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q8_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q6K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q3K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q2K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4_1 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5_1 { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq4Nl { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq4Xs { data, rows, .. } => single!(data, *rows),
            HostWeight::Mxfp4 { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2Xs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq3S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2Xxs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq3Xxs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq1S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq1M { data, rows, .. } => single!(data, *rows),
            HostWeight::NvFp4Gguf { data, rows, .. } => single!(data, *rows),
            // NVFP4 compressed-tensors: wartości i skale osobno. Wiersze są
            // niezależne w obu (bloki biegną wzdłuż kolumn), więc ta sama
            // permutacja nałożona na oba daje ten sam wynik co dla formatów
            // jednobuforowych — bez dekwantyzacji.
            HostWeight::NvFp4 {
                packed,
                scales,
                rows,
                cols,
                ..
            } => {
                let (rows, cols) = (*rows, *cols);
                Ok((rows, vec![(packed, cols / 2), (scales, cols / 16)]))
            }
            _ => Err(ForgeError::Unsupported(
                "ten format wag nie deklaruje układu wierszowego".into(),
            )),
        }
    }
}
