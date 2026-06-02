// =============================================================================
// Plik: web_research/security.rs
// Opis: URL validation, DNS pinning and public-address checks for web research
//       HTTP requests.
// =============================================================================

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use url::Url;

use super::error::{Result, WebResearchError};

pub fn validate_public_http_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw)
        .map_err(|e| WebResearchError::InvalidRequest(format!("invalid url: {}", e)))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(WebResearchError::PolicyDenied(
            "only http and https urls are allowed".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?;
    if is_blocked_host_literal(host) {
        return Err(WebResearchError::PolicyDenied(
            "local or metadata host is not allowed".to_string(),
        ));
    }
    if host
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == 'x' || c == 'X')
        && host.parse::<IpAddr>().is_err()
    {
        return Err(WebResearchError::PolicyDenied(
            "numeric host aliases are not allowed".to_string(),
        ));
    }
    Ok(url)
}

pub fn resolve_public_addrs(url: &Url) -> Result<Vec<SocketAddr>> {
    let host = url
        .host_str()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no port".to_string()))?;
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| WebResearchError::PolicyDenied(format!("dns resolution failed: {}", e)))?;
    let out: Vec<SocketAddr> = addrs.collect();
    if out.is_empty() {
        return Err(WebResearchError::PolicyDenied(
            "dns resolution returned no addresses".to_string(),
        ));
    }
    if out.iter().any(|addr| !is_public_ip(addr.ip())) {
        return Err(WebResearchError::PolicyDenied(
            "dns resolution points to a local or private address".to_string(),
        ));
    }
    Ok(out)
}

fn is_blocked_host_literal(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    matches!(
        host.as_str(),
        "localhost"
            | "127.0.0.1"
            | "0.0.0.0"
            | "::1"
            | "[::1]"
            | "0"
            | "169.254.169.254"
            | "metadata.google.internal"
    )
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.octets()[0] == 0
                || v4.is_broadcast())
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            if v6.segments()[0] & 0xffc0 == 0xfe80 {
                return false;
            }
            if v6.segments()[0] & 0xff00 == 0xfd00 {
                return false;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_localhost_url() {
        let err = validate_public_http_url("http://localhost/admin").unwrap_err();

        assert!(matches!(err, WebResearchError::PolicyDenied(_)));
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = validate_public_http_url("file:///etc/passwd").unwrap_err();

        assert!(matches!(err, WebResearchError::PolicyDenied(_)));
    }

    #[test]
    fn public_ip_check_blocks_private_ranges() {
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(is_public_ip("93.184.216.34".parse().unwrap()));
    }
}
