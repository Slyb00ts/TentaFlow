// =============================================================================
// File: services/backend/codex_oauth.rs — ChatGPT subscription OAuth login
//
// Real OAuth login for the ChatGPT plan using the DEVICE-CODE flow, identical to
// OpenAI's `codex login` device-code path. This works no matter where the
// browser is relative to the node (the node is headless / on another machine):
//   1. node asks `auth.openai.com/api/accounts/deviceauth/usercode` for a code,
//   2. the user opens `auth.openai.com/codex/device` in ANY browser and enters
//      the one-time code,
//   3. the node polls `/deviceauth/token` until it gets an authorization code,
//      then exchanges it at `/oauth/token` for the access/refresh tokens.
//
// Endpoints, request/response shapes and the token exchange are taken verbatim
// from the openai/codex source (codex-rs/login: device_code_auth.rs, server.rs).
// =============================================================================

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tracing::{info, warn};

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Page the user opens to enter the one-time code.
const VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct FlowState {
    status: &'static str, // "pending" | "done" | "error"
    blob: Option<String>,
    account_label: Option<String>,
    error: Option<String>,
}

impl FlowState {
    fn pending() -> Self {
        Self {
            status: "pending",
            blob: None,
            account_label: None,
            error: None,
        }
    }
    fn error(msg: impl Into<String>) -> Self {
        Self {
            status: "error",
            blob: None,
            account_label: None,
            error: Some(msg.into()),
        }
    }
}

fn flows() -> &'static Mutex<HashMap<String, FlowState>> {
    static M: OnceLock<Mutex<HashMap<String, FlowState>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_state(flow_id: &str, state: FlowState) {
    if let Ok(mut map) = flows().lock() {
        map.insert(flow_id.to_string(), state);
    }
}

/// Public status snapshot: `(status, account_label, error)`.
pub fn poll(flow_id: &str) -> (String, Option<String>, Option<String>) {
    match flows().lock().ok().and_then(|m| m.get(flow_id).cloned()) {
        Some(s) => (s.status.to_string(), s.account_label, s.error),
        None => (
            "error".to_string(),
            None,
            Some("unknown login flow".to_string()),
        ),
    }
}

/// Consume the credential blob produced by a completed flow (removes the entry).
/// The blob is the `~/.codex/auth.json`-equivalent JSON consumed by
/// `codex::parse_creds`.
pub fn take_tokens(flow_id: &str) -> Option<String> {
    let mut map = flows().lock().ok()?;
    map.remove(flow_id).and_then(|s| s.blob)
}

fn parse_interval(v: &Value) -> u64 {
    v.get("interval")
        .and_then(|i| {
            i.as_u64()
                .or_else(|| i.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
        .unwrap_or(5)
        .max(1)
}

/// Begin a device-code login. Returns `(flow_id, verification_url, user_code)`;
/// the dashboard shows the URL + code and then polls `poll(flow_id)`.
pub async fn start_login() -> Result<(String, String, String), String> {
    let client = reqwest::Client::new();
    let usercode_url = format!("{ISSUER}/api/accounts/deviceauth/usercode");
    let resp = client
        .post(&usercode_url)
        .header("Content-Type", "application/json")
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|e| format!("device-code request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("device-code request returned {status}: {body}"));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("device-code bad response: {e}"))?;
    let device_auth_id = v
        .get("device_auth_id")
        .and_then(Value::as_str)
        .ok_or("device-code response missing device_auth_id")?
        .to_string();
    let user_code = v
        .get("user_code")
        .or_else(|| v.get("usercode"))
        .and_then(Value::as_str)
        .ok_or("device-code response missing user_code")?
        .to_string();
    let interval = parse_interval(&v);

    let flow_id = uuid::Uuid::new_v4().to_string();
    set_state(&flow_id, FlowState::pending());

    let flow_id_task = flow_id.clone();
    let user_code_task = user_code.clone();
    tokio::spawn(async move {
        run_poll(flow_id_task, device_auth_id, user_code_task, interval).await;
    });

    Ok((flow_id, VERIFICATION_URL.to_string(), user_code))
}

async fn run_poll(flow_id: String, device_auth_id: String, user_code: String, interval: u64) {
    let client = reqwest::Client::new();
    let token_url = format!("{ISSUER}/api/accounts/deviceauth/token");
    let start = Instant::now();

    loop {
        if start.elapsed() >= MAX_WAIT {
            set_state(
                &flow_id,
                FlowState::error("login timed out after 15 minutes"),
            );
            return;
        }
        let resp = client
            .post(&token_url)
            .header("Content-Type", "application/json")
            .json(&json!({ "device_auth_id": device_auth_id, "user_code": user_code }))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let code_resp: Value = match r.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        set_state(
                            &flow_id,
                            FlowState::error(format!("device token parse: {e}")),
                        );
                        return;
                    }
                };
                let authorization_code = code_resp
                    .get("authorization_code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let code_verifier = code_resp
                    .get("code_verifier")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if authorization_code.is_empty() || code_verifier.is_empty() {
                    set_state(
                        &flow_id,
                        FlowState::error("device token response incomplete"),
                    );
                    return;
                }
                match exchange_code(&client, &authorization_code, &code_verifier).await {
                    Ok((id_token, access_token, refresh_token)) => {
                        let account_id = super::codex::account_id_from_jwt(&id_token);
                        let account_label = super::codex::email_from_jwt(&id_token);
                        let blob = json!({
                            "tokens": {
                                "id_token": id_token,
                                "access_token": access_token,
                                "refresh_token": refresh_token,
                                "account_id": account_id,
                            }
                        })
                        .to_string();
                        set_state(
                            &flow_id,
                            FlowState {
                                status: "done",
                                blob: Some(blob),
                                account_label,
                                error: None,
                            },
                        );
                        info!("codex device-code login completed for flow {flow_id}");
                        return;
                    }
                    Err(e) => {
                        set_state(&flow_id, FlowState::error(e));
                        return;
                    }
                }
            }
            // 403/404 = still pending (user hasn't entered the code yet).
            Ok(r)
                if r.status() == reqwest::StatusCode::FORBIDDEN
                    || r.status() == reqwest::StatusCode::NOT_FOUND =>
            {
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                set_state(
                    &flow_id,
                    FlowState::error(format!("device auth failed ({status}): {body}")),
                );
                return;
            }
            Err(e) => {
                warn!("codex device-code poll transient error: {e}");
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
        }
    }
}

/// Exchange the device-flow authorization code for tokens. `redirect_uri` and
/// `code_verifier` are the device-callback values, matching `codex login`.
async fn exchange_code(
    client: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
) -> Result<(String, String, String), String> {
    let redirect_uri = format!("{ISSUER}/deviceauth/callback");
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(authorization_code),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(code_verifier),
    );
    let resp = client
        .post(format!("{ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("token exchange request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange returned {status}: {detail}"));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("token exchange bad response: {e}"))?;
    let id_token = v
        .get("id_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err("token exchange returned no access_token".to_string());
    }
    Ok((id_token, access_token, refresh_token))
}
