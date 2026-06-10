// =============================================================================
// File: services/backend/codex_oauth.rs — ChatGPT subscription OAuth login
//
// Real browser OAuth (PKCE) for the ChatGPT plan, identical to OpenAI's `codex`
// CLI: open `auth.openai.com/oauth/authorize`, catch the redirect on a loopback
// listener at `http://localhost:1455/auth/callback`, exchange the code for
// tokens. The user clicks a button and logs in — no pasting of any file.
//
// Flow constants (authorize params, scope, ports, token exchange) are taken
// verbatim from the openai/codex source (codex-rs/login: server.rs, pkce.rs).
//
// The loopback callback necessarily lands on the NODE running this process, so
// the dashboard browser must reach that node's localhost (the standard self-host
// case — same assumption the codex CLI makes).
// =============================================================================

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const ORIGINATOR: &str = "codex_cli_rs";
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

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

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_pkce() -> (String, String) {
    let mut bytes = [0u8; 64];
    getrandom::fill(&mut bytes).expect("OS RNG");
    let verifier = b64url(&bytes);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS RNG");
    b64url(&bytes)
}

fn build_authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", ORIGINATOR),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{ISSUER}/oauth/authorize?{qs}")
}

async fn bind_loopback() -> Result<TcpListener, String> {
    let mut last_err = String::new();
    for port in CALLBACK_PORTS {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return Ok(l),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!(
        "could not bind OAuth callback on 127.0.0.1:{CALLBACK_PORTS:?} ({last_err}) — is a `codex login` already running?"
    ))
}

/// Begin a login flow. Returns `(flow_id, authorize_url)`; the caller hands the
/// URL to the browser and then polls `poll(flow_id)`.
pub async fn start_login() -> Result<(String, String), String> {
    let listener = bind_loopback().await?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();
    let authorize_url = build_authorize_url(&redirect_uri, &challenge, &state);
    let flow_id = uuid::Uuid::new_v4().to_string();
    set_state(&flow_id, FlowState::pending());

    let flow_id_task = flow_id.clone();
    tokio::spawn(async move {
        run_callback_listener(listener, flow_id_task, redirect_uri, verifier, state).await;
    });

    Ok((flow_id, authorize_url))
}

fn parse_query(path_and_query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(qs) = path_and_query.split_once('?').map(|(_, q)| q) {
        for pair in qs.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let val = urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string());
                out.insert(k.to_string(), val);
            }
        }
    }
    out
}

async fn respond(sock: &mut tokio::net::TcpStream, title: &str, body: &str) {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui;padding:40px;text-align:center\">\
         <h2>{title}</h2><p>{body}</p></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.flush().await;
}

async fn run_callback_listener(
    listener: TcpListener,
    flow_id: String,
    redirect_uri: String,
    verifier: String,
    state: String,
) {
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let client = reqwest::Client::new();
    loop {
        let accepted = match tokio::time::timeout_at(deadline, listener.accept()).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                warn!("codex oauth accept error: {e}");
                continue;
            }
            Err(_) => {
                set_state(
                    &flow_id,
                    FlowState {
                        status: "error",
                        blob: None,
                        account_label: None,
                        error: Some("login timed out".to_string()),
                    },
                );
                return;
            }
        };
        let (mut sock, _) = accepted;

        let mut buf = vec![0u8; 8192];
        let n = match sock.read(&mut buf).await {
            Ok(n) if n > 0 => n,
            _ => continue,
        };
        let request = String::from_utf8_lossy(&buf[..n]);
        let target = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");

        if !target.starts_with("/auth/callback") {
            // Ignore favicon / probes; keep waiting for the real redirect.
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            continue;
        }

        let params = parse_query(target);
        if params.get("state").map(String::as_str) != Some(state.as_str()) {
            respond(&mut sock, "Login failed", "State mismatch — please try again.").await;
            set_state(
                &flow_id,
                FlowState {
                    status: "error",
                    blob: None,
                    account_label: None,
                    error: Some("state mismatch".to_string()),
                },
            );
            return;
        }
        let Some(code) = params.get("code").cloned() else {
            let err = params
                .get("error_description")
                .or_else(|| params.get("error"))
                .cloned()
                .unwrap_or_else(|| "no authorization code".to_string());
            respond(&mut sock, "Login failed", &err).await;
            set_state(
                &flow_id,
                FlowState {
                    status: "error",
                    blob: None,
                    account_label: None,
                    error: Some(err),
                },
            );
            return;
        };

        match exchange_code(&client, &redirect_uri, &verifier, &code).await {
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
                respond(
                    &mut sock,
                    "Login complete",
                    "You are signed in. Return to TentaFlow to finish.",
                )
                .await;
                info!("codex oauth login completed for flow {flow_id}");
                return;
            }
            Err(e) => {
                respond(&mut sock, "Login failed", &e).await;
                set_state(
                    &flow_id,
                    FlowState {
                        status: "error",
                        blob: None,
                        account_label: None,
                        error: Some(e),
                    },
                );
                return;
            }
        }
    }
}

async fn exchange_code(
    client: &reqwest::Client,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
) -> Result<(String, String, String), String> {
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(verifier),
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
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("token exchange bad response: {e}"))?;
    let id_token = v.get("id_token").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err("token exchange returned no access_token".to_string());
    }
    Ok((id_token, access_token, refresh_token))
}
