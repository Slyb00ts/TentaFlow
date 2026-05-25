// =============================================================================
// Plik: sync/storage_monitor.rs
// Opis: Monitor rozmiaru storage Sync Ledger, blobow i wolnego miejsca na dysku.
// =============================================================================

use std::path::{Path, PathBuf};

use crate::sync::ledger::{LedgerResult, SyncLedgerError};

pub const INFO_FREE_PERCENT: f64 = 20.0;
pub const WARNING_FREE_PERCENT: f64 = 10.0;
pub const CRITICAL_FREE_PERCENT: f64 = 5.0;
pub const LARGE_BLOB_BLOCK_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePressureLevel {
    Ok,
    Info,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePathUsage {
    pub label: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoragePressureReport {
    pub root: PathBuf,
    pub level: StoragePressureLevel,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub free_percent: Option<f64>,
    pub sqlite_bytes: u64,
    pub fjall_ledger_bytes: u64,
    pub snapshot_blob_bytes: u64,
    pub final_blob_bytes: u64,
    pub pending_blob_chunk_bytes: u64,
    pub paths: Vec<StoragePathUsage>,
}

impl StoragePressureReport {
    pub fn can_accept_large_blob(&self, size_bytes: u64) -> bool {
        self.level != StoragePressureLevel::Critical || size_bytes <= LARGE_BLOB_BLOCK_BYTES
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn current_report() -> LedgerResult<StoragePressureReport> {
    report_for_root(crate::paths::tentaflow_home())
}

pub fn report_for_root(root: &Path) -> LedgerResult<StoragePressureReport> {
    let disk = find_disk_space(root);
    report_for_root_with_disk(root, disk)
}

pub fn ensure_large_blob_allowed(size_bytes: u64) -> LedgerResult<()> {
    let report = current_report()?;
    if report.can_accept_large_blob(size_bytes) {
        return Ok(());
    }
    Err(SyncLedgerError::Runtime(format!(
        "storage pressure critical: free={:?}%, blob_size={} bytes",
        report.free_percent, size_bytes
    )))
}

fn report_for_root_with_disk(
    root: &Path,
    disk: Option<DiskSpace>,
) -> LedgerResult<StoragePressureReport> {
    let root = root.to_path_buf();
    let sqlite_path = root.join("data").join("router.db");
    let fjall_ledger_path = root.join("sync").join("ledger");
    let snapshot_blobs_path = root.join("sync").join("snapshot-blobs");
    let final_blobs_path = root.join("blobs");
    let pending_chunks_path = root.join("sync").join("blob-chunks");

    let sqlite_bytes = path_size(&sqlite_path)?;
    let fjall_ledger_bytes = path_size(&fjall_ledger_path)?;
    let snapshot_blob_bytes = path_size(&snapshot_blobs_path)?;
    let final_blob_bytes = path_size(&final_blobs_path)?;
    let pending_blob_chunk_bytes = path_size(&pending_chunks_path)?;
    let (total_bytes, available_bytes, free_percent, level) = match disk {
        Some(disk) if disk.total_bytes > 0 => {
            let free_percent = (disk.available_bytes as f64 / disk.total_bytes as f64) * 100.0;
            (
                Some(disk.total_bytes),
                Some(disk.available_bytes),
                Some(free_percent),
                pressure_level(free_percent),
            )
        }
        _ => (None, None, None, StoragePressureLevel::Unknown),
    };

    Ok(StoragePressureReport {
        root,
        level,
        total_bytes,
        available_bytes,
        free_percent,
        sqlite_bytes,
        fjall_ledger_bytes,
        snapshot_blob_bytes,
        final_blob_bytes,
        pending_blob_chunk_bytes,
        paths: vec![
            StoragePathUsage {
                label: "sqlite",
                path: sqlite_path,
                bytes: sqlite_bytes,
            },
            StoragePathUsage {
                label: "fjall_ledger",
                path: fjall_ledger_path,
                bytes: fjall_ledger_bytes,
            },
            StoragePathUsage {
                label: "snapshot_blobs",
                path: snapshot_blobs_path,
                bytes: snapshot_blob_bytes,
            },
            StoragePathUsage {
                label: "final_blobs",
                path: final_blobs_path,
                bytes: final_blob_bytes,
            },
            StoragePathUsage {
                label: "pending_blob_chunks",
                path: pending_chunks_path,
                bytes: pending_blob_chunk_bytes,
            },
        ],
    })
}

fn pressure_level(free_percent: f64) -> StoragePressureLevel {
    if free_percent <= CRITICAL_FREE_PERCENT {
        StoragePressureLevel::Critical
    } else if free_percent <= WARNING_FREE_PERCENT {
        StoragePressureLevel::Warning
    } else if free_percent <= INFO_FREE_PERCENT {
        StoragePressureLevel::Info
    } else {
        StoragePressureLevel::Ok
    }
}

fn path_size(path: &Path) -> LedgerResult<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(SyncLedgerError::Runtime(format!("storage stat: {e}"))),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)
        .map_err(|e| SyncLedgerError::Runtime(format!("storage read_dir: {e}")))?
    {
        let entry = entry.map_err(|e| SyncLedgerError::Runtime(format!("storage entry: {e}")))?;
        total = total.saturating_add(path_size(&entry.path())?);
    }
    Ok(total)
}

fn find_disk_space(root: &Path) -> Option<DiskSpace> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<DiskSpace> = None;
    let mut best_len = 0usize;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if root.starts_with(mount) {
            let mount_len = mount.components().count();
            if mount_len >= best_len {
                best_len = mount_len;
                best = Some(DiskSpace {
                    total_bytes: disk.total_space(),
                    available_bytes: disk.available_space(),
                });
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_level_thresholds() {
        assert_eq!(pressure_level(25.0), StoragePressureLevel::Ok);
        assert_eq!(pressure_level(20.0), StoragePressureLevel::Info);
        assert_eq!(pressure_level(10.0), StoragePressureLevel::Warning);
        assert_eq!(pressure_level(5.0), StoragePressureLevel::Critical);
    }

    #[test]
    fn report_counts_storage_paths() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("data")).expect("data dir");
        std::fs::create_dir_all(root.join("sync/ledger/a")).expect("ledger dir");
        std::fs::create_dir_all(root.join("sync/snapshot-blobs")).expect("snapshot dir");
        std::fs::create_dir_all(root.join("sync/blob-chunks")).expect("chunks dir");
        std::fs::create_dir_all(root.join("blobs")).expect("blobs dir");
        std::fs::write(root.join("data/router.db"), vec![1u8; 7]).expect("sqlite");
        std::fs::write(root.join("sync/ledger/a/log"), vec![1u8; 11]).expect("ledger");
        std::fs::write(root.join("sync/snapshot-blobs/snap"), vec![1u8; 13]).expect("snapshot");
        std::fs::write(root.join("sync/blob-chunks/chunk"), vec![1u8; 17]).expect("chunk");
        std::fs::write(root.join("blobs/blob"), vec![1u8; 19]).expect("blob");

        let report = report_for_root_with_disk(
            root,
            Some(DiskSpace {
                total_bytes: 100,
                available_bytes: 9,
            }),
        )
        .expect("report");

        assert_eq!(report.level, StoragePressureLevel::Warning);
        assert_eq!(report.sqlite_bytes, 7);
        assert_eq!(report.fjall_ledger_bytes, 11);
        assert_eq!(report.snapshot_blob_bytes, 13);
        assert_eq!(report.pending_blob_chunk_bytes, 17);
        assert_eq!(report.final_blob_bytes, 19);
    }

    #[test]
    fn critical_blocks_only_large_blobs() {
        let report = StoragePressureReport {
            root: PathBuf::from("/tmp/test"),
            level: StoragePressureLevel::Critical,
            total_bytes: Some(100),
            available_bytes: Some(4),
            free_percent: Some(4.0),
            sqlite_bytes: 0,
            fjall_ledger_bytes: 0,
            snapshot_blob_bytes: 0,
            final_blob_bytes: 0,
            pending_blob_chunk_bytes: 0,
            paths: Vec::new(),
        };

        assert!(report.can_accept_large_blob(LARGE_BLOB_BLOCK_BYTES));
        assert!(!report.can_accept_large_blob(LARGE_BLOB_BLOCK_BYTES + 1));
    }
}
