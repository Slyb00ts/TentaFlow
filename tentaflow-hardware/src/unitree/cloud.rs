// =============================================================================
// File: unitree/cloud.rs
// Purpose: Unitree cloud account client — e-mail login and the bound-device
//          list, which carries each robot's serial and the per-device AES-128
//          key a Go2 on firmware >= 1.1.15 (con_notify data2=3) needs for LAN
//          signaling. Mirrors what the official Android app sends; reference:
//          legion1581/unitree_webrtc_connect `unitree_cloud.py`.
//
//          The password is only MD5-hashed and forwarded — nothing here stores
//          it, the access token or the key. The caller decides what to keep.
// =============================================================================

use anyhow::{anyhow, bail, Context, Result};
use md5::{Digest, Md5};
use serde::Deserialize;
use std::time::Duration;

// Signing secret baked into the Unitree app: `AppSign = md5(secret + ts + nonce)`.
const APP_SIGN_SECRET: &str = "XyvkwK45hp5PHfA8";

// The API sits behind a WAF that answers an HTML block page to anything that
// does not look like the app's WebView — the User-Agent is what it checks.
const APP_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 14; SM-S931B Build/AP3A.240905.015.A2; wv) \
     AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/127.0.6533.103 Mobile Safari/537.36";

// Header values the app sends verbatim; a mismatch (AppVersion in particular)
// turns the success code 100 into 1003.
const APP_HEADERS: &[(&str, &str)] = &[
    ("DeviceId", "Samsung/Samsung/SM-S931B/s24/14/34"),
    ("DevicePlatform", "Android"),
    ("DeviceModel", "SM-S931B"),
    ("SystemVersion", "34"),
    ("AppVersion", "1.11.4"),
    ("AppLocale", "en_US"),
    ("Channel", "UMENG_CHANNEL"),
    ("AppTimezone", "UTC"),
];

const CODE_OK: i64 = 100;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Cloud region an account is registered in. Accounts created in mainland
/// China live on a separate host and are invisible from the global one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudRegion {
    Global,
    China,
}

impl CloudRegion {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "global" => Ok(Self::Global),
            "cn" => Ok(Self::China),
            other => bail!("unknown Unitree cloud region '{other}' (expected 'global' or 'cn')"),
        }
    }

    fn base_url(self) -> &'static str {
        match self {
            Self::Global => "https://global-robot-api.unitree.com/",
            Self::China => "https://robot-api.unitree.com/",
        }
    }
}

/// Robot family, which selects the account namespace (`AppName`) the cloud
/// signs against: the Go2 app has its own, the humanoids share "B2".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudAppFamily {
    Go2,
}

impl CloudAppFamily {
    fn app_name(self) -> &'static str {
        match self {
            Self::Go2 => "Go2",
        }
    }
}

/// One robot bound to the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudDevice {
    pub serial: String,
    pub alias: String,
    pub series: String,
    pub model: String,
    pub mac: String,
    pub online: Option<bool>,
    /// AES-128 key as 32 hex chars; empty when the robot's firmware does not
    /// use per-device keys (data2 < 3).
    pub aes_key: String,
}

/// Failure reported by the cloud itself (as opposed to a transport error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudApiError {
    pub action: &'static str,
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for CloudApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unitree cloud {} failed: code={} {}", self.action, self.code, self.message)
    }
}

impl std::error::Error for CloudApiError {}

#[derive(Deserialize)]
struct Envelope {
    code: i64,
    #[serde(default, rename = "errorMsg")]
    error_msg: Option<String>,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct LoginData {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Deserialize)]
struct RawDevice {
    #[serde(default)]
    sn: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    series: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    mac: Option<String>,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    gcm_key: Option<String>,
}

/// Logs into `region` with `email`/`password` and returns every robot bound
/// to the account. One call, no session kept: the access token lives only for
/// the duration of this function.
pub async fn fetch_bound_devices(
    region: CloudRegion,
    family: CloudAppFamily,
    email: &str,
    password: &str,
) -> Result<Vec<CloudDevice>> {
    let email = email.trim();
    if email.is_empty() || password.is_empty() {
        bail!("Unitree account e-mail and password are required");
    }
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(APP_USER_AGENT)
        .build()
        .context("building the Unitree cloud HTTP client")?;

    let password_md5 = hex::encode(Md5::digest(password.as_bytes()));
    let login = call(
        region,
        family,
        "",
        "login",
        client
            .post(format!("{}login/email", region.base_url()))
            .form(&[("email", email), ("password", password_md5.as_str())]),
    )
    .await?;
    let token = serde_json::from_value::<LoginData>(login)
        .context("Unitree cloud login returned no access token")?
        .access_token;
    if token.is_empty() {
        bail!("Unitree cloud login returned an empty access token");
    }

    let list = call(
        region,
        family,
        &token,
        "device list",
        client.get(format!("{}device/bind/list", region.base_url())),
    )
    .await?;
    let raw: Vec<RawDevice> = match list {
        serde_json::Value::Null => Vec::new(),
        other => serde_json::from_value(other).context("unexpected Unitree device list shape")?,
    };
    Ok(raw.into_iter().filter_map(RawDevice::into_device).collect())
}

impl RawDevice {
    fn into_device(self) -> Option<CloudDevice> {
        if self.sn.is_empty() {
            return None;
        }
        Some(CloudDevice {
            serial: self.sn,
            alias: self.alias.unwrap_or_default(),
            series: self.series.unwrap_or_default(),
            model: self.model.unwrap_or_default(),
            mac: self.mac.unwrap_or_default(),
            online: self.online,
            aes_key: self
                .key
                .filter(|k| !k.is_empty())
                .or(self.gcm_key)
                .unwrap_or_default(),
        })
    }
}

async fn call(
    region: CloudRegion,
    family: CloudAppFamily,
    token: &str,
    action: &'static str,
    request: reqwest::RequestBuilder,
) -> Result<serde_json::Value> {
    let mut request = request;
    for (name, value) in signed_headers(family, token) {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("Unitree cloud {action}: {} unreachable", region.base_url()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("Unitree cloud {action}: reading response"))?;
    let envelope: Envelope = serde_json::from_str(&body).map_err(|_| {
        anyhow!("Unitree cloud {action}: HTTP {status}, response is not the API's JSON (blocked by the cloud firewall?)")
    })?;
    if envelope.code != CODE_OK {
        return Err(CloudApiError {
            action,
            code: envelope.code,
            message: envelope.error_msg.unwrap_or_default(),
        }
        .into());
    }
    Ok(envelope.data)
}

fn signed_headers(family: CloudAppFamily, token: &str) -> Vec<(&'static str, String)> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
        .to_string();
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    let mut out: Vec<(&'static str, String)> = APP_HEADERS
        .iter()
        .map(|(k, v)| (*k, (*v).to_string()))
        .collect();
    out.push(("AppSign", app_sign(&timestamp, &nonce)));
    out.push(("AppTimestamp", timestamp));
    out.push(("AppNonce", nonce));
    out.push(("AppName", family.app_name().to_string()));
    out.push(("Token", token.to_string()));
    out
}

fn app_sign(timestamp: &str, nonce: &str) -> String {
    hex::encode(Md5::digest(format!("{APP_SIGN_SECRET}{timestamp}{nonce}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_sign_matches_reference_formula() {
        // md5("XyvkwK45hp5PHfA8" + "1700000000000" + "abc") computed with Python hashlib.
        assert_eq!(
            app_sign("1700000000000", "abc"),
            hex::encode(Md5::digest(b"XyvkwK45hp5PHfA81700000000000abc"))
        );
    }

    #[test]
    fn region_parse_rejects_unknown() {
        assert_eq!(CloudRegion::parse("global").unwrap(), CloudRegion::Global);
        assert_eq!(CloudRegion::parse("cn").unwrap(), CloudRegion::China);
        assert!(CloudRegion::parse("eu").is_err());
    }

    #[test]
    fn device_key_falls_back_to_gcm_key() {
        let raw: Vec<RawDevice> = serde_json::from_str(
            r#"[{"sn":"B42D","key":"","gcm_key":"00112233445566778899aabbccddeeff"},{"sn":""}]"#,
        )
        .unwrap();
        let devices: Vec<CloudDevice> = raw.into_iter().filter_map(RawDevice::into_device).collect();
        assert_eq!(devices.len(), 1, "a row without a serial is dropped");
        assert_eq!(devices[0].aes_key, "00112233445566778899aabbccddeeff");
    }

    /// Live check that the signed headers and this TLS stack pass the cloud's
    /// firewall: an unregistered e-mail must come back as an API error (JSON),
    /// not as the HTML block page. `cargo test -p tentaflow-hardware -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_login_reaches_the_api() {
        let err = fetch_bound_devices(
            CloudRegion::Global,
            CloudAppFamily::Go2,
            "nobody@example.invalid",
            "x",
        )
        .await
        .unwrap_err();
        let api = err.downcast_ref::<CloudApiError>().expect("API-level error, not a block page");
        assert_ne!(api.code, CODE_OK);
    }
}
