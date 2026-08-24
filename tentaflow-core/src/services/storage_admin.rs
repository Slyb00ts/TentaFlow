// =============================================================================
// File: services/storage_admin.rs — Ustawienia → Magazyn danych.
// Overview kategorii (ścieżki + rozmiary + dysk), przeglądarka katalogów dla
// pickera (drzewko + mkdir) i migracja danych:
//   - kategorie live (models/containers/cache/blobs/recordings/addony/keys):
//     wstrzymanie usług zależnych → przeniesienie (rename albo kopia z
//     progresem przy EXDEV) → przełączenie override + settings → wznowienie.
//     Postęp leci przez deploy log_bus (job_id = deploy_id), więc dashboard
//     używa istniejącej subskrypcji `deploymentLogStreamRequest`.
//   - kategorie restartowe (data/sync): zapis pending do
//     storage-migration-pending.conf; `paths::apply_pending_boot_migrations()`
//     wykonuje move przy następnym starcie, zanim baza/ledger się otworzą.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tentaflow_protocol::{
    StorageBrowseResponse, StorageCategoryInfo, StorageDirEntry, StorageMigrateResponse,
    StorageOverviewResponse,
};

use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::paths::{self, StorageCategory};
use crate::services::ports::PortAllocator;
use crate::services_repo::services::{DeployMethod, ServiceRow, ServiceStatus};

/// Kategorie usług, które trzymają otwarte pliki modeli / cache (Docker
/// bind-mount albo natywny proces). Wstrzymywane na czas migracji `models`
/// i `cache`.
const MODEL_BOUND_CATEGORIES: [&str; 12] = [
    "llm",
    "stt",
    "tts",
    "embedding",
    "embeddings",
    "reranker",
    "rerank",
    "vision",
    "image-gen",
    "image_gen",
    "audio",
    "video",
];

/// Jedna migracja live na raz — dwie równoległe przenosiny mogłyby dotyczyć
/// zagnieżdżonych katalogów.
static MIGRATION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[derive(thiserror::Error, Debug)]
pub enum StorageAdminError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Internal(String),
}

fn bad(msg: impl Into<String>) -> StorageAdminError {
    StorageAdminError::BadRequest(msg.into())
}

// =============================================================================
// Overview
// =============================================================================

pub async fn overview() -> StorageOverviewResponse {
    tokio::task::spawn_blocking(overview_blocking)
        .await
        .unwrap_or_else(|_| overview_blocking())
}

fn overview_blocking() -> StorageOverviewResponse {
    let root = paths::tentaflow_home().to_path_buf();
    let mut categories = Vec::with_capacity(paths::ALL_STORAGE_CATEGORIES.len());
    for cat in paths::ALL_STORAGE_CATEGORIES {
        let path = paths::category_dir(cat);
        let pending = if cat.live_migratable() {
            None
        } else {
            // Move zaplanowany na restart LUB zmiana ścieżki bez przenoszenia
            // zapisana w conf, która jeszcze nie obowiązuje.
            paths::pending_boot_migration(cat)
                .or_else(|| paths::boot_override_value(cat).filter(|v| PathBuf::from(v) != path))
        };
        categories.push(StorageCategoryInfo {
            key: cat.setting_key().to_string(),
            path: path.to_string_lossy().to_string(),
            default_path: cat.default_dir().to_string_lossy().to_string(),
            overridden: paths::category_override(cat).is_some(),
            size_bytes: dir_size(&path),
            live_migratable: cat.live_migratable(),
            pending_path: pending,
        });
    }
    let (disk_total_bytes, disk_available_bytes) = disk_space(&root).unwrap_or((0, 0));
    StorageOverviewResponse {
        categories,
        root: root.to_string_lossy().to_string(),
        disk_total_bytes,
        disk_available_bytes,
    }
}

/// Rekurencyjny rozmiar katalogu. Best-effort: błędy IO liczą się jako 0,
/// symlinki nie są śledzone (metadata linku, nie celu).
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(u64, u64)> = None;
    let mut best_len = 0usize;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if path.starts_with(mount) {
            let mount_len = mount.components().count();
            if mount_len >= best_len {
                best_len = mount_len;
                best = Some((disk.total_space(), disk.available_space()));
            }
        }
    }
    best
}

// =============================================================================
// Browse + mkdir (picker drzewka)
// =============================================================================

pub async fn browse(path: String) -> Result<StorageBrowseResponse, StorageAdminError> {
    tokio::task::spawn_blocking(move || browse_blocking(&path))
        .await
        .map_err(|e| StorageAdminError::Internal(e.to_string()))?
}

fn browse_blocking(path: &str) -> Result<StorageBrowseResponse, StorageAdminError> {
    let dir = if path.trim().is_empty() {
        PathBuf::from("/")
    } else {
        let p = PathBuf::from(path.trim());
        if !p.is_absolute() {
            return Err(bad("ścieżka musi być bezwzględna"));
        }
        p
    };
    if !dir.is_dir() {
        return Err(bad(format!("katalog '{}' nie istnieje", dir.display())));
    }
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| bad(format!("odczyt katalogu '{}': {}", dir.display(), e)))?;
    let mut entries: Vec<StorageDirEntry> = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let full = entry.path();
        let has_children = std::fs::read_dir(&full)
            .map(|mut d| {
                d.any(|c| {
                    c.ok()
                        .and_then(|c| c.file_type().ok())
                        .map(|t| t.is_dir())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        let writable = !std::fs::metadata(&full)
            .map(|m| m.permissions().readonly())
            .unwrap_or(true);
        entries.push(StorageDirEntry {
            name,
            path: full.to_string_lossy().to_string(),
            has_children,
            writable,
        });
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let (total_bytes, free_bytes) = disk_space(&dir).unwrap_or((0, 0));
    Ok(StorageBrowseResponse {
        path: dir.to_string_lossy().to_string(),
        entries,
        free_bytes,
        total_bytes,
    })
}

pub fn create_dir(parent: &str, name: &str) -> Result<String, StorageAdminError> {
    let parent_path = PathBuf::from(parent.trim());
    if !parent_path.is_absolute() || !parent_path.is_dir() {
        return Err(bad("katalog nadrzędny nie istnieje"));
    }
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(bad("niepoprawna nazwa katalogu"));
    }
    let full = parent_path.join(name);
    std::fs::create_dir(&full)
        .map_err(|e| bad(format!("utworzenie '{}': {}", full.display(), e)))?;
    Ok(full.to_string_lossy().to_string())
}

// =============================================================================
// Migracja
// =============================================================================

pub struct MigrationDeps {
    pub db: DbPool,
    pub port_allocator: Option<Arc<PortAllocator>>,
    pub settings_cipher: Arc<crate::crypto::SettingsCipher>,
}

/// Wejście z handlera `StorageMigrateRequest`. Zwraca tryb obsługi; dla
/// `mode == "live"` w tle rusza job publikujący postęp na log_bus pod
/// zwróconym `job_id`.
pub fn start_migration(
    deps: MigrationDeps,
    key: &str,
    new_path: &str,
    move_data: bool,
) -> Result<StorageMigrateResponse, StorageAdminError> {
    let cat = StorageCategory::from_setting_key(key)
        .ok_or_else(|| bad(format!("nieznana kategoria magazynu '{}'", key)))?;
    let new_path = new_path.trim();
    if new_path.is_empty() || !Path::new(new_path).is_absolute() {
        return Err(bad("nowa ścieżka musi być bezwzględna"));
    }
    let new_dir = PathBuf::from(new_path);
    let old_dir = paths::category_dir(cat);
    if new_dir == old_dir {
        return Err(bad("nowa ścieżka jest identyczna z obecną"));
    }
    if new_dir.starts_with(&old_dir) || old_dir.starts_with(&new_dir) {
        return Err(bad(
            "nowa ścieżka nie może być wewnątrz obecnej (ani odwrotnie)",
        ));
    }
    std::fs::create_dir_all(&new_dir)
        .map_err(|e| bad(format!("utworzenie '{}': {}", new_dir.display(), e)))?;

    if !cat.live_migratable() {
        // Data/Sync: zmiana obowiązuje od następnego startu. Move planujemy w
        // pending conf; samo przełączenie ścieżki zapisujemy od razu w conf.
        if move_data {
            paths::schedule_boot_migration(cat, new_path)
                .map_err(|e| StorageAdminError::Internal(e.to_string()))?;
        } else {
            paths::set_boot_override(cat, Some(new_path))
                .map_err(|e| StorageAdminError::Internal(e.to_string()))?;
        }
        return Ok(StorageMigrateResponse {
            mode: "boot".to_string(),
            job_id: None,
        });
    }

    if !move_data {
        // Samo przełączenie: zapis ustawienia + override na żywo.
        crate::db::repository::set_setting(&deps.db, key, new_path)
            .map_err(|e| StorageAdminError::Internal(e.to_string()))?;
        paths::set_category_override(cat, Some(new_path.to_string()));
        let _ = paths::ensure_app_dirs();
        return Ok(StorageMigrateResponse {
            mode: "switched".to_string(),
            job_id: None,
        });
    }

    if MIGRATION_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err(bad("inna migracja magazynu jest w toku"));
    }

    let job_id = format!("storage-mig-{}-{}", key, uuid::Uuid::new_v4().simple());
    // Kanał musi istnieć zanim frontend zdąży zasubskrybować.
    let sender = log_bus::sender_for(&job_id);
    let key_owned = key.to_string();
    let job_id_task = job_id.clone();
    tokio::spawn(async move {
        let job_id = job_id_task;
        let started = std::time::Instant::now();
        let result = run_live_migration(&deps, cat, &key_owned, &new_dir, &sender, &job_id).await;
        let (final_status, error_message) = match result {
            Ok(()) => ("success".to_string(), String::new()),
            Err(e) => {
                tracing::error!("storage migration {} failed: {}", key_owned, e);
                ("failed".to_string(), e.to_string())
            }
        };
        let _ = sender.send(BusMessage::End {
            deploy_id: job_id.clone(),
            final_status,
            image_tag: String::new(),
            container_name: String::new(),
            error_message,
            duration_ms: started.elapsed().as_millis() as i64,
        });
        log_bus::close(&job_id);
        MIGRATION_IN_PROGRESS.store(false, Ordering::SeqCst);
    });

    Ok(StorageMigrateResponse {
        mode: "live".to_string(),
        job_id: Some(job_id),
    })
}

fn emit(
    sender: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
    kind: &str,
    phase: &str,
    pct: u32,
    line: String,
) {
    let _ = sender.send(BusMessage::Line(LogLine {
        deploy_id: job_id.to_string(),
        kind: kind.to_string(),
        line,
        phase: phase.to_string(),
        progress_pct: pct,
        ts_ms: chrono::Utc::now().timestamp_millis(),
    }));
}

async fn run_live_migration(
    deps: &MigrationDeps,
    cat: StorageCategory,
    key: &str,
    new_dir: &Path,
    sender: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
) -> anyhow::Result<()> {
    let job_id = job_id.to_string();
    let old_dir = paths::category_dir(cat);

    // ---- Faza 1: wstrzymanie zależnych ----------------------------------
    emit(
        sender,
        &job_id,
        "phase",
        "pause",
        0,
        "Wstrzymywanie usług zależnych".into(),
    );
    let stopped = pause_dependents(deps, cat, sender, &job_id).await?;
    if matches!(cat, StorageCategory::AddonData) {
        crate::addon::storage_sql::set_addon_storage_frozen(true);
        crate::services::vector_namespace_manager(&deps.db).invalidate_all();
        #[cfg(feature = "graph")]
        crate::services::graph_manager(&deps.db).invalidate_all();
        // Chwila na domknięcie połączeń, których uchwyty właśnie opadły.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // ---- Faza 2: przenoszenie -------------------------------------------
    emit(
        sender,
        &job_id,
        "phase",
        "move",
        0,
        format!("Przenoszenie danych do {}", new_dir.display()),
    );
    let move_result = {
        let old = old_dir.clone();
        let new = new_dir.to_path_buf();
        let sender2 = sender.clone();
        let job2 = job_id.clone();
        tokio::task::spawn_blocking(move || {
            move_dir_with_progress(&old, &new, |pct, detail| {
                emit(&sender2, &job2, "progress", "move", pct, detail);
            })
        })
        .await?
    };

    if let Err(e) = move_result {
        // Rollback: stara lokalizacja pozostaje aktywna; odmroź i wznów.
        if matches!(cat, StorageCategory::AddonData) {
            crate::addon::storage_sql::set_addon_storage_frozen(false);
        }
        resume_services(deps, &stopped, sender, &job_id).await;
        return Err(anyhow::anyhow!("przenoszenie danych: {}", e));
    }

    // ---- Faza 3: przełączenie ścieżki -----------------------------------
    emit(
        sender,
        &job_id,
        "phase",
        "switch",
        0,
        "Przełączanie ścieżki i weryfikacja".into(),
    );
    crate::db::repository::set_setting(&deps.db, key, &new_dir.to_string_lossy())?;
    paths::set_category_override(cat, Some(new_dir.to_string_lossy().to_string()));
    let _ = paths::ensure_app_dirs();

    if matches!(cat, StorageCategory::AddonData) {
        // Vector index and graph collection file paths are stored in the DB as
        // absolute paths and are what decides where a file is opened from, so
        // rewrite the prefix to the new location.
        let old_prefix = format!("{}/", old_dir.to_string_lossy());
        let new_prefix = format!("{}/", new_dir.to_string_lossy());
        if let Ok(conn) = deps.db.write() {
            let _ = conn.execute(
                "UPDATE addon_vector_namespaces SET file_path = REPLACE(file_path, ?1, ?2) \
                 WHERE file_path LIKE ?1 || '%'",
                rusqlite::params![old_prefix, new_prefix],
            );
            let _ = conn.execute(
                "UPDATE addon_graph_collections SET file_path = REPLACE(file_path, ?1, ?2) \
                 WHERE file_path LIKE ?1 || '%'",
                rusqlite::params![old_prefix, new_prefix],
            );
        }
        crate::addon::storage_sql::set_addon_storage_frozen(false);
    }

    // ---- Faza 4: wznowienie ---------------------------------------------
    emit(
        sender,
        &job_id,
        "phase",
        "resume",
        0,
        "Wznawianie usług".into(),
    );
    resume_services(deps, &stopped, sender, &job_id).await;

    let _ = crate::db::repository::log_audit(
        &deps.db,
        None,
        None,
        "storage.migrate",
        Some("settings"),
        Some(&format!("{} -> {}", key, new_dir.display())),
        None,
        None,
    );
    Ok(())
}

/// Wstrzymuje usługi trzymające uchwyty w migrowanej kategorii. Zwraca listę
/// zatrzymanych wierszy (do wznowienia).
async fn pause_dependents(
    deps: &MigrationDeps,
    cat: StorageCategory,
    sender: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
) -> anyhow::Result<Vec<ServiceRow>> {
    let needs_service_pause = matches!(
        cat,
        StorageCategory::Models | StorageCategory::Cache | StorageCategory::Containers
    );
    if !needs_service_pause {
        return Ok(Vec::new());
    }
    let Some(ports) = deps.port_allocator.clone() else {
        return Ok(Vec::new());
    };
    let services: Vec<ServiceRow> = {
        let conn = deps
            .db
            .read()
            .map_err(|_| anyhow::anyhow!("db pool poisoned"))?;
        crate::services_repo::services::list_all(&conn)?
    };
    let mut to_stop: Vec<ServiceRow> = services
        .into_iter()
        .filter(|s| {
            matches!(
                s.status,
                ServiceStatus::Running | ServiceStatus::Degraded | ServiceStatus::Starting
            ) && !s.paused
        })
        .filter(|s| match cat {
            StorageCategory::Models => MODEL_BOUND_CATEGORIES.contains(&s.category.as_str()),
            // Cache: natywne bundle Pythona ruszają z venvów w cache, a
            // kontenery Docker mają bind-mount vllm-cache — pauzujemy oba.
            StorageCategory::Cache => {
                s.deploy_method == DeployMethod::NativePythonBundle
                    || MODEL_BOUND_CATEGORIES.contains(&s.category.as_str())
            }
            // Containers: konteksty build — nic nie działa z tego katalogu.
            _ => false,
        })
        .collect();
    to_stop.sort_by_key(|s| s.id);

    for svc in &to_stop {
        emit(
            sender,
            job_id,
            "log",
            "pause",
            0,
            format!("Wstrzymuję: {} (#{})", svc.engine_id, svc.id),
        );
        if let Err(e) = crate::services::deploy::stop(svc, ports.clone()).await {
            emit(
                sender,
                job_id,
                "log",
                "pause",
                0,
                format!("Stop {} nieudany: {}", svc.engine_id, e),
            );
        }
        // Wbudowany LLM nie jest zwalniany przez deploy::stop — model mmapuje
        // pliki z katalogu modeli, więc zwalniamy jawnie.
        if svc.deploy_method == DeployMethod::NativeEmbedded && svc.category == "llm" {
            let mgr = crate::inference::shared_inference_manager();
            let mut guard = mgr.write().await;
            let _ = guard.unload_model().await;
        }
        if let Ok(conn) = deps.db.write() {
            let _ = crate::services_repo::services::update_status(
                &conn,
                svc.id,
                ServiceStatus::Stopped,
            );
        }
    }
    Ok(to_stop)
}

async fn resume_services(
    deps: &MigrationDeps,
    stopped: &[ServiceRow],
    sender: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
) {
    let Some(ports) = deps.port_allocator.clone() else {
        return;
    };
    for svc in stopped {
        emit(
            sender,
            job_id,
            "log",
            "resume",
            0,
            format!("Wznawiam: {} (#{})", svc.engine_id, svc.id),
        );
        if let Ok(conn) = deps.db.write() {
            let _ = crate::services_repo::services::update_status(
                &conn,
                svc.id,
                ServiceStatus::Starting,
            );
        }
        let respawn = crate::services::deploy::respawn(
            &svc.engine_id,
            svc.deploy_method,
            &svc.config_json,
            ports.clone(),
            &deps.db,
            &deps.settings_cipher,
            svc.runtime_port,
        )
        .await;
        match respawn {
            Ok(handle) => {
                if let Ok(conn) = deps.db.write() {
                    let _ = crate::services_repo::services::update_runtime(
                        &conn,
                        svc.id,
                        handle.pid,
                        handle.port,
                        handle.sidecar_port,
                        handle.endpoint_url.as_deref(),
                    );
                    let _ = crate::services_repo::services::update_status(
                        &conn,
                        svc.id,
                        ServiceStatus::Running,
                    );
                }
            }
            Err(e) => {
                emit(
                    sender,
                    job_id,
                    "log",
                    "resume",
                    0,
                    format!("Wznowienie {} nieudane: {}", svc.engine_id, e),
                );
                if let Ok(conn) = deps.db.write() {
                    let _ = crate::services_repo::services::mark_failed_clear_runtime(
                        &conn,
                        svc.id,
                        &e.to_string(),
                    );
                }
            }
        }
    }
}

// =============================================================================
// Move z progresem
// =============================================================================

/// Przenosi zawartość `src` → `dst`. Ten sam system plików = szybki `rename`
/// (progres skacze na 100%); EXDEV = rekurencyjna kopia z licznikiem bajtów
/// (`progress(pct, opis_pliku)`) i usunięcie źródła po sukcesie.
fn move_dir_with_progress(
    src: &Path,
    dst: &Path,
    progress: impl Fn(u32, String),
) -> std::io::Result<()> {
    if !src.exists() {
        std::fs::create_dir_all(dst)?;
        progress(100, "brak danych do przeniesienia".into());
        return Ok(());
    }
    // Pusty katalog docelowy usuwamy, żeby rename był możliwy.
    if dst.is_dir()
        && std::fs::read_dir(dst)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(dst);
    }
    if !dst.exists() {
        match std::fs::rename(src, dst) {
            Ok(()) => {
                progress(100, "przeniesiono (ten sam wolumen)".into());
                return Ok(());
            }
            Err(e) if is_exdev(&e) => {}
            Err(e) => return Err(e),
        }
    }
    let total = dir_size(src).max(1);
    let mut copied = 0u64;
    let mut last_pct = 0u32;
    copy_tree_with_progress(src, dst, total, &mut copied, &mut last_pct, &progress)?;
    std::fs::remove_dir_all(src)?;
    progress(100, "kopiowanie zakończone, źródło usunięte".into());
    Ok(())
}

fn is_exdev(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    let code = 17;
    #[cfg(not(windows))]
    let code = 18;
    e.raw_os_error() == Some(code)
}

fn copy_tree_with_progress(
    src: &Path,
    dst: &Path,
    total: u64,
    copied: &mut u64,
    last_pct: &mut u32,
    progress: &impl Fn(u32, String),
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree_with_progress(&from, &to, total, copied, last_pct, progress)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                let _ = std::fs::remove_file(&to);
                std::os::unix::fs::symlink(target, &to)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&from, &to)?;
            }
        } else {
            let bytes = std::fs::copy(&from, &to)?;
            *copied = copied.saturating_add(bytes);
            let pct = ((*copied as f64 / total as f64) * 100.0).min(99.0) as u32;
            if pct > *last_pct {
                *last_pct = pct;
                progress(pct, from.to_string_lossy().to_string());
            }
        }
    }
    Ok(())
}
