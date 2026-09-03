// =============================================================================
// File: receipt.rs — what the installer recorded about this installation
// =============================================================================
//
// `tentaflow status` and `tentaflow update` both need facts the binary cannot
// derive from itself: which edition and GPU variant were installed, where the
// prefix and config live, and whether the service runs in the system or the
// user systemd scope. The installer writes them once, here.
//
// Every consumer must work without the file too — a repo build, a tarball
// unpacked by hand and a `cargo run` all have no receipt, and refusing to
// report status on those would be worse than reporting less.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Written by `install.sh` next to the configuration. Field names are the wire
/// contract with the installer: renaming one silently degrades every installed
/// binary to the no-receipt path, so add fields, never rename them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallReceipt {
    /// Version installed, without the leading `v`.
    pub version: String,
    /// `full` or `slim` — decides which asset an update downloads.
    pub edition: String,
    /// GPU variant the installer picked or the user chose (`vulkan`, `cuda`,
    /// `metal`, or `none` for slim).
    pub variant: String,
    /// Rust target triple of the installed artifact.
    pub target: String,
    /// Install prefix; `<prefix>/current` is the symlink to the live version.
    pub prefix: PathBuf,
    /// Configuration file the service is started with.
    pub config: PathBuf,
    /// `TENTAFLOW_HOME` — data, TLS identity, SQLite.
    pub home: PathBuf,
    /// `system`, `user`, or `none` when autostart was declined.
    pub service_scope: String,
}

impl InstallReceipt {
    /// Paths the installer may have written to, most authoritative first: a
    /// system install owns /etc, a user install keeps everything under $HOME.
    fn candidates() -> Vec<PathBuf> {
        let mut out = vec![PathBuf::from("/etc/tentaflow/install-receipt.json")];
        if let Some(home) = std::env::var_os("HOME") {
            out.push(PathBuf::from(&home).join(".config/tentaflow/install-receipt.json"));
            out.push(
                PathBuf::from(&home)
                    .join(".local/share/tentaflow/install-receipt.json"),
            );
        }
        out
    }

    /// Reads the receipt for this installation, or `None` when there is none.
    /// A malformed file is treated as absent and reported, not silently
    /// ignored: a receipt the installer could not write correctly is a bug
    /// worth seeing rather than a missing feature.
    pub fn load() -> Option<Self> {
        for path in Self::candidates() {
            if !path.exists() {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|raw| serde_json::from_str::<Self>(&raw).map_err(|e| e.to_string()))
            {
                Ok(receipt) => return Some(receipt),
                Err(err) => {
                    eprintln!("malformed receipt {}: {err}", path.display());
                }
            }
        }
        None
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
