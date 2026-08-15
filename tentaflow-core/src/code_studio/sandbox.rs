// ===== File: code_studio/sandbox.rs — materialising a mount × network profile into a workplace =====
//
// A sandbox is the pair (mount access, network access) the PEP handed out,
// turned into something a process can actually be started in. The pair is the
// identity; `ephemeral` is the third axis and it decides LIFETIME, not access:
// a shared sandbox is one per profile per session, while an ephemeral one holds
// a lease and is destroyed with it, so two test runs on the same profile can
// execute at the same time without ever seeing each other's writes.
//
// Two rules shape everything below.
//
// **`repo/` is never part of a sandbox.** Git metadata stays with the broker
// (§7.3): a worktree handed to an agent is a plain file tree, so `rm -rf .git`,
// a swapped hook or an edited config have no target. The copy path below skips
// the `.git` pointer for the same reason — a copied pointer would aim the copy
// at the reference repository.
//
// **`cow` fails closed.** When a cheap copy cannot be made, or it would exceed
// its size/time budget, or the tree contains a link that would let a write out
// of the copy, the acquisition is REFUSED. There is deliberately no automatic
// degrade to `rw`: that would hand a caller that only ever had copy-on-write
// access a writable working tree, which is a permission being violated rather
// than a warning being emitted. This module therefore has exactly one answer to
// an impossible copy — `CowUnavailable` — and no API that turns it into
// anything else. Escalating to the real worktree would need a capability of its
// own, and this build has none: `pep::Capability` carries no `profile_degrade_rw`
// variant, so nothing anywhere can grant one. The refusal is the end of the
// story until that capability exists.
//
// The two execution modes materialise the same model with different means, and
// the difference is honest rather than hidden:
//
// * `container` — the runtime enforces the profile. `ro` is a read-only bind,
//   `network = none` means no route exists, and the sandbox row carries the
//   container name in `runtime_ref`.
// * `trusted_native` — the OS enforces NOTHING. The process runs as the
//   TentaFlow service user, with the service user's rights on the worktree and
//   the host's network. `cow` is the one real boundary here, and only because
//   the process is pointed at a different directory; `ro` is a promise the
//   caller keeps by withholding write tools (`Lease::tools_read_only`), and
//   `network = none` is not kept at all. `runtime_ref IS NULL` is what tells an
//   auditor which of the two a row describes (§7.6).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tracing::warn;

use super::models::ExecMode;
use super::paths;
use super::pep::{self, MountAccess, NetworkAccess};
use crate::db::DbPool;

/// Full sandbox identity. The PEP decides the (mount, network) pair; the caller
/// adds the lifetime axis, because whether a run wants its own throwaway layer
/// is a property of the RUN, not of the permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxProfile {
    pub mount: MountAccess,
    pub network: NetworkAccess,
    pub ephemeral: bool,
}

impl SandboxProfile {
    pub fn new(mount: MountAccess, network: NetworkAccess, ephemeral: bool) -> Self {
        Self {
            mount,
            network,
            ephemeral,
        }
    }

    /// Profile of an authorized call. `Decision::Allow` always names the pair,
    /// so an allowance can never reach the executor without one.
    pub fn from_decision(allowed: pep::SandboxProfile, ephemeral: bool) -> Self {
        Self {
            mount: allowed.mount,
            network: allowed.network,
            ephemeral,
        }
    }
}

pub fn mount_slug(mount: MountAccess) -> &'static str {
    match mount {
        MountAccess::ReadOnly => "ro",
        MountAccess::CopyOnWrite => "cow",
        MountAccess::ReadWrite => "rw",
    }
}

pub fn network_slug(network: NetworkAccess) -> &'static str {
    match network {
        NetworkAccess::None => "none",
        NetworkAccess::Gateway => "gateway",
    }
}

/// Why an acquisition failed. `CowUnavailable` is a distinct variant rather
/// than a string so a caller cannot accidentally treat it as a generic error
/// and retry on the real worktree.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// A copy-on-write workplace could not be built. There is no second answer:
    /// the operation does not run. Nothing in this build can escalate it to the
    /// real worktree, because the capability that would authorize that does not
    /// exist (see the module header).
    #[error("copy-on-write sandbox unavailable: {reason}")]
    CowUnavailable { reason: String },
    /// The session already holds the shared sandbox of this profile. Shared
    /// sandboxes are one per profile by design (§7.2); a caller that needs a
    /// second concurrent workplace asks for an ephemeral one.
    #[error("session {session_id} already holds the shared {mount}+{network} sandbox")]
    SharedProfileBusy {
        session_id: String,
        mount: &'static str,
        network: &'static str,
    },
    /// The node cannot provide what the profile promises. Refusing is the point:
    /// a `gateway` profile started on the default bridge would have unfiltered
    /// internet access while claiming to be filtered.
    #[error("sandbox runtime unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SandboxError {
    fn cow(reason: impl Into<String>) -> Self {
        SandboxError::CowUnavailable {
            reason: reason.into(),
        }
    }
}

/// Ceiling on building a copy-on-write workplace. A repository large enough to
/// blow through this is exactly the case where a silent full copy would stall a
/// session for minutes, so the budget is part of the fail-closed contract.
#[derive(Debug, Clone, Copy)]
pub struct CowBudget {
    pub max_bytes: u64,
    pub max_files: u64,
    pub max_duration: Duration,
}

impl Default for CowBudget {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024 * 1024,
            max_files: 200_000,
            max_duration: Duration::from_secs(120),
        }
    }
}

/// Which container runtime the node manages. Both speak the same CLI surface
/// for what a sandbox needs, and the CLI is what the executor drives when it
/// starts a process inside an already-running sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
}

impl ContainerRuntime {
    pub fn binary(self) -> &'static str {
        match self {
            ContainerRuntime::Docker => "docker",
            ContainerRuntime::Podman => "podman",
        }
    }
}

/// Everything a `container` workspace needs beyond the profile itself.
#[derive(Debug, Clone)]
pub struct ContainerConfig {
    pub runtime: ContainerRuntime,
    pub image: String,
    /// Name of the runtime network the egress gateway sits on. It must be an
    /// INTERNAL network (no default route), because that is the whole mechanism
    /// behind `network_access = gateway`. Absent = a gateway profile is refused
    /// rather than quietly started on the default bridge.
    pub egress_network: Option<String>,
    /// Non-root user the sandbox process runs as, `uid:gid` or a name that
    /// exists in the image.
    pub user: String,
}

/// Where a process of this lease has to be started. The executor never derives
/// this itself — a sandbox that decided "container" must not be bypassed by a
/// caller that decides to run locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecTarget {
    /// Runs on the host as the TentaFlow service user, in `cwd`.
    Local { cwd: PathBuf },
    /// Runs inside an already-started container.
    Container {
        runtime: ContainerRuntime,
        name: String,
        workdir: PathBuf,
        user: String,
    },
}

/// Mount point of the worktree inside a container. Fixed, so nothing in the
/// image can depend on a host path.
#[cfg(feature = "docker")]
const CONTAINER_WORKDIR: &str = "/workspace";
#[cfg(feature = "docker")]
const CONTAINER_TOOLCHAIN_BASE: &str = "/toolchain/base";
#[cfg(feature = "docker")]
const CONTAINER_TOOLCHAIN_OVERLAY: &str = "/toolchain/ov";

/// A copy-on-write workplace, described so that releasing it undoes exactly
/// what was done. `mounted` is set when the layer is a host overlayfs, which
/// has to be unmounted before its directory tree can be removed; a reflink or
/// full copy has nothing to unmount.
#[derive(Debug, Clone)]
struct CowLayer {
    root: PathBuf,
    mounted: Option<PathBuf>,
}

/// A held sandbox. Dropping one without releasing it leaks the upper layer and
/// (in container mode) a running container, so the drop path complains loudly
/// instead of hiding it.
#[derive(Debug)]
pub struct Lease {
    pub sandbox_id: String,
    /// `None` for the shared sandbox of a profile, `Some` for an ephemeral one.
    pub lease_id: Option<String>,
    pub owner_run_id: Option<String>,
    pub session_id: String,
    pub profile: SandboxProfile,
    /// The caller must not hand this lease a write tool.
    ///
    /// In `trusted_native` with `mount = ro` the process keeps the OS rights of
    /// the service user: a write to the worktree SUCCEEDS. Nothing in this
    /// module can stop it, so the flag is not a guarantee — it is an
    /// instruction to whoever assembles the tool list, and it is worth exactly
    /// as much as that caller's compliance with it.
    pub tools_read_only: bool,
    pub toolchain_base: PathBuf,
    pub toolchain_overlay: PathBuf,
    pub tmp_dir: PathBuf,
    pub home_dir: PathBuf,
    target: ExecTarget,
    layer: Option<CowLayer>,
    /// Everything this sandbox owns on the host, in one directory.
    workplace: PathBuf,
    container_name: Option<String>,
    runtime: Option<ContainerRuntime>,
    released: bool,
}

impl Lease {
    pub fn target(&self) -> &ExecTarget {
        &self.target
    }

    /// Directory the copy-on-write work happens in, when there is one. The
    /// tests are its only readers: it is how "the build wrote into the layer
    /// and not into the worktree" is asserted at all.
    pub fn layer_dir(&self) -> Option<&Path> {
        self.layer.as_ref().map(|layer| layer.root.as_path())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.released {
            warn!(
                sandbox_id = %self.sandbox_id,
                session_id = %self.session_id,
                "sandbox lease dropped without release: upper layer and container are leaked"
            );
        }
    }
}

/// Sandboxes of ONE workspace. Holds the workspace root explicitly so the same
/// code serves production (root derived from the id) and tests (root in a
/// temporary directory) — the derivation is the only difference.
pub struct SandboxManager {
    root: PathBuf,
    exec_mode: ExecMode,
    container: Option<ContainerConfig>,
    cow_budget: CowBudget,
}

impl SandboxManager {
    /// Manager of a real workspace. The root goes through the path guard, so no
    /// caller can aim a sandbox at an arbitrary directory.
    pub fn for_workspace(
        workspace_id: &str,
        exec_mode: ExecMode,
        container: Option<ContainerConfig>,
    ) -> Result<Self> {
        Ok(Self {
            root: paths::workspace_dir(workspace_id)?,
            exec_mode,
            container,
            cow_budget: CowBudget::default(),
        })
    }

    /// Manager over an explicit workspace root.
    pub fn at(
        root: impl Into<PathBuf>,
        exec_mode: ExecMode,
        container: Option<ContainerConfig>,
    ) -> Self {
        Self {
            root: root.into(),
            exec_mode,
            container,
            cow_budget: CowBudget::default(),
        }
    }

    /// Production always runs on `CowBudget::default()`; this exists so the
    /// fail-closed path can be driven with a budget a test can actually exceed
    /// without building an eight-gigabyte fixture.
    #[cfg(test)]
    fn with_cow_budget(mut self, budget: CowBudget) -> Self {
        self.cow_budget = budget;
        self
    }

    pub fn worktree_dir(&self, session_id: &str) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        Ok(self.root.join("worktrees").join(session_id))
    }

    fn toolchain_base(&self) -> PathBuf {
        self.root.join("toolchain-cache").join("base")
    }

    /// Per-session toolchain overlay. It outlives an ephemeral sandbox on
    /// purpose: a build cache that is thrown away with every run is not a cache.
    /// A SHARED writable cache across sessions deliberately does not exist —
    /// one session could poison every other session's toolchain.
    fn toolchain_overlay(&self, session_id: &str) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        Ok(self
            .root
            .join("toolchain-cache")
            .join("ov")
            .join(session_id))
    }

    fn session_tmp(&self, session_id: &str) -> Result<PathBuf> {
        paths::validate_session_id(session_id)?;
        Ok(self.root.join("tmp").join(session_id))
    }

    /// Takes a sandbox of `profile` for `session_id`.
    ///
    /// The database row is inserted BEFORE the workplace is built, so the unique
    /// index on shared sandboxes — not a check-then-act race in this function —
    /// is what arbitrates two callers asking for the same shared profile. A
    /// failure to build removes the row again, because a sandbox that never
    /// existed must not keep a profile occupied.
    pub fn acquire(
        &self,
        pool: &DbPool,
        session_id: &str,
        profile: SandboxProfile,
        owner_run_id: Option<&str>,
    ) -> std::result::Result<Lease, SandboxError> {
        paths::validate_session_id(session_id).map_err(SandboxError::Other)?;
        let worktree = self.worktree_dir(session_id).map_err(SandboxError::Other)?;
        if !worktree.is_dir() {
            return Err(SandboxError::Other(anyhow!(
                "session {session_id} has no worktree at {}",
                worktree.display()
            )));
        }

        // Resolved before anything is created: a failure here must not happen
        // between "the workplace exists" and "the caller holds it".
        let toolchain_overlay = self
            .toolchain_overlay(session_id)
            .map_err(SandboxError::Other)?;

        let sandbox_id = uuid::Uuid::new_v4().to_string();
        let lease_id = profile
            .ephemeral
            .then(|| uuid::Uuid::new_v4().to_string());
        self.insert_row(pool, &sandbox_id, session_id, profile, &lease_id, owner_run_id)?;

        let runtime = self.container.as_ref().map(|c| c.runtime);
        let workplace = self
            .workplace_dir(session_id, &sandbox_id)
            .map_err(SandboxError::Other)?;
        let built = match self.materialise(session_id, &sandbox_id, &workplace, profile, &worktree) {
            Ok(built) => built,
            Err(e) => {
                // Nothing was handed out, so nothing of it may stay on disk —
                // including the half-built directories `materialise` created
                // before it gave up.
                remove_workplace(&workplace);
                self.delete_row(pool, &sandbox_id);
                return Err(e);
            }
        };

        // A sandbox that cannot be marked ready is a sandbox nobody holds, so
        // it has to come down with the row. Returning here without the teardown
        // would leave the container running, the full copy on disk and the row
        // stuck at 'starting' — and the partial unique index counts 'starting',
        // so the shared profile would stay occupied for the rest of the session.
        if let Err(e) = self.mark_ready(pool, &sandbox_id, built.runtime_ref.as_deref()) {
            match tear_down(
                runtime,
                built.runtime_ref.as_deref(),
                built.layer.as_ref(),
                &workplace,
            ) {
                Ok(()) => self.delete_row(pool, &sandbox_id),
                // The workplace is still on this node; the row is the only
                // record of it, so it stays behind as 'failed' instead of
                // being deleted. It keeps the profile occupied on purpose —
                // handing the same profile out again would put a second
                // sandbox on top of one that was never removed.
                Err(teardown) => {
                    warn!(
                        sandbox_id = %sandbox_id,
                        "sandbox could not be torn down after a failed start: {teardown:#}"
                    );
                    self.close_row(pool, &sandbox_id, "failed");
                }
            }
            return Err(SandboxError::Other(e));
        }

        Ok(Lease {
            sandbox_id,
            lease_id,
            owner_run_id: owner_run_id.map(str::to_string),
            session_id: session_id.to_string(),
            profile,
            tools_read_only: built.tools_read_only,
            toolchain_base: self.toolchain_base(),
            toolchain_overlay,
            tmp_dir: built.tmp_dir,
            home_dir: built.home_dir,
            target: built.target,
            layer: built.layer,
            workplace,
            container_name: built.runtime_ref,
            runtime,
            released: false,
        })
    }

    /// Releases a lease: stops the container, destroys the upper layer and
    /// closes the row. An ephemeral sandbox loses everything a run wrote, which
    /// is the entire point — a second test run must not inherit the first one's
    /// `target/` or `node_modules`.
    ///
    /// The row is only closed as 'stopped' when the teardown actually
    /// succeeded. A container that refused to die or a layer that could not be
    /// removed is still there, and a row claiming otherwise would both lie to
    /// the operator and free a profile whose workplace is still running.
    pub fn release(&self, pool: &DbPool, mut lease: Lease) -> Result<()> {
        lease.released = true;
        let workplace = lease.workplace.clone();
        let outcome = tear_down(
            lease.runtime,
            lease.container_name.as_deref(),
            lease.layer.take().as_ref(),
            &workplace,
        );
        let state = if outcome.is_ok() { "stopped" } else { "failed" };
        self.close_row(pool, &lease.sandbox_id, state);
        outcome
    }

    /// Closes sandboxes left behind by a crash. Their upper layers are gone with
    /// the temporary directory sweep, and their rows must not keep a shared
    /// profile occupied for a session that is being resumed.
    pub fn reconcile_after_restart(&self, pool: &DbPool, session_id: &str) -> Result<usize> {
        let conn = pool.write().map_err(|e| anyhow!("workspace db write: {e}"))?;
        let closed = conn.execute(
            "UPDATE sandboxes SET state='stopped', stopped_at=datetime('now'), lease_id=NULL \
             WHERE session_id = ?1 AND state != 'stopped'",
            rusqlite::params![session_id],
        )?;
        drop(conn);
        let stale = self.session_tmp(session_id)?.join("sandbox");
        if stale.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&stale) {
                warn!(session_id, "stale sandbox layers not removed: {e}");
            }
        }
        Ok(closed)
    }

    fn insert_row(
        &self,
        pool: &DbPool,
        sandbox_id: &str,
        session_id: &str,
        profile: SandboxProfile,
        lease_id: &Option<String>,
        owner_run_id: Option<&str>,
    ) -> std::result::Result<(), SandboxError> {
        let conn = pool
            .write()
            .map_err(|e| SandboxError::Other(anyhow!("workspace db write: {e}")))?;
        let inserted = conn.execute(
            "INSERT INTO sandboxes \
              (id, session_id, mount_access, network_access, lease_id, owner_run_id, state, \
               ephemeral, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'starting', ?7, datetime('now'))",
            rusqlite::params![
                sandbox_id,
                session_id,
                mount_slug(profile.mount),
                network_slug(profile.network),
                lease_id,
                owner_run_id,
                i64::from(profile.ephemeral),
            ],
        );
        match inserted {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation && !profile.ephemeral =>
            {
                Err(SandboxError::SharedProfileBusy {
                    session_id: session_id.to_string(),
                    mount: mount_slug(profile.mount),
                    network: network_slug(profile.network),
                })
            }
            Err(e) => Err(SandboxError::Other(anyhow!("sandbox row: {e}"))),
        }
    }

    fn mark_ready(&self, pool: &DbPool, sandbox_id: &str, runtime_ref: Option<&str>) -> Result<()> {
        let conn = pool.write().map_err(|e| anyhow!("workspace db write: {e}"))?;
        conn.execute(
            "UPDATE sandboxes SET state='ready', runtime_ref=?2 WHERE id = ?1",
            rusqlite::params![sandbox_id, runtime_ref],
        )?;
        Ok(())
    }

    /// Closes a row in a terminal state. 'stopped' frees the shared profile
    /// (the partial unique index ignores it); 'failed' does not, because a
    /// workplace that could not be removed is still occupying the node.
    fn close_row(&self, pool: &DbPool, sandbox_id: &str, state: &str) {
        let result = pool
            .write()
            .map_err(|e| anyhow!("workspace db write: {e}"))
            .and_then(|conn| {
                conn.execute(
                    "UPDATE sandboxes SET state=?2, stopped_at=datetime('now'), lease_id=NULL \
                     WHERE id = ?1",
                    rusqlite::params![sandbox_id, state],
                )
                .map_err(|e| anyhow!("close sandbox row: {e}"))
            });
        if let Err(e) = result {
            warn!(sandbox_id, state, "sandbox row not closed: {e:#}");
        }
    }

    fn delete_row(&self, pool: &DbPool, sandbox_id: &str) {
        match pool.write() {
            Ok(conn) => {
                if let Err(e) = conn.execute(
                    "DELETE FROM sandboxes WHERE id = ?1",
                    rusqlite::params![sandbox_id],
                ) {
                    warn!(sandbox_id, "sandbox row of a failed acquisition kept: {e}");
                }
            }
            Err(e) => warn!(sandbox_id, "sandbox row of a failed acquisition kept: {e}"),
        }
    }

    /// Everything one sandbox writes on the host: its copy-on-write layer, its
    /// `TMPDIR` and its `HOME`. One directory, so removing the sandbox is one
    /// removal and cannot leave two of the three behind.
    fn workplace_dir(&self, session_id: &str, sandbox_id: &str) -> Result<PathBuf> {
        Ok(self
            .session_tmp(session_id)?
            .join("sandbox")
            .join(sandbox_id))
    }

    fn materialise(
        &self,
        session_id: &str,
        sandbox_id: &str,
        workplace: &Path,
        profile: SandboxProfile,
        worktree: &Path,
    ) -> std::result::Result<Built, SandboxError> {
        let overlay = self
            .toolchain_overlay(session_id)
            .map_err(SandboxError::Other)?;
        let base = self.toolchain_base();
        for dir in [&base, &overlay] {
            std::fs::create_dir_all(dir)
                .map_err(|e| SandboxError::Other(anyhow!("toolchain cache {}: {e}", dir.display())))?;
        }
        std::fs::create_dir_all(workplace.join("tmp"))
            .map_err(|e| SandboxError::Other(anyhow!("sandbox tmp: {e}")))?;
        std::fs::create_dir_all(workplace.join("home"))
            .map_err(|e| SandboxError::Other(anyhow!("sandbox home: {e}")))?;

        match self.exec_mode {
            ExecMode::TrustedNative => {
                self.materialise_native(profile, worktree, workplace, &base, &overlay)
            }
            ExecMode::Container => self.materialise_container(
                sandbox_id, profile, worktree, workplace, &base, &overlay,
            ),
        }
    }

    /// `trusted_native`: the same conceptual model, enforced by nothing at the
    /// OS level — except for `cow`, where the copy is a real boundary because
    /// the process is pointed at a different directory.
    ///
    /// `profile.network` is deliberately not consulted: this mode has no way to
    /// take a route away from a process running as the service user. The
    /// sandbox row records the profile that was ASKED for and leaves
    /// `runtime_ref` NULL, which is how an auditor tells "the runtime removed
    /// the route" from "nobody could" (§7.6).
    fn materialise_native(
        &self,
        profile: SandboxProfile,
        worktree: &Path,
        tmp: &Path,
        _base: &Path,
        _overlay: &Path,
    ) -> std::result::Result<Built, SandboxError> {
        let (cwd, layer, tools_read_only) = match profile.mount {
            MountAccess::ReadWrite => (worktree.to_path_buf(), None, false),
            // The command runs in the worktree and the OS does not stop it from
            // writing there. What stops it is that the caller holding this lease
            // gets no write tools. Stated, not hidden (§7.2).
            MountAccess::ReadOnly => (worktree.to_path_buf(), None, true),
            MountAccess::CopyOnWrite => {
                let (dir, layer) = self.build_cow_layer(tmp, worktree, false)?;
                (dir, Some(layer), false)
            }
        };
        Ok(Built {
            target: ExecTarget::Local { cwd },
            layer,
            tools_read_only,
            runtime_ref: None,
            tmp_dir: tmp.join("tmp"),
            home_dir: tmp.join("home"),
        })
    }

    /// `container`: the profile is enforced by the runtime. `ro` is a read-only
    /// bind, `cow` is an overlay (or a reflink copy where overlayfs is not
    /// mountable), `rw` is a read-write bind, and `network = none` means the
    /// container has no route at all rather than a filtered one.
    #[cfg(feature = "docker")]
    fn materialise_container(
        &self,
        sandbox_id: &str,
        profile: SandboxProfile,
        worktree: &Path,
        tmp: &Path,
        base: &Path,
        overlay: &Path,
    ) -> std::result::Result<Built, SandboxError> {
        let config = self.container.as_ref().ok_or_else(|| {
            SandboxError::RuntimeUnavailable("workspace has no container configuration".into())
        })?;
        let network = container_network(profile.network, config)?;

        let (source, layer) = match profile.mount {
            MountAccess::ReadOnly | MountAccess::ReadWrite => (worktree.to_path_buf(), None),
            MountAccess::CopyOnWrite => {
                let (dir, layer) = self.build_cow_layer(tmp, worktree, true)?;
                (dir, Some(layer))
            }
        };
        let bind_mode = container_bind_mode(profile.mount);

        let limits = crate::deploy::docker::SandboxLimits::code_session(network);
        let name = format!("tentaflow-cs-{sandbox_id}");
        let mut argv: Vec<String> = vec![
            config.runtime.binary().to_string(),
            "run".into(),
            "--detach".into(),
            "--name".into(),
            name.clone(),
            "--network".into(),
            limits.network_mode.clone(),
            "--user".into(),
            config.user.clone(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--read-only".into(),
            "--memory".into(),
            limits.memory_bytes.to_string(),
            "--cpus".into(),
            format!("{:.2}", limits.nano_cpus as f64 / 1_000_000_000.0),
            "--pids-limit".into(),
            limits.pids_limit.to_string(),
            "--tmpfs".into(),
            format!("/tmp:rw,noexec,nosuid,nodev,size={}", limits.tmpfs_bytes),
            "--workdir".into(),
            CONTAINER_WORKDIR.into(),
        ];
        // The docker socket is deliberately absent from this list: a mounted
        // socket is a root shell on the host, which would make every limit above
        // decoration.
        for (host, guest, mode) in [
            (source.as_path(), CONTAINER_WORKDIR, bind_mode),
            (base, CONTAINER_TOOLCHAIN_BASE, "ro"),
            (overlay, CONTAINER_TOOLCHAIN_OVERLAY, "rw"),
        ] {
            let host = host.to_str().ok_or_else(|| {
                SandboxError::Other(anyhow!("sandbox path is not valid UTF-8: {}", host.display()))
            })?;
            argv.push("--volume".into());
            argv.push(format!("{host}:{guest}:{mode}"));
        }
        argv.push(config.image.clone());
        // Keeps the sandbox alive between commands; every command is then a
        // separate exec into this container.
        argv.push("sleep".into());
        argv.push("infinity".into());

        run_runtime_command(&argv).map_err(|e| {
            if let Some(layer) = &layer {
                let _ = destroy_layer(layer);
            }
            SandboxError::RuntimeUnavailable(format!("{e:#}"))
        })?;

        Ok(Built {
            target: ExecTarget::Container {
                runtime: config.runtime,
                name: name.clone(),
                workdir: PathBuf::from(CONTAINER_WORKDIR),
                user: config.user.clone(),
            },
            layer,
            tools_read_only: matches!(profile.mount, MountAccess::ReadOnly),
            runtime_ref: Some(name),
            tmp_dir: tmp.join("tmp"),
            home_dir: tmp.join("home"),
        })
    }

    #[cfg(not(feature = "docker"))]
    fn materialise_container(
        &self,
        _sandbox_id: &str,
        _profile: SandboxProfile,
        _worktree: &Path,
        _tmp: &Path,
        _base: &Path,
        _overlay: &Path,
    ) -> std::result::Result<Built, SandboxError> {
        Err(SandboxError::RuntimeUnavailable(
            "this build has no container management (feature `docker` is off)".into(),
        ))
    }

    /// The one upper layer builder of both execution modes.
    ///
    /// `allow_overlay` is true only for `container`, where the merged directory
    /// is what gets bound into the container and mounting it is a privilege the
    /// runtime host may actually have. A `trusted_native` sandbox runs as the
    /// service user, which cannot mount anything, so it goes straight to the
    /// copy instead of failing a mount on every single acquisition.
    ///
    /// Where overlayfs is not available (no privilege, a filesystem that
    /// refuses to be a lower dir, a path with a comma in it) the fallback is a
    /// reflink copy, which keeps the SAME guarantee — the worktree is not
    /// written — rather than degrading to it. When the copy is impossible too,
    /// the half-built layer is removed and the acquisition is refused.
    fn build_cow_layer(
        &self,
        tmp: &Path,
        worktree: &Path,
        allow_overlay: bool,
    ) -> std::result::Result<(PathBuf, CowLayer), SandboxError> {
        let root = tmp.join("cow");
        let overlay_reason = if allow_overlay {
            let merged = root.join("merged");
            let upper = root.join("upper");
            let work = root.join("ovwork");
            for dir in [&merged, &upper, &work] {
                std::fs::create_dir_all(dir)
                    .map_err(|e| SandboxError::cow(format!("create overlay directory: {e}")))?;
            }
            match mount_overlay(worktree, &upper, &work, &merged) {
                Ok(()) => {
                    return Ok((
                        merged.clone(),
                        CowLayer {
                            root,
                            mounted: Some(merged),
                        },
                    ))
                }
                Err(reason) => format!("overlayfs: {reason}; "),
            }
        } else {
            String::new()
        };

        let copy = root.join("work");
        std::fs::create_dir_all(&copy)
            .map_err(|e| SandboxError::cow(format!("{overlay_reason}create copy directory: {e}")))?;
        match copy_tree(worktree, &copy, &self.cow_budget) {
            Ok(()) => Ok((
                copy,
                CowLayer {
                    root,
                    mounted: None,
                },
            )),
            Err(copy_reason) => {
                // Fail closed: nothing half-built survives and the caller gets
                // a refusal, never the real worktree.
                let _ = std::fs::remove_dir_all(&root);
                Err(SandboxError::cow(format!("{overlay_reason}{copy_reason}")))
            }
        }
    }
}

/// Runtime network a container profile must be started on.
///
/// `none` is the runtime's own "no route at all". `gateway` REQUIRES the
/// workspace's internal egress network and is refused without one: the default
/// bridge would be unfiltered internet under a profile that claims to be
/// filtered (§7.6). Kept out of `materialise_container` so the decision is
/// testable in a build that has no container runtime at all.
fn container_network(
    network: NetworkAccess,
    config: &ContainerConfig,
) -> std::result::Result<String, SandboxError> {
    match network {
        NetworkAccess::None => Ok("none".to_string()),
        NetworkAccess::Gateway => config.egress_network.clone().ok_or_else(|| {
            SandboxError::RuntimeUnavailable(
                "network_access=gateway needs the workspace egress network; refusing to start on \
                 the default bridge"
                    .into(),
            )
        }),
    }
}

/// Bind mode of the worktree mount. Only `ro` gets a read-only bind; `cow`
/// binds its own layer read-write, which is what makes writes land there
/// instead of in the worktree.
fn container_bind_mode(mount: MountAccess) -> &'static str {
    match mount {
        MountAccess::ReadOnly => "ro",
        MountAccess::CopyOnWrite | MountAccess::ReadWrite => "rw",
    }
}

/// Undoes what `materialise` built. Both ends of a sandbox's life go through
/// it — the failure path of `acquire` and `release` — so a workplace is never
/// removed one way in one place and a different way in another.
fn tear_down(
    runtime: Option<ContainerRuntime>,
    container: Option<&str>,
    layer: Option<&CowLayer>,
    workplace: &Path,
) -> Result<()> {
    let mut failure: Option<anyhow::Error> = None;
    if let (Some(runtime), Some(name)) = (runtime, container) {
        if let Err(e) = remove_container(runtime, name) {
            failure = Some(e);
        }
    }
    // The overlay has to come off before the tree beneath it can be removed.
    if let Some(layer) = layer {
        if let Err(e) = destroy_layer(layer) {
            failure = Some(e);
        }
    }
    // The layer is only part of what a sandbox wrote: its `TMPDIR` and its
    // `HOME` live in the same workplace, and removing one of the three is how a
    // session used to accumulate dead directories until the next restart swept
    // them.
    if let Err(e) = remove_workplace_checked(workplace) {
        failure = Some(e);
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn remove_workplace_checked(workplace: &Path) -> Result<()> {
    match std::fs::remove_dir_all(workplace) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!(
            "remove sandbox workplace {}: {e}",
            workplace.display()
        )),
    }
}

/// Removes the workplace of a sandbox that was never handed out. The failure is
/// logged rather than returned: the caller is already reporting why the
/// acquisition was refused, and that reason is the one worth surfacing.
fn remove_workplace(workplace: &Path) {
    if let Err(e) = remove_workplace_checked(workplace) {
        warn!("workplace of a refused sandbox not removed: {e:#}");
    }
}

/// Result of materialising a profile.
struct Built {
    target: ExecTarget,
    layer: Option<CowLayer>,
    tools_read_only: bool,
    runtime_ref: Option<String>,
    tmp_dir: PathBuf,
    home_dir: PathBuf,
}

fn destroy_layer(layer: &CowLayer) -> Result<()> {
    if let Some(merged) = &layer.mounted {
        unmount_overlay(merged)?;
    }
    std::fs::remove_dir_all(&layer.root)
        .map_err(|e| anyhow!("remove sandbox layer {}: {e}", layer.root.display()))
}

#[cfg(target_os = "linux")]
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<(), String> {
    use std::ffi::CString;

    let mut options = String::new();
    for (key, path) in [("lowerdir", lower), ("upperdir", upper), ("workdir", work)] {
        let text = path
            .to_str()
            .ok_or_else(|| "overlay path is not valid UTF-8".to_string())?;
        if text.contains([',', ':', '"', '\\']) {
            return Err("overlay path contains a character the mount options cannot quote".into());
        }
        if !options.is_empty() {
            options.push(',');
        }
        options.push_str(key);
        options.push('=');
        options.push_str(text);
    }
    let fstype = CString::new("overlay").map_err(|e| e.to_string())?;
    let target = CString::new(merged.as_os_str().to_string_lossy().as_bytes())
        .map_err(|e| e.to_string())?;
    let data = CString::new(options).map_err(|e| e.to_string())?;
    let rc = unsafe {
        libc::mount(
            fstype.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            data.as_ptr() as *const libc::c_void,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "mount overlay: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn mount_overlay(_lower: &Path, _upper: &Path, _work: &Path, _merged: &Path) -> Result<(), String> {
    Err("overlayfs exists only on Linux".into())
}

#[cfg(target_os = "linux")]
fn unmount_overlay(merged: &Path) -> Result<()> {
    use std::ffi::CString;
    let target = CString::new(merged.as_os_str().to_string_lossy().as_bytes())
        .map_err(|e| anyhow!("overlay path: {e}"))?;
    // MNT_DETACH so a process still holding a file in the layer delays the
    // teardown instead of blocking the release.
    let rc = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    if rc == 0 {
        Ok(())
    } else {
        Err(anyhow!("umount overlay: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(not(target_os = "linux"))]
fn unmount_overlay(_merged: &Path) -> Result<()> {
    Ok(())
}

#[cfg(feature = "docker")]
fn run_runtime_command(argv: &[String]) -> Result<()> {
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| anyhow!("{} not runnable: {e}", argv[0]))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{} failed ({}): {}",
        argv[0],
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn remove_container(runtime: ContainerRuntime, name: &str) -> Result<()> {
    let output = std::process::Command::new(runtime.binary())
        .args(["rm", "--force", name])
        .output()
        .map_err(|e| anyhow!("{} not runnable: {e}", runtime.binary()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{} rm {name} failed: {}",
        runtime.binary(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Copies a worktree into `dst`, cheaply where the filesystem allows it.
///
/// Reflink first (`FICLONE` on Btrfs/XFS, `clonefile` on APFS), full copy
/// otherwise. Returns the reason as a string on refusal, because every caller
/// turns it into `CowUnavailable` and nothing else.
///
/// `.git` is skipped: in a worktree it is a POINTER into the reference
/// repository, and a copy of that pointer would aim git at the real repo from
/// inside a sandbox that is supposed to have no git metadata at all (§7.3).
///
/// A symbolic link that leaves the tree is a REFUSAL, not a copy — see
/// `copy_symlink`.
fn copy_tree(src: &Path, dst: &Path, budget: &CowBudget) -> Result<(), String> {
    let started = Instant::now();
    let mut bytes = 0u64;
    let mut files = 0u64;
    copy_dir(src, src, dst, budget, started, &mut bytes, &mut files)
}

fn copy_dir(
    root: &Path,
    src: &Path,
    dst: &Path,
    budget: &CowBudget,
    started: Instant,
    bytes: &mut u64,
    files: &mut u64,
) -> Result<(), String> {
    let entries = std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", src.display()))?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(".git") {
            continue;
        }
        if started.elapsed() > budget.max_duration {
            return Err(format!(
                "copy exceeded its {} s budget",
                budget.max_duration.as_secs()
            ));
        }
        let from = entry.path();
        let to = dst.join(&name);
        let meta = entry
            .metadata()
            .map_err(|e| format!("stat {}: {e}", from.display()))?;
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            copy_symlink(root, &from, &to)?;
            *files += 1;
        } else if file_type.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("create {}: {e}", to.display()))?;
            copy_dir(root, &from, &to, budget, started, bytes, files)?;
        } else if file_type.is_file() {
            *files += 1;
            *bytes += meta.len();
            if *files > budget.max_files {
                return Err(format!("copy exceeded its {} file budget", budget.max_files));
            }
            if *bytes > budget.max_bytes {
                return Err(format!(
                    "copy exceeded its {} MiB budget",
                    budget.max_bytes / (1024 * 1024)
                ));
            }
            copy_file(&from, &to)?;
        }
        // Sockets, fifos and devices are not copied: nothing in a source tree
        // needs them and recreating one would be a capability, not a file.
    }
    Ok(())
}

/// Reproduces one symbolic link inside the copy — but only when following it
/// inside the copy keeps the process inside the copy.
///
/// Reproducing a link verbatim is how a copy-on-write sandbox loses its entire
/// point: `x -> ../../worktrees/s-1` (or any absolute path) copied as a link
/// means a build writing through `<layer>/x/...` writes into the REAL worktree,
/// with no refusal and nobody's consent. Materialising the outside content
/// instead would be just as wrong in the other direction — it would import
/// files the profile never granted, and silently turn a link into a divergent
/// copy.
///
/// So an escaping link is a refusal, which is the same answer this module gives
/// to every other copy it cannot make honestly. A link that stays inside the
/// tree is reproduced as written, because inside the copy it resolves inside
/// the copy.
fn copy_symlink(root: &Path, from: &Path, to: &Path) -> Result<(), String> {
    let target = std::fs::read_link(from).map_err(|e| format!("readlink {}: {e}", from.display()))?;
    if !link_stays_inside(root, from, &target) {
        return Err(format!(
            "{} is a symbolic link to {}, which is outside the tree being copied; a copy-on-write \
             workplace cannot contain a way back out of itself",
            from.display(),
            target.display()
        ));
    }
    create_symlink(&target, from, to)
}

/// Whether following `target` from the directory holding `link` stays under
/// `root`.
///
/// Resolved LEXICALLY, and that is the correct resolution here: the copy
/// reproduces the link text, so what decides where a write lands is where that
/// text points once it is followed inside the copy — not what the original link
/// happens to resolve to today.
fn link_stays_inside(root: &Path, link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let Some(parent) = link.parent() else {
        return false;
    };
    let Ok(relative) = parent.strip_prefix(root) else {
        return false;
    };
    let mut depth: usize = relative.components().count();
    for component in target.components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => match depth.checked_sub(1) {
                Some(next) => depth = next,
                None => return false,
            },
            // A root or a drive prefix in a relative-looking path is the same
            // escape by another spelling.
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(unix)]
fn create_symlink(target: &Path, _from: &Path, to: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, to).map_err(|e| format!("symlink {}: {e}", to.display()))
}

#[cfg(not(unix))]
fn create_symlink(target: &Path, from: &Path, to: &Path) -> Result<(), String> {
    // Creating a symlink on Windows needs a privilege the service user may not
    // hold. The target is known to stay inside the tree being copied, so
    // copying the content it points at reproduces the same bytes at the same
    // place without needing the privilege. It is resolved against the SOURCE
    // tree, which is complete, rather than against the copy, which is still
    // being built.
    let source = from
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", from.display()))?
        .join(target);
    let meta = std::fs::metadata(&source).map_err(|e| format!("stat {}: {e}", source.display()))?;
    if meta.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))
    } else {
        std::fs::copy(&source, to)
            .map(|_| ())
            .map_err(|e| format!("copy {}: {e}", source.display()))
    }
}

/// One file, reflinked when the filesystem supports it. A failed clone is not
/// an error — it is the normal answer on ext4, NTFS or across filesystems — so
/// it falls through to a byte copy.
fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if reflink(from, to) {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))
}

#[cfg(target_os = "linux")]
fn reflink(from: &Path, to: &Path) -> bool {
    use std::os::unix::io::AsRawFd;

    let Ok(src) = std::fs::File::open(from) else {
        return false;
    };
    let Ok(dst) = std::fs::File::create(to) else {
        return false;
    };
    let rc = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE as _, src.as_raw_fd()) };
    if rc == 0 {
        // A clone copies content, not mode. Without this the executable bit of
        // every script in the tree would be lost, and a build in the copy would
        // fail in a way that looks like a broken repository.
        if let Ok(meta) = src.metadata() {
            let _ = dst.set_permissions(meta.permissions());
        }
        return true;
    }
    // The destination was created empty by the failed attempt; the byte copy
    // that follows overwrites it, so nothing is left behind.
    false
}

#[cfg(target_os = "macos")]
fn reflink(from: &Path, to: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let (Ok(src), Ok(dst)) = (
        CString::new(from.as_os_str().as_bytes()),
        CString::new(to.as_os_str().as_bytes()),
    ) else {
        return false;
    };
    // APFS clone. It refuses when the destination exists, which is exactly the
    // state a fresh copy directory is in.
    unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) == 0 }
}

/// Windows block cloning needs `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS; until
/// that is wired the copy is a full one, which is slower but has the identical
/// guarantee — and the budget above is what keeps a slow copy from hanging a
/// session.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reflink(_from: &Path, _to: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::workspace_db;

    fn workspace(exec_mode: ExecMode) -> (tempfile::TempDir, SandboxManager, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        for sub in ["worktrees/s-1", "toolchain-cache/base", "tmp"] {
            std::fs::create_dir_all(root.join(sub)).expect("layout");
        }
        let (pool, _) = workspace_db::open_pool_at(&root).expect("workspace.db");
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-1', 'ws-1', 'u-1', 'S', 'cs/u/1', 'normal', 'f', 'v', 'idle', \
                  datetime('now'), datetime('now'))",
                [],
            )
            .expect("session");
        }
        let manager = SandboxManager::at(&root, exec_mode, None);
        (dir, manager, pool)
    }

    /// A worktree that looks like a real one: sources, an executable script, a
    /// `.git` pointer file and — because a checkout routinely has them — a
    /// symbolic link INSIDE the tree.
    fn seed_worktree(manager: &SandboxManager) -> PathBuf {
        let worktree = manager.worktree_dir("s-1").unwrap();
        std::fs::write(worktree.join("Cargo.toml"), b"[package]\n").unwrap();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/main.rs"), b"fn main() {}\n").unwrap();
        std::fs::write(worktree.join("build.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                worktree.join("build.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            // An ordinary in-tree link, of the kind `node_modules/.bin` or a
            // vendored crate produces. It must survive the copy.
            std::os::unix::fs::symlink("main.rs", worktree.join("src/alias.rs")).unwrap();
        }
        // A worktree's `.git` is a pointer file into the reference repository.
        std::fs::write(worktree.join(".git"), b"gitdir: ../../repo/.git/worktrees/s-1\n").unwrap();
        worktree
    }

    fn cow(ephemeral: bool) -> SandboxProfile {
        SandboxProfile::new(MountAccess::CopyOnWrite, NetworkAccess::None, ephemeral)
    }

    fn live_sandboxes(pool: &DbPool) -> i64 {
        let conn = pool.read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sandboxes WHERE state != 'stopped'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_cow_sandbox_builds_in_a_copy_and_leaves_the_worktree_untouched() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);

        let lease = manager
            .acquire(&pool, "s-1", cow(true), Some("run-1"))
            .expect("acquire");
        let ExecTarget::Local { cwd } = lease.target().clone() else {
            panic!("trusted_native must run locally");
        };
        assert_ne!(cwd, worktree, "a cow sandbox worked on the real worktree");
        assert_eq!(
            std::fs::read(cwd.join("src/main.rs")).unwrap(),
            b"fn main() {}\n",
            "the copy did not receive the worktree content"
        );
        assert!(
            !cwd.join(".git").exists(),
            "git metadata leaked into the sandbox"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cwd.join("build.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "the copy lost the executable bit, so a build in it would fail"
            );
            // The in-tree link is still a link, and it resolves inside the copy.
            assert_eq!(
                std::fs::read_link(cwd.join("src/alias.rs")).unwrap(),
                PathBuf::from("main.rs")
            );
            assert_eq!(
                std::fs::read(cwd.join("src/alias.rs")).unwrap(),
                b"fn main() {}\n"
            );
        }

        // A build writes into the copy: new artefacts appear there, and an edit
        // of an existing file must not reach the original.
        std::fs::create_dir_all(cwd.join("target")).unwrap();
        std::fs::write(cwd.join("target/binary"), b"built").unwrap();
        std::fs::write(cwd.join("src/main.rs"), b"fn main() { panic!() }\n").unwrap();

        assert!(!worktree.join("target").exists(), "target/ escaped into the worktree");
        assert_eq!(
            std::fs::read(worktree.join("src/main.rs")).unwrap(),
            b"fn main() {}\n",
            "the worktree was modified through a cow sandbox"
        );

        let layer = lease.layer_dir().unwrap().to_path_buf();
        manager.release(&pool, lease).expect("release");
        assert!(!layer.exists(), "the upper layer survived its lease");
        assert_eq!(
            std::fs::read(worktree.join("src/main.rs")).unwrap(),
            b"fn main() {}\n"
        );
    }

    /// The escape A13 describes, from the outside: a link in the worktree that
    /// points back OUT of it. Copied verbatim, `<layer>/escape/<file>` is the
    /// real worktree and the whole copy-on-write guarantee is gone — with no
    /// refusal recorded anywhere. The acquisition must fail instead, and the
    /// half-built copy must not survive it.
    #[cfg(unix)]
    #[test]
    fn a_link_that_leaves_the_tree_is_refused_and_never_reproduced_into_the_copy() {
        for target in ["../../worktrees/s-1", "../..", "/etc"] {
            let (dir, manager, pool) = workspace(ExecMode::TrustedNative);
            let worktree = seed_worktree(&manager);
            std::os::unix::fs::symlink(target, worktree.join("escape")).unwrap();

            let error = manager
                .acquire(&pool, "s-1", cow(true), Some("run-1"))
                .expect_err("a link out of the tree must refuse the sandbox");
            match &error {
                SandboxError::CowUnavailable { reason } => {
                    assert!(
                        reason.contains("outside the tree being copied"),
                        "the refusal did not name the link: {reason}"
                    );
                }
                other => panic!("expected CowUnavailable for {target}, got {other:?}"),
            }

            // Nothing half-built survives, nothing occupies the profile, and no
            // path anywhere under the sandbox area leads back to the worktree.
            assert_eq!(live_sandboxes(&pool), 0);
            let sandbox_area = dir.path().join("tmp/s-1/sandbox");
            let mut escapes = Vec::new();
            collect_links(&sandbox_area, &mut escapes);
            assert!(
                escapes.is_empty(),
                "a link into the worktree was reproduced anyway: {escapes:?}"
            );
        }
    }

    /// Every link found under `dir`, recursively. Used to prove that a refused
    /// copy left none behind.
    #[cfg(unix)]
    fn collect_links(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_symlink() {
                found.push(entry.path());
            } else if meta.is_dir() {
                collect_links(&entry.path(), found);
            }
        }
    }

    #[test]
    fn a_link_is_judged_by_where_its_text_leads_not_by_what_exists_today() {
        let root = Path::new("/w/tree");
        // Inside: plain names, `.` and a `..` that is paid back.
        for (link, target) in [
            ("/w/tree/src/alias.rs", "main.rs"),
            ("/w/tree/src/deep/x", "../../Cargo.toml"),
            ("/w/tree/a", "./b/c"),
            ("/w/tree/src/x", "../src/./y"),
        ] {
            assert!(
                link_stays_inside(root, Path::new(link), Path::new(target)),
                "{link} -> {target} was called an escape"
            );
        }
        // Outside: absolute, one `..` too many, and the sibling worktree of the
        // defect report.
        for (link, target) in [
            ("/w/tree/x", "/etc/passwd"),
            ("/w/tree/x", ".."),
            ("/w/tree/src/x", "../../other"),
            ("/w/tree/x", "../../worktrees/s-1"),
            ("/w/tree/src/deep/x", "../../../escape"),
        ] {
            assert!(
                !link_stays_inside(root, Path::new(link), Path::new(target)),
                "{link} -> {target} was allowed out of the tree"
            );
        }
    }

    #[test]
    fn a_copy_that_does_not_fit_the_size_budget_is_refused_rather_than_run_on_the_original() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);
        std::fs::write(worktree.join("big.bin"), vec![7u8; 4096]).unwrap();

        let manager = manager.with_cow_budget(CowBudget {
            max_bytes: 1024,
            max_files: 200_000,
            max_duration: Duration::from_secs(120),
        });
        let error = manager
            .acquire(&pool, "s-1", cow(true), Some("run-1"))
            .expect_err("an oversized copy must be refused");
        match &error {
            SandboxError::CowUnavailable { reason } => assert!(
                reason.contains("MiB budget"),
                "the refusal did not name the size budget: {reason}"
            ),
            other => panic!("expected CowUnavailable, got {other:?}"),
        }

        // Nothing was started, so nothing occupies the profile and no half-built
        // layer is left behind.
        assert_eq!(
            live_sandboxes(&pool),
            0,
            "a refused acquisition kept the profile occupied"
        );
    }

    /// The TIME half of the fail-closed budget, which the size test cannot
    /// reach: a copy that is still running when its deadline passes is refused
    /// mid-way, with the same outcome as one that is too large.
    #[test]
    fn a_copy_that_runs_past_its_time_budget_is_refused_mid_way() {
        let (dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);

        let timed = manager.with_cow_budget(CowBudget {
            max_bytes: 8 * 1024 * 1024,
            max_files: 200_000,
            // Any real copy takes longer than this, so the deadline is already
            // past while the entries are still being walked.
            max_duration: Duration::from_nanos(1),
        });

        let error = timed
            .acquire(&pool, "s-1", cow(true), Some("run-1"))
            .expect_err("a copy past its deadline must be refused");
        match &error {
            SandboxError::CowUnavailable { reason } => assert!(
                reason.contains("s budget"),
                "the refusal did not name the time budget: {reason}"
            ),
            other => panic!("expected CowUnavailable, got {other:?}"),
        }
        assert_eq!(live_sandboxes(&pool), 0);

        // The very same tree copies without complaint once there is time for
        // it, so the refusal above was the deadline and nothing else.
        let generous = SandboxManager::at(dir.path(), ExecMode::TrustedNative, None);
        let lease = generous
            .acquire(&pool, "s-1", cow(true), Some("run-2"))
            .expect("the same tree with a workable deadline");
        assert!(lease.layer_dir().is_some());
        assert!(worktree.join("src/main.rs").exists());
        generous.release(&pool, lease).unwrap();
    }

    /// §24, the part the budget tests never reach: a copy can also be
    /// impossible because the source cannot be READ or the destination cannot
    /// be created. Every one of those ends the same way — a refusal — and never
    /// with a lease pointing at the worktree.
    #[cfg(unix)]
    #[test]
    fn a_copy_that_cannot_be_made_at_all_is_refused_and_never_degrades_to_the_worktree() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: root ignores the permission bits this test relies on");
            return;
        }

        // 1. An unreadable directory inside the tree: `read_dir` fails.
        let (unreadable_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);
        let locked = worktree.join("vendor");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("lib.rs"), b"pub fn x() {}\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let error = manager.acquire(&pool, "s-1", cow(true), Some("run-1"));
        // Restore before asserting, so a failure does not leave an
        // undeletable temporary directory behind.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        match error {
            Err(SandboxError::CowUnavailable { reason }) => {
                assert!(reason.contains("read "), "unexpected reason: {reason}")
            }
            Err(other) => panic!("expected CowUnavailable, got {other:?}"),
            Ok(lease) => panic!("an unreadable source produced {:?}", lease.target()),
        }
        assert_eq!(live_sandboxes(&pool), 0);
        drop(unreadable_dir);

        // 2. A sandbox area that cannot be written: `create_dir_all` fails.
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let tmp = manager.session_tmp("s-1").unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o500)).unwrap();

        let error = manager.acquire(&pool, "s-1", cow(true), Some("run-1"));
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        match error {
            // Refusing is the invariant; which refusal it is depends on how far
            // the acquisition got before the read-only directory stopped it.
            Err(_) => {}
            Ok(lease) => panic!("a read-only sandbox area produced {:?}", lease.target()),
        }
        assert_eq!(live_sandboxes(&pool), 0);
    }

    /// A sandbox whose row cannot be marked ready is a sandbox nobody holds.
    /// Before this was fixed the copy stayed on disk and the row stayed at
    /// 'starting' — which the partial unique index counts, so the shared
    /// profile was occupied until the session ended.
    #[test]
    fn a_start_that_cannot_be_recorded_takes_the_workplace_and_the_row_down_with_it() {
        let (dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let profile = SandboxProfile::new(MountAccess::CopyOnWrite, NetworkAccess::None, false);

        // A trigger that refuses exactly the 'ready' transition: the row is
        // inserted, the copy is built, and only the last step fails — which is
        // the interleaving the defect was about.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "CREATE TRIGGER refuse_ready BEFORE UPDATE OF state ON sandboxes \
                 WHEN NEW.state = 'ready' \
                 BEGIN SELECT RAISE(ABORT, 'the row could not be marked ready'); END",
                [],
            )
            .unwrap();
        }
        let error = manager
            .acquire(&pool, "s-1", profile, None)
            .expect_err("a sandbox that cannot be recorded must not be handed out");
        assert!(matches!(error, SandboxError::Other(_)), "{error:?}");

        // The copy that was built for it is gone from the session's sandbox
        // area — nothing is left running against the worktree, and nothing
        // occupies disk for a lease that was never returned.
        let sandbox_area = dir.path().join("tmp/s-1/sandbox");
        let leftovers: Vec<PathBuf> = std::fs::read_dir(&sandbox_area)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the workplace of a failed start was left behind: {leftovers:?}"
        );

        // And the profile is free: the row of a failed start does not survive.
        assert_eq!(
            live_sandboxes(&pool),
            0,
            "a failed start kept the shared profile occupied"
        );
        {
            let conn = pool.write().unwrap();
            conn.execute("DROP TRIGGER refuse_ready", []).unwrap();
        }
        let again = manager
            .acquire(&pool, "s-1", profile, None)
            .expect("the profile must be free again");
        manager.release(&pool, again).unwrap();
    }

    #[test]
    fn a_missing_worktree_is_an_error_and_never_a_silent_empty_sandbox() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let profile = SandboxProfile::new(MountAccess::ReadWrite, NetworkAccess::None, false);
        assert!(manager.acquire(&pool, "s-2", profile, None).is_err());
    }

    #[test]
    fn two_ephemeral_runs_on_one_profile_work_side_by_side_and_cannot_see_each_other() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);

        let first = manager
            .acquire(&pool, "s-1", cow(true), Some("run-a"))
            .expect("first lease");
        let second = manager
            .acquire(&pool, "s-1", cow(true), Some("run-b"))
            .expect("second lease on the same profile");
        assert_ne!(first.lease_id, second.lease_id);
        assert!(first.lease_id.is_some() && second.lease_id.is_some());

        let (ExecTarget::Local { cwd: a }, ExecTarget::Local { cwd: b }) =
            (first.target().clone(), second.target().clone())
        else {
            panic!("expected local targets");
        };
        assert_ne!(a, b, "two leases shared one layer");

        std::fs::write(a.join("only-in-a"), b"a").unwrap();
        std::fs::write(b.join("only-in-b"), b"b").unwrap();
        assert!(!b.join("only-in-a").exists(), "lease b saw lease a's layer");
        assert!(!a.join("only-in-b").exists(), "lease a saw lease b's layer");
        assert!(!worktree.join("only-in-a").exists());

        // Releasing one leaves the other working.
        manager.release(&pool, first).expect("release a");
        assert!(b.join("only-in-b").exists(), "releasing a lease destroyed another");
        manager.release(&pool, second).expect("release b");
    }

    #[test]
    fn a_session_holds_at_most_one_shared_sandbox_per_profile() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let profile = SandboxProfile::new(MountAccess::ReadWrite, NetworkAccess::None, false);

        let held = manager.acquire(&pool, "s-1", profile, None).expect("first");
        match manager.acquire(&pool, "s-1", profile, None) {
            Err(SandboxError::SharedProfileBusy { mount, network, .. }) => {
                assert_eq!((mount, network), ("rw", "none"));
            }
            other => panic!("a second shared sandbox was created: {other:?}"),
        }

        // A different profile is a different sandbox, and an ephemeral one is
        // never blocked by a shared one.
        let other_profile = SandboxProfile::new(MountAccess::ReadOnly, NetworkAccess::None, false);
        let read_only = manager
            .acquire(&pool, "s-1", other_profile, None)
            .expect("a different profile");
        let ephemeral = manager
            .acquire(
                &pool,
                "s-1",
                SandboxProfile::new(MountAccess::ReadWrite, NetworkAccess::None, true),
                Some("run-1"),
            )
            .expect("an ephemeral sandbox of an occupied profile");

        manager.release(&pool, held).unwrap();
        manager.release(&pool, read_only).unwrap();
        manager.release(&pool, ephemeral).unwrap();

        // Releasing frees the profile again.
        let again = manager.acquire(&pool, "s-1", profile, None).expect("re-acquire");
        manager.release(&pool, again).unwrap();
    }

    /// `trusted_native` + `ro` enforces nothing at the OS level, and the test
    /// says so out loud: the lease points AT the worktree, a write through it
    /// SUCCEEDS, and the only thing standing between the two is the caller
    /// honouring `tools_read_only`. Asserting the flag alone would suggest a
    /// boundary that does not exist.
    #[test]
    fn read_only_in_trusted_native_is_an_instruction_to_the_caller_not_a_boundary() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);
        let lease = manager
            .acquire(
                &pool,
                "s-1",
                SandboxProfile::new(MountAccess::ReadOnly, NetworkAccess::None, false),
                None,
            )
            .expect("acquire");
        assert!(
            lease.tools_read_only,
            "a ro lease must tell the caller to withhold write tools"
        );
        assert_eq!(lease.target(), &ExecTarget::Local { cwd: worktree.clone() });

        // The gap, demonstrated rather than described: the process this lease
        // describes runs as the service user, in the worktree, and the OS lets
        // it write. Whoever holds this lease has to withhold the tools.
        let ExecTarget::Local { cwd } = lease.target() else {
            panic!("expected a local target");
        };
        std::fs::write(cwd.join("written-anyway"), b"x").unwrap();
        assert!(
            worktree.join("written-anyway").exists(),
            "the assumption behind tools_read_only changed: something now stops the write"
        );
        std::fs::remove_file(worktree.join("written-anyway")).unwrap();

        manager.release(&pool, lease).unwrap();
    }

    #[test]
    fn a_read_write_lease_is_the_worktree_itself() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);
        let lease = manager
            .acquire(
                &pool,
                "s-1",
                SandboxProfile::new(MountAccess::ReadWrite, NetworkAccess::None, false),
                None,
            )
            .expect("acquire");
        assert!(!lease.tools_read_only);
        assert_eq!(lease.target(), &ExecTarget::Local { cwd: worktree });
        assert!(lease.layer_dir().is_none(), "rw must not build a layer");
        manager.release(&pool, lease).unwrap();
    }

    #[test]
    fn the_toolchain_cache_is_a_read_only_base_plus_a_per_session_overlay() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let lease = manager
            .acquire(&pool, "s-1", cow(true), Some("run-1"))
            .expect("acquire");
        assert!(lease.toolchain_base.ends_with("toolchain-cache/base"));
        assert!(lease.toolchain_overlay.ends_with("toolchain-cache/ov/s-1"));
        assert!(lease.toolchain_overlay.is_dir());

        let overlay = lease.toolchain_overlay.clone();
        manager.release(&pool, lease).expect("release");
        assert!(
            overlay.is_dir(),
            "the session overlay must outlive an ephemeral sandbox"
        );
    }

    /// The overlay path of `build_cow_layer`, driven directly.
    ///
    /// Whether the mount succeeds depends on the privileges of whoever runs the
    /// tests, so the test asserts the INVARIANT both outcomes must keep — the
    /// process works somewhere that is not the worktree, and releasing the
    /// layer leaves the worktree exactly as it was — instead of asserting a
    /// mechanism the machine may not have.
    #[test]
    fn asking_for_an_overlay_either_mounts_one_or_falls_back_to_a_copy_with_the_same_guarantee() {
        let (dir, manager, _pool) = workspace(ExecMode::TrustedNative);
        let worktree = seed_worktree(&manager);
        let tmp = dir.path().join("tmp/s-1/sandbox/overlay-test");
        std::fs::create_dir_all(&tmp).unwrap();

        let (work, layer) = manager
            .build_cow_layer(&tmp, &worktree, true)
            .expect("a cow layer, by either mechanism");
        assert_ne!(work, worktree, "the layer was the worktree itself");
        assert_eq!(
            std::fs::read(work.join("src/main.rs")).unwrap(),
            b"fn main() {}\n"
        );

        std::fs::write(work.join("src/main.rs"), b"fn main() { changed() }\n").unwrap();
        std::fs::write(work.join("fresh"), b"new").unwrap();
        assert_eq!(
            std::fs::read(worktree.join("src/main.rs")).unwrap(),
            b"fn main() {}\n",
            "a write through the layer reached the worktree"
        );
        assert!(!worktree.join("fresh").exists());

        destroy_layer(&layer).expect("destroy the layer");
        assert!(!layer.root.exists());
        assert_eq!(
            std::fs::read(worktree.join("src/main.rs")).unwrap(),
            b"fn main() {}\n"
        );
    }

    /// The two container decisions that are pure policy — which network the
    /// container joins and how the worktree is bound — checked in a build that
    /// has no container runtime at all. Behind `#[cfg(feature = "docker")]`
    /// they would be untested in every default test run.
    #[test]
    fn a_gateway_profile_without_an_egress_network_is_refused_rather_than_bridged() {
        let with_gateway = ContainerConfig {
            runtime: ContainerRuntime::Docker,
            image: "img".into(),
            egress_network: Some("tf-egress-ws1".into()),
            user: "1000:1000".into(),
        };
        let without = ContainerConfig {
            egress_network: None,
            ..with_gateway.clone()
        };

        assert_eq!(
            container_network(NetworkAccess::None, &without).unwrap(),
            "none",
            "network_access=none must mean no route, not the default bridge"
        );
        assert_eq!(
            container_network(NetworkAccess::Gateway, &with_gateway).unwrap(),
            "tf-egress-ws1"
        );
        match container_network(NetworkAccess::Gateway, &without) {
            Err(SandboxError::RuntimeUnavailable(reason)) => {
                assert!(reason.contains("default bridge"), "{reason}")
            }
            other => panic!("a gateway profile fell back to the default bridge: {other:?}"),
        }

        assert_eq!(container_bind_mode(MountAccess::ReadOnly), "ro");
        assert_eq!(container_bind_mode(MountAccess::CopyOnWrite), "rw");
        assert_eq!(container_bind_mode(MountAccess::ReadWrite), "rw");
    }

    #[test]
    fn a_container_workspace_without_a_runtime_refuses_instead_of_running_locally() {
        let (_dir, manager, pool) = workspace(ExecMode::Container);
        seed_worktree(&manager);
        match manager.acquire(&pool, "s-1", cow(true), Some("run-1")) {
            Err(SandboxError::RuntimeUnavailable(_)) => {}
            other => panic!("a container profile fell back to the host: {other:?}"),
        }
        assert_eq!(live_sandboxes(&pool), 0);
    }

    #[test]
    fn a_crash_leaves_no_profile_occupied_after_reconciliation() {
        let (_dir, manager, pool) = workspace(ExecMode::TrustedNative);
        seed_worktree(&manager);
        let profile = SandboxProfile::new(MountAccess::ReadWrite, NetworkAccess::None, false);
        let leaked = manager.acquire(&pool, "s-1", profile, None).expect("acquire");
        // Simulates a crash: the row stays behind with no owner.
        std::mem::forget(leaked);

        assert_eq!(manager.reconcile_after_restart(&pool, "s-1").unwrap(), 1);
        let again = manager
            .acquire(&pool, "s-1", profile, None)
            .expect("the profile is free again");
        manager.release(&pool, again).unwrap();
    }

    #[test]
    fn the_copy_budget_counts_files_as_well_as_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        for i in 0..10 {
            std::fs::write(src.join(format!("f{i}")), b"x").unwrap();
        }
        let budget = CowBudget {
            max_bytes: 1024 * 1024,
            max_files: 3,
            max_duration: Duration::from_secs(60),
        };
        assert!(copy_tree(&src, &dst, &budget).is_err());
        assert!(copy_tree(&src, &dst, &CowBudget::default()).is_ok());
    }
}
