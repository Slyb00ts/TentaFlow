// ===== File: code_studio/fs/mod.rs — the only filesystem a session ever touches =====
//
// Everything a session writes or reads goes through `SessionRoot`. There is one
// implementation, and the agent tool, the browser upload and the mesh call all
// enter through it, because three implementations would mean three places to
// get containment wrong and only one of them would be tested.
//
// **Why not `canonicalize` + `starts_with`.** That pattern answers a question
// about a path STRING at one moment, and the operation that follows re-resolves
// the same string later. Between the two, a path segment can become a symlink
// and the "verified" path lands outside the tree. It also cannot answer the
// question at all for a file that does not exist yet, which is exactly the case
// for every create. So containment here is a property of an open directory
// HANDLE: `SessionRoot` holds one for the session root, every operation
// resolves relative to it, and the final component is acted on with an `*at()`
// call against the parent handle it was verified in.
//
// **Three defences, all of them independent.**
//   1. Lexical. A path is relative, has no NUL, no `\`, no `:`, no `..`, and no
//      `.git` component in ANY case. Every one of those rules, including the
//      Windows-specific ones (reserved device names, a trailing dot or space,
//      characters Win32 cannot represent), is applied on EVERY platform and
//      lives in `validate_component` here — one guard, so a path written on
//      Linux cannot become a UNC path, a `\\?\` path, an alternate data stream,
//      a device or git metadata when the same repository is opened on a Windows
//      or macOS node, both of which resolve names case-insensitively.
//   2. Kernel. Linux resolves the whole relative path in one `openat2` with
//      `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`; every
//      other unix walks segments with `O_NOFOLLOW | O_DIRECTORY` and compares
//      `st_dev`/`st_ino` across the open; Windows refuses reparse points and
//      checks `GetFinalPathNameByHandle` against the parent. See the platform
//      modules for what each one buys.
//   3. Structural. Nothing in this module ever builds an absolute path string
//      to hand to the kernel. Recursive removal descends into directory
//      handles, atomic writes rename inside the parent handle, and traversal
//      opens each child from the handle of its parent.
//
// **Preconditions are the concurrency model.** `Precondition` is checked for
// `write`, `edit`, `remove` and `rename` alike (§13.2), because a delete that
// races an edit is exactly as damaging as two racing edits. The identity is the
// git blob object id — `sha1("blob <len>\0" + bytes)`, byte-identical to
// `git hash-object` — so the same value the agent saw in a tool result is the
// value that later goes into a commit built from accepted blobs (§11.5), with
// no re-hashing under a different scheme in between.
//
// **Recursive delete stops at git metadata.** `walk` skips `.git`, so no scan
// can read it; a recursive `remove` would otherwise be the one operation able
// to reach it, and a vendored checkout or a submodule would lose its history to
// a delete aimed at the directory above. The subtree is therefore SEARCHED for
// git metadata before anything is unlinked, and a hit refuses the whole
// removal — refusing mid-walk would leave the tree half deleted.
//
// **What this layer deliberately does not do.** It does not read `.gitignore`:
// a session tool that silently skipped files because of a file the agent can
// edit would be a confusing security boundary and an easy way to hide code from
// review. It excludes exactly three things from traversal, and each for a
// physical reason: `.git` (git metadata belongs to the broker, §7.3), files
// over the byte limits, and files whose first 8 KiB contain a NUL, which are
// binary and would only burn context.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use self::unix::{
    create_exclusive, mkdir_at, open_child_dir, open_file, open_root, read_dir, rename_at,
    rmdir_at, stat_at, stat_handle, sync_dir, unlink_at, DirHandle, RawEntry, RawStat,
};

#[cfg(target_os = "linux")]
use self::linux::resolve_dir;
#[cfg(all(unix, not(target_os = "linux")))]
use self::unix::resolve_dir;

#[cfg(windows)]
use self::windows::{
    create_exclusive, mkdir_at, open_child_dir, open_file, open_root, read_dir, rename_at,
    resolve_dir, rmdir_at, stat_at, stat_handle, sync_dir, unlink_at, DirHandle, RawEntry, RawStat,
};

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::RegexBuilder;
use sha1::{Digest, Sha1};

use super::paths;

/// Git metadata is the broker's, never the session's (§7.3). Refused as a path
/// component and skipped during traversal, and a recursive delete refuses a
/// subtree that contains it, so nothing here can read, write or destroy it.
const GIT_DIR: &str = ".git";

/// Names Win32 resolves to a DEVICE rather than to a file, in every directory
/// and with any extension appended.
const DEVICE_NAMES: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

/// Is this entry git's own metadata directory? Compared case-insensitively
/// because APFS and NTFS — both first-class targets (§21) — resolve `.GIT` to
/// `.git`, so a byte-exact comparison would guard the name on Linux only.
pub(crate) fn is_git_metadata(name: &str) -> bool {
    name.eq_ignore_ascii_case(GIT_DIR)
}

/// Name rules that hold on EVERY platform, whichever one the request arrives
/// on. A repository is written on one node and opened on another, so a name
/// whose meaning changes on the way — a device on Windows, a name whose
/// trailing dot or space Windows strips (`.git.` becomes `.git`), a character
/// Win32 cannot represent — is refused where it is created rather than
/// discovered where it does damage.
///
/// This is the ONLY definition of what a path component may be called.
/// `git_broker::validate_repo_path` guards the same names on their way into
/// git's argv, and two lists that drift apart mean one of the two guards passes
/// a name the other refuses.
pub(crate) fn validate_component(name: &str) -> Result<(), String> {
    if name.len() > MAX_COMPONENT_CHARS {
        return Err(format!(
            "component longer than {MAX_COMPONENT_CHARS} bytes"
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(format!(
            "'{name}' ends with a dot or space, which Windows silently strips"
        ));
    }
    if name.contains(':') {
        return Err(format!(
            "'{name}' contains ':', which names a drive or an alternate data stream"
        ));
    }
    for character in name.chars() {
        if (character as u32) < 32
            || character == '\u{7f}'
            || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        {
            return Err(format!("'{name}' contains a character Win32 cannot name"));
        }
    }
    let stem = name.split('.').next().unwrap_or(name);
    if DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return Err(format!("'{name}' is a reserved device name"));
    }
    Ok(())
}

/// Longest accepted request path and path component. The component bound is
/// the limit every filesystem we target enforces anyway; the path bound keeps
/// a pathological request from allocating before it is refused.
const MAX_PATH_CHARS: usize = 4096;
const MAX_COMPONENT_CHARS: usize = 255;

/// A file whose first 8 KiB contain a NUL is treated as binary and skipped by
/// `grep`. Reading further would not change the verdict and would cost the
/// scan budget.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Longest line fragment reported in a `GrepHit`. A minified bundle has lines
/// of megabytes, and the whole point of a hit is to be readable.
const MAX_HIT_CHARS: usize = 400;

/// How often the scan budget checks the clock. Checking on every entry would
/// make `Instant::now` a measurable part of a large tree walk.
const BUDGET_CLOCK_INTERVAL: usize = 256;

/// Server-side ceilings from §8.6 and §10. They exist because a tool result
/// travels into a model context and a scan travels over the mesh: an unbounded
/// answer is an outage, not a feature.
#[derive(Debug, Clone)]
pub struct FsLimits {
    /// Largest file this layer will read into memory, and therefore the largest
    /// file it can hash for a `Precondition` or return from `read`.
    pub max_read_bytes: u64,
    /// Largest content accepted by `write`.
    pub max_write_bytes: u64,
    /// Largest number of entries a single `list` may return.
    pub max_dir_entries: usize,
    /// Deepest a traversal descends below its starting directory.
    pub max_depth: u32,
    /// Entries a single `glob` or `grep` may look at before giving up.
    pub max_scan_entries: usize,
    /// Wall-clock budget for one `glob` or `grep`.
    pub scan_budget: Duration,
    /// Compiled size ceiling for a `grep` regex. A pattern that would need a
    /// larger automaton is refused instead of consuming the node's memory.
    pub regex_size_limit: usize,
}

impl Default for FsLimits {
    fn default() -> Self {
        FsLimits {
            max_read_bytes: 8 * 1024 * 1024,
            max_write_bytes: 8 * 1024 * 1024,
            max_dir_entries: 10_000,
            max_depth: 32,
            max_scan_entries: 200_000,
            scan_budget: Duration::from_secs(10),
            regex_size_limit: 1024 * 1024,
        }
    }
}

/// What the caller believes the target looks like right now. Enforced for
/// `write`, `edit`, `remove` and `rename` (§13.2, P1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Nothing may exist at the path.
    Absent,
    /// The path must hold exactly this git blob object id.
    BlobIs(String),
    /// No expectation. Used by upload and by the first write of a fresh file
    /// whose prior state genuinely does not matter.
    Any,
}

/// Line window of a `read`, 1-based like every editor and every diff.
#[derive(Debug, Clone, Copy)]
pub struct LineRange {
    pub start: u64,
    pub count: u64,
}

/// One replacement. `old_string` must occur EXACTLY once in the file: two
/// occurrences are an error rather than a silent choice of the first, because
/// the model cannot see which one it hit and would build its next edit on a
/// wrong picture of the file (§10).
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub blob_sha: String,
    pub bytes: u64,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub struct FileSlice {
    pub content: String,
    /// Blob id of the WHOLE file, never of the returned slice — it is the value
    /// a later `Precondition::BlobIs` is compared against.
    pub blob_sha: String,
    pub truncated: bool,
    pub total_lines: u64,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub is_symlink: bool,
}

#[derive(Debug, Clone)]
pub struct FileStat {
    pub path: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified_unix_ms: Option<i64>,
    pub readonly: bool,
    /// Present for regular files within `max_read_bytes`. This is what a caller
    /// reads to build the `Precondition` of its next operation.
    pub blob_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GrepQuery {
    pub pattern: String,
    pub is_regex: bool,
    /// Restricts the scan to paths matching this glob.
    pub glob: Option<String>,
    pub max_results: usize,
    pub max_bytes_per_file: u64,
}

#[derive(Debug, Clone)]
pub struct GrepHit {
    pub path: String,
    /// 1-based line number.
    pub line: u64,
    /// 1-based character column of the match within the line.
    pub column: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct GrepResult {
    pub hits: Vec<GrepHit>,
    pub files_scanned: usize,
    /// True when `max_results` cut the scan short, so the caller knows the
    /// answer is a prefix rather than the whole truth.
    pub truncated: bool,
}

/// Errors this layer distinguishes. `Conflict` is separate from every other
/// failure on purpose: it is the only one a caller can resolve by re-reading
/// and retrying, and the operation journal has to be able to tell a lost race
/// from a broken request.
#[derive(Debug)]
pub enum FsError {
    InvalidPath(String),
    InvalidRequest(String),
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    NotText,
    Conflict {
        expected: String,
        actual: Option<String>,
    },
    AmbiguousEdit {
        excerpt: String,
        matches: usize,
    },
    EditNotFound {
        excerpt: String,
    },
    TooLarge {
        size: u64,
        limit: u64,
    },
    LimitExceeded(String),
    Denied(String),
    Io(io::Error),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::InvalidPath(reason) => write!(f, "invalid path: {reason}"),
            FsError::InvalidRequest(reason) => write!(f, "invalid request: {reason}"),
            FsError::NotFound => write!(f, "no such file or directory"),
            FsError::AlreadyExists => write!(f, "already exists"),
            FsError::NotADirectory => write!(f, "not a directory"),
            FsError::IsADirectory => write!(f, "is a directory"),
            FsError::NotText => write!(f, "file is not valid UTF-8 text"),
            FsError::Conflict { expected, actual } => match actual {
                Some(actual) => write!(f, "conflict: expected {expected}, found {actual}"),
                None => write!(f, "conflict: expected {expected}, found nothing"),
            },
            FsError::AmbiguousEdit { excerpt, matches } => write!(
                f,
                "edit is ambiguous: {matches} occurrences of {excerpt:?}; extend it until it is unique"
            ),
            FsError::EditNotFound { excerpt } => {
                write!(f, "edit target {excerpt:?} does not occur in the file")
            }
            FsError::TooLarge { size, limit } => {
                write!(f, "{size} bytes exceeds the {limit} byte limit")
            }
            FsError::LimitExceeded(reason) => write!(f, "limit exceeded: {reason}"),
            FsError::Denied(reason) => write!(f, "refused: {reason}"),
            FsError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FsError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for FsError {
    fn from(err: io::Error) -> Self {
        map_io(err)
    }
}

/// Translates a kernel answer into the vocabulary above. The refusals that
/// matter for containment (`ELOOP` from a symlink, `EXDEV` from a
/// `RESOLVE_BENEATH` violation) become `Denied`, so a caller never mistakes a
/// blocked escape for a missing file.
fn map_io(err: io::Error) -> FsError {
    match err.kind() {
        io::ErrorKind::NotFound => FsError::NotFound,
        io::ErrorKind::AlreadyExists => FsError::AlreadyExists,
        io::ErrorKind::PermissionDenied => FsError::Denied(err.to_string()),
        _ => {
            #[cfg(unix)]
            if let Some(code) = err.raw_os_error() {
                if code == libc::ELOOP || code == libc::EXDEV {
                    return FsError::Denied(format!("path resolution refused: {err}"));
                }
                if code == libc::ENOTDIR {
                    return FsError::NotADirectory;
                }
            }
            FsError::Io(err)
        }
    }
}

/// A validated, normalized, relative path. Constructing one is the lexical half
/// of containment; nothing in this module accepts a bare string as a target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelPath {
    components: Vec<String>,
    text: String,
}

impl RelPath {
    /// The session root itself. Valid for `list`, `glob`, `grep` and `stat`;
    /// every mutating operation needs a path that names an entry.
    pub fn root() -> RelPath {
        RelPath {
            components: Vec::new(),
            text: String::new(),
        }
    }

    pub fn parse(raw: &str) -> Result<RelPath, FsError> {
        if raw.len() > MAX_PATH_CHARS {
            return Err(FsError::InvalidPath(format!(
                "longer than {MAX_PATH_CHARS} bytes"
            )));
        }
        if raw.contains('\0') {
            return Err(FsError::InvalidPath("contains a NUL byte".into()));
        }
        // Refused on every platform, not only on Windows: this is what keeps a
        // UNC path, a `\\?\` path and an alternate data stream from ever being
        // written into a repository on one node and opened on another.
        if raw.contains('\\') {
            return Err(FsError::InvalidPath(
                "contains a backslash; paths are '/'-separated and relative".into(),
            ));
        }
        if raw.starts_with('/') {
            return Err(FsError::InvalidPath("is absolute".into()));
        }

        let mut components = Vec::new();
        for segment in raw.split('/') {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                return Err(FsError::InvalidPath("contains '..'".into()));
            }
            if is_git_metadata(segment) {
                return Err(FsError::InvalidPath(
                    "git metadata belongs to the broker and is not reachable from a session".into(),
                ));
            }
            validate_component(segment).map_err(FsError::InvalidPath)?;
            components.push(segment.to_string());
        }

        let text = components.join("/");
        Ok(RelPath { components, text })
    }

    /// Directory holding this entry, or the root when the path has one
    /// component. Built from the already-validated components, so it can never
    /// widen what `parse` accepted.
    pub fn parent(&self) -> RelPath {
        let components: Vec<String> = self
            .components
            .iter()
            .take(self.components.len().saturating_sub(1))
            .cloned()
            .collect();
        let text = components.join("/");
        RelPath { components, text }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }

    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// Components of the directory the entry lives in.
    fn parent_components(&self) -> &[String] {
        match self.components.len() {
            0 => &[],
            n => &self.components[..n - 1],
        }
    }

    fn file_name(&self) -> Option<&str> {
        self.components.last().map(|s| s.as_str())
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Git blob object id of `content`: `sha1("blob <len>\0" + content)`. Identical
/// to `git hash-object`, which is what lets a commit be assembled from the very
/// blobs a review accepted (§11.5) instead of from whatever the worktree holds
/// at commit time.
pub fn blob_sha(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", content.len()).as_bytes());
    hasher.update(content);
    hex::encode(hasher.finalize())
}

// =============================================================================
// Workspace disk quota (§13.5)
// =============================================================================

/// Bytes one workspace may occupy when it names no quota of its own (§25.3).
/// There is no "unlimited": a workspace that never chose a number still has to
/// live inside one, or the column would only bind the people who filled it in.
pub const DEFAULT_WORKSPACE_DISK_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// How long a measurement is reused. Every REFUSAL re-measures regardless, so
/// this only bounds how far an ADMISSION can drift behind bytes that arrived
/// without passing through this module — a clone, a checkout, or a build
/// running in the sandbox.
const USAGE_TTL: Duration = Duration::from_secs(30);

/// Entries one measurement will visit, and how deep it goes. A tree past either
/// bound is refused rather than undercounted: a partial total is a quota that
/// opens on failure.
const MAX_USAGE_ENTRIES: u64 = 2_000_000;
const MAX_USAGE_DEPTH: u32 = 128;

/// Bytes the workspace directory holds right now, measured rather than
/// estimated: every file under `<data>/code-studio/<workspace_id>` is stat'ed,
/// which counts the repository, the worktrees, the artifacts and whatever a
/// command wrote, because all of them sit on the same disk the quota is about.
///
/// Symlinks are never followed — a link's target may live outside the workspace
/// and belongs to somebody else's quota.
pub fn workspace_disk_usage(workspace_id: &str) -> Result<u64, FsError> {
    let root =
        paths::workspace_dir(workspace_id).map_err(|err| FsError::InvalidPath(err.to_string()))?;
    let mut total = 0u64;
    let mut visited = 0u64;
    let mut stack = vec![(root, 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A directory removed between being listed and being descended into
            // holds no bytes any more; anything else is a measurement that did
            // not happen and must not be reported as a number.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(FsError::Io(err)),
        };
        for entry in entries {
            let entry = entry.map_err(FsError::Io)?;
            visited += 1;
            if visited > MAX_USAGE_ENTRIES {
                return Err(FsError::Denied(format!(
                    "workspace holds more than {MAX_USAGE_ENTRIES} entries and cannot be measured"
                )));
            }
            // `DirEntry::metadata` does not traverse symlinks.
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(FsError::Io(err)),
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if depth >= MAX_USAGE_DEPTH {
                    return Err(FsError::Denied(format!(
                        "workspace is deeper than {MAX_USAGE_DEPTH} levels and cannot be measured"
                    )));
                }
                stack.push((entry.path(), depth + 1));
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Last measurement of one workspace, shared by every session of it.
struct UsageCell {
    bytes: u64,
    /// `None` until the tree has been walked once — a zero with no measurement
    /// behind it would admit the first write of an already-full workspace.
    measured_at: Option<Instant>,
}

fn usage_cell(workspace_id: &str) -> Arc<Mutex<UsageCell>> {
    static CELLS: OnceLock<Mutex<HashMap<String, Arc<Mutex<UsageCell>>>>> = OnceLock::new();
    let cells = CELLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cells.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .entry(workspace_id.to_string())
        .or_insert_with(|| {
            Arc::new(Mutex::new(UsageCell {
                bytes: 0,
                measured_at: None,
            }))
        })
        .clone()
}

/// The disk allowance of one workspace, and the accounting that enforces it.
///
/// The cost of a write is the REAL delta — the length of what is about to be on
/// disk minus the length of what is there now — added to a measured total, not
/// an estimate of what a request "probably" costs.
#[derive(Debug, Clone)]
pub struct WorkspaceQuota {
    workspace_id: String,
    limit_bytes: u64,
}

impl WorkspaceQuota {
    /// A missing, zero or negative column means the workspace never chose a
    /// number, not that it may have the whole disk.
    pub fn new(workspace_id: &str, quota_disk_bytes: Option<i64>) -> WorkspaceQuota {
        WorkspaceQuota {
            workspace_id: workspace_id.to_string(),
            limit_bytes: quota_disk_bytes
                .and_then(|bytes| u64::try_from(bytes).ok())
                .filter(|bytes| *bytes > 0)
                .unwrap_or(DEFAULT_WORKSPACE_DISK_BYTES),
        }
    }

    pub fn limit_bytes(&self) -> u64 {
        self.limit_bytes
    }

    /// Refuses when the workspace ALREADY holds more than it may. Used after
    /// the operations that arrive in bulk — a clone, a checkout — where the
    /// size is not known until the bytes have landed.
    pub fn assert_within(&self) -> Result<(), FsError> {
        let cell = usage_cell(&self.workspace_id);
        let mut cell = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.remeasure(&mut cell)?;
        if cell.bytes > self.limit_bytes {
            return Err(self.exceeded(cell.bytes, 0));
        }
        Ok(())
    }

    /// Refuses when `additional` bytes would push the workspace past its limit.
    fn admit(&self, additional: u64) -> Result<(), FsError> {
        let cell = usage_cell(&self.workspace_id);
        let mut cell = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let stale = match cell.measured_at {
            Some(at) => at.elapsed() >= USAGE_TTL,
            None => true,
        };
        if stale {
            self.remeasure(&mut cell)?;
        }
        if cell.bytes.saturating_add(additional) > self.limit_bytes {
            // A refusal is never decided on a cached number: bytes may have
            // been freed since the last walk, and telling somebody their
            // workspace is full when it is not is its own kind of wrong.
            self.remeasure(&mut cell)?;
            if cell.bytes.saturating_add(additional) > self.limit_bytes {
                return Err(self.exceeded(cell.bytes, additional));
            }
        }
        Ok(())
    }

    fn remeasure(&self, cell: &mut UsageCell) -> Result<(), FsError> {
        cell.bytes = workspace_disk_usage(&self.workspace_id)?;
        cell.measured_at = Some(Instant::now());
        Ok(())
    }

    fn exceeded(&self, usage: u64, additional: u64) -> FsError {
        FsError::LimitExceeded(format!(
            "workspace disk quota: {usage} of {} bytes used, this operation needs {additional} more",
            self.limit_bytes
        ))
    }

    /// Books bytes that just landed, so the next admission does not need a walk
    /// to know about them.
    fn charge(&self, added: u64) {
        if added == 0 {
            return;
        }
        let cell = usage_cell(&self.workspace_id);
        let mut cell = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cell.bytes = cell.bytes.saturating_add(added);
    }

    fn credit(&self, freed: u64) {
        if freed == 0 {
            return;
        }
        let cell = usage_cell(&self.workspace_id);
        let mut cell = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cell.bytes = cell.bytes.saturating_sub(freed);
    }

    /// Drops the measurement after a change whose size is not known — a
    /// recursive delete. The next admission walks the tree instead of trusting
    /// a number nobody adjusted.
    fn invalidate(&self) {
        let cell = usage_cell(&self.workspace_id);
        let mut cell = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cell.measured_at = None;
    }
}

/// The session worktree, held open. Cheap to clone descriptors from and safe to
/// share across threads, so one instance serves a session for its whole life.
pub struct SessionRoot {
    root: DirHandle,
    limits: FsLimits,
    /// Present when the root belongs to a workspace, which is every session.
    /// `open` on a bare directory has no workspace to measure and therefore no
    /// quota to enforce — it exists for server-internal roots and for tests.
    quota: Option<WorkspaceQuota>,
}

impl fmt::Debug for SessionRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRoot")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SessionRoot {
    /// Opens an arbitrary directory as a session root. The path must come from
    /// the server (`code_studio::paths`), never from a request.
    pub fn open(path: &Path) -> Result<SessionRoot, FsError> {
        SessionRoot::open_with_limits(path, FsLimits::default())
    }

    pub fn open_with_limits(path: &Path, limits: FsLimits) -> Result<SessionRoot, FsError> {
        let root = open_root(path).map_err(map_io)?;
        Ok(SessionRoot {
            root,
            limits,
            quota: None,
        })
    }

    /// Opens the worktree of a session by identity rather than by path, so the
    /// caller never has a chance to compose a directory of its own.
    ///
    /// The disk allowance comes from the workspace's own runtime database — the
    /// reservation the provisioning saga made on this node (§6 S1) — because
    /// this call is reached from the tool path, from the protocol handlers and
    /// from the mesh with nothing but two identifiers in hand, and a quota that
    /// only some of those three carried would not be a quota.
    pub fn open_session(workspace_id: &str, session_id: &str) -> Result<SessionRoot, FsError> {
        let path = paths::session_worktree_dir(workspace_id, session_id)
            .map_err(|err| FsError::InvalidPath(err.to_string()))?;
        let reserved = super::workspace_db::disk_quota(workspace_id).map_err(|err| {
            FsError::Denied(format!("workspace disk reservation is unreadable: {err:#}"))
        })?;
        let root = open_root(&path).map_err(map_io)?;
        Ok(SessionRoot {
            root,
            limits: FsLimits::default(),
            quota: Some(WorkspaceQuota::new(workspace_id, reserved)),
        })
    }

    pub fn limits(&self) -> &FsLimits {
        &self.limits
    }

    // ----- reads -------------------------------------------------------

    pub fn read(&self, path: &RelPath, range: Option<LineRange>) -> Result<FileSlice, FsError> {
        let (dir, name) = self.open_parent(path)?;
        let stat = self.entry_stat(&dir, &name)?.ok_or(FsError::NotFound)?;
        if stat.is_symlink {
            return Err(FsError::Denied("refusing to read through a symlink".into()));
        }
        if stat.is_dir {
            return Err(FsError::IsADirectory);
        }

        let bytes = read_file(&dir, &name, self.limits.max_read_bytes)?;
        let blob = blob_sha(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| FsError::NotText)?;

        // `split_inclusive` keeps the line terminators, so a slice can be
        // concatenated back without inventing or losing a trailing newline.
        let lines: Vec<&str> = if text.is_empty() {
            Vec::new()
        } else {
            text.split_inclusive('\n').collect()
        };
        let total_lines = lines.len() as u64;

        let (from, to) = match range {
            Some(range) => {
                let from = range.start.saturating_sub(1).min(total_lines) as usize;
                let to = from.saturating_add(range.count.min(total_lines) as usize);
                (from, to.min(lines.len()))
            }
            None => (0, lines.len()),
        };
        let content: String = lines[from..to].concat();

        Ok(FileSlice {
            content,
            blob_sha: blob,
            truncated: from > 0 || to < lines.len(),
            total_lines,
        })
    }

    pub fn stat(&self, path: &RelPath) -> Result<FileStat, FsError> {
        if path.is_root() {
            let stat = stat_handle(&self.root).map_err(map_io)?;
            return Ok(self.file_stat(path, &stat, None));
        }
        let (dir, name) = self.open_parent(path)?;
        let stat = self.entry_stat(&dir, &name)?.ok_or(FsError::NotFound)?;
        let blob = if stat.is_file && stat.size <= self.limits.max_read_bytes {
            Some(blob_sha(&read_file(
                &dir,
                &name,
                self.limits.max_read_bytes,
            )?))
        } else {
            None
        };
        Ok(self.file_stat(path, &stat, blob))
    }

    pub fn list(&self, path: &RelPath, depth: u32) -> Result<Vec<DirEntry>, FsError> {
        let depth = depth.clamp(1, self.limits.max_depth);
        let dir = resolve_dir(&self.root, path.components()).map_err(map_io)?;
        let mut visitor = ListVisitor {
            entries: Vec::new(),
            max_entries: self.limits.max_dir_entries,
        };
        let mut budget = self.budget();
        let mut prefix = path.components().to_vec();
        self.walk(&dir, &mut prefix, depth, &mut budget, &mut visitor)?;
        Ok(visitor.entries)
    }

    pub fn glob(&self, pattern: &str, limit: usize) -> Result<Vec<RelPath>, FsError> {
        let matcher = Glob::parse(pattern)?;
        let limit = limit.clamp(1, self.limits.max_dir_entries);
        let mut visitor = GlobVisitor {
            matcher,
            matches: Vec::new(),
            limit,
        };
        let mut budget = self.budget();
        let mut prefix = Vec::new();
        self.walk(
            &self.root,
            &mut prefix,
            self.limits.max_depth,
            &mut budget,
            &mut visitor,
        )?;
        Ok(visitor.matches)
    }

    pub fn grep(&self, query: &GrepQuery) -> Result<GrepResult, FsError> {
        if query.pattern.is_empty() {
            return Err(FsError::InvalidRequest("empty grep pattern".into()));
        }
        let matcher = if query.is_regex {
            LineMatcher::Pattern(Box::new(
                RegexBuilder::new(&query.pattern)
                    .size_limit(self.limits.regex_size_limit)
                    .dfa_size_limit(self.limits.regex_size_limit)
                    .build()
                    .map_err(|err| FsError::InvalidRequest(format!("bad regex: {err}")))?,
            ))
        } else {
            LineMatcher::Literal(query.pattern.clone())
        };
        let filter = match &query.glob {
            Some(pattern) => Some(Glob::parse(pattern)?),
            None => None,
        };

        let mut visitor = GrepVisitor {
            matcher,
            filter,
            max_results: query.max_results.clamp(1, self.limits.max_dir_entries),
            max_bytes_per_file: query.max_bytes_per_file.min(self.limits.max_read_bytes),
            hits: Vec::new(),
            files_scanned: 0,
            truncated: false,
        };
        let mut budget = self.budget();
        let mut prefix = Vec::new();
        self.walk(
            &self.root,
            &mut prefix,
            self.limits.max_depth,
            &mut budget,
            &mut visitor,
        )?;

        Ok(GrepResult {
            hits: visitor.hits,
            files_scanned: visitor.files_scanned,
            truncated: visitor.truncated,
        })
    }

    // ----- writes ------------------------------------------------------

    pub fn write(
        &self,
        path: &RelPath,
        content: &[u8],
        expect: Precondition,
    ) -> Result<WriteOutcome, FsError> {
        if content.len() as u64 > self.limits.max_write_bytes {
            return Err(FsError::TooLarge {
                size: content.len() as u64,
                limit: self.limits.max_write_bytes,
            });
        }
        let (dir, name) = self.open_parent_for_write(path, &expect)?;
        let current = self.entry_stat(&dir, &name)?;
        if let Some(stat) = &current {
            if stat.is_dir {
                return Err(FsError::IsADirectory);
            }
            if stat.is_symlink {
                return Err(FsError::Denied(
                    "refusing to write through a symlink".into(),
                ));
            }
        }
        enforce_precondition(&expect, current.is_some(), || {
            self.blob_at(&dir, &name, current.as_ref())
        })?;

        let previous = current.as_ref().map(|stat| stat.size).unwrap_or(0);
        self.admit_growth(content.len() as u64, previous)?;

        let created = current.is_none();
        if created {
            // Claim the name atomically before any content exists, so two
            // concurrent creators lose the race here rather than after both
            // have written a full file.
            drop(create_exclusive(&dir, &name).map_err(map_io)?);
        }
        if let Err(err) = atomic_replace(&dir, &name, content) {
            if created {
                let _ = unlink_at(&dir, &name);
            }
            return Err(err);
        }
        self.book_growth(content.len() as u64, previous);

        Ok(WriteOutcome {
            blob_sha: blob_sha(content),
            bytes: content.len() as u64,
            created,
        })
    }

    pub fn edit(
        &self,
        path: &RelPath,
        edits: &[TextEdit],
        expect: Precondition,
    ) -> Result<WriteOutcome, FsError> {
        if edits.is_empty() {
            return Err(FsError::InvalidRequest("no edits given".into()));
        }
        let (dir, name) = self.open_parent(path)?;
        let stat = self.entry_stat(&dir, &name)?.ok_or(FsError::NotFound)?;
        if stat.is_symlink {
            return Err(FsError::Denied("refusing to edit through a symlink".into()));
        }
        if stat.is_dir {
            return Err(FsError::IsADirectory);
        }

        let bytes = read_file(&dir, &name, self.limits.max_read_bytes)?;
        let current = blob_sha(&bytes);
        enforce_precondition(&expect, true, || Ok(current.clone()))?;

        let mut text = String::from_utf8(bytes).map_err(|_| FsError::NotText)?;
        for edit in edits {
            if edit.old_string.is_empty() {
                return Err(FsError::InvalidRequest("empty old_string".into()));
            }
            let matches = text.matches(&edit.old_string).count();
            match matches {
                0 => {
                    return Err(FsError::EditNotFound {
                        excerpt: excerpt(&edit.old_string),
                    })
                }
                1 => {}
                _ => {
                    return Err(FsError::AmbiguousEdit {
                        excerpt: excerpt(&edit.old_string),
                        matches,
                    })
                }
            }
            let at = text
                .find(&edit.old_string)
                .expect("occurrence counted above");
            text.replace_range(at..at + edit.old_string.len(), &edit.new_string);
        }

        let content = text.into_bytes();
        if content.len() as u64 > self.limits.max_write_bytes {
            return Err(FsError::TooLarge {
                size: content.len() as u64,
                limit: self.limits.max_write_bytes,
            });
        }
        self.admit_growth(content.len() as u64, stat.size)?;
        atomic_replace(&dir, &name, &content)?;
        self.book_growth(content.len() as u64, stat.size);
        Ok(WriteOutcome {
            blob_sha: blob_sha(&content),
            bytes: content.len() as u64,
            created: false,
        })
    }

    /// Refuses a write whose NET growth would take the workspace past its disk
    /// allowance. A write that shrinks a file costs nothing and is never
    /// refused, which is what keeps a full workspace repairable from inside.
    fn admit_growth(&self, new_size: u64, previous_size: u64) -> Result<(), FsError> {
        match &self.quota {
            Some(quota) => quota.admit(new_size.saturating_sub(previous_size)),
            None => Ok(()),
        }
    }

    fn book_growth(&self, new_size: u64, previous_size: u64) {
        let Some(quota) = &self.quota else {
            return;
        };
        if new_size >= previous_size {
            quota.charge(new_size - previous_size);
        } else {
            quota.credit(previous_size - new_size);
        }
    }

    /// Creates `path` and every missing directory above it. Each level is
    /// created and then re-opened through its parent's handle, so a level that
    /// exists as a file or as a symlink stops the walk instead of being
    /// traversed.
    pub fn mkdir(&self, path: &RelPath) -> Result<(), FsError> {
        if path.is_root() {
            return Ok(());
        }
        if path.components().len() as u32 > self.limits.max_depth {
            return Err(FsError::LimitExceeded(format!(
                "deeper than {} levels",
                self.limits.max_depth
            )));
        }
        let mut current = self.root.try_clone().map_err(map_io)?;
        for component in path.components() {
            match mkdir_at(&current, component) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(map_io(err)),
            }
            current = open_child_dir(&current, component).map_err(map_io)?;
        }
        Ok(())
    }

    pub fn remove(
        &self,
        path: &RelPath,
        recursive: bool,
        expect: Precondition,
    ) -> Result<(), FsError> {
        if path.is_root() {
            return Err(FsError::Denied(
                "refusing to remove the session root".into(),
            ));
        }
        let (dir, name) = self.open_parent(path)?;
        let current = self.entry_stat(&dir, &name)?;
        enforce_precondition(&expect, current.is_some(), || {
            self.blob_at(&dir, &name, current.as_ref())
        })?;

        let Some(stat) = current else {
            // `Absent` and absent: the effect the caller asked for is already
            // in place, which is what makes a retried delete safe (§ recovery).
            return match expect {
                Precondition::Absent => Ok(()),
                _ => Err(FsError::NotFound),
            };
        };

        if stat.is_symlink || !stat.is_dir {
            unlink_at(&dir, &name).map_err(map_io)?;
            if let Some(quota) = &self.quota {
                quota.credit(stat.size);
            }
            return Ok(());
        }
        if !recursive {
            return rmdir_at(&dir, &name).map_err(map_io);
        }
        if let Some(found) = self.find_git_metadata(&dir, &name, path.as_str(), 0)? {
            return Err(FsError::Denied(format!(
                "{found} is git metadata; remove the entries you own instead"
            )));
        }
        self.remove_tree(&dir, &name, 0)?;
        // How much a subtree held is not known without walking it, and the walk
        // it would take is the same walk the next admission does anyway.
        if let Some(quota) = &self.quota {
            quota.invalidate();
        }
        Ok(())
    }

    pub fn rename(
        &self,
        from: &RelPath,
        to: &RelPath,
        expect: Precondition,
    ) -> Result<(), FsError> {
        if from.is_root() || to.is_root() {
            return Err(FsError::InvalidPath(
                "rename needs two paths that name an entry".into(),
            ));
        }
        if from == to {
            return Err(FsError::InvalidRequest(
                "source and destination are the same path".into(),
            ));
        }

        let (from_dir, from_name) = self.open_parent(from)?;
        let source = self.entry_stat(&from_dir, &from_name)?;
        enforce_precondition(&expect, source.is_some(), || {
            self.blob_at(&from_dir, &from_name, source.as_ref())
        })?;
        if source.is_none() {
            return Err(FsError::NotFound);
        }

        let (to_dir, to_name) = self.open_parent(to)?;
        // The destination must be absent (§13.2): a rename that replaced an
        // existing file would destroy content nobody agreed to lose.
        if self.entry_stat(&to_dir, &to_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        rename_at(&from_dir, &from_name, &to_dir, &to_name).map_err(map_io)
    }

    // ----- internals ---------------------------------------------------

    /// Parent handle for a write, creating the directory chain when the write
    /// is a creation.
    ///
    /// A caller that names `src/api/handler.rs` in a tree that has no `src/`
    /// means to create the file, and demanding a separate `mkdir` first only
    /// buys a failed tool call. A `BlobIs` precondition is different: it asserts
    /// the file already holds specific content, so a missing parent is a real
    /// `NotFound` and must stay one. The chain is created by the same
    /// segment-by-segment handle walk as `mkdir`, so containment is identical —
    /// nothing here can follow a symlink out of the worktree.
    fn open_parent_for_write(
        &self,
        path: &RelPath,
        expect: &Precondition,
    ) -> Result<(DirHandle, String), FsError> {
        match self.open_parent(path) {
            Ok(found) => Ok(found),
            Err(FsError::NotFound) if !matches!(expect, Precondition::BlobIs(_)) => {
                self.mkdir(&path.parent())?;
                self.open_parent(path)
            }
            Err(err) => Err(err),
        }
    }

    fn open_parent(&self, path: &RelPath) -> Result<(DirHandle, String), FsError> {
        let Some(name) = path.file_name() else {
            return Err(FsError::InvalidPath(
                "this operation needs a path that names an entry".into(),
            ));
        };
        let dir = resolve_dir(&self.root, path.parent_components()).map_err(map_io)?;
        Ok((dir, name.to_string()))
    }

    fn entry_stat(&self, dir: &DirHandle, name: &str) -> Result<Option<RawStat>, FsError> {
        match stat_at(dir, name) {
            Ok(stat) => Ok(Some(stat)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(map_io(err)),
        }
    }

    fn blob_at(
        &self,
        dir: &DirHandle,
        name: &str,
        stat: Option<&RawStat>,
    ) -> Result<String, FsError> {
        let Some(stat) = stat else {
            return Err(FsError::NotFound);
        };
        if stat.is_symlink {
            return Err(FsError::Denied("a symlink has no blob id".into()));
        }
        if stat.is_dir {
            return Err(FsError::IsADirectory);
        }
        Ok(blob_sha(&read_file(dir, name, self.limits.max_read_bytes)?))
    }

    fn file_stat(&self, path: &RelPath, stat: &RawStat, blob: Option<String>) -> FileStat {
        FileStat {
            path: path.as_str().to_string(),
            is_dir: stat.is_dir,
            is_file: stat.is_file,
            is_symlink: stat.is_symlink,
            size: stat.size,
            modified_unix_ms: stat.modified_unix_ms,
            readonly: stat.readonly,
            blob_sha: blob,
        }
    }

    fn budget(&self) -> ScanBudget {
        ScanBudget {
            started: Instant::now(),
            seen: 0,
            max_entries: self.limits.max_scan_entries,
            deadline: self.limits.scan_budget,
        }
    }

    /// Relative path of the first git metadata directory below `name`, or
    /// `None`. Run before a recursive removal starts unlinking, so a subtree
    /// holding a nested repository or a submodule is refused as a whole instead
    /// of being half deleted and then failing on the entry that must survive.
    /// It descends the same way `remove_tree` does — handle by handle, never
    /// through a symlink — and stops at the same depth, so it cannot approve a
    /// tree deeper than the removal would walk.
    fn find_git_metadata(
        &self,
        parent: &DirHandle,
        name: &str,
        prefix: &str,
        depth: u32,
    ) -> Result<Option<String>, FsError> {
        if depth > self.limits.max_depth {
            return Err(FsError::LimitExceeded(format!(
                "tree deeper than {} levels",
                self.limits.max_depth
            )));
        }
        let dir = open_child_dir(parent, name).map_err(map_io)?;
        for entry in read_dir(&dir).map_err(map_io)? {
            let path = format!("{prefix}/{}", entry.name);
            if is_git_metadata(&entry.name) {
                return Ok(Some(path));
            }
            if entry.is_dir && !entry.is_symlink {
                if let Some(found) = self.find_git_metadata(&dir, &entry.name, &path, depth + 1)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    /// Depth-first removal that never composes a path string: each level is
    /// opened from the handle of the level above, emptied, closed, and only
    /// then removed from its parent.
    fn remove_tree(&self, parent: &DirHandle, name: &str, depth: u32) -> Result<(), FsError> {
        if depth > self.limits.max_depth {
            return Err(FsError::LimitExceeded(format!(
                "tree deeper than {} levels",
                self.limits.max_depth
            )));
        }
        let dir = open_child_dir(parent, name).map_err(map_io)?;
        for entry in read_dir(&dir).map_err(map_io)? {
            if entry.is_dir && !entry.is_symlink {
                self.remove_tree(&dir, &entry.name, depth + 1)?;
            } else {
                unlink_at(&dir, &entry.name).map_err(map_io)?;
            }
        }
        // The handle has to be gone before the directory can be removed on
        // Windows, and dropping it first costs nothing on unix.
        drop(dir);
        rmdir_at(parent, name).map_err(map_io)
    }

    fn walk(
        &self,
        dir: &DirHandle,
        prefix: &mut Vec<String>,
        depth_left: u32,
        budget: &mut ScanBudget,
        visitor: &mut dyn TreeVisitor,
    ) -> Result<Flow, FsError> {
        let mut entries = read_dir(dir).map_err(map_io)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        for entry in entries {
            if is_git_metadata(&entry.name) {
                continue;
            }
            budget.tick()?;

            prefix.push(entry.name.clone());
            let relative = prefix.join("/");
            let mut flow = visitor.visit(&relative, &entry, dir)?;

            if matches!(flow, Flow::Continue) && entry.is_dir && !entry.is_symlink && depth_left > 1
            {
                match open_child_dir(dir, &entry.name) {
                    Ok(child) => {
                        flow = self.walk(&child, prefix, depth_left - 1, budget, visitor)?;
                    }
                    // The entry was listed and then removed, or it turned out
                    // not to be a plain directory after all. Either way it is
                    // not part of the answer.
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(map_io(err)),
                }
            }

            prefix.pop();
            if matches!(flow, Flow::Stop) {
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }
}

enum Flow {
    Continue,
    Stop,
}

trait TreeVisitor {
    fn visit(&mut self, relative: &str, entry: &RawEntry, dir: &DirHandle)
        -> Result<Flow, FsError>;
}

struct ListVisitor {
    entries: Vec<DirEntry>,
    max_entries: usize,
}

impl TreeVisitor for ListVisitor {
    fn visit(
        &mut self,
        relative: &str,
        entry: &RawEntry,
        dir: &DirHandle,
    ) -> Result<Flow, FsError> {
        let size = match stat_at(dir, &entry.name) {
            Ok(stat) => stat.size,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Flow::Continue),
            Err(err) => return Err(map_io(err)),
        };
        if self.entries.len() >= self.max_entries {
            return Err(FsError::LimitExceeded(format!(
                "more than {} entries",
                self.max_entries
            )));
        }
        self.entries.push(DirEntry {
            path: relative.to_string(),
            is_dir: entry.is_dir,
            size,
            is_symlink: entry.is_symlink,
        });
        Ok(Flow::Continue)
    }
}

struct GlobVisitor {
    matcher: Glob,
    matches: Vec<RelPath>,
    limit: usize,
}

impl TreeVisitor for GlobVisitor {
    fn visit(
        &mut self,
        relative: &str,
        entry: &RawEntry,
        _dir: &DirHandle,
    ) -> Result<Flow, FsError> {
        if entry.is_symlink || !self.matcher.matches(relative) {
            return Ok(Flow::Continue);
        }
        // A name this layer would refuse as a request path is a name it cannot
        // act on, so returning it would only produce a broken follow-up call.
        if let Ok(path) = RelPath::parse(relative) {
            self.matches.push(path);
        }
        if self.matches.len() >= self.limit {
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }
}

enum LineMatcher {
    Literal(String),
    Pattern(Box<regex::Regex>),
}

impl LineMatcher {
    /// Byte offset of the first match in `line`, or `None`.
    fn find(&self, line: &str) -> Option<usize> {
        match self {
            LineMatcher::Literal(needle) => line.find(needle.as_str()),
            LineMatcher::Pattern(regex) => regex.find(line).map(|m| m.start()),
        }
    }
}

struct GrepVisitor {
    matcher: LineMatcher,
    filter: Option<Glob>,
    max_results: usize,
    max_bytes_per_file: u64,
    hits: Vec<GrepHit>,
    files_scanned: usize,
    truncated: bool,
}

impl TreeVisitor for GrepVisitor {
    fn visit(
        &mut self,
        relative: &str,
        entry: &RawEntry,
        dir: &DirHandle,
    ) -> Result<Flow, FsError> {
        if entry.is_dir || entry.is_symlink {
            return Ok(Flow::Continue);
        }
        if let Some(filter) = &self.filter {
            if !filter.matches(relative) {
                return Ok(Flow::Continue);
            }
        }
        let stat = match stat_at(dir, &entry.name) {
            Ok(stat) => stat,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Flow::Continue),
            Err(err) => return Err(map_io(err)),
        };
        if stat.size > self.max_bytes_per_file {
            return Ok(Flow::Continue);
        }
        let bytes = match read_file(dir, &entry.name, self.max_bytes_per_file) {
            Ok(bytes) => bytes,
            Err(FsError::NotFound) | Err(FsError::TooLarge { .. }) | Err(FsError::Denied(_)) => {
                return Ok(Flow::Continue)
            }
            Err(err) => return Err(err),
        };
        if is_binary(&bytes) {
            return Ok(Flow::Continue);
        }
        self.files_scanned += 1;

        // Lossy on purpose: a latin-1 source file still greps usefully, and the
        // alternative is skipping files for a reason that has nothing to do
        // with the query.
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            let Some(offset) = self.matcher.find(line) else {
                continue;
            };
            self.hits.push(GrepHit {
                path: relative.to_string(),
                line: index as u64 + 1,
                column: line[..offset].chars().count() as u64 + 1,
                text: truncate_chars(line, MAX_HIT_CHARS),
            });
            if self.hits.len() >= self.max_results {
                self.truncated = true;
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }
}

struct ScanBudget {
    started: Instant,
    seen: usize,
    max_entries: usize,
    deadline: Duration,
}

impl ScanBudget {
    fn tick(&mut self) -> Result<(), FsError> {
        self.seen += 1;
        if self.seen > self.max_entries {
            return Err(FsError::LimitExceeded(format!(
                "scan looked at more than {} entries",
                self.max_entries
            )));
        }
        if self.seen % BUDGET_CLOCK_INTERVAL == 0 && self.started.elapsed() > self.deadline {
            return Err(FsError::LimitExceeded(format!(
                "scan exceeded {:?}",
                self.deadline
            )));
        }
        Ok(())
    }
}

fn enforce_precondition(
    expect: &Precondition,
    exists: bool,
    current: impl FnOnce() -> Result<String, FsError>,
) -> Result<(), FsError> {
    match expect {
        Precondition::Any => Ok(()),
        Precondition::Absent => {
            if exists {
                Err(FsError::Conflict {
                    expected: "absent".into(),
                    actual: Some(current().unwrap_or_else(|_| "an existing entry".into())),
                })
            } else {
                Ok(())
            }
        }
        Precondition::BlobIs(want) => {
            if !exists {
                return Err(FsError::Conflict {
                    expected: want.clone(),
                    actual: None,
                });
            }
            let found = current()?;
            if &found == want {
                Ok(())
            } else {
                Err(FsError::Conflict {
                    expected: want.clone(),
                    actual: Some(found),
                })
            }
        }
    }
}

fn read_file(dir: &DirHandle, name: &str, limit: u64) -> Result<Vec<u8>, FsError> {
    let file = open_file(dir, name).map_err(map_io)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(FsError::Io)?;
    if bytes.len() as u64 > limit {
        return Err(FsError::TooLarge {
            size: bytes.len() as u64,
            limit,
        });
    }
    Ok(bytes)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0)
}

/// Temporary name for an atomic write. Unique per process and per call, and
/// prefixed so an interrupted write is recognizable in a worktree.
fn temp_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    format!(".tf-fs-{}-{sequence}-{nanos}.tmp", std::process::id())
}

/// Writes `content` into a temporary file in the SAME directory handle and
/// renames it over `name`. A reader therefore sees either the old file or the
/// whole new one, never a half-written file, and a failure anywhere before the
/// rename leaves the target exactly as it was.
fn atomic_replace(dir: &DirHandle, name: &str, content: &[u8]) -> Result<(), FsError> {
    let mut attempts = 0;
    let (temp, mut file) = loop {
        let candidate = temp_name();
        match create_exclusive(dir, &candidate) {
            Ok(file) => break (candidate, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempts < 8 => {
                attempts += 1;
            }
            Err(err) => return Err(map_io(err)),
        }
    };

    let written = file.write_all(content).and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = written {
        let _ = unlink_at(dir, &temp);
        return Err(FsError::Io(err));
    }
    if let Err(err) = rename_at(dir, &temp, dir, name) {
        let _ = unlink_at(dir, &temp);
        return Err(map_io(err));
    }
    sync_dir(dir);
    Ok(())
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

/// Short, quoted form of an edit target for an error message. Full file content
/// must not travel into logs, and an error is enough to identify the edit
/// without reproducing it.
fn excerpt(value: &str) -> String {
    truncate_chars(value, 60)
}

// ----- glob ------------------------------------------------------------

/// `*` (within one component), `**` (any number of components), `?` and
/// `[...]` classes. No brace expansion and no escaping: `\` is already refused
/// everywhere in this module, and a pattern language nobody can escape from is
/// a pattern language with no ambiguity about what a literal `*` means.
struct Glob {
    parts: Vec<GlobPart>,
}

enum GlobPart {
    AnyDepth,
    Segment(Vec<GlobToken>),
}

enum GlobToken {
    Literal(char),
    AnyRun,
    One,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
}

enum ClassItem {
    Char(char),
    Range(char, char),
}

/// Backtracking is exponential in the number of `*`, so the pattern itself is
/// bounded rather than the search.
const MAX_GLOB_CHARS: usize = 512;
const MAX_GLOB_WILDCARDS: usize = 32;

impl Glob {
    fn parse(pattern: &str) -> Result<Glob, FsError> {
        if pattern.is_empty() {
            return Err(FsError::InvalidRequest("empty glob pattern".into()));
        }
        if pattern.len() > MAX_GLOB_CHARS {
            return Err(FsError::InvalidRequest(format!(
                "glob pattern longer than {MAX_GLOB_CHARS} bytes"
            )));
        }
        if pattern.contains('\0') || pattern.contains('\\') {
            return Err(FsError::InvalidRequest(
                "glob pattern contains a NUL or a backslash".into(),
            ));
        }
        if pattern.matches('*').count() > MAX_GLOB_WILDCARDS {
            return Err(FsError::InvalidRequest(format!(
                "glob pattern has more than {MAX_GLOB_WILDCARDS} wildcards"
            )));
        }

        let mut parts = Vec::new();
        for segment in pattern.split('/') {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                return Err(FsError::InvalidRequest("glob pattern contains '..'".into()));
            }
            if segment.chars().all(|c| c == '*') && segment.len() > 1 {
                parts.push(GlobPart::AnyDepth);
                continue;
            }
            parts.push(GlobPart::Segment(parse_glob_segment(segment)?));
        }
        if parts.is_empty() {
            return Err(FsError::InvalidRequest(
                "glob pattern matches nothing".into(),
            ));
        }
        Ok(Glob { parts })
    }

    fn matches(&self, path: &str) -> bool {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        match_glob_parts(&self.parts, &components)
    }
}

fn parse_glob_segment(segment: &str) -> Result<Vec<GlobToken>, FsError> {
    let mut tokens = Vec::new();
    let mut chars = segment.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '*' => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                tokens.push(GlobToken::AnyRun);
            }
            '?' => tokens.push(GlobToken::One),
            '[' => {
                let negated = matches!(chars.peek(), Some('!') | Some('^'));
                if negated {
                    chars.next();
                }
                let mut items = Vec::new();
                let mut closed = false;
                while let Some(member) = chars.next() {
                    if member == ']' && !items.is_empty() {
                        closed = true;
                        break;
                    }
                    if chars.peek() == Some(&'-') {
                        let mut lookahead = chars.clone();
                        lookahead.next();
                        if let Some(&end) = lookahead.peek() {
                            if end != ']' {
                                chars.next();
                                chars.next();
                                items.push(ClassItem::Range(member, end));
                                continue;
                            }
                        }
                    }
                    items.push(ClassItem::Char(member));
                }
                if !closed {
                    return Err(FsError::InvalidRequest(
                        "unterminated '[' in glob pattern".into(),
                    ));
                }
                tokens.push(GlobToken::Class { negated, items });
            }
            literal => tokens.push(GlobToken::Literal(literal)),
        }
    }
    Ok(tokens)
}

fn match_glob_parts(parts: &[GlobPart], components: &[&str]) -> bool {
    match parts.split_first() {
        None => components.is_empty(),
        Some((GlobPart::AnyDepth, rest)) => {
            (0..=components.len()).any(|skip| match_glob_parts(rest, &components[skip..]))
        }
        Some((GlobPart::Segment(tokens), rest)) => {
            !components.is_empty()
                && match_glob_tokens(tokens, &components[0].chars().collect::<Vec<char>>())
                && match_glob_parts(rest, &components[1..])
        }
    }
}

fn match_glob_tokens(tokens: &[GlobToken], value: &[char]) -> bool {
    match tokens.split_first() {
        None => value.is_empty(),
        Some((GlobToken::AnyRun, rest)) => {
            (0..=value.len()).any(|skip| match_glob_tokens(rest, &value[skip..]))
        }
        Some((GlobToken::One, rest)) => !value.is_empty() && match_glob_tokens(rest, &value[1..]),
        Some((GlobToken::Literal(expected), rest)) => {
            !value.is_empty() && value[0] == *expected && match_glob_tokens(rest, &value[1..])
        }
        Some((GlobToken::Class { negated, items }, rest)) => {
            !value.is_empty()
                && (class_matches(items, value[0]) != *negated)
                && match_glob_tokens(rest, &value[1..])
        }
    }
}

fn class_matches(items: &[ClassItem], candidate: char) -> bool {
    items.iter().any(|item| match item {
        ClassItem::Char(expected) => *expected == candidate,
        ClassItem::Range(start, end) => *start <= candidate && candidate <= *end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Tests that compare this layer against the real `git` binary need it to
    /// be there. A missing git is a broken test environment, not a passing
    /// test, so it fails loudly instead of returning early and reporting green.
    fn require_git() {
        let available = Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(
            available,
            "git is not installed; this test compares blob ids against `git hash-object`"
        );
    }

    fn rel(path: &str) -> RelPath {
        RelPath::parse(path).expect("test path should parse")
    }

    struct Fixture {
        dir: tempfile::TempDir,
        root: SessionRoot,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let root = SessionRoot::open(dir.path()).unwrap();
            Fixture { dir, root }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    // ----- disk quota (§13.5) ------------------------------------------

    /// A workspace laid out on disk, with a session worktree and a recorded
    /// reservation — the shape `SessionRoot::open_session` expects.
    struct QuotaFixture {
        _data: tempfile::TempDir,
        workspace_id: String,
        root: SessionRoot,
    }

    impl QuotaFixture {
        /// `headroom` is bytes ON TOP of what the fresh workspace already
        /// occupies, so the runtime database's own size cannot decide the
        /// outcome. `None` records no reservation at all.
        fn new(workspace_id: &str, headroom: Option<u64>) -> QuotaFixture {
            let data = tempfile::tempdir().unwrap();
            crate::paths::set_category_override(
                crate::paths::StorageCategory::Data,
                Some(data.path().to_string_lossy().to_string()),
            );
            paths::create_workspace_layout(workspace_id).unwrap();
            let worktree = paths::session_worktree_dir(workspace_id, "s-1").unwrap();
            std::fs::create_dir_all(&worktree).unwrap();

            // The runtime database is created and CLOSED before the baseline is
            // taken: an open WAL is bytes that disappear at the checkpoint, and
            // a baseline holding them would hand the test invisible headroom.
            let dir = paths::workspace_dir(workspace_id).unwrap();
            let (pool, _) = super::super::workspace_db::open_pool_at(&dir).unwrap();
            drop(pool);
            let baseline = workspace_disk_usage(workspace_id).unwrap();

            let (pool, _) = super::super::workspace_db::open_pool_at(&dir).unwrap();
            let disk_bytes = headroom.map(|extra| (baseline + extra) as i64);
            super::super::workspace_db::set_disk_quota(&pool, disk_bytes).unwrap();
            drop(pool);
            super::super::workspace_db::close(workspace_id);

            let root = SessionRoot::open_session(workspace_id, "s-1").unwrap();
            QuotaFixture {
                _data: data,
                workspace_id: workspace_id.to_string(),
                root,
            }
        }
    }

    impl Drop for QuotaFixture {
        fn drop(&mut self) {
            super::super::workspace_db::close(&self.workspace_id);
            crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
        }
    }

    #[test]
    fn a_write_past_the_workspace_disk_quota_is_refused() {
        let _guard = paths::test_data_dir_guard();
        // 4 MiB of allowance and 6 MiB of content: the second write is the one
        // that does not fit, and it is refused rather than truncated. The sizes
        // are megabytes so that the runtime database's own churn cannot decide
        // the outcome.
        let fx = QuotaFixture::new("ws-quota", Some(4 * 1024 * 1024));
        let chunk = vec![b'x'; 3 * 1024 * 1024];
        assert!(fx.root.limits().max_write_bytes > chunk.len() as u64);

        fx.root
            .write(&rel("first.bin"), &chunk, Precondition::Any)
            .expect("the first write fits inside the quota");

        let refused = fx
            .root
            .write(&rel("second.bin"), &chunk, Precondition::Any)
            .expect_err("a write past the quota was accepted");
        assert!(
            matches!(&refused, FsError::LimitExceeded(reason) if reason.contains("disk quota")),
            "refused for the wrong reason: {refused:?}"
        );
        assert!(
            !paths::session_worktree_dir("ws-quota", "s-1")
                .unwrap()
                .join("second.bin")
                .exists(),
            "the refused write left its file behind"
        );

        // The measurement is of the WHOLE workspace, not of one worktree: bytes
        // written next to the worktree count against the same allowance.
        assert!(
            workspace_disk_usage("ws-quota").unwrap() >= chunk.len() as u64,
            "the write was not counted"
        );

        // Freeing space makes the same write fit again — the quota tracks what
        // is really there rather than a high-water mark.
        fx.root
            .remove(&rel("first.bin"), false, Precondition::Any)
            .unwrap();
        fx.root
            .write(&rel("second.bin"), &chunk, Precondition::Any)
            .expect("space freed by a delete was never given back");
    }

    #[test]
    fn an_edit_that_grows_a_file_past_the_quota_is_refused_but_shrinking_it_is_not() {
        let _guard = paths::test_data_dir_guard();
        let fx = QuotaFixture::new("ws-quota-edit", Some(4 * 1024 * 1024));
        // Every line is unique, so an edit target occurs exactly once and the
        // test measures the quota rather than `edit`'s ambiguity rule.
        let content: String = (0..262_144).map(|i| format!("line {i:06}\n")).collect();
        fx.root
            .write(&rel("a.txt"), content.as_bytes(), Precondition::Any)
            .unwrap();

        let grow = TextEdit {
            old_string: "line 000005\n".into(),
            new_string: "z".repeat(2 * 1024 * 1024),
        };
        let refused = fx
            .root
            .edit(&rel("a.txt"), &[grow], Precondition::Any)
            .expect_err("an edit past the quota was accepted");
        assert!(
            matches!(&refused, FsError::LimitExceeded(reason) if reason.contains("disk quota")),
            "refused for the wrong reason: {refused:?}"
        );
        assert_eq!(
            fx.root.read(&rel("a.txt"), None).unwrap().content,
            content,
            "a refused edit changed the file anyway"
        );

        // Shrinking costs nothing and must stay possible: a full workspace has
        // to be repairable from inside.
        let block: String = (100..100_000).map(|i| format!("line {i:06}\n")).collect();
        let shrink = TextEdit {
            old_string: block,
            new_string: String::new(),
        };
        fx.root
            .edit(&rel("a.txt"), &[shrink], Precondition::Any)
            .expect("an edit that frees bytes was refused");
    }

    #[test]
    fn a_workspace_without_a_recorded_reservation_still_has_a_limit() {
        let _guard = paths::test_data_dir_guard();
        let fx = QuotaFixture::new("ws-quota-default", None);
        assert_eq!(
            WorkspaceQuota::new("ws-quota-default", None).limit_bytes(),
            DEFAULT_WORKSPACE_DISK_BYTES
        );
        // Zero and negative are "never chosen", not "unlimited".
        assert_eq!(
            WorkspaceQuota::new("ws-quota-default", Some(0)).limit_bytes(),
            DEFAULT_WORKSPACE_DISK_BYTES
        );
        assert_eq!(
            WorkspaceQuota::new("ws-quota-default", Some(-1)).limit_bytes(),
            DEFAULT_WORKSPACE_DISK_BYTES
        );
        fx.root
            .write(&rel("ok.txt"), b"content", Precondition::Any)
            .expect("the default allowance refused an ordinary write");
    }

    // ----- containment (§24) -------------------------------------------

    #[test]
    fn every_lexically_hostile_path_is_refused() {
        for hostile in [
            "..",
            "../..",
            "../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "/",
            "sub/../../escape",
            "with\0nul",
            "nul\0",
            "back\\slash",
            "\\\\server\\share\\file",
            "\\\\?\\C:\\Windows",
            "C:/Windows/system32",
            "file.txt:stream",
            ".git",
            ".git/config",
            "src/.git/hooks/pre-commit",
        ] {
            assert!(
                RelPath::parse(hostile).is_err(),
                "accepted hostile path {hostile:?}"
            );
        }
    }

    #[test]
    fn adversarial_git_metadata_is_reachable_through_a_case_variant() {
        // The module header states the lexical rules exist so that a path
        // "written on Linux cannot become [something else] when the same
        // repository is opened on a Windows node", and `GIT_DIR` is documented
        // as "Refused as a path component".
        //
        // The comparison is `segment == ".git"`, byte-exact. macOS (APFS,
        // case-insensitive by default) and Windows (NTFS, case-insensitive)
        // both resolve `.GIT` to `.git`, and neither platform module makes up
        // the difference: `fs/unix.rs::validate_component` is a no-op and
        // `fs/windows.rs::validate_component` only carries device names and the
        // trailing dot/space trick. On Windows `verify_contained` even
        // lowercases both sides before comparing, so the redirect passes its
        // own containment check.
        //
        // The broker already knows this: `git_broker::validate_repo_path` uses
        // `segment.eq_ignore_ascii_case(".git")`. The two guards disagree.
        for hostile in [
            ".GIT", ".Git", ".gIt", ".GIT/config", ".Git/hooks/pre-commit",
            "src/.GIT/config",
        ] {
            assert!(
                RelPath::parse(hostile).is_err(),
                "accepted git metadata path {hostile:?}"
            );
        }

        // Same class, same header: names Windows resolves to a device no matter
        // which directory they sit in, and names whose trailing dot/space
        // Windows strips (so `.git.` becomes `.git`). Written from a Linux node
        // into a repository a Windows node later opens.
        for hostile in ["NUL", "CON", "COM1", "aux.txt", ".git.", ".git "] {
            assert!(
                RelPath::parse(hostile).is_err(),
                "accepted cross-platform hostile name {hostile:?}"
            );
        }
    }

    #[test]
    fn adversarial_a_recursive_remove_walks_straight_through_git_metadata() {
        // Module header: `.git` is "Refused as a path component and skipped
        // during traversal, so neither a direct write nor a recursive delete
        // can reach it."
        //
        // `walk` does skip it. `remove_tree` does not: it lists a directory and
        // unlinks every entry, `.git` included. A vendored checkout, a
        // submodule or a nested repository loses its git metadata to a single
        // `fs_delete` on the directory above it.
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.path().join("vendor/lib/.git")).unwrap();
        std::fs::write(fx.path().join("vendor/lib/.git/HEAD"), b"ref: refs/heads/main").unwrap();
        std::fs::write(fx.path().join("vendor/lib/src.rs"), b"fn main() {}").unwrap();

        let _ = fx.root.remove(&rel("vendor"), true, Precondition::Any);

        assert!(
            fx.path().join("vendor/lib/.git/HEAD").exists(),
            "a recursive remove deleted git metadata the module says it cannot reach"
        );
    }

    #[test]
    fn a_refused_recursive_remove_deletes_nothing_at_all() {
        // The refusal is decided before the first unlink, so a subtree holding
        // a nested repository comes out of a failed delete intact rather than
        // stripped down to the metadata that had to survive.
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.path().join("vendor/lib/.git")).unwrap();
        std::fs::write(fx.path().join("vendor/lib/.git/HEAD"), b"ref: refs/heads/main").unwrap();
        std::fs::write(fx.path().join("vendor/lib/src.rs"), b"fn main() {}").unwrap();
        std::fs::write(fx.path().join("vendor/notes.md"), b"# notes").unwrap();

        let err = fx
            .root
            .remove(&rel("vendor"), true, Precondition::Any)
            .expect_err("a tree holding git metadata was removed");
        assert!(matches!(err, FsError::Denied(_)), "got {err:?}");
        assert!(fx.path().join("vendor/lib/src.rs").exists());
        assert!(fx.path().join("vendor/notes.md").exists());
    }

    #[test]
    fn name_rules_hold_on_every_platform_not_only_on_windows() {
        // One guard for every node: these names are refused where the file is
        // created, not discovered when the repository is opened on Windows.
        for name in [
            "CON",
            "con",
            "NUL",
            "nul.txt",
            "COM1",
            "com9.log",
            "LPT3",
            "AUX",
            "PRN",
            "CONIN$",
            "trailing.",
            "trailing ",
            "quote\"name",
            "pipe|name",
            "star*name",
            "question?name",
            "less<name",
            "greater>name",
            "control\u{1}name",
        ] {
            assert!(validate_component(name).is_err(), "accepted {name:?}");
            assert!(
                RelPath::parse(&format!("src/{name}")).is_err(),
                "accepted {name:?} as a path component"
            );
        }
    }

    #[test]
    fn ordinary_names_still_pass() {
        for name in ["src", "main.rs", "Cargo.toml", "conflict.rs", "console.js"] {
            assert!(validate_component(name).is_ok(), "refused {name:?}");
        }
    }

    #[test]
    fn ordinary_paths_normalize_instead_of_being_refused() {
        assert_eq!(rel("src/main.rs").as_str(), "src/main.rs");
        assert_eq!(rel("./src/./main.rs").as_str(), "src/main.rs");
        assert_eq!(rel("src//main.rs").as_str(), "src/main.rs");
        assert_eq!(rel("").as_str(), "");
        assert!(rel("").is_root());
        assert_eq!(rel(".gitignore").as_str(), ".gitignore");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_pointing_outside_the_root_is_never_read_through() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"classified").unwrap();
        std::os::unix::fs::symlink(outside.path(), fixture.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            fixture.path().join("direct.txt"),
        )
        .unwrap();

        assert!(fixture.root.read(&rel("escape/secret.txt"), None).is_err());
        assert!(fixture.root.read(&rel("direct.txt"), None).is_err());
        assert!(fixture
            .root
            .write(&rel("direct.txt"), b"x", Precondition::Any)
            .is_err());
        assert!(fixture.root.list(&rel("escape"), 1).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_directory_in_the_middle_of_a_path_is_refused() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.path().join("real/deep")).unwrap();
        std::fs::write(fixture.path().join("real/deep/file.txt"), b"inside").unwrap();
        std::os::unix::fs::symlink("real", fixture.path().join("alias")).unwrap();

        assert_eq!(
            fixture
                .root
                .read(&rel("real/deep/file.txt"), None)
                .unwrap()
                .content,
            "inside"
        );
        assert!(fixture
            .root
            .read(&rel("alias/deep/file.txt"), None)
            .is_err());
    }

    #[test]
    #[cfg(unix)]
    fn swapping_a_segment_for_a_symlink_after_the_root_is_open_never_escapes() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.path().join("work/nested")).unwrap();
        std::fs::write(fixture.path().join("work/nested/target.txt"), b"mine").unwrap();

        // The root handle is already open and a first operation has verified
        // the path; now the attacker replaces a segment.
        assert_eq!(
            fixture
                .root
                .read(&rel("work/nested/target.txt"), None)
                .unwrap()
                .content,
            "mine"
        );

        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("nested")).unwrap();
        std::fs::write(outside.path().join("nested/target.txt"), b"attacker").unwrap();
        std::fs::remove_dir_all(fixture.path().join("work")).unwrap();
        std::os::unix::fs::symlink(outside.path(), fixture.path().join("work")).unwrap();

        // Reading, writing and deleting all refuse: the swapped segment is a
        // symlink and no operation follows one.
        assert!(fixture
            .root
            .read(&rel("work/nested/target.txt"), None)
            .is_err());
        assert!(fixture
            .root
            .write(&rel("work/nested/target.txt"), b"x", Precondition::Any)
            .is_err());
        assert!(fixture
            .root
            .remove(&rel("work/nested/target.txt"), false, Precondition::Any)
            .is_err());

        // And the file outside is untouched.
        assert_eq!(
            std::fs::read(outside.path().join("nested/target.txt")).unwrap(),
            b"attacker"
        );
    }

    #[test]
    fn the_session_root_itself_cannot_be_removed() {
        let fixture = Fixture::new();
        assert!(fixture
            .root
            .remove(&RelPath::root(), true, Precondition::Any)
            .is_err());
    }

    // ----- CAS (§13.2) --------------------------------------------------

    #[test]
    fn a_first_and_a_second_edit_of_the_same_file_both_pass() {
        let fixture = Fixture::new();
        let path = rel("notes.txt");

        let base = fixture
            .root
            .write(&path, b"alpha\n", Precondition::Absent)
            .unwrap();
        assert!(base.created);

        let first = fixture
            .root
            .edit(
                &path,
                &[TextEdit {
                    old_string: "alpha".into(),
                    new_string: "beta".into(),
                }],
                Precondition::BlobIs(base.blob_sha.clone()),
            )
            .unwrap();
        assert!(!first.created);

        // The second edit expects the CURRENT blob, not the base one — the
        // rule the 1.2 revision got wrong (§13.2).
        let second = fixture
            .root
            .edit(
                &path,
                &[TextEdit {
                    old_string: "beta".into(),
                    new_string: "gamma".into(),
                }],
                Precondition::BlobIs(first.blob_sha.clone()),
            )
            .unwrap();
        assert_eq!(fixture.root.read(&path, None).unwrap().content, "gamma\n");

        // Replaying the first precondition now loses.
        let err = fixture
            .root
            .edit(
                &path,
                &[TextEdit {
                    old_string: "gamma".into(),
                    new_string: "delta".into(),
                }],
                Precondition::BlobIs(base.blob_sha),
            )
            .expect_err("a stale precondition was accepted");
        match err {
            FsError::Conflict { actual, .. } => assert_eq!(actual, Some(second.blob_sha)),
            other => panic!("expected a conflict, got {other}"),
        }
    }

    #[test]
    fn creating_a_file_that_already_exists_conflicts_instead_of_overwriting() {
        let fixture = Fixture::new();
        let path = rel("once.txt");
        fixture
            .root
            .write(&path, b"first", Precondition::Absent)
            .unwrap();
        assert!(matches!(
            fixture.root.write(&path, b"second", Precondition::Absent),
            Err(FsError::Conflict { .. })
        ));
        assert_eq!(fixture.root.read(&path, None).unwrap().content, "first");
    }

    #[test]
    fn delete_and_rename_honour_their_precondition() {
        let fixture = Fixture::new();
        let source = rel("a.txt");
        let written = fixture
            .root
            .write(&source, b"payload", Precondition::Absent)
            .unwrap();

        assert!(matches!(
            fixture
                .root
                .remove(&source, false, Precondition::BlobIs("0".repeat(40))),
            Err(FsError::Conflict { .. })
        ));
        assert!(matches!(
            fixture
                .root
                .rename(&source, &rel("b.txt"), Precondition::BlobIs("0".repeat(40))),
            Err(FsError::Conflict { .. })
        ));

        fixture
            .root
            .rename(
                &source,
                &rel("b.txt"),
                Precondition::BlobIs(written.blob_sha.clone()),
            )
            .unwrap();
        assert!(matches!(
            fixture.root.read(&source, None),
            Err(FsError::NotFound)
        ));
        assert_eq!(
            fixture.root.read(&rel("b.txt"), None).unwrap().content,
            "payload"
        );

        fixture
            .root
            .remove(&rel("b.txt"), false, Precondition::BlobIs(written.blob_sha))
            .unwrap();
        assert!(matches!(
            fixture.root.read(&rel("b.txt"), None),
            Err(FsError::NotFound)
        ));
    }

    #[test]
    fn a_rename_onto_an_existing_name_is_refused() {
        let fixture = Fixture::new();
        fixture
            .root
            .write(&rel("from.txt"), b"one", Precondition::Absent)
            .unwrap();
        fixture
            .root
            .write(&rel("onto.txt"), b"two", Precondition::Absent)
            .unwrap();
        assert!(matches!(
            fixture
                .root
                .rename(&rel("from.txt"), &rel("onto.txt"), Precondition::Any),
            Err(FsError::AlreadyExists)
        ));
        assert_eq!(
            fixture.root.read(&rel("onto.txt"), None).unwrap().content,
            "two"
        );
    }

    #[test]
    fn deleting_something_already_absent_succeeds_only_under_absent() {
        let fixture = Fixture::new();
        fixture
            .root
            .remove(&rel("ghost.txt"), false, Precondition::Absent)
            .unwrap();
        assert!(matches!(
            fixture
                .root
                .remove(&rel("ghost.txt"), false, Precondition::Any),
            Err(FsError::NotFound)
        ));
    }

    // ----- writes -------------------------------------------------------

    #[test]
    fn no_write_path_leaves_a_temporary_file_behind() {
        let fixture = Fixture::new();
        let limits = FsLimits {
            max_write_bytes: 32,
            ..FsLimits::default()
        };
        let root = SessionRoot::open_with_limits(fixture.path(), limits).unwrap();
        root.mkdir(&rel("pkg")).unwrap();
        root.write(&rel("pkg/kept.txt"), b"kept", Precondition::Absent)
            .unwrap();

        // A successful replace: the temporary file must have been renamed away,
        // not left next to the target.
        root.write(&rel("pkg/kept.txt"), b"replaced", Precondition::Any)
            .unwrap();
        // A conflicting precondition fails before anything is created.
        assert!(root
            .write(
                &rel("pkg/kept.txt"),
                b"never",
                Precondition::BlobIs("0".repeat(40))
            )
            .is_err());
        // So does content over the write limit.
        assert!(root
            .write(&rel("pkg/kept.txt"), &vec![b'x'; 64], Precondition::Any)
            .is_err());
        // And a create that loses its race against an existing name.
        assert!(root
            .write(&rel("pkg/kept.txt"), b"new", Precondition::Absent)
            .is_err());

        let leftovers: Vec<String> = root
            .list(&rel("pkg"), 1)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(leftovers, vec!["pkg/kept.txt".to_string()]);
        assert_eq!(
            root.read(&rel("pkg/kept.txt"), None).unwrap().content,
            "replaced"
        );
    }

    #[test]
    fn an_ambiguous_edit_is_an_error_rather_than_a_choice() {
        let fixture = Fixture::new();
        let path = rel("dup.rs");
        fixture
            .root
            .write(&path, b"let x = 1;\nlet x = 1;\n", Precondition::Absent)
            .unwrap();

        let err = fixture
            .root
            .edit(
                &path,
                &[TextEdit {
                    old_string: "let x = 1;".into(),
                    new_string: "let x = 2;".into(),
                }],
                Precondition::Any,
            )
            .expect_err("an ambiguous edit was applied");
        match err {
            FsError::AmbiguousEdit { matches, .. } => assert_eq!(matches, 2),
            other => panic!("expected an ambiguity error, got {other}"),
        }
        // The file is untouched.
        assert_eq!(
            fixture.root.read(&path, None).unwrap().content,
            "let x = 1;\nlet x = 1;\n"
        );

        // Extending the target until it is unique makes the same edit pass.
        fixture
            .root
            .edit(
                &path,
                &[TextEdit {
                    old_string: "let x = 1;\nlet x = 1;".into(),
                    new_string: "let x = 2;\nlet x = 1;".into(),
                }],
                Precondition::Any,
            )
            .unwrap();
        assert_eq!(
            fixture.root.read(&path, None).unwrap().content,
            "let x = 2;\nlet x = 1;\n"
        );
    }

    #[test]
    fn an_edit_whose_target_is_missing_reports_it_instead_of_writing() {
        let fixture = Fixture::new();
        let path = rel("plain.txt");
        fixture
            .root
            .write(&path, b"hello", Precondition::Absent)
            .unwrap();
        assert!(matches!(
            fixture.root.edit(
                &path,
                &[TextEdit {
                    old_string: "goodbye".into(),
                    new_string: "hi".into(),
                }],
                Precondition::Any,
            ),
            Err(FsError::EditNotFound { .. })
        ));
        assert_eq!(fixture.root.read(&path, None).unwrap().content, "hello");
    }

    #[test]
    fn mkdir_creates_missing_levels_and_remove_takes_the_tree_back_out() {
        let fixture = Fixture::new();
        fixture.root.mkdir(&rel("a/b/c")).unwrap();
        fixture
            .root
            .write(&rel("a/b/c/leaf.txt"), b"leaf", Precondition::Absent)
            .unwrap();

        // Non-recursive removal of a populated directory fails.
        assert!(fixture
            .root
            .remove(&rel("a"), false, Precondition::Any)
            .is_err());
        fixture
            .root
            .remove(&rel("a"), true, Precondition::Any)
            .unwrap();
        assert!(fixture.root.list(&RelPath::root(), 3).unwrap().is_empty());
    }

    // ----- reads --------------------------------------------------------

    #[test]
    fn a_line_range_slices_without_losing_the_whole_file_identity() {
        let fixture = Fixture::new();
        let path = rel("lines.txt");
        let outcome = fixture
            .root
            .write(&path, b"one\ntwo\nthree\nfour\n", Precondition::Absent)
            .unwrap();

        let whole = fixture.root.read(&path, None).unwrap();
        assert_eq!(whole.total_lines, 4);
        assert!(!whole.truncated);
        assert_eq!(whole.blob_sha, outcome.blob_sha);

        let slice = fixture
            .root
            .read(&path, Some(LineRange { start: 2, count: 2 }))
            .unwrap();
        assert_eq!(slice.content, "two\nthree\n");
        assert!(slice.truncated);
        assert_eq!(slice.total_lines, 4);
        assert_eq!(slice.blob_sha, outcome.blob_sha);
    }

    #[test]
    fn list_is_bounded_by_depth_and_sorted() {
        let fixture = Fixture::new();
        fixture.root.mkdir(&rel("src/inner")).unwrap();
        fixture
            .root
            .write(&rel("src/a.rs"), b"a", Precondition::Absent)
            .unwrap();
        fixture
            .root
            .write(&rel("src/inner/b.rs"), b"b", Precondition::Absent)
            .unwrap();

        let shallow: Vec<String> = fixture
            .root
            .list(&rel("src"), 1)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(
            shallow,
            vec!["src/a.rs".to_string(), "src/inner".to_string()]
        );

        let deep: Vec<String> = fixture
            .root
            .list(&rel("src"), 2)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(
            deep,
            vec![
                "src/a.rs".to_string(),
                "src/inner".to_string(),
                "src/inner/b.rs".to_string()
            ]
        );
    }

    #[test]
    fn git_metadata_is_invisible_to_traversal_and_to_search() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.path().join(".git")).unwrap();
        std::fs::write(fixture.path().join(".git/config"), b"needle inside").unwrap();
        fixture
            .root
            .write(&rel("visible.txt"), b"needle inside", Precondition::Absent)
            .unwrap();

        let listed: Vec<String> = fixture
            .root
            .list(&RelPath::root(), 4)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(listed, vec!["visible.txt".to_string()]);

        let hits = fixture
            .root
            .grep(&GrepQuery {
                pattern: "needle".into(),
                is_regex: false,
                glob: None,
                max_results: 50,
                max_bytes_per_file: 1024,
            })
            .unwrap();
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].path, "visible.txt");
    }

    // ----- glob and grep ------------------------------------------------

    #[test]
    fn glob_matches_across_and_within_components() {
        let fixture = Fixture::new();
        fixture.root.mkdir(&rel("src/deep/deeper")).unwrap();
        for path in [
            "top.rs",
            "src/one.rs",
            "src/two.txt",
            "src/deep/three.rs",
            "src/deep/deeper/four.rs",
        ] {
            fixture
                .root
                .write(&rel(path), b"x", Precondition::Absent)
                .unwrap();
        }

        let found: Vec<String> = fixture
            .root
            .glob("**/*.rs", 100)
            .unwrap()
            .into_iter()
            .map(|path| path.as_str().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "src/deep/deeper/four.rs".to_string(),
                "src/deep/three.rs".to_string(),
                "src/one.rs".to_string(),
                "top.rs".to_string(),
            ]
        );

        let shallow: Vec<String> = fixture
            .root
            .glob("src/*.rs", 100)
            .unwrap()
            .into_iter()
            .map(|path| path.as_str().to_string())
            .collect();
        assert_eq!(shallow, vec!["src/one.rs".to_string()]);

        let classed: Vec<String> = fixture
            .root
            .glob("src/[ot]??.rs", 100)
            .unwrap()
            .into_iter()
            .map(|path| path.as_str().to_string())
            .collect();
        assert_eq!(classed, vec!["src/one.rs".to_string()]);

        assert!(Glob::parse("src/[abc").is_err());
        assert!(Glob::parse("../escape/*").is_err());
    }

    #[test]
    fn glob_stops_at_its_limit() {
        let fixture = Fixture::new();
        for index in 0..10 {
            fixture
                .root
                .write(&rel(&format!("f{index}.txt")), b"x", Precondition::Absent)
                .unwrap();
        }
        assert_eq!(fixture.root.glob("*.txt", 3).unwrap().len(), 3);
    }

    #[test]
    fn grep_reports_positions_respects_its_limit_and_skips_binaries() {
        let fixture = Fixture::new();
        fixture.root.mkdir(&rel("src")).unwrap();
        fixture
            .root
            .write(
                &rel("src/a.rs"),
                b"fn alpha() {}\nfn beta() {}\nfn alpha_two() {}\n",
                Precondition::Absent,
            )
            .unwrap();
        fixture
            .root
            .write(&rel("src/b.txt"), b"alpha in text\n", Precondition::Absent)
            .unwrap();
        fixture
            .root
            .write(
                &rel("src/blob.bin"),
                b"alpha\0binary payload",
                Precondition::Absent,
            )
            .unwrap();

        let all = fixture
            .root
            .grep(&GrepQuery {
                pattern: "alpha".into(),
                is_regex: false,
                glob: None,
                max_results: 50,
                max_bytes_per_file: 4096,
            })
            .unwrap();
        let paths: Vec<&str> = all.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/a.rs", "src/b.txt"]);
        assert_eq!(all.hits[0].line, 1);
        assert_eq!(all.hits[0].column, 4);
        assert!(!all.truncated);

        let limited = fixture
            .root
            .grep(&GrepQuery {
                pattern: "alpha".into(),
                is_regex: false,
                glob: None,
                max_results: 2,
                max_bytes_per_file: 4096,
            })
            .unwrap();
        assert_eq!(limited.hits.len(), 2);
        assert!(limited.truncated);

        let filtered = fixture
            .root
            .grep(&GrepQuery {
                pattern: r"^fn\s+alpha".into(),
                is_regex: true,
                glob: Some("**/*.rs".into()),
                max_results: 50,
                max_bytes_per_file: 4096,
            })
            .unwrap();
        assert_eq!(filtered.hits.len(), 2);
        assert!(filtered.hits.iter().all(|hit| hit.path == "src/a.rs"));
    }

    #[test]
    fn grep_refuses_a_regex_that_would_not_fit_its_size_limit() {
        let fixture = Fixture::new();
        let limits = FsLimits {
            regex_size_limit: 64,
            ..FsLimits::default()
        };
        let tight = SessionRoot::open_with_limits(fixture.path(), limits).unwrap();
        assert!(matches!(
            tight.grep(&GrepQuery {
                pattern: r"(?i)[a-z0-9_]{1,64}(alpha|beta|gamma|delta|epsilon)[a-z0-9_]{1,64}"
                    .into(),
                is_regex: true,
                glob: None,
                max_results: 10,
                max_bytes_per_file: 4096,
            }),
            Err(FsError::InvalidRequest(_))
        ));
    }

    // ----- blob identity -------------------------------------------------

    #[test]
    fn the_blob_id_matches_the_known_git_vectors() {
        // `git hash-object` of an empty file and of "hello\n" are the two
        // values every git implementation agrees on.
        assert_eq!(blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn the_blob_id_matches_git_hash_object_on_this_machine() {
        require_git();
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("empty", &b""[..]),
            ("text", &b"first line\nsecond line\n"[..]),
            ("no-trailing-newline", &b"tail"[..]),
            ("binary", &b"\x00\x01\x02\xff\xfe"[..]),
        ] {
            let file = dir.path().join(name);
            std::fs::write(&file, content).unwrap();
            let output = Command::new("git")
                .arg("hash-object")
                .arg("--")
                .arg(&file)
                .output()
                .unwrap();
            assert!(output.status.success(), "git hash-object failed for {name}");
            let expected = String::from_utf8(output.stdout).unwrap().trim().to_string();
            assert_eq!(blob_sha(content), expected, "blob id drifted for {name}");
        }
    }

    #[test]
    fn a_write_reports_the_same_blob_id_a_later_read_reports() {
        let fixture = Fixture::new();
        let path = rel("round.txt");
        let written = fixture
            .root
            .write(&path, b"round trip\n", Precondition::Absent)
            .unwrap();
        assert_eq!(
            fixture.root.read(&path, None).unwrap().blob_sha,
            written.blob_sha
        );
        assert_eq!(
            fixture.root.stat(&path).unwrap().blob_sha,
            Some(written.blob_sha)
        );
    }

    // ----- limits --------------------------------------------------------

    #[test]
    fn a_file_over_the_read_limit_is_refused_rather_than_streamed_into_context() {
        let fixture = Fixture::new();
        let limits = FsLimits {
            max_read_bytes: 16,
            ..FsLimits::default()
        };
        let tight = SessionRoot::open_with_limits(fixture.path(), limits).unwrap();
        fixture
            .root
            .write(&rel("big.txt"), &vec![b'a'; 64], Precondition::Absent)
            .unwrap();
        assert!(matches!(
            tight.read(&rel("big.txt"), None),
            Err(FsError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_listing_over_the_entry_limit_is_refused_rather_than_silently_cut() {
        let fixture = Fixture::new();
        for index in 0..8 {
            fixture
                .root
                .write(&rel(&format!("f{index}.txt")), b"x", Precondition::Absent)
                .unwrap();
        }
        let limits = FsLimits {
            max_dir_entries: 4,
            ..FsLimits::default()
        };
        let tight = SessionRoot::open_with_limits(fixture.path(), limits).unwrap();
        assert!(matches!(
            tight.list(&RelPath::root(), 1),
            Err(FsError::LimitExceeded(_))
        ));
    }
}
