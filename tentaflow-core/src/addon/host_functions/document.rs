// =============================================================================
// Plik: addon/host_functions/document.rs
// Opis: Host functions document/blob store (RAG E1.3) — per-instancja magazyn
//       wgranych przez użytkownika plików (PDF/obraz, często > limit KV 1MB).
//       KV (`storage_*`) ma sufit 1MB — za mało na surowy dokument. Tu addon
//       wgrywa plik KAWAŁKAMI (chunked, bajty osobnym ptr/len — NIE base64 w
//       CBOR), odczytuje go kawałkami i kasuje. Plik przechowywany w osobnym
//       per-instance `FileBlobStore`-podobnym sharded layout pod katalogiem
//       danych instancji; rejestr (doc_id → sha/mime/size) w dedykowanym
//       SQLite `documents.db` tej instancji. `doc_parse` (E1.2) konsumuje plik.
//
// Streaming uploadu: kawałki NIE są akumulowane w pamięci. Każdy `put` dopisuje
// bajty do pliku partial `documents/tmp/<doc_id>.partial`; stan pending trzyma
// tylko metadane (ścieżka, bytes_dotąd, next_index, last_activity, total/mime).
// Twarde limity bajtów/uploadów + GC porzuconych partiali przy każdym `put`
// chronią przed OOM/DoS. Finalizacja hashuje plik strumieniowo (bloki) i robi
// atomic rename do content-addressed bloba. Odczyt `get` robi seek do offsetu
// kawałka i czyta tylko `chunk_len` bajtów — NIE ładuje całego pliku.
//
// Izolacja per instancja: katalog `<addon_data_dir>/documents/` jest unikalny
// dla każdej zainstalowanej instancji (instalator nadaje każdej instancji własny
// unikalny `addon_id` — patrz lifecycle::unique_instance_id), więc dwie
// instancje tego samego pakietu mają fizycznie rozdzielone pliki ORAZ osobny
// rejestr. Addon NIE może zderefować cudzego `doc_id` — rejestr widzi tylko
// dokumenty tej instancji. Uninstall kasuje cały `addon_data_dir`, więc pliki +
// rejestr znikają (patrz lifecycle::uninstall_instance).
// Uprawnienia: "document.read" (get/list), "document.write" (put/delete).
// Audit RiskClass::B — dokumenty mogą nieść dane regulowane.
// =============================================================================

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use rusqlite::OpenFlags;
use sha2::{Digest, Sha256};
use tentaflow_sdk_spec::{
    DocumentDeleteInput, DocumentDeleteOutput, DocumentGetInput, DocumentGetMeta, DocumentListInput,
    DocumentListOutput, DocumentMeta, DocumentPutInput, DocumentPutOutput,
};

use super::abi_helpers::{write_output_with_retry_semantics, PayloadKind};
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::addon::fs_sandbox::{addon_data_dir, validate_addon_id};
use crate::audit::RiskClass;

const PERM_DOCUMENT_READ: &str = "document.read";
const PERM_DOCUMENT_WRITE: &str = "document.write";

/// Rozmiar kawałka odczytu (`document_get_v1`). Stały, żeby addon mógł
/// deterministycznie iterować `chunk_index`. 256 KiB mieści się z zapasem w
/// typowym buforze guest i nie generuje nadmiernej liczby wywołań dla plików
/// wielomegabajtowych.
pub const DOCUMENT_CHUNK_BYTES: usize = 256 * 1024;

/// Maksymalny rozmiar pojedynczego kawałka wgrywanego (`document_put_v1`).
/// Bajty kawałka wchodzą osobnym ptr/len (nie CBOR), ale i tak ograniczamy
/// pojedynczy transfer, żeby adversarial addon nie zażądał gigantycznej
/// alokacji jednym wywołaniem. 8 MiB = sufit `ServiceCall`.
const MAX_PUT_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Maksymalna liczba kawałków jednego dokumentu — twardy limit przeciw
/// nieograniczonej akumulacji (DoS przez `total_chunks = u32::MAX`).
const MAX_TOTAL_CHUNKS: u32 = 100_000;

/// Maksymalna liczba dokumentów per instancja — guard przed zalaniem rejestru.
const MAX_DOCUMENTS_PER_INSTANCE: i64 = 10_000;

/// Maksymalna długość `mime` (sanity, nie ozdoba — trafia do rejestru).
const MAX_MIME_LEN: usize = 255;

/// Twardy sufit rozmiaru pojedynczego wgrywanego dokumentu (suma kawałków).
/// Niezależny od `document_storage_mb` — chroni dysk nawet gdy addon nie ma
/// ustawionego limitu storage. 512 MiB.
pub const MAX_PENDING_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Maksymalna liczba równoległych niezakończonych uploadów na instancję.
/// Każdy pending trzyma otwarty plik partial — guard przed wyczerpaniem
/// deskryptorów/inode przez adversarial addon otwierający tysiące uploadów.
pub const MAX_PENDING_UPLOADS_PER_INSTANCE: usize = 8;

/// Globalny (cały proces) sufit sumy bajtów wszystkich pending partiali. Twardy
/// hamulec na dysk niezależnie od liczby instancji. 2 GiB.
const MAX_TOTAL_PENDING_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// TTL porzuconego partiala (sekundy). Pending bez aktywności dłużej niż TTL jest
/// kasowany przy najbliższym sweepie (GC). 5 minut.
pub const PENDING_TTL_SECS: u64 = 5 * 60;

/// Rozmiar bloku przy strumieniowym hashowaniu pliku przy finalizacji.
const HASH_BLOCK_BYTES: usize = 256 * 1024;

// =============================================================================
// Override roota dla testów
// =============================================================================

/// Nadpisanie roota magazynu dokumentów (TYLKO testy). Gdy ustawione, katalog
/// instancji to `<root>/<org>/addons/<addon_id>/documents/` zamiast realnego
/// `addon_data_dir`. Pozwala testom trzymać dane w tempdir na /mnt/d bez dotykania
/// `$HOME`. W produkcji `None` → realny `addon_data_dir`.
fn root_override() -> &'static Mutex<Option<PathBuf>> {
    static OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Ustawia override roota (testy). Zwraca poprzednią wartość.
#[doc(hidden)]
pub fn set_root_override(root: Option<PathBuf>) -> Option<PathBuf> {
    std::mem::replace(&mut *root_override().lock().unwrap_or_else(|e| e.into_inner()), root)
}

/// Zwraca katalog `documents/` instancji, tworząc go idempotentnie. Gdy
/// override ustawiony — buduje ścieżkę ręcznie (z walidacją id); inaczej
/// deleguje do `addon_data_dir` (które samo waliduje + chmod 0700).
pub fn documents_dir(org_id: &str, addon_id: &str) -> Result<PathBuf, AbiError> {
    let base = {
        let guard = root_override().lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(root) => {
                validate_addon_id(org_id)?;
                validate_addon_id(addon_id)?;
                root.join(org_id).join("addons").join(addon_id)
            }
            None => addon_data_dir(org_id, addon_id)?,
        }
    };
    let dir = base.join("documents");
    std::fs::create_dir_all(&dir).map_err(|_| AbiError::Operation)?;
    Ok(dir)
}

/// Katalog plików partial (`documents/tmp/`) instancji, tworzony idempotentnie.
pub fn tmp_dir(dir: &Path) -> Result<PathBuf, AbiError> {
    let t = dir.join("tmp");
    std::fs::create_dir_all(&t).map_err(|_| AbiError::Operation)?;
    Ok(t)
}

/// Ścieżka pliku partial dla danego `doc_id`.
pub fn partial_path(dir: &Path, doc_id: &str) -> PathBuf {
    dir.join("tmp").join(format!("{doc_id}.partial"))
}

// =============================================================================
// Rejestr dokumentów — dedykowany SQLite per instancja
// =============================================================================

/// Otwiera (tworząc idempotentnie schemat) rejestr `documents.db` instancji.
/// Plik żyje w katalogu `documents/` instancji, więc kasuje się razem z
/// `addon_data_dir` przy uninstall. Połączenie krótkożyjące — operacje są
/// rzadkie (upload/odczyt pliku), brak potrzeby pool.
pub fn open_registry(dir: &std::path::Path) -> Result<rusqlite::Connection, AbiError> {
    let db_path = dir.join("documents.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|_| AbiError::Operation)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|_| AbiError::Operation)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|_| AbiError::Operation)?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|_| AbiError::Operation)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS documents (
            doc_id TEXT PRIMARY KEY,
            sha256 TEXT NOT NULL,
            mime TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );",
    )
    .map_err(|_| AbiError::Operation)?;
    Ok(conn)
}

/// Sharded ścieżka pliku w obrębie per-instance roota: `<dir>/blobs/<sha[0:2]>/
/// <sha[2:4]>/<sha>.bin`. Content-addressed WEWNĄTRZ instancji — dwa identyczne
/// pliki tej samej instancji deduplikują się, ale izolacja jest strukturalna
/// (osobny root per instancja), więc dedup nie przecieka między właścicielami.
pub fn blob_path(dir: &std::path::Path, sha256: &str) -> PathBuf {
    dir.join("blobs")
        .join(&sha256[0..2])
        .join(&sha256[2..4])
        .join(format!("{sha256}.bin"))
}

/// Wczytuje CAŁY dokument instancji (org_id, addon_id) po `doc_id` z document
/// store. Zwraca `(bajty, mime)`. Używane przez host fn `ingest_invoke_v1`,
/// który pobiera bajty PO STRONIE HOSTA (zamiast strumieniować je przez ABI z
/// addona, jak robi `run_ingest_pipeline` przez `document_get`), żeby zseedować
/// binarny envelope flow-ingestu. Reużywa DOKŁADNIE tę samą warstwę co
/// `document_get_v1` (rejestr instancji + content-addressed blob) — jeden store,
/// żaden obcy `doc_id` nie jest osiągalny (rejestr widzi tylko dokumenty tej
/// instancji). `NotFound` gdy wiersza nie ma; `Operation` przy błędzie I/O.
/// Mutex instancji wzajemnie wyklucza odczyt z równoległym `delete` (czytelnik
/// widzący wiersz ZAWSZE ma istniejący blob).
pub fn read_full_document(
    org_id: &str,
    addon_id: &str,
    doc_id: &str,
) -> Result<(Vec<u8>, String), AbiError> {
    validate_doc_id(doc_id)?;
    let dir = documents_dir(org_id, addon_id)?;
    let conn = open_registry(&dir)?;

    let inst_lock = instance_lock(org_id, addon_id);
    let _inst_guard = inst_lock.lock().unwrap_or_else(|e| e.into_inner());

    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT sha256, mime, size_bytes FROM documents WHERE doc_id = ?1",
            rusqlite::params![doc_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (sha256, mime, size_bytes) = row.ok_or(AbiError::NotFound)?;

    let path = blob_path(&dir, &sha256);
    let mut f = std::fs::File::open(&path).map_err(|_| AbiError::Operation)?;
    let mut bytes = Vec::with_capacity(size_bytes.max(0) as usize);
    f.read_to_end(&mut bytes).map_err(|_| AbiError::Operation)?;
    Ok((bytes, mime))
}

// =============================================================================
// Stan pending uploadów — TYLKO metadane (bajty leżą na dysku w partialu)
// =============================================================================

/// Stan jednego niezakończonego uploadu. Bajty NIE są tu trzymane — leżą w
/// pliku partial; tu jest tylko offset/postęp + znacznik aktywności dla GC.
struct PendingUpload {
    /// Ścieżka pliku partial (`documents/tmp/<doc_id>.partial`).
    path: PathBuf,
    mime: String,
    total_chunks: u32,
    /// Indeks następnego oczekiwanego kawałka (sekwencja monotoniczna od 0).
    next_index: u32,
    /// Bajty dopisane dotąd (rośnie z każdym kawałkiem).
    bytes_so_far: u64,
    /// Unix-ts ostatniej aktywności — podstawa GC (kasuj starsze niż TTL).
    last_activity: u64,
}

/// Klucz pending uploadu: (org_id, addon_id, doc_id) — instancja izolowana
/// przez `addon_id`.
type PendingKey = (String, String, String);
type PendingMap = HashMap<PendingKey, PendingUpload>;

fn pending_uploads() -> &'static Mutex<PendingMap> {
    static ACC: OnceLock<Mutex<PendingMap>> = OnceLock::new();
    ACC.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// =============================================================================
// Serializacja per-instancja — strukturalne kasowanie wyścigów
// =============================================================================

/// Rejestr muteksów per instancja: klucz `(org_id, addon_id)`. Sekcje krytyczne
/// (finalizacja put, cała delete, GC, akceptacja chunku) trzymają TEN mutex, więc
/// dwie operacje na tej samej instancji NIE wejdą równolegle. Operacje na
/// dokumentach są rzadkie (user wgrywa plik), więc gruba serializacja jest tania
/// i kasuje całą klasę wyścigów (GC-vs-finalize, chunk-0, orphan-purge, delete).
/// Czytelnicy `get` zostają lock-free dzięki publikacji blob-przed-wierszem.
fn instance_locks() -> &'static DashMap<(String, String), Arc<Mutex<()>>> {
    static LOCKS: OnceLock<DashMap<(String, String), Arc<Mutex<()>>>> = OnceLock::new();
    LOCKS.get_or_init(DashMap::new)
}

/// Zwraca `Arc<Mutex<()>>` instancji (tworzy idempotentnie). Wołający bierze
/// `.lock()` na zwróconym mutexie na czas sekcji krytycznej.
fn instance_lock(org_id: &str, addon_id: &str) -> Arc<Mutex<()>> {
    instance_locks()
        .entry((org_id.to_string(), addon_id.to_string()))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Kasuje wpis muteksu instancji z mapy. Wołane przy uninstall instancji
/// (`lifecycle::uninstall_instance`), żeby `instance_locks()` nie rosło w
/// nieskończoność dla nieistniejących już instancji (Bug 3). Wpis to mały
/// `Arc<Mutex<()>>`; gdy jakaś operacja akurat trzyma `Arc` (mało prawdopodobne
/// w trakcie uninstall — instancja jest usuwana), jej guard żyje dalej na
/// własnym `Arc`, a nowy `instance_lock` po skasowaniu utworzy świeży wpis.
pub fn forget_instance_lock(org_id: &str, addon_id: &str) {
    instance_locks().remove(&(org_id.to_string(), addon_id.to_string()));
}

/// Ścieżka pliku „finalizing" dla `doc_id`. Finalizacja renamuje
/// `<doc_id>.partial` → `<doc_id>.finalizing` PRZED slow-hashem, więc GC
/// (sweepuje tylko `*.partial`) nie widzi finalizującego partiala jako „stary".
fn finalizing_path(dir: &Path, doc_id: &str) -> PathBuf {
    dir.join("tmp").join(format!("{doc_id}.finalizing"))
}

/// GC porzuconych pending uploadów. Kasuje wpisy starsze niż `PENDING_TTL_SECS`
/// (last_activity) WRAZ z plikiem partial. Wołane przy każdym `put` oraz przy
/// starcie/uninstall. Zwraca liczbę skasowanych wpisów (do testów/diagnozy).
pub fn sweep_abandoned_pending() -> usize {
    let now = now_unix();
    let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
    let stale: Vec<(String, String, String)> = map
        .iter()
        .filter(|(_, p)| now.saturating_sub(p.last_activity) > PENDING_TTL_SECS)
        .map(|(k, _)| k.clone())
        .collect();
    for key in &stale {
        if let Some(p) = map.remove(key) {
            let _ = std::fs::remove_file(&p.path);
        }
    }
    stale.len()
}

/// Łączna suma bajtów wszystkich pending partiali w procesie (do limitu
/// globalnego). Liczona pod tym samym lockiem co reszta operacji pending.
fn total_pending_bytes(map: &PendingMap) -> u64 {
    map.values().map(|p| p.bytes_so_far).fold(0u64, |a, b| a.saturating_add(b))
}

/// Liczba pending uploadów danej instancji (org, addon).
fn pending_count_for_instance(
    map: &PendingMap,
    org_id: &str,
    addon_id: &str,
) -> usize {
    map.keys()
        .filter(|(o, a, _)| o == org_id && a == addon_id)
        .count()
}

/// Czyści osierocone pliki tymczasowe dla danego katalogu instancji (best-effort).
/// Usuwa: (1) pliki `*.partial`, których pending nie ma już w mapie (po restarcie
/// proces stracił stan pending, ale partiale mogły zostać na dysku); (2) stare
/// pliki `*.finalizing` starsze niż `PENDING_TTL_SECS` wg mtime — `.finalizing`
/// powstaje przy renamie `partial→finalizing` PRZED slow-hashem i publikacją
/// bloba, więc crash w tym oknie zostawia osierocony `.finalizing` na zawsze
/// (GC partiali go nie widzi, bo to nie `*.partial`) (Bug 2). `.finalizing`
/// aktywnej finalizacji ma świeży mtime i żyje pod mutexem instancji, więc próg
/// wieku NIE rusza pliku trwającej publikacji — tylko ten po crashu. Bezpieczne:
/// to dane tymczasowe, finalny blob ma już osobną content-addressed ścieżkę.
/// Wołane raz na (org, addon) w procesie z `put` (`purge_orphans_once`, pod
/// mutexem instancji) oraz dostępne do wołania ze startu/uninstall.
pub fn purge_orphan_partials(dir: &Path) {
    let tmp = dir.join("tmp");
    let Ok(entries) = std::fs::read_dir(&tmp) else {
        return;
    };
    let map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
    let tracked: std::collections::HashSet<PathBuf> = map.values().map(|p| p.path.clone()).collect();
    drop(map);
    let now = now_unix();
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("partial") if !tracked.contains(&path) => {
                let _ = std::fs::remove_file(&path);
            }
            Some("finalizing") if finalizing_is_stale(&path, now) => {
                let _ = std::fs::remove_file(&path);
            }
            _ => {}
        }
    }
}

/// Czy `.finalizing` jest osierocony po crashu (mtime starszy niż TTL). Aktywna
/// finalizacja trzyma mutex instancji i właśnie zrenamowała plik, więc jego mtime
/// jest świeży — próg wieku chroni trwającą publikację przed skasowaniem. Gdy
/// mtime nieczytelny, zachowujemy plik (zwracamy false, fail-safe).
fn finalizing_is_stale(path: &Path, now: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let mtime = modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(now);
    now.saturating_sub(mtime) > PENDING_TTL_SECS
}

/// Zbiór katalogów instancji już raz wyczyszczonych z osieroconych partiali w
/// TYM procesie. Pierwszy `put` instancji po starcie kasuje partiale pozostałe
/// po poprzednim procesie (crash w trakcie uploadu) — GC startowe.
fn purged_dirs() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static SET: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Idempotentne (per proces, per katalog) GC osieroconych partiali.
fn purge_orphans_once(dir: &Path) {
    {
        let set = purged_dirs().lock().unwrap_or_else(|e| e.into_inner());
        if set.contains(dir) {
            return;
        }
    }
    purge_orphan_partials(dir);
    purged_dirs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.to_path_buf());
}

// =============================================================================
// Helpery audytu / limitu
// =============================================================================

fn audit(state: &AddonState, action: &str, doc_id: Option<&str>, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("document"),
        doc_id,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

/// Waliduje `doc_id` jako bezpieczny segment ścieżki/klucz rejestru. Te same
/// reguły co `addon_id` (małe litery/cyfry/myślnik, brak `..`/`/`/NULL).
/// `doc_id` trafia teraz DO ścieżki pliku partial (`tmp/<doc_id>.partial`),
/// więc walidacja jest krytyczna dla bezpieczeństwa ścieżki.
pub fn validate_doc_id(doc_id: &str) -> Result<(), AbiError> {
    validate_addon_id(doc_id)
}

/// Łączny rozmiar dokumentów instancji (z rejestru). Używane do egzekucji
/// limitu `document_storage_mb` z `addon_resource_limits`.
pub fn current_storage_bytes(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM documents",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Limit `document_storage_mb` dla addona z `addon_resource_limits` (globalna DB
/// core, nie rejestr). `0` = brak limitu. Bierze `&DbPool` wprost, więc ten sam
/// helper obsługuje ścieżkę WASM (`AddonState.db`) i host/dispatch (`AppState.db`)
/// — jeden punkt prawdy o limicie storage dla document store.
pub fn document_storage_limit_mb(db: &crate::db::DbPool, addon_id: &str) -> i64 {
    match db.read() {
        Ok(conn) => conn
            .query_row(
                "SELECT document_storage_mb FROM addon_resource_limits WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |row| row.get(0),
            )
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Transakcyjna rezerwacja + zapis wiersza dokumentu w rejestrze instancji.
/// `BEGIN IMMEDIATE` bierze writer-lock SQLite, więc współbieżne finalizacje
/// nie przekroczą limitu liczby ani rozmiaru: liczenie count/size, sprawdzenie
/// limitów i `INSERT OR REPLACE` dzieją się w JEDNEJ transakcji. Zwraca Ok przy
/// zatwierdzeniu, QuotaExceeded przy przekroczeniu (rollback), Operation przy
/// błędzie DB (rollback). Limit netto przy nadpisaniu istniejącego `doc_id`:
/// nowy rozmiar zastępuje stary.
#[allow(clippy::too_many_arguments)]
pub fn commit_document_row(
    conn: &rusqlite::Connection,
    doc_id: &str,
    sha256: &str,
    mime: &str,
    size_bytes: u64,
    created_at: &str,
    limit_mb: i64,
) -> Result<(), AbiError> {
    conn.execute("BEGIN IMMEDIATE", []).map_err(|_| AbiError::Operation)?;

    let res = (|| -> Result<(), AbiError> {
        let prev_size: i64 = conn
            .query_row(
                "SELECT size_bytes FROM documents WHERE doc_id = ?1",
                rusqlite::params![doc_id],
                |row| row.get(0),
            )
            .unwrap_or(-1);
        let already_exists = prev_size >= 0;

        if !already_exists {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
                .map_err(|_| AbiError::Operation)?;
            if count >= MAX_DOCUMENTS_PER_INSTANCE {
                return Err(AbiError::QuotaExceeded);
            }
        }

        if limit_mb > 0 {
            let total: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(size_bytes), 0) FROM documents",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| AbiError::Operation)?;
            let prev = if already_exists { prev_size } else { 0 };
            let projected = total - prev + size_bytes as i64;
            if projected > limit_mb * 1024 * 1024 {
                return Err(AbiError::QuotaExceeded);
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO documents (doc_id, sha256, mime, size_bytes, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![doc_id, sha256, mime, size_bytes as i64, created_at],
        )
        .map_err(|_| AbiError::Operation)?;
        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute("COMMIT", []).map_err(|_| AbiError::Operation)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Egzekucja wstępnej rezerwacji quoty na PIERWSZYM kawałku (chunk 0). Znamy już
/// `total_chunks`, ale nie finalny rozmiar — szacujemy MINIMALNY projektowany
/// rozmiar jako bieżący narastający + bajty pierwszego kawałka, a górny pułap
/// liczby dokumentów sprawdzamy od razu. Twarde, tanie odrzucenie zanim zaczniemy
/// pisać wielomegabajtowy partial. Pełna egzekucja i tak jest atomowa przy
/// finalizacji (`commit_document_row`).
fn precheck_quota(
    conn: &rusqlite::Connection,
    doc_id: &str,
    first_chunk_len: u64,
    limit_mb: i64,
) -> Result<(), AbiError> {
    let already_exists: bool = conn
        .query_row(
            "SELECT 1 FROM documents WHERE doc_id = ?1",
            rusqlite::params![doc_id],
            |_| Ok(()),
        )
        .is_ok();
    if !already_exists {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .unwrap_or(0);
        if count >= MAX_DOCUMENTS_PER_INSTANCE {
            return Err(AbiError::QuotaExceeded);
        }
    }
    if limit_mb > 0 {
        let prev_size: i64 = conn
            .query_row(
                "SELECT size_bytes FROM documents WHERE doc_id = ?1",
                rusqlite::params![doc_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        // Minimalny projektowany rozmiar znany na chunk 0: pierwszy kawałek.
        let projected = current_storage_bytes(conn) - prev_size + first_chunk_len as i64;
        if projected > limit_mb * 1024 * 1024 {
            return Err(AbiError::QuotaExceeded);
        }
    }
    Ok(())
}

/// Wynik fazy akceptacji kawałka: bufor (czekaj na kolejny) lub finalizuj.
enum PutOutcome {
    Buffered,
    Finalize { mime: String, size_bytes: u64 },
}

/// Rozpoczyna nowy upload (chunk-0): limity instancji/globalne + rezerwacja
/// quoty + utworzenie świeżego partiala i wpisu pending. Wołane TYLKO gdy nie
/// ma już świeżego pending dla `acc_key` (C) — pod mutexem instancji.
#[allow(clippy::too_many_arguments)]
fn start_new_upload(
    map: &mut PendingMap,
    limit_mb: i64,
    dir: &Path,
    acc_key: &PendingKey,
    part_path: &Path,
    input: &DocumentPutInput,
    chunk: &[u8],
    chunk_len_u64: u64,
    addon_id: &str,
    doc_id: &str,
) -> Result<PutOutcome, (&'static str, AbiError)> {
    let (org_id, _, _) = acc_key;
    if pending_count_for_instance(map, org_id, addon_id) >= MAX_PENDING_UPLOADS_PER_INSTANCE {
        return Err(("pending_uploads_limit", AbiError::QuotaExceeded));
    }
    if total_pending_bytes(map).saturating_add(chunk_len_u64) > MAX_TOTAL_PENDING_BYTES {
        return Err(("total_pending_bytes_limit", AbiError::QuotaExceeded));
    }
    if chunk_len_u64 > MAX_PENDING_UPLOAD_BYTES {
        return Err(("upload_too_large", AbiError::QuotaExceeded));
    }
    // Rezerwacja quoty PRZED akceptacją: minimalny projektowany rozmiar.
    let pre = open_registry(dir).and_then(|conn| precheck_quota(&conn, doc_id, chunk_len_u64, limit_mb));
    match pre {
        Err(AbiError::QuotaExceeded) => return Err(("storage_limit_exceeded", AbiError::QuotaExceeded)),
        Err(_) => return Err(("registry_open_failed", AbiError::Operation)),
        Ok(()) => {}
    }
    // Świeży partial — truncate ewentualnego osieroconego pliku.
    let mut f = std::fs::File::create(part_path).map_err(|_| ("partial_create_failed", AbiError::Operation))?;
    if f.write_all(chunk).is_err() {
        let _ = std::fs::remove_file(part_path);
        return Err(("partial_write_failed", AbiError::Operation));
    }
    map.insert(
        acc_key.clone(),
        PendingUpload {
            path: part_path.to_path_buf(),
            mime: input.mime.clone(),
            total_chunks: input.total_chunks,
            next_index: 1,
            bytes_so_far: chunk_len_u64,
            last_activity: now_unix(),
        },
    );
    if input.total_chunks == 1 {
        Ok(PutOutcome::Finalize {
            mime: input.mime.clone(),
            size_bytes: chunk_len_u64,
        })
    } else {
        Ok(PutOutcome::Buffered)
    }
}

/// Finalizacja partiala → opublikowany content-addressed blob + wiersz rejestru.
/// Wspólna dla ABI (`document_put_v1`) i hosta (`accept_upload_chunk_host`), żeby
/// NIE duplikować publikacji bloba ani inwariantu „blob PRZED wierszem". Trzyma
/// te same gwarancje co dawniej: (1) atomowo zdejmij pending + rename partial →
/// `<doc_id>.finalizing` (GC sweepuje tylko `*.partial`), (2) strumieniowy
/// sha256, (3) rename finalizing → blob NAJPIERW, (4) DOPIERO POTEM commit
/// wiersza. Czytelnik widzący wiersz ZAWSZE ma istniejący blob. Wołać pod
/// mutexem instancji. Zwraca sha256 albo `(reason, AbiError)`.
fn finalize_partial(
    dir: &Path,
    acc_key: &PendingKey,
    part_path: &Path,
    doc_id: &str,
    mime: &str,
    size_bytes: u64,
    limit_mb: i64,
) -> Result<String, (&'static str, AbiError)> {
    let final_path = finalizing_path(dir, doc_id);
    {
        let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::rename(part_path, &final_path).is_err() {
            map.remove(acc_key);
            drop(map);
            let _ = std::fs::remove_file(part_path);
            return Err(("finalizing_rename_failed", AbiError::Operation));
        }
        map.remove(acc_key);
    }

    let sha256 = match hash_file_streaming(&final_path) {
        Ok(s) => s,
        Err(_) => {
            let _ = std::fs::remove_file(&final_path);
            return Err(("hash_failed", AbiError::Operation));
        }
    };

    let conn = match open_registry(dir) {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(&final_path);
            return Err(("registry_open_failed", AbiError::Operation));
        }
    };

    let blob = blob_path(dir, &sha256);
    if let Some(parent) = blob.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            let _ = std::fs::remove_file(&final_path);
            return Err(("blob_mkdir_failed", AbiError::Operation));
        }
    }
    if blob.exists() {
        let _ = std::fs::remove_file(&final_path);
    } else if std::fs::rename(&final_path, &blob).is_err() && !blob.exists() {
        let _ = std::fs::remove_file(&final_path);
        return Err(("blob_rename_failed", AbiError::Operation));
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    match commit_document_row(&conn, doc_id, &sha256, mime, size_bytes, &created_at, limit_mb) {
        Ok(()) => Ok(sha256),
        Err(AbiError::QuotaExceeded) => {
            cleanup_unreferenced_blob(&conn, dir, &sha256);
            Err(("storage_limit_exceeded", AbiError::QuotaExceeded))
        }
        Err(_) => {
            cleanup_unreferenced_blob(&conn, dir, &sha256);
            Err(("registry_insert_failed", AbiError::Operation))
        }
    }
}

/// Wynik akceptacji fragmentu uploadu po stronie hosta (panel UI, nie WASM).
#[derive(Debug)]
pub enum HostUploadOutcome {
    /// Fragment zbuforowany — czekamy na kolejne. `doc_id` jest stabilny dla
    /// całego uploadu (generowany na chunk-0), zwracamy go żeby klient mógł go
    /// powtarzać w polach (spójność akumulatora).
    Buffered { doc_id: String },
    /// Ostatni fragment przyjęty i sfinalizowany — `doc_ref` to id bloba w
    /// document store instancji (czytelny przez `document_get`/`ingest_document`).
    Finalized { doc_ref: String, size_bytes: u64 },
}

/// Host-side akceptacja JEDNEGO fragmentu uploadu z panelu UI addona do document
/// store instancji `addon_id`. Reużywa DOKŁADNIE tę samą warstwę co
/// `document_put_v1` (akumulator partiali na dysku, mutex instancji, finalizacja
/// blob-przed-wierszem), więc upload z panelu i odczyt addona przez
/// `document_get_v1` współdzielą jeden store i jedną serializację — zero
/// duplikacji zapisu blobów.
///
/// Izolacja: wołający (handler dispatch) MUSI przekazać `org_id` z
/// UWIERZYTELNIONEJ sesji oraz zwalidowane `addon_id` (własność instancji). Tu
/// `org_id`/`addon_id` wyznaczają fizyczny katalog store — nie ma sposobu, by
/// dosięgnąć cudzego store. `seq` jest sekwencją monotoniczną 0..total_chunks;
/// `upload_id` izoluje równoległe uploady tej samej instancji.
///
/// `upload_id` jest mieszany do `doc_id` (deterministyczny, walidowany), żeby
/// dwa równoległe uploady tej samej instancji miały rozłączne partiale i wiersze.
#[allow(clippy::too_many_arguments)]
pub fn accept_upload_chunk_host(
    org_id: &str,
    addon_id: &str,
    upload_id: &str,
    mime: &str,
    seq: u32,
    total_chunks: u32,
    chunk: &[u8],
    limit_mb: i64,
) -> Result<HostUploadOutcome, (&'static str, AbiError)> {
    if total_chunks == 0 || total_chunks > MAX_TOTAL_CHUNKS {
        return Err(("invalid_total_chunks", AbiError::Operation));
    }
    if seq >= total_chunks {
        return Err(("chunk_index_out_of_range", AbiError::Operation));
    }
    if mime.len() > MAX_MIME_LEN {
        return Err(("mime_too_long", AbiError::Operation));
    }
    if chunk.len() > MAX_PUT_CHUNK_BYTES {
        return Err(("chunk_too_large", AbiError::PayloadTooLarge));
    }

    // doc_id deterministyczny z upload_id — stabilny przez cały upload i unikalny
    // per równoległy upload tej samej instancji. Walidacja jako segment ścieżki.
    let doc_id = format!("up-{}", sanitize_upload_id(upload_id));
    if validate_doc_id(&doc_id).is_err() {
        return Err(("invalid_upload_id", AbiError::Operation));
    }

    let dir = documents_dir(org_id, addon_id).map_err(|_| ("documents_dir_failed", AbiError::Operation))?;
    tmp_dir(&dir).map_err(|_| ("tmp_dir_failed", AbiError::Operation))?;

    let inst_lock = instance_lock(org_id, addon_id);
    let _inst_guard = inst_lock.lock().unwrap_or_else(|e| e.into_inner());

    sweep_abandoned_pending();
    purge_orphans_once(&dir);

    let acc_key = (org_id.to_string(), addon_id.to_string(), doc_id.clone());
    let part_path = partial_path(&dir, &doc_id);
    let chunk_len_u64 = chunk.len() as u64;

    let input = DocumentPutInput {
        doc_id: doc_id.clone(),
        mime: mime.to_string(),
        chunk_index: seq,
        total_chunks,
    };

    let outcome: Result<PutOutcome, (&'static str, AbiError)> = {
        let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
        if seq == 0 {
            let existing_fresh = matches!(
                map.get(&acc_key),
                Some(p) if now_unix().saturating_sub(p.last_activity) <= PENDING_TTL_SECS
            );
            if existing_fresh {
                Err(("upload_already_in_progress", AbiError::Operation))
            } else {
                if let Some(stale) = map.remove(&acc_key) {
                    let _ = std::fs::remove_file(&stale.path);
                }
                start_new_upload(
                    &mut map, limit_mb, &dir, &acc_key, &part_path,
                    &input, chunk, chunk_len_u64, addon_id, &doc_id,
                )
            }
        } else {
            let total_pending = total_pending_bytes(&map);
            let matches_seq = matches!(
                map.get(&acc_key),
                Some(p) if p.next_index == seq && p.total_chunks == total_chunks
            );
            if !matches_seq {
                if let Some(p) = map.remove(&acc_key) {
                    let _ = std::fs::remove_file(&p.path);
                }
                Err(("chunk_sequence_mismatch", AbiError::Operation))
            } else {
                let p = map.get_mut(&acc_key).expect("sprawdzone wyżej");
                let projected = p.bytes_so_far.saturating_add(chunk_len_u64);
                if projected > MAX_PENDING_UPLOAD_BYTES {
                    let path = p.path.clone();
                    map.remove(&acc_key);
                    let _ = std::fs::remove_file(&path);
                    Err(("upload_too_large", AbiError::QuotaExceeded))
                } else if total_pending.saturating_add(chunk_len_u64) > MAX_TOTAL_PENDING_BYTES {
                    let path = p.path.clone();
                    map.remove(&acc_key);
                    let _ = std::fs::remove_file(&path);
                    Err(("total_pending_bytes_limit", AbiError::QuotaExceeded))
                } else {
                    let append = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&p.path)
                        .and_then(|mut f| f.write_all(chunk));
                    match append {
                        Ok(()) => {
                            p.next_index += 1;
                            p.bytes_so_far = projected;
                            p.last_activity = now_unix();
                            if seq + 1 == total_chunks {
                                Ok(PutOutcome::Finalize { mime: p.mime.clone(), size_bytes: p.bytes_so_far })
                            } else {
                                Ok(PutOutcome::Buffered)
                            }
                        }
                        Err(_) => {
                            let path = p.path.clone();
                            map.remove(&acc_key);
                            let _ = std::fs::remove_file(&path);
                            Err(("partial_append_failed", AbiError::Operation))
                        }
                    }
                }
            }
        }
    };

    match outcome? {
        PutOutcome::Buffered => Ok(HostUploadOutcome::Buffered { doc_id }),
        PutOutcome::Finalize { mime, size_bytes } => {
            let sha = finalize_partial(&dir, &acc_key, &part_path, &doc_id, &mime, size_bytes, limit_mb)?;
            let _ = sha;
            Ok(HostUploadOutcome::Finalized { doc_ref: doc_id, size_bytes })
        }
    }
}

/// Mapuje surowy `upload_id` (klient, dowolny string) na bezpieczny segment
/// (małe litery/cyfry/myślnik), bez `..`/`/`. Walidacja końcowa i tak w
/// `validate_doc_id` — to redukcja typowego UUID/`up-...` do dozwolonego alfabetu.
fn sanitize_upload_id(upload_id: &str) -> String {
    upload_id
        .chars()
        .map(|c| {
            let lc = c.to_ascii_lowercase();
            if lc.is_ascii_alphanumeric() || lc == '-' {
                lc
            } else {
                '-'
            }
        })
        .take(96)
        .collect()
}

// =============================================================================
// Host function: document_put_v1
// =============================================================================

/// ABI: (input_ptr, input_len, chunk_ptr, chunk_len, out_ptr, out_cap,
///       out_len_ptr) -> i32
///
/// Streaming upload: metadane kawałka w CBOR (`DocumentPutInput`), surowe bajty
/// osobnym ptr/len. Każdy kawałek jest DOPISYWANY do pliku partial na dysku
/// (NIE akumulowany w pamięci). Ostatni kawałek finalizuje: strumieniowy sha256
/// + atomic rename do bloba + transakcyjny zapis rejestru.
#[allow(clippy::too_many_arguments)]
pub fn document_put_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    chunk_ptr: i32,
    chunk_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "document.put", None, "error", Some("memory_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_DOCUMENT_WRITE, None) {
        audit(caller.data(), "document.put", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let input: DocumentPutInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::DocumentMeta,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "document.put",
                None,
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

    if input.total_chunks == 0 || input.total_chunks > MAX_TOTAL_CHUNKS {
        audit(caller.data(), "document.put", None, "denied", Some("invalid_total_chunks"));
        return AbiError::Operation.as_i32();
    }
    if input.chunk_index >= input.total_chunks {
        audit(caller.data(), "document.put", None, "denied", Some("chunk_index_out_of_range"));
        return AbiError::Operation.as_i32();
    }
    if input.mime.len() > MAX_MIME_LEN {
        audit(caller.data(), "document.put", None, "denied", Some("mime_too_long"));
        return AbiError::Operation.as_i32();
    }

    let chunk = match read_guest_bytes(&memory, &caller, chunk_ptr, chunk_len) {
        Some(b) if b.len() <= MAX_PUT_CHUNK_BYTES => b.to_vec(),
        Some(_) => {
            audit(caller.data(), "document.put", None, "denied", Some("chunk_too_large"));
            return AbiError::PayloadTooLarge.as_i32();
        }
        None => {
            audit(caller.data(), "document.put", None, "denied", Some("invalid_chunk_ptr"));
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());

    // doc_id: pusty na pierwszym kawałku → generujemy; inaczej walidujemy podany.
    let doc_id = if input.chunk_index == 0 && input.doc_id.is_empty() {
        format!("doc-{}", uuid::Uuid::new_v4().simple())
    } else {
        if validate_doc_id(&input.doc_id).is_err() {
            audit(caller.data(), "document.put", Some(&input.doc_id), "denied", Some("invalid_doc_id"));
            return AbiError::Operation.as_i32();
        }
        input.doc_id.clone()
    };

    let dir = match documents_dir(&org_id, &addon_id) {
        Ok(d) => d,
        Err(_) => {
            audit(caller.data(), "document.put", Some(&doc_id), "error", Some("documents_dir_failed"));
            return AbiError::Operation.as_i32();
        }
    };
    if tmp_dir(&dir).is_err() {
        audit(caller.data(), "document.put", Some(&doc_id), "error", Some("tmp_dir_failed"));
        return AbiError::Operation.as_i32();
    }
    // Serializacja per-instancja: cały put (sweep GC, akceptacja chunku,
    // finalizacja) trzyma mutex instancji, więc GC/finalize/delete/chunk-0 nie
    // wejdą równolegle — strukturalne kasowanie wyścigów (A/C/D).
    let inst_lock = instance_lock(&org_id, &addon_id);
    let _inst_guard = inst_lock.lock().unwrap_or_else(|e| e.into_inner());

    // GC porzuconych partiali pod mutexem instancji — sweep tani (tylko metadane).
    sweep_abandoned_pending();
    // GC startowe: kasuj partiale osierocone przez poprzedni proces (raz/dir).
    purge_orphans_once(&dir);

    let acc_key = (org_id.clone(), addon_id.clone(), doc_id.clone());
    let part_path = partial_path(&dir, &doc_id);
    let chunk_len_u64 = chunk.len() as u64;
    let limit_mb = document_storage_limit_mb(&caller.data().db, &addon_id);

    // Faza dopisania kawałka do partiala (pod lockiem pending: spójność stanu).
    let outcome: Result<PutOutcome, (&'static str, AbiError)> = {
        let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());

        if input.chunk_index == 0 {
            // Odrzuć równoległy chunk-0 dla istniejącego pending (C): drugie
            // równoległe rozpoczęcie tego samego doc_id NIE truncatuje partiala
            // pierwszego. Przeterminowany pending najpierw GC, potem nowy.
            let existing_fresh = matches!(
                map.get(&acc_key),
                Some(p) if now_unix().saturating_sub(p.last_activity) <= PENDING_TTL_SECS
            );
            if existing_fresh {
                Err(("upload_already_in_progress", AbiError::Operation))
            } else if let Some(stale) = map.remove(&acc_key) {
                // Pending przeterminowany — sprzątnij stary partial przed nowym.
                let _ = std::fs::remove_file(&stale.path);
                start_new_upload(
                    &mut map,
                    limit_mb,
                    &dir,
                    &acc_key,
                    &part_path,
                    &input,
                    &chunk,
                    chunk_len_u64,
                    &addon_id,
                    &doc_id,
                )
            } else {
                start_new_upload(
                    &mut map,
                    limit_mb,
                    &dir,
                    &acc_key,
                    &part_path,
                    &input,
                    &chunk,
                    chunk_len_u64,
                    &addon_id,
                    &doc_id,
                )
            }
        } else {
            // Kolejny kawałek: sekwencja MUSI być monotoniczna i total spójny.
            // Suma globalna liczona PRZED mutowalnym borrowem (one writer per put).
            let total_pending = total_pending_bytes(&map);
            let matches_seq = matches!(
                map.get(&acc_key),
                Some(p) if p.next_index == input.chunk_index && p.total_chunks == input.total_chunks
            );
            if !matches_seq {
                if let Some(p) = map.remove(&acc_key) {
                    let _ = std::fs::remove_file(&p.path);
                }
                Err(("chunk_sequence_mismatch", AbiError::Operation))
            } else {
                let p = map.get_mut(&acc_key).expect("sprawdzone wyżej");
                let projected = p.bytes_so_far.saturating_add(chunk_len_u64);
                if projected > MAX_PENDING_UPLOAD_BYTES {
                    let path = p.path.clone();
                    map.remove(&acc_key);
                    let _ = std::fs::remove_file(&path);
                    Err(("upload_too_large", AbiError::QuotaExceeded))
                } else if total_pending.saturating_add(chunk_len_u64) > MAX_TOTAL_PENDING_BYTES {
                    let path = p.path.clone();
                    map.remove(&acc_key);
                    let _ = std::fs::remove_file(&path);
                    Err(("total_pending_bytes_limit", AbiError::QuotaExceeded))
                } else {
                    // Dopisanie do partiala (append).
                    let append = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&p.path)
                        .and_then(|mut f| f.write_all(&chunk));
                    match append {
                        Ok(()) => {
                            p.next_index += 1;
                            p.bytes_so_far = projected;
                            p.last_activity = now_unix();
                            if input.chunk_index + 1 == input.total_chunks {
                                Ok(PutOutcome::Finalize {
                                    mime: p.mime.clone(),
                                    size_bytes: p.bytes_so_far,
                                })
                            } else {
                                Ok(PutOutcome::Buffered)
                            }
                        }
                        Err(_) => {
                            let path = p.path.clone();
                            map.remove(&acc_key);
                            let _ = std::fs::remove_file(&path);
                            Err(("partial_append_failed", AbiError::Operation))
                        }
                    }
                }
            }
        }
    };

    let (mime, size_bytes) = match outcome {
        Ok(PutOutcome::Buffered) => {
            let out = DocumentPutOutput {
                doc_id: doc_id.clone(),
                finalized: false,
                size_bytes: 0,
                sha256: String::new(),
            };
            // „ok" DOPIERO po udanym zapisie wyjścia (F): OutputBufferTooSmall ≠ ok.
            let code = write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::DocumentMeta,
            );
            if code == AbiError::Ok.as_i32() {
                audit(caller.data(), "document.put", Some(&doc_id), "ok", Some("chunk_buffered"));
            } else if code == AbiError::OutputBufferTooSmall.as_i32() {
                audit(caller.data(), "document.put", Some(&doc_id), "error", Some("output_buffer_too_small"));
            } else {
                audit(caller.data(), "document.put", Some(&doc_id), "error", Some("output_write_failed"));
            }
            return code;
        }
        Ok(PutOutcome::Finalize { mime, size_bytes }) => (mime, size_bytes),
        Err((reason, err)) => {
            let result = if err == AbiError::QuotaExceeded { "denied" } else { "error" };
            audit(caller.data(), "document.put", Some(&doc_id), result, Some(reason));
            return err.as_i32();
        }
    };

    // -------------------------------------------------------------------------
    // Finalizacja (pod mutexem instancji): publikacja blob-PRZED-wierszem.
    // (1) atomowo: usuń pending z mapy + rename partial → `<doc_id>.finalizing`,
    //     żeby GC (sweepuje tylko `*.partial`) nie widział slow-hashu jako „stary
    //     partial" (D). (2) strumieniowy sha256 z finalizing. (3) rename
    //     finalizing → content-addressed blob NAJPIERW (content opublikowany).
    //     (4) DOPIERO POTEM commit_document_row. Skutek: czytelnik widzący wiersz
    //     ZAWSZE ma istniejący blob (B); overwrite nie zostawia okna wiszącego.
    // -------------------------------------------------------------------------
    let sha256 = match finalize_partial(&dir, &acc_key, &part_path, &doc_id, &mime, size_bytes, limit_mb) {
        Ok(s) => s,
        Err((reason, err)) => {
            let result = if err == AbiError::QuotaExceeded { "denied" } else { "error" };
            audit(caller.data(), "document.put", Some(&doc_id), result, Some(reason));
            return err.as_i32();
        }
    };

    let out = DocumentPutOutput {
        doc_id: doc_id.clone(),
        finalized: true,
        size_bytes,
        sha256,
    };
    let code = write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::DocumentMeta,
    );
    if code == AbiError::Ok.as_i32() {
        audit(caller.data(), "document.put", Some(&doc_id), "ok", None);
    } else if code == AbiError::OutputBufferTooSmall.as_i32() {
        audit(caller.data(), "document.put", Some(&doc_id), "error", Some("output_buffer_too_small"));
    } else {
        audit(caller.data(), "document.put", Some(&doc_id), "error", Some("output_write_failed"));
    }
    code
}

/// Kasuje opublikowany blob TYLKO gdy żaden wiersz rejestru nie referuje tego
/// sha. Wołane gdy commit wiersza padł po publikacji bloba — content-addressed
/// blob bez referencji jest osierocony i bezpieczny do usunięcia, ale jeśli inny
/// `doc_id` ma to samo sha (dedup), NIE wolno go ruszać. Best-effort.
fn cleanup_unreferenced_blob(conn: &rusqlite::Connection, dir: &Path, sha256: &str) {
    let referenced: bool = conn
        .query_row(
            "SELECT 1 FROM documents WHERE sha256 = ?1 LIMIT 1",
            rusqlite::params![sha256],
            |_| Ok(()),
        )
        .is_ok();
    if !referenced {
        let _ = std::fs::remove_file(blob_path(dir, sha256));
    }
}

/// Strumieniowy sha256 pliku — czyta blokami `HASH_BLOCK_BYTES`, nie ładuje
/// całego pliku do pamięci. Zwraca hex sha256.
pub fn hash_file_streaming(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_BLOCK_BYTES];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// =============================================================================
// Host function: document_get_v1
// =============================================================================

/// ABI: (input_ptr, input_len, blob_out_ptr, blob_out_cap, meta_out_ptr,
///       meta_out_cap, meta_out_len_ptr) -> i32
///
/// Czyta kawałek `chunk_index` dokumentu `doc_id` przez SEEK do offsetu kawałka
/// (NIE ładuje całego pliku ani nie re-hashuje per chunk). Bajty kawałka idą do
/// `blob_out_ptr`, metadane (`DocumentGetMeta`) do `meta_out_ptr`. Gdy
/// `blob_out_cap` za mały — `OutputBufferTooSmall` i NIE zapisuje metadanych.
#[allow(clippy::too_many_arguments)]
pub fn document_get_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    blob_out_ptr: i32,
    blob_out_cap: i32,
    meta_out_ptr: i32,
    meta_out_cap: i32,
    meta_out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "document.get", None, "error", Some("memory_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_DOCUMENT_READ, None) {
        audit(caller.data(), "document.get", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let input: DocumentGetInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::DocumentMeta,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(caller.data(), "document.get", None, "denied", Some("invalid_payload"));
            return e.as_i32();
        }
    };

    if validate_doc_id(&input.doc_id).is_err() {
        audit(caller.data(), "document.get", Some(&input.doc_id), "denied", Some("invalid_doc_id"));
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());

    let dir = match documents_dir(&org_id, &addon_id) {
        Ok(d) => d,
        Err(_) => {
            audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("documents_dir_failed"));
            return AbiError::Operation.as_i32();
        }
    };
    let conn = match open_registry(&dir) {
        Ok(c) => c,
        Err(_) => {
            audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("registry_open_failed"));
            return AbiError::Operation.as_i32();
        }
    };

    // Serializacja per-instancja: get trzyma mutex instancji przez SELECT wiersza
    // ORAZ open+read bloba. Bez tego równoległy delete (DELETE wiersza + unlink
    // bloba) mógłby wbić się między nasz SELECT a open pliku → otwieramy już
    // skasowany blob (Bug 1). Pod mutexem delete i get tej samej instancji są
    // wzajemnie wykluczone: get widzący wiersz ZAWSZE czyta istniejący blob, a
    // get po skasowaniu wiersza dostaje NotFound. Operacje na dokumentach rzadkie,
    // więc serializacja get z put/delete jest tania.
    let inst_lock = instance_lock(&org_id, &addon_id);
    let _inst_guard = inst_lock.lock().unwrap_or_else(|e| e.into_inner());

    // Ownership: SELECT scoped do rejestru tej instancji. Obcy doc_id → brak
    // wiersza → NotFound.
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT sha256, mime, size_bytes FROM documents WHERE doc_id = ?1",
            rusqlite::params![&input.doc_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (sha256, mime, size_bytes) = match row {
        Some(t) => t,
        None => {
            audit(caller.data(), "document.get", Some(&input.doc_id), "denied", Some("not_found"));
            return AbiError::NotFound.as_i32();
        }
    };

    let total_chunks = ((size_bytes as usize).div_ceil(DOCUMENT_CHUNK_BYTES)).max(1) as u32;
    if input.chunk_index >= total_chunks {
        audit(caller.data(), "document.get", Some(&input.doc_id), "denied", Some("chunk_index_out_of_range"));
        return AbiError::Operation.as_i32();
    }

    // Seek do offsetu kawałka, czytaj TYLKO chunk_len bajtów. sha zweryfikowane
    // przy zapisie — przy odczycie ufamy (zero re-hash per chunk).
    let path = blob_path(&dir, &sha256);
    let start = input.chunk_index as u64 * DOCUMENT_CHUNK_BYTES as u64;
    let want = ((size_bytes as u64).saturating_sub(start)).min(DOCUMENT_CHUNK_BYTES as u64) as usize;
    let mut chunk = vec![0u8; want];
    let read_res = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut chunk)?;
        Ok(())
    })();
    if read_res.is_err() {
        audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("blob_read_failed"));
        return AbiError::Operation.as_i32();
    }

    // Najpierw zapisz bajty kawałka. Gdy bufor za mały — zwróć required size i
    // NIE pisz metadanych (addon realokuje i ponawia).
    let blob_code = write_output_with_retry_semantics(
        &memory,
        &mut caller,
        &chunk,
        blob_out_ptr,
        blob_out_cap,
        meta_out_len_ptr,
    );
    if blob_code != AbiError::Ok.as_i32() {
        if blob_code == AbiError::OutputBufferTooSmall.as_i32() {
            audit(caller.data(), "document.get", Some(&input.doc_id), "denied", Some("blob_buffer_too_small"));
        } else {
            audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("blob_write_failed"));
        }
        return blob_code;
    }

    let meta = DocumentGetMeta {
        total_chunks,
        chunk_len: chunk.len() as u32,
        mime,
        size_bytes: size_bytes as u64,
    };
    let meta_code = write_cbor_capped(
        &memory,
        &mut caller,
        &meta,
        meta_out_ptr,
        meta_out_cap,
        meta_out_len_ptr,
        PayloadKind::DocumentMeta,
    );
    if meta_code == AbiError::Ok.as_i32() {
        audit(caller.data(), "document.get", Some(&input.doc_id), "ok", None);
    } else if meta_code == AbiError::OutputBufferTooSmall.as_i32() {
        audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("meta_buffer_too_small"));
    } else {
        audit(caller.data(), "document.get", Some(&input.doc_id), "error", Some("meta_write_failed"));
    }
    meta_code
}

// =============================================================================
// Host function: document_delete_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Kasuje plik + wpis rejestru dla `doc_id`. Tylko własne dokumenty (rejestr
/// scoped do instancji). Idempotentny — nieistniejący `doc_id` → removed=false.
/// Kolejność: najpierw plik (gdy nie jest już referowany przez inny doc_id), a
/// dopiero potem wiersz rejestru — błąd kasowania pliku przerywa operację
/// (zwraca błąd) zamiast po cichu zostawić drift plik↔rejestr.
pub fn document_delete_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "document.delete", None, "error", Some("memory_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_DOCUMENT_WRITE, None) {
        audit(caller.data(), "document.delete", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let input: DocumentDeleteInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::DocumentMeta,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(caller.data(), "document.delete", None, "denied", Some("invalid_payload"));
            return e.as_i32();
        }
    };

    if validate_doc_id(&input.doc_id).is_err() {
        audit(caller.data(), "document.delete", Some(&input.doc_id), "denied", Some("invalid_doc_id"));
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());

    let dir = match documents_dir(&org_id, &addon_id) {
        Ok(d) => d,
        Err(_) => {
            audit(caller.data(), "document.delete", Some(&input.doc_id), "error", Some("documents_dir_failed"));
            return AbiError::Operation.as_i32();
        }
    };
    let conn = match open_registry(&dir) {
        Ok(c) => c,
        Err(_) => {
            audit(caller.data(), "document.delete", Some(&input.doc_id), "error", Some("registry_open_failed"));
            return AbiError::Operation.as_i32();
        }
    };

    // Serializacja per-instancja: cała delete trzyma mutex instancji, więc nie
    // przeplata się z finalizacją put ani GC (A/E). Bez tego ref-check + DELETE +
    // unlink miałyby okno wyścigu.
    let inst_lock = instance_lock(&org_id, &addon_id);
    let _inst_guard = inst_lock.lock().unwrap_or_else(|e| e.into_inner());

    let sha256: Option<String> = conn
        .query_row(
            "SELECT sha256 FROM documents WHERE doc_id = ?1",
            rusqlite::params![&input.doc_id],
            |r| r.get(0),
        )
        .ok();
    let sha256 = match sha256 {
        Some(s) => s,
        None => {
            let out = DocumentDeleteOutput {
                doc_id: input.doc_id.clone(),
                removed: false,
            };
            let code = write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::DocumentMeta,
            );
            if code == AbiError::Ok.as_i32() {
                audit(caller.data(), "document.delete", Some(&input.doc_id), "ok", Some("not_found"));
            } else {
                audit(caller.data(), "document.delete", Some(&input.doc_id), "error", Some("output_write_failed"));
            }
            return code;
        }
    };

    // Transakcyjnie (pod mutexem instancji): ref-check + DELETE wiersza, a DOPIERO
    // POTEM unlink bloba. Kolejność „wiersz przed plikiem" gwarantuje, że czytelnik
    // bez wiersza dostaje NotFound (nie dangling) — nigdy nie ma okna, w którym
    // wiersz wskazuje na już skasowany plik (E).
    let other_ref: bool = conn
        .query_row(
            "SELECT 1 FROM documents WHERE sha256 = ?1 AND doc_id != ?2 LIMIT 1",
            rusqlite::params![&sha256, &input.doc_id],
            |_| Ok(()),
        )
        .is_ok();

    if conn
        .execute(
            "DELETE FROM documents WHERE doc_id = ?1",
            rusqlite::params![&input.doc_id],
        )
        .is_err()
    {
        audit(caller.data(), "document.delete", Some(&input.doc_id), "error", Some("registry_delete_failed"));
        return AbiError::Operation.as_i32();
    }

    // Wiersz usunięty — teraz unlink bloba, ale tylko gdy NIKT inny nie referuje
    // tego sha (dedup wewnątrz instancji). Błąd unlinku osieroca plik (GC sprzątnie),
    // ale rejestr jest już spójny — nie przerywamy operacji.
    if !other_ref {
        let _ = std::fs::remove_file(blob_path(&dir, &sha256));
    }

    let out = DocumentDeleteOutput {
        doc_id: input.doc_id.clone(),
        removed: true,
    };
    let code = write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::DocumentMeta,
    );
    if code == AbiError::Ok.as_i32() {
        audit(caller.data(), "document.delete", Some(&input.doc_id), "ok", None);
    } else {
        audit(caller.data(), "document.delete", Some(&input.doc_id), "error", Some("output_write_failed"));
    }
    code
}

// =============================================================================
// Host function: document_list_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Listuje dokumenty instancji (doc_id + metadane). Scoped do (org, instancja).
pub fn document_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "document.list", None, "error", Some("memory_unavailable"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_DOCUMENT_READ, None) {
        audit(caller.data(), "document.list", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let _input: DocumentListInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::DocumentMeta,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(caller.data(), "document.list", None, "denied", Some("invalid_payload"));
            return e.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());

    let dir = match documents_dir(&org_id, &addon_id) {
        Ok(d) => d,
        Err(_) => {
            audit(caller.data(), "document.list", None, "error", Some("documents_dir_failed"));
            return AbiError::Operation.as_i32();
        }
    };
    let conn = match open_registry(&dir) {
        Ok(c) => c,
        Err(_) => {
            audit(caller.data(), "document.list", None, "error", Some("registry_open_failed"));
            return AbiError::Operation.as_i32();
        }
    };

    let documents: Vec<DocumentMeta> = {
        let mut stmt = match conn.prepare(
            "SELECT doc_id, mime, size_bytes, sha256, created_at FROM documents ORDER BY created_at",
        ) {
            Ok(s) => s,
            Err(_) => {
                audit(caller.data(), "document.list", None, "error", Some("registry_query_failed"));
                return AbiError::Operation.as_i32();
            }
        };
        let rows = stmt.query_map([], |r| {
            Ok(DocumentMeta {
                doc_id: r.get(0)?,
                mime: r.get(1)?,
                size_bytes: r.get::<_, i64>(2)? as u64,
                sha256: r.get(3)?,
                created_at: r.get(4)?,
            })
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => {
                audit(caller.data(), "document.list", None, "error", Some("registry_query_failed"));
                return AbiError::Operation.as_i32();
            }
        }
    };

    let out = DocumentListOutput { documents };
    let code = write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::DocumentMeta,
    );
    if code == AbiError::Ok.as_i32() {
        audit(caller.data(), "document.list", None, "ok", None);
    } else if code == AbiError::OutputBufferTooSmall.as_i32() {
        audit(caller.data(), "document.list", None, "error", Some("output_buffer_too_small"));
    } else {
        audit(caller.data(), "document.list", None, "error", Some("output_write_failed"));
    }
    code
}

// =============================================================================
// Testy — operują na warstwie store BEZ wasmtime (jak vector test_api).
// Ćwiczą realny zapis/odczyt/izolację per instancja na tempdir (root override).
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    pub use super::{
        accept_upload_chunk_host, blob_path, commit_document_row, current_storage_bytes,
        document_storage_limit_mb, documents_dir, forget_instance_lock, hash_file_streaming,
        open_registry, partial_path, purge_orphan_partials, set_root_override,
        sweep_abandoned_pending, tmp_dir, validate_doc_id, HostUploadOutcome, DOCUMENT_CHUNK_BYTES,
        MAX_PENDING_UPLOADS_PER_INSTANCE, MAX_PENDING_UPLOAD_BYTES, PENDING_TTL_SECS,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Override roota jest globalny — testy modyfikujące go MUSZĄ być
    /// serializowane, inaczej współbieżne testy widzą cudzy tempdir.
    fn override_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// Symuluje streamingowy `put`: dopisuje kawałki do partiala, finalizuje
    /// strumieniowym hashem + transakcyjnym insertem (logika `document_put_v1`
    /// finalizacji, bez wasmtime). Zwraca (doc_id, sha256). `chunk_size` steruje
    /// liczbą kawałków. Sprawdza istnienie partiala w trakcie.
    fn stream_put(
        dir: &std::path::Path,
        doc_id: &str,
        mime: &str,
        data: &[u8],
        chunk_size: usize,
        limit_mb: i64,
        assert_partial_midway: bool,
    ) -> (String, String) {
        tmp_dir(dir).unwrap();
        let part = partial_path(dir, doc_id);
        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(chunk_size).collect()
        };
        let total = chunks.len();
        for (i, ch) in chunks.iter().enumerate() {
            if i == 0 {
                let mut f = std::fs::File::create(&part).unwrap();
                f.write_all(ch).unwrap();
            } else {
                let mut f = std::fs::OpenOptions::new().append(true).open(&part).unwrap();
                f.write_all(ch).unwrap();
            }
            if assert_partial_midway && i + 1 < total {
                assert!(part.exists(), "partial musi istnieć w trakcie streamingu");
            }
        }
        // Finalizacja — lustro produkcyjnej kolejności: blob PRZED wierszem.
        let sha = finalize_publish(dir, doc_id, mime, data.len() as u64, limit_mb).unwrap();
        (doc_id.to_string(), sha)
    }

    /// Lustro produkcyjnej finalizacji (`document_put_v1`): rename partial →
    /// finalizing → blob NAJPIERW, DOPIERO POTEM commit wiersza. Zwraca sha.
    fn finalize_publish(
        dir: &std::path::Path,
        doc_id: &str,
        mime: &str,
        size_bytes: u64,
        limit_mb: i64,
    ) -> Result<String, AbiError> {
        let part = partial_path(dir, doc_id);
        let final_path = finalizing_path(dir, doc_id);
        std::fs::rename(&part, &final_path).map_err(|_| AbiError::Operation)?;
        let sha = hash_file_streaming(&final_path).map_err(|_| AbiError::Operation)?;
        let blob = blob_path(dir, &sha);
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        if blob.exists() {
            std::fs::remove_file(&final_path).unwrap();
        } else {
            std::fs::rename(&final_path, &blob).map_err(|_| AbiError::Operation)?;
        }
        let conn = open_registry(dir).unwrap();
        let created = "2026-06-21T00:00:00Z";
        match commit_document_row(&conn, doc_id, &sha, mime, size_bytes, created, limit_mb) {
            Ok(()) => Ok(sha),
            Err(e) => {
                cleanup_unreferenced_blob(&conn, dir, &sha);
                Err(e)
            }
        }
    }

    /// Odczyt kawałka przez seek (lustro `document_get_v1`) — NIE ładuje całości.
    fn get_chunk_seek(dir: &std::path::Path, doc_id: &str, chunk_index: usize) -> Option<(Vec<u8>, u32)> {
        let conn = open_registry(dir).unwrap();
        let (sha, size): (String, i64) = conn
            .query_row(
                "SELECT sha256, size_bytes FROM documents WHERE doc_id = ?1",
                rusqlite::params![doc_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        let total = ((size as usize).div_ceil(DOCUMENT_CHUNK_BYTES)).max(1) as u32;
        let start = chunk_index as u64 * DOCUMENT_CHUNK_BYTES as u64;
        let want = ((size as u64).saturating_sub(start)).min(DOCUMENT_CHUNK_BYTES as u64) as usize;
        let mut buf = vec![0u8; want];
        let mut f = std::fs::File::open(blob_path(dir, &sha)).ok()?;
        f.seek(SeekFrom::Start(start)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some((buf, total))
    }

    #[test]
    fn streaming_put_large_then_get_by_seek_roundtrip() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();

        // Plik > 1MB (5 × 256KiB + ogon) — niemożliwy w KV. Streaming po 256 KiB.
        let big: Vec<u8> = (0..(1_300_000u32)).map(|i| (i % 251) as u8).collect();
        let (doc_id, sha) =
            stream_put(&dir, "doc-big", "application/pdf", &big, DOCUMENT_CHUNK_BYTES, 0, true);

        // Po finalizacji partial nie istnieje, blob istnieje.
        assert!(!partial_path(&dir, "doc-big").exists(), "partial skasowany po finalizacji");
        assert!(blob_path(&dir, &sha).exists(), "blob istnieje po finalizacji");

        // Odczyt kawałkami przez seek i ponowne złożenie.
        let mut assembled = Vec::new();
        let (_first, total) = get_chunk_seek(&dir, &doc_id, 0).unwrap();
        assert_eq!(total, 5, "1.3MB / 256KiB → 5 kawałków, mam {total}");
        assert!(big.len() > 1_048_576, "plik > limitu KV 1 MB");
        for i in 0..total as usize {
            let (chunk, _t) = get_chunk_seek(&dir, &doc_id, i).unwrap();
            assembled.extend_from_slice(&chunk);
        }
        assert_eq!(assembled, big, "złożone bajty identyczne z oryginałem");
        set_root_override(None);
    }

    #[test]
    fn gc_sweeps_abandoned_partial() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();

        // Ręcznie utwórz partial + wpis pending ze starym last_activity.
        let part = partial_path(&dir, "doc-stale");
        std::fs::write(&part, b"porzucone bajty").unwrap();
        {
            let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                ("org-default".into(), "alpha".into(), "doc-stale".into()),
                PendingUpload {
                    path: part.clone(),
                    mime: "application/pdf".into(),
                    total_chunks: 3,
                    next_index: 1,
                    bytes_so_far: 15,
                    last_activity: now_unix().saturating_sub(PENDING_TTL_SECS + 10),
                },
            );
        }
        assert!(part.exists());

        let swept = sweep_abandoned_pending();
        assert!(swept >= 1, "GC skasował co najmniej 1 porzucony partial");
        assert!(!part.exists(), "porzucony partial fizycznie skasowany");
        let still = pending_uploads()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&("org-default".into(), "alpha".into(), "doc-stale".into()));
        assert!(!still, "wpis pending usunięty");
        set_root_override(None);
    }

    #[test]
    fn pending_upload_count_limit_is_enforced() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        // Wstaw MAX pending dla instancji i sprawdź, że N+1 byłby odrzucony.
        {
            let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
            map.clear();
            for i in 0..MAX_PENDING_UPLOADS_PER_INSTANCE {
                map.insert(
                    ("org-default".into(), "alpha".into(), format!("doc-{i}")),
                    PendingUpload {
                        path: PathBuf::from(format!("/tmp/none-{i}")),
                        mime: "x".into(),
                        total_chunks: 2,
                        next_index: 1,
                        bytes_so_far: 1,
                        last_activity: now_unix(),
                    },
                );
            }
            assert_eq!(
                pending_count_for_instance(&map, "org-default", "alpha"),
                MAX_PENDING_UPLOADS_PER_INSTANCE,
                "instancja na pełnym limicie pending"
            );
            // N+1 przekracza limit → ścieżka put zwróciłaby QuotaExceeded.
            assert!(
                pending_count_for_instance(&map, "org-default", "alpha")
                    >= MAX_PENDING_UPLOADS_PER_INSTANCE
            );
            map.clear();
        }
        set_root_override(None);
    }

    #[test]
    fn concurrent_finalize_does_not_exceed_quota() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();

        // Limit 1 MB. Dwa pliki po 600 KB — transakcyjna rezerwacja musi wpuścić
        // dokładnie jeden i odrzucić drugi (suma 1.2 MB > 1 MB).
        let limit_mb = 1i64;
        let data = vec![7u8; 600_000];

        let conn = open_registry(&dir).unwrap();
        // Pierwszy: 600 KB < 1 MB → OK.
        commit_document_row(&conn, "doc-1", "sha1aaaa", "application/pdf", data.len() as u64, "t", limit_mb)
            .expect("pierwszy 600KB mieści się w 1MB");
        // Drugi: projekcja 1.2 MB > 1 MB → QuotaExceeded (rollback).
        let r = commit_document_row(
            &conn,
            "doc-2",
            "sha2bbbb",
            "application/pdf",
            data.len() as u64,
            "t",
            limit_mb,
        );
        assert_eq!(r, Err(AbiError::QuotaExceeded), "drugi przekracza limit → odrzucony");
        let used = current_storage_bytes(&conn);
        assert_eq!(used, 600_000, "tylko jeden dokument zaksięgowany — limit nieprzekroczony");
        set_root_override(None);
    }

    #[test]
    fn isolation_instance_b_cannot_read_instance_a_doc() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir_a = documents_dir("org-default", "alpha").unwrap();
        let dir_b = documents_dir("org-default", "beta").unwrap();

        stream_put(&dir_a, "doc-secret", "text/plain", b"poufne A", DOCUMENT_CHUNK_BYTES, 0, false);

        let conn_b = open_registry(&dir_b).unwrap();
        let found: Option<String> = conn_b
            .query_row(
                "SELECT sha256 FROM documents WHERE doc_id = ?1",
                rusqlite::params!["doc-secret"],
                |r| r.get(0),
            )
            .ok();
        assert!(found.is_none(), "instancja B NIE widzi doc_id instancji A");
        assert_ne!(dir_a, dir_b);
        set_root_override(None);
    }

    #[test]
    fn delete_removes_file_and_registry_entry() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();

        let (doc_id, sha) =
            stream_put(&dir, "doc-del", "text/plain", b"do skasowania", DOCUMENT_CHUNK_BYTES, 0, false);
        let path = blob_path(&dir, &sha);
        assert!(path.exists());

        let conn = open_registry(&dir).unwrap();
        // Kolejność delete: plik, potem wiersz.
        let other_ref: bool = conn
            .query_row(
                "SELECT 1 FROM documents WHERE sha256 = ?1 AND doc_id != ?2 LIMIT 1",
                rusqlite::params![&sha, &doc_id],
                |_| Ok(()),
            )
            .is_ok();
        assert!(!other_ref);
        std::fs::remove_file(&path).unwrap();
        conn.execute("DELETE FROM documents WHERE doc_id = ?1", rusqlite::params![&doc_id])
            .unwrap();
        assert!(!path.exists(), "plik skasowany");
        let gone: bool = conn
            .query_row("SELECT 1 FROM documents WHERE doc_id = ?1", rusqlite::params![&doc_id], |_| Ok(()))
            .is_ok();
        assert!(!gone, "wpis rejestru skasowany");
        set_root_override(None);
    }

    #[test]
    fn ownership_foreign_doc_id_is_not_found() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir_a = documents_dir("org-default", "alpha").unwrap();
        let dir_b = documents_dir("org-default", "beta").unwrap();
        stream_put(&dir_a, "doc-a", "text/plain", b"A", DOCUMENT_CHUNK_BYTES, 0, false);

        let conn_b = open_registry(&dir_b).unwrap();
        let row: Option<String> = conn_b
            .query_row("SELECT sha256 FROM documents WHERE doc_id = ?1", rusqlite::params!["doc-a"], |r| r.get(0))
            .ok();
        assert!(row.is_none(), "obcy doc_id → NotFound (brak w rejestrze instancji)");
        set_root_override(None);
    }

    #[test]
    fn storage_limit_projection_blocks_oversize() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        stream_put(&dir, "doc-1", "application/pdf", &vec![0u8; 600_000], DOCUMENT_CHUNK_BYTES, 0, false);
        let conn = open_registry(&dir).unwrap();
        assert_eq!(current_storage_bytes(&conn), 600_000);

        // Limit 1MB, nowy plik 600KB → transakcja odrzuca (projekcja 1.2MB > 1MB).
        let r = commit_document_row(&conn, "doc-2", "shaXXXX", "application/pdf", 600_000, "t", 1);
        assert_eq!(r, Err(AbiError::QuotaExceeded));
        set_root_override(None);
    }

    #[test]
    fn invalid_doc_id_rejected() {
        assert!(validate_doc_id("doc-abc").is_ok());
        assert!(validate_doc_id("../etc").is_err());
        assert!(validate_doc_id("a/b").is_err());
        assert!(validate_doc_id("").is_err());
        assert!(validate_doc_id("UPPER").is_err());
    }

    /// Symuluje akceptację chunk-0 (logika `start_new_upload` gałąź pending):
    /// świeży pending dla tego doc_id → „upload already in progress" (NIE truncate).
    fn try_start_chunk0(
        dir: &std::path::Path,
        org: &str,
        addon: &str,
        doc_id: &str,
        chunk: &[u8],
    ) -> Result<(), &'static str> {
        tmp_dir(dir).unwrap();
        let acc_key = (org.to_string(), addon.to_string(), doc_id.to_string());
        let part = partial_path(dir, doc_id);
        let mut map = pending_uploads().lock().unwrap_or_else(|e| e.into_inner());
        let existing_fresh = matches!(
            map.get(&acc_key),
            Some(p) if now_unix().saturating_sub(p.last_activity) <= PENDING_TTL_SECS
        );
        if existing_fresh {
            return Err("upload_already_in_progress");
        }
        std::fs::File::create(&part)
            .unwrap()
            .write_all(chunk)
            .unwrap();
        map.insert(
            acc_key,
            PendingUpload {
                path: part,
                mime: "application/pdf".into(),
                total_chunks: 4,
                next_index: 1,
                bytes_so_far: chunk.len() as u64,
                last_activity: now_unix(),
            },
        );
        Ok(())
    }

    #[test]
    fn concurrent_chunk0_same_doc_second_is_in_progress() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        // Pierwszy chunk-0 zakłada pending; drugi chunk-0 tego samego doc_id musi
        // dostać „in progress" i NIE nadpisać (truncate) partiala pierwszego.
        let first = try_start_chunk0(&dir, "org-default", "alpha", "doc-race", b"AAAA");
        assert!(first.is_ok(), "pierwszy chunk-0 akceptowany");
        let part = partial_path(&dir, "doc-race");
        let before = std::fs::read(&part).unwrap();

        let second = try_start_chunk0(&dir, "org-default", "alpha", "doc-race", b"BBBBBBBB");
        assert_eq!(second, Err("upload_already_in_progress"), "drugi chunk-0 odrzucony");
        let after = std::fs::read(&part).unwrap();
        assert_eq!(before, after, "partial pierwszego NIE został nadpisany przez drugi chunk-0");

        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    #[test]
    fn get_concurrent_with_finalize_never_sees_dangling_row() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();

        // Wgraj partial, ale NIE finalizuj — symuluj stan w trakcie publikacji.
        let data = vec![3u8; 700_000];
        let part = partial_path(&dir, "doc-pub");
        std::fs::write(&part, &data).unwrap();

        // Przed finalizacją: get NIE widzi wiersza (brak w rejestrze) → NotFound.
        assert!(get_chunk_seek(&dir, "doc-pub", 0).is_none(), "przed publikacją brak wiersza");

        // Finalizacja publikuje blob PRZED wierszem. Po commitcie wiersza blob
        // ZAWSZE istnieje, więc każdy get widzący wiersz odczyta bajty (nie dangling).
        let sha = finalize_publish(&dir, "doc-pub", "application/pdf", data.len() as u64, 0).unwrap();
        assert!(blob_path(&dir, &sha).exists(), "blob opublikowany przed/wraz z wierszem");
        let (chunk0, total) = get_chunk_seek(&dir, "doc-pub", 0).expect("po publikacji get widzi wiersz i blob");
        assert_eq!(total, 3, "700KB / 256KiB → 3 kawałki");
        assert_eq!(&chunk0[..], &data[..DOCUMENT_CHUNK_BYTES], "pierwszy kawałek poprawny");
        set_root_override(None);
    }

    #[test]
    fn overwrite_commit_fail_keeps_old_document() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();

        // Stary dokument zapisany w limicie 1MB.
        let old = vec![1u8; 500_000];
        let part = partial_path(&dir, "doc-ow");
        std::fs::write(&part, &old).unwrap();
        let old_sha = finalize_publish(&dir, "doc-ow", "application/pdf", old.len() as u64, 1).unwrap();
        assert!(blob_path(&dir, &old_sha).exists(), "stary blob istnieje");

        // Nadpisanie tego samego doc_id większym plikiem, który przekracza limit
        // 1MB → commit (BEGIN IMMEDIATE projekcja) ODRZUCA. Projekcja przy
        // nadpisaniu: total(500k) - prev(500k) + new(1.2MB) = 1.2MB > 1MB. Stary
        // wiersz+blob MUSZĄ przeżyć (brak compensating DELETE z poprzedniej wersji).
        let new = vec![2u8; 1_200_000];
        std::fs::write(&part, &new).unwrap();
        let r = finalize_publish(&dir, "doc-ow", "application/pdf", new.len() as u64, 1);
        assert_eq!(r, Err(AbiError::QuotaExceeded), "nadpisanie przekracza limit → odrzucone");

        // Stary dokument nadal kompletny: wiersz wskazuje stary sha, stary blob żyje.
        let conn = open_registry(&dir).unwrap();
        let (sha, size): (String, i64) = conn
            .query_row(
                "SELECT sha256, size_bytes FROM documents WHERE doc_id = ?1",
                rusqlite::params!["doc-ow"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("stary wiersz przetrwał fail nadpisania");
        assert_eq!(sha, old_sha, "wiersz nadal wskazuje stary blob");
        assert_eq!(size, 500_000, "rozmiar starego dokumentu nienaruszony");
        assert!(blob_path(&dir, &old_sha).exists(), "stary blob nieskasowany");
        set_root_override(None);
    }

    #[test]
    fn delete_then_get_is_not_found_not_dangling() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();

        let (doc_id, sha) =
            stream_put(&dir, "doc-dg", "text/plain", b"do skasowania", DOCUMENT_CHUNK_BYTES, 0, false);
        assert!(blob_path(&dir, &sha).exists());

        // Delete transakcyjnie: DELETE wiersza PRZED unlink bloba (E).
        let conn = open_registry(&dir).unwrap();
        let other_ref: bool = conn
            .query_row(
                "SELECT 1 FROM documents WHERE sha256 = ?1 AND doc_id != ?2 LIMIT 1",
                rusqlite::params![&sha, &doc_id],
                |_| Ok(()),
            )
            .is_ok();
        conn.execute("DELETE FROM documents WHERE doc_id = ?1", rusqlite::params![&doc_id])
            .unwrap();
        if !other_ref {
            let _ = std::fs::remove_file(blob_path(&dir, &sha));
        }

        // Po delete: get → NotFound (brak wiersza), NIE dangling (brak okna gdzie
        // wiersz istnieje a plik już nie).
        assert!(get_chunk_seek(&dir, &doc_id, 0).is_none(), "po delete get → NotFound");
        assert!(!blob_path(&dir, &sha).exists(), "blob skasowany (brak referencji)");
        set_root_override(None);
    }

    #[test]
    fn get_concurrent_with_delete_same_instance_no_dangling() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        // Wgraj dokument, potem N rund: wątek get i wątek delete RÓWNOLEGLE na tym
        // samym doc_id, oba pod mutexem instancji (Bug 1). Inwariant: get widzący
        // wiersz ZAWSZE otwiera istniejący blob (nie open-fail na istniejącym
        // wierszu); get po delete dostaje NotFound. NIGDY open-fail-na-wierszu.
        let dir = Arc::new(dir);
        let lock = instance_lock("org-default", "alpha");

        for round in 0..50u32 {
            let doc_id = format!("doc-gd{round}");
            let data = vec![(round % 251) as u8; 700_000];
            let part = partial_path(&dir, &doc_id);
            std::fs::write(&part, &data).unwrap();
            let sha = {
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                finalize_publish(&dir, &doc_id, "application/pdf", data.len() as u64, 0).unwrap()
            };
            assert!(blob_path(&dir, &sha).exists());

            // Lustro produkcyjnego get pod mutexem instancji: SELECT wiersza +
            // open/seek/read bloba trzymają mutex, więc nie przeplatają się z delete.
            let get_doc = doc_id.clone();
            let get_dir = Arc::clone(&dir);
            let get_lock = Arc::clone(&lock);
            let getter = std::thread::spawn(move || -> Result<bool, &'static str> {
                let _guard = get_lock.lock().unwrap_or_else(|e| e.into_inner());
                let conn = open_registry(&get_dir).unwrap();
                let row: Option<(String, i64)> = conn
                    .query_row(
                        "SELECT sha256, size_bytes FROM documents WHERE doc_id = ?1",
                        rusqlite::params![&get_doc],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                match row {
                    None => Ok(false), // NotFound — delete wygrał wyścig, OK.
                    Some((sha, size)) => {
                        // Wiersz istnieje → blob MUSI istnieć (brak dangling).
                        let want = (size as u64).min(DOCUMENT_CHUNK_BYTES as u64) as usize;
                        let mut buf = vec![0u8; want];
                        let mut f = std::fs::File::open(blob_path(&get_dir, &sha))
                            .map_err(|_| "open_fail_na_istniejacym_wierszu")?;
                        f.seek(SeekFrom::Start(0)).map_err(|_| "seek_fail")?;
                        f.read_exact(&mut buf).map_err(|_| "read_fail")?;
                        Ok(true)
                    }
                }
            });

            let del_doc = doc_id.clone();
            let del_dir = Arc::clone(&dir);
            let del_lock = Arc::clone(&lock);
            let deleter = std::thread::spawn(move || {
                let _guard = del_lock.lock().unwrap_or_else(|e| e.into_inner());
                let conn = open_registry(&del_dir).unwrap();
                let sha: Option<String> = conn
                    .query_row(
                        "SELECT sha256 FROM documents WHERE doc_id = ?1",
                        rusqlite::params![&del_doc],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(sha) = sha {
                    let other_ref: bool = conn
                        .query_row(
                            "SELECT 1 FROM documents WHERE sha256 = ?1 AND doc_id != ?2 LIMIT 1",
                            rusqlite::params![&sha, &del_doc],
                            |_| Ok(()),
                        )
                        .is_ok();
                    conn.execute("DELETE FROM documents WHERE doc_id = ?1", rusqlite::params![&del_doc])
                        .unwrap();
                    if !other_ref {
                        let _ = std::fs::remove_file(blob_path(&del_dir, &sha));
                    }
                }
            });

            let got = getter.join().unwrap();
            deleter.join().unwrap();
            assert!(
                got.is_ok(),
                "runda {round}: get pod mutexem NIE może open-failować na istniejącym wierszu: {:?}",
                got
            );
        }

        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    #[test]
    fn gc_sweeps_stale_finalizing_after_crash() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();

        // Symuluj crash po renamie partial→finalizing, przed publikacją bloba:
        // osierocony `.finalizing` ze starym mtime (Bug 2).
        let stale = finalizing_path(&dir, "doc-crashed");
        std::fs::write(&stale, b"osierocony po crashu").unwrap();
        // Cofnij mtime poza TTL.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(PENDING_TTL_SECS + 60);
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old)).unwrap();
        assert!(stale.exists());

        // Świeży `.finalizing` (aktywna finalizacja) — NIE może zostać ruszony.
        let fresh = finalizing_path(&dir, "doc-active");
        std::fs::write(&fresh, b"trwajaca finalizacja").unwrap();

        purge_orphan_partials(&dir);

        assert!(!stale.exists(), "stary .finalizing po crashu skasowany przez GC");
        assert!(fresh.exists(), "świeży .finalizing aktywnej finalizacji NIE ruszony");
        set_root_override(None);
    }

    #[test]
    fn concurrent_put_threads_same_instance_serialize() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        let dir = documents_dir("org-default", "alpha").unwrap();
        tmp_dir(&dir).unwrap();
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        // N wątków finalizuje RÓŻNE doc_id tej samej instancji równolegle, każdy pod
        // mutexem instancji. Wszystkie muszą zaksięgować spójnie (brak korupcji
        // rejestru, brak wiszących wierszy: każdy widziany wiersz ma blob).
        let dir = Arc::new(dir);
        let lock = instance_lock("org-default", "alpha");
        let mut handles = Vec::new();
        for i in 0..8 {
            let dir = Arc::clone(&dir);
            let lock = Arc::clone(&lock);
            handles.push(std::thread::spawn(move || {
                let doc_id = format!("doc-t{i}");
                let data = vec![i as u8; 100_000 + i * 1000];
                let part = partial_path(&dir, &doc_id);
                std::fs::write(&part, &data).unwrap();
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                finalize_publish(&dir, &doc_id, "application/pdf", data.len() as u64, 0).unwrap()
            }));
        }
        let mut shas = Vec::new();
        for h in handles {
            shas.push(h.join().unwrap());
        }

        // Każdy zaksięgowany wiersz MA istniejący blob (publikacja blob-przed-wierszem).
        let conn = open_registry(&dir).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 8, "wszystkie 8 dokumentów zaksięgowane");
        for i in 0..8 {
            let doc_id = format!("doc-t{i}");
            let (chunk0, _total) = get_chunk_seek(&dir, &doc_id, 0)
                .unwrap_or_else(|| panic!("{doc_id} ma wiersz i blob"));
            assert_eq!(chunk0[0], i as u8, "bajty {doc_id} poprawne");
        }
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    // =========================================================================
    // Host-side upload (panel UI → document store) — accept_upload_chunk_host
    // =========================================================================

    /// Host-side upload wielu fragmentów: po ostatnim fragmencie zwraca doc_ref,
    /// a bajty są czytelne przez tę samą ścieżkę co `document_get_v1` (seek).
    #[test]
    fn host_upload_accumulates_and_finalizes_readable_doc_ref() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        let dir = documents_dir("org-default", "alpha").unwrap();

        // 1.3 MB → 6 fragmentów po 256 KiB (ostatni krótszy). Niemożliwe w KV.
        let data: Vec<u8> = (0..1_300_000u32).map(|i| (i % 251) as u8).collect();
        let upload_id = "abc-123";
        let total_chunks = data.len().div_ceil(DOCUMENT_CHUNK_BYTES) as u32;

        let mut doc_ref = None;
        for seq in 0..total_chunks {
            let start = seq as usize * DOCUMENT_CHUNK_BYTES;
            let end = (start + DOCUMENT_CHUNK_BYTES).min(data.len());
            let out = accept_upload_chunk_host(
                "org-default", "alpha", upload_id, "application/pdf",
                seq, total_chunks, &data[start..end], 0,
            )
            .unwrap();
            match out {
                HostUploadOutcome::Buffered { .. } => assert!(seq + 1 < total_chunks),
                HostUploadOutcome::Finalized { doc_ref: r, size_bytes } => {
                    assert_eq!(seq + 1, total_chunks, "finalizacja na ostatnim fragmencie");
                    assert_eq!(size_bytes, data.len() as u64);
                    doc_ref = Some(r);
                }
            }
        }
        let doc_ref = doc_ref.expect("doc_ref po ostatnim fragmencie");

        // doc_ref czytelny przez seek (ścieżka document_get) — pełny roundtrip.
        let mut assembled = Vec::new();
        let (_c0, total) = get_chunk_seek(&dir, &doc_ref, 0).unwrap();
        for i in 0..total as usize {
            let (c, _t) = get_chunk_seek(&dir, &doc_ref, i).unwrap();
            assembled.extend_from_slice(&c);
        }
        assert_eq!(assembled, data, "bajty wgrane = bajty czytane przez doc_ref");

        // Brak osieroconego partiala po finalizacji.
        assert!(!partial_path(&dir, &doc_ref).exists(), "partial skasowany");
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    /// Pojedynczy fragment (total_chunks=1) finalizuje od razu.
    #[test]
    fn host_upload_single_chunk_finalizes_immediately() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        let dir = documents_dir("org-default", "alpha").unwrap();

        let data = b"maly plik";
        let out = accept_upload_chunk_host(
            "org-default", "alpha", "u1", "text/plain", 0, 1, data, 0,
        )
        .unwrap();
        let doc_ref = match out {
            HostUploadOutcome::Finalized { doc_ref, size_bytes } => {
                assert_eq!(size_bytes, data.len() as u64);
                doc_ref
            }
            HostUploadOutcome::Buffered { .. } => panic!("1 fragment → finalizacja od razu"),
        };
        let (chunk, total) = get_chunk_seek(&dir, &doc_ref, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(chunk, data);
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    /// Izolacja multi-tenant: doc_ref wgrany przez (org-a, alpha) NIE jest
    /// widoczny w store innej instancji/org (osobny rejestr + katalog).
    #[test]
    fn host_upload_isolated_per_instance_and_org() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        let out = accept_upload_chunk_host(
            "org-a", "alpha", "u-iso", "text/plain", 0, 1, b"sekret", 0,
        )
        .unwrap();
        let doc_ref = match out {
            HostUploadOutcome::Finalized { doc_ref, .. } => doc_ref,
            _ => panic!("finalized"),
        };

        // Inna instancja tego samego org — brak wiersza.
        let other_inst = documents_dir("org-a", "beta").unwrap();
        assert!(get_chunk_seek(&other_inst, &doc_ref, 0).is_none(), "inna instancja nie widzi doc_ref");
        // Inny org, ta sama nazwa instancji — brak wiersza.
        let other_org = documents_dir("org-b", "alpha").unwrap();
        assert!(get_chunk_seek(&other_org, &doc_ref, 0).is_none(), "inny org nie widzi doc_ref");
        // Właściwa instancja — widzi.
        let own = documents_dir("org-a", "alpha").unwrap();
        assert!(get_chunk_seek(&own, &doc_ref, 0).is_some(), "właściciel widzi doc_ref");
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    /// Cap: niemonotoniczna sekwencja (skok seq) odrzucona i czyści partial.
    #[test]
    fn host_upload_rejects_sequence_gap() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        accept_upload_chunk_host("org-default", "alpha", "u-seq", "text/plain", 0, 3, b"aaa", 0).unwrap();
        // Skok do seq=2 (pominięty 1) → mismatch.
        let err = accept_upload_chunk_host("org-default", "alpha", "u-seq", "text/plain", 2, 3, b"ccc", 0);
        assert!(matches!(err, Err(("chunk_sequence_mismatch", AbiError::Operation))));
        // Partial skasowany po mismatch.
        let doc_id = format!("up-{}", sanitize_upload_id("u-seq"));
        assert!(!partial_path(&documents_dir("org-default", "alpha").unwrap(), &doc_id).exists());
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    /// Cap: za duży pojedynczy fragment (> MAX_PUT_CHUNK_BYTES) odrzucony.
    #[test]
    fn host_upload_rejects_oversized_chunk() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        let huge = vec![0u8; MAX_PUT_CHUNK_BYTES + 1];
        let err = accept_upload_chunk_host("org-default", "alpha", "u-big", "text/plain", 0, 1, &huge, 0);
        assert!(matches!(err, Err(("chunk_too_large", AbiError::PayloadTooLarge))));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }

    /// Cap: invalid total_chunks i seq poza zakresem odrzucone wcześnie.
    #[test]
    fn host_upload_rejects_bad_seq_total() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        assert!(matches!(
            accept_upload_chunk_host("o", "alpha", "u", "text/plain", 0, 0, b"x", 0),
            Err(("invalid_total_chunks", _))
        ));
        assert!(matches!(
            accept_upload_chunk_host("o", "alpha", "u", "text/plain", 5, 3, b"x", 0),
            Err(("chunk_index_out_of_range", _))
        ));
        set_root_override(None);
    }

    /// Storage quota (limit_mb) egzekwowana przy finalizacji.
    #[test]
    fn host_upload_enforces_storage_quota() {
        let _lock = override_lock().lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempdir().unwrap();
        let _g = set_root_override(Some(tmp.path().to_path_buf()));
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();

        // limit_mb=0 w precheck (start), ale finalizacja dostaje realny limit. Tu
        // 1 MB limit, plik 1 fragment ~9 bajtów → mieści się; potem duży > limit.
        let data = vec![7u8; 2 * 1024 * 1024];
        // 1 fragment 2 MB przekracza MAX_PUT_CHUNK_BYTES? Nie (8 MiB). limit 1 MB.
        let err = accept_upload_chunk_host("org-default", "alpha", "u-q", "text/plain", 0, 1, &data, 1);
        assert!(
            matches!(err, Err(("storage_limit_exceeded", AbiError::QuotaExceeded))),
            "2MB plik przy limicie 1MB odrzucony, dostałem {err:?}"
        );
        pending_uploads().lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_root_override(None);
    }
}
