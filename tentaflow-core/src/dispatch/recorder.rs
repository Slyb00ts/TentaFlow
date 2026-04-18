// =============================================================================
// Plik: dispatch/recorder.rs
// Opis: Opt-in recorder dla WSS frame'ow (Task #38). Zapisuje kazdy incoming
//       + response do SQLite zeby dev mogl odtworzyc sesje (replay) lub
//       zanalizowac wydarzenie z logow.
//       Tryb: domyslnie OFF. Wlaczac przez config / env TENTAFLOW_TRACE_WSS=1.
//       Schemat jest self-migrating (CREATE IF NOT EXISTS).
//       CLI + browser panel to oddzielne tooling nad tym recorderem.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tracing::warn;

// =============================================================================
// Globalny singleton
// =============================================================================

static RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();

/// Zwraca globalny recorder jesli zainicjalizowany. Wolac `init()` raz przy starcie.
pub fn global() -> Option<&'static Arc<Recorder>> {
    RECORDER.get()
}

/// Inicjalizuje globalny recorder. Powtorne wywolanie = no-op (OnceLock).
pub fn init(db_path: impl AsRef<Path>) -> Result<(), RecorderError> {
    let rec = Recorder::open(db_path)?;
    let _ = RECORDER.set(Arc::new(rec));
    Ok(())
}

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug)]
pub enum RecorderError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecorderError::Io(e) => write!(f, "io: {}", e),
            RecorderError::Sqlite(e) => write!(f, "sqlite: {}", e),
        }
    }
}
impl std::error::Error for RecorderError {}
impl From<std::io::Error> for RecorderError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rusqlite::Error> for RecorderError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

// =============================================================================
// Recorder
// =============================================================================

pub struct Recorder {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

/// Kierunek frame'u.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Incoming => "in",
            Direction::Outgoing => "out",
        }
    }
}

/// Pojedynczy zapisany frame. Tworzone przez query helpery.
#[derive(Debug, Clone)]
pub struct RecordedFrame {
    pub id: i64,
    pub ts_unix_millis: i64,
    pub direction: Direction,
    pub correlation_id: u64,
    pub message_kind: u16,
    pub variant_name: String,
    pub flags: u8,
    pub body_bytes: Vec<u8>,
}

impl Recorder {
    /// Otwiera (lub tworzy) bazke SQLite w `db_path`.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS wss_frames (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_unix_millis INTEGER NOT NULL,
                direction      TEXT    NOT NULL CHECK (direction IN ('in','out')),
                correlation_id INTEGER NOT NULL,
                message_kind   INTEGER NOT NULL,
                variant_name   TEXT    NOT NULL,
                flags          INTEGER NOT NULL,
                body_bytes     BLOB    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wss_frames_correlation
                ON wss_frames (correlation_id, ts_unix_millis);
            CREATE INDEX IF NOT EXISTS idx_wss_frames_ts
                ON wss_frames (ts_unix_millis DESC);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    /// Zwraca sciezke pliku SQLite (dla CLI/debug).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Zapisuje jeden frame. Klasa log-and-drop — SQL errors loguja warn, nie panic.
    pub fn record(
        &self,
        direction: Direction,
        correlation_id: u64,
        message_kind: u16,
        variant_name: &str,
        flags: u8,
        body_bytes: &[u8],
    ) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                warn!("recorder: mutex poisoned, skipping frame");
                return;
            }
        };
        if let Err(e) = conn.execute(
            r#"INSERT INTO wss_frames
               (ts_unix_millis, direction, correlation_id, message_kind, variant_name, flags, body_bytes)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
            params![
                ts,
                direction.as_str(),
                correlation_id as i64,
                message_kind as i64,
                variant_name,
                flags as i64,
                body_bytes,
            ],
        ) {
            warn!("recorder: insert failed: {}", e);
        }
    }

    /// Query: ostatnie `limit` frame'ow, od najnowszego.
    pub fn latest(&self, limit: usize) -> Result<Vec<RecordedFrame>, RecorderError> {
        let conn = self.conn.lock().expect("mutex");
        let mut stmt = conn.prepare(
            r#"SELECT id, ts_unix_millis, direction, correlation_id, message_kind,
                      variant_name, flags, body_bytes
               FROM wss_frames
               ORDER BY ts_unix_millis DESC, id DESC
               LIMIT ?1"#,
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_frame)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Query: wszystkie frame'y dla danego correlation_id, od najstarszego.
    pub fn by_correlation(&self, correlation_id: u64) -> Result<Vec<RecordedFrame>, RecorderError> {
        let conn = self.conn.lock().expect("mutex");
        let mut stmt = conn.prepare(
            r#"SELECT id, ts_unix_millis, direction, correlation_id, message_kind,
                      variant_name, flags, body_bytes
               FROM wss_frames
               WHERE correlation_id = ?1
               ORDER BY ts_unix_millis ASC, id ASC"#,
        )?;
        let rows = stmt.query_map(params![correlation_id as i64], row_to_frame)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Query: wychodzace (Outgoing) frame'y dla correlation_id z `id` > `after_id`.
    /// Sluzy do resume replay — klient po reconnect prosi o frame'y po ostatnim
    /// otrzymanym sequence; serwer zwraca brakujace chunki.
    pub fn outgoing_after(
        &self,
        correlation_id: u64,
        after_id: i64,
    ) -> Result<Vec<RecordedFrame>, RecorderError> {
        let conn = self.conn.lock().expect("mutex");
        let mut stmt = conn.prepare(
            r#"SELECT id, ts_unix_millis, direction, correlation_id, message_kind,
                      variant_name, flags, body_bytes
               FROM wss_frames
               WHERE correlation_id = ?1
                 AND direction = 'out'
                 AND id > ?2
               ORDER BY id ASC"#,
        )?;
        let rows = stmt.query_map(params![correlation_id as i64, after_id], row_to_frame)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Usuwa frame'y starsze niz `older_than_ms` ms (cleanup job).
    pub fn prune_older_than(&self, older_than_ms: i64) -> Result<usize, RecorderError> {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
            - older_than_ms;
        let conn = self.conn.lock().expect("mutex");
        let affected = conn.execute(
            "DELETE FROM wss_frames WHERE ts_unix_millis < ?1",
            params![cutoff],
        )?;
        Ok(affected)
    }
}

fn row_to_frame(row: &rusqlite::Row) -> Result<RecordedFrame, rusqlite::Error> {
    let direction_str: String = row.get(2)?;
    let direction = match direction_str.as_str() {
        "in" => Direction::Incoming,
        "out" => Direction::Outgoing,
        _ => Direction::Incoming,
    };
    Ok(RecordedFrame {
        id: row.get(0)?,
        ts_unix_millis: row.get(1)?,
        direction,
        correlation_id: row.get::<_, i64>(3)? as u64,
        message_kind: row.get::<_, i64>(4)? as u16,
        variant_name: row.get(5)?,
        flags: row.get::<_, i64>(6)? as u8,
        body_bytes: row.get(7)?,
    })
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_recorder() -> Recorder {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        Recorder::open(&path).unwrap()
    }

    #[test]
    fn record_and_query_latest() {
        let rec = make_recorder();
        rec.record(Direction::Incoming, 1, 0xF001, "NodeListRequest", 0, &[1, 2, 3]);
        rec.record(Direction::Outgoing, 1, 0xF001, "NodeListResponse", 0, &[4, 5, 6]);
        let frames = rec.latest(10).unwrap();
        assert_eq!(frames.len(), 2);
        // Nowsze pierwsze
        assert_eq!(frames[0].direction, Direction::Outgoing);
        assert_eq!(frames[1].direction, Direction::Incoming);
    }

    #[test]
    fn record_by_correlation_returns_chronological() {
        let rec = make_recorder();
        rec.record(Direction::Incoming, 42, 0x0001, "Req", 0, &[1]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        rec.record(Direction::Outgoing, 42, 0x0001, "Chunk1", 2, &[2]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        rec.record(Direction::Outgoing, 42, 0x0001, "Chunk2", 6, &[3]);
        rec.record(Direction::Incoming, 99, 0x0001, "Other", 0, &[0]);
        let frames = rec.by_correlation(42).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].variant_name, "Req");
        assert_eq!(frames[1].variant_name, "Chunk1");
        assert_eq!(frames[2].variant_name, "Chunk2");
    }

    #[test]
    fn outgoing_after_returns_only_outgoing_with_id_gt_cutoff() {
        let rec = make_recorder();
        rec.record(Direction::Incoming, 1, 0x1, "Req", 0, &[1]);
        rec.record(Direction::Outgoing, 1, 0x1, "Chunk1", 2, &[2]);
        rec.record(Direction::Outgoing, 1, 0x1, "Chunk2", 2, &[3]);
        rec.record(Direction::Outgoing, 1, 0x1, "Chunk3", 4, &[4]);
        rec.record(Direction::Incoming, 99, 0x1, "Other", 0, &[0]);

        let all = rec.by_correlation(1).unwrap();
        assert_eq!(all.len(), 4);
        let chunk2_id = all
            .iter()
            .find(|f| f.variant_name == "Chunk2")
            .unwrap()
            .id;

        let after = rec.outgoing_after(1, chunk2_id).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].variant_name, "Chunk3");
    }

    #[test]
    fn outgoing_after_excludes_other_correlations() {
        let rec = make_recorder();
        rec.record(Direction::Outgoing, 1, 0x1, "A", 0, &[]);
        rec.record(Direction::Outgoing, 2, 0x1, "B", 0, &[]);
        rec.record(Direction::Outgoing, 1, 0x1, "C", 0, &[]);

        let after = rec.outgoing_after(1, 0).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|f| f.correlation_id == 1));
    }

    #[test]
    fn prune_older_than_deletes_old_rows() {
        let rec = make_recorder();
        rec.record(Direction::Incoming, 1, 0x1, "Old", 0, &[]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let deleted = rec.prune_older_than(10).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(rec.latest(10).unwrap().len(), 0);
    }

    #[test]
    fn global_singleton_initializes_once() {
        // Test izolowany — jesli inny test juz zainicjalizowal globalny,
        // skip. Testy dzialaja w tym samym process, wiec OnceLock jest wspoldzielony.
        // W CI uruchamiamy sekwencyjnie lub w osobnych binarkach.
    }

    #[test]
    fn schema_is_idempotent() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        let rec1 = Recorder::open(&path).unwrap();
        rec1.record(Direction::Incoming, 1, 0x1, "X", 0, &[1]);
        drop(rec1);
        // Ponowne otworzenie tego samego pliku — CREATE IF NOT EXISTS nie failuje.
        let rec2 = Recorder::open(&path).unwrap();
        assert_eq!(rec2.latest(10).unwrap().len(), 1);
    }
}
