// ===== File: services/agent_account.rs — the one place a run's account is decided =====
//
// An agent that runs a CLI application does not carry an account: it carries an
// account BINDING (`agents.runtime_json`, migration 156) that names one of two
// modes. Resolving that binding into a concrete account, an engine and a node
// is a single decision, made here, for every caller — the delegation node, the
// console, a chat turn — so that "who ran this and on what" has exactly one
// answer to read back.
//
// The rule that shapes the whole file: a run never SWITCHES mode. `mode="user"`
// means the account of whoever is running the agent, and when that user has no
// account of their own for the engine the run is refused — not quietly served by
// a global account, and not quietly served by an API key. Falling back would
// hand a user somebody else's subscription (or the organisation's key) while
// telling them the opposite, and no amount of later logging would make that
// honest. `mode="global"` names exactly one account and still checks the grant:
// being assigned to an agent is not permission to use it (plan §5.2).
//
// Refusals are typed, and each one carries a code to the wire, so the console
// can offer the ONE action that helps: connect an account, ask an
// administrator, sign in again, or install the runtime on a node. The mapping is
// not one to one — `AccountDisabled` is an access decision (relogging in cannot
// undo it) and so reads as a grant denial, while a node that cannot hold the
// account's current credential reads as a runtime problem. The plan names four
// codes for what can be said about an ACCOUNT, and those four are the whole
// vocabulary of `resolve_run_account`; the two further names `code()` knows are
// about what a run may do with an account it already has, and offering one of
// the four there would send the operator to fix something that is not broken.
//
// The selected node is where the bridge will run. Choosing it here rather than
// in the caller is what lets a node with no CLI installed be reported as such
// before a process is started, and what makes `home_node_id` (the account's one
// refresher) a preference rather than a rule: a satellite node that holds the
// credential runs the turn, and the home node refreshes it.

use std::fmt;

use crate::agents::runtime::{AccountBinding, AgentRuntime};
use crate::agents::AgentPrincipal;
use crate::db::models::DbAgent;
use crate::db::DbPool;
use crate::provider_accounts::repository as store;
use crate::provider_accounts::AccountRecord;

/// Which of the two credential sources an account runs on. The delegation
/// adapter keys its behaviour off this and nothing else (§H.2): an API key goes
/// through the ticketed adapter path without ever entering the sandbox, a
/// provider login is the material the bridge holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    ApiKey,
    ProviderLogin,
}

impl CredentialKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "api_key" => Some(Self::ApiKey),
            "provider_login" => Some(Self::ProviderLogin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::ProviderLogin => "provider_login",
        }
    }
}

/// The account a run will use, and where it will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccount {
    pub account_id: String,
    pub engine_id: String,
    pub credential_kind: CredentialKind,
    /// The node whose bridge serves the turn. Its engine is installed there and
    /// it may hold account credentials; the caller materializes the credential
    /// onto it before the first session.
    pub node_id: String,
    /// The credential revision this run must be running. A node that applies a
    /// different one is running a token the fleet has since replaced.
    pub revision: i64,
}

/// Why no account could be handed to the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountRefusal {
    /// No node may hold account credentials, or none has this engine installed.
    NoRuntimeNode { engine_id: String },
    /// The caller asked for a specific node and that node cannot run the engine.
    EngineNotInstalled { engine_id: String, node_id: String },
    /// The running user has no account of their own for this engine (C01).
    NoAccountForUser { engine_id: String },
    /// The account exists but this principal may not use it.
    NotGranted { account_id: String },
    /// The account has no usable credential — never signed in, or signed in
    /// with a token that has since been revoked or rejected.
    CredentialMissing { account_id: String },
    /// An administrator took the account out of service.
    AccountDisabled { account_id: String },
    /// Every node that could run the account is stuck on an older revision, so
    /// the current credential cannot be brought to any of them.
    StaleCredential { account_id: String, node_id: String },
    /// The account is at the number of sessions an operator allowed it (§2.5,
    /// A03). The count is this node's, because that is the state the admission
    /// decision can read: `provider_account_sessions` never replicates.
    SessionLimitReached {
        account_id: String,
        /// `0` never reaches here — an account without a limit is admitted.
        limit: i64,
        open: i64,
    },
    /// The run would continue a provider conversation that another account
    /// opened (§2.5). A vendor thread belongs to the account that created it, so
    /// the run is refused rather than migrated onto a different subscription.
    ConversationUnderAnotherAccount {
        account_id: String,
        recorded_account_id: String,
    },
    /// No decision about an account was reached: the registry could not be
    /// read, or the agent does not run a CLI at all. The plan's four names have
    /// no term for this because it is not a decision about an account, and
    /// `code()` reports the one of the four that promises nothing rather than
    /// inventing a fifth value a client would not recognise.
    Undecided { detail: String },
}

impl AccountRefusal {
    /// The code this refusal carries to a client.
    ///
    /// The plan's four names (`user_account_missing`, `account_grant_denied`,
    /// `account_relogin_required`, `node_runtime_unavailable`) are what the
    /// resolver says about an ACCOUNT, and they are still the whole vocabulary
    /// of `resolve_run_account`. Two further names cover conditions that are not
    /// about the account being available — a conversation another account
    /// opened, and a session limit an operator set — because folding them into
    /// one of the four would offer the client an action that cannot help
    /// (connecting, signing in again, installing a runtime).
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoAccountForUser { .. } => "user_account_missing",
            Self::NotGranted { .. } | Self::AccountDisabled { .. } => "account_grant_denied",
            Self::CredentialMissing { .. } => "account_relogin_required",
            Self::NoRuntimeNode { .. }
            | Self::EngineNotInstalled { .. }
            | Self::StaleCredential { .. }
            | Self::Undecided { .. } => "node_runtime_unavailable",
            Self::SessionLimitReached { .. } => "account_session_limit_reached",
            Self::ConversationUnderAnotherAccount { .. } => "account_conversation_mismatch",
        }
    }

    /// The engine a "connect your account" action would have to connect, when
    /// the refusal knows one. This is the `{engine}` the plan puts in
    /// `user_account_missing`, and the only piece of the refusal a client needs
    /// besides the code.
    pub fn engine_id(&self) -> Option<&str> {
        match self {
            Self::NoRuntimeNode { engine_id }
            | Self::EngineNotInstalled { engine_id, .. }
            | Self::NoAccountForUser { engine_id } => Some(engine_id),
            _ => None,
        }
    }
}

impl fmt::Display for AccountRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuntimeNode { engine_id } => write!(
                f,
                "no node in this installation is set up to run '{engine_id}' agent accounts: \
                 install the runtime on a node and mark it as receiving accounts"
            ),
            Self::EngineNotInstalled { engine_id, node_id } => write!(
                f,
                "engine '{engine_id}' is not installed on node '{node_id}'"
            ),
            Self::NoAccountForUser { engine_id } => write!(
                f,
                "the user running this agent has no '{engine_id}' account of their own"
            ),
            Self::NotGranted { account_id } => write!(
                f,
                "agent account '{account_id}' is not granted to the user running this agent"
            ),
            Self::CredentialMissing { account_id } => write!(
                f,
                "agent account '{account_id}' has no usable credential: sign in again"
            ),
            Self::AccountDisabled { account_id } => {
                write!(f, "agent account '{account_id}' is disabled")
            }
            Self::StaleCredential { account_id, node_id } => write!(
                f,
                "agent account '{account_id}' cannot run on node '{node_id}': it holds a \
                 credential the fleet has replaced"
            ),
            Self::SessionLimitReached {
                account_id,
                limit,
                open,
            } => write!(
                f,
                "agent account '{account_id}' already has {open} of its {limit} allowed sessions \
                 open on this node: wait for one to finish, or raise the limit on the account"
            ),
            Self::ConversationUnderAnotherAccount {
                account_id,
                recorded_account_id,
            } => write!(
                f,
                "the conversation this run would continue was opened by agent account \
                 '{recorded_account_id}', and this run resolved to '{account_id}': a provider \
                 conversation belongs to the account that opened it, so start a new session for \
                 this agent, or point it back at the account that owns the conversation"
            ),
            Self::Undecided { detail } => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for AccountRefusal {}

/// Resolves the account a run of `agent` may use, for `principal`, near
/// `preferred_node`.
///
/// `preferred_node` is the node the caller would like to run on — the node of
/// the workspace session, say. It is honoured when that node can run the engine
/// and otherwise reported as `EngineNotInstalled` rather than silently replaced,
/// because a caller that named a node has a reason to; passing `None` asks for
/// the best available node.
pub fn resolve_run_account(
    db: &DbPool,
    agent: &DbAgent,
    principal: &AgentPrincipal,
    preferred_node: Option<&str>,
) -> Result<ResolvedAccount, AccountRefusal> {
    let runtime = AgentRuntime::parse(&agent.runtime_json).map_err(|error| {
        AccountRefusal::Undecided {
            detail: format!("agent '{}' has no usable runtime: {error}", agent.id),
        }
    })?;
    let AgentRuntime::Cli(cli) = runtime else {
        return Err(AccountRefusal::Undecided {
            detail: format!(
                "agent '{}' runs an LLM, so there is no provider account to resolve",
                agent.id
            ),
        });
    };
    let engine_id = cli.engine;

    let account = match &cli.account {
        AccountBinding::Global { account_id } => {
            global_account(db, principal, account_id, &engine_id)?
        }
        AccountBinding::User => user_account(db, principal, &engine_id)?,
    };

    // The account's own state, before any node is considered: an account nobody
    // may use is not a runtime problem, and reporting it as one would send the
    // operator to install software that would change nothing.
    if account.status == "disabled" {
        return Err(AccountRefusal::AccountDisabled {
            account_id: account.account_id,
        });
    }
    let Some(credential) = store::credential_summary(db, &account.account_id).map_err(registry)?
    else {
        return Err(AccountRefusal::CredentialMissing {
            account_id: account.account_id,
        });
    };
    // A revoked revision is spent for good: the credential row may still be
    // there, but the fleet has been told to drop it, and running on it would use
    // a token the provider has already been asked to forget.
    if account.status != "active" || credential.revision <= account.credential_revoked_revision {
        return Err(AccountRefusal::CredentialMissing {
            account_id: account.account_id,
        });
    }

    let credential_kind = CredentialKind::parse(&account.credential_kind).ok_or_else(|| {
        AccountRefusal::Undecided {
            detail: format!(
                "agent account '{}' carries an unknown credential kind",
                account.account_id
            ),
        }
    })?;

    // A03's optional limit (§2.5), off unless an operator set one: `0` is "no
    // limit", which is what every account said before the column existed. What
    // is counted is the sessions THIS node has open on the account — the rows
    // `CliBridge::open` writes and `close` deletes — because that is the state a
    // per-node admission decision can see; the table never replicates.
    if account.max_sessions > 0 {
        let open = store::session_counts(db, std::slice::from_ref(&account.account_id))
            .map_err(registry)?
            .get(&account.account_id)
            .copied()
            .unwrap_or(0);
        if i64::from(open) >= account.max_sessions {
            return Err(AccountRefusal::SessionLimitReached {
                account_id: account.account_id,
                limit: account.max_sessions,
                open: i64::from(open),
            });
        }
    }

    let node_id = select_node(
        db,
        &account,
        &engine_id,
        credential.revision,
        preferred_node,
    )?;

    Ok(ResolvedAccount {
        account_id: account.account_id,
        engine_id,
        credential_kind,
        node_id,
        revision: credential.revision,
    })
}

/// What the console needs to SAY about the account an agent names — for the
/// chip on a run (C02) and for the question a refused run raises (C01).
///
/// Deliberately not a `ResolvedAccount`: this is read without deciding whether
/// the run may use the account, so it exists even when the resolution FAILS —
/// which is exactly the case that has to be described to the person who can fix
/// it. No node, no credential and no grant are consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLabel {
    /// The account the binding points at, when one can be named. `None` for
    /// `mode="user"` with no account of that user's own for the engine — the
    /// state C01 asks the person to leave.
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub engine_id: String,
    /// The provider's product name; the id itself when the catalog has no entry
    /// for it, because a raw id is a thin label and a wrong name is a lie.
    pub engine_name: String,
    /// `global` | `user`, as `agents.runtime_json` wrote it.
    pub mode: String,
}

/// Describes the account binding of `agent` for `principal`, without judging it.
///
/// `None` when the agent runs no CLI at all (there is no account behind an LLM
/// agent, and a chip naming one would be a fabrication) or when the runtime
/// cannot be read. Errors are not propagated on purpose: every caller is either
/// decorating a list row or about to raise a question, and a run that cannot be
/// DESCRIBED is still a run whose refusal the delegation node reports itself.
pub fn describe_agent_account(
    db: &DbPool,
    agent: &DbAgent,
    principal: &AgentPrincipal,
) -> Option<AccountLabel> {
    let AgentRuntime::Cli(cli) = AgentRuntime::parse(&agent.runtime_json).ok()? else {
        return None;
    };
    let engine_id = cli.engine.clone();
    let mode = match &cli.account {
        AccountBinding::Global { .. } => "global",
        AccountBinding::User => "user",
    };
    // The account is looked up by the same rule the resolver uses, so the label
    // and the run agree on WHICH account is meant; only the checks that turn a
    // missing or unusable account into a refusal are skipped here.
    let account = match &cli.account {
        AccountBinding::Global { account_id } => store::get_account(db, account_id).ok().flatten(),
        AccountBinding::User => user_account(db, principal, &engine_id).ok(),
    };
    Some(AccountLabel {
        account_id: account.as_ref().map(|a| a.account_id.clone()),
        account_name: account.map(|a| a.display_name),
        engine_name: crate::provider_accounts::engine(&engine_id)
            .map(|e| e.display_name.to_string())
            .unwrap_or_else(|| engine_id.clone()),
        engine_id,
        mode: mode.to_string(),
    })
}

/// The accounts of `engine_id` this principal could use instead: their own for
/// the engine plus every global one granted to them.
///
/// This is the "instead" list of a refused run — the mockup says in so many
/// words that a global account will not be used in place of a personal one, so
/// the question that offers a sign-in must be able to say which accounts exist.
/// Names only: an id here would reach a card the person cannot act on.
pub fn candidate_accounts(
    db: &DbPool,
    principal: &AgentPrincipal,
    engine_id: &str,
) -> Vec<String> {
    let (Some(user_id), Some(org_id)) = (principal.user_id.as_deref(), principal.org_id.as_deref())
    else {
        return Vec::new();
    };
    store::list_accounts_for_user(db, org_id, user_id, Some(engine_id))
        .map(|accounts| {
            accounts
                .into_iter()
                .map(|account| account.display_name)
                .collect()
        })
        .unwrap_or_default()
}

/// The account named by `mode="global"`: it must exist, run the SAME engine the
/// agent claims, be a global account, and be granted to the principal.
///
/// Each of those is a refusal with the same code — to a user they are all "you
/// may not use this account", and the reason belongs in the log, not in a
/// message the console would have to translate five ways.
fn global_account(
    db: &DbPool,
    principal: &AgentPrincipal,
    account_id: &str,
    engine_id: &str,
) -> Result<AccountRecord, AccountRefusal> {
    let denied = || AccountRefusal::NotGranted {
        account_id: account_id.to_string(),
    };
    let account = store::get_account(db, account_id)
        .map_err(registry)?
        .ok_or_else(denied)?;
    if account.engine_id != engine_id || account.scope != "global" {
        return Err(denied());
    }
    // A grant is decided between a user and an organisation. An unattended run
    // has neither, and inventing one would turn "no identity" into an access
    // decision nobody made.
    let (Some(user_id), Some(org_id)) = (principal.user_id.as_deref(), principal.org_id.as_deref())
    else {
        return Err(denied());
    };
    if !store::user_may_use_account(db, org_id, &account.account_id, user_id).map_err(registry)? {
        return Err(AccountRefusal::NotGranted {
            account_id: account.account_id,
        });
    }
    Ok(account)
}

/// The account named by `mode="user"`: the principal's OWN, for this engine.
///
/// Never a granted global account, however many of those the user may use —
/// that is the fallback this mode exists to forbid.
fn user_account(
    db: &DbPool,
    principal: &AgentPrincipal,
    engine_id: &str,
) -> Result<AccountRecord, AccountRefusal> {
    let missing = || AccountRefusal::NoAccountForUser {
        engine_id: engine_id.to_string(),
    };
    let Some(user_id) = principal.user_id.as_deref() else {
        return Err(missing());
    };
    let mut accounts = store::personal_accounts_for_engine(
        db,
        principal.org_id.as_deref(),
        user_id,
        engine_id,
    )
    .map_err(registry)?;
    match accounts.len() {
        0 => Err(missing()),
        1 => Ok(accounts.remove(0)),
        // Several accounts for one engine is legal — a work subscription and a
        // personal one. Migration 156 has no `is_default` column, so the tie is
        // broken by the only fact that expresses an intent: the account this
        // user ran most recently. The others stay reachable by naming them,
        // which is what an agent bound `mode="global"` to one's own account
        // does not do — a personal account is not assignable, and the deliberate
        // way to pick one is to keep it as the last used.
        _ => {
            let last_used = store::last_used_by_user(db, user_id).map_err(registry)?;
            accounts.sort_by(|a, b| {
                let a_used = last_used.get(&a.account_id);
                let b_used = last_used.get(&b.account_id);
                b_used
                    .cmp(&a_used)
                    .then_with(|| a.display_name.cmp(&b.display_name))
                    .then_with(|| a.account_id.cmp(&b.account_id))
            });
            Ok(accounts.remove(0))
        }
    }
}

/// The node whose bridge will serve the turn.
///
/// A candidate must both be allowed to hold credentials and have the engine
/// installed: the first is the operator's decision about the machine, the second
/// is whether the vendor CLI is on it. Among the candidates, one already holding
/// the CURRENT revision wins, so the common case costs no materialization.
fn select_node(
    db: &DbPool,
    account: &AccountRecord,
    engine_id: &str,
    revision: i64,
    preferred_node: Option<&str>,
) -> Result<String, AccountRefusal> {
    let nodes = store::list_runtime_nodes(db).map_err(registry)?;
    let capable: Vec<&str> = nodes
        .iter()
        .filter(|node| node.receives_accounts)
        .filter(|node| {
            node.engines
                .iter()
                .any(|engine| engine.engine_id == engine_id && engine.install_state == "installed")
        })
        .map(|node| node.node_id.as_str())
        .collect();
    if capable.is_empty() {
        return Err(AccountRefusal::NoRuntimeNode {
            engine_id: engine_id.to_string(),
        });
    }
    if let Some(preferred) = preferred_node {
        if !capable.contains(&preferred) {
            return Err(AccountRefusal::EngineNotInstalled {
                engine_id: engine_id.to_string(),
                node_id: preferred.to_string(),
            });
        }
    }

    let local = local_node_id(db)?;
    let mut ordered: Vec<String> = capable.iter().map(|id| id.to_string()).collect();
    ordered.sort_by_key(|id| {
        // Preference is an order, not a filter: a node that is not the home, not
        // the caller's choice and not this one is still a place the turn can run.
        let rank = if Some(id.as_str()) == preferred_node {
            0
        } else if Some(id.as_str()) == account.home_node_id.as_deref() {
            1
        } else if Some(id.as_str()) == local.as_deref() {
            2
        } else {
            3
        };
        (rank, id.clone())
    });

    // A node that already holds the revision the run needs wins outright: costs
    // no materialization, so the preference order only breaks ties below.
    let mut stuck: Option<String> = None;
    let mut fallback: Option<String> = None;
    for node_id in &ordered {
        match store::node_state(db, &account.account_id, node_id).map_err(registry)? {
            Some(state) if state.applied_revision == revision && state.runtime_state == "ready" => {
                return Ok(node_id.clone())
            }
            // A node whose last materialization failed is skipped: the credential
            // cannot be brought there until somebody fixes it, and another node
            // that holds it can serve the turn now.
            Some(state) if state.runtime_state == "error" => {
                stuck.get_or_insert_with(|| node_id.clone());
            }
            // Never materialized, or behind: usable, the caller materializes.
            _ => {
                fallback.get_or_insert_with(|| node_id.clone());
            }
        }
    }
    if let Some(node_id) = fallback {
        return Ok(node_id);
    }
    Err(AccountRefusal::StaleCredential {
        account_id: account.account_id.clone(),
        node_id: stuck.unwrap_or_else(|| ordered.remove(0)),
    })
}

/// This installation's own node id, read from the data rather than from the sync
/// runtime's process global: the resolver is also reached from paths that run
/// before the mesh is up, and a rule written against a global is a rule that
/// quietly does nothing in every test.
fn local_node_id(db: &DbPool) -> Result<Option<String>, AccountRefusal> {
    crate::db::repository::get_setting(db, crate::db::repository::LOCAL_NODE_ID_SETTING)
        .map_err(registry)
}

fn registry(error: impl fmt::Display) -> AccountRefusal {
    AccountRefusal::Undecided {
        detail: format!("the provider account registry could not be read: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_accounts::{CredentialMeta, GrantInput, NewAccount};
    use crate::crypto::SettingsCipher;

    const ORG: &str = "org-default";
    const USER: &str = "alice";
    const OTHER_USER: &str = "bob";

    fn db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).unwrap()
    }

    fn cipher() -> SettingsCipher {
        SettingsCipher::new(&[11u8; 32])
    }

    fn account(db: &DbPool, id: &str, scope: &str, engine: &str, owner: Option<&str>) {
        store::create_account(
            db,
            &NewAccount {
                account_id: id.to_string(),
                org_id: ORG.to_string(),
                engine_id: engine.to_string(),
                display_name: id.to_string(),
                scope: scope.to_string(),
                owner_user_id: owner.map(str::to_string),
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .unwrap();
    }

    /// An enrolled user. Membership is not bookkeeping: a grant naming somebody
    /// outside the account's organisation is refused at write time, and the
    /// effective-grant rule demands membership of the account's org.
    fn member(db: &DbPool, user_id: &str) {
        let conn = db.write().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, is_active, is_admin, role) \
             VALUES (?1, ?1, 'x', ?1, 1, 0, 'user')",
            rusqlite::params![user_id],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-org-viewer', datetime('now'), 'test')",
            rusqlite::params![ORG, user_id],
        )
        .unwrap();
    }

    /// A signed-in account: the credential write is what moves it to `active`,
    /// exactly as a real login does.
    fn sign_in(db: &DbPool, id: &str) {
        store::mint_credential(
            db,
            &cipher(),
            id,
            &format!("material-{id}"),
            &CredentialMeta {
                provider_subject: Some(format!("subject-{id}")),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// A node the operator enrolled in the account fleet, with the engine
    /// installed — the two rows the materializer writes on a receiving node.
    fn node(db: &DbPool, node_id: &str, engine: &str) {
        let conn = db.write().unwrap();
        conn.execute(
            "INSERT INTO agent_runtime_nodes (node_id, receives_accounts) VALUES (?1, 1)",
            rusqlite::params![node_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_runtime_engines (node_id, engine_id, install_state, version) \
             VALUES (?1, ?2, 'installed', '1.0.0')",
            rusqlite::params![node_id, engine],
        )
        .unwrap();
    }

    fn cli_agent(engine: &str, binding: &str) -> DbAgent {
        DbAgent {
            id: "agent-1".to_string(),
            name: "agent-1".to_string(),
            display_name: None,
            description: String::new(),
            system_prompt: None,
            model: None,
            tools_json: "[]".to_string(),
            skills_json: "[]".to_string(),
            params_json: "{}".to_string(),
            max_iterations: 1,
            timeout_secs: 60,
            max_subagents: 0,
            max_spawn_depth: 0,
            flow_id: None,
            routable: false,
            is_enabled: true,
            on_child_complete: "notify".to_string(),
            allowed_agents_json: None,
            runtime_json: format!(r#"{{"kind":"cli","engine":"{engine}","account":{binding}}}"#),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn principal(user: Option<&str>) -> AgentPrincipal {
        AgentPrincipal::new(
            user.map(str::to_string),
            Some(ORG.to_string()),
            crate::flow_engine::dispatcher::FlowOrigin::CodeStudio,
            crate::flow_engine::dispatcher::FlowActor::user(user.unwrap_or("system")),
        )
    }

    #[test]
    fn a_global_binding_resolves_the_named_account_on_a_node_that_can_run_it() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-global", "global", "claude-code", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: USER.to_string(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "claude-code");

        let resolved = resolve_run_account(
            &db,
            &cli_agent("claude-code", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
        assert_eq!(resolved.account_id, "acc-global");
        assert_eq!(resolved.engine_id, "claude-code");
        assert_eq!(resolved.credential_kind, CredentialKind::ProviderLogin);
        assert_eq!(resolved.node_id, "node-a");
        assert_eq!(resolved.revision, 1);
    }

    /// The negative case the whole mode distinction exists for. The user has a
    /// global account granted to them and no account of their own; a `mode=user`
    /// agent must be refused, and the refusal must not be satisfiable by that
    /// global account.
    #[test]
    fn a_user_binding_is_refused_rather_than_served_by_a_granted_global_account() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-global", "global", "codex", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: USER.to_string(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "codex");

        // The same agent, bound global, proves the account and the node are
        // usable — so the refusal below is about the MODE and nothing else.
        resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();

        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"user"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::NoAccountForUser {
                engine_id: "codex".to_string()
            }
        );
        assert_eq!(refusal.code(), "user_account_missing");
        assert_eq!(refusal.engine_id(), Some("codex"));
    }

    #[test]
    fn a_user_binding_takes_the_users_own_account_for_that_engine_only() {
        let db = db();
        account(&db, "acc-mine", "user", "claude-code", Some(USER));
        account(&db, "acc-other-engine", "user", "codex", Some(USER));
        sign_in(&db, "acc-mine");
        sign_in(&db, "acc-other-engine");
        node(&db, "node-a", "claude-code");
        node(&db, "node-b", "codex");

        let resolved = resolve_run_account(
            &db,
            &cli_agent("claude-code", r#"{"mode":"user"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
        assert_eq!(resolved.account_id, "acc-mine");
        assert_eq!(resolved.node_id, "node-a");

        // Another user's account of the same engine is not a candidate.
        account(&db, "acc-bob", "user", "claude-code", Some(OTHER_USER));
        sign_in(&db, "acc-bob");
        let resolved = resolve_run_account(
            &db,
            &cli_agent("claude-code", r#"{"mode":"user"}"#),
            &principal(Some(OTHER_USER)),
            None,
        )
        .unwrap();
        assert_eq!(resolved.account_id, "acc-bob");
    }

    #[test]
    fn every_refusal_has_its_reason_and_its_wire_code() {
        // An account nobody granted to the user.
        let pool = db();
        member(&pool, USER);
        account(&pool, "acc-global", "global", "codex", None);
        sign_in(&pool, "acc-global");
        node(&pool, "node-a", "codex");
        let refusal = resolve_run_account(
            &pool,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::NotGranted {
                account_id: "acc-global".to_string()
            }
        );
        assert_eq!(refusal.code(), "account_grant_denied");

        // An incorrect engine on the binding is the same decision: the account
        // cannot be what the agent says it runs.
        account(&pool, "acc-codex", "global", "codex", None);
        sign_in(&pool, "acc-codex");
        node(&pool, "node-c", "claude-code");
        let refusal = resolve_run_account(
            &pool,
            &cli_agent("claude-code", r#"{"mode":"global","account_id":"acc-codex"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "account_grant_denied");

        // An account that was never signed in.
        account(&pool, "acc-bare", "global", "claude-code", None);
        store::set_grants(
            &pool,
            "acc-bare",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: USER.to_string(),
            }],
            "admin",
        )
        .unwrap();
        let refusal = resolve_run_account(
            &pool,
            &cli_agent("claude-code", r#"{"mode":"global","account_id":"acc-bare"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::CredentialMissing {
                account_id: "acc-bare".to_string()
            }
        );
        assert_eq!(refusal.code(), "account_relogin_required");

        // A disabled account is an administrator's decision, not a lost login:
        // signing in again cannot undo it.
        account(&pool, "acc-off", "global", "codex", None);
        sign_in(&pool, "acc-off");
        store::update_account(
            &pool,
            "acc-off",
            &crate::provider_accounts::AccountUpdate {
                status: Some("disabled".to_string()),
                ..Default::default()
            },
            Some("admin"),
        )
        .unwrap();
        store::set_grants(
            &pool,
            "acc-off",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        let refusal = resolve_run_account(
            &pool,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-off"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::AccountDisabled {
                account_id: "acc-off".to_string()
            }
        );
        assert_eq!(
            refusal.code(),
            "account_grant_denied",
            "a disabled account is an access decision: a new login would not lift it"
        );

        // No node receives accounts at all.
        let no_nodes = db();
        member(&no_nodes, USER);
        account(&no_nodes, "acc-global", "global", "codex", None);
        sign_in(&no_nodes, "acc-global");
        store::set_grants(
            &no_nodes,
            "acc-global",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        let refusal = resolve_run_account(
            &no_nodes,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::NoRuntimeNode {
                engine_id: "codex".to_string()
            }
        );
        assert_eq!(refusal.code(), "node_runtime_unavailable");
    }

    #[test]
    fn a_named_node_that_cannot_run_the_engine_is_reported_as_such() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-global", "global", "codex", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "codex");

        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            Some("node-b"),
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::EngineNotInstalled {
                engine_id: "codex".to_string(),
                node_id: "node-b".to_string()
            }
        );
        assert_eq!(refusal.code(), "node_runtime_unavailable");

        // The named node wins when it CAN run the engine, even against the
        // account's home.
        node(&db, "node-b", "codex");
        store::update_account(
            &db,
            "acc-global",
            &crate::provider_accounts::AccountUpdate {
                home_node_id: Some("node-a".to_string()),
                ..Default::default()
            },
            Some("admin"),
        )
        .unwrap();
        let resolved = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            Some("node-b"),
        )
        .unwrap();
        assert_eq!(resolved.node_id, "node-b");
    }

    /// A node already holding the current revision is preferred over the
    /// account's home, which would have to be given it; and when the only node
    /// that can run the engine has a failed materialization, the run is refused
    /// rather than silently sent to a node that cannot serve it.
    #[test]
    fn a_node_holding_the_current_revision_is_preferred_over_one_that_is_behind() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-global", "global", "codex", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "codex");
        node(&db, "node-b", "codex");
        store::update_account(
            &db,
            "acc-global",
            &crate::provider_accounts::AccountUpdate {
                home_node_id: Some("node-a".to_string()),
                ..Default::default()
            },
            Some("admin"),
        )
        .unwrap();
        store::set_node_state(&db, "acc-global", "node-b", 1, "ready", None).unwrap();

        let resolved = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
        assert_eq!(
            resolved.node_id, "node-b",
            "the node that already holds revision 1 runs the turn"
        );

        // Now the only node that can run the engine is the one whose last
        // materialization failed — there is nothing left to fall back to.
        db.write()
            .unwrap()
            .execute(
                "UPDATE agent_runtime_engines SET install_state = 'absent' WHERE node_id = 'node-a'",
                [],
            )
            .unwrap();
        store::set_node_state(&db, "acc-global", "node-b", 0, "error", Some("boom")).unwrap();
        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::StaleCredential {
                account_id: "acc-global".to_string(),
                node_id: "node-b".to_string()
            }
        );
        assert_eq!(refusal.code(), "node_runtime_unavailable");
    }

    /// A revoked credential is not usable even though its row is still there:
    /// the fleet was told to drop that revision.
    #[test]
    fn a_credential_covered_by_the_revocation_mark_is_not_usable() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-global", "global", "codex", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "codex");
        store::clear_credential(&db, "acc-global", Some("admin")).unwrap();

        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::CredentialMissing {
                account_id: "acc-global".to_string()
            }
        );
        assert_eq!(refusal.code(), "account_relogin_required");
    }

    #[test]
    fn an_unattended_run_cannot_claim_a_global_grant() {
        let db = db();
        account(&db, "acc-global", "global", "codex", None);
        sign_in(&db, "acc-global");
        store::set_grants(
            &db,
            "acc-global",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            "admin",
        )
        .unwrap();
        node(&db, "node-a", "codex");

        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-global"}"#),
            &crate::agents::AgentPrincipal::new(
                None,
                None,
                crate::flow_engine::dispatcher::FlowOrigin::System,
                crate::flow_engine::dispatcher::FlowActor::system(),
            ),
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "account_grant_denied");

        // The same for a personal account: there is no user to own it.
        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"user"}"#),
            &crate::agents::AgentPrincipal::new(
                None,
                None,
                crate::flow_engine::dispatcher::FlowOrigin::System,
                crate::flow_engine::dispatcher::FlowActor::system(),
            ),
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "user_account_missing");
    }

    #[test]
    fn an_agent_that_runs_no_cli_has_no_account_to_resolve() {
        let db = db();
        let mut agent = cli_agent("codex", r#"{"mode":"user"}"#);
        agent.runtime_json = crate::agents::runtime::LLM_RUNTIME_JSON.to_string();
        let refusal =
            resolve_run_account(&db, &agent, &principal(Some(USER)), None).unwrap_err();
        assert_eq!(refusal.code(), "node_runtime_unavailable");

        agent.runtime_json = "{ not json".to_string();
        let refusal =
            resolve_run_account(&db, &agent, &principal(Some(USER)), None).unwrap_err();
        assert!(matches!(refusal, AccountRefusal::Undecided { .. }));
    }

    /// Several accounts for one engine are legal, and the tie is broken by the
    /// one the user ran most recently — not by an invented `is_default`.
    #[test]
    fn the_most_recently_used_personal_account_is_the_one_a_user_binding_takes() {
        let db = db();
        account(&db, "acc-a", "user", "codex", Some(USER));
        account(&db, "acc-b", "user", "codex", Some(USER));
        sign_in(&db, "acc-a");
        sign_in(&db, "acc-b");
        node(&db, "node-a", "codex");
        store::upsert_session(
            &db,
            &crate::provider_accounts::SessionRecord {
                account_id: "acc-b".to_string(),
                session_id: "sess-1".to_string(),
                user_id: USER.to_string(),
                agent_id: None,
                workspace_id: None,
                node_id: "node-a".to_string(),
                vendor_session_id: None,
                started_at: "2026-09-18T00:00:00Z".to_string(),
                last_used_at: Some("2026-09-18T12:00:00Z".to_string()),
            },
        )
        .unwrap();

        let resolved = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"user"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
        assert_eq!(resolved.account_id, "acc-b");
    }

    /// One open session on the account, as `CliBridge::open` leaves it.
    fn open_session(db: &DbPool, account_id: &str, session_id: &str) {
        store::upsert_session(
            db,
            &crate::provider_accounts::SessionRecord {
                account_id: account_id.to_string(),
                session_id: session_id.to_string(),
                user_id: USER.to_string(),
                agent_id: None,
                workspace_id: None,
                node_id: "node-a".to_string(),
                vendor_session_id: None,
                started_at: "2026-09-18T00:00:00Z".to_string(),
                last_used_at: None,
            },
        )
        .unwrap();
    }

    fn set_limit(db: &DbPool, account_id: &str, max_sessions: i64) {
        store::update_account(
            db,
            account_id,
            &crate::provider_accounts::AccountUpdate {
                max_sessions: Some(max_sessions),
                ..Default::default()
            },
            Some("admin"),
        )
        .unwrap();
    }

    /// A03's optional limit (§2.5).
    ///
    /// The two accounts here differ in ONE thing — whether somebody set a
    /// limit — and carry the same open sessions, so what the test can tell
    /// apart is the column and not the session table: the limited account is
    /// refused with its own code, and the account nobody limited resolves
    /// exactly as it did before the column existed. Setting the limit back to 0
    /// is what removes it, and the value that leaves it alone is `None` — two
    /// different things, which is why the update carries `Option<i64>` and not
    /// "0 means do not touch".
    #[test]
    fn a_session_limit_refuses_the_next_run_and_an_account_without_one_is_unchanged() {
        let db = db();
        member(&db, USER);
        account(&db, "acc-limited", "global", "codex", None);
        sign_in(&db, "acc-limited");
        account(&db, "acc-open", "global", "claude-code", None);
        sign_in(&db, "acc-open");
        node(&db, "node-a", "codex");
        node(&db, "node-o", "claude-code");
        for id in ["acc-limited", "acc-open"] {
            store::set_grants(
                &db,
                id,
                &[GrantInput {
                    subject_type: "org".to_string(),
                    subject_id: String::new(),
                }],
                "admin",
            )
            .unwrap();
        }
        open_session(&db, "acc-limited", "sess-1");
        open_session(&db, "acc-open", "sess-1");

        // Nobody set a limit on `acc-open`, so one open session means nothing
        // to it — the behaviour of every account before this column existed.
        let open = resolve_run_account(
            &db,
            &cli_agent(
                "claude-code",
                r#"{"mode":"global","account_id":"acc-open"}"#,
            ),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
        assert_eq!(open.account_id, "acc-open");

        set_limit(&db, "acc-limited", 1);
        let refusal = resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-limited"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            AccountRefusal::SessionLimitReached {
                account_id: "acc-limited".to_string(),
                limit: 1,
                open: 1,
            }
        );
        assert_eq!(refusal.code(), "account_session_limit_reached");
        assert!(
            refusal.to_string().contains("acc-limited"),
            "the refusal has to name the account it is about: {refusal}"
        );

        // The refusal is about the ACCOUNT, not about this node's slot: the
        // node is still there and still runs the engine, so a caller that
        // retries after a session ends is admitted again.
        open_session(&db, "acc-limited", "sess-2");
        set_limit(&db, "acc-limited", 3);
        resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-limited"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();

        // 0 removes the limit rather than meaning "zero sessions allowed".
        set_limit(&db, "acc-limited", 0);
        resolve_run_account(
            &db,
            &cli_agent("codex", r#"{"mode":"global","account_id":"acc-limited"}"#),
            &principal(Some(USER)),
            None,
        )
        .unwrap();
    }
}
