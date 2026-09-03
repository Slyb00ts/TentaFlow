// =============================================================================
// File: tentanas/elevation.rs — the privilege channel of one node (plan-02
//       §3.1). Two modes, chosen per node in the "Uprawnienia systemowe"
//       wizard and stored in tentanas.db:
//
//       helper       sudoers lets the core user run `tentanas-helper` without
//                    a password; the helper accepts only its compiled-in
//                    catalog. Nothing secret is kept in memory.
//       interactive  the admin types the sudo password; it stays in RAM
//                    (`Zeroizing`) for a TTL and every privileged call feeds
//                    it to `sudo -S`. Never written anywhere, wiped on TTL,
//                    disarm, disable and process exit.
//
//       A password that arrives from another node (the admin manages this
//       node from a different dashboard) is the same RAM-only object; the
//       mesh transport is already encrypted and the remote side zeroizes its
//       copy after the request (the protocol type redacts Debug).
// =============================================================================

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::{NasElevation, NasElevationPlan};

use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use crate::profiling::elevation_runner::ElevationRunner;

pub const SETTING_MODE: &str = "elevation_mode";
pub const SETTING_TTL: &str = "elevation_ttl_secs";
/// When mode A was provisioned and who ran it. Written by the provisioning
/// job, cleared by the removal job — the Environment tab's "provisioned on
/// … by …" line comes from here and from nowhere else.
pub const SETTING_PROVISIONED_AT: &str = "elevation_provisioned_at";
pub const SETTING_PROVISIONED_BY: &str = "elevation_provisioned_by";
/// Monotonic count of privileged invocations the broker has carried. Nothing
/// else in the app counts them: the syslog line the helper writes lives on the
/// node's journal, and the job log only covers work that became a job.
pub const SETTING_AUDIT_COUNT: &str = "elevation_audit_entries";
pub const DEFAULT_TTL_SECS: u32 = 15 * 60;
const MAX_TTL_SECS: u32 = 8 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Not chosen yet: every privileged call refuses until the wizard ran.
    Unset,
    Helper,
    Interactive,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Helper => "helper",
            Self::Interactive => "interactive",
        }
    }

    fn parse(value: Option<String>) -> Self {
        match value.as_deref() {
            Some("helper") => Self::Helper,
            Some("interactive") => Self::Interactive,
            _ => Self::Unset,
        }
    }
}

pub fn mode(db: &DbPool) -> Mode {
    Mode::parse(super::db::setting(db, SETTING_MODE).ok().flatten())
}

pub fn set_mode(db: &DbPool, mode: Mode) -> Result<()> {
    super::db::set_setting(db, SETTING_MODE, mode.as_str())
}

pub fn ttl_secs(db: &DbPool) -> u32 {
    super::db::setting(db, SETTING_TTL)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TTL_SECS)
        .min(MAX_TTL_SECS)
}

// ----- interactive mode: the armed password -------------------------------------

struct Armed {
    token: Arc<ElevationToken>,
    until: Instant,
    until_utc: chrono::DateTime<chrono::Utc>,
}

/// One slot per process: the app is a singleton and a node has one sudo
/// identity, so a second admin arming replaces the first — same as typing a
/// password over someone else's shoulder in a shared shell.
fn armed_slot() -> &'static Mutex<Option<Armed>> {
    static SLOT: Mutex<Option<Armed>> = Mutex::new(None);
    &SLOT
}

/// Validates the password against sudo and keeps it for `ttl_secs`
/// (0 = the node's configured TTL).
pub async fn arm(db: &DbPool, password: String, requested_ttl: u32) -> Result<NasElevation> {
    let token = ElevationToken::new_sudo(password);
    ElevationRunner::validate_sudo(&token)
        .await
        .map_err(|e| anyhow!("sudo validation failed: {e}"))?;
    let ttl = if requested_ttl == 0 {
        ttl_secs(db)
    } else {
        requested_ttl.min(MAX_TTL_SECS)
    };
    {
        let mut slot = armed_slot().lock().unwrap_or_else(|p| p.into_inner());
        *slot = Some(Armed {
            token: Arc::new(token),
            until: Instant::now() + Duration::from_secs(u64::from(ttl)),
            until_utc: chrono::Utc::now() + chrono::Duration::seconds(i64::from(ttl)),
        });
    }
    Ok(status(db).await)
}

/// Forgets the password. Idempotent; also the disable/teardown hook and the
/// shutdown path call this.
pub fn disarm() {
    let mut slot = armed_slot().lock().unwrap_or_else(|p| p.into_inner());
    // Dropping the Arc zeroizes the bytes once no in-flight sudo holds it.
    *slot = None;
}

/// The armed token if the TTL has not run out. An expired slot is cleared on
/// the way out so the secret does not linger until the next arm.
pub fn armed_token() -> Option<Arc<ElevationToken>> {
    let mut slot = armed_slot().lock().unwrap_or_else(|p| p.into_inner());
    match slot.as_ref() {
        Some(a) if a.until > Instant::now() => Some(a.token.clone()),
        Some(_) => {
            *slot = None;
            None
        }
        None => None,
    }
}

fn armed_until() -> Option<String> {
    let slot = armed_slot().lock().unwrap_or_else(|p| p.into_inner());
    slot.as_ref()
        .filter(|a| a.until > Instant::now())
        .map(|a| a.until_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

// ----- helper mode: state of the installed wrapper --------------------------------

/// What `helper_state` reports (§3.1). "ok" is the only state in which mode A
/// executes anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperStatus {
    pub state: &'static str,
    pub version: Option<String>,
}

/// Verifies the whole chain, not just the file: the binary exists, reports the
/// version this core was built with, and sudo runs it without a password.
/// A version mismatch after a core upgrade is reported (not auto-fixed): the
/// admin re-provisions consciously, the helper never updates itself.
pub async fn helper_status() -> HelperStatus {
    if !cfg!(target_os = "linux") {
        return HelperStatus {
            state: "unsupported",
            version: None,
        };
    }
    let path = tentanas_helper::HELPER_INSTALL_PATH;
    if !std::path::Path::new(path).is_file() {
        return HelperStatus {
            state: "missing",
            version: None,
        };
    }
    let version = match super::broker::run_unprivileged(path, &["--version"], Duration::from_secs(5)).await {
        Ok(out) if out.success() => out.stdout.trim().to_string(),
        _ => {
            return HelperStatus {
                state: "broken",
                version: None,
            }
        }
    };
    if version != tentanas_helper::VERSION {
        return HelperStatus {
            state: "version_mismatch",
            version: Some(version),
        };
    }
    if !std::path::Path::new(tentanas_helper::SUDOERS_INSTALL_PATH).is_file() {
        return HelperStatus {
            state: "sudoers_missing",
            version: Some(version),
        };
    }
    // `sudo -n` fails instead of prompting; `--version` is answered before the
    // helper's root check, so this proves the NOPASSWD line without running
    // any catalog command.
    let passwordless = super::broker::run_unprivileged(
        "sudo",
        &["-n", "--", path, "--version"],
        Duration::from_secs(5),
    )
    .await
    .map(|o| o.success())
    .unwrap_or(false);
    HelperStatus {
        state: if passwordless { "ok" } else { "not_passwordless" },
        version: Some(version),
    }
}

/// The user the core runs as — the one the sudoers line grants to.
pub async fn core_user() -> String {
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) {
        if !user.is_empty() {
            return user;
        }
    }
    super::broker::run_unprivileged("id", &["-un"], Duration::from_secs(3))
        .await
        .ok()
        .filter(|o| o.success())
        .map(|o| o.stdout.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Where the helper binary ships: next to the core executable (packaging
/// puts both in the same directory), overridable for development builds via
/// `TENTANAS_HELPER_SOURCE`.
pub fn helper_source() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("TENTANAS_HELPER_SOURCE") {
        return std::path::PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("tentanas-helper")))
        .unwrap_or_else(|| std::path::PathBuf::from("tentanas-helper"))
}

pub fn sudoers_line(user: &str) -> String {
    format!(
        "{user} ALL=(root) NOPASSWD: {}\n",
        tentanas_helper::HELPER_INSTALL_PATH
    )
}

/// The exact commands provisioning will run, shown verbatim in the wizard
/// (§3.1: "the admin sees precisely what will be executed"). The sudoers
/// line is staged in the instance data dir so no shell is involved: install
/// copies it with root ownership, visudo validates, an invalid file is
/// removed again rather than left to lock the admin out of sudo.
pub async fn plan(staging_dir: &std::path::Path) -> NasElevationPlan {
    let source = helper_source();
    let user = core_user().await;
    let staged = staging_dir.join("tentanas-sudoers.staged");
    let s = |v: &str| v.to_string();
    let commands = vec![
        vec![s("install"), s("-o"), s("root"), s("-g"), s("root"), s("-m"), s("0755"),
             source.display().to_string(), s(tentanas_helper::HELPER_INSTALL_PATH)],
        vec![s("install"), s("-o"), s("root"), s("-g"), s("root"), s("-m"), s("0440"),
             staged.display().to_string(), s(tentanas_helper::SUDOERS_INSTALL_PATH)],
        vec![s("visudo"), s("-c"), s("-f"), s(tentanas_helper::SUDOERS_INSTALL_PATH)],
    ];
    NasElevationPlan {
        helper_source_present: source.is_file(),
        helper_source: source.display().to_string(),
        helper_path: tentanas_helper::HELPER_INSTALL_PATH.to_string(),
        sudoers_path: tentanas_helper::SUDOERS_INSTALL_PATH.to_string(),
        sudoers_line: sudoers_line(&user).trim_end().to_string(),
        core_user: user,
        core_version: tentanas_helper::VERSION.to_string(),
        commands,
    }
}

/// Runs one plan command under `sudo -S` with the one-shot password and
/// returns its combined output. Used by the provisioning/removal jobs.
pub async fn run_plan_step(token: &ElevationToken, argv: &[String]) -> Result<String> {
    let Some((program, args)) = argv.split_first() else {
        return Err(anyhow!("empty command"));
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let child = ElevationRunner::spawn_sudo(token, program, &args, &[("LC_ALL", "C")])
        .await
        .map_err(|e| anyhow!("{e}"))?;
    let out = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| anyhow!("{program} timed out"))??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(anyhow!(
            "{program} exited with {}: {}",
            out.status.code().unwrap_or(-1),
            text.trim()
        ))
    }
}

/// Mode A removal: sudoers first (the grant), then the binary.
pub fn removal_commands() -> Vec<Vec<String>> {
    vec![
        vec!["rm".into(), "-f".into(), tentanas_helper::SUDOERS_INSTALL_PATH.into()],
        vec!["rm".into(), "-f".into(), tentanas_helper::HELPER_INSTALL_PATH.into()],
    ]
}

/// Records who provisioned mode A and when. Called by the provisioning job
/// after the helper verified, so a failed attempt never leaves a claim behind.
pub fn record_provisioning(db: &DbPool, admin: &str) -> Result<()> {
    super::db::set_setting(db, SETTING_PROVISIONED_AT, &super::db::now())?;
    super::db::set_setting(db, SETTING_PROVISIONED_BY, admin)
}

/// The counterpart: the helper is gone, so the claim must go with it.
pub fn clear_provisioning(db: &DbPool) -> Result<()> {
    super::db::set_setting(db, SETTING_PROVISIONED_AT, "")?;
    super::db::set_setting(db, SETTING_PROVISIONED_BY, "")
}

fn provisioning_value(db: &DbPool, key: &str) -> Option<String> {
    super::db::setting(db, key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
}

pub fn audit_entries(db: &DbPool) -> u64 {
    super::db::setting(db, SETTING_AUDIT_COUNT)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

pub async fn status(db: &DbPool) -> NasElevation {
    let helper = helper_status().await;
    // Compatibility is about the CATALOG, not the file: a helper that runs but
    // was built from a different catalog would accept commands this core does
    // not know it can send, so it is treated as incompatible, not as working.
    let core_compatible = helper.version.as_deref() == Some(tentanas_helper::VERSION);
    NasElevation {
        mode: mode(db).as_str().to_string(),
        helper_state: helper.state.to_string(),
        helper_path: tentanas_helper::HELPER_INSTALL_PATH.to_string(),
        helper_version: helper.version,
        sudoers_path: tentanas_helper::SUDOERS_INSTALL_PATH.to_string(),
        core_user: core_user().await,
        core_version: tentanas_helper::VERSION.to_string(),
        armed_until: armed_until(),
        ttl_secs: ttl_secs(db),
        provisioned_at: provisioning_value(db, SETTING_PROVISIONED_AT),
        provisioned_by: provisioning_value(db, SETTING_PROVISIONED_BY),
        audit_entries: audit_entries(db),
        core_compatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_arm_is_forgotten_on_read() {
        disarm();
        {
            let mut slot = armed_slot().lock().unwrap();
            *slot = Some(Armed {
                token: Arc::new(ElevationToken::new_sudo("x".into())),
                until: Instant::now() - Duration::from_secs(1),
                until_utc: chrono::Utc::now(),
            });
        }
        assert!(armed_token().is_none());
        assert!(armed_slot().lock().unwrap().is_none());
        assert!(armed_until().is_none());
    }

    #[test]
    fn sudoers_line_grants_only_the_helper() {
        let line = sudoers_line("tentaflow");
        assert_eq!(
            line,
            "tentaflow ALL=(root) NOPASSWD: /usr/local/libexec/tentanas-helper\n"
        );
    }
}
