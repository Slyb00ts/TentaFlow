// =============================================================================
// File: api/tls_identity.rs — per-installation HTTPS certificate (rcgen)
//
// Generates an EC P-256 self-signed certificate into `<data>/tls/{cert,key}.pem`
// on first start, with SANs covering localhost, the hostname, every local IP
// and `[server.tls] extra_sans`. The certificate is regenerated when the
// desired SAN set is no longer covered (IPs changed). The certificate embedded
// in the binary is only an emergency fallback when the data dir is unusable.
// =============================================================================

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use rcgen::{CertificateParams, DnType, KeyPair, SanType, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing::{info, warn};

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";
const VALIDITY_YEARS: i32 = 10;

/// Loaded TLS identity ready for `rustls::ServerConfig::with_single_cert`.
pub struct TlsIdentity {
    pub certs: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// One subject alternative name. Ordered so a set has a stable textual form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum San {
    Dns(String),
    Ip(IpAddr),
}

impl San {
    /// `extra_sans` entries: an IP literal becomes an IP SAN, anything else
    /// a DNS SAN.
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(match trimmed.parse::<IpAddr>() {
            Ok(ip) => San::Ip(ip),
            Err(_) => San::Dns(trimmed.to_ascii_lowercase()),
        })
    }

    fn to_rcgen(&self) -> anyhow::Result<SanType> {
        Ok(match self {
            San::Dns(name) => SanType::DnsName(
                name.as_str()
                    .try_into()
                    .map_err(|e| anyhow!("invalid DNS SAN {name:?}: {e}"))?,
            ),
            San::Ip(ip) => SanType::IpAddress(*ip),
        })
    }
}

impl std::fmt::Display for San {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            San::Dns(name) => write!(f, "DNS:{name}"),
            San::Ip(ip) => write!(f, "IP:{ip}"),
        }
    }
}

/// Desired SAN set for this host: localhost, hostname, loopback, every address
/// of every interface that is up (IPv4 and IPv6), plus `extra_sans`.
pub fn desired_sans(hostname: &str, extra_sans: &[String]) -> BTreeSet<San> {
    let mut sans = BTreeSet::new();
    sans.insert(San::Dns("localhost".into()));
    if let Some(host) = San::parse(hostname) {
        sans.insert(host);
    }
    sans.insert(San::Ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)));
    sans.insert(San::Ip(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)));
    for iface in netdev::get_interfaces() {
        if !iface.is_up() {
            continue;
        }
        for net in &iface.ipv4 {
            sans.insert(San::Ip(IpAddr::V4(net.addr())));
        }
        for net in &iface.ipv6 {
            sans.insert(San::Ip(IpAddr::V6(net.addr())));
        }
    }
    for extra in extra_sans {
        if let Some(san) = San::parse(extra) {
            sans.insert(san);
        }
    }
    sans
}

/// A stored certificate is reusable only when it still covers every desired
/// SAN. Extra names in the certificate (an IP that disappeared) are harmless.
pub fn needs_regeneration(existing: &BTreeSet<San>, desired: &BTreeSet<San>) -> bool {
    !desired.is_subset(existing)
}

/// SANs of the first certificate in a PEM bundle.
pub fn sans_from_cert_pem(cert_pem: &[u8]) -> anyhow::Result<BTreeSet<San>> {
    let certs = crate::api::tls_pem::parse_certs_pem(cert_pem)?;
    let leaf = certs
        .first()
        .ok_or_else(|| anyhow!("no certificate in PEM"))?;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|e| anyhow!("x509 parse: {e}"))?;
    let mut out = BTreeSet::new();
    if let Some(ext) = cert
        .subject_alternative_name()
        .map_err(|e| anyhow!("SAN extension: {e}"))?
    {
        for name in &ext.value.general_names {
            match name {
                x509_parser::extensions::GeneralName::DNSName(dns) => {
                    out.insert(San::Dns(dns.to_ascii_lowercase()));
                }
                x509_parser::extensions::GeneralName::IPAddress(bytes) => {
                    let ip = match bytes.len() {
                        4 => Some(IpAddr::from(<[u8; 4]>::try_from(*bytes).unwrap())),
                        16 => Some(IpAddr::from(<[u8; 16]>::try_from(*bytes).unwrap())),
                        _ => None,
                    };
                    if let Some(ip) = ip {
                        out.insert(San::Ip(ip));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

fn format_sans(sans: &BTreeSet<San>) -> String {
    sans.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generates a fresh EC P-256 self-signed certificate (PEM cert, PEM key).
pub fn generate(hostname: &str, sans: &BTreeSet<San>) -> anyhow::Result<(String, String)> {
    let mut params = CertificateParams::default();
    params.subject_alt_names = sans
        .iter()
        .map(San::to_rcgen)
        .collect::<anyhow::Result<Vec<_>>>()?;
    params.distinguished_name.push(DnType::CommonName, hostname);
    params
        .distinguished_name
        .push(DnType::OrganizationName, "TentaFlow");
    let now = chrono::Utc::now();
    use chrono::Datelike;
    // Clamp the day so the +10y date exists in every year (Feb 29).
    let day = now.day().min(28) as u8;
    params.not_before = rcgen::date_time_ymd(now.year() - 1, now.month() as u8, day);
    params.not_after = rcgen::date_time_ymd(now.year() + VALIDITY_YEARS, now.month() as u8, day);
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).context("generate EC key")?;
    let cert = params
        .self_signed(&key_pair)
        .context("self-sign certificate")?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

fn parse_identity(cert_pem: &[u8], key_pem: &[u8]) -> anyhow::Result<TlsIdentity> {
    let certs = crate::api::tls_pem::parse_certs_pem(cert_pem)?;
    if certs.is_empty() {
        return Err(anyhow!("certificate PEM holds no certificate"));
    }
    let key = crate::api::tls_pem::parse_key_pem(key_pem)?;
    Ok(TlsIdentity { certs, key })
}

/// Loads `<tls_dir>/cert.pem` + `key.pem` when present and still covering the
/// desired SAN set, otherwise generates and stores a new pair. Any failure is
/// returned so the caller can fall back to the embedded certificate.
pub fn load_or_generate(
    tls_dir: &Path,
    hostname: &str,
    extra_sans: &[String],
) -> anyhow::Result<TlsIdentity> {
    std::fs::create_dir_all(tls_dir)
        .with_context(|| format!("create TLS dir {}", tls_dir.display()))?;
    let cert_path: PathBuf = tls_dir.join(CERT_FILE);
    let key_path: PathBuf = tls_dir.join(KEY_FILE);
    let desired = desired_sans(hostname, extra_sans);

    if cert_path.exists() && key_path.exists() {
        match try_reuse(&cert_path, &key_path, &desired) {
            Ok(Some(identity)) => {
                info!(path = %cert_path.display(), "TLS: using stored certificate");
                return Ok(identity);
            }
            Ok(None) => {}
            Err(e) => warn!(
                path = %cert_path.display(),
                error = %e,
                "TLS: stored certificate unusable, regenerating"
            ),
        }
    }

    let (cert_pem, key_pem) = generate(hostname, &desired)?;
    let identity = parse_identity(cert_pem.as_bytes(), key_pem.as_bytes())?;
    write_private(&key_path, &key_pem).with_context(|| format!("write {}", key_path.display()))?;
    std::fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("write {}", cert_path.display()))?;
    info!(
        path = %cert_path.display(),
        sans = %format_sans(&desired),
        "TLS: generated per-installation certificate"
    );
    Ok(identity)
}

fn try_reuse(
    cert_path: &Path,
    key_path: &Path,
    desired: &BTreeSet<San>,
) -> anyhow::Result<Option<TlsIdentity>> {
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;
    let identity = parse_identity(&cert_pem, &key_pem)?;
    let existing = sans_from_cert_pem(&cert_pem)?;
    if needs_regeneration(&existing, desired) {
        info!(
            old_sans = %format_sans(&existing),
            new_sans = %format_sans(desired),
            "TLS: local addresses changed, regenerating certificate"
        );
        return Ok(None);
    }
    Ok(Some(identity))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<San> {
        items.iter().filter_map(|s| San::parse(s)).collect()
    }

    #[test]
    fn san_parse_distinguishes_ip_and_dns() {
        assert_eq!(
            San::parse("192.168.1.5"),
            Some(San::Ip("192.168.1.5".parse().unwrap()))
        );
        assert_eq!(San::parse("::1"), Some(San::Ip("::1".parse().unwrap())));
        assert_eq!(
            San::parse(" Host.Local "),
            Some(San::Dns("host.local".into()))
        );
        assert_eq!(San::parse("  "), None);
    }

    #[test]
    fn regeneration_only_when_a_desired_san_is_missing() {
        let existing = set(&["localhost", "127.0.0.1", "10.0.0.5", "::1"]);
        assert!(!needs_regeneration(
            &existing,
            &set(&["localhost", "127.0.0.1"])
        ));
        assert!(!needs_regeneration(&existing, &existing));
        assert!(needs_regeneration(
            &existing,
            &set(&["localhost", "10.0.0.6"])
        ));
        assert!(needs_regeneration(
            &existing,
            &set(&["localhost", "newhost"])
        ));
    }

    #[test]
    fn desired_sans_include_extras_and_loopback() {
        let sans = desired_sans(
            "myhost",
            &["10.9.8.7".into(), "api.example.org".into(), "".into()],
        );
        assert!(sans.contains(&San::Dns("localhost".into())));
        assert!(sans.contains(&San::Dns("myhost".into())));
        assert!(sans.contains(&San::Ip("127.0.0.1".parse().unwrap())));
        assert!(sans.contains(&San::Ip("::1".parse().unwrap())));
        assert!(sans.contains(&San::Ip("10.9.8.7".parse().unwrap())));
        assert!(sans.contains(&San::Dns("api.example.org".into())));
    }

    #[test]
    fn generated_cert_round_trips_sans_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let extras = vec!["203.0.113.9".to_string(), "gw.example.test".to_string()];
        let first = load_or_generate(dir.path(), "unit-host", &extras).unwrap();
        assert_eq!(first.certs.len(), 1);

        let cert_pem = std::fs::read(dir.path().join(CERT_FILE)).unwrap();
        let stored = sans_from_cert_pem(&cert_pem).unwrap();
        let desired = desired_sans("unit-host", &extras);
        assert_eq!(stored, desired);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Same SAN set → reused, file bytes unchanged.
        let second = load_or_generate(dir.path(), "unit-host", &extras).unwrap();
        assert_eq!(first.certs[0].as_ref(), second.certs[0].as_ref());

        // A new extra SAN → regenerated, new cert covers it.
        let more = vec!["198.51.100.4".to_string()];
        let third = load_or_generate(dir.path(), "unit-host", &more).unwrap();
        assert_ne!(first.certs[0].as_ref(), third.certs[0].as_ref());
        let stored =
            sans_from_cert_pem(&std::fs::read(dir.path().join(CERT_FILE)).unwrap()).unwrap();
        assert!(stored.contains(&San::Ip("198.51.100.4".parse().unwrap())));
    }

    #[test]
    fn corrupt_files_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CERT_FILE), b"garbage").unwrap();
        std::fs::write(dir.path().join(KEY_FILE), b"garbage").unwrap();
        let identity = load_or_generate(dir.path(), "unit-host", &[]).unwrap();
        assert_eq!(identity.certs.len(), 1);
        assert!(sans_from_cert_pem(&std::fs::read(dir.path().join(CERT_FILE)).unwrap()).is_ok());
    }
}
