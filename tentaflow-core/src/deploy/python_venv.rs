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
    /// Podkatalog w sklonowanym repo gdzie lezy pyproject/setup.py
    /// (np. SGLang trzyma package w `python/`). Pusty = root.
    #[serde(default)]
    pub install_subdir: Option<String>,
    /// "editable" (domyslne, pip install -e .) lub "requirements_txt"
    /// (tylko pip install -r requirements.txt — dla ComfyUI co nie jest
    /// package, uruchamia sie przez python main.py).
    #[serde(default)]
    pub install_mode: Option<String>,
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
    let extracted = tempfile::tempdir()?;
    super::bundle::extract_to(extracted.path())?;
    let spec = read_bundle_spec(extracted.path(), engine)?;
    check_platform_compat(&spec.requires)?;

    let detected = crate::system_check::collect();
    let backend_name = backend_to_str(&detected.gpu.preferred_backend);
    let variant = pick_install_variant(&spec.install_variants, backend_name)?;
    tracing::info!(engine=%engine, backend=%backend_name, "Bootstrap python bundle");

    let cache = cache_root()?;
    let python_bin = ensure_python(&cache, &spec.bundle.python_version)?;
    let uv_bin = ensure_uv(&cache).ok();

    let venv_dir = cache.join("envs").join(engine);
    let bundle_src = extracted
        .path()
        .join("tentaflow-containers/python-bundles")
        .join(engine);

    create_venv(&python_bin, &venv_dir)?;
    install_deps(&venv_dir, &uv_bin, &spec, variant, &bundle_src)?;
    copy_bundle_files(&bundle_src, &venv_dir)?;

    Ok(BootstrappedEngine {
        engine: engine.to_string(),
        venv_dir,
        python_bin,
        internal_port: spec.launch.internal_port,
    })
}

/// Glowna funkcja. Odpowiada tentaflow-core::deploy::docker::deploy() ale
/// dla Pythona bez kontenera.
pub fn deploy(req: &NativeDeployRequest) -> Result<RunningEngine> {
    let extracted = tempfile::tempdir()?;
    super::bundle::extract_to(extracted.path())?;
    let spec = read_bundle_spec(extracted.path(), &req.engine)?;

    check_platform_compat(&spec.requires)?;

    // Wykryj backend (CUDA/ROCm/Metal/XPU) i wybierz odpowiedni variant.
    let detected = crate::system_check::collect();
    let backend_name = backend_to_str(&detected.gpu.preferred_backend);
    let variant = pick_install_variant(&spec.install_variants, backend_name)?;
    tracing::info!(engine=%req.engine, backend=%backend_name, "Wybrany wariant instalacji");

    let cache = cache_root()?;
    let python_bin = ensure_python(&cache, &spec.bundle.python_version)?;
    let uv_bin = ensure_uv(&cache).ok();

    let venv_dir = cache.join("envs").join(&req.engine);
    let bundle_src = extracted
        .path()
        .join("tentaflow-containers/python-bundles")
        .join(&req.engine);

    create_venv(&python_bin, &venv_dir)?;
    install_deps(&venv_dir, &uv_bin, &spec, variant, &bundle_src)?;
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
fn ensure_python(cache: &Path, py_ver: &str) -> Result<PathBuf> {
    let target_dir = cache.join("python").join(py_ver);
    let python_bin = python_bin_path(&target_dir);
    if python_bin.exists() {
        return Ok(python_bin);
    }

    let triple = pbs_triple()
        .with_context(|| format!("nie znam PBS triple dla {}-{}", std::env::consts::OS, std::env::consts::ARCH))?;
    let full_ver = resolve_full_python_version(py_ver);
    let date = pbs_date();
    let url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{date}/cpython-{ver}+{date}-{triple}-install_only.tar.gz",
        date = date, ver = full_ver, triple = triple
    );

    tracing::info!(url = %url, "Pobieram python-build-standalone");
    std::fs::create_dir_all(&target_dir)?;
    download_and_extract(&url, &target_dir)?;

    if !python_bin.exists() {
        anyhow::bail!(
            "po wypakowaniu python-build-standalone nie znalazlem {:?}",
            python_bin
        );
    }
    Ok(python_bin)
}

/// Zapewnia binarke `uv` w `<cache>/bin/uv`. Reuse jesli juz jest.
fn ensure_uv(cache: &Path) -> Result<PathBuf> {
    let bin_dir = cache.join("bin");
    let uv_name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let uv_path = bin_dir.join(uv_name);
    if uv_path.exists() {
        return Ok(uv_path);
    }
    std::fs::create_dir_all(&bin_dir)?;

    let triple = uv_triple()
        .context("nie znam uv target triple dla tej platformy")?;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let url = format!(
        "https://github.com/astral-sh/uv/releases/download/{ver}/uv-{triple}.{ext}",
        ver = UV_VERSION, triple = triple, ext = ext
    );

    tracing::info!(url = %url, "Pobieram uv");
    download_and_extract(&url, &bin_dir)?;

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
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&p) {
                for ie in inner.flatten() { out.push(ie.path()); }
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
        other  => other.to_string(),
    }
}

fn pbs_date() -> String {
    std::env::var("TENTAFLOW_PBS_DATE").unwrap_or_else(|_| PBS_DATE.to_string())
}

fn pbs_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux",  "x86_64")  => Some("x86_64-unknown-linux-gnu"),
        ("linux",  "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos",  "aarch64") => Some("aarch64-apple-darwin"),
        ("macos",  "x86_64")  => Some("x86_64-apple-darwin"),
        ("windows","x86_64")  => Some("x86_64-pc-windows-msvc-shared"),
        _ => None,
    }
}

fn uv_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux",   "x86_64")  => Some("x86_64-unknown-linux-gnu"),
        ("linux",   "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos",   "aarch64") => Some("aarch64-apple-darwin"),
        ("macos",   "x86_64")  => Some("x86_64-apple-darwin"),
        ("windows", "x86_64")  => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// Pobiera i rozpakowuje archiwum tar.gz / zip do docelowego katalogu.
/// Blocking; wolamy synchronicznie z thread pool (deploy to rzadka operacja).
fn download_and_extract(url: &str, dst: &Path) -> Result<()> {
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

fn create_venv(python: &Path, venv: &Path) -> Result<()> {
    if venv.join("pyvenv.cfg").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(venv.parent().unwrap()).ok();
    run(Command::new(python).args(["-m", "venv", venv.to_str().unwrap()]))
        .context("tworzenie venv")
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
) -> Result<()> {
    let extra_index = variant.and_then(|v| v.extra_index.clone());
    let installer = Installer::new(venv, uv.as_deref(), extra_index);
    // setuptools>=77 wymagane zeby VoxCPM / niektore nowe pyproject.toml
    // z `license = "MIT"` (string form, PEP 639) sie instalowaly.
    installer.upgrade_pip()?;

    let lock = bundle_src.join("requirements.lock");
    if lock.exists() {
        installer.install_requirements(&lock).context("install lock")?;
    }

    // Extras (wymagajace tylko pypi — accelerate, vllm-metal, nemo_toolkit itp.).
    // Pakiety z `extras_no_build_isolation` beda zainstalowane pozniej, juz po
    // glownym pakiecie (kiedy torch jest obecny).
    if let Some(v) = variant {
        for extra in &v.extras {
            installer.install_package(extra)
                .with_context(|| format!("install extra {}", extra))?;
        }
    }

    match spec.bundle.source.as_str() {
        "pypi" => {
            let pkg = spec.bundle.pypi_package.as_deref().unwrap_or(&spec.bundle.engine);
            installer.install_package(pkg)
                .with_context(|| format!("install {}", pkg))?;
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
                "editable" => installer.install_editable(&pkg_dir).context("install -e .")?,
                "requirements_txt" => {
                    let req = pkg_dir.join("requirements.txt");
                    if !req.exists() {
                        anyhow::bail!("install_mode=requirements_txt a brak {}", req.display());
                    }
                    installer.install_requirements(&req).context("install -r requirements.txt")?;
                }
                other => anyhow::bail!("nieznany install_mode: {}", other),
            }
        }
        other => anyhow::bail!("nieznane source: {}", other),
    }

    // Teraz torch jest zainstalowany (z glownego pakietu jego deps).
    // Instalujemy extras ktore wymagaja torcha do buildu kerneli CUDA.
    if let Some(v) = variant {
        for extra in &v.extras_no_build_isolation {
            installer.install_package_no_build_isolation(extra)
                .with_context(|| format!("install {} (no-build-isolation)", extra))?;
        }
    }

    Ok(())
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
    if !pj.exists() { return Ok(()); }
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
                        if inner.contains('}') { break; }
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
        Xpu  => "xpu",
        Metal => "metal",
        Cpu  => "cpu",
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
        backend, variants[0].backend
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
}

impl<'a> Installer<'a> {
    fn new(venv: &Path, uv: Option<&'a Path>, extra_index_url: Option<String>) -> Self {
        Self { venv: venv.to_path_buf(), uv, extra_index_url }
    }
    fn cmd(&self) -> Command {
        if let Some(uv) = self.uv {
            let mut c = Command::new(uv);
            c.env("VIRTUAL_ENV", &self.venv);
            c.arg("pip");
            c
        } else {
            let pip = venv_bin(&self.venv, "pip");
            Command::new(pip)
        }
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
        let mut c = self.cmd();
        c.arg("install").arg("--upgrade").arg("pip").arg("wheel").arg("setuptools>=77");
        run(&mut c)
    }
    fn install_requirements(&self, path: &Path) -> Result<()> {
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("-r").arg(path);
        run(&mut c)
    }
    fn install_package(&self, pkg: &str) -> Result<()> {
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg(pkg);
        run(&mut c)
    }
    fn install_editable(&self, path: &Path) -> Result<()> {
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("-e").arg(path);
        run(&mut c)
    }
    /// Instalacja z wylaczona izolacja buildu (`--no-build-isolation`) —
    /// pakiet ma dostep do zainstalowanego torcha podczas budowy natywnych
    /// kerneli. Wymagane dla flash-attn, niektorych wariantow xformers itp.
    fn install_package_no_build_isolation(&self, pkg: &str) -> Result<()> {
        let mut c = self.cmd();
        c.arg("install");
        self.add_index(&mut c);
        self.add_install_flags(&mut c);
        c.arg("--no-build-isolation").arg(pkg);
        run(&mut c)
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

fn spawn_engine(venv: &Path, spec: &BundleSpec, req: &NativeDeployRequest) -> Result<Child> {
    let exe = venv_bin(venv, &spec.launch.command);
    let bundle_dir = venv.join("app");

    let mut cmd = Command::new(&exe);
    for arg in &spec.launch.args {
        cmd.arg(substitute_vars_full(arg, &req.env, &bundle_dir, venv));
    }
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    cmd.env("BUNDLE_DIR", &bundle_dir);
    cmd.env("VENV_DIR", venv);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let child = cmd.spawn()
        .with_context(|| format!("spawn {:?}", exe))?;
    Ok(child)
}

/// Podstawia `${VAR}` i `${VAR:-default}` w stringu na wartosci z env+bundle_dir.
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
        let Some(end_rel) = out[start..].find('}') else { break };
        let end = start + end_rel;
        let inner = &out[start + 2..end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d.to_string())),
            None => (inner, None),
        };
        let value = match name {
            "BUNDLE_DIR" => bundle_dir_str.clone(),
            "VENV_DIR" => venv_dir_str.clone(),
            _ => env.get(name).cloned().unwrap_or_else(|| default.unwrap_or_default()),
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
    fn pick_variant_matches_backend() {
        let variants = vec![
            InstallVariant { backend: "cuda".into(), extra_index: Some("a".into()), extras: vec![], install_hint: None },
            InstallVariant { backend: "rocm".into(), extra_index: Some("b".into()), extras: vec![], install_hint: None },
            InstallVariant { backend: "metal".into(), extra_index: None, extras: vec!["vllm-metal".into()], install_hint: None },
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
}
