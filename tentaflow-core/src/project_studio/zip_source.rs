// ===== File: project_studio/zip_source.rs — ZIP-archive knowledge sources (F3) =====
//
// Extracts an uploaded archive into the source's working tree
// (`<cache>/project-studio/<project_id>/sources/<source_id>/`) so the same
// `ingest::collect_tree_files` walker that serves git sources can index it and
// `build_profiles::detect_toolchain` can read its manifests.
//
// Zip-bomb containment mirrors `ml_studio::project_archive`: entry count cap,
// total uncompressed-byte budget enforced WHILE writing (a lying central
// directory cannot buy extra bytes), `enclosed_name` path containment and a
// hard refusal of symlink entries (their target would let a later entry write
// through them outside the staging tree).

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

/// Entry cap of one archive.
pub const MAX_ZIP_ENTRIES: usize = 50_000;
/// Total uncompressed-byte budget of one archive.
pub const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Per-entry uncompressed cap.
pub const MAX_ZIP_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
/// Copy buffer — constant RAM regardless of entry size.
const COPY_BUF: usize = 256 * 1024;

/// Extracts `archive_path` into the source's working tree, replacing any
/// previous content. Returns the tree root. Blocking — call from
/// `spawn_blocking`.
pub fn extract(project_id: &str, source_id: &str, archive_path: &Path) -> Result<PathBuf> {
    let dir = super::git_source::source_dir(project_id, source_id)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| anyhow!("cannot read the uploaded archive: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| anyhow!("uploaded file is not a valid ZIP archive: {e}"))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        bail!(
            "archive has {} entries — the limit is {MAX_ZIP_ENTRIES}",
            archive.len()
        );
    }

    let mut written: u64 = 0;
    let mut buf = vec![0u8; COPY_BUF];
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if let Some(mode) = entry.unix_mode() {
            // 0xA000 = S_IFLNK.
            if mode & 0xF000 == 0xA000 {
                bail!("archive contains a symbolic link: {}", entry.name());
            }
        }
        let Some(rel) = entry.enclosed_name() else {
            bail!("unsafe path in the archive: {}", entry.name());
        };
        if rel.components().count() == 0 {
            continue;
        }
        let out_path = dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if entry.size() > MAX_ZIP_ENTRY_BYTES {
            bail!(
                "archive entry {} exceeds the {MAX_ZIP_ENTRY_BYTES} byte limit",
                rel.display()
            );
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > MAX_ZIP_BYTES {
                bail!("extracted data exceeded the {MAX_ZIP_BYTES} byte limit");
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
        }
        std::io::Write::flush(&mut out)?;
    }

    // A single top-level directory (the usual `repo-main/` GitHub export) is
    // unwrapped so paths in the knowledge base match the repository layout.
    Ok(unwrap_single_root(&dir))
}

fn unwrap_single_root(dir: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return dir.to_path_buf();
    };
    let mut only: Option<PathBuf> = None;
    for entry in entries.flatten() {
        if only.is_some() {
            return dir.to_path_buf();
        }
        let path = entry.path();
        if !path.is_dir() {
            return dir.to_path_buf();
        }
        only = Some(path);
    }
    only.unwrap_or_else(|| dir.to_path_buf())
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use std::io::Write;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create zip");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("start file");
            writer.write_all(bytes).expect("write");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn extract_unwraps_single_root_and_contains_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let zip_path = tmp.path().join("src.zip");
        write_zip(
            &zip_path,
            &[
                ("repo-main/src/lib.rs", b"fn main() {}"),
                ("repo-main/README.md", b"# demo"),
            ],
        );
        let project_id = format!("{}", uuid::Uuid::new_v4());
        let source_id = uuid::Uuid::new_v4().to_string();
        let root = extract(&project_id, &source_id, &zip_path).expect("extract");
        assert!(root.join("src/lib.rs").is_file());
        assert!(root.join("README.md").is_file());
        super::super::git_source::remove_source_dir(&project_id, &source_id);
    }

    #[test]
    fn extract_rejects_traversal_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let zip_path = tmp.path().join("evil.zip");
        write_zip(&zip_path, &[("../../etc/passwd", b"root")]);
        let project_id = format!("{}", uuid::Uuid::new_v4());
        let source_id = uuid::Uuid::new_v4().to_string();
        assert!(extract(&project_id, &source_id, &zip_path).is_err());
        super::super::git_source::remove_source_dir(&project_id, &source_id);
    }
}
