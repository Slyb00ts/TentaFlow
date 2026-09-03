// ===== File: code_studio/fs/unix.rs — handle-based filesystem primitives for unix =====
//
// Every primitive here takes a directory HANDLE, never a path string. That is
// the whole point: a path string is re-resolved by the kernel on each call, so
// an agent that swaps a directory for a symlink between two calls moves the
// second call somewhere else. A file descriptor names the inode that was
// verified, and `*at()` syscalls resolve the final component against it.
//
// Path resolution walks segment by segment with `O_NOFOLLOW | O_DIRECTORY`.
// Refusing symlinks at every level is what makes `RESOLVE_BENEATH`-style
// containment hold on kernels and systems that have no such flag (macOS, BSD,
// and Linux without `openat2` — see `linux.rs`): a segment can only be a real
// directory that lives directly inside the previously verified one, and `..` is
// already gone lexically, so there is no edge that leaves the root.
//
// The `st_dev`/`st_ino` comparison around each open closes the remaining race:
// we stat the name without following, open it, then stat the OPEN handle. If
// the name was re-pointed at another directory in between, the two identities
// differ and the open is refused instead of silently continuing somewhere else.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::path::Path;

/// Modes this layer creates with — the same ones `git checkout` produces. The
/// session worktree itself is `0o700` (see `code_studio::paths`), so the group
/// and other bits never widen access beyond the account the owner node runs as,
/// and a sandbox that mounts the worktree still sees the permissions a build
/// expects instead of a tree it cannot traverse.
const FILE_MODE: libc::mode_t = 0o644;
const DIRECTORY_MODE: libc::mode_t = 0o755;

/// An open directory. Cloning duplicates the descriptor, so a clone keeps
/// pointing at the same inode even if the name it was opened under is replaced.
#[derive(Debug)]
pub(super) struct DirHandle {
    fd: OwnedFd,
    dev: u64,
    ino: u64,
}

impl DirHandle {
    pub(super) fn try_clone(&self) -> io::Result<DirHandle> {
        Ok(DirHandle {
            fd: self.fd.try_clone()?,
            dev: self.dev,
            ino: self.ino,
        })
    }

    pub(super) fn raw_fd(&self) -> libc::c_int {
        self.fd.as_raw_fd()
    }

    /// Wraps a descriptor the caller already opened as a directory, recording
    /// its identity so later comparisons have something to compare against.
    pub(super) fn from_owned_fd(fd: OwnedFd) -> io::Result<DirHandle> {
        let st = fstat(fd.as_raw_fd())?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        Ok(DirHandle {
            fd,
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        })
    }
}

/// One entry of a directory listing. Names that are not valid UTF-8 are dropped
/// by `read_dir`, because the wire contract carries paths as strings and a name
/// we cannot round-trip is a name we cannot safely hand back to a caller.
pub(super) struct RawEntry {
    pub(super) name: String,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

/// Metadata taken WITHOUT following symlinks, so `is_symlink` describes the
/// entry itself rather than whatever it points at.
pub(super) struct RawStat {
    pub(super) is_dir: bool,
    pub(super) is_file: bool,
    pub(super) is_symlink: bool,
    pub(super) size: u64,
    pub(super) modified_unix_ms: Option<i64>,
    pub(super) readonly: bool,
}

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains a NUL byte",
        )
    })
}

fn fstat(fd: libc::c_int) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn fstatat_nofollow(dir: &DirHandle, name: &CStr) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn to_raw_stat(st: &libc::stat) -> RawStat {
    let fmt = st.st_mode & libc::S_IFMT;
    let modified_unix_ms = (st.st_mtime as i64)
        .checked_mul(1000)
        .map(|ms| ms + (st.st_mtime_nsec as i64) / 1_000_000);
    RawStat {
        is_dir: fmt == libc::S_IFDIR,
        is_file: fmt == libc::S_IFREG,
        is_symlink: fmt == libc::S_IFLNK,
        size: st.st_size.max(0) as u64,
        modified_unix_ms,
        readonly: st.st_mode & 0o200 == 0,
    }
}

/// Opens the session root. This is the ONE place a path string is resolved by
/// the kernel in the usual way, and it is safe because the path comes from
/// `code_studio::paths`, never from a request — on macOS the data directory is
/// commonly reached through `/var -> /private/var`, so refusing symlinks here
/// would refuse every session.
pub(super) fn open_root(path: &Path) -> io::Result<DirHandle> {
    let c = cstring(path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    DirHandle::from_owned_fd(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens one child directory of `parent`, refusing anything that is not a real
/// directory living directly inside it.
pub(super) fn open_child_dir(parent: &DirHandle, name: &str) -> io::Result<DirHandle> {
    let c = cstring(name.as_bytes())?;
    let before = fstatat_nofollow(parent, &c)?;
    let fmt = before.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFLNK {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing a symlinked path segment",
        ));
    }
    if fmt != libc::S_IFDIR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path segment is not a directory",
        ));
    }

    let fd = unsafe {
        libc::openat(
            parent.raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let handle = DirHandle::from_owned_fd(unsafe { OwnedFd::from_raw_fd(fd) })?;
    if handle.dev != before.st_dev as u64 || handle.ino != before.st_ino as u64 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory changed identity between check and open",
        ));
    }
    Ok(handle)
}

/// Walks `components` one segment at a time. `linux.rs` uses this as the
/// fallback when the kernel has no `openat2`; every other unix uses it always.
pub(super) fn resolve_segments(root: &DirHandle, components: &[String]) -> io::Result<DirHandle> {
    let mut current = root.try_clone()?;
    for component in components {
        current = open_child_dir(&current, component)?;
    }
    Ok(current)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn resolve_dir(root: &DirHandle, components: &[String]) -> io::Result<DirHandle> {
    resolve_segments(root, components)
}

pub(super) fn stat_at(dir: &DirHandle, name: &str) -> io::Result<RawStat> {
    let c = cstring(name.as_bytes())?;
    Ok(to_raw_stat(&fstatat_nofollow(dir, &c)?))
}

pub(super) fn stat_handle(dir: &DirHandle) -> io::Result<RawStat> {
    Ok(to_raw_stat(&fstat(dir.raw_fd())?))
}

pub(super) fn open_file(dir: &DirHandle, name: &str) -> io::Result<File> {
    let c = cstring(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            dir.raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// `O_CREAT | O_EXCL | O_NOFOLLOW`: the name is claimed atomically or not at
/// all, and an existing symlink is never written through.
pub(super) fn create_exclusive(dir: &DirHandle, name: &str) -> io::Result<File> {
    let c = cstring(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            dir.raw_fd(),
            c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            FILE_MODE as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn mkdir_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    let c = cstring(name.as_bytes())?;
    if unsafe { libc::mkdirat(dir.raw_fd(), c.as_ptr(), DIRECTORY_MODE) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn unlink_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    let c = cstring(name.as_bytes())?;
    if unsafe { libc::unlinkat(dir.raw_fd(), c.as_ptr(), 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn rmdir_at(dir: &DirHandle, name: &str) -> io::Result<()> {
    let c = cstring(name.as_bytes())?;
    if unsafe { libc::unlinkat(dir.raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn rename_at(
    from_dir: &DirHandle,
    from_name: &str,
    to_dir: &DirHandle,
    to_name: &str,
) -> io::Result<()> {
    let from = cstring(from_name.as_bytes())?;
    let to = cstring(to_name.as_bytes())?;
    let rc = unsafe {
        libc::renameat(
            from_dir.raw_fd(),
            from.as_ptr(),
            to_dir.raw_fd(),
            to.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Flushes the directory entry itself, so a rename that completed is still
/// there after a crash. Best effort by design: filesystems that do not support
/// it report `EINVAL` and the data is already durable through `sync_all`.
pub(super) fn sync_dir(dir: &DirHandle) {
    unsafe { libc::fsync(dir.raw_fd()) };
}

/// Address of `errno`. `readdir` signals both "end of directory" and "error"
/// with a NULL return, so the only way to tell them apart is to clear `errno`
/// first and read it back — there is no portable accessor in `libc`.
fn errno_ptr() -> *mut libc::c_int {
    // Bionic is not glibc here: Android exports `__errno`, not
    // `__errno_location`, so grouping it with linux fails to link the mobile
    // build (E0425 at compile time — the symbol is not even declared).
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::__errno_location() }
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    {
        unsafe { libc::__error() }
    }
    #[cfg(any(
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        unsafe { libc::__errno() }
    }
}

/// Lists `dir` through its descriptor. `fdopendir` takes ownership of the
/// descriptor it is given, so it gets a duplicate and the caller's handle stays
/// usable for the `*at()` calls that follow the listing.
pub(super) fn read_dir(dir: &DirHandle) -> io::Result<Vec<RawEntry>> {
    let duplicate = dir.fd.try_clone()?.into_raw_fd();
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let err = io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(err);
    }
    unsafe { libc::rewinddir(stream) };

    let mut entries = Vec::new();
    let mut failure = None;
    loop {
        unsafe { *errno_ptr() = 0 };
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let code = unsafe { *errno_ptr() };
            if code != 0 {
                failure = Some(io::Error::from_raw_os_error(code));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else { continue };
        if name == "." || name == ".." {
            continue;
        }
        let name = name.to_string();
        let (is_dir, is_symlink) = match unsafe { (*entry).d_type } {
            libc::DT_DIR => (true, false),
            libc::DT_LNK => (false, true),
            libc::DT_UNKNOWN => match stat_at(dir, &name) {
                Ok(st) => (st.is_dir, st.is_symlink),
                Err(_) => continue,
            },
            _ => (false, false),
        };
        entries.push(RawEntry {
            name,
            is_dir,
            is_symlink,
        });
    }
    unsafe { libc::closedir(stream) };

    match failure {
        Some(err) => Err(err),
        None => Ok(entries),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symlinked_segment_is_refused_even_when_it_points_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/file.txt"), b"data").unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();

        let root = open_root(dir.path()).unwrap();
        assert!(open_child_dir(&root, "real").is_ok());
        let err = open_child_dir(&root, "link").expect_err("a symlink was followed");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_symlink_pointing_outside_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let root = open_root(dir.path()).unwrap();
        assert!(resolve_segments(&root, &["escape".to_string()]).is_err());
    }

    #[test]
    fn a_handle_keeps_working_on_the_inode_it_verified_after_the_name_is_swapped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        std::fs::write(dir.path().join("work/keep.txt"), b"original").unwrap();

        let root = open_root(dir.path()).unwrap();
        let work = open_child_dir(&root, "work").unwrap();

        // Swap the name for a symlink aiming somewhere else entirely.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), b"attacker").unwrap();
        std::fs::remove_dir_all(dir.path().join("work")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("work")).unwrap();

        // The old handle still names the old (now unlinked) directory, so the
        // attacker's file is unreachable through it, and re-resolving the name
        // is refused outright.
        assert!(open_file(&work, "keep.txt").is_err());
        assert!(open_child_dir(&root, "work").is_err());
    }

    #[test]
    fn listing_reports_entry_kinds_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("f.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("sub", dir.path().join("l")).unwrap();

        let root = open_root(dir.path()).unwrap();
        let mut entries = read_dir(&root).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["f.txt", "l", "sub"]);
        assert!(entries[1].is_symlink, "symlink reported as a directory");
        assert!(entries[2].is_dir);
    }

    #[test]
    fn creating_an_existing_name_fails_instead_of_truncating_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(dir.path()).unwrap();
        create_exclusive(&root, "once.txt").unwrap();
        let err =
            create_exclusive(&root, "once.txt").expect_err("exclusive create succeeded twice");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
}
