// =============================================================================
// Plik: deploy/python_venv.rs
// Opis: Deploy silnikow Pythonowych (vLLM/SGLang/XTTS/VoxCPM/Parakeet/
//       Qwen-ASR/ComfyUI) **BEZ Dockera**, natywnie na maszynie uzytkownika.
//
//       Flow:
//        1. Rozpakuj embed bundle (deploy::bundle::extract_to) do tmpdir.
//        2. Odczytaj tentaflow-containers/<kategoria>/python/<engine>/bundle.toml.
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
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// Log callback: wywolywany dla kazdej linii stdout/stderr subprocesu oraz
/// wysokopoziomowych faz deployu. `Arc` zeby wolno bylo clone'owac do watkow
/// czytajacych piped stdio.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// Noop sink dla wywolan gdzie caller nie chce logow (np. legacy bootstrap).
pub fn noop_log_sink() -> LogSink {
    Arc::new(|_: &str| {})
}

/// Sparsowane bundle.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct BundleSpec {
    pub bundle: BundleMeta,
    pub launch: LaunchSpec,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default, rename = "install_variants")]
    pub install_variants: Vec<InstallVariant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallVariant {
    /// "cuda" | "rocm" | "xpu" | "metal" | "cpu"
    pub backend: String,
    #[serde(default)]
    pub extra_index: Option<String>,
    #[serde(default)]
    pub extras: Vec<String>,
    /// Pakiety ktore buduja natywne kernele z torcha (flash-attn, xformers
    /// bez prebuilt wheel itp.). Instalowane PO glownym pakiecie z flaga
    /// `--no-build-isolation` zeby build mial dostep do zainstalowanego torcha.
    #[serde(default)]
    pub extras_no_build_isolation: Vec<String>,
    #[serde(default)]
    pub install_hint: Option<String>,
    /// Pakiety force-reinstallowane PO calym install flow (lock + extras +
    /// main + extras_no_build_isolation). Naprawia sytuacje gdy main package
    /// upstream upgraduje wersje, ktore my musimy trzymac na konkretnej
    /// wartosci (np. coqui-tts 0.27.4 wymaga transformers >=4.50, ale Coqui
    /// XTTS gpt.py uzywa transformers.pytorch_utils.isin_mps_friendly ktore
    /// usunieto w >=4.57). force_pins z `--force-reinstall --no-deps`
    /// nadpisuje resolver decision bez zmiany topologii grafu zaleznosci.
    #[serde(default)]
    pub force_pins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleMeta {
    pub engine: String,
    pub description: String,
    pub python_version: String,
    pub source: String, // "pypi" | "git" | "vllm-metal"
    #[serde(default)]
    pub pypi_package: Option<String>,
    #[serde(default)]
    pub git_repo: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    /// Podkatalog w sklonowanym repo gdzie lezy pyproject/setup.py
    /// (np. SGLang trzyma package w `python/`). Pusty = root.
    #[serde(default)]
    pub install_subdir: Option<String>,
    /// "editable" (domyslne, pip install -e .) lub "requirements_txt"
    /// (tylko pip install -r requirements.txt — dla ComfyUI co nie jest
    /// package, uruchamia sie przez python main.py).
    #[serde(default)]
    pub install_mode: Option<String>,
    /// source="vllm-metal": wersja upstream vllm tarballa z GitHub Releases
    /// (np. "0.19.1"). Wymagana dla tego source.
    #[serde(default)]
    pub vllm_version: Option<String>,
    /// source="vllm-metal": repo pluginu w formacie "<owner>/<name>"
    /// (default "vllm-project/vllm-metal").
    #[serde(default)]
    pub vllm_metal_repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LaunchSpec {
    pub command: String,
    pub args: Vec<String>,
    pub internal_port: u16,
    /// Statyczne env vars wymuszane na procesie silnika niezaleznie od tego
    /// co user/GUI poda. Przyklady: TVM_FFI_GPU_BACKEND=cuda dla sglang na
    /// hybrid CUDA+ROCm hostach. Klucze tu maja PRIORYTET nad req.env i
    /// HF_HOME/TORCH_HOME — sa twardym kontraktem bundla.
    #[serde(default)]
    pub env: HashMap<String, String>,
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

/// Katalog cache tentaflow. Delegates to the portable layout in
/// `crate::paths::cache_dir()` (honors `TENTAFLOW_CACHE_DIR`, falls back
/// to `<tentaflow_home>/cache`).
pub fn cache_root() -> Result<PathBuf> {
    let path = crate::paths::cache_dir();
    std::fs::create_dir_all(&path)
        .with_context(|| format!("create cache dir {}", path.display()))?;
    Ok(path)
}

/// Workspace root that already contains the extracted `tentaflow-containers/`
/// tree. `paths::ensure_app_dirs()` populates this at startup, so deploy
/// flows skip the legacy "extract bundle into a tmpdir" step.
fn runtime_bundle_root() -> Result<PathBuf> {
    let containers = crate::paths::containers_root();
    let parent = containers
        .parent()
        .ok_or_else(|| anyhow::anyhow!("containers_root has no parent: {}", containers.display()))?
        .to_path_buf();
    if !containers.is_dir() {
        anyhow::bail!(
            "tentaflow-containers/ not extracted yet at {} — run paths::ensure_app_dirs() first",
            containers.display()
        );
    }
    Ok(parent)
}

/// Znajduje katalog bundla Pythona dla danego silnika.
/// Skanuje wszystkie kategorie w tentaflow-containers/ szukajac
/// <category>/python/<engine_id>/. Zwraca pierwsze trafienie (engine_id
/// powinien byc unikalny w obrebie projektu).
fn find_bundle_dir(workspace_root: &Path, engine_id: &str) -> Option<PathBuf> {
    let containers = workspace_root.join("tentaflow-containers");
    let categories = [
        "llm",
        "stt",
        "tts",
        "embeddings",
        "reranker",
        "vision",
        "image-gen",
        "video-gen",
        "music-gen",
        "model-3d-gen",
        "agents",
        "tools",
    ];
    for category in categories {
        let candidate = containers.join(category).join("python").join(engine_id);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Odczytuje bundle.toml z rozpakowanego kontekstu.
pub fn read_bundle_spec(extracted_root: &Path, engine: &str) -> Result<BundleSpec> {
    let bundle_dir = find_bundle_dir(extracted_root, engine)
        .ok_or_else(|| anyhow::anyhow!(
            "brak katalogu bundla Pythona dla silnika '{}' w tentaflow-containers/<kategoria>/python/",
            engine
        ))?;
    let path = bundle_dir.join("bundle.toml");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("brak bundle.toml: {}", path.display()))?;
    let spec: BundleSpec =
        toml::from_str(&content).with_context(|| format!("parsowanie {}", path.display()))?;
    Ok(spec)
}

/// Wynik bootstrapu bez uruchamiania procesu silnika — sluzy do walidacji
/// ze srodowisko (Python + venv + wheels) zostalo poprawnie przygotowane.
pub struct BootstrappedEngine {
    pub engine: String,
    pub venv_dir: PathBuf,
    pub python_bin: PathBuf,
    pub internal_port: u16,
}

/// Wykonuje wszystkie kroki `deploy()` poza `spawn_engine`. Uzywane przez
/// `cargo run --example bootstrap_python_bundle` do sprawdzenia czy
/// pobieranie Pythona/uv + instalacja wheels dzialaja na danej maszynie.
pub fn bootstrap(engine: &str) -> Result<BootstrappedEngine> {
    bootstrap_with_logs(engine, &noop_log_sink())
}

pub fn bootstrap_with_logs(engine: &str, log: &LogSink) -> Result<BootstrappedEngine> {
    let workspace = runtime_bundle_root()?;
    let spec = read_bundle_spec(&workspace, engine)?;
    check_platform_compat(&spec.requires)?;

    let detected = crate::system_check::collect();
    let backend_name = backend_to_str(&detected.gpu.preferred_backend);
    let variant = pick_install_variant(&spec.install_variants, backend_name)?;
    log(&format!(
        "bootstrap: engine={} backend={}",
        engine, backend_name
    ));

    let cache = cache_root()?;
    let python_bin = ensure_python(&cache, &spec.bundle.python_version, log)?;
    let uv_bin = ensure_uv(&cache, log).ok();

    let bundle_src = find_bundle_dir(&workspace, engine)
        .ok_or_else(|| anyhow::anyhow!(
            "brak katalogu bundla Pythona dla silnika '{}' w tentaflow-containers/<kategoria>/python/",
            engine
        ))?;

    let empty_env: HashMap<String, String> = HashMap::new();
    let venv_dir = prepare_template_env(
        &cache,
        &python_bin,
        &uv_bin,
        &spec,
        variant,
        &bundle_src,
        &empty_env,
        log,
    )?;

    Ok(BootstrappedEngine {
        engine: engine.to_string(),
        venv_dir,
        python_bin,
        internal_port: spec.launch.internal_port,
    })
}

/// Glowna funkcja. Odpowiada tentaflow-core::deploy::docker::deploy() ale
/// dla Pythona bez kontenera. Wersja `deploy_with_logs` streamuje kazda linie
/// stdout/stderr subprocesu przez `log_cb` — preferowana sciezka dla runnera
/// GUI. `deploy()` to backward-compat wrapper dla wywolan bez streamu logow.
pub fn deploy(req: &NativeDeployRequest) -> Result<RunningEngine> {
    deploy_with_logs(req, &noop_log_sink())
}

pub fn deploy_with_logs(req: &NativeDeployRequest, log: &LogSink) -> Result<RunningEngine> {
    let workspace = runtime_bundle_root()?;
    let spec = read_bundle_spec(&workspace, &req.engine)?;

    check_platform_compat(&spec.requires)?;

    // Wykryj backend (CUDA/ROCm/Metal/XPU) i wybierz odpowiedni variant.
    let detected = crate::system_check::collect();
    let backend_name = backend_to_str(&detected.gpu.preferred_backend);
    let variant = pick_install_variant(&spec.install_variants, backend_name)?;
    log(&format!(
        "wariant instalacji: engine={} backend={}",
        req.engine, backend_name
    ));

    let cache = cache_root()?;
    log("przygotowanie Pythona i uv");
    let python_bin = ensure_python(&cache, &spec.bundle.python_version, log)?;
    let uv_bin = ensure_uv(&cache, log).ok();

    let bundle_src = find_bundle_dir(&workspace, &req.engine)
        .ok_or_else(|| anyhow::anyhow!(
            "brak katalogu bundla Pythona dla silnika '{}' w tentaflow-containers/<kategoria>/python/",
            req.engine
        ))?;

    let instance_name = req
        .instance_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}-native", req.engine));
    log(&format!(
        "template venv + instalacja zaleznosci dla {}",
        req.engine
    ));
    let template_venv = prepare_template_env(
        &cache,
        &python_bin,
        &uv_bin,
        &spec,
        variant,
        &bundle_src,
        &req.env,
        log,
    )?;
    let template_id = template_identity(&spec, variant, &bundle_src)?;
    log(&format!("instance venv: {}", instance_name));
    let venv_dir = prepare_instance_env(
        &cache,
        &req.engine,
        &instance_name,
        &template_venv,
        &template_id,
        log,
    )?;

    log(&format!(
        "uruchamiam silnik: {} (port wewn. {})",
        req.engine, spec.launch.internal_port
    ));
    let child = spawn_engine(&venv_dir, &spec, req)?;

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
            current,
            req.platforms
        );
    }
    Ok(())
}

/// Wersja python-build-standalone i uv jaka pobieramy. Aktualizacje recznie —
/// ta wartosc sluzy jako lock, zeby cache byl deterministyczny.
/// Release tag python-build-standalone (aktualizujemy rocznie, nadpisywalny
/// przez env TENTAFLOW_PBS_DATE). Lista:
/// https://github.com/astral-sh/python-build-standalone/releases
const PBS_DATE: &str = "20260408";
/// uv release (env TENTAFLOW_UV_VERSION do override).
const UV_VERSION: &str = "0.5.14";

/// Zapewnia relokowalnego Pythona w `<cache>/python/<py_ver>/`. Jesli
/// katalog istnieje -> reuse. W przeciwnym razie pobiera odpowiednie archiwum
/// z github.com/astral-sh/python-build-standalone/releases.
fn ensure_python(cache: &Path, py_ver: &str, log: &LogSink) -> Result<PathBuf> {
    let target_dir = cache.join("python").join(py_ver);
    let python_bin = python_bin_path(&target_dir);
    if python_bin.exists() {
        log(&format!("python {}: reuse z cache", py_ver));
        return Ok(python_bin);
    }

    let triple = pbs_triple().with_context(|| {
        format!(
            "nie znam PBS triple dla {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let full_ver = resolve_full_python_version(py_ver);
    let date = pbs_date();
    let url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{date}/cpython-{ver}+{date}-{triple}-install_only.tar.gz",
        date = date, ver = full_ver, triple = triple
    );

    log(&format!("pobieram Python {} ({})", full_ver, triple));
    std::fs::create_dir_all(&target_dir)?;
    download_and_extract(&url, &target_dir, log)?;

    if !python_bin.exists() {
        anyhow::bail!(
            "po wypakowaniu python-build-standalone nie znalazlem {:?}",
            python_bin
        );
    }
    Ok(python_bin)
}

/// Zapewnia binarke `uv` w `<cache>/bin/uv`. Reuse jesli juz jest.
fn ensure_uv(cache: &Path, log: &LogSink) -> Result<PathBuf> {
    let bin_dir = cache.join("bin");
    let uv_name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let uv_path = bin_dir.join(uv_name);
    if uv_path.exists() {
        log(&format!("uv: reuse z cache ({})", uv_path.display()));
        return Ok(uv_path);
    }
    std::fs::create_dir_all(&bin_dir)?;

    let triple = uv_triple().context("nie znam uv target triple dla tej platformy")?;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let url = format!(
        "https://github.com/astral-sh/uv/releases/download/{ver}/uv-{triple}.{ext}",
        ver = UV_VERSION,
        triple = triple,
        ext = ext
    );

    log(&format!("pobieram uv {} ({})", UV_VERSION, triple));
    download_and_extract(&url, &bin_dir, log)?;

    // Po extract uv konczy jako `<bin_dir>/uv-<triple>/uv` — przenosimy wprost
    let nested = bin_dir.join(format!("uv-{}", triple)).join(uv_name);
    if nested.exists() && !uv_path.exists() {
        std::fs::rename(&nested, &uv_path).ok();
    }
    if !uv_path.exists() {
        // fallback: szukaj binarki w glebi
        for entry in walkdir_shallow(&bin_dir) {
            if entry.file_name().map(|f| f == uv_name).unwrap_or(false) {
                std::fs::rename(&entry, &uv_path).ok();
                break;
            }
        }
    }
    if !uv_path.exists() {
        anyhow::bail!("nie udalo sie znalezc uv po wypakowaniu");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&uv_path)?.permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&uv_path, p)?;
    }
    Ok(uv_path)
}

/// Rekurencyjne (plytko, 2 poziomy) wyszukiwanie plikow do znalezienia uv po extract.
fn walkdir_shallow(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&p) {
                for ie in inner.flatten() {
                    out.push(ie.path());
                }
            }
        } else {
            out.push(p);
        }
    }
    out
}

fn python_bin_path(base: &Path) -> PathBuf {
    // python-build-standalone rozpakowuje do `python/` a binarka jest w bin/python3.
    if cfg!(windows) {
        base.join("python").join("python.exe")
    } else {
        base.join("python").join("bin").join("python3")
    }
}

/// Rozwiaza "3.12" -> "3.12.13" (aktualna dla PBS_DATE).
/// Patche sa pinowane recznie z kazdym releasem PBS; gdy URL 404, uzytkownik
/// moze nadpisac przez env TENTAFLOW_PYTHON_FULL_VERSION.
fn resolve_full_python_version(v: &str) -> String {
    if let Ok(override_full) = std::env::var("TENTAFLOW_PYTHON_FULL_VERSION") {
        return override_full;
    }
    // Patche dla PBS_DATE = 20260408
    match v {
        "3.11" => "3.11.15".into(),
        "3.12" => "3.12.13".into(),
        "3.13" => "3.13.13".into(),
        other => other.to_string(),
    }
}

fn pbs_date() -> String {
    std::env::var("TENTAFLOW_PBS_DATE").unwrap_or_else(|_| PBS_DATE.to_string())
}

fn pbs_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc-shared"),
        _ => None,
    }
}

fn uv_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// Pobiera i rozpakowuje archiwum tar.gz / zip do docelowego katalogu.
/// Blocking; wolamy synchronicznie z thread pool (deploy to rzadka operacja).
fn download_and_extract(url: &str, dst: &Path, log: &LogSink) -> Result<()> {
    log(&format!("pobieranie: {}", url));
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("GET {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("HTTP {} przy {}", response.status(), url);
    }
    let bytes = response.bytes()?;
    log(&format!(
        "pobrane: {} bajtow, rozpakowuje do {}",
        bytes.len(),
        dst.display()
    ));

    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        let decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dst)?;
    } else if url.ends_with(".tar.zst") {
        let decoder = zstd::Decoder::new(&bytes[..])?;
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dst)?;
    } else if url.ends_with(".zip") {
        // Dla Windows uv
        let reader = std::io::Cursor::new(&bytes[..]);
        let mut zip = zip::ZipArchive::new(reader)?;
        zip.extract(dst)?;
    } else {
        anyhow::bail!("nieznany format archiwum w URL: {}", url);
    }
    Ok(())
}

fn create_venv(python: &Path, venv: &Path, log: &LogSink) -> Result<()> {
    if venv.join("pyvenv.cfg").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(venv.parent().unwrap()).ok();
    log(&format!("python -m venv {}", venv.display()));
    run_with_logs(
        Command::new(python).args(["-m", "venv", venv.to_str().unwrap()]),
        log,
    )
    .context("tworzenie venv")
}

fn prepare_template_env(
    cache: &Path,
    python: &Path,
    uv: &Option<PathBuf>,
    spec: &BundleSpec,
    variant: Option<&InstallVariant>,
    bundle_src: &Path,
    extra_env: &HashMap<String, String>,
    log: &LogSink,
) -> Result<PathBuf> {
    let template_id = template_identity(spec, variant, bundle_src)?;
    let template_dir = templates_root(cache)
        .join(&spec.bundle.engine)
        .join(&template_id)
        .join("venv");

    // Marker pisany dopiero po SUKCESIE install_deps + copy_bundle_files.
    // pyvenv.cfg powstaje na samym poczatku `python -m venv`, wiec gdy uv
    // crashnie w trakcie pobierania wheels (np. broken pipe na nvidia-cublas),
    // template ma pyvenv.cfg ale brakuje pakietow. Bez tego markera nastepny
    // deploy "reuse" pomijal install i silnik padal z ModuleNotFoundError.
    let install_complete_marker = template_dir.join(".tentaflow-install-complete");
    if template_dir.join("pyvenv.cfg").exists() && install_complete_marker.exists() {
        log("template venv: reuse (install complete)");
        return Ok(template_dir);
    }

    if template_dir.exists() {
        log(&format!(
            "template venv: niekompletny ({}), czyszcze przed ponowna instalacja",
            template_dir.display()
        ));
        std::fs::remove_dir_all(&template_dir).with_context(|| {
            format!(
                "czyszczenie niekompletnego template venv {}",
                template_dir.display()
            )
        })?;
    }

    std::fs::create_dir_all(template_dir.parent().unwrap()).ok();
    if let Some(legacy) = legacy_env_dir(cache, &spec.bundle.engine) {
        log(&format!(
            "migracja legacy env {} → {}",
            legacy.display(),
            template_dir.display()
        ));
        copy_dir_recursive(&legacy, &template_dir)?;
        let stale_clone = template_dir.join("src").join(&spec.bundle.engine);
        if stale_clone.exists() {
            std::fs::remove_dir_all(&stale_clone).with_context(|| {
                format!(
                    "usuwanie starego checkoutu {} przed odswiezeniem template",
                    stale_clone.display()
                )
            })?;
        }
    } else {
        create_venv(python, &template_dir, log)?;
    }
    install_deps(&template_dir, uv, spec, variant, bundle_src, extra_env, log)?;
    copy_bundle_files(bundle_src, &template_dir)?;
    std::fs::write(&install_complete_marker, template_id.as_bytes())
        .context("zapis markera template install complete")?;
    Ok(template_dir)
}

fn prepare_instance_env(
    cache: &Path,
    engine: &str,
    instance_name: &str,
    template_venv: &Path,
    template_id: &str,
    log: &LogSink,
) -> Result<PathBuf> {
    let instance_dir = instances_root(cache)
        .join(engine)
        .join(sanitize_fs_name(instance_name));
    let marker = instance_dir.join(".tentaflow-template-id");

    if instance_dir.join("pyvenv.cfg").exists()
        && std::fs::read_to_string(&marker).ok().as_deref() == Some(template_id)
    {
        log(&format!(
            "instance venv: reuse (template id zgodny) {}",
            instance_dir.display()
        ));
        return Ok(instance_dir);
    }

    if instance_dir.exists() {
        log(&format!(
            "usuwam stary instance venv {} (inny template id)",
            instance_dir.display()
        ));
        std::fs::remove_dir_all(&instance_dir).with_context(|| {
            format!("usuwanie starego env instancji {}", instance_dir.display())
        })?;
    }

    // Bundle/dependency update: zmiana template_id == rebuild venv. Globalne
    // JIT compile cache (FlashInfer, Triton, torch_extensions) zapisuja
    // absolutne sciezki do plikow zrodlowych z poprzedniego venv. Po jego
    // usunieciu cache wskazuje na nieistniejace pliki i ninja crashuje
    // (np. "missing and no known rule to make"). Czyscimy je defensywnie
    // przy KAZDEJ aktualizacji bundla — to jest jedyny sposob zeby byc
    // bulletproof na wszystkich platformach (Linux/Windows/macOS, CUDA/ROCm).
    purge_global_jit_caches(log);

    log(&format!(
        "klonuje template venv do instance {}",
        instance_dir.display()
    ));
    copy_dir_recursive(template_venv, &instance_dir)?;
    std::fs::write(&marker, template_id)?;
    Ok(instance_dir)
}

/// Czysci globalne JIT cache'e (FlashInfer, Triton, torch_extensions, nvidia
/// cuda_compile_cache) ktore zapisuja absolutne sciezki do plikow zrodlowych
/// z konkretnej instancji venv. Po rebuild venv te cache zwracaja stale
/// referencje i lamia kompilacje on-demand. Wywolujemy przy kazdej zmianie
/// template_id (== zmiana bundle.toml / requirements.lock / Dockerfile).
fn purge_global_jit_caches(log: &LogSink) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let candidates = [
        home.join(".cache").join("flashinfer"),
        home.join(".cache").join("torch_extensions"),
        home.join(".triton").join("cache"),
        home.join(".cache").join("nv").join("ComputeCache"),
        home.join(".nv").join("ComputeCache"),
    ];
    for path in &candidates {
        if path.exists() {
            match std::fs::remove_dir_all(path) {
                Ok(()) => log(&format!("purged stale JIT cache {}", path.display())),
                Err(e) => log(&format!(
                    "ostrzezenie: nie udalo sie wyczyscic {} ({})",
                    path.display(),
                    e
                )),
            }
        }
    }
}

fn template_identity(
    spec: &BundleSpec,
    variant: Option<&InstallVariant>,
    bundle_src: &Path,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(spec.bundle.engine.as_bytes());
    hasher.update(spec.bundle.python_version.as_bytes());
    hasher.update(spec.bundle.source.as_bytes());

    if let Some(pkg) = &spec.bundle.pypi_package {
        hasher.update(pkg.as_bytes());
    }
    if let Some(repo) = &spec.bundle.git_repo {
        hasher.update(repo.as_bytes());
    }
    if let Some(git_ref) = &spec.bundle.git_ref {
        hasher.update(git_ref.as_bytes());
    }
    if let Some(subdir) = &spec.bundle.install_subdir {
        hasher.update(subdir.as_bytes());
    }
    if let Some(mode) = &spec.bundle.install_mode {
        hasher.update(mode.as_bytes());
    }

    if let Some(v) = variant {
        hasher.update(v.backend.as_bytes());
        if let Some(extra_index) = &v.extra_index {
            hasher.update(extra_index.as_bytes());
        }
        for extra in &v.extras {
            hasher.update(extra.as_bytes());
        }
        for extra in &v.extras_no_build_isolation {
            hasher.update(extra.as_bytes());
        }
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(bundle_src)?
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for file in files {
        hasher.update(
            file.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
        hasher.update(std::fs::read(&file)?);
    }

    let digest = hasher.finalize();
    Ok(hex::encode(&digest[..8]))
}

fn templates_root(cache: &Path) -> PathBuf {
    cache.join("bundle-templates")
}

fn instances_root(cache: &Path) -> PathBuf {
    cache.join("bundle-instances")
}

fn legacy_env_dir(cache: &Path, engine: &str) -> Option<PathBuf> {
    let candidate = cache.join("envs").join(engine);
    if candidate.join("pyvenv.cfg").exists() {
        Some(candidate)
    } else {
        None
    }
}

fn sanitize_fs_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "instance".to_string()
    } else {
        out.to_string()
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&src_path)?;
        let file_type = meta.file_type();

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
            continue;
        }

        if file_type.is_symlink() {
            let target = std::fs::read_link(&src_path)?;
            create_symlink(&target, &dst_path)?;
            continue;
        }

        link_or_copy_file(&src_path, &dst_path)?;
    }
    Ok(())
}

fn link_or_copy_file(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    let metadata = std::fs::metadata(target).ok();
    if metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
        std::os::windows::fs::symlink_dir(target, link)?;
    } else {
        std::os::windows::fs::symlink_file(target, link)?;
    }
    Ok(())
}

/// Instaluje zaleznosci przez `uv pip` lub klasyczny `pip`. Parametr
/// `variant` niesie konfiguracje specyficzna dla backendu GPU
/// (extra_index -> PyTorch wheels per CUDA/ROCm/Metal, extras -> dodatkowe
/// pakiety typu vllm-metal/flash-attn).
fn install_deps(
    venv: &Path,
    uv: &Option<PathBuf>,
    spec: &BundleSpec,
    variant: Option<&InstallVariant>,
    bundle_src: &Path,
    extra_env: &HashMap<String, String>,
    log: &LogSink,
) -> Result<()> {
    let extra_index = variant.and_then(|v| v.extra_index.clone());
    let installer = Installer::new(
        venv,
        uv.as_deref(),
        extra_index,
        Arc::clone(log),
        extra_env.clone(),
    );
    // setuptools>=77 wymagane zeby VoxCPM / niektore nowe pyproject.toml
    // z `license = "MIT"` (string form, PEP 639) sie instalowaly.
    installer.upgrade_pip()?;

    let lock = bundle_src.join("requirements.lock");
    if lock.exists() {
        installer
            .install_requirements(&lock)
            .context("install lock")?;
    }

    // Extras (wymagajace tylko pypi — accelerate, vllm-metal, nemo_toolkit itp.).
    // Pakiety z `extras_no_build_isolation` beda zainstalowane pozniej, juz po
    // glownym pakiecie (kiedy torch jest obecny).
    if let Some(v) = variant {
        for extra in &v.extras {
            installer
                .install_package(extra)
                .with_context(|| format!("install extra {}", extra))?;
        }
    }

    match spec.bundle.source.as_str() {
        "pypi" => {
            // Fallback do engine.id zostal usuniety: dawal mylacy blad
            // "No versions of <engine_id>" gdy bundle.toml mial literowke
            // (np. `package = "x"` zamiast `pypi_package = "x"`). Wymuszamy
            // explicit pypi_package zeby walic z czytelnym bledem przy
            // deploy zamiast 5 min po fakcie.
            let pkg = spec.bundle.pypi_package.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "bundle.toml dla '{}': source=\"pypi\" wymaga pola \
                     `pypi_package = \"<nazwa-na-pypi>\"`. Pole `package` \
                     nie jest rozpoznawane (literowka).",
                    spec.bundle.engine
                )
            })?;
            installer
                .install_package(pkg)
                .with_context(|| format!("install {}", pkg))?;
        }
        "git" => {
            let repo = spec
                .bundle
                .git_repo
                .as_deref()
                .context("source=git wymaga git_repo")?;
            let refname = spec.bundle.git_ref.as_deref().unwrap_or("main");
            let clone_dir = venv.join("src").join(&spec.bundle.engine);
            if !clone_dir.exists() {
                std::fs::create_dir_all(clone_dir.parent().unwrap()).ok();
                log(&format!(
                    "git clone --depth 1 --branch {} {}",
                    refname, repo
                ));
                run_with_logs(
                    Command::new("git")
                        .arg("clone")
                        .arg("--depth")
                        .arg("1")
                        .arg("--branch")
                        .arg(refname)
                        .arg(repo)
                        .arg(&clone_dir),
                    log,
                )
                .context("git clone")?;
            }
            // Podkatalog z pyproject/setup.py (np. SGLang -> python/)
            let pkg_dir = match spec.bundle.install_subdir.as_deref() {
                Some(sub) if !sub.is_empty() => clone_dir.join(sub),
                _ => clone_dir.clone(),
            };
            // Fix upstream bugs znanych repo (np. VoxCPM 'license = "MIT"' w formie
            // string ktora wymaga setuptools 77+; pomimo upgrade'u zdarza sie ze
            // build backend cache uzywa starszej wersji. Zastepujemy na obiekt.)
            patch_pyproject_if_needed(&pkg_dir)?;
            // Tryb instalacji: editable (domyslne) vs requirements_txt (ComfyUI)
            let mode = spec.bundle.install_mode.as_deref().unwrap_or("editable");
            match mode {
                "editable" => installer
                    .install_editable(&pkg_dir)
                    .context("install -e .")?,
                "requirements_txt" => {
                    let req = pkg_dir.join("requirements.txt");
                    if !req.exists() {
                        anyhow::bail!("install_mode=requirements_txt a brak {}", req.display());
                    }
                    installer
                        .install_requirements(&req)
                        .context("install -r requirements.txt")?;
                }
                other => anyhow::bail!("nieznany install_mode: {}", other),
            }
        }
        "vllm-metal" => {
            install_vllm_metal(&installer, &spec.bundle, log)
                .context("install vllm-metal (MLX plugin)")?;
        }
        other => anyhow::bail!("nieznane source: {}", other),
    }

    // Teraz torch jest zainstalowany (z glownego pakietu jego deps).
    // Instalujemy extras ktore wymagaja torcha do buildu kerneli CUDA.
    if let Some(v) = variant {
        for extra in &v.extras_no_build_isolation {
            installer
                .install_package_no_build_isolation(extra)
                .with_context(|| format!("install {} (no-build-isolation)", extra))?;
        }
    }

    // Force pins — ostatnia faza, nadpisuje wersje ktorych resolver wybral
    // wbrew naszym ograniczeniom. Wymuszane bezposrednio z `pip install
    // --force-reinstall --no-deps <pkg==ver>`.
    if let Some(v) = variant {
        for pkg in &v.force_pins {
            installer
                .install_force_pin(pkg)
                .with_context(|| format!("force-pin {}", pkg))?;
        }
    }

    Ok(())
}

/// Restartuje proces silnika z istniejacego venv instancji — bez reinstall.
/// Uzywana przy autostartcie tentaflow dla serwisow `deploy_mode=native`
/// ktorych proces padl (crash OS, reboot) albo ktorych stare PID-y sa juz
/// nieaktywne. Zaklada ze venv w `<cache>/bundle-instances/<engine>/<name>/`
/// istnieje z poprzedniego deploy — jesli nie, zwraca blad i caller powinien
/// zdecydowac czy oznaczyc serwis jako `stopped` w DB.
pub fn relaunch(req: &NativeDeployRequest) -> Result<RunningEngine> {
    let workspace = runtime_bundle_root()?;
    let spec = read_bundle_spec(&workspace, &req.engine)?;
    check_platform_compat(&spec.requires)?;

    let cache = cache_root()?;
    let instance_name = req
        .instance_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}-native", req.engine));
    let venv_dir = instances_root(&cache)
        .join(&req.engine)
        .join(sanitize_fs_name(&instance_name));
    if !venv_dir.join("pyvenv.cfg").exists() {
        anyhow::bail!(
            "brak instance venv w {} — nie mozna restartowac bez ponownej instalacji",
            venv_dir.display()
        );
    }

    let child = spawn_engine(&venv_dir, &spec, req)?;
    Ok(RunningEngine {
        engine: req.engine.clone(),
        instance_name,
        child,
        venv_dir,
        internal_port: spec.launch.internal_port,
    })
}

/// Install flow dla `source = "vllm-metal"` — odwzorowuje
/// https://github.com/vllm-project/vllm-metal/blob/main/install.sh:
///   1) pobierz tarball vllm v<vllm_version> z GitHub Releases i rozpakuj
///   2) `uv pip install -r vllm-<ver>/requirements/cpu.txt --index-strategy unsafe-best-match`
///   3) `CXXFLAGS="-Wno-parentheses" uv pip install <vllm-<ver>/>`
///   4) pobierz `.whl` z vllm-project/vllm-metal releases/latest → `uv pip install <wheel>`
fn install_vllm_metal(installer: &Installer<'_>, meta: &BundleMeta, log: &LogSink) -> Result<()> {
    let vllm_ver = meta
        .vllm_version
        .as_deref()
        .context("source=vllm-metal wymaga pola vllm_version w [bundle]")?;
    let metal_repo = meta
        .vllm_metal_repo
        .as_deref()
        .unwrap_or("vllm-project/vllm-metal");

    installer.upgrade_pip()?;

    let work = tempfile::tempdir().context("tmpdir dla vllm-metal")?;
    let tarball_url = format!(
        "https://github.com/vllm-project/vllm/releases/download/v{ver}/vllm-{ver}.tar.gz",
        ver = vllm_ver
    );
    log(&format!("pobieram upstream vLLM {} tarball", vllm_ver));
    download_and_extract(&tarball_url, work.path(), log)?;

    let vllm_src = work.path().join(format!("vllm-{}", vllm_ver));
    if !vllm_src.exists() {
        anyhow::bail!(
            "tarball vllm rozpakowal sie bez oczekiwanego podkatalogu {}",
            vllm_src.display()
        );
    }

    let cpu_req = vllm_src.join("requirements").join("cpu.txt");
    if !cpu_req.exists() {
        anyhow::bail!(
            "vllm tarball nie zawiera {} (zmiana upstream layoutu?)",
            cpu_req.display()
        );
    }
    log("instaluje vLLM requirements/cpu.txt (torch CPU)");
    installer.install_requirements(&cpu_req)?;

    log("kompiluje vLLM z CXXFLAGS=-Wno-parentheses");
    let mut cmd = installer.cmd();
    cmd.env("CXXFLAGS", "-Wno-parentheses");
    cmd.arg("install");
    installer.add_install_flags(&mut cmd);
    cmd.arg(vllm_src.to_str().context("nie-UTF8 sciezka do vllm src")?);
    run_with_logs(&mut cmd, log).context("kompilacja vllm ze zrodla")?;

    let wheel_dir = tempfile::tempdir().context("tmpdir dla wheel vllm-metal")?;
    let wheel_path = download_vllm_metal_wheel(metal_repo, wheel_dir.path(), log)?;
    log(&format!(
        "instaluje vllm-metal wheel: {}",
        wheel_path.display()
    ));
    installer.install_package(
        wheel_path
            .to_str()
            .context("nie-UTF8 sciezka do wheel vllm-metal")?,
    )?;

    Ok(())
}

/// Pobiera najnowszy asset `.whl` z GitHub Releases/latest danego repo i
/// zapisuje do `dst_dir`. Zwraca sciezke do zapisanego pliku.
fn download_vllm_metal_wheel(repo: &str, dst_dir: &Path, log: &LogSink) -> Result<PathBuf> {
    let api_url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    log(&format!("GET {}", api_url));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("tentaflow")
        .build()?;
    let resp = client
        .get(&api_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .with_context(|| format!("GET {}", api_url))?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API {} zwrocil HTTP {}", api_url, resp.status());
    }
    let json: serde_json::Value = resp.json().context("parse JSON z releases/latest")?;
    let assets = json
        .get("assets")
        .and_then(|a| a.as_array())
        .context("brak `assets` w odpowiedzi releases/latest")?;
    let (wheel_name, wheel_url) = assets
        .iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(|n| n.as_str())?;
            let url = a.get("browser_download_url").and_then(|u| u.as_str())?;
            if name.ends_with(".whl") {
                Some((name.to_string(), url.to_string()))
            } else {
                None
            }
        })
        .next()
        .context("zadne z assets w releases/latest nie konczy sie na .whl")?;
    log(&format!("pobieram wheel {}", wheel_name));
    let dst = dst_dir.join(&wheel_name);
    let resp = client
        .get(&wheel_url)
        .send()
        .with_context(|| format!("GET {}", wheel_url))?;
    if !resp.status().is_success() {
        anyhow::bail!("download wheel HTTP {}", resp.status());
    }
    let bytes = resp.bytes()?;
    std::fs::write(&dst, &bytes).with_context(|| format!("zapis {}", dst.display()))?;
    Ok(dst)
}

/// Naprawia znane upstream problemy w pyproject.toml sklonowanych repo.
///
/// Problem: PEP 639 zmienil format pola `license` w `[project]` — stare
/// setuptools (<77) wymagaja `{text = "MIT"}` / `{file = "LICENSE"}`, nowe
/// setuptools (>=77) wymagaja string `"MIT"`, a czesc repo ma zle dla
/// setuptools ktorego uv uzywa w build isolation. VoxCPM mial string gdy
/// uv wzial stare setuptools (padalo), vLLM ma object gdy uv wzial nowe
/// setuptools (padalo).
///
/// Bezpieczne rozwiazanie uniwersalne: **usunac** linie `license = ...` z
/// sekcji `[project]`. Pole jest opcjonalne per PEP 621, wiec pyproject
/// bez niego jest dalej valid. Nie dotykamy nic innego.
fn patch_pyproject_if_needed(pkg_dir: &Path) -> Result<()> {
    let pj = pkg_dir.join("pyproject.toml");
    if !pj.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&pj)?;

    let mut out = String::with_capacity(content.len());
    let mut in_project_section = false;
    let mut patched = false;
    let mut iter = content.lines().peekable();
    while let Some(line) = iter.next() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_project_section = trimmed == "[project]";
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_project_section {
            // Usun wiersz zaczynajacy sie od `license =` (obie formy: string / object
            // inline / object multi-line).
            if trimmed.starts_with("license") && trimmed.contains('=') {
                patched = true;
                // Jesli object multi-line (np. `license = { ... }` → pominac az do zamykajacego `}`).
                if trimmed.contains('{') && !trimmed.contains('}') {
                    // Drop linie az zlapie zamykajacy `}`
                    while let Some(inner) = iter.next() {
                        if inner.contains('}') {
                            break;
                        }
                    }
                }
                continue; // skip tej linii
            }
        }
        out.push_str(line);
        out.push('\n');
    }

    if patched {
        std::fs::write(&pj, &out)?;
        tracing::info!(path=%pj.display(), "Usunieto pole license z [project] (kompatybilnosc setuptools)");
    }
    Ok(())
}

fn backend_to_str(b: &crate::system_check::GpuBackend) -> &'static str {
    use crate::system_check::GpuBackend::*;
    match b {
        Cuda => "cuda",
        Rocm => "rocm",
        Xpu => "xpu",
        Metal => "metal",
        Cpu => "cpu",
    }
}

/// Wybiera wariant instalacji pasujacy do backendu. Jesli brak wariantu
/// dla danego backendu — fallback w kolejnosci cuda/rocm/metal/xpu/cpu.
fn pick_install_variant<'a>(
    variants: &'a [InstallVariant],
    backend: &str,
) -> Result<Option<&'a InstallVariant>> {
    if variants.is_empty() {
        return Ok(None);
    }
    if let Some(v) = variants.iter().find(|v| v.backend == backend) {
        return Ok(Some(v));
    }
    // Fallback: spytaj pierwsze dostepne, ale ostrzez
    tracing::warn!(
        "brak wariantu dla backendu '{}', uzywam '{}' jako fallback",
        backend,
        variants[0].backend
    );
    Ok(Some(&variants[0]))
}

/// Abstrakcja ponad `uv` i `pip` — ten sam interfejs instalacji.
/// `extra_index_url` wstrzykuje `--extra-index-url <url>` do kazdej instalacji,
/// co wybiera wariant torcha (cu124, rocm7.0, cpu, itd.).
struct Installer<'a> {
    venv: PathBuf,
    uv: Option<&'a Path>,
    extra_index_url: Option<String>,
    log: LogSink,
    extra_env: HashMap<String, String>,
}

impl<'a> Installer<'a> {
    fn new(
        venv: &Path,
        uv: Option<&'a Path>,
        extra_index_url: Option<String>,
        log: LogSink,
        extra_env: HashMap<String, String>,
    ) -> Self {
        Self {
            venv: venv.to_path_buf(),
            uv,
            extra_index_url,
            log,
            extra_env,
        }
    }
    fn cmd(&self) -> Command {
        let mut c = if let Some(uv) = self.uv {
            let mut c = Command::new(uv);
            c.env("VIRTUAL_ENV", &self.venv);
            // Duze wheels NVIDIA (cublas, cudnn, cudart) sa czesto > 500MB i
            // przy slabszej sieci uv default timeout (30s) tnie polaczenie ze
            // "stream closed because of a broken pipe". 600s pokrywa nawet
            // 50KB/s edge case'y.
            c.env("UV_HTTP_TIMEOUT", "600");
            c.arg("pip");
            c
        } else {
            let pip = venv_bin(&self.venv, "pip");
            Command::new(pip)
        };
        // Propaguj HF_TOKEN/HF_HOME/HUGGINGFACE_HUB_CACHE/TRANSFORMERS_CACHE/
        // TORCH_HOME z runner.rs zeby pip install gated repo i kompilacja
        // torchow widzialy token + wspolny katalog modeli.
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        c
    }

    /// Uruchamia komende instalacyjna z retry dla transient network errors
    /// (broken pipe, connection reset). Trzy proby z exp backoff (2s, 4s).
    /// Bledy nie-sieciowe (np. resolver conflict) nie sa retryowane —
    /// drugi run da ten sam wynik. Heurystyka: retryujemy ZAWSZE przy
    /// niezerowym exit code, bo `uv pip install` przy network failu zwraca 1
    /// bez specjalnego kodu, a koszt retry'a po prawdziwym konflikcie to
    /// kilka sekund — vs. utrata 5min pobrania torch+cu130.
    fn run_install(&self, c: &mut Command) -> Result<()> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=3 {
            match run_with_logs(c, &self.log) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < 3 {
                        let backoff_secs = 2u64.pow(attempt as u32);
                        (self.log)(&format!(
                            "pip install failed (attempt {}/3): {} — retry za {}s",
                            attempt, e, backoff_secs
                        ));
                        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }
    /// Dopisuje flagi do `pip install` (po subkomendzie). Osobno bo uv
    /// uzywa --index-strategy a pip nie zna tego flaga.
    fn add_install_flags(&self, c: &mut Command) {
        if self.uv.is_some() {
            // unsafe-best-match: pozwol uv brac wheels z KAZDEGO index'a
            // (domyslnie uv blokuje zeby nie bylo dependency confusion, ale
            // dla torch+cu124 to normalne).
            c.arg("--index-strategy").arg("unsafe-best-match");
        }
    }
    fn add_index(&self, c: &mut Command) {
        if let Some(idx) = &self.extra_index_url {
            c.arg("--extra-index-url").arg(idx);
        }
    }
    fn upgrade_pip(&self) -> Result<()> {
        (self.log)("pip: upgrade pip/wheel/setuptools");
        let mut c = self.cmd();
        c.arg("install")
            .arg("--upgrade")
            .arg("pip")
            .arg("wheel")
            .arg("setuptools>=77");
        self.run_install(&mut c)
    }
    fn install_requirements(&self, path: &Path) -> Result<()> {
        (self.log)(&format!("pip: install -r {}", path.display()));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("-r").arg(path);
        self.run_install(&mut c)
    }
    fn install_package(&self, pkg: &str) -> Result<()> {
        (self.log)(&format!("pip: install {}", pkg));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg(pkg);
        self.run_install(&mut c)
    }
    fn install_editable(&self, path: &Path) -> Result<()> {
        (self.log)(&format!("pip: install -e {}", path.display()));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("-e").arg(path);
        self.run_install(&mut c)
    }
    /// Instalacja z wylaczona izolacja buildu (`--no-build-isolation`) —
    /// pakiet ma dostep do zainstalowanego torcha podczas budowy natywnych
    /// kerneli. Wymagane dla flash-attn, niektorych wariantow xformers itp.
    fn install_package_no_build_isolation(&self, pkg: &str) -> Result<()> {
        (self.log)(&format!("pip: install --no-build-isolation {}", pkg));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("--no-build-isolation").arg(pkg);
        self.run_install(&mut c)
    }
    /// `pip install --force-reinstall --no-deps <pkg>` — nadpisuje wersje
    /// ktora resolver wybral, bez ruszania grafu zaleznosci. Uzywane do
    /// wymuszenia konkretnej wersji deps po main package install (force_pins
    /// w bundle.toml).
    fn install_force_pin(&self, pkg: &str) -> Result<()> {
        (self.log)(&format!("pip: install --force-reinstall --no-deps {}", pkg));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("--force-reinstall").arg("--no-deps").arg(pkg);
        self.run_install(&mut c)
    }
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

/// Wyodrebniona logika budowania listy args dla spawn_engine. Pozwala
/// jednostkowo testowac VLLM_ARGS/SGLANG_ARGS passthrough bez spawn'owania
/// realnego procesu.
pub(crate) fn build_engine_args(
    spec: &BundleSpec,
    env: &HashMap<String, String>,
    bundle_dir: &Path,
    venv: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::with_capacity(spec.launch.args.len() + 8);
    for arg in &spec.launch.args {
        args.push(substitute_vars_full(arg, env, bundle_dir, venv));
    }
    // VLLM_ARGS / SGLANG_ARGS / itd. z deploy wizard (Advanced section) -
    // appendowane PO arguments z bundle.toml. shlex split honoruje cudzyslowy
    // (np. --extra-config '{"key": "val"}'). Pozwala uzytkownikowi nadpisac
    // tensor-parallel-size, max-model-len, kv-cache-dtype itp. dla bundle
    // python tak samo jak dla docker (gdzie VLLM_ARGS jest expanded w
    // entrypoint.sh przez shell).
    let extra_args_env_keys = ["VLLM_ARGS", "SGLANG_ARGS", "TRTLLM_ARGS", "EXTRA_ARGS"];
    for key in extra_args_env_keys {
        if let Some(extra) = env.get(key) {
            let trimmed = extra.trim();
            if trimmed.is_empty() {
                continue;
            }
            match shlex::split(trimmed) {
                Some(parts) => {
                    for part in parts {
                        args.push(substitute_vars_full(&part, env, bundle_dir, venv));
                    }
                }
                None => {
                    // Quotes mismatch - fallback do prostego whitespace split.
                    for part in trimmed.split_whitespace() {
                        args.push(substitute_vars_full(part, env, bundle_dir, venv));
                    }
                }
            }
        }
    }
    args
}

/// Buduje `Command` ktora opakowuje docelowa binarke w `nice` + `ionice`
/// na Linuksie zeby silnik podczas startu (model load, torch.compile,
/// flashinfer JIT) nie zabijal responsywnosci hosta. Wartosci nice/ionice
/// mozna nadpisac przez TENTAFLOW_ENGINE_NICE / TENTAFLOW_ENGINE_IONICE_CLASS
/// / TENTAFLOW_ENGINE_IONICE_LEVEL. Ustaw TENTAFLOW_ENGINE_NICE=0 zeby
/// wylaczyc.
#[cfg(target_os = "linux")]
fn build_engine_command(exe: &Path) -> Command {
    let nice_level = std::env::var("TENTAFLOW_ENGINE_NICE")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(5);
    let ionice_class = std::env::var("TENTAFLOW_ENGINE_IONICE_CLASS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(2);
    let ionice_level = std::env::var("TENTAFLOW_ENGINE_IONICE_LEVEL")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(7);

    if nice_level == 0 {
        return Command::new(exe);
    }

    let nice_available = std::process::Command::new("nice")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !nice_available {
        return Command::new(exe);
    }

    let ionice_available = std::process::Command::new("ionice")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut cmd = Command::new("nice");
    cmd.arg("-n").arg(nice_level.to_string());
    if ionice_available {
        cmd.arg("ionice")
            .arg("-c")
            .arg(ionice_class.to_string())
            .arg("-n")
            .arg(ionice_level.to_string());
    }
    cmd.arg(exe);
    cmd
}

#[cfg(target_os = "macos")]
fn build_engine_command(exe: &Path) -> Command {
    let nice_level = std::env::var("TENTAFLOW_ENGINE_NICE")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(5);
    if nice_level == 0 {
        return Command::new(exe);
    }
    let mut cmd = Command::new("nice");
    cmd.arg("-n").arg(nice_level.to_string()).arg(exe);
    cmd
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn build_engine_command(exe: &Path) -> Command {
    Command::new(exe)
}

/// Szuka instalacji CUDA toolkit na hoscie. Zwraca katalog, w ktorym
/// `bin/nvcc` istnieje. Sprawdza w kolejnosci: `which nvcc` (PATH), potem
/// znane lokacje systemowe. Wynik nie jest cache'owany — koszt to kilka
/// stat() przy spawn engine'a.
fn find_nvcc_root() -> Option<PathBuf> {
    let nvcc_name = if cfg!(windows) { "nvcc.exe" } else { "nvcc" };

    if let Ok(output) = std::process::Command::new("which").arg(nvcc_name).output() {
        if output.status.success() {
            if let Ok(path) = std::str::from_utf8(&output.stdout) {
                let nvcc_path = PathBuf::from(path.trim());
                if nvcc_path.exists() {
                    if let Some(bin_dir) = nvcc_path.parent() {
                        if let Some(root) = bin_dir.parent() {
                            return Some(root.to_path_buf());
                        }
                    }
                }
            }
        }
    }

    let candidates = [
        "/usr/local/cuda",
        "/opt/cuda",
        "/usr/lib/cuda",
        "/usr/local/cuda-13.0",
        "/usr/local/cuda-12.8",
        "/usr/local/cuda-12.4",
        "/usr/local/cuda-12.1",
    ];
    for cand in &candidates {
        let root = PathBuf::from(cand);
        if root.join("bin").join(nvcc_name).exists() {
            return Some(root);
        }
    }
    None
}

fn spawn_engine(venv: &Path, spec: &BundleSpec, req: &NativeDeployRequest) -> Result<Child> {
    let exe = venv_bin(venv, &spec.launch.command);
    let bundle_dir = venv.join("app");

    let mut cmd = build_engine_command(&exe);
    for arg in build_engine_args(spec, &req.env, &bundle_dir, venv) {
        cmd.arg(arg);
    }
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    // Statyczne env z bundle.toml [launch.env] — wymuszane PO req.env zeby
    // wartosci z manifestu wygraly nad ad-hoc env z deploy req'a (np.
    // TVM_FFI_GPU_BACKEND=cuda dla sglang).
    for (k, v) in &spec.launch.env {
        cmd.env(k, v);
    }
    cmd.env("BUNDLE_DIR", &bundle_dir);
    cmd.env("VENV_DIR", venv);

    // Prepend venv/bin to PATH tak, zeby procesy potomne (np. flashinfer
    // JIT wolajacy `ninja` przez subprocess.run) znalazly binarki ktore pip
    // zainstalowal w venv (ninja, cmake) zamiast szukac w systemowym PATH.
    let venv_bin_dir = venv.join("bin");
    let new_path = match std::env::var_os("PATH") {
        Some(existing) => {
            let mut p = std::ffi::OsString::from(&venv_bin_dir);
            p.push(":");
            p.push(existing);
            p
        }
        None => std::ffi::OsString::from(&venv_bin_dir),
    };
    cmd.env("PATH", new_path);
    cmd.env("VIRTUAL_ENV", venv);

    // Shared <tentaflow_home>/models/ — same root Docker uses, so a model
    // pulled by Docker vLLM lives in the same hub/models--*/ directory that
    // native Python vLLM (and every other engine on this host) sees.
    let _ = crate::paths::ensure_models_dirs();
    let hf = crate::paths::hf_home();
    let torch = crate::paths::torch_home();
    for (k, v) in [
        ("HF_HOME", hf.clone()),
        ("HUGGINGFACE_HUB_CACHE", hf.clone()),
        ("TRANSFORMERS_CACHE", hf.clone()),
        ("TORCH_HOME", torch.clone()),
    ] {
        if !req.env.contains_key(k) {
            cmd.env(k, &v);
        }
    }

    // CUDA_HOME / CUDA_PATH: flashinfer JIT odpala nvcc po sciezce
    // <CUDA_HOME>/bin/nvcc. Gdy env wskazuje na nieistniejacy katalog
    // (np. runai container `/workspace/cuda-13.0` na bare-metalu) lub gdy
    // env nie jest ustawione a system ma nvcc tylko w PATH, JIT crashuje
    // 5 minut po starcie z `nvcc: not found`. Wymuszamy realna sciezke
    // wyszukana w runtime.
    let env_cuda = req
        .env
        .get("CUDA_HOME")
        .or_else(|| req.env.get("CUDA_PATH"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CUDA_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("CUDA_PATH").map(PathBuf::from));
    let cuda_home_valid = env_cuda
        .as_ref()
        .map(|p| p.join("bin").join("nvcc").exists())
        .unwrap_or(false);
    let cuda_home = if cuda_home_valid {
        env_cuda
    } else {
        find_nvcc_root()
    };
    if let Some(home) = &cuda_home {
        cmd.env("CUDA_HOME", home);
        cmd.env("CUDA_PATH", home);
    } else {
        eprintln!(
            "WARN: nvcc nie znaleziony w PATH ani CUDA_HOME — flashinfer JIT \
             bedzie crashowal przy pierwszym FP4/FP8 kernelu. Zainstaluj \
             CUDA toolkit albo ustaw CUDA_HOME na poprawna sciezke."
        );
    }

    // Cap rownoleglosci compile threads tak, zeby torch.compile / inductor /
    // flashinfer JIT nie odpalaly N watkow == liczba CPU (na 20-rdzeniowym
    // node'ie to powoduje ze caly host wisi przez kilka minut przy starcie
    // modelu). Polowa CPU domyslnie. Override przez TENTAFLOW_COMPILE_THREADS.
    if !req.env.contains_key("TORCHINDUCTOR_COMPILE_THREADS")
        && !spec
            .launch
            .env
            .contains_key("TORCHINDUCTOR_COMPILE_THREADS")
    {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let compile_threads = std::env::var("TENTAFLOW_COMPILE_THREADS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or_else(|| std::cmp::max(2, cpus / 2));
        cmd.env("TORCHINDUCTOR_COMPILE_THREADS", compile_threads.to_string());
        // MAX_JOBS jest honorowane przez setuptools/cmake (flashinfer JIT
        // cz. nvcc fork-bomb) i ninja przy build_and_load.
        cmd.env("MAX_JOBS", compile_threads.to_string());
    }

    // FlashInfer JIT cache musi byc per-instancja: build.ninja zapisuje
    // absolutna sciezke do `<venv>/lib/python3.X/site-packages/flashinfer/data/csrc/*.cu`,
    // a kazda instancja vLLM ma losowy katalog venv. Globalny cache w
    // ~/.cache/flashinfer pamieta sciezke poprzedniej (juz usunietej)
    // instancji i ninja crashuje z "missing and no known rule to make it".
    let flashinfer_cache = venv.join(".flashinfer-cache");
    let _ = std::fs::create_dir_all(&flashinfer_cache);
    if !req.env.contains_key("FLASHINFER_WORKSPACE_BASE") {
        cmd.env("FLASHINFER_WORKSPACE_BASE", &flashinfer_cache);
    }

    // Stdout/stderr -> <venv>/engine.log. `Stdio::piped()` bez aktywnego
    // readera zapycha bufor pipe (~64KB) i Python blokuje na write podczas
    // ladowania modelu — vLLM widziany z zewnatrz jako "wisi przy starcie".
    // Plik jest tez jedynym sposobem diagnostyki padajacego silnika
    // (Connection refused z 127.0.0.1:8000 nic nie mowi o przyczynie).
    let log_path = venv.join("engine.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("open engine log {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .context("clone engine log fd dla stderr")?;
    cmd.stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));

    let child = cmd.spawn().with_context(|| format!("spawn {:?}", exe))?;
    Ok(child)
}

/// Podstawia `${VAR}` i `${VAR:-default}` w stringu na wartosci z env+bundle_dir.
/// Test-only convenience wrapper — production code uses
/// `substitute_vars_full` z explicit `venv_dir`.
#[cfg(test)]
fn substitute_vars(s: &str, env: &HashMap<String, String>, bundle_dir: &Path) -> String {
    substitute_vars_full(s, env, bundle_dir, Path::new(""))
}

fn substitute_vars_full(
    s: &str,
    env: &HashMap<String, String>,
    bundle_dir: &Path,
    venv_dir: &Path,
) -> String {
    let bundle_dir_str = bundle_dir.to_string_lossy().to_string();
    let venv_dir_str = venv_dir.to_string_lossy().to_string();
    let mut out = s.to_string();
    loop {
        let Some(start) = out.find("${") else { break };
        let Some(end_rel) = out[start..].find('}') else {
            break;
        };
        let end = start + end_rel;
        let inner = &out[start + 2..end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d.to_string())),
            None => (inner, None),
        };
        let value = match name {
            "BUNDLE_DIR" => bundle_dir_str.clone(),
            "VENV_DIR" => venv_dir_str.clone(),
            _ => env
                .get(name)
                .cloned()
                .unwrap_or_else(|| default.unwrap_or_default()),
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

/// Odpala subprocess z piped stdout/stderr i forwarduje kazda linie przez
/// `log_cb`. Bloku az subprocess sie zakonczy — wewnatrz `spawn_blocking`
/// caller nie blokuje tokio runtime. Errory subprocesu (kod != 0) zwracane
/// jako anyhow::Error, logi stderr juz wyszly do sink po drodze.
fn run_with_logs(cmd: &mut Command, log_cb: &LogSink) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let program = format!("{:?}", cmd.get_program());
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", program))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let cb_out = Arc::clone(log_cb);
    let stdout_handle = std::thread::spawn(move || {
        if let Some(o) = stdout {
            for line in BufReader::new(o).lines().map_while(Result::ok) {
                cb_out(&line);
            }
        }
    });
    let cb_err = Arc::clone(log_cb);
    let stderr_handle = std::thread::spawn(move || {
        if let Some(e) = stderr {
            for line in BufReader::new(e).lines().map_while(Result::ok) {
                cb_err(&line);
            }
        }
    });

    let status = child.wait().with_context(|| format!("wait {}", program))?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();
    if !status.success() {
        anyhow::bail!("{} zwrocilo kod {}", program, status);
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

    fn vllm_bundle_spec() -> BundleSpec {
        BundleSpec {
            bundle: BundleMeta {
                engine: "vllm".to_string(),
                description: String::new(),
                python_version: "3.12".to_string(),
                source: "pypi".to_string(),
                pypi_package: Some("vllm==0.20.0".to_string()),
                git_repo: None,
                git_ref: None,
                install_subdir: None,
                install_mode: None,
                vllm_version: None,
                vllm_metal_repo: None,
            },
            launch: LaunchSpec {
                command: "python".to_string(),
                args: vec![
                    "-m".to_string(),
                    "vllm.entrypoints.openai.api_server".to_string(),
                    "--host".to_string(),
                    "127.0.0.1".to_string(),
                    "--port".to_string(),
                    "${PORT:-8000}".to_string(),
                    "--model".to_string(),
                    "${MODEL}".to_string(),
                ],
                internal_port: 8000,
                env: HashMap::new(),
            },
            requires: Requires::default(),
            install_variants: vec![],
        }
    }

    #[test]
    fn build_engine_args_includes_vllm_args_from_env() {
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "Qwen/Qwen2.5-0.5B-Instruct".into());
        env.insert("PORT".to_string(), "9001".into());
        env.insert(
            "VLLM_ARGS".to_string(),
            "--tensor-parallel-size 4 --max-model-len 16384 --kv-cache-dtype fp8".into(),
        );

        let args = build_engine_args(&spec, &env, Path::new("/tmp/b"), Path::new("/tmp/v"));

        // Bundle defaults
        assert!(args
            .iter()
            .any(|a| a == "vllm.entrypoints.openai.api_server"));
        assert!(args.contains(&"Qwen/Qwen2.5-0.5B-Instruct".to_string()));
        assert!(args.contains(&"9001".to_string()));

        // VLLM_ARGS appendowane PO bundle args
        assert!(args.contains(&"--tensor-parallel-size".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"--max-model-len".to_string()));
        assert!(args.contains(&"16384".to_string()));
        assert!(args.contains(&"--kv-cache-dtype".to_string()));
        assert!(args.contains(&"fp8".to_string()));
    }

    #[test]
    fn build_engine_args_handles_quoted_vllm_args() {
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        // Symuluje cudzyslowy w vllm_args (np. JSON config)
        env.insert(
            "VLLM_ARGS".to_string(),
            r#"--tensor-parallel-size 2 --override-generation-config '{"max_tokens": 100}'"#.into(),
        );
        let args = build_engine_args(&spec, &env, Path::new("/tmp/b"), Path::new("/tmp/v"));
        assert!(args.contains(&"--tensor-parallel-size".to_string()));
        assert!(args.contains(&"2".to_string()));
        assert!(args.contains(&"--override-generation-config".to_string()));
        // shlex powinien zachowac JSON jako jeden token (bez surrounding ')
        assert!(
            args.iter().any(|a| a == r#"{"max_tokens": 100}"#),
            "args: {:?}",
            args
        );
    }

    #[test]
    fn build_engine_args_skip_empty_vllm_args() {
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        env.insert("VLLM_ARGS".to_string(), "   ".into()); // whitespace only
        let args = build_engine_args(&spec, &env, Path::new("/tmp/b"), Path::new("/tmp/v"));
        // Powinno byc tylko bundle defaults, BEZ trailing junk
        let last = args.last().unwrap();
        assert_ne!(last, " ");
        assert_eq!(args.len(), spec.launch.args.len());
    }

    #[test]
    fn build_engine_args_supports_sglang_args_too() {
        let mut spec = vllm_bundle_spec();
        spec.bundle.engine = "sglang".to_string();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        env.insert(
            "SGLANG_ARGS".to_string(),
            "--mem-fraction-static 0.85 --tp 2".into(),
        );
        let args = build_engine_args(&spec, &env, Path::new("/tmp/b"), Path::new("/tmp/v"));
        assert!(args.contains(&"--mem-fraction-static".to_string()));
        assert!(args.contains(&"0.85".to_string()));
        assert!(args.contains(&"--tp".to_string()));
    }

    #[test]
    fn read_bundle_spec_parses_vllm() {
        // Sprawdzamy ze kazdy bundle.toml w repo jest poprawny
        let workspace = std::path::PathBuf::from("..");
        for engine in [
            "vllm", "sglang", "xtts", "voxcpm", "parakeet", "qwen-asr", "comfyui",
        ] {
            let bundle_dir = match find_bundle_dir(&workspace, engine) {
                Some(d) => d,
                None => continue,
            };
            let path = bundle_dir.join("bundle.toml");
            if !path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            let spec: BundleSpec =
                toml::from_str(&content).unwrap_or_else(|e| panic!("parse {}: {}", engine, e));
            assert_eq!(spec.bundle.engine, engine);
            assert!(spec.launch.internal_port > 0);
        }
    }

    #[test]
    fn pick_variant_matches_backend() {
        let variants = vec![
            InstallVariant {
                backend: "cuda".into(),
                extra_index: Some("a".into()),
                extras: vec![],
                extras_no_build_isolation: vec![],
                install_hint: None,
                force_pins: vec![],
            },
            InstallVariant {
                backend: "rocm".into(),
                extra_index: Some("b".into()),
                extras: vec![],
                extras_no_build_isolation: vec![],
                install_hint: None,
                force_pins: vec![],
            },
            InstallVariant {
                backend: "metal".into(),
                extra_index: None,
                extras: vec!["vllm-metal".into()],
                extras_no_build_isolation: vec![],
                install_hint: None,
                force_pins: vec![],
            },
        ];
        let v = pick_install_variant(&variants, "rocm").unwrap().unwrap();
        assert_eq!(v.backend, "rocm");
        let v = pick_install_variant(&variants, "metal").unwrap().unwrap();
        assert_eq!(v.extras, vec!["vllm-metal".to_string()]);
        // Fallback gdy brak pasujacego
        let v = pick_install_variant(&variants, "xpu").unwrap().unwrap();
        assert_eq!(v.backend, "cuda"); // pierwszy jako fallback
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

    #[test]
    fn cache_root_respects_env_override() {
        let temp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("TENTAFLOW_CACHE_DIR", temp.path());
        }
        let root = cache_root().unwrap();
        unsafe {
            std::env::remove_var("TENTAFLOW_CACHE_DIR");
        }
        assert_eq!(root, temp.path());
    }
}
