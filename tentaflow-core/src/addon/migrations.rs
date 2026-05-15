// =============================================================================
// Plik: addon/migrations.rs
// Opis: Runner migracji per-addon SQLite (F1a §6.5 M1.W4). Wczytuje
//       `*.sql` z `migrations/` w bundle addona w kolejnosci leksykograficznej,
//       liczy SHA256 tresci, sprawdza w core DB `addon_migrations_applied`
//       czy migracja byla juz zaaplikowana. Idempotent re-install: hash
//       match → skip; hash mismatch → reject (manualna podmiana wykryta).
//       Apply atomic w jednej transakcji per migracja (BEGIN; <sql>; COMMIT).
//       Wpiety w `lifecycle::install` przed startem addonu — install fail
//       jesli ktorakolwiek migracja sie nie zaaplikuje.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;
use rusqlite::params;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::errors::AbiError;
use super::storage_sql::open_addon_db;
use crate::db::DbPool;

// =============================================================================
// Walidacja nazewnictwa migracji
// =============================================================================

/// Wymagany format: `NNN_name.sql` gdzie NNN to >=3 cyfry, name to lowercase
/// + cyfry + podkreslnik. Plik nie matchujacy regex jest ignorowany z warning.
fn migration_filename_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^[0-9]{3,}_[a-z0-9_]+\.sql$").expect("migration regex stale poprawny")
    })
}

// =============================================================================
// SHA256 helper
// =============================================================================

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    hex::encode(digest)
}

// =============================================================================
// Pojedynczy plik migracji do zaaplikowania
// =============================================================================

struct MigrationFile {
    name: String,
    full_path: PathBuf,
    sql: String,
    hash: String,
}

/// Zbiera i sortuje pliki migracji z `migrations_root`. Nieprawidlowe nazwy
/// loguje warning i pomija. Nieczytelne pliki → blad twardy (caller decyduje
/// czy install nadal kontynuuje).
fn collect_migrations(migrations_root: &Path) -> Result<Vec<MigrationFile>, AbiError> {
    if !migrations_root.exists() {
        // Brak katalogu migracji to OK — addon moze mieć `[storage] sql=true`
        // ale jeszcze nie zdefiniowal schematu. Zwroc puste.
        return Ok(Vec::new());
    }

    let mut entries: Vec<MigrationFile> = Vec::new();
    let dir = std::fs::read_dir(migrations_root).map_err(|_| AbiError::Operation)?;

    let rx = migration_filename_regex();
    for entry in dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !rx.is_match(name) {
            warn!(
                "migracja '{}' ma niepoprawna nazwe (oczekiwany format: NNN_lowercase.sql) — pomijam",
                name
            );
            continue;
        }
        let sql = std::fs::read_to_string(&path).map_err(|_| AbiError::Operation)?;
        let hash = sha256_hex(sql.as_bytes());
        entries.push(MigrationFile {
            name: name.to_string(),
            full_path: path.clone(),
            sql,
            hash,
        });
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

// =============================================================================
// Public API
// =============================================================================

/// Aplikuje wszystkie migracje per-addon. Wywolywane przez `lifecycle::install`
/// po sdk_version check, przed faktyczna aktywacja addona.
///
/// Parametry:
/// - `addon_id` — identyfikator addona (musi byc bezpieczny — walidacja w
///   fs_sandbox::validate_addon_id przy open_addon_db).
/// - `addon_version` — wersja addona z manifestu (zapis do
///   `applied_in_addon_version`).
/// - `manifest_migrations_dir` — sciezka wzgledna z manifestu (default
///   "migrations"; user-controlled — nigdy nie laczymy z hostowym path,
///   tylko z `bundle_root`).
/// - `bundle_root` — katalog bundle addona (zaufany — host go wybral).
/// - `core_db` — pula core DB do zapisu wpisow `addon_migrations_applied`.
///
/// Zwraca:
/// - `Ok(count)` z liczba ZAaplikowanych migracji (skipped przez idempotent
///   nie sa liczone).
/// - `Err(AbiError::Operation)` przy IO failure, hash mismatch, lub SQL fail.
pub fn apply_migrations(
    addon_id: &str,
    addon_version: &str,
    manifest_migrations_dir: &str,
    bundle_root: &Path,
    core_db: &DbPool,
) -> Result<usize, AbiError> {
    // Sciezka migracji: bundle_root + manifest_migrations_dir. Sanitizujemy
    // manifest path zeby nie wyjsc poza bundle (path traversal w manifest).
    if manifest_migrations_dir.contains("..") || manifest_migrations_dir.starts_with('/') {
        warn!(
            "addon '{}': migrations_dir='{}' zawiera path traversal lub absolute path — odrzucam",
            addon_id, manifest_migrations_dir
        );
        return Err(AbiError::Operation);
    }
    let migrations_root = bundle_root.join(manifest_migrations_dir);

    let migrations = collect_migrations(&migrations_root)?;
    if migrations.is_empty() {
        info!(
            "addon '{}': brak migracji w {:?} — schemat pusty",
            addon_id, migrations_root
        );
        return Ok(0);
    }

    // Otworz/utworz pool dla addona — bedzie potrzebny do apply.
    let pool = open_addon_db(addon_id)?;

    let mut applied_count = 0usize;
    for mig in &migrations {
        let outcome = apply_single_migration(
            addon_id,
            addon_version,
            mig,
            &pool,
            core_db,
        )?;
        if outcome == ApplyOutcome::Applied {
            applied_count += 1;
        }
    }

    info!(
        "addon '{}': zaaplikowano {} migracji (z {} dostepnych w {:?})",
        addon_id,
        applied_count,
        migrations.len(),
        migrations_root
    );
    Ok(applied_count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Applied,
    SkippedIdempotent,
}

fn apply_single_migration(
    addon_id: &str,
    addon_version: &str,
    mig: &MigrationFile,
    addon_pool: &super::storage_sql::AddonDbPool,
    core_db: &DbPool,
) -> Result<ApplyOutcome, AbiError> {
    // 1. Sprawdz w core DB czy ta migracja juz byla applied.
    let prior: Option<(String, String)> = {
        let conn = core_db.lock().map_err(|_| AbiError::Operation)?;
        conn.query_row(
            "SELECT migration_hash, status FROM addon_migrations_applied \
             WHERE addon_id = ?1 AND migration_name = ?2",
            params![addon_id, &mig.name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
    };

    if let Some((prior_hash, prior_status)) = prior {
        if prior_status == "success" {
            if prior_hash == mig.hash {
                // Idempotent: ta sama migracja, ten sam hash — skip.
                info!(
                    "addon '{}': migracja '{}' juz applied (hash match) — skip",
                    addon_id, mig.name
                );
                return Ok(ApplyOutcome::SkippedIdempotent);
            } else {
                // Hash mismatch: tresc migracji zmieniona PO apply — anomaly.
                warn!(
                    "addon '{}': migracja '{}' ma inny hash niz w DB (prior={} got={}) — \
                     mozliwe ze plik zmodyfikowany po apply",
                    addon_id, mig.name, prior_hash, mig.hash
                );
                return Err(AbiError::Operation);
            }
        }
        // Prior status 'failed' lub 'partial' — sprobuj ponownie (nadpisze
        // wpis przez UPSERT ponizej).
        info!(
            "addon '{}': migracja '{}' wczesniej z statusem '{}', powtarzam",
            addon_id, mig.name, prior_status
        );
    }

    // 2. Apply w transakcji per-addon SQLite (atomic).
    let start = Instant::now();
    let apply_result: Result<(), String> = (|| {
        let mut conn = addon_pool.get().map_err(|_| "pool get failed".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("BEGIN failed: {e}"))?;
        // `execute_batch` obsluguje multi-statement w jednym pliku — kazda
        // migracja moze miec kilka CREATE/INSERT oddzielonych srednikami.
        tx.execute_batch(&mig.sql)
            .map_err(|e| format!("execute_batch failed: {e}"))?;
        tx.commit().map_err(|e| format!("COMMIT failed: {e}"))?;
        Ok(())
    })();
    let duration_ms = start.elapsed().as_millis() as i64;

    // 3. Zapisz wynik do core DB (UPSERT — PRIMARY KEY (addon_id, migration_name)).
    let (status, error_message) = match &apply_result {
        Ok(()) => ("success", None),
        Err(e) => ("failed", Some(e.as_str())),
    };
    {
        let conn = core_db.lock().map_err(|_| AbiError::Operation)?;
        conn.execute(
            "INSERT INTO addon_migrations_applied \
             (addon_id, migration_name, migration_hash, applied_in_addon_version, \
              status, error_message, duration_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(addon_id, migration_name) DO UPDATE SET \
               migration_hash = excluded.migration_hash, \
               applied_at = datetime('now'), \
               applied_in_addon_version = excluded.applied_in_addon_version, \
               status = excluded.status, \
               error_message = excluded.error_message, \
               duration_ms = excluded.duration_ms",
            params![
                addon_id,
                &mig.name,
                &mig.hash,
                addon_version,
                status,
                error_message,
                duration_ms,
            ],
        )
        .map_err(|_| AbiError::Operation)?;
    }

    match apply_result {
        Ok(()) => {
            info!(
                "addon '{}': migracja '{}' applied w {} ms (path={:?})",
                addon_id, mig.name, duration_ms, mig.full_path
            );
            Ok(ApplyOutcome::Applied)
        }
        Err(err) => {
            warn!(
                "addon '{}': migracja '{}' FAILED ({} ms): {}",
                addon_id, mig.name, duration_ms, err
            );
            Err(AbiError::Operation)
        }
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tmp_home<F: FnOnce()>(f: F) {
        let _guard = super::super::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }

    fn setup_core_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("core db init")
    }

    fn write_migrations(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
    }

    #[test]
    fn test_apply_001_init_creates_table() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            write_migrations(
                &mig_dir,
                &[("001_init.sql", "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL);")],
            );
            let core_db = setup_core_db();
            let n = apply_migrations(
                "mig-init-test",
                "0.1.0",
                "migrations",
                bundle.path(),
                &core_db,
            )
            .unwrap();
            assert_eq!(n, 1);

            // Sprawdz tabele w per-addon DB.
            let pool = super::super::storage_sql::open_addon_db("mig-init-test").unwrap();
            let conn = pool.get().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='items'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            super::super::storage_sql::close_addon_db("mig-init-test");
        });
    }

    #[test]
    fn test_apply_multiple_migrations_in_order() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            // Specjalnie napisane out-of-order na FS — runner ma sortowac.
            write_migrations(
                &mig_dir,
                &[
                    ("002_add_col.sql", "ALTER TABLE items ADD COLUMN ts INTEGER;"),
                    ("001_init.sql", "CREATE TABLE items (id INTEGER PRIMARY KEY);"),
                ],
            );
            let core_db = setup_core_db();
            let n = apply_migrations("mig-order-test", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            assert_eq!(n, 2);
            let pool = super::super::storage_sql::open_addon_db("mig-order-test").unwrap();
            let conn = pool.get().unwrap();
            // ts kolumna istnieje (drugi migration zaaplikowany po pierwszym).
            let info_cols: Vec<String> = conn
                .prepare("PRAGMA table_info(items)")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            assert!(info_cols.contains(&"ts".to_string()));
            super::super::storage_sql::close_addon_db("mig-order-test");
        });
    }

    #[test]
    fn test_idempotent_reapply_skip() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            write_migrations(&mig_dir, &[("001_init.sql", "CREATE TABLE x (id INTEGER);")]);
            let core_db = setup_core_db();
            let n1 = apply_migrations("mig-idem", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            assert_eq!(n1, 1);
            let n2 = apply_migrations("mig-idem", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            assert_eq!(n2, 0, "drugi run skip");
            super::super::storage_sql::close_addon_db("mig-idem");
        });
    }

    #[test]
    fn test_hash_modification_detected_and_rejected() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            write_migrations(&mig_dir, &[("001_init.sql", "CREATE TABLE x (id INTEGER);")]);
            let core_db = setup_core_db();
            apply_migrations("mig-hash", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            // Zmodyfikuj tresc migracji po apply.
            std::fs::write(mig_dir.join("001_init.sql"), "CREATE TABLE x (id INTEGER, extra TEXT);").unwrap();
            let res = apply_migrations("mig-hash", "0.1.0", "migrations", bundle.path(), &core_db);
            assert!(res.is_err(), "hash mismatch powinien byc wykryty");
            super::super::storage_sql::close_addon_db("mig-hash");
        });
    }

    #[test]
    fn test_failed_migration_rolls_back_atomic() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            // Pierwszy statement OK, drugi syntax error — caly batch ma byc rolled back.
            write_migrations(
                &mig_dir,
                &[(
                    "001_bad.sql",
                    "CREATE TABLE good (id INTEGER); CREATE TABLE bad (id, ;",
                )],
            );
            let core_db = setup_core_db();
            let res = apply_migrations("mig-fail", "0.1.0", "migrations", bundle.path(), &core_db);
            assert!(res.is_err());
            let pool = super::super::storage_sql::open_addon_db("mig-fail").unwrap();
            let conn = pool.get().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='good'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "tabela 'good' nie powinna istniec po rollback");
            super::super::storage_sql::close_addon_db("mig-fail");
        });
    }

    #[test]
    fn test_invalid_filename_ignored() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            write_migrations(
                &mig_dir,
                &[
                    ("001_init.sql", "CREATE TABLE a (id INTEGER);"),
                    ("README.md", "ignored markdown"),
                    ("not-numbered.sql", "SELECT 1;"),
                    ("01_too_short.sql", "SELECT 1;"),
                ],
            );
            let core_db = setup_core_db();
            let n = apply_migrations("mig-invalid", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            assert_eq!(n, 1, "tylko 001_init.sql zaakceptowane");
            super::super::storage_sql::close_addon_db("mig-invalid");
        });
    }

    #[test]
    fn test_partial_fail_status_recorded() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let mig_dir = bundle.path().join("migrations");
            write_migrations(&mig_dir, &[("001_bad.sql", "CREATE TABL x (;")]);
            let core_db = setup_core_db();
            let _ = apply_migrations("mig-status", "0.1.0", "migrations", bundle.path(), &core_db);
            let conn = core_db.lock().unwrap();
            let status: String = conn
                .query_row(
                    "SELECT status FROM addon_migrations_applied WHERE addon_id=?1 AND migration_name=?2",
                    params!["mig-status", "001_bad.sql"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "failed");
            drop(conn);
            super::super::storage_sql::close_addon_db("mig-status");
        });
    }

    #[test]
    fn test_migrations_dir_path_traversal_rejected() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let core_db = setup_core_db();
            assert!(apply_migrations("mig-trav", "0.1.0", "../etc", bundle.path(), &core_db).is_err());
            assert!(apply_migrations("mig-trav", "0.1.0", "/tmp", bundle.path(), &core_db).is_err());
        });
    }

    #[test]
    fn test_missing_migrations_dir_is_ok() {
        with_tmp_home(|| {
            let bundle = tempfile::tempdir().unwrap();
            let core_db = setup_core_db();
            let n = apply_migrations("mig-nodir", "0.1.0", "migrations", bundle.path(), &core_db).unwrap();
            assert_eq!(n, 0);
            super::super::storage_sql::close_addon_db("mig-nodir");
        });
    }
}
