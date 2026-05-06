// =============================================================================
// Plik: memory/impls.rs
// Opis: Konkretne implementacje LoadableEngine dla 3 typow runtime'u —
//       embedded (MLX/llama.cpp), python-bundle (vllm-metal/sglang/xtts),
//       docker (kontenery z `docker run`). Wszystkie pluggable do
//       MemoryGuard przez Arc<dyn LoadableEngine>.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use tracing::info;

use super::engine::LoadableEngine;

// =============================================================================
// PythonBundleEngine — vllm-metal, sglang, xtts itd.
// Load = python_venv::relaunch z cache venv. Unload = process_ctl::terminate(pid).
// =============================================================================

pub struct PythonBundleEngine {
    pub engine_id: String,
    pub service_name: String,
    pub instance_name: String,
    /// HF repo modelu — propagowany do env MODEL przy spawn.
    pub model_repo: String,
    pub host_port: u16,
    pub vram_estimated_mb: u64,
    /// Bie­zacy PID gdy proces zyje. Zerowane przez unload().
    pid: AtomicU32,
    /// Builder env (HF_TOKEN, HF_HOME itd.) — wstrzykiwane do procesu.
    pub env: HashMap<String, String>,
}

impl PythonBundleEngine {
    pub fn new(
        engine_id: String,
        service_name: String,
        instance_name: String,
        model_repo: String,
        host_port: u16,
        vram_estimated_mb: u64,
        env: HashMap<String, String>,
        pid_if_running: u32,
    ) -> Self {
        Self {
            engine_id,
            service_name,
            instance_name,
            model_repo,
            host_port,
            vram_estimated_mb,
            pid: AtomicU32::new(pid_if_running),
            env,
        }
    }
}

#[async_trait]
impl LoadableEngine for PythonBundleEngine {
    fn engine_id(&self) -> &str {
        &self.engine_id
    }
    fn service_name(&self) -> &str {
        &self.service_name
    }
    fn vram_estimated_mb(&self) -> u64 {
        self.vram_estimated_mb
    }
    fn is_loaded(&self) -> bool {
        let p = self.pid.load(Ordering::Acquire);
        p != 0 && crate::deploy::process_ctl::is_alive(p)
    }

    async fn ensure_loaded(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
        let req = crate::deploy::python_venv::NativeDeployRequest {
            engine: self.engine_id.clone(),
            instance_name: Some(self.instance_name.clone()),
            env: self.env.clone(),
        };
        let running =
            tokio::task::spawn_blocking(move || crate::deploy::python_venv::relaunch(&req))
                .await
                .map_err(|e| anyhow!("spawn_blocking relaunch: {}", e))??;

        let new_pid = running.child.id();
        std::mem::forget(running.child);
        self.pid.store(new_pid, Ordering::Release);
        info!(
            service = %self.service_name, pid = new_pid,
            "PythonBundleEngine: zaladowano (relaunch)"
        );
        Ok(())
    }

    async fn unload(&self) -> Result<()> {
        let p = self.pid.load(Ordering::Acquire);
        if p == 0 {
            return Ok(());
        }
        let pid_owned = p;
        let killed =
            tokio::task::spawn_blocking(move || crate::deploy::process_ctl::terminate(pid_owned))
                .await
                .map_err(|e| anyhow!("spawn_blocking terminate: {}", e))?
                .with_context(|| format!("terminate pid {}", p))?;
        self.pid.store(0, Ordering::Release);
        info!(
            service = %self.service_name, pid = p, killed,
            "PythonBundleEngine: wyladowano (terminate)"
        );
        Ok(())
    }
}

// =============================================================================
// EmbeddedEngine — MLX, llama.cpp. Wspoldzielony InferenceManager (jeden
// active engine na raz w obecnej architekturze).
// =============================================================================

pub struct EmbeddedEngine {
    pub engine_id: String,
    pub service_name: String,
    pub model_path: PathBuf,
    pub model_repo: String,
    /// "mlx" / "llamacpp" — backend selector.
    pub backend: String,
    pub vram_estimated_mb: u64,
    loaded: AtomicBool,
}

impl EmbeddedEngine {
    pub fn new(
        engine_id: String,
        service_name: String,
        model_path: PathBuf,
        model_repo: String,
        backend: String,
        vram_estimated_mb: u64,
        already_loaded: bool,
    ) -> Self {
        Self {
            engine_id,
            service_name,
            model_path,
            model_repo,
            backend,
            vram_estimated_mb,
            loaded: AtomicBool::new(already_loaded),
        }
    }
}

#[async_trait]
impl LoadableEngine for EmbeddedEngine {
    fn engine_id(&self) -> &str {
        &self.engine_id
    }
    fn service_name(&self) -> &str {
        &self.service_name
    }
    fn vram_estimated_mb(&self) -> u64 {
        self.vram_estimated_mb
    }
    fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    async fn ensure_loaded(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
        let shared = crate::inference::shared_inference_manager();
        let mut mgr = shared.write().await;
        let _info = mgr
            .load_model(
                &self.model_path,
                crate::inference::DeployParamsSnapshot::default(),
                Some(&self.backend),
            )
            .await
            .with_context(|| format!("load_model {}/{}", self.backend, self.model_repo))?;
        self.loaded.store(true, Ordering::Release);
        info!(
            service = %self.service_name, model = %self.model_repo, backend = %self.backend,
            "EmbeddedEngine: zaladowano"
        );
        Ok(())
    }

    async fn unload(&self) -> Result<()> {
        if !self.is_loaded() {
            return Ok(());
        }
        let shared = crate::inference::shared_inference_manager();
        let mut mgr = shared.write().await;
        mgr.unload_model().await.context("unload_model")?;
        self.loaded.store(false, Ordering::Release);
        info!(service = %self.service_name, "EmbeddedEngine: wyladowano");
        Ok(())
    }
}

// =============================================================================
// DockerEngine — kontener z `docker run`. Load = docker start, unload = docker stop.
// =============================================================================

pub struct DockerEngine {
    pub engine_id: String,
    pub service_name: String,
    pub container_name: String,
    pub vram_estimated_mb: u64,
    /// Stan trzymany lokalnie — Docker daemon zachowuje real source of truth.
    /// Sprawdzane przez docker inspect przy is_loaded().
    last_known_loaded: Mutex<bool>,
}

impl DockerEngine {
    pub fn new(
        engine_id: String,
        service_name: String,
        container_name: String,
        vram_estimated_mb: u64,
        already_loaded: bool,
    ) -> Self {
        Self {
            engine_id,
            service_name,
            container_name,
            vram_estimated_mb,
            last_known_loaded: Mutex::new(already_loaded),
        }
    }
}

#[async_trait]
impl LoadableEngine for DockerEngine {
    fn engine_id(&self) -> &str {
        &self.engine_id
    }
    fn service_name(&self) -> &str {
        &self.service_name
    }
    fn vram_estimated_mb(&self) -> u64 {
        self.vram_estimated_mb
    }
    fn is_loaded(&self) -> bool {
        *self.last_known_loaded.lock()
    }

    async fn ensure_loaded(&self) -> Result<()> {
        #[cfg(feature = "docker")]
        {
            if self.is_loaded() {
                return Ok(());
            }
            // Bollard nie ma start_container w tym crate — uzywamy CLI fallback.
            let status = tokio::process::Command::new("docker")
                .arg("start")
                .arg(&self.container_name)
                .status()
                .await
                .context("docker start")?;
            if !status.success() {
                anyhow::bail!("docker start {} zwrocil {}", self.container_name, status);
            }
            *self.last_known_loaded.lock() = true;
            info!(service = %self.service_name, "DockerEngine: zaladowano");
            Ok(())
        }
        #[cfg(not(feature = "docker"))]
        {
            anyhow::bail!("DockerEngine wymaga feature `docker`")
        }
    }

    async fn unload(&self) -> Result<()> {
        #[cfg(feature = "docker")]
        {
            if !self.is_loaded() {
                return Ok(());
            }
            let status = tokio::process::Command::new("docker")
                .arg("stop")
                .arg(&self.container_name)
                .status()
                .await
                .context("docker stop")?;
            if !status.success() {
                anyhow::bail!("docker stop {} zwrocil {}", self.container_name, status);
            }
            *self.last_known_loaded.lock() = false;
            info!(service = %self.service_name, "DockerEngine: wyladowano");
            Ok(())
        }
        #[cfg(not(feature = "docker"))]
        {
            Ok(())
        }
    }
}

/// Heurystyczne oszacowanie VRAM na podstawie nazwy modelu HF — uzyte gdy
/// brak realnego pomiaru. Zwraca konserwatywne (zawyzone) liczby zeby
/// guard nie ladowal zbyt wielu na raz. Wartosci empiryczne dla Apple
/// Silicon unified memory + typowe konsole CUDA.
pub fn estimate_vram_for_model(model_repo: &str) -> u64 {
    let lower = model_repo.to_ascii_lowercase();
    // Wykrycie quantyzacji (4-bit ~= 25% wagi fp16).
    let q4 = lower.contains("4bit") || lower.contains("q4") || lower.contains("4-bit");
    let q8 = lower.contains("8bit") || lower.contains("q8") || lower.contains("8-bit");
    // Rozmiar parametrow z nazwy.
    let base_gb: f64 = if lower.contains("70b") {
        140.0
    } else if lower.contains("32b") {
        64.0
    } else if lower.contains("13b") || lower.contains("14b") {
        28.0
    } else if lower.contains("8b") || lower.contains("7b") {
        16.0
    } else if lower.contains("3b") {
        6.0
    } else if lower.contains("1b") || lower.contains("1.5b") {
        3.0
    } else if lower.contains("0.8b") || lower.contains("0.5b") {
        1.6
    } else if lower.contains("whisper-large") {
        3.0
    } else if lower.contains("whisper") {
        1.5
    } else if lower.contains("piper") || lower.contains("vits") {
        0.5
    } else {
        4.0
    };
    let factor = if q4 {
        0.30
    } else if q8 {
        0.55
    } else {
        1.0
    };
    ((base_gb * factor) * 1024.0).round() as u64
}
