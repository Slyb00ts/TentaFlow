// =============================================================================
// Plik: addon/oauth_master_key.rs
// Opis: Multiplatformowe ladowanie master-key dla szyfrowania OAuth addonow.
//       Hierarchia zrodel: (1) zmienna srodowiskowa TENTAFLOW_OAUTH_KEY,
//       (2) plik <data_dir>/oauth.key zwiazany XOR-em z machine binding ID,
//       (3) nowy losowy klucz z CSPRNG zapisany do (2). Dziala na Linux,
//       macOS, Windows, iOS, Android oraz w kontenerach Docker/k8s.
// Przyklad:
//   let key = oauth_master_key::load_or_init()?;
//   oauth_crypto::encrypt(&key, secret)?;
// =============================================================================

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Zmienna srodowiskowa z master-keyem w base64 — jesli ustawiona, bierze priorytet.
/// Uzywane w Docker/k8s/systemd EnvironmentFile (sekret zarzadzany przez orkiestratora).
const ENV_VAR: &str = "TENTAFLOW_OAUTH_KEY";

/// Nazwa pliku w data_dir zawierajaca master-key (XOR z machine binding ID).
const KEY_FILE: &str = "oauth.key";

/// Info label dla HKDF-SHA256 wyliczajacego mask z machine binding ID.
const HKDF_INFO: &[u8] = b"tentaflow-oauth-master-v1";

/// Laduje master-key z najbezpieczniejszego dostepnego zrodla; tworzy nowy gdy brak.
pub fn load_or_init() -> Result<[u8; 32]> {
    // 1. Priorytet: env (wstrzykiwany przez orkiestrator kontenerow / init system).
    if let Ok(b64) = std::env::var(ENV_VAR) {
        let bytes = STANDARD
            .decode(b64.trim())
            .map_err(|e| anyhow!("{} base64 decode failed: {}", ENV_VAR, e))?;
        if bytes.len() != 32 {
            bail!(
                "{} musi miec 32 bajty po base64 decode (aktualnie {})",
                ENV_VAR,
                bytes.len()
            );
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 2. Plik w data_dir zwiazany XOR z machine binding ID.
    let path = key_file_path()?;
    if path.exists() {
        verify_permissions(&path)?;
        let raw = fs::read(&path)?;
        if raw.len() != 32 {
            bail!(
                "Plik {} uszkodzony — nieprawidlowy rozmiar (oczekiwano 32B, jest {}B)",
                path.display(),
                raw.len()
            );
        }
        let mut stored = [0u8; 32];
        stored.copy_from_slice(&raw);
        return Ok(xor_with_binding(&stored));
    }

    // 3. Generacja nowego klucza (CSPRNG z OS-a).
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow!("OS RNG: {}", e))?;
    save_new_key(&path, &key)?;
    tracing::warn!(
        "Wygenerowano nowy master-key OAuth w {}. Zrob backup!",
        path.display()
    );
    Ok(key)
}

/// Jawnie zapisuje podany klucz do pliku (uzywane przy migracji z legacy settings).
pub fn write_key_file(key: &[u8; 32]) -> Result<PathBuf> {
    let path = key_file_path()?;
    save_new_key(&path, key)?;
    Ok(path)
}

/// Wylicza docelowa sciezke pliku `oauth.key` wg platformy.
fn key_file_path() -> Result<PathBuf> {
    // Override dla testow/CI/deployow niestandardowych.
    if let Ok(dir) = std::env::var("TENTAFLOW_DATA_DIR") {
        return Ok(PathBuf::from(dir).join(KEY_FILE));
    }

    #[cfg(target_os = "linux")]
    {
        if is_running_as_service() {
            return Ok(PathBuf::from("/var/lib/tentaflow").join(KEY_FILE));
        }
        let home = std::env::var("HOME").map_err(|_| anyhow!("Brak HOME"))?;
        Ok(PathBuf::from(home)
            .join(".local/share/tentaflow")
            .join(KEY_FILE))
    }
    #[cfg(target_os = "macos")]
    {
        if is_running_as_service() {
            return Ok(PathBuf::from("/usr/local/var/tentaflow").join(KEY_FILE));
        }
        let home = std::env::var("HOME").map_err(|_| anyhow!("Brak HOME"))?;
        Ok(PathBuf::from(home)
            .join("Library/Application Support/tentaflow")
            .join(KEY_FILE))
    }
    #[cfg(target_os = "windows")]
    {
        if is_running_as_service() {
            if let Ok(programdata) = std::env::var("ProgramData") {
                return Ok(PathBuf::from(programdata).join("tentaflow").join(KEY_FILE));
            }
        }
        let appdata = std::env::var("APPDATA").map_err(|_| anyhow!("Brak APPDATA"))?;
        Ok(PathBuf::from(appdata).join("tentaflow").join(KEY_FILE))
    }
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let dir = std::env::var("TENTAFLOW_DATA_DIR")
            .map_err(|_| anyhow!("TENTAFLOW_DATA_DIR musi byc ustawione na mobile"))?;
        Ok(PathBuf::from(dir).join(KEY_FILE))
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_os = "ios",
        target_os = "android"
    )))]
    {
        let dir = std::env::var("TENTAFLOW_DATA_DIR")
            .map_err(|_| anyhow!("TENTAFLOW_DATA_DIR musi byc ustawione na tej platformie"))?;
        Ok(PathBuf::from(dir).join(KEY_FILE))
    }
}

/// Heurystyka: czy proces dziala jako service/daemon (a nie sesja interaktywna).
fn is_running_as_service() -> bool {
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    {
        // UID 0 (root) lub brak HOME (systemd user bez HOME) traktujemy jako service.
        if unsafe { libc::getuid() } == 0 {
            return true;
        }
        if std::env::var("HOME").is_err() {
            return true;
        }
        false
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("SESSIONNAME")
            .map(|s| s.eq_ignore_ascii_case("Services"))
            .unwrap_or(false)
    }
    #[cfg(not(any(
        all(unix, not(any(target_os = "ios", target_os = "android"))),
        target_os = "windows"
    )))]
    {
        false
    }
}

/// Wykrywa czy proces dziala w kontenerze (Docker / containerd / k8s).
fn is_containerized() -> bool {
    #[cfg(target_os = "linux")]
    {
        if Path::new("/.dockerenv").exists() {
            return true;
        }
        fs::read_to_string("/proc/1/cgroup")
            .map(|s| s.contains("docker") || s.contains("kubepods") || s.contains("containerd"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Pobiera stabilny identyfikator "powiazania" z maszyna — uzywany jako XOR-mask dla
/// pliku. W kontenerze `/etc/machine-id` nie jest wiarygodny, wiec uzywamy HOSTNAME.
fn machine_binding_id() -> Vec<u8> {
    if is_containerized() {
        std::env::var("HOSTNAME")
            .unwrap_or_else(|_| "container-default".to_string())
            .into_bytes()
    } else {
        native_machine_id()
    }
}

#[cfg(target_os = "linux")]
fn native_machine_id() -> Vec<u8> {
    fs::read_to_string("/etc/machine-id")
        .or_else(|_| fs::read_to_string("/var/lib/dbus/machine-id"))
        .map(|s| s.trim().as_bytes().to_vec())
        .unwrap_or_else(|_| b"linux-no-machine-id".to_vec())
}

#[cfg(target_os = "macos")]
fn native_machine_id() -> Vec<u8> {
    std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find(|l| l.contains("IOPlatformUUID"))
                .and_then(|l| l.split('"').nth(3).map(|x| x.as_bytes().to_vec()))
        })
        .unwrap_or_else(|| b"macos-no-uuid".to_vec())
}

#[cfg(target_os = "windows")]
fn native_machine_id() -> Vec<u8> {
    // MachineGuid z rejestru to stabilny identyfikator instalacji Windows.
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .ok()
        .and_then(|k| k.get_value::<String, _>("MachineGuid").ok())
        .map(|s| s.into_bytes())
        .unwrap_or_else(|| b"windows-no-guid".to_vec())
}

#[cfg(any(target_os = "ios", target_os = "android"))]
fn native_machine_id() -> Vec<u8> {
    // Na mobile device_id musi byc wstrzykniety przez bridge jako env.
    std::env::var("TENTAFLOW_DEVICE_ID")
        .unwrap_or_else(|_| "mobile-no-device-id".to_string())
        .into_bytes()
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "ios",
    target_os = "android"
)))]
fn native_machine_id() -> Vec<u8> {
    b"unknown-platform".to_vec()
}

/// XOR-uje przechowywany klucz z mask = SHA256(machine_id || HKDF_INFO).
/// Podwojne zastosowanie tej samej funkcji jest operacja inwolutywna (dekoduje plik).
fn xor_with_binding(stored: &[u8; 32]) -> [u8; 32] {
    let binding = machine_binding_id();
    let mut hasher = Sha256::new();
    hasher.update(&binding);
    hasher.update(HKDF_INFO);
    let mask = hasher.finalize();
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = stored[i] ^ mask[i];
    }
    out
}

/// Weryfikuje ze plik ma ograniczone uprawnienia (0600 na Unix; Windows ACL nie weryfikowane).
fn verify_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o600 {
            bail!(
                "Plik {} ma niepoprawne uprawnienia: {:o} (oczekiwano 600). Uruchom: chmod 600 {}",
                path.display(),
                mode,
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Zapisuje nowy klucz (XOR z binding) z restrykcyjnymi uprawnieniami.
fn save_new_key(path: &Path, raw: &[u8; 32]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Nieprawidlowa sciezka (brak parent)"))?;
    fs::create_dir_all(parent)?;
    let bound = xor_with_binding(raw);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(&bound)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let mut f = fs::File::create(path)?;
        f.write_all(&bound)?;
        f.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_with_binding_is_involution() {
        let raw = [42u8; 32];
        let a = xor_with_binding(&raw);
        let b = xor_with_binding(&a);
        assert_eq!(b, raw, "podwojny XOR musi dawac oryginal");
    }

    #[test]
    fn env_var_override_loads_valid_key() {
        let key = [0x5Au8; 32];
        let b64 = STANDARD.encode(key);
        // SAFETY: test single-threaded w tym module.
        unsafe { std::env::set_var(ENV_VAR, &b64) };
        let loaded = load_or_init().expect("load z env");
        unsafe { std::env::remove_var(ENV_VAR) };
        assert_eq!(loaded, key);
    }

    #[test]
    fn env_var_wrong_size_errors() {
        let b64 = STANDARD.encode([0u8; 16]);
        unsafe { std::env::set_var(ENV_VAR, &b64) };
        let res = load_or_init();
        unsafe { std::env::remove_var(ENV_VAR) };
        assert!(res.is_err(), "16-bajtowy klucz z env musi byc odrzucony");
    }
}
