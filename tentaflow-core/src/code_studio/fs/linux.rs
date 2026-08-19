// ===== File: code_studio/fs/linux.rs — openat2 containment for the session root =====
//
// `openat2` is the only interface that makes containment a KERNEL property
// rather than a property of our loop: `RESOLVE_BENEATH` refuses any resolution
// step that would leave the directory the descriptor names, `RESOLVE_NO_SYMLINKS`
// refuses every symlink (including a relative one that would land back inside),
// and `RESOLVE_NO_MAGICLINKS` refuses `/proc/*/fd/*`-style links, which are the
// classic way out of an otherwise contained tree. The whole relative path is
// resolved in one syscall, so there is no window between the segments at all.
//
// The syscall landed in Linux 5.6 and is frequently filtered by container
// seccomp profiles, so an `ENOSYS`/`EPERM` answer means "this kernel will never
// serve us" and we latch it once and walk segments with `openat` +
// `O_NOFOLLOW | O_DIRECTORY` instead (`unix.rs`). That fallback is not a weaker
// promise — it refuses the same edges — it is only more syscalls and it has to
// verify inode identity by hand.
//
// Any other error is a real answer about this path (`EXDEV` = the path tried to
// escape, `ELOOP` = a symlink was in the way) and is returned as such.

use std::ffi::CString;
use std::io;
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use super::unix::{resolve_segments, DirHandle};

/// Latched once the kernel tells us `openat2` is unavailable. Only ever moves
/// from false to true, so a racing reader either takes one more failing syscall
/// or skips straight to the fallback — both correct.
static OPENAT2_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

fn openat2_beneath(root: &DirHandle, relative: &str) -> io::Result<DirHandle> {
    let path = CString::new(relative)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;

    // `open_how` is `#[non_exhaustive]`, so it is zeroed and filled field by
    // field; the zeroed tail is exactly what the kernel expects for the fields
    // this build does not know about.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
    how.mode = 0;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;

    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.raw_fd(),
            path.as_ptr(),
            &mut how as *mut libc::open_how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    DirHandle::from_owned_fd(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// `ENOSYS` is a kernel older than 5.6; `EPERM` is what a seccomp profile
/// returns for a syscall it does not allow. Neither says anything about the
/// path, so both mean "use the per-segment walk from now on".
fn means_unavailable(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::ENOSYS) | Some(libc::EPERM))
}

pub(super) fn resolve_dir(root: &DirHandle, components: &[String]) -> io::Result<DirHandle> {
    if components.is_empty() {
        return root.try_clone();
    }
    if !OPENAT2_UNAVAILABLE.load(Ordering::Relaxed) {
        match openat2_beneath(root, &components.join("/")) {
            Ok(handle) => return Ok(handle),
            Err(err) if means_unavailable(&err) => {
                OPENAT2_UNAVAILABLE.store(true, Ordering::Relaxed);
            }
            Err(err) => return Err(err),
        }
    }
    resolve_segments(root, components)
}

#[cfg(test)]
mod tests {
    use super::super::unix::{open_root, read_dir};
    use super::*;

    #[test]
    fn a_relative_escape_is_refused_by_the_kernel_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let root = open_root(dir.path()).unwrap();

        // `..` never reaches this layer through `RelPath`, but the kernel flag
        // is the second, independent refusal the plan asks for (§8.1). On a
        // kernel without `openat2` the same call fails with ENOSYS, which is
        // also a refusal, so the assertion holds either way.
        assert!(openat2_beneath(&root, "a/../..").is_err());
        assert!(resolve_dir(&root, &["a".to_string(), "b".to_string()]).is_ok());
    }

    #[test]
    fn a_symlinked_segment_is_refused_on_both_the_fast_path_and_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        let root = open_root(dir.path()).unwrap();

        assert!(resolve_dir(&root, &["link".to_string()]).is_err());
        assert!(resolve_segments(&root, &["link".to_string()]).is_err());
    }

    #[test]
    fn the_fast_path_and_the_fallback_reach_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("a/b/c/marker.txt"), b"here").unwrap();
        let root = open_root(dir.path()).unwrap();

        let components = ["a".to_string(), "b".to_string(), "c".to_string()];
        for handle in [
            resolve_dir(&root, &components).unwrap(),
            resolve_segments(&root, &components).unwrap(),
        ] {
            let names: Vec<String> = read_dir(&handle)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect();
            assert_eq!(names, vec!["marker.txt".to_string()]);
        }
    }
}
