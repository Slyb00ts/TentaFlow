// ============ File: process_sandbox.rs — native filesystem and process confinement ============

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "macos_supervisor.rs"]
mod macos_supervisor;

#[cfg(target_os = "linux")]
#[path = "linux_sandbox_net.rs"]
mod linux_sandbox_net;

/// Why this machine cannot confine an agent process, as a CLOSED set of causes.
///
/// "Unavailable" is not an answer a node picker can give: on macOS the gap
/// between a missing GUI launchd domain and a sandbox front end that is not
/// installed at all is the gap between "start the node from a desktop session"
/// and "this machine will never do it". The cause therefore travels as a
/// variant, while `Display` keeps the exact sentences the probes used to bail
/// with — every log line and every `to_string()` still reads as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxUnavailable {
    /// No `/usr/bin/sandbox-exec` (macOS) and no `/usr/bin/bwrap` (Linux).
    NoSandboxBinary,
    /// `bwrap` is installed but the kernel refuses it the unprivileged user
    /// namespace it confines with. Ubuntu 23.10+ ships
    /// `kernel.apparmor_restrict_unprivileged_userns=1`, which denies it to any
    /// program without an AppArmor profile granting `userns` — the binary being
    /// present said nothing about whether it could run.
    UserNamespacesDenied,
    /// The macOS supervisor entry point never ran in this process, so nothing
    /// can own the resource coalition of its descendants.
    SupervisorNotInitialized,
    /// launchd assigned this process no resource coalition to hand down.
    MissingCoalition,
    /// This process has no GUI launchd domain: an SSH login, a LaunchDaemon or
    /// any other session without a window server cannot host the supervisor.
    GuiSessionRequired,
}

impl std::fmt::Display for SandboxUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoSandboxBinary => {
                "process sandbox unavailable: requires macOS sandbox-exec or Linux /usr/bin/bwrap"
            }
            Self::UserNamespacesDenied => {
                "process sandbox unavailable: the kernel denies /usr/bin/bwrap an unprivileged \
                 user namespace (AppArmor restricts unprivileged user namespaces)"
            }
            Self::SupervisorNotInitialized => {
                "this executable has not initialized the process supervisor entry point"
            }
            Self::MissingCoalition => "missing resource coalition",
            Self::GuiSessionRequired => {
                "process isolation requires the current user's GUI launchd domain"
            }
        })
    }
}

impl std::error::Error for SandboxUnavailable {}

/// The DECISION, with every probe already taken.
///
/// Split from the probes so each cause can be exercised in a table: none of the
/// three macOS absences can be produced on demand on a machine that has a GUI
/// session, a launchd domain and a working supervisor.
///
/// The three macOS facts are `Option` because a Linux node has none of that
/// machinery. It cannot fail on launchd, and a sentinel "fine" would be exactly
/// the conflation of "not applicable" with "verified" these causes exist to
/// undo. `user_namespaces` is the Linux fact, `None` on macOS for the same
/// reason.
fn classify(
    sandbox_binary: bool,
    user_namespaces: Option<bool>,
    supervisor_initialized: Option<bool>,
    coalition: Option<u64>,
    launchd_domain: Option<bool>,
) -> Result<(), SandboxUnavailable> {
    if !sandbox_binary {
        return Err(SandboxUnavailable::NoSandboxBinary);
    }
    if user_namespaces == Some(false) {
        return Err(SandboxUnavailable::UserNamespacesDenied);
    }
    if supervisor_initialized == Some(false) {
        return Err(SandboxUnavailable::SupervisorNotInitialized);
    }
    if coalition == Some(0) {
        return Err(SandboxUnavailable::MissingCoalition);
    }
    if launchd_domain == Some(false) {
        return Err(SandboxUnavailable::GuiSessionRequired);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const BWRAP: &str = "/usr/bin/bwrap";

/// How long a namespace probe answers for. The availability question is asked
/// on every node-info refresh and every workspace create; one short process per
/// window is what keeps the answer honest without forking on each ask.
#[cfg(target_os = "linux")]
const USERNS_PROBE_TTL: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
static USERNS_PROBE: std::sync::Mutex<Option<(Instant, bool)>> = std::sync::Mutex::new(None);

/// Whether `bwrap` can actually create the namespaces the sandbox runs in, by
/// running it with the same `--unshare-all` the real policy uses. Checking that
/// the binary exists reported a working sandbox on every Ubuntu that denies it
/// user namespaces, and the first real launch then failed with no reason shown.
#[cfg(target_os = "linux")]
fn bwrap_can_unshare() -> bool {
    let mut cached = USERNS_PROBE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, answer)) = *cached {
        if at.elapsed() < USERNS_PROBE_TTL {
            return answer;
        }
    }
    let answer = std::process::Command::new(BWRAP)
        .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/", "/", "/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    *cached = Some((Instant::now(), answer));
    answer
}

/// A private temporary directory whose owner is PROVABLE.
///
/// Something that keeps per-run state under `TMPDIR` has to clean up after a
/// process that was SIGKILLed, and "is anything still using it" cannot be
/// answered by looking at the directory: a relay that has created its directory
/// but not yet bound its socket looks exactly like a dead one. Time heuristics
/// are no better — a slow start is not a death.
///
/// So ownership is an exclusive `flock`, and ONE ordering makes the sweep's
/// question answerable without ever guessing:
///
/// 1. the directory is built under a staging name the sweep does not match;
/// 2. its lock file is created there and locked;
/// 3. only then is it renamed to `<prefix><nonce>`, the name the sweep matches.
///
/// The invariant that follows is the whole design: **a directory has carried a
/// swept name only since an instant at which its lock was already held.** A
/// crash before step 3 therefore leaves a staging name, which is out of the
/// sweep's scope by construction — so the sweep never has to reason about a
/// half-built root, and a concurrent claim is invisible to it until it is
/// already locked. The cost of that guarantee is that an interrupted claim
/// leaks one empty staging directory, which is the OS tmp cleanup's to reclaim;
/// sweeping staging names is what would put the original defect back.
///
/// Only the Linux network relay claims one; other unix hosts compile it for its
/// tests, which hold the sweep contract on every unix platform.
#[cfg(all(unix, any(target_os = "linux", test)))]
mod private_root {
    use anyhow::{Context, Result};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::{Path, PathBuf};

    const LOCK_NAME: &str = "lock";

    pub struct PrivateRoot {
        path: PathBuf,
        /// Held open for the whole life of the root. Its lock is the only thing
        /// that says "live", and the kernel releases it however the process
        /// dies.
        _lock: std::fs::File,
    }

    impl PrivateRoot {
        pub fn claim(prefix: &str) -> Result<Self> {
            let parent = std::env::temp_dir();
            let nonce = nonce()?;
            // Leading dot: a staged claim must NOT match the sweep's prefix.
            let staging = parent.join(format!(".{prefix}staging-{nonce}"));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&staging)
                .with_context(|| format!("create {}", staging.display()))?;
            let claimed = claim_lock(&staging).and_then(|lock| {
                let path = parent.join(format!("{prefix}{nonce}"));
                std::fs::rename(&staging, &path)
                    .with_context(|| format!("publish {}", path.display()))?;
                Ok(Self { path, _lock: lock })
            });
            if claimed.is_err() {
                let _ = std::fs::remove_dir_all(&staging);
            }
            claimed
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Removes roots of `prefix` left behind by a process that died without
        /// dropping them. Only this euid's directories are considered, only
        /// those carrying the published name, and only those whose lock is free
        /// — so a live claim, bound or not, survives.
        pub fn sweep(prefix: &str) {
            let parent = std::env::temp_dir();
            let Ok(entries) = std::fs::read_dir(&parent) else {
                return;
            };
            for entry in entries.flatten() {
                // Published names only. A staging name is deliberately out of
                // scope: it is the one state in which a lock may not exist yet.
                if !entry.file_name().to_string_lossy().starts_with(prefix) {
                    continue;
                }
                let root = entry.path();
                if !owned_directory(&root) {
                    continue;
                }
                // A published root of this build always has a lock file, so the
                // absence of one means this is not a root this build produced —
                // a directory from an earlier build, or someone else's naming
                // collision. Leaving it is the only answer that cannot delete
                // live state; the OS tmp cleanup reclaims it.
                let Ok(lock) = std::fs::File::open(root.join(LOCK_NAME)) else {
                    continue;
                };
                if lock_exclusive(&lock).is_ok() {
                    let _ = std::fs::remove_dir_all(&root);
                }
            }
        }
    }

    impl Drop for PrivateRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn claim_lock(root: &Path) -> Result<std::fs::File> {
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(LOCK_NAME))
            .with_context(|| format!("create the lock of {}", root.display()))?;
        lock_exclusive(&lock).context("lock a freshly created private root")?;
        Ok(lock)
    }

    fn lock_exclusive(lock: &std::fs::File) -> Result<()> {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn owned_directory(path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() })
    }

    fn nonce() -> Result<String> {
        let mut bytes = [0u8; 12];
        if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Both halves of the sweep contract, on every unix platform: the
        /// regression was a sweep that deleted a concurrent relay's directory
        /// in the window between creating it and binding its socket.
        #[test]
        fn a_sweep_removes_an_abandoned_root_and_spares_a_claimed_one() {
            let prefix = format!("tfroot-test-{}-", nonce().unwrap());

            // Claimed and deliberately EMPTY: no socket, nothing to connect to.
            // This is exactly the state a starting relay passes through.
            let live = PrivateRoot::claim(&prefix).unwrap();
            let live_path = live.path().to_path_buf();
            assert!(live_path.is_dir());

            // What a SIGKILLed relay leaves: the directory and an unlocked lock
            // file, because the kernel dropped the lock with the process.
            let dead = std::env::temp_dir().join(format!("{prefix}dead"));
            std::fs::create_dir(&dead).unwrap();
            std::fs::write(dead.join(LOCK_NAME), b"").unwrap();

            // A root with no lock file at all cannot be proved dead.
            let foreign = std::env::temp_dir().join(format!("{prefix}foreign"));
            std::fs::create_dir(&foreign).unwrap();

            // A staging name is out of scope even with a free lock: it is the
            // one state a claim passes through before its lock exists, so the
            // sweep must not look there at all.
            let staging = std::env::temp_dir().join(format!(".{prefix}staging-abandoned"));
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join(LOCK_NAME), b"").unwrap();

            PrivateRoot::sweep(&prefix);

            assert!(live_path.is_dir(), "a claimed root must survive a sweep");
            assert!(!dead.exists(), "an abandoned root must be removed");
            assert!(foreign.is_dir(), "a root without a lock must be left alone");
            assert!(staging.is_dir(), "a staging name must be out of scope");

            drop(live);
            assert!(!live_path.exists(), "dropping a root removes it");
            std::fs::remove_dir_all(&foreign).unwrap();
            std::fs::remove_dir_all(&staging).unwrap();
        }

        /// The ordering the sweep depends on, asserted rather than described: a
        /// published name never exists without a lock, so an interrupted claim
        /// can only leave a staging name.
        #[test]
        fn an_interrupted_claim_can_only_leave_a_staging_name() {
            let prefix = format!("tfroot-test-{}-", nonce().unwrap());
            let root = PrivateRoot::claim(&prefix).unwrap();
            assert!(
                root.path().join(LOCK_NAME).is_file(),
                "a published root always carries its lock"
            );
            let published = root
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap()
                .to_string();
            assert!(published.starts_with(&prefix));
            assert!(
                !published.starts_with('.'),
                "the published name must be the one the sweep matches"
            );
        }

        #[test]
        fn a_claim_is_private_and_never_visible_unlocked() {
            let prefix = format!("tfroot-test-{}-", nonce().unwrap());
            let root = PrivateRoot::claim(&prefix).unwrap();
            let mode = std::fs::symlink_metadata(root.path())
                .unwrap()
                .permissions();
            assert_eq!(
                std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
                0o700
            );
            // The lock exists the moment the public name does, which is what
            // makes the sweep's question answerable.
            assert!(root.path().join(LOCK_NAME).is_file());
            let taken = std::fs::File::open(root.path().join(LOCK_NAME)).unwrap();
            assert!(
                lock_exclusive(&taken).is_err(),
                "a live root's lock must not be acquirable"
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub fn process_birthtime(pid: i32) -> Result<(u64, u64)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            std::mem::size_of_val(&info) as i32,
        )
    };
    if bytes != std::mem::size_of_val(&info) as i32 {
        return Err(std::io::Error::last_os_error().into());
    }
    if info.pbi_status == 5 {
        return Ok((0, 0));
    }
    Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// The first thing every binary that can host a sandbox does: this process may
/// have been started as one of the sandbox's own helpers — the macOS supervisor
/// or the Linux in-namespace proxy forwarder — rather than as the application.
/// `Some(code)` means the helper ran and the caller must exit with that code.
pub fn maybe_run_sandbox_entrypoint() -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        macos_supervisor::maybe_run()
    }
    #[cfg(target_os = "linux")]
    {
        linux_sandbox_net::maybe_run()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

pub fn is_supervisor_command(argv: &[String]) -> bool {
    cfg!(target_os = "macos")
        && argv
            .get(1)
            .is_some_and(|value| value == "--tentaflow-process-supervisor")
}

pub fn ensure_supervisor_quiescent(root: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos_supervisor::ensure_quiescent(root)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = root;
    }
    Ok(())
}

pub fn supervisor_root(argv: &[String]) -> Result<Option<PathBuf>> {
    if !is_supervisor_command(argv) {
        return Ok(None);
    }
    let spec: serde_json::Value = serde_json::from_str(
        argv.get(2)
            .context("missing process supervisor configuration")?,
    )?;
    let root = PathBuf::from(spec["root"].as_str().context("missing supervisor root")?);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = name.strip_prefix("tfp-").unwrap_or_default();
    if root.parent() != Some(Path::new("/private/tmp"))
        || suffix.len() != 24
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("invalid process supervisor root");
    }
    Ok(Some(root))
}

pub fn wait_for_supervisor(root: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match ensure_supervisor_quiescent(root) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The caller may cancel only after the OS rejected spawn, before any frontend existed.
pub fn cancel_supervisor_launch(argv: &[String]) -> Result<()> {
    let Some(root) = supervisor_root(argv)? else {
        return Ok(());
    };
    let spec: serde_json::Value = serde_json::from_str(&argv[2])?;
    let invocation = Path::new(
        spec["invocation"]
            .as_str()
            .context("missing launch intent")?,
    );
    if invocation.parent() != Some(root.as_path()) {
        bail!("invalid launch intent");
    }
    // remove_dir deliberately refuses a handoff that has already written worker state.
    std::fs::remove_dir(invocation).context("process launch intent is already active")
}

/// Where a sandboxed process reaches its egress proxy.
///
/// The CLI is given the same credentialed `http://tf:<token>@127.0.0.1:<port>`
/// URL on every platform. macOS lets the sandboxed process open that host
/// socket directly; Linux keeps the network namespace unshared, so the same
/// endpoint is served inside it by a forwarder relaying to `socket`. Obtained
/// only from `ProxyTransport`, which owns the host end of that route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    address: std::net::SocketAddr,
    #[cfg(target_os = "linux")]
    socket: PathBuf,
}

impl ProxyEndpoint {
    /// Names the endpoint for a process that did not open the transport: the
    /// bridge is told the loopback address by the component that owns the
    /// proxy, plus — where the sandbox has no route to the host — the path of
    /// the unix socket that component opened for it.
    pub fn from_parts(address: std::net::SocketAddr, socket: Option<PathBuf>) -> Result<Self> {
        validate_proxy_address(address)?;
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                address,
                socket: socket.context("sandbox proxy transport socket is required here")?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = socket;
            Ok(Self { address })
        }
    }

    pub fn socket_path(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            Some(self.socket.clone())
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

fn validate_proxy_address(address: std::net::SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() || address.port() == 0 {
        bail!("sandbox proxy must be a bound loopback endpoint");
    }
    Ok(())
}

/// The host end of the route sandboxes take to one egress proxy. Held by
/// whoever owns the proxy: dropping it closes the route before the proxy's own
/// listener is gone.
pub struct ProxyTransport {
    endpoint: ProxyEndpoint,
    #[cfg(target_os = "linux")]
    relay: linux_sandbox_net::HostRelay,
}

impl ProxyTransport {
    pub fn open(address: std::net::SocketAddr) -> Result<Self> {
        validate_proxy_address(address)?;
        #[cfg(target_os = "linux")]
        {
            Self::from_relay(address, linux_sandbox_net::HostRelay::open(address)?)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self {
                endpoint: ProxyEndpoint { address },
            })
        }
    }

    #[cfg(target_os = "linux")]
    fn from_relay(
        address: std::net::SocketAddr,
        relay: linux_sandbox_net::HostRelay,
    ) -> Result<Self> {
        let socket = relay.socket().to_path_buf();
        Ok(Self {
            endpoint: ProxyEndpoint { address, socket },
            relay,
        })
    }

    /// Only a test needs to choose the relay's bounds; production takes the
    /// ones the transport defines.
    #[cfg(all(test, target_os = "linux"))]
    fn open_with(
        address: std::net::SocketAddr,
        timeouts: linux_sandbox_net::RelayTimeouts,
    ) -> Result<Self> {
        validate_proxy_address(address)?;
        Self::from_relay(
            address,
            linux_sandbox_net::HostRelay::open_with(address, timeouts)?,
        )
    }

    /// Connections the sandbox currently has open through this transport.
    #[cfg(all(test, target_os = "linux"))]
    fn live_relays(&self) -> usize {
        self.relay.live_relays()
    }

    pub fn endpoint(&self) -> ProxyEndpoint {
        self.endpoint.clone()
    }
}

/// One file of the account's credential, made visible at the path the engine
/// inside a session's private profile reads it from.
///
/// It is the single thing a session's sandbox reaches outside its own profile:
/// the instance's HOME, history, caches, tmp and engine configuration stay
/// private, while the credential file is the ACCOUNT's one file — a refresh
/// token that rotates on use cannot live in per-instance copies, because the
/// copies would rotate apart and all but the last would be retired at the
/// provider.
///
/// The platform decides how it becomes visible, and the difference is real:
///
/// * Linux mounts the file (`--bind`), so the destination is a mount point that
///   nothing inside the sandbox can replace, rename over or write around;
/// * macOS `sandbox-exec` has no bind mount, so the exposure is a symlink and the
///   policy allows the SOURCE literally. An engine that rotates through a
///   temporary file would replace that symlink with a file of its own, so a spawn
///   that finds anything else at the destination refuses instead of running the
///   account's engine against a credential nobody else can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialExposure {
    source: PathBuf,
    destination: PathBuf,
}

impl CredentialExposure {
    pub fn new(source: PathBuf, destination: PathBuf) -> Self {
        Self {
            source,
            destination,
        }
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// Puts the file where the engine will open it, or refuses a destination
    /// that is not this exposure.
    fn install(&self) -> Result<()> {
        let replaced = || {
            anyhow::anyhow!(
                "credential exposure was replaced: {}",
                self.destination.display()
            )
        };
        #[cfg(target_os = "macos")]
        {
            return match std::fs::symlink_metadata(&self.destination) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    if std::fs::read_link(&self.destination)? == self.source {
                        Ok(())
                    } else {
                        Err(replaced())
                    }
                }
                Ok(_) => Err(replaced()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::os::unix::fs::symlink(&self.source, &self.destination)?;
                    Ok(())
                }
                Err(error) => Err(error.into()),
            };
        }
        #[cfg(target_os = "linux")]
        {
            // A mount point for a file source is created by bwrap; an entry of
            // any other kind there is not this exposure.
            return match std::fs::symlink_metadata(&self.destination) {
                Ok(metadata) if metadata.file_type().is_file() => Ok(()),
                Ok(_) => Err(replaced()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&self.destination)?;
                    Ok(())
                }
                Err(error) => Err(error.into()),
            };
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = replaced;
            bail!("credential exposure is unsupported on this operating system")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSandbox {
    workspace: PathBuf,
    private_root: PathBuf,
    read_only: bool,
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    credential: Option<CredentialExposure>,
    proxy: Option<ProxyEndpoint>,
    #[cfg(target_os = "macos")]
    supervisor_root: PathBuf,
}

impl ProcessSandbox {
    /// Where this policy's supervisor keeps its state, on a platform that has
    /// one. Linux tears a sandbox down through the pid namespace, so the answer
    /// there is `None` — and the bridge, which never asks, is why this is
    /// unused on a Linux build of THIS file. Core calls it on both platforms.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn supervisor_root(&self) -> Option<&Path> {
        #[cfg(target_os = "macos")]
        {
            return Some(&self.supervisor_root);
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    pub fn new(
        workspace: &Path,
        private_root: &Path,
        read_only: bool,
        read_roots: &[PathBuf],
        write_roots: &[PathBuf],
    ) -> Result<Self> {
        Self::check_available()?;
        let workspace = canonical_directory(workspace)?;
        let private_root = canonical_directory(private_root)?;
        if workspace.starts_with(&private_root) || private_root.starts_with(&workspace) {
            bail!("sandbox private state and workspace must not overlap");
        }
        let canonical_roots = |roots: &[PathBuf]| -> Result<Vec<PathBuf>> {
            roots.iter().map(|root| canonical_directory(root)).collect()
        };
        let policy = Self {
            workspace,
            private_root,
            read_only,
            read_roots: canonical_roots(read_roots)?,
            write_roots: canonical_roots(write_roots)?,
            credential: None,
            proxy: None,
            #[cfg(target_os = "macos")]
            supervisor_root: macos_supervisor::new_root()?,
        };
        #[cfg(target_os = "macos")]
        for granted in std::iter::once(&policy.workspace)
            .chain(std::iter::once(&policy.private_root))
            .chain(policy.read_roots.iter())
            .chain(policy.write_roots.iter())
        {
            if policy.supervisor_root.starts_with(granted) {
                bail!("supervisor state must remain outside every sandbox grant");
            }
        }
        policy.validate()?;
        Ok(policy)
    }

    /// Adds the account's credential file to the policy, installing it where the
    /// engine will open it.
    ///
    /// The source is checked before anything is mounted or linked: a regular file
    /// reached without a symbolic link in any component, outside every grant this
    /// sandbox already makes, and inside a real directory. The destination must
    /// be inside the private profile — it is the profile that is disposable, and
    /// a destination anywhere else would be a grant nobody asked for.
    pub fn with_credential(mut self, exposure: CredentialExposure) -> Result<Self> {
        let source = exposure.source();
        let directory = source
            .parent()
            .context("credential source has no directory")?;
        // `canonicalize` resolves links, so a canonical parent equal to the given
        // one is the proof that no component of the source is a link.
        if canonical_directory(directory)? != *directory
            || std::fs::canonicalize(source)? != *source
        {
            bail!("credential source must not be reached through a symbolic link");
        }
        if !std::fs::symlink_metadata(source)?.file_type().is_file() {
            bail!("credential source must be a regular file");
        }
        if source.starts_with(&self.private_root) || source.starts_with(&self.workspace) {
            bail!("credential source must be outside every sandbox grant");
        }
        let destination = exposure.destination();
        let parent = destination
            .parent()
            .context("credential destination has no directory")?;
        if canonical_directory(parent)? != *parent || !parent.starts_with(&self.private_root) {
            bail!("credential destination must be inside the private profile");
        }
        if destination.starts_with(&self.workspace) {
            bail!("credential destination must not be inside the workspace");
        }
        exposure.install()?;
        self.credential = Some(exposure);
        Ok(self)
    }

    pub fn with_proxy(mut self, endpoint: ProxyEndpoint) -> Result<Self> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            bail!("process sandbox proxy transport is unavailable on this platform");
        }
        #[cfg(target_os = "linux")]
        if !std::fs::symlink_metadata(&endpoint.socket)
            .map(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()))
            .unwrap_or(false)
        {
            bail!("sandbox proxy transport socket is missing");
        }
        self.proxy = Some(endpoint);
        Ok(self)
    }

    pub fn check_available() -> Result<(), SandboxUnavailable> {
        #[cfg(target_os = "macos")]
        return macos_supervisor::check_available();
        #[cfg(target_os = "linux")]
        // Linux confines by namespaces and bind mounts, so a `bwrap` that can
        // create them is the whole requirement and there is no supervisor,
        // coalition or window server to probe — those `None`s carry that
        // absence, they are not a check passed.
        return {
            let binary = Path::new(BWRAP).is_file();
            classify(
                binary,
                binary.then(bwrap_can_unshare),
                None,
                None,
                None,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(SandboxUnavailable::NoSandboxBinary);
    }

    /// Drops the cached namespace probe, so the next `check_available` measures
    /// again. Called after something changed the answer on purpose — installing
    /// the AppArmor profile — instead of waiting out the cache.
    pub fn forget_availability_probe() {
        #[cfg(target_os = "linux")]
        {
            *USERNS_PROBE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    fn validate(&self) -> Result<()> {
        for root in std::iter::once(&self.workspace)
            .chain(std::iter::once(&self.private_root))
            .chain(self.write_roots.iter())
        {
            if canonical_directory(root)? != *root {
                bail!("sandbox root changed: {}", root.display());
            }
            validate_workspace_tree(root)?;
        }
        Ok(())
    }

    pub fn wrap(&self, argv: &[String], cwd: &Path) -> Result<Vec<String>> {
        let command = self.native_command(argv, cwd)?;
        #[cfg(target_os = "macos")]
        {
            return macos_supervisor::wrap(
                command,
                cwd,
                self.supervisor_root().context("missing supervisor root")?,
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(command)
        }
    }

    /// Same shape as `supervisor_root`: a Linux sandbox has nothing to settle,
    /// and the bridge never asks, so this is unused on a Linux build of this
    /// shared file while core calls it on both platforms.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn ensure_quiescent(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            macos_supervisor::ensure_quiescent(&self.supervisor_root)?;
        }
        Ok(())
    }

    fn native_command(&self, argv: &[String], cwd: &Path) -> Result<Vec<String>> {
        if argv.is_empty() || argv.iter().any(|arg| arg.contains('\0')) {
            bail!("invalid sandbox command");
        }
        self.validate()?;
        let cwd = canonical_directory(cwd)?;
        if !cwd.starts_with(&self.workspace) {
            bail!("sandbox working directory is outside the workspace");
        }
        let mut command = self.platform_command(&cwd)?;
        command.extend_from_slice(argv);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    fn platform_command(&self, _cwd: &Path) -> Result<Vec<String>> {
        let mut profile = String::from(
            "(version 1)\n(deny default)\n(allow process-exec process-fork)\n\
             (allow process-info* signal (target same-sandbox))\n\
             (allow sysctl-read (sysctl-name-prefix \"hw.\") (sysctl-name-prefix \"machdep.cpu.\") (sysctl-name \"kern.ostype\") (sysctl-name \"kern.osrelease\") (sysctl-name \"kern.osversion\") (sysctl-name \"kern.version\") (sysctl-name \"kern.argmax\") (sysctl-name \"kern.maxfiles\") (sysctl-name \"kern.maxfilesperproc\") (sysctl-name \"kern.osproductversion\") (sysctl-name \"vm.loadavg\"))\n(allow pseudo-tty)\n\
             (allow file-read-data (literal \"/\"))\n(allow file-read-metadata (literal \"/var\") (literal \"/tmp\") (literal \"/etc\"))\n\
             (allow file-read* file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/ptmx\") (literal \"/dev/tty\"))\n\
             (allow file-read* (literal \"/dev/random\") (literal \"/dev/urandom\"))\n\
             (allow file-ioctl (literal \"/dev/ptmx\") (literal \"/dev/tty\"))\n",
        );
        for root in system_read_roots()
            .iter()
            .chain(self.read_roots.iter())
            .chain(std::iter::once(&self.workspace))
            .chain(std::iter::once(&self.private_root))
            .chain(self.write_roots.iter())
        {
            for ancestor in root.ancestors() {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    quote_path(ancestor)?
                ));
            }
            profile.push_str(&format!(
                "(allow file-read* (subpath {}))\n",
                quote_path(root)?
            ));
        }
        for root in std::iter::once(&self.private_root)
            .chain(self.write_roots.iter())
            .chain((!self.read_only).then_some(&self.workspace))
        {
            profile.push_str(&format!(
                "(allow file-write* (subpath {}))\n",
                quote_path(root)?
            ));
            profile.push_str(&format!(
                "(deny file-write-unlink (literal {}))\n",
                quote_path(root)?
            ));
        }
        profile.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote_path(&self.workspace.join(".git"))?
        ));
        if let Some(credential) = &self.credential {
            // The destination is a link, so the engine's open resolves to the
            // source: the policy has to allow THAT path, and the ancestors are
            // what the lookup walks through. The link itself is covered by the
            // private root's rules, the source's siblings and the rest of the
            // account's directory are not — they are not named here at all.
            for ancestor in credential.source().ancestors() {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    quote_path(ancestor)?
                ));
            }
            profile.push_str(&format!(
                "(allow file-read* file-write* (literal {}))\n",
                quote_path(credential.source())?
            ));
        }
        if let Some(proxy) = &self.proxy {
            profile.push_str(&format!(
                "(allow network-outbound (remote tcp \"localhost:{}\"))\n",
                proxy.address.port()
            ));
        }
        Ok(vec![
            "/usr/bin/sandbox-exec".into(),
            "-p".into(),
            profile,
            "--".into(),
        ])
    }

    #[cfg(target_os = "linux")]
    fn platform_command(&self, cwd: &Path) -> Result<Vec<String>> {
        let mut args: Vec<String> = [
            "/usr/bin/bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for root in system_read_roots().iter().chain(self.read_roots.iter()) {
            args.extend([
                "--ro-bind".into(),
                root.display().to_string(),
                root.display().to_string(),
            ]);
        }
        args.extend([
            if self.read_only {
                "--ro-bind"
            } else {
                "--bind"
            }
            .into(),
            self.workspace.display().to_string(),
            self.workspace.display().to_string(),
        ]);
        for root in std::iter::once(&self.private_root).chain(self.write_roots.iter()) {
            args.extend([
                "--bind".into(),
                root.display().to_string(),
                root.display().to_string(),
            ]);
        }
        let git = self.workspace.join(".git");
        if git.exists() {
            args.extend([
                "--ro-bind".into(),
                git.display().to_string(),
                git.display().to_string(),
            ]);
        }
        if let Some(credential) = &self.credential {
            // Read-write: the engine on this account rotates the file, and the
            // rotation is the whole reason the account has ONE file instead of a
            // copy per instance. After the private root, so the mount point it
            // replaces is the profile's own path and not the other way round.
            args.extend([
                "--bind".into(),
                credential.source().display().to_string(),
                credential.destination().display().to_string(),
            ]);
        }
        if let Some(proxy) = &self.proxy {
            // Read-write, because `connect` on a unix socket is not a write to
            // the filesystem but a read-only mount is a needless difference
            // from the permissions the host itself enforces on it.
            let socket = proxy.socket.display().to_string();
            args.extend(["--bind".into(), socket.clone(), socket]);
            // The forwarder is this very executable, so it has to be readable
            // inside. Only the file: binding its directory would hand the
            // sandbox everything that happens to sit next to the binary, which
            // on a development machine is the whole build output.
            let forwarder = linux_sandbox_net::host_executable()?.display().to_string();
            args.extend(["--ro-bind".into(), forwarder.clone(), forwarder]);
        }
        args.extend(["--chdir".into(), cwd.display().to_string(), "--".into()]);
        if let Some(proxy) = &self.proxy {
            args.extend(linux_sandbox_net::entry_command(
                proxy.address.port(),
                &proxy.socket,
            )?);
        }
        Ok(args)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn platform_command(&self, _cwd: &Path) -> Result<Vec<String>> {
        bail!("process sandbox is unsupported on this operating system")
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("sandbox path {}", path.display()))?;
    if !canonical.is_dir() || canonical.parent().is_none() {
        bail!("sandbox root must be a non-root directory");
    }
    Ok(canonical)
}

/// Admission rejects aliases crossing the grant or its protected metadata boundary.
/// Other unsandboxed writers must stay out of admitted directories during a lease.
pub fn validate_workspace_tree(root: &Path) -> Result<()> {
    let started = Instant::now();
    let mut directories = vec![root.to_path_buf()];
    let mut count = 0usize;
    #[cfg(unix)]
    let mut snapshots = Vec::new();
    #[cfg(unix)]
    let mut links: std::collections::HashMap<(u64, u64, bool), (u64, u64)> =
        std::collections::HashMap::new();
    while let Some(directory) = directories.pop() {
        #[cfg(unix)]
        snapshots.push((directory.clone(), std::fs::symlink_metadata(&directory)?));
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            count += 1;
            if count > 1_000_000 || started.elapsed() > Duration::from_secs(30) {
                bail!("sandbox admission scan exceeded its budget");
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            let kind = metadata.file_type();
            if kind.is_dir() {
                directories.push(path.clone());
            } else if kind.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let protected = path.strip_prefix(root)?.components().any(|part| {
                        part.as_os_str()
                            .to_str()
                            .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
                    });
                    let entry = links
                        .entry((metadata.dev(), metadata.ino(), protected))
                        .or_insert((metadata.nlink(), 0));
                    if entry.0 != metadata.nlink() {
                        bail!("sandbox inode changed during admission");
                    }
                    entry.1 += 1;
                }
            } else if !kind.is_symlink() {
                bail!("sandbox rejects special file: {}", path.display());
            }
            #[cfg(unix)]
            snapshots.push((path, metadata));
        }
    }
    #[cfg(unix)]
    {
        if links.values().any(|(total, inside)| total != inside) {
            bail!(
                "sandbox rejects hardlinked file crossing its grant or protected metadata boundary"
            );
        }
        for (path, before) in snapshots {
            if started.elapsed() > Duration::from_secs(30) {
                bail!("sandbox admission scan exceeded its budget");
            }
            let after = std::fs::symlink_metadata(&path)?;
            if metadata_identity(&before) != metadata_identity(&after) {
                bail!("sandbox entry changed during admission: {}", path.display());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_identity(
    metadata: &std::fs::Metadata,
) -> (u64, u64, u64, u64, u32, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(target_os = "macos")]
fn quote_path(path: &Path) -> Result<String> {
    let path = path.to_str().context("sandbox paths must be UTF-8")?;
    if path.chars().any(char::is_control) {
        bail!("sandbox paths must not contain control characters");
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn system_read_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let paths = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/libexec",
        "/System/Library",
        "/System/Volumes/Preboot/Cryptexes/OS/usr/lib",
        "/Library/Apple",
        "/private/var/select/sh",
        "/private/etc/ssl",
        "/private/etc/localtime",
    ];
    #[cfg(not(target_os = "macos"))]
    let paths = [
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/etc/ssl",
        "/etc/ld.so.cache",
        "/etc/localtime",
    ];
    // Only the macOS branch below extends the list.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut roots: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| Path::new(path).exists())
        .map(PathBuf::from)
        .collect();
    #[cfg(target_os = "macos")]
    for sdk in [
        "/Library/Developer/CommandLineTools",
        "/Applications/Xcode.app/Contents/Developer",
    ] {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(sdk) {
            if metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0 {
                roots.push(PathBuf::from(sdk));
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_test_entrypoint() {
        if std::env::var_os("TENTAFLOW_SUPERVISOR_TEST_MODE").is_none() {
            return;
        }
        // Cargo's dylib search paths load the test host, never the sandboxed CLI.
        for name in ["DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] {
            std::env::remove_var(name);
        }
        unsafe {
            for descriptor in 0..3 {
                assert!(libc::dup2(descriptor + 3, descriptor) >= 0);
                libc::close(descriptor + 3);
            }
        }
        std::process::exit(
            super::maybe_run_sandbox_entrypoint().expect("supervisor test invocation"),
        );
    }
    use super::*;

    /// One machine state per row, and the ONE cause it must produce.
    ///
    /// The classifier, not the prose, is what the node picker keys on: an
    /// assertion over `Display` alone would pass while every refusal produced
    /// the same cause.
    #[test]
    fn classify_names_the_cause_behind_every_unavailable_sandbox() {
        type Decision = Result<(), SandboxUnavailable>;
        let table: &[(bool, Option<bool>, Option<u64>, Option<bool>, Decision)] = &[
            // A Linux node: the sandbox binary is the whole requirement, and it
            // satisfies the classifier on its own.
            (true, None, None, None, Ok(())),
            (
                false,
                None,
                None,
                None,
                Err(SandboxUnavailable::NoSandboxBinary),
            ),
            // A macOS node with every mechanism in place.
            (true, Some(true), Some(7), Some(true), Ok(())),
            (
                true,
                Some(false),
                Some(7),
                Some(true),
                Err(SandboxUnavailable::SupervisorNotInitialized),
            ),
            (
                true,
                Some(true),
                Some(0),
                Some(true),
                Err(SandboxUnavailable::MissingCoalition),
            ),
            // The GUI cause the plan names — a node started over SSH, with a
            // working supervisor and a coalition of its own.
            (
                true,
                Some(true),
                Some(7),
                Some(false),
                Err(SandboxUnavailable::GuiSessionRequired),
            ),
            // A missing front end outranks every macOS fact, because there is
            // nothing to probe it against.
            (
                false,
                Some(false),
                Some(0),
                Some(false),
                Err(SandboxUnavailable::NoSandboxBinary),
            ),
        ];
        for (binary, initialized, coalition, domain, expected) in table {
            assert_eq!(
                classify(*binary, None, *initialized, *coalition, *domain),
                *expected,
                "binary={binary} initialized={initialized:?} coalition={coalition:?} \
                 domain={domain:?}"
            );
        }
    }

    /// A Linux `bwrap` that is present but denied its namespaces is a refusal
    /// of its own, not a working sandbox; a missing binary still outranks it,
    /// since there is nothing to deny anything to.
    #[test]
    fn a_denied_namespace_is_its_own_refusal() {
        assert_eq!(
            classify(true, Some(false), None, None, None),
            Err(SandboxUnavailable::UserNamespacesDenied)
        );
        assert_eq!(classify(true, Some(true), None, None, None), Ok(()));
        assert_eq!(
            classify(false, Some(false), None, None, None),
            Err(SandboxUnavailable::NoSandboxBinary)
        );
    }

    /// Every `.to_string()` consumer — the node picker's diagnostic, the deploy
    /// refusal, the installer log — reads these sentences, so the cause split
    /// must not have reworded any of them.
    #[test]
    fn every_cause_keeps_the_sentence_its_callers_already_print() {
        assert_eq!(
            SandboxUnavailable::NoSandboxBinary.to_string(),
            "process sandbox unavailable: requires macOS sandbox-exec or Linux /usr/bin/bwrap"
        );
        assert_eq!(
            SandboxUnavailable::SupervisorNotInitialized.to_string(),
            "this executable has not initialized the process supervisor entry point"
        );
        assert_eq!(
            SandboxUnavailable::MissingCoalition.to_string(),
            "missing resource coalition"
        );
        assert_eq!(
            SandboxUnavailable::GuiSessionRequired.to_string(),
            "process isolation requires the current user's GUI launchd domain"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn rejected_spawn_releases_only_its_unused_launch_intent() {
        let workspace = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let policy =
            ProcessSandbox::new(workspace.path(), private.path(), false, &[], &[]).unwrap();
        let mut argv = policy
            .wrap(&["/usr/bin/true".into()], workspace.path())
            .unwrap();
        let root = supervisor_root(&argv).unwrap().unwrap();
        assert!(ensure_supervisor_quiescent(&root).is_err());
        argv[0] = private
            .path()
            .join("absent-executable")
            .display()
            .to_string();
        assert_eq!(
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .spawn()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        cancel_supervisor_launch(&argv).unwrap();
        ensure_supervisor_quiescent(&root).unwrap();

        let argv = policy
            .wrap(&["/usr/bin/true".into()], workspace.path())
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&argv[2]).unwrap();
        let invocation = Path::new(request["invocation"].as_str().unwrap());
        std::fs::write(invocation.join("spec.json"), b"handoff has started").unwrap();
        assert!(cancel_supervisor_launch(&argv).is_err());
        assert!(invocation.join("spec.json").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn admission_counts_only_internal_hardlinks_in_the_same_access_class() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let artifact = project.path().join("artifact");
        std::fs::write(&artifact, b"build output").unwrap();
        std::fs::hard_link(&artifact, project.path().join("artifact-alias")).unwrap();
        validate_workspace_tree(project.path()).unwrap();
        std::fs::hard_link(&artifact, outside.path().join("third-link")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(outside.path().join("third-link")).unwrap();
        std::fs::remove_file(project.path().join("artifact-alias")).unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        std::fs::hard_link(&artifact, project.path().join(".git/config")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(project.path().join(".git/config")).unwrap();
        std::fs::remove_dir(project.path().join(".git")).unwrap();
        std::fs::create_dir(project.path().join(".GIT")).unwrap();
        std::fs::hard_link(&artifact, project.path().join(".GIT/config")).unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
        std::fs::remove_file(project.path().join(".GIT/config")).unwrap();
        std::fs::hard_link(&artifact, outside.path().join("third-link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("third-link"),
            project.path().join("symlink-alias"),
        )
        .unwrap();
        assert!(validate_workspace_tree(project.path()).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn admission_rejects_hardlinks_and_sockets() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "other account").unwrap();
        std::fs::hard_link(outside.path().join("secret"), root.path().join("alias")).unwrap();
        assert!(validate_workspace_tree(root.path())
            .unwrap_err()
            .to_string()
            .contains("hardlinked"));
        std::fs::remove_file(root.path().join("alias")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(root.path().join("host.sock")).unwrap();
        assert!(validate_workspace_tree(root.path())
            .unwrap_err()
            .to_string()
            .contains("special file"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_works_on_inherited_pty() {
        assert_inherited_pty(false);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_transfers_controlling_pty() {
        assert_inherited_pty(true);
    }

    #[cfg(target_os = "macos")]
    fn assert_inherited_pty(supervised: bool) {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&private).unwrap();
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let argv = policy
            .native_command(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "test -t 0 && test -t 1 && printf terminal > pty-result".into(),
                ],
                &workspace,
            )
            .unwrap();
        let argv = if supervised {
            macos_supervisor::wrap(argv, &workspace, &policy.supervisor_root).unwrap()
        } else {
            argv
        };
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        unsafe {
            assert_ne!(libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC), -1);
            assert_ne!(libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC), -1);
        }
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &private)
            .current_dir(&workspace)
            .stdin(std::process::Stdio::from(slave.try_clone().unwrap()))
            .stdout(std::process::Stdio::from(slave.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(slave.try_clone().unwrap()));
        let descriptor = slave.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || (!supervised && libc::ioctl(descriptor, libc::TIOCSCTTY as _, 0) == -1)
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(command);
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut file = std::fs::File::from(master);
            let mut output = Vec::new();
            let _ = file.read_to_end(&mut output);
            output
        });
        drop(slave);
        let status = child.wait().unwrap();
        let output = reader.join().unwrap();
        assert!(
            status.success(),
            "{status}: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(
            std::fs::read(workspace.join("pty-result")).unwrap(),
            b"terminal"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn supervisor_reaps_double_fork_and_cancellation() {
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("project");
            let private = root.path().join("profile");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::create_dir(&private).unwrap();
            let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
            let script = format!("my $c=fork();if($c==0){{setsid();my $g=fork();exit 0 if $g;open(my $f, '>', 'daemon.pid') or die $!;print $f $$;close $f;close STDIN;close STDOUT;close STDERR;sleep 60;exit 0;}}sleep {};exit 0;",if cancel {60}else{1});
            let argv = policy
                .wrap(
                    &[
                        "/usr/bin/perl".into(),
                        "-MPOSIX".into(),
                        "-e".into(),
                        script,
                    ],
                    &workspace,
                )
                .unwrap();
            let mut child = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &private)
                .current_dir(&workspace)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let daemon_file = workspace.join("daemon.pid");
            // The daemon opens the file BEFORE it prints its pid into it, so the
            // file's existence is not the pid being there yet — an existence test
            // followed by a read races the write it is waiting for. The READ is
            // what the deadline is for: an absent file and one that is still
            // empty are the same answer, "not written yet".
            let mut stamped = None;
            while Instant::now() < deadline {
                if let Some(pid) = std::fs::read_to_string(&daemon_file)
                    .ok()
                    .and_then(|contents| contents.trim().parse::<i32>().ok())
                {
                    stamped = Some(pid);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let pid = stamped.expect("daemon never started");
            if cancel {
                child.kill().unwrap();
            }
            let status = child.wait().unwrap();
            if !cancel {
                assert!(status.success(), "{status}");
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while policy.ensure_quiescent().is_err() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            policy.ensure_quiescent().unwrap();
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let count = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    std::mem::size_of_val(&info) as i32,
                )
            };
            assert!(
                count == 0 || info.pbi_status == 5,
                "detached descendant survived cleanup"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_only_reaches_its_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&private).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let run = |policy: &ProcessSandbox| {
            let argv = policy
                .native_command(
                    &[
                        "/usr/bin/curl".into(),
                        "--max-time".into(),
                        "2".into(),
                        "--silent".into(),
                        url.clone(),
                    ],
                    &workspace,
                )
                .unwrap();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("HOME", &private)
                .current_dir(&workspace)
                .output()
                .unwrap()
        };
        assert!(!run(&policy).status.success());
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        connection
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0; 4096];
                        assert!(
                            connection.read(&mut request).unwrap() > 0,
                            "the sandbox opened the connection but sent no request"
                        );
                        connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
                        return;
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    other => panic!("proxy connection failed: {other:?}"),
                }
            }
        });
        let transport = ProxyTransport::open(address).unwrap();
        let proxied = policy.with_proxy(transport.endpoint()).unwrap();
        let output = run(&proxied);
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"ok");
        worker.join().unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn real_process_sandbox_confines_descendants_and_read_only_mounts() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        let outside = root.path().join("other-user");
        for path in [&workspace, &private, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(workspace.join("input"), "allowed").unwrap();
        std::fs::create_dir(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/config"), "protected").unwrap();
        std::fs::write(outside.join("secret"), "private").unwrap();
        symlink(outside.join("secret"), workspace.join("alias")).unwrap();
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let run = |policy: &ProcessSandbox, script: &str| {
            let argv = policy
                .native_command(&["/bin/sh".into(), "-c".into(), script.into()], &workspace)
                .unwrap();
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &private)
                .current_dir(&workspace)
                .output()
                .unwrap()
        };
        let output = run(
            &policy,
            "cat input; printf saved > output; printf profile > \"$HOME/state\"",
        );
        assert!(
            output.status.success(),
            "status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"allowed");
        assert_eq!(std::fs::read(workspace.join("output")).unwrap(), b"saved");
        assert!(!run(&policy, "ln .git/config metadata-alias")
            .status
            .success());
        let output = run(&policy, "sh -c 'cat alias'");
        assert!(!output.status.success());
        assert!(!run(&policy, "printf leaked > alias").status.success());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"private");
        let read_only = ProcessSandbox::new(&workspace, &private, true, &[], &[]).unwrap();
        assert!(!run(&read_only, "printf denied > output").status.success());
        assert!(run(&read_only, "printf yes > \"$HOME/state\"")
            .status
            .success());
        assert!(policy.native_command(&["true".into()], &outside).is_err());
    }

    /// The Linux forwarder re-enters through this test, because libtest owns
    /// the argv of a test binary: the sandbox invocation travels in the
    /// environment instead. Mirrors `supervisor_test_entrypoint` on macOS.
    #[test]
    #[cfg(target_os = "linux")]
    fn sandbox_network_test_entrypoint() {
        if std::env::var_os("TENTAFLOW_SANDBOX_NETWORK_TEST_ARGV").is_none() {
            return;
        }
        std::process::exit(
            super::maybe_run_sandbox_entrypoint().expect("sandbox network test invocation"),
        );
    }

    #[cfg(target_os = "linux")]
    fn sandbox_test_entry() -> String {
        format!(
            "{}::sandbox_network_test_entrypoint",
            module_path!().split_once("::").unwrap().1
        )
    }

    /// Runs a sandboxed command whose forwarder is this test binary.
    #[cfg(target_os = "linux")]
    fn run_sandboxed(argv: &[String], private: &Path, cwd: &Path) -> std::process::Output {
        let mut command = std::process::Command::new(&argv[0]);
        match argv
            .iter()
            .position(|argument| argument == linux_sandbox_net::FORWARDER)
        {
            Some(index) => {
                command.args(&argv[1..index]).args([
                    "--exact",
                    &sandbox_test_entry(),
                    "--nocapture",
                ]);
                command.env_clear().env(
                    "TENTAFLOW_SANDBOX_NETWORK_TEST_ARGV",
                    argv[index - 1..].join("\u{1}"),
                );
            }
            None => {
                command.args(&argv[1..]);
                command.env_clear();
            }
        }
        command
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", private)
            .current_dir(cwd)
            .output()
            .unwrap()
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_sandbox_carries_its_proxy_without_sharing_the_network() {
        // A policy is only built where the sandbox can run: the availability
        // probe starts bwrap for real, so a host denying unprivileged
        // namespaces refuses the policy before there is an argv to inspect.
        if let Some(reason) = unprivileged_netns_denial() {
            eprintln!(
                "SKIP linux_sandbox_carries_its_proxy_without_sharing_the_network: {reason}"
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        for path in [&workspace, &private] {
            std::fs::create_dir(path).unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ProxyTransport::open(address).unwrap();
        let socket = transport.endpoint().socket.display().to_string();
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[])
            .unwrap()
            .with_proxy(transport.endpoint())
            .unwrap();
        let argv = policy
            .native_command(&["/bin/true".into()], &workspace)
            .unwrap();
        assert_eq!(argv[0], "/usr/bin/bwrap");
        assert!(argv.iter().any(|argument| argument == "--unshare-all"));
        assert!(!argv.iter().any(|argument| argument == "--share-net"));
        assert!(argv
            .windows(3)
            .any(|window| window == ["--bind".to_string(), socket.clone(), socket.clone()]));
        // The forwarder enters as a single file. Its directory is the build
        // output on a development machine and a shared install root on a node;
        // neither belongs to the sandbox.
        let forwarder = linux_sandbox_net::host_executable()
            .unwrap()
            .display()
            .to_string();
        assert!(argv.windows(3).any(|window| window
            == [
                "--ro-bind".to_string(),
                forwarder.clone(),
                forwarder.clone()
            ]));
        let directory = linux_sandbox_net::host_executable()
            .unwrap()
            .parent()
            .unwrap()
            .display()
            .to_string();
        assert!(
            !argv.contains(&directory),
            "the forwarder's directory must not be bound"
        );
        let index = argv
            .iter()
            .position(|argument| argument == linux_sandbox_net::FORWARDER)
            .expect("the forwarder is bubblewrap's initial child");
        assert_eq!(argv[index - 2], "--");
        assert_eq!(
            argv[index - 1],
            linux_sandbox_net::host_executable()
                .unwrap()
                .display()
                .to_string()
        );
        let spec: serde_json::Value = serde_json::from_str(&argv[index + 1]).unwrap();
        assert_eq!(spec["port"].as_u64(), Some(u64::from(address.port())));
        assert_eq!(spec["socket"].as_str(), Some(socket.as_str()));
        assert_eq!(argv[index + 2..], ["/bin/true".to_string()]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_host_relay_dials_the_gateway_only_when_the_sandbox_speaks() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ProxyTransport::open(address).unwrap();
        let socket = transport.endpoint().socket;

        // A readiness probe opens and closes the transport; the gateway must
        // not see a connection it would have to screen and log.
        drop(UnixStream::connect(&socket).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );

        let mut client = UnixStream::connect(&socket).unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut gateway = loop {
            match listener.accept() {
                Ok((connection, _)) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                other => panic!("the relay never reached the gateway: {other:?}"),
            }
        };
        let mut request = [0u8; 18];
        gateway.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"GET / HTTP/1.1\r\n\r\n");
        gateway.write_all(b"ok").unwrap();
        drop(gateway);
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "ok");

        drop(transport);
        assert!(UnixStream::connect(&socket).is_err());
    }

    /// The transport is the one part of the sandbox a CLI can drive without
    /// limit, so its cost must be bounded: connections past the cap are closed
    /// instead of queued as threads, and a connection that never speaks is not
    /// a permanent thread.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_host_relay_bounds_what_a_sandbox_can_open() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        // Never accepts: a relay is claimed when the connection arrives, before
        // the gateway is dialled, which is exactly the window being bounded.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ProxyTransport::open(address).unwrap();
        let socket = transport.endpoint().socket;

        let held: Vec<UnixStream> = (0..linux_sandbox_net::MAX_RELAYS)
            .map(|_| UnixStream::connect(&socket).unwrap())
            .collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        while transport.live_relays() < linux_sandbox_net::MAX_RELAYS && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(transport.live_relays(), linux_sandbox_net::MAX_RELAYS);

        // One more is accepted by the kernel and then closed by the relay. The
        // caller finds out at once instead of waiting on a thread that will
        // never exist; whether the close arrives as an end of stream or as a
        // reset is the kernel's choice, and both say refused rather than
        // served.
        let mut refused = UnixStream::connect(&socket).unwrap();
        refused
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut answer = Vec::new();
        match refused.read_to_end(&mut answer) {
            Ok(_) => assert!(
                answer.is_empty(),
                "a refused connection must not be served: {answer:?}"
            ),
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset,
                "a refused connection must be closed, not left hanging"
            ),
        }

        // Releasing the held connections frees the slots again.
        drop(held);
        while transport.live_relays() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(transport.live_relays(), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_silent_connection_does_not_hold_a_relay_for_ever() {
        use std::os::unix::net::UnixStream;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ProxyTransport::open_with(
            address,
            linux_sandbox_net::RelayTimeouts {
                opening: Duration::from_millis(200),
                ..Default::default()
            },
        )
        .unwrap();
        let client = UnixStream::connect(transport.endpoint().socket).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while transport.live_relays() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(transport.live_relays(), 1);
        // The client says nothing at all. The relay must let go on its own.
        while transport.live_relays() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            transport.live_relays(),
            0,
            "a silent connection kept its relay"
        );
        // The gateway was never dialled for a connection that said nothing.
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        drop(client);
    }

    /// Relays are opened per sandbox run, so opening one while another is
    /// starting is ordinary. The sweep that each `open` runs must not touch a
    /// live relay — including one that has not bound its socket yet, which is
    /// what a filesystem or connect-based sweep could not tell from a dead one.
    #[test]
    #[cfg(target_os = "linux")]
    fn opening_a_relay_never_disturbs_one_that_is_already_live() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let first = ProxyTransport::open(address).unwrap();
        let socket = first.endpoint().socket;

        // Every one of these runs a sweep. None may remove the first relay's
        // root, and none may remove each other's.
        let others: Vec<ProxyTransport> = (0..4)
            .map(|_| ProxyTransport::open(address).unwrap())
            .collect();
        assert!(socket.exists(), "a live relay's socket was swept away");
        for other in &others {
            assert!(other.endpoint().socket.exists());
        }

        // Still functional, not merely present.
        let gateway = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0u8; 18];
            connection.read_exact(&mut request).unwrap();
            connection.write_all(b"ok").unwrap();
        });
        let mut client = UnixStream::connect(&socket).unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "ok");
        gateway.join().unwrap();
    }

    /// One byte used to clear the opening deadline for good, so a peer that
    /// spoke once and went quiet pinned a relay slot, two descriptors and a
    /// gateway connection until the sandbox ended.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_connection_that_speaks_once_and_goes_quiet_is_not_permanent() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let transport = ProxyTransport::open_with(
            address,
            linux_sandbox_net::RelayTimeouts {
                opening: Duration::from_secs(10),
                idle: Duration::from_millis(300),
            },
        )
        .unwrap();
        // The gateway accepts and then says nothing either, which is what makes
        // this connection idle in BOTH directions.
        let gateway = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));

        let mut client = UnixStream::connect(transport.endpoint().socket).unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let held = gateway.join().unwrap().expect("the relay reached upstream");
        let deadline = Instant::now() + Duration::from_secs(20);
        while transport.live_relays() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(transport.live_relays(), 1);
        while transport.live_relays() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            transport.live_relays(),
            0,
            "a connection idle in both directions kept its relay"
        );
        drop(held);
        drop(client);
    }

    /// A read deadline alone left the other half unbounded: a peer that stops
    /// READING parks the pump inside its write, where the idle clock was never
    /// consulted, pinning the slot, both threads and the gateway connection.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_peer_that_stops_reading_does_not_park_the_relay() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (client, relay_side) = UnixStream::pair().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = std::net::TcpStream::connect(address).unwrap();
        let (mut gateway, _) = listener.accept().unwrap();

        let relayed = std::thread::spawn(move || {
            linux_sandbox_net::couple(relay_side, upstream, Duration::from_millis(300));
        });

        // Far more than any socket buffer, from a peer the client never reads:
        // the pump fills the client's receive buffer and then blocks in write.
        let flood = std::thread::spawn(move || {
            let payload = vec![b'x'; 64 * 1024];
            for _ in 0..256 {
                if gateway.write_all(&payload).is_err() {
                    return;
                }
            }
        });

        let deadline = Instant::now() + Duration::from_secs(30);
        while !relayed.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            relayed.is_finished(),
            "a client that stopped reading kept the relay for ever"
        );
        relayed.join().unwrap();
        let _ = flood.join();
        drop(client);
    }

    /// `couple` itself, driven directly: the inbound direction ends and the
    /// client keeps its write half open for ever. Before the close of both
    /// halves, the join at the end of `couple` never returned.
    #[test]
    #[cfg(target_os = "linux")]
    fn couple_returns_when_a_keep_alive_client_never_closes_its_write_half() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (mut client, relay_side) = UnixStream::pair().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = std::net::TcpStream::connect(address).unwrap();
        let (mut gateway, _) = listener.accept().unwrap();

        let relayed = std::thread::spawn(move || {
            // A generous idle bound: what ends this call must be the close of
            // both halves, not a timeout.
            linux_sandbox_net::couple(relay_side, upstream, Duration::from_secs(600));
        });

        // The gateway answers and closes, exactly as `Connection: close` means.
        gateway.write_all(b"ok").unwrap();
        drop(gateway);
        let mut answer = Vec::new();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Reads the body, then the end of stream the relay's half close produced.
        let mut chunk = [0u8; 16];
        while let Ok(read) = client.read(&mut chunk) {
            if read == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..read]);
        }
        assert_eq!(answer, b"ok");

        // The client's write half is still open. `couple` must return anyway.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !relayed.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            relayed.is_finished(),
            "couple must not wait on a client that never closes its write half"
        );
        relayed.join().unwrap();
        drop(client);
    }

    /// Why this host cannot run the real sandbox, or `None` when it can.
    ///
    /// The sandbox is `bwrap --unshare-all`, which brings up `lo` inside the new
    /// network namespace. A kernel that denies unprivileged user namespaces
    /// (Ubuntu ships `kernel.apparmor_restrict_unprivileged_userns=1`; `unshare
    /// -Urn true` fails the same way) makes that impossible, so the test has no
    /// sandbox to measure. Probed rather than assumed: a host that CAN do it
    /// always runs the test, which a blanket `#[ignore]` would not.
    #[cfg(target_os = "linux")]
    fn unprivileged_netns_denial() -> Option<String> {
        match std::process::Command::new("/usr/bin/bwrap")
            .args(["--unshare-all", "--dev-bind", "/", "/", "/bin/true"])
            .output()
        {
            Ok(probe) if probe.status.success() => None,
            Ok(probe) => Some(format!(
                "bwrap cannot create a usable private network namespace ({}): {}",
                probe.status,
                String::from_utf8_lossy(&probe.stderr).trim()
            )),
            Err(error) => Some(format!("/usr/bin/bwrap is not runnable: {error}")),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_linux_sandbox_only_reaches_its_proxy() {
        use std::io::{Read, Write};
        if let Some(reason) = unprivileged_netns_denial() {
            eprintln!(
                "SKIP real_linux_sandbox_only_reaches_its_proxy: {reason}. \
                 This host denies unprivileged user/network namespaces \
                 (kernel.apparmor_restrict_unprivileged_userns=1), so the \
                 sandbox under test cannot start at all — nothing is asserted."
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let private = root.path().join("profile");
        for path in [&workspace, &private] {
            std::fs::create_dir(path).unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");
        // No extra read root: the forwarder is this test binary and the policy
        // binds that one file itself.
        let policy = ProcessSandbox::new(&workspace, &private, false, &[], &[]).unwrap();
        let run = |policy: &ProcessSandbox| {
            let argv = policy
                .native_command(
                    &[
                        "/usr/bin/curl".into(),
                        "--max-time".into(),
                        "5".into(),
                        "--silent".into(),
                        "--output".into(),
                        "result".into(),
                        url.clone(),
                    ],
                    &workspace,
                )
                .unwrap();
            run_sandboxed(&argv, &private, &workspace)
        };
        assert!(!run(&policy).status.success());
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        connection
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0; 4096];
                        assert!(
                            connection.read(&mut request).unwrap() > 0,
                            "the relay opened the connection but forwarded no request"
                        );
                        connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
                        return;
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    other => panic!("proxy connection failed: {other:?}"),
                }
            }
        });
        let transport = ProxyTransport::open(address).unwrap();
        let proxied = policy.with_proxy(transport.endpoint()).unwrap();
        let output = run(&proxied);
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(workspace.join("result")).unwrap(), b"ok");
        worker.join().unwrap();
    }
}
