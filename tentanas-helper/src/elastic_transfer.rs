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
//!
//! # Moving a file while the share stays writable
//!
//! The union is never made read-only. What keeps a client's write from being
//! lost is a kernel write LEASE (`F_SETLEASE F_WRLCK`) the helper holds on the
//! cache original and on its copy. The kernel grants such a lease only while
//! no other open file description of the inode exists anywhere on the node — a
//! descriptor of any process in any namespace, or a mapping — and while it
//! stands, every `open(2)` and `truncate(2)` of the inode runs into it
//! (`do_dentry_open` → `break_lease`), so the helper sees the break and the
//! opener waits until the helper lets go. The content of a leased file cannot
//! change without such a break; only path-based metadata calls (chmod, chown,
//! xattrs, utimes) can, and those are compared. Per file, each phase written
//! to the journal before its syscall:
//!
//! 1. `CopyIntent` — lease the original (refused: someone has it open, the
//!    file is left for a later run); measure it against the record; copy it
//!    FD-relatively to `.tentanas-transfer-<op>-<seq>` in the data branch
//!    root, leased too, with owner, mode, times, ACLs and xattrs; every chunk
//!    is written out to the medium before both leases are polled, and every
//!    later hash of either file polls both leases per chunk too. A client
//!    opening either file therefore waits for one chunk of work, not for a
//!    whole copy, fsync or hash — the only other waits under a lease are the
//!    journal writes and directory fsyncs between the steps. The open breaks
//!    the lease: the copy is withdrawn at once and the client's open proceeds
//!    on the untouched original. Clients see: the original. The temporary is
//!    a dotfile in the share root; SMB shares veto it (`SMB_VETO_FILES`), an
//!    NFS client can still see it.
//! 2. `CopyConfirmed`/`RenameIntent` — rename the temporary to the file's path
//!    on the data branch (`RENAME_NOREPLACE`). `RenameConfirmed` pins it.
//!    Clients see: still the original. mergerfs lists the cache branch first
//!    and every open resolves there (`ff`); `func.getattr=newest` finds two
//!    inodes with the same mtime and reports the first. Path-based actions
//!    (`epall`) reach both copies, which is why metadata is compared.
//! 3. `QuarantineIntent` — with both leases still standing, re-check the
//!    original's and the copy's metadata, then rename the original to
//!    `.tentanas-quarantine-<op>-<seq>` in the cache branch root: one
//!    `renameat2` on one filesystem. From this instant a lookup of the path
//!    finds no cache entry and resolves to the copy. `QuarantineConfirmed`
//!    records it. Clients see: the copy, byte-identical to the original.
//! 4. The decision. The original's lease still standing means nothing opened
//!    or truncated it since before the copy: `UnlinkIntent`, drop the copy's
//!    lease, check the original's lease one last time, unlink the quarantine
//!    name, `UnlinkConfirmed`, `Done`. A broken lease or changed metadata
//!    means somebody reached the original: `RestoreIntent` renames the
//!    quarantine back to the path (NOREPLACE), removes the copy only if its
//!    own lease proves nobody opened it and it is unchanged, and `Restored`
//!    leaves the original — with whatever the opener writes after the helper
//!    lets go — under its name for a later run. A copy that clients already
//!    use keeps the path and the original stays as the quarantine file: both
//!    are kept, and the record sticks for an admin. A quarantined original
//!    somebody removed ends the record as a move, with a notice.
//!
//! Crash recovery reopens both files, takes the leases again and measures the
//! content in full, then continues the same decision from the phase on disk:
//! before the quarantine rename the move is finished only if both files are
//! unchanged and unopened, after it the original is released only if it is,
//! and anything else is withdrawn. Every step finds its files by pinned
//! (device, inode), so a repeated step recognises its own earlier effect.
//!
//! Clients may have used the files between the crash and the reopen. After a
//! reboot the array's Restore settles the record BEFORE it publishes the
//! union, so nobody can; but a helper killed in the same boot leaves the union
//! serving until the next run. That is why a reversal never trusts the phase
//! alone: once the copy has owned the path, a copy no longer found under it
//! means a client replaced, renamed or deleted the file, and the original is
//! then never renamed back over the client's result — the record sticks with
//! both names.
//!
//! # The residual lost-write window
//!
//! A lease is met by an opener only in `do_dentry_open`, AFTER its path
//! lookup. Step 4 checks the original's lease a last time, calls `unlinkat`,
//! checks it once more and closes the descriptor. Writes are lost for exactly
//! one interleaving: an `open(2)` whose lookup resolved to the cache original —
//! by the file's path before step 3's rename, or by the quarantine name before
//! the `unlinkat` — and whose `do_dentry_open` runs AFTER that last check.
//! If it runs before the helper closes the descriptor, it waits on the lease,
//! the check after the unlink sees the break and the run reports it
//! (a notice on the run); if it runs after, nothing sees it. Either way its descriptor
//! refers to an unlinked inode and what it writes is gone. The same holds for a
//! path-based metadata change (chmod, chown, xattrs, utimes) whose lookup
//! resolved to the original and which lands after the last metadata check.
//! Content cannot change any other way while the lease stands.
//!
//! How wide it is: a lookup by path must precede the rename, so the opener has
//! to stall inside one `open(2)` across the rename, a directory fsync, two
//! journal writes and the checks (milliseconds) — normally the gap between its
//! lookup and `do_dentry_open` is microseconds. A lookup by the quarantine name
//! needs a client that listed the share root and opens that exact dotfile in
//! the instants before it is unlinked.
//!
//! The reversal has the mirror image of this window. It proves the copy
//! unopened and unchanged, renames the original back under the path, checks
//! the copy's lease ONCE more — an open that had resolved the path to the copy
//! before that rename is seen here, and any failure of that check gives the
//! path back to the copy with both files kept — and unlinks the copy with no
//! other check in between. An open that resolved the path to the copy before
//! the rename back and reaches the copy's lease only after that last check
//! (the gap is the `unlinkat` call itself) holds an unlinked copy: seen and
//! reported if it lands before the helper closes the copy, unseen after. The copy owned the path only since
//! step 3 and is byte-identical to the restored original, so what is lost is
//! that one client's writes through that one descriptor.
//!
//! Why the kernel does not let it close further: an open in progress holds a
//! dentry reference, not a descriptor or an open file description, so neither
//! a lease, `/proc/*/fd` nor fanotify can see it before `do_dentry_open`; and
//! an unlinked inode cannot be linked back (`linkat` refuses `i_nlink == 0`
//! unless the inode was created `O_TMPFILE`), so the helper cannot undo the
//! unlink once it notices.
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "linux")]
use std::fs::File;
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
    /// The original is about to be renamed to its quarantine name.
    QuarantineIntent,
    /// The original is under its quarantine name; the copy owns the path.
    QuarantineConfirmed,
    /// The quarantined original is about to be unlinked.
    UnlinkIntent,
    UnlinkConfirmed,
    Done,
    /// The move is being reversed: the original goes back under its path and
    /// the copy is removed.
    RestoreIntent,
    /// The move was reversed; the file waits on the cache for a later run.
    Restored,
}

/// The prefix of the copy's temporary name in the data branch root.
pub(crate) const TEMPORARY_PREFIX: &str = ".tentanas-transfer-";
/// The prefix of the original's quarantine name in the cache branch root.
pub(crate) const QUARANTINE_PREFIX: &str = ".tentanas-quarantine-";

/// Where the original waits while its copy takes over the path: the record's
/// temporary name under the quarantine prefix, so it needs no field of its own
/// and names the same operation and sequence.
pub(crate) fn quarantine_name(temporary: &str) -> Result<String, String> {
    temporary
        .strip_prefix(TEMPORARY_PREFIX)
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        .map(|rest| format!("{QUARANTINE_PREFIX}{rest}"))
        .ok_or_else(|| "nazwa tymczasowa bez prefiksu transferu".to_string())
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

/// POLICY: the helper is FORWARD-ONLY. `deny_unknown_fields` is what catches a
/// journal this helper did not write, and it stays — which also means a journal
/// written by a NEWER helper is deliberately rejected by an older binary, field
/// by field. That is the intended behaviour, not an oversight: there is no
/// schema versioning here and no migration path. Rolling the helper back to an
/// older build therefore requires stopping the array first, so nothing is left
/// holding a journal the older binary will refuse to read.
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
    /// The orphaned temporary of this record was VERIFIED as ours and deleted
    /// from the branch. `temporary` itself stays: the journal validator
    /// requires an in-flight record to name one, and a name that no longer
    /// resolves is still how a reader recognises which copy was removed. What
    /// this flag governs is what the record CLAIMS downstream — a stuck record
    /// built from it reports no temporary at all, so nothing reports an orphan
    /// this helper already cleaned up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub temporary_removed: bool,
    pub phase: TransferFilePhase,
}

/// How a record ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransferEnd {
    /// `Done`: the copy owns the path and the original is gone.
    Moved {
        /// After the rename the original could no longer be read (the error is
        /// kept); it was released on a stat-only identity match against the
        /// pinned copy, re-read from its medium.
        unreadable: Option<String>,
        /// What an admin must hear about this file although it moved: an open
        /// that reached the original's lease between the last check and the
        /// unlink (the residual window the module doc names), a quarantined
        /// original somebody else removed, an unlink whose directory entry
        /// could not be confirmed on disk.
        notices: Vec<String>,
    },
    /// `Restored`: the original is under its path again, the copy is gone,
    /// and the file waits for a later run. `reason` says why; `notices` as
    /// above, for the reversal.
    Withdrawn { reason: String, notices: Vec<String> },
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
    // A stuck record may set this before the closing write, and it only ever
    // costs bytes when true.
    pinned.temporary_removed = true;
    // The longest phase name the record can carry.
    pinned.phase = TransferFilePhase::QuarantineConfirmed;
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
    /// Whether the directory fsync AFTER an orphan unlink fails in this thread.
    static UNSYNCABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_listing_of(identity: Option<(u64, u64)>) {
    UNLISTABLE.with(|cell| cell.set(identity));
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_directory_fsync(on: bool) {
    UNSYNCABLE.with(|cell| cell.set(on));
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
    /// The measurement ran under this descriptor's own write lease and
    /// somebody opened the file: it stopped at once, so they do not wait.
    LeaseBroken,
    Other(String),
}

#[cfg(target_os = "linux")]
impl MeasureError {
    fn into_message(self) -> String {
        match self {
            Self::MediaRead(message) | Self::Other(message) => message,
            Self::LeaseBroken => "plik otwarty przez inny proces podczas pomiaru".into(),
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
    /// `(device, inode)` of every chunk `measure_fd` read in this test thread.
    static MEASURED_CHUNKS: std::cell::RefCell<Vec<(u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Called after each measured chunk with the file's identity and the
    /// leases the measurement polls.
    static MEASURE_HOOK: std::cell::RefCell<Option<MeasureHook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_os = "linux"))]
type MeasureHook = Box<dyn FnMut((u64, u64), &[&OwnedFd])>;

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// `(device, inode)` whose content reads fail with EIO in this test thread.
    static UNREADABLE: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
}

/// A moment at which a test may act on a file the state machine holds leased.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseMoment {
    /// After one chunk of the original was written to the temporary.
    CopyChunk,
    /// The original is under its quarantine name and leased, before the
    /// checks that decide whether it is released.
    BeforeRelease,
    /// A reversal just renamed the original back under its path; the copy is
    /// still leased and has not been checked again.
    AfterRenameBack,
}

#[cfg(all(test, target_os = "linux"))]
type LeaseHook = Box<dyn FnMut(LeaseMoment, &OwnedFd)>;

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static LEASE_HOOK: std::cell::RefCell<Option<LeaseHook>> = const { std::cell::RefCell::new(None) };
}

/// Installs (or clears) the calling test's action at a `LeaseMoment`. It is
/// handed the leased original, so it can wait for its own opener to reach the
/// lease instead of guessing at timing.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn on_lease_moment(hook: Option<LeaseHook>) {
    LEASE_HOOK.with(|cell| *cell.borrow_mut() = hook);
}

#[cfg(target_os = "linux")]
fn lease_moment(_moment: LeaseMoment, _original: &OwnedFd) {
    #[cfg(test)]
    LEASE_HOOK.with(|cell| {
        if let Some(hook) = cell.borrow_mut().as_mut() {
            hook(_moment, _original);
        }
    });
}

/// Makes content reads of one inode fail with EIO for the calling test.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn fail_content_reads(identity: Option<(u64, u64)>) {
    UNREADABLE.with(|cell| cell.set(identity));
}

/// Reads the whole file and its attributes. Every descriptor in `leases`
/// carries a write lease of the session, and all of them are checked between
/// chunks: a client opening EITHER file while the other is being read waits
/// one chunk, not the whole read.
#[cfg(target_os = "linux")]
fn measure_fd(fd: &OwnedFd, leases: &[&OwnedFd]) -> Result<TransferIdentity, MeasureError> {
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
    #[cfg(test)]
    let measured = (stat.st_dev as u64, stat.st_ino as u64);
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
        #[cfg(test)]
        {
            MEASURED_CHUNKS.with(|chunks| chunks.borrow_mut().push(measured));
            MEASURE_HOOK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook(measured, leases);
                }
            });
        }
        for lease in leases {
            if !lease_intact(lease)? {
                return Err(MeasureError::LeaseBroken);
            }
        }
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
    measure_fd(fd, &[]).map_err(MeasureError::into_message)
}

/// Makes the next read of `fd` come from the medium: dirty pages are written
/// back, then the clean ones are dropped from the page cache. The kernel
/// treats DONTNEED as advice: pages another process has mapped or locked stay
/// resident, so this guarantees a re-read from disk only for a file nobody
/// maps — which a write lease on `fd` proves, since a mapping is an open file
/// description the lease is refused for.
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
    rename_at_noreplace(
        source_parent.as_ref().map_or(rootfd, AsRawFd::as_raw_fd),
        source.rsplit_once('/').map_or(source, |(_, name)| name),
        target_parent.as_ref().map_or(rootfd, AsRawFd::as_raw_fd),
        target.rsplit_once('/').map_or(target, |(_, name)| name),
    )
    .map_err(|error| error.to_string())
}

/// Writes one written range out to the medium and waits for it, so a later
/// `fsync` of the file has almost nothing left to do.
#[cfg(target_os = "linux")]
fn flush_range(fd: i32, offset: u64, count: usize) -> Result<(), String> {
    let flags = libc::SYNC_FILE_RANGE_WAIT_BEFORE | libc::SYNC_FILE_RANGE_WRITE | libc::SYNC_FILE_RANGE_WAIT_AFTER;
    let offset = i64::try_from(offset).map_err(|_| "przesunięcie poza zakresem")?;
    let count = i64::try_from(count).map_err(|_| "rozmiar poza zakresem")?;
    if unsafe { libc::sync_file_range(fd, offset, count, flags) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// One `renameat2(RENAME_NOREPLACE)` of a single name between two directory
/// descriptors: the target is never replaced, and the caller reads the errno.
#[cfg(target_os = "linux")]
fn rename_at_noreplace(
    source_dirfd: i32,
    source: &str,
    target_dirfd: i32,
    target: &str,
) -> Result<(), std::io::Error> {
    for name in [source, target] {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(std::io::Error::other("nieprawidłowa nazwa wpisu"));
        }
    }
    let source = CString::new(source)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "źródło zawiera NUL"))?;
    let target = CString::new(target)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "cel zawiera NUL"))?;
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
        return Err(std::io::Error::last_os_error());
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
    if exists_at(source_root.as_raw_fd(), &quarantine_name(temporary)?)? {
        return Err("nazwa odsunięcia oryginału jest zajęta".into());
    }
    Ok(TransferFile {
        source: path.into(),
        destination: path.into(),
        temporary: Some(temporary.into()),
        temporary_identity: None,
        temporary_pin: None,
        // A record being planned has a temporary that exists, or is about to.
        temporary_removed: false,
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
/// `/proc`. The helper's own descriptors are left out. This is a snapshot for
/// choosing and counting what a run skips, not a barrier: what protects a file
/// while it moves is the write lease `transfer_file` holds on it.
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

/// What removing an orphaned temporary did.
// Not `Copy`: `Removed` carries the text of a directory fsync that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrphanCleanup {
    /// The file was verified as this record's own and unlinked. `fsync` carries
    /// a DIRECTORY fsync that failed AFTER the unlink: the file is gone either
    /// way, so that is a notice, never a reversal of the outcome.
    Removed { fsync: Option<String> },
    /// Nothing was there. An earlier crash, an earlier run or an admin already
    /// cleaned it up, and that is a clean outcome, not a failure.
    Absent,
}

/// Deletes the temporary a record is about to be STUCK on, and only when this
/// helper can prove the file is its own.
///
/// The proof is the record's own pins re-read from the branch: the pinned
/// (device, inode) must still be what lies under the temporary name, it must
/// be a regular file with exactly one link, and where the record pinned a size
/// that size must match. An unpinned copy is accepted only in the fresh,
/// private state `fresh_temporary` describes — the same standard `roll_back`
/// applies, and for the same reason.
///
/// The content hash is NOT re-read. Re-hashing would mean reading the whole
/// copy back off a branch whose I/O has just failed, which is when it is least
/// trustworthy; (device, inode) under `RESOLVE_NO_SYMLINKS` is the identity the
/// transfer path itself trusts to unlink a source.
///
/// Anything that cannot be proved deletes NOTHING and says why. A missing file
/// is `Absent`: the caller must treat it as success, because an orphan that is
/// already gone is exactly the state this function exists to reach.
#[cfg(target_os = "linux")]
pub(crate) fn remove_orphan_temporary(
    destination_root: &Path,
    file: &TransferFile,
) -> Result<OrphanCleanup, String> {
    let temporary = file
        .temporary
        .as_deref()
        .ok_or("brak trwałej ścieżki tymczasowej")?;
    // One component, validated when the record was planned, so `unlinkat`
    // relative to the branch FD cannot reach outside it. `stat_at_io` refuses
    // a name with a separator or a special component and never follows a
    // symlink; the branch itself is opened with RESOLVE_NO_SYMLINKS.
    let destination_root = open_root_directory(destination_root)?;
    let stat = match stat_at_io(destination_root.as_raw_fd(), temporary) {
        Ok(stat) => stat,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            return Ok(OrphanCleanup::Absent)
        }
        Err(error) => return Err(error.to_string()),
    };
    if (stat.st_mode as u32 & libc::S_IFMT as u32) != libc::S_IFREG as u32 || stat.st_nlink != 1 {
        return Err("pod nazwą tymczasową nie leży zwykły plik tej operacji".into());
    }
    let ours = match file.temporary_pin {
        Some(pin) => (stat.st_dev as u64, stat.st_ino as u64) == pin,
        None => fresh_temporary(&stat),
    };
    if !ours {
        return Err("pod nazwą tymczasową leży obcy plik".into());
    }
    if let Some(pinned) = file.temporary_identity.as_ref() {
        if pinned.size != stat.st_size as u64 {
            return Err("kopia tymczasowa ma inny rozmiar niż przypięty".into());
        }
    }
    let name = CString::new(temporary).map_err(|_| "nazwa zawiera NUL".to_string())?;
    if unsafe { libc::unlinkat(destination_root.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        // Lost a race with another cleanup: the orphan is gone either way.
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(OrphanCleanup::Absent);
        }
        return Err(error.to_string());
    }
    // THE OUTCOME IS DECIDED HERE: `unlinkat` returned 0, so the orphan is
    // gone. Durability of the directory entry is a separate question, and it
    // must not be able to rewrite this into "not deleted" — a branch failing
    // I/O is precisely WHY a record sticks, so EIO here is the expected case,
    // not an exotic one. Reporting a phantom orphan would send an operator
    // hunting for a file that does not exist, while the stuck record, the skip
    // set and the syslog line all went on naming it. The worst case of an
    // un-fsynced unlink is a crash resurrecting the name, which the
    // ENOENT-tolerant cleanup already treats as clean.
    let fsync = fsync_after_unlink(destination_root.as_raw_fd()).err();
    Ok(OrphanCleanup::Removed { fsync })
}

/// The directory fsync that follows a successful unlink. Split out so a test
/// can fail it while the unlink still succeeds — hooking `fsync_fd` itself
/// would also fire on the transfer path's own fsyncs of the same directory.
#[cfg(target_os = "linux")]
fn fsync_after_unlink(fd: i32) -> Result<(), String> {
    #[cfg(test)]
    if UNSYNCABLE.with(|cell| cell.get()) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO).to_string());
    }
    fsync_fd(fd)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn remove_orphan_temporary(_: &Path, _: &TransferFile) -> Result<OrphanCleanup, String> {
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// Why a step of the state machine stopped: a decision to reverse the move,
/// or a failure the caller handles.
#[cfg(target_os = "linux")]
enum Stop {
    /// The file must stay on the cache; the text is why.
    Withdraw(String),
    Fail(String),
}

#[cfg(target_os = "linux")]
impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Fail(message)
    }
}

#[cfg(target_os = "linux")]
impl From<&str> for Stop {
    fn from(message: &str) -> Self {
        Self::Fail(message.into())
    }
}

/// A descriptor this session holds under its own write lease.
#[cfg(target_os = "linux")]
struct Leased {
    fd: OwnedFd,
    /// The content was measured while this lease stood: as long as the lease
    /// still stands, nobody has opened or truncated the inode since, so only
    /// its path-reachable metadata can differ from that measurement.
    measured: bool,
}

/// One invocation's view of a record: both branch roots and the leases it
/// holds. Dropping it closes the descriptors, which releases the leases.
#[cfg(target_os = "linux")]
struct Session {
    source_root: OwnedFd,
    destination_root: OwnedFd,
    original: Option<Leased>,
    copy: Option<Leased>,
    /// The original could not be read after the copy was renamed into place,
    /// and its stat identity still matched.
    unreadable: Option<String>,
    /// Notices for the record's end (`TransferEnd`).
    notices: Vec<String>,
}

/// Where the pinned original is on the cache branch.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Path,
    Quarantine,
}

/// Where the pinned copy is on the data branch.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyPlace {
    Temporary,
    Path,
}

#[cfg(target_os = "linux")]
fn quiet_lease_breaks() {
    static IGNORED: std::sync::Once = std::sync::Once::new();
    // A broken lease is announced with SIGIO, whose default action ends the
    // process. The state machine polls F_GETLEASE instead, and the owner is
    // cleared after every F_SETLEASE; ignoring the signal only covers the
    // instant between the two calls.
    IGNORED.call_once(|| unsafe {
        libc::signal(libc::SIGIO, libc::SIG_IGN);
    });
}

/// Takes a write lease on `fd`. `Ok(false)` means another open file
/// description of the inode exists — an open descriptor or a mapping, in any
/// process and any namespace — which is exactly what the kernel refuses a
/// write lease for.
#[cfg(target_os = "linux")]
fn take_lease(fd: &OwnedFd) -> Result<bool, String> {
    quiet_lease_breaks();
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETLEASE, libc::F_WRLCK) } != 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EBUSY) | Some(libc::ETXTBSY) => Ok(false),
            _ => Err(format!("lease: {error}")),
        };
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETOWN, 0) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(true)
}

/// Whether the write lease on `fd` still stands. Any open or truncate of the
/// inode since it was taken turns it into a pending break.
#[cfg(target_os = "linux")]
fn lease_intact(fd: &OwnedFd) -> Result<bool, String> {
    let lease = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETLEASE) };
    if lease < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(lease == libc::F_WRLCK)
}

#[cfg(target_os = "linux")]
fn temporary_of(file: &TransferFile) -> Result<&str, String> {
    file.temporary
        .as_deref()
        .ok_or_else(|| "brak trwałej ścieżki tymczasowej".to_string())
}

/// Whether `name` in `dirfd` is the pinned regular inode; a missing name is
/// `false`, never an error.
#[cfg(target_os = "linux")]
fn pinned_at(dirfd: i32, name: &str, pin: (u64, u64)) -> Result<bool, String> {
    match stat_at_io(dirfd, name) {
        Ok(stat) => Ok((stat.st_dev as u64, stat.st_ino as u64) == pin
            && (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// The parent directory of `path` beneath `rootfd` and its last component,
/// or `None` when that directory no longer exists.
#[cfg(target_os = "linux")]
fn existing_parent(rootfd: i32, path: &str) -> Result<Option<(OwnedFd, String)>, String> {
    let parent = parent_path(path)?;
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name).to_string();
    if parent.is_empty() {
        return Ok(Some((duplicate_fd(rootfd)?, name)));
    }
    match open_relative_io(
        rootfd,
        parent,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(directory) => Ok(Some((directory, name))),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(target_os = "linux")]
fn original_pin(file: &TransferFile) -> (u64, u64) {
    (file.source_identity.device, file.source_identity.inode)
}

#[cfg(target_os = "linux")]
fn locate_original(session: &Session, file: &TransferFile) -> Result<Option<Place>, String> {
    let pin = original_pin(file);
    let at_path = match existing_parent(session.source_root.as_raw_fd(), &file.source)? {
        Some((parent, name)) => pinned_at(parent.as_raw_fd(), &name, pin)?,
        None => false,
    };
    let quarantined = pinned_at(
        session.source_root.as_raw_fd(),
        &quarantine_name(temporary_of(file)?)?,
        pin,
    )?;
    match (at_path, quarantined) {
        (true, false) => Ok(Some(Place::Path)),
        (false, true) => Ok(Some(Place::Quarantine)),
        (false, false) => Ok(None),
        (true, true) => Err("oryginał jest pod ścieżką i pod nazwą odsunięcia".into()),
    }
}

/// Where the copy pinned at its creation is, if it is still there.
#[cfg(target_os = "linux")]
fn locate_copy(session: &Session, file: &TransferFile) -> Result<Option<CopyPlace>, String> {
    let Some(pin) = file.temporary_pin else {
        return Ok(None);
    };
    let destination_root = session.destination_root.as_raw_fd();
    let at_temporary = pinned_at(destination_root, temporary_of(file)?, pin)?;
    let at_path = match existing_parent(destination_root, &file.destination)? {
        Some((parent, name)) => pinned_at(parent.as_raw_fd(), &name, pin)?,
        None => false,
    };
    match (at_temporary, at_path) {
        (true, false) => Ok(Some(CopyPlace::Temporary)),
        (false, true) => Ok(Some(CopyPlace::Path)),
        (false, false) => Ok(None),
        (true, true) => Err("kopia jest pod nazwą tymczasową i pod ścieżką".into()),
    }
}

/// Opens the original where it is and leases it, once per session.
#[cfg(target_os = "linux")]
fn hold_original(session: &mut Session, file: &TransferFile, place: Place) -> Result<(), Stop> {
    if session.original.is_some() {
        return Ok(());
    }
    let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC;
    let fd = match place {
        Place::Path => {
            let (parent, name) = existing_parent(session.source_root.as_raw_fd(), &file.source)?
                .ok_or("katalog oryginału zniknął")?;
            open_relative(parent.as_raw_fd(), &name, flags, 0)?
        }
        Place::Quarantine => open_relative(
            session.source_root.as_raw_fd(),
            &quarantine_name(temporary_of(file)?)?,
            flags,
            0,
        )?,
    };
    let stat = stat_fd(fd.as_raw_fd())?;
    if (stat.st_dev as u64, stat.st_ino as u64) != original_pin(file) {
        return Err(Stop::Withdraw("plik zmienił się od skanu".into()));
    }
    if !take_lease(&fd)? {
        return Err(Stop::Withdraw("plik otwarty przez inny proces".into()));
    }
    session.original = Some(Leased { fd, measured: false });
    Ok(())
}

/// Opens the copy where it is and leases it, once per session.
#[cfg(target_os = "linux")]
fn hold_copy(session: &mut Session, file: &TransferFile) -> Result<CopyPlace, Stop> {
    let place = locate_copy(session, file)?.ok_or("kopia zniknęła z dysku danych")?;
    if session.copy.is_some() {
        return Ok(place);
    }
    let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC;
    let destination_root = session.destination_root.as_raw_fd();
    let fd = match place {
        CopyPlace::Temporary => open_relative(destination_root, temporary_of(file)?, flags, 0)?,
        CopyPlace::Path => {
            let (parent, name) = existing_parent(destination_root, &file.destination)?
                .ok_or("katalog kopii zniknął")?;
            open_relative(parent.as_raw_fd(), &name, flags, 0)?
        }
    };
    let stat = stat_fd(fd.as_raw_fd())?;
    if Some((stat.st_dev as u64, stat.st_ino as u64)) != file.temporary_pin {
        return Err(Stop::Fail("kopia zmieniła przypięty inode".into()));
    }
    if !take_lease(&fd)? {
        return Err(Stop::Withdraw("kopia na dysku danych otwarta przez inny proces".into()));
    }
    session.copy = Some(Leased { fd, measured: false });
    Ok(place)
}

/// The original's stat identity and attributes against the record.
#[cfg(target_os = "linux")]
fn original_metadata_matches(fd: &OwnedFd, identity: &TransferIdentity) -> Result<bool, String> {
    if !same_stat(&stat_fd(fd.as_raw_fd())?, identity) {
        return Ok(false);
    }
    let (acl, xattr) = attributes_fd(fd.as_raw_fd())?;
    Ok(acl == identity.acl && xattr == identity.xattr)
}

/// The copy's pinned inode with the content metadata of the original.
#[cfg(target_os = "linux")]
fn copy_metadata_matches(fd: &OwnedFd, pin: &TransferPin, identity: &TransferIdentity) -> Result<bool, String> {
    let stat = stat_fd(fd.as_raw_fd())?;
    let (acl, xattr) = attributes_fd(fd.as_raw_fd())?;
    Ok(stat.st_nlink == 1
        && (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32
        && (stat.st_dev as u64, stat.st_ino as u64) == (pin.device, pin.inode)
        && u64::try_from(stat.st_size).ok() == Some(pin.size)
        && stat.st_mtime as i128 * 1_000_000_000 + stat.st_mtime_nsec as i128 == identity.mtime_ns
        && stat.st_uid == identity.uid
        && stat.st_gid == identity.gid
        && stat.st_mode as u32 & 0o7777 == identity.mode
        && acl == identity.acl
        && xattr == identity.xattr)
}

/// Whether the leased original still is what the record measured. An
/// unreadable original is accepted on its stat identity only when
/// `unreadable_allowed`, i.e. once its copy stands verified under the path.
#[cfg(target_os = "linux")]
fn check_original(session: &mut Session, file: &TransferFile, unreadable_allowed: bool) -> Result<(), Stop> {
    let Session { original, copy, .. } = session;
    let held = original.as_mut().ok_or("brak trzymanego oryginału")?;
    if !lease_intact(&held.fd)? {
        return Err(Stop::Withdraw("plik otwarty przez inny proces".into()));
    }
    if held.measured {
        if !original_metadata_matches(&held.fd, &file.source_identity)? {
            return Err(Stop::Withdraw("metadane pliku zmieniły się podczas przenoszenia".into()));
        }
        return Ok(());
    }
    let leases: Vec<&OwnedFd> = std::iter::once(&held.fd).chain(copy.as_ref().map(|copy| &copy.fd)).collect();
    let unreadable = match measure_fd(&held.fd, &leases) {
        Ok(identity) if identity == file.source_identity => None,
        Ok(_) => return Err(Stop::Withdraw("plik zmienił się podczas przenoszenia".into())),
        Err(MeasureError::LeaseBroken) => {
            return Err(Stop::Withdraw("plik lub jego kopia otwarte przez inny proces".into()));
        }
        Err(MeasureError::MediaRead(error))
            if unreadable_allowed && same_stat(&stat_fd(held.fd.as_raw_fd())?, &file.source_identity) =>
        {
            Some(error)
        }
        Err(error) => return Err(Stop::Fail(error.into_message())),
    };
    if !lease_intact(&held.fd)? {
        return Err(Stop::Withdraw("plik otwarty przez inny proces".into()));
    }
    held.measured = true;
    if unreadable.is_some() {
        session.unreadable = unreadable;
    }
    Ok(())
}

/// Whether the leased copy still is the verified copy of the original. After
/// an unreadable original its content is re-read from the medium, not from
/// the page cache: that hash is what releases an original nobody could read.
#[cfg(target_os = "linux")]
fn check_copy(session: &mut Session, file: &TransferFile) -> Result<(), Stop> {
    let pin = file
        .temporary_identity
        .clone()
        .ok_or("brak tożsamości kopii")?;
    let reread = session.unreadable.is_some();
    let Session { original, copy, .. } = session;
    let held = copy.as_mut().ok_or("brak trzymanej kopii")?;
    if !lease_intact(&held.fd)? {
        return Err(Stop::Withdraw("kopia na dysku danych otwarta przez inny proces".into()));
    }
    if held.measured && !reread {
        if !copy_metadata_matches(&held.fd, &pin, &file.source_identity)? {
            return Err(Stop::Fail("kopia ma inne metadane niż źródło".into()));
        }
        return Ok(());
    }
    if reread {
        drop_cached_pages(held.fd.as_raw_fd())?;
    }
    let leases: Vec<&OwnedFd> = std::iter::once(&held.fd).chain(original.as_ref().map(|original| &original.fd)).collect();
    let actual = match measure_fd(&held.fd, &leases) {
        Ok(actual) => actual,
        Err(MeasureError::LeaseBroken) => {
            return Err(Stop::Withdraw("plik lub jego kopia otwarte przez inny proces".into()));
        }
        Err(error) => return Err(Stop::Fail(error.into_message())),
    };
    if pin_of(&actual) != pin || !same_content_metadata(&actual, &file.source_identity) {
        return Err(Stop::Fail("kopia nie odpowiada przypiętej tożsamości i metadanym źródła".into()));
    }
    if !lease_intact(&held.fd)? {
        return Err(Stop::Withdraw("kopia na dysku danych otwarta przez inny proces".into()));
    }
    held.measured = true;
    Ok(())
}

/// Bytes copied, and written out to the medium, between two lease checks: a
/// client that opens either file during a long copy waits at most for one
/// chunk to be read, written and flushed before the copy is abandoned. The
/// flush per chunk is what keeps the closing `fsync` from becoming a wait of
/// its own under the lease.
#[cfg(target_os = "linux")]
const COPY_CHUNK: usize = 1024 * 1024;

/// `CopyIntent`: the leased original into the leased temporary.
#[cfg(target_os = "linux")]
fn copy_original<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<(), Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    let temporary = temporary_of(file)?.to_string();
    persist(file)?;
    if locate_original(session, file)? != Some(Place::Path) {
        return Err(Stop::Withdraw("plik zmienił się od skanu".into()));
    }
    hold_original(session, file, Place::Path)?;
    check_original(session, file, false)?;
    let destination_fd = session.destination_root.as_raw_fd();
    let target = match open_relative_io(
        destination_fd,
        &temporary,
        libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    ) {
        Ok(target) => {
            // The name is unique to this operation and sequence, so an
            // unpinned file there is this record's own create cut short
            // before its pin was written: adopt it, but only in that fresh
            // state.
            if file.temporary_pin.is_none() && !fresh_temporary(&stat_fd(target.as_raw_fd())?) {
                return Err(Stop::Fail("plik tymczasowy bez pina nie jest świeżą kopią tej operacji".into()));
            }
            target
        }
        // A pinned temporary that is not there did not survive a power loss
        // (its directory entry never reached the disk) or was removed by
        // somebody else. Recreating it could only fail its pin: the record is
        // withdrawn instead, and a later run copies the file afresh.
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) && file.temporary_pin.is_some() => {
            return Err(Stop::Withdraw("kopia tymczasowa zniknęła przed potwierdzeniem".into()));
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => open_relative(
            destination_fd,
            &temporary,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0o600,
        )?,
        Err(error) => return Err(Stop::Fail(error.to_string())),
    };
    let temporary_stat = stat_fd(target.as_raw_fd())?;
    if temporary_stat.st_nlink != 1
        || (temporary_stat.st_mode as u32 & libc::S_IFMT as u32) != libc::S_IFREG as u32
    {
        return Err(Stop::Fail("mover odmawia tymczasowego hardlinku lub pliku specjalnego".into()));
    }
    let temporary_pin = (temporary_stat.st_dev as u64, temporary_stat.st_ino as u64);
    match file.temporary_pin {
        Some(pin) if pin != temporary_pin => {
            return Err(Stop::Fail("plik tymczasowy zmienił przypięty inode".into()));
        }
        Some(_) => {}
        None => {
            file.temporary_pin = Some(temporary_pin);
            persist(file)?;
        }
    }
    if !take_lease(&target)? {
        return Err(Stop::Withdraw("kopia tymczasowa otwarta przez inny proces".into()));
    }
    if unsafe { libc::ftruncate(target.as_raw_fd(), 0) } != 0 {
        return Err(Stop::Fail(std::io::Error::last_os_error().to_string()));
    }
    {
        let original = session.original.as_ref().ok_or("brak trzymanego oryginału")?;
        let input = File::from(original.fd.try_clone().map_err(|e| e.to_string())?);
        let output = File::from(target.try_clone().map_err(|e| e.to_string())?);
        let mut buffer = vec![0u8; COPY_CHUNK];
        let mut offset = 0u64;
        loop {
            let count = input.read_at(&mut buffer, offset).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            output
                .write_all_at(&buffer[..count], offset)
                .map_err(|e| e.to_string())?;
            flush_range(output.as_raw_fd(), offset, count)?;
            offset = offset.checked_add(count as u64).ok_or("rozmiar pliku przekracza limit")?;
            lease_moment(LeaseMoment::CopyChunk, &original.fd);
            if !lease_intact(&original.fd)? {
                return Err(Stop::Withdraw("plik otwarty przez inny proces podczas kopiowania".into()));
            }
            if !lease_intact(&target)? {
                return Err(Stop::Withdraw("kopia tymczasowa otwarta przez inny proces".into()));
            }
        }
    }
    if unsafe { libc::fchown(target.as_raw_fd(), file.source_identity.uid, file.source_identity.gid) } != 0 {
        return Err(Stop::Fail(std::io::Error::last_os_error().to_string()));
    }
    strip_foreign_acls(target.as_raw_fd(), &file.source_identity.acl)?;
    restore_attributes(
        target.as_raw_fd(),
        file.source_identity.acl.iter().chain(file.source_identity.xattr.iter()),
    )?;
    // The temporary stays 0600 until here: the final mode, set-id bits
    // included, comes after `fchown`, which would clear them.
    if unsafe { libc::fchmod(target.as_raw_fd(), file.source_identity.mode) } != 0 {
        return Err(Stop::Fail(std::io::Error::last_os_error().to_string()));
    }
    let seconds = file.source_identity.mtime_ns.div_euclid(1_000_000_000);
    let nanos = file.source_identity.mtime_ns.rem_euclid(1_000_000_000);
    let times = [
        libc::timespec { tv_sec: seconds as _, tv_nsec: nanos as _ },
        libc::timespec { tv_sec: seconds as _, tv_nsec: nanos as _ },
    ];
    if unsafe { libc::futimens(target.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(Stop::Fail(std::io::Error::last_os_error().to_string()));
    }
    fsync_fd(target.as_raw_fd())?;
    // Compared before CopyConfirmed: a copy the branch altered (an inherited
    // ACL, a label) is withdrawn instead of being published.
    let original_fd = &session.original.as_ref().ok_or("brak trzymanego oryginału")?.fd;
    if !lease_intact(original_fd)? {
        return Err(Stop::Withdraw("plik otwarty przez inny proces podczas kopiowania".into()));
    }
    let actual = match measure_fd(&target, &[&target, original_fd]) {
        Ok(actual) => actual,
        Err(MeasureError::LeaseBroken) => {
            return Err(Stop::Withdraw("plik lub kopia tymczasowa otwarte przez inny proces".into()));
        }
        Err(error) => return Err(Stop::Fail(error.into_message())),
    };
    if !same_content_metadata(&actual, &file.source_identity) {
        return Err(Stop::Fail("kopia tymczasowa ma inną treść lub metadane niż źródło".into()));
    }
    if !lease_intact(&target)? {
        return Err(Stop::Withdraw("kopia tymczasowa otwarta przez inny proces".into()));
    }
    check_original(session, file, false)?;
    file.temporary_identity = Some(pin_of(&actual));
    session.copy = Some(Leased { fd: target, measured: true });
    file.phase = TransferFilePhase::CopyConfirmed;
    persist(file)?;
    Ok(())
}

/// `RenameIntent`: the verified copy under the file's path on the data branch.
/// The original still owns the path in the union: mergerfs resolves it on the
/// cache branch, which comes first.
#[cfg(target_os = "linux")]
fn rename_copy<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<(), Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    file.phase = TransferFilePhase::RenameIntent;
    persist(file)?;
    // A copy is never put under the path of an original that is gone or was
    // replaced: that file is somebody else's now.
    if locate_original(session, file)? != Some(Place::Path) {
        return Err(Stop::Withdraw("plik zmienił się od skanu".into()));
    }
    let place = hold_copy(session, file)?;
    check_copy(session, file)?;
    let destination_fd = session.destination_root.as_raw_fd();
    if place == CopyPlace::Temporary {
        rename_without_replace(destination_fd, temporary_of(file)?, &file.destination)?;
    }
    let (parent, _) = existing_parent(destination_fd, &file.destination)?.ok_or("katalog kopii zniknął")?;
    fsync_fd(parent.as_raw_fd())?;
    fsync_fd(destination_fd)?;
    file.destination_identity = file.temporary_identity.clone();
    file.phase = TransferFilePhase::RenameConfirmed;
    persist(file)?;
    Ok(())
}

/// Renames the original from its path to its quarantine name, after checking
/// under both leases that neither file was opened or changed. From the rename
/// on, the path resolves to the copy.
#[cfg(target_os = "linux")]
fn move_aside(session: &mut Session, file: &TransferFile) -> Result<(), Stop> {
    hold_original(session, file, Place::Path)?;
    check_original(session, file, true)?;
    if hold_copy(session, file)? != CopyPlace::Path {
        return Err(Stop::Fail("kopia nie stoi pod ścieżką pliku".into()));
    }
    check_copy(session, file)?;
    let original = session.original.as_ref().ok_or("brak trzymanego oryginału")?;
    if !lease_intact(&original.fd)? {
        return Err(Stop::Withdraw("plik otwarty przez inny proces".into()));
    }
    let copy = session.copy.as_ref().ok_or("brak trzymanej kopii")?;
    if !lease_intact(&copy.fd)? {
        return Err(Stop::Withdraw("kopia na dysku danych otwarta przez inny proces".into()));
    }
    let source_root = session.source_root.as_raw_fd();
    let quarantine = quarantine_name(temporary_of(file)?)?;
    let (parent, name) = existing_parent(source_root, &file.source)?.ok_or("katalog oryginału zniknął")?;
    rename_at_noreplace(parent.as_raw_fd(), &name, source_root, &quarantine).map_err(|error| error.to_string())?;
    // The name was resolved again by the rename: what moved must be the leased
    // inode, or a file swapped in under the path goes straight back.
    if !pinned_at(source_root, &quarantine, original_pin(file))? {
        return Err(Stop::Fail(match rename_at_noreplace(source_root, &quarantine, parent.as_raw_fd(), &name) {
            Ok(()) => "pod ścieżką pliku leżał inny plik niż przypięty oryginał; przywrócono go".to_string(),
            Err(error) => format!("pod ścieżką pliku leżał inny plik niż przypięty oryginał; nie przywrócono go: {error}"),
        }));
    }
    fsync_fd(parent.as_raw_fd())?;
    fsync_fd(source_root)?;
    Ok(())
}

/// `QuarantineIntent`: the original steps aside and the copy takes the path.
#[cfg(target_os = "linux")]
fn quarantine_original<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<(), Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    if file.phase == TransferFilePhase::RenameConfirmed {
        file.phase = TransferFilePhase::QuarantineIntent;
        persist(file)?;
    }
    match locate_original(session, file)? {
        // The rename landed before an interruption; the release checks the
        // original again before anything is removed.
        Some(Place::Quarantine) => {}
        Some(Place::Path) => move_aside(session, file)?,
        None => return Err(Stop::Fail("oryginał zniknął spod swojej ścieżki".into())),
    }
    if let Some(error) = session.unreadable.as_deref() {
        file.unread_source = Some(bounded_error(error));
    }
    file.phase = TransferFilePhase::QuarantineConfirmed;
    persist(file)?;
    Ok(())
}

/// `QuarantineConfirmed`/`UnlinkIntent`: the original is released only while
/// its lease proves nobody opened or truncated it since before the copy.
#[cfg(target_os = "linux")]
fn release_original<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<TransferEnd, Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    match locate_original(session, file)? {
        // The rename did not survive an interruption; it is still covered by
        // the persisted intent.
        Some(Place::Path) if file.phase == TransferFilePhase::QuarantineConfirmed => move_aside(session, file)?,
        Some(Place::Path) => {
            return Err(Stop::Fail("oryginał wrócił pod swoją ścieżkę po zamiarze usunięcia".into()));
        }
        Some(Place::Quarantine) => {}
        None if file.phase == TransferFilePhase::UnlinkIntent => {
            file.phase = TransferFilePhase::UnlinkConfirmed;
            persist(file)?;
            return finish_release(session, file, persist);
        }
        // Somebody removed the quarantined original — the dotfile is in the
        // share root. The copy under the path is the file, byte-identical to
        // the original when it stepped aside: the move is complete, and the
        // admin hears whose delete it was.
        None if locate_copy(session, file)? == Some(CopyPlace::Path) => {
            session.notices.push(format!(
                "odsunięty oryginał {} usunięto spoza movera; plik pozostaje jako kopia na dysku danych",
                quarantine_name(temporary_of(file)?)?
            ));
            file.phase = TransferFilePhase::UnlinkConfirmed;
            persist(file)?;
            return finish_release(session, file, persist);
        }
        None => return Err(Stop::Fail("odsunięty oryginał i kopia zniknęły".into())),
    }
    hold_original(session, file, Place::Quarantine)?;
    if let Some(original) = session.original.as_ref() {
        lease_moment(LeaseMoment::BeforeRelease, &original.fd);
    }
    // An original that could not be read is released only on the identity its
    // verified copy was re-read against BEFORE it stepped aside: after that,
    // the copy is the live file and may legitimately change.
    check_original(session, file, file.unread_source.is_some())?;
    if session.unreadable.is_some() && file.unread_source.is_none() {
        return Err(Stop::Fail("nieczytelny oryginał bez potwierdzonej kopii".into()));
    }
    if file.phase == TransferFilePhase::QuarantineConfirmed {
        file.phase = TransferFilePhase::UnlinkIntent;
        persist(file)?;
    }
    let source_root = session.source_root.as_raw_fd();
    let quarantine = quarantine_name(temporary_of(file)?)?;
    let opened = || Stop::Withdraw("oryginał otwarto po jego odsunięciu".into());
    // Checked once while the copy is still leased, so a withdrawal decided
    // here finds the copy provably untouched, and once more after its lease
    // is dropped — the copy owns the path, and its clients must not wait for
    // the unlink.
    if !lease_intact(&session.original.as_ref().ok_or("brak trzymanego oryginału")?.fd)? {
        return Err(opened());
    }
    session.copy = None;
    let original = session.original.as_ref().ok_or("brak trzymanego oryginału")?;
    if !lease_intact(&original.fd)? {
        return Err(opened());
    }
    let name = CString::new(quarantine.as_str()).map_err(|_| "nazwa zawiera NUL".to_string())?;
    if unsafe { libc::unlinkat(source_root, name.as_ptr(), 0) } != 0 {
        return Err(Stop::Fail(std::io::Error::last_os_error().to_string()));
    }
    if !lease_intact(&original.fd)? {
        session.notices.push(
            "oryginał otwarto w chwili jego usuwania z cache; zapis przez ten deskryptor mógł zostać utracony".into(),
        );
    }
    session.original = None;
    // THE UNLINK DECIDED IT: the original is gone. A directory fsync that fails
    // afterwards leaves only the durability of that in doubt, and reporting the
    // move as failed would send the record into a reversal that cannot happen.
    if let Err(error) = fsync_after_unlink(source_root) {
        session.notices.push(format!("usunięcie oryginału niepotwierdzone na dysku: {error}"));
    }
    file.phase = TransferFilePhase::UnlinkConfirmed;
    persist(file)?;
    finish_release(session, file, persist)
}

/// `UnlinkConfirmed`/`Done`. The copy is the live file by now: clients may
/// have changed, renamed or deleted it, so only the original's absence is
/// checked.
#[cfg(target_os = "linux")]
fn finish_release<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<TransferEnd, Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    if pinned_at(
        session.source_root.as_raw_fd(),
        &quarantine_name(temporary_of(file)?)?,
        original_pin(file),
    )? {
        return Err(Stop::Fail("odsunięty oryginał nadal istnieje po usunięciu".into()));
    }
    if file.phase == TransferFilePhase::UnlinkConfirmed {
        file.phase = TransferFilePhase::Done;
        persist(file)?;
    }
    Ok(TransferEnd::Moved { unreadable: file.unread_source.clone(), notices: std::mem::take(&mut session.notices) })
}

/// Holds the copy under the path and proves removing it discards nothing:
/// nobody has it open and its content is the pinned one. While the original
/// still owns the path only the content matters — a path-based change
/// (`epall`) reached the original too. Once the copy owned the path its
/// metadata must be untouched as well.
#[cfg(target_os = "linux")]
fn copy_removable(session: &mut Session, file: &TransferFile, owned_path: bool) -> Result<(), String> {
    let changed = || "kopia na dysku danych zmieniła się".to_string();
    match hold_copy(session, file) {
        Ok(_) => {}
        Err(Stop::Withdraw(reason) | Stop::Fail(reason)) => return Err(reason),
    }
    let pin = file.temporary_identity.clone().ok_or("brak tożsamości kopii")?;
    let held = session.copy.as_ref().ok_or("brak trzymanej kopii")?;
    if !lease_intact(&held.fd)? {
        return Err("kopia na dysku danych otwarta przez inny proces".into());
    }
    if !held.measured {
        // The original's lease, while the session holds it, is polled too: an
        // opener of the original waits for one chunk of this hash, not all of
        // it. An opener of the original is let go at once and the hash starts
        // over on the copy's lease alone.
        let actual = loop {
            let mut leases = vec![&held.fd];
            leases.extend(session.original.as_ref().map(|original| &original.fd));
            match measure_fd(&held.fd, &leases) {
                Ok(actual) => break actual,
                Err(MeasureError::LeaseBroken) if lease_intact(&held.fd)? && session.original.is_some() => {
                    session.original = None;
                }
                Err(MeasureError::LeaseBroken) => return Err("kopia na dysku danych otwarta przez inny proces".into()),
                Err(_) => return Err(changed()),
            }
        };
        if pin_of(&actual) != pin {
            return Err(changed());
        }
        if !lease_intact(&held.fd)? {
            return Err("kopia na dysku danych otwarta przez inny proces".into());
        }
        session.copy.as_mut().ok_or("brak trzymanej kopii")?.measured = true;
    }
    let held = session.copy.as_ref().ok_or("brak trzymanej kopii")?;
    if owned_path && !copy_metadata_matches(&held.fd, &pin, &file.source_identity)? {
        return Err(changed());
    }
    Ok(())
}

/// `RestoreIntent`: reverses the move from whichever point it reached. The
/// original goes back under its path first, so the path never resolves to
/// nothing; the copy is removed only while its own lease proves nobody opened
/// it. A copy clients already changed keeps the path and the original stays
/// under its quarantine name: the error names it and nothing is removed.
#[cfg(target_os = "linux")]
fn withdraw<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<Withdrawal, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    match file.phase {
        TransferFilePhase::Restored => return Ok(Withdrawal::Restored),
        TransferFilePhase::UnlinkConfirmed | TransferFilePhase::Done => {
            return Err("rekordu po usunięciu oryginału nie można wycofać".into());
        }
        TransferFilePhase::RestoreIntent => {}
        _ => {
            file.phase = TransferFilePhase::RestoreIntent;
            persist(file)?;
        }
    }
    let source_root = session.source_root.as_raw_fd();
    let destination_root = session.destination_root.as_raw_fd();
    let quarantine = quarantine_name(temporary_of(file)?)?;
    let copy = locate_copy(session, file)?;
    // Whether the copy under the path was proven removable AFTER the original
    // took the path back, so nothing between that proof and the unlink below
    // runs a second check whose failure could no longer give the path back.
    let mut removable_after_rename_back = false;
    match locate_original(session, file)? {
        Some(Place::Quarantine) => {
            // The quarantine name exists only after the copy took the path. A
            // copy no longer under it means a client replaced, renamed or
            // deleted the live file since, and the client's action wins:
            // renaming the original back would put old content over a newer
            // save, or bring a deleted file back. Both stay as they are.
            if copy != Some(CopyPlace::Path) {
                return Err(format!(
                    "konflikt: plik pod ścieżką zastąpiono, przeniesiono lub usunięto po odsunięciu oryginału; \
                     oryginał zostaje jako {quarantine}"
                ));
            }
            copy_removable(session, file, true)
                .map_err(|reason| format!("konflikt: {reason}; oryginał zostaje jako {quarantine}"))?;
            let (parent, name) = existing_parent(source_root, &file.source)?.ok_or_else(|| {
                format!("konflikt: katalog pliku zniknął z cache; oryginał zostaje jako {quarantine}")
            })?;
            rename_at_noreplace(source_root, &quarantine, parent.as_raw_fd(), &name).map_err(|error| {
                format!("konflikt: ścieżka pliku jest zajęta na cache ({error}); oryginał zostaje jako {quarantine}")
            })?;
            if let Some(held) = session.copy.as_ref() {
                lease_moment(LeaseMoment::AfterRenameBack, &held.fd);
            }
            // Until that rename the path resolved to the copy. An open that
            // looked it up then reaches the copy's lease only now; if one did,
            // the copy is in use and removing or shadowing it would hide what
            // that client writes. This is the LAST check before the copy is
            // unlinked, and any failure of it gives the path back to the copy,
            // with both files kept.
            if let Err(reason) = copy_removable(session, file, false) {
                return Err(match rename_at_noreplace(parent.as_raw_fd(), &name, source_root, &quarantine) {
                    Ok(()) => {
                        let _ = fsync_fd(parent.as_raw_fd());
                        let _ = fsync_fd(source_root);
                        format!("konflikt: {reason} w trakcie wycofania; oryginał zostaje jako {quarantine}")
                    }
                    Err(error) => format!(
                        "konflikt: {reason} w trakcie wycofania, a ścieżki nie oddano kopii ({error}); \
                         oryginał jest pod ścieżką, kopia zostaje na dysku danych"
                    ),
                });
            }
            removable_after_rename_back = true;
            fsync_fd(source_root)?;
            fsync_fd(parent.as_raw_fd())?;
        }
        Some(Place::Path) => {}
        // The quarantined original was removed by somebody else while the
        // copy owned the path: nothing is left to restore, and the copy is the
        // file. The record ends as a move.
        None if copy == Some(CopyPlace::Path) => {
            session.original = None;
            session.notices.push(format!(
                "odsunięty oryginał {quarantine} usunięto spoza movera; plik pozostaje jako kopia na dysku danych"
            ));
            file.phase = TransferFilePhase::Done;
            persist(file)?;
            return Ok(Withdrawal::CopyKept);
        }
        // Neither file is where the record left it after the copy had taken
        // the path on the data branch. A client's rename moves BOTH (mergerfs
        // renames the path on every branch that has it): the copy then sits
        // under the new name, untracked, and would silently fall behind the
        // cache original. A client's delete removes both. Only a copy this
        // session still holds can tell the two apart; otherwise the record
        // sticks and names the copy, instead of ending as a clean reversal.
        None if copy.is_none() && file.destination_identity.is_some() => {
            let deleted = session
                .copy
                .as_ref()
                .map(|held| stat_fd(held.fd.as_raw_fd()).map(|stat| stat.st_nlink == 0))
                .transpose()?;
            if deleted != Some(true) {
                let pin = file.temporary_pin.unwrap_or_default();
                return Err(format!(
                    "plik przeniesiono lub usunięto podczas przenoszenia; kopia na dysku danych (urządzenie {}, i-węzeł {}) mogła zostać pod nową ścieżką",
                    pin.0, pin.1
                ));
            }
            session.notices.push("plik usunięto podczas przenoszenia; nic nie zostało do wycofania".into());
        }
        None => {}
    }
    // The original is under its path again: an opener waiting on its lease
    // may go on.
    session.original = None;
    if copy == Some(CopyPlace::Path) {
        if !removable_after_rename_back {
            copy_removable(session, file, false)?;
        }
        let (parent, name) = existing_parent(destination_root, &file.destination)?.ok_or("katalog kopii zniknął")?;
        let c_name = CString::new(name.as_str()).map_err(|_| "nazwa zawiera NUL".to_string())?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), c_name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // The same residual window as the forward path's, on the copy: an open
        // that resolved the path to the copy before the rename back and reached
        // the copy's lease between the last check and this unlink holds an
        // unlinked inode now. It is seen and said; it cannot be undone.
        if !lease_intact(&session.copy.as_ref().ok_or("brak trzymanej kopii")?.fd)? {
            session.notices.push(
                "kopię otwarto w chwili jej usuwania z dysku danych; zapis przez ten deskryptor mógł zostać utracony".into(),
            );
        }
        if let Err(error) = fsync_fd(parent.as_raw_fd()) {
            session.notices.push(format!("usunięcie kopii niepotwierdzone na dysku: {error}"));
        }
    }
    session.copy = None;
    let temporary = temporary_of(file)?.to_string();
    match stat_at_io(destination_root, &temporary) {
        Ok(stat) => {
            let ours = match file.temporary_pin {
                Some(pin) => (stat.st_dev as u64, stat.st_ino as u64) == pin,
                None => fresh_temporary(&stat),
            };
            if !ours {
                return Err("pod nazwą tymczasową leży obcy plik".into());
            }
            let c_name = CString::new(temporary.as_str()).map_err(|_| "nazwa zawiera NUL".to_string())?;
            if unsafe { libc::unlinkat(destination_root, c_name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
        Err(error) => return Err(error.to_string()),
    }
    if let Some(intent) = &file.directory_intent {
        let (parent, name) = directory_parent(destination_root, intent)?;
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
    fsync_fd(destination_root)?;
    file.phase = TransferFilePhase::Restored;
    persist(file)?;
    Ok(Withdrawal::Restored)
}

/// How a reversal ended.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    /// `Restored`: the original is under its path, the copy is gone.
    Restored,
    /// `Done`: the original was removed by somebody else and the copy owns the
    /// path, so the record ended as a move.
    CopyKept,
}

#[cfg(target_os = "linux")]
fn open_session(source_root: &Path, destination_root: &Path) -> Result<Session, String> {
    Ok(Session {
        source_root: open_root_directory(source_root)?,
        destination_root: open_root_directory(destination_root)?,
        original: None,
        copy: None,
        unreadable: None,
        notices: Vec::new(),
    })
}

#[cfg(target_os = "linux")]
fn ended(session: &mut Session, file: &TransferFile, withdrawal: Withdrawal, reason: String) -> TransferEnd {
    let notices = std::mem::take(&mut session.notices);
    match withdrawal {
        Withdrawal::Restored => TransferEnd::Withdrawn { reason, notices },
        Withdrawal::CopyKept => TransferEnd::Moved { unreadable: file.unread_source.clone(), notices },
    }
}

#[cfg(target_os = "linux")]
fn drive<F>(session: &mut Session, file: &mut TransferFile, persist: &mut F) -> Result<TransferEnd, Stop>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    loop {
        match file.phase {
            TransferFilePhase::CopyIntent => copy_original(session, file, persist)?,
            TransferFilePhase::CopyConfirmed | TransferFilePhase::RenameIntent => {
                rename_copy(session, file, persist)?
            }
            TransferFilePhase::RenameConfirmed | TransferFilePhase::QuarantineIntent => {
                quarantine_original(session, file, persist)?
            }
            TransferFilePhase::QuarantineConfirmed | TransferFilePhase::UnlinkIntent => {
                return release_original(session, file, persist);
            }
            TransferFilePhase::UnlinkConfirmed | TransferFilePhase::Done => {
                return finish_release(session, file, persist);
            }
            TransferFilePhase::RestoreIntent => {
                let withdrawal = withdraw(session, file, persist)?;
                return Ok(ended(session, file, withdrawal, "wycofanie dokończone po przerwaniu".into()));
            }
            TransferFilePhase::Restored => {
                return Ok(ended(session, file, Withdrawal::Restored, "przeniesienie wycofane przed przerwaniem".into()));
            }
        }
    }
}

/// Drives one record to `Done` or `Restored`, persisting every phase before
/// its syscall. A decision to keep the file on the cache (it was opened or
/// changed) is carried out here and ends as `Withdrawn`; a failure is returned
/// as it is, with the record at the phase it reached, for the caller to
/// withdraw or keep.
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
    let mut session = open_session(source_root, destination_root)?;
    match drive(&mut session, file, &mut persist) {
        Ok(end) => Ok(end),
        Err(Stop::Fail(error)) => Err(error),
        Err(Stop::Withdraw(reason)) => {
            let withdrawal = withdraw(&mut session, file, &mut persist)
                .map_err(|error| format!("{reason}; wycofanie nieudane: {error}"))?;
            Ok(ended(&mut session, file, withdrawal, reason))
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn transfer_file<F>(_: &Path, _: &Path, _: &mut TransferFile, _: F) -> Result<TransferEnd, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

/// Reverses a record after a failure, from a fresh session: every file is
/// found again by its pin and leased again before anything is removed.
#[cfg(target_os = "linux")]
pub(crate) fn withdraw_record<F>(
    source_root: &Path,
    destination_root: &Path,
    file: &mut TransferFile,
    mut persist: F,
) -> Result<TransferEnd, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    let mut session = open_session(source_root, destination_root)?;
    withdraw(&mut session, file, &mut persist).map(|withdrawal| ended(&mut session, file, withdrawal, String::new()))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn withdraw_record<F>(_: &Path, _: &Path, _: &mut TransferFile, _: F) -> Result<TransferEnd, String>
where
    F: FnMut(&TransferFile) -> Result<(), String>,
{
    Err("transfer FD-relative jest obsługiwany wyłącznie na Linuxie".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    /// Every test here takes leases or starts processes. A fork carries the
    /// parent's descriptors — close-on-exec ones included — until its exec, and
    /// such a descriptor is an open file description a write lease is refused
    /// for; so no fork of another test may overlap a lease of this one.
    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        crate::elastic::execution::tests::FORK_REOPEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    /// The SMB veto value names exactly the two names this module creates.
    #[test]
    fn the_smb_veto_names_the_temporary_and_the_quarantine() {
        assert_eq!(crate::SMB_VETO_FILES, format!("/{TEMPORARY_PREFIX}*/{QUARANTINE_PREFIX}*/"));
        assert!(crate::validate_smb_config(&format!("[x]\n\tpath = /mnt/media\n\tveto files = {}\n", crate::SMB_VETO_FILES)).is_ok());
        assert!(crate::validate_smb_config("[x]\n\tpath = /mnt/media\n\tveto files = /*/\n").is_err());
    }

    #[test]
    fn plan_refuses_existing_destination_before_any_write() {
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let mut first_seen: Vec<TransferFilePhase> = Vec::new();
        for phase in phases {
            if first_seen.last() != Some(&phase) {
                first_seen.push(phase);
            }
        }
        assert_eq!(
            first_seen,
            vec![
                TransferFilePhase::CopyIntent,
                TransferFilePhase::CopyConfirmed,
                TransferFilePhase::RenameIntent,
                TransferFilePhase::RenameConfirmed,
                TransferFilePhase::QuarantineIntent,
                TransferFilePhase::QuarantineConfirmed,
                TransferFilePhase::UnlinkIntent,
                TransferFilePhase::UnlinkConfirmed,
                TransferFilePhase::Done,
            ],
            "every phase is written, in order, before its step"
        );
        assert_eq!(fs::read_dir(&source_root).unwrap().count(), 0, "no quarantine name left behind");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn persisted_record_reopens_after_copy_boundary_and_finishes() {
        let _isolation = isolated();
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
        let _isolation = isolated();
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

    /// Once the original is released the copy is the file clients use: a
    /// finished record never judges what they did to it. An original still
    /// under its quarantine name after a confirmed unlink is a contradiction.
    #[test]
    fn a_released_record_leaves_the_live_copy_to_its_clients() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).unwrap();
        fs::remove_file(destination_root.join("payload.bin")).unwrap();
        fs::write(destination_root.join("payload.bin"), b"rewritten by a client").unwrap();
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        assert_eq!(end, Ok(TransferEnd::Moved { unreadable: None, notices: Vec::new() }));
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"rewritten by a client");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();

        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::UnlinkIntent);
        file.phase = TransferFilePhase::UnlinkConfirmed;
        let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
        assert!(quarantine.exists());
        assert!(transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).is_err());
        assert_eq!(fs::read(&quarantine).unwrap(), b"source", "nothing removed on a contradiction");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn scan_refuses_symlink_root_and_reports_symlinks_per_entry() {
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
    fn quarantine_refuses_a_file_swapped_under_the_path() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"source").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let mut replaced = false;
        let result = transfer_file(&source_root, &destination_root, &mut file, |record| {
            if record.phase == TransferFilePhase::QuarantineIntent && !replaced {
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
    fn withdrawal_removes_only_the_pinned_copy_and_never_a_released_original() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameIntent);
        let temporary = destination_root.join(file.temporary.as_ref().unwrap());
        assert!(temporary.exists());
        let mut foreign = file.clone();
        withdraw_record(&source_root, &destination_root, &mut file, |_| Ok(())).expect("wycofanie");
        assert_eq!(file.phase, TransferFilePhase::Restored);
        assert!(!temporary.exists());
        assert!(!destination_root.join("payload.bin").exists());
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");

        // A file under the temporary name that is not the pinned inode stays.
        fs::write(&temporary, b"foreign").unwrap();
        assert!(withdraw_record(&source_root, &destination_root, &mut foreign, |_| Ok(())).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"foreign");
        fs::remove_file(&temporary).unwrap();

        // With the copy already under the path the original still owns it in
        // the union, so the copy goes and the original stays.
        let mut renamed = stopped_at(&source_root, &destination_root, TransferFilePhase::QuarantineIntent);
        withdraw_record(&source_root, &destination_root, &mut renamed, |_| Ok(())).expect("wycofanie");
        assert!(!destination_root.join("payload.bin").exists());
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");

        // Once the original is released nothing can bring it back.
        let mut released = stopped_at(&source_root, &destination_root, TransferFilePhase::Done);
        assert!(withdraw_record(&source_root, &destination_root, &mut released, |_| Ok(())).is_err());
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        assert!(!source_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn entry_exists_resolves_beneath_the_branch_without_symlinks() {
        let _isolation = isolated();
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
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        fail_content_reads(None);
        assert!(
            matches!(end, Ok(TransferEnd::Moved { unreadable: Some(ref error), ref notices }) if error.contains("os error 5") && notices.is_empty()),
            "{end:?}"
        );
        assert_eq!(file.phase, TransferFilePhase::Done);
        assert!(!source_root.join("payload.bin").exists());
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn identity_finish_is_refused_before_the_copy_and_for_other_errors() {
        let _isolation = isolated();
        // Before a verified copy exists an unreadable source is an error, never an unlink.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::CopyIntent);
        let source = fs::metadata(source_root.join("payload.bin")).unwrap();
        fail_content_reads(Some((source.dev(), source.ino())));
        let error = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()))
            .expect_err("przed kopią");
        fail_content_reads(None);
        assert!(error.contains("os error 5"), "{error}");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        assert!(!destination_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();

        // After the rename, a source that changed is not a read error: the move
        // is reversed and the changed file stays where it is.
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("payload.bin"))
            .unwrap()
            .write_all(b"+")
            .unwrap();
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        assert!(matches!(end, Ok(TransferEnd::Withdrawn { .. })), "{end:?}");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload+");
        assert!(!destination_root.join("payload.bin").exists());
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        let _isolation = isolated();
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
        assert!(matches!(end, Ok(TransferEnd::Moved { unreadable: Some(_), .. })), "{end:?}");
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn every_persist_boundary_reopens_safely() {
        let _isolation = isolated();
        let phases = [
            TransferFilePhase::CopyIntent,
            TransferFilePhase::CopyConfirmed,
            TransferFilePhase::RenameIntent,
            TransferFilePhase::RenameConfirmed,
            TransferFilePhase::QuarantineIntent,
            TransferFilePhase::QuarantineConfirmed,
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

    /// A `sh` that appends `bytes` to `path` with a plain blocking open, the
    /// way any client writes.
    fn writer(path: &Path, bytes: &str) -> std::process::Child {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("printf %s \"$1\" >> \"$2\"")
            .arg("sh")
            .arg(bytes)
            .arg(path)
            .spawn()
            .unwrap()
    }

    /// Waits until something has run into the lease on `fd`. A test opener
    /// is then provably inside its `open(2)`, past its path lookup.
    fn wait_for_break(fd: &OwnedFd) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while lease_intact(fd).unwrap() {
            assert!(std::time::Instant::now() < deadline, "otwierający nie dotarł do lease");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// The names this module leaves in a branch root.
    fn helper_names(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(TEMPORARY_PREFIX) || name.starts_with(QUARANTINE_PREFIX))
            .collect()
    }

    #[test]
    fn an_open_original_is_skipped_before_anything_is_written() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let mut reader = std::process::Command::new("sleep")
            .arg("30")
            .stdin(File::open(source_root.join("payload.bin")).unwrap())
            .spawn()
            .unwrap();
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        reader.kill().unwrap();
        reader.wait().unwrap();
        assert_eq!(end, Ok(TransferEnd::Withdrawn { reason: "plik otwarty przez inny proces".into(), notices: Vec::new() }));
        assert_eq!(file.phase, TransferFilePhase::Restored);
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0, "nothing written to the data branch");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// A client that opens the original to write while it is being copied
    /// waits for the copy to be abandoned, then writes into the original,
    /// which never left its path.
    #[test]
    fn a_writer_during_the_copy_withdraws_it_and_keeps_its_write() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        let payload: Vec<u8> = (0..3 * COPY_CHUNK).map(|index| (index % 251) as u8).collect();
        fs::write(source_root.join("payload.bin"), &payload).unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let child = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (hooked, path) = (child.clone(), source_root.join("payload.bin"));
        on_lease_moment(Some(Box::new(move |moment, original| {
            if moment == LeaseMoment::CopyChunk && hooked.borrow().is_none() {
                *hooked.borrow_mut() = Some(writer(&path, "MORE"));
                wait_for_break(original);
            }
        })));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        on_lease_moment(None);
        let status = child.borrow_mut().take().expect("pisarz uruchomiony").wait().unwrap();
        assert!(status.success());
        assert_eq!(end, Ok(TransferEnd::Withdrawn { reason: "plik otwarty przez inny proces podczas kopiowania".into(), notices: Vec::new() }));
        let mut expected = payload.clone();
        expected.extend_from_slice(b"MORE");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), expected, "the write landed in the original");
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0, "no copy and no temporary left");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// The residual-window case that CAN be closed: an opener that reaches the
    /// original after it stepped aside but before the release checks. The
    /// original goes back under its path, the copy goes, and the opener's
    /// write lands in the original.
    #[test]
    fn an_opener_after_the_quarantine_rename_gets_the_original_back_with_its_write() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
        let child = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (hooked, path) = (child.clone(), quarantine.clone());
        on_lease_moment(Some(Box::new(move |moment, original| {
            if moment == LeaseMoment::BeforeRelease && hooked.borrow().is_none() {
                assert!(path.exists(), "the original is under its quarantine name");
                *hooked.borrow_mut() = Some(writer(&path, "NEW"));
                wait_for_break(original);
            }
        })));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        on_lease_moment(None);
        let status = child.borrow_mut().take().expect("pisarz uruchomiony").wait().unwrap();
        assert!(status.success());
        assert_eq!(end, Ok(TransferEnd::Withdrawn { reason: "plik otwarty przez inny proces".into(), notices: Vec::new() }));
        assert_eq!(file.phase, TransferFilePhase::Restored);
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payloadNEW");
        assert!(!destination_root.join("payload.bin").exists(), "no copy left on the data branch");
        assert!(!quarantine.exists());
        assert!(helper_names(&source_root).is_empty() && helper_names(&destination_root).is_empty());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn a_metadata_change_after_the_quarantine_rename_restores_the_original() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        fs::set_permissions(source_root.join("payload.bin"), fs::Permissions::from_mode(0o644)).unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
        let path = quarantine.clone();
        on_lease_moment(Some(Box::new(move |moment, _| {
            if moment == LeaseMoment::BeforeRelease {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        })));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        on_lease_moment(None);
        assert_eq!(end, Ok(TransferEnd::Withdrawn { reason: "metadane pliku zmieniły się podczas przenoszenia".into(), notices: Vec::new() }));
        assert_eq!(mode(&source_root.join("payload.bin")), 0o600, "the change is kept");
        assert!(!destination_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// Once the path resolves to the copy, clients use the copy: a reader of
    /// it is no reason to keep the original, and it gets its bytes.
    #[test]
    fn a_client_reading_the_copy_does_not_stop_the_release() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let reader = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (hooked, copy) = (reader.clone(), destination_root.join("payload.bin"));
        on_lease_moment(Some(Box::new(move |moment, _| {
            if moment == LeaseMoment::BeforeRelease && hooked.borrow().is_none() {
                *hooked.borrow_mut() = Some(
                    std::process::Command::new("cat")
                        .arg(&copy)
                        .stdout(std::process::Stdio::piped())
                        .spawn()
                        .unwrap(),
                );
            }
        })));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        on_lease_moment(None);
        let output = reader.borrow_mut().take().expect("czytelnik").wait_with_output().unwrap();
        assert_eq!(end, Ok(TransferEnd::Moved { unreadable: None, notices: Vec::new() }));
        assert_eq!(output.stdout, b"payload");
        assert!(!source_root.join("payload.bin").exists());
        assert!(helper_names(&source_root).is_empty());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// Somebody reached the original after it stepped aside AND somebody uses
    /// the copy under the path: any reversal would discard one of them. Both
    /// stay, the original under its quarantine name, and the error says where.
    #[test]
    fn both_files_touched_after_the_quarantine_rename_keep_both() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let quarantine_file = quarantine_name(file.temporary.as_deref().unwrap()).unwrap();
        let quarantine = source_root.join(&quarantine_file);
        let child = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (hooked, path, copy) = (child.clone(), quarantine.clone(), destination_root.join("payload.bin"));
        on_lease_moment(Some(Box::new(move |moment, original| {
            if moment == LeaseMoment::BeforeRelease && hooked.borrow().is_none() {
                *hooked.borrow_mut() = Some(writer(&path, "NEW"));
                wait_for_break(original);
                let refused = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&copy)
                    .expect_err("the copy is leased");
                assert_eq!(refused.raw_os_error(), Some(libc::EWOULDBLOCK));
            }
        })));
        let error = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).expect_err("konflikt");
        on_lease_moment(None);
        assert!(child.borrow_mut().take().expect("pisarz").wait().unwrap().success());
        assert!(error.contains("konflikt") && error.contains(&quarantine_file), "{error}");
        assert_eq!(file.phase, TransferFilePhase::RestoreIntent);
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        assert_eq!(fs::read(&quarantine).unwrap(), b"payloadNEW", "the opener's write is kept");
        assert!(!source_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// A power loss after ANY durable phase — on both sides of each write,
    /// on the move and on the reversal — followed by a client writing to the
    /// file as the union shows it. The resumed record must end with exactly
    /// one copy under the path, holding that write, and nothing of the
    /// helper's left in either branch root. The one outcome that keeps two
    /// copies is the conflict: a reversal already decided when the client
    /// wrote to the live copy.
    /// H1 of the 2026-09-17 review. A reversal renames the original back
    /// under the path; an open that had resolved the path to the copy just
    /// before reaches the copy's lease only then. The path must go back to
    /// the copy, so that client's save lands where the share shows it — and
    /// the late opener of the original keeps its write under the quarantine
    /// name. Both files are kept and the record sticks.
    #[test]
    fn a_client_opening_the_copy_during_a_reversal_keeps_the_path() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
        let children = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (hooked, late, copy) = (children.clone(), quarantine.clone(), destination_root.join("payload.bin"));
        on_lease_moment(Some(Box::new(move |moment, leased| match moment {
            LeaseMoment::BeforeRelease if hooked.borrow().is_empty() => {
                hooked.borrow_mut().push(writer(&late, "A"));
                wait_for_break(leased);
            }
            LeaseMoment::AfterRenameBack => {
                hooked.borrow_mut().push(writer(&copy, "B"));
                wait_for_break(leased);
            }
            _ => {}
        })));
        let error = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).expect_err("konflikt");
        on_lease_moment(None);
        for child in children.borrow_mut().iter_mut() {
            assert!(child.wait().unwrap().success());
        }
        assert!(error.contains("otwarta przez inny proces w trakcie wycofania"), "{error}");
        assert!(!source_root.join("payload.bin").exists(), "the path resolves to the copy again");
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payloadB", "B's save is visible");
        assert_eq!(fs::read(&quarantine).unwrap(), b"payloadA", "A's save is kept");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// M2: a client opening the original while the COPY is being hashed
    /// waits one chunk, not the whole hash — every lease the session holds is
    /// polled while either file is read.
    #[test]
    fn opening_the_original_while_the_copy_is_hashed_stops_the_hash_within_a_chunk() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        let payload: Vec<u8> = (0..4 * COPY_CHUNK).map(|index| (index % 253) as u8).collect();
        fs::write(source_root.join("payload.bin"), &payload).unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::RenameConfirmed);
        let original = fs::metadata(source_root.join("payload.bin")).unwrap();
        let copy = fs::metadata(destination_root.join("payload.bin")).unwrap();
        let copy_id = (copy.dev(), copy.ino());
        // A fresh session re-hashes both files: the original first, with only
        // its own lease, then the copy with both.
        MEASURED_CHUNKS.with(|chunks| chunks.borrow_mut().clear());
        let child = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (hooked, path) = (child.clone(), source_root.join("payload.bin"));
        let original_id = (original.dev(), original.ino());
        // Chunks of the copy read while BOTH leases were polled: the hash the
        // open has to wait on. The reversal re-reads the copy afterwards with
        // the original already released, which no client waits on.
        let guarded_chunks = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counted = guarded_chunks.clone();
        MEASURE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |measured, leases: &[&OwnedFd]| {
                if measured != copy_id || leases.len() < 2 {
                    return;
                }
                counted.set(counted.get() + 1);
                if hooked.borrow().is_some() {
                    return;
                }
                let original_fd = leases
                    .iter()
                    .find(|fd| stat_fd(fd.as_raw_fd()).is_ok_and(|stat| (stat.st_dev as u64, stat.st_ino as u64) == original_id))
                    .expect("the original's lease is polled while the copy is hashed");
                *hooked.borrow_mut() = Some(writer(&path, "X"));
                wait_for_break(original_fd);
            }))
        });
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        MEASURE_HOOK.with(|hook| *hook.borrow_mut() = None);
        let status = child.borrow_mut().take().expect("pisarz").wait().unwrap();
        assert!(status.success());
        assert!(matches!(end, Ok(TransferEnd::Withdrawn { .. })), "{end:?}");
        assert_eq!(guarded_chunks.get(), 1, "the copy's hash stopped at the chunk the open arrived in");
        assert!(MEASURED_CHUNKS.with(|chunks| chunks.borrow().iter().any(|id| *id == copy_id)));
        let mut expected = payload.clone();
        expected.extend_from_slice(b"X");
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), expected);
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// M3: the quarantine name is a dotfile in the share root, and a client
    /// may delete it. Before the release decision that ends the record as a
    /// move — the copy is the file — and during a reversal it does too; in
    /// both the admin hears about it, and nothing is stuck.
    #[test]
    fn a_client_deleting_the_quarantined_original_ends_the_record_as_a_move() {
        let _isolation = isolated();
        for opened_first in [false, true] {
            let (source_root, destination_root) = fixture();
            fs::write(source_root.join("payload.bin"), b"payload").unwrap();
            let mut file = planned(&source_root, &destination_root, "payload.bin");
            let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
            let child = std::rc::Rc::new(std::cell::RefCell::new(None));
            let (hooked, path) = (child.clone(), quarantine.clone());
            on_lease_moment(Some(Box::new(move |moment, leased| {
                if moment == LeaseMoment::BeforeRelease && hooked.borrow().is_none() {
                    if opened_first {
                        *hooked.borrow_mut() = Some(
                            std::process::Command::new("cat").arg(&path).stdout(std::process::Stdio::null()).spawn().unwrap(),
                        );
                        wait_for_break(leased);
                    }
                    fs::remove_file(&path).unwrap();
                }
            })));
            let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
            on_lease_moment(None);
            if let Some(mut child) = child.borrow_mut().take() {
                child.wait().unwrap();
            }
            match &end {
                Ok(TransferEnd::Moved { unreadable: None, notices }) => {
                    assert!(notices.iter().any(|notice| notice.contains("usunięto spoza movera")), "{notices:?}");
                }
                other => panic!("opened_first={opened_first}: {other:?}"),
            }
            assert_eq!(file.phase, TransferFilePhase::Done);
            assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
            assert!(!source_root.join("payload.bin").exists() && !quarantine.exists());
            fs::remove_dir_all(source_root).unwrap();
            fs::remove_dir_all(destination_root).unwrap();
        }
    }

    /// D1 of the second review: a reversal that finds the copy gone from the
    /// path after the original was quarantined never renames the original
    /// back. A client replaced the file (an editor's save renames its own
    /// temporary over the path) or deleted it, and its action wins: the newer
    /// content stays visible, a deleted file stays deleted, and the record
    /// sticks naming the quarantined original.
    #[test]
    fn a_reversal_never_puts_the_original_back_over_a_replaced_or_deleted_copy() {
        let _isolation = isolated();
        for replaced in [true, false] {
            for stop in [TransferFilePhase::QuarantineConfirmed, TransferFilePhase::RestoreIntent] {
                let (source_root, destination_root) = fixture();
                fs::write(source_root.join("payload.bin"), b"payload").unwrap();
                let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::QuarantineConfirmed);
                if stop == TransferFilePhase::RestoreIntent {
                    file.phase = TransferFilePhase::RestoreIntent;
                }
                let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
                let copy = destination_root.join("payload.bin");
                if replaced {
                    let save = destination_root.join(".payload.bin.swp");
                    fs::write(&save, b"newer save").unwrap();
                    fs::rename(&save, &copy).unwrap();
                } else {
                    fs::remove_file(&copy).unwrap();
                }
                let error = withdraw_record(&source_root, &destination_root, &mut file, |_| Ok(()))
                    .expect_err("the record sticks");
                assert!(error.contains("zastąpiono, przeniesiono lub usunięto"), "{replaced} {stop:?}: {error}");
                assert!(!source_root.join("payload.bin").exists(), "{replaced} {stop:?}: the original stays aside");
                assert_eq!(fs::read(&quarantine).unwrap(), b"payload", "{replaced} {stop:?}");
                if replaced {
                    assert_eq!(fs::read(&copy).unwrap(), b"newer save", "{stop:?}");
                } else {
                    assert!(!copy.exists(), "{stop:?}");
                }
                fs::remove_dir_all(source_root).unwrap();
                fs::remove_dir_all(destination_root).unwrap();
            }
        }
    }

    /// M3, after a restart: the helper stopped with the original quarantined
    /// and a client deleted the dotfile before the next run. The next run
    /// finds only the copy under the path and ends the record as a move.
    #[test]
    fn a_quarantined_original_deleted_between_runs_ends_the_record_as_a_move() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::QuarantineConfirmed);
        let quarantine = source_root.join(quarantine_name(file.temporary.as_deref().unwrap()).unwrap());
        fs::remove_file(&quarantine).unwrap();
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        match &end {
            Ok(TransferEnd::Moved { unreadable: None, notices }) => {
                assert!(notices.iter().any(|notice| notice.contains("usunięto spoza movera")), "{notices:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(file.phase, TransferFilePhase::Done);
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        assert!(!source_root.join("payload.bin").exists());
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// M5: power lost right after the temporary's pin was written, before its
    /// directory entry reached the disk. The pinned name is gone; the record is
    /// withdrawn instead of creating a file its own pin refuses, and the file
    /// moves on the next attempt.
    #[test]
    fn a_pinned_temporary_lost_to_a_power_cut_withdraws_the_record_cleanly() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        let mut durable = file.clone();
        assert!(transfer_file(&source_root, &destination_root, &mut file, |record| {
            durable = record.clone();
            if record.phase == TransferFilePhase::CopyIntent && record.temporary_pin.is_some() {
                return Err("utrata zasilania".into());
            }
            Ok(())
        })
        .is_err());
        assert!(durable.temporary_pin.is_some());
        fs::remove_file(destination_root.join(durable.temporary.as_deref().unwrap())).unwrap();
        let end = transfer_file(&source_root, &destination_root, &mut durable, |_| Ok(()));
        assert!(matches!(end, Ok(TransferEnd::Withdrawn { ref reason, .. }) if reason.contains("zniknęła")), "{end:?}");
        assert_eq!(durable.phase, TransferFilePhase::Restored);
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
        assert_eq!(fs::read(source_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// M6: the unlink of the quarantined original succeeded and only the
    /// directory fsync after it failed. The move is complete — a reversal
    /// could not bring the original back — and the doubt is a notice.
    #[test]
    fn a_failed_fsync_after_the_original_is_unlinked_is_a_notice_not_a_failure() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = planned(&source_root, &destination_root, "payload.bin");
        on_lease_moment(Some(Box::new(|moment, _| {
            if moment == LeaseMoment::BeforeRelease {
                fail_directory_fsync(true);
            }
        })));
        let end = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(()));
        on_lease_moment(None);
        fail_directory_fsync(false);
        match &end {
            Ok(TransferEnd::Moved { notices, .. }) => {
                assert!(notices.iter().any(|notice| notice.contains("niepotwierdzone na dysku")), "{notices:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(file.phase, TransferFilePhase::Done);
        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    /// M1: a client renames the file while its copy already holds the path on
    /// the data branch; mergerfs renames it on both branches. The reversal
    /// must not end as if nothing were left: the copy sits under the new name
    /// and the record says so.
    #[test]
    fn a_client_rename_during_the_move_is_named_not_silently_withdrawn() {
        let _isolation = isolated();
        let (source_root, destination_root) = fixture();
        fs::write(source_root.join("payload.bin"), b"payload").unwrap();
        let mut file = stopped_at(&source_root, &destination_root, TransferFilePhase::QuarantineIntent);
        for root in [&source_root, &destination_root] {
            fs::rename(root.join("payload.bin"), root.join("renamed.bin")).unwrap();
        }
        let error = transfer_file(&source_root, &destination_root, &mut file, |_| Ok(())).expect_err("zniknął");
        assert!(error.contains("zniknął"), "{error}");
        let refusal = withdraw_record(&source_root, &destination_root, &mut file, |_| Ok(())).expect_err("named");
        assert!(refusal.contains("mogła zostać pod nową ścieżką"), "{refusal}");
        assert_eq!(fs::read(destination_root.join("renamed.bin")).unwrap(), b"payload", "nothing removed");
        assert_eq!(fs::read(source_root.join("renamed.bin")).unwrap(), b"payload");
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(destination_root).unwrap();
    }

    #[test]
    fn every_phase_recovers_to_one_visible_copy_with_the_newest_content() {
        let _isolation = isolated();
        let moving = [
            TransferFilePhase::CopyIntent,
            TransferFilePhase::CopyConfirmed,
            TransferFilePhase::RenameIntent,
            TransferFilePhase::RenameConfirmed,
            TransferFilePhase::QuarantineIntent,
            TransferFilePhase::QuarantineConfirmed,
            TransferFilePhase::UnlinkIntent,
            TransferFilePhase::UnlinkConfirmed,
            TransferFilePhase::Done,
        ];
        let reversing = [TransferFilePhase::RestoreIntent, TransferFilePhase::Restored];
        let cases = moving
            .iter()
            .map(|phase| (*phase, false))
            .chain(reversing.iter().map(|phase| (*phase, true)));
        for (stop, reverse) in cases {
            let calls: &[usize] = if stop == TransferFilePhase::CopyIntent { &[0, 1] } else { &[0] };
            for &target_call in calls {
                for before_write in [true, false] {
                    let case = format!("{stop:?} call={target_call} before_write={before_write}");
                    let (source_root, destination_root) = fixture();
                    fs::write(source_root.join("payload.bin"), b"boundary payload").unwrap();
                    let mut file = planned(&source_root, &destination_root, "payload.bin");
                    let quarantine = quarantine_name(file.temporary.as_deref().unwrap()).unwrap();
                    let record_path = unique("record").join("record.json");
                    fs::write(&record_path, serde_json::to_vec(&file).unwrap()).unwrap();
                    let durable = |record: &TransferFile, path: &Path| {
                        let mut output = File::create(path).unwrap();
                        output.write_all(&serde_json::to_vec(record).unwrap()).unwrap();
                        output.sync_all().unwrap();
                    };
                    if reverse {
                        let path = source_root.join(&quarantine);
                        on_lease_moment(Some(Box::new(move |moment, _| {
                            if moment == LeaseMoment::BeforeRelease {
                                let _ = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path);
                            }
                        })));
                    }
                    let mut seen = 0usize;
                    let mut interrupted = false;
                    let first = transfer_file(&source_root, &destination_root, &mut file, |record| {
                        let hit = record.phase == stop && {
                            let call = seen;
                            seen += 1;
                            call == target_call
                        };
                        if hit && !interrupted {
                            interrupted = true;
                            if before_write {
                                return Err("utrata zasilania przed zapisem".into());
                            }
                            durable(record, &record_path);
                            return Err("utrata zasilania po zapisie".into());
                        }
                        durable(record, &record_path);
                        Ok(())
                    });
                    on_lease_moment(None);
                    assert!(interrupted, "{case}: granica nieosiągnięta ({first:?})");
                    // The client writes to the path as the union resolves it:
                    // the cache branch first.
                    let visible = if source_root.join("payload.bin").exists() {
                        source_root.join("payload.bin")
                    } else {
                        destination_root.join("payload.bin")
                    };
                    fs::OpenOptions::new().append(true).open(&visible).unwrap().write_all(b"NEWER").unwrap();
                    let mut reopened: TransferFile = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
                    let resumed = transfer_file(&source_root, &destination_root, &mut reopened, |record| {
                        durable(record, &record_path);
                        Ok(())
                    });
                    let on_cache = source_root.join("payload.bin").exists();
                    let on_data = destination_root.join("payload.bin").exists();
                    if reverse && stop == TransferFilePhase::RestoreIntent && !before_write {
                        let error = resumed.expect_err(&case);
                        assert!(error.contains("konflikt"), "{case}: {error}");
                        assert!(!on_cache && on_data, "{case}");
                        assert_eq!(fs::read(destination_root.join("payload.bin")).unwrap(), b"boundary payloadNEWER", "{case}");
                        assert_eq!(fs::read(source_root.join(&quarantine)).unwrap(), b"boundary payload", "{case}: the original is kept");
                    } else {
                        resumed.unwrap_or_else(|error| panic!("{case}: {error}"));
                        assert!(on_cache != on_data, "{case}: exactly one copy under the path");
                        assert_eq!(fs::read(&visible).unwrap(), b"boundary payloadNEWER", "{case}: the newest content");
                        assert!(helper_names(&source_root).is_empty(), "{case}: {:?}", helper_names(&source_root));
                        assert!(helper_names(&destination_root).is_empty(), "{case}: {:?}", helper_names(&destination_root));
                        assert!(matches!(reopened.phase, TransferFilePhase::Done | TransferFilePhase::Restored), "{case}");
                    }
                    fs::remove_dir_all(record_path.parent().unwrap()).unwrap();
                    fs::remove_dir_all(source_root).unwrap();
                    fs::remove_dir_all(destination_root).unwrap();
                }
            }
        }
    }
}
