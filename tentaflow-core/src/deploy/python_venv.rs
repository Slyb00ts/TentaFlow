// =============================================================================
// Plik: deploy/python_venv.rs
// Opis: Deploy silnikow Pythonowych (vLLM/SGLang/XTTS/VoxCPM/Parakeet/
//       Qwen-ASR/ComfyUI) **BEZ Dockera**, natywnie na maszynie uzytkownika.
//
//       Flow:
//        1. Rozpakuj embed bundle (deploy::bundle::extract_to) do tmpdir.
//        2. Odczytaj tentaflow-containers/python-bundles/<engine>/bundle.toml.
//        3. Zapewnij Pythona relokowalnego w ~/.cache/tentaflow/python/<ver>/
//           (pobierz python-build-standalone dla platformy, jesli brak).
//        4. Zapewnij `uv` binarke w ~/.cache/tentaflow/bin/ (pobierz z GitHub).
//        5. Stworz venv ~/.cache/tentaflow/envs/<engine>/ + zainstaluj pakiet
//           (pypi albo git clone + pip install -e .) + requirements.lock.
//        6. Skopiuj server.py (jesli jest) do venv app-dir.
//        7. Uruchom subprocess wg [launch] z bundle.toml, z `env`.
// =============================================================================

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::collections::HashMap;

/// Sparsowane bundle.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct BundleSpec {
    pub bundle: BundleMeta,
    pub launch: LaunchSpec,
    #[serde(default)]
    pub requires: Requires,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleMeta {
    pub engine: String,
    pub description: String,
    pub python_version: String,
    pub source: String,            // "pypi" | "git"
    #[serde(default)]
    pub pypi_package: Option<String>,
    #[serde(default)]
    pub git_repo: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LaunchSpec {
    pub command: String,
    pub args: Vec<String>,
    pub internal_port: u16,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Requires {
    #[serde(default)]
    pub cuda: Option<String>,
    #[serde(default)]
    pub gpu_memory_gb: Option<u32>,
    #[serde(default)]
    pub disk_gb: Option<u32>,
    #[serde(default)]
    pub platforms: Vec<String>,
}

/// Konfiguracja deployu z wizarda (analog do docker::DeployRequest).
#[derive(Debug, Clone)]
pub struct NativeDeployRequest {
    pub engine: String,
    pub instance_name: Option<String>,
    pub env: HashMap<String, String>,
}

/// Wynik: uruchomiony subprocess + sciezki.
pub struct RunningEngine {
    pub engine: String,
    pub instance_name: String,
    pub child: Child,
    pub venv_dir: PathBuf,
    pub internal_port: u16,
}

/// Katalog cache tentaflow (`~/.cache/tentaflow/` na linux,
/// `~/Library/Caches/tentaflow/` na macOS).
pub fn cache_root() -> Result<PathBuf> {
    dirs::cache_dir()
        .map(|c| c.join("tentaflow"))
        .ok_or_else(|| anyhow::anyhow!("nie mozna ustalic cache dir"))
}

/// Odczytuje bundle.toml z rozpakowanego kontekstu.
pub fn read_bundle_spec(extracted_root: &Path, engine: &str) -> Result<BundleSpec> {
    let path = extracted_root
        .join("tentaflow-containers/python-bundles")
        .join(engine)
        .join("bundle.toml");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("brak bundle.toml: {}", path.display()))?;
    let spec: BundleSpec = toml::from_str(&content)
        .with_context(|| format!("parsowanie {}", path.display()))?;
    Ok(spec)
}

/// Glowna funkcja. Odpowiada tentaflow-core::deploy::docker::deploy() ale
/// dla Pythona bez kontenera.
pub fn deploy(req: &NativeDeployRequest) -> Result<RunningEngine> {
    let extracted = tempfile::tempdir()?;
    super::bundle::extract_to(extracted.path())?;
    let spec = read_bundle_spec(extracted.path(), &req.engine)?;

    check_platform_compat(&spec.requires)?;

    let cache = cache_root()?;
    let python_bin = ensure_python(&cache, &spec.bundle.python_version)?;
    let _ = ensure_uv(&cache)?;  // tentaflow poki co wola `uv` z PATH — patrz ensure_uv

    let venv_dir = cache.join("envs").join(&req.engine);
    let bundle_src = extracted
        .path()
        .join("tentaflow-containers/python-bundles")
        .join(&req.engine);

    create_venv(&python_bin, &venv_dir)?;
    install_deps(&venv_dir, &spec, &bundle_src)?;
    copy_bundle_files(&bundle_src, &venv_dir)?;

    let child = spawn_engine(&venv_dir, &spec, req)?;
    let instance_name = req
        .instance_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}-native", req.engine));

    Ok(RunningEngine {
        engine: req.engine.clone(),
        instance_name,
        child,
        venv_dir,
        internal_port: spec.launch.internal_port,
    })
}

/// Sprawdza `[requires].platforms` przeciwko obecnej platformie.
fn check_platform_compat(req: &Requires) -> Result<()> {
    if req.platforms.is_empty() {
        return Ok(());
    }
    let current = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    // Normalizacja np. "linux-x86_64" -> supported check
    if !req.platforms.iter().any(|p| p == &current) {
        anyhow::bail!(
            "silnik nie wspiera platformy {} (wspierane: {:?})",
            current, req.platforms
        );
    }
    Ok(())
}

/// Zapewnia relokowalnego Pythona w cache. TYLKO szkielet — pobieranie
/// python-build-standalone bedzie dopisane w kolejnej iteracji (wymaga
/// tylko reqwest::get + tar unpack). Na razie zwraca sciezke do `python3`
/// z systemu jako fallback.
fn ensure_python(cache: &Path, _version: &str) -> Result<PathBuf> {
    // TODO: pobrac python-build-standalone z
    // https://github.com/astral-sh/python-build-standalone/releases
    // Na razie fallback do systemowego Pythona, zeby przetestowac flow.
    let system = which::which("python3")
        .or_else(|_| which::which("python"))
        .context("nie znalazlem Pythona w PATH — pobieranie relokowalnego Pythona jeszcze niezaimplementowane, zainstaluj Pythona 3.11+")?;
    let _ = cache; // silence unused
    Ok(system)
}

/// Zapewnia `uv` w cache. Jak wyzej — TODO pobieranie z release.
fn ensure_uv(_cache: &Path) -> Result<()> {
    if which::which("uv").is_err() {
        tracing::warn!(
            "brak `uv` w PATH — spadek do `pip`. Zainstaluj uv (`curl -LsSf https://astral.sh/uv/install.sh | sh`) dla szybszego bootstrapu."
        );
    }
    Ok(())
}

fn create_venv(python: &Path, venv: &Path) -> Result<()> {
    if venv.join("pyvenv.cfg").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(venv.parent().unwrap()).ok();
    run(Command::new(python).args(["-m", "venv", venv.to_str().unwrap()]))
        .context("tworzenie venv")
}

fn install_deps(venv: &Path, spec: &BundleSpec, bundle_src: &Path) -> Result<()> {
    let pip = venv_bin(venv, "pip");
    run(Command::new(&pip).args(["install", "--upgrade", "pip", "wheel"]))?;

    // requirements.lock jesli jest
    let lock = bundle_src.join("requirements.lock");
    if lock.exists() {
        run(Command::new(&pip).arg("install").arg("-r").arg(&lock))
            .context("instalacja requirements.lock")?;
    }

    match spec.bundle.source.as_str() {
        "pypi" => {
            let pkg = spec.bundle.pypi_package.as_deref().unwrap_or(&spec.bundle.engine);
            run(Command::new(&pip).arg("install").arg(pkg))
                .with_context(|| format!("pip install {}", pkg))?;
        }
        "git" => {
            let repo = spec.bundle.git_repo.as_deref()
                .context("source=git wymaga git_repo")?;
            let refname = spec.bundle.git_ref.as_deref().unwrap_or("main");
            let clone_dir = venv.join("src").join(&spec.bundle.engine);
            if !clone_dir.exists() {
                std::fs::create_dir_all(clone_dir.parent().unwrap()).ok();
                run(Command::new("git")
                    .arg("clone")
                    .arg("--depth").arg("1")
                    .arg("--branch").arg(refname)
                    .arg(repo)
                    .arg(&clone_dir))
                    .context("git clone")?;
            }
            run(Command::new(&pip).arg("install").arg("-e").arg(&clone_dir))
                .context("pip install -e .")?;
        }
        other => anyhow::bail!("nieznane source: {}", other),
    }
    Ok(())
}

/// Kopiuje dodatkowe pliki bundla (np. server.py) do venv app-dir.
fn copy_bundle_files(bundle_src: &Path, venv: &Path) -> Result<()> {
    let dst = venv.join("app");
    std::fs::create_dir_all(&dst).ok();
    for entry in std::fs::read_dir(bundle_src)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let name = entry.file_name();
            std::fs::copy(&p, dst.join(&name))?;
        }
    }
    Ok(())
}

fn spawn_engine(venv: &Path, spec: &BundleSpec, req: &NativeDeployRequest) -> Result<Child> {
    let exe = venv_bin(venv, &spec.launch.command);
    let bundle_dir = venv.join("app");

    let mut cmd = Command::new(&exe);
    for arg in &spec.launch.args {
        cmd.arg(substitute_vars(arg, &req.env, &bundle_dir));
    }
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    cmd.env("BUNDLE_DIR", &bundle_dir);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let child = cmd.spawn()
        .with_context(|| format!("spawn {:?}", exe))?;
    Ok(child)
}

/// Podstawia `${VAR}` i `${VAR:-default}` w stringu na wartosci z env+bundle_dir.
fn substitute_vars(s: &str, env: &HashMap<String, String>, bundle_dir: &Path) -> String {
    let bundle_dir_str = bundle_dir.to_string_lossy().to_string();
    let mut out = s.to_string();
    loop {
        let Some(start) = out.find("${") else { break };
        let Some(end_rel) = out[start..].find('}') else { break };
        let end = start + end_rel;
        let inner = &out[start + 2..end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d.to_string())),
            None => (inner, None),
        };
        let value = if name == "BUNDLE_DIR" {
            bundle_dir_str.clone()
        } else {
            env.get(name).cloned().unwrap_or_else(|| default.unwrap_or_default())
        };
        out.replace_range(start..=end, &value);
    }
    out
}

fn venv_bin(venv: &Path, bin: &str) -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let dir = if cfg!(windows) { "Scripts" } else { "bin" };
    venv.join(dir).join(format!("{}{}", bin, suffix))
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("uruchomienie {:?}", cmd.get_program()))?;
    if !status.success() {
        anyhow::bail!("{:?} zwrocilo kod {}", cmd.get_program(), status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_basic() {
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "meta-llama/Llama-3.1-8B".to_string());
        let s = substitute_vars("--model=${MODEL}", &env, Path::new("/tmp/b"));
        assert_eq!(s, "--model=meta-llama/Llama-3.1-8B");
    }

    #[test]
    fn substitute_default() {
        let env = HashMap::new();
        let s = substitute_vars("--mem=${MEM:-0.9}", &env, Path::new("/tmp/b"));
        assert_eq!(s, "--mem=0.9");
    }

    #[test]
    fn substitute_bundle_dir() {
        let env = HashMap::new();
        let s = substitute_vars("--app-dir ${BUNDLE_DIR}", &env, Path::new("/tmp/b"));
        assert_eq!(s, "--app-dir /tmp/b");
    }

    #[test]
    fn read_bundle_spec_parses_vllm() {
        // Sprawdzamy ze kazdy bundle.toml w repo jest poprawny
        let workspace = std::path::PathBuf::from("..");
        for engine in ["vllm", "sglang", "xtts", "voxcpm", "parakeet", "qwen-asr", "comfyui"] {
            let path = workspace
                .join("tentaflow-containers/python-bundles")
                .join(engine)
                .join("bundle.toml");
            if !path.exists() { continue; }
            let content = std::fs::read_to_string(&path).unwrap();
            let spec: BundleSpec = toml::from_str(&content)
                .unwrap_or_else(|e| panic!("parse {}: {}", engine, e));
            assert_eq!(spec.bundle.engine, engine);
            assert!(spec.launch.internal_port > 0);
        }
    }

    #[test]
    fn platform_compat_blocks_unsupported() {
        let req = Requires {
            platforms: vec!["linux-x86_64".into(), "linux-aarch64".into()],
            ..Default::default()
        };
        let current = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let should_pass = req.platforms.contains(&current);
        let ok = check_platform_compat(&req);
        assert_eq!(ok.is_ok(), should_pass);
    }
}
