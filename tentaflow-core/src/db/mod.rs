// =============================================================================
// Plik: db/mod.rs
// Opis: Modul bazy danych SQLite - inicjalizacja, pool, migracje.
// =============================================================================

pub mod migrations;
pub mod models;
pub mod repository;
pub mod seed;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::info;

/// Pool polaczen SQLite (single-writer, multi-reader)
pub type DbPool = Arc<Mutex<Connection>>;

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
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    conn.pragma_update(None, "optimize", "0x10002")?;
    info!("WAL checkpoint + optimize wykonane");
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

    // Seed domyslnych danych
    seed::seed_defaults(&conn)?;

    let pool = Arc::new(Mutex::new(conn));
    set_global_pool(pool.clone());

    // Upgrade path for PR5: copy `trusted_nodes` rows + parse legacy
    // `settings.trusted_contact:*` JSON entries into peer_persisted /
    // peer_hints. Idempotent (INSERT OR IGNORE), so a second startup is a
    // no-op once both source sets are empty.
    match repository::migrate_settings_trusted_contacts_to_peer_hints(&pool) {
        Ok(n) if n > 0 => info!("Migrated {} trusted peer rows into peer_persisted", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("peer_persisted migration failed: {}", e),
    }

    info!("Baza danych zainicjalizowana pomyslnie");

    Ok(pool)
}
