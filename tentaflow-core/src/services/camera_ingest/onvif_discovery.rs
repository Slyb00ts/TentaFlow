// =============================================================================
// File: services/camera_ingest/onvif_discovery.rs — WS-Discovery client for
// ONVIF cameras (F1b P1.D)
// =============================================================================
//
// WS-Discovery (multicast 239.255.255.250:3702): sends a SOAP Probe envelope
// targeting `NetworkVideoTransmitter` (the ONVIF camera type) and listens for
// ProbeMatches replies for `timeout`. The replies are SOAP envelopes whose
// `<d:ProbeMatch>` body carries one or more XAddrs (the device-service HTTP
// endpoints) plus Types and Scopes. Scopes encode manufacturer / model /
// location as `onvif://www.onvif.org/<category>/<value>` URIs and we extract
// the well-known ones (manufacturer + model) on a best-effort basis.
//
// We do not pull in a full XML stack — ONVIF probe responses are a stable
// shape and a focused tag extractor is enough. Anything that does not match
// the expected tag set degrades to an empty field, which the caller treats
// as "unknown".

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

/// One ONVIF device discovered on the local network.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredCamera {
    /// Source IP of the ProbeMatch UDP packet (the camera's NIC address).
    pub address: String,
    /// ONVIF device-service URLs advertised by the camera, e.g.
    /// `http://192.168.1.50/onvif/device_service`. Usually one entry.
    pub xaddrs: Vec<String>,
    /// ONVIF type tokens, e.g. `dn:NetworkVideoTransmitter`.
    pub types: Vec<String>,
    /// Raw scope URIs. Parse via `parse_scope_value(scopes, "manufacturer")`.
    pub scopes: Vec<String>,
    /// `wsa:RelatesTo` / `wsa:MessageID` from the ProbeMatch SOAP header.
    pub message_id: String,
    /// Best-effort manufacturer extracted from scopes (empty if absent).
    pub manufacturer: String,
    /// Best-effort model extracted from scopes (empty if absent).
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid probe response: {0}")]
    InvalidResponse(String),
}

/// Tunables for `discover`. Defaults match the ONVIF Discovery 1.06 spec
/// (multicast group + 3 s collection window).
pub struct DiscoveryOptions {
    pub timeout: Duration,
    /// Interface to bind on. `0.0.0.0` lets the kernel pick.
    pub bind_addr: Ipv4Addr,
    /// SOAP `Probe/Types` tokens. Default is `dn:NetworkVideoTransmitter`.
    pub probe_types: Vec<String>,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            bind_addr: Ipv4Addr::UNSPECIFIED,
            probe_types: vec!["dn:NetworkVideoTransmitter".to_string()],
        }
    }
}

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const MULTICAST_PORT: u16 = 3702;

/// Multicast WS-Discovery probe. Returns one entry per unique device (dedup
/// is keyed on the first xaddr because a single camera with multiple NICs
/// may answer twice).
pub async fn discover(opts: DiscoveryOptions) -> Result<Vec<DiscoveredCamera>, DiscoveryError> {
    let socket = UdpSocket::bind((opts.bind_addr, 0)).await?;
    socket.join_multicast_v4(MULTICAST_ADDR, opts.bind_addr)?;

    let message_id = format!("uuid:{}", uuid::Uuid::new_v4());
    let envelope = build_probe_envelope(&message_id, &opts.probe_types);
    socket
        .send_to(envelope.as_bytes(), (MULTICAST_ADDR, MULTICAST_PORT))
        .await?;

    let mut cameras: Vec<DiscoveredCamera> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let deadline = tokio::time::Instant::now() + opts.timeout;
    let mut buf = vec![0u8; 8192];

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, addr))) => {
                let body = &buf[..n];
                let ip = match addr.ip() {
                    std::net::IpAddr::V4(v) => v.to_string(),
                    std::net::IpAddr::V6(v) => v.to_string(),
                };
                if let Ok(camera) = parse_probe_match(body, &ip) {
                    let key = camera
                        .xaddrs
                        .first()
                        .cloned()
                        .unwrap_or_else(|| camera.address.clone());
                    if !key.is_empty() && seen.insert(key) {
                        cameras.push(camera);
                    }
                }
            }
            Ok(Err(e)) => return Err(DiscoveryError::Io(e)),
            // Window elapsed — return whatever we collected.
            Err(_) => break,
        }
    }

    Ok(cameras)
}

/// SOAP 1.2 envelope per ONVIF Discovery 1.06.
fn build_probe_envelope(message_id: &str, types: &[String]) -> String {
    let types_joined = types.join(" ");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope"
            xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing"
            xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
            xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
  <e:Header>
    <w:MessageID>{message_id}</w:MessageID>
    <w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>
    <w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>
  </e:Header>
  <e:Body>
    <d:Probe>
      <d:Types>{types_joined}</d:Types>
    </d:Probe>
  </e:Body>
</e:Envelope>"#
    )
}

fn parse_probe_match(body: &[u8], source_ip: &str) -> Result<DiscoveredCamera, DiscoveryError> {
    let text =
        std::str::from_utf8(body).map_err(|e| DiscoveryError::InvalidResponse(e.to_string()))?;

    let xaddrs = extract_xml_text(text, "XAddrs")
        .map(|s| s.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let types = extract_xml_text(text, "Types")
        .map(|s| s.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let scopes = extract_xml_text(text, "Scopes")
        .map(|s| s.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let message_id = extract_xml_text(text, "MessageID").unwrap_or_default();

    if xaddrs.is_empty() {
        return Err(DiscoveryError::InvalidResponse(
            "no XAddrs in ProbeMatch".into(),
        ));
    }

    let manufacturer = parse_scope_value(&scopes, "manufacturer").unwrap_or_default();
    let model = parse_scope_value(&scopes, "hardware")
        .or_else(|| parse_scope_value(&scopes, "name"))
        .unwrap_or_default();

    Ok(DiscoveredCamera {
        address: source_ip.to_string(),
        xaddrs,
        types,
        scopes,
        message_id,
        manufacturer,
        model,
    })
}

/// Tag extractor that tolerates an optional XML namespace prefix (e.g.
/// `<d:XAddrs>`, `<wsdd:XAddrs>` or `<XAddrs>` all match `tag="XAddrs"`).
/// Returns the **first** match. Comments / attributes inside the open tag
/// are skipped. The returned text is XML-trimmed (no entity decoding — ONVIF
/// payloads use plain ASCII for URIs / types / scopes).
fn extract_xml_text(xml: &str, tag: &str) -> Option<String> {
    let mut cursor = 0usize;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        // Skip closing tags, comments, processing instructions.
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        // Split possible `ns:Tag` and the rest of the open tag up to '>'.
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        // Name token = up to first whitespace or '/'.
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == tag && !open_body.ends_with('/') {
            // Locate matching close tag — allow any namespace prefix.
            let content_start = cursor + lt + 1 + open_end + 1;
            let after_open = &xml[content_start..];
            let close_idx = find_close_tag(after_open, tag)?;
            return Some(after_open[..close_idx].trim().to_string());
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

fn find_close_tag(haystack: &str, tag: &str) -> Option<usize> {
    let mut cursor = 0usize;
    while cursor < haystack.len() {
        let rest = &haystack[cursor..];
        let lt = rest.find("</")?;
        let after = &rest[lt + 2..];
        let close_end = after.find('>')?;
        let qname = after[..close_end].trim_end();
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == tag {
            return Some(cursor + lt);
        }
        cursor += lt + 2 + close_end + 1;
    }
    None
}

/// Extract a single scope value, e.g. `parse_scope_value(scopes,
/// "manufacturer")` on a scope `onvif://www.onvif.org/manufacturer/Hikvision`
/// returns `Some("Hikvision")`. Returns the first match; trailing slashes
/// are stripped. URL-decoding is intentionally **not** applied — manufacturer
/// values in real cameras are plain ASCII; if a vendor ever encodes one we
/// keep the raw form so the operator can still recognise it.
pub fn parse_scope_value(scopes: &[String], category: &str) -> Option<String> {
    let needle = format!("/{category}/");
    for s in scopes {
        if let Some(pos) = s.find(&needle) {
            let tail = &s[pos + needle.len()..];
            let value = tail.trim_end_matches('/').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_probe_envelope_contains_message_id_and_types() {
        let env = build_probe_envelope("uuid:abc-123", &["dn:NetworkVideoTransmitter".into()]);
        assert!(env.contains("uuid:abc-123"));
        assert!(env.contains("dn:NetworkVideoTransmitter"));
        assert!(env.contains("http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe"));
        assert!(env.contains("<d:Probe>"));
    }

    const SAMPLE_REPLY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing"
              xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
              xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
  <env:Header>
    <wsa:MessageID>urn:uuid:reply-1</wsa:MessageID>
    <wsa:RelatesTo>uuid:probe-1</wsa:RelatesTo>
    <wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches</wsa:Action>
  </env:Header>
  <env:Body>
    <d:ProbeMatches>
      <d:ProbeMatch>
        <d:Types>dn:NetworkVideoTransmitter tds:Device</d:Types>
        <d:Scopes>onvif://www.onvif.org/type/video_encoder onvif://www.onvif.org/manufacturer/Hikvision onvif://www.onvif.org/hardware/DS-2CD2042WD onvif://www.onvif.org/location/lab</d:Scopes>
        <d:XAddrs>http://192.168.1.50/onvif/device_service</d:XAddrs>
        <d:MetadataVersion>1</d:MetadataVersion>
      </d:ProbeMatch>
    </d:ProbeMatches>
  </env:Body>
</env:Envelope>"#;

    #[test]
    fn test_parse_probe_match_basic_response() {
        let cam = parse_probe_match(SAMPLE_REPLY.as_bytes(), "192.168.1.50").expect("parse");
        assert_eq!(cam.xaddrs, vec!["http://192.168.1.50/onvif/device_service"]);
        assert!(cam.types.contains(&"dn:NetworkVideoTransmitter".to_string()));
        assert_eq!(cam.manufacturer, "Hikvision");
        assert_eq!(cam.model, "DS-2CD2042WD");
        assert_eq!(cam.message_id, "urn:uuid:reply-1");
        assert_eq!(cam.address, "192.168.1.50");
    }

    #[test]
    fn test_parse_probe_match_missing_xaddrs_fails() {
        let payload = r#"<Envelope><Body><ProbeMatches><ProbeMatch>
            <Types>dn:NetworkVideoTransmitter</Types>
            <Scopes>onvif://www.onvif.org/manufacturer/Acme</Scopes>
            <MetadataVersion>1</MetadataVersion>
        </ProbeMatch></ProbeMatches></Body></Envelope>"#;
        let err = parse_probe_match(payload.as_bytes(), "10.0.0.2").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidResponse(_)));
    }

    #[test]
    fn test_extract_xml_text_handles_namespace_prefix() {
        let xml = "<root><d:XAddrs>http://x/y</d:XAddrs></root>";
        assert_eq!(extract_xml_text(xml, "XAddrs").as_deref(), Some("http://x/y"));
        let xml2 = "<root><XAddrs>http://a/b</XAddrs></root>";
        assert_eq!(extract_xml_text(xml2, "XAddrs").as_deref(), Some("http://a/b"));
    }

    #[test]
    fn test_parse_scope_value_finds_manufacturer() {
        let scopes = vec![
            "onvif://www.onvif.org/type/video_encoder".to_string(),
            "onvif://www.onvif.org/manufacturer/AxisCommunications".to_string(),
        ];
        assert_eq!(
            parse_scope_value(&scopes, "manufacturer").as_deref(),
            Some("AxisCommunications")
        );
        assert!(parse_scope_value(&scopes, "model").is_none());
    }

    #[test]
    fn test_extract_xml_text_skips_self_closing() {
        let xml = "<root><XAddrs/><XAddrs>real</XAddrs></root>";
        assert_eq!(extract_xml_text(xml, "XAddrs").as_deref(), Some("real"));
    }
}
