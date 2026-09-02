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

use tentanas_helper::HelperCommand;
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
    // Validate against the catalog BEFORE choosing a channel: a bad device
    // name must fail the same way whether or not the node is armed.
    let resolved = command.resolve().map_err(|e| match e {
        tentanas_helper::CatalogError::InvalidArgument(d) => BrokerError::InvalidArgument(d),
        tentanas_helper::CatalogError::ToolMissing(t) => BrokerError::ToolMissing(t),
    })?;

    if let Some(token) = explicit {
        let out = sudo_argv(token, &resolved, None, timeout).await?;
        return Ok((out, Channel::Explicit));
    }
    match super::elevation::mode(db) {
        super::elevation::Mode::Helper => {
            let out = through_helper(command, None, timeout).await?;
            Ok((out, Channel::Helper))
        }
        super::elevation::Mode::Interactive => {
            let Some(token) = super::elevation::armed_token() else {
                return Err(BrokerError::Unarmed("password not armed or expired"));
            };
            let out = sudo_argv(&token, &resolved, None, timeout).await?;
            Ok((out, Channel::Interactive))
        }
        super::elevation::Mode::Unset => Err(BrokerError::Unarmed("privilege mode not configured")),
    }
}

/// Runs one catalog command as root, feeding it raw key material on stdin.
/// Only the three ZFS encryption entries accept it (`reads_key_from_stdin`);
/// the key is written once and zeroized here, and it is never an argv word,
/// so it cannot appear in `ps`, in a job log or in the syslog audit line.
pub async fn run_privileged_with_key(
    db: &DbPool,
    command: &HelperCommand,
    key: &[u8],
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<(CommandOutput, Channel), BrokerError> {
    if !command.reads_key_from_stdin() {
        return Err(BrokerError::InvalidArgument(
            "this command does not take key material".to_string(),
        ));
    }
    let resolved = command.resolve().map_err(|e| match e {
        tentanas_helper::CatalogError::InvalidArgument(d) => BrokerError::InvalidArgument(d),
        tentanas_helper::CatalogError::ToolMissing(t) => BrokerError::ToolMissing(t),
    })?;
    if let Some(token) = explicit {
        return Ok((sudo_argv(token, &resolved, Some(key), timeout).await?, Channel::Explicit));
    }
    match super::elevation::mode(db) {
        super::elevation::Mode::Helper => {
            let out = through_helper(command, Some(key), timeout).await?;
            Ok((out, Channel::Helper))
        }
        super::elevation::Mode::Interactive => {
            let Some(token) = super::elevation::armed_token() else {
                return Err(BrokerError::Unarmed("password not armed or expired"));
            };
            let out = sudo_argv(&token, &resolved, Some(key), timeout).await?;
            Ok((out, Channel::Interactive))
        }
        super::elevation::Mode::Unset => Err(BrokerError::Unarmed("privilege mode not configured")),
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

/// Mode A: `sudo -n -- /usr/local/libexec/tentanas-helper` with the command as
/// one JSON line on stdin; the helper resolves it against the same catalog.
/// `key` is appended after that line and the helper forwards it to the tool.
async fn through_helper(
    command: &HelperCommand,
    key: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandOutput, BrokerError> {
    let helper = tentanas_helper::HELPER_INSTALL_PATH;
    let mut child = Command::new("sudo")
        .args(["-n", "--", helper])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| BrokerError::Io(format!("sudo: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let mut payload = Zeroizing::new(command.to_json_line().into_bytes());
        if let Some(key) = key {
            payload.extend_from_slice(key);
        }
        let write = stdin.write_all(&payload).await;
        let _ = stdin.shutdown().await;
        write.map_err(|e| BrokerError::Io(e.to_string()))?;
    }
    let out = wait_output(child, helper, timeout).await?;
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
        _ => Ok(out),
    }
}

/// Whether ANY channel could run a privileged command right now. Used by the
/// sampler to decide whether SMART refresh is possible without producing an
/// error per disk per tick.
pub async fn channel_available(db: &DbPool) -> bool {
    match super::elevation::mode(db) {
        super::elevation::Mode::Helper => super::elevation::helper_status().await.state == "ok",
        super::elevation::Mode::Interactive => super::elevation::armed_token().is_some(),
        super::elevation::Mode::Unset => false,
    }
}
