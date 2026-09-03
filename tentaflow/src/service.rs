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

use std::path::{Path, PathBuf};
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
    /// launchd DAEMON — /Library/LaunchDaemons/ai.tentaflow.plist. This is what
    /// "starts with the system" means on macOS: an agent only runs once someone
    /// logs in, which is not a server.
    LaunchdDaemon,
    /// launchd agent — ~/Library/LaunchAgents/ai.tentaflow.plist (user install)
    LaunchdAgent,
}

impl Manager {
    /// The launchd service target: `system/<label>` for a daemon, `gui/<uid>/<label>`
    /// for a per-user agent. Every launchctl verb since Yosemite addresses one.
    fn launchd_target(self) -> String {
        match self {
            Manager::LaunchdDaemon => format!("system/{LAUNCHD_LABEL}"),
            _ => format!("gui/{}/{LAUNCHD_LABEL}", unsafe { libc_getuid() }),
        }
    }

    fn launchd_domain(self) -> String {
        match self {
            Manager::LaunchdDaemon => "system".to_string(),
            _ => format!("gui/{}", unsafe { libc_getuid() }),
        }
    }
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

fn launchd_daemon_path() -> PathBuf {
    PathBuf::from("/Library/LaunchDaemons").join(format!("{LAUNCHD_LABEL}.plist"))
}

fn launchd_agent_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
    )
}

fn launchd_plist_path(manager: Manager) -> Option<PathBuf> {
    match manager {
        Manager::LaunchdDaemon => Some(launchd_daemon_path()),
        _ => launchd_agent_path(),
    }
}

/// Finds the manager that actually owns this installation.
pub fn detect() -> Option<Manager> {
    if cfg!(target_os = "macos") {
        if launchd_daemon_path().exists() {
            return Some(Manager::LaunchdDaemon);
        }
        if launchd_agent_path().is_some_and(|p| p.exists()) {
            return Some(Manager::LaunchdAgent);
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
        "TentaFlow is not registered as a service on this machine.\n\
         Install it with install.sh, or run the server yourself: tentaflow --config <file>"
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
        m => launchd_pid(m).is_some(),
    }
}

pub fn start() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["start"])?;
            if !status.success() {
                return Err(anyhow!("systemctl start failed ({status})"));
            }
            println!("TentaFlow started.");
        }
        m => {
            let plist = launchd_plist_path(m).ok_or_else(not_installed)?;
            let status = launchctl(m, &["bootstrap", &m.launchd_domain()], Some(&plist))?;
            if !status.success() {
                return Err(anyhow!("launchctl bootstrap failed ({status})"));
            }
            println!("TentaFlow started.");
        }
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["stop"])?;
            if !status.success() {
                return Err(anyhow!("systemctl stop failed ({status})"));
            }
            println!("TentaFlow stopped.");
        }
        m => {
            let status = launchctl(m, &["bootout", &m.launchd_target()], None)?;
            if !status.success() {
                return Err(anyhow!("launchctl bootout failed ({status})"));
            }
            println!("TentaFlow stopped.");
        }
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    match detect().ok_or_else(not_installed)? {
        m @ (Manager::SystemdSystem | Manager::SystemdUser) => {
            let status = systemctl(m, &["restart"])?;
            if !status.success() {
                return Err(anyhow!("systemctl restart failed ({status})"));
            }
            println!("TentaFlow restarted.");
            Ok(())
        }
        m => {
            // `kickstart -k` restarts a loaded service in one call; when nothing
            // is loaded there is nothing to kick, so fall back to a plain start.
            let status = launchctl(m, &["kickstart", "-k", &m.launchd_target()], None)?;
            if status.success() {
                println!("TentaFlow restarted.");
                return Ok(());
            }
            start()
        }
    }
}

/// Runs one launchctl verb. The system domain needs root, and a plain user
/// invocation there fails with a bare "Operation not permitted", so the call is
/// re-run under sudo rather than reported as a broken installation.
fn launchctl(
    manager: Manager,
    args: &[&str],
    plist: Option<&Path>,
) -> Result<std::process::ExitStatus> {
    let needs_root = manager == Manager::LaunchdDaemon && unsafe { libc_getuid() } != 0;
    let mut cmd = if needs_root {
        let mut c = Command::new("sudo");
        c.arg("launchctl");
        c
    } else {
        Command::new("launchctl")
    };
    cmd.args(args);
    if let Some(p) = plist {
        cmd.arg(p);
    }
    Ok(cmd.status()?)
}

/// PID of the running service, or `None` when launchd holds no running copy.
/// `launchctl print` is the only verb that reports it for a modern service
/// target; `list` predates domains and lies about daemons.
fn launchd_pid(manager: Manager) -> Option<u32> {
    let out = Command::new("launchctl")
        .args(["print", &manager.launchd_target()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = "))
        .and_then(|v| v.trim().parse().ok())
}

// `getuid(2)` — launchd domains are per-uid (`gui/<uid>`) and there is no
// portable std API for it. Declared locally so the binary does not take a libc
// dependency for one call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Where the dashboard is, and where to actually knock for `/health`.
///
/// These are two different URLs on purpose. The one printed is for a human, so
/// it says `localhost`; the one probed is built from the configured bind
/// address, because a bind is not always something `localhost` reaches — a
/// server bound to a LAN address answers there and nowhere else, and a
/// `localhost` that resolves to `::1` first misses an IPv4-only listener.
struct Endpoint {
    display: String,
    probe: String,
    config: Option<PathBuf>,
}

fn probe_url(bind: &str) -> String {
    let (host, port) = match bind.rsplit_once(':') {
        Some((h, p)) => (h.trim_matches(['[', ']']), p),
        None => ("127.0.0.1", "8090"),
    };
    // A wildcard bind is not an address to connect to; loopback is.
    let host = match host {
        "0.0.0.0" | "" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    if host.contains(':') {
        format!("https://[{host}]:{port}")
    } else {
        format!("https://{host}:{port}")
    }
}

/// Resolves the endpoint from the installed configuration. Falls back to the
/// documented default port when no config file is readable — a status command
/// must still say something useful on a half-installed machine.
fn dashboard_url(receipt: Option<&InstallReceipt>) -> Endpoint {
    for path in config_candidates(receipt) {
        if !path.exists() {
            continue;
        }
        if let Ok(cfg) = tentaflow_core::config::NodeConfig::from_file(&path) {
            let bind = cfg.protocols.openai_api.bind.clone();
            let port = bind.rsplit(':').next().unwrap_or("8090").to_string();
            return Endpoint {
                display: format!("https://localhost:{port}"),
                probe: probe_url(&bind),
                config: Some(path),
            };
        }
    }
    Endpoint {
        display: "https://localhost:8090".to_string(),
        probe: "https://127.0.0.1:8090".to_string(),
        config: None,
    }
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
    let endpoint = dashboard_url(receipt.as_ref());

    println!("TentaFlow {}", env!("CARGO_PKG_VERSION"));
    if let Some(r) = &receipt {
        println!("  edition:   {} ({})", r.edition, r.variant);
        println!("  prefix:    {}", r.prefix.display());
        if r.version != env!("CARGO_PKG_VERSION") {
            // The running binary is not the one the installer recorded — an
            // update that swapped files without rewriting the receipt, or a
            // developer build shadowing the installed one in PATH.
            println!(
                "  WARNING:   the receipt says version {}, but this binary is {}",
                r.version,
                env!("CARGO_PKG_VERSION")
            );
        }
    }
    match manager {
        None => {
            println!("  service:   NOT REGISTERED (no systemd unit / launchd agent)");
        }
        Some(m @ (Manager::LaunchdDaemon | Manager::LaunchdAgent)) => {
            let scope = if m == Manager::LaunchdDaemon {
                "launchd (daemon)"
            } else {
                "launchd (agent)"
            };
            match launchd_pid(m) {
                Some(pid) => {
                    println!("  service:   {scope}, running");
                    println!("  PID:       {pid}");
                }
                None => println!("  service:   {scope}, not loaded"),
            }
            // RunAtLoad in the plist is the autostart; a daemon plist in
            // /Library/LaunchDaemons is loaded by launchd at boot, an agent only
            // after the user logs in.
            println!(
                "  autostart: {}",
                if m == Manager::LaunchdDaemon {
                    "at system start"
                } else {
                    "after the user logs in"
                }
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
            println!("  service:   {scope}, {active} ({sub})");
            if let Some(pid) = systemd_property(m, "MainPID") {
                println!("  PID:       {pid}");
            }
            if let Some(since) = systemd_property(m, "ExecMainStartTimestamp") {
                println!("  since:     {since}");
            }
            let enabled = systemd_property(m, "UnitFileState").unwrap_or_else(|| "unknown".into());
            println!(
                "  autostart: {enabled}{}",
                if enabled == "enabled" {
                    ""
                } else {
                    "  (will not come up after a reboot)"
                }
            );
        }
    }

    if let Some(r) = &receipt {
        println!("  data:      {}", r.home.display());
    }
    match &endpoint.config {
        Some(p) => println!("  config:    {}", p.display()),
        None => println!("  config:    not found (using the default port)"),
    }
    println!("  dashboard: {}", endpoint.display);
    match health(&endpoint.probe) {
        Some(true) => println!("  health:    OK"),
        Some(false) => println!("  health:    bad response (the server is starting or unhealthy)"),
        None => println!("  health:    no response"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::probe_url;

    #[test]
    fn adres_sondy_bierze_sie_z_bindu() {
        assert_eq!(probe_url("127.0.0.1:8090"), "https://127.0.0.1:8090");
        assert_eq!(probe_url("192.168.1.10:9000"), "https://192.168.1.10:9000");
    }

    #[test]
    fn wildcard_zamienia_sie_na_loopback() {
        assert_eq!(probe_url("0.0.0.0:8090"), "https://127.0.0.1:8090");
        assert_eq!(probe_url("[::]:8090"), "https://[::1]:8090");
    }

    #[test]
    fn ipv6_dostaje_nawiasy() {
        assert_eq!(probe_url("[::1]:8090"), "https://[::1]:8090");
    }
}
