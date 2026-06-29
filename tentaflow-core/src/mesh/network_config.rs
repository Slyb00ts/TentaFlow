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
    mtu: Option<u32>,
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
    if let Some(m) = mtu {
        validate_mtu(m)?;
    }

    // MTU-only update (P1-4): gdy nie podano adresu ani DHCP, ale jest MTU,
    // zmieniamy WYLACZNIE MTU bez przepisywania konfiguracji IP. To chroni
    // interfejs interconnectu — pelny zapis netplan/networkd nadpisalby jego
    // istniejace adresowanie. Robimy to runtime (`ip link set ... mtu`), bo to
    // operacja non-destructive na adresach.
    if ipv4.is_none() && gateway.is_none() && !dhcp {
        if let Some(m) = mtu {
            return build_mtu_only_command(manager, interface, m);
        }
    }

    let prefix = netmask.map(netmask_to_prefix).transpose()?.unwrap_or(24);

    match manager {
        NetworkManager::NetworkManager => {
            build_nm_command(interface, ipv4, prefix, gateway, dhcp, mtu)
        }
        NetworkManager::SystemdNetworkd => {
            build_systemd_networkd_command(interface, ipv4, prefix, gateway, dhcp, mtu)
        }
        NetworkManager::Netplan => {
            build_netplan_command(interface, ipv4, prefix, gateway, dhcp, mtu)
        }
        NetworkManager::Ifupdown => build_ifupdown_command(
            interface,
            ipv4,
            netmask.unwrap_or("255.255.255.0"),
            gateway,
            dhcp,
            mtu,
        ),
        NetworkManager::MacOS => build_macos_command(interface, ipv4, netmask, gateway, dhcp, mtu),
        NetworkManager::Windows => {
            build_windows_command(interface, ipv4, prefix, gateway, dhcp, mtu)
        }
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
    mtu: Option<u32>,
    sudo_password: &str,
) -> Result<String> {
    let manager = detect_network_manager();
    info!(
        manager = ?manager,
        interface = %interface,
        dhcp = dhcp,
        mtu = ?mtu,
        "Aplikowanie konfiguracji sieciowej"
    );

    let command = build_config_command(&manager, interface, ipv4, netmask, gateway, dhcp, mtu)?;
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

/// Buduje komende zmieniajaca TYLKO MTU interfejsu, bez dotykania adresacji IP.
/// Na Linuksie uzywamy `ip link set` (runtime) niezaleznie od managera — to
/// najprostsza droga, ktora nie nadpisuje istniejacej konfiguracji IP karty.
fn build_mtu_only_command(
    manager: &NetworkManager,
    interface: &str,
    mtu: u32,
) -> Result<String> {
    match manager {
        NetworkManager::NetworkManager
        | NetworkManager::SystemdNetworkd
        | NetworkManager::Netplan
        | NetworkManager::Ifupdown => Ok(format!("ip link set dev \"{}\" mtu {}", interface, mtu)),
        NetworkManager::MacOS => Ok(format!("ifconfig \"{}\" mtu {}", interface, mtu)),
        NetworkManager::Windows => Ok(format!(
            "netsh interface ipv4 set subinterface \"{}\" mtu={} store=persistent",
            interface, mtu
        )),
        NetworkManager::Unknown => bail!("Nie wykryto network managera na tym systemie"),
    }
}

/// Walidacja MTU — zakres akceptowany przez karty Ethernet/RoCE (jumbo do 9216).
fn validate_mtu(mtu: u32) -> Result<()> {
    if !(576..=9216).contains(&mtu) {
        bail!("MTU poza zakresem 576-9216: {}", mtu);
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
    mtu: Option<u32>,
) -> Result<String> {
    let mtu_clause = mtu
        .map(|m| format!(" 802-3-ethernet.mtu {}", m))
        .unwrap_or_default();
    if dhcp {
        Ok(format!(
            "nmcli con mod \"{}\" ipv4.method auto{} && nmcli con up \"{}\"",
            interface, mtu_clause, interface
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
        cmd.push_str(&mtu_clause);
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
    mtu: Option<u32>,
) -> Result<String> {
    let network_file = format!("/etc/systemd/network/99-tentaflow-{}.network", interface);
    // MTU nalezy do sekcji [Link], nie [Network].
    let link_section = mtu
        .map(|m| format!("\n[Link]\nMTUBytes={}\n", m))
        .unwrap_or_default();
    let content = if dhcp {
        format!(
            "[Match]\nName={}\n\n[Network]\nDHCP=yes\n{}",
            interface, link_section
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
        net.push_str(&link_section);
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
    mtu: Option<u32>,
) -> Result<String> {
    let mtu_line = mtu
        .map(|m| format!("      mtu: {}\n", m))
        .unwrap_or_default();
    let yaml = if dhcp {
        format!(
            "network:\n  version: 2\n  ethernets:\n    {}:\n      dhcp4: true\n{}",
            interface, mtu_line
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
        y.push_str(&mtu_line);
        y
    };

    Ok(format!(
        "printf '{}' > /etc/netplan/99-tentaflow-{}.yaml && netplan apply",
        shell_escape(&yaml),
        interface
    ))
}

fn build_ifupdown_command(
    interface: &str,
    ipv4: Option<&str>,
    netmask: &str,
    gateway: Option<&str>,
    dhcp: bool,
    mtu: Option<u32>,
) -> Result<String> {
    let iface_file = format!("/etc/network/interfaces.d/tentaflow-{}", interface);
    let mtu_line = mtu.map(|m| format!("  mtu {}\n", m)).unwrap_or_default();
    let content = if dhcp {
        format!(
            "auto {}\niface {} inet dhcp\n{}",
            interface, interface, mtu_line
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
        c.push_str(&mtu_line);
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
    mtu: Option<u32>,
) -> Result<String> {
    // networksetup uzywa nazwy uslugi sieciowej; MTU ustawiamy bezposrednio na
    // urzadzeniu BSD przez ifconfig (nazwa = `interface`).
    let mtu_suffix = mtu
        .map(|m| format!(" && ifconfig \"{}\" mtu {}", interface, m))
        .unwrap_or_default();
    if dhcp {
        Ok(format!(
            "networksetup -setdhcp \"{}\"{}",
            interface, mtu_suffix
        ))
    } else {
        let ip = ipv4.ok_or_else(|| anyhow::anyhow!("Adres IPv4 wymagany dla trybu static"))?;
        let mask = netmask.unwrap_or("255.255.255.0");
        let gw = gateway.unwrap_or("0.0.0.0");
        Ok(format!(
            "networksetup -setmanual \"{}\" {} {} {}{}",
            interface, ip, mask, gw, mtu_suffix
        ))
    }
}

fn build_windows_command(
    interface: &str,
    ipv4: Option<&str>,
    prefix: u8,
    gateway: Option<&str>,
    dhcp: bool,
    mtu: Option<u32>,
) -> Result<String> {
    let mtu_suffix = mtu
        .map(|m| {
            format!(
                "; netsh interface ipv4 set subinterface '{}' mtu={} store=persistent",
                interface, m
            )
        })
        .unwrap_or_default();
    if dhcp {
        Ok(format!(
            "powershell -Command \"Set-NetIPInterface -InterfaceAlias '{}' -Dhcp Enabled{}\"",
            interface, mtu_suffix
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
        cmd.push_str(&mtu_suffix);
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
