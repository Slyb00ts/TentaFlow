// =============================================================================
// Plik: addon/permissions.rs
// Opis: PermissionChecker — proaktywny cache uprawnien addonow.
//       Cache jest ZAWSZE pelny — check() nigdy nie trafia do DB.
//       Odswiezanie: co 5 minut w tle + natychmiast po zmianie z UI.
//       Hierarchia: admin bypass > explicit deny > user grant > group grant > default deny.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tracing::{debug, warn};

use crate::db::DbPool;

// =============================================================================
// Typy uprawnien
// =============================================================================

/// Wynik sprawdzenia uprawnienia
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResult {
    /// Przyznano — uzytkownik ma uprawnienie
    Granted,
    /// Jawnie odmowiono (explicit deny)
    Denied,
    /// Nie skonfigurowano — domyslnie odmowiono
    NotConfigured,
}

impl PermissionResult {
    /// Sprawdza czy uprawnienie zostalo przyznane
    pub fn is_granted(&self) -> bool {
        *self == PermissionResult::Granted
    }
}

// =============================================================================
// Klucz cache
// =============================================================================

/// Klucz cache uprawnien
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    addon_id: String,
    user_id: i64,
    permission_type: String,
    resource: String,
}

// =============================================================================
// Interwaly odswiezania
// =============================================================================

/// Interwal odswiezania cache w tle (5 minut)
const BACKGROUND_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

// =============================================================================
// PermissionChecker — proaktywny cache
// =============================================================================

/// Checker uprawnien z proaktywnym cache w pamieci.
/// Cache jest ZAWSZE pelny — check() NIGDY nie trafia do DB.
/// Odswiezanie odbywa sie w tle co 5 minut oraz natychmiast po zmianie z UI.
///
/// Hierarchia sprawdzania:
/// 1. Admin bypass (user w grupie "admins")
/// 2. Explicit deny (granted=0)
/// 3. User grant (granted=1)
/// 4. Group grant (granted=1)
/// 5. Default deny (NotConfigured)
pub struct PermissionChecker {
    db: DbPool,
    /// Cache uprawnien: CacheKey → PermissionResult
    cache: Arc<RwLock<HashMap<CacheKey, PermissionResult>>>,
    /// Cache listy adminow (user_id)
    admin_cache: Arc<RwLock<Vec<i64>>>,
    /// Licznik trafien cache — monitoring
    cache_hits: AtomicU64,
    /// Licznik odpytan — monitoring
    cache_lookups: AtomicU64,
}

impl PermissionChecker {
    /// Tworzy nowy PermissionChecker z podana baza danych
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            cache: Arc::new(RwLock::new(HashMap::with_capacity(256))),
            admin_cache: Arc::new(RwLock::new(Vec::new())),
            cache_hits: AtomicU64::new(0),
            cache_lookups: AtomicU64::new(0),
        }
    }

    /// Sprawdza uprawnienie addonu dla uzytkownika.
    /// ZAWSZE z cache — nigdy nie trafia do DB.
    pub fn check(
        &self,
        addon_id: &str,
        user_id: i64,
        permission_type: &str,
        resource: Option<&str>,
    ) -> PermissionResult {
        self.cache_lookups.fetch_add(1, Ordering::Relaxed);

        // 1. Admin bypass — sprawdz z cache listy adminow
        {
            let admins = self.admin_cache.read();
            if admins.contains(&user_id) {
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
                return PermissionResult::Granted;
            }
        }

        // 2. Sprawdz z cache uprawnien
        let resource_str = resource.unwrap_or("*").to_string();
        let cache_key = CacheKey {
            addon_id: addon_id.to_string(),
            user_id,
            permission_type: permission_type.to_string(),
            resource: resource_str,
        };

        let cache = self.cache.read();
        if let Some(result) = cache.get(&cache_key) {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return *result;
        }

        // Brak wpisu w cache — domyslnie NotConfigured
        PermissionResult::NotConfigured
    }

    /// Zaladuj WSZYSTKIE uprawnienia z DB do cache.
    /// Wywolywane przy starcie i co 5 minut w tle.
    pub fn refresh_all(&self) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => {
                warn!("refresh_all: nie mozna zablokowac DB: {}", e);
                return;
            }
        };

        // Zaladuj liste adminow
        let admins = Self::load_admins(&conn);

        // Zaladuj wszystkie uprawnienia i zbuduj nowa mape
        let new_cache = Self::load_all_permissions(&conn);

        // Zamien cache atomowo (swap)
        {
            let mut admin_cache = self.admin_cache.write();
            *admin_cache = admins;
        }
        {
            let mut cache = self.cache.write();
            *cache = new_cache;
        }

        debug!("Cache uprawnien odswiezony (refresh_all)");
    }

    /// Odswierz uprawnienia jednego addonu.
    /// Wywolywane natychmiast po zmianie z UI.
    pub fn refresh_addon(&self, addon_id: &str) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => {
                warn!("refresh_addon: nie mozna zablokowac DB: {}", e);
                return;
            }
        };

        let addon_entries = Self::load_addon_permissions(&conn, addon_id);

        // Zaktualizuj wpisy w cache dla tego addonu
        let mut cache = self.cache.write();
        cache.retain(|key, _| key.addon_id != addon_id);
        cache.extend(addon_entries);

        debug!("Cache uprawnien odswiezony dla addonu '{}'", addon_id);
    }

    /// Odswierz liste adminow.
    /// Wywolywane po zmianie przynaleznosci do grup.
    pub fn refresh_admins(&self) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => {
                warn!("refresh_admins: nie mozna zablokowac DB: {}", e);
                return;
            }
        };

        let admins = Self::load_admins(&conn);
        let mut admin_cache = self.admin_cache.write();
        *admin_cache = admins;

        debug!("Cache listy adminow odswiezony");
    }

    /// Uruchom background task odswiezajacy cache co 5 minut.
    /// Nie blokuje — dziala w tle jako tokio task.
    pub fn start_background_refresh(self: &Arc<Self>) {
        let checker = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(BACKGROUND_REFRESH_INTERVAL).await;
                checker.refresh_all();
            }
        });
    }

    /// Uniewaznij caly cache (kompatybilnosc wsteczna — wywoluje refresh_all)
    pub fn invalidate_cache(&self) {
        self.refresh_all();
    }

    /// Uniewaznij cache dla konkretnego addonu (kompatybilnosc — wywoluje refresh_addon)
    pub fn invalidate_addon(&self, addon_id: &str) {
        self.refresh_addon(addon_id);
    }

    /// Zwraca statystyki cache (hits, lookups)
    pub fn cache_stats(&self) -> (u64, u64) {
        (
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_lookups.load(Ordering::Relaxed),
        )
    }

    // =========================================================================
    // Metody prywatne — ladowanie z DB
    // =========================================================================

    /// Laduje liste user_id adminow (uzytkownikow w grupie "admins")
    fn load_admins(conn: &rusqlite::Connection) -> Vec<i64> {
        let result = conn.prepare(
            "SELECT gm.user_id FROM group_members gm \
             JOIN user_groups g ON g.id = gm.group_id \
             WHERE g.name = 'admins'"
        );
        let mut stmt = match result {
            Ok(s) => s,
            Err(e) => {
                warn!("load_admins: blad przygotowania zapytania: {}", e);
                return Vec::new();
            }
        };

        let admins: Vec<i64> = match stmt.query_map([], |row| row.get(0)) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                warn!("load_admins: blad zapytania: {}", e);
                Vec::new()
            }
        };

        debug!("Zaladowano {} adminow", admins.len());
        admins
    }

    /// Laduje WSZYSTKIE uprawnienia z DB i buduje mape cache.
    /// Rozwiazuje hierarchie: deny > user grant > group grant dla kazdego
    /// unikalnego klucza (addon_id, user_id, permission_type, resource).
    fn load_all_permissions(conn: &rusqlite::Connection) -> HashMap<CacheKey, PermissionResult> {
        // Zbierz surowe wpisy z DB
        let raw = Self::query_all_raw_entries(conn);

        // Zbuduj mape z rozwiazana hierarchia
        Self::resolve_permissions(raw)
    }

    /// Laduje uprawnienia jednego addonu z DB
    fn load_addon_permissions(conn: &rusqlite::Connection, addon_id: &str) -> HashMap<CacheKey, PermissionResult> {
        let raw = Self::query_addon_raw_entries(conn, addon_id);
        Self::resolve_permissions(raw)
    }

    /// Pobiera surowe wpisy uprawnien dla WSZYSTKICH addonow
    fn query_all_raw_entries(conn: &rusqlite::Connection) -> Vec<RawEntry> {
        let mut entries = Vec::new();

        // Uprawnienia per user (subject_type = 'user')
        if let Ok(mut stmt) = conn.prepare(
            "SELECT addon_id, subject_id, permission_id, granted \
             FROM addon_permissions \
             WHERE subject_type = 'user'"
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(RawEntry {
                    addon_id: row.get(0)?,
                    user_id: row.get(1)?,
                    permission_id: row.get(2)?,
                    source: "user".to_string(),
                    granted: row.get::<_, i32>(3)? != 0,
                })
            }) {
                entries.extend(rows.filter_map(|r| r.ok()));
            }
        }

        // Uprawnienia per group — rozwin na user_id przez group_members
        if let Ok(mut stmt) = conn.prepare(
            "SELECT ap.addon_id, gm.user_id, ap.permission_id, ap.granted \
             FROM addon_permissions ap \
             JOIN group_members gm ON gm.group_id = ap.subject_id \
             WHERE ap.subject_type = 'group'"
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(RawEntry {
                    addon_id: row.get(0)?,
                    user_id: row.get(1)?,
                    permission_id: row.get(2)?,
                    source: "group".to_string(),
                    granted: row.get::<_, i32>(3)? != 0,
                })
            }) {
                entries.extend(rows.filter_map(|r| r.ok()));
            }
        }

        entries
    }

    /// Pobiera surowe wpisy uprawnien dla jednego addonu
    fn query_addon_raw_entries(conn: &rusqlite::Connection, addon_id: &str) -> Vec<RawEntry> {
        let mut entries = Vec::new();

        if let Ok(mut stmt) = conn.prepare(
            "SELECT addon_id, subject_id, permission_id, granted \
             FROM addon_permissions \
             WHERE subject_type = 'user' AND addon_id = ?1"
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![addon_id], |row| {
                Ok(RawEntry {
                    addon_id: row.get(0)?,
                    user_id: row.get(1)?,
                    permission_id: row.get(2)?,
                    source: "user".to_string(),
                    granted: row.get::<_, i32>(3)? != 0,
                })
            }) {
                entries.extend(rows.filter_map(|r| r.ok()));
            }
        }

        if let Ok(mut stmt) = conn.prepare(
            "SELECT ap.addon_id, gm.user_id, ap.permission_id, ap.granted \
             FROM addon_permissions ap \
             JOIN group_members gm ON gm.group_id = ap.subject_id \
             WHERE ap.subject_type = 'group' AND ap.addon_id = ?1"
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![addon_id], |row| {
                Ok(RawEntry {
                    addon_id: row.get(0)?,
                    user_id: row.get(1)?,
                    permission_id: row.get(2)?,
                    source: "group".to_string(),
                    granted: row.get::<_, i32>(3)? != 0,
                })
            }) {
                entries.extend(rows.filter_map(|r| r.ok()));
            }
        }

        entries
    }

    /// Rozwiazuje hierarchie uprawnien z surowych wpisow.
    /// Hierarchia: deny (dowolne) > user grant > group grant > NotConfigured.
    fn resolve_permissions(raw: Vec<RawEntry>) -> HashMap<CacheKey, PermissionResult> {
        // Grupuj po kluczu
        let mut grouped: HashMap<CacheKey, Vec<&RawEntry>> = HashMap::new();
        for entry in &raw {
            let key = CacheKey {
                addon_id: entry.addon_id.clone(),
                user_id: entry.user_id,
                permission_type: entry.permission_id.clone(),
                resource: "*".to_string(),
            };
            grouped.entry(key).or_default().push(entry);
        }

        let mut result = HashMap::with_capacity(grouped.len());

        for (key, entries) in grouped {
            // Faza 1: Sprawdz explicit deny (granted=false)
            if entries.iter().any(|e| !e.granted) {
                result.insert(key, PermissionResult::Denied);
                continue;
            }

            // Faza 2: Sprawdz user grants
            if entries.iter().any(|e| e.source == "user" && e.granted) {
                result.insert(key, PermissionResult::Granted);
                continue;
            }

            // Faza 3: Sprawdz group grants
            if entries.iter().any(|e| e.source == "group" && e.granted) {
                result.insert(key, PermissionResult::Granted);
                continue;
            }

            // Default deny
            result.insert(key, PermissionResult::NotConfigured);
        }

        result
    }
}

/// Surowy wpis z DB przed rozwiazaniem hierarchii
struct RawEntry {
    addon_id: String,
    user_id: i64,
    permission_id: String,
    source: String,
    granted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Sprawdza czy wzorzec glob pasuje do zasobu.
    /// Obsluguje wzorce: "*" (wszystko), "prefix*" (prefix match), "exact" (dokladne).
    fn pattern_matches(pattern: &str, resource: &str) -> bool {
        if pattern == "*" {
            return true;
        }

        if let Some(prefix) = pattern.strip_suffix('*') {
            return resource.starts_with(prefix);
        }

        // Dokladne dopasowanie
        pattern == resource
    }

    #[test]
    fn test_pattern_matches() {
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("bielik-*", "bielik-11b"));
        assert!(pattern_matches("bielik-*", "bielik-7b"));
        assert!(!pattern_matches("bielik-*", "llama-70b"));
        assert!(pattern_matches("exact", "exact"));
        assert!(!pattern_matches("exact", "other"));
        assert!(pattern_matches("*.microsoft.com", "*.microsoft.com"));
    }

    // =========================================================================
    // Funkcje pomocnicze — tworzenie in-memory DB z pelnym schematem
    // =========================================================================

    /// Tworzy in-memory DB z migracjami i seedem — prawdziwa baza do testow
    fn create_test_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    /// Wstawia uzytkownika do bazy testowej, zwraca user_id
    fn insert_test_user(db: &DbPool, username: &str) -> i64 {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO user_accounts (username, password_hash, display_name) VALUES (?1, 'hash', ?1)",
            rusqlite::params![username],
        ).expect("Nie udalo sie wstawic uzytkownika");
        conn.last_insert_rowid()
    }

    /// Tworzy grupe i zwraca group_id
    fn insert_test_group(db: &DbPool, name: &str) -> i64 {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO user_groups (name, description) VALUES (?1, ?1)",
            rusqlite::params![name],
        ).expect("Nie udalo sie wstawic grupy");
        conn.query_row(
            "SELECT id FROM user_groups WHERE name = ?1",
            rusqlite::params![name],
            |row| row.get(0),
        ).expect("Nie udalo sie pobrac group_id")
    }

    /// Dodaje uzytkownika do grupy
    fn add_user_to_group(db: &DbPool, group_id: i64, user_id: i64) {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?1, ?2)",
            rusqlite::params![group_id, user_id],
        ).expect("Nie udalo sie dodac uzytkownika do grupy");
    }

    /// Ustawia uprawnienie addonu (per user lub per group)
    fn set_permission(db: &DbPool, addon_id: &str, subject_type: &str, subject_id: i64, permission_id: &str, granted: bool) {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO addon_permissions (addon_id, subject_type, subject_id, permission_id, granted) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(addon_id, subject_type, subject_id, permission_id) \
             DO UPDATE SET granted = excluded.granted",
            rusqlite::params![addon_id, subject_type, subject_id, permission_id, granted as i32],
        ).expect("Nie udalo sie ustawic uprawnienia");
    }

    // =========================================================================
    // Test 1: Invalidacja cache uprawnien
    // =========================================================================

    #[test]
    fn permission_cache_invalidation() {
        // Arrange — in-memory DB z uzytkownikiem i uprawnieniem
        let db = create_test_db();
        let user_id = insert_test_user(&db, "test_user_cache");
        let addon_id = "test-addon";

        // Ustaw uprawnienie granted=true per user
        set_permission(&db, addon_id, "user", user_id, "chat_read", true);

        let checker = PermissionChecker::new(db.clone());
        // Warm-up cache
        checker.refresh_all();

        // Act 1 — sprawdzenie (z cache)
        let result1 = checker.check(addon_id, user_id, "chat_read", None);

        // Assert 1 — powinno zwrocic Granted
        assert_eq!(result1, PermissionResult::Granted, "Pierwsze sprawdzenie powinno zwrocic Granted");

        // Arrange — zmien uprawnienie na granted=false
        set_permission(&db, addon_id, "user", user_id, "chat_read", false);

        // Act 2 — sprawdz BEZ invalidacji cache (powinno zwrocic stary wynik z cache)
        let result2 = checker.check(addon_id, user_id, "chat_read", None);

        // Assert 2 — cache nadal zwraca Granted (stary wynik)
        assert_eq!(result2, PermissionResult::Granted, "Cache powinien zwrocic stary wynik Granted");

        // Act 3 — invaliduj cache i sprawdz ponownie
        checker.invalidate_cache();
        let result3 = checker.check(addon_id, user_id, "chat_read", None);

        // Assert 3 — teraz powinno zwrocic Denied (granted=false → explicit deny)
        assert_eq!(result3, PermissionResult::Denied, "Po invalidacji cache powinno zwrocic Denied");

        // Assert — statystyki cache: 2 hity (oba sprawdzenia z cache), 3 odpytania
        let (hits, lookups) = checker.cache_stats();
        assert!(hits >= 2, "Powinny byc przynajmniej 2 cache hity, jest: {}", hits);
        assert!(lookups >= 3, "Powinny byc przynajmniej 3 odpytania, jest: {}", lookups);
    }

    // =========================================================================
    // Test 2: Suma uprawnien z wielu grup (OR)
    // =========================================================================

    #[test]
    fn permission_group_union() {
        // Arrange — DB z 2 grupami, user w obu
        let db = create_test_db();
        let user_id = insert_test_user(&db, "test_user_groups");

        let group_a_id = insert_test_group(&db, "group_a");
        let group_b_id = insert_test_group(&db, "group_b");

        add_user_to_group(&db, group_a_id, user_id);
        add_user_to_group(&db, group_b_id, user_id);

        // Grupa A: granted=true dla "chat_read"
        set_permission(&db, "teams", "group", group_a_id, "chat_read", true);
        // Grupa B: brak uprawnien dla "chat_read" (nie ustawiamy nic)

        let checker = PermissionChecker::new(db.clone());
        checker.refresh_all();

        // Act
        let result = checker.check("teams", user_id, "chat_read", None);

        // Assert — user powinien miec Granted (suma grup, OR — jedna grupa przyznaje)
        assert_eq!(result, PermissionResult::Granted, "Suma grup powinna dac Granted jesli chociaz jedna grupa przyznaje");
    }

    // =========================================================================
    // Test 3: Admin bypass — admin ma dostep do wszystkiego
    // =========================================================================

    #[test]
    fn permission_admin_bypass() {
        // Arrange — DB z userem w grupie "admins"
        let db = create_test_db();
        let user_id = insert_test_user(&db, "admin_user");

        // Grupa "admins" jest tworzona przez seed — pobierz jej ID
        let admins_group_id = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT id FROM user_groups WHERE name = 'admins'",
                [],
                |row| row.get::<_, i64>(0),
            ).expect("Grupa 'admins' powinna istniec po seedzie")
        };

        add_user_to_group(&db, admins_group_id, user_id);

        // NIE ustawiaj zadnych uprawnien addonu
        let checker = PermissionChecker::new(db.clone());
        checker.refresh_all();

        // Act — sprawdz dowolne uprawnienie
        let result = checker.check("nieistniejacy-addon", user_id, "cokolwiek", None);

        // Assert — admin bypass powinien przyznac dostep
        assert_eq!(result, PermissionResult::Granted, "Admin powinien miec dostep do wszystkiego bez jawnych uprawnien");
    }

    // =========================================================================
    // Test 4: Brak konfiguracji uprawnien → NotConfigured
    // =========================================================================

    #[test]
    fn permission_not_configured() {
        // Arrange — DB z userem NIE w zadnej grupie, bez uprawnien
        let db = create_test_db();
        let user_id = insert_test_user(&db, "lonely_user");

        // NIE dodawaj do zadnej grupy, NIE ustawiaj uprawnien
        let checker = PermissionChecker::new(db.clone());
        checker.refresh_all();

        // Act
        let result = checker.check("test-addon", user_id, "chat_read", None);

        // Assert — powinno zwrocic NotConfigured (default deny)
        assert_eq!(result, PermissionResult::NotConfigured, "Brak uprawnien powinien dac NotConfigured");
    }

    // =========================================================================
    // Test 5: Wydajnosc cache — 10000 sprawdzen
    // =========================================================================

    #[test]
    fn permission_cache_performance() {
        // Arrange
        let db = create_test_db();
        let user_id = insert_test_user(&db, "perf_user");
        set_permission(&db, "perf-addon", "user", user_id, "llm", true);

        let checker = PermissionChecker::new(db.clone());
        checker.refresh_all();

        // Act — sprawdz 10000 razy
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let result = checker.check("perf-addon", user_id, "llm", None);
            assert_eq!(result, PermissionResult::Granted);
        }
        let elapsed = start.elapsed();

        // Assert — cache powinien byc szybki (zero DB queries)
        let (hits, lookups) = checker.cache_stats();
        assert_eq!(lookups, 10000, "Powinno byc 10000 odpytan");
        assert_eq!(hits, 10000, "Powinno byc 10000 cache hitow (zero DB queries)");
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "10000 sprawdzen powinno zajac < 100ms, zajelo: {:?}", elapsed
        );
    }

    // =========================================================================
    // Test 6: Cykl zycia — instalacja i deinstalacja addonu
    // =========================================================================

    #[test]
    fn lifecycle_install_uninstall() {
        // Arrange — tymczasowy katalog z minimalnym addonem
        let tmp_dir = std::env::temp_dir().join(format!("tentaflow_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("Nie udalo sie utworzyc katalogu tymczasowego");

        // Manifest testowy
        let manifest_content = r#"
addon_id = "test-lifecycle"
version = "1.0.0"
display_name = "Test Lifecycle"
description = "Addon testowy do cyklu zycia"
author = "Test"
wasm_file = "addon.wasm"
platforms = []
permissions = []
tools = []
declared_permissions = []
"#;
        std::fs::write(tmp_dir.join("manifest.toml"), manifest_content)
            .expect("Nie udalo sie zapisac manifestu");

        // Minimalny plik WASM (magic number + pusty modul)
        let wasm_bytes: Vec<u8> = vec![
            0x00, 0x61, 0x73, 0x6D, // magic: \0asm
            0x01, 0x00, 0x00, 0x00, // wersja 1
        ];
        std::fs::write(tmp_dir.join("addon.wasm"), &wasm_bytes)
            .expect("Nie udalo sie zapisac WASM");

        let db = create_test_db();

        // Act 1 — instalacja
        let manifest = crate::addon::lifecycle::install(&tmp_dir, &db)
            .expect("Instalacja addonu powinna sie udac");

        // Assert — manifest sparsowany poprawnie
        assert_eq!(manifest.addon_id, "test-lifecycle");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.display_name, "Test Lifecycle");

        // Assert — addon w DB
        let addon = crate::db::repository::get_addon(&db, "test-lifecycle")
            .expect("Blad pobierania addonu")
            .expect("Addon powinien byc w DB po instalacji");
        assert_eq!(addon.addon_id, "test-lifecycle");
        assert_eq!(addon.version, "1.0.0");

        // Arrange — dodaj dane powiazane (uprawnienia, config) zeby sprawdzic czyszczenie
        set_permission(&db, "test-lifecycle", "user", 1, "chat_read", true);
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO addon_config (addon_id, key, value) VALUES ('test-lifecycle', 'test_key', 'test_val')",
                [],
            ).ok();
        }

        // Act 2 — deinstalacja
        crate::addon::lifecycle::uninstall("test-lifecycle", &db)
            .expect("Deinstalacja addonu powinna sie udac");

        // Assert — addon usuniety z DB
        let addon_after = crate::db::repository::get_addon(&db, "test-lifecycle")
            .expect("Blad pobierania addonu");
        assert!(addon_after.is_none(), "Addon powinien byc usuniety z DB po deinstalacji");

        // Assert — powiazane dane wyczyszczone
        {
            let conn = db.lock().unwrap();
            let perm_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM addon_permissions WHERE addon_id = 'test-lifecycle'",
                [],
                |row| row.get(0),
            ).unwrap_or(-1);
            assert_eq!(perm_count, 0, "Uprawnienia powinny byc wyczyszczone po deinstalacji");

            let config_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM addon_config WHERE addon_id = 'test-lifecycle'",
                [],
                |row| row.get(0),
            ).unwrap_or(-1);
            assert_eq!(config_count, 0, "Konfiguracja powinna byc wyczyszczona po deinstalacji");
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // =========================================================================
    // Test 7: Invalidacja cache per addon po zmianie uprawnien
    // =========================================================================

    #[test]
    fn permission_invalidate_on_change() {
        // Arrange
        let db = create_test_db();
        let user_id = insert_test_user(&db, "invalidate_user");
        let addon_id = "addon-invalidate-test";

        let checker = PermissionChecker::new(db.clone());
        checker.refresh_all();

        // Act 1 — sprawdz uprawnienie (brak konfiguracji → NotConfigured)
        let result1 = checker.check(addon_id, user_id, "chat_read", None);
        assert_eq!(result1, PermissionResult::NotConfigured, "Brak uprawnien → NotConfigured");

        // Arrange — ustaw uprawnienie granted=true
        set_permission(&db, addon_id, "user", user_id, "chat_read", true);

        // Act 2 — sprawdz bez invalidacji (cache nie ma wpisu → nadal NotConfigured)
        let result2 = checker.check(addon_id, user_id, "chat_read", None);
        assert_eq!(result2, PermissionResult::NotConfigured, "Cache powinien zwrocic stary wynik NotConfigured");

        // Act 3 — invaliduj cache dla tego addonu i sprawdz ponownie
        checker.invalidate_addon(addon_id);
        let result3 = checker.check(addon_id, user_id, "chat_read", None);
        assert_eq!(result3, PermissionResult::Granted, "Po invalidacji powinno zwrocic Granted");

        // Assert — statystyki
        let (hits, lookups) = checker.cache_stats();
        assert!(hits >= 1, "Powinien byc przynajmniej 1 cache hit, jest: {}", hits);
        assert!(lookups >= 3, "Powinny byc przynajmniej 3 odpytania, jest: {}", lookups);
    }
}
