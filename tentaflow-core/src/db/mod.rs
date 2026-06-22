// =============================================================================
// Plik: db/mod.rs
// Opis: Modul bazy danych SQLite - inicjalizacja, pool, migracje.
// =============================================================================

pub mod legal_documents;
pub mod migrations;
pub mod models;
pub mod repository;
pub mod seed;

use anyhow::Result;
use parking_lot::{Mutex, MutexGuard};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tracing::info;

/// Uchwyt do bazy SQLite współdzielony przez cały runtime.
pub type DbPool = Arc<Db>;

/// Błąd dostępu do bazy. Implementuje `Display`/`Error`, więc dotychczasowe
/// `.map_err(|e| anyhow!("...: {e}"))` przy `db.read()/db.write()` działają bez zmian.
#[derive(Debug)]
pub enum DbError {
    /// Pula odczytu nie wydała połączenia w `connection_timeout` (wyczerpana / I/O).
    Pool(r2d2::Error),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Pool(e) => write!(f, "db read pool: {e}"),
        }
    }
}

impl std::error::Error for DbError {}

/// Warstwa dostępu do SQLite rozdzielająca odczyty od zapisów.
///
/// SQLite w trybie WAL pozwala na wielu równoległych czytelników i JEDNEGO
/// pisarza. Dawniej cały runtime dzielił jedno `Mutex<Connection>`, więc każdy
/// odczyt serializował się za niezwiązanym zapisem. Tu:
/// - `read()` wydaje połączenie z puli r2d2 (`query_only`) — odczyty idą równolegle,
/// - `write()` bierze jedyne połączenie pisarza spod `Mutex` — zapisy serializują się
///   między sobą (nieuniknione w SQLite), ale NIE blokują odczytów.
///
/// Bazy in-memory (testy) nie mają puli: każde `:memory:` to osobna pusta baza,
/// więc `read()` spada wtedy na połączenie pisarza.
#[derive(Debug)]
pub struct Db {
    writer: Mutex<Connection>,
    read_pool: Option<r2d2::Pool<SqliteConnectionManager>>,
}

/// Wynik `Db::read()` — połączenie z puli albo (dla in-memory) uchwyt pisarza.
/// Dereferencuje do `Connection`, więc kod wołający używa go jak dawnego guarda.
pub enum ReadGuard<'a> {
    Pooled(r2d2::PooledConnection<SqliteConnectionManager>),
    Writer(MutexGuard<'a, Connection>),
}

impl Deref for ReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            ReadGuard::Pooled(c) => c,
            ReadGuard::Writer(c) => c,
        }
    }
}

impl DerefMut for ReadGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        match self {
            ReadGuard::Pooled(c) => c,
            ReadGuard::Writer(c) => c,
        }
    }
}

impl Db {
    /// Wydaje połączenie do odczytu (pula r2d2; in-memory → pisarz).
    pub fn read(&self) -> std::result::Result<ReadGuard<'_>, DbError> {
        match &self.read_pool {
            Some(pool) => pool.get().map(ReadGuard::Pooled).map_err(DbError::Pool),
            None => Ok(ReadGuard::Writer(self.writer.lock())),
        }
    }

    /// Bierze jedyne połączenie pisarza. Zapisy serializują się tutaj, nie na odczytach.
    /// Zwraca `Result` dla zgodności wzorców wołających (`?`, `.map_err`); nigdy nie błądzi.
    pub fn write(&self) -> std::result::Result<MutexGuard<'_, Connection>, DbError> {
        Ok(self.writer.lock())
    }

    /// Konstruktor dla testów / baz in-memory: jedno połączenie, bez puli odczytu.
    pub fn from_connection(conn: Connection) -> Self {
        Db {
            writer: Mutex::new(conn),
            read_pool: None,
        }
    }
}

/// Globalny uchwyt do poola — ustawiony w `init()`. Pozwala modulom ktore nie
/// dostaja DbPool przez argumenty (np. transcript_store wolany z reverse_request)
/// na zapis trwaly do SQLite bez przekazywania referencji przez polowe stacku.
static GLOBAL_POOL: OnceLock<DbPool> = OnceLock::new();

/// Ustawia globalny pool — wolane raz, w `init()`. Kolejne wywolania ignorowane.
fn set_global_pool(pool: DbPool) {
    let _ = GLOBAL_POOL.set(pool);
}

/// Zwraca globalny pool jesli `init()` zostal wywolany. None w testach bez DB.
pub fn global_pool() -> Option<DbPool> {
    GLOBAL_POOL.get().cloned()
}

/// Wymusza WAL checkpoint — migruje wszystkie strony z pliku -wal do glownej
/// bazy i obciąż WAL. Wolac przy shutdown zeby nie zostawiac niesfl ushowanych
/// zmian (wazne szczegolnie po SIGKILL).
pub fn checkpoint_wal(pool: &DbPool) -> Result<()> {
    let conn = pool.write()?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    conn.pragma_update(None, "optimize", "0x10002")?;
    info!("WAL checkpoint + optimize wykonane");
    Ok(())
}

/// Pragmy ustawiane na KAŻDYM połączeniu w puli odczytu. `query_only=ON` gwarantuje,
/// że pula nigdy nie zapisuje (zapisy idą wyłącznie przez `Db::write()`); `journal_mode`
/// jest atrybutem pliku (ustawiony przez pisarza), tu dbamy o per-connection busy_timeout
/// i foreign_keys, by odczyty zachowywały się spójnie.
fn init_read_connection(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "query_only", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -65536)?;
    conn.pragma_update(None, "mmap_size", 268435456_i64)?;
    Ok(())
}

/// Inicjalizuje baze danych SQLite.
/// Tworzy plik jesli nie istnieje, uruchamia migracje i seed.
pub fn init(db_path: &Path) -> Result<DbPool> {
    info!("Inicjalizacja bazy danych: {:?}", db_path);

    let conn = Connection::open(db_path)?;

    // Pragmy wydajnosciowe SQLite. cache_size=-65536 (64MB) dla high-throughput
    // mesh_topology upsertow i per-request metryk. busy_timeout=5000 — pod mesh
    // gossip burstem writery z roznych taskow moga kolidowac; bez timeoutu SQLITE_BUSY
    // wraca natychmiast. wal_autocheckpoint=2000 (8MB) — checkpoint rzadziej,
    // mniej fsync na tick.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA mmap_size=268435456;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA busy_timeout=5000;\
         PRAGMA wal_autocheckpoint=2000;",
    )?;

    // Uruchom migracje
    migrations::run(&conn)?;

    // F2 P1.b — migrate any pre-F2 `~/.tentaflow/addons/<addon_id>/`
    // directories into the new per-org layout
    // `~/.tentaflow/orgs/org-default/addons/<addon_id>/`. Runs once after
    // migrations have populated the DB column; subsequent boots find the
    // legacy root gone and return Ok(0). Failure is logged, not fatal —
    // an addon whose dir refuses to move will surface a config error at
    // first runtime access, which is easier to diagnose than a boot abort.
    if let Some(home) = dirs::home_dir() {
        match crate::addon::lifecycle::migrate_addon_dirs_to_org_default(&home) {
            Ok(n) if n > 0 => info!(
                "lifecycle: migrated {} legacy addon dir(s) to per-org layout",
                n
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                "lifecycle: legacy addon dir migration partial: {} — manual cleanup may be required",
                e
            ),
        }
    }

    // F1c P5 — flow_invocations rows left in `status='running'` after a
    // crash/restart can never be finalized by a live scheduler, so reconcile
    // them to `failed/core_restart` before the new process begins issuing
    // invocations.
    match crate::flow_runtime::boot::mark_orphaned_invocations(&conn) {
        Ok(n) if n > 0 => info!("flow_runtime: reconciled {} orphaned flow_invocations", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("flow_runtime: orphan reconciliation failed: {}", e),
    }

    // Seed domyslnych danych
    seed::seed_defaults(&conn)?;

    // Pula odczytu — osobne połączenia do tego samego pliku. WAL pozwala im czytać
    // równolegle z pisarzem. Rozmiar skalowany do liczby rdzeni, ale TWARDO ograniczony:
    // na maszynach o dużej liczbie rdzeni (np. DGX, setki rdzeni) niesprowadzony rozmiar
    // tworzyłby setki połączeń, a `build()` z domyślnym `min_idle == max_size` otwierałby
    // je WSZYSTKIE od razu (każde z 256MB mmap), przekraczając `connection_timeout` →
    // "timed out waiting for connection" już przy starcie. 16 połączeń odczytu w zupełności
    // wystarcza na równoległość; `min_idle(1)` sprawia, że start otwiera tylko jedno, a
    // reszta powstaje leniwie na żądanie.
    let read_size = (num_cpus::get() as u32 * 2).clamp(4, 16);
    let manager = SqliteConnectionManager::file(db_path)
        .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_init(init_read_connection);
    let read_pool = r2d2::Pool::builder()
        .max_size(read_size)
        .min_idle(Some(1))
        .connection_timeout(std::time::Duration::from_secs(5))
        .build(manager)?;

    let pool = Arc::new(Db {
        writer: Mutex::new(conn),
        read_pool: Some(read_pool),
    });
    set_global_pool(pool.clone());

    // F1c P5 chunk B — install the flow scheduler singleton with the same
    // pool the rest of the runtime uses. Idempotent: a second `init` (test
    // harnesses, integration suites) leaves the original instance in place.
    crate::flow_runtime::scheduler::FlowScheduler::init(pool.clone());

    // Upgrade path: copy `trusted_nodes` rows + legacy contact hint settings
    // into peer_persisted / peer_hints. Idempotent (INSERT OR IGNORE), so a
    // second startup is a no-op once both source sets are empty.
    match repository::migrate_settings_trusted_contacts_to_peer_hints(&pool) {
        Ok(n) if n > 0 => info!("Migrated {} trusted peer rows into peer_persisted", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("peer_persisted migration failed: {}", e),
    }

    info!("Baza danych zainicjalizowana pomyslnie");

    Ok(pool)
}
