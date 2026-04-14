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
    pub metal_available: bool,
    pub vulkan_available: bool,
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
    GpuSnapshot {
        nvidia: detect_nvidia(),
        metal_available: cfg!(target_os = "macos"),
        vulkan_available: detect_vulkan(),
    }
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
    let has_metal = gpu.metal_available;
    let has_docker = backends.contains(&DeployBackend::Docker);
    let has_python = runtimes.python.is_some();

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
    out.push(engine(
        "llm-vllm", "llm", has_docker && has_cuda,
        if !has_docker { "wymaga Dockera" } else if !has_cuda { "wymaga CUDA" } else { "OK" },
        &["Docker", "NVIDIA GPU + CUDA"],
        &[DeployBackend::Docker],
    ));
    out.push(engine(
        "llm-sglang", "llm", has_docker && has_cuda,
        if !has_docker { "wymaga Dockera" } else if !has_cuda { "wymaga CUDA" } else { "OK" },
        &["Docker", "NVIDIA GPU + CUDA"],
        &[DeployBackend::Docker],
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
        "stt-parakeet", "stt", has_docker && has_cuda,
        if !has_docker { "wymaga Dockera (NeMo)" } else if !has_cuda { "wymaga CUDA" } else { "OK" },
        &["Docker", "NVIDIA + CUDA (NeMo)"],
        &[DeployBackend::Docker],
    ));
    out.push(engine(
        "stt-qwen-asr", "stt", has_docker && has_cuda,
        if !has_docker { "wymaga Dockera" } else if !has_cuda { "wymaga CUDA" } else { "OK" },
        &["Docker", "CUDA", "transformers"],
        &[DeployBackend::Docker],
    ));

    // TTS
    out.push(engine(
        "tts-sherpa", "tts", true,
        "sherpa-onnx dziala na CPU",
        &["sherpa-onnx (natywna binarka)"],
        &[DeployBackend::Native, DeployBackend::Docker],
    ));
    out.push(engine(
        "tts-xtts", "tts", has_docker || has_python,
        if !has_docker && !has_python { "wymaga Dockera lub Pythona" } else { "OK" },
        &["Docker lub Python 3.10+ + venv"],
        &[DeployBackend::Docker],
    ));
    out.push(engine(
        "tts-voxcpm", "tts", has_docker || has_python,
        if !has_docker && !has_python { "wymaga Dockera lub Pythona" } else { "OK" },
        &["Docker lub Python + transformers"],
        &[DeployBackend::Docker],
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
        "comfyui", "image", has_docker,
        if has_docker { "OK" } else { "wymaga Dockera (ComfyUI + Python)" },
        &["Docker"],
        &[DeployBackend::Docker],
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
    fn vllm_requires_docker_and_cuda() {
        let caps = collect();
        let vllm = caps.supported_engines.iter().find(|e| e.engine == "llm-vllm").unwrap();
        let has_docker = caps.runtimes.docker.is_some() || caps.runtimes.podman.is_some();
        let has_cuda = !caps.gpu.nvidia.is_empty() && caps.runtimes.cuda_toolkit.is_some();
        assert_eq!(vllm.available, has_docker && has_cuda);
    }
}
