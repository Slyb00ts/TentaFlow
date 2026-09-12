// =============================================================================
// Plik: tentanas-helper/src/elastic_transfer.rs
// Opis: FD-relative transfer plików cache z weryfikacją tożsamości i wznowieniem.
// Przykład: service przekazuje rekordy journalu oraz callback Root.save.
// =============================================================================

//! Moduł nie zarządza journalem, blokadami ani trybem service. Każdy etap jest
//! najpierw przekazywany callbackowi, a dopiero potem wykonywany przez syscall.
//!
//! The journal holds at most ONE record: the file in flight. The cache walk is
//! repeated on every invocation instead of being stored, because a moved file
//! has left the cache and an unmoved one is still there to be found again, so
//! the durable state does not grow with the number of files.

use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransferFilePhase {
    CopyIntent,
    CopyConfirmed,
    RenameIntent,
    RenameConfirmed,
    UnlinkIntent,
    UnlinkConfirmed,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferAttribute {
    pub name: String,
    /// Hex in the journal: an xattr costs twice its size there, not up to
    /// four times as a JSON number array (Samba NTACL values are ~4 KiB).
    #[serde(with = "hex_value")]
    pub value: Vec<u8>,
}

mod hex_value {
    pub(super) fn serialize<S: serde::Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        if text.len() % 2 != 0
            || !text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom("wartość atrybutu nie jest zapisem hex"));
        }
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).map_err(serde::de::Error::custom))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub sha256: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub mtime_ns: i128,
    pub acl: Vec<TransferAttribute>,
    pub xattr: Vec<TransferAttribute>,
}

/// What pins a copy: its inode and its content. Its metadata is compared with
/// the full source identity instead of being stored a second and third time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferPin {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub sha256: String,
}

fn pin_of(identity: &TransferIdentity) -> TransferPin {
    TransferPin {
        device: identity.device,
        inode: identity.inode,
        size: identity.size,
        sha256: identity.sha256.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferFile {
    pub source: String,
    pub destination: String,
    pub temporary: Option<String>,
    #[serde(default)]
    pub temporary_identity: Option<TransferPin>,
    #[serde(default)]
    pub temporary_pin: Option<(u64, u64)>,
    pub source_identity: TransferIdentity,
    pub destination_identity: Option<TransferPin>,
    /// Destination directory whose `mkdirat` may have run before its final
    /// metadata was applied. Only the directory named here, still empty and
    /// still in its private creation state, may be adopted after a crash.
    #[serde(default)]
    pub directory_intent: Option<String>,
    /// The read error that made this record finish by identity, written with
    /// UnlinkIntent before the unlink, so a crash after the unlink still
    /// reports how the source was removed.
    #[serde(default)]
    pub unread_source: Option<String>,
    pub phase: TransferFilePhase,
}

/// How a record reached Done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransferEnd {
    /// Every re-read of the source matched the measured identity.
    Verified,
    /// After the rename the source could no longer be read (the error is
    /// kept); it was unlinked on a stat-only identity match against the
    /// pinned, re-verified destination.
    SourceUnreadable(String),
}

/// One regular file the cache walk found, measured without reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanFile {
    pub path: String,
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub allocated: u64,
    pub mtime_ns: i128,
}

/// One non-directory entry of the cache walk. An entry the mover can never
/// move is reported on its own instead of failing the whole walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScanEntry {
    File(ScanFile),
    Refused { path: String, reason: String },
}

impl ScanEntry {
    fn path(&self) -> &str {
        match self {
            Self::File(file) => &file.path,
            Self::Refused { path, .. } => path,
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryIdentity {
    uid: u32,
    gid: u32,
    mode: u32,
    acl: Vec<TransferAttribute>,
    xattr: Vec<TransferAttribute>,
}

#[cfg(target_os = "linux")]
fn refused(path: String, reason: &str) -> ScanEntry {
    ScanEntry::Refused {
        path,
        reason: reason.into(),
    }
}

/// Walks the cache branch FD-relative and lists every entry outside the
/// pruned (pinned) paths. Directories are reopened by relative path beneath
/// the pinned root, so the walk holds two descriptors whatever the tree width.
#[cfg(target_os = "linux")]
pub(crate) fn scan_cache(
    source_root: &Path,
    prune: impl Fn(&str) -> bool,
) -> Result<Vec<ScanEntry>, String> {
    let root = open_root_directory(source_root)?;
    let root_device = stat_fd(root.as_raw_fd())?.st_dev;
    let mut entries = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(relative_dir) = pending.pop() {
        // One unreadable directory or entry is reported on its own and the
        // walk goes on; only an unreadable cache root stops it.
        let shown = if relative_dir.is_empty() { ".".to_string() } else { relative_dir.clone() };
        let directory = if relative_dir.is_empty() {
            duplicate_fd(root.as_raw_fd())?
        } else {
            match open_relative(
                root.as_raw_fd(),
                &relative_dir,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            ) {
                Ok(directory) => directory,
                Err(error) => {
                    entries.push(refused(shown, &format!("katalog niedostępny: {error}")));
                    continue;
                }
            }
        };
        let join = |name: &str| {
            if relative_dir.is_empty() {
                name.to_string()
            } else {
                format!("{relative_dir}/{name}")
            }
        };
        let names = match read_directory(directory.as_raw_fd()) {
            Ok(names) => names,
            Err(error) => {
                entries.push(refused(shown, &format!("listowanie katalogu: {error}")));
                continue;
            }
        };
        for raw_name in names {
            let name = match String::from_utf8(raw_name) {
                Ok(name) => name,
                Err(error) => {
                    entries.push(refused(
                        join(&String::from_utf8_lossy(error.as_bytes())),
                        "nazwa nie jest UTF-8",
                    ));
                    continue;
                }
            };
            let relative = join(&name);
            if prune(&relative) {
                continue;
            }
            let stat = match stat_at(directory.as_raw_fd(), &name) {
                Ok(stat) => stat,
                Err(error) => {
                    entries.push(refused(relative, &format!("stat: {error}")));
                    continue;
                }
            };
            let kind = stat.st_mode as u32 & libc::S_IFMT as u32;
            if stat.st_dev != root_device {
                entries.push(refused(relative, "wpis na innym systemie plików"));
            } else if kind == libc::S_IFDIR as u32 {
                pending.push(relative);
            } else if kind == libc::S_IFLNK as u32 {
                entries.push(refused(relative, "mover odmawia symlinku"));
            } else if kind != libc::S_IFREG as u32 {
                entries.push(refused(relative, "mover odmawia pliku specjalnego"));
            } else if stat.st_nlink != 1 {
                entries.push(refused(relative, "mover odmawia hardlinku"));
            } else {
                let size = u64::try_from(stat.st_size).map_err(|_| "ujemny rozmiar pliku")?;
                let allocated = u64::try_from(stat.st_blocks)
                    .map_err(|_| "ujemna liczba bloków pliku")?
                    .saturating_mul(512);
                if allocated < size {
                    entries.push(refused(relative, "mover odmawia pliku sparse"));
                } else {
                    entries.push(ScanEntry::File(ScanFile {
                        path: relative,
                        device: stat.st_dev as u64,
                        inode: stat.st_ino as u64,
                        size,
                        allocated,
                        mtime_ns: stat.st_mtime as i128 * 1_000_000_000
                            + stat.st_mtime_nsec as i128,
                    }));
                }
            }
        }
    }
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    Ok(entries)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn scan_cache(_: &Path, _: impl Fn(&str) -> bool) -> Result<Vec<ScanEntry>, String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

fn valid_relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("ścieżka transferu musi być względna i bez składników specjalnych".into());
    }
    Ok(())
}

/// Bytes of the read error an identity finish keeps in its record.
pub(crate) const UNREAD_SOURCE_LIMIT: usize = 128;

/// A read error as a record keeps it: bounded, control characters replaced.
#[cfg(target_os = "linux")]
fn bounded_error(error: &str) -> String {
    let mut text: String = error.chars().map(|c| if c.is_control() { '?' } else { c }).collect();
    if text.len() > UNREAD_SOURCE_LIMIT {
        let mut end = UNREAD_SOURCE_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The record at its largest: every identity pinned, a directory intent taken
/// and the read error of an identity finish kept. It is an upper bound for
/// sizing, not a record to persist — the intent it takes is the destination
/// itself, which is never shorter than the parent the real one names.
pub(crate) fn worst_case_record(file: &TransferFile) -> TransferFile {
    let mut pinned = file.clone();
    // Every byte of the kept read error escaped in JSON.
    pinned.unread_source = Some("\"".repeat(UNREAD_SOURCE_LIMIT));
    pinned.temporary_identity = Some(pin_of(&file.source_identity));
    pinned.destination_identity = Some(pin_of(&file.source_identity));
    pinned.temporary_pin = Some((u64::MAX, u64::MAX));
    pinned.directory_intent = Some(file.destination.clone());
    pinned.phase = TransferFilePhase::UnlinkConfirmed;
    pinned
}

/// Serialized size the record reaches once every identity is pinned, so the
/// journal bound is checked before the first byte is written.
pub(crate) fn worst_case_record_size(file: &TransferFile) -> Result<usize, String> {
    serde_json::to_vec(&worst_case_record(file))
        .map(|bytes| bytes.len())
        .map_err(|e| e.to_string())
}

/// SHA-256 of an exact relative path: how journal history names a path it
/// keeps only bounded for display.
pub(crate) fn path_digest(path: &str) -> String {
    Sha256::digest(path.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[cfg(target_os = "linux")]
const RESOLVE_BENEATH: u64 = 0x08;
#[cfg(target_os = "linux")]
const RESOLVE_NO_SYMLINKS: u64 = 0x04;

#[cfg(target_os = "linux")]
fn open_root_directory(path: &Path) -> Result<OwnedFd, String> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "ścieżka katalogu zawiera NUL".to_string())?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_NO_SYMLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(target_os = "linux")]
fn duplicate_fd(fd: i32) -> Result<OwnedFd, String> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// `(device, inode)` of a directory whose listing fails with EIO in this test thread.
    static UNLISTABLE: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
    /// An entry name whose `fstatat` fails with EIO in this test thread.
    static UNSTATABLE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_listing_of(identity: Option<(u64, u64)>) {
    UNLISTABLE.with(|cell| cell.set(identity));
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_stat_of(name: Option<&str>) {
    UNSTATABLE.with(|cell| *cell.borrow_mut() = name.map(str::to_string));
}

#[cfg(target_os = "linux")]
fn read_directory(fd: i32) -> Result<Vec<Vec<u8>>, String> {
    #[cfg(test)]
    {
        let stat = stat_fd(fd)?;
        if UNLISTABLE.with(|cell| cell.get()) == Some((stat.st_dev as u64, stat.st_ino as u64)) {
            return Err(std::io::Error::from_raw_os_error(libc::EIO).to_string());
        }
    }
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(error.to_string());
    }
    // The duplicate shares its offset with `fd`; every listing starts at the top.
    unsafe {
        libc::rewinddir(directory);
        *libc::__errno_location() = 0;
    }
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
            .to_bytes()
            .to_vec();
        if name.as_slice() != b"." && name.as_slice() != b".." {
            names.push(name);
        }
    }
    let read_error = unsafe { *libc::__errno_location() };
    if unsafe { libc::closedir(directory) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if read_error != 0 {
        return Err(std::io::Error::from_raw_os_error(read_error).to_string());
    }
    Ok(names)
}

#[cfg(target_os = "linux")]
fn open_relative_io(
    dirfd: i32,
    path: &str,
    flags: i32,
    mode: u32,
) -> Result<OwnedFd, std::io::Error> {
    valid_relative(path).map_err(std::io::Error::other)?;
    let path = CString::new(path).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "ścieżka zawiera NUL")
    })?;
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(target_os = "linux")]
fn open_relative(dirfd: i32, path: &str, flags: i32, mode: u32) -> Result<OwnedFd, String> {
    open_relative_io(dirfd, path, flags, mode).map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn stat_fd(fd: i32) -> Result<libc::stat, String> {
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(stat)
}

/// `fstatat` of one name inside `dirfd`, never following a symlink.
#[cfg(target_os = "linux")]
fn stat_at_io(dirfd: i32, name: &str) -> Result<libc::stat, std::io::Error> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(std::io::Error::other("nieprawidłowa nazwa wpisu"));
    }
    #[cfg(test)]
    if UNSTATABLE.with(|cell| cell.borrow().as_deref() == Some(name)) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    let name = CString::new(name).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "nazwa zawiera NUL")
    })?;
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(stat)
}

#[cfg(target_os = "linux")]
fn stat_at(dirfd: i32, name: &str) -> Result<libc::stat, String> {
    stat_at_io(dirfd, name).map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn exists_at(dirfd: i32, name: &str) -> Result<bool, String> {
    match stat_at_io(dirfd, name) {
        Ok(_) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// Why measuring a file failed: its medium could not deliver the content,
/// or anything else.
#[cfg(target_os = "linux")]
enum MeasureError {
    MediaRead(String),
    Other(String),
}

#[cfg(target_os = "linux")]
impl MeasureError {
    fn into_message(self) -> String {
        match self {
            Self::MediaRead(message) | Self::Other(message) => message,
        }
    }
}

#[cfg(target_os = "linux")]
impl From<String> for MeasureError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

#[cfg(target_os = "linux")]
impl From<&str> for MeasureError {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}

/// Errors that mean the medium failed to deliver data, not that the file
/// is other than measured.
#[cfg(target_os = "linux")]
fn media_read_error(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EIO)
            | Some(libc::ENXIO)
            | Some(libc::ENODATA)
            | Some(libc::EBADMSG)
            | Some(libc::EUCLEAN)
            | Some(libc::EREMOTEIO)
    )
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// `(device, inode)` whose content reads fail with EIO in this test thread.
    static UNREADABLE: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
}

/// Makes content reads of one inode fail with EIO for the calling test.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_content_reads(identity: Option<(u64, u64)>) {
    UNREADABLE.with(|cell| cell.set(identity));
}

#[cfg(target_os = "linux")]
fn measure_fd(fd: &OwnedFd) -> Result<TransferIdentity, MeasureError> {
    let stat = stat_fd(fd.as_raw_fd())?;
    let mode = stat.st_mode as u32;
    if stat.st_nlink != 1 || (mode & libc::S_IFMT as u32) != libc::S_IFREG as u32 {
        return Err("mover odmawia symlinku, hardlinku lub pliku specjalnego".into());
    }
    let blocks = u64::try_from(stat.st_blocks).map_err(|_| "ujemna liczba bloków pliku")?;
    let file_size = u64::try_from(stat.st_size).map_err(|_| "ujemny rozmiar pliku")?;
    if blocks
        .checked_mul(512)
        .ok_or("rozmiar bloków przekracza limit")?
        < file_size
    {
        return Err("mover odmawia pliku sparse".into());
    }
    #[cfg(test)]
    if UNREADABLE.with(|cell| cell.get()) == Some((stat.st_dev as u64, stat.st_ino as u64)) {
        return Err(MeasureError::MediaRead(
            std::io::Error::from_raw_os_error(libc::EIO).to_string(),
        ));
    }
    let input = File::from(fd.try_clone().map_err(|e| e.to_string())?);
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = input.read_at(&mut buffer, size).map_err(|error| {
            if media_read_error(&error) {
                MeasureError::MediaRead(error.to_string())
            } else {
                MeasureError::Other(error.to_string())
            }
        })?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or("rozmiar pliku przekracza limit")?;
        hash.update(&buffer[..count]);
    }
    let after = stat_fd(fd.as_raw_fd())?;
    if after.st_dev != stat.st_dev
        || after.st_ino != stat.st_ino
        || after.st_size != stat.st_size
        || after.st_mtime != stat.st_mtime
        || after.st_mtime_nsec != stat.st_mtime_nsec
    {
        return Err("plik zmienił się podczas pomiaru".into());
    }
    let (acl, xattr) = attributes_fd(fd.as_raw_fd())?;
    Ok(TransferIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        size,
        sha256: hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: mode & 0o7777,
        mtime_ns: stat.st_mtime as i128 * 1_000_000_000 + stat.st_mtime_nsec as i128,
        acl,
        xattr,
    })
}

#[cfg(target_os = "linux")]
fn identity_fd(fd: &OwnedFd) -> Result<TransferIdentity, String> {
    measure_fd(fd).map_err(MeasureError::into_message)
}

/// Makes the next read of `fd` come from the medium: dirty pages are written
/// back, then the clean ones are dropped from the page cache. The kernel
/// treats DONTNEED as advice: pages another process has mapped or locked stay
/// resident, so this guarantees a re-read from disk only for a file nobody
/// maps, which under the RO Hold is every branch file.
#[cfg(target_os = "linux")]
fn drop_cached_pages(fd: i32) -> Result<(), String> {
    if unsafe { libc::fdatasync(fd) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result = unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED) };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result).to_string());
    }
    Ok(())
}

fn transfer_end(file: &TransferFile) -> TransferEnd {
    match &file.unread_source {
        Some(error) => TransferEnd::SourceUnreadable(error.clone()),
        None => TransferEnd::Verified,
    }
}

/// The measured identity without reading content: the same single-linked
/// regular inode with the same size, mtime, owner and mode.
#[cfg(target_os = "linux")]
fn same_stat(stat: &libc::stat, identity: &TransferIdentity) -> bool {
    stat.st_nlink == 1
        && (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32
        && (stat.st_dev as u64, stat.st_ino as u64) == (identity.device, identity.inode)
        && u64::try_from(stat.st_size).ok() == Some(identity.size)
        && stat.st_mtime as i128 * 1_000_000_000 + stat.st_mtime_nsec as i128 == identity.mtime_ns
        && stat.st_uid == identity.uid
        && stat.st_gid == identity.gid
        && stat.st_mode as u32 & 0o7777 == identity.mode
}

#[cfg(target_os = "linux")]
fn attributes_fd(fd: i32) -> Result<(Vec<TransferAttribute>, Vec<TransferAttribute>), String> {
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok((Vec::new(), Vec::new()));
        }
        return Err(error.to_string());
    }
    let mut names = vec![0u8; size as usize];
    let read = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut acl = Vec::new();
    let mut xattr = Vec::new();
    for name in names[..read as usize]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = CString::new(name).map_err(|_| "xattr zawiera NUL".to_string())?;
        let value_size = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0) };
        if value_size < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut value = vec![0u8; value_size as usize];
        let read_value =
            unsafe { libc::fgetxattr(fd, name.as_ptr(), value.as_mut_ptr().cast(), value.len()) };
        if read_value < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if read_value as usize != value.len() {
            return Err("atrybut pliku zmienił rozmiar podczas odczytu".into());
        }
        let attribute = TransferAttribute {
            name: name.to_string_lossy().into_owned(),
            value,
        };
        if attribute.name == "system.posix_acl_access"
            || attribute.name == "system.posix_acl_default"
        {
            acl.push(attribute);
        } else {
            xattr.push(attribute);
        }
    }
    acl.sort_by(|left, right| left.name.cmp(&right.name));
    xattr.sort_by(|left, right| left.name.cmp(&right.name));
    Ok((acl, xattr))
}

#[cfg(target_os = "linux")]
fn restore_attributes<'a>(
    fd: i32,
    attributes: impl Iterator<Item = &'a TransferAttribute>,
) -> Result<(), String> {
    for attribute in attributes {
        let name =
            CString::new(attribute.name.as_str()).map_err(|_| "xattr zawiera NUL".to_string())?;
        let result = unsafe {
            libc::fsetxattr(
                fd,
                name.as_ptr(),
                attribute.value.as_ptr().cast(),
                attribute.value.len(),
                0,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn same_identity(fd: &OwnedFd, expected: &TransferIdentity) -> Result<(), String> {
    if identity_fd(fd)? != *expected {
        return Err("plik zmienił tożsamość względem journalu".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn same_content_metadata(actual: &TransferIdentity, source: &TransferIdentity) -> bool {
    actual.size == source.size
        && actual.sha256 == source.sha256
        && actual.uid == source.uid
        && actual.gid == source.gid
        && actual.mode == source.mode
        && actual.mtime_ns == source.mtime_ns
        && actual.acl == source.acl
        && actual.xattr == source.xattr
}

#[cfg(target_os = "linux")]
fn rename_without_replace(rootfd: i32, source: &str, target: &str) -> Result<(), String> {
    valid_relative(source)?;
    valid_relative(target)?;
    let source_parent = source
        .rsplit_once('/')
        .map(|(parent, _)| {
            open_relative(
                rootfd,
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        })
        .transpose()?;
    let target_parent = target
        .rsplit_once('/')
        .map(|(parent, _)| {
            open_relative(
                rootfd,
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        })
        .transpose()?;
    let source_dirfd = source_parent.as_ref().map_or(rootfd, AsRawFd::as_raw_fd);
    let target_dirfd = target_parent.as_ref().map_or(rootfd, AsRawFd::as_raw_fd);
    let source = CString::new(source.rsplit_once('/').map_or(source, |(_, name)| name))
        .map_err(|_| "źródło zawiera NUL".to_string())?;
    let target = CString::new(target.rsplit_once('/').map_or(target, |(_, name)| name))
        .map_err(|_| "cel zawiera NUL".to_string())?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_dirfd,
            source.as_ptr(),
            target_dirfd,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn fsync_fd(fd: i32) -> Result<(), String> {
    if unsafe { libc::fsync(fd) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn directory_identity_fd(fd: &OwnedFd) -> Result<DirectoryIdentity, String> {
    let stat = stat_fd(fd.as_raw_fd())?;
    let (acl, xattr) = attributes_fd(fd.as_raw_fd())?;
    Ok(DirectoryIdentity {
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode as u32 & 0o7777,
        acl,
        xattr,
    })
}

/// Whether an existing destination directory grants exactly what the cache
/// directory grants. Its xattrs and mtime may differ: mergerfs clones a path
/// with its own timing, and every later rename changes the mtime anyway.
#[cfg(target_os = "linux")]
fn same_directory_access(actual: &DirectoryIdentity, expected: &DirectoryIdentity) -> bool {
    actual.uid == expected.uid
        && actual.gid == expected.gid
        && actual.mode == expected.mode
        && actual.acl == expected.acl
}

#[cfg(target_os = "linux")]
fn directory_parent(rootfd: i32, path: &str) -> Result<(OwnedFd, String), String> {
    valid_relative(path)?;
    let (parent, name) = path
        .rsplit_once('/')
        .map_or(("", path), |(parent, name)| (parent, name));
    let parent = if parent.is_empty() {
        duplicate_fd(rootfd)?
    } else {
        open_relative(
            rootfd,
            parent,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?
    };
    Ok((parent, name.to_string()))
}

#[cfg(target_os = "linux")]
fn parent_path(path: &str) -> Result<&str, String> {
    valid_relative(path)?;
    Ok(path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or(""))
}

/// Cumulative parent prefixes of `parent`: `a`, `a/b`, `a/b/c`.
#[cfg(target_os = "linux")]
fn parent_prefixes(parent: &str) -> Vec<&str> {
    if parent.is_empty() {
        return Vec::new();
    }
    let mut prefixes: Vec<&str> = parent
        .match_indices('/')
        .map(|(index, _)| &parent[..index])
        .collect();
    prefixes.push(parent);
    prefixes
}

/// A directory the helper made with `mkdirat(0700)` and has not finished:
/// its own, private and empty.
#[cfg(target_os = "linux")]
fn created_privately(fd: &OwnedFd, actual: &DirectoryIdentity) -> Result<bool, String> {
    Ok(actual.uid == unsafe { libc::geteuid() }
        && actual.mode & 0o777 == 0o700
        && read_directory(fd.as_raw_fd())?.is_empty())
}

/// Removes POSIX ACLs the source does not carry. A file or directory created
/// under a default ACL inherits it, and `fsetxattr` alone never removes it.
#[cfg(target_os = "linux")]
fn strip_foreign_acls(fd: i32, source_acl: &[TransferAttribute]) -> Result<(), String> {
    for name in ["system.posix_acl_access", "system.posix_acl_default"] {
        if source_acl.iter().any(|attribute| attribute.name == name) {
            continue;
        }
        let c_name = CString::new(name).map_err(|_| "xattr zawiera NUL".to_string())?;
        if unsafe { libc::fremovexattr(fd, c_name.as_ptr()) } != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::ENODATA) | Some(libc::ENOTSUP)) {
                return Err(error.to_string());
            }
        }
    }
    Ok(())
}

/// Owner, ACL/xattrs and finally the mode (set-id bits survive only after
/// `fchown`), then a re-read: the directory must now equal its source.
#[cfg(target_os = "linux")]
fn finalize_directory(
    fd: &OwnedFd,
    parent: i32,
    identity: &DirectoryIdentity,
) -> Result<(), String> {
    if unsafe { libc::fchown(fd.as_raw_fd(), identity.uid, identity.gid) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    strip_foreign_acls(fd.as_raw_fd(), &identity.acl)?;
    restore_attributes(
        fd.as_raw_fd(),
        identity.acl.iter().chain(identity.xattr.iter()),
    )?;
    if unsafe { libc::fchmod(fd.as_raw_fd(), identity.mode) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    fsync_fd(fd.as_raw_fd())?;
    fsync_fd(parent)?;
    if directory_identity_fd(fd)? != *identity {
        return Err("utworzony katalog ma inne metadane niż źródło".into());
    }
    Ok(())
}

/// Measures one scanned file and checks its destination before anything is
/// written. Every error here is a per-file refusal: nothing was mutated.
#[cfg(target_os = "linux")]
pub(crate) fn plan_file(
    source_root: &Path,
    destination_root: &Path,
    path: &str,
    scanned: (u64, u64),
    temporary: &str,
) -> Result<TransferFile, String> {
    valid_relative(path)?;
    valid_relative(temporary)?;
    if temporary.contains('/') {
        return Err("nazwa tymczasowa musi leżeć w korzeniu brancha".into());
    }
    let source_root = open_root_directory(source_root)?;
    let destination_root = open_root_directory(destination_root)?;
    let source = open_relative(
        source_root.as_raw_fd(),
        path,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )?;
    let source_identity = identity_fd(&source)?;
    if (source_identity.device, source_identity.inode) != scanned {
        return Err("plik zmienił się od skanu".into());
    }
    let mut complete = true;
    for prefix in parent_prefixes(parent_path(path)?) {
        let expected = directory_identity_fd(&open_relative(
            source_root.as_raw_fd(),
            prefix,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?)?;
        match open_relative_io(
            destination_root.as_raw_fd(),
            prefix,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        ) {
            Ok(existing) => {
                if !same_directory_access(&directory_identity_fd(&existing)?, &expected) {
                    return Err(format!("katalog docelowy {prefix} ma obce metadane"));
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                complete = false;
                break;
            }
            Err(error) => return Err(format!("katalog docelowy {prefix}: {error}")),
        }
    }
    if complete {
        let (parent, name) = directory_parent(destination_root.as_raw_fd(), path)?;
        if exists_at(parent.as_raw_fd(), &name)? {
            return Err("cel już istnieje".into());
        }
    }
    if exists_at(destination_root.as_raw_fd(), temporary)? {
        return Err("nazwa tymczasowa jest zajęta".into());
    }
    Ok(TransferFile {
        source: path.into(),
        destination: path.into(),
        temporary: Some(temporary.into()),
        temporary_identity: None,
        temporary_pin: None,
        source_identity,
        destination_identity: None,
        directory_intent: None,
        unread_source: None,
        phase: TransferFilePhase::CopyIntent,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn plan_file(
    _: &Path,
    _: &Path,
    _: &str,
    _: (u64, u64),
    _: &str,
) -> Result<TransferFile, String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// Creates the destination's missing parent directories FD-relative, with the
/// cache directory's owner, mode, ACL and xattrs. Each one is made as a
/// private 0700 directory and finished before the next is made, so a crash
/// leaves at most the one directory named by `directory_intent`.
#[cfg(target_os = "linux")]
pub(crate) fn prepare_parents<F>(
    source_root: &Path,
    destination_root: &Path,
    file: &mut TransferFile,
    mut persist: F,
) -> Result<(), String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    let source_root = open_root_directory(source_root)?;
    let destination_root = open_root_directory(destination_root)?;
    let destination = file.destination.clone();
    let prefixes = parent_prefixes(parent_path(&destination)?);
    if file
        .directory_intent
        .as_deref()
        .is_some_and(|intent| !prefixes.contains(&intent))
    {
        return Err("zamiar katalogu spoza ścieżki celu".into());
    }
    for prefix in prefixes {
        let expected = directory_identity_fd(&open_relative(
            source_root.as_raw_fd(),
            prefix,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?)?;
        let (parent, name) = directory_parent(destination_root.as_raw_fd(), prefix)?;
        let intended = file.directory_intent.as_deref() == Some(prefix);
        match open_relative_io(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        ) {
            Ok(existing) => {
                let actual = directory_identity_fd(&existing)?;
                if !same_directory_access(&actual, &expected) {
                    if !intended || !created_privately(&existing, &actual)? {
                        return Err(format!("katalog docelowy {prefix} ma obce metadane"));
                    }
                    finalize_directory(&existing, parent.as_raw_fd(), &expected)?;
                }
                if intended {
                    file.directory_intent = None;
                    persist(file)?;
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                file.directory_intent = Some(prefix.to_string());
                persist(file)?;
                let c_name = CString::new(name.as_str())
                    .map_err(|_| "nazwa katalogu zawiera NUL".to_string())?;
                if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let created = open_relative(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                    0,
                )?;
                if !created_privately(&created, &directory_identity_fd(&created)?)? {
                    return Err("utworzony katalog ma nieoczekiwane metadane".into());
                }
                finalize_directory(&created, parent.as_raw_fd(), &expected)?;
                file.directory_intent = None;
                persist(file)?;
            }
            Err(error) => return Err(format!("katalog docelowy {prefix}: {error}")),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn prepare_parents<F>(_: &Path, _: &Path, _: &mut TransferFile, _: F) -> Result<(), String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// `(device, inode)` of every file another process holds open, read from
/// `/proc`. The helper's own descriptors are left out. This is a snapshot
/// taken under the global RO barrier, not a barrier of its own.
///
/// It sees union clients only through `daemon`, the private mergerfs: the
/// worker unshares CLONE_NEWNS alone (no PID namespace), and with
/// `cache.files=off` mergerfs holds the backing file of every open union
/// file. So the daemon's descriptors must be readable, or the snapshot
/// refuses instead of reporting "nothing open".
#[cfg(target_os = "linux")]
pub(crate) fn open_file_identities(daemon: u32) -> Result<BTreeSet<(u64, u64)>, String> {
    let own = std::process::id();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))? {
        let entry = entry.map_err(|e| format!("/proc: {e}"))?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        {
            if pid != own {
                pids.push(pid);
            }
        }
    }
    open_file_identities_of(&pids, daemon)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn open_file_identities(_: u32) -> Result<BTreeSet<(u64, u64)>, String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// Open descriptors of the given processes. Another process or descriptor
/// that disappears during the walk is skipped; `required` (the mergerfs
/// daemon) must be listed and readable, and any other error is a refusal.
#[cfg(target_os = "linux")]
pub(crate) fn open_file_identities_of(
    pids: &[u32],
    required: u32,
) -> Result<BTreeSet<(u64, u64)>, String> {
    if !pids.contains(&required) {
        return Err(format!("proces mergerfs {required} jest niewidoczny w /proc"));
    }
    let gone = |error: &std::io::Error| {
        matches!(error.raw_os_error(), Some(libc::ENOENT) | Some(libc::ESRCH))
    };
    let mut identities = BTreeSet::new();
    for pid in pids {
        let descriptors = match std::fs::read_dir(format!("/proc/{pid}/fd")) {
            Ok(descriptors) => descriptors,
            Err(error) if *pid != required && gone(&error) => continue,
            Err(error) => return Err(format!("/proc/{pid}/fd: {error}")),
        };
        for descriptor in descriptors {
            let descriptor = match descriptor {
                Ok(descriptor) => descriptor,
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(format!("/proc/{pid}/fd: {error}")),
            };
            match std::fs::metadata(descriptor.path()) {
                Ok(metadata) => {
                    identities.insert((metadata.dev(), metadata.ino()));
                }
                Err(error) if gone(&error) => continue,
                Err(error) => return Err(format!("/proc/{pid}/fd: {error}")),
            }
        }
    }
    Ok(identities)
}

/// Whether anything exists at `path` beneath `root`, resolved FD-relative
/// without following symlinks. A symlinked parent is an error, not "absent".
#[cfg(target_os = "linux")]
pub(crate) fn entry_exists(root: &Path, path: &str) -> Result<bool, String> {
    let root = open_root_directory(root)?;
    let parent = parent_path(path)?;
    let parent = if parent.is_empty() {
        duplicate_fd(root.as_raw_fd())?
    } else {
        match open_relative_io(
            root.as_raw_fd(),
            parent,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        ) {
            Ok(parent) => parent,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
    };
    exists_at(
        parent.as_raw_fd(),
        path.rsplit_once('/').map_or(path, |(_, name)| name),
    )
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn entry_exists(_: &Path, _: &str) -> Result<bool, String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// A temporary this record created and never pinned: the helper's own, still
/// private (no group/other bits) and never linked twice.
#[cfg(target_os = "linux")]
fn fresh_temporary(stat: &libc::stat) -> bool {
    stat.st_nlink == 1
        && (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32
        && stat.st_uid == unsafe { libc::geteuid() }
        && stat.st_mode as u32 & 0o077 == 0
}

/// Withdraws a record whose source was never touched (CopyIntent up to
/// RenameIntent): removes the helper's own copy, found by its pinned inode
/// under the temporary name or already renamed into place, and the private
/// directory this record was still creating.
#[cfg(target_os = "linux")]
pub(crate) fn roll_back(
    source_root: &Path,
    destination_root: &Path,
    file: &TransferFile,
) -> Result<(), String> {
    if !matches!(
        file.phase,
        TransferFilePhase::CopyIntent | TransferFilePhase::CopyConfirmed | TransferFilePhase::RenameIntent
    ) {
        return Err("rekordu po potwierdzonej zmianie nazwy nie można wycofać".into());
    }
    let source_root = open_root_directory(source_root)?;
    let destination_root = open_root_directory(destination_root)?;
    let source = open_relative(
        source_root.as_raw_fd(),
        &file.source,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )?;
    let source_stat = stat_fd(source.as_raw_fd())?;
    if (source_stat.st_dev as u64, source_stat.st_ino as u64)
        != (file.source_identity.device, file.source_identity.inode)
    {
        return Err("źródło wycofywanego pliku zmieniło inode".into());
    }
    let unlink = |dirfd: i32, name: &str| -> Result<(), String> {
        let name = CString::new(name).map_err(|_| "nazwa zawiera NUL".to_string())?;
        if unsafe { libc::unlinkat(dirfd, name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        fsync_fd(dirfd)
    };
    let temporary = file.temporary.as_deref().ok_or("brak trwałej ścieżki tymczasowej")?;
    match stat_at_io(destination_root.as_raw_fd(), temporary) {
        Ok(stat) => {
            let ours = match file.temporary_pin {
                Some(pin) => (stat.st_dev as u64, stat.st_ino as u64) == pin,
                None => fresh_temporary(&stat),
            };
            if !ours {
                return Err("pod nazwą tymczasową leży obcy plik".into());
            }
            unlink(destination_root.as_raw_fd(), temporary)?;
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
        Err(error) => return Err(error.to_string()),
    }
    if let (TransferFilePhase::RenameIntent, Some(pin)) = (file.phase, file.temporary_pin) {
        let parent = parent_path(&file.destination)?;
        let parent = if parent.is_empty() {
            Some(duplicate_fd(destination_root.as_raw_fd())?)
        } else {
            match open_relative_io(
                destination_root.as_raw_fd(),
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            ) {
                Ok(parent) => Some(parent),
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
                Err(error) => return Err(error.to_string()),
            }
        };
        if let Some(parent) = parent {
            let name = file.destination.rsplit_once('/').map_or(file.destination.as_str(), |(_, name)| name);
            match stat_at_io(parent.as_raw_fd(), name) {
                Ok(stat) if (stat.st_dev as u64, stat.st_ino as u64) == pin => {
                    unlink(parent.as_raw_fd(), name)?;
                }
                Ok(_) => {}
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    if let Some(intent) = &file.directory_intent {
        let (parent, name) = directory_parent(destination_root.as_raw_fd(), intent)?;
        match open_relative_io(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        ) {
            Ok(directory) => {
                if created_privately(&directory, &directory_identity_fd(&directory)?)? {
                    let c_name = CString::new(name.as_str())
                        .map_err(|_| "nazwa katalogu zawiera NUL".to_string())?;
                    if unsafe { libc::unlinkat(parent.as_raw_fd(), c_name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    fsync_fd(parent.as_raw_fd())?;
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    fsync_fd(destination_root.as_raw_fd())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn roll_back(_: &Path, _: &Path, _: &TransferFile) -> Result<(), String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// Wykonuje jeden rekord i zapisuje każdą zmianę fazy przed syscall.
#[cfg(target_os = "linux")]
pub(crate) fn transfer_file<F>(
    source_root: &Path,
    destination_root: &Path,
    file: &mut TransferFile,
    mut persist: F,
) -> Result<TransferEnd, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    let source_root = open_root_directory(source_root)?;
    let destination_root = open_root_directory(destination_root)?;
    let source_fd = source_root.as_raw_fd();
    let destination_fd = destination_root.as_raw_fd();
    let source = match open_relative_io(
        source_fd,
        &file.source,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    ) {
        Ok(source) => Some(source),
        Err(error)
            if matches!(
                file.phase,
                TransferFilePhase::UnlinkIntent
                    | TransferFilePhase::UnlinkConfirmed
                    | TransferFilePhase::Done
            ) && error.raw_os_error() == Some(libc::ENOENT) =>
        {
            None
        }
        Err(error) => return Err(error.to_string()),
    };
    if source.is_none()
        && matches!(
            file.phase,
            TransferFilePhase::UnlinkIntent
                | TransferFilePhase::UnlinkConfirmed
                | TransferFilePhase::Done
        )
    {
        let parent = parent_path(&file.source)?;
        if !parent.is_empty() {
            open_relative(
                source_fd,
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )?;
        }
    }
    // After the rename the destination is the verified, pinned copy and the
    // source cannot change under the RO Hold. A source its medium no longer
    // reads is then finished on a stat-only identity instead of failing every
    // retry; before the rename, or for any other error, it is never.
    let mut unreadable = None;
    if let Some(source) = &source {
        match measure_fd(source) {
            Ok(identity) if identity == file.source_identity => {}
            Ok(_) => return Err("plik zmienił tożsamość względem journalu".into()),
            Err(MeasureError::MediaRead(error))
                if matches!(
                    file.phase,
                    TransferFilePhase::RenameConfirmed | TransferFilePhase::UnlinkIntent
                ) && same_stat(&stat_fd(source.as_raw_fd())?, &file.source_identity) =>
            {
                unreadable = Some(error);
            }
            Err(error) => return Err(error.into_message()),
        }
    }
    // A copy is accepted only as a pinned inode whose content and metadata
    // equal the measured source.
    let source_identity = file.source_identity.clone();
    let verified = |fd: &OwnedFd, pin: &TransferPin| -> Result<(), String> {
        let actual = identity_fd(fd)?;
        if pin_of(&actual) != *pin || !same_content_metadata(&actual, &source_identity) {
            return Err("kopia nie odpowiada przypiętej tożsamości i metadanym źródła".into());
        }
        Ok(())
    };
    if file.phase == TransferFilePhase::CopyIntent {
        let temporary = file
            .temporary
            .clone()
            .ok_or("brak trwałej ścieżki tymczasowej")?;
        persist(file)?;
        let target = match open_relative_io(
            destination_fd,
            &temporary,
            libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        ) {
            Ok(target) => {
                // The name is unique to this operation and sequence and the
                // branch is private under Hold, so an unpinned file there is
                // this record's own create cut short before its pin was
                // written: adopt it, but only in that fresh state.
                if file.temporary_pin.is_none() && !fresh_temporary(&stat_fd(target.as_raw_fd())?) {
                    return Err("plik tymczasowy bez pina nie jest świeżą kopią tej operacji".into());
                }
                target
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => open_relative(
                destination_fd,
                &temporary,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )?,
            Err(error) => return Err(error.to_string()),
        };
        let temporary_stat = stat_fd(target.as_raw_fd())?;
        if temporary_stat.st_nlink != 1
            || (temporary_stat.st_mode as u32 & libc::S_IFMT as u32) != libc::S_IFREG as u32
        {
            return Err("mover odmawia tymczasowego hardlinku lub pliku specjalnego".into());
        }
        let temporary_pin = (temporary_stat.st_dev as u64, temporary_stat.st_ino as u64);
        match file.temporary_pin {
            Some(pin) if pin != temporary_pin => {
                return Err("plik tymczasowy zmienił przypięty inode".into());
            }
            Some(_) => {}
            None => {
                file.temporary_pin = Some(temporary_pin);
                persist(file)?;
            }
        }
        if unsafe { libc::ftruncate(target.as_raw_fd(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let input_clone = source
            .as_ref()
            .ok_or("brak źródła przed kopiowaniem")?
            .try_clone()
            .map_err(|e| e.to_string())?;
        let target_clone = target.try_clone().map_err(|e| e.to_string())?;
        let mut input = File::from(input_clone);
        let mut output = File::from(target_clone);
        std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
        if unsafe {
            libc::fchown(
                target.as_raw_fd(),
                file.source_identity.uid,
                file.source_identity.gid,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        strip_foreign_acls(target.as_raw_fd(), &file.source_identity.acl)?;
        restore_attributes(
            target.as_raw_fd(),
            file.source_identity
                .acl
                .iter()
                .chain(file.source_identity.xattr.iter()),
        )?;
        // The temporary stays 0600 until here: the final mode, set-id bits
        // included, comes after `fchown`, which would clear them.
        if unsafe { libc::fchmod(target.as_raw_fd(), file.source_identity.mode) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let seconds = file.source_identity.mtime_ns.div_euclid(1_000_000_000);
        let nanos = file.source_identity.mtime_ns.rem_euclid(1_000_000_000);
        let times = [
            libc::timespec {
                tv_sec: seconds as _,
                tv_nsec: nanos as _,
            },
            libc::timespec {
                tv_sec: seconds as _,
                tv_nsec: nanos as _,
            },
        ];
        if unsafe { libc::futimens(target.as_raw_fd(), times.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        fsync_fd(target.as_raw_fd())?;
        // Compared before CopyConfirmed: a copy the branch altered (an
        // inherited ACL, a label) is withdrawn instead of being published.
        let actual = identity_fd(&target)?;
        if !same_content_metadata(&actual, &file.source_identity) {
            return Err("kopia tymczasowa ma inną treść lub metadane niż źródło".into());
        }
        file.temporary_identity = Some(pin_of(&actual));
        file.phase = TransferFilePhase::CopyConfirmed;
        persist(file)?;
    }
    if matches!(
        file.phase,
        TransferFilePhase::CopyConfirmed | TransferFilePhase::RenameIntent
    ) {
        let temporary = file
            .temporary
            .clone()
            .ok_or("brak trwałej ścieżki tymczasowej")?;
        let temporary_identity = file
            .temporary_identity
            .clone()
            .ok_or("brak tożsamości pliku tymczasowego")?;
        file.phase = TransferFilePhase::RenameIntent;
        persist(file)?;
        let already_renamed = match open_relative(
            destination_fd,
            &file.destination,
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        ) {
            Ok(destination) => pin_of(&identity_fd(&destination)?) == temporary_identity,
            Err(_) => false,
        };
        if already_renamed {
            let destination = open_relative(
                destination_fd,
                &file.destination,
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0,
            )?;
            verified(&destination, &temporary_identity)?;
        } else {
            let temporary_fd = open_relative(
                destination_fd,
                &temporary,
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0,
            )?;
            verified(&temporary_fd, &temporary_identity)?;
            rename_without_replace(destination_fd, &temporary, &file.destination)?;
        }
        let parent = parent_path(&file.destination)?;
        if parent.is_empty() {
            fsync_fd(destination_fd)?;
        } else {
            let destination_parent = open_relative(
                destination_fd,
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )?;
            fsync_fd(destination_parent.as_raw_fd())?;
        }
        file.phase = TransferFilePhase::RenameConfirmed;
        persist(file)?;
    }
    if matches!(
        file.phase,
        TransferFilePhase::UnlinkConfirmed | TransferFilePhase::Done
    ) {
        if source.is_some() {
            return Err("źródło nadal istnieje po potwierdzonym unlinku".into());
        }
        let destination = open_relative(
            destination_fd,
            &file.destination,
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )?;
        let pinned = file
            .destination_identity
            .clone()
            .ok_or("brak tożsamości potwierdzonego celu")?;
        verified(&destination, &pinned).map_err(|_| "cel zmienił tożsamość po unlinku".to_string())?;
        if file.phase == TransferFilePhase::UnlinkConfirmed {
            file.phase = TransferFilePhase::Done;
            persist(file)?;
        }
        return Ok(transfer_end(file));
    }
    if matches!(
        file.phase,
        TransferFilePhase::RenameConfirmed | TransferFilePhase::UnlinkIntent
    ) {
        let destination = open_relative(
            destination_fd,
            &file.destination,
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )?;
        // The hash that authorises an unlink without reading the source must
        // come from the destination's medium, not from its page cache.
        if unreadable.is_some() {
            drop_cached_pages(destination.as_raw_fd())?;
        }
        let actual = identity_fd(&destination)?;
        if !same_content_metadata(&actual, &file.source_identity) {
            return Err("cel ma inną treść lub metadane źródła".into());
        }
        if file.phase == TransferFilePhase::UnlinkIntent {
            if file.destination_identity.as_ref() != Some(&pin_of(&actual)) {
                return Err("cel nie odpowiada przypiętej tożsamości".into());
            }
        } else {
            file.destination_identity = Some(pin_of(&actual));
        }
        // A source already gone was unlinked by this record: the intent it
        // wrote before that unlink stays as it is.
        if source.is_some() {
            file.unread_source = unreadable.as_deref().map(bounded_error);
        }
        file.phase = TransferFilePhase::UnlinkIntent;
        persist(file)?;
        let source_parent_path = parent_path(&file.source)?;
        let source_parent = if source_parent_path.is_empty() {
            None
        } else {
            Some(open_relative(
                source_fd,
                source_parent_path,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )?)
        };
        let source_parent_fd = source_parent.as_ref().map_or(source_fd, AsRawFd::as_raw_fd);
        let source_name_text = file
            .source
            .rsplit_once('/')
            .map_or(file.source.as_str(), |(_, name)| name);
        let source_name =
            CString::new(source_name_text).map_err(|_| "źródło zawiera NUL".to_string())?;
        if source.is_some() {
            let current = open_relative(
                source_parent_fd,
                source_name_text,
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0,
            )?;
            if unreadable.is_some() {
                if !same_stat(&stat_fd(current.as_raw_fd())?, &file.source_identity) {
                    return Err("nieczytelne źródło zmieniło tożsamość".into());
                }
            } else {
                same_identity(&current, &file.source_identity)?;
            }
            if unsafe { libc::unlinkat(source_parent_fd, source_name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        fsync_fd(source_parent_fd)?;
        file.phase = TransferFilePhase::UnlinkConfirmed;
        persist(file)?;
        file.phase = TransferFilePhase::Done;
        persist(file)?;
    }
    Ok(transfer_end(file))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn transfer_file<F>(_: &Path, _: &Path, _: &mut TransferFile, _: F) -> Result<TransferEnd, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn unique(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tentanas-transfer-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn fixture() -> (PathBuf, PathBuf) {
        (unique("source"), unique("destination"))
    }

    fn scanned(source_root: &Path, path: &str) -> ScanFile {
        scan_cache(source_root, |_| false)
            .unwrap()
            .into_iter()
            .find_map(|entry| match entry {
                ScanEntry::File(file) if file.path == path => Some(file),
                _ => None,
            })
            .expect("skanowany plik")
    }

    fn planned(source_root: &Path, destination_root: &Path, path: &str) -> TransferFile {
        let file = scanned(source_root, path);
        plan_file(
            source_root,
            destination_root,
            path,
            (file.device, file.inode),
            ".tentanas-transfer-test-0",
        )
        .expect("zaplanowany plik")
    }

    fn set_xattr(path: &Path, name: &str, value: &[u8]) {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = CString::new(name).unwrap();
        assert_eq!(
            unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }

    fn get_xattr(path: &Path, name: &str) -> Vec<u8> {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = CString::new(name).unwrap();
        let mut value = vec![0u8; 4096];
        let size = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        assert!(size >= 0, "{}", std::io::Error::last_os_error());
        value.truncate(size as usize);
        value
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn plan_refuses_existing_destination_before_any_write() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"transfer payload").unwrap();
        fs::write(destination_root.join("payload.bin"), b"existing").unwrap();
        let file = scanned(&source_root, "payload.bin");
        let error = plan_file(
            &source_root,
            &destination_root,
            "payload.bin",
            (file.device, file.inode),
            ".tentanas-transfer-test-0",
        )
        .expect_err("istniejący cel");
        assert_eq!(error, "cel już istnieje");
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 1);
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn transfer_never_replaces_destination_created_after_planning() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"transfer payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        fs::write(destination_root.join("payload.bin"), b"existing").unwrap();
        let result = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        assert!(result.is_err());
        assert_eq!(
            fs::read(destination_root.join("payload.bin")).unwrap(),
            b"existing"
        );
        assert_eq!(
            fs::read(source_root.join("payload.bin")).unwrap(),
            b"transfer payload"
        );
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn transfer_completes_and_records_each_boundary() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"transfer payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let mut phases = Vec::new();
        transfer_file(&source_root, &destination_root, &mut file, |record| {
            let reopened: TransferFile =
                serde_json::from_slice(&serde_json::to_vec(record).unwrap()).unwrap();
            phases.push(reopened.phase);
            Ok(())
        })
        .unwrap();
        assert_eq!(file.phase, TransferFilePhase::Done);
        assert_eq!(
            fs::read(destination_root.join("payload.bin")).unwrap(),
            b"transfer payload"
        );
        assert!(!source_root.join("payload.bin").exists());
        assert!(phases.contains(&TransferFilePhase::CopyIntent));
        assert!(phases.contains(&TransferFilePhase::CopyConfirmed));
        assert!(phases.contains(&TransferFilePhase::RenameConfirmed));
        assert!(phases.contains(&TransferFilePhase::UnlinkConfirmed));
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn persisted_record_reopens_after_copy_boundary_and_finishes() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"transfer payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let record_path = unique("record").join("record.json");
        let mut interrupted = false;
        let result = transfer_file(&source_root, &destination_root, &mut file, |record| {
            let bytes = serde_json::to_vec(record).unwrap();
            let mut output = File::create(&record_path).unwrap();
            output.write_all(&bytes).unwrap();
            output.sync_all().unwrap();
            if record.phase == TransferFilePhase::CopyConfirmed && !interrupted {
                interrupted = true;
                return Err("wstrzymanie po potwierdzeniu kopii".into());
            }
            Ok(())
        });
        assert!(result.is_err());
        let bytes = fs::read(&record_path).unwrap();
        let mut reopened: TransferFile = serde_json::from_slice(&bytes).unwrap();
        transfer_file(&source_root, &destination_root, &mut reopened, |record| {
            let bytes = serde_json::to_vec(record).unwrap();
            let mut output = File::create(&record_path).unwrap();
            output.write_all(&bytes).unwrap();
            output.sync_all().unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(reopened.phase, TransferFilePhase::Done);
        assert!(!source_root.join("payload.bin").exists());
        assert_eq!(
            fs::read(destination_root.join("payload.bin")).unwrap(),
            b"transfer payload"
        );
        fs::remove_dir_all(record_path.parent().unwrap()).unwrap();
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn unpinned_partial_temporary_is_refused_without_touching_source() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        fs::write(
            destination_root.join(file.temporary.as_ref().unwrap()),
            b"partial",
        )
        .unwrap();
        let result = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        assert!(result.is_err());
        assert_eq!(
            fs::read(source_root.join("payload.bin")).unwrap(),
            b"source"
        );
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn renamed_destination_with_replaced_inode_is_refused() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).unwrap();
        fs::remove_file(destination_root.join("payload.bin")).unwrap();
        fs::write(destination_root.join("payload.bin"), b"source").unwrap();
        file.phase = TransferFilePhase::Done;
        let result = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        assert!(result.is_err());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn scan_refuses_symlink_root_and_reports_symlinks_per_entry() {
        let real_root = unique("real-root");
        fs::write(real_root.join("payload.bin"), b"source").unwrap();
        let linked_root = real_root.with_file_name(format!(
            "tentanas-transfer-linked-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        symlink(&real_root, &linked_root).unwrap();
        assert!(scan_cache(&linked_root, |_| false).is_err());
        fs::remove_file(&linked_root).unwrap();

        let root = unique("links");
        let outside = unique("outside");
        fs::write(outside.join("payload.bin"), b"outside").unwrap();
        symlink(&outside, root.join("nested")).unwrap();
        symlink(outside.join("payload.bin"), root.join("leaf.bin")).unwrap();
        fs::write(root.join("plain.bin"), b"plain").unwrap();
        let entries = scan_cache(&root, |_| false).unwrap();
        let refused: Vec<(&str, &str)> = entries
            .iter()
            .filter_map(|entry| match entry {
                ScanEntry::Refused { path, reason } => Some((path.as_str(), reason.as_str())),
                ScanEntry::File(_) => None,
            })
            .collect();
        assert_eq!(
            refused,
            vec![
                ("leaf.bin", "mover odmawia symlinku"),
                ("nested", "mover odmawia symlinku")
            ]
        );
        let files: Vec<&str> = entries
            .iter()
            .filter_map(|entry| match entry {
                ScanEntry::File(file) => Some(file.path.as_str()),
                ScanEntry::Refused { .. } => None,
            })
            .collect();
        assert_eq!(files, vec!["plain.bin"]);
        for path in [real_root, root, outside] {
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn scan_reports_hardlink_sparse_fifo_and_non_utf8_per_entry() {
        let (root, destination_root) = fixture();
        fs::write(root.join("ok.bin"), b"ok").unwrap();
        fs::write(root.join("payload.bin"), b"source").unwrap();
        fs::hard_link(root.join("payload.bin"), root.join("alias.bin")).unwrap();
        fs::File::create(root.join("sparse.bin"))
            .unwrap()
            .set_len(8192)
            .unwrap();
        let fifo = CString::new(root.join("payload.pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        fs::write(
            root.join(std::ffi::OsStr::from_bytes(b"bad\xff.bin")),
            b"bytes",
        )
        .unwrap();
        let entries = scan_cache(&root, |_| false).unwrap();
        let mut refused: Vec<(String, String)> = entries
            .iter()
            .filter_map(|entry| match entry {
                ScanEntry::Refused { path, reason } => Some((path.clone(), reason.clone())),
                ScanEntry::File(_) => None,
            })
            .collect();
        refused.sort();
        assert_eq!(
            refused,
            vec![
                ("alias.bin".into(), "mover odmawia hardlinku".into()),
                ("bad\u{fffd}.bin".into(), "nazwa nie jest UTF-8".into()),
                ("payload.bin".into(), "mover odmawia hardlinku".into()),
                ("payload.pipe".into(), "mover odmawia pliku specjalnego".into()),
                ("sparse.bin".into(), "mover odmawia pliku sparse".into()),
            ]
        );
        let ok = scanned(&root, "ok.bin");
        assert_eq!(ok.size, 2);
        let stat = fs::metadata(root.join("payload.bin")).unwrap();
        assert!(plan_file(
            &root,
            &destination_root,
            "payload.bin",
            (stat.dev(), stat.ino()),
            ".tentanas-transfer-test-0"
        )
        .is_err());
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn scan_prunes_paths_without_descending() {
        let root = unique("prune");
        fs::create_dir_all(root.join("foto/deep")).unwrap();
        fs::write(root.join("foto/deep/keep.bin"), b"keep").unwrap();
        fs::write(root.join("move.bin"), b"move").unwrap();
        let visited = std::cell::RefCell::new(Vec::new());
        let entries = scan_cache(&root, |path| {
            visited.borrow_mut().push(path.to_string());
            path == "foto"
        })
        .unwrap();
        let mut visited = visited.into_inner();
        visited.sort();
        assert_eq!(visited, vec!["foto".to_string(), "move.bin".to_string()]);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path().to_string())
                .collect::<Vec<_>>(),
            vec!["move.bin".to_string()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unlink_refuses_replaced_leaf_under_pinned_parent() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let mut replaced = false;
        let result = transfer_file(&source_root, &destination_root, &mut file, |record| {
            if record.phase == TransferFilePhase::UnlinkIntent && !replaced {
                fs::rename(
                    source_root.join("payload.bin"),
                    source_root.join("original.bin"),
                )
                .unwrap();
                fs::write(source_root.join("payload.bin"), b"replacement").unwrap();
                replaced = true;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(source_root.join("payload.bin")).unwrap(),
            b"replacement"
        );
        assert_eq!(
            fs::read(source_root.join("original.bin")).unwrap(),
            b"source"
        );
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn prepare_parents_creates_nested_directories_with_source_metadata() {
        let (source_root, destination_root) = fixture();
        fs::create_dir_all(source_root.join("nested/inner")).unwrap();
        fs::write(source_root.join("nested/inner/payload.bin"), b"nested payload").unwrap();
        fs::write(source_root.join("nested/inner/second.bin"), b"second").unwrap();
        set_xattr(&source_root.join("nested"), "user.tentanas", &[0, 1, 2, 255]);
        fs::set_permissions(source_root.join("nested/inner"), fs::Permissions::from_mode(0o705))
            .unwrap();
        fs::set_permissions(source_root.join("nested"), fs::Permissions::from_mode(0o750))
            .unwrap();
        let mut file = planned(&source_root, &destination_root, "nested/inner/payload.bin");
        let mut intents = Vec::new();
        prepare_parents(&source_root, &destination_root, &mut file, |record| {
            let reopened: TransferFile =
                serde_json::from_slice(&serde_json::to_vec(record).unwrap()).unwrap();
            intents.push(reopened.directory_intent);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            intents,
            vec![
                Some("nested".to_string()),
                None,
                Some("nested/inner".to_string()),
                None
            ]
        );
        assert_eq!(mode(&destination_root.join("nested")), 0o750);
        assert_eq!(mode(&destination_root.join("nested/inner")), 0o705);
        assert_eq!(
            get_xattr(&destination_root.join("nested"), "user.tentanas"),
            vec![0, 1, 2, 255]
        );
        let source_meta = fs::metadata(source_root.join("nested")).unwrap();
        let destination_meta = fs::metadata(destination_root.join("nested")).unwrap();
        assert_eq!(
            (source_meta.uid(), source_meta.gid()),
            (destination_meta.uid(), destination_meta.gid())
        );
        transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).unwrap();
        assert_eq!(
            fs::read(destination_root.join("nested/inner/payload.bin")).unwrap(),
            b"nested payload"
        );
        let mut second = planned(&source_root, &destination_root, "nested/inner/second.bin");
        let mut calls = 0;
        prepare_parents(&source_root, &destination_root, &mut second, |_| {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 0, "istniejące zgodne katalogi nie wymagają zapisu");
        fs::set_permissions(source_root.join("nested"), fs::Permissions::from_mode(0o700))
            .unwrap();
        fs::set_permissions(
            destination_root.join("nested"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn prepare_parents_adopts_only_its_own_interrupted_directory() {
        let (source_root, destination_root) = fixture();
        fs::create_dir(source_root.join("nested")).unwrap();
        fs::set_permissions(source_root.join("nested"), fs::Permissions::from_mode(0o755))
            .unwrap();
        fs::write(source_root.join("nested/payload.bin"), b"payload").unwrap();
        let planned_file = planned(&source_root, &destination_root, "nested/payload.bin");

        // A crash after mkdirat: the private 0700 directory named by the intent.
        fs::create_dir(destination_root.join("nested")).unwrap();
        fs::set_permissions(
            destination_root.join("nested"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut file = planned_file.clone();
        file.directory_intent = Some("nested".into());
        prepare_parents(&source_root, &destination_root, &mut file, |_| Ok(())).unwrap();
        assert_eq!(mode(&destination_root.join("nested")), 0o755);
        assert_eq!(file.directory_intent, None);

        // Without an intent, a directory with other access is never adopted.
        fs::remove_dir(destination_root.join("nested")).unwrap();
        fs::create_dir(destination_root.join("nested")).unwrap();
        fs::set_permissions(
            destination_root.join("nested"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut foreign = planned_file.clone();
        assert!(prepare_parents(&source_root, &destination_root, &mut foreign, |_| Ok(())).is_err());
        assert_eq!(mode(&destination_root.join("nested")), 0o700);
        let identity = (
            planned_file.source_identity.device,
            planned_file.source_identity.inode,
        );
        assert!(plan_file(
            &source_root,
            &destination_root,
            "nested/payload.bin",
            identity,
            ".tentanas-transfer-test-1"
        )
        .is_err());

        // An intent never adopts a directory that already holds something.
        fs::write(destination_root.join("nested/foreign.bin"), b"foreign").unwrap();
        foreign.directory_intent = Some("nested".into());
        assert!(prepare_parents(&source_root, &destination_root, &mut foreign, |_| Ok(())).is_err());
        assert_eq!(mode(&destination_root.join("nested")), 0o700);
        assert_eq!(
            fs::read(source_root.join("nested/payload.bin")).unwrap(),
            b"payload"
        );
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn symlinked_destination_parent_is_refused_without_writing_outside() {
        let (source_root, destination_root) = fixture();
        let outside = unique("outside-parent");
        fs::create_dir(source_root.join("nested")).unwrap();
        fs::write(source_root.join("nested/payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "nested/payload.bin");
        symlink(&outside, destination_root.join("nested")).unwrap();
        let identity = (file.source_identity.device, file.source_identity.inode);
        let error = plan_file(
            &source_root,
            &destination_root,
            "nested/payload.bin",
            identity,
            ".tentanas-transfer-test-1",
        )
        .expect_err("symlinkowany rodzic celu");
        assert!(error.starts_with("katalog docelowy nested"), "{error}");
        assert!(prepare_parents(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        assert!(transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(
            fs::read(source_root.join("nested/payload.bin")).unwrap(),
            b"payload"
        );
        for path in [source_root, destination_root, outside] {
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn open_descriptor_of_other_process_is_reported() {
        let root = unique("open");
        let path = root.join("held.bin");
        fs::write(&path, b"held").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(File::open(&path).unwrap())
            .spawn()
            .unwrap();
        let identities = open_file_identities_of(&[child.id()], child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(identities
            .unwrap()
            .contains(&(metadata.dev(), metadata.ino())));
        // A daemon whose descriptors cannot be read fails closed.
        assert!(open_file_identities_of(&[child.id()], child.id()).is_err());
        assert!(open_file_identities_of(&[], child.id()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn default_acl() -> Vec<u8> {
        // POSIX ACL xattr v2: user::rwx, user:65534:rwx, group::r-x, mask::rwx, other::r-x.
        let mut value = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (0x01u16, 7u16, u32::MAX),
            (0x02, 7, 65534),
            (0x04, 5, u32::MAX),
            (0x10, 7, u32::MAX),
            (0x20, 5, u32::MAX),
        ] {
            value.extend_from_slice(&tag.to_le_bytes());
            value.extend_from_slice(&perm.to_le_bytes());
            value.extend_from_slice(&id.to_le_bytes());
        }
        value
    }

    fn acl_names(path: &Path) -> Vec<String> {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut names = vec![0u8; 4096];
        let size = unsafe { libc::listxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        assert!(size >= 0, "{}", std::io::Error::last_os_error());
        names[..size as usize]
            .split(|byte| *byte == 0)
            .filter(|name| name.starts_with(b"system.posix_acl"))
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect()
    }

    #[test]
    fn unpinned_temporary_is_adopted_only_in_its_fresh_state() {
        // A crash right after O_CREAT: the fresh 0600 file exists, its pin was never written.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let temporary = destination_root.join(file.temporary.as_ref().unwrap());
        fs::File::create(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
        transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).expect("adopcja");
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        assert!(!temporary.exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();

        // Anything else under that name is never adopted or overwritten.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let temporary = destination_root.join(file.temporary.as_ref().unwrap());
        fs::write(&temporary, b"foreign").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"foreign");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn copy_under_an_inherited_default_acl_equals_its_source() {
        let (source_root, destination_root) = fixture();
        fs::create_dir(source_root.join("nested")).unwrap();
        fs::write(source_root.join("nested/payload.bin"), b"payload").unwrap();
        set_xattr(&destination_root, "system.posix_acl_default", &default_acl());
        let mut file = planned(&source_root, &destination_root, "nested/payload.bin");
        prepare_parents(&source_root, &destination_root, &mut file, |_| Ok(())).expect("katalog");
        assert!(acl_names(&destination_root.join("nested")).is_empty(), "odziedziczone ACL katalogu");
        transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).expect("transfer");
        assert!(acl_names(&destination_root.join("nested/payload.bin")).is_empty(), "odziedziczone ACL pliku");
        assert_eq!(fs::read(destination_root.join("nested/payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn temporary_stays_private_until_the_final_mode_is_set() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("tool.bin"), b"tool").unwrap();
        fs::set_permissions(source_root.join("tool.bin"), fs::Permissions::from_mode(0o4750)).unwrap();
        let mut file = planned(&source_root, &destination_root, "tool.bin");
        let temporary = destination_root.join(file.temporary.as_ref().unwrap());
        let mut private_seen = false;
        transfer_file(&source_root, &destination_root, &mut file, |record| {
            if record.phase == TransferFilePhase::CopyIntent && record.temporary_pin.is_some() {
                assert_eq!(mode(&temporary), 0o600, "kopia w toku jest prywatna");
                private_seen = true;
            }
            Ok(())
        })
        .expect("transfer");
        assert!(private_seen);
        assert_eq!(mode(&destination_root.join("tool.bin")), 0o4750, "bity set-id po fchown");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn roll_back_removes_only_the_pinned_copy_of_an_untouched_source() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let temporary = destination_root.join(file.temporary.as_ref().unwrap());
        assert!(transfer_file(&source_root, &destination_root, &mut file, |record| {
            if record.phase == TransferFilePhase::RenameIntent {
                return Err("wstrzymanie przed zmianą nazwy".into());
            }
            Ok(())
        })
        .is_err());
        assert!(temporary.exists());
        roll_back(&source_root, &destination_root, &file).expect("wycofanie");
        assert!(!temporary.exists());
        assert!(!destination_root.join("payload.bin").exists());
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");

        // A file under the temporary name that is not the pinned inode stays.
        fs::write(&temporary, b"foreign").unwrap();
        assert!(roll_back(&source_root, &destination_root, &file).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"foreign");
        fs::remove_file(&temporary).unwrap();

        // After the rename is confirmed the record can only go forward.
        let mut renamed = planned(&source_root, &destination_root, "payload.bin");
        assert!(transfer_file(&source_root, &destination_root, &mut renamed, |record| {
            if record.phase == TransferFilePhase::RenameConfirmed {
                return Err("wstrzymanie po zmianie nazwy".into());
            }
            Ok(())
        })
        .is_err());
        assert!(roll_back(&source_root, &destination_root, &renamed).is_err());
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn entry_exists_resolves_beneath_the_branch_without_symlinks() {
        let root = unique("exists");
        let outside = unique("exists-outside");
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/b.bin"), b"b").unwrap();
        fs::write(outside.join("c.bin"), b"c").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        assert!(entry_exists(&root, "a/b.bin").unwrap());
        assert!(!entry_exists(&root, "a/missing.bin").unwrap());
        assert!(!entry_exists(&root, "missing/b.bin").unwrap());
        assert!(entry_exists(&root, "link").unwrap());
        assert!(entry_exists(&root, "link/c.bin").is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    fn stopped_at(source_root: &Path, destination_root: &Path, stop: TransferFilePhase) -> TransferFile {
        let mut file = planned(source_root, destination_root, "payload.bin");
        assert!(transfer_file(source_root, destination_root, &mut file, |record| {
            if record.phase == stop {
                return Err("zatrzymanie testowe".into());
            }
            Ok(())
        })
        .is_err());
        assert_eq!(file.phase, stop);
        file
    }

    #[test]
    fn unreadable_source_is_finished_by_identity_only_after_the_rename() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        fail_content_reads(None);
        assert!(matches!(end, Ok(TransferEnd::SourceUnreadable(ref error)) if error.contains("os error 5")), "{end:?}");
        assert_eq!(file.phase, TransferFilePhase::Done);
        assert!(!source_root.join("payload.bin").exists());
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn identity_finish_is_refused_before_the_rename_and_for_other_errors() {
        // Before the rename an unreadable source is an error, never an unlink.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::CopyConfirmed);
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        let error = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()))
            .expect_err("przed zmianą nazwy");
        fail_content_reads(None);
        assert!(error.contains("os error 5"), "{error}");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        assert!(!destination_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();

        // After the rename, a source that changed is not a read error.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("payload.bin"))
            .unwrap()
            .write_all(b"+")
            .unwrap();
        assert!(transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload+");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();

        // An unreadable source whose inode metadata no longer matches is kept too.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        fs::set_permissions(source_root.join("payload.bin"), fs::Permissions::from_mode(0o600)).unwrap();
        file.source_identity.mode = 0o640;
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        assert!(transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        fail_content_reads(None);
        assert!(source_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn media_read_errors_are_exactly_the_medium_failures() {
        for errno in [libc::EIO, libc::ENXIO, libc::ENODATA, libc::EBADMSG, libc::EUCLEAN, libc::EREMOTEIO] {
            assert!(media_read_error(&std::io::Error::from_raw_os_error(errno)), "{errno}");
        }
        for errno in [libc::EACCES, libc::EPERM, libc::ENOENT, libc::EINTR, libc::ENOSPC, libc::EBADF, libc::EINVAL] {
            assert!(!media_read_error(&std::io::Error::from_raw_os_error(errno)), "{errno}");
        }
        assert!(!media_read_error(&std::io::Error::other("bez errno")));
    }

    #[test]
    fn scan_reports_unreadable_directories_listings_and_entries_and_walks_on() {
        let root = unique("faults");
        fs::write(root.join("a.bin"), b"a").unwrap();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/b.bin"), b"b").unwrap();
        fs::create_dir(root.join("unlistable")).unwrap();
        fs::write(root.join("unlistable/c.bin"), b"c").unwrap();
        fs::write(root.join("d.bin"), b"d").unwrap();
        fs::create_dir(root.join("deep")).unwrap();
        fs::write(root.join("deep/e.bin"), b"e").unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let unlistable = fs::metadata(root.join("unlistable")).unwrap();
        fail_listing_of(Some((unlistable.dev(), unlistable.ino())));
        fail_stat_of(Some("d.bin"));
        // Root ignores mode 0: the permission case is only real for a user.
        let as_user = unsafe { libc::geteuid() } != 0;
        let entries = scan_cache(&root, |_| false);
        fail_listing_of(None);
        fail_stat_of(None);
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        let entries = entries.expect("skan idzie dalej");
        let files: Vec<&str> = entries
            .iter()
            .filter_map(|entry| match entry {
                ScanEntry::File(file) => Some(file.path.as_str()),
                ScanEntry::Refused { .. } => None,
            })
            .collect();
        if !as_user {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert_eq!(files, vec!["a.bin", "deep/e.bin"]);
        let refused: Vec<(&str, &str)> = entries
            .iter()
            .filter_map(|entry| match entry {
                ScanEntry::Refused { path, reason } => Some((path.as_str(), reason.as_str())),
                ScanEntry::File(_) => None,
            })
            .collect();
        assert_eq!(refused.len(), 3, "{refused:?}");
        assert!(refused.iter().any(|(path, reason)| *path == "locked" && reason.starts_with("katalog niedostępny")));
        assert!(refused.iter().any(|(path, reason)| *path == "unlistable" && reason.starts_with("listowanie katalogu")));
        assert!(refused.iter().any(|(path, reason)| *path == "d.bin" && reason.starts_with("stat:")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identity_finish_survives_a_crash_after_the_unlink() {
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        // Only what `persist` accepted survives the power loss.
        let mut durable = serde_json::to_vec(&file).unwrap();
        let crashed = transfer_file(&source_root, &destination_root, &mut file, |record| {
            if record.phase == TransferFilePhase::UnlinkConfirmed {
                return Err("utrata zasilania po unlinku".into());
            }
            durable = serde_json::to_vec(record).unwrap();
            Ok(())
        });
        fail_content_reads(None);
        assert!(crashed.is_err());
        assert!(!source_root.join("payload.bin").exists(), "źródło usunięte przed awarią");
        let mut reopened: TransferFile = serde_json::from_slice(&durable).unwrap();
        assert_eq!(reopened.phase, TransferFilePhase::UnlinkIntent);
        assert!(reopened.unread_source.is_some(), "zamiar zapisany przed unlinkiem");
        let end = transfer_file(&source_root, &destination_root, &mut reopened, |_| Ok(()));
        assert!(matches!(end, Ok(TransferEnd::SourceUnreadable(_))), "{end:?}");
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn every_persist_boundary_reopens_safely() {
        let phases = [
            TransferFilePhase::CopyIntent,
            TransferFilePhase::CopyConfirmed,
            TransferFilePhase::RenameIntent,
            TransferFilePhase::RenameConfirmed,
            TransferFilePhase::UnlinkIntent,
            TransferFilePhase::UnlinkConfirmed,
            TransferFilePhase::Done,
        ];
        for phase in phases {
            let target_calls = if phase == TransferFilePhase::CopyIntent {
                vec![0usize, 1usize]
            } else {
                vec![0usize]
            };
            for target_call in target_calls {
                for before_write in [true, false] {
                    let (source_root, destination_root) = fixture();
                    fs::write(source_root.join("payload.bin"), b"boundary payload").unwrap();
                    let mut file = planned(&source_root, &destination_root, "payload.bin");
                    let record_path = unique("boundary").join("record.json");
                    let initial = serde_json::to_vec(&file).unwrap();
                    fs::write(&record_path, &initial).unwrap();
                    let source_before = identity_fd(
                        &open_relative(
                            std::fs::File::open(&source_root).unwrap().as_raw_fd(),
                            "payload.bin",
                            libc::O_RDONLY | libc::O_CLOEXEC,
                            0,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    let mut interrupted = false;
                    let mut phase_call = 0usize;
                    let result =
                        transfer_file(&source_root, &destination_root, &mut file, |record| {
                            let current_call = if record.phase == phase {
                                let current_call = phase_call;
                                phase_call += 1;
                                Some(current_call)
                            } else {
                                None
                            };
                            if current_call == Some(target_call) && !interrupted {
                                interrupted = true;
                                if before_write {
                                    return Err("przerwanie przed trwałym zapisem".into());
                                }
                            }
                            let bytes = serde_json::to_vec(record).unwrap();
                            let mut output = File::create(&record_path).unwrap();
                            output.write_all(&bytes).unwrap();
                            output.sync_all().unwrap();
                            if current_call == Some(target_call) && interrupted && !before_write {
                                return Err("przerwanie po trwałym zapisie".into());
                            }
                            Ok(())
                        });
                    assert!(result.is_err(), "oczekiwano przerwania: phase={phase:?}, before_write={before_write}, target_call={target_call}");
                    assert!(interrupted, "nie osiągnięto granicy: phase={phase:?}, before_write={before_write}, target_call={target_call}");
                    if phase == TransferFilePhase::CopyIntent && target_call == 1 && before_write {
                        // Crash between creating the temporary and pinning it.
                        assert_eq!(fs::read(&record_path).unwrap(), initial);
                        assert!(destination_root.join(file.temporary.as_ref().unwrap()).exists());
                        assert!(!destination_root.join("payload.bin").exists());
                        assert_eq!(
                            identity_fd(
                                &open_relative(
                                    std::fs::File::open(&source_root).unwrap().as_raw_fd(),
                                    "payload.bin",
                                    libc::O_RDONLY | libc::O_CLOEXEC,
                                    0,
                                )
                                .unwrap()
                            )
                            .unwrap(),
                            source_before
                        );
                    }
                    let bytes = fs::read(&record_path).unwrap();
                    let mut reopened: TransferFile = serde_json::from_slice(&bytes).unwrap();
                    let resumed =
                        transfer_file(&source_root, &destination_root, &mut reopened, |record| {
                            let bytes = serde_json::to_vec(record).unwrap();
                            let mut output = File::create(&record_path).unwrap();
                            output.write_all(&bytes).unwrap();
                            output.sync_all().unwrap();
                            Ok(())
                        });
                    resumed.unwrap_or_else(|error| panic!("wznowienie nieudane: phase={phase:?}, before_write={before_write}, target_call={target_call}: {error}"));
                    assert_eq!(reopened.phase, TransferFilePhase::Done, "nie zakończono transferu: phase={phase:?}, before_write={before_write}, target_call={target_call}");
                    assert_eq!(
                        fs::read(destination_root.join("payload.bin")).unwrap(),
                        b"boundary payload"
                    );
                    assert!(!source_root.join("payload.bin").exists());
                    fs::remove_dir_all(record_path.parent().unwrap()).unwrap();
                    fs::remove_dir_all(source_root).unwrap();
                    fs::remove_dir_all(destination_root).unwrap();
                }
            }
        }
    }
}
