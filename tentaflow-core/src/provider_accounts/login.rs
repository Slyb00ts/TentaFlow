// ===== File: provider_accounts/login.rs — the provider sign-in, driven from Core =====
//
// A sign-in is the vendor CLI's own device-code flow, run by the account's
// bridge inside a PTY: the CLI prints a verification URL, the person opens it,
// the provider shows a code, and the code is typed back into the same terminal.
// Nothing about that is a protocol Core could re-implement, so Core does not
// try: it starts the CLI, reads its terminal output, types what the dashboard
// pasted, and then takes the credential the CLI left behind.
//
// Core keeps the flow because the dashboard must not: the browser would
// otherwise hold a half-finished sign-in, and a reload would lose the only
// handle on a running CLI. A flow here is addressed by an id that NAMES its
// node (`<node_id>:<uuid>`), so a status poll finds the node that is running
// the terminal without the caller having to remember which one it was.
//
// The material never passes through a response: it goes from the bridge into
// the credential store, sealed, in the task below, and the dashboard learns
// only that the sign-in succeeded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::provider_accounts::{
    repository as store, AccountRecord, CredentialMeta, CredentialWrite,
};
use crate::services::agent_runtime::{self, BridgeHandle};

/// How long `start` waits for the CLI to print its verification URL. Every
/// supported engine prints one within a second or two; past this the flow is
/// cancelled rather than handed back without the one thing it exists to
/// produce.
const URL_TIMEOUT: Duration = Duration::from_secs(45);

/// How long a started flow may stay unfinished before the terminal is closed.
/// A device code expires at the provider well inside this, and a PTY holding a
/// CLI forever is the failure mode the old GUI-driven loop had whenever a tab
/// was closed.
const FLOW_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The single instruction the dashboard renders for every engine: all four
/// flows are "open this address, then paste the code it shows". The text lives
/// in the five locale files, never here.
pub const INSTRUCTION_KEY: &str = "agent_accounts.login.instruction.code";

/// What a caller may learn about a flow. Deliberately not the flow itself: the
/// transcript of the terminal can contain a token (Claude prints one), and
/// nothing outside this module ever needs it.
#[derive(Debug, Clone)]
pub struct LoginSnapshot {
    pub login_id: String,
    pub node_id: String,
    pub verification_url: String,
    pub expires_at: String,
    pub state: String,
    pub provider_subject: Option<String>,
    pub plan_label: Option<String>,
    pub message_key: Option<String>,
}

struct Flow {
    account_id: String,
    engine_id: String,
    node_id: String,
    actor_user_id: String,
    bridge_flow_id: String,
    verification_url: String,
    expires_at: String,
    state: String,
    provider_subject: Option<String>,
    plan_label: Option<String>,
    message_key: Option<String>,
    /// When the flow was started. The driver closes a flow that runs past
    /// `FLOW_TIMEOUT`, but a driver that never got there — its task cancelled,
    /// or the whole runtime torn down under it — would otherwise leave an entry
    /// nothing can ever finish.
    started_at: Instant,
    /// When the flow reached `succeeded` or `failed`. A finished flow is kept
    /// only long enough for the dashboard that started it to read the outcome —
    /// see `FINISHED_RETENTION`.
    finished_at: Option<Instant>,
}

/// How long a finished sign-in stays readable. The dashboard polls every second
/// or two, so this is generous for a reload and short enough that the registry
/// cannot grow into a list of every sign-in this process ever ran.
const FINISHED_RETENTION: Duration = Duration::from_secs(5 * 60);

type Flows = Arc<Mutex<HashMap<String, Flow>>>;

fn flows() -> &'static Flows {
    static FLOWS: OnceLock<Flows> = OnceLock::new();
    FLOWS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// A sign-in this node started on ANOTHER node, on behalf of one person.
///
/// It is what makes a poll forwardable without asking the wire where to go. A
/// login id names its node, but taking that name from the request would let
/// anybody address any peer through this node — one probe per guess, each one
/// an audited operation on the far side. A forward now needs a record this node
/// wrote itself, for the person who is asking.
struct RemoteFlow {
    node_id: String,
    account_id: String,
    actor_user_id: String,
    started_at: Instant,
}

type RemoteFlows = Arc<Mutex<HashMap<String, RemoteFlow>>>;

fn remote_flows() -> &'static RemoteFlows {
    static REMOTE: OnceLock<RemoteFlows> = OnceLock::new();
    REMOTE.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn snapshot(login_id: &str, flow: &Flow) -> LoginSnapshot {
    LoginSnapshot {
        login_id: login_id.to_string(),
        node_id: flow.node_id.clone(),
        verification_url: flow.verification_url.clone(),
        expires_at: flow.expires_at.clone(),
        state: flow.state.clone(),
        provider_subject: flow.provider_subject.clone(),
        plan_label: flow.plan_label.clone(),
        message_key: flow.message_key.clone(),
    }
}

/// Whether the terminal of this sign-in is running HERE.
pub fn runs_here(login_id: &str) -> bool {
    sweep();
    flows()
        .lock()
        .map(|flows| flows.contains_key(login_id))
        .unwrap_or(false)
}

/// The node a sign-in THIS node started runs on, for the person who started it.
///
/// `None` is "this node knows of no such sign-in of yours", which is the same
/// answer for an id that never existed, for somebody else's and for one this
/// node never forwarded — a login id must not be a way to address a peer.
pub fn remote_node_of(login_id: &str, actor_user_id: &str) -> Option<String> {
    sweep();
    let remote = remote_flows().lock().ok()?;
    remote
        .get(login_id)
        .filter(|flow| flow.actor_user_id == actor_user_id)
        .map(|flow| flow.node_id.clone())
}

/// Records a sign-in this node started on `node_id` for `actor_user_id`.
pub fn remember_remote(login_id: &str, node_id: &str, account_id: &str, actor_user_id: &str) {
    sweep();
    if let Ok(mut remote) = remote_flows().lock() {
        remote.insert(
            login_id.to_string(),
            RemoteFlow {
                node_id: node_id.to_string(),
                account_id: account_id.to_string(),
                actor_user_id: actor_user_id.to_string(),
                started_at: Instant::now(),
            },
        );
    }
}

/// Drops what nobody can act on any more: finished flows past their retention,
/// flows whose deadline expired without the driver getting to them, and remote
/// records older than the far node's own flow timeout.
///
/// Called from every entry point rather than from a timer: these maps only grow
/// through `start`, and a sweep on access cannot outlive the thing it sweeps.
fn sweep() {
    if let Ok(mut flows) = flows().lock() {
        flows.retain(|_, flow| match flow.finished_at {
            Some(finished) => finished.elapsed() < FINISHED_RETENTION,
            // Unfinished, and older than the deadline its driver enforces plus
            // the grace a finished flow gets: whatever was supposed to close it
            // is not going to.
            None => flow.started_at.elapsed() < FLOW_TIMEOUT + FINISHED_RETENTION,
        });
    }
    if let Ok(mut remote) = remote_flows().lock() {
        remote.retain(|_, flow| flow.started_at.elapsed() < FLOW_TIMEOUT + FINISHED_RETENTION);
    }
}

fn read_flow(login_id: &str, actor_user_id: &str) -> Result<LoginSnapshot> {
    sweep();
    let flows = flows()
        .lock()
        .map_err(|_| anyhow!("login registry is poisoned"))?;
    let flow = flows
        .get(login_id)
        .ok_or_else(|| anyhow!("this sign-in is not running on this node"))?;
    // A sign-in is the one operation that puts a credential behind somebody's
    // name, so it answers to the person who started it and to nobody else —
    // not even to an administrator, who has their own flow to start.
    if flow.actor_user_id != actor_user_id {
        return Err(anyhow!("this sign-in belongs to another user"));
    }
    Ok(snapshot(login_id, flow))
}

// =============================================================================
// Start
// =============================================================================

/// Starts the vendor CLI's sign-in for `account` on THIS node and returns as
/// soon as the CLI has printed its verification URL.
pub async fn start(
    db: &DbPool,
    cipher: &Arc<SettingsCipher>,
    node_id: &str,
    account: &AccountRecord,
    actor_user_id: &str,
) -> Result<LoginSnapshot> {
    if account.credential_kind != "provider_login" {
        return Err(anyhow!(
            "this account authenticates with a key, so there is nothing to sign in to"
        ));
    }
    // The bridge's sign-in home starts from whatever credential this node
    // already holds, so a re-login of a replicated account continues the same
    // provider session instead of looking like a first one.
    agent_runtime::ensure_account_materialized(db, cipher, node_id, &account.account_id).await?;
    let bridge =
        agent_runtime::ensure_runtime(db, node_id, &account.account_id, &account.engine_id).await?;

    let started = bridge.post("/auth/start", json!({})).await?;
    let bridge_flow_id = started
        .get("flow_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the bridge started no sign-in"))?
        .to_string();
    let login_id = format!("{node_id}:{}", uuid::Uuid::new_v4());
    let expires_at = expiry_stamp();

    let url = match await_verification_url(&bridge, &bridge_flow_id).await {
        Ok(url) => url,
        Err(error) => {
            close_terminal(&bridge, &bridge_flow_id).await;
            return Err(error);
        }
    };

    flows()
        .lock()
        .map_err(|_| anyhow!("login registry is poisoned"))?
        .insert(
            login_id.clone(),
            Flow {
                account_id: account.account_id.clone(),
                engine_id: account.engine_id.clone(),
                node_id: node_id.to_string(),
                actor_user_id: actor_user_id.to_string(),
                bridge_flow_id: bridge_flow_id.clone(),
                verification_url: url.clone(),
                expires_at: expires_at.clone(),
                state: "awaiting_input".to_string(),
                provider_subject: None,
                plan_label: None,
                message_key: None,
                started_at: Instant::now(),
                finished_at: None,
            },
        );

    tokio::spawn(drive(
        db.clone(),
        cipher.clone(),
        node_id.to_string(),
        login_id.clone(),
        account.account_id.clone(),
        actor_user_id.to_string(),
        bridge,
        bridge_flow_id,
    ));

    Ok(LoginSnapshot {
        login_id,
        node_id: node_id.to_string(),
        verification_url: url,
        expires_at,
        state: "awaiting_input".to_string(),
        provider_subject: None,
        plan_label: None,
        message_key: None,
    })
}

fn expiry_stamp() -> String {
    (chrono::Utc::now() + chrono::Duration::from_std(FLOW_TIMEOUT).unwrap_or_default())
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Reads the sign-in terminal until the CLI prints the address the person has
/// to open.
///
/// A failed sign-in is checked FIRST, and it is checked against the flow's own
/// outcome rather than the text: a `codex login` that cannot reach the provider
/// prints the endpoint it failed to call, and scanning for the first address
/// would hand the person an API URL to open. Measured against the real CLI.
async fn await_verification_url(bridge: &BridgeHandle, bridge_flow_id: &str) -> Result<String> {
    let deadline = Instant::now() + URL_TIMEOUT;
    let mut after_seq = 0_u64;
    let mut transcript = String::new();
    loop {
        let events = bridge
            .get(&format!(
                "/sessions/{bridge_flow_id}/events?after_seq={after_seq}"
            ))
            .await?;
        for event in events
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            after_seq = after_seq.max(event.get("seq").and_then(Value::as_u64).unwrap_or(0));
            if let Some(text) = event.pointer("/data/text").and_then(Value::as_str) {
                transcript.push_str(text);
            }
        }
        if flow_failed(bridge, bridge_flow_id).await {
            return Err(anyhow!("the sign-in failed before it showed an address"));
        }
        if let Some(url) = first_url(&transcript) {
            return Ok(url);
        }
        if events.get("status").and_then(Value::as_str) == Some("closed") {
            return Err(anyhow!("the sign-in ended before it showed an address"));
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("the CLI did not show a sign-in address in time"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether the bridge has already declared THIS sign-in unsuccessful. A status
/// call that fails answers `false`: an unreadable status is not an outcome, and
/// the deadline above is what ends a flow nobody can read.
async fn flow_failed(bridge: &BridgeHandle, bridge_flow_id: &str) -> bool {
    let Ok(status) = bridge.get("/auth/status").await else {
        return false;
    };
    status.get("login_flow_id").and_then(Value::as_str) == Some(bridge_flow_id)
        && status.get("login_completed").and_then(Value::as_bool) == Some(false)
}

/// The first `https://` address in the terminal output.
///
/// Terminal escape sequences are stripped by the bridge for Claude and left in
/// place for the others, so the scan stops at whitespace AND at the escape
/// byte — a URL with a colour reset glued to its tail is not a URL anybody can
/// open.
fn first_url(transcript: &str) -> Option<String> {
    let start = transcript.find("https://")?;
    let tail = &transcript[start..];
    let end = tail
        .find(|c: char| c.is_whitespace() || c == '\u{1b}' || c == '"')
        .unwrap_or(tail.len());
    let url = tail[..end].trim_end_matches(['.', ',', ')', ';']);
    // A URL is only complete once the CLI has finished writing the line; a
    // bare origin is what a half-written one looks like.
    (url.len() > "https://x.y/".len()).then(|| url.to_string())
}

// =============================================================================
// Input, status, cancel
// =============================================================================

/// Types the code the person pasted into the sign-in terminal.
pub async fn input(login_id: &str, actor_user_id: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 || value.contains('\n') {
        return Err(anyhow!("a verification code is one line of text"));
    }
    sweep();
    let (account_id, bridge_flow_id) = {
        let mut flows = flows()
            .lock()
            .map_err(|_| anyhow!("login registry is poisoned"))?;
        let flow = flows
            .get_mut(login_id)
            .ok_or_else(|| anyhow!("this sign-in is not running on this node"))?;
        if flow.actor_user_id != actor_user_id {
            return Err(anyhow!("this sign-in belongs to another user"));
        }
        if flow.state != "awaiting_input" && flow.state != "awaiting_open" {
            return Err(anyhow!("this sign-in is no longer waiting for a code"));
        }
        flow.state = "verifying".to_string();
        (flow.account_id.clone(), flow.bridge_flow_id.clone())
    };
    let bridge = agent_runtime::running_bridge(&account_id)
        .await
        .ok_or_else(|| anyhow!("the sign-in terminal is no longer running"))?;
    bridge
        .post(
            &format!("/sessions/{bridge_flow_id}/input"),
            json!({"text": format!("{value}\r")}),
        )
        .await?;
    Ok(())
}

pub fn status(login_id: &str, actor_user_id: &str) -> Result<LoginSnapshot> {
    read_flow(login_id, actor_user_id)
}

/// Abandons a sign-in: the terminal is closed and the flow forgotten. The
/// account keeps whatever credential it had — a cancelled sign-in changes
/// nothing.
pub async fn cancel(login_id: &str, actor_user_id: &str) -> Result<()> {
    sweep();
    let (account_id, bridge_flow_id) = {
        let mut flows = flows()
            .lock()
            .map_err(|_| anyhow!("login registry is poisoned"))?;
        let flow = flows
            .get(login_id)
            .ok_or_else(|| anyhow!("this sign-in is not running on this node"))?;
        if flow.actor_user_id != actor_user_id {
            return Err(anyhow!("this sign-in belongs to another user"));
        }
        let addressed = (flow.account_id.clone(), flow.bridge_flow_id.clone());
        flows.remove(login_id);
        addressed
    };
    if let Some(bridge) = agent_runtime::running_bridge(&account_id).await {
        close_terminal(&bridge, &bridge_flow_id).await;
    }
    Ok(())
}

async fn close_terminal(bridge: &BridgeHandle, bridge_flow_id: &str) {
    if let Err(error) = bridge
        .call(
            reqwest::Method::DELETE,
            &format!("/sessions/{bridge_flow_id}"),
            None,
        )
        .await
    {
        tracing::warn!(%error, "the sign-in terminal did not confirm it closed");
    }
}

// =============================================================================
// The driver
// =============================================================================

/// Marks a flow's outcome and starts its retention clock.
fn finish(login_id: &str, state: &str, message_key: Option<&str>, subject: Option<String>) {
    if let Ok(mut flows) = flows().lock() {
        if let Some(flow) = flows.get_mut(login_id) {
            flow.state = state.to_string();
            flow.message_key = message_key.map(str::to_string);
            if subject.is_some() {
                flow.provider_subject = subject;
            }
            flow.finished_at = Some(Instant::now());
        }
    }
}

/// Watches one sign-in to its end and stores what it produced.
///
/// It runs detached from the request that started it, because the person is
/// away in a browser tab at a provider for most of it, and a dashboard that
/// reloads must be able to come back to a finished sign-in.
#[allow(clippy::too_many_arguments)]
async fn drive(
    db: DbPool,
    cipher: Arc<SettingsCipher>,
    node_id: String,
    login_id: String,
    account_id: String,
    actor_user_id: String,
    bridge: BridgeHandle,
    bridge_flow_id: String,
) {
    let deadline = Instant::now() + FLOW_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            close_terminal(&bridge, &bridge_flow_id).await;
            finish(
                &login_id,
                "failed",
                Some("agent_accounts.login.expired"),
                None,
            );
            return;
        }
        // A flow the caller cancelled is gone from the registry; the terminal
        // was closed there and there is nothing left to watch.
        if flows()
            .lock()
            .map(|flows| !flows.contains_key(&login_id))
            .unwrap_or(true)
        {
            return;
        }
        let status = match bridge.get("/auth/status").await {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(%error, "the sign-in status could not be read");
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
        };
        let same_flow = status
            .get("login_flow_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == bridge_flow_id);
        let completed = status.get("login_completed").and_then(Value::as_bool);
        let authenticated = status
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let credential_present = status
            .get("credential_present")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if same_flow && completed == Some(false) {
            finish(
                &login_id,
                "failed",
                Some("agent_accounts.login.failed"),
                None,
            );
            return;
        }
        // Two shapes of success, and both are the CLI's own answer: an engine
        // whose status command verifies the login says `authenticated`, and one
        // that only writes a file (Muse, Grok) says the credential is there. The
        // second is "stored, unverified", which is what the account's status
        // will honestly say afterwards.
        if authenticated || (same_flow && completed == Some(true) && credential_present) {
            match adopt_credential(&db, &cipher, &node_id, &account_id, &actor_user_id, &bridge)
                .await
            {
                Ok(subject) => finish(&login_id, "succeeded", None, subject),
                Err(error) => {
                    tracing::warn!(account_id = %account_id, %error, "a finished sign-in produced no storable credential");
                    finish(
                        &login_id,
                        "failed",
                        Some("agent_accounts.login.credential_not_stored"),
                        None,
                    );
                }
            }
            close_terminal(&bridge, &bridge_flow_id).await;
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Takes the credential the sign-in produced out of the bridge and into the
/// store. Returns the provider identity the material names, when its format
/// carries one.
async fn adopt_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    node_id: &str,
    account_id: &str,
    actor_user_id: &str,
    bridge: &BridgeHandle,
) -> Result<Option<String>> {
    let (material, _sha256, identity) = agent_runtime::read_bridge_credential(bridge)
        .await?
        .ok_or_else(|| anyhow!("the sign-in left no credential"))?;
    let meta = CredentialMeta {
        provider_subject: identity.clone(),
        expires_at: None,
        refreshed_by_node: Some(node_id.to_string()),
        actor: Some(actor_user_id.to_string()),
    };
    let outcome = store::mint_credential(db, cipher, account_id, &material, &meta)?;
    let revision = match outcome {
        CredentialWrite::Applied { revision } | CredentialWrite::Unchanged { revision } => revision,
        CredentialWrite::Conflict { .. } | CredentialWrite::Stale { .. } => {
            return Err(anyhow!(
            "another node stored a different credential for this account while it was signing in"
        ))
        }
    };
    // The node that ran the sign-in is where the credential was minted, so it
    // is this account's home until somebody signs in somewhere else.
    store::update_account(
        db,
        account_id,
        &crate::provider_accounts::AccountUpdate {
            home_node_id: Some(node_id.to_string()),
            ..Default::default()
        },
        Some(actor_user_id),
    )?;
    store::set_node_state(db, account_id, node_id, revision, "ready", None)?;
    // The digest identifies WHICH credential a node holds, so it belongs in the
    // audit row the store already writes, not in a log line that ships to
    // whatever collects stdout.
    tracing::info!(%account_id, revision, "a provider sign-in was stored");
    Ok(identity)
}

/// Forgets the flows of an account that is going away, so a deleted account
/// leaves no sign-in that could still store a credential for it — and no record
/// that would let a poll be forwarded to a node still running one.
pub fn forget_account(account_id: &str) {
    if let Ok(mut flows) = flows().lock() {
        flows.retain(|_, flow| flow.account_id != account_id);
    }
    if let Ok(mut remote) = remote_flows().lock() {
        remote.retain(|_, flow| flow.account_id != account_id);
    }
}

/// The engine a flow belongs to, for the caller that has to refuse a second
/// sign-in of the same account.
pub fn engine_of_running_flow(account_id: &str) -> Option<String> {
    let flows = flows().lock().ok()?;
    flows
        .values()
        .find(|flow| {
            flow.account_id == account_id
                && matches!(
                    flow.state.as_str(),
                    "awaiting_input" | "awaiting_open" | "verifying"
                )
        })
        .map(|flow| flow.engine_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::agent_runtime::testing::fake_bridge;

    fn terminal(text: &str) -> String {
        serde_json::json!({
            "status": "open",
            "events": [{"seq": 1, "data": {"text": text}}]
        })
        .to_string()
    }

    /// A `codex login` that cannot reach the provider prints the endpoint it
    /// failed to call, so the first address in the terminal is not always an
    /// address anybody can open. The outcome the bridge reports decides, not the
    /// text — measured against the real CLI, which produced exactly this
    /// transcript with no network path to the provider.
    #[tokio::test]
    async fn a_failed_sign_in_never_hands_back_the_url_it_could_not_reach() {
        let bridge = fake_bridge(
            "codex",
            vec![
                (
                    "/auth/status",
                    serde_json::json!({
                        "authenticated": false,
                        "login_flow_id": "auth-1",
                        "login_completed": false,
                    })
                    .to_string(),
                ),
                (
                    "/sessions/auth-1/events",
                    terminal(
                        "Error logging in with device code: error sending request for url \
                         (https://auth.openai.com/api/accounts/deviceauth/usercode)",
                    ),
                ),
            ],
        )
        .await;
        let error = await_verification_url(&bridge, "auth-1")
            .await
            .expect_err("an endpoint a failed CLI named is not a sign-in address");
        assert!(error.to_string().contains("failed before"), "{error}");
    }

    /// The same loop on a sign-in that is working returns the address the person
    /// opens, and returns it as soon as the line is complete.
    #[tokio::test]
    async fn a_running_sign_in_hands_back_the_address_the_cli_printed() {
        let bridge = fake_bridge(
            "codex",
            vec![
                (
                    "/auth/status",
                    serde_json::json!({"authenticated": false, "status": "authenticating"})
                        .to_string(),
                ),
                (
                    "/sessions/auth-2/events",
                    terminal(
                        "Open https://auth.openai.com/device?code=WXYZ-1234 and paste the code",
                    ),
                ),
            ],
        )
        .await;
        assert_eq!(
            await_verification_url(&bridge, "auth-2").await.unwrap(),
            "https://auth.openai.com/device?code=WXYZ-1234"
        );
    }

    /// A poll is forwarded on THIS node's own record of having started the
    /// sign-in, never on the node name inside the id the caller sent: that name
    /// would otherwise let anybody address any peer, one guess at a time.
    #[test]
    fn a_poll_is_forwarded_only_for_a_sign_in_this_node_started() {
        remember_remote("node-7:abc", "node-7", "acc-remote", "alice");
        assert_eq!(
            remote_node_of("node-7:abc", "alice").as_deref(),
            Some("node-7")
        );
        assert_eq!(
            remote_node_of("node-7:abc", "bob"),
            None,
            "somebody else's sign-in is not addressable"
        );
        assert_eq!(
            remote_node_of("node-9:never-started", "alice"),
            None,
            "a node named in an id this node never issued is not a destination"
        );
        assert!(!runs_here("node-7:abc"));

        forget_account("acc-remote");
        assert_eq!(remote_node_of("node-7:abc", "alice"), None);
    }

    #[test]
    fn the_first_complete_address_is_what_the_person_opens() {
        assert_eq!(
            first_url("Open https://auth.openai.com/device?code=ABCD and paste"),
            Some("https://auth.openai.com/device?code=ABCD".to_string())
        );
        // A colour reset glued to the tail is not part of the address.
        assert_eq!(
            first_url("go to https://x.ai/device/XYZ123\u{1b}[0m now"),
            Some("https://x.ai/device/XYZ123".to_string())
        );
        // A half-written line has no address yet.
        assert_eq!(first_url("Open https://"), None);
        assert_eq!(first_url("nothing to open"), None);
    }

    /// Every refusal here is about WHO is asking, and an unknown flow answers
    /// the same way as somebody else's: a login id is guessable enough that
    /// telling the two apart would be a probe.
    #[test]
    fn a_flow_answers_only_to_the_person_who_started_it() {
        flows().lock().unwrap().insert(
            "node-1:flow".to_string(),
            Flow {
                account_id: "acc".to_string(),
                engine_id: "codex".to_string(),
                node_id: "node-1".to_string(),
                actor_user_id: "alice".to_string(),
                bridge_flow_id: "auth-1".to_string(),
                verification_url: "https://example.test/device/1".to_string(),
                expires_at: "2026-01-01T00:00:00Z".to_string(),
                state: "awaiting_input".to_string(),
                provider_subject: None,
                plan_label: None,
                message_key: None,
                started_at: Instant::now(),
                finished_at: None,
            },
        );
        assert_eq!(
            status("node-1:flow", "alice").unwrap().verification_url,
            "https://example.test/device/1"
        );
        assert!(status("node-1:flow", "bob").is_err());
        assert!(status("node-1:missing", "alice").is_err());
        assert_eq!(engine_of_running_flow("acc").as_deref(), Some("codex"));
        assert!(runs_here("node-1:flow"));

        forget_account("acc");
        assert!(status("node-1:flow", "alice").is_err());
        assert!(engine_of_running_flow("acc").is_none());
    }

    /// A finished sign-in is readable for as long as a dashboard could still be
    /// polling it, and then it goes: the registry is a process-lifetime map, and
    /// one entry per sign-in ever run is a leak with a credential-shaped name in
    /// it (the account id, the actor and the bridge flow).
    #[test]
    fn a_finished_sign_in_is_forgotten_once_nobody_can_still_be_reading_it() {
        let flow = |started_at: Instant, finished_at: Option<Instant>| Flow {
            account_id: "acc-ttl".to_string(),
            engine_id: "codex".to_string(),
            node_id: "node-2".to_string(),
            actor_user_id: "alice".to_string(),
            bridge_flow_id: "auth-2".to_string(),
            verification_url: "https://example.test/device/2".to_string(),
            expires_at: "2026-01-01T00:00:00Z".to_string(),
            state: "awaiting_input".to_string(),
            provider_subject: None,
            plan_label: None,
            message_key: None,
            started_at,
            finished_at,
        };
        flows()
            .lock()
            .unwrap()
            .insert("node-2:ttl".to_string(), flow(Instant::now(), None));
        finish("node-2:ttl", "succeeded", None, Some("acct-7".to_string()));
        let snapshot = status("node-2:ttl", "alice").expect("the outcome is readable");
        assert_eq!(snapshot.state, "succeeded");
        assert_eq!(snapshot.provider_subject.as_deref(), Some("acct-7"));

        // The same flow, finished longer ago than anybody could be polling.
        flows().lock().unwrap().insert(
            "node-2:stale".to_string(),
            flow(
                Instant::now(),
                Some(Instant::now() - FINISHED_RETENTION - Duration::from_secs(1)),
            ),
        );
        assert!(status("node-2:stale", "alice").is_err());
        assert!(!flows().lock().unwrap().contains_key("node-2:stale"));
        assert!(
            flows().lock().unwrap().contains_key("node-2:ttl"),
            "a freshly finished flow is not swept with it"
        );

        // A flow whose driver never reached its own deadline — the task was
        // cancelled, or the runtime went down under it — is finished by nobody
        // and would otherwise stay in the map for the life of the process.
        flows().lock().unwrap().insert(
            "node-2:abandoned".to_string(),
            flow(
                Instant::now() - FLOW_TIMEOUT - FINISHED_RETENTION - Duration::from_secs(1),
                None,
            ),
        );
        assert!(status("node-2:abandoned", "alice").is_err());
        assert!(!flows().lock().unwrap().contains_key("node-2:abandoned"));
        assert!(
            flows().lock().unwrap().contains_key("node-2:ttl"),
            "a flow still inside its deadline is not swept with it"
        );
        forget_account("acc-ttl");
    }

    /// What is typed into the sign-in terminal is one line of a device code.
    /// The check runs BEFORE the flow is looked up, because the value ends up in
    /// a PTY: a newline would submit whatever follows it as a second command to
    /// the vendor CLI.
    #[tokio::test]
    async fn a_verification_code_is_one_line_and_nothing_else() {
        for refused in ["", "   ", "123456\nrm -rf /", &"9".repeat(5000)] {
            let error = input("node-2:typed", "alice", refused)
                .await
                .expect_err("refused before it can reach a terminal");
            assert!(
                error.to_string().contains("one line of text"),
                "{refused:?} answered {error}"
            );
        }
        // A well-formed code on a flow this node is not running is refused for
        // that reason, and not by the shape check.
        let error = input("node-2:typed", "alice", "123456")
            .await
            .expect_err("no such flow");
        assert!(
            error.to_string().contains("not running on this node"),
            "{error}"
        );
    }
}
