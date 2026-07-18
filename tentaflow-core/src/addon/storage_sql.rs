// =============================================================================
// Plik: addon/storage_sql.rs
// Opis: Per-addon SQLite connection pool (F1a §6.5 M1.W4 TentaVision). Kazdy
//       addon dostaje wlasny plik `~/.tentaflow/addons/<addon_id>/data.db`
//       z WAL mode, foreign_keys=ON, busy_timeout=5s. Pool size 5 — wspolny
//       limit dla host functions sql_exec_v1, sql_query_v1, sql_query_one_v1,
//       sql_transaction_v1. Globalny rejestr keyed by addon_id zywie tu;
//       lifecycle (install/uninstall) wola open_addon_db / close_addon_db.
// =============================================================================

use std::sync::OnceLock;
use std::time::Duration;

use dashmap::DashMap;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OpenFlags;

use super::errors::AbiError;
use super::fs_sandbox::addon_db_path;

// =============================================================================
// Stale konfiguracyjne pool
// =============================================================================

/// Liczba polaczen w pool per addon. Wartosc dobrana empirycznie: F1a addony
/// rzadko maja wiecej niz 2-3 rownoleglosci (service tick + ui_render +
/// sporadyczny event handler). 5 daje zapas bez nadmiernej rezerwacji RAM.
const POOL_MAX_SIZE: u32 = 5;

/// Timeout pobierania polaczenia z pool — guard przed deadlock-iem.
const POOL_GET_TIMEOUT: Duration = Duration::from_secs(10);

/// SQLite busy_timeout: jak dlugo czekac gdy inny pisarz trzyma lock.
/// 5s wystarcza WAL mode na typowy peak load (sql_exec batch +
/// ui_render lekki SELECT obok). Zapobiega `SQLITE_BUSY` w normalnym
/// uzyciu, jednoczesnie nie maskuje patologicznych deadlock-ow.
const SQLITE_BUSY_TIMEOUT_MS: i64 = 5000;

// =============================================================================
// AddonDbPool — opaque wrapper na r2d2::Pool
// =============================================================================

/// Pula polaczen SQLite dla pojedynczego addonu. Klonowanie jest tanie (Arc
/// wewnetrznie). Pool jest lazy — pierwsze `get()` otwiera plik i runuje
/// init pragmas.
#[derive(Clone)]
pub struct AddonDbPool {
    inner: r2d2::Pool<SqliteConnectionManager>,
}

impl AddonDbPool {
    /// Pobiera polaczenie z pool. Zwraca `AbiError::Operation` na timeout
    /// lub blad otwarcia. Caller dostaje `PooledConnection` ktore wraca do
    /// pool po dropie.
    pub fn get(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, AbiError> {
        // r2d2::Error z `get_timeout` reprezentuje wylacznie wait timeout
        // (jedyna gałąź errora w bibliotece — patrz lib.rs r2d2 0.8.10).
        // Addon dostaje Timeout zamiast generycznego Operation.
        self.inner
            .get_timeout(POOL_GET_TIMEOUT)
            .map_err(|_| AbiError::Timeout)
    }

    /// Liczba aktualnie zaalokowanych polaczen (do testow / metryk).
    pub fn state(&self) -> r2d2::State {
        self.inner.state()
    }
}

// =============================================================================
// Globalny rejestr pool per addon
// =============================================================================

/// Mapa (org_id, addon_id) -> pool. DashMap zapewnia per-shard lock — open
/// i close dla roznych addonow nie konkuruja. Lifecycle: install_addon woła
/// `open_addon_db`, uninstall_addon → `close_addon_db`.
///
/// F2 P1.b — the key gained `org_id` so two tenants installing the same
/// addon_id keep isolated SQLite files (each lives under
/// `~/.tentaflow/orgs/<org_id>/addons/<addon_id>/data.db`).
fn registry() -> &'static DashMap<(String, String), AddonDbPool> {
    static REGISTRY: OnceLock<DashMap<(String, String), AddonDbPool>> = OnceLock::new();
    REGISTRY.get_or_init(DashMap::new)
}

/// Otwiera (lub zwraca istniejacy) pool dla danego addona w danej organizacji.
///
/// Pierwsze wywolanie:
/// - Tworzy plik `data.db` w `addon_data_dir(org_id, addon_id)` (przez fs_sandbox).
/// - Konfiguruje pragmas: journal=WAL, foreign_keys=ON, synchronous=NORMAL,
///   temp_store=MEMORY, busy_timeout=5000ms.
/// - Buduje r2d2::Pool z max_size = POOL_MAX_SIZE.
///
/// Kolejne wywolania zwracaja sklonowany handle do tego samego pool.
///
/// Bezpieczenstwo: oba identyfikatory walidowane przez `addon_db_path` → `validate_addon_id`.
pub fn open_addon_db(org_id: &str, addon_id: &str) -> Result<AddonDbPool, AbiError> {
    if addon_storage_frozen() {
        // Migracja katalogu danych addonow w toku — pliki wlasnie sa
        // przenoszone; otwarcie poola trzymaloby uchwyt na starej sciezce.
        return Err(AbiError::Operation);
    }
    let key = (org_id.to_string(), addon_id.to_string());
    if let Some(existing) = registry().get(&key) {
        return Ok(existing.clone());
    }

    let db_path = addon_db_path(org_id, addon_id)?;

    let manager = SqliteConnectionManager::file(&db_path)
        .with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)
        .with_init(|conn| {
            // PRAGMAs ustawione raz przy kazdym nowym connection w pool.
            // WAL zapewnia jednoczesny reader+writer; foreign_keys default
            // OFF w SQLite — musimy wlaczyc per-connection.
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "temp_store", "MEMORY")?;
            conn.pragma_update(None, "busy_timeout", SQLITE_BUSY_TIMEOUT_MS)?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS __tentaflow_sync_captures (
                    capture_id TEXT PRIMARY KEY,
                    org_id TEXT NOT NULL,
                    addon_id TEXT NOT NULL,
                    table_name TEXT NOT NULL,
                    action TEXT NOT NULL CHECK(action IN ('insert','update','delete')),
                    resource_type TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    query TEXT NOT NULL,
                    params_json TEXT NOT NULL,
                    rows_affected INTEGER NOT NULL,
                    last_insert_id INTEGER NOT NULL,
                    actor_user_id TEXT NULL,
                    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
                    operation_id TEXT NULL,
                    error_message TEXT NULL,
                    created_at_ms INTEGER NOT NULL,
                    ledgered_at_ms INTEGER NULL
                );
                CREATE INDEX IF NOT EXISTS idx_tentaflow_sync_captures_status
                    ON __tentaflow_sync_captures(status, created_at_ms);
                CREATE TABLE IF NOT EXISTS __tentaflow_sync_applied (
                    operation_id TEXT PRIMARY KEY,
                    applied_at_ms INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS __tentaflow_sync_conflicts (
                    operation_id TEXT PRIMARY KEY,
                    org_id TEXT NOT NULL,
                    addon_id TEXT NOT NULL,
                    table_name TEXT NOT NULL,
                    resource_type TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    action TEXT NOT NULL,
                    source_node_id TEXT NOT NULL,
                    error_kind TEXT NOT NULL,
                    error_message TEXT NOT NULL,
                    capture_json TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','resolved','ignored','superseded')),
                    created_at_ms INTEGER NOT NULL,
                    resolved_at_ms INTEGER NULL,
                    resolution TEXT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_tentaflow_sync_conflicts_status
                    ON __tentaflow_sync_conflicts(status, created_at_ms);
                CREATE INDEX IF NOT EXISTS idx_tentaflow_sync_conflicts_resource
                    ON __tentaflow_sync_conflicts(resource_type, resource_id);",
            )?;
            let has_operation_id = conn
                .prepare("PRAGMA table_info(__tentaflow_sync_captures)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == "operation_id");
            if !has_operation_id {
                conn.execute(
                    "ALTER TABLE __tentaflow_sync_captures ADD COLUMN operation_id TEXT NULL",
                    [],
                )?;
            }
            Ok(())
        });

    let inner = r2d2::Pool::builder()
        .max_size(POOL_MAX_SIZE)
        .connection_timeout(POOL_GET_TIMEOUT)
        .build(manager)
        .map_err(|_| AbiError::Operation)?;

    let pool = AddonDbPool { inner };
    registry().insert(key, pool.clone());
    Ok(pool)
}

/// Usuwa pool z rejestru dla pary (org, addon). Wszystkie polaczenia zostana
/// zamkniete gdy ich uchwyty zostana zwolnione (Arc drop). Plik `data.db`
/// NIE jest usuwany (user moze chciec backup). Czyszczenie pliku manualne.
pub fn close_addon_db(org_id: &str, addon_id: &str) {
    registry().remove(&(org_id.to_string(), addon_id.to_string()));
}

/// Pobiera pool dla addona jezeli istnieje w rejestrze. Uzywane przez
/// host functions: SQL pool MUSI byc juz otwarty przez install_addon —
/// brak pool → blad konfiguracji (addon nie zadeklarowal [storage] sql=true
/// lub install fail).
pub fn get_addon_pool(org_id: &str, addon_id: &str) -> Option<AddonDbPool> {
    if addon_storage_frozen() {
        return None;
    }
    registry()
        .get(&(org_id.to_string(), addon_id.to_string()))
        .map(|p| p.clone())
}

/// Zamraza dostep do per-addon SQLite na czas migracji katalogu danych
/// addonow (Ustawienia → Magazyn danych). Przy `frozen=true` wszystkie poole
/// sa zrzucane z rejestru — polaczenia zamykaja sie gdy ostatnie uchwyty
/// opadna, a nowe otwarcia sa odrzucane do odmrozenia.
pub fn set_addon_storage_frozen(frozen: bool) {
    ADDON_STORAGE_FROZEN.store(frozen, std::sync::atomic::Ordering::SeqCst);
    if frozen {
        registry().clear();
    }
}

fn addon_storage_frozen() -> bool {
    ADDON_STORAGE_FROZEN.load(std::sync::atomic::Ordering::SeqCst)
}

static ADDON_STORAGE_FROZEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
        // Also pin TENTAFLOW_HOME: tentaflow_home() prefers the repo's live
        // .runtime/ over HOME, so HOME alone would not isolate the test.
        let prev_tf = std::env::var_os("TENTAFLOW_HOME");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(p) = prev_tf {
            std::env::set_var("TENTAFLOW_HOME", p);
        } else {
            std::env::remove_var("TENTAFLOW_HOME");
        }
    }

    #[test]
    fn test_open_addon_db_creates_file_with_wal_mode() {
        with_tmp_home(|| {
            let pool = open_addon_db("org-default", "wal-test").expect("open");
            let conn = pool.get().expect("connection");
            let mode: String = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .expect("journal_mode query");
            assert_eq!(mode.to_lowercase(), "wal");
            let fk: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .expect("foreign_keys query");
            assert_eq!(fk, 1, "foreign_keys ON");
            close_addon_db("org-default", "wal-test");
        });
    }

    #[test]
    fn test_pool_returns_connection() {
        with_tmp_home(|| {
            let pool = open_addon_db("org-default", "conn-test").expect("open");
            let conn = pool.get().expect("get connection");
            let val: i64 = conn
                .query_row("SELECT 1", [], |row| row.get(0))
                .expect("select 1");
            assert_eq!(val, 1);
            close_addon_db("org-default", "conn-test");
        });
    }

    #[test]
    fn test_two_addons_have_separate_pools_and_files() {
        with_tmp_home(|| {
            let p1 = open_addon_db("org-default", "alpha").expect("alpha");
            let p2 = open_addon_db("org-default", "beta").expect("beta");

            // Rozne pliki — utworz tabele w alpha, sprawdz ze brak jej w beta.
            {
                let c1 = p1.get().expect("c1");
                c1.execute("CREATE TABLE foo (x INTEGER)", []).unwrap();
            }
            {
                let c2 = p2.get().expect("c2");
                let count: i64 = c2
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='foo'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0, "tabela foo widoczna tylko w alpha");
            }

            close_addon_db("org-default", "alpha");
            close_addon_db("org-default", "beta");
        });
    }

    #[test]
    fn test_close_addon_db_purges_pool() {
        with_tmp_home(|| {
            let _ = open_addon_db("org-default", "purge-test").expect("open");
            assert!(get_addon_pool("org-default", "purge-test").is_some());
            close_addon_db("org-default", "purge-test");
            assert!(
                get_addon_pool("org-default", "purge-test").is_none(),
                "pool wyczyszczony z rejestru"
            );
        });
    }

    #[test]
    fn test_concurrent_connections_from_pool() {
        with_tmp_home(|| {
            let pool = open_addon_db("org-default", "concurrent-test").expect("open");
            // Pobierz POOL_MAX_SIZE polaczen jednoczesnie — wszystkie OK.
            let conns: Vec<_> = (0..POOL_MAX_SIZE)
                .map(|_| pool.get().expect("connection"))
                .collect();
            assert_eq!(conns.len() as u32, POOL_MAX_SIZE);
            assert_eq!(pool.state().connections, POOL_MAX_SIZE);
            drop(conns);
            close_addon_db("org-default", "concurrent-test");
        });
    }

    #[test]
    fn test_invalid_addon_id_rejected_by_open() {
        with_tmp_home(|| {
            assert!(open_addon_db("org-default", "../etc").is_err());
            assert!(open_addon_db("org-default", "with/slash").is_err());
            assert!(open_addon_db("org-default", "").is_err());
        });
    }
}
