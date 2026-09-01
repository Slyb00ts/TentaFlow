// =============================================================================
// File: service.rs — `tentaflow start|stop|restart|status`
// =============================================================================
//
// A thin, honest wrapper over the platform service manager. TentaFlow runs as a
// systemd unit (Linux) or a launchd agent (macOS); these subcommands drive that
// manager rather than reimplementing process supervision, so a service started
// here is the same service the machine starts at boot.
//
// Scope is discovered from where the unit actually lives — a user install
// (`systemctl --user`) must not silently fall back to the system manager and
// report "not installed" for a service that is running.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::receipt::InstallReceipt;

/// Where the service definition lives, which decides how every later call is
/// addressed. Detected, never guessed: the two scopes have separate unit
/// directories and separate `systemctl` namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// systemd system unit — /etc/systemd/system/tentaflow.service
    SystemdSystem,
    /// systemd user unit — ~/.config/systemd/user/tentaflow.service
    SystemdUser,
    /// launchd agent — ~/Library/LaunchAgents/ai.tentaflow.plist
    Launchd,
}

const UNIT: &str = "tentaflow.service";
const LAUNCHD_LABEL: &str = "ai.tentaflow";

/// Candidate config paths, most specific first. The receipt wins when there is
/// one — it records the exact file the service is started with. Otherwise the
/// installer default, then a portable or repo run next to the binary.
fn config_candidates(receipt: Option<&InstallReceipt>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(r) = receipt {
        out.push(r.config.clone());
    }
    out.push(PathBuf::from("/etc/tentaflow/config.toml"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("config.toml"));
        }
    }
    out.push(PathBuf::from("config.toml"));
    out
}

fn system_unit_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(UNIT)
}

fn user_unit_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config/systemd/user")
            .join(UNIT),
    )
}

fn launchd_plist_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
    )
}

/// Finds the manager that actually owns this installation.
pub fn detect() -> Option<Manager> {
    if cfg!(target_os = "macos") {
        if launchd_plist_path().is_some_and(|p| p.exists()) {
            return Some(Manager::Launchd);
        }
    }
    if system_unit_path().exists() {
        return Some(Manager::SystemdSystem);
    }
    if user_unit_path().is_some_and(|p| p.exists()) {
        return Some(Manager::SystemdUser);
    }
    None
}

fn not_installed() -> anyhow::Error {
    anyhow!(
        "TentaFlow nie jest zarejestrowany jako usluga na tej maszynie.\n\
         Zainstaluj przez install.sh albo uruchom serwer w tle recznie: tentaflow --config <plik>"
    )
}

fn systemctl(manager: Manager, args: &[&str]) -> Result<std::process::ExitStatus> {
    let mut cmd = Command::new("systemctl");
    if manager == Manager::SystemdUser {
        cmd.arg("--user");
    }
    cmd.args(args).arg(UNIT);
    Ok(cmd.status()?)
}

/// Reads one `systemctl show` property. Returns `None` when systemd has no
/// value for it (an inactive unit reports an empty MainPID, for instance).
fn systemd_property(manager: Manager, property: &str) -> Option<String> {
    let mut cmd = Command::new("systemctl");
    if manager == Manager::SystemdUser {
        cmd.arg("--user");
    }
    let out = cmd
        .args(["show", UNIT, "--property", property, "--value"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() || value == "0" {
        None
    } else {
        Some(value)
    }
}

/// Whether the service is running right now. Absent manager or absent unit is
/// "not running" rather than an error: `update` uses this only to decide
/// whether it owes the machine a restart afterwards.
pub fn is_active() -> bool {
    let Some(manager) = detect() else {
        return false;
    };
    match manager {
        Manager::SystemdSystem | Manager::SystemdUser => {
            let mut cmd = Command::new("systemctl");
            if manager == Manager::SystemdUser {
                cmd.arg("--user");
            }
            cmd.args(["is-active", "--quiet", UNIT])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        Manager::Launchd => Command::new("launchctl")
            .args(["list", LAUNCHD_LABEL])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
    }
}

pub fn start() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["start"])?;
            if !status.success() {
                return Err(anyhow!("systemctl start nie powiodlo sie ({status})"));
            }
            println!("TentaFlow uruchomiony.");
        }
        Manager::Launchd => {
            let plist = launchd_plist_path().ok_or_else(not_installed)?;
            let uid = unsafe { libc_getuid() };
            let status = Command::new("launchctl")
                .args(["bootstrap", &format!("gui/{uid}")])
                .arg(&plist)
                .status()?;
            if !status.success() {
                return Err(anyhow!("launchctl bootstrap nie powiodlo sie ({status})"));
            }
            println!("TentaFlow uruchomiony.");
        }
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["stop"])?;
            if !status.success() {
                return Err(anyhow!("systemctl stop nie powiodlo sie ({status})"));
            }
            println!("TentaFlow zatrzymany.");
        }
        Manager::Launchd => {
            let uid = unsafe { libc_getuid() };
            let status = Command::new("launchctl")
                .args(["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
                .status()?;
            if !status.success() {
                return Err(anyhow!("launchctl bootout nie powiodlo sie ({status})"));
            }
            println!("TentaFlow zatrzymany.");
        }
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["restart"])?;
            if !status.success() {
                return Err(anyhow!("systemctl restart nie powiodlo sie ({status})"));
            }
            println!("TentaFlow zrestartowany.");
            Ok(())
        }
        Manager::Launchd => {
            // launchd has no restart verb; bootout may legitimately fail when the
            // agent is not loaded, so only the bootstrap result decides.
            let _ = stop();
            start()
        }
    }
}

// `getuid(2)` — launchd domains are per-uid (`gui/<uid>`) and there is no
// portable std API for it. Declared locally so the binary does not take a libc
// dependency for one call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Resolves the dashboard base URL from the installed configuration. Falls back
/// to the documented default port when no config file is readable — a status
/// command must still say something useful on a half-installed machine.
fn dashboard_url(receipt: Option<&InstallReceipt>) -> (String, Option<PathBuf>) {
    for path in config_candidates(receipt) {
        if !path.exists() {
            continue;
        }
        if let Ok(cfg) = tentaflow_core::config::NodeConfig::from_file(&path) {
            let bind = cfg.protocols.openai_api.bind.clone();
            let port = bind.rsplit(':').next().unwrap_or("8090").to_string();
            return (format!("https://localhost:{port}"), Some(path));
        }
    }
    ("https://localhost:8090".to_string(), None)
}

/// Asks the running server whether it is healthy. `/health` is public (no auth)
/// and the certificate is a per-installation self-signed one, so verification is
/// deliberately skipped for this localhost call only.
fn health(url: &str) -> Option<bool> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(format!("{url}/health")).send().ok()?;
    Some(resp.status().is_success())
}

pub fn status() -> Result<()> {
    let receipt = InstallReceipt::load();
    let manager = detect();
    let (url, config_path) = dashboard_url(receipt.as_ref());

    println!("TentaFlow {}", env!("CARGO_PKG_VERSION"));
    if let Some(r) = &receipt {
        println!("  edycja:    {} ({})", r.edition, r.variant);
        println!("  prefix:    {}", r.prefix.display());
        if r.version != env!("CARGO_PKG_VERSION") {
            // The running binary is not the one the installer recorded — an
            // update that swapped files without rewriting the receipt, or a
            // developer build shadowing the installed one in PATH.
            println!(
                "  UWAGA:     receipt mowi o wersji {}, a ta binarka to {}",
                r.version,
                env!("CARGO_PKG_VERSION")
            );
        }
    }
    match manager {
        None => {
            println!("  usluga:    NIEZAREJESTROWANA (brak unitu systemd / agenta launchd)");
        }
        Some(Manager::Launchd) => {
            let out = Command::new("launchctl")
                .args(["print", &format!("gui/{}/{LAUNCHD_LABEL}", unsafe { libc_getuid() })])
                .output();
            let running = out.map(|o| o.status.success()).unwrap_or(false);
            println!(
                "  usluga:    launchd, {}",
                if running { "zaladowana" } else { "niezaladowana" }
            );
        }
        Some(m) => {
            let scope = if m == Manager::SystemdUser {
                "systemd --user"
            } else {
                "systemd"
            };
            let active = systemd_property(m, "ActiveState").unwrap_or_else(|| "unknown".into());
            let sub = systemd_property(m, "SubState").unwrap_or_else(|| "-".into());
            println!("  usluga:    {scope}, {active} ({sub})");
            if let Some(pid) = systemd_property(m, "MainPID") {
                println!("  PID:       {pid}");
            }
            if let Some(since) = systemd_property(m, "ExecMainStartTimestamp") {
                println!("  od:        {since}");
            }
            let enabled = systemd_property(m, "UnitFileState").unwrap_or_else(|| "unknown".into());
            println!(
                "  autostart: {enabled}{}",
                if enabled == "enabled" {
                    ""
                } else {
                    "  (nie wstanie po restarcie systemu)"
                }
            );
        }
    }

    if let Some(r) = &receipt {
        println!("  dane:      {}", r.home.display());
    }
    match config_path {
        Some(p) => println!("  config:    {}", p.display()),
        None => println!("  config:    nie znaleziono (uzyto domyslnego portu)"),
    }
    println!("  dashboard: {url}");
    match health(&url) {
        Some(true) => println!("  health:    OK"),
        Some(false) => println!("  health:    odpowiedz bledna (serwer wstaje albo jest niesprawny)"),
        None => println!("  health:    brak odpowiedzi"),
    }
    Ok(())
}
