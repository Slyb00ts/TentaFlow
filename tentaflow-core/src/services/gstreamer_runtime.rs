// =============================================================================
// File: services/gstreamer_runtime.rs — runtime environment for GStreamer
// =============================================================================
//
// Prepares dynamic-library and plugin-scanner paths before `gst::init()` so
// Homebrew and framework installs work when TentaFlow is launched from a plain
// shell or GUI process.

use std::sync::Once;

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

static GSTREAMER_ENV_ONCE: Once = Once::new();

pub fn prepare_runtime_environment() {
    GSTREAMER_ENV_ONCE.call_once(|| {
        #[cfg(target_os = "macos")]
        prepare_macos_runtime_environment();
    });
}

#[cfg(target_os = "macos")]
fn prepare_macos_runtime_environment() {
    let prefixes = macos_gstreamer_prefixes();
    let lib_dirs = collect_existing_dirs(&prefixes, &["lib"]);
    let typelib_dirs = collect_existing_dirs(&prefixes, &["lib/girepository-1.0"]);
    let plugin_dirs = collect_existing_dirs(&prefixes, &["lib/gstreamer-1.0"]);
    let runtime_plugin_dirs = prepare_macos_plugin_dirs(&plugin_dirs).unwrap_or_else(|| {
        tracing::warn!(
            "gstreamer_runtime: nie mozna przygotowac filtrowanego katalogu pluginow macOS"
        );
        plugin_dirs.clone()
    });

    prepend_path_env("DYLD_FALLBACK_LIBRARY_PATH", &lib_dirs);
    prepend_path_env("GI_TYPELIB_PATH", &typelib_dirs);
    set_path_env("GST_PLUGIN_PATH_1_0", &runtime_plugin_dirs);
    set_path_env("GST_PLUGIN_SYSTEM_PATH_1_0", &runtime_plugin_dirs);
    set_macos_registry_path();

    if std::env::var_os("GST_PLUGIN_SCANNER").is_none() {
        if let Some(scanner) = find_macos_plugin_scanner(&prefixes) {
            std::env::set_var("GST_PLUGIN_SCANNER", scanner);
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_gstreamer_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    for p in [
        "/opt/homebrew",
        "/usr/local",
        "/Library/Frameworks/GStreamer.framework/Versions/1.0",
        "~/Library/Frameworks/GStreamer.framework/Versions/1.0",
    ] {
        let expanded = if let Some(rest) = p.strip_prefix("~/") {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(rest))
                .unwrap_or_else(|| PathBuf::from(p))
        } else {
            PathBuf::from(p)
        };
        if expanded.exists() {
            prefixes.push(expanded);
        }
    }
    prefixes
}

#[cfg(target_os = "macos")]
fn collect_existing_dirs(prefixes: &[PathBuf], suffixes: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for prefix in prefixes {
        for suffix in suffixes {
            let path = prefix.join(suffix);
            if path.is_dir() && !out.iter().any(|existing| existing == &path) {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn find_macos_plugin_scanner(prefixes: &[PathBuf]) -> Option<PathBuf> {
    for prefix in prefixes {
        for suffix in [
            "libexec/gstreamer-1.0/gst-plugin-scanner",
            "lib/gstreamer-1.0/gst-plugin-scanner",
        ] {
            let path = prefix.join(suffix);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    for cellar in [
        "/opt/homebrew/Cellar/gstreamer",
        "/usr/local/Cellar/gstreamer",
    ] {
        let root = Path::new(cellar);
        if !root.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        versions.reverse();
        for version in versions {
            let path = version.join("libexec/gstreamer-1.0/gst-plugin-scanner");
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn prepare_macos_plugin_dirs(plugin_dirs: &[PathBuf]) -> Option<Vec<PathBuf>> {
    if plugin_dirs.is_empty() {
        return None;
    }

    let filtered_dir = crate::paths::cache_dir()
        .join("gstreamer")
        .join("plugins-macos-no-gtk");
    if let Err(err) = std::fs::create_dir_all(&filtered_dir) {
        tracing::warn!(
            path = %filtered_dir.display(),
            error = %err,
            "gstreamer_runtime: tworzenie katalogu pluginow nie powiodlo sie"
        );
        return None;
    }

    let mut wanted = std::collections::HashSet::new();
    for dir in plugin_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let source = entry.path();
            if !source.is_file() || source.extension().and_then(|v| v.to_str()) != Some("dylib") {
                continue;
            }
            let Some(file_name) = source.file_name() else {
                continue;
            };
            let file_name_str = file_name.to_string_lossy().to_ascii_lowercase();
            if file_name_str.contains("gtk") {
                continue;
            }
            let target = filtered_dir.join(file_name);
            wanted.insert(target.clone());
            let should_link = match std::fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    match std::fs::read_link(&target) {
                        Ok(existing) if existing == source => false,
                        _ => {
                            let _ = std::fs::remove_file(&target);
                            true
                        }
                    }
                }
                Ok(_) => false,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                Err(err) => {
                    tracing::warn!(
                        target = %target.display(),
                        error = %err,
                        "gstreamer_runtime: odczyt linku pluginu nie powiodl sie"
                    );
                    false
                }
            };
            if !should_link {
                continue;
            }
            #[cfg(unix)]
            if let Err(err) = std::os::unix::fs::symlink(&source, &target) {
                tracing::warn!(
                    source = %source.display(),
                    target = %target.display(),
                    error = %err,
                    "gstreamer_runtime: tworzenie linku pluginu nie powiodlo sie"
                );
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&filtered_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if wanted.contains(&path) {
                continue;
            }
            if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    Some(vec![filtered_dir])
}

#[cfg(target_os = "macos")]
fn set_macos_registry_path() {
    let registry = crate::paths::cache_dir()
        .join("gstreamer")
        .join("registry-macos-no-gtk.bin");
    if let Some(parent) = registry.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::env::set_var("GST_REGISTRY", registry);
}

#[cfg(target_os = "macos")]
fn prepend_path_env(name: &str, dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }

    let separator = ":";
    let current = std::env::var_os(name)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut entries: Vec<String> = dirs
        .iter()
        .filter_map(|p| p.to_str().map(ToOwned::to_owned))
        .collect();
    for existing in current.split(separator).filter(|s| !s.is_empty()) {
        if !entries.iter().any(|entry| entry == existing) {
            entries.push(existing.to_string());
        }
    }
    std::env::set_var(name, entries.join(separator));
}

#[cfg(target_os = "macos")]
fn set_path_env(name: &str, dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }

    let value = dirs
        .iter()
        .filter_map(|p| p.to_str())
        .collect::<Vec<_>>()
        .join(":");
    if !value.is_empty() {
        std::env::set_var(name, value);
    }
}
