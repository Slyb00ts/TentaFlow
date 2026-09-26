//! How many bytes one top-level folder of an Elastic Array holds on one of its
//! branches (n11 "Foldery", column "Użycie").
//!
//! A folder of an array is one directory name under the union, and mergerfs
//! spreads it over every data disk and the cache: `media/filmy` is
//! `<branch>/filmy` on each branch that has files of it. Its usage is the sum
//! over those branches, which only a walk can give — XFS and ext4 keep no
//! per-directory total unless project quotas were set up when the branch was
//! mounted, and the branches are mounted without them.
//!
//! WHAT IS COUNTED is ALLOCATED space (`st_blocks * 512`), as `du` counts it,
//! directories included, and not the apparent size: the column sits next to
//! the branch capacity bars, which are allocation too, and a sparse image
//! would otherwise claim terabytes it never took. A file with several links
//! counts once per folder walk; an entry on another filesystem (something
//! mounted inside a branch) is not the branch's and is not counted, as
//! `du -x` does.
//!
//! THE WALK IS BOUNDED. It holds a budget of entries and a deadline shared by
//! every folder of one measurement, and a folder the budget runs out in is
//! reported OVER BUDGET, never as the part that was counted: a partial sum
//! reads exactly like a smaller folder. An entry that cannot be read (an I/O
//! error on a failing disk) makes its folder UNREADABLE for the same reason.
//! An entry removed while the walk runs is simply gone — the measurement is a
//! point-in-time reading of a live tree, as `du`'s is.
//!
//! Every name is charged as its directory is READ, so one huge directory
//! cannot be listed whole before the budget sees it, and the deadline is
//! read with the budget. What the deadline cannot end is one system call
//! that never returns (an `fstatat` on a dying disk): that is bounded only
//! by whoever runs the helper giving up on it.
//!
//! Descent is FD-relative and never follows a symlink, so a user who swaps a
//! directory for a link while the walk runs cannot make root count, or even
//! stat, anything outside the branch.
use std::collections::HashSet;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Instant;

/// Why a folder has no figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gap {
    /// The entry budget or the deadline ran out inside this folder.
    OverBudget,
    /// An entry of the folder could not be read.
    Unreadable,
}

/// What one measurement may still spend. Shared by every folder and branch
/// of it, so the whole command is bounded, not each folder.
pub(crate) struct Budget {
    entries_left: u64,
    deadline: Instant,
    /// Entries seen since the clock was last read: `Instant::now` per entry
    /// would cost more than the `fstatat` it guards.
    since_clock: u32,
}

/// How many entries pass between two reads of the clock.
const CLOCK_EVERY: u32 = 1024;

/// Deeper than this is not a folder tree anybody keeps on purpose, and each
/// level holds one open descriptor.
const MAX_DEPTH: usize = 256;

impl Budget {
    pub(crate) fn new(entries: u64, deadline: Instant) -> Self {
        Self { entries_left: entries, deadline, since_clock: 0 }
    }

    /// Takes one entry. `false` once the entries or the time are spent.
    fn take(&mut self) -> bool {
        if self.entries_left == 0 {
            return false;
        }
        self.entries_left -= 1;
        self.since_clock += 1;
        if self.since_clock >= CLOCK_EVERY {
            self.since_clock = 0;
            if Instant::now() >= self.deadline {
                self.entries_left = 0;
                return false;
            }
        }
        true
    }

    /// Whether anything is left before the first entry of a folder.
    pub(crate) fn spent(&self) -> bool {
        self.entries_left == 0 || Instant::now() >= self.deadline
    }
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn stat_fd(fd: i32) -> Result<libc::stat, std::io::Error> {
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(last_error());
    }
    Ok(stat)
}

fn stat_at(fd: i32, name: &CString) -> Result<libc::stat, std::io::Error> {
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(last_error());
    }
    Ok(stat)
}

/// Opens a directory entry of `fd` as a directory, refusing a symlink.
fn open_directory_at(fd: i32, name: &CString) -> Result<OwnedFd, std::io::Error> {
    let child = unsafe {
        libc::openat(
            fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if child < 0 {
        return Err(last_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(child) })
}

/// Why a directory could not be listed.
enum ListGap {
    Io,
    /// The budget ran out while the names were read.
    OverBudget,
}

/// Every name in the directory `fd`, `.` and `..` left out.
///
/// Each name is charged to `budget` AS IT IS READ (critic wave 7, MINOR 7):
/// one huge directory would otherwise be read whole — unbounded in memory
/// and time — before the walk charged a single entry. A name charged here is
/// not charged again when the walk visits it. The deadline is read with the
/// budget, so a directory the size of the budget cannot outlast it either.
fn list(fd: i32, budget: &mut Budget) -> Result<Vec<CString>, ListGap> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(ListGap::Io);
    }
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(ListGap::Io);
    }
    unsafe { *libc::__errno_location() = 0 };
    let mut names = Vec::new();
    let mut spent = false;
    loop {
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." {
            if !budget.take() {
                spent = true;
                break;
            }
            names.push(name.to_owned());
        }
    }
    let read_error = unsafe { *libc::__errno_location() };
    unsafe { libc::closedir(directory) };
    if spent {
        return Err(ListGap::OverBudget);
    }
    if read_error != 0 {
        return Err(ListGap::Io);
    }
    Ok(names)
}

impl From<ListGap> for Gap {
    fn from(gap: ListGap) -> Self {
        match gap {
            ListGap::Io => Gap::Unreadable,
            ListGap::OverBudget => Gap::OverBudget,
        }
    }
}

/// Whether the directory just opened is the entry `stat` described: the
/// same device and inode. A directory swapped (or a filesystem mounted over
/// it) between the `fstatat` and the `openat` is not the one that was judged
/// to be on this branch.
fn same_entry(opened: &OwnedFd, stat: &libc::stat) -> bool {
    stat_fd(opened.as_raw_fd()).is_ok_and(|now| now.st_dev == stat.st_dev && now.st_ino == stat.st_ino)
}

fn allocated(stat: &libc::stat) -> u64 {
    u64::try_from(stat.st_blocks).unwrap_or(0).saturating_mul(512)
}

/// The device a branch directory lives on. The caller compares it with the
/// device of the branch's parent: equal means the branch is NOT mounted and
/// its directory is an empty one on the root filesystem, whose zero bytes
/// must not be read as "the folder is empty on this disk".
pub(crate) fn device_of(path: &Path) -> Result<u64, String> {
    let path = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
        .map_err(|_| "ścieżka zawiera NUL".to_string())?;
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(path.as_ptr(), &mut stat) } != 0 {
        return Err(last_error().to_string());
    }
    Ok(stat.st_dev as u64)
}

/// Allocated bytes of `<branch>/<folder>`, or why there is no figure.
///
/// A folder the branch does not hold is 0 on it — mergerfs creates a folder
/// on a disk only when a file of it lands there, so that is the normal case
/// for a new disk, not a gap.
pub(crate) fn folder_bytes(branch: &Path, folder: &str, budget: &mut Budget) -> Result<u64, Gap> {
    let root_path = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(branch.as_os_str()))
        .map_err(|_| Gap::Unreadable)?;
    let root = unsafe {
        libc::open(root_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
    };
    if root < 0 {
        return Err(Gap::Unreadable);
    }
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    let device = stat_fd(root.as_raw_fd()).map_err(|_| Gap::Unreadable)?.st_dev;
    let name = CString::new(folder).map_err(|_| Gap::Unreadable)?;
    if !budget.take() {
        return Err(Gap::OverBudget);
    }
    let top = match stat_at(root.as_raw_fd(), &name) {
        Ok(stat) => stat,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(0),
        Err(_) => return Err(Gap::Unreadable),
    };
    if top.st_dev != device {
        return Ok(0);
    }
    let mut total = allocated(&top);
    if top.st_mode & libc::S_IFMT != libc::S_IFDIR {
        // A plain file under a folder's name on this branch: it is what the
        // name holds here.
        return Ok(total);
    }
    let opened = match open_directory_at(root.as_raw_fd(), &name) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(0),
        Err(_) => return Err(Gap::Unreadable),
    };
    if !same_entry(&opened, &top) {
        // Swapped between the two calls: what was measured is gone, and
        // what is there now was never judged to be on this branch.
        return Err(Gap::Unreadable);
    }
    let names = list(opened.as_raw_fd(), budget)?;
    let mut linked: HashSet<(u64, u64)> = HashSet::new();
    // One frame per open directory: its descriptor and the names still to visit.
    let mut stack: Vec<(OwnedFd, Vec<CString>)> = vec![(opened, names)];
    while let Some((fd, names)) = stack.last_mut() {
        let Some(name) = names.pop() else {
            stack.pop();
            continue;
        };
        let dir = fd.as_raw_fd();
        // The name was charged when its directory was listed (`list`).
        let stat = match stat_at(dir, &name) {
            Ok(stat) => stat,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(_) => return Err(Gap::Unreadable),
        };
        if stat.st_dev != device {
            continue;
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            total = total.saturating_add(allocated(&stat));
            if stack.len() >= MAX_DEPTH {
                return Err(Gap::OverBudget);
            }
            let child = match open_directory_at(dir, &name) {
                Ok(child) => child,
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(_) => return Err(Gap::Unreadable),
            };
            if !same_entry(&child, &stat) {
                // Swapped for another directory, or mounted over, after it
                // was judged: never descended into (defence in depth; the
                // branches are invisible from the host namespace).
                continue;
            }
            let names = list(child.as_raw_fd(), budget)?;
            stack.push((child, names));
            continue;
        }
        if stat.st_nlink > 1 && !linked.insert((stat.st_dev as u64, stat.st_ino as u64)) {
            continue;
        }
        total = total.saturating_add(allocated(&stat));
    }
    Ok(total)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tentanas-folder-usage-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(path: &Path, bytes: usize) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&vec![7u8; bytes]).unwrap();
        file.sync_all().unwrap();
    }

    fn blocks(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    fn roomy() -> Budget {
        Budget::new(1_000_000, Instant::now() + Duration::from_secs(60))
    }

    /// The figure is `du`'s: allocation of every file AND directory under the
    /// folder, a second link of a file counted once, and a symlink counted as
    /// the link it is — its target (here a big file outside the folder) is
    /// never followed.
    #[test]
    fn a_folder_counts_allocation_once_per_inode_and_never_follows_a_link() {
        let branch = scratch("sum");
        let outside = scratch("outside");
        write(&outside.join("big"), 1 << 20);
        let folder = branch.join("filmy");
        fs::create_dir_all(folder.join("a/b")).unwrap();
        write(&folder.join("one"), 10_000);
        write(&folder.join("a/b/two"), 70_000);
        fs::hard_link(folder.join("one"), folder.join("a/one-again")).unwrap();
        symlink(outside.join("big"), folder.join("a/link")).unwrap();
        let expected = blocks(&folder)
            + blocks(&folder.join("a"))
            + blocks(&folder.join("a/b"))
            + blocks(&folder.join("one"))
            + blocks(&folder.join("a/b/two"))
            + blocks(&folder.join("a/link"));
        assert_eq!(folder_bytes(&branch, "filmy", &mut roomy()), Ok(expected));
        assert!(expected < 1 << 20, "the link target must not be in the figure");
    }

    /// mergerfs puts a folder on a disk only when a file of it lands there.
    #[test]
    fn a_folder_the_branch_does_not_hold_is_zero_on_it() {
        let branch = scratch("absent");
        assert_eq!(folder_bytes(&branch, "muzyka", &mut roomy()), Ok(0));
    }

    /// Running out inside a folder is OVER BUDGET — the part counted so far is
    /// never returned as the folder's size — and the budget stays spent for
    /// every folder after it.
    #[test]
    fn a_folder_the_budget_runs_out_in_has_no_figure() {
        let branch = scratch("budget");
        let folder = branch.join("foto");
        fs::create_dir_all(&folder).unwrap();
        for i in 0..20 {
            write(&folder.join(format!("f{i}")), 100);
        }
        let mut budget = Budget::new(10, Instant::now() + Duration::from_secs(60));
        assert_eq!(folder_bytes(&branch, "foto", &mut budget), Err(Gap::OverBudget));
        assert!(budget.spent());
        // The same tree with room for every entry measures.
        assert!(folder_bytes(&branch, "foto", &mut Budget::new(22, Instant::now() + Duration::from_secs(60))).is_ok());
    }

    /// The deadline is a budget too, read every `CLOCK_EVERY` entries.
    #[test]
    fn a_passed_deadline_stops_the_walk() {
        let branch = scratch("deadline");
        let folder = branch.join("foto");
        fs::create_dir_all(&folder).unwrap();
        for i in 0..(CLOCK_EVERY + 10) {
            fs::File::create(folder.join(format!("f{i}"))).unwrap();
        }
        let mut budget = Budget::new(u64::MAX, Instant::now());
        assert_eq!(folder_bytes(&branch, "foto", &mut budget), Err(Gap::OverBudget));
    }

    /// Critic wave 7, MINOR 7: a directory is charged while it is READ, so a
    /// huge one stops at the budget instead of being listed whole first, and
    /// each name is charged once — listed, not again when visited.
    #[test]
    fn a_huge_directory_is_charged_while_it_is_read() {
        let branch = scratch("huge");
        let folder = branch.join("foto");
        fs::create_dir_all(&folder).unwrap();
        for i in 0..50 {
            fs::File::create(folder.join(format!("f{i}"))).unwrap();
        }
        let open = |path: &Path| {
            let path = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str())).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
            assert!(fd >= 0);
            unsafe { OwnedFd::from_raw_fd(fd) }
        };
        let dir = open(&folder);
        let mut tight = Budget::new(5, Instant::now() + Duration::from_secs(60));
        assert!(matches!(list(dir.as_raw_fd(), &mut tight), Err(ListGap::OverBudget)));
        assert!(tight.spent());
        // A fresh descriptor: a listed one has its offset at the end.
        let dir = open(&folder);
        let mut exact = Budget::new(50, Instant::now() + Duration::from_secs(60));
        assert_eq!(list(dir.as_raw_fd(), &mut exact).ok().map(|names| names.len()), Some(50));
        // The folder's own entry and its 50 names: nothing is charged twice.
        assert!(folder_bytes(&branch, "foto", &mut Budget::new(51, Instant::now() + Duration::from_secs(60))).is_ok());
        assert_eq!(
            folder_bytes(&branch, "foto", &mut Budget::new(50, Instant::now() + Duration::from_secs(60))),
            Err(Gap::OverBudget)
        );
    }

    /// Critic wave 7, MINOR 7: a descriptor is used only if it is the entry
    /// that was judged — same device and inode as its `fstatat`.
    #[test]
    fn a_directory_is_entered_only_if_it_is_the_one_judged() {
        let branch = scratch("same");
        fs::create_dir_all(branch.join("a")).unwrap();
        fs::create_dir_all(branch.join("b")).unwrap();
        let root = {
            let path = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(branch.as_os_str())).unwrap();
            unsafe { OwnedFd::from_raw_fd(libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)) }
        };
        let a = CString::new("a").unwrap();
        let b = CString::new("b").unwrap();
        let judged = stat_at(root.as_raw_fd(), &a).unwrap();
        assert!(same_entry(&open_directory_at(root.as_raw_fd(), &a).unwrap(), &judged));
        assert!(!same_entry(&open_directory_at(root.as_raw_fd(), &b).unwrap(), &judged), "a swapped directory is not entered");
    }

    /// A directory root cannot list is a folder with no figure, not a smaller one.
    #[test]
    fn an_unlistable_directory_makes_the_folder_unreadable() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root lists a 0o000 directory; the refusal needs a user.
        }
        use std::os::unix::fs::PermissionsExt;
        let branch = scratch("unreadable");
        let folder = branch.join("dom");
        fs::create_dir_all(folder.join("closed")).unwrap();
        write(&folder.join("closed/x"), 100);
        fs::set_permissions(folder.join("closed"), fs::Permissions::from_mode(0o000)).unwrap();
        let result = folder_bytes(&branch, "dom", &mut roomy());
        fs::set_permissions(folder.join("closed"), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result, Err(Gap::Unreadable));
    }

    /// A folder is summed over every branch; one branch it cannot be read on,
    /// or one the budget runs out on, leaves the folder with NO figure — never
    /// with the other branches' share passed off as the whole.
    #[test]
    fn a_folder_is_summed_over_branches_and_all_or_nothing() {
        use crate::elastic::{ElasticFolderBytes, ElasticFolderGap};
        let d1 = scratch("d1");
        let c1 = scratch("c1");
        fs::create_dir_all(d1.join("filmy")).unwrap();
        fs::create_dir_all(c1.join("filmy")).unwrap();
        write(&d1.join("filmy/a"), 50_000);
        write(&c1.join("filmy/b"), 20_000);
        let expected = blocks(&d1.join("filmy")) + blocks(&d1.join("filmy/a"))
            + blocks(&c1.join("filmy")) + blocks(&c1.join("filmy/b"));
        let branches = vec![d1.clone(), c1.clone()];
        let folders = vec!["filmy".to_string(), "muzyka".to_string()];
        let usage = crate::elastic::execution::folder_usage_over(&branches, &folders, &mut roomy());
        assert_eq!(usage, vec![
            ElasticFolderBytes { name: "filmy".into(), bytes: Some(expected), gap: None },
            ElasticFolderBytes { name: "muzyka".into(), bytes: Some(0), gap: None },
        ]);
        // Room for the first branch only: the folder has no figure at all.
        let mut tight = Budget::new(3, Instant::now() + Duration::from_secs(60));
        let usage = crate::elastic::execution::folder_usage_over(&branches, &folders, &mut tight);
        assert_eq!(usage[0], ElasticFolderBytes { name: "filmy".into(), bytes: None, gap: Some(ElasticFolderGap::OverBudget) });
        assert_eq!(usage[1].gap, Some(ElasticFolderGap::OverBudget), "a spent budget stays spent for later folders");
    }

    /// Walk speed on a real tree: `TENTANAS_USAGE_BENCH=<branch>:<folder>`.
    #[test]
    #[ignore]
    fn bench_walk() {
        let spec = std::env::var("TENTANAS_USAGE_BENCH").expect("TENTANAS_USAGE_BENCH=<branch>:<folder>");
        let (branch, folder) = spec.rsplit_once(':').expect("<branch>:<folder>");
        let mut budget = Budget::new(u64::MAX, Instant::now() + Duration::from_secs(3600));
        let started = Instant::now();
        let bytes = folder_bytes(Path::new(branch), folder, &mut budget);
        let entries = u64::MAX - budget.entries_left;
        let elapsed = started.elapsed();
        println!(
            "bench: {entries} entries, {bytes:?} bytes, {:.3} s, {:.2} us/entry",
            elapsed.as_secs_f64(),
            elapsed.as_secs_f64() * 1e6 / entries.max(1) as f64
        );
    }
}
