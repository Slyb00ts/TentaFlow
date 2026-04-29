// =============================================================================
// Plik: chromium_provisioner.rs
// Opis: Auto-detekcja i fallback download Chromium dla native teams-bota.
//       1. Sprawdza systemowy Chromium (PATH + standardowe sciezki).
//       2. Sprawdza cache <TENTAFLOW_HOME>/chromium/<platform>/.
//       3. Jesli nie ma — pobiera Chrome for Testing z googleapis.
//       4. Na Linuksie sprawdza ldd dla brakujacych runtime libs i ostrzega.
// =============================================================================

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use crate::deploy::python_venv::LogSink;

/// Zwraca sciezke do dzialajacego Chromium. Najpierw probuje znalezc
/// systemowa instalacje, potem cache, potem pobiera Chrome for Testing.
pub fn ensure_chromium(log: &LogSink) -> Result<PathBuf> {
    if let Some(path) = find_system_chromium() {
        log(&format!("chromium: znaleziony systemowy {}", path.display()));
        check_runtime_libs(&path, log);
        return Ok(path);
    }

    let cache_dir = chromium_cache_dir()?;
    let cached = cached_chromium_path(&cache_dir);
    if cached.is_file() {
        log(&format!("chromium: reuse z cache {}", cached.display()));
        check_runtime_libs(&cached, log);
        return Ok(cached);
    }

    log("chromium: brak na hoscie i w cache — pobieram Chrome for Testing");
    download_chrome_for_testing(&cache_dir, log)?;
    if !cached.is_file() {
        return Err(anyhow!(
            "chromium: pobranie zakonczone, ale binarka nie istnieje pod {}",
            cached.display()
        ));
    }
    check_runtime_libs(&cached, log);
    Ok(cached)
}

fn find_system_chromium() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TENTAFLOW_CHROMIUM_PATH") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Chromium\Application\chrome.exe",
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ]
    } else {
        &[
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/snap/bin/chromium",
            "/usr/bin/brave-browser",
            "/usr/bin/microsoft-edge",
        ]
    };
    for path in candidates {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    let names: &[&str] = if cfg!(target_os = "windows") {
        &["chrome.exe", "chromium.exe", "msedge.exe", "brave.exe"]
    } else {
        &["chromium", "chromium-browser", "google-chrome", "brave-browser"]
    };
    for name in names {
        if let Some(found) = which_in_path(name) {
            return Some(found);
        }
    }
    None
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn chromium_cache_dir() -> Result<PathBuf> {
    Ok(crate::paths::tentaflow_home().join("chromium"))
}

fn cached_chromium_path(cache_dir: &Path) -> PathBuf {
    let platform = chrome_for_testing_platform();
    let subdir = format!("chrome-{platform}");
    let bin = if cfg!(target_os = "windows") {
        "chrome.exe"
    } else if cfg!(target_os = "macos") {
        "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    } else {
        "chrome"
    };
    cache_dir.join(subdir).join(bin)
}

fn chrome_for_testing_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "mac-arm64"
        } else {
            "mac-x64"
        }
    } else if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            "win64"
        } else {
            "win64"
        }
    } else {
        if cfg!(target_arch = "aarch64") {
            // Chrome for Testing oficjalnie nie wspiera linux-arm64.
            // Zwracamy x64 dla podzialu sciezek; download zwroci 404
            // i blad zawiera czytelna informacje.
            "linux64"
        } else {
            "linux64"
        }
    }
}

fn download_chrome_for_testing(cache_dir: &Path, log: &LogSink) -> Result<()> {
    let platform = chrome_for_testing_platform();
    if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        return Err(anyhow!(
            "chromium: Chrome for Testing nie ma builda dla linux-aarch64. \
             Zainstaluj systemowy Chromium: `apt-get install -y chromium` \
             albo `pacman -S chromium`."
        ));
    }

    std::fs::create_dir_all(cache_dir).with_context(|| {
        format!("tworzenie katalogu cache {}", cache_dir.display())
    })?;

    let versions_url =
        "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
    log(&format!("chromium: pobieram metadata {versions_url}"));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("budowa klienta http")?;
    let meta: serde_json::Value = client
        .get(versions_url)
        .send()
        .context("download versions json")?
        .error_for_status()
        .context("versions json status")?
        .json()
        .context("parsowanie versions json")?;

    let downloads = meta
        .pointer("/channels/Stable/downloads/chrome")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("brak channels.Stable.downloads.chrome w metadata"))?;
    let url = downloads
        .iter()
        .find_map(|entry| {
            let p = entry.get("platform")?.as_str()?;
            if p == platform {
                entry.get("url")?.as_str().map(String::from)
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow!("brak buildu Chrome for Testing dla platformy {platform}"))?;

    log(&format!("chromium: pobieram archiwum {url}"));
    let zip_bytes = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .context("download chromium zip")?
        .error_for_status()
        .context("chromium zip status")?
        .bytes()
        .context("read chromium zip body")?;

    log(&format!(
        "chromium: rozpakowuje {} bajtow do {}",
        zip_bytes.len(),
        cache_dir.display()
    ));
    extract_zip(&zip_bytes, cache_dir)?;

    if cfg!(unix) {
        if let Some(bin) = cached_chromium_path(cache_dir).to_str() {
            std::process::Command::new("chmod")
                .arg("+x")
                .arg(bin)
                .status()
                .ok();
        }
    }

    log("chromium: pobranie ukonczone");
    Ok(())
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Cursor;
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("otwieranie zip")?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("odczyt entry zip")?;
        let Some(rel_path) = file.enclosed_name() else {
            continue;
        };
        let outpath = dest.join(rel_path);
        if file.is_dir() {
            std::fs::create_dir_all(&outpath).ok();
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut out = std::fs::File::create(&outpath)
            .with_context(|| format!("create {}", outpath.display()))?;
        std::io::copy(&mut file, &mut out)
            .with_context(|| format!("copy {}", outpath.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                let _ = std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_runtime_libs(chrome: &Path, log: &LogSink) {
    let Ok(output) = std::process::Command::new("ldd").arg(chrome).output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("not found"))
        .filter_map(|l| l.trim().split_whitespace().next())
        .collect();
    if missing.is_empty() {
        return;
    }
    log(&format!(
        "chromium: brakujace runtime libs ({}): {}",
        missing.len(),
        missing.join(", ")
    ));
    log(
        "chromium: doinstaluj zaleznosci, np.: \
         apt-get install -y libnss3 libgbm1 libasound2 libdrm2 libatk-bridge2.0-0 \
         libxkbcommon0 libxcomposite1 libxdamage1 libxrandr2 libxss1 libpango-1.0-0 \
         fonts-liberation",
    );
}

#[cfg(not(target_os = "linux"))]
fn check_runtime_libs(_chrome: &Path, _log: &LogSink) {}
