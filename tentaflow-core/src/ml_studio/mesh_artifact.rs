// ===== File: ml_studio/mesh_artifact.rs — transfer artefaktu modelu między węzłami mesh =====
//
// Przenosi katalog artefaktu modelu (np. model MLX safetensors) z węzła, na
// którym powstał (np. B), na węzeł docelowy deployu (np. C). Transfer idzie
// JEDNYM bi-streamem mesh (ALPN_ARTIFACT, `IrohMeshManager::push_artifact_stream`):
// węzeł-źródło pakuje katalog do ZIP (Stored) i przepycha bajty strumieniem;
// odbiorca składa cały ZIP i rozpakowuje do lokalnego katalogu. Zero round-tripów
// per-fragment — w przeciwieństwie do komend request/response strumień QUIC sam
// obsługuje kontrolę przepływu, więc 1 GB modelu leci niezawodnie i szybko.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::ml_studio::train_recognition::{blob_content_hash, zip_dir};

/// Górny limit rozmiaru artefaktu ML Studio (model MLX/eksport treningu). Chroni
/// odbiorcę przed alokacją bufora z ogromnego `zip_len` od złośliwego peera —
/// ODEBRANY ZIP JEST TRZYMANY W CAŁOŚCI W RAM (patrz `recv_artifact_stream`), więc
/// limit = maksymalny jednorazowy narzut pamięci ścieżki artefaktów ML Studio.
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Osobny, WYŻSZY limit dla transferu modelu HF (`hf-model|...`, P0 cluster deploy):
/// pełne wagi modelu potrafią mieć setki GB. Używany WYŁĄCZNIE dla nazw z prefiksem
/// `HF_MODEL_NAME_PREFIX`, żeby nie rozszerzać powierzchni DoS ścieżki ML Studio.
/// WHY bufor-w-RAM mimo dużego limitu: pełny streaming-zip (rozpakowanie w locie bez
/// trzymania całości w `Vec`) to osobny, większy refactor (`store_*_zip` biorą
/// `&[u8]`); tu wprowadzamy tylko rozdzielone limity, a streaming zostaje kolejnym
/// krokiem. Ryzyko RAM przy dużym modelu jest świadome i ograniczone tą stałą.
const MAX_HF_MODEL_BYTES: u64 = 200 * 1024 * 1024 * 1024;

/// Prefiks nazwy strumienia znaczacy, ze artefakt to snapshot modelu HF do zapisu
/// w cache HF (a NIE artefakt ML Studio). Format nazwy:
/// `hf-model|<models--ORG--NAME>|<snapshot-hash>`. Odbiorca (`store_artifact_zip`)
/// routuje takie transfery do cache HF zamiast do `ml-studio/mesh-artifacts`.
const HF_MODEL_NAME_PREFIX: &str = "hf-model|";

/// Ziarno strumienia: bajty przesuwamy porcjami tej wielkości, żeby watchdog
/// STALL mógł działać na granulacji porcji (a nie całego pliku).
pub const ARTIFACT_CHUNK_BYTES: usize = 1024 * 1024;

/// Brak postępu transferu (0 B/s) przez tyle sekund = STALL → błąd. NIE liczymy
/// sztywnego deadline na cały transfer: duży model może iść długo, a błędem jest
/// dopiero zatrzymanie strumienia. Aktywny transfer NIGDY nie wywala timeoutu.
pub const ARTIFACT_STALL_SECS: u64 = 30;

/// Postęp transferu artefaktu (faza wdrożenia modelu na zdalny węzeł). Trzymane
/// in-memory na węźle, który REALNIE pcha bajty (sender). Odpytywane przez UI
/// (przez metryki modelu) do paska B/s — dokładnie jak transfer datasetu treningu.
#[derive(Clone, Debug, Default)]
pub struct ArtifactTransferProgress {
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub rate_bps: u64,
}

static ARTIFACT_PROGRESS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ArtifactTransferProgress>>,
> = std::sync::OnceLock::new();

fn artifact_progress_map(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ArtifactTransferProgress>> {
    ARTIFACT_PROGRESS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub fn set_artifact_progress_pub(key: &str, p: ArtifactTransferProgress) {
    if let Ok(mut m) = artifact_progress_map().lock() {
        m.insert(key.to_string(), p);
    }
}

/// Bieżący postęp transferu artefaktu dla klucza (None gdy brak/zakończony).
pub fn artifact_progress(key: &str) -> Option<ArtifactTransferProgress> {
    artifact_progress_map().lock().ok()?.get(key).cloned()
}

/// Usuwa wpis postępu (po zakończeniu/błędzie transferu).
pub fn clear_artifact_progress(key: &str) {
    if let Ok(mut m) = artifact_progress_map().lock() {
        m.remove(key);
    }
}

/// Katalog, do którego węzeł docelowy rozpakowuje odebrane artefakty.
fn artifact_dest_root() -> PathBuf {
    crate::paths::cache_dir().join("ml-studio").join("mesh-artifacts")
}

/// Nazwa katalogu artefaktu na węźle docelowym (ostatni segment ścieżki źródła,
/// odcięty z path-traversal). Pusta/niebezpieczna → `model`.
fn safe_artifact_name(src_dir: &str) -> String {
    Path::new(src_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.replace(['/', '\\', '.'], "_"))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "model".to_string())
}

/// Czy `path` jest dozwoloną lokalizacją artefaktu (fail-closed dla `MlArtifactPushTo`
/// od zdalnego węzła). Wymaga istniejącego KATALOGU pod znanym rootem ML Studio.
pub fn is_allowed_artifact_dir(path: &str) -> bool {
    let canon = match std::fs::canonicalize(Path::new(path)) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if !canon.is_dir() {
        return false;
    }
    let roots = [
        crate::paths::cache_dir(),
        crate::paths::data_dir(),
        crate::paths::tentaflow_home().to_path_buf(),
    ];
    if roots.iter().any(|r| {
        std::fs::canonicalize(r)
            .map(|rc| canon.starts_with(&rc))
            .unwrap_or(false)
    }) {
        return true;
    }
    // Artefakty treningowe lądują w `ml-training-out/exports/...` (poza home przy
    // konfigurowalnym katalogu modeli) — dopuszczamy po komponencie ścieżki.
    let s = canon.to_string_lossy();
    s.contains("/ml-training-out/") || s.contains("/exports/") || s.contains("/mlx-model")
}

/// Węzeł-źródło: pakuje `src_dir` do ZIP i przepycha JEDNYM strumieniem mesh do
/// `target_node_id`. Zwraca ścieżkę katalogu artefaktu NA węźle docelowym.
pub async fn push_dir_to(
    iroh: &crate::mesh::iroh_manager::IrohMeshManager,
    target_node_id: &str,
    src_dir: &str,
    progress_key: Option<&str>,
) -> anyhow::Result<String> {
    if !Path::new(src_dir).is_dir() {
        anyhow::bail!("artefakt do transferu nie jest katalogiem: {}", src_dir);
    }
    let zip = zip_dir(Path::new(src_dir))?;
    if zip.len() as u64 > MAX_ARTIFACT_BYTES {
        anyhow::bail!("artefakt przekracza limit transferu ({} B)", zip.len());
    }
    let name = safe_artifact_name(src_dir);
    iroh.push_artifact_stream(target_node_id, &name, &zip, progress_key)
        .await
}

/// Węzeł-źródło (head cluster deploy): pakuje snapshot modelu z lokalnego cache HF
/// (`snapshot_dir`) i przepycha go JEDNYM strumieniem mesh do `target_node_id`.
/// Nazwa strumienia koduje docelowy katalog cache (`models--ORG--NAME`) i rewizję
/// (`hash`), więc odbiorca odtworzy poprawny layout HF (patrz `store_hf_model_zip`).
pub async fn push_hf_model_to(
    iroh: &crate::mesh::iroh_manager::IrohMeshManager,
    target_node_id: &str,
    model_repo: &str,
    snapshot_dir: &str,
    hash: &str,
    progress_key: Option<&str>,
) -> anyhow::Result<String> {
    if !Path::new(snapshot_dir).is_dir() {
        anyhow::bail!("snapshot modelu do transferu nie jest katalogiem: {snapshot_dir}");
    }
    let zip = zip_dir(Path::new(snapshot_dir))?;
    if zip.len() as u64 > MAX_HF_MODEL_BYTES {
        anyhow::bail!("snapshot modelu przekracza limit transferu ({} B)", zip.len());
    }
    let models_dir = crate::services::deploy::distributed::model_dir_name(model_repo);
    let name = format!("{HF_MODEL_NAME_PREFIX}{models_dir}|{hash}");
    iroh.push_artifact_stream(target_node_id, &name, &zip, progress_key)
        .await
}

/// Węzeł docelowy (accept loop ALPN_ARTIFACT): czyta z bi-streamu
/// `[name_len u32][name][zip_len u64][zip]` i zwraca `(name, zip_bytes)`.
pub async fn recv_artifact_stream(
    recv: &mut iroh::endpoint::RecvStream,
) -> anyhow::Result<(String, Vec<u8>)> {
    let mut l4 = [0u8; 4];
    recv.read_exact(&mut l4)
        .await
        .map_err(|e| anyhow::anyhow!("artifact recv: name_len: {e}"))?;
    let name_len = u32::from_be_bytes(l4) as usize;
    if name_len == 0 || name_len > 512 {
        anyhow::bail!("artifact recv: zła długość nazwy {}", name_len);
    }
    let mut nb = vec![0u8; name_len];
    recv.read_exact(&mut nb)
        .await
        .map_err(|e| anyhow::anyhow!("artifact recv: name: {e}"))?;
    let name = String::from_utf8_lossy(&nb).to_string();

    let mut l8 = [0u8; 8];
    recv.read_exact(&mut l8)
        .await
        .map_err(|e| anyhow::anyhow!("artifact recv: zip_len: {e}"))?;
    let zip_len = u64::from_be_bytes(l8);
    // Limit zależy od typu transferu (nazwa jest już znana): model HF ma osobny,
    // wyższy limit; artefakty ML Studio zostają na węższym limicie DoS.
    let max_bytes = if name.starts_with(HF_MODEL_NAME_PREFIX) {
        MAX_HF_MODEL_BYTES
    } else {
        MAX_ARTIFACT_BYTES
    };
    if zip_len == 0 || zip_len > max_bytes {
        anyhow::bail!("artifact recv: zła długość zip {}", zip_len);
    }
    // Czytamy odczytami CZĄSTKOWYMI (`read`, nie `read_exact`) z watchdogiem STALL:
    // każdy `read` zwraca, gdy tylko napłyną JAKIEKOLWIEK bajty, i ma świeży limit
    // bezczynności (ARTIFACT_STALL_SECS). Dopóki choć bajt napływa w oknie, licznik
    // się resetuje — transfer może trwać dowolnie długo i wolno. Timeout pojedynczego
    // `read` = ZERO bajtów przez okno = STALL (a nie „za wolno").
    let mut zip = vec![0u8; zip_len as usize];
    let mut filled = 0usize;
    let stall = std::time::Duration::from_secs(ARTIFACT_STALL_SECS);
    while filled < zip.len() {
        match tokio::time::timeout(stall, recv.read(&mut zip[filled..])).await {
            Ok(Ok(Some(0))) => anyhow::bail!(
                "artifact recv: strumień zamknięty przedwcześnie ({}/{} B)",
                filled,
                zip.len()
            ),
            Ok(Ok(Some(n))) => filled += n,
            Ok(Ok(None)) => anyhow::bail!(
                "artifact recv: koniec strumienia przed pełnym zip ({}/{} B)",
                filled,
                zip.len()
            ),
            Ok(Err(e)) => return Err(anyhow::anyhow!("artifact recv: zip: {e}")),
            Err(_) => anyhow::bail!(
                "artifact recv: transfer utknął — brak NOWYCH danych przez {}s ({}/{} B)",
                ARTIFACT_STALL_SECS,
                filled,
                zip.len()
            ),
        }
    }
    Ok((name, zip))
}

/// Węzeł docelowy: rozpakowuje odebrany ZIP do unikalnego katalogu
/// `<cache>/ml-studio/mesh-artifacts/<name>-<content-hash>` i zwraca ścieżkę.
pub fn store_artifact_zip(name: &str, zip: &[u8]) -> anyhow::Result<String> {
    // Transfery modelu HF (P0 cluster deploy) mają nazwę `hf-model|<dir>|<hash>`
    // i lądują w cache HF, a nie w `ml-studio/mesh-artifacts`.
    if let Some(rest) = name.strip_prefix(HF_MODEL_NAME_PREFIX) {
        let (models_dir, hash) = rest
            .split_once('|')
            .ok_or_else(|| anyhow::anyhow!("zła nazwa transferu modelu HF: {name}"))?;
        return store_hf_model_zip(models_dir, hash, zip);
    }
    let id = blob_content_hash(zip);
    let dest = artifact_dest_root().join(format!("{}-{}", safe_artifact_name(name), id));
    // Artefakt ML Studio: brak sztywnego kontraktu plików, wymagamy tylko, by ZIP
    // rozpakował się do NIEPUSTEGO katalogu (nie zostawiamy pustego dest).
    unzip_to_dir(zip, &dest, |staging| {
        if dir_is_empty(staging) {
            anyhow::bail!("artefakt jest pusty po rozpakowaniu");
        }
        Ok(())
    })?;
    Ok(dest.to_string_lossy().to_string())
}

/// Czy katalog nie zawiera żadnego wpisu (używane jako minimalna walidacja
/// kompletności artefaktu ML Studio).
fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut rd| rd.next().is_none())
        .unwrap_or(true)
}

/// Czy rozpakowany snapshot modelu HF jest kompletny do serwowania: ma `config.json`
/// i choć jeden plik `*.safetensors`. Walidacja PRZED atomową podmianą cache, żeby
/// częściowy/złośliwy ZIP nie nadpisał kompletnego modelu.
fn hf_snapshot_complete(dir: &Path) -> bool {
    if !dir.join("config.json").is_file() {
        return false;
    }
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                e.path()
                    .extension()
                    .map(|x| x == "safetensors")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Czy segment (`models--ORG--NAME` lub rewizja snapshotu) jest bezpieczny do
/// zbudowania ścieżki cache — fail-closed wobec path-traversal od peera.
fn hf_segment_safe(s: &str, must_prefix: Option<&str>) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && must_prefix.map(|p| s.starts_with(p)).unwrap_or(true)
}

/// Węzeł docelowy: rozpakowuje odebrany snapshot modelu do cache HF pod
/// `models_root()/<models_dir>/snapshots/<hash>/` i zapisuje `refs/main = <hash>`,
/// żeby `vllm serve` z `HF_HUB_OFFLINE=1` rozwiązał rewizję. Pliki są zwykłe (nie
/// symlinki do blobów) — offline resolver huggingface_hub tego nie wymaga.
fn store_hf_model_zip(models_dir: &str, hash: &str, zip: &[u8]) -> anyhow::Result<String> {
    if !hf_segment_safe(models_dir, Some("models--")) {
        anyhow::bail!("niebezpieczna nazwa katalogu modelu: {models_dir}");
    }
    if !hf_segment_safe(hash, None) {
        anyhow::bail!("niebezpieczna rewizja snapshotu: {hash}");
    }
    let base = crate::paths::models_root().join(models_dir);
    let dest = base.join("snapshots").join(hash);
    // Walidacja kompletności PRZED podmianą — niekompletny transfer nie kasuje
    // istniejącego, dobrego snapshotu w cache HF (integralność, P1-C).
    unzip_to_dir(zip, &dest, |staging| {
        if !hf_snapshot_complete(staging) {
            anyhow::bail!("snapshot modelu niekompletny (brak config.json lub *.safetensors)");
        }
        Ok(())
    })?;
    let refs = base.join("refs");
    std::fs::create_dir_all(&refs)?;
    std::fs::write(refs.join("main"), hash.as_bytes())?;
    Ok(dest.to_string_lossy().to_string())
}

/// Rozpakowuje ZIP do TYMCZASOWEGO katalogu obok `dest`, uruchamia `validate` na
/// rozpakowanej zawartości i DOPIERO wtedy atomowo podmienia `dest` (rename w tym
/// samym katalogu nadrzędnym = ten sam FS). Przy błędzie ZIP/walidacji istniejący
/// `dest` NIE jest ruszany — częściowy/złośliwy transfer nie kasuje już zapisanego,
/// kompletnego artefaktu/modelu. Odcina path-traversal (`enclosed_name`).
fn unzip_to_dir(
    zip_bytes: &[u8],
    dest: &Path,
    validate: impl Fn(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("dest bez katalogu nadrzędnego: {}", dest.display()))?;
    std::fs::create_dir_all(parent)?;
    let dest_name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("model");
    let id = blob_content_hash(zip_bytes);
    let staging = parent.join(format!(".{dest_name}.incoming-{id}"));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)?;

    // Rozpakowanie + walidacja w stagingu; przy KAŻDYM błędzie sprzątamy staging i
    // NIE dotykamy dest.
    let staged = (|| -> anyhow::Result<()> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|e| anyhow::anyhow!("artefakt nie jest poprawnym zip: {}", e))?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let Some(rel) = entry.enclosed_name() else {
                continue;
            };
            let out_path = staging.join(&rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
                continue;
            }
            if let Some(p) = out_path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&out_path, &buf)?;
        }
        validate(&staging)
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // Atomowa podmiana: istniejący dest odsuwamy do backupu i przywracamy przy
    // niepowodzeniu renamu, żeby nigdy nie zostać z pustym/połowicznym dest.
    let backup = parent.join(format!(".{dest_name}.old-{id}"));
    let had_dest = dest.exists();
    if had_dest {
        if backup.exists() {
            let _ = std::fs::remove_dir_all(&backup);
        }
        std::fs::rename(dest, &backup)
            .map_err(|e| anyhow::anyhow!("backup istniejącego dest: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&staging, dest) {
        if had_dest {
            let _ = std::fs::rename(&backup, dest);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(anyhow::anyhow!("atomowa podmiana dest: {e}"));
    }
    if had_dest {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}
