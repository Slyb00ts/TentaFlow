// =============================================================================
// File: tentanas/broker.rs — the ONLY place TentaNas runs a system command
//       (plan-02 §3.2). Two entry points:
//
//       run_unprivileged  argv as the core user, for reads that need no root
//                         (lsblk, /proc, version probes).
//       run_privileged    one `HelperCommand` of the typed catalog, through
//                         whichever channel the node has: an explicit
//                         one-shot password, the passwordless helper, or the
//                         armed interactive password. No channel → refused,
//                         never a prompt, never `sh -c`.
//
//       Both return captured output with a hard timeout; callers parse JSON,
//       never scrape text.
// =============================================================================

use std::process::Stdio;
use std::time::Duration;

use tentanas_helper::{HelperCommand, Plan};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use crate::profiling::elevation_runner::ElevationRunner;

#[derive(Debug, Error)]
pub enum BrokerError {
    /// No usable privilege channel on this node: the UI opens the wizard /
    /// asks for the password instead of retrying.
    #[error("privilege channel not available: {0}")]
    Unarmed(&'static str),
    #[error("{0} is not installed")]
    ToolMissing(&'static str),
    #[error("invalid command: {0}")]
    InvalidArgument(String),
    #[error("{program} timed out after {secs}s")]
    Timeout { program: String, secs: u64 },
    #[error("{program} exited with {code}: {stderr}")]
    Exit {
        program: String,
        code: i32,
        stderr: String,
    },
    #[error("{0}")]
    Io(String),
    /// The installed helper is not the build this core expects, or its version
    /// could not be read at all. Elastic operations are refused on it: an
    /// older helper accepts the command and runs an OLDER sequence, which is
    /// how a 0.12.0 helper would still freeze a union for a mover this core
    /// no longer knows how to release.
    #[error("{0}")]
    HelperVersion(String),
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// Which privilege channel answered — surfaced in job logs so an admin can
/// tell "the helper did it" from "the typed password did it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Explicit,
    Helper,
    Interactive,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "one-shot password",
            Self::Helper => "helper",
            Self::Interactive => "armed password",
        }
    }
}

async fn wait_output(
    child: tokio::process::Child,
    program: &str,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(BrokerError::Io(e.to_string())),
        Err(_) => {
            // Children spawned here carry `kill_on_drop`; a `sudo -S` child
            // from the elevation runner does not, and killing the sudo
            // wrapper would leave the root process anyway — it finishes on
            // its own and the OS reaps it.
            return Err(BrokerError::Timeout {
                program: program.to_string(),
                secs: timeout.as_secs(),
            });
        }
    };
    Ok(CommandOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Runs `program args…` as the core user with a sanitized locale. Non-zero
/// exit is NOT an error here — probes decide what an exit code means.
pub async fn run_unprivileged(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    let child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| BrokerError::Io(format!("{program}: {e}")))?;
    wait_output(child, program, timeout).await
}

/// Turns a non-zero exit into `BrokerError::Exit`. Callers whose tool uses
/// the exit code as a bitmask (smartctl) do NOT go through this — they keep
/// the output and read the bits themselves.
pub fn require_success(program: &str, out: CommandOutput) -> Result<CommandOutput, BrokerError> {
    if out.success() {
        Ok(out)
    } else {
        Err(BrokerError::Exit {
            program: program.to_string(),
            code: out.code,
            stderr: out.stderr.trim().chars().take(2000).collect(),
        })
    }
}

/// Runs one catalog command as root. `explicit` is a password sent with the
/// request (used once, never stored); otherwise the node's configured channel
/// decides. Returns the output (whatever its exit code — smartctl's is a
/// bitmask) and the channel that produced it; a refused channel is an error.
pub async fn run_privileged(
    db: &DbPool,
    command: &HelperCommand,
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<(CommandOutput, Channel), BrokerError> {
    run_with_stdin(db, command, None, explicit, timeout).await
}

/// Runs one catalog command as root, feeding it a raw payload on stdin: the
/// key material of the three ZFS encryption entries, the document of the two
/// service-config writers, the password of the share-user setter
/// (`reads_key_from_stdin`). The payload is written once and never becomes an
/// argv word, so it cannot appear in `ps`, in a job log or in the syslog
/// audit line.
pub async fn run_privileged_with_key(
    db: &DbPool,
    command: &HelperCommand,
    key: &[u8],
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<(CommandOutput, Channel), BrokerError> {
    if !command.reads_key_from_stdin() {
        return Err(BrokerError::InvalidArgument(
            "this command does not take a stdin payload".to_string(),
        ));
    }
    run_with_stdin(db, command, Some(key), explicit, timeout).await
}

fn catalog(error: tentanas_helper::CatalogError) -> BrokerError {
    match error {
        tentanas_helper::CatalogError::InvalidArgument(d) => BrokerError::InvalidArgument(d),
        tentanas_helper::CatalogError::ToolMissing(t) => BrokerError::ToolMissing(t),
    }
}

/// The helper binary a builtin entry can be run through: the provisioned one,
/// or the copy shipped next to the core binary. Mode B has nothing installed,
/// but running OUR binary under `sudo -S` with the admin's password is still
/// mode B — nothing persistent is granted, every invocation is authorized.
fn helper_binary() -> Result<String, BrokerError> {
    for candidate in [
        std::path::PathBuf::from(tentanas_helper::HELPER_INSTALL_PATH),
        super::elevation::helper_source(),
    ] {
        if candidate.is_file() {
            return Ok(candidate.display().to_string());
        }
    }
    Err(BrokerError::ToolMissing("tentanas-helper"))
}

/// Counts one privileged invocation. The helper writes an authpriv line per
/// call on the node itself; this is the app's own tally, so the Environment
/// tab can say how much the channel has been used without reading syslog.
/// A failed count must never fail the command it was counting.
fn record_invocation(db: &DbPool) {
    if let Err(e) = super::db::bump_counter(db, super::elevation::SETTING_AUDIT_COUNT) {
        tracing::warn!("tentanas: privilege audit counter not updated: {e}");
    }
}

async fn run_with_stdin(
    db: &DbPool,
    command: &HelperCommand,
    payload: Option<&[u8]>,
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<(CommandOutput, Channel), BrokerError> {
    // Validate against the catalog BEFORE choosing a channel: a bad device
    // name must fail the same way whether or not the node is armed.
    let plan = command.plan().map_err(catalog)?;
    command.validate_payload(payload.unwrap_or_default()).map_err(catalog)?;
    if let Some(out) = recorded(db, command, payload) {
        return Ok((out, Channel::Helper));
    }
    // A builtin has no argv to hand to `sudo`: the sequence lives in the
    // helper, so it always crosses the channel as a helper invocation.
    let exec = match &plan {
        Plan::Exec(resolved) => Some(resolved),
        Plan::Builtin(_) => None,
    };

    // Counted once a channel has been chosen and the command is about to run:
    // a request refused by the catalog or by an unarmed node never touched the
    // system, so it is not an invocation.
    if let Some(token) = explicit {
        record_invocation(db);
        let out = explicit_channel(command, exec, payload, token, timeout).await?;
        return Ok((out, Channel::Explicit));
    }
    match super::elevation::mode(db) {
        super::elevation::Mode::Helper => {
            record_invocation(db);
            let out = through_helper(
                tentanas_helper::HELPER_INSTALL_PATH,
                None,
                command,
                payload,
                timeout,
            )
            .await?;
            Ok((out, Channel::Helper))
        }
        super::elevation::Mode::Interactive => {
            let Some(token) = super::elevation::armed_token() else {
                return Err(BrokerError::Unarmed("password not armed or expired"));
            };
            record_invocation(db);
            let out = match exec {
                Some(resolved) => sudo_argv(&token, resolved, payload, timeout).await?,
                None => {
                    through_helper(&helper_binary()?, Some(&token), command, payload, timeout)
                        .await?
                }
            };
            Ok((out, Channel::Interactive))
        }
        super::elevation::Mode::Unset => Err(BrokerError::Unarmed("privilege mode not configured")),
    }
}

/// Runs one catalog command as root with a password the operator has just
/// typed, for a surface outside TentaNas (the agent sandbox repair). There is
/// no TentaNas instance behind it, so there is no configured channel to fall
/// back on and no instance tally to count it in — the catalog, the payload
/// check and `sudo -S` are exactly the TentaNas explicit path.
pub async fn run_with_password(
    command: &HelperCommand,
    payload: Option<&[u8]>,
    token: &ElevationToken,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    let plan = command.plan().map_err(catalog)?;
    command.validate_payload(payload.unwrap_or_default()).map_err(catalog)?;
    if payload.is_some() != command.reads_key_from_stdin() {
        return Err(BrokerError::InvalidArgument(
            "stdin payload does not match what the command takes".to_string(),
        ));
    }
    let exec = match &plan {
        Plan::Exec(resolved) => Some(resolved),
        Plan::Builtin(_) => None,
    };
    explicit_channel(command, exec, payload, token, timeout).await
}

/// The explicit channel: an exec entry straight under `sudo -S`, a builtin
/// through the helper binary under the same password.
async fn explicit_channel(
    command: &HelperCommand,
    exec: Option<&tentanas_helper::Resolved>,
    payload: Option<&[u8]>,
    token: &ElevationToken,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    match exec {
        Some(resolved) => sudo_argv(token, resolved, payload, timeout).await,
        None => through_helper(&helper_binary()?, Some(token), command, payload, timeout).await,
    }
}

/// Mode B / explicit: `sudo -S -- <resolved program> <args>` with the password
/// on stdin. The catalog resolution ran on this host, so the argv is exactly
/// what the helper would have executed.
///
/// `key` appends raw key material after the password line. `sudo -S` reads the
/// password one byte at a time and stops at the newline, so what follows stays
/// in the pipe for the child it execs — that is how `zfs load-key` reaches
/// `keylocation=prompt` without the key ever touching a file or an argv.
async fn sudo_argv(
    token: &ElevationToken,
    resolved: &tentanas_helper::Resolved,
    key: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    let program = resolved.program.display().to_string();
    let child = match key {
        None => {
            let args: Vec<&str> = resolved.args.iter().map(String::as_str).collect();
            let env: Vec<(&str, &str)> = resolved
                .env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            ElevationRunner::spawn_sudo(token, &program, &args, &env)
                .await
                .map_err(|e| BrokerError::Io(e.to_string()))?
        }
        Some(key) => {
            let mut cmd = Command::new("sudo");
            cmd.arg("-S").arg("--").arg(&program).args(&resolved.args);
            for (k, v) in &resolved.env {
                cmd.env(k, v);
            }
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| BrokerError::Io(format!("sudo: {e}")))?;
            let mut payload = Zeroizing::new(Vec::with_capacity(key.len() + 64));
            payload.extend_from_slice(token.as_secret_bytes());
            payload.push(b'\n');
            payload.extend_from_slice(key);
            if let Some(mut stdin) = child.stdin.take() {
                let write = stdin.write_all(&payload).await;
                let _ = stdin.shutdown().await;
                write.map_err(|e| BrokerError::Io(e.to_string()))?;
            }
            child
        }
    };
    let out = wait_output(child, &program, timeout).await?;
    if out.code == 1 && out.stderr.contains("incorrect password") {
        return Err(BrokerError::Unarmed("sudo rejected the password"));
    }
    Ok(out)
}

/// Runs the command through the helper binary with the command as one JSON
/// line on stdin; the helper resolves it against the same catalog. `key` is
/// appended after that line and the helper forwards it to the tool or to the
/// builtin. `token` picks the mode: `None` is mode A's passwordless
/// `sudo -n`, `Some` is `sudo -S` with the password on the first line —
/// `sudo -S` stops reading at that newline, so the command line and its
/// payload stay in the pipe for the helper it execs.
// ----- the version gate: an Elastic command never reaches a helper of another
// ----- build than the one this core was compiled against ------------------------

/// The marker every version refusal carries. `db::nothing_ran` matches on it to
/// close the operation row as "refused before anything ran", so a refused
/// cadence tick leaves the array exactly as it was instead of parking a fault
/// on it.
pub const HELPER_VERSION_MARKER: &str = "helper w innej wersji niż rdzeń";

/// How long a version probe is trusted. The probe is one unprivileged
/// `--version` exec; the scheduler ticks every minute and the automatic mover
/// reads the cache too, so without this the node would exec the helper once
/// per tick per array to learn something that changes only when an admin
/// re-provisions (which clears the cache outright,
/// `forget_helper_version`).
const VERSION_PROBE_TTL: Duration = Duration::from_secs(60);

struct VersionProbe {
    path: String,
    /// `None` = the probe could not read a version. Cached as well as a
    /// success, because "unknown" is a verdict the gate acts on, and probing
    /// a broken binary every tick is the same exec storm.
    version: Option<String>,
    at: std::time::Instant,
}

fn version_cache() -> &'static std::sync::Mutex<Option<VersionProbe>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<VersionProbe>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// The cached verdict for `path`, or `None` when there is no FRESH one. The
/// two layers are separate so the cache can be tested without an exec: the
/// outer `Option` is "do we know", the inner one is "what the probe read".
fn cached_version(path: &str, now: std::time::Instant) -> Option<Option<String>> {
    let cache = version_cache().lock().unwrap_or_else(|p| p.into_inner());
    cache
        .as_ref()
        .filter(|probe| probe.path == path && now.duration_since(probe.at) < VERSION_PROBE_TTL)
        .map(|probe| probe.version.clone())
}

fn remember_version(path: &str, version: Option<String>, now: std::time::Instant) {
    let mut cache = version_cache().lock().unwrap_or_else(|p| p.into_inner());
    *cache = Some(VersionProbe { path: path.to_string(), version, at: now });
}

/// Drops the cached probe. Called when provisioning installs or removes the
/// helper, so the very next Elastic operation sees the new binary instead of
/// waiting out the TTL.
pub fn forget_helper_version() {
    *version_cache().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Whether this command is an Elastic Array operation, i.e. one whose whole
/// sequence lives in the helper and whose protocol between core and helper is
/// what a version skew breaks.
///
/// SCOPE, deliberately: the ZFS/zpool guards and the share writers are not
/// gated. They are single commands with a stable argv contract, and the one
/// thing an older helper does differently with them — reading the Elastic
/// journals to decide a guard — already fails CLOSED there, because a journal
/// it cannot parse makes it refuse by name. An Elastic operation is the
/// opposite: an older helper accepts the command and performs an older
/// sequence (0.12.0 still entered a service Hold for a mover and remounted the
/// union read-only, and this core no longer carries the release for it), which
/// is a freeze nobody asked for.
fn is_elastic(command: &HelperCommand) -> bool {
    command
        .builtin_label()
        .is_some_and(|label| label.starts_with("elastic_"))
}

/// The gate itself, over the version the probe read (`None` = unknown).
///
/// UNKNOWN REFUSES, and that is the point of the third arm: a probe that fails
/// is not evidence that the helper is fine, and the failure modes that produce
/// it — the file is gone, it is not executable, it hangs — are exactly the ones
/// where running it blind is worst. An older helper is refused for the same
/// reason as a newer one: neither speaks this core's sequence.
fn version_gate(command: &HelperCommand, installed: Option<&str>) -> Result<(), BrokerError> {
    if !is_elastic(command) {
        return Ok(());
    }
    let expected = tentanas_helper::VERSION;
    match installed {
        Some(version) if version == expected => Ok(()),
        Some(version) => Err(BrokerError::HelperVersion(format!(
            "{HELPER_VERSION_MARKER}: zainstalowany helper ma wersję {version}, a ten rdzeń wymaga \
             {expected}. Operacja nie została uruchomiona — powtórz nadanie uprawnień systemowych \
             (TentaNas → Środowisko → kanał uprawnień), aby zainstalować pasującą wersję helpera."
        ))),
        None => Err(BrokerError::HelperVersion(format!(
            "{HELPER_VERSION_MARKER}: nie udało się odczytać wersji zainstalowanego helpera (ten \
             rdzeń wymaga {expected}). Operacja nie została uruchomiona — powtórz nadanie uprawnień \
             systemowych (TentaNas → Środowisko → kanał uprawnień)."
        ))),
    }
}

/// The version `path` reports, cached. One unprivileged exec of `--version`,
/// which needs no password and no channel: the same probe `helper_status`
/// makes for the Environment tab, taken here because the tab is not what
/// stands between a cadence and the helper.
async fn installed_version(path: &str) -> Option<String> {
    if let Some(hit) = cached_version(path, std::time::Instant::now()) {
        return hit;
    }
    let probed = run_unprivileged(path, &["--version"], Duration::from_secs(5))
        .await
        .ok()
        .filter(|out| out.success())
        .map(|out| out.stdout.trim().to_string())
        .filter(|version| !version.is_empty());
    remember_version(path, probed.clone(), std::time::Instant::now());
    probed
}

async fn through_helper(
    helper: &str,
    token: Option<&ElevationToken>,
    command: &HelperCommand,
    key: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    // THE VERSION GATE, here because this is the only function that execs a
    // helper builtin and the only one that knows WHICH binary will run — the
    // provisioned one, or the copy beside the core in mode B. Every Elastic
    // caller passes through it: the manual handlers, the scheduled passes, the
    // automatic mover, the startup restores and the privileged reads that are
    // not jobs at all (the cache and age probes). A check at the job-spawn
    // boundary would have missed the last of those.
    if is_elastic(command) {
        version_gate(command, installed_version(helper).await.as_deref())?;
    }
    let mut cmd = Command::new("sudo");
    match token {
        None => cmd.args(["-n", "--", helper]),
        Some(_) => cmd.args(["-S", "--", helper]),
    };
    let mut child = cmd
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| BrokerError::Io(format!("sudo: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let mut payload = Zeroizing::new(Vec::new());
        if let Some(token) = token {
            payload.extend_from_slice(token.as_secret_bytes());
            payload.push(b'\n');
        }
        payload.extend_from_slice(command.to_json_line().as_bytes());
        if let Some(key) = key {
            payload.extend_from_slice(key);
        }
        let write = stdin.write_all(&payload).await;
        let _ = stdin.shutdown().await;
        write.map_err(|e| BrokerError::Io(e.to_string()))?;
    }
    let out = wait_output(child, helper, timeout).await?;
    if out.code == 1 && out.stderr.contains("incorrect password") {
        return Err(BrokerError::Unarmed("sudo rejected the password"));
    }
    // The helper's own refusals map back to broker errors so the UI can show
    // "not provisioned" instead of a raw exit code. The child's exit code
    // passes through the helper, so its codes are recognized only together
    // with the helper's stderr prefix.
    let own = out.stderr.starts_with("tentanas-helper:");
    match out.code {
        1 if out.stderr.contains("a password is required") => {
            Err(BrokerError::Unarmed("helper is not passwordless"))
        }
        65 if own => Err(BrokerError::InvalidArgument(out.stderr.trim().to_string())),
        66 if own => Err(BrokerError::ToolMissing("tool reported missing by helper")),
        67 if own => Err(BrokerError::Unarmed("helper did not run as root")),
        68 if own => Err(BrokerError::Io(out.stderr.trim().to_string())),
        // 69 is a builtin that ran and failed (testparm rejected the config,
        // exportfs refused it, the mount timed out): the caller wants the
        // helper's own one-line reason, so it stays a non-zero output rather
        // than becoming a channel error.
        _ => Ok(out),
    }
}

/// Production has no test channel (`test_channel`, test builds only).
#[cfg(not(test))]
fn recorded(_db: &DbPool, _command: &HelperCommand, _payload: Option<&[u8]>) -> Option<CommandOutput> {
    None
}

#[cfg(not(test))]
fn recorded_channel(_db: &DbPool) -> bool {
    false
}

/// Whether ANY channel could run a privileged command right now. Used by the
/// sampler to decide whether SMART refresh is possible without producing an
/// error per disk per tick.
pub async fn channel_available(db: &DbPool) -> bool {
    if recorded_channel(db) {
        return true;
    }
    match super::elevation::mode(db) {
        super::elevation::Mode::Helper => super::elevation::helper_status().await.state == "ok",
        super::elevation::Mode::Interactive => super::elevation::armed_token().is_some(),
        super::elevation::Mode::Unset => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AN ELASTIC OPERATION NEVER REACHES A HELPER OF ANOTHER BUILD, and an
    /// unreadable version is refused exactly like a wrong one.
    ///
    /// The window this closes is real and not hypothetical: the helper is
    /// installed separately, by hand, with the admin's sudo password, so a new
    /// core necessarily runs against the previous helper until someone
    /// re-provisions. In that window the automatic mover fires on cadence, and
    /// a 0.12.0 helper still entered a service Hold and remounted the union
    /// read-only for it — while this core no longer carries the release for
    /// that Hold, because it moved into the helper. Unattended, that is the
    /// freeze this whole series removed.
    #[test]
    fn the_version_gate_refuses_a_mismatch_and_an_unknown_and_admits_a_match() {
        let elastic = HelperCommand::ElasticJournals {};
        assert!(is_elastic(&elastic));

        // The version this core was built against is admitted.
        version_gate(&elastic, Some(tentanas_helper::VERSION)).expect("the matching helper runs");

        // An OLDER helper is refused, and the refusal names both versions and
        // the remedy — an admin who is not told cannot fix it.
        let older = version_gate(&elastic, Some("0.12.0")).expect_err("an older helper is refused");
        let BrokerError::HelperVersion(why) = &older else {
            panic!("the refusal has to be its own error: {older:?}")
        };
        assert!(why.contains("0.12.0"), "{why}");
        assert!(why.contains(tentanas_helper::VERSION), "{why}");
        assert!(why.contains("uprawnień"), "{why}");
        // And it carries the marker the store reads to close the row as
        // "nothing ran", so a refused cadence tick parks no fault.
        assert!(why.contains(HELPER_VERSION_MARKER), "{why}");

        // A NEWER helper is refused for the same reason: neither build speaks
        // this core's sequence.
        assert!(matches!(
            version_gate(&elastic, Some("99.0.0")),
            Err(BrokerError::HelperVersion(_))
        ));

        // UNKNOWN REFUSES. A probe that failed is not evidence that the helper
        // is fine, and it is produced by exactly the states in which running
        // it blind is worst: the file is gone, it is not executable, it hangs.
        let unknown = version_gate(&elastic, None).expect_err("unknown is not fine");
        let BrokerError::HelperVersion(why) = &unknown else { panic!("{unknown:?}") };
        assert!(why.contains("nie udało się odczytać"), "{why}");
        assert!(why.contains(HELPER_VERSION_MARKER), "{why}");

        // NOT an Elastic command: the share writers and the ZFS guards keep
        // working on an older helper, deliberately — a single command with a
        // stable argv, and the one thing an older helper does differently with
        // them (reading an Elastic journal it cannot parse) already fails
        // closed on its own side.
        let share = HelperCommand::SmbIncludeEnsure {};
        assert!(!is_elastic(&share));
        for installed in [Some(tentanas_helper::VERSION), Some("0.12.0"), None] {
            version_gate(&share, installed).expect("a share writer is not gated");
        }
    }

    /// THE GATE IS ON THE EXEC PATH, not merely defined next to it.
    ///
    /// The two tests above prove what the gate decides; this one proves that
    /// the only function which execs a helper builtin asks it, BEFORE it builds
    /// the `sudo` command. It reads this file's own source because there is no
    /// way to observe the wiring otherwise: reaching `through_helper` from a
    /// test would either exec `sudo` on the machine running the suite or prove
    /// nothing on a machine where the helper happens to match.
    #[test]
    fn every_helper_exec_asks_the_version_gate_first() {
        const SOURCE: &str = include_str!("broker.rs");
        let start = SOURCE
            .find("async fn through_helper(")
            .expect("through_helper moved or was renamed");
        let body = &SOURCE[start..];
        let gate = body.find("version_gate(command,").expect("the exec path does not ask the gate");
        let sudo = body.find("Command::new(\"sudo\")").expect("through_helper no longer execs sudo");
        assert!(gate < sudo, "the gate has to run BEFORE the command is built");
        // And it is the only exec of a helper builtin: every channel arm — a
        // one-shot password, the passwordless helper, the armed password —
        // funnels through this one function, so one gate covers them all. The
        // count is over PRODUCTION source only (this module's own text mentions
        // the name too), and it is three call sites plus the definition.
        let production = &SOURCE[..SOURCE.find("#[cfg(test)]").expect("the test module")];
        assert_eq!(
            production.matches("through_helper(").count(),
            4,
            "a new call site of through_helper has to be checked against the gate"
        );
    }

    /// The probe is CACHED, so the gate is not an exec per tick: the scheduler
    /// wakes every minute and the automatic mover reads the same verdict for
    /// every array on the node.
    #[test]
    fn the_version_probe_is_cached_per_binary_until_its_ttl_or_a_provisioning() {
        let now = std::time::Instant::now();
        let path = "/probe/tentanas-helper";
        forget_helper_version();
        assert_eq!(cached_version(path, now), None, "nothing is known before the first probe");

        remember_version(path, Some("0.13.0".into()), now);
        assert_eq!(cached_version(path, now), Some(Some("0.13.0".into())));
        // Still fresh just inside the TTL, gone just outside it.
        assert_eq!(
            cached_version(path, now + VERSION_PROBE_TTL - Duration::from_millis(1)),
            Some(Some("0.13.0".into()))
        );
        assert_eq!(cached_version(path, now + VERSION_PROBE_TTL), None, "a stale entry is unknown");

        // A FAILED probe is cached too — otherwise a broken binary would be
        // re-executed every tick — and it stays "unknown", which refuses.
        remember_version(path, None, now);
        assert_eq!(cached_version(path, now), Some(None));

        // The cache is per binary: mode B runs the copy beside the core, and a
        // verdict about one path says nothing about the other.
        assert_eq!(cached_version("/usr/local/libexec/tentanas-helper", now), None);

        // Provisioning installs a new binary, so the verdict is dropped at
        // once rather than waiting out the TTL.
        remember_version(path, Some("0.13.0".into()), now);
        forget_helper_version();
        assert_eq!(cached_version(path, now), None);
    }
}

// Kept after the tests module on purpose: the source scans above count
// production call sites up to the first test-only attribute.
#[cfg(test)]
fn recorded(db: &DbPool, command: &HelperCommand, payload: Option<&[u8]>) -> Option<CommandOutput> {
    test_channel::for_db(db).map(|r| r.run(command, payload))
}

#[cfg(test)]
fn recorded_channel(db: &DbPool) -> bool {
    test_channel::for_db(db).is_some()
}

/// A privilege channel for tests, bound to ONE database (the `Arc` it is
/// registered for), so tests running side by side never see each other's:
/// every privileged command a job sends on that node is recorded with its
/// stdin payload (the generated smb.conf include, /etc/exports) and answers
/// success, and nothing reaches the host. It exists so a job can be run
/// end to end — the real apply, the real sweep, the real job — without root.
#[cfg(test)]
pub mod test_channel {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use tentanas_helper::HelperCommand;

    use super::CommandOutput;
    use crate::db::DbPool;

    #[derive(Default)]
    pub struct Recorder {
        pub calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl Recorder {
        pub(super) fn run(&self, command: &HelperCommand, payload: Option<&[u8]>) -> CommandOutput {
            let name = serde_json::to_value(command)
                .ok()
                .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(str::to_string))
                .unwrap_or_default();
            let payload = payload.map(|p| String::from_utf8_lossy(p).into_owned());
            self.calls.lock().unwrap().push((name, payload));
            CommandOutput { code: 0, stdout: String::new(), stderr: String::new() }
        }

        /// The payload of the last call named `command`.
        pub fn last_payload(&self, command: &str) -> Option<String> {
            self.calls.lock().unwrap().iter().rev().find(|(n, _)| n == command).and_then(|(_, p)| p.clone())
        }
    }

    fn registry() -> &'static Mutex<HashMap<usize, Arc<Recorder>>> {
        static REG: OnceLock<Mutex<HashMap<usize, Arc<Recorder>>>> = OnceLock::new();
        REG.get_or_init(Default::default)
    }

    /// Routes every privileged command sent for `db` into a new recorder,
    /// until the returned guard is dropped (an address a dropped database
    /// had can be handed to another test's database).
    pub fn install(db: &DbPool) -> Installed {
        let key = Arc::as_ptr(db) as *const () as usize;
        let recorder = Arc::new(Recorder::default());
        registry().lock().unwrap().insert(key, recorder.clone());
        Installed { key, recorder }
    }

    pub struct Installed {
        key: usize,
        pub recorder: Arc<Recorder>,
    }

    impl std::ops::Deref for Installed {
        type Target = Recorder;
        fn deref(&self) -> &Recorder {
            &self.recorder
        }
    }

    impl Drop for Installed {
        fn drop(&mut self) {
            registry().lock().unwrap().remove(&self.key);
        }
    }

    pub(super) fn for_db(db: &DbPool) -> Option<Arc<Recorder>> {
        registry().lock().unwrap().get(&(Arc::as_ptr(db) as *const () as usize)).cloned()
    }
}

