// ===== File: code_studio/egress/firewall.rs — what this node can actually enforce =====
//
// `egress_enforcement` is a claim about the machine, so it is probed, never
// assumed. Both probes here answer `false` for everything they cannot
// positively confirm: a missing tool, a ruleset we may not read, an output we
// cannot parse. An optimistic guess would put `firewall` in the registry and
// the UI would then promise filtering that nothing performs.
//
// The firewall rule itself is NOT created here. It is part of node setup (§21):
// an outbound rule matched on the process owner of the TentaFlow service —
// Linux `nft ... skuid` / `iptables --uid-owner`, macOS PF `user`, Windows a
// WFP rule scoped with `LocalUser`. This module only answers whether that rule
// is present and live right now.

use std::path::Path;

/// Name the node installer gives the Windows firewall rule. It is a contract:
/// the probe looks the rule up by this name.
#[cfg(target_os = "windows")]
pub const WINDOWS_EGRESS_RULE_NAME: &str = "TentaFlow Code Studio Egress";

/// True when a container runtime answers on this node. Detection is by control
/// socket, because that is what the provisioner will actually talk to — a
/// binary on `PATH` proves nothing about a running daemon.
pub fn container_runtime_present() -> bool {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        if let Some(path) = host.strip_prefix("unix://") {
            if Path::new(path).exists() {
                return true;
            }
        }
        if let Some(pipe) = host.strip_prefix("npipe://") {
            if Path::new(pipe).exists() {
                return true;
            }
        }
    }
    runtime_socket_candidates()
        .iter()
        .any(|path| Path::new(path).exists())
}

#[cfg(target_os = "windows")]
fn runtime_socket_candidates() -> Vec<String> {
    vec![
        r"\\.\pipe\docker_engine".to_string(),
        r"\\.\pipe\podman-machine-default".to_string(),
    ]
}

#[cfg(not(target_os = "windows"))]
fn runtime_socket_candidates() -> Vec<String> {
    let mut candidates = vec![
        "/var/run/docker.sock".to_string(),
        "/run/docker.sock".to_string(),
        "/run/podman/podman.sock".to_string(),
        "/var/run/crio/crio.sock".to_string(),
    ];
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        candidates.push(format!("{runtime_dir}/podman/podman.sock"));
        candidates.push(format!("{runtime_dir}/docker.sock"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(
            home.join(".docker/run/docker.sock")
                .to_string_lossy()
                .into_owned(),
        );
        candidates.push(
            home.join(".colima/default/docker.sock")
                .to_string_lossy()
                .into_owned(),
        );
    }
    candidates
}

/// True when an outbound firewall rule scoped to the service account is
/// installed and active.
pub fn uid_owner_firewall_present() -> bool {
    platform_uid_owner_rule()
}

/// Runs a probe command and returns its stdout, or `None` for anything that
/// makes the answer unknown — a non-zero exit, a missing binary, output we
/// cannot read as text.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn probe(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};

    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Whether a line matches on our uid. Written as a token comparison rather than
/// a substring search so uid 10 does not match a rule about uid 100.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn line_names_uid(line: &str, keyword: &str, uid: u32) -> bool {
    let uid = uid.to_string();
    let mut tokens = line.split(|c: char| c.is_whitespace() || c == '=' || c == '"');
    while let Some(token) = tokens.next() {
        if token != keyword {
            continue;
        }
        for next in tokens.by_ref() {
            if next.is_empty() {
                continue;
            }
            return next.trim_matches(|c: char| c == '{' || c == '}' || c == ',') == uid;
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn platform_uid_owner_rule() -> bool {
    let uid = unsafe { libc::geteuid() };

    // nftables. `skuid` is the owner match; the ruleset may only be readable by
    // root, in which case `probe` returns None and we stay pessimistic.
    if let Some(ruleset) = probe("nft", &["list", "ruleset"]) {
        if ruleset
            .lines()
            .any(|line| line_names_uid(line, "skuid", uid))
        {
            return true;
        }
    }

    // iptables/ip6tables, legacy but still what most installations run.
    for tool in ["iptables-save", "ip6tables-save"] {
        if let Some(rules) = probe(tool, &["-t", "filter"]) {
            if rules
                .lines()
                .any(|line| line_names_uid(line, "--uid-owner", uid))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn platform_uid_owner_rule() -> bool {
    // PF must be enabled AND carry a rule scoped to our uid. Either half alone
    // enforces nothing.
    let enabled = probe("pfctl", &["-s", "info"])
        .map(|info| info.contains("Status: Enabled"))
        .unwrap_or(false);
    if !enabled {
        return false;
    }
    let uid = unsafe { libc::geteuid() };
    probe("pfctl", &["-s", "rules"])
        .map(|rules| rules.lines().any(|line| line_names_uid(line, "user", uid)))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn platform_uid_owner_rule() -> bool {
    // `netsh` output is localized, so it cannot be parsed reliably; the
    // PowerShell cmdlets return invariant values. The rule must exist, be
    // enabled, block outbound traffic AND be scoped to a specific account —
    // a rule whose `LocalUser` is `Any` restricts nobody in particular.
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         $r = Get-NetFirewallRule -DisplayName '{}' | \
              Where-Object {{ $_.Direction -eq 'Outbound' -and $_.Action -eq 'Block' -and $_.Enabled -eq 'True' }}; \
         if ($r) {{ ($r | Get-NetFirewallSecurityFilter).LocalUser }}",
        WINDOWS_EGRESS_RULE_NAME
    );
    probe(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
    .map(|out| {
        let scope = out.trim();
        !scope.is_empty() && !scope.eq_ignore_ascii_case("Any")
    })
    .unwrap_or(false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_uid_owner_rule() -> bool {
    // No probe for this platform means no confirmation, which means no claim.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_uid_match_is_read_as_a_whole_token() {
        assert!(line_names_uid(
            "meta skuid 1000 counter drop",
            "skuid",
            1000
        ));
        assert!(line_names_uid(
            "-A OUTPUT -m owner --uid-owner 1000 -j REJECT",
            "--uid-owner",
            1000
        ));
        // A longer uid must not satisfy a rule about a shorter one.
        assert!(!line_names_uid("meta skuid 10000 drop", "skuid", 1000));
        assert!(!line_names_uid("meta skuid 100 drop", "skuid", 1000));
        // A ruleset that says nothing about owners says nothing about us.
        assert!(!line_names_uid("ip daddr 10.0.0.0/8 accept", "skuid", 1000));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn an_unavailable_probe_is_not_a_confirmation() {
        assert!(probe("tentaflow-no-such-binary", &["--version"]).is_none());
    }
}
