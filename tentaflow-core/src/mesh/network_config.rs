// =============================================================================
// Plik: network_config.rs
// Opis: Detekcja network managera i zdalna konfiguracja sieci przez sudo.
// =============================================================================

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::info;

/// Wykryty network manager na systemie
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkManager {
    Netplan,
    NetworkManager,
    SystemdNetworkd,
    Ifupdown,
    MacOS,
    Windows,
    Unknown,
}

/// Wykrywa aktywny network manager na biezacym systemie
pub fn detect_network_manager() -> NetworkManager {
    if cfg!(target_os = "macos") {
        return NetworkManager::MacOS;
    }
    if cfg!(target_os = "windows") {
        return NetworkManager::Windows;
    }

    // Linux — priorytet detekcji wg design doc
    if command_exists("netplan") {
        return NetworkManager::Netplan;
    }
    if systemctl_is_active("NetworkManager") {
        return NetworkManager::NetworkManager;
    }
    if systemctl_is_active("systemd-networkd") {
        return NetworkManager::SystemdNetworkd;
    }
    if std::path::Path::new("/etc/network/interfaces").exists() {
        return NetworkManager::Ifupdown;
    }

    NetworkManager::Unknown
}

/// Buduje komende konfiguracji sieci dla wykrytego managera
pub fn build_config_command(
    manager: &NetworkManager,
    interface: &str,
    ipv4: Option<&str>,
    netmask: Option<&str>,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    validate_interface_name(interface)?;
    if !dhcp {
        if let Some(ip) = ipv4 {
            validate_ipv4(ip)?;
        }
        if let Some(gw) = gateway {
            validate_ipv4(gw)?;
        }
    }

    let prefix = netmask
        .map(netmask_to_prefix)
        .transpose()?
        .unwrap_or(24);

    match manager {
        NetworkManager::NetworkManager => build_nm_command(interface, ipv4, prefix, gateway, dhcp),
        NetworkManager::SystemdNetworkd => {
            build_systemd_networkd_command(interface, ipv4, prefix, gateway, dhcp)
        }
        NetworkManager::Netplan => {
            build_netplan_command(interface, ipv4, prefix, gateway, dhcp)
        }
        NetworkManager::Ifupdown => {
            build_ifupdown_command(interface, ipv4, netmask.unwrap_or("255.255.255.0"), gateway, dhcp)
        }
        NetworkManager::MacOS => build_macos_command(interface, ipv4, netmask, gateway, dhcp),
        NetworkManager::Windows => build_windows_command(interface, ipv4, prefix, gateway, dhcp),
        NetworkManager::Unknown => bail!("Nie wykryto network managera na tym systemie"),
    }
}

/// Wykonuje komende z sudo, podajac haslo przez stdin pipe
pub fn execute_with_sudo(command: &str, sudo_password: &str) -> Result<String> {
    let mut child = Command::new("sudo")
        .arg("-S")
        .args(["sh", "-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Nie udalo sie uruchomic sudo")?;

    // Podaj haslo przez stdin
    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(sudo_password.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
    drop(child.stdin.take());

    let output = wait_with_timeout(&mut child, Duration::from_secs(30))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stderr.contains("incorrect password")
            || stderr.contains("Sorry, try again")
            || stderr.contains("Authentication failure")
        {
            bail!("Nieprawidlowe haslo sudo");
        }
        if stderr.contains("no tty present") || stderr.contains("requiretty") {
            bail!(
                "sudo wymaga tty — dodaj 'Defaults !requiretty' w /etc/sudoers \
                 lub uzyj polkit (pkexec)"
            );
        }
        bail!("Blad wykonania komendy: {}", stderr.trim())
    }
}

/// Wykryj managera, zbuduj komende, wykonaj z sudo
pub fn apply_network_config(
    interface: &str,
    ipv4: Option<&str>,
    netmask: Option<&str>,
    gateway: Option<&str>,
    dhcp: bool,
    sudo_password: &str,
) -> Result<String> {
    let manager = detect_network_manager();
    info!(
        manager = ?manager,
        interface = %interface,
        dhcp = dhcp,
        "Aplikowanie konfiguracji sieciowej"
    );

    let command = build_config_command(&manager, interface, ipv4, netmask, gateway, dhcp)?;
    execute_with_sudo(&command, sudo_password)
}

// ---------------------------------------------------------------------------
// Walidacja wejsc
// ---------------------------------------------------------------------------

/// Walidacja nazwy interfejsu — tylko alfanumeryczne, myslnik, podkreslenie
fn validate_interface_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("Nazwa interfejsu musi miec 1-64 znakow");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Nazwa interfejsu zawiera niedozwolone znaki (dozwolone: a-z, 0-9, -, _)");
    }
    Ok(())
}

/// Walidacja adresu IPv4 — format i zakresy oktetow
fn validate_ipv4(ip: &str) -> Result<()> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        bail!("Niepoprawny format IPv4: {}", ip);
    }
    for part in &parts {
        let octet: u16 = part
            .parse()
            .map_err(|_| anyhow::anyhow!("Niepoprawny oktet w IPv4: {}", part))?;
        if octet > 255 {
            bail!("Oktet IPv4 poza zakresem 0-255: {}", octet);
        }
    }
    Ok(())
}

/// Konwersja maski sieciowej (dotted lub CIDR) na dlugosc prefixu
fn netmask_to_prefix(mask: &str) -> Result<u8> {
    // CIDR: "/24" lub "24"
    let stripped = mask.strip_prefix('/').unwrap_or(mask);
    if let Ok(prefix) = stripped.parse::<u8>() {
        if prefix <= 32 {
            return Ok(prefix);
        }
    }

    // Dotted notation: "255.255.255.0"
    let parts: Vec<&str> = mask.split('.').collect();
    if parts.len() == 4 {
        let mut bits = 0u32;
        for part in &parts {
            let octet: u8 = part
                .parse()
                .map_err(|_| anyhow::anyhow!("Niepoprawna maska: {}", mask))?;
            bits = (bits << 8) | octet as u32;
        }
        // Policz jedynki od lewej
        let prefix = bits.leading_ones();
        // Sprawdz czy maska jest ciagla (same jedynki, potem same zera)
        if prefix + bits.trailing_zeros() == 32 || bits == 0xFFFFFFFF {
            return Ok(prefix as u8);
        }
    }

    bail!("Niepoprawna maska sieciowa: {}", mask)
}

// ---------------------------------------------------------------------------
// Komendy per network manager
// ---------------------------------------------------------------------------

fn build_nm_command(
    interface: &str,
    ipv4: Option<&str>,
    prefix: u8,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    if dhcp {
        Ok(format!(
            "nmcli con mod \"{}\" ipv4.method auto && nmcli con up \"{}\"",
            interface, interface
        ))
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mut cmd = format!(
            "nmcli con mod \"{}\" ipv4.addresses \"{}/{}\" ipv4.method manual",
            interface, ip, prefix
        );
        if let Some(gw) = gateway {
            cmd.push_str(&format!(" ipv4.gateway \"{}\"", gw));
        }
        cmd.push_str(&format!(" && nmcli con up \"{}\"", interface));
        Ok(cmd)
    }
}

fn build_systemd_networkd_command(
    interface: &str,
    ipv4: Option<&str>,
    prefix: u8,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    let network_file = format!("/etc/systemd/network/99-tentaflow-{}.network", interface);
    let content = if dhcp {
        format!(
            "[Match]\nName={}\n\n[Network]\nDHCP=yes\n",
            interface
        )
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mut net = format!(
            "[Match]\nName={}\n\n[Network]\nAddress={}/{}\n",
            interface, ip, prefix
        );
        if let Some(gw) = gateway {
            net.push_str(&format!("Gateway={}\n", gw));
        }
        net
    };

    // Zapisz plik + przeladuj
    Ok(format!(
        "printf '{}' > '{}' && networkctl reload",
        shell_escape(&content),
        network_file
    ))
}

fn build_netplan_command(
    interface: &str,
    ipv4: Option<&str>,
    prefix: u8,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    let yaml = if dhcp {
        format!(
            "network:\n  version: 2\n  ethernets:\n    {}:\n      dhcp4: true\n",
            interface
        )
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mut y = format!(
            "network:\n  version: 2\n  ethernets:\n    {}:\n      addresses:\n        - {}/{}\n",
            interface, ip, prefix
        );
        if let Some(gw) = gateway {
            y.push_str(&format!(
                "      routes:\n        - to: default\n          via: {}\n",
                gw
            ));
        }
        y
    };

    Ok(format!(
        "printf '{}' > /etc/netplan/99-tentaflow.yaml && netplan apply",
        shell_escape(&yaml)
    ))
}

fn build_ifupdown_command(
    interface: &str,
    ipv4: Option<&str>,
    netmask: &str,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    let iface_file = format!("/etc/network/interfaces.d/tentaflow-{}", interface);
    let content = if dhcp {
        format!(
            "auto {}\niface {} inet dhcp\n",
            interface, interface
        )
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mut c = format!(
            "auto {}\niface {} inet static\n  address {}\n  netmask {}\n",
            interface, interface, ip, netmask
        );
        if let Some(gw) = gateway {
            c.push_str(&format!("  gateway {}\n", gw));
        }
        c
    };

    Ok(format!(
        "printf '{}' > '{}' && ifdown {} 2>/dev/null; ifup {}",
        shell_escape(&content),
        iface_file,
        interface,
        interface
    ))
}

fn build_macos_command(
    interface: &str,
    ipv4: Option<&str>,
    netmask: Option<&str>,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    if dhcp {
        Ok(format!("networksetup -setdhcp \"{}\"", interface))
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mask = netmask.unwrap_or("255.255.255.0");
        let gw = gateway.unwrap_or("0.0.0.0");
        Ok(format!(
            "networksetup -setmanual \"{}\" {} {} {}",
            interface, ip, mask, gw
        ))
    }
}

fn build_windows_command(
    interface: &str,
    ipv4: Option<&str>,
    prefix: u8,
    gateway: Option<&str>,
    dhcp: bool,
) -> Result<String> {
    if dhcp {
        Ok(format!(
            "powershell -Command \"Set-NetIPInterface -InterfaceAlias '{}' -Dhcp Enabled\"",
            interface
        ))
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mut cmd = format!(
            "powershell -Command \"New-NetIPAddress -InterfaceAlias '{}' -IPAddress '{}' -PrefixLength {}",
            interface, ip, prefix
        );
        if let Some(gw) = gateway {
            cmd.push_str(&format!(" -DefaultGateway '{}'", gw));
        }
        cmd.push('"');
        Ok(cmd)
    }
}

// ---------------------------------------------------------------------------
// Helpery
// ---------------------------------------------------------------------------

/// Sprawdza czy polecenie istnieje w PATH
fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Sprawdza czy usluga systemd jest aktywna
fn systemctl_is_active(service: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Escapowanie tekstu do uzycia w printf '%s'
fn shell_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "'\\''")
        .replace('%', "%%")
        .replace('\n', "\\n")
}

/// Czeka na zakonczenie procesu z timeoutem
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    bail!("Przekroczono limit czasu (30s) — node nie odpowiedzial");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Blad oczekiwania na proces: {}", e),
        }
    }
}
