// ===== File: code_studio/fs/windows.rs — reparse-point and final-path containment =====
//
// Win32 has no `*at()` family, so the unix trick of resolving every segment
// against a descriptor is not available. The plan's Windows mechanism (§8) is
// built from the three primitives the platform does offer:
//
//   * `FILE_FLAG_OPEN_REPARSE_POINT` opens the reparse point ITSELF instead of
//     following it, and any handle whose attributes carry
//     `FILE_ATTRIBUTE_REPARSE_POINT` is refused. Junctions, directory symlinks,
//     file symlinks and mount points are all reparse points, so one check
//     covers every redirection Windows has.
//   * `GetFinalPathNameByHandle` answers "where did this HANDLE actually land",
//     which is a property of the opened object rather than of the string we
//     asked for. Each opened child must land exactly one component below its
//     parent's final path; anything deeper means a redirection happened.
//   * Directory handles are opened WITHOUT `FILE_SHARE_DELETE`, so while this
//     layer holds a handle on a directory, nobody can rename or delete it. That
//     pins the path a subsequent open resolves through and is what closes the
//     check-then-open window that unix closes with `st_dev`/`st_ino`.
//
// Name rules are NOT here. Windows resolves several name shapes to something
// other than a file in the current directory — alternate data streams
// (`file:stream`), reserved device names (`CON`, `COM1`, …), UNC paths, the
// `\\?\` prefix, names whose trailing dot or space it strips — and every one of
// those is refused by `fs/mod.rs::validate_component` on EVERY platform, so a
// tree written on Linux already carries names that keep their meaning here.
// This module is only the handle mechanics.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    GetFinalPathNameByHandleW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, VOLUME_NAME_DOS,
};

/// An open directory plus the path this layer will compose child paths from and
/// the final path the handle actually resolved to. Both are kept: the first is
/// what Win32 needs to open anything, the second is what every containment
/// check compares against.
#[derive(Debug)]
pub(super) struct DirHandle {
    handle: File,
    path: PathBuf,
    final_path: String,
}

impl DirHandle {
    pub(super) fn try_clone(&self) -> io::Result<DirHandle> {
        Ok(DirHandle {
            handle: self.handle.try_clone()?,
            path: self.path.clone(),
            final_path: self.final_path.clone(),
        })
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

pub(super) struct RawEntry {
    pub(super) name: String,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

pub(super) struct RawStat {
    pub(super) is_dir: bool,
    pub(super) is_file: bool,
    pub(super) is_symlink: bool,
    pub(super) size: u64,
    pub(super) modified_unix_ms: Option<i64>,
    pub(super) readonly: bool,
}

/// Windows FILETIME counts 100-nanosecond ticks from 1601-01-01; unix time
/// starts 11644473600 seconds later.
const FILETIME_UNIX_EPOCH_MS: i64 = 11_644_473_600_000;

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.to_string())
}

/// Where the handle really landed, normalized and expressed as a DOS path
/// (`\\?\C:\...`). This is the containment oracle: it describes the object, not
/// the string that was used to reach it.
fn final_path(file: &File) -> io::Result<String> {
    let handle = file.as_raw_handle() as HANDLE;
    let mut buffer = vec![0u16; 512];
    loop {
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        let written = written as usize;
        if written < buffer.len() {
            buffer.truncate(written);
            return Ok(String::from_utf16_lossy(&buffer));
        }
        buffer.resize(written + 1, 0);
    }
}

/// The child handle must have landed exactly one component below the parent.
/// A junction or symlink would land somewhere else entirely, and a reparse
/// point pointing at a deeper path inside the same parent would show up as a
/// tail containing a separator.
fn verify_contained(parent: &DirHandle, name: &str, child: &File) -> io::Result<String> {
    let landed = final_path(child)?;
    let expected_prefix = format!("{}\\", parent.final_path);

    // Compared character by character rather than by byte slice: Windows is
    // case-insensitive, and lowercasing a non-ASCII path can change its byte
    // length, which would make a byte offset land mid-character.
    let landed_chars: Vec<char> = landed.chars().collect();
    let prefix_chars: Vec<char> = expected_prefix.chars().collect();
    if landed_chars.len() <= prefix_chars.len() {
        return Err(denied("resolved outside the session root"));
    }
    for (found, expected) in landed_chars.iter().zip(prefix_chars.iter()) {
        if !found.to_lowercase().eq(expected.to_lowercase()) {
            return Err(denied("resolved outside the session root"));
        }
    }

    let tail: String = landed_chars[prefix_chars.len()..].iter().collect();
    if tail.contains('\\') {
        return Err(denied("resolved through a redirection"));
    }
    if tail.to_lowercase() != name.to_lowercase() {
        return Err(denied("resolved to a different name"));
    }
    Ok(landed)
}

fn ensure_no_reparse(attributes: u32) -> io::Result<()> {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("refusing a reparse point"));
    }
    Ok(())
}

fn open_directory(path: &Path, follow_reparse: bool) -> io::Result<File> {
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow_reparse {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        // No FILE_SHARE_DELETE: while this handle lives, the directory cannot
        // be renamed or deleted, which pins every path composed from it.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(flags)
        .open(path)
}

/// Opens the session root. Like the unix counterpart this is the one path that
/// may legitimately sit behind a redirection (an administrator can host the
/// data directory on a junction), and it comes from `code_studio::paths` rather
/// than from a request, so reparse points are followed exactly here and nowhere
/// else.
pub(super) fn open_root(path: &Path) -> io::Result<DirHandle> {
    let handle = open_directory(path, true)?;
    let metadata = handle.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    let final_path = final_path(&handle)?;
    Ok(DirHandle {
        path: PathBuf::from(&final_path),
        handle,
        final_path,
    })
}

pub(super) fn open_child_dir(parent: &DirHandle, name: &str) -> io::Result<DirHandle> {
    let path = parent.child(name);
    let handle = open_directory(&path, false)?;
    let metadata = handle.metadata()?;
    ensure_no_reparse(metadata.file_attributes())?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path segment is not a directory",
        ));
    }
    let final_path = verify_contained(parent, name, &handle)?;
    Ok(DirHandle {
        path: PathBuf::from(&final_path),
        handle,
        final_path,
    })
}

pub(super) fn resolve_dir(root: &DirHandle, components: &[String]) -> io::Result<DirHandle> {
    let mut current = root.try_clone()?;
    for component in components {
        current = open_child_dir(&current, component)?;
    }
    Ok(current)
}

fn raw_stat_from(metadata: &std::fs::Metadata) -> RawStat {
    let attributes = metadata.file_attributes();
    let modified_unix_ms = (metadata.last_write_time() / 10_000) as i64 - FILETIME_UNIX_EPOCH_MS;
    RawStat {
        is_dir: attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        is_file: attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        is_symlink: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        size: metadata.file_size(),
        modified_unix_ms: Some(modified_unix_ms),
        readonly: metadata.permissions().readonly(),
    }
}

/// Metadata read from a HANDLE opened with `FILE_READ_ATTRIBUTES` only, so it
/// works on entries the caller may not read, and without following reparse
/// points, so `is_symlink` describes the entry itself.
pub(super) fn stat_at(dir: &DirHandle, name: &str) -> io::Result<RawStat> {
    let handle = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir.child(name))?;
    let metadata = handle.metadata()?;
    let stat = raw_stat_from(&metadata);
    // A reparse point is reported, never traversed: `verify_contained` would
    // reject it and the shared layer refuses to read or write through it.
    if !stat.is_symlink {
        verify_contained(dir, name, &handle)?;
    }
    Ok(stat)
}

pub(super) fn stat_handle(dir: &DirHandle) -> io::Result<RawStat> {
    Ok(raw_stat_from(&dir.handle.metadata()?))
}

pub(super) fn open_file(dir: &DirHandle, name: &str) -> io::Result<File> {
    let handle = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir.child(name))?;
    ensure_no_reparse(handle.metadata()?.file_attributes())?;
    verify_contained(dir, name, &handle)?;
    Ok(handle)
}

/// `create_new` maps to `CREATE_NEW`, which claims the name atomically and
/// fails if anything is already there — including a dangling reparse point,
/// which `FILE_FLAG_OPEN_REPARSE_POINT` keeps us from writing through.
pub(super) fn create_exclusive(dir: &DirHandle, name: &str) -> io::Result<File> {
    let handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir.child(name))?;
    verify_contained(dir, name, &handle)?;
    Ok(handle)
}

pub(super) fn mkdir_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    std::fs::create_dir(dir.child(name))
}

pub(super) fn unlink_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    std::fs::remove_file(dir.child(name))
}

pub(super) fn rmdir_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    std::fs::remove_dir(dir.child(name))
}

pub(super) fn rename_at(
    from_dir: &DirHandle,
    from_name: &str,
    to_dir: &DirHandle,
    to_name: &str,
) -> io::Result<()> {
    std::fs::rename(from_dir.child(from_name), to_dir.child(to_name))
}

/// Win32 has no directory flush; file contents are already durable through
/// `sync_all` before the rename, and NTFS journals the rename itself.
pub(super) fn sync_dir(_dir: &DirHandle) {}

pub(super) fn read_dir(dir: &DirHandle) -> io::Result<Vec<RawEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir.path)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let attributes = metadata.file_attributes();
        entries.push(RawEntry {
            name,
            is_dir: attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            is_symlink: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_junction_inside_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        let link = dir.path().join("link");
        if std::os::windows::fs::symlink_dir(dir.path().join("real"), &link).is_err() {
            eprintln!("skipping: creating a directory symlink needs developer mode or SeCreateSymbolicLinkPrivilege");
            return;
        }
        let root = open_root(dir.path()).unwrap();
        assert!(open_child_dir(&root, "real").is_ok());
        assert!(
            open_child_dir(&root, "link").is_err(),
            "a directory symlink was traversed"
        );
    }
}
