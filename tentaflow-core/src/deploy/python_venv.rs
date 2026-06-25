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
    /// Env vars wstrzykiwane wylacznie do procesow `pip install` (a wiec
    /// rowniez do source builda gdy `source = "git"`). Uzywane np. przez
    /// `vllm-spark` zeby ustawic `TORCH_CUDA_ARCH_LIST=12.1a` przed
    /// `pip install -e .` (build CUDA kerneli pod sm_121a). Mergowane na
    /// install z `extra_env` z deploy requestu — wariant wygrywa gdy klucze
    /// sie pokrywaja, bo manifest jest twardszym kontraktem niz runtime hint.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Pakiety force-reinstallowane PO calym install flow (lock + extras +
    /// main + extras_no_build_isolation). Naprawia sytuacje gdy main package
    /// upstream upgraduje wersje, ktore my musimy trzymac na konkretnej
    /// wartosci (np. coqui-tts 0.27.4 wymaga transformers >=4.50, ale Coqui
    /// XTTS gpt.py uzywa transformers.pytorch_utils.isin_mps_friendly ktore
    /// usunieto w >=4.57). force_pins z `--force-reinstall --no-deps`
    /// nadpisuje resolver decision bez zmiany topologii grafu zaleznosci.
    #[serde(default)]
    pub force_pins: Vec<String>,
    /// Jak `extras_no_build_isolation`, ale instalowane DODATKOWO z `--no-deps`.
    /// Dla pakietow ktore buduja sie z torcha (no-build-isolation), ale ich
    /// graf zaleznosci ciagnie ciezkie/niekompilowalne pakiety nieuzywane w
    /// runtime (np. YOLOX -> onnx-simplifier/pycocotools); realne deps runtime
    /// dostarczamy jawnie w `extras`.
    #[serde(default)]
    pub extras_no_build_isolation_no_deps: Vec<String>,
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
    /// Git source + editable: instaluj `requirements.txt` z repo PRZED
    /// `pip install -e .` i rob editable z `--no-build-isolation`. Wymagane gdy
    /// `setup.py` importuje wlasny pakiet w czasie buildu (np. SearXNG:
    /// `from searx.version import VERSION_TAG` ciagnie `import msgspec`), bo
    /// build-isolation nie widzi runtime deps i build pada na ModuleNotFound.
    #[serde(default)]
    pub editable_no_build_isolation: bool,
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
    /// Strukturalne argumenty CLI budowane przez Rust (np.
    /// `["--speculative-config", "{...json...}"]` albo
    /// `["--gpu-memory-utilization", "0.85"]`). Dolaczane do argv silnika
    /// 1:1, BEZ shlex-a — inaczej parser shellowy zjada wewnetrzne cudzyslowy
    /// kompaktowego JSON-a w `--speculative-config` i vLLM dostaje zepsuty
    /// payload. To jest sciezka dla argumentow ktore MY skladamy z typed
    /// configu; user-typed `VLLM_ARGS` (env) nadal idzie przez shlex bo user
    /// sam cytuje. Last-wins z dedup nadpisuje pokrywajace sie flagi z
    /// bundle.toml i `VLLM_ARGS`.
    pub extra_args: Vec<String>,
    /// Jawna sciezka katalogu bundla z manifestu (`[deploy.native].bundle_path`),
    /// wzgledem tentaflow-containers/. Wymagane dla bundli wspoldzielonych przez
    /// kilka silnikow (engine_id != nazwa katalogu). None => skan po engine_id.
    pub bundle_subpath: Option<String>,
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
        "training",
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

/// Odczytuje bundle.toml z konkretnego katalogu bundla.
pub fn read_bundle_spec_from_dir(bundle_dir: &Path) -> Result<BundleSpec> {
    let path = bundle_dir.join("bundle.toml");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("brak bundle.toml: {}", path.display()))?;
    let spec: BundleSpec =
        toml::from_str(&content).with_context(|| format!("parsowanie {}", path.display()))?;
    Ok(spec)
}

/// Odczytuje bundle.toml z rozpakowanego kontekstu, rozwiazujac katalog po engine_id.
pub fn read_bundle_spec(extracted_root: &Path, engine: &str) -> Result<BundleSpec> {
    let bundle_dir = find_bundle_dir(extracted_root, engine)
        .ok_or_else(|| anyhow::anyhow!(
            "brak katalogu bundla Pythona dla silnika '{}' w tentaflow-containers/<kategoria>/python/",
            engine
        ))?;
    read_bundle_spec_from_dir(&bundle_dir)
}

/// Rozwiazuje katalog bundla: preferuje jawny `bundle_subpath` z manifestu
/// (wymagany dla bundli WSPOLDZIELONYCH przez kilka silnikow, np. nemotron-yolox
/// uzywany przez page/graphic/table-elements), inaczej skanuje po engine_id.
fn resolve_bundle_src(workspace: &Path, engine: &str, subpath: Option<&str>) -> Result<PathBuf> {
    if let Some(sub) = subpath.map(str::trim).filter(|s| !s.is_empty()) {
        // bundle_path z manifestu jest wzgledem tentaflow-containers/ (jak baza
        // find_bundle_dir), a `workspace` to jego rodzic.
        let dir = workspace.join("tentaflow-containers").join(sub);
        if dir.is_dir() {
            return Ok(dir);
        }
        anyhow::bail!("bundle_path nie istnieje: {}", dir.display());
    }
    find_bundle_dir(workspace, engine).ok_or_else(|| {
        anyhow::anyhow!(
            "brak katalogu bundla Pythona dla silnika '{}' w tentaflow-containers/<kategoria>/python/",
            engine
        )
    })
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
    let backend_name = install_variant_tag(&detected.gpu);
    let picked = pick_install_variant(&spec.install_variants, &backend_name)?;
    let variant_owned = inject_torch_cuda_arch(picked, &detected.gpu, log);
    let variant = variant_owned.as_ref();
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
    let bundle_src = resolve_bundle_src(&workspace, &req.engine, req.bundle_subpath.as_deref())?;
    let spec = read_bundle_spec_from_dir(&bundle_src)?;

    check_platform_compat(&spec.requires)?;

    // Wykryj backend (CUDA/ROCm/Metal/XPU) i wybierz odpowiedni variant.
    let detected = crate::system_check::collect();
    let backend_name = install_variant_tag(&detected.gpu);
    let picked = pick_install_variant(&spec.install_variants, &backend_name)?;
    let variant_owned = inject_torch_cuda_arch(picked, &detected.gpu, log);
    let variant = variant_owned.as_ref();
    log(&format!(
        "wariant instalacji: engine={} backend={}",
        req.engine, backend_name
    ));

    let cache = cache_root()?;
    log("przygotowanie Pythona i uv");
    let python_bin = ensure_python(&cache, &spec.bundle.python_version, log)?;
    let uv_bin = ensure_uv(&cache, log).ok();

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

    // Faktyczny port to PORT z env (alokowany przez PortAllocator). Pole
    // `spec.launch.internal_port` z bundle.toml to tylko metadana w jakiej
    // wartosci `${PORT}` substytuujemy gdy env nie zawiera PORT — wiec
    // logowanie go jako "port" wprowadza w blad gdy alokator nadal port
    // inny niz manifestowy default. Bierzemy z env, fallback na metadana.
    let actual_port = req
        .env
        .get("PORT")
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(spec.launch.internal_port);
    log(&format!(
        "uruchamiam silnik: {} (port {})",
        req.engine, actual_port
    ));
    let child = spawn_engine(&venv_dir, &spec, req, Some(log))?;

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
/// template_id (== zmiana build-relevant wejsc: requirements.lock, git_ref,
/// install_variants.env itd. — patrz `template_identity`; `[launch]` z
/// bundle.toml NIE liczy sie do template_id).
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
    if spec.bundle.editable_no_build_isolation {
        hasher.update(b"editable_no_build_isolation");
    }
    if let Some(ver) = &spec.bundle.vllm_version {
        hasher.update(ver.as_bytes());
    }
    if let Some(repo) = &spec.bundle.vllm_metal_repo {
        hasher.update(repo.as_bytes());
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
        for extra in &v.extras_no_build_isolation_no_deps {
            hasher.update(extra.as_bytes());
        }
        // `env` (np. TORCH_CUDA_ARCH_LIST) i `force_pins` realnie zmieniaja
        // skompilowane kernele / rozwiazany graf pakietow → musza wejsc do
        // hasha. HashMap nie ma deterministycznej kolejnosci, wiec sortujemy
        // klucze przed mieszaniem.
        let mut env_kv: Vec<(&String, &String)> = v.env.iter().collect();
        env_kv.sort();
        for (k, val) in env_kv {
            hasher.update(k.as_bytes());
            hasher.update([0u8]);
            hasher.update(val.as_bytes());
            hasher.update([0u8]);
        }
        for pin in &v.force_pins {
            hasher.update(pin.as_bytes());
        }
    }

    // Hashujemy pliki bundla (requirements.lock, patche, skrypty) — ale
    // POMIJAMY `bundle.toml`. Jego pola build-relevant sa juz wmieszane jawnie
    // powyzej; reszta to sekcja `[launch]` (command/args/env) ktora dotyczy
    // WYLACZNIE runtime. Gdyby bundle.toml wchodzil tu w calosci, zmiana flagi
    // startowej (np. dodanie `--served-model-name`) zmienialaby template_id i
    // wymuszala pelny `pip install -e` (rekompilacja kerneli CUDA, 20-30 min)
    // mimo ze venv jest identyczny.
    let mut files: Vec<PathBuf> = std::fs::read_dir(bundle_src)?
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.is_file())
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some("bundle.toml"))
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
    let mut merged_env = extra_env.clone();
    if let Some(v) = variant {
        for (k, val) in &v.env {
            merged_env.insert(k.clone(), val.clone());
        }
    }
    let installer = Installer::new(
        venv,
        uv.as_deref(),
        extra_index,
        Arc::clone(log),
        merged_env,
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
                "editable" if spec.bundle.editable_no_build_isolation => {
                    // setup.py importuje wlasny pakiet -> deps musza byc w venv
                    // PRZED buildem, a build bez izolacji (zeby je widzial).
                    let req = pkg_dir.join("requirements.txt");
                    if req.exists() {
                        installer
                            .install_requirements(&req)
                            .context("install repo requirements.txt (przed editable)")?;
                    }
                    installer
                        .install_editable_no_build_isolation(&pkg_dir)
                        .context("install -e . --no-build-isolation")?;
                }
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
        for extra in &v.extras_no_build_isolation_no_deps {
            installer
                .install_package_no_build_isolation_no_deps(extra)
                .with_context(|| format!("install {} (no-build-isolation,no-deps)", extra))?;
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
    let bundle_src = resolve_bundle_src(&workspace, &req.engine, req.bundle_subpath.as_deref())?;
    let spec = read_bundle_spec_from_dir(&bundle_src)?;
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

    let child = spawn_engine(&venv_dir, &spec, req, None)?;
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

/// Tag wariantu installu uwzgledniajacy DGX Spark. Dla zwyklych hostow
/// zwraca arch-tag CUDA z compute capability. Dla Sparka (sm_121a) zwraca
/// `"cuda-spark"` — bundle moze zadeklarowac osobny wariant z innym
/// `extra_index` (nightly aarch64 wheels) i innymi env (TORCH_CUDA_ARCH_LIST).
/// Tag wariantu instalacji = arch-tag GPU (`cuda-ampere`/`-ada`/`-hopper`/
/// `-blackwell`/`-spark` lub `rocm`/`metal`/`xpu`/`cpu`). Jedno zrodlo prawdy z
/// docker (`GpuSnapshot::cuda_arch_tag`).
fn install_variant_tag(gpu: &crate::system_check::GpuSnapshot) -> String {
    gpu.cuda_arch_tag()
}

/// Wybiera wariant instalacji pasujacy do arch-tagu GPU. Lancuch fallbacku dla
/// tagow CUDA: dokladny arch (`cuda-ampere`) → ogolny `cuda` → pierwszy wariant
/// CUDA → pierwszy jakikolwiek (+ ostrzezenie). Dzieki temu bundle deklarujacy
/// tylko ogolny `cuda` (PyPI fat wheels) dziala na kazdej karcie, a bundle z
/// per-arch wariantami (np. sglang) dostaje dokladne dopasowanie.
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
    // Tagi CUDA degraduja do ogolnego 'cuda', potem do dowolnego wariantu CUDA.
    if backend.starts_with("cuda") {
        if let Some(v) = variants.iter().find(|v| v.backend == "cuda") {
            // Brak arch-specyficznego wariantu NIE jest problemem: deploy
            // wstrzykuje dokladny TORCH_CUDA_ARCH_LIST pod wykryte GPU do
            // ogolnego wariantu 'cuda' (inject_torch_cuda_arch), wiec kernele
            // i tak kompiluja sie pod realna karte. Stad info!, nie warn!.
            tracing::info!(
                "brak arch-specyficznego wariantu '{}', uzywam ogolnego 'cuda' \
                 (+ wstrzyniety TORCH_CUDA_ARCH_LIST pod wykryte GPU)",
                backend
            );
            return Ok(Some(v));
        }
        if let Some(v) = variants.iter().find(|v| v.backend.starts_with("cuda")) {
            tracing::warn!(
                "bundle nie ma wariantu '{}' ani 'cuda' — fallback na '{}'",
                backend,
                v.backend
            );
            return Ok(Some(v));
        }
    }
    // Fallback: pierwsze dostepne, ale ostrzez.
    tracing::warn!(
        "brak wariantu dla backendu '{}', uzywam '{}' jako fallback",
        backend,
        variants[0].backend
    );
    Ok(Some(&variants[0]))
}

/// Wstrzykuje dokladny `TORCH_CUDA_ARCH_LIST` do wybranego wariantu cuda, jesli
/// host ma karty NVIDIA, a wariant SAM go nie deklaruje. Custom-kernele CUDA
/// (np. OCR quad_nms, yolox FastCOCOEvalOp) kompiluja sie tylko pod architektury
/// z tej listy — bez niej build polega na niedeterministycznej auto-detekcji
/// hosta i potrafi dac binarke bez kernela dla realnej karty ("no kernel image",
/// np. na B300). Liczymy liste z compute capability WSZYSTKICH wykrytych GPU, wiec
/// kazdy host kompiluje dokladnie pod swoja karte — jeden ogolny wariant `cuda`
/// w bundlu dziala na kazdej architekturze (B300/3090/H100/Ada/B200).
///
/// Bundle env WYGRYWA: jesli wariant juz ma `TORCH_CUDA_ARCH_LIST` (np.
/// vllm-spark `12.1a`), NIE nadpisujemy. Zwraca owned variant z wstrzyknietym env
/// (uczestniczy w `template_identity`, wiec zmiana arch wymusza rekompilacje).
fn inject_torch_cuda_arch(
    variant: Option<&InstallVariant>,
    gpu: &crate::system_check::GpuSnapshot,
    log: &LogSink,
) -> Option<InstallVariant> {
    let v = variant?;
    if !v.backend.starts_with("cuda") {
        return Some(v.clone());
    }
    if v.env.contains_key("TORCH_CUDA_ARCH_LIST") {
        return Some(v.clone());
    }
    // Brak nvidia-smi / brak compute capability -> None: nie ustawiamy nic,
    // fallback na zachowanie torcha (native cuda deploy i tak powinien miec GPU).
    let Some(arch_list) = gpu.torch_cuda_arch_list() else {
        return Some(v.clone());
    };
    log(&format!(
        "wstrzykuje TORCH_CUDA_ARCH_LIST={} (custom-kernele CUDA pod wykryte GPU)",
        arch_list
    ));
    let mut owned = v.clone();
    owned
        .env
        .insert("TORCH_CUDA_ARCH_LIST".to_string(), arch_list);
    Some(owned)
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
        (self.log)(&format!("pip: install -e {} (verbose)", path.display()));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        // `-v` jest celowo TYLKO dla install_editable. To jedyna faza ktora
        // potrafi byc cicha 20-30 min (vllm/sglang z source -> CMake -> nvcc
        // kernels). Verbose pokazuje kazdy subprocess ktory uv/pip odpala
        // (`python setup.py build`, `cmake --build`, `nvcc ...`), wiec user
        // widzi co sie dzieje w tle bez polegania tylko na heartbeacie.
        c.arg("-v");
        c.arg("-e").arg(path);
        self.run_install(&mut c)
    }
    /// Editable + `--no-build-isolation`: build widzi runtime deps juz
    /// zainstalowane w venv (setup.py importujacy wlasny pakiet, np. SearXNG).
    fn install_editable_no_build_isolation(&self, path: &Path) -> Result<()> {
        (self.log)(&format!(
            "pip: install -e {} --no-build-isolation (verbose)",
            path.display()
        ));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("-v");
        c.arg("--no-build-isolation");
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
    /// Jak wyzej, ale z `--no-deps` — buduje z torcha, lecz NIE instaluje grafu
    /// zaleznosci pakietu (dostarczamy je jawnie w `extras`).
    fn install_package_no_build_isolation_no_deps(&self, pkg: &str) -> Result<()> {
        (self.log)(&format!("pip: install --no-build-isolation --no-deps {}", pkg));
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("--no-build-isolation").arg("--no-deps").arg(pkg);
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

/// Wewnetrzne klucze arg-carrier: przenosza user-typed argi z deploy wizard
/// do build_engine_args (tam konsumowane do argv przez shlex). NIE sa realnymi
/// zmiennymi srodowiskowymi silnika — spawn_engine pomija je przy `cmd.env`,
/// inaczej vLLM warnuje "Unknown vLLM environment variable detected: VLLM_ARGS".
pub(crate) const EXTRA_ARGS_ENV_KEYS: [&str; 4] =
    ["VLLM_ARGS", "SGLANG_ARGS", "TRTLLM_ARGS", "EXTRA_ARGS"];

/// Wyodrebniona logika budowania listy args dla spawn_engine. Pozwala
/// jednostkowo testowac VLLM_ARGS/SGLANG_ARGS passthrough bez spawn'owania
/// realnego procesu.
pub(crate) fn build_engine_args(
    spec: &BundleSpec,
    env: &HashMap<String, String>,
    extra_args: &[String],
    bundle_dir: &Path,
    venv: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::with_capacity(spec.launch.args.len() + extra_args.len() + 8);
    for arg in &spec.launch.args {
        let substituted = substitute_vars_full(arg, env, bundle_dir, venv);
        // `${VAR?--flag:}` z falsy env produkuje pusty token. Pomijamy go,
        // zeby flagi typu `--enable-chunked-prefill` znikaly z CLI calkowicie
        // gdy user wylaczy je w wizardzie. Nieintuicyjne `arg.contains("${")`
        // gate chroni stare argumenty ktore mialy literal pusty string —
        // ich nie dotykamy (zachowujemy backward compat).
        if substituted.is_empty() && arg.contains("${") {
            continue;
        }
        args.push(substituted);
    }
    // VLLM_ARGS / SGLANG_ARGS / itd. z deploy wizard (Advanced section) —
    // user-typed string, wiec shlex split honoruje cudzyslowy ktore USER sam
    // postawil (np. --override-generation-config '{"max_tokens":100}'). To
    // jest jedyna sciezka ktora powinna tokenizowac przez shlex.
    for key in EXTRA_ARGS_ENV_KEYS {
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
    // Strukturalne argi budowane przez Rust (speculative-config JSON,
    // gpu-memory-utilization). Dolaczane 1:1 BEZ shlex-a — kompaktowy JSON
    // `{"model":"...","num_speculative_tokens":3}` jest pojedynczym elementem
    // Vec i MUSI przezyc nietkniety; shlex zjadlby wewnetrzne cudzyslowy.
    // `${VAR}` wewnatrz nadal podstawiamy (np. sciezki), ale bez re-tokenizacji.
    for arg in extra_args {
        args.push(substitute_vars_full(arg, env, bundle_dir, venv));
    }
    // Dedup last-wins: bundle.toml dostarcza baseline (--dtype, --max-model-len,
    // boolean --enable-x), a user VLLM_ARGS i strukturalne extra_args moga te
    // same flagi nadpisac. Bez dedupu vLLM dostaje duplikaty (warning
    // "duplicate keys") albo wręcz konflikt --enable-x / --no-enable-x.
    dedup_cli_args_last_wins(args)
}

/// Czy token wyglada jak flaga CLI (`--flag` / `--flag=value` / `-f`).
fn is_cli_flag(tok: &str) -> bool {
    tok.starts_with('-') && tok.len() > 1 && !tok.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
}

/// Czy flaga jest boolean toggle (nie konsumuje nastepnego tokenu jako wartosci).
/// Rodzina vLLM/sglang `--enable-*` / `--no-enable-*` to czyste przelaczniki —
/// gdyby konsumowaly nastepny token, sekwencja `--enable-x file --no-enable-x`
/// skasowalaby pozycjonalne `file` razem z para enable/no-enable przy last-wins.
fn is_boolean_cli_flag(tok: &str) -> bool {
    if !is_cli_flag(tok) || tok.contains('=') {
        return false;
    }
    let name = tok.trim_start_matches('-');
    name.starts_with("enable-") || name.starts_with("no-enable-")
}

/// Wyciaga kanoniczna nazwe flagi dla potrzeb dedupu. `--max-model-len=8192`
/// → `--max-model-len`. Boolean pary `--enable-foo` / `--no-enable-foo`
/// kolapsuja do wspolnego klucza `--enable-foo`, zeby ostatni wariant w argv
/// (enable albo no-enable) wygral zamiast obu naraz lecieć do silnika.
fn flag_canonical_name(tok: &str) -> Option<String> {
    if !is_cli_flag(tok) {
        return None;
    }
    let name = tok.split('=').next().unwrap_or(tok);
    let stripped = name.trim_start_matches('-');
    let canonical = stripped.strip_prefix("no-").unwrap_or(stripped);
    Some(format!("--{canonical}"))
}

/// Dedup argumentow CLI z semantyka last-wins. Obsluguje formy:
///   * `--flag value`  (wartosc to nastepny token nie bedacy flaga)
///   * `--flag=value`
///   * `--flag`         (boolean bez wartosci)
///   * pary `--enable-x` / `--no-enable-x` (wspolny klucz, ostatnie wygrywa)
/// Pozycjonalne argumenty (nie-flagi na poczatku, np. `-m module`, sciezki)
/// nie maja kanonicznej nazwy i sa zachowywane bez dedupu.
///
/// Algorytm: parsujemy argv na (klucz, segment) grupy, idziemy od TYLU
/// i zachowujemy pierwsze (czyli ostatnie w oryginalnej kolejnosci)
/// wystapienie kazdego klucza, potem odwracamy by przywrocic kolejnosc.
pub(crate) fn dedup_cli_args_last_wins(args: Vec<String>) -> Vec<String> {
    // Segment = jedna flaga z opcjonalna wartoscia, albo pojedynczy
    // pozycjonalny token. `key=None` → pozycjonalny (nigdy nie deduplikowany).
    struct Segment {
        key: Option<String>,
        tokens: Vec<String>,
    }
    let mut segments: Vec<Segment> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let tok = &args[i];
        match flag_canonical_name(tok) {
            Some(key) => {
                // `--flag=value` niesie wartosc w sobie; `--flag value`
                // konsumuje nastepny token jako wartosc TYLKO gdy flaga nie jest
                // boolean toggle (--enable-x / --no-enable-x nie maja wartosci)
                // ORAZ nastepny token nie jest sam flaga. Inaczej pozycjonalny
                // token za boolean (np. `--enable-x file`) zostalby blednie
                // wciagniety i skasowany razem z flaga przy last-wins.
                if tok.contains('=') {
                    segments.push(Segment {
                        key: Some(key),
                        tokens: vec![tok.clone()],
                    });
                    i += 1;
                } else if !is_boolean_cli_flag(tok)
                    && i + 1 < args.len()
                    && !is_cli_flag(&args[i + 1])
                {
                    segments.push(Segment {
                        key: Some(key),
                        tokens: vec![tok.clone(), args[i + 1].clone()],
                    });
                    i += 2;
                } else {
                    segments.push(Segment {
                        key: Some(key),
                        tokens: vec![tok.clone()],
                    });
                    i += 1;
                }
            }
            None => {
                segments.push(Segment {
                    key: None,
                    tokens: vec![tok.clone()],
                });
                i += 1;
            }
        }
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept_rev: Vec<Segment> = Vec::with_capacity(segments.len());
    for seg in segments.into_iter().rev() {
        match &seg.key {
            Some(k) => {
                if seen.insert(k.clone()) {
                    kept_rev.push(seg);
                }
            }
            None => kept_rev.push(seg),
        }
    }
    kept_rev.into_iter().rev().flat_map(|s| s.tokens).collect()
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

fn spawn_engine(
    venv: &Path,
    spec: &BundleSpec,
    req: &NativeDeployRequest,
    log: Option<&LogSink>,
) -> Result<Child> {
    let exe = venv_bin(venv, &spec.launch.command);
    let bundle_dir = venv.join("app");

    // Override z wizarda: gdy user nadpisal komende tekstowo, odpalamy ja
    // verbatim przez `sh -c` zamiast komendy z bundle.toml. Placeholdery
    // $MODEL/$PORT/$SERVED_MODEL_NAME rozwija powloka z env ustawionego nizej,
    // a venv/bin jest na poczatku PATH, wiec `python`/`vllm` celuja w venv.
    let launch_override = req
        .env
        .get("ENGINE_LAUNCH_CMD")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let mut cmd = if let Some(override_cmd) = launch_override {
        if let Some(log) = log {
            log("[native] launch_command_override aktywny (sh -c)");
        }
        let mut c = build_engine_command(Path::new("sh"));
        c.arg("-c").arg(override_cmd);
        c
    } else {
        let mut c = build_engine_command(&exe);
        for arg in build_engine_args(spec, &req.env, &req.extra_args, &bundle_dir, venv) {
            c.arg(arg);
        }
        c
    };
    for (k, v) in &req.env {
        // Klucze arg-carrier sa juz skonsumowane do argv przez build_engine_args
        // wyzej; nie wolno ich przekazac do env procesu silnika (vLLM warnuje
        // "Unknown vLLM environment variable detected: VLLM_ARGS").
        if EXTRA_ARGS_ENV_KEYS.contains(&k.as_str()) {
            continue;
        }
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
    let vllm_cache = crate::paths::vllm_cache_dir();
    let _ = std::fs::create_dir_all(&vllm_cache);
    // Artefakty treningu (adaptery, scalone modele, GGUF) — pod cache_dir
    // (np. /mnt/d), NIE na dysku root. Serwis ml-training czyta ARTIFACTS_ROOT.
    let artifacts = crate::paths::ml_artifacts_dir();
    let _ = std::fs::create_dir_all(&artifacts);
    for (k, v) in [
        ("HF_HOME", hf.clone().into_os_string()),
        ("HUGGINGFACE_HUB_CACHE", hf.clone().into_os_string()),
        ("TRANSFORMERS_CACHE", hf.clone().into_os_string()),
        ("TORCH_HOME", torch.clone().into_os_string()),
        ("ARTIFACTS_ROOT", artifacts.clone().into_os_string()),
        // Shared vLLM kernel cache (host path for native; Docker uses
        // CONTAINER_VLLM_CACHE_PATH from standard_engine_env). Persists
        // Triton/torch.compile/FlashInfer JIT across restarts.
        ("VLLM_CACHE_ROOT", vllm_cache.clone().into_os_string()),
        // Read-timeout (sekundy) dla huggingface_hub. Bez niego martwe/throttled
        // polaczenie z HF CDN (sockety w CLOSE-WAIT) wisi w nieskonczonosc przy
        // pobieraniu wielogigabajtowych wag. Po timeoucie hub retryuje + resume
        // zamiast czekac wiecznie. NIE wlaczamy HF_HUB_ENABLE_HF_TRANSFER —
        // wymaga pakietu hf_transfer w venv (ImportError gdy go brak).
        ("HF_HUB_DOWNLOAD_TIMEOUT", std::ffi::OsString::from("30")),
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

    // Stdout/stderr -> tee: kazda linia idzie i do GUI (LogSink, gdy obecny),
    // i do `<venv>/engine.log` dla post-mortem. `Stdio::piped()` bez aktywnego
    // readera zapchaloby bufor (~64KB) — dlatego startujemy reader threads
    // zaraz po spawn. W trybie autostartu (relaunch bez GUI) caller daje
    // `log = None` i wtedy lecimy bezposrednio do pliku, bez piped+threadow.
    let log_path = venv.join("engine.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("open engine log {}", log_path.display()))?;

    // setsid() przed exec: child staje sie liderem nowej sesji + process
    // group. Wszystkie subprocess'y ktore vLLM uvicorn parent spawn'uje
    // (EngineCore workers, multiproc DP) dziedziczą tę grupę. Bez tego
    // SIGTERM na parent zostawiał zombie engine cores trzymajace GPU
    // memory (9GB+ na 0.5B model przez fragmentacje). Stop_all_supervised
    // zabija teraz `kill(-pid)` (negative = group), co dotyka wszystkich.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = if log.is_some() {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.spawn().with_context(|| format!("spawn {:?}", exe))?
    } else {
        let log_file_err = log_file
            .try_clone()
            .context("clone engine log fd dla stderr")?;
        cmd.stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_file_err));
        return Ok(cmd.spawn().with_context(|| format!("spawn {:?}", exe))?);
    };

    let sink = log.expect("log is Some in piped branch").clone();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let file_arc = Arc::new(std::sync::Mutex::new(log_file));

    let sink_out = Arc::clone(&sink);
    let file_out = Arc::clone(&file_arc);
    std::thread::spawn(move || {
        if let Some(o) = stdout {
            let mut reader = BufReader::new(o);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break, // EOF — proces zakonczyl, pipe zamkniety naturalnie
                    Ok(_) => {
                        // Lossy: nie-UTF-8 (paski tqdm z mlx-audio) NIE moga zatrzymac
                        // drenowania. `Lines::next` walidowalby UTF-8 i `map_while`
                        // konczyl petle na pierwszym blednym bajcie — read-end pipe'a
                        // zamykal sie, a dlugozyjacy engine dostawal SIGPIPE/BrokenPipe
                        // przy nastepnym zapisie progresu na stdout (HTTP 500 przy TTS).
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']);
                        sink_out(line);
                        if let Ok(mut f) = file_out.lock() {
                            use std::io::Write;
                            let _ = writeln!(f, "{}", line);
                        }
                    }
                    Err(_) => break, // realny blad IO (pipe zamkniety) — koniec
                }
            }
        }
    });
    let sink_err = Arc::clone(&sink);
    let file_err = Arc::clone(&file_arc);
    std::thread::spawn(move || {
        if let Some(e) = stderr {
            let mut reader = BufReader::new(e);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break, // EOF — proces zakonczyl, pipe zamkniety naturalnie
                    Ok(_) => {
                        // Lossy: nie-UTF-8 NIE moze zatrzymac drenowania, inaczej
                        // zamkniecie read-endu zabija dlugozyjacy engine SIGPIPE.
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']);
                        sink_err(line);
                        if let Ok(mut f) = file_err.lock() {
                            use std::io::Write;
                            let _ = writeln!(f, "{}", line);
                        }
                    }
                    Err(_) => break, // realny blad IO (pipe zamkniety) — koniec
                }
            }
        }
    });

    // Drop our handle on the file; reader threads keep theirs via Arc and
    // close it naturally when stdout/stderr pipes hit EOF (engine exit).
    drop(file_arc);

    Ok(child)
}

/// Podstawia `${VAR}` i `${VAR:-default}` w stringu na wartosci z env+bundle_dir.
/// Test-only convenience wrapper — production code uses
/// `substitute_vars_full` z explicit `venv_dir`.
#[cfg(test)]
fn substitute_vars(s: &str, env: &HashMap<String, String>, bundle_dir: &Path) -> String {
    substitute_vars_full(s, env, bundle_dir, Path::new(""))
}

/// Substitution syntax dla bundle.toml `[launch] args`. Obslugiwane formy:
///   * `${VAR}` — wartosc env, pusty string gdy brak.
///   * `${VAR:-default}` — wartosc env lub default.
///   * `${VAR?yes:no}` — ternary on truthy: `yes` gdy env jest truthy
///     (1/true/yes/on/enabled, case-insensitive), `no` gdy falsy
///     (0/false/no/off/disabled, puste, brak env).
///   * `${VAR?--flag:}` — specjalizacja: `--flag` gdy truthy, empty string
///     gdy falsy. Empty token jest filtrowany z args list w
///     `build_engine_args` (single-line `${...}` produkujace tylko empty
///     daje pusty token; wieksze stringi z embedded `${...?:}` daja zwykla
///     pusta wartosc w srodku).
///
/// Special tokens: `${BUNDLE_DIR}` → bundle path, `${VENV_DIR}` → venv path.
///
/// Brak escape sequences. Brak nesting (`${A:-${B}}` → DeployError przez
/// regex check). Malformed truthy/falsy values (np. env=`"banan"`) → empty
/// string jako falsy fallback (defensive — nie hard-fail).
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

        let value = if let Some((name, branches)) = inner.split_once('?') {
            // ${VAR?yes:no} ternary
            let (yes_branch, no_branch) = branches.split_once(':').unwrap_or((branches, ""));
            let env_value = lookup_env(name, env, &bundle_dir_str, &venv_dir_str);
            if is_truthy(&env_value) {
                yes_branch.to_string()
            } else {
                no_branch.to_string()
            }
        } else if let Some((name, default)) = inner.split_once(":-") {
            // ${VAR:-default}
            let env_value = lookup_env(name, env, &bundle_dir_str, &venv_dir_str);
            if env_value.is_empty() {
                default.to_string()
            } else {
                env_value
            }
        } else {
            // ${VAR}
            lookup_env(inner, env, &bundle_dir_str, &venv_dir_str)
        };

        out.replace_range(start..=end, &value);
    }
    out
}

/// Lookup env var z fallback do special tokens (BUNDLE_DIR, VENV_DIR).
/// Empty string gdy brak.
fn lookup_env(
    name: &str,
    env: &HashMap<String, String>,
    bundle_dir_str: &str,
    venv_dir_str: &str,
) -> String {
    match name {
        "BUNDLE_DIR" => bundle_dir_str.to_string(),
        "VENV_DIR" => venv_dir_str.to_string(),
        _ => env.get(name).cloned().unwrap_or_default(),
    }
}

/// Czy wartosc jest "truthy" dla ternary substitution. Lista zamknieta —
/// reszta = falsy. Case-insensitive dla wygody (user moze pisac `True`).
fn is_truthy(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    )
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
    let child_pid = child.id();
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

    // Heartbeat — emit kazde 30s wskazujac ze subprocess wciaz zyje + co
    // konkretnie teraz robi (lista descendants z RSS). Krytyczne dla
    // dlugich `pip install -e` (kompilacja CUDA kerneli moze siedziec cicho
    // przez 20-30 min). Wątek wisi na flagstop ustawianej po `wait()`.
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let cb_hb = Arc::clone(log_cb);
    let hb_handle = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let interval = std::time::Duration::from_secs(30);
        let mut next_tick = start + interval;
        loop {
            if stop_t.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            let now = std::time::Instant::now();
            if now < next_tick {
                continue;
            }
            next_tick = now + interval;
            let elapsed = now.duration_since(start);
            let mins = elapsed.as_secs() / 60;
            let secs = elapsed.as_secs() % 60;
            let descendants = collect_descendant_summary(child_pid);
            let summary = if descendants.is_empty() {
                "(brak aktywnych pod-procesow)".to_string()
            } else {
                descendants
            };
            cb_hb(&format!(
                "[heartbeat] elapsed={:02}:{:02} active: {}",
                mins, secs, summary
            ));
        }
    });

    let status = child.wait().with_context(|| format!("wait {}", program))?;
    stop.store(true, Ordering::Relaxed);
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();
    let _ = hb_handle.join();
    if !status.success() {
        anyhow::bail!("{} zwrocilo kod {}", program, status);
    }
    Ok(())
}

/// Zbiera listę aktywnych descendants procesu `root_pid` z czytelnym
/// streszczeniem `comm(rss_mb)`. Linux-only — na innych OS zwraca pusty
/// string (heartbeat dalej leci, tylko bez listy podprocesów). Używa `ps`,
/// bo to jedyny sposób bez doinstalowania zewnętrznych crate'ów.
fn collect_descendant_summary(root_pid: u32) -> String {
    #[cfg(target_os = "linux")]
    {
        // ps -e -o pid,ppid,rss,comm — RSS w KB. Jedna kolumna comm na koncu.
        let out = Command::new("ps")
            .args(["-e", "-o", "pid=,ppid=,rss=,comm="])
            .output();
        let stdout = match out {
            Ok(o) if o.status.success() => o.stdout,
            _ => return String::new(),
        };
        let text = String::from_utf8_lossy(&stdout);
        // Build pid -> (ppid, rss_kb, comm)
        let mut by_pid: std::collections::HashMap<u32, (u32, u64, String)> =
            std::collections::HashMap::new();
        for line in text.lines() {
            let mut it = line.split_ascii_whitespace();
            let pid: u32 = match it.next().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let ppid: u32 = match it.next().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let rss: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let comm: String = it.collect::<Vec<_>>().join(" ");
            if comm.is_empty() {
                continue;
            }
            by_pid.insert(pid, (ppid, rss, comm));
        }
        // BFS od root_pid przez ppid relację.
        let mut active = std::collections::HashSet::new();
        active.insert(root_pid);
        let mut changed = true;
        while changed {
            changed = false;
            for (pid, (ppid, _, _)) in &by_pid {
                if active.contains(ppid) && !active.contains(pid) {
                    active.insert(*pid);
                    changed = true;
                }
            }
        }
        // Render — tylko descendants (pomijamy root_pid), max 6 wpisów po
        // RSS malejaco zeby linia heartbeat nie spuchla.
        let mut entries: Vec<(u64, String)> = active
            .iter()
            .filter(|p| **p != root_pid)
            .filter_map(|p| by_pid.get(p).map(|(_, rss, comm)| (*rss, comm.clone())))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let max = 6;
        let truncated_count = entries.len().saturating_sub(max);
        entries.truncate(max);
        let parts: Vec<String> = entries
            .into_iter()
            .map(|(rss_kb, comm)| format!("{}({}MB)", comm, rss_kb / 1024))
            .collect();
        if truncated_count > 0 {
            format!("{} +{} more", parts.join(", "), truncated_count)
        } else {
            parts.join(", ")
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root_pid;
        String::new()
    }
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
    fn substitute_ternary_truthy() {
        let mut env = HashMap::new();
        env.insert("ENABLE_PREFIX".to_string(), "true".to_string());
        let s = substitute_vars(
            "${ENABLE_PREFIX?--enable-prefix-caching:}",
            &env,
            Path::new("/tmp/b"),
        );
        assert_eq!(s, "--enable-prefix-caching");
    }

    #[test]
    fn substitute_ternary_falsy() {
        let mut env = HashMap::new();
        env.insert("ENABLE_PREFIX".to_string(), "false".to_string());
        let s = substitute_vars(
            "${ENABLE_PREFIX?--enable-prefix-caching:}",
            &env,
            Path::new("/tmp/b"),
        );
        assert_eq!(s, "");
    }

    #[test]
    fn substitute_ternary_missing_env_is_falsy() {
        let env = HashMap::new();
        let s = substitute_vars(
            "${ENABLE_PREFIX?--enable-prefix-caching:}",
            &env,
            Path::new("/tmp/b"),
        );
        assert_eq!(s, "");
    }

    #[test]
    fn substitute_ternary_yes_no_branches() {
        let mut env = HashMap::new();
        env.insert("MODE".to_string(), "yes".to_string());
        let s = substitute_vars("--dtype=${MODE?fp16:fp8}", &env, Path::new("/tmp/b"));
        assert_eq!(s, "--dtype=fp16");
        env.insert("MODE".to_string(), "no".to_string());
        let s2 = substitute_vars("--dtype=${MODE?fp16:fp8}", &env, Path::new("/tmp/b"));
        assert_eq!(s2, "--dtype=fp8");
    }

    #[test]
    fn substitute_ternary_all_truthy_aliases() {
        for alias in [
            "1", "true", "True", "TRUE", "yes", "YES", "on", "ON", "enabled", "Enabled",
        ] {
            let mut env = HashMap::new();
            env.insert("FLAG".to_string(), alias.to_string());
            let s = substitute_vars("${FLAG?yes:no}", &env, Path::new("/tmp/b"));
            assert_eq!(s, "yes", "expected truthy for alias '{}'", alias);
        }
    }

    #[test]
    fn substitute_ternary_falsy_aliases() {
        for alias in [
            "0", "false", "False", "no", "NO", "off", "disabled", "", "garbage",
        ] {
            let mut env = HashMap::new();
            env.insert("FLAG".to_string(), alias.to_string());
            let s = substitute_vars("${FLAG?yes:no}", &env, Path::new("/tmp/b"));
            assert_eq!(s, "no", "expected falsy for alias '{}'", alias);
        }
    }

    #[test]
    fn build_engine_args_filters_empty_ternary_tokens() {
        let mut spec = vllm_bundle_spec();
        spec.launch.args = vec![
            "-m".to_string(),
            "vllm.entrypoints.openai.api_server".to_string(),
            "--port".to_string(),
            "${PORT:-8000}".to_string(),
            "${ENABLE_CHUNKED?--enable-chunked-prefill:}".to_string(),
        ];
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "5001".to_string());
        env.insert("ENABLE_CHUNKED".to_string(), "false".to_string());
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
        // Empty token z falsy ternary jest filtrowany.
        assert_eq!(
            args,
            vec!["-m", "vllm.entrypoints.openai.api_server", "--port", "5001"]
        );

        // Truthy ternary daje --enable-chunked-prefill jako oddzielny token.
        env.insert("ENABLE_CHUNKED".to_string(), "true".to_string());
        let args2 = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
        assert_eq!(
            args2,
            vec![
                "-m",
                "vllm.entrypoints.openai.api_server",
                "--port",
                "5001",
                "--enable-chunked-prefill"
            ]
        );
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
                editable_no_build_isolation: false,
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

        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));

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
    fn arg_carrier_keys_consumed_to_argv_not_passed_as_env() {
        // VLLM_ARGS jest skonsumowane do argv przez build_engine_args, ale spawn_engine
        // pomija je przy cmd.env (filtr po EXTRA_ARGS_ENV_KEYS), zeby vLLM nie warnowal
        // "Unknown vLLM environment variable detected: VLLM_ARGS".
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        env.insert("VLLM_ARGS".to_string(), "--tensor-parallel-size 2".into());

        // build_engine_args nadal czyta VLLM_ARGS i buduje argv.
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
        assert!(args.contains(&"--tensor-parallel-size".to_string()));
        assert!(args.contains(&"2".to_string()));

        // Filtr env z spawn_engine usuwa klucze arg-carrier, zostawia reszte.
        let spawn_env: HashMap<&str, &str> = env
            .iter()
            .filter(|(k, _)| !EXTRA_ARGS_ENV_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert!(!spawn_env.contains_key("VLLM_ARGS"));
        assert_eq!(spawn_env.get("MODEL"), Some(&"test"));
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
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
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
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
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
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
        assert!(args.contains(&"--mem-fraction-static".to_string()));
        assert!(args.contains(&"0.85".to_string()));
        assert!(args.contains(&"--tp".to_string()));
    }

    #[test]
    fn build_engine_args_keeps_speculative_json_as_single_element() {
        // KLUCZOWE: JSON `--speculative-config {...}` jako element extra_args
        // MUSI przezyc jako jeden token z nietknietymi wewnetrznymi
        // cudzyslowami. To jest bug z produkcji — shlex/round-trip przez
        // VLLM_ARGS zjadal cudzyslowy i vLLM padal na "cannot be converted".
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "TentaFlow/Bielik-1.5B-NVFP4".into());
        let json = r#"{"model":"TentaFlow/Bielik-1.5B-NVFP4","num_speculative_tokens":3}"#;
        let extra = vec!["--speculative-config".to_string(), json.to_string()];
        let args = build_engine_args(
            &spec,
            &env,
            &extra,
            Path::new("/tmp/b"),
            Path::new("/tmp/v"),
        );
        // Flaga obecna i bezposrednio po niej caly nietkniety JSON.
        let pos = args
            .iter()
            .position(|a| a == "--speculative-config")
            .expect("flaga --speculative-config musi byc w argv");
        assert_eq!(
            args[pos + 1],
            json,
            "JSON musi byc jednym nietknietym elementem, dostalem: {:?}",
            args
        );
    }

    #[test]
    fn build_engine_args_extra_args_not_shlex_split() {
        // extra_args nie przechodza przez shlex — element z apostrofami w
        // srodku zostaje jednym tokenem (gdyby szedl przez shlex, '{' zostalby
        // zjedzony / rozbity).
        let spec = vllm_bundle_spec();
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        let extra = vec![
            "--speculative-config".to_string(),
            r#"{"method":"ngram","num_speculative_tokens":3}"#.to_string(),
        ];
        let args = build_engine_args(
            &spec,
            &env,
            &extra,
            Path::new("/tmp/b"),
            Path::new("/tmp/v"),
        );
        assert!(args
            .iter()
            .any(|a| a == r#"{"method":"ngram","num_speculative_tokens":3}"#));
    }

    #[test]
    fn dedup_last_wins_space_separated() {
        let args = vec![
            "--dtype".into(),
            "auto".into(),
            "--max-model-len".into(),
            "8192".into(),
            "--max-model-len".into(),
            "16384".into(),
        ];
        let out = dedup_cli_args_last_wins(args);
        assert_eq!(out, vec!["--dtype", "auto", "--max-model-len", "16384"]);
    }

    #[test]
    fn dedup_last_wins_equals_form() {
        let args = vec![
            "--gpu-memory-utilization=0.90".into(),
            "--dtype".into(),
            "auto".into(),
            "--gpu-memory-utilization".into(),
            "0.70".into(),
        ];
        let out = dedup_cli_args_last_wins(args);
        assert_eq!(
            out,
            vec!["--dtype", "auto", "--gpu-memory-utilization", "0.70"]
        );
    }

    #[test]
    fn dedup_last_wins_boolean_enable_no_enable_pair() {
        // --enable-x i --no-enable-x kolapsuja do wspolnego klucza, ostatni
        // wygrywa — eliminuje konflikt ktory vLLM odrzucal.
        let args = vec![
            "--enable-flashinfer-autotune".into(),
            "--dtype".into(),
            "auto".into(),
            "--no-enable-flashinfer-autotune".into(),
        ];
        let out = dedup_cli_args_last_wins(args);
        assert_eq!(
            out,
            vec!["--dtype", "auto", "--no-enable-flashinfer-autotune"]
        );
    }

    #[test]
    fn dedup_boolean_flag_does_not_eat_positional() {
        // `--enable-x file --no-enable-x`: boolean flaga NIE zjada `file`.
        // Pozycjonalny `file` zostaje, a z pary enable/no-enable wygrywa
        // ostatni (--no-enable-x) przez last-wins.
        let args = vec![
            "--enable-prefix-caching".into(),
            "file".into(),
            "--no-enable-prefix-caching".into(),
        ];
        let out = dedup_cli_args_last_wins(args);
        assert!(
            out.iter().any(|a| a == "file"),
            "pozycjonalny zniknal: {out:?}"
        );
        assert!(
            out.iter().any(|a| a == "--no-enable-prefix-caching"),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|a| a == "--enable-prefix-caching"),
            "{out:?}"
        );
        assert_eq!(out, vec!["file", "--no-enable-prefix-caching"]);
    }

    #[test]
    fn dedup_pure_boolean_pair_last_wins() {
        // Czysta para boolean bez pozycjonalnego — last-wins, jeden token.
        let args = vec![
            "--enable-prefix-caching".into(),
            "--no-enable-prefix-caching".into(),
        ];
        let out = dedup_cli_args_last_wins(args);
        assert_eq!(out, vec!["--no-enable-prefix-caching"]);
    }

    #[test]
    fn dedup_preserves_positional_and_negative_values() {
        // Pozycjonalne (-m module) i wartosci ujemne nie sa traktowane jak
        // flagi do dedupu.
        let args = vec![
            "-m".into(),
            "vllm.entrypoints.openai.api_server".into(),
            "--seed".into(),
            "-1".into(),
        ];
        let out = dedup_cli_args_last_wins(args.clone());
        assert_eq!(out, args);
    }

    #[test]
    fn dedup_does_not_touch_json_value() {
        // JSON jako wartosc flagi nie jest re-tokenizowany ani gubiony.
        let json = r#"{"model":"x","num_speculative_tokens":3}"#;
        let args = vec!["--speculative-config".into(), json.to_string()];
        let out = dedup_cli_args_last_wins(args);
        assert_eq!(out, vec!["--speculative-config", json]);
    }

    #[test]
    fn build_engine_args_dedup_user_overrides_bundle() {
        // bundle.toml ma --max-model-len 8192, user VLLM_ARGS nadpisuje 32768.
        // Last-wins → tylko jedna flaga z wartoscia usera.
        let mut spec = vllm_bundle_spec();
        spec.launch.args = vec![
            "-m".into(),
            "vllm.entrypoints.openai.api_server".into(),
            "--max-model-len".into(),
            "8192".into(),
        ];
        let mut env = HashMap::new();
        env.insert("MODEL".to_string(), "test".into());
        env.insert("VLLM_ARGS".to_string(), "--max-model-len 32768".into());
        let args = build_engine_args(&spec, &env, &[], Path::new("/tmp/b"), Path::new("/tmp/v"));
        let count = args.iter().filter(|a| *a == "--max-model-len").count();
        assert_eq!(
            count, 1,
            "tylko jedna --max-model-len, dostalem: {:?}",
            args
        );
        let pos = args.iter().position(|a| a == "--max-model-len").unwrap();
        assert_eq!(args[pos + 1], "32768");
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
                extras_no_build_isolation_no_deps: vec![],
                install_hint: None,
                env: HashMap::new(),
                force_pins: vec![],
            },
            InstallVariant {
                backend: "rocm".into(),
                extra_index: Some("b".into()),
                extras: vec![],
                extras_no_build_isolation: vec![],
                extras_no_build_isolation_no_deps: vec![],
                install_hint: None,
                env: HashMap::new(),
                force_pins: vec![],
            },
            InstallVariant {
                backend: "metal".into(),
                extra_index: None,
                extras: vec!["vllm-metal".into()],
                extras_no_build_isolation: vec![],
                extras_no_build_isolation_no_deps: vec![],
                install_hint: None,
                env: HashMap::new(),
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
    fn pick_variant_cuda_arch_fallback_chain() {
        let mk = |backend: &str| InstallVariant {
            backend: backend.into(),
            extra_index: None,
            extras: vec![],
            extras_no_build_isolation: vec![],
            extras_no_build_isolation_no_deps: vec![],
            install_hint: None,
            env: HashMap::new(),
            force_pins: vec![],
        };
        // Per-arch wariant ma pierwszenstwo.
        let with_arch = vec![mk("cuda"), mk("cuda-ampere")];
        assert_eq!(
            pick_install_variant(&with_arch, "cuda-ampere")
                .unwrap()
                .unwrap()
                .backend,
            "cuda-ampere"
        );
        // Brak per-arch → degraduje do ogolnego 'cuda'.
        let generic_only = vec![mk("cuda"), mk("rocm")];
        assert_eq!(
            pick_install_variant(&generic_only, "cuda-ampere")
                .unwrap()
                .unwrap()
                .backend,
            "cuda"
        );
        // Spark tez degraduje do 'cuda'.
        assert_eq!(
            pick_install_variant(&generic_only, "cuda-spark")
                .unwrap()
                .unwrap()
                .backend,
            "cuda"
        );
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

    /// template_id musi byc niezmienniczy na zmiany sekcji `[launch]` (flagi
    /// runtime jak `--served-model-name`) — inaczej kazda zmiana flagi kasuje
    /// zbudowany venv i wymusza 20-30 min rekompilacji kerneli. Zmiana wejsc
    /// build (requirements.lock, install_variants.env) MUSI zmieniac id.
    #[test]
    fn template_identity_ignores_launch_but_tracks_build_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("requirements.lock");
        std::fs::write(&lock, "vllm==0.21.0\n").unwrap();
        // bundle.toml jest pomijany w hashu — jego tresc nie powinna ruszac id.
        std::fs::write(
            dir.path().join("bundle.toml"),
            "[launch]\nargs = [\"--model\", \"x\"]\n",
        )
        .unwrap();

        let base = vllm_bundle_spec();
        let id0 = template_identity(&base, None, dir.path()).unwrap();

        // 1) Zmiana launch args (spec) — ten sam id.
        let mut launch_changed = vllm_bundle_spec();
        launch_changed
            .launch
            .args
            .push("--served-model-name".to_string());
        launch_changed.launch.args.push("bielik".to_string());
        let id_launch = template_identity(&launch_changed, None, dir.path()).unwrap();
        assert_eq!(id0, id_launch, "launch args nie moga zmieniac template_id");

        // 2) Zmiana tresci bundle.toml (pominiety plik) — ten sam id.
        std::fs::write(
            dir.path().join("bundle.toml"),
            "[launch]\nargs = [\"--served-model-name\", \"bielik\"]\n",
        )
        .unwrap();
        let id_toml = template_identity(&base, None, dir.path()).unwrap();
        assert_eq!(
            id0, id_toml,
            "tresc bundle.toml nie moze zmieniac template_id"
        );

        // 3) Zmiana requirements.lock — INNY id.
        std::fs::write(&lock, "vllm==0.21.1\n").unwrap();
        let id_lock = template_identity(&base, None, dir.path()).unwrap();
        assert_ne!(id0, id_lock, "zmiana requirements.lock musi zmienic id");

        // 4) Zmiana install_variants.env (np. TORCH_CUDA_ARCH_LIST) — INNY id.
        std::fs::write(&lock, "vllm==0.21.0\n").unwrap();
        let mut env = HashMap::new();
        env.insert("TORCH_CUDA_ARCH_LIST".to_string(), "12.1a".to_string());
        let variant = InstallVariant {
            backend: "cuda-spark".to_string(),
            extra_index: None,
            extras: vec![],
            extras_no_build_isolation: vec![],
            extras_no_build_isolation_no_deps: vec![],
            install_hint: None,
            env,
            force_pins: vec![],
        };
        let id_env = template_identity(&base, Some(&variant), dir.path()).unwrap();
        assert_ne!(id0, id_env, "zmiana install_variants.env musi zmienic id");
    }
}
