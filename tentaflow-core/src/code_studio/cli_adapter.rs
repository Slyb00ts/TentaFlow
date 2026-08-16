// ===== File: code_studio/cli_adapter.rs — the provider credential never enters the sandbox =====
//
// A vendor CLI (Codex, Claude Code) must talk to its provider, and the
// organization's credential is what pays for that. The obvious designs both
// fail:
//
//   * Handing the CLI the credential (file, env var, config) puts organization
//     key material inside a process we treat as untrusted (§2.1). Anything it
//     runs — a test, a build script, a repository's own tooling — can read it.
//   * Swapping a header in the egress gateway is IMPOSSIBLE, not merely
//     awkward: a `CONNECT` proxy sees the host and the SNI, never the inside of
//     the TLS session. Revision 1.2 of the plan proposed exactly that and it was
//     unimplementable (§7.5).
//
// So the credential stays on the owner node and the CLI is pointed somewhere
// else entirely:
//
//     CLI (sandbox, base URL → adapter) ──TLS──▶ adapter (owner node)
//        holds a TICKET, not a credential            │ validates the ticket
//                                                    │ injects vault material
//                                                    │ meters usage, enforces budget
//                                                    └──TLS──▶ provider
//
// What each property buys:
//
// **The ticket is not a credential.** It is bound to one session, one run, one
// CLI instance, one model, a method and path allowlist and a budget. A stolen
// ticket buys exactly what that run was already allowed to do, and expires with
// it. Issuing one needs `cli_delegate` through the PEP — holding `net_egress`
// is not enough (§7.5).
//
// **Usage is measured HERE.** The meter sits on the wire the answer travels
// through, so the budget does not depend on the CLI or the provider reporting
// honestly (§17.3). A response that carries no usage still spends the request
// and byte budget, and traffic STOPS when a budget is crossed — mid-stream if
// that is when it happens. A budget that only logs is not a budget.
//
// **The adapter's certificate is trusted in one sandbox only.** It is issued by
// a CA generated for this session and handed to the sandbox as a file; the
// host's trust store is untouched.
//
// Two things this module deliberately does NOT do:
//
//   * **No cost arithmetic.** There is no price feed on the node, and inventing
//     one would make an exact-looking number out of a guess. The budget is
//     expressed in tokens, requests and bytes; a caller that knows its price
//     list converts a cost ceiling into a token ceiling before issuing.
//   * **No database writes.** Every decision produces an `EventPayload` the
//     caller appends to the session timeline, exactly like the egress gateway.
//
// Phase 0B (§17.1) has been performed against pinned binaries, inside a network
// namespace that captured every TCP connection the CLI opened and logged its
// SNI. What it found is why this file looks the way it does:
//
//   * `claude 2.1.233` meets points 6 and 7 — but only with
//     `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and a session-private
//     `CLAUDE_CONFIG_DIR`. Both are part of `EngineWiring`, because without them
//     the process also reached api.anthropic.com, github.com and a Datadog
//     intake, and could have used a claude.ai login of its own. It meets point 3
//     through `--permission-prompt-tool stdio`, which routes its permission
//     questions onto the bridge's own stream instead of an MCP server — with one
//     honest bound recorded in `cli_bridge`: the CLI asks only about what its
//     own rules escalate, so a tool it allows by default is never offered to the
//     policy engine.
//   * `codex 0.147.0` does NOT meet point 6. `OPENAI_BASE_URL` is ignored
//     outright; a provider passed as `-c model_providers.*` moves the model
//     traffic, and connections to chatgpt.com and api.github.com remain. It runs
//     only where a gateway sees that residual traffic — see
//     `ensure_residual_traffic_is_contained`.
//
// Measuring is not deciding: `ensure_engine_verified` still refuses every engine
// until an administrator records the organization's go/no-go (a flag AND a
// note). Nothing in this build sets that flag on its own.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, warn};

use super::events::EventPayload;
use super::models::EgressEnforcement;
use super::pep::{self, Capability, Decision, Target};
use super::vault::AgentCredential;
use crate::db::DbPool;

/// Setting prefix of the Phase 0B gate. One key per engine
/// (`…verified.claude-code`), because go/no-go is decided per CLI.
pub const BASE_URL_OVERRIDE_VERIFIED_PREFIX: &str = "code_studio_cli_base_url_override_verified.";
/// Setting prefix of the note that records WHAT was verified and when. The flag
/// alone cannot enable an engine: §17.1 asks for a note plus transcripts plus a
/// decision, and a flag with no note is a decision nobody can audit.
pub const GO_NO_GO_NOTE_PREFIX: &str = "code_studio_cli_go_no_go_note.";

/// Largest request body the adapter buffers. It has to buffer: the model is in
/// the body, and a request whose model was never checked is a request the ticket
/// did not authorize.
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEAD_BYTES: usize = 64 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// A long answer is normal; an infinite one is not.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(600);
/// How much of a response body is kept for the "parse the whole thing as JSON"
/// fallback when no streaming usage line appeared.
const METER_BUFFER_BYTES: usize = 1024 * 1024;

// =============================================================================
// Phase 0B gate
// =============================================================================

/// Why delegation to an engine is refused. Carries the engine and the reason so
/// the caller can put both in front of a person instead of "not available".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRefusal {
    pub engine_id: String,
    pub reason: String,
}

impl std::fmt::Display for GateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.engine_id, self.reason)
    }
}

impl std::error::Error for GateRefusal {}

/// The gate `delegate_cli` calls BEFORE starting anything.
///
/// Default is off, and the refusal names the missing artefact. This is the
/// implementation of "the isolation promise is either kept or the feature is
/// absent" (§7.5): an engine nobody has verified simply does not run, rather
/// than running with a weaker story.
///
/// The organization's decision (the flag plus the note) is necessary for every
/// engine. It is not SUFFICIENT for an engine measured to keep traffic outside
/// the adapter: `codex` does, so its refusal also names the containment the
/// workspace has to provide — see `ensure_residual_traffic_is_contained`.
pub fn ensure_engine_verified(
    core_db: &DbPool,
    engine_id: &str,
    egress_enforcement: EgressEnforcement,
) -> Result<(), GateRefusal> {
    let refusal = |reason: String| GateRefusal {
        engine_id: engine_id.to_string(),
        reason,
    };
    let flag_key = format!("{BASE_URL_OVERRIDE_VERIFIED_PREFIX}{engine_id}");
    let note_key = format!("{GO_NO_GO_NOTE_PREFIX}{engine_id}");
    let verified = crate::db::repository::get_setting(core_db, &flag_key)
        .ok()
        .flatten()
        .is_some_and(|value| value == "true" || value == "1");
    if !verified {
        return Err(refusal(format!(
            "Phase 0B go/no-go (§17.1 points 6 and 7 — base URL override captures all traffic, \
             and the CLI has no credential refresh channel of its own) has not been recorded for \
             this engine. An administrator sets '{flag_key}' to true after verifying it against \
             the pinned CLI version; until then delegation to this engine is disabled"
        )));
    }
    let note = crate::db::repository::get_setting(core_db, &note_key)
        .ok()
        .flatten()
        .unwrap_or_default();
    if note.trim().is_empty() {
        return Err(refusal(format!(
            "'{flag_key}' is set but '{note_key}' is empty. The gate requires the go/no-go note \
             (CLI version, what was verified, who decided) — a flag nobody can audit is not a \
             verification"
        )));
    }
    ensure_residual_traffic_is_contained(engine_id, egress_enforcement).map_err(refusal)
}

/// §17.1 point 6 for an engine that does not meet it on its own.
///
/// Phase 0B measured `codex 0.147.0` inside a network namespace that captured
/// every TCP connection: `OPENAI_BASE_URL` is ignored outright, and even with
/// the model provider redirected through `-c model_providers.*` the process
/// still opened connections to `chatgpt.com` and `api.github.com`. The adapter
/// therefore does NOT see all of that CLI's traffic, and no wiring on our side
/// can make it. What is left is containment by the workspace: under `namespace`
/// or `firewall` that residual traffic is filtered and audited by the egress
/// gateway (§7.6), and under `unrestricted` nothing sees it at all.
///
/// This is deliberately a property of the WORKSPACE, not a flag an administrator
/// can set: the refusal has to name a mechanism that exists, not a promise.
fn ensure_residual_traffic_is_contained(
    engine_id: &str,
    egress_enforcement: EgressEnforcement,
) -> Result<(), String> {
    if engine_id != "codex" || egress_enforcement != EgressEnforcement::Unrestricted {
        return Ok(());
    }
    Err(format!(
        "Phase 0B measured that codex keeps opening connections to chatgpt.com and \
         api.github.com even when its model provider is redirected to the adapter, so the \
         override does not capture all of its traffic (§17.1 point 6). That residual traffic is \
         only filtered and audited where the workspace enforces egress through the gateway, and \
         this workspace runs 'unrestricted' — where §7.6 promises neither. Run the workspace in \
         a container (egress_enforcement 'namespace') or on a node with the firewall rule \
         ('firewall'), or delegate to an engine whose traffic the adapter does capture. The \
         organization's OpenAI credential is still safe here: the CLI is started with a \
         session-private, empty CODEX_HOME, so it can never fall back to a ChatGPT \
         subscription credential of its own"
    ))
}

// =============================================================================
// Which credential pays for the delegation
// =============================================================================

/// How one delegation authenticates to the provider.
///
/// Two mechanisms, one checkable condition, no flag — a deployment does not
/// choose between them, its own state decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationAuth {
    /// §7.5 in full. The organization's credential is in this node's vault, the
    /// adapter holds it in memory, the CLI is pointed at the adapter and gets a
    /// ticket instead of a key, and every request it makes crosses a socket we
    /// own — which is what makes the ticket's budget enforceable (§17.3).
    OrgCredential,
    /// The engine authenticates ITSELF: the operator running this node logged
    /// the CLI into its provider, and the delegation spends that login.
    ///
    /// Nothing of §7.5 applies here, and pretending otherwise would break the
    /// mode rather than harden it: a base URL override, an API key variable or a
    /// session-private configuration directory each, on its own, TAKES THAT
    /// LOGIN AWAY — the config directory is where the login lives. So the CLI is
    /// started with none of them and sees exactly the account it already had.
    ///
    /// What is given up is measurement, and it is given up openly: no provider
    /// traffic crosses anything of ours, so §17.3's "measured in the adapter" is
    /// not satisfied and the budget is enforced on
    /// `cli_bridge::ProviderReportedUsage` — the vendor's own numbers. What is
    /// NOT given up: the Phase 0B gate, the `cli_delegate` decision, every
    /// permission question the CLI raises, and the session worktree as the
    /// process's boundary.
    ProviderLogin,
}

impl DelegationAuth {
    pub fn slug(self) -> &'static str {
        match self {
            DelegationAuth::OrgCredential => "org_credential",
            DelegationAuth::ProviderLogin => "provider_login",
        }
    }

    /// Who counts what a run of this mode spends — the honest answer, recorded
    /// next to the numbers so nobody reads a provider's word as a measurement.
    pub fn usage_source(self) -> &'static str {
        match self {
            DelegationAuth::OrgCredential => "adapter",
            DelegationAuth::ProviderLogin => "provider_reported",
        }
    }
}

/// Decides which of the two an engine takes on this node.
///
/// The order is the decision, and both facts are read from the node rather than
/// configured:
///
///   1. **Does this node's vault hold the organization's credential for the
///      engine?** Then that is what the delegation spends, and it spends it
///      through the adapter. An organization that provisioned a key meant the
///      runs to use it, and the metered path is the stronger one.
///   2. **Otherwise, is the CLI logged in on this node?** The bridge asks the
///      vendor's own status command (`CliBridge::provider_login`). If it is, the
///      engine authenticates itself.
///
/// Neither branch is a fallback for the other: they are different accounts paid
/// for by different parties, and each is refused for its own reason. With no
/// credential AND no login there is nothing to authenticate with, and the
/// refusal says both halves — the old `credential_missing` alone would send an
/// administrator looking for a vault row that is not the only answer.
///
/// `provider_login` is taken as a future so the probe is never run when the
/// vault already answered: it spawns the vendor binary, and a delegation must
/// not pay for a question it does not need.
pub async fn resolve_delegation_auth(
    core_db: &DbPool,
    org_id: &str,
    node_id: &str,
    engine_id: &str,
    provider_login: impl std::future::Future<Output = Result<bool>>,
) -> Result<DelegationAuth, GateRefusal> {
    let refusal = |reason: String| GateRefusal {
        engine_id: engine_id.to_string(),
        reason,
    };
    let stored = super::vault::get_agent_credential_record(core_db, org_id, node_id, engine_id)
        .map_err(|error| refusal(format!("the vault could not be read: {error}")))?;
    if stored.is_some() {
        return Ok(DelegationAuth::OrgCredential);
    }
    match provider_login.await {
        Ok(true) => Ok(DelegationAuth::ProviderLogin),
        Ok(false) => Err(refusal(format!(
            "credential_missing: node '{node_id}' holds no organization credential for this \
             engine, and the CLI reports no provider login of its own. Store the organization's \
             key for the engine (it never enters the sandbox — the adapter injects it), or log \
             the CLI in on this node"
        ))),
        Err(error) => Err(refusal(format!(
            "credential_missing: node '{node_id}' holds no organization credential for this \
             engine, and whether the CLI is logged in could not be established: {error:#}"
        ))),
    }
}

// =============================================================================
// Tickets
// =============================================================================

/// What one run may spend through the adapter. Tokens are the currency because
/// they are what the wire actually reports; requests and bytes are the floor
/// that still applies when a provider reports nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_requests: u32,
    pub max_total_tokens: u64,
    pub max_bytes: u64,
}

impl Budget {
    /// A budget that stops a runaway loop but does not get in the way of one
    /// delegated task. Callers with their own accounting override it.
    pub fn default_for_run() -> Self {
        Self {
            max_requests: 400,
            max_total_tokens: 4_000_000,
            max_bytes: 512 * 1024 * 1024,
        }
    }
}

/// What has been spent. Every field is counted by the adapter itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub requests: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    fn exceeds(&self, budget: &Budget) -> Option<&'static str> {
        if self.requests > budget.max_requests {
            return Some("requests");
        }
        if self.total_tokens() > budget.max_total_tokens {
            return Some("tokens");
        }
        if self.bytes_up.saturating_add(self.bytes_down) > budget.max_bytes {
            return Some("bytes");
        }
        None
    }
}

/// How the model an operator configured relates to what the CLI actually puts
/// in the request body.
///
/// The two vocabularies are NOT the same, and treating them as one is why a
/// ticket configured `model = "sonnet"` refused every request the CLI made:
/// Claude Code takes `sonnet` on `--model`, resolves it against the account's
/// entitlements and sends `claude-sonnet-4-5-<snapshot>` on the wire. The
/// catalog `services::coding_agent` offers is the ALIAS list, so the alias is
/// what an operator picks and the dated id is what has to be authorized.
///
/// `Ok(None)` — the configured string is a wire id already, matched verbatim.
/// `Ok(Some(family))` — it is a vendor alias for one model family; the ticket
/// binds that family, and every other family is still refused.
/// `Err(reason)` — it is an alias that resolves to MORE THAN ONE model, which
/// no single-model ticket can express.
pub fn ticket_model_binding(
    engine_id: &str,
    model: &str,
) -> std::result::Result<Option<String>, String> {
    let model = model.trim().to_ascii_lowercase();
    if engine_id != "claude-code" {
        // `codex` puts the configured id on the wire unchanged, and no other
        // engine has adapter wiring at all.
        return Ok(None);
    }
    match model.as_str() {
        "opus" | "sonnet" | "haiku" => Ok(Some(model)),
        // Phase 0B: this alias is Claude Code's "plan with Opus, execute with
        // Sonnet" mode, so one turn addresses two models. A ticket binds one
        // (§7.5), so the honest answer is a refusal at configuration time
        // rather than a delegation that dies on the first request.
        "opusplan" => Err(
            "'opusplan' makes the CLI address two models in one turn (Opus while planning, \
             Sonnet while executing) and a ticket binds exactly one model. Configure 'opus' or \
             'sonnet'"
                .to_string(),
        ),
        _ => Ok(None),
    }
}

/// Whether a wire model id belongs to a Claude model family.
///
/// The vendor spells its ids `claude-<family>-<version…>-<snapshot>`, and older
/// releases `claude-<version…>-<family>-<snapshot>`: exactly one family token
/// among otherwise numeric components. Requiring every other component to be
/// numeric is what keeps the match narrow — `sonnet` cannot reach
/// `claude-opus-4-5-…`, and a spelling this rule does not recognize is refused
/// loudly with `model_not_allowed` instead of being waved through.
fn claude_id_is_family(model_lower: &str, family: &str) -> bool {
    let Some(rest) = model_lower.strip_prefix("claude-") else {
        return false;
    };
    let mut seen = false;
    for segment in rest.split('-') {
        if segment == family {
            if seen {
                return false;
            }
            seen = true;
        } else if segment.is_empty() || !segment.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    seen
}

/// Everything a ticket is bound to. A request that does not match every field
/// is refused; there is no partial match and no default that widens the scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketClaims {
    pub ticket_id: String,
    pub session_id: String,
    pub run_id: String,
    pub cli_instance_id: String,
    pub engine_id: String,
    /// The single model this run may call, as the operator configured it.
    pub model: String,
    /// Other spellings of the SAME model, supplied by the caller from our own
    /// catalog convention (`<engine>/<id>` and the bare id).
    pub model_aliases: BTreeSet<String>,
    /// Vendor family the configured model is an alias for, resolved once by
    /// `ticket_model_binding`. `None` means the configured string is the wire
    /// id and only exact spellings are accepted.
    pub model_family: Option<String>,
    pub methods: BTreeSet<String>,
    /// Allowed path prefixes. A prefix is matched after normalization, so `..`
    /// cannot walk out of one.
    pub path_prefixes: Vec<String>,
    pub budget: Budget,
    pub expires_at_unix: i64,
}

/// A freshly minted ticket. `presentation` is what goes into the sandbox as the
/// CLI's "API key" — it is a capability for this run and nothing else, which is
/// precisely why it may live there while the real credential may not.
#[derive(Clone)]
pub struct IssuedTicket {
    pub claims: TicketClaims,
    pub presentation: String,
}

/// A ticket is a bearer secret, so `Debug` prints the claims and NOT the string
/// that authorizes them. Everything in this codebase that logs a struct logs it
/// through `Debug`.
impl std::fmt::Debug for IssuedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedTicket")
            .field("claims", &self.claims)
            .field("presentation", &"<redacted>")
            .finish()
    }
}

impl IssuedTicket {
    /// The timeline entry for the issuance. Written by the caller, so this
    /// module keeps its "no database writes" property.
    pub fn event(&self) -> EventPayload {
        EventPayload::TicketIssued {
            ticket_id: self.claims.ticket_id.clone(),
            engine_id: self.claims.engine_id.clone(),
            budget_tokens: self.claims.budget.max_total_tokens,
        }
    }
}

/// Why a presented ticket did not authorize a request. Every variant is an
/// event, and none of them falls through to "allow".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketRejection {
    Missing,
    Malformed,
    Unknown,
    BadSecret,
    Expired,
    Revoked,
    WrongInstance,
    MethodNotAllowed(String),
    PathNotAllowed(String),
    ModelNotAllowed(String),
    BudgetExhausted(&'static str),
}

impl TicketRejection {
    pub fn slug(&self) -> &'static str {
        match self {
            TicketRejection::Missing => "ticket_missing",
            TicketRejection::Malformed => "ticket_malformed",
            TicketRejection::Unknown => "ticket_unknown",
            TicketRejection::BadSecret => "ticket_bad_secret",
            TicketRejection::Expired => "ticket_expired",
            TicketRejection::Revoked => "ticket_revoked",
            TicketRejection::WrongInstance => "ticket_wrong_instance",
            TicketRejection::MethodNotAllowed(_) => "method_not_allowed",
            TicketRejection::PathNotAllowed(_) => "path_not_allowed",
            TicketRejection::ModelNotAllowed(_) => "model_not_allowed",
            TicketRejection::BudgetExhausted(_) => "budget_exhausted",
        }
    }

    /// HTTP status the CLI sees. A budget stop is a 429 so a well-behaved client
    /// stops rather than hammering; everything else is a flat 403, because a
    /// ticket that does not fit is not a retryable condition.
    fn status(&self) -> (u16, &'static str) {
        match self {
            TicketRejection::BudgetExhausted(_) => (429, "Too Many Requests"),
            _ => (403, "Forbidden"),
        }
    }

    fn detail(&self) -> String {
        match self {
            TicketRejection::Missing => "no ticket was presented".into(),
            TicketRejection::Malformed => "the ticket is not in the expected form".into(),
            TicketRejection::Unknown => "the ticket is not known to this adapter".into(),
            TicketRejection::BadSecret => "the ticket secret does not match".into(),
            TicketRejection::Expired => "the ticket has expired".into(),
            TicketRejection::Revoked => "the ticket was revoked with its run".into(),
            TicketRejection::WrongInstance => {
                "the ticket belongs to another run of this session".into()
            }
            TicketRejection::MethodNotAllowed(method) => {
                format!("method {method} is outside the ticket")
            }
            TicketRejection::PathNotAllowed(path) => format!("path {path} is outside the ticket"),
            TicketRejection::ModelNotAllowed(model) => {
                format!("model {model} is outside the ticket")
            }
            TicketRejection::BudgetExhausted(what) => {
                format!("the ticket's {what} budget is exhausted")
            }
        }
    }
}

impl std::fmt::Display for TicketRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.slug(), self.detail())
    }
}

struct TicketState {
    claims: TicketClaims,
    secret_hash: [u8; 32],
    spent: Usage,
    revoked: bool,
    /// Set the moment a budget is crossed. Kept separate from `revoked` so the
    /// timeline can distinguish "the run ended" from "the run spent its ceiling".
    exhausted: Option<&'static str>,
}

/// The live tickets of this process.
///
/// Deliberately in memory only. A ticket outlives nothing: after a restart every
/// run is closed and every CLI instance is `reaped`, so a ticket that survived
/// would be a capability with no owner. Losing them on restart is the correct
/// behaviour, not a limitation.
#[derive(Default)]
pub struct TicketRegistry {
    tickets: Mutex<HashMap<String, TicketState>>,
}

impl TicketRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mints a ticket for already-authorized claims. The PEP check lives in
    /// `issue_ticket`, which is the only intended caller.
    fn insert(&self, claims: TicketClaims) -> Result<IssuedTicket> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|e| anyhow!("ticket secret: {e}"))?;
        let secret_b64 = B64URL.encode(secret);
        let presentation = format!("tfck_{}_{}", claims.ticket_id, secret_b64);
        let secret_hash = digest32(secret_b64.as_bytes());
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|e| anyhow!("ticket registry: {e}"))?;
        tickets.insert(
            claims.ticket_id.clone(),
            TicketState {
                claims: claims.clone(),
                secret_hash,
                spent: Usage::default(),
                revoked: false,
                exhausted: None,
            },
        );
        Ok(IssuedTicket {
            claims,
            presentation,
        })
    }

    /// Invalidates every ticket of a run. Called when the run settles — a ticket
    /// that outlives its run is exactly the thing §7.5 promises cannot happen.
    ///
    /// The entries are marked rather than dropped, so a CLI that keeps using a
    /// ticket after its run ended is told `ticket_revoked` instead of the
    /// ambiguous `ticket_unknown`. Long-dead entries are pruned here, which
    /// bounds the map without a sweeper.
    pub fn revoke_run(&self, run_id: &str) -> usize {
        let Ok(mut tickets) = self.tickets.lock() else {
            return 0;
        };
        let mut revoked = 0;
        for state in tickets.values_mut() {
            if state.claims.run_id == run_id && !state.revoked {
                state.revoked = true;
                revoked += 1;
            }
        }
        let stale_before = now_unix() - 3600;
        tickets.retain(|_, state| state.claims.expires_at_unix > stale_before);
        revoked
    }

    /// What a ticket has spent, for the caller's own accounting.
    pub fn usage(&self, ticket_id: &str) -> Option<Usage> {
        let tickets = self.tickets.lock().ok()?;
        tickets.get(ticket_id).map(|state| state.spent)
    }

    /// Which budget a ticket crossed, if any.
    ///
    /// The registry is asked rather than the caller re-comparing `usage()`
    /// against the budget: the ceiling is enforced in `authorize`/`record`, and
    /// a second comparison elsewhere would be a second opinion about when a run
    /// has spent its allowance.
    pub fn exhausted(&self, ticket_id: &str) -> Option<&'static str> {
        let tickets = self.tickets.lock().ok()?;
        tickets.get(ticket_id).and_then(|state| state.exhausted)
    }

    /// Validates a presented ticket against one request. Returns the claims so
    /// the caller knows which credential to inject and which run to bill.
    pub fn authorize(
        &self,
        presented: Option<&str>,
        request: &RequestFacts<'_>,
    ) -> std::result::Result<TicketClaims, TicketRejection> {
        let presented = presented.ok_or(TicketRejection::Missing)?;
        let (ticket_id, secret) =
            split_presentation(presented).ok_or(TicketRejection::Malformed)?;
        let mut tickets = self.tickets.lock().map_err(|_| TicketRejection::Unknown)?;
        let state = tickets.get_mut(ticket_id).ok_or(TicketRejection::Unknown)?;

        let presented_hash = digest32(secret.as_bytes());
        if presented_hash.ct_eq(&state.secret_hash).unwrap_u8() != 1 {
            return Err(TicketRejection::BadSecret);
        }
        if state.revoked {
            return Err(TicketRejection::Revoked);
        }
        if let Some(what) = state.exhausted {
            return Err(TicketRejection::BudgetExhausted(what));
        }
        if now_unix() >= state.claims.expires_at_unix {
            return Err(TicketRejection::Expired);
        }
        // A ticket is bound to ONE CLI instance. The check only applies when the
        // caller knows which instance it is speaking for; the adapter itself
        // learns that from the ticket, so this is the path a caller replaying a
        // ticket into another run trips over.
        if let Some(instance) = request.cli_instance_id {
            if instance != state.claims.cli_instance_id {
                return Err(TicketRejection::WrongInstance);
            }
        }
        if !state
            .claims
            .methods
            .contains(&request.method.to_ascii_uppercase())
        {
            return Err(TicketRejection::MethodNotAllowed(
                request.method.to_string(),
            ));
        }
        let path = normalized_path(request.path);
        if !state
            .claims
            .path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
        {
            return Err(TicketRejection::PathNotAllowed(path));
        }
        if let Some(model) = request.model {
            let model_lower = model.to_ascii_lowercase();
            let allowed = model_lower == state.claims.model.to_ascii_lowercase()
                || state
                    .claims
                    .model_aliases
                    .iter()
                    .any(|alias| alias.to_ascii_lowercase() == model_lower)
                || state
                    .claims
                    .model_family
                    .as_deref()
                    .is_some_and(|family| claude_id_is_family(&model_lower, family));
            if !allowed {
                return Err(TicketRejection::ModelNotAllowed(model.to_string()));
            }
        }

        // Reserve the request before it is forwarded: a budget that is only
        // checked afterwards lets an unbounded number of concurrent requests
        // through the last slot.
        let mut projected = state.spent;
        projected.requests = projected.requests.saturating_add(1);
        projected.bytes_up = projected.bytes_up.saturating_add(request.body_len as u64);
        if let Some(what) = projected.exceeds(&state.claims.budget) {
            state.exhausted = Some(what);
            return Err(TicketRejection::BudgetExhausted(what));
        }
        state.spent = projected;
        Ok(state.claims.clone())
    }

    /// Books what a response cost. Returns the budget that was crossed, if any —
    /// the caller stops relaying on the spot, which is what makes the budget a
    /// limit rather than a log line.
    ///
    /// `delta` is an INCREMENT. The provider's counters are cumulative per
    /// response, so the relay loop books the difference since its own last call;
    /// taking a maximum here would make a second request of the same ticket look
    /// free whenever it was smaller than the first.
    pub fn record(&self, ticket_id: &str, delta: Usage) -> Option<&'static str> {
        let mut tickets = self.tickets.lock().ok()?;
        let state = tickets.get_mut(ticket_id)?;
        state.spent.input_tokens = state.spent.input_tokens.saturating_add(delta.input_tokens);
        state.spent.output_tokens = state
            .spent
            .output_tokens
            .saturating_add(delta.output_tokens);
        state.spent.bytes_down = state.spent.bytes_down.saturating_add(delta.bytes_down);
        let crossed = state.spent.exceeds(&state.claims.budget);
        if let Some(what) = crossed {
            state.exhausted = Some(what);
        }
        crossed
    }
}

fn digest32(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Splits `tfck_<id>_<secret>`. Nothing else is accepted, so a credential the
/// CLI was configured with by mistake cannot be confused for a ticket.
fn split_presentation(presented: &str) -> Option<(&str, &str)> {
    let rest = presented.trim().strip_prefix("tfck_")?;
    let (ticket_id, secret) = rest.split_once('_')?;
    let id_ok = !ticket_id.is_empty()
        && ticket_id.len() <= 64
        && ticket_id.chars().all(|c| c.is_ascii_alphanumeric());
    (id_ok && !secret.is_empty()).then_some((ticket_id, secret))
}

/// What the adapter knows about one request when it validates the ticket.
#[derive(Debug, Clone)]
pub struct RequestFacts<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub model: Option<&'a str>,
    pub body_len: usize,
    /// Set by a caller that already knows which instance it speaks for (the
    /// tests, and any in-process user of the registry).
    pub cli_instance_id: Option<&'a str>,
}

/// Collapses `.` and `..` and strips the query, so a prefix check cannot be
/// walked around with `/v1/messages/../../admin`.
fn normalized_path(raw: &str) -> String {
    let path = raw.split(['?', '#']).next().unwrap_or(raw);
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

// =============================================================================
// Issuance through the PEP
// =============================================================================

/// What a caller asks for. `ttl` is bounded by the run: the caller passes the
/// remaining budget of the run, and the ticket cannot outlive it.
#[derive(Debug, Clone)]
pub struct TicketRequest {
    pub session_id: String,
    pub run_id: String,
    pub cli_instance_id: String,
    pub engine_id: String,
    pub model: String,
    pub model_aliases: BTreeSet<String>,
    pub methods: BTreeSet<String>,
    pub path_prefixes: Vec<String>,
    pub budget: Budget,
    pub ttl: Duration,
    /// The provider host resolved for this engine, already matched against the
    /// workspace allowlist by the caller — the PEP never resolves targets
    /// itself.
    pub host_allowlisted: bool,
}

/// Outcome of asking for a ticket. `Ask` is not a failure: the operator has to
/// answer, and the caller re-issues once a grant exists (the same shape every
/// other capability uses).
#[derive(Debug)]
pub enum TicketDecision {
    Issued(Box<IssuedTicket>),
    Ask { summary: String },
    Denied { reason: String },
}

/// What the PEP said about `cli_delegate`, on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegateDecision {
    Allow,
    Ask { summary: String },
    Deny { reason: String },
}

/// The `cli_delegate` decision, separated from the ticket.
///
/// The capability is "may this run delegate a turn to a vendor CLI", not "may
/// this run be handed a ticket", so it has to be askable without one: on a
/// self-authenticated engine (`DelegationAuth::ProviderLogin`) there is no
/// ticket to mint and the decision is exactly as required. `issue_ticket` calls
/// this first, so both modes reach the PEP through one function and possessing
/// `net_egress` entitles neither of them (§7.5).
pub fn authorize_delegation(ctx: &pep::SessionCtx, host_allowlisted: bool) -> DelegateDecision {
    let target = Target::Host {
        allowlisted: host_allowlisted,
    };
    match pep::authorize(ctx, Capability::CliDelegate, &target) {
        Decision::Deny { reason } => DelegateDecision::Deny { reason },
        Decision::AskUser { summary, .. } => DelegateDecision::Ask { summary },
        Decision::Allow(_) => DelegateDecision::Allow,
    }
}

/// The only intended way to obtain a ticket.
///
/// `cli_delegate` is checked here, through the same PEP every other operation
/// goes through: possessing `net_egress` does not entitle a run to spend the
/// organization's provider credential (§7.5).
pub fn issue_ticket(
    registry: &TicketRegistry,
    ctx: &pep::SessionCtx,
    request: TicketRequest,
) -> Result<TicketDecision> {
    match authorize_delegation(ctx, request.host_allowlisted) {
        DelegateDecision::Deny { reason } => return Ok(TicketDecision::Denied { reason }),
        DelegateDecision::Ask { summary } => return Ok(TicketDecision::Ask { summary }),
        DelegateDecision::Allow => {}
    }
    if request.model.trim().is_empty() {
        return Ok(TicketDecision::Denied {
            reason: "a ticket without a model would authorize any model".to_string(),
        });
    }
    // Resolved HERE rather than passed in: the mapping from an operator's model
    // name to what the CLI puts on the wire is vendor knowledge, and a caller
    // that forgot to supply it would mint a ticket that refuses every request.
    let model_family = match ticket_model_binding(&request.engine_id, &request.model) {
        Ok(family) => family,
        Err(reason) => return Ok(TicketDecision::Denied { reason }),
    };
    if request.methods.is_empty() || request.path_prefixes.is_empty() {
        return Ok(TicketDecision::Denied {
            reason:
                "a ticket without a method and path allowlist authorizes the whole provider API"
                    .to_string(),
        });
    }
    let ticket_id = new_ticket_id()?;
    let claims = TicketClaims {
        ticket_id,
        session_id: request.session_id,
        run_id: request.run_id,
        cli_instance_id: request.cli_instance_id,
        engine_id: request.engine_id,
        model: request.model,
        model_aliases: request.model_aliases,
        model_family,
        methods: request
            .methods
            .iter()
            .map(|m| m.to_ascii_uppercase())
            .collect(),
        path_prefixes: request
            .path_prefixes
            .iter()
            .map(|p| normalized_path(p))
            .collect(),
        budget: request.budget,
        expires_at_unix: now_unix() + request.ttl.as_secs().min(24 * 60 * 60) as i64,
    };
    Ok(TicketDecision::Issued(Box::new(registry.insert(claims)?)))
}

fn new_ticket_id() -> Result<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("ticket id: {e}"))?;
    Ok(hex::encode(bytes))
}

// =============================================================================
// Session trust anchor
// =============================================================================

/// The CA that exists for one session and is trusted in one sandbox.
///
/// Generated in memory; only the CA CERTIFICATE (not its key) is written to a
/// file the sandbox can read. Nothing is added to the host's trust store, so the
/// adapter cannot be used to make the host believe anything it did not already.
pub struct SessionTrust {
    ca_pem: String,
    ca_path: PathBuf,
    leaf_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    leaf_key: rustls::pki_types::PrivateKeyDer<'static>,
}

impl SessionTrust {
    /// Generates the CA and the adapter's leaf certificate and writes the CA
    /// where the sandbox will read it.
    pub fn generate(ca_path: &Path, dns_names: &[String]) -> Result<Self> {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
            PKCS_ECDSA_P256_SHA256,
        };

        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "TentaFlow Code Studio session CA");
        let ca_cert = ca_params.self_signed(&ca_key)?;
        let ca_pem = ca_cert.pem();

        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut leaf_params = CertificateParams::new(dns_names.to_vec())?;
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, "TentaFlow Code Studio provider adapter");
        leaf_params.use_authority_key_identifier_extension = true;
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer)?;

        if let Some(parent) = ca_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(ca_path, ca_pem.as_bytes())
            .with_context(|| format!("write session CA to {}", ca_path.display()))?;

        Ok(Self {
            ca_pem,
            ca_path: ca_path.to_path_buf(),
            leaf_chain: vec![leaf_cert.der().clone()],
            leaf_key: rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        })
    }

    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    pub fn ca_path(&self) -> &Path {
        &self.ca_path
    }

    fn acceptor(&self) -> Result<TlsAcceptor> {
        // TLS 1.3 only, matching the rest of the product. Both CLIs are modern
        // runtimes; a client that cannot speak 1.3 is not one we want holding a
        // ticket.
        // The provider is named rather than taken from the process-wide default.
        // `tentaflow-core` links rustls through several dependencies, so the
        // default is whatever installed itself first — or, when two backends are
        // compiled in, nothing at all, and the builder PANICS. An adapter that
        // holds an organisation's provider credential must not depend on link
        // order for whether it starts.
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("adapter TLS provider")?
        .with_no_client_auth()
                .with_single_cert(self.leaf_chain.clone(), self.leaf_key.clone_key())
                .context("adapter TLS configuration")?;
        // Forces HTTP/1.1, which is the protocol this adapter parses. A client
        // that negotiated h2 would be talking a framing nothing here reads.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

// =============================================================================
// Engine wiring
// =============================================================================

/// Which header carries the credential upstream, and how a CLI is pointed at
/// the adapter.
///
/// Every field here was measured in Phase 0B against the pinned binaries
/// (`claude 2.1.233`, `codex 0.147.0`) inside a network namespace that logged
/// SNI for every TCP connection the process opened. Two findings shape it:
///
///   * claude honours `ANTHROPIC_BASE_URL` and prefers `ANTHROPIC_API_KEY` over
///     a claude.ai login, but WITHOUT
///     `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and a private
///     `CLAUDE_CONFIG_DIR` it also reaches api.anthropic.com, github.com and a
///     Datadog intake. Those two variables are the mechanism behind §17.1
///     point 6, not cosmetics.
///   * codex ignores `OPENAI_BASE_URL` completely. Its provider is moved only by
///     `-c model_providers.*` arguments given at startup, which is why the
///     wiring carries ARGUMENTS as well as environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialHeader {
    /// `Authorization: Bearer <credential>` (OpenAI-shaped APIs).
    AuthorizationBearer,
    /// `x-api-key: <credential>` (Anthropic-shaped APIs).
    XApiKey,
}

#[derive(Debug, Clone)]
pub struct EngineWiring {
    pub engine_id: String,
    pub credential_header: CredentialHeader,
    /// Environment variable that overrides the CLI's base URL, where the CLI
    /// honours one. `None` for codex, which does not: setting `OPENAI_BASE_URL`
    /// changed nothing and 100% of its traffic still went to the vendor, so
    /// declaring the variable would describe a mechanism that does not exist.
    pub base_url_var: Option<String>,
    /// Environment variable the CLI reads its API key from — it gets the ticket.
    pub api_key_var: String,
    /// Variable pointing the CLI at a session-private configuration directory.
    /// It is containment, not tidiness: whatever account an operator logged this
    /// CLI into on this host lives in the default directory, and a CLI that
    /// finds a credential of its own there uses it INSTEAD of the ticket —
    /// which is exactly the credential-refresh channel §17.1 point 7 forbids.
    pub config_dir_var: String,
    /// Variables that switch off everything the CLI does besides talking to its
    /// model. Measured, not assumed: without them claude also reached
    /// api.anthropic.com, github.com and http-intake.logs.us5.datadoghq.com.
    pub isolation_env: Vec<(String, String)>,
    /// Methods a ticket for this engine is minted with.
    pub ticket_methods: BTreeSet<String>,
    /// Path prefixes a ticket for this engine is minted with. Both providers
    /// serve their inference API under `/v1`; the ticket says so explicitly
    /// rather than defaulting to "the whole host", because a ticket without a
    /// path allowlist authorizes the provider's account-management surface too.
    pub ticket_path_prefixes: Vec<String>,
    /// Requests the adapter answers ITSELF, without a ticket and without
    /// forwarding, as `(method, path)`. Claude Code opens a session with
    /// `HEAD /api/hello` carrying no authentication at all: it is a "is this
    /// base URL alive" probe, and the honest answer comes from the thing that
    /// IS the base URL. Refusing it would log a denial per session for a
    /// request that was never going to reach a provider.
    pub unauthenticated_probes: Vec<(String, String)>,
}

impl EngineWiring {
    pub fn for_engine(engine_id: &str) -> Result<Self> {
        let methods = || {
            ["GET".to_string(), "POST".to_string()]
                .into_iter()
                .collect::<BTreeSet<String>>()
        };
        match engine_id {
            "claude-code" => Ok(Self {
                engine_id: engine_id.to_string(),
                credential_header: CredentialHeader::XApiKey,
                base_url_var: Some("ANTHROPIC_BASE_URL".to_string()),
                api_key_var: "ANTHROPIC_API_KEY".to_string(),
                config_dir_var: "CLAUDE_CONFIG_DIR".to_string(),
                isolation_env: vec![(
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                    "1".to_string(),
                )],
                ticket_methods: methods(),
                ticket_path_prefixes: vec!["/v1".to_string()],
                unauthenticated_probes: vec![("HEAD".to_string(), "/api/hello".to_string())],
            }),
            "codex" => Ok(Self {
                engine_id: engine_id.to_string(),
                credential_header: CredentialHeader::AuthorizationBearer,
                base_url_var: None,
                // Named by the provider configuration below, so the CLI reads
                // the ticket out of a variable that means nothing to the vendor
                // — there is no chance of it being mistaken for a real key.
                api_key_var: CODEX_TICKET_VAR.to_string(),
                config_dir_var: "CODEX_HOME".to_string(),
                isolation_env: Vec::new(),
                ticket_methods: methods(),
                ticket_path_prefixes: vec!["/v1".to_string()],
                unauthenticated_probes: Vec::new(),
            }),
            other => Err(anyhow!(
                "no adapter wiring for engine '{other}': an engine reaches the provider through \
                 the adapter or not at all"
            )),
        }
    }

    /// Arguments the CLI has to be STARTED with for its model traffic to reach
    /// the adapter, given the adapter's base URL.
    ///
    /// Empty for claude, whose base URL is an environment variable. For codex it
    /// is the only thing that works: the provider is a configuration entry, and
    /// `-c` is how a configuration entry is set for one process without writing
    /// into `config.toml`.
    pub fn cli_args(&self, base_url: &str) -> Vec<String> {
        if self.engine_id != "codex" {
            return Vec::new();
        }
        let provider = CODEX_PROVIDER_ID;
        [
            format!("model_provider={provider}"),
            format!(
                "model_providers.{provider}.base_url={}/v1",
                base_url.trim_end_matches('/')
            ),
            format!("model_providers.{provider}.env_key={CODEX_TICKET_VAR}"),
            format!("model_providers.{provider}.wire_api=responses"),
        ]
        .into_iter()
        .flat_map(|setting| ["-c".to_string(), setting])
        .collect()
    }

    /// Whether the adapter answers this request itself instead of matching it
    /// against a ticket.
    fn is_unauthenticated_probe(&self, method: &str, path: &str) -> bool {
        self.unauthenticated_probes
            .iter()
            .any(|(probe_method, probe_path)| {
                probe_method.eq_ignore_ascii_case(method) && probe_path == path
            })
    }
}

/// Name of the codex provider entry the CLI is started with. Ours, so it cannot
/// collide with a provider an operator configured.
const CODEX_PROVIDER_ID: &str = "tfadapter";
/// Variable that provider entry reads the ticket from.
const CODEX_TICKET_VAR: &str = "TF_TICKET";

// =============================================================================
// The adapter
// =============================================================================

/// Where the adapter's decisions go. The caller owns the session timeline; this
/// module never writes to a database (same contract as the egress gateway).
pub trait AdapterEventSink: Send + Sync + 'static {
    fn record(&self, event: EventPayload);
}

struct AdapterInner {
    wiring: EngineWiring,
    upstream: url::Url,
    credential: AgentCredential,
    tickets: Arc<TicketRegistry>,
    sink: Arc<dyn AdapterEventSink>,
    client: reqwest::Client,
}

/// A bound adapter: one engine, one upstream, one listening socket.
pub struct ProviderAdapter {
    inner: Arc<AdapterInner>,
    acceptor: TlsAcceptor,
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl ProviderAdapter {
    /// Binds the adapter.
    ///
    /// `addr` is the caller's decision, exactly as for the egress gateway:
    /// loopback in native mode, the container bridge address when the sandbox
    /// lives in its own namespace. It is a TCP socket rather than a unix socket
    /// because a base URL override can only name one — which is why the ticket
    /// is mandatory: on the loopback of a shared host, the ticket IS the peer
    /// check (§7.6).
    pub async fn bind(
        addr: SocketAddr,
        wiring: EngineWiring,
        credential: AgentCredential,
        trust: &SessionTrust,
        tickets: Arc<TicketRegistry>,
        sink: Arc<dyn AdapterEventSink>,
    ) -> Result<Self> {
        let upstream = url::Url::parse(credential.provider_base_url.trim_end_matches('/'))
            .with_context(|| "the vault row's provider_base_url is not a URL".to_string())?;
        if !matches!(upstream.scheme(), "https" | "http") {
            return Err(anyhow!(
                "provider_base_url must be http(s), got {}",
                upstream.scheme()
            ));
        }
        let acceptor = trust.acceptor()?;
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind provider adapter on {addr}"))?;
        let local_addr = listener.local_addr()?;
        let client = reqwest::Client::builder()
            // The adapter's own route to the provider is the node's, not the
            // sandbox's; the sandbox has no route at all. `no_proxy` keeps the
            // node's `HTTP(S)_PROXY` — which in a Code Studio deployment points
            // at the egress gateway meant for sandboxes — from swallowing the
            // one connection that already carries organization credentials.
            .timeout(UPSTREAM_TIMEOUT)
            .no_proxy()
            .build()
            .context("adapter upstream client")?;
        Ok(Self {
            inner: Arc::new(AdapterInner {
                wiring,
                upstream,
                credential,
                tickets,
                sink,
                client,
            }),
            acceptor,
            listener,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Accept loop. One task per connection; a failing connection never takes
    /// the listener down, because losing the adapter would leave the CLI without
    /// a provider rather than with an unmediated one.
    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, peer)) => {
                    let inner = self.inner.clone();
                    let acceptor = self.acceptor.clone();
                    tokio::spawn(async move {
                        if let Err(error) = serve_connection(stream, acceptor, inner).await {
                            debug!(%peer, "provider adapter connection ended: {error:#}");
                        }
                    });
                }
                Err(error) => {
                    warn!("provider adapter accept failed: {error:#}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
}

/// What starting an adapter for one session needs. Assembled by the caller
/// (`delegate_cli`), because every field is a decision that belongs to the
/// session, not to this module.
pub struct AdapterConfig {
    /// Loopback in native mode, the container bridge address in namespace mode.
    /// Port 0 asks the OS for a free one, which is the normal choice: the CLI
    /// learns the address from its base URL, not from a convention.
    pub bind_addr: SocketAddr,
    pub engine_id: String,
    pub org_id: String,
    pub node_id: String,
    /// Where the session CA is written for the sandbox to read — inside the
    /// session's tmp directory (`paths::session_tmp_dir`).
    pub ca_path: PathBuf,
    /// Configuration directory handed to the CLI, per session and per engine.
    /// Created empty, so the CLI starts with no account of its own; it is the
    /// session, not the host user, that the CLI is logged into.
    pub cli_home_dir: PathBuf,
    /// How the workspace enforces egress. Read by the Phase 0B gate for an
    /// engine whose traffic the adapter does not fully capture.
    pub egress_enforcement: EgressEnforcement,
    /// Names the adapter's certificate is valid for. `localhost` plus whatever
    /// hostname the sandbox will resolve.
    pub dns_names: Vec<String>,
    pub tickets: Arc<TicketRegistry>,
    pub sink: Arc<dyn AdapterEventSink>,
}

/// A running adapter, and the only handle `delegate_cli` needs.
pub struct AdapterHandle {
    local_addr: SocketAddr,
    wiring: EngineWiring,
    ca_path: PathBuf,
    cli_home_dir: PathBuf,
    tickets: Arc<TicketRegistry>,
    task: tokio::task::JoinHandle<()>,
}

impl AdapterHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn tickets(&self) -> &Arc<TicketRegistry> {
        &self.tickets
    }

    /// The engine wiring this adapter was started with — the ticket's method
    /// and path allowlist come from here, so the issuer and the validator read
    /// one table.
    pub fn wiring(&self) -> &EngineWiring {
        &self.wiring
    }

    /// The base URL the CLI is pointed at — the adapter's own listening socket.
    pub fn base_url(&self) -> String {
        format!("https://{}", self.local_addr)
    }

    /// Arguments the CLI has to be started with, for an engine whose base URL
    /// is not an environment variable.
    pub fn cli_args(&self) -> Vec<String> {
        self.wiring.cli_args(&self.base_url())
    }

    /// Environment a sandboxed CLI needs: the adapter as its base URL, the
    /// ticket as its API key, a session-private configuration directory, the
    /// isolation switches the vendor honours, and the session CA as the only
    /// extra trust anchor.
    ///
    /// The organization's credential appears in NONE of these — that is the
    /// property §24 tests by scanning the CLI process's mounts, environment and
    /// argv. Nothing else may be added here; a second variable carrying real key
    /// material would silently undo the whole design.
    pub fn sandbox_env(&self, ticket: &IssuedTicket) -> Vec<(String, String)> {
        let ca = self.ca_path.display().to_string();
        let mut env = Vec::new();
        if let Some(base_url_var) = &self.wiring.base_url_var {
            env.push((base_url_var.clone(), self.base_url()));
        }
        env.push((self.wiring.api_key_var.clone(), ticket.presentation.clone()));
        env.push((
            self.wiring.config_dir_var.clone(),
            self.cli_home_dir.display().to_string(),
        ));
        env.extend(self.wiring.isolation_env.iter().cloned());
        // One anchor per runtime family, because a CLI is a Node process
        // today and may be a Rust or Python one tomorrow. Each names the
        // same file.
        env.push(("NODE_EXTRA_CA_CERTS".to_string(), ca.clone()));
        env.push(("SSL_CERT_FILE".to_string(), ca.clone()));
        env.push(("REQUESTS_CA_BUNDLE".to_string(), ca));
        env
    }

    /// Stops accepting. Called when the session's CLI work ends: the credential
    /// only lives in memory while an adapter does, so an adapter nobody stops is
    /// key material nobody released.
    pub fn shutdown(&self) {
        self.task.abort();
    }
}

/// Starts the adapter for one engine of one session.
///
/// Only `DelegationAuth::OrgCredential` reaches this function — the mode is
/// decided by `resolve_delegation_auth` before anything is started — so the
/// missing vault row here is a genuine inconsistency (the row was deleted
/// between the decision and the start), not the ordinary state of a node whose
/// CLI carries its own login.
///
/// The order of the three steps is the contract:
///   1. the Phase 0B gate (an unverified engine never gets this far),
///   2. the vault row (no row → `credential_missing`, and `delegate_cli` must
///      refuse to start rather than run the CLI unauthenticated),
///   3. bind and serve.
pub async fn start_adapter(
    core_db: &DbPool,
    cipher: &crate::crypto::SettingsCipher,
    config: AdapterConfig,
) -> Result<AdapterHandle> {
    ensure_engine_verified(core_db, &config.engine_id, config.egress_enforcement)?;
    let wiring = EngineWiring::for_engine(&config.engine_id)?;
    // The directory has to exist before the CLI reads it, and it has to be OURS:
    // an engine that finds a credential of its own in there never presents the
    // ticket (§17.1 point 7).
    std::fs::create_dir_all(&config.cli_home_dir).with_context(|| {
        format!(
            "create the session's CLI configuration directory {}",
            config.cli_home_dir.display()
        )
    })?;
    let credential = super::vault::get_agent_credential(
        core_db,
        cipher,
        &config.org_id,
        &config.node_id,
        &config.engine_id,
    )?;
    let trust = SessionTrust::generate(&config.ca_path, &config.dns_names)?;
    let adapter = ProviderAdapter::bind(
        config.bind_addr,
        wiring.clone(),
        credential,
        &trust,
        config.tickets.clone(),
        config.sink,
    )
    .await?;
    let local_addr = adapter.local_addr();
    let task = tokio::spawn(adapter.run());
    Ok(AdapterHandle {
        local_addr,
        wiring,
        ca_path: config.ca_path,
        cli_home_dir: config.cli_home_dir,
        tickets: config.tickets,
        task,
    })
}

/// One TLS connection carrying exactly one request. `Connection: close` is not
/// an optimization choice: a keep-alive socket approved for the first request
/// would carry a second one that was never screened.
async fn serve_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    inner: Arc<AdapterInner>,
) -> Result<()> {
    let mut tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
        .await
        .map_err(|_| anyhow!("TLS handshake timed out"))??;

    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut tls)).await {
        Ok(Ok(head)) => head,
        Ok(Err(error)) => {
            write_error(
                &mut tls,
                400,
                "Bad Request",
                "malformed_request",
                &format!("{error:#}"),
            )
            .await;
            return Ok(());
        }
        Err(_) => return Ok(()),
    };
    let body = match read_body(&mut tls, &head).await {
        Ok(body) => body,
        Err(error) => {
            write_error(
                &mut tls,
                413,
                "Payload Too Large",
                "body_rejected",
                &format!("{error:#}"),
            )
            .await;
            return Ok(());
        }
    };

    // The connectivity probe is answered here, before the ticket is consulted:
    // it carries no authentication (so every ticket rule would reject it) and it
    // asks about the adapter, not about the provider. Nothing is forwarded, no
    // budget is spent, and no denial is recorded — a session must not open with
    // a refusal event for a request that was never going upstream.
    if inner
        .wiring
        .is_unauthenticated_probe(&head.method, &normalized_path(&head.target))
    {
        let _ = tls
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        let _ = tls.flush().await;
        return Ok(());
    }

    let presented = head
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer ").or(Some(value)))
        .or_else(|| head.header("x-api-key"))
        .map(str::trim);
    let model = model_of(&body);
    let facts = RequestFacts {
        method: &head.method,
        path: &head.target,
        model: model.as_deref(),
        body_len: body.len(),
        cli_instance_id: None,
    };
    let claims = match inner.tickets.authorize(presented, &facts) {
        Ok(claims) => claims,
        Err(rejection) => {
            inner.sink.record(EventPayload::Egress {
                url: format!("{}{}", inner.upstream, head.target.trim_start_matches('/')),
                allowed: false,
                reason: rejection.to_string(),
            });
            let (status, reason) = rejection.status();
            write_error(
                &mut tls,
                status,
                reason,
                rejection.slug(),
                &rejection.detail(),
            )
            .await;
            return Ok(());
        }
    };

    inner.sink.record(EventPayload::Egress {
        url: format!("{}{}", inner.upstream, head.target.trim_start_matches('/')),
        allowed: true,
        reason: format!(
            "ticket {} run {} model {}",
            claims.ticket_id, claims.run_id, claims.model
        ),
    });
    forward(&mut tls, head, body, claims, inner).await
}

/// Forwards one request upstream with the vault credential injected, relays the
/// answer and meters it on the way through.
async fn forward(
    tls: &mut tokio_rustls::server::TlsStream<TcpStream>,
    head: RequestHead,
    body: Vec<u8>,
    claims: TicketClaims,
    inner: Arc<AdapterInner>,
) -> Result<()> {
    // Concatenated rather than `Url::join`ed: joining a relative path against a
    // base that has its own path segment (`https://host/api`) would REPLACE that
    // segment, sending the request somewhere the operator never configured.
    let path = head.target.trim_start_matches('/');
    let url = url::Url::parse(&format!(
        "{}/{path}",
        inner.upstream.as_str().trim_end_matches('/')
    ))
    .with_context(|| format!("build upstream url for {path}"))?;
    let method =
        reqwest::Method::from_bytes(head.method.as_bytes()).context("unsupported HTTP method")?;
    let mut request = inner.client.request(method, url);
    for (name, value) in &head.headers {
        if is_hop_by_hop(name) || is_credential_header(name) || name.eq_ignore_ascii_case("host") {
            continue;
        }
        request = request.header(name, value);
    }
    // The one place organization key material is used. It exists in this
    // process, in this call, and nowhere the sandbox can observe.
    request = match inner.wiring.credential_header {
        CredentialHeader::AuthorizationBearer => request.header(
            "authorization",
            format!("Bearer {}", inner.credential.material.expose()),
        ),
        CredentialHeader::XApiKey => {
            request.header("x-api-key", inner.credential.material.expose())
        }
    };
    let response = match request.body(body).send().await {
        Ok(response) => response,
        Err(error) => {
            write_error(
                tls,
                502,
                "Bad Gateway",
                "upstream_failed",
                &format!("the provider could not be reached: {error}"),
            )
            .await;
            return Ok(());
        }
    };

    let status = response.status();
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("OK")
    );
    for (name, value) in response.headers() {
        let name = name.as_str();
        if is_hop_by_hop(name)
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        if let Ok(value) = value.to_str() {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    let bodyless = matches!(status.as_u16(), 204 | 304);
    if !bodyless {
        out.push_str("Transfer-Encoding: chunked\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    tls.write_all(out.as_bytes()).await?;

    if bodyless {
        tls.shutdown().await.ok();
        return Ok(());
    }

    let mut meter = TokenMeter::default();
    let mut response = response;
    let mut bytes_down: u64 = 0;
    // What has already been charged for THIS response. The provider's counters
    // are cumulative, the registry adds up, so only the difference is booked.
    let mut booked = (0_u64, 0_u64);
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                debug!("provider adapter upstream stream ended: {error}");
                break;
            }
        };
        bytes_down = bytes_down.saturating_add(chunk.len() as u64);
        meter.observe(&chunk);
        tls.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await?;
        tls.write_all(&chunk).await?;
        tls.write_all(b"\r\n").await?;

        // Booking DURING the stream is what makes the budget stop traffic
        // instead of describing it afterwards.
        let delta = Usage {
            requests: 0,
            input_tokens: meter.input.saturating_sub(booked.0),
            output_tokens: meter.output.saturating_sub(booked.1),
            bytes_up: 0,
            bytes_down: chunk.len() as u64,
        };
        booked = (meter.input, meter.output);
        if let Some(what) = inner.tickets.record(&claims.ticket_id, delta) {
            inner.sink.record(EventPayload::Egress {
                url: inner.upstream.to_string(),
                allowed: false,
                reason: format!(
                    "ticket {} stopped mid-response: {what} budget exhausted",
                    claims.ticket_id
                ),
            });
            // Dropping the upstream response cancels the provider stream; the
            // client's connection is closed without a terminating chunk, which
            // is how a truncated answer is supposed to look.
            drop(response);
            tls.shutdown().await.ok();
            return Ok(());
        }
    }
    // The tail: a non-streaming body reports its usage only once the whole
    // document has been parsed, which happens in `finish`.
    meter.finish();
    inner.tickets.record(
        &claims.ticket_id,
        Usage {
            requests: 0,
            input_tokens: meter.input.saturating_sub(booked.0),
            output_tokens: meter.output.saturating_sub(booked.1),
            bytes_up: 0,
            bytes_down: 0,
        },
    );
    tls.write_all(b"0\r\n\r\n").await?;
    tls.shutdown().await.ok();
    debug!(
        ticket = %claims.ticket_id,
        run = %claims.run_id,
        input_tokens = meter.input,
        output_tokens = meter.output,
        bytes_down,
        "provider adapter relayed a response"
    );
    Ok(())
}

/// Counts tokens off the wire.
///
/// Both provider shapes report cumulative counters, in a JSON body or in SSE
/// `data:` lines, so the running maximum is the honest reading. A response that
/// reports nothing spends the request and byte budget and no tokens — an
/// invented estimate would be a number that looks measured and is not.
#[derive(Default)]
struct TokenMeter {
    input: u64,
    output: u64,
    line: Vec<u8>,
    buffered: Vec<u8>,
    saw_usage: bool,
}

impl TokenMeter {
    fn observe(&mut self, chunk: &[u8]) {
        if self.buffered.len() < METER_BUFFER_BYTES {
            let room = METER_BUFFER_BYTES - self.buffered.len();
            self.buffered
                .extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        for byte in chunk {
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.observe_line(&line);
            } else if self.line.len() < 1024 * 1024 {
                self.line.push(*byte);
            }
        }
    }

    fn observe_line(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let payload = text.trim().trim_start_matches("data:").trim();
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            self.observe_json(&value);
        }
    }

    /// Called when the body ends: a non-streaming answer is one JSON document
    /// that may never have contained a newline.
    fn finish(&mut self) {
        let line = std::mem::take(&mut self.line);
        if !line.is_empty() {
            self.observe_line(&line);
        }
        if self.saw_usage {
            return;
        }
        let buffered = std::mem::take(&mut self.buffered);
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&buffered) {
            self.observe_json(&value);
        }
    }

    fn observe_json(&mut self, value: &serde_json::Value) {
        // `usage` sits at the top level of a completion, and one level down in
        // an Anthropic `message_start` frame.
        for usage in [value.get("usage"), value.pointer("/message/usage")]
            .into_iter()
            .flatten()
        {
            let input = usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(serde_json::Value::as_u64);
            let output = usage
                .get("output_tokens")
                .or_else(|| usage.get("completion_tokens"))
                .and_then(serde_json::Value::as_u64);
            let total = usage
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64);
            if let Some(input) = input {
                self.input = self.input.max(input);
                self.saw_usage = true;
            }
            if let Some(output) = output {
                self.output = self.output.max(output);
                self.saw_usage = true;
            }
            if let (Some(total), None, None) = (total, input, output) {
                self.output = self.output.max(total);
                self.saw_usage = true;
            }
        }
    }
}

/// The model named in a request body, when there is one. A body without a model
/// is not rejected here — the ticket's method and path allowlist still applies —
/// because not every provider call names a model (`/models`, `/health`).
fn model_of(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

fn is_hop_by_hop(name: &str) -> bool {
    const HOP: [&str; 8] = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];
    HOP.iter().any(|hop| name.eq_ignore_ascii_case(hop))
}

/// Headers the CLI may have set that must never reach the provider: they carry
/// the TICKET, and the provider gets the credential instead.
fn is_credential_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("x-api-key")
        || name.eq_ignore_ascii_case("api-key")
}

// =============================================================================
// Minimal HTTP/1.1 reading
// =============================================================================

struct RequestHead {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    rest: Vec<u8>,
}

impl RequestHead {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn read_head<S>(stream: &mut S) -> Result<RequestHead>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(anyhow!("the client closed before sending a request"));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(anyhow!("request head exceeds {MAX_HEAD_BYTES} bytes"));
        }
        if let Some(end) = find_head_end(&buffer) {
            let rest = buffer.split_off(end);
            let text = String::from_utf8_lossy(&buffer).into_owned();
            let mut lines = text.split("\r\n");
            let request_line = lines.next().unwrap_or_default();
            let mut parts = request_line.split_whitespace();
            let method = parts
                .next()
                .ok_or_else(|| anyhow!("request line has no method"))?
                .to_string();
            let target = parts
                .next()
                .ok_or_else(|| anyhow!("request line has no target"))?
                .to_string();
            let mut headers = Vec::new();
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                let (name, value) = line
                    .split_once(':')
                    .ok_or_else(|| anyhow!("malformed header line"))?;
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
            return Ok(RequestHead {
                method,
                target,
                headers,
                rest,
            });
        }
    }
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

async fn read_body<S>(stream: &mut S, head: &RequestHead) -> Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut body = head.rest.clone();
    if head
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return read_chunked(stream, body).await;
    }
    let length: usize = head
        .header("content-length")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    if length > MAX_REQUEST_BODY_BYTES {
        return Err(anyhow!(
            "request body of {length} bytes exceeds the adapter limit"
        ));
    }
    let mut chunk = [0_u8; 8192];
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > MAX_REQUEST_BODY_BYTES {
            return Err(anyhow!("request body exceeds the adapter limit"));
        }
    }
    body.truncate(length.min(body.len()));
    Ok(body)
}

async fn read_chunked<S>(stream: &mut S, mut pending: Vec<u8>) -> Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut body = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        // Frame boundary: <hex size>\r\n<data>\r\n
        let Some(line_end) = pending
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|position| position + 2)
        else {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(anyhow!("chunked body ended mid-frame"));
            }
            pending.extend_from_slice(&chunk[..read]);
            continue;
        };
        let size_line = String::from_utf8_lossy(&pending[..line_end - 2]).into_owned();
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .context("chunk size is not hexadecimal")?;
        if size == 0 {
            return Ok(body);
        }
        if body.len() + size > MAX_REQUEST_BODY_BYTES {
            return Err(anyhow!("chunked request body exceeds the adapter limit"));
        }
        while pending.len() < line_end + size + 2 {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(anyhow!("chunked body ended mid-frame"));
            }
            pending.extend_from_slice(&chunk[..read]);
        }
        body.extend_from_slice(&pending[line_end..line_end + size]);
        pending.drain(..line_end + size + 2);
    }
}

async fn write_error<S>(stream: &mut S, status: u16, reason: &str, slug: &str, detail: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::json!({"error": {"type": slug, "message": detail}}).to_string();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{AutonomyMode, WorkspaceRole};

    fn ctx() -> pep::SessionCtx {
        pep::SessionCtx {
            role: WorkspaceRole::Editor,
            autonomy: AutonomyMode::Normal,
            is_coordinator: false,
            has_accepted_patch_set: false,
            allowlisted: false,
            session_granted: true,
            run_granted: false,
        }
    }

    fn request() -> TicketRequest {
        TicketRequest {
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            cli_instance_id: "cli-1".into(),
            engine_id: "claude-code".into(),
            model: "sonnet".into(),
            model_aliases: ["claude-sonnet-4-5".to_string()].into_iter().collect(),
            methods: ["POST".to_string()].into_iter().collect(),
            path_prefixes: vec!["/v1/messages".to_string()],
            budget: Budget {
                max_requests: 3,
                max_total_tokens: 1_000,
                max_bytes: 10_000,
            },
            ttl: Duration::from_secs(300),
            host_allowlisted: true,
        }
    }

    fn facts<'a>(method: &'a str, path: &'a str, model: Option<&'a str>) -> RequestFacts<'a> {
        RequestFacts {
            method,
            path,
            model,
            body_len: 10,
            cli_instance_id: None,
        }
    }

    fn issued(registry: &TicketRegistry, req: TicketRequest) -> IssuedTicket {
        match issue_ticket(registry, &ctx(), req).expect("issue") {
            TicketDecision::Issued(ticket) => *ticket,
            other => panic!("expected an issued ticket, got {other:?}"),
        }
    }

    #[test]
    fn issuing_a_ticket_needs_cli_delegate_through_the_pep() {
        let registry = TicketRegistry::new();

        // A viewer does not hold `cli_delegate` at all.
        let mut viewer = ctx();
        viewer.role = WorkspaceRole::Viewer;
        assert!(matches!(
            issue_ticket(&registry, &viewer, request()).expect("decide"),
            TicketDecision::Denied { .. }
        ));

        // `plan` forbids delegation outright.
        let mut planning = ctx();
        planning.autonomy = AutonomyMode::Plan;
        assert!(matches!(
            issue_ticket(&registry, &planning, request()).expect("decide"),
            TicketDecision::Denied { .. }
        ));

        // Holding net_egress is not holding cli_delegate: without a grant the
        // answer is a question, not a ticket.
        let mut ungranted = ctx();
        ungranted.session_granted = false;
        assert!(matches!(
            issue_ticket(&registry, &ungranted, request()).expect("decide"),
            TicketDecision::Ask { .. }
        ));

        // A provider host outside the allowlist is refused before anything else.
        let mut off_allowlist = request();
        off_allowlist.host_allowlisted = false;
        assert!(matches!(
            issue_ticket(&registry, &ctx(), off_allowlist).expect("decide"),
            TicketDecision::Denied { .. }
        ));
    }

    #[test]
    fn a_ticket_without_a_model_or_a_path_list_is_refused() {
        let registry = TicketRegistry::new();
        let mut no_model = request();
        no_model.model = "  ".into();
        assert!(matches!(
            issue_ticket(&registry, &ctx(), no_model).expect("decide"),
            TicketDecision::Denied { .. }
        ));

        let mut no_paths = request();
        no_paths.path_prefixes.clear();
        assert!(matches!(
            issue_ticket(&registry, &ctx(), no_paths).expect("decide"),
            TicketDecision::Denied { .. }
        ));
    }

    /// The catalog offers `sonnet`; the CLI sends `claude-sonnet-4-5-<date>`.
    /// A ticket that only compared the two vocabularies literally refused every
    /// request an operator could ever have configured — so the alias binds the
    /// FAMILY, and nothing wider.
    #[test]
    fn a_ticket_for_a_vendor_alias_accepts_the_dated_id_the_cli_really_sends() {
        let registry = TicketRegistry::new();
        // The request budget of `request()` is deliberately tiny, and this test
        // spends a request per spelling; a budget refusal here would look like
        // a model refusal and prove nothing.
        let mut roomy = request();
        roomy.budget.max_requests = 100;
        let ticket = issued(&registry, roomy);
        let presented = Some(ticket.presentation.as_str());

        for wire_id in [
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-5",
            "claude-3-5-sonnet-20241022",
            "CLAUDE-SONNET-4-5-20250929",
        ] {
            assert!(
                registry
                    .authorize(presented, &facts("POST", "/v1/messages", Some(wire_id)))
                    .is_ok(),
                "'{wire_id}' is the same model the ticket was minted for"
            );
        }

        // Another family, a neighbouring vendor's id and a spelling that only
        // contains the family token stay refused: the ticket still binds ONE
        // model, it merely knows how that model is spelled on the wire.
        for foreign in [
            "claude-opus-4-5-20251101",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-preview",
            "sonnet-of-someone-else",
            "gpt-5-codex",
        ] {
            assert_eq!(
                registry
                    .authorize(presented, &facts("POST", "/v1/messages", Some(foreign)))
                    .unwrap_err()
                    .slug(),
                "model_not_allowed",
                "'{foreign}' is not the model this ticket paid for"
            );
        }
    }

    /// A configured DATED id keeps exact matching: an operator who pinned one
    /// snapshot did not ask for the family.
    #[test]
    fn a_ticket_for_a_dated_id_does_not_widen_to_its_family() {
        let registry = TicketRegistry::new();
        let mut pinned = request();
        pinned.model = "claude-sonnet-4-5-20250929".into();
        pinned.model_aliases.clear();
        pinned.budget.max_requests = 100;
        let ticket = issued(&registry, pinned);
        let presented = Some(ticket.presentation.as_str());

        assert!(registry
            .authorize(
                presented,
                &facts("POST", "/v1/messages", Some("claude-sonnet-4-5-20250929"))
            )
            .is_ok());
        assert_eq!(
            registry
                .authorize(
                    presented,
                    &facts("POST", "/v1/messages", Some("claude-sonnet-4-5-20260101"))
                )
                .unwrap_err()
                .slug(),
            "model_not_allowed"
        );
    }

    /// `opusplan` addresses two models in one turn, so no single-model ticket
    /// can express it. Refusing it at issuance is the honest answer; minting a
    /// ticket that refuses every request is not.
    #[test]
    fn an_alias_that_resolves_to_two_models_cannot_bind_a_ticket() {
        assert_eq!(ticket_model_binding("claude-code", "sonnet"), Ok(Some("sonnet".into())));
        assert_eq!(ticket_model_binding("claude-code", "SONNET"), Ok(Some("sonnet".into())));
        assert_eq!(
            ticket_model_binding("claude-code", "claude-sonnet-4-5-20250929"),
            Ok(None)
        );
        assert_eq!(ticket_model_binding("codex", "gpt-5-codex"), Ok(None));
        assert!(ticket_model_binding("claude-code", "opusplan").is_err());

        let registry = TicketRegistry::new();
        let mut composite = request();
        composite.model = "opusplan".into();
        composite.model_aliases.clear();
        let TicketDecision::Denied { reason } =
            issue_ticket(&registry, &ctx(), composite).expect("decide")
        else {
            panic!("a two-model alias must not mint a ticket");
        };
        assert!(reason.contains("two models"), "{reason}");
    }

    #[test]
    fn a_ticket_authorizes_its_own_shape_and_nothing_else() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        let presented = Some(ticket.presentation.as_str());

        assert!(registry
            .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
            .is_ok());
        // An alias the caller declared resolves to the same model.
        assert!(registry
            .authorize(
                presented,
                &facts("POST", "/v1/messages", Some("claude-sonnet-4-5"))
            )
            .is_ok());

        assert_eq!(
            registry.authorize(presented, &facts("POST", "/v1/messages", Some("opus"))),
            Err(TicketRejection::ModelNotAllowed("opus".into())),
            "a ticket for one model must not pay for another"
        );
        assert_eq!(
            registry
                .authorize(presented, &facts("DELETE", "/v1/messages", Some("sonnet")))
                .unwrap_err()
                .slug(),
            "method_not_allowed"
        );
        assert_eq!(
            registry
                .authorize(presented, &facts("POST", "/v1/admin/keys", Some("sonnet")))
                .unwrap_err()
                .slug(),
            "path_not_allowed"
        );
        assert_eq!(
            registry
                .authorize(
                    presented,
                    &facts("POST", "/v1/messages/../../admin", Some("sonnet"))
                )
                .unwrap_err()
                .slug(),
            "path_not_allowed",
            "a traversal must not walk out of the allowed prefix"
        );
        assert_eq!(
            registry
                .authorize(
                    Some("tfck_deadbeef_nope"),
                    &facts("POST", "/v1/messages", None)
                )
                .unwrap_err()
                .slug(),
            "ticket_unknown"
        );
        assert_eq!(
            registry
                .authorize(
                    Some("sk-a-real-looking-key"),
                    &facts("POST", "/v1/messages", None)
                )
                .unwrap_err()
                .slug(),
            "ticket_malformed",
            "a provider-shaped credential is not a ticket"
        );
        assert_eq!(
            registry
                .authorize(None, &facts("POST", "/v1/messages", None))
                .unwrap_err()
                .slug(),
            "ticket_missing"
        );

        // The right id with the wrong secret is refused, and the comparison is
        // constant time.
        let forged = format!("tfck_{}_{}", ticket.claims.ticket_id, "AAAA");
        assert_eq!(
            registry
                .authorize(Some(&forged), &facts("POST", "/v1/messages", None))
                .unwrap_err()
                .slug(),
            "ticket_bad_secret"
        );
    }

    #[test]
    fn a_ticket_is_bound_to_its_run_and_dies_with_it() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        let presented = Some(ticket.presentation.as_str());

        let mut other_instance = facts("POST", "/v1/messages", Some("sonnet"));
        other_instance.cli_instance_id = Some("cli-2");
        assert_eq!(
            registry.authorize(presented, &other_instance).unwrap_err(),
            TicketRejection::WrongInstance,
            "a ticket replayed into another CLI instance must be refused"
        );

        assert_eq!(registry.revoke_run("run-1"), 1);
        assert_eq!(
            registry
                .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
                .unwrap_err(),
            TicketRejection::Revoked,
            "the ticket must not survive its run"
        );
    }

    #[test]
    fn an_expired_ticket_is_refused() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        {
            let mut tickets = registry.tickets.lock().expect("registry");
            let state = tickets
                .get_mut(&ticket.claims.ticket_id)
                .expect("just issued");
            state.claims.expires_at_unix = now_unix() - 1;
        }
        assert_eq!(
            registry
                .authorize(
                    Some(ticket.presentation.as_str()),
                    &facts("POST", "/v1/messages", Some("sonnet"))
                )
                .unwrap_err(),
            TicketRejection::Expired
        );
    }

    #[test]
    fn the_budget_stops_traffic_rather_than_describing_it() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        let presented = Some(ticket.presentation.as_str());

        // Request ceiling: the fourth call has no slot left, and the refusal is
        // permanent for that ticket.
        for _ in 0..3 {
            assert!(registry
                .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
                .is_ok());
        }
        assert_eq!(
            registry
                .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
                .unwrap_err()
                .slug(),
            "budget_exhausted"
        );

        // Token ceiling: recording a response over the budget both reports the
        // crossing to the relay loop and closes the ticket for good.
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        let presented = Some(ticket.presentation.as_str());
        assert!(registry
            .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
            .is_ok());
        assert_eq!(
            registry.record(
                &ticket.claims.ticket_id,
                Usage {
                    input_tokens: 900,
                    output_tokens: 200,
                    ..Usage::default()
                }
            ),
            Some("tokens"),
            "the relay loop must be told to stop mid-response"
        );
        assert_eq!(
            registry
                .authorize(presented, &facts("POST", "/v1/messages", Some("sonnet")))
                .unwrap_err(),
            TicketRejection::BudgetExhausted("tokens")
        );
    }

    #[test]
    fn usage_is_read_off_the_wire_in_both_provider_shapes() {
        // Anthropic-style streaming: cumulative counters across frames.
        let mut meter = TokenMeter::default();
        meter.observe(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":\
              {\"input_tokens\":120,\"output_tokens\":1}}}\n\n",
        );
        meter.observe(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":57}}\n\n");
        meter.finish();
        assert_eq!((meter.input, meter.output), (120, 57));

        // OpenAI-style single JSON document with no newline at all.
        let mut meter = TokenMeter::default();
        meter.observe(br#"{"id":"x","usage":{"prompt_tokens":11,"completion_tokens":22}}"#);
        meter.finish();
        assert_eq!((meter.input, meter.output), (11, 22));

        // A response that reports nothing yields nothing: the request and byte
        // budgets still applied, but no token count is invented.
        let mut meter = TokenMeter::default();
        meter.observe(b"plain text answer\n");
        meter.finish();
        assert_eq!((meter.input, meter.output), (0, 0));
    }

    #[test]
    fn the_ticket_never_travels_upstream_and_the_credential_never_travels_down() {
        // Header policy, stated as a test because it is the whole point of the
        // adapter: whatever the CLI presents as a credential is dropped, and the
        // vault material is added by the adapter alone.
        for name in ["authorization", "Authorization", "x-api-key", "API-Key"] {
            assert!(is_credential_header(name), "{name} must be stripped");
        }
        assert!(!is_credential_header("content-type"));
        assert!(is_hop_by_hop("Transfer-Encoding"));
    }

    #[tokio::test]
    async fn the_sandbox_environment_carries_a_ticket_and_no_credential() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        let handle = AdapterHandle {
            local_addr: "127.0.0.1:9443".parse().expect("addr"),
            wiring: EngineWiring::for_engine("claude-code").expect("wiring"),
            ca_path: PathBuf::from("/tmp/session/ca.pem"),
            cli_home_dir: PathBuf::from("/tmp/session/cli-claude-code-home"),
            tickets: Arc::new(TicketRegistry::new()),
            task: tokio::spawn(async {}),
        };
        let env: HashMap<String, String> = handle.sandbox_env(&ticket).into_iter().collect();
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://127.0.0.1:9443")
        );
        assert!(
            env.get("ANTHROPIC_API_KEY")
                .is_some_and(|value| value.starts_with("tfck_")),
            "the sandbox gets a ticket, never a provider key"
        );
        // §17.1 points 6 and 7 are these two variables. Without them the CLI was
        // measured reaching api.anthropic.com, github.com and a Datadog intake,
        // and picking up whatever account this host is logged into.
        assert_eq!(
            env.get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/tmp/session/cli-claude-code-home")
        );
        assert!(EngineWiring::for_engine("some-other-cli").is_err());
    }

    /// `OPENAI_BASE_URL` was measured to be ignored: with it set, 100% of the
    /// CLI's traffic still went to the vendor. The wiring therefore does not
    /// declare it, and the redirect is carried by the arguments the process is
    /// started with.
    #[test]
    fn codex_is_redirected_by_configuration_arguments_not_by_an_environment_variable() {
        let wiring = EngineWiring::for_engine("codex").expect("wiring");
        assert_eq!(wiring.base_url_var, None);
        assert_eq!(wiring.config_dir_var, "CODEX_HOME");
        assert_eq!(wiring.api_key_var, CODEX_TICKET_VAR);
        assert_eq!(
            wiring.cli_args("https://127.0.0.1:9443/"),
            vec![
                "-c",
                "model_provider=tfadapter",
                "-c",
                "model_providers.tfadapter.base_url=https://127.0.0.1:9443/v1",
                "-c",
                "model_providers.tfadapter.env_key=TF_TICKET",
                "-c",
                "model_providers.tfadapter.wire_api=responses",
            ]
        );
        assert!(
            EngineWiring::for_engine("claude-code")
                .expect("wiring")
                .cli_args("https://127.0.0.1:9443")
                .is_empty(),
            "claude's base URL is an environment variable; it needs no arguments"
        );
    }

    #[test]
    fn an_unverified_engine_cannot_be_delegated_to() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        let db = crate::db::init(file.path()).expect("db");
        let contained = EgressEnforcement::Namespace;

        let refusal =
            ensure_engine_verified(&db, "claude-code", contained).expect_err("default is off");
        assert_eq!(refusal.engine_id, "claude-code");
        assert!(
            refusal.reason.contains("Phase 0B"),
            "the refusal must name the missing verification: {}",
            refusal.reason
        );

        // The flag alone is not enough — the note is the audit trail.
        crate::db::repository::set_setting(
            &db,
            &format!("{BASE_URL_OVERRIDE_VERIFIED_PREFIX}claude-code"),
            "true",
        )
        .expect("set flag");
        let refusal =
            ensure_engine_verified(&db, "claude-code", contained).expect_err("note is missing");
        assert!(refusal.reason.contains(GO_NO_GO_NOTE_PREFIX));

        crate::db::repository::set_setting(
            &db,
            &format!("{GO_NO_GO_NOTE_PREFIX}claude-code"),
            "claude-code 2.1.233 verified on 2026-08-14 by an operator",
        )
        .expect("set note");
        assert!(ensure_engine_verified(&db, "claude-code", contained).is_ok());
        // claude's traffic IS captured by the adapter, so the workspace's
        // enforcement is not part of its gate.
        assert!(
            ensure_engine_verified(&db, "claude-code", EgressEnforcement::Unrestricted).is_ok()
        );

        // Verification is per engine and never leaks to another one.
        assert!(ensure_engine_verified(&db, "codex", contained).is_err());
    }

    /// The organization's decision does not make codex's residual traffic go
    /// away. Phase 0B measured connections to chatgpt.com and api.github.com
    /// surviving the provider redirect, so the engine only runs where the
    /// gateway sees them — and the refusal says exactly that, rather than
    /// "engine not available".
    #[test]
    fn codex_is_refused_where_nothing_watches_the_traffic_the_adapter_misses() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        let db = crate::db::init(file.path()).expect("db");
        crate::db::repository::set_setting(
            &db,
            &format!("{BASE_URL_OVERRIDE_VERIFIED_PREFIX}codex"),
            "true",
        )
        .expect("set flag");
        crate::db::repository::set_setting(
            &db,
            &format!("{GO_NO_GO_NOTE_PREFIX}codex"),
            "codex 0.147.0, API key in the vault, decided on 2026-08-15",
        )
        .expect("set note");

        let refusal = ensure_engine_verified(&db, "codex", EgressEnforcement::Unrestricted)
            .expect_err("unrestricted has no mechanism for the residual traffic");
        for named in ["chatgpt.com", "api.github.com", "unrestricted", "CODEX_HOME"] {
            assert!(
                refusal.reason.contains(named),
                "the refusal must name what is missing, not merely refuse: {}",
                refusal.reason
            );
        }
        assert!(ensure_engine_verified(&db, "codex", EgressEnforcement::Namespace).is_ok());
        assert!(ensure_engine_verified(&db, "codex", EgressEnforcement::Firewall).is_ok());
    }

    /// Claude opens a session with an UNAUTHENTICATED `HEAD /api/hello`. It is
    /// the adapter's own liveness that is being asked about, so the adapter
    /// answers it; matching it against the ticket would deny a request that was
    /// never going upstream and put a refusal in every session's audit.
    #[test]
    fn the_connectivity_probe_is_answered_by_the_adapter_itself() {
        let claude = EngineWiring::for_engine("claude-code").expect("wiring");
        assert!(claude.is_unauthenticated_probe("HEAD", "/api/hello"));
        assert!(claude.is_unauthenticated_probe("head", "/api/hello"));
        // Nothing else is: the probe is one method on one path, not a hole.
        assert!(!claude.is_unauthenticated_probe("GET", "/api/hello"));
        assert!(!claude.is_unauthenticated_probe("HEAD", "/v1/messages"));
        assert!(!EngineWiring::for_engine("codex")
            .expect("wiring")
            .is_unauthenticated_probe("HEAD", "/api/hello"));
    }

    #[test]
    fn the_session_ca_is_written_for_the_sandbox_and_the_key_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_path = dir.path().join("adapter-ca.pem");
        let trust =
            SessionTrust::generate(&ca_path, &["localhost".to_string()]).expect("generate trust");
        let written = std::fs::read_to_string(&ca_path).expect("ca file");
        assert!(written.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_eq!(written, trust.ca_pem());
        assert!(
            !written.contains("PRIVATE KEY"),
            "the sandbox must receive a trust anchor, not a signing key"
        );
        // The acceptor builds, which is what proves the leaf and its key match.
        trust.acceptor().expect("tls acceptor");
    }

    #[test]
    fn paths_are_normalized_before_they_are_matched() {
        assert_eq!(normalized_path("/v1/messages?beta=1"), "/v1/messages");
        assert_eq!(normalized_path("/v1/./messages"), "/v1/messages");
        assert_eq!(normalized_path("/v1/messages/../../admin"), "/admin");
        assert_eq!(normalized_path("v1//messages"), "/v1/messages");
    }

    // =========================================================================
    // End to end, over a real TLS socket
    // =========================================================================

    /// The provider credential as the vault stores it. The test asserts this
    /// string reaches the PROVIDER and never the client.
    const PROVIDER_KEY: &str = "sk-provider-org-key-not-for-the-sandbox";

    struct RecordedRequest {
        head: String,
        body: String,
    }

    /// A stub provider: answers every request with one JSON document and records
    /// what it was sent.
    async fn stub_provider(
        response_body: &'static str,
    ) -> (SocketAddr, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorder = recorder.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    let (head, body) = loop {
                        let Ok(read) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        let Some(end) = find_head_end(&buffer) else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length: ")
                                    .or_else(|| line.strip_prefix("Content-Length: "))
                            })
                            .and_then(|value| value.trim().parse().ok())
                            .unwrap_or(0);
                        while buffer.len() < end + length {
                            let Ok(read) = socket.read(&mut chunk).await else {
                                return;
                            };
                            if read == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&chunk[..read]);
                        }
                        break (head, String::from_utf8_lossy(&buffer[end..]).into_owned());
                    };
                    recorder
                        .lock()
                        .expect("recorder")
                        .push(RecordedRequest { head, body });
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{response_body}",
                        response_body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (addr, seen)
    }

    struct CollectingSink(Mutex<Vec<EventPayload>>);

    impl AdapterEventSink for CollectingSink {
        fn record(&self, event: EventPayload) {
            self.0.lock().expect("sink").push(event);
        }
    }

    /// Puts a credential in the vault and starts an adapter in front of the stub
    /// provider. Returns everything the assertions need.
    async fn adapter_over_stub(
        upstream: SocketAddr,
    ) -> (
        SocketAddr,
        SessionTrust,
        Arc<TicketRegistry>,
        Arc<CollectingSink>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("db");
        let cipher = crate::crypto::SettingsCipher::new(&[9_u8; 32]);
        crate::code_studio::vault::put_agent_credential(
            &db,
            &cipher,
            "org-1",
            "node-1",
            "claude-code",
            PROVIDER_KEY,
            &format!("http://{upstream}"),
            "u-owner",
        )
        .expect("store credential");
        let credential = crate::code_studio::vault::get_agent_credential(
            &db,
            &cipher,
            "org-1",
            "node-1",
            "claude-code",
        )
        .expect("read credential");

        let trust = SessionTrust::generate(&dir.path().join("ca.pem"), &["localhost".to_string()])
            .expect("trust");
        let tickets = Arc::new(TicketRegistry::new());
        let sink = Arc::new(CollectingSink(Mutex::new(Vec::new())));
        let adapter = ProviderAdapter::bind(
            "127.0.0.1:0".parse().expect("addr"),
            EngineWiring::for_engine("claude-code").expect("wiring"),
            credential,
            &trust,
            tickets.clone(),
            sink.clone(),
        )
        .await
        .expect("bind adapter");
        let addr = adapter.local_addr();
        tokio::spawn(adapter.run());
        (addr, trust, tickets, sink, dir)
    }

    fn client_trusting(trust: &SessionTrust, adapter: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(trust.ca_pem().as_bytes()).expect("ca"),
            )
            // The sandbox reaches the adapter by name; forcing resolution here
            // is what lets the certificate be checked the way a CLI would check
            // it, instead of being bypassed with an IP.
            .resolve("localhost", adapter)
            .no_proxy()
            .build()
            .expect("client")
    }

    #[tokio::test]
    async fn the_provider_sees_the_vault_credential_and_the_client_never_does() {
        let (upstream, seen) =
            stub_provider(r#"{"usage":{"input_tokens":10,"output_tokens":5},"ok":true}"#).await;
        let (adapter, trust, tickets, sink, _dir) = adapter_over_stub(upstream).await;
        let ticket = issued(&tickets, request());
        let client = client_trusting(&trust, adapter);

        let response = client
            .post(format!("https://localhost:{}/v1/messages", adapter.port()))
            .header("x-api-key", &ticket.presentation)
            .header("content-type", "application/json")
            .body(r#"{"model":"sonnet","messages":[]}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 200);
        let body = response.text().await.expect("body");
        assert!(body.contains("\"ok\":true"), "the answer was not relayed");

        let recorded = seen.lock().expect("recorder");
        let request = recorded.first().expect("the provider was called");
        let head = request.head.to_ascii_lowercase();
        assert!(
            head.contains(&format!("x-api-key: {PROVIDER_KEY}").to_ascii_lowercase()),
            "the adapter did not inject the vault credential: {}",
            request.head
        );
        assert!(
            !request.head.contains(&ticket.presentation),
            "the ticket must not travel upstream"
        );
        assert_eq!(request.body, r#"{"model":"sonnet","messages":[]}"#);

        // Usage is booked from the wire, not from anything the client claims.
        let usage = tickets.usage(&ticket.claims.ticket_id).expect("usage");
        assert_eq!(usage.requests, 1);
        assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));

        // And the timeline saw one allowed egress naming the run and the model.
        let events = sink.0.lock().expect("sink");
        assert!(events.iter().any(|event| matches!(
            event,
            EventPayload::Egress { allowed: true, reason, .. } if reason.contains("run-1")
        )));
    }

    #[tokio::test]
    async fn a_request_outside_the_ticket_never_reaches_the_provider() {
        let (upstream, seen) = stub_provider(r#"{"ok":true}"#).await;
        let (adapter, trust, tickets, _sink, _dir) = adapter_over_stub(upstream).await;
        let ticket = issued(&tickets, request());
        let client = client_trusting(&trust, adapter);

        // Wrong model.
        let response = client
            .post(format!("https://localhost:{}/v1/messages", adapter.port()))
            .header("x-api-key", &ticket.presentation)
            .body(r#"{"model":"opus","messages":[]}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 403);
        assert!(response
            .text()
            .await
            .expect("body")
            .contains("model_not_allowed"));

        // No ticket at all.
        let response = client
            .post(format!("https://localhost:{}/v1/messages", adapter.port()))
            .body(r#"{"model":"sonnet"}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 403);

        // A path the ticket does not cover.
        let response = client
            .post(format!(
                "https://localhost:{}/v1/organizations",
                adapter.port()
            ))
            .header("x-api-key", &ticket.presentation)
            .body(r#"{"model":"sonnet"}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 403);

        assert!(
            seen.lock().expect("recorder").is_empty(),
            "a refused request must not reach the provider at all"
        );
    }

    #[tokio::test]
    async fn crossing_the_budget_stops_the_traffic() {
        let (upstream, seen) =
            stub_provider(r#"{"usage":{"input_tokens":400,"output_tokens":400},"ok":true}"#).await;
        let (adapter, trust, tickets, _sink, _dir) = adapter_over_stub(upstream).await;
        let mut small = request();
        small.budget = Budget {
            max_requests: 10,
            max_total_tokens: 100,
            max_bytes: 1_000_000,
        };
        let ticket = issued(&tickets, small);
        let client = client_trusting(&trust, adapter);
        let url = format!("https://localhost:{}/v1/messages", adapter.port());

        // The first call goes through and spends 800 tokens against a 100 token
        // ceiling; the adapter books that while it relays.
        let first = client
            .post(&url)
            .header("x-api-key", &ticket.presentation)
            .body(r#"{"model":"sonnet","messages":[]}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(first.status(), 200);
        let _ = first.text().await;

        // The second call is refused BEFORE the provider is asked — the budget
        // stops traffic rather than describing it afterwards.
        let second = client
            .post(&url)
            .header("x-api-key", &ticket.presentation)
            .body(r#"{"model":"sonnet","messages":[]}"#)
            .send()
            .await
            .expect("send");
        assert_eq!(second.status(), 429);
        assert!(second
            .text()
            .await
            .expect("body")
            .contains("budget_exhausted"));
        assert_eq!(
            seen.lock().expect("recorder").len(),
            1,
            "only the first request may have reached the provider"
        );
    }

    /// A ticket belongs to ONE run. Two delegations of the same session hold
    /// two tickets, and neither buys anything in the other's run — which is the
    /// property that makes a stolen ticket worth "what that run could already
    /// do" and nothing more (§7.5).
    #[test]
    fn a_ticket_of_another_run_authorizes_nothing_here() {
        let registry = TicketRegistry::new();
        let mut first = request();
        first.run_id = "run-a".into();
        first.cli_instance_id = "cli-a".into();
        let mut second = request();
        second.run_id = "run-b".into();
        second.cli_instance_id = "cli-b".into();
        let ticket_a = issued(&registry, first);
        let ticket_b = issued(&registry, second);

        let facts_for = |instance: &'static str| {
            let mut facts = facts("POST", "/v1/messages", Some("sonnet"));
            facts.cli_instance_id = Some(instance);
            facts
        };

        // Each ticket works in its own run…
        assert!(registry
            .authorize(Some(ticket_a.presentation.as_str()), &facts_for("cli-a"))
            .is_ok());
        assert!(registry
            .authorize(Some(ticket_b.presentation.as_str()), &facts_for("cli-b"))
            .is_ok());

        // …and in nobody else's, in either direction.
        assert_eq!(
            registry
                .authorize(Some(ticket_a.presentation.as_str()), &facts_for("cli-b"))
                .unwrap_err(),
            TicketRejection::WrongInstance
        );
        assert_eq!(
            registry
                .authorize(Some(ticket_b.presentation.as_str()), &facts_for("cli-a"))
                .unwrap_err(),
            TicketRejection::WrongInstance
        );

        // Ending one run does not disarm the other, and the ended one is told
        // `revoked` rather than the ambiguous `unknown`.
        assert_eq!(registry.revoke_run("run-a"), 1);
        assert_eq!(
            registry
                .authorize(Some(ticket_a.presentation.as_str()), &facts_for("cli-a"))
                .unwrap_err(),
            TicketRejection::Revoked
        );
        assert!(registry
            .authorize(Some(ticket_b.presentation.as_str()), &facts_for("cli-b"))
            .is_ok());
    }

    /// The registry is asked which ceiling was crossed, so the caller that ends
    /// a delegation and the adapter that cuts the traffic read one state rather
    /// than each forming an opinion.
    #[test]
    fn the_registry_reports_which_budget_it_stopped_on() {
        let registry = TicketRegistry::new();
        let ticket = issued(&registry, request());
        assert_eq!(registry.exhausted(&ticket.claims.ticket_id), None);
        registry.record(
            &ticket.claims.ticket_id,
            Usage {
                requests: 0,
                input_tokens: 900,
                output_tokens: 900,
                bytes_up: 0,
                bytes_down: 0,
            },
        );
        assert_eq!(
            registry.exhausted(&ticket.claims.ticket_id),
            Some("tokens"),
            "1800 tokens against a 1000-token ceiling"
        );
        assert_eq!(registry.exhausted("no-such-ticket"), None);
    }
}
