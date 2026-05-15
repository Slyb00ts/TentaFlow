// =============================================================================
// Plik: addon/fs_sandbox.rs
// Opis: Per-addon FS sandbox dla F1a §6.5 (TentaVision M1.W4). Zapewnia
//       deterministyczne sciezki katalogu danych addonu w ~/.tentaflow/addons/
//       z sanityzacja addon_id (regex strict, blokuje path traversal i NULL byte).
//       Pliki SQLite per-addon (data.db) zywia w katalogu zwracanym przez
//       addon_data_dir(); migrations runner i storage_sql wolaja tylko stad.
// =============================================================================

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use super::errors::AbiError;

// =============================================================================
// Walidacja addon_id
// =============================================================================

/// Regex sciezki bezpiecznej dla addon_id: tylko male litery, cyfry, myslnik;
/// pierwszy znak alfanumeryczny; dlugosc 1-64. Strict subset addon.id z
/// manifestu (manifest dopuszcza takze '.' i '_' — tutaj zacieskamy, bo nazwa
/// trafia bezposrednio do sciezki na dysku).
fn addon_id_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}$").expect("regex addon_id stale poprawny")
    })
}

/// Sprawdza czy addon_id jest bezpieczny do uzycia jako segment sciezki.
///
/// Zwraca `AbiError::Operation` gdy:
/// - addon_id pusty
/// - addon_id zawiera path traversal (`..`), separator (`/`, `\`), NULL byte
/// - addon_id zawiera znaki spoza regex (uppercase, kropki, podkreslnik...)
///
/// Wywolywane przed `addon_data_dir` i `addon_db_path` (oba uzywaja id w
/// `PathBuf::join`). Tym samym uniemozliwia eskape sandboxa.
pub fn validate_addon_id(addon_id: &str) -> Result<(), AbiError> {
    if addon_id.is_empty() {
        return Err(AbiError::Operation);
    }
    if addon_id.contains('\0')
        || addon_id.contains('/')
        || addon_id.contains('\\')
        || addon_id.contains("..")
    {
        return Err(AbiError::Operation);
    }
    if !addon_id_regex().is_match(addon_id) {
        return Err(AbiError::Operation);
    }
    Ok(())
}

// =============================================================================
// Lokalizacja katalogu danych
// =============================================================================

/// Korzen sandboxa dla addona w katalogu uzytkownika.
///
/// W F1a uzywamy `~/.tentaflow/addons`. Mozna w przyszlosci podmieniac (env
/// var TENTAFLOW_ADDONS_ROOT) na potrzeby testow E2E — w F1a testy uzywaja
/// `tempfile::tempdir()` i mockuja przez parametr (nie przez to API).
fn addons_root() -> Result<PathBuf, AbiError> {
    // `dirs::home_dir()` zwraca None na headless srodowiskach bez HOME — w
    // takiej sytuacji zwracamy Operation (nie panikujemy).
    let home = dirs::home_dir().ok_or(AbiError::Operation)?;
    Ok(home.join(".tentaflow").join("addons"))
}

/// Zwraca per-addon katalog danych `~/.tentaflow/addons/<addon_id>/`.
/// Tworzy katalog (idempotent) wraz z hierarchia jesli nie istnieje.
/// Na Unixach ustawia uprawnienia 0700 (tylko wlasciciel).
pub fn addon_data_dir(addon_id: &str) -> Result<PathBuf, AbiError> {
    validate_addon_id(addon_id)?;
    let root = addons_root()?;
    let path = root.join(addon_id);
    std::fs::create_dir_all(&path).map_err(|_| AbiError::Operation)?;

    // Restrict dostepu do katalogu addonu — chroni dane przed innymi
    // uzytkownikami systemu (multi-user host). Best-effort: gdy chmod
    // sie nie powiedzie (FAT/exFAT, Windows), kontynuujemy bez bledu.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }

    Ok(path)
}

/// Sciezka per-addon SQLite (data.db) — uzywana przez storage_sql i migrations.
pub fn addon_db_path(addon_id: &str) -> Result<PathBuf, AbiError> {
    Ok(addon_data_dir(addon_id)?.join("data.db"))
}

// =============================================================================
// Testy
// =============================================================================

/// Globalny mutex serializujacy testy ktore modyfikuja `HOME`. Cargo
/// uruchamia testy z jednego modulu sekwencyjnie, ale roznych modulow
/// rownolegle — bez tej blokady `fs_sandbox`, `storage_sql` i `migrations`
/// walcza o globalna zmienna srodowiskowa. Eksportowany pod cfg(test).
#[cfg(test)]
pub(crate) fn test_home_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tmp_home<F: FnOnce()>(f: F) {
        let _guard = super::test_home_lock().lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn valid_addon_ids() {
        for id in &[
            "a",
            "a1",
            "tentavision",
            "teams-bot",
            "x-y-z",
            "abc-123-def",
            "0",
        ] {
            assert!(
                validate_addon_id(id).is_ok(),
                "powinno akceptowac: {}",
                id
            );
        }
    }

    #[test]
    fn invalid_addon_id_with_path_traversal_rejected() {
        for id in &["..", "../etc", "a/..", ".."] {
            assert!(
                validate_addon_id(id).is_err(),
                "powinno odrzucic path traversal: {}",
                id
            );
        }
    }

    #[test]
    fn invalid_addon_id_with_absolute_path_rejected() {
        for id in &["/etc/passwd", "\\windows\\system32"] {
            assert!(
                validate_addon_id(id).is_err(),
                "powinno odrzucic absolute path: {}",
                id
            );
        }
    }

    #[test]
    fn invalid_addon_id_with_null_byte_rejected() {
        let with_null = "abc\0def";
        assert!(validate_addon_id(with_null).is_err());
    }

    #[test]
    fn invalid_empty_or_disallowed_chars_rejected() {
        assert!(validate_addon_id("").is_err());
        assert!(validate_addon_id("ABC").is_err(), "uppercase forbidden");
        assert!(validate_addon_id("a.b").is_err(), "dot forbidden");
        assert!(validate_addon_id("a_b").is_err(), "underscore forbidden");
        assert!(validate_addon_id("-abc").is_err(), "pierwszy znak dash");
        let too_long = "a".repeat(65);
        assert!(validate_addon_id(&too_long).is_err(), "max 64 chars");
    }

    #[test]
    fn test_addon_data_dir_creates_directory() {
        with_tmp_home(|| {
            let path = addon_data_dir("test-addon").expect("addon_data_dir");
            assert!(path.exists(), "katalog utworzony");
            assert!(path.is_dir());
            assert!(path.ends_with("test-addon"));
        });
    }

    #[test]
    fn test_addon_data_dir_idempotent() {
        with_tmp_home(|| {
            let p1 = addon_data_dir("idem-addon").expect("first");
            let p2 = addon_data_dir("idem-addon").expect("second");
            assert_eq!(p1, p2, "ta sama sciezka");
            assert!(p1.exists());
        });
    }

    #[test]
    fn test_addon_db_path_in_data_dir() {
        with_tmp_home(|| {
            let dbp = addon_db_path("db-addon").expect("db path");
            assert!(dbp.ends_with("data.db"));
            assert!(dbp.parent().unwrap().exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_addon_data_dir_permissions_0700() {
        use std::os::unix::fs::PermissionsExt;
        with_tmp_home(|| {
            let p = addon_data_dir("perm-test").expect("created");
            let meta = std::fs::metadata(&p).unwrap();
            // Niskie 9 bitow = mode (rwx rwx rwx). 0700 = rwx --- ---.
            let mode_low = meta.permissions().mode() & 0o777;
            assert_eq!(mode_low, 0o700, "katalog addona powinien byc 0700");
        });
    }
}
