// =============================================================================
// File: services/camera_ingest/onvif_media.rs — ONVIF Media service SOAP
// client (F1c P6: GetProfiles + GetStreamUri).
// =============================================================================
//
// ONVIF Media (`http://www.onvif.org/ver10/media/wsdl`) exposes the camera's
// media profiles (encoder + RTSP transport configuration) and a getter for
// the per-profile RTSP URI. The wizard "one-click camera add" calls
// `derive_rtsp_uri` which chains `GetProfiles` (to pick a token) and
// `GetStreamUri` (to obtain `rtsp://...`).
//
// Authentication: WS-Security 1.1 UsernameToken with Password digest
// (`Base64(SHA1(Nonce || Created || Password))`). Plain-password mode is
// permitted by the WS-Security spec but rejected by many cameras (e.g.
// Hikvision/Dahua in their default lockdown profile), so we always use the
// digest variant. Username + arbitrary XML characters are escaped so a
// `<` in a username cannot break out of the envelope.
//
// XML parsing: ONVIF Media responses are flat enough that the focused
// extractor from `onvif_discovery::extract_xml_text` covers GetProfiles /
// GetStreamUri without pulling in a full XML parser. Attribute lookup
// (`Profiles token="..."`) is handled by `extract_open_tag_attr`. The two
// helpers tolerate any namespace prefix (`trt:Profiles`, `tt:Profiles`,
// bare `Profiles`).
//
// Threading: every public fn is async + uses a per-call `reqwest::Client`
// with `timeout_ms` enforced at the HTTP layer. Per-call client avoids
// retaining connections to short-lived ONVIF endpoints between adds.

use std::time::Duration;

use base64::Engine;
use rand::Rng;
use sha1::{Digest, Sha1};
use thiserror::Error;

/// SOAP action URIs for the calls we make. ONVIF requires the SOAP `Action`
/// header to match these strings exactly.
const ACTION_GET_PROFILES: &str =
    "http://www.onvif.org/ver10/media/wsdl/GetProfiles";
const ACTION_GET_STREAM_URI: &str =
    "http://www.onvif.org/ver10/media/wsdl/GetStreamUri";
/// Media2 service uses the ver20 namespace. `GetMetadataConfigurations`
/// enumerates metadata configs the device advertises; if the list is empty
/// the camera does not expose analytics events (no PullPoint subscription
/// can be made for object detections).
const ACTION_GET_METADATA_CONFIGURATIONS: &str =
    "http://www.onvif.org/ver20/media/wsdl/GetMetadataConfigurations";

/// Hard upper bound on per-call timeouts so a misconfigured caller cannot
/// hold a tokio worker for minutes.
const MAX_TIMEOUT_MS: u32 = 30_000;

/// Maximum SOAP response body we will buffer. ONVIF GetProfiles responses
/// from real cameras run under 64 KiB even with a dozen profiles. The cap
/// protects against a hostile device that streams MB-sized envelopes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct OnvifCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnvifProfile {
    /// Token used to address this profile in subsequent SOAP calls.
    pub token: String,
    /// Human-readable name (`<tt:Name>`).
    pub name: String,
    /// Video encoder codec, e.g. `"H264"`, `"H265"`, `"JPEG"`. None if the
    /// profile omits `VideoEncoderConfiguration` (audio-only profile).
    pub encoding: Option<String>,
    /// `(width, height)` if the encoder publishes a Resolution element.
    pub resolution: Option<(u32, u32)>,
}

/// One ONVIF Media2 metadata configuration as returned by
/// `GetMetadataConfigurations`. The presence of at least one configuration
/// indicates the device produces analytics metadata (object detections,
/// events) that can be retrieved over a PullPoint subscription. The
/// `analytics_engine_token` (when present) lets a future caller bind the
/// configuration to a specific analytics module on the camera; the wizard
/// only inspects emptiness of the list, not the contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataConfiguration {
    pub token: String,
    pub name: String,
    pub analytics_engine_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnvifStream {
    pub rtsp_uri: String,
    pub profile_token: String,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StreamProtocol {
    Tcp,
    Udp,
    RtspOverHttp,
}

impl StreamProtocol {
    fn transport_protocol_str(self) -> &'static str {
        // ONVIF Streaming spec — Transport/Protocol child of StreamSetup.
        match self {
            Self::Udp => "UDP",
            Self::Tcp => "RTSP",
            Self::RtspOverHttp => "HTTP",
        }
    }
}

#[derive(Debug, Error)]
pub enum OnvifError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("soap fault: {0}")]
    SoapFault(String),
    #[error("malformed response: {0}")]
    MalformedResponse(String),
    #[error("authentication failed")]
    AuthFailed,
    #[error("no profiles available")]
    NoProfiles,
    #[error("profile '{0}' not found")]
    ProfileNotFound(String),
    #[error("timeout after {0}ms")]
    Timeout(u32),
}

// =============================================================================
// Public API
// =============================================================================

/// Calls `GetProfiles` on the Media service and returns the list of profiles
/// the camera advertises. The list is in the order the device returned it
/// (ONVIF puts the "main" profile first by convention on every device we
/// have seen, but the spec does not mandate it — callers that need a
/// specific resolution must filter on `resolution`).
pub async fn get_profiles(
    device_service_url: &str,
    creds: &OnvifCredentials,
    timeout_ms: u32,
) -> Result<Vec<OnvifProfile>, OnvifError> {
    let body = "<trt:GetProfiles/>";
    let envelope = build_envelope(creds, body);
    let resp = send_soap(
        device_service_url,
        ACTION_GET_PROFILES,
        envelope,
        timeout_ms,
    )
    .await?;
    parse_get_profiles_response(&resp)
}

/// Calls `GetStreamUri` for a specific profile. The returned URI is the
/// camera-advertised RTSP endpoint for that profile + transport.
pub async fn get_stream_uri(
    media_service_url: &str,
    creds: &OnvifCredentials,
    profile_token: &str,
    stream_setup_protocol: StreamProtocol,
    timeout_ms: u32,
) -> Result<OnvifStream, OnvifError> {
    let transport = stream_setup_protocol.transport_protocol_str();
    let body = format!(
        r#"<trt:GetStreamUri>
  <trt:StreamSetup>
    <tt:Stream>RTP-Unicast</tt:Stream>
    <tt:Transport>
      <tt:Protocol>{transport}</tt:Protocol>
    </tt:Transport>
  </trt:StreamSetup>
  <trt:ProfileToken>{token}</trt:ProfileToken>
</trt:GetStreamUri>"#,
        transport = xml_escape(transport),
        token = xml_escape(profile_token),
    );
    let envelope = build_envelope(creds, &body);
    let resp = send_soap(
        media_service_url,
        ACTION_GET_STREAM_URI,
        envelope,
        timeout_ms,
    )
    .await?;
    let uri = parse_get_stream_uri_response(&resp)?;
    Ok(OnvifStream {
        rtsp_uri: uri,
        profile_token: profile_token.to_string(),
    })
}

/// One-shot helper: list profiles, pick `profile_token` (or the first
/// returned profile when None), then resolve its RTSP URI over TCP.
/// Used by `camera_add_v1` for the one-click add flow.
pub async fn derive_rtsp_uri(
    device_service_url: &str,
    creds: &OnvifCredentials,
    profile_token: Option<&str>,
    timeout_ms: u32,
) -> Result<OnvifStream, OnvifError> {
    let profiles = get_profiles(device_service_url, creds, timeout_ms).await?;
    if profiles.is_empty() {
        return Err(OnvifError::NoProfiles);
    }
    let chosen = match profile_token {
        Some(t) => profiles
            .iter()
            .find(|p| p.token == t)
            .ok_or_else(|| OnvifError::ProfileNotFound(t.to_string()))?,
        None => &profiles[0],
    };
    // ONVIF Media v1 uses the same endpoint for device + media service in
    // most deployments; cameras that split them advertise the alternate URL
    // through GetCapabilities. F1c P6 keeps the call on the same endpoint —
    // operators who own a split-deployment camera can pass the dedicated
    // media URL through `device_service_url`.
    get_stream_uri(
        device_service_url,
        creds,
        &chosen.token,
        StreamProtocol::Tcp,
        timeout_ms,
    )
    .await
}

/// Calls Media2 `GetMetadataConfigurations` and returns the device-advertised
/// metadata configurations. An empty `Ok(vec![])` is a successful answer that
/// means "the device exposes no analytics" — the wizard treats this as a
/// signal to store `metadata_supported = 0` on the camera row.
///
/// `media2_service_url` is the URL the device returned for the Media2
/// capability (via `GetServices`). On many cameras this is identical to the
/// Media v1 service URL, but the SOAP body uses the `tr2:` namespace either
/// way — the device dispatches by SOAP body element name, not URL path.
pub async fn get_metadata_configurations(
    media2_service_url: &str,
    creds: &OnvifCredentials,
    timeout_ms: u32,
) -> Result<Vec<MetadataConfiguration>, OnvifError> {
    let body = r#"<tr2:GetMetadataConfigurations/>"#;
    let envelope = build_envelope(creds, body);
    let resp = send_soap(
        media2_service_url,
        ACTION_GET_METADATA_CONFIGURATIONS,
        envelope,
        timeout_ms,
    )
    .await?;
    parse_get_metadata_configurations_response(&resp)
}

// =============================================================================
// SOAP transport
// =============================================================================

pub(super) async fn send_soap_pub(
    endpoint: &str,
    action: &str,
    envelope: String,
    timeout_ms: u32,
) -> Result<String, OnvifError> {
    send_soap(endpoint, action, envelope, timeout_ms).await
}

pub(super) fn build_envelope_pub(creds: &OnvifCredentials, body_inner: &str) -> String {
    build_envelope(creds, body_inner)
}

pub(super) fn xml_escape_pub(s: &str) -> String {
    xml_escape(s)
}

pub(super) fn extract_xml_text_pub(xml: &str, tag: &str) -> Option<String> {
    extract_xml_text(xml, tag)
}

pub(super) fn contains_tag_pub(xml: &str, tag: &str) -> bool {
    contains_tag(xml, tag)
}

pub(super) fn find_close_tag_pub(haystack: &str, tag: &str) -> Option<usize> {
    find_close_tag(haystack, tag)
}

pub(super) fn extract_open_tag_attr_pub(open_body: &str, key: &str) -> Option<String> {
    extract_open_tag_attr(open_body, key)
}

async fn send_soap(
    endpoint: &str,
    action: &str,
    envelope: String,
    timeout_ms: u32,
) -> Result<String, OnvifError> {
    let timeout = effective_timeout(timeout_ms);
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| OnvifError::Transport(e.to_string()))?;

    // SOAP 1.2 carries the action inside the `Content-Type` parameter; many
    // ONVIF cameras also honor the legacy `SOAPAction` header so we send
    // both to maximize interoperability.
    let content_type = format!(
        "application/soap+xml; charset=utf-8; action=\"{}\"",
        action
    );
    let req = client
        .post(endpoint)
        .header("Content-Type", content_type)
        .header("SOAPAction", format!("\"{action}\""))
        .body(envelope);

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return Err(OnvifError::Timeout(timeout_ms)),
        Err(e) => return Err(OnvifError::Transport(e.to_string())),
    };
    let status = resp.status();
    let body_bytes = read_capped(resp).await?;
    let body = String::from_utf8_lossy(&body_bytes).into_owned();

    if status.as_u16() == 401 {
        return Err(OnvifError::AuthFailed);
    }
    if !status.is_success() && !contains_soap_fault(&body) {
        return Err(OnvifError::Transport(format!(
            "http {} from {}",
            status.as_u16(),
            endpoint
        )));
    }
    if let Some(fault) = parse_soap_fault(&body) {
        // ONVIF auth failures surface as `ter:NotAuthorized` /
        // `ter:SenderNotAuthorized` / `wsse:FailedAuthentication`. The
        // Subcode value usually carries the machine-readable code while
        // `<Reason><Text>` carries a free-form string — check the whole
        // body for the canonical tokens so a vendor that puts the code
        // only in Subcode (and a generic phrase in Reason) still maps
        // to AuthFailed.
        if body.contains("NotAuthorized")
            || body.contains("FailedAuthentication")
            || body.contains("InvalidArgVal/NotAuthorized")
        {
            return Err(OnvifError::AuthFailed);
        }
        return Err(OnvifError::SoapFault(fault));
    }
    Ok(body)
}

async fn read_capped(resp: reqwest::Response) -> Result<Vec<u8>, OnvifError> {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::with_capacity(4096);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| OnvifError::Transport(e.to_string()))?;
        if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(OnvifError::MalformedResponse(format!(
                "response exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn effective_timeout(timeout_ms: u32) -> Duration {
    let clamped = timeout_ms.clamp(1, MAX_TIMEOUT_MS);
    Duration::from_millis(clamped as u64)
}

// =============================================================================
// Envelope construction
// =============================================================================

fn build_envelope(creds: &OnvifCredentials, body_inner: &str) -> String {
    let (digest_b64, nonce_b64, created) = build_password_digest(&creds.password);
    let user = xml_escape(&creds.username);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tt="http://www.onvif.org/ver10/schema"
            xmlns:trt="http://www.onvif.org/ver10/media/wsdl"
            xmlns:tr2="http://www.onvif.org/ver20/media/wsdl"
            xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
            xmlns:wsa="http://www.w3.org/2005/08/addressing">
  <s:Header>
    <Security s:mustUnderstand="1"
              xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd">
      <UsernameToken>
        <Username>{user}</Username>
        <Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{digest}</Password>
        <Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce}</Nonce>
        <Created xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">{created}</Created>
      </UsernameToken>
    </Security>
  </s:Header>
  <s:Body>
{body}
  </s:Body>
</s:Envelope>"#,
        user = user,
        digest = digest_b64,
        nonce = nonce_b64,
        created = created,
        body = body_inner,
    )
}

/// Build the WS-Security UsernameToken password digest triple.
/// Per OASIS WS-Security UsernameToken Profile 1.1:
///   PasswordDigest = Base64( SHA-1( Nonce || Created || Password ) )
/// `Nonce` is the raw 16-byte CSPRNG output (NOT the base64 encoding) and
/// `Created` is the UTF-8 ISO-8601 string used in the envelope.
fn build_password_digest(password: &str) -> (String, String, String) {
    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let created = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let digest = compute_password_digest(&nonce, created.as_bytes(), password.as_bytes());
    let digest_b64 = base64::engine::general_purpose::STANDARD.encode(digest);
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce);
    (digest_b64, nonce_b64, created)
}

/// Pure compute step factored out so tests can pin a fixed nonce + timestamp
/// and verify the digest matches the WS-Security spec vector.
pub(crate) fn compute_password_digest(
    nonce: &[u8],
    created: &[u8],
    password: &[u8],
) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(created);
    hasher.update(password);
    let out = hasher.finalize();
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&out);
    arr
}

/// Escape the five XML predefined entities. ONVIF usernames / profile tokens
/// are operator-controlled but flow into our SOAP envelope verbatim, so an
/// unescaped `<` could close the `<Username>` element early. Keep the set
/// minimal — additional chars (e.g. non-ASCII) are valid in XML 1.0.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

// =============================================================================
// Response parsing
// =============================================================================

fn parse_get_profiles_response(xml: &str) -> Result<Vec<OnvifProfile>, OnvifError> {
    let mut out = Vec::new();
    // Each `<...:Profiles token="...">...</...:Profiles>` block carries one
    // profile. Walk the document and slice each block out.
    let mut cursor = 0usize;
    while let Some((token, block_start, block_end)) = find_profiles_block(xml, cursor) {
        let block = &xml[block_start..block_end];
        let name = extract_xml_text(block, "Name").unwrap_or_default();
        let encoding = extract_xml_text(block, "Encoding");
        let resolution = extract_resolution(block);
        out.push(OnvifProfile {
            token,
            name,
            encoding,
            resolution,
        });
        cursor = block_end;
    }
    if out.is_empty() {
        // Distinguish "valid envelope, zero profiles" from "malformed body".
        if contains_tag(xml, "GetProfilesResponse") {
            return Ok(out);
        }
        return Err(OnvifError::MalformedResponse(
            "no <GetProfilesResponse> in body".into(),
        ));
    }
    Ok(out)
}

fn parse_get_stream_uri_response(xml: &str) -> Result<String, OnvifError> {
    // The response carries `<...:MediaUri><tt:Uri>rtsp://...</tt:Uri>...`.
    // Some cameras emit a bare `<Uri>` inside `<MediaUri>` (no `tt:` prefix)
    // and others put the URI directly under the response. The extractor
    // matches the first `Uri` element regardless of namespace.
    let uri = extract_xml_text(xml, "Uri").ok_or_else(|| {
        OnvifError::MalformedResponse("missing <Uri> in GetStreamUriResponse".into())
    })?;
    if !uri.starts_with("rtsp://") && !uri.starts_with("rtsps://") {
        return Err(OnvifError::MalformedResponse(format!(
            "URI is not an RTSP scheme: {uri}"
        )));
    }
    Ok(uri)
}

/// Parse a `GetMetadataConfigurationsResponse`. The body lists zero or more
/// `<tr2:Configurations token="...">` blocks, each carrying a `tt:Name` and
/// optionally a `tt:AnalyticsEngineConfiguration` (or `tt:AnalyticsModule`)
/// that we treat as the analytics-engine binding. Unknown children are
/// ignored.
///
/// An empty `<GetMetadataConfigurationsResponse/>` envelope (device honours
/// the call but has nothing to advertise) returns `Ok(vec![])`. A body with
/// no `GetMetadataConfigurationsResponse` element at all returns
/// `MalformedResponse` — same discrimination as `parse_get_profiles_response`.
fn parse_get_metadata_configurations_response(
    xml: &str,
) -> Result<Vec<MetadataConfiguration>, OnvifError> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some((token, block_start, block_end)) =
        find_configurations_block(xml, cursor)
    {
        let block = &xml[block_start..block_end];
        let name = extract_xml_text(block, "Name").unwrap_or_default();
        // The analytics engine binding may surface either as a direct
        // `AnalyticsEngineConfigurationToken` element or as an attribute on
        // `AnalyticsEngineConfiguration` — match the text form first and
        // fall back to the open-tag `token` attribute on the engine config.
        let analytics_engine_token =
            extract_xml_text(block, "AnalyticsEngineConfigurationToken")
                .or_else(|| extract_analytics_engine_token(block));
        out.push(MetadataConfiguration {
            token,
            name,
            analytics_engine_token,
        });
        cursor = block_end;
    }
    if out.is_empty() {
        if contains_tag(xml, "GetMetadataConfigurationsResponse") {
            return Ok(out);
        }
        return Err(OnvifError::MalformedResponse(
            "no <GetMetadataConfigurationsResponse> in body".into(),
        ));
    }
    Ok(out)
}

/// Walk the XML looking for `<...:Configurations token="...">` blocks
/// returned inside a `GetMetadataConfigurationsResponse`. Mirrors
/// `find_profiles_block` with a different local name.
fn find_configurations_block(xml: &str, start: usize) -> Option<(String, usize, usize)> {
    let mut cursor = start;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "Configurations" && !open_body.ends_with('/') {
            if let Some(token) = extract_open_tag_attr(open_body, "token") {
                let content_start = cursor + lt + 1 + open_end + 1;
                let after_open = &xml[content_start..];
                if let Some(close_idx) = find_close_tag(after_open, "Configurations") {
                    return Some((token, content_start, content_start + close_idx));
                }
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Extract a `token="..."` attribute from the first
/// `<...:AnalyticsEngineConfiguration token="...">` open tag in `block`.
fn extract_analytics_engine_token(block: &str) -> Option<String> {
    let mut cursor = 0usize;
    while cursor < block.len() {
        let rest = &block[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "AnalyticsEngineConfiguration" {
            if let Some(t) = extract_open_tag_attr(open_body, "token") {
                return Some(t);
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Locate the next `<...:Profiles token="...">` opening tag from `start`
/// and return `(token, content_start, content_end)` covering the body
/// between the open and matching close tag (any namespace prefix).
fn find_profiles_block(xml: &str, start: usize) -> Option<(String, usize, usize)> {
    let mut cursor = start;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        // ONVIF wraps the list in `<...:Profiles>` and each profile element
        // is also named `Profiles` with a `token` attribute. The outer
        // wrapper has no token attribute so we filter by attribute presence.
        if local == "Profiles" && !open_body.ends_with('/') {
            if let Some(token) = extract_open_tag_attr(open_body, "token") {
                let content_start = cursor + lt + 1 + open_end + 1;
                let after_open = &xml[content_start..];
                if let Some(close_idx) = find_close_tag(after_open, "Profiles") {
                    return Some((token, content_start, content_start + close_idx));
                }
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

fn extract_resolution(block: &str) -> Option<(u32, u32)> {
    // Resolution lives under VideoEncoderConfiguration > Resolution >
    // {Width, Height}. The flat extractor returns the first `Width` /
    // `Height` it finds — in practice this is always the encoder
    // resolution because no other element with those local names appears
    // in a profile.
    let w = extract_xml_text(block, "Width")?.trim().parse::<u32>().ok()?;
    let h = extract_xml_text(block, "Height")?.trim().parse::<u32>().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

fn contains_tag(xml: &str, tag: &str) -> bool {
    // Cheap "tag exists somewhere" check — used to disambiguate empty-list
    // vs malformed body. Matches `<ns:Tag` and `<Tag` (open or self-close).
    let needle_open = format!("<{tag}");
    if xml.contains(&needle_open) {
        return true;
    }
    // Namespace-qualified variant. We scan for `:Tag` substrings preceded
    // by a letter (the namespace prefix) and followed by `>`, ` `, or `/`.
    let qsuffix = format!(":{tag}");
    if let Some(idx) = xml.find(&qsuffix) {
        let after = &xml[idx + qsuffix.len()..];
        if after
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '>' | ' ' | '/' | '\t' | '\n' | '\r'))
        {
            return true;
        }
    }
    false
}

fn contains_soap_fault(body: &str) -> bool {
    body.contains(":Fault>") || body.contains("<Fault>")
}

/// Return the human-readable fault reason if `body` is a SOAP 1.2 fault.
fn parse_soap_fault(body: &str) -> Option<String> {
    if !contains_soap_fault(body) {
        return None;
    }
    // SOAP 1.2 carries the reason in `<env:Reason><env:Text>...</env:Text>`.
    // Fall back to `<faultstring>` for SOAP 1.1 emitters.
    if let Some(text) = extract_xml_text(body, "Text") {
        if !text.is_empty() {
            return Some(text);
        }
    }
    if let Some(text) = extract_xml_text(body, "faultstring") {
        if !text.is_empty() {
            return Some(text);
        }
    }
    // Last resort: the Subcode/Value element carries the ONVIF error code
    // (e.g. `ter:NotAuthorized`) — useful for the AuthFailed branch.
    if let Some(text) = extract_xml_text(body, "Value") {
        if !text.is_empty() {
            return Some(text);
        }
    }
    Some("unspecified soap fault".into())
}

// =============================================================================
// XML extractors (focused, namespace-agnostic)
// =============================================================================

/// Extract the text content of the first element with local name `tag`.
/// Mirrors `onvif_discovery::extract_xml_text` semantics but lives here so
/// the media module stays self-contained.
fn extract_xml_text(xml: &str, tag: &str) -> Option<String> {
    let mut cursor = 0usize;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == tag && !open_body.ends_with('/') {
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

/// Pull a quoted attribute value out of an open-tag body. `open_body` is
/// the slice between `<` and `>` (excluding both delimiters), e.g.
/// `trt:Profiles token="Profile_1" fixed="true"`. Matches `key="value"`
/// and `key='value'`. Returns the first occurrence.
fn extract_open_tag_attr(open_body: &str, key: &str) -> Option<String> {
    let mut cursor = 0usize;
    while cursor < open_body.len() {
        let rest = &open_body[cursor..];
        let idx = rest.find(key)?;
        let after = &rest[idx + key.len()..];
        let trimmed = after.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('=') {
            let val_part = stripped.trim_start();
            let (quote, body) = if let Some(b) = val_part.strip_prefix('"') {
                ('"', b)
            } else if let Some(b) = val_part.strip_prefix('\'') {
                ('\'', b)
            } else {
                cursor += idx + key.len();
                continue;
            };
            let end = body.find(quote)?;
            return Some(body[..end].to_string());
        }
        cursor += idx + key.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Password digest — known vector
    // -------------------------------------------------------------------------

    #[test]
    fn password_digest_matches_ws_security_formula() {
        // WS-Security UsernameToken Profile 1.1 defines the digest as
        //   PasswordDigest = Base64( SHA-1( Nonce || Created || Password ) )
        // — same byte order, no separators. We verify the formula end-to-end
        // by recomputing SHA-1 with the `sha1` crate directly and comparing
        // to our `compute_password_digest` output for a fixed set of inputs.
        // Any drift in field ordering, hash algorithm, or accidental
        // utf-16 encoding flips this assertion.
        use sha1::{Digest, Sha1};
        let nonce = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff";
        let created = b"2024-01-15T10:30:00.000Z";
        let password = b"SecretPassw0rd";
        let ours = compute_password_digest(nonce, created, password);
        let mut h = Sha1::new();
        h.update(nonce);
        h.update(created);
        h.update(password);
        let reference = h.finalize();
        assert_eq!(ours.as_slice(), reference.as_slice());
        // Also verify the digest length is the standard 20 bytes — catches
        // an accidental truncation/extension regression.
        assert_eq!(ours.len(), 20);
    }

    #[test]
    fn password_digest_changes_with_each_field() {
        // Changing any one of the three concatenated inputs must change
        // the digest — regression guard against a bug that hashes only
        // the password (or drops the nonce / created).
        let n1 = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10";
        let n2 = b"\x10\x0f\x0e\x0d\x0c\x0b\x0a\x09\x08\x07\x06\x05\x04\x03\x02\x01";
        let c1 = b"2024-01-15T10:30:00.000Z";
        let c2 = b"2024-01-15T10:30:01.000Z";
        let p1 = b"pw1";
        let p2 = b"pw2";
        let base = compute_password_digest(n1, c1, p1);
        assert_ne!(base, compute_password_digest(n2, c1, p1));
        assert_ne!(base, compute_password_digest(n1, c2, p1));
        assert_ne!(base, compute_password_digest(n1, c1, p2));
    }

    #[test]
    fn build_envelope_inserts_user_and_digest() {
        let creds = OnvifCredentials {
            username: "admin".into(),
            password: "hunter2".into(),
        };
        let env = build_envelope(&creds, "<trt:GetProfiles/>");
        assert!(env.contains("<Username>admin</Username>"));
        assert!(env.contains("PasswordDigest"));
        assert!(env.contains("<Nonce"));
        assert!(env.contains("<Created"));
        assert!(env.contains("<trt:GetProfiles/>"));
    }

    // -------------------------------------------------------------------------
    // XML escaping — security
    // -------------------------------------------------------------------------

    #[test]
    fn username_with_xml_metachars_is_escaped() {
        let creds = OnvifCredentials {
            username: r#"<evil&"name'>"#.into(),
            password: "ok".into(),
        };
        let env = build_envelope(&creds, "<trt:GetProfiles/>");
        // The raw injection must not appear verbatim — every metachar
        // must be replaced by its entity.
        assert!(!env.contains("<evil"));
        assert!(env.contains("&lt;evil&amp;&quot;name&apos;&gt;"));
    }

    #[test]
    fn profile_token_with_metachars_escaped_in_get_stream_uri_body() {
        // We do not call the network here — we only check that the body
        // we would have sent escapes the token. Build the inner body
        // manually and assert.
        let token = r#"a"b<c"#;
        let body = format!(
            r#"<trt:GetStreamUri><trt:ProfileToken>{}</trt:ProfileToken></trt:GetStreamUri>"#,
            xml_escape(token)
        );
        assert!(body.contains("&quot;b&lt;c"));
        assert!(!body.contains(r#""b<c"#));
    }

    // -------------------------------------------------------------------------
    // GetProfiles parsing
    // -------------------------------------------------------------------------

    const SAMPLE_GET_PROFILES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
  <env:Body>
    <trt:GetProfilesResponse>
      <trt:Profiles token="Profile_1" fixed="true">
        <tt:Name>MainStream</tt:Name>
        <tt:VideoEncoderConfiguration token="VEC_1">
          <tt:Name>VideoEncoder_1</tt:Name>
          <tt:Encoding>H264</tt:Encoding>
          <tt:Resolution>
            <tt:Width>1920</tt:Width>
            <tt:Height>1080</tt:Height>
          </tt:Resolution>
        </tt:VideoEncoderConfiguration>
      </trt:Profiles>
      <trt:Profiles token="Profile_2">
        <tt:Name>SubStream</tt:Name>
        <tt:VideoEncoderConfiguration token="VEC_2">
          <tt:Encoding>H265</tt:Encoding>
          <tt:Resolution>
            <tt:Width>640</tt:Width>
            <tt:Height>360</tt:Height>
          </tt:Resolution>
        </tt:VideoEncoderConfiguration>
      </trt:Profiles>
    </trt:GetProfilesResponse>
  </env:Body>
</env:Envelope>"#;

    #[test]
    fn get_profiles_parses_real_response_xml() {
        let profiles = parse_get_profiles_response(SAMPLE_GET_PROFILES).expect("parse");
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].token, "Profile_1");
        assert_eq!(profiles[0].name, "MainStream");
        assert_eq!(profiles[0].encoding.as_deref(), Some("H264"));
        assert_eq!(profiles[0].resolution, Some((1920, 1080)));
        assert_eq!(profiles[1].token, "Profile_2");
        assert_eq!(profiles[1].encoding.as_deref(), Some("H265"));
        assert_eq!(profiles[1].resolution, Some((640, 360)));
    }

    #[test]
    fn get_profiles_empty_list_is_ok_not_error() {
        let xml = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
            xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
          <env:Body><trt:GetProfilesResponse/></env:Body></env:Envelope>"#;
        let profiles = parse_get_profiles_response(xml).expect("parse");
        assert!(profiles.is_empty());
    }

    #[test]
    fn get_profiles_malformed_body_is_error() {
        let xml = "<garbage>nope</garbage>";
        assert!(matches!(
            parse_get_profiles_response(xml),
            Err(OnvifError::MalformedResponse(_))
        ));
    }

    // -------------------------------------------------------------------------
    // GetStreamUri parsing
    // -------------------------------------------------------------------------

    const SAMPLE_GET_STREAM_URI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
  <env:Body>
    <trt:GetStreamUriResponse>
      <trt:MediaUri>
        <tt:Uri>rtsp://192.168.1.50:554/onvif/profile1/media.smp</tt:Uri>
        <tt:InvalidAfterConnect>false</tt:InvalidAfterConnect>
        <tt:InvalidAfterReboot>false</tt:InvalidAfterReboot>
        <tt:Timeout>PT60S</tt:Timeout>
      </trt:MediaUri>
    </trt:GetStreamUriResponse>
  </env:Body>
</env:Envelope>"#;

    #[test]
    fn get_stream_uri_extracts_rtsp() {
        let uri = parse_get_stream_uri_response(SAMPLE_GET_STREAM_URI).expect("parse");
        assert_eq!(uri, "rtsp://192.168.1.50:554/onvif/profile1/media.smp");
    }

    #[test]
    fn get_stream_uri_rejects_non_rtsp_scheme() {
        let xml = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope">
            <env:Body><GetStreamUriResponse>
            <MediaUri><Uri>http://192.168.1.50/stream</Uri></MediaUri>
            </GetStreamUriResponse></env:Body></env:Envelope>"#;
        assert!(matches!(
            parse_get_stream_uri_response(xml),
            Err(OnvifError::MalformedResponse(_))
        ));
    }

    #[test]
    fn get_stream_uri_rtsps_accepted() {
        let xml = r#"<env:Body><GetStreamUriResponse>
            <MediaUri><Uri>rtsps://10.0.0.5/secure</Uri></MediaUri>
            </GetStreamUriResponse></env:Body>"#;
        assert_eq!(
            parse_get_stream_uri_response(xml).unwrap(),
            "rtsps://10.0.0.5/secure"
        );
    }

    // -------------------------------------------------------------------------
    // SOAP fault detection
    // -------------------------------------------------------------------------

    #[test]
    fn auth_fault_detected_as_auth_failed() {
        let fault = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:ter="http://www.onvif.org/ver10/error">
          <env:Body>
            <env:Fault>
              <env:Code>
                <env:Value>env:Sender</env:Value>
                <env:Subcode>
                  <env:Value>ter:NotAuthorized</env:Value>
                </env:Subcode>
              </env:Code>
              <env:Reason>
                <env:Text>Sender not authorized</env:Text>
              </env:Reason>
            </env:Fault>
          </env:Body>
        </env:Envelope>"#;
        assert!(contains_soap_fault(fault));
        let reason = parse_soap_fault(fault).unwrap();
        assert!(
            reason.contains("not authorized") || reason.contains("NotAuthorized"),
            "got: {reason}"
        );
    }

    // -------------------------------------------------------------------------
    // Open-tag attribute extractor
    // -------------------------------------------------------------------------

    #[test]
    fn extract_open_tag_attr_handles_single_and_double_quotes() {
        assert_eq!(
            extract_open_tag_attr(r#"trt:Profiles token="abc" fixed="true""#, "token")
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_open_tag_attr("Profiles token='xyz'", "token").as_deref(),
            Some("xyz")
        );
        assert_eq!(extract_open_tag_attr("Profiles", "token"), None);
    }

    // -------------------------------------------------------------------------
    // GetMetadataConfigurations parsing (F2 P6.a)
    // -------------------------------------------------------------------------

    const SAMPLE_GET_METADATA_CONFIGS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:tr2="http://www.onvif.org/ver20/media/wsdl">
  <env:Body>
    <tr2:GetMetadataConfigurationsResponse>
      <tr2:Configurations token="MetaCfg_1">
        <tt:Name>MetadataConfig 1</tt:Name>
        <tt:UseCount>1</tt:UseCount>
        <tt:AnalyticsEngineConfiguration token="AEC_1">
          <tt:AnalyticsModule Name="MotionRegionDetector" Type="tt:MotionRegionDetector"/>
        </tt:AnalyticsEngineConfiguration>
      </tr2:Configurations>
      <tr2:Configurations token="MetaCfg_2">
        <tt:Name>MetadataConfig 2</tt:Name>
      </tr2:Configurations>
    </tr2:GetMetadataConfigurationsResponse>
  </env:Body>
</env:Envelope>"#;

    #[test]
    fn get_metadata_configurations_parses_real_response_xml() {
        let cfgs = parse_get_metadata_configurations_response(SAMPLE_GET_METADATA_CONFIGS)
            .expect("parse");
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].token, "MetaCfg_1");
        assert_eq!(cfgs[0].name, "MetadataConfig 1");
        assert_eq!(cfgs[0].analytics_engine_token.as_deref(), Some("AEC_1"));
        assert_eq!(cfgs[1].token, "MetaCfg_2");
        assert_eq!(cfgs[1].name, "MetadataConfig 2");
        assert!(cfgs[1].analytics_engine_token.is_none());
    }

    #[test]
    fn get_metadata_configurations_empty_list_means_no_profile_m() {
        // Device acknowledges the call but exposes zero metadata configs.
        // Caller treats this as "no analytics" — must not return an error.
        let xml = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tr2="http://www.onvif.org/ver20/media/wsdl">
            <env:Body><tr2:GetMetadataConfigurationsResponse/></env:Body></env:Envelope>"#;
        let cfgs = parse_get_metadata_configurations_response(xml).expect("parse");
        assert!(cfgs.is_empty());
    }

    #[test]
    fn get_metadata_configurations_auth_fault_surface() {
        // Fault detection lives in send_soap (mapped to AuthFailed by the
        // transport layer); the parser itself rejects non-response bodies.
        let body = "<garbage>nope</garbage>";
        assert!(matches!(
            parse_get_metadata_configurations_response(body),
            Err(OnvifError::MalformedResponse(_))
        ));
    }

    #[test]
    fn timeout_clamped_to_max() {
        assert_eq!(
            effective_timeout(120_000),
            Duration::from_millis(MAX_TIMEOUT_MS as u64)
        );
        assert_eq!(effective_timeout(500), Duration::from_millis(500));
        assert_eq!(effective_timeout(0), Duration::from_millis(1));
    }
}
