// =============================================================================
// File: vision/ort_common.rs — shared ONNX Runtime session plumbing (ort)
// =============================================================================
//
// Wspolna warstwa `ort` dla wszystkich silnikow CV opartych o ONNX Runtime
// (detektor RF-DETR i generyczny runner `onnx-cv`): lokalizacja dylibu
// onnxruntime (native-libs → systemowy fallback) oraz budowa sesji z lancuchem
// execution providerow TensorRT→CUDA→(CoreML)→CPU. Wydzielone z
// `detector_rfdetr.rs`, zeby dynamiczne modele z rejestru `vision_models`
// dzielily dokladnie te sama sciezke wydajnosci.

#![cfg(feature = "inference-supertonic")]

use anyhow::{anyhow, Result};
use tracing::{info, warn};

/// Domyślna ścieżka biblioteki ONNX Runtime dla dlopen (`ort` z `load-dynamic`),
/// gdy `ORT_DYLIB_PATH` nie jest ustawione w środowisku.
const DEFAULT_ORT_DYLIB: &str = "/usr/lib/libonnxruntime.so.1.24.4";

/// Ustawia `ORT_DYLIB_PATH` na wykrytą ścieżkę, jeśli nie ma jej w środowisku —
/// `ort` z `load-dynamic` dlopuje onnxruntime spod tej zmiennej przy pierwszym
/// użyciu. Preferujemy runtime z drzewa `native-libs/` (zawiera providery
/// TensorRT i CUDA), a dopiero gdy go brak — systemowy [`DEFAULT_ORT_DYLIB`]
/// (który ma zwykle tylko CUDA). Edycja 2021: `set_var` jest bezpieczne.
pub fn ensure_ort_dylib() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    let path = locate_ort_dylib().unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_ORT_DYLIB));
    std::env::set_var("ORT_DYLIB_PATH", &path);
}

/// Szuka `libonnxruntime.{so*,dylib}` w drzewie `native-libs/<platform>/lib-dynamic/`
/// (build-all.sh provisionuje tam runtime GPU z TensorRT). Lustrzana logika do
/// `services::document::rasterize::locate_pdfium_library`, ale zawężona do runtime
/// ONNX. Zwraca pierwszy trafiony plik albo `None` (wtedy caller bierze systemowy).
fn locate_ort_dylib() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    let (platform, lib_glob): (&str, &[&str]) = if cfg!(target_os = "macos") {
        (
            if cfg!(target_arch = "aarch64") { "macos-arm64" } else { "macos-x86_64" },
            &["libonnxruntime.dylib"],
        )
    } else if cfg!(target_os = "linux") {
        (
            if cfg!(target_arch = "aarch64") { "linux-aarch64" } else { "linux-x86_64" },
            // Prebuilty rozpakowują wersjonowany soname (np. .so.1.26.0) obok
            // dowiązania .so — bierzemy oba warianty, wersjonowany jako pierwszy.
            &["libonnxruntime.so", "libonnxruntime.so.*"],
        )
    } else {
        return None;
    };

    // Wspinamy się w górę od CARGO_MANIFEST_DIR / cwd / katalogu binarki aż do
    // katalogu zawierającego `native-libs/`.
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        starts.push(PathBuf::from(manifest));
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        let mut cur: Option<&std::path::Path> = Some(start.as_path());
        while let Some(dir) = cur {
            let lib_dir = dir.join("native-libs").join(platform).join("lib-dynamic");
            if lib_dir.is_dir() {
                if let Some(found) = pick_ort_dylib(&lib_dir, lib_glob) {
                    return Some(found);
                }
            }
            cur = dir.parent();
        }
    }
    None
}

/// Wybiera najlepszy plik runtime ONNX z katalogu `lib-dynamic`. Dla wersjonowanego
/// soname (`libonnxruntime.so.*`) preferuje najświeższą wersję (sort malejący po
/// nazwie), by uniknąć niedeterminizmu gdy leży kilka wariantów.
fn pick_ort_dylib(lib_dir: &std::path::Path, lib_glob: &[&str]) -> Option<std::path::PathBuf> {
    for pattern in lib_glob {
        if let Some(suffix) = pattern.strip_suffix('*') {
            // Wzorzec wersjonowany: dopasuj prefiks, wybierz najświeższy.
            let entries = std::fs::read_dir(lib_dir).ok()?;
            let mut matches: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(suffix) && n != suffix)
                })
                .collect();
            matches.sort();
            if let Some(latest) = matches.pop() {
                return Some(latest);
            }
        } else {
            let candidate = lib_dir.join(pattern);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Buduje sesję `ort` z modelu ONNX, rejestrując łańcuch execution providerów w
/// kolejności priorytetu z MIĘKKĄ rejestracją (bez `error_on_failure`): jeśli dany
/// EP jest niedostępny w załadowanym runtime, `ort` loguje ostrzeżenie i przechodzi
/// do następnego (patrz `ort::ep::apply_execution_providers`). ONNX Runtime sam
/// przydziela węzły grafu do najwyżej-priorytetowego zarejestrowanego EP, więc gdy
/// TensorRT jest obecny — użyje go, a inaczej płynnie zejdzie na CUDA (lub CPU).
///
/// `trt_cache_dir` to per-model katalog engine-cache TensorRT (zserializowane
/// plany silników; pierwszy forward po zmianie modelu/GPU buduje je od nowa).
///
/// Kolejność: TensorRT (engine-cache + FP16) → CUDA → [macOS] CoreML → CPU.
pub fn build_ort_session(
    model_path: &std::path::Path,
    trt_cache_dir: &std::path::Path,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir)?
        .commit_from_file(model_path)
        .map_err(|e| anyhow!("ort commit_from_file {}: {e}", model_path.display()))
}

/// Like [`build_ort_session`] but committing from an in-memory model. Used by
/// the `onnx-cv` runner so the sha256-verified bytes are EXACTLY the bytes the
/// session is built from (no hash-then-reopen TOCTOU window on the file).
pub fn build_ort_session_from_memory(
    model_bytes: &[u8],
    trt_cache_dir: &std::path::Path,
) -> Result<ort::session::Session> {
    session_builder_with_eps(trt_cache_dir)?
        .commit_from_memory(model_bytes)
        .map_err(|e| anyhow!("ort commit_from_memory: {e}"))
}

fn session_builder_with_eps(
    trt_cache_dir: &std::path::Path,
) -> Result<ort::session::builder::SessionBuilder> {
    use ort::ep::{ExecutionProvider, ExecutionProviderDispatch};
    use ort::session::Session;

    let mut eps: Vec<ExecutionProviderDispatch> = Vec::new();

    #[cfg(not(target_os = "macos"))]
    {
        // TensorRT — najwyższy priorytet. Engine-cache trzyma zserializowane plany
        // silników na dysku (pierwszy forward po zmianie modelu/GPU buduje je od
        // nowa i jest wolny; kolejne wczytują z cache). FP16 dla przepustowości.
        if let Err(e) = std::fs::create_dir_all(trt_cache_dir) {
            warn!(
                "[ort] nie udało się utworzyć cache TensorRT {}: {e}",
                trt_cache_dir.display()
            );
        }
        eps.push(
            ort::ep::TensorRT::default()
                .with_engine_cache(true)
                .with_engine_cache_path(trt_cache_dir.to_string_lossy().to_string())
                .with_timing_cache(true)
                .with_fp16(true)
                .build(),
        );
        // CUDA — dotychczasowa, działająca ścieżka; teraz MIĘKKO (bez
        // error_on_failure), bo poprzedza ją TensorRT.
        eps.push(ort::ep::CUDA::default().build());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = trt_cache_dir;
        // CoreML (Metal/ANE) — akceleracja na Apple Silicon.
        eps.push(ort::ep::CoreML::default().build());
    }
    // CPU — zawsze ostatni fallback.
    eps.push(ort::ep::CPU::default().build());

    // Introspekcja: logujemy które akceleratory widzi załadowany runtime (ort nie
    // raportuje per-węzeł finalnego EP, ale ONNX Runtime bierze najwyżej-priorytetowy
    // z dostępnych, więc to jednoznacznie wskazuje realnie użytą ścieżkę).
    #[cfg(not(target_os = "macos"))]
    {
        let trt = ort::ep::TensorRT::default().is_available().unwrap_or(false);
        let cuda = ort::ep::CUDA::default().is_available().unwrap_or(false);
        info!("[ort] dostępne EP w runtime — TensorRT={trt}, CUDA={cuda} (priorytet: TensorRT>CUDA>CPU)");
    }
    #[cfg(target_os = "macos")]
    {
        let coreml = ort::ep::CoreML::default().is_available().unwrap_or(false);
        info!("[ort] dostępne EP w runtime — CoreML={coreml} (priorytet: CoreML>CPU)");
    }

    Session::builder()
        .map_err(|e| anyhow!("ort Session::builder: {e}"))?
        .with_execution_providers(eps)
        .map_err(|e| anyhow!("ort with_execution_providers: {e}"))
}
