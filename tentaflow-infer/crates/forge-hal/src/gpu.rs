// ===== File: gpu.rs — forge-hal: GPU backend selection (which compiled backend owns this host) =====
//
// Warstwy wyżej trzymają `Arc<dyn Device>` i nie wiedzą, czy pod spodem jest
// CUDA czy HIP. Ten moduł jest jedynym miejscem, które o tym decyduje: pyta
// sterowniki skompilowanych backendów o urządzenie i zwraca pierwszy, który
// odpowiada. `FORGE_DEVICE=cuda|hip` przypina wybór, żeby maszyna z dwiema
// kartami różnych producentów była powtarzalna w benchmarkach.

use std::sync::Arc;

use forge_types::{ForgeError, Result};

use crate::{Device, PoolSizes};

/// Backend GPU wybrany dla tego procesu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cuda,
    Hip,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Hip => "hip",
        }
    }
}

/// Kolejność prób przy autodetekcji. Ustalona, nie zależna od kolejności cech
/// w Cargo, więc host z obiema kartami zawsze wybiera to samo.
const PROBE_ORDER: &[Backend] = &[
    #[cfg(feature = "cuda")]
    Backend::Cuda,
    #[cfg(feature = "hip")]
    Backend::Hip,
];

fn pinned() -> Result<Option<Backend>> {
    let Ok(name) = std::env::var("FORGE_DEVICE") else {
        return Ok(None);
    };
    let requested = match name.trim().to_ascii_lowercase().as_str() {
        "cuda" => Backend::Cuda,
        "hip" | "rocm" | "amd" => Backend::Hip,
        other => {
            return Err(ForgeError::Device(format!(
                "FORGE_DEVICE={other} — dozwolone: cuda, hip"
            )))
        }
    };
    if !PROBE_ORDER.contains(&requested) {
        return Err(ForgeError::Device(format!(
            "FORGE_DEVICE={} — ten backend nie jest wkompilowany (dostępne: {})",
            requested.as_str(),
            compiled_backends()
        )));
    }
    Ok(Some(requested))
}

fn compiled_backends() -> String {
    if PROBE_ORDER.is_empty() {
        return String::from("brak");
    }
    PROBE_ORDER
        .iter()
        .map(|b| b.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Backend, który obsłuży `ordinal`. Autodetekcja pyta sterowniki o wolny VRAM,
/// bo to najtańsze wywołanie, które i tak musi się udać, żeby cokolwiek działało.
pub fn backend(ordinal: usize) -> Result<Backend> {
    if let Some(requested) = pinned()? {
        return Ok(requested);
    }
    let mut failures = Vec::new();
    for candidate in PROBE_ORDER {
        match free_vram_on(*candidate, ordinal) {
            Ok(_) => return Ok(*candidate),
            Err(err) => failures.push(format!("{}: {err}", candidate.as_str())),
        }
    }
    Err(ForgeError::Device(format!(
        "żaden wkompilowany backend GPU nie widzi urządzenia {ordinal} ({})",
        if failures.is_empty() {
            String::from("brak wkompilowanych backendów")
        } else {
            failures.join("; ")
        }
    )))
}

fn free_vram_on(backend: Backend, ordinal: usize) -> Result<usize> {
    match backend {
        #[cfg(feature = "cuda")]
        Backend::Cuda => crate::cuda::CudaDevice::free_vram(ordinal),
        #[cfg(feature = "hip")]
        Backend::Hip => crate::hip::HipDevice::free_vram(ordinal),
        #[allow(unreachable_patterns)]
        other => Err(ForgeError::Device(format!(
            "backend {} nie jest wkompilowany",
            other.as_str()
        ))),
    }
}

/// Wolny VRAM na urządzeniu `ordinal` — do wymiarowania pul przed ich zajęciem.
pub fn free_vram(ordinal: usize) -> Result<usize> {
    free_vram_on(backend(ordinal)?, ordinal)
}

/// Otwiera urządzenie `ordinal` z podanymi budżetami pul.
pub fn open(ordinal: usize, pools: PoolSizes) -> Result<Arc<dyn Device>> {
    match backend(ordinal)? {
        #[cfg(feature = "cuda")]
        Backend::Cuda => Ok(crate::cuda::CudaDevice::new(ordinal, pools)?),
        #[cfg(feature = "hip")]
        Backend::Hip => Ok(crate::hip::HipDevice::new(ordinal, pools)?),
        #[allow(unreachable_patterns)]
        other => Err(ForgeError::Device(format!(
            "backend {} nie jest wkompilowany",
            other.as_str()
        ))),
    }
}

/// Otwiera urządzenie z domyślnym budżetem (90% wolnego VRAM).
pub fn open_default_pools(ordinal: usize) -> Result<Arc<dyn Device>> {
    match backend(ordinal)? {
        #[cfg(feature = "cuda")]
        Backend::Cuda => Ok(crate::cuda::CudaDevice::with_default_pools(ordinal)?),
        #[cfg(feature = "hip")]
        Backend::Hip => Ok(crate::hip::HipDevice::with_default_pools(ordinal)?),
        #[allow(unreachable_patterns)]
        other => Err(ForgeError::Device(format!(
            "backend {} nie jest wkompilowany",
            other.as_str()
        ))),
    }
}
