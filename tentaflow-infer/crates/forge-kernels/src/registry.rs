// ===== File: registry.rs — PTX artifact loading (embedded defaults + dir override) =====

use std::collections::HashMap;
use std::path::Path;

use forge_hal::{Device, KernelHandle, Module};
use forge_types::{ForgeError, Result};
use serde::Deserialize;

/// Parsed kernels/build/<arch>/manifest.json.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub arch: String,
    pub kernels: HashMap<String, ManifestEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub entry: String,
}

/// One embedded (name, ptx) pair for the default artifact set. The list is
/// kept in lockstep with build_kernels.mojo registrations; a mismatch against
/// the manifest fails loudly at load time rather than at first launch.
struct EmbeddedArtifact {
    name: &'static str,
    ptx: &'static [u8],
}

macro_rules! embedded {
    ($($name:literal),+ $(,)?) => {
        &[$(EmbeddedArtifact {
            name: $name,
            ptx: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../kernels/mojo/build/sm_89/",
                $name,
                ".ptx"
            )),
        }),+]
    };
}

const EMBEDDED_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/sm_89/manifest.json"
));

const EMBEDDED_SM89: &[EmbeddedArtifact] = embedded![
    "rmsnorm_f16",
    "rmsnorm_residual_f16",
    "silu_mul_f16",
    "rope_neox_f16",
    "gemv_q8_0_f16",
    "gemv_f16",
    "attn_decode_f16_hd64",
    "attn_decode_f16_hd128",
    "gemv_nvfp4_f16",
    "gather_rows_f16",
    "gemv_f16_out_f32",
    "gemv_q8_0_out_f32",
    "layernorm_f16",
    "layernorm_residual_f16",
    "gelu_f16",
    "conv1d_k3_f16",
    "attn_full_f16_hd64",
    "attn_full_f16_hd128",
    "gemv_f16_bias",
    "kv_append_f16",
];

/// Loaded modules + resolved kernel handles for one device.
pub struct KernelArtifacts {
    handles: HashMap<String, KernelHandle>,
    arch: String,
}

impl KernelArtifacts {
    /// Load kernel artifacts for `device`. `FORGE_KERNEL_DIR` (pointing at
    /// kernels/mojo/build) overrides the embedded set for development
    /// iteration without a Rust rebuild.
    pub fn load(device: &dyn Device) -> Result<Self> {
        let arch = device.caps().arch.clone();
        if let Ok(dir) = std::env::var("FORGE_KERNEL_DIR") {
            return Self::load_dir(device, Path::new(&dir), &arch);
        }
        Self::load_embedded(device, &arch)
    }

    fn load_embedded(device: &dyn Device, arch: &str) -> Result<Self> {
        // Embedded artifacts are compiled for sm_89; PTX is forward-compatible
        // within a major architecture via driver JIT, so newer sm_8x/9x parts
        // still load them. Older parts must supply FORGE_KERNEL_DIR.
        let manifest: Manifest = serde_json::from_str(EMBEDDED_MANIFEST)
            .map_err(|e| ForgeError::Kernel(format!("embedded manifest parse: {e}")))?;
        let mut handles = HashMap::new();
        for art in EMBEDDED_SM89 {
            let entry = manifest.kernels.get(art.name).ok_or_else(|| {
                ForgeError::Kernel(format!("kernel {} missing from embedded manifest", art.name))
            })?;
            let module = device.load_module(art.ptx)?;
            handles.insert(art.name.to_string(), module.kernel(&entry.entry)?);
        }
        // The reverse direction: manifest entries with no embedded bytes mean
        // build_kernels.mojo and this crate went out of sync.
        for name in manifest.kernels.keys() {
            if !EMBEDDED_SM89.iter().any(|a| a.name == name) {
                return Err(ForgeError::Kernel(format!(
                    "manifest kernel {name} not embedded — update forge-kernels EMBEDDED_SM89"
                )));
            }
        }
        Ok(Self { handles, arch: arch.to_string() })
    }

    fn load_dir(device: &dyn Device, dir: &Path, arch: &str) -> Result<Self> {
        let arch_dir = dir.join(arch);
        let manifest_path = arch_dir.join("manifest.json");
        let manifest_src = std::fs::read_to_string(&manifest_path).map_err(|e| {
            ForgeError::Kernel(format!("read {}: {e}", manifest_path.display()))
        })?;
        let manifest: Manifest = serde_json::from_str(&manifest_src)
            .map_err(|e| ForgeError::Kernel(format!("manifest parse: {e}")))?;
        let mut handles = HashMap::new();
        for (name, entry) in &manifest.kernels {
            let ptx = std::fs::read(arch_dir.join(&entry.file)).map_err(|e| {
                ForgeError::Kernel(format!("read {}: {e}", entry.file))
            })?;
            let module: Module = device.load_module(&ptx)?;
            handles.insert(name.clone(), module.kernel(&entry.entry)?);
        }
        Ok(Self { handles, arch: arch.to_string() })
    }

    pub fn arch(&self) -> &str {
        &self.arch
    }

    pub fn get(&self, name: &str) -> Result<&KernelHandle> {
        self.handles
            .get(name)
            .ok_or_else(|| ForgeError::Kernel(format!("kernel not loaded: {name}")))
    }
}
