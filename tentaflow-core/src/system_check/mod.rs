// =============================================================================
// Plik: system_check/mod.rs
// Opis: Detekcja srodowiska maszyny — CUDA, Metal, Vulkan, Docker, RAM, GPU.
//       Uzywane przez wizard startowy GUI zeby pokazac userowi co moze odpalac
//       i co mu brakuje (np. "brak CUDA — vLLM niedostepny, llama-server CPU OK").
// =============================================================================

use serde::Serialize;
use std::process::Command;

/// Pelny snapshot moliwoci maszyny.
#[derive(Debug, Clone, Serialize)]
pub struct SystemCapabilities {
    pub platform: String,
    pub arch: String,
    pub cpu_features: CpuFeatures,
    pub memory: MemoryInfo,
    pub gpu: GpuSnapshot,
    pub runtimes: Runtimes,
    pub deploy_backends: Vec<DeployBackend>,
    /// Silniki ktore maszyna rzeczywiscie moze uruchomic, z uzasadnieniem.
    pub supported_engines: Vec<EngineSupport>,
    /// Ujednolicone sciezki — wszyskie modele leza w <tentaflow_home>/models/.
    pub paths: PathsSnapshot,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PathsSnapshot {
    pub tentaflow_home: String,
    /// Shared root between Docker and native deploys.
    pub models_root: String,
    /// HF_HOME / HUGGINGFACE_HUB_CACHE / TRANSFORMERS_CACHE value. HF creates
    /// `hub/models--*` under this automatically.
    pub hf_home: String,
    /// TORCH_HOME value (subdir of models_root so HF's and torch's `hub/`
    /// directories do not collide).
    pub torch_home: String,
    /// Path inside a Docker container that models_root is mounted to.
    pub container_models_path: String,
}

fn collect_paths() -> PathsSnapshot {
    let _ = crate::paths::ensure_models_dirs();
    PathsSnapshot {
        tentaflow_home:         crate::paths::tentaflow_home().display().to_string(),
        models_root:            crate::paths::models_root().display().to_string(),
        hf_home:                crate::paths::hf_home().display().to_string(),
        torch_home:             crate::paths::torch_home().display().to_string(),
        container_models_path:  crate::paths::CONTAINER_MODELS_PATH.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CpuFeatures {
    pub logical_cores: usize,
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryInfo {
    pub total_mb: u64,
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GpuSnapshot {
    pub nvidia: Vec<NvidiaGpu>,
    pub amd: Vec<AmdGpu>,
    pub intel: Vec<IntelGpu>,
    pub metal_available: bool,
    pub vulkan_available: bool,
    /// Prefferowany backend do deployu (wygrywa karta z najwieksza VRAM).
    pub preferred_backend: GpuBackend,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    Cuda,
    Rocm,
    Xpu,
    Metal,
    #[default]
    Cpu,
}

#[derive(Debug, Clone, Serialize)]
pub struct AmdGpu {
    pub index: u32,
    pub name: String,
    pub vram_mb: u64,
    pub rocm_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntelGpu {
    pub name: String,
    pub oneapi_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NvidiaGpu {
    pub index: u32,
    pub name: String,
    pub vram_mb: u64,
    pub compute_capability: Option<String>,
    pub driver_version: Option<String>,
    pub cuda_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Runtimes {
    pub docker: Option<String>,
    pub podman: Option<String>,
    pub python: Option<String>,
    pub cuda_toolkit: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum DeployBackend {
    Docker,
    Native,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineSupport {
    pub engine: String,
    pub category: String,
    pub available: bool,
    pub reason: String,
    pub requires: Vec<String>,
    pub backends: Vec<DeployBackend>,
}

/// Glowny entrypoint — zbiera wszystkie informacje synchronicznie.
/// Nie powinno blokowac dluzej niz ~500ms na rozsadnej maszynie.
pub fn collect() -> SystemCapabilities {
    let platform = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();

    let cpu_features = detect_cpu_features();
    let memory = detect_memory();
    let gpu = detect_gpu();
    let runtimes = detect_runtimes();

    let mut deploy_backends = vec![DeployBackend::Native];
    if runtimes.docker.is_some() || runtimes.podman.is_some() {
        deploy_backends.push(DeployBackend::Docker);
    }

    let supported_engines = build_engine_support(&gpu, &runtimes, &deploy_backends);

    SystemCapabilities {
        platform,
        arch,
        cpu_features,
        memory,
        gpu,
        runtimes,
        deploy_backends,
        supported_engines,
        paths: collect_paths(),
    }
}

fn detect_cpu_features() -> CpuFeatures {
    let mut out = CpuFeatures {
        logical_cores: num_cpus::get(),
        ..Default::default()
    };
    #[cfg(target_arch = "x86_64")]
    {
        out.avx2 = is_x86_feature_detected!("avx2");
        out.avx512 = is_x86_feature_detected!("avx512f");
    }
    #[cfg(target_arch = "aarch64")]
    {
        out.neon = std::arch::is_aarch64_feature_detected!("neon");
    }
    out
}

fn detect_memory() -> MemoryInfo {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    MemoryInfo {
        total_mb: sys.total_memory() / 1024 / 1024,
        available_mb: sys.available_memory() / 1024 / 1024,
    }
}

fn detect_gpu() -> GpuSnapshot {
    let nvidia = detect_nvidia();
    let amd = detect_amd_rocm();
    let intel = detect_intel_xpu();
    let metal_available = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");
    let vulkan_available = detect_vulkan();

    let preferred_backend = if !nvidia.is_empty() {
        GpuBackend::Cuda
    } else if !amd.is_empty() {
        GpuBackend::Rocm
    } else if metal_available {
        GpuBackend::Metal
    } else if !intel.is_empty() {
        GpuBackend::Xpu
    } else {
        GpuBackend::Cpu
    };

    GpuSnapshot {
        nvidia, amd, intel,
        metal_available, vulkan_available,
        preferred_backend,
    }
}

/// Wykrywa karty AMD z ROCm 7+. Uzywamy `rocminfo` jesli jest, inaczej
/// nic nie zwraca — ROCm wymaga tego binary do detekcji kart.
fn detect_amd_rocm() -> Vec<AmdGpu> {
    let Ok(out) = Command::new("rocminfo").output() else { return Vec::new(); };
    if !out.status.success() { return Vec::new(); }
    let text = String::from_utf8_lossy(&out.stdout);

    let rocm_version = detect_rocm_version();
    let mut gpus = Vec::new();
    let mut idx = 0;
    // Bardzo prosty parser — rocminfo zwraca "Name: <agent>" per agent
    // i "Device Type: GPU" w sekcji. Dla hostu-CPU skip.
    let mut current_name: Option<String> = None;
    let mut is_gpu = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Name:") {
            current_name = Some(t.trim_start_matches("Name:").trim().to_string());
            is_gpu = false;
        } else if t.starts_with("Device Type:") && t.contains("GPU") {
            is_gpu = true;
        } else if t.starts_with("Marketing Name:") && is_gpu {
            let name = t.trim_start_matches("Marketing Name:").trim().to_string();
            gpus.push(AmdGpu {
                index: idx,
                name: if name.is_empty() { current_name.clone().unwrap_or_default() } else { name },
                vram_mb: 0, // rocminfo nie raportuje bezposrednio; live metryki z rocm-smi
                rocm_version: rocm_version.clone(),
            });
            idx += 1;
        }
    }
    gpus
}

fn detect_rocm_version() -> Option<String> {
    // /opt/rocm/.info/version, /opt/rocm/bin/rocm-smi --showversion, lub ENV ROCM_VERSION
    if let Ok(v) = std::fs::read_to_string("/opt/rocm/.info/version") {
        return Some(v.trim().to_string());
    }
    let out = Command::new("rocm-smi").arg("--showversion").output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("ROCm version:") {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// Wykrywa Intel Arc / iGPU przez `sycl-ls` (cz. oneAPI).
fn detect_intel_xpu() -> Vec<IntelGpu> {
    let Ok(out) = Command::new("sycl-ls").output() else { return Vec::new(); };
    if !out.status.success() { return Vec::new(); }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut gpus = Vec::new();
    let oneapi_version = detect_oneapi_version();
    for line in text.lines() {
        let t = line.trim();
        if t.contains("level_zero:gpu") || t.contains("opencl:gpu") {
            // format: "[ext_oneapi_level_zero:gpu:0] Intel(R) Arc(TM) A770 ..."
            if let Some(name) = t.rsplit_once("] ").map(|(_, n)| n.to_string()) {
                gpus.push(IntelGpu { name, oneapi_version: oneapi_version.clone() });
            }
        }
    }
    gpus
}

fn detect_oneapi_version() -> Option<String> {
    run_version(["icx", "--version"])
        .or_else(|| std::env::var("ONEAPI_ROOT").ok())
}

fn detect_nvidia() -> Vec<NvidiaGpu> {
    let output = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total,compute_cap,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let cuda_version = detect_cuda_runtime_version();

    stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if parts.len() < 5 {
                return None;
            }
            Some(NvidiaGpu {
                index: parts[0].parse().unwrap_or(0),
                name: parts[1].to_string(),
                vram_mb: parts[2].parse().unwrap_or(0),
                compute_capability: Some(parts[3].to_string()),
                driver_version: Some(parts[4].to_string()),
                cuda_version: cuda_version.clone(),
            })
        })
        .collect()
}

fn detect_cuda_runtime_version() -> Option<String> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=cuda_version", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "N/A" { None } else { Some(s) }
}

fn detect_vulkan() -> bool {
    Command::new("vulkaninfo")
        .arg("--summary")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn detect_runtimes() -> Runtimes {
    Runtimes {
        docker: run_version(["docker", "--version"]),
        podman: run_version(["podman", "--version"]),
        python: run_version(["python3", "--version"]).or_else(|| run_version(["python", "--version"])),
        cuda_toolkit: run_version(["nvcc", "--version"]),
    }
}

fn run_version<const N: usize>(argv: [&str; N]) -> Option<String> {
    let out = Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s.lines().next().unwrap_or(&s).to_string())
    }
}

/// Katalog wspieranych silnikow — ta sama lista co w tentaflow-containers/
/// + flagi wymagan. `build_engine_support` zaznacza dostepnosc wg wykrytego
/// srodowiska.
fn build_engine_support(
    gpu: &GpuSnapshot,
    runtimes: &Runtimes,
    backends: &[DeployBackend],
) -> Vec<EngineSupport> {
    let has_cuda = !gpu.nvidia.is_empty() && runtimes.cuda_toolkit.is_some();
    let has_rocm = !gpu.amd.is_empty();
    let has_xpu = !gpu.intel.is_empty();
    let has_metal = gpu.metal_available;
    let has_any_gpu = has_cuda || has_rocm || has_xpu || has_metal;
    let has_docker = backends.contains(&DeployBackend::Docker);
    let _has_python = runtimes.python.is_some();

    // Helper text pokazujacy ktory backend wygrywa
    let gpu_reason = || -> String {
        if has_cuda { "CUDA GPU".into() }
        else if has_rocm { "AMD ROCm GPU".into() }
        else if has_metal { "Apple Metal".into() }
        else if has_xpu { "Intel XPU".into() }
        else { "tylko CPU".into() }
    };
    let _ = gpu_reason;
    let _ = has_any_gpu;

    let mut out = Vec::new();

    // LLM
    out.push(engine(
        "llm-llamacpp", "llm",
        true, // CPU fallback zawsze dziala
        if has_cuda { "CUDA GPU wykryte" }
        else if has_metal { "Metal dostepny" }
        else if gpu.vulkan_available { "Vulkan dostepny" }
        else { "CPU fallback" },
        &["llama.cpp-server (natywna binarka)"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));
    // vLLM: CUDA, ROCm 7+, Metal (vllm-metal plugin) — nie CPU
    out.push(engine(
        "llm-vllm", "llm", has_cuda || has_rocm || has_metal,
        if has_cuda { "CUDA OK" }
        else if has_rocm { "AMD ROCm OK" }
        else if has_metal { "Metal (vllm-metal)" }
        else { "wymaga GPU (CUDA/ROCm/Metal) — dla CPU uzyj llama.cpp" },
        &["CUDA / ROCm 7 / Metal"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));
    // SGLang: CUDA lub ROCm
    out.push(engine(
        "llm-sglang", "llm", has_cuda || has_rocm,
        if has_cuda { "CUDA OK" }
        else if has_rocm { "AMD ROCm" }
        else { "wymaga CUDA/ROCm — dla CPU uzyj llama.cpp" },
        &["CUDA / ROCm 7"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));
    out.push(engine(
        "llm-ollama", "llm", has_docker || true,
        "Ollama ma wlasna natywna binarke",
        &["Docker lub Ollama binarka"],
        &[DeployBackend::Docker, DeployBackend::Native],
    ));

    // STT
    out.push(engine(
        "stt-whisper", "stt", true,
        if has_cuda { "CUDA OK" } else if has_metal { "Metal OK" } else { "CPU fallback" },
        &["whisper.cpp-server"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));
    out.push(engine(
        "stt-parakeet", "stt", has_cuda || has_rocm,
        if has_cuda { "CUDA OK (NeMo)" }
        else if has_rocm { "ROCm exp. (NeMo)" }
        else { "wymaga CUDA (NeMo)" },
        &["CUDA / ROCm 7 (NeMo)"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));
    out.push(engine(
        "stt-qwen-asr", "stt", has_cuda || has_rocm || has_metal,
        if has_cuda { "CUDA + flash-attn" }
        else if has_rocm { "AMD ROCm" }
        else if has_metal { "Metal (MPS)" }
        else { "wymaga GPU" },
        &["CUDA / ROCm / Metal + transformers"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));

    // TTS
    out.push(engine(
        "tts-sherpa", "tts", true,
        "sherpa-onnx dziala na CPU",
        &["sherpa-onnx (natywna binarka)"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));
    out.push(engine(
        "tts-xtts", "tts", has_cuda || has_rocm || has_metal,
        if has_cuda { "CUDA" } else if has_rocm { "ROCm" } else if has_metal { "Metal MPS" } else { "wymaga GPU" },
        &["CUDA / ROCm / Metal (coqui-TTS)"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));
    out.push(engine(
        "tts-voxcpm", "tts", has_cuda || has_rocm || has_metal,
        if has_cuda { "CUDA" } else if has_rocm { "ROCm" } else if has_metal { "Metal" } else { "wymaga GPU" },
        &["CUDA / ROCm / Metal (VoxCPM)"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));

    // Embeddings / Reranker (TEI, Rust+Candle — natywna binarka)
    out.push(engine(
        "embeddings", "embeddings", true,
        if has_cuda { "CUDA + Candle" } else if has_metal { "Metal + Candle" } else { "CPU Candle" },
        &["text-embeddings-router (natywna binarka)"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));
    out.push(engine(
        "reranker", "reranker", true,
        if has_cuda { "CUDA + Candle" } else if has_metal { "Metal + Candle" } else { "CPU" },
        &["text-embeddings-router"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));

    // Image
    out.push(engine(
        "comfyui", "image", has_cuda || has_rocm || has_metal,
        if has_cuda { "CUDA" } else if has_rocm { "ROCm" } else if has_metal { "Metal" } else { "wymaga GPU" },
        &["CUDA / ROCm / Metal (ComfyUI)"],
        if has_docker { &[DeployBackend::Native, DeployBackend::Docker][..] } else { &[DeployBackend::Native][..] },
    ));

    out
}

fn engine(
    name: &str,
    category: &str,
    available: bool,
    reason: &str,
    requires: &[&str],
    backends: &[DeployBackend],
) -> EngineSupport {
    EngineSupport {
        engine: name.to_string(),
        category: category.to_string(),
        available,
        reason: reason.to_string(),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        backends: backends.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_runs_without_panic() {
        let caps = collect();
        assert!(!caps.platform.is_empty());
        assert!(!caps.arch.is_empty());
        assert!(!caps.supported_engines.is_empty());
    }

    #[test]
    fn llama_is_always_marked_available() {
        // llama-server ma CPU fallback wiec powinien byc OK wszedzie
        let caps = collect();
        let llm = caps.supported_engines.iter().find(|e| e.engine == "llm-llamacpp").unwrap();
        assert!(llm.available);
    }

    #[test]
    fn vllm_requires_any_gpu_but_not_docker() {
        let caps = collect();
        let vllm = caps.supported_engines.iter().find(|e| e.engine == "llm-vllm").unwrap();
        let has_cuda = !caps.gpu.nvidia.is_empty() && caps.runtimes.cuda_toolkit.is_some();
        let has_rocm = !caps.gpu.amd.is_empty();
        let has_metal = caps.gpu.metal_available;
        assert_eq!(vllm.available, has_cuda || has_rocm || has_metal);
    }

    #[test]
    fn preferred_backend_matches_detected_gpu() {
        let caps = collect();
        use super::GpuBackend::*;
        let expected = if !caps.gpu.nvidia.is_empty() { Cuda }
            else if !caps.gpu.amd.is_empty() { Rocm }
            else if caps.gpu.metal_available { Metal }
            else if !caps.gpu.intel.is_empty() { Xpu }
            else { Cpu };
        assert_eq!(caps.gpu.preferred_backend, expected);
    }
}
