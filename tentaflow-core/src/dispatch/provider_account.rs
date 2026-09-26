// =============================================================================
// File: dispatch/provider_account.rs — the agent provider-account family
//       (docs/agent-accounts-technical-design.md §B.1, WP2).
//
//       Two audiences share one family, and the difference between them is the
//       whole access model:
//
//         * an ADMINISTRATOR manages the organisation's accounts (A01/A03/A04)
//           and the node matrix (N01). Managing an account is not using it: no
//           handler here ever returns credential material, in any shape, to
//           anybody;
//         * a USER manages their OWN accounts (U01) and may see the global
//           accounts they were granted. An account they were neither given nor
//           own does not exist as far as this family is concerned — the refusal
//           is the same `NotFound` a missing id gets, so the wire never reveals
//           that somebody else's account exists.
//
//       Every human-readable name is resolved HERE, from the same tables
//       `analytics.js` reads (`user_accounts`, `user_groups`, `sync_nodes`), so
//       the dashboard never renders a bare UUID as a title.
//
//       The login exchange, credential replication and CLI installation are
//       later packages. Their variants exist on the wire already (the family is
//       append-only, so they cannot be added later without a rename risk) and
//       are refused in ONE place with `NotAvailable` — never with an empty
//       result that would read as success.
// =============================================================================

use std::collections::{HashMap, HashSet};

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::provider_account::{
    AccountAgentInfo, AccountNodeInfo, AccountSessionInfo, AccountUsageNode, EngineSummary,
    GrantEntry, MyAccountInfo, ProviderAccountInfo, ProviderAccountPayload as P, RuntimeEngineInfo,
    RuntimeNodeInfo,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::agents::{AccountBinding, AgentRuntime};
use crate::db::repository;
use crate::provider_accounts::{
    self as accounts, login, repository as store, AccountFilter, AccountRecord, AccountUpdate,
    CredentialMeta, CredentialWrite, GrantInput, NewAccount,
};

fn pa(body: P) -> MessageBody {
    MessageBody::ProviderAccountBody(body)
}

fn internal(scope: &str, error: impl std::fmt::Display) -> ProtocolError {
    tracing::warn!(scope, error = %error, "provider account error");
    ProtocolError::internal(format!("provider account {scope} failed"))
}

/// The one refusal a caller who may not see an account ever gets. Identical for
/// "no such account", "another organisation's account" and "not yours", so the
/// existence of an account is never readable from the error code.
fn not_found() -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::NotFound, "provider account not found")
}

/// Who is asking, and whether they administer the organisation.
///
/// `is_admin` is the same session check the Services screen uses
/// (`handlers::session_is_admin`) — one definition of "administrator" for the
/// whole dashboard, not a second one that could drift from it.
struct Caller {
    user_id: String,
    org_id: String,
    is_admin: bool,
}

fn caller(ctx: &HandlerContext) -> Result<Caller, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "provider accounts require a signed-in user",
        )
    })?;
    Ok(Caller {
        user_id: org.user_id.clone(),
        org_id: org.org_id.clone(),
        is_admin: super::handlers::session_is_admin(ctx),
    })
}

impl Caller {
    fn require_admin(&self, what: &str) -> Result<(), ProtocolError> {
        if self.is_admin {
            return Ok(());
        }
        Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{what} requires an administrator"),
        ))
    }

    /// Administering ONE account: an administrator, or the person whose account
    /// it is. A grant is not enough — being allowed to use a subscription is
    /// not being allowed to rename or delete it.
    fn require_owner_or_admin(&self, account: &AccountRecord) -> Result<(), ProtocolError> {
        if self.is_admin || account.is_owned_by(&self.user_id) {
            return Ok(());
        }
        Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the owner or an administrator may change this account",
        ))
    }

    /// Holding the account's IDENTITY — its credential and the name it is
    /// listed under. On a PERSONAL account that is the owner and nobody else:
    /// plan §2.2 gives an administrator the power to take a personal account
    /// out of service (disable, delete), not the power to put a credential of
    /// their choosing behind somebody's name and let runs be attributed to it.
    /// A shared account has no owner, so it is the administrator's.
    fn require_account_keeper(
        &self,
        account: &AccountRecord,
        what: &str,
    ) -> Result<(), ProtocolError> {
        if account.scope == "user" {
            if account.is_owned_by(&self.user_id) {
                return Ok(());
            }
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                format!(
                    "{what} on a personal account is the owner's alone; an administrator may \
                     disable or delete it"
                ),
            ));
        }
        self.require_admin(what)
    }
}

/// Resolves an account the caller is allowed to KNOW ABOUT. An administrator
/// sees every account of their organisation, including personal ones (the
/// fleet is theirs to administer); everybody else sees the accounts they own
/// plus the global ones granted to them.
fn visible_account(
    ctx: &HandlerContext,
    caller: &Caller,
    account_id: &str,
) -> Result<AccountRecord, ProtocolError> {
    let account = store::get_account(&ctx.state.db, account_id)
        .map_err(|e| internal("account read", e))?
        .ok_or_else(not_found)?;
    if account.org_id != caller.org_id {
        return Err(not_found());
    }
    if caller.is_admin || account.is_owned_by(&caller.user_id) {
        return Ok(account);
    }
    let granted =
        store::user_may_use_account(&ctx.state.db, &caller.org_id, account_id, &caller.user_id)
            .map_err(|e| internal("grant check", e))?;
    if granted {
        Ok(account)
    } else {
        Err(not_found())
    }
}

fn engine_catalog() -> Vec<EngineSummary> {
    accounts::AGENT_ENGINES
        .iter()
        .map(|engine| EngineSummary {
            engine_id: engine.engine_id.to_string(),
            display_name: engine.display_name.to_string(),
            supports_login: engine.supports_login,
            supports_api_key: engine.supports_api_key,
        })
        .collect()
}

// =============================================================================
// Name and count resolution
// =============================================================================

/// Which agents run on which account, read once for the whole answer.
///
/// An agent binds either to ONE named account (`mode = "global"`) or to "the
/// account of whoever runs me" (`mode = "user"`), and the second form names no
/// account at all — it applies to every personal account of that engine. Both
/// are counted, because both make the account load-bearing: deleting it breaks
/// the agent either way.
#[derive(Default)]
struct AgentBindings {
    by_account: HashMap<String, Vec<(String, String)>>,
    by_engine: HashMap<String, Vec<(String, String)>>,
}

impl AgentBindings {
    fn of(&self, account: &AccountRecord) -> Vec<AccountAgentInfo> {
        let mut out: Vec<AccountAgentInfo> = self
            .by_account
            .get(&account.account_id)
            .into_iter()
            .flatten()
            .map(|(agent_id, agent_name)| AccountAgentInfo {
                agent_id: agent_id.clone(),
                agent_name: agent_name.clone(),
                bind_mode: "global".to_string(),
            })
            .collect();
        if account.scope == "user" {
            out.extend(
                self.by_engine
                    .get(&account.engine_id)
                    .into_iter()
                    .flatten()
                    .map(|(agent_id, agent_name)| AccountAgentInfo {
                        agent_id: agent_id.clone(),
                        agent_name: agent_name.clone(),
                        bind_mode: "user".to_string(),
                    }),
            );
        }
        out
    }

    fn count(&self, account: &AccountRecord) -> u32 {
        u32::try_from(self.of(account).len()).unwrap_or(u32::MAX)
    }
}

/// A row whose `runtime_json` does not parse is skipped, not fatal: the account
/// list must not stop working because one agent holds a runtime this build does
/// not understand. The save path is what keeps such a row from being written.
fn agent_bindings(ctx: &HandlerContext) -> Result<AgentBindings, ProtocolError> {
    let agents =
        repository::list_cli_agents(&ctx.state.db).map_err(|e| internal("agent list", e))?;
    let mut bindings = AgentBindings::default();
    for agent in &agents {
        let Ok(AgentRuntime::Cli(cli)) = AgentRuntime::parse(&agent.runtime_json) else {
            continue;
        };
        let label = (
            agent.id.clone(),
            agent
                .display_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| agent.name.clone()),
        );
        match &cli.account {
            AccountBinding::Global { account_id } => bindings
                .by_account
                .entry(account_id.clone())
                .or_default()
                .push(label),
            AccountBinding::User => bindings
                .by_engine
                .entry(cli.engine.clone())
                .or_default()
                .push(label),
        }
    }
    Ok(bindings)
}

/// Everything a list of accounts needs beyond its own rows: owner names, home
/// node names, and the counts the cards show.
struct Decorations {
    owners: HashMap<String, String>,
    nodes: HashMap<String, Option<String>>,
    grants: HashMap<String, u32>,
    sessions: HashMap<String, u32>,
    session_users: HashMap<String, u32>,
    credentials: HashMap<String, (i64, Option<String>)>,
    used_on: HashMap<String, Vec<String>>,
    agents: AgentBindings,
}

impl Decorations {
    /// The nodes one account is materialized on, named. A node whose name this
    /// installation does not know is still listed — it holds the credential
    /// whether or not its row reached us — under its id.
    fn used_on(&self, account_id: &str) -> Vec<AccountUsageNode> {
        self.used_on
            .get(account_id)
            .into_iter()
            .flatten()
            .map(|node_id| AccountUsageNode {
                node_name: self
                    .nodes
                    .get(node_id)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| node_id.clone()),
                node_id: node_id.clone(),
            })
            .collect()
    }
}

fn decorate(
    ctx: &HandlerContext,
    accounts_list: &[AccountRecord],
) -> Result<Decorations, ProtocolError> {
    let owner_ids: Vec<String> =
        unique(accounts_list.iter().filter_map(|a| a.owner_user_id.clone()));
    // Every count below is asked ONLY about the rows being rendered: the detail
    // window decorates a single account through this same path, and an
    // installation-wide aggregate to fill in one card is work that grows with
    // the history of the whole fleet.
    let account_ids: Vec<String> = accounts_list
        .iter()
        .map(|a| a.account_id.clone())
        .collect::<Vec<_>>();
    let owners = repository::lookup_user_names(&ctx.state.db, &owner_ids)
        .map_err(|e| internal("owner names", e))?
        .into_iter()
        .map(|(id, row)| (id, super::model_metrics::user_presentation(&row).0))
        .collect();
    let used_on = store::materialized_nodes(&ctx.state.db, &account_ids)
        .map_err(|e| internal("materialized nodes", e))?;
    // One name lookup for both the home node and every node holding a copy: the
    // two overlap in the ordinary case, and asking twice would render the same
    // node under two names if one query saw a rename the other missed.
    let node_ids: Vec<String> = unique(
        accounts_list
            .iter()
            .filter_map(|a| a.home_node_id.clone())
            .chain(used_on.values().flatten().cloned()),
    );
    Ok(Decorations {
        owners,
        nodes: node_names(ctx, &node_ids)?,
        grants: store::grant_counts(&ctx.state.db, &account_ids)
            .map_err(|e| internal("grant counts", e))?,
        sessions: store::session_counts(&ctx.state.db, &account_ids)
            .map_err(|e| internal("session counts", e))?,
        session_users: store::session_user_counts(&ctx.state.db, &account_ids)
            .map_err(|e| internal("session people", e))?,
        credentials: store::credential_revisions(&ctx.state.db, &account_ids)
            .map_err(|e| internal("credential revisions", e))?,
        used_on,
        agents: agent_bindings(ctx)?,
    })
}

fn unique(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// `node_id -> display name`, `None` when the node is unknown to `sync_nodes`.
/// The caller decides what to render then; this layer does not invent a name
/// out of the id.
fn node_names(
    ctx: &HandlerContext,
    ids: &[String],
) -> Result<HashMap<String, Option<String>>, ProtocolError> {
    let resolved =
        super::model_metrics::resolve_nodes(ctx, ids).map_err(|e| internal("node names", e))?;
    Ok(resolved
        .into_iter()
        .map(|(id, node)| (id, node.display_name))
        .collect())
}

fn account_info(account: &AccountRecord, dec: &Decorations) -> ProviderAccountInfo {
    let (credential_revision, expires_at) = dec
        .credentials
        .get(&account.account_id)
        .cloned()
        .unwrap_or((0, None));
    ProviderAccountInfo {
        account_id: account.account_id.clone(),
        engine_id: account.engine_id.clone(),
        display_name: account.display_name.clone(),
        scope: account.scope.clone(),
        owner_display_name: account
            .owner_user_id
            .as_deref()
            .and_then(|id| dec.owners.get(id).cloned()),
        owner_user_id: account.owner_user_id.clone(),
        credential_kind: account.credential_kind.clone(),
        provider_subject: account.provider_subject.clone(),
        plan_label: account.plan_label.clone(),
        status: account.status.clone(),
        home_node_name: account
            .home_node_id
            .as_deref()
            .and_then(|id| dec.nodes.get(id).cloned().flatten()),
        home_node_id: account.home_node_id.clone(),
        // Sent raw, with no name beside it: the node named here is gone from the
        // registry, so `node_names` has nothing to resolve and a name field would
        // be NULL in every answer that matters. The client shortens the id.
        home_lost_node_id: account.home_lost_node_id.clone(),
        credential_revision,
        expires_at,
        grant_count: dec.grants.get(&account.account_id).copied().unwrap_or(0),
        session_count: dec.sessions.get(&account.account_id).copied().unwrap_or(0),
        agent_count: dec.agents.count(account),
        updated_at: account.updated_at.clone(),
        used_on: dec.used_on(&account.account_id),
        max_sessions: account.max_sessions,
        session_user_count: dec.session_users.get(&account.account_id).copied().unwrap_or(0),
    }
}

/// One account plus its decorations, for the handlers that answer with a single
/// account and would otherwise build the whole list machinery for one row.
fn single_account_info(
    ctx: &HandlerContext,
    account: &AccountRecord,
) -> Result<ProviderAccountInfo, ProtocolError> {
    let dec = decorate(ctx, std::slice::from_ref(account))?;
    Ok(account_info(account, &dec))
}

// =============================================================================
// Accounts
// =============================================================================

fn account_list(
    ctx: &HandlerContext,
    engine_id: &Option<String>,
    scope: &Option<String>,
    query: &Option<String>,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let filter = AccountFilter {
        engine_id: engine_id.clone(),
        scope: scope.clone(),
        query: query.clone(),
    };
    let rows = if caller.is_admin {
        store::list_accounts(&ctx.state.db, &caller.org_id, &filter)
            .map_err(|e| internal("account list", e))?
    } else {
        // A non-administrator asking the admin list gets exactly what they may
        // see, filtered by the same criteria — not a refusal, and not somebody
        // else's account.
        store::list_accounts_for_user(
            &ctx.state.db,
            &caller.org_id,
            &caller.user_id,
            filter.engine_id.as_deref(),
        )
        .map_err(|e| internal("account list", e))?
        .into_iter()
        .filter(|account| matches_filter(account, &filter))
        .collect()
    };
    let dec = decorate(ctx, &rows)?;
    Ok(pa(P::AccountListResponse {
        accounts: rows.iter().map(|a| account_info(a, &dec)).collect(),
        engines: engine_catalog(),
    }))
}

/// The scope/query half of `AccountFilter`, applied in Rust for the caller's
/// own list (whose SQL filters by reachability instead). Same rule as
/// `repository::list_accounts`, so both lists answer one question the same way.
fn matches_filter(account: &AccountRecord, filter: &AccountFilter) -> bool {
    if let Some(scope) = filter.scope.as_deref() {
        if account.scope != scope {
            return false;
        }
    }
    match filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        Some(query) => {
            let needle = query.to_lowercase();
            account.display_name.to_lowercase().contains(&needle)
                || account
                    .provider_subject
                    .as_deref()
                    .is_some_and(|s| s.to_lowercase().contains(&needle))
        }
        None => true,
    }
}

fn account_get(ctx: &HandlerContext, account_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    // The detail window is the administration view: who else has access, which
    // sessions are open, which nodes hold the credential. A grantee may use the
    // account but is not entitled to that, and saying so is safe — they already
    // know the account exists.
    caller.require_owner_or_admin(&account)?;

    let dec = decorate(ctx, std::slice::from_ref(&account))?;
    Ok(pa(P::AccountGetResponse {
        grants: grant_entries(ctx, &account)?,
        sessions: session_entries(ctx, &account)?,
        nodes: node_entries(ctx, &account)?,
        agents: dec.agents.of(&account),
        account: account_info(&account, &dec),
    }))
}

fn account_create(
    ctx: &HandlerContext,
    engine_id: &str,
    display_name: &str,
    scope: &str,
    owner_user_id: &Option<String>,
    credential_kind: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    // A personal account is minted for the CALLER, whatever the request says: a
    // user must not be able to create an account in somebody else's name, and
    // an administrator creating one for somebody else would own a credential
    // that person is then asked to log in with.
    let owner = match scope {
        "user" => Some(caller.user_id.clone()),
        "global" => {
            caller.require_admin("creating a shared account")?;
            None
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "account scope must be 'global' or 'user', not '{other}'"
            )))
        }
    };
    if owner_user_id
        .as_deref()
        .is_some_and(|id| Some(id) != owner.as_deref())
    {
        return Err(ProtocolError::bad_request(
            "a personal account belongs to the caller and cannot name another owner",
        ));
    }
    let new = NewAccount {
        account_id: uuid::Uuid::new_v4().to_string(),
        org_id: caller.org_id.clone(),
        engine_id: engine_id.to_string(),
        display_name: display_name.trim().to_string(),
        scope: scope.to_string(),
        owner_user_id: owner,
        credential_kind: credential_kind.to_string(),
        created_by: caller.user_id.clone(),
    };
    store::validate_new_account(&new).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let account =
        store::create_account(&ctx.state.db, &new).map_err(|e| internal("account create", e))?;
    Ok(pa(P::AccountCreateResponse {
        account: single_account_info(ctx, &account)?,
    }))
}

fn account_update(
    ctx: &HandlerContext,
    account_id: &str,
    display_name: &Option<String>,
    status: &Option<String>,
    max_sessions: &Option<i64>,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_owner_or_admin(&account)?;
    // The name an account is listed under belongs to whoever keeps it; on a
    // personal account that is its owner.
    if display_name.is_some() {
        caller.require_account_keeper(&account, "renaming")?;
    }
    // Disabling an account takes it away from everyone it was granted to, so it
    // is an administrator's decision even on a personal account — the owner can
    // still delete their own.
    if status.is_some() {
        caller.require_admin("changing an account's status")?;
    }
    // A limit is not about this account's credential but about how much of the
    // node one account may occupy at once, so it is reached through the same
    // gate as disabling: an administrator's decision, also on an account the
    // caller owns.
    if max_sessions.is_some() {
        caller.require_admin("limiting an account's concurrent sessions")?;
    }
    let update = AccountUpdate {
        display_name: display_name.clone(),
        status: status.clone(),
        plan_label: None,
        home_node_id: None,
        max_sessions: *max_sessions,
    };
    let existed = store::update_account(&ctx.state.db, account_id, &update, Some(&caller.user_id))
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if !existed {
        return Err(not_found());
    }
    Ok(ack(account_id, None, true))
}

async fn account_delete(
    ctx: &HandlerContext,
    account_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_owner_or_admin(&account)?;
    let removed = store::delete_account(&ctx.state.db, account_id, Some(&caller.user_id))
        .map_err(|e| internal("account delete", e))?;
    if !removed {
        return Err(not_found());
    }
    // A sign-in still running for the account now has nowhere to store what it
    // obtains, and its bridge would keep a provider terminal open against an
    // account this organisation no longer has. The credential goes with it, with
    // or without a bridge: `drop_account_credential` tells a running bridge to
    // drop what it holds, and removes the files from the account root itself when
    // no bridge is up — a file left there would be a retired token on this disk
    // with no row left to say whose it was.
    login::forget_account(account_id);
    // The row is gone, so a failure here is one the operator can never be asked
    // about again: nothing left in the store names the file the bridge kept, and
    // the reconcile reaches accounts its own rows point at. The warning carries
    // the path (`purge_account_credentials` names every tree it could not
    // empty), and the ack says the outcome was not clean instead of the plain
    // success the screen would otherwise print over a plaintext token still on
    // the disk.
    let purge = crate::services::agent_runtime::drop_account_credential(account_id).await;
    if let Err(error) = &purge {
        tracing::warn!(
            %account_id,
            error = %format!("{error:#}"),
            "a deleted agent account left a credential on this node"
        );
    }
    Ok(ack(
        account_id,
        purge.is_err().then_some(PURGE_INCOMPLETE),
        purge.is_ok(),
    ))
}

/// The i18n key that says a purge landed in the store but not on the disk.
///
/// `AccountOpAck` carries no path field, so the path itself reaches the operator
/// through the node's log, next to the warning that names it.
const PURGE_INCOMPLETE: &str = "agent_accounts.purge_incomplete";

/// The acknowledgement for a write that has nothing to return. `message_key` is
/// an i18n key, never a sentence.
///
/// `ok` is false for the one partial outcome this family has: the operation the
/// operator asked for was performed, but a plaintext provider credential could
/// not be removed from this node's disk. A screen that rendered its success
/// message anyway would be telling the operator the token is gone while it is
/// still readable on the node.
fn ack(account_id: &str, message_key: Option<&str>, ok: bool) -> MessageBody {
    pa(P::AccountOpAck {
        account_id: Some(account_id.to_string()),
        ok,
        message_key: message_key.map(str::to_string),
    })
}

// =============================================================================
// Credential (API keys only — a provider login is the WP3 exchange)
// =============================================================================

fn credential_set(
    ctx: &HandlerContext,
    account_id: &str,
    material: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_account_keeper(&account, "storing a credential")?;
    if account.credential_kind != "api_key" {
        return Err(ProtocolError::bad_request(
            "this account signs in through the provider, so it has no key to paste",
        ));
    }
    // A paste is the `api_key` twin of a sign-in: both put the account's
    // credential on THIS disk, so both pass the same node gate. `mint_credential`
    // claims the home, and a node may not become the home of a credential it is
    // forbidden to hold — `set_receives_accounts` refuses to take a home node out
    // of the fleet, and without this check the paste path went around that
    // protection in the other order: it homed the account here and the fleet
    // purged the only copy on the next reconcile, leaving a home with no
    // credential on it. Refused BEFORE the write, so nothing is minted, nothing
    // is homed and no audit row claims a move.
    if !store::receives_accounts(&ctx.state.db, ctx.state.local_node_id.as_ref())
        .map_err(|e| internal("runtime node", e))?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            crate::services::agent_runtime::NOT_RECEIVING_ACCOUNTS,
        ));
    }
    let meta = CredentialMeta {
        provider_subject: None,
        expires_at: None,
        refreshed_by_node: Some(ctx.state.local_node_id.to_string()),
        actor: Some(caller.user_id.clone()),
    };
    let outcome = store::mint_credential(
        &ctx.state.db,
        &ctx.state.settings_cipher,
        account_id,
        material,
        &meta,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let message_key = match outcome {
        CredentialWrite::Applied { .. } => {
            // Every node in the account fleet gets the key now rather than at
            // its next reconnect: an agent scheduled on another node would
            // otherwise refuse for as long as the peers stay connected.
            crate::provider_accounts::credential_sync::publish_to_fleet();
            None
        }
        // The same key pasted twice is not an error and must not bump the
        // revision: every node that already materialized it would re-fetch a
        // credential that did not change.
        CredentialWrite::Unchanged { .. } => Some("agent_accounts.credential_unchanged"),
        CredentialWrite::Conflict { .. } | CredentialWrite::Stale { .. } => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Conflict,
                "another node stored a different credential for this account; sign in again",
            ))
        }
    };
    Ok(ack(account_id, message_key, true))
}

async fn credential_clear(
    ctx: &HandlerContext,
    account_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_account_keeper(&account, "removing a credential")?;
    let cleared = store::clear_credential(&ctx.state.db, account_id, Some(&caller.user_id))
        .map_err(|e| internal("credential clear", e))?;
    let mut purge_failed = false;
    if cleared {
        // This node's own copy goes now; the other nodes learn from the
        // revocation mark the account row carries and purge on their own
        // reconcile. There is nothing to "push" for a removal — the material
        // never travels, so its absence carries nothing.
        let purge = crate::services::agent_runtime::drop_account_credential(account_id).await;
        purge_failed = purge.is_err();
        if let Err(error) = &purge {
            tracing::warn!(
                %account_id,
                error = %format!("{error:#}"),
                "a cleared agent credential could not be dropped from this node"
            );
        }
    }
    Ok(ack(
        account_id,
        if purge_failed {
            // The revocation is real and the row is gone, so the file is what
            // the operator still has to deal with — saying only "cleared" would
            // report a token this node can no longer name as gone.
            Some(PURGE_INCOMPLETE)
        } else {
            (!cleared).then_some("agent_accounts.credential_absent")
        },
        !purge_failed,
    ))
}

// =============================================================================
// Grants
// =============================================================================

fn grant_entries(
    ctx: &HandlerContext,
    account: &AccountRecord,
) -> Result<Vec<GrantEntry>, ProtocolError> {
    let records = store::list_grants(&ctx.state.db, &account.account_id)
        .map_err(|e| internal("grant list", e))?;
    let user_ids = unique(
        records
            .iter()
            .filter(|g| g.subject_type == "user")
            .map(|g| g.subject_id.clone()),
    );
    let group_ids = unique(
        records
            .iter()
            .filter(|g| g.subject_type == "group")
            .map(|g| g.subject_id.clone()),
    );
    // Who granted each row is a user id like any other, so it is resolved in
    // the SAME lookup as the subjects — one query, and a grantor who is also a
    // subject is not looked up twice.
    let named_ids = unique(
        user_ids
            .iter()
            .cloned()
            .chain(records.iter().map(|g| g.granted_by.clone())),
    );
    let users = repository::lookup_user_names(&ctx.state.db, &named_ids)
        .map_err(|e| internal("grant user names", e))?;
    let groups = repository::lookup_group_info(&ctx.state.db, &group_ids)
        .map_err(|e| internal("grant group names", e))?;
    // How many people a group grant actually reaches, which is its members
    // INSIDE this account's organisation — the effective-grant SQL admits
    // nobody else, and a group's own size would promise access it never gives.
    let members = store::group_member_counts_in_org(&ctx.state.db, &account.org_id, &group_ids)
        .map_err(|e| internal("grant group members", e))?;
    let org_name = records
        .iter()
        .any(|g| g.subject_type == "org")
        .then(|| org_display_name(ctx, &account.org_id))
        .transpose()?;

    Ok(records
        .into_iter()
        .map(|grant| {
            let (display_name, member_count) = match grant.subject_type.as_str() {
                "user" => (
                    users
                        .get(&grant.subject_id)
                        .map(|row| super::model_metrics::user_presentation(row).0)
                        .unwrap_or_else(|| grant.subject_id.clone()),
                    None,
                ),
                "group" => (
                    groups
                        .get(&grant.subject_id)
                        .map(|(name, _)| name.clone())
                        .unwrap_or_else(|| grant.subject_id.clone()),
                    Some(members.get(&grant.subject_id).copied().unwrap_or(0)),
                ),
                _ => (
                    org_name.clone().unwrap_or_else(|| account.org_id.clone()),
                    None,
                ),
            };
            GrantEntry {
                subject_type: grant.subject_type,
                subject_id: grant.subject_id,
                display_name,
                member_count,
                granted_by_name: users
                    .get(&grant.granted_by)
                    .map(|row| super::model_metrics::user_presentation(row).0),
                granted_at: grant.granted_at,
            }
        })
        .collect())
}

fn org_display_name(ctx: &HandlerContext, org_id: &str) -> Result<String, ProtocolError> {
    let org = crate::services::org::get_organization(&ctx.state.db, org_id)
        .map_err(|e| internal("organisation name", e))?;
    Ok(org
        .map(|org| org.name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| org_id.to_string()))
}

fn grants_set(
    ctx: &HandlerContext,
    account_id: &str,
    grants: &[GrantEntry],
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_admin("granting access to an account")?;
    let desired: Vec<GrantInput> = grants
        .iter()
        .map(|grant| GrantInput {
            subject_type: grant.subject_type.clone(),
            subject_id: grant.subject_id.clone(),
        })
        .collect();
    store::set_grants(&ctx.state.db, account_id, &desired, &caller.user_id)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(pa(P::GrantsSetResponse {
        grants: grant_entries(ctx, &account)?,
    }))
}

// =============================================================================
// Sessions and per-node state
// =============================================================================

fn session_entries(
    ctx: &HandlerContext,
    account: &AccountRecord,
) -> Result<Vec<AccountSessionInfo>, ProtocolError> {
    let records = store::list_sessions(&ctx.state.db, &account.account_id)
        .map_err(|e| internal("session list", e))?;
    let users = repository::lookup_user_names(
        &ctx.state.db,
        &unique(records.iter().map(|s| s.user_id.clone())),
    )
    .map_err(|e| internal("session user names", e))?;
    let nodes = node_names(ctx, &unique(records.iter().map(|s| s.node_id.clone())))?;
    // Names are resolved per LIST, not per row: a busy shared account is
    // exactly the one with many sessions, and it is the one that would pay a
    // query per session for the agent and another for the workspace.
    let agents = repository::lookup_agent_names(
        &ctx.state.db,
        &unique(records.iter().filter_map(|s| s.agent_id.clone())),
    )
    .map_err(|e| internal("session agent names", e))?;
    let workspaces = crate::code_studio::repository::lookup_workspace_names(
        &ctx.state.db,
        &unique(records.iter().filter_map(|s| s.workspace_id.clone())),
    )
    .map_err(|e| internal("session workspace names", e))?;

    let mut out = Vec::with_capacity(records.len());
    for session in records {
        out.push(AccountSessionInfo {
            user_display_name: users
                .get(&session.user_id)
                .map(|row| super::model_metrics::user_presentation(row).0)
                .unwrap_or_else(|| session.user_id.clone()),
            agent_name: session
                .agent_id
                .as_deref()
                .and_then(|id| agents.get(id).cloned()),
            workspace_name: session
                .workspace_id
                .as_deref()
                .and_then(|id| workspaces.get(id).cloned()),
            node_name: nodes
                .get(&session.node_id)
                .cloned()
                .flatten()
                .unwrap_or_else(|| session.node_id.clone()),
            session_id: session.session_id,
            user_id: session.user_id,
            agent_id: session.agent_id,
            workspace_id: session.workspace_id,
            node_id: session.node_id,
            started_at: session.started_at,
            last_used_at: session.last_used_at,
        });
    }
    Ok(out)
}

fn session_list(ctx: &HandlerContext, account_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_owner_or_admin(&account)?;
    Ok(pa(P::SessionListResponse {
        sessions: session_entries(ctx, &account)?,
    }))
}

fn node_entries(
    ctx: &HandlerContext,
    account: &AccountRecord,
) -> Result<Vec<AccountNodeInfo>, ProtocolError> {
    let states = store::list_node_states(&ctx.state.db, &account.account_id)
        .map_err(|e| internal("node state list", e))?;
    let runtime: HashMap<String, bool> = store::list_runtime_nodes(&ctx.state.db)
        .map_err(|e| internal("runtime nodes", e))?
        .into_iter()
        .map(|node| (node.node_id, node.receives_accounts))
        .collect();
    let names = node_names(ctx, &unique(states.iter().map(|s| s.node_id.clone())))?;
    Ok(states
        .into_iter()
        .map(|state| AccountNodeInfo {
            node_name: names
                .get(&state.node_id)
                .cloned()
                .flatten()
                .unwrap_or_else(|| state.node_id.clone()),
            receives_accounts: runtime.get(&state.node_id).copied().unwrap_or(false),
            node_id: state.node_id,
            applied_revision: state.applied_revision,
            runtime_state: state.runtime_state,
            last_error: state.last_error,
        })
        .collect())
}

// =============================================================================
// The user's own accounts (U01)
// =============================================================================

fn my_account_list(
    ctx: &HandlerContext,
    engine_id: &Option<String>,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let rows = store::list_accounts_for_user(
        &ctx.state.db,
        &caller.org_id,
        &caller.user_id,
        engine_id.as_deref(),
    )
    .map_err(|e| internal("my account list", e))?;
    let last_used = store::last_used_by_user(&ctx.state.db, &caller.user_id)
        .map_err(|e| internal("last used", e))?;
    let dec = decorate(ctx, &rows)?;
    Ok(pa(P::MyAccountListResponse {
        accounts: rows
            .iter()
            .map(|account| {
                let is_mine = account.is_owned_by(&caller.user_id);
                let supports_login = accounts::engine(&account.engine_id)
                    .is_some_and(|engine| engine.supports_login);
                MyAccountInfo {
                    // Only the person an account belongs to can sign it in, and
                    // only they can throw it away. A grant makes a shared
                    // account usable, not manageable.
                    can_login: is_mine
                        && supports_login
                        && account.credential_kind == "provider_login",
                    can_delete: is_mine,
                    last_used_at: last_used.get(&account.account_id).cloned(),
                    used_on: dec.used_on(&account.account_id),
                    session_count: dec.sessions.get(&account.account_id).copied().unwrap_or(0),
                    account_id: account.account_id.clone(),
                    engine_id: account.engine_id.clone(),
                    display_name: account.display_name.clone(),
                    scope: account.scope.clone(),
                    status: account.status.clone(),
                    provider_subject: account.provider_subject.clone(),
                    plan_label: account.plan_label.clone(),
                    credential_kind: account.credential_kind.clone(),
                }
            })
            .collect(),
    }))
}

// =============================================================================
// The node matrix (N01)
// =============================================================================

/// Every node the matrix knows about, in display order, with what is known
/// about each. Shared with the flag handler so that screen and that decision
/// answer to ONE definition of "a node of this installation" — a flag set on a
/// node the matrix does not list is a decision nobody can see or undo.
struct NodeCatalog {
    local_id: String,
    order: Vec<String>,
    online: HashMap<String, bool>,
    hostnames: HashMap<String, String>,
    rows: Vec<crate::provider_accounts::RuntimeNodeRecord>,
}

fn node_catalog(ctx: &HandlerContext) -> Result<NodeCatalog, ProtocolError> {
    let rows =
        store::list_runtime_nodes(&ctx.state.db).map_err(|e| internal("runtime nodes", e))?;
    // The matrix is the union of "nodes we could run on" and "nodes somebody
    // already decided about". A node that was configured and has since left
    // the mesh must stay visible: its `receives_accounts` flag is still true
    // and it may still hold credentials.
    let local_id = ctx.state.local_node_id.to_string();
    let mut order: Vec<String> = vec![local_id.clone()];
    let mut online: HashMap<String, bool> = HashMap::from([(local_id.clone(), true)]);
    let mut hostnames: HashMap<String, String> = HashMap::new();
    if let Some(iroh) = ctx.state.quic_mesh.as_ref() {
        for peer in ctx.state.mesh_peer_store.list() {
            if peer.node_id == local_id || !iroh.is_trusted(&peer.node_id) {
                continue;
            }
            online.insert(peer.node_id.clone(), peer.quic_connected);
            if !peer.hostname.is_empty() {
                hostnames.insert(peer.node_id.clone(), peer.hostname.clone());
            }
            order.push(peer.node_id.clone());
        }
    }
    for row in &rows {
        if !order.contains(&row.node_id) {
            order.push(row.node_id.clone());
        }
    }
    Ok(NodeCatalog {
        local_id,
        order,
        online,
        hostnames,
        rows,
    })
}

fn runtime_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    caller(ctx)?.require_admin("the agent runtime matrix")?;
    let counts = store::materialized_account_counts(&ctx.state.db)
        .map_err(|e| internal("runtime account counts", e))?;
    let NodeCatalog {
        local_id,
        order,
        online,
        hostnames,
        rows,
    } = node_catalog(ctx)?;
    let names = node_names(ctx, &order)?;
    let configured: HashMap<&str, &crate::provider_accounts::RuntimeNodeRecord> =
        rows.iter().map(|row| (row.node_id.as_str(), row)).collect();

    // Probed once for the whole matrix: the probe starts a process.
    let local_sandbox = crate::code_studio::process_sandbox::ProcessSandbox::check_available();
    let nodes = order
        .iter()
        .map(|node_id| {
            let row = configured.get(node_id.as_str());
            RuntimeNodeInfo {
                node_name: names
                    .get(node_id)
                    .cloned()
                    .flatten()
                    .or_else(|| hostnames.get(node_id).cloned())
                    .unwrap_or_else(|| node_id.clone()),
                // Only a node can probe its own process isolation, so a remote
                // one stays unknown until it reports — the same rule the Code
                // Studio node picker already applies.
                sandbox_capable: (node_id == &local_id).then(|| local_sandbox.is_ok()),
                sandbox_cause: (node_id == &local_id)
                    .then(|| local_sandbox.err().map(|e| crate::code_studio::sandbox_cause(&e)))
                    .flatten(),
                // Same rule, same reason: this process knows what IT runs on,
                // and a peer's operating system is its own report to make.
                os: (node_id == &local_id)
                    .then(|| crate::services::agent_runtime::host_platform().to_string()),
                online: online.get(node_id).copied().unwrap_or(false),
                receives_accounts: row.is_some_and(|row| row.receives_accounts),
                engines: row
                    .map(|row| {
                        row.engines
                            .iter()
                            .map(|engine| RuntimeEngineInfo {
                                engine_id: engine.engine_id.clone(),
                                install_state: engine.install_state.clone(),
                                version: engine.version.clone(),
                                last_error: engine.last_error.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                account_count: counts.get(node_id).copied().unwrap_or(0),
                // Nothing else on the wire names this machine, so a window that
                // wants to say "this row is the node you are looking at" has
                // only the answering node's own answer to go on.
                is_local: node_id == &local_id,
                node_id: node_id.clone(),
            }
        })
        .collect();
    Ok(pa(P::RuntimeListResponse { nodes }))
}

fn runtime_set_receives_accounts(
    ctx: &HandlerContext,
    node_id: &str,
    enabled: bool,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    caller.require_admin("changing which nodes receive accounts")?;
    if node_id.trim().is_empty() {
        return Err(ProtocolError::bad_request("node_id is required"));
    }
    // Only a node the matrix lists: a flag on anything else is a row nobody can
    // see, and on a mistyped id it would silently replicate a decision about a
    // node that does not exist.
    if !node_catalog(ctx)?.order.iter().any(|id| id == node_id) {
        return Err(not_found());
    }
    // A refusal here is the store telling the administrator what to do first
    // (move the home of the accounts living on that node), so its text is the
    // answer — not an internal error that hides it.
    store::set_receives_accounts(&ctx.state.db, node_id, enabled, Some(&caller.user_id))
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(pa(P::AccountOpAck {
        account_id: None,
        ok: true,
        message_key: None,
    }))
}

// =============================================================================
// Remote nodes
// =============================================================================

/// Runs one request of this family on another node, signed.
///
/// The bytes travel through `AppRouteOp`: the origin mints a `SessionAssertion`
/// for the person acting, `args_digest` binds these exact bytes, and the
/// executing node rebuilds the actor from ITS OWN database and runs the same
/// dispatcher this function belongs to. Nothing is decided from what the wire
/// said about the caller — which is the whole difference from the unsigned
/// `AgentRpc.user_id` these operations used to travel on, where a peer's claim
/// about who was asking was taken at face value.
async fn on_node(
    ctx: &HandlerContext,
    node_id: &str,
    payload: &P,
) -> Result<MessageBody, ProtocolError> {
    let bytes = tentaflow_protocol::cbor::encode(&pa(payload.clone()))
        .map_err(|e| internal("request encode", e))?;
    super::app_route::forward_to_node(ctx, node_id, bytes).await
}

/// Where an operation about `node_id` has to run. `None` is "here".
fn elsewhere(ctx: &HandlerContext, node_id: &str) -> Option<String> {
    (!node_id.is_empty() && node_id != ctx.state.local_node_id.as_ref())
        .then(|| node_id.to_string())
}

// =============================================================================
// Login (A02)
// =============================================================================

/// Who may sign an account in: its owner, and for a shared account an
/// administrator.
///
/// A grant is deliberately not enough. Signing in decides WHICH provider
/// identity every grantee's runs are attributed to and whose subscription pays
/// for them, and that is the account keeper's decision — the same rule
/// `CredentialSetRequest` applies to an API key, because a login and a pasted
/// key put the same thing in the same place.
fn require_login_authority(caller: &Caller, account: &AccountRecord) -> Result<(), ProtocolError> {
    caller.require_account_keeper(account, "signing in")?;
    if account.credential_kind != "provider_login" {
        return Err(ProtocolError::bad_request(
            "this account authenticates with a key, so there is nothing to sign in to",
        ));
    }
    if account.status == "disabled" {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "this account is disabled",
        ));
    }
    Ok(())
}

async fn login_start(
    ctx: &HandlerContext,
    account_id: &str,
    node_id: &Option<String>,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    require_login_authority(&caller, &account)?;
    // Where the CLI runs: what the caller asked for, else the account's home,
    // else this node. A sign-in is a process, so it happens somewhere specific
    // and the answer says where.
    let target = node_id
        .clone()
        .filter(|id| !id.is_empty())
        .or_else(|| account.home_node_id.clone())
        .unwrap_or_else(|| ctx.state.local_node_id.to_string());
    if let Some(remote) = elsewhere(ctx, &target) {
        let answer = on_node(
            ctx,
            &remote,
            &P::LoginStartRequest {
                account_id: account_id.to_string(),
                node_id: Some(remote.clone()),
            },
        )
        .await?;
        // What makes the follow-up polls forwardable: this node started the
        // sign-in for this person, so it remembers where it runs instead of
        // reading the destination out of whatever login id it is later handed.
        if let MessageBody::ProviderAccountBody(P::LoginStartResponse { login_id, .. }) = &answer {
            login::remember_remote(login_id, &remote, account_id, &caller.user_id);
        }
        return Ok(answer);
    }
    // A sign-in leaves the account's credential on the node that runs it, which
    // is what the matrix toggle decides about. Checked here as well as in the
    // runtime, because the error an operator reads for it is "this node was
    // excluded", not "the bridge would not start".
    if !store::receives_accounts(&ctx.state.db, &target).map_err(|e| internal("runtime node", e))? {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            crate::services::agent_runtime::NOT_RECEIVING_ACCOUNTS,
        ));
    }
    if let Some(engine_id) = login::engine_of_running_flow(account_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("a {engine_id} sign-in for this account is already running on this node"),
        ));
    }
    let snapshot = login::start(
        &ctx.state.db,
        &ctx.state.settings_cipher,
        &target,
        &account,
        &caller.user_id,
    )
    .await
    .map_err(login_failed)?;
    Ok(pa(P::LoginStartResponse {
        login_id: snapshot.login_id,
        node_id: snapshot.node_id,
        verification_url: snapshot.verification_url,
        instruction_key: login::INSTRUCTION_KEY.to_string(),
        expires_at: snapshot.expires_at,
    }))
}

/// A sign-in that could not be started is an environment problem the operator
/// has to read — a missing runtime, a node without isolation, a CLI that never
/// printed an address — so the text travels instead of being flattened into
/// "internal error".
fn login_failed(error: anyhow::Error) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{error:#}"))
}

async fn login_input(
    ctx: &HandlerContext,
    login_id: &str,
    value: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    if let Some(remote) = login_node(ctx, login_id, &caller)? {
        return on_node(
            ctx,
            &remote,
            &P::LoginInputRequest {
                login_id: login_id.to_string(),
                value: value.to_string(),
            },
        )
        .await;
    }
    login::input(login_id, &caller.user_id, value)
        .await
        .map_err(login_failed)?;
    Ok(pa(P::AccountOpAck {
        account_id: None,
        ok: true,
        message_key: None,
    }))
}

async fn login_status(ctx: &HandlerContext, login_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    if let Some(remote) = login_node(ctx, login_id, &caller)? {
        return on_node(
            ctx,
            &remote,
            &P::LoginStatusRequest {
                login_id: login_id.to_string(),
            },
        )
        .await;
    }
    let snapshot = login::status(login_id, &caller.user_id)
        .map_err(|error| ProtocolError::new(ProtocolErrorCode::NotFound, format!("{error:#}")))?;
    Ok(pa(P::LoginStatusResponse {
        state: snapshot.state,
        provider_subject: snapshot.provider_subject,
        plan_label: snapshot.plan_label,
        message_key: snapshot.message_key,
    }))
}

async fn login_cancel(ctx: &HandlerContext, login_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    if let Some(remote) = login_node(ctx, login_id, &caller)? {
        return on_node(
            ctx,
            &remote,
            &P::LoginCancelRequest {
                login_id: login_id.to_string(),
            },
        )
        .await;
    }
    login::cancel(login_id, &caller.user_id)
        .await
        .map_err(|error| ProtocolError::new(ProtocolErrorCode::NotFound, format!("{error:#}")))?;
    Ok(pa(P::AccountOpAck {
        account_id: None,
        ok: true,
        message_key: None,
    }))
}

/// Where a sign-in the caller is polling actually runs. `None` is "here".
///
/// The destination comes from what THIS node knows — its own running flows, and
/// the ones it started on a peer for this person — never from the node name
/// inside the login id. Taking it from the id made every id a way to address
/// any trusted peer: a caller could walk the ids, and each guess produced a
/// signed operation and an audit row on the far side before any ownership was
/// checked. An id this node has no record of answers `NotFound`, the same as
/// one that never existed.
fn login_node(
    ctx: &HandlerContext,
    login_id: &str,
    caller: &Caller,
) -> Result<Option<String>, ProtocolError> {
    if login::runs_here(login_id) {
        return Ok(None);
    }
    match login::remote_node_of(login_id, &caller.user_id) {
        // A record from before this node restarted, or a peer that has since
        // become this node: run it locally and let the flow lookup answer.
        Some(node_id) => Ok(elsewhere(ctx, &node_id)),
        None => Err(not_found()),
    }
}

// =============================================================================
// Sessions
// =============================================================================

async fn session_revoke(
    ctx: &HandlerContext,
    account_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_owner_or_admin(&account)?;
    let Some(session) = store::list_sessions(&ctx.state.db, account_id)
        .map_err(|e| internal("session list", e))?
        .into_iter()
        .find(|session| session.session_id == session_id)
    else {
        return Err(not_found());
    };
    if let Some(remote) = elsewhere(ctx, &session.node_id) {
        return on_node(
            ctx,
            &remote,
            &P::SessionRevokeRequest {
                account_id: account_id.to_string(),
                session_id: session_id.to_string(),
            },
        )
        .await;
    }
    // The CLI goes first. The row is this node's record that the session is
    // open, and deleting it before the process is gone turns a bridge that
    // refuses to close into a session nobody can see and nobody can close: the
    // operator is told "internal error" and the list no longer offers the row
    // the CLI is still running behind.
    if let Some(bridge) = crate::services::agent_runtime::running_bridge(account_id).await {
        bridge
            .call(
                reqwest::Method::DELETE,
                &format!("/sessions/{session_id}"),
                None,
            )
            .await
            .map_err(|e| internal("session close", e))?;
    }
    store::delete_session(&ctx.state.db, account_id, session_id)
        .map_err(|e| internal("session delete", e))?;
    Ok(ack(account_id, None, true))
}

// =============================================================================
// Runtime installation (N01)
// =============================================================================

async fn runtime_install(
    ctx: &HandlerContext,
    node_id: &str,
    engine_id: &str,
    install: bool,
) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    caller.require_admin("installing an agent runtime")?;
    if let Some(remote) = elsewhere(ctx, node_id) {
        let forwarded = if install {
            P::RuntimeInstallRequest {
                node_id: remote.clone(),
                engine_id: engine_id.to_string(),
            }
        } else {
            P::RuntimeUninstallRequest {
                node_id: remote.clone(),
                engine_id: engine_id.to_string(),
            }
        };
        return on_node(ctx, &remote, &forwarded).await;
    }
    use crate::services::agent_runtime as runtime;
    // The administrator is named in the matrix row's audit entry. On a forwarded
    // request that is the actor the far node re-derived from its own database,
    // never the one the wire claimed — `on_node` is what guarantees it.
    if install {
        runtime::install_engine(&ctx.state.db, engine_id, &caller.user_id)
            .await
            .map_err(|error| {
                ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{error:#}"))
            })?;
    } else {
        runtime::uninstall_engine(&ctx.state.db, engine_id, &caller.user_id)
            .await
            .map_err(|error| {
                ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{error:#}"))
            })?;
    }
    // The matrix row this node now reports, so the screen redraws from the
    // measurement instead of from what it asked for.
    let MessageBody::ProviderAccountBody(P::RuntimeListResponse { nodes }) = runtime_list(ctx)?
    else {
        return Err(internal("runtime list", "unexpected runtime list answer"));
    };
    let local = ctx.state.local_node_id.to_string();
    nodes
        .into_iter()
        .find(|node| node.node_id == local)
        .map(|node| pa(P::RuntimeStatusResponse { node }))
        .ok_or_else(|| internal("runtime list", "this node is missing from its own matrix"))
}

// =============================================================================
// Dispatcher
// =============================================================================

#[handler(variant = "ProviderAccountBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn provider_account_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ProviderAccountBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected ProviderAccountBody")),
    };
    match payload {
        P::AccountListRequest {
            engine_id,
            scope,
            query,
        } => account_list(ctx, engine_id, scope, query),
        P::AccountGetRequest { account_id } => account_get(ctx, account_id),
        P::AccountCreateRequest {
            engine_id,
            display_name,
            scope,
            owner_user_id,
            credential_kind,
        } => account_create(
            ctx,
            engine_id,
            display_name,
            scope,
            owner_user_id,
            credential_kind,
        ),
        P::AccountUpdateRequest {
            account_id,
            display_name,
            status,
            max_sessions,
        } => account_update(ctx, account_id, display_name, status, max_sessions),
        P::AccountDeleteRequest { account_id } => account_delete(ctx, account_id).await,
        P::CredentialSetRequest {
            account_id,
            material,
        } => credential_set(ctx, account_id, material),
        P::CredentialClearRequest { account_id } => credential_clear(ctx, account_id).await,
        P::GrantsSetRequest { account_id, grants } => grants_set(ctx, account_id, grants),
        P::SessionListRequest { account_id } => session_list(ctx, account_id),
        P::MyAccountListRequest { engine_id } => my_account_list(ctx, engine_id),
        P::RuntimeListRequest {} => runtime_list(ctx),
        P::RuntimeSetReceivesAccountsRequest { node_id, enabled } => {
            runtime_set_receives_accounts(ctx, node_id, *enabled)
        }
        P::LoginStartRequest {
            account_id,
            node_id,
        } => login_start(ctx, account_id, node_id).await,
        P::LoginInputRequest { login_id, value } => login_input(ctx, login_id, value).await,
        P::LoginStatusRequest { login_id } => login_status(ctx, login_id).await,
        P::LoginCancelRequest { login_id } => login_cancel(ctx, login_id).await,
        P::SessionRevokeRequest {
            account_id,
            session_id,
        } => session_revoke(ctx, account_id, session_id).await,
        P::RuntimeInstallRequest { node_id, engine_id } => {
            runtime_install(ctx, node_id, engine_id, true).await
        }
        P::RuntimeUninstallRequest { node_id, engine_id } => {
            runtime_install(ctx, node_id, engine_id, false).await
        }

        // Responses share the enum with the requests; a client sending one back
        // is a protocol error, not a request this server can answer.
        other => Err(ProtocolError::bad_request(format!(
            "'{}' is not a provider account request",
            variant_of(other)
        ))),
    }
}

fn variant_of(payload: &P) -> String {
    serde_json::to_value(payload)
        .ok()
        .and_then(|v| v.as_object().and_then(|m| m.keys().next().cloned()))
        .unwrap_or_else(|| "unknown".to_string())
}

// =============================================================================
// Variant registration
// =============================================================================

/// `#[handler]` registers the dispatcher under the family name, which no frame
/// ever carries — `variant_name_of` reports the concrete variant. Each request
/// variant therefore needs its own registry entry pointing at the same
/// dispatch wrapper, or `dispatch::find` answers NotImplemented.
macro_rules! register_provider_account_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_provider_account_dispatch,
            }
        }
    };
}

register_provider_account_variant!(
    "ProviderAccountAccountListRequest",
    "tentaflow_ws_handler_pa_account_list"
);
register_provider_account_variant!(
    "ProviderAccountAccountGetRequest",
    "tentaflow_ws_handler_pa_account_get"
);
register_provider_account_variant!(
    "ProviderAccountAccountCreateRequest",
    "tentaflow_ws_handler_pa_account_create"
);
register_provider_account_variant!(
    "ProviderAccountAccountUpdateRequest",
    "tentaflow_ws_handler_pa_account_update"
);
register_provider_account_variant!(
    "ProviderAccountAccountDeleteRequest",
    "tentaflow_ws_handler_pa_account_delete"
);
register_provider_account_variant!(
    "ProviderAccountCredentialSetRequest",
    "tentaflow_ws_handler_pa_credential_set"
);
register_provider_account_variant!(
    "ProviderAccountCredentialClearRequest",
    "tentaflow_ws_handler_pa_credential_clear"
);
register_provider_account_variant!(
    "ProviderAccountGrantsSetRequest",
    "tentaflow_ws_handler_pa_grants_set"
);
register_provider_account_variant!(
    "ProviderAccountSessionListRequest",
    "tentaflow_ws_handler_pa_session_list"
);
register_provider_account_variant!(
    "ProviderAccountMyAccountListRequest",
    "tentaflow_ws_handler_pa_my_list"
);
register_provider_account_variant!(
    "ProviderAccountRuntimeListRequest",
    "tentaflow_ws_handler_pa_runtime_list"
);
register_provider_account_variant!(
    "ProviderAccountRuntimeSetReceivesAccountsRequest",
    "tentaflow_ws_handler_pa_runtime_receives"
);
register_provider_account_variant!(
    "ProviderAccountLoginStartRequest",
    "tentaflow_ws_handler_pa_login_start"
);
register_provider_account_variant!(
    "ProviderAccountLoginInputRequest",
    "tentaflow_ws_handler_pa_login_input"
);
register_provider_account_variant!(
    "ProviderAccountLoginStatusRequest",
    "tentaflow_ws_handler_pa_login_status"
);
register_provider_account_variant!(
    "ProviderAccountLoginCancelRequest",
    "tentaflow_ws_handler_pa_login_cancel"
);
register_provider_account_variant!(
    "ProviderAccountSessionRevokeRequest",
    "tentaflow_ws_handler_pa_session_revoke"
);
register_provider_account_variant!(
    "ProviderAccountRuntimeInstallRequest",
    "tentaflow_ws_handler_pa_runtime_install"
);
register_provider_account_variant!(
    "ProviderAccountRuntimeUninstallRequest",
    "tentaflow_ws_handler_pa_runtime_uninstall"
);

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_accounts::NewAccount;
    use crate::services::rbac::OrgContext;

    /// The org every fresh database has (migration v32 seeds it), so a
    /// membership row in these tests satisfies the same foreign keys production
    /// rows do.
    const ORG: &str = crate::services::org::DEFAULT_ORG_ID;
    const ALICE: &str = "11111111-1111-4111-8111-111111111111";
    const BOB: &str = "22222222-2222-4222-8222-222222222222";

    fn ctx_for(role: Option<&str>, user_id: &str) -> HandlerContext {
        let state = crate::dispatch::state::AppState::for_test();
        crate::dispatch::test_handler_context(
            state,
            role,
            Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: ORG.to_string(),
                role_id: "role-1".to_string(),
                permissions: Default::default(),
            }),
        )
    }

    /// Lets this node hold account credentials, which every sign-in needs
    /// before the authorization it is really testing is even reached.
    fn node_receives_accounts(ctx: &HandlerContext) {
        store::set_receives_accounts(&ctx.state.db, ctx.state.local_node_id.as_ref(), true, None)
            .expect("runtime node");
    }

    /// Same database, two sessions: the ACL is a property of the caller, so
    /// every test needs both views of one fixture.
    fn second_session(base: &HandlerContext, role: Option<&str>, user_id: &str) -> HandlerContext {
        session_in_org(base, role, user_id, ORG)
    }

    fn session_in_org(
        base: &HandlerContext,
        role: Option<&str>,
        user_id: &str,
        org_id: &str,
    ) -> HandlerContext {
        let mut ctx = crate::dispatch::test_handler_context(
            base.state.clone(),
            role,
            Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: org_id.to_string(),
                role_id: "role-1".to_string(),
                permissions: Default::default(),
            }),
        );
        ctx.correlation_id = base.correlation_id + 1;
        ctx
    }

    fn seed_user(ctx: &HandlerContext, id: &str, display: &str) {
        seed_user_in_org(ctx, id, display, ORG);
    }

    /// A user plus the org membership every session implies — a grant may only
    /// name somebody the organisation has, so a fixture without the membership
    /// is not a fixture production could produce.
    fn seed_user_in_org(ctx: &HandlerContext, id: &str, display: &str, org_id: &str) {
        let conn = ctx.state.db.write().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts (id, username, password_hash, is_active, \
             must_change_password, role, display_name) \
             VALUES (?1, ?2, 'x', 1, 0, 'user', ?3)",
            rusqlite::params![id, display.to_lowercase(), display],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, \
             granted_by) VALUES (?1, ?2, 'role-org-viewer', datetime('now'), 'test')",
            rusqlite::params![org_id, id],
        )
        .unwrap();
    }

    fn seed_org(ctx: &HandlerContext, org_id: &str) {
        ctx.state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, created_at) \
                 VALUES (?1, ?1, ?1, datetime('now'))",
                rusqlite::params![org_id],
            )
            .unwrap();
    }

    fn seed_account(
        ctx: &HandlerContext,
        account_id: &str,
        scope: &str,
        owner: Option<&str>,
    ) -> AccountRecord {
        seed_account_in_org(ctx, account_id, scope, owner, ORG)
    }

    fn seed_account_in_org(
        ctx: &HandlerContext,
        account_id: &str,
        scope: &str,
        owner: Option<&str>,
        org_id: &str,
    ) -> AccountRecord {
        store::create_account(
            &ctx.state.db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: org_id.to_string(),
                engine_id: "claude-code".to_string(),
                display_name: account_id.to_string(),
                scope: scope.to_string(),
                owner_user_id: owner.map(str::to_string),
                credential_kind: "provider_login".to_string(),
                created_by: "admin".to_string(),
            },
        )
        .unwrap()
    }

    /// Rows this run wrote under one action — the dispatch layer's own record of
    /// what it decided, which is the only place a "nothing happened" claim can
    /// be falsified without trusting the handler's return value.
    fn audit_rows(ctx: &HandlerContext, action: &str) -> i64 {
        ctx.state
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = ?1",
                rusqlite::params![action],
                |row| row.get(0),
            )
            .expect("audit rows")
    }

    fn listed(body: &MessageBody) -> Vec<String> {
        match body {
            MessageBody::ProviderAccountBody(P::AccountListResponse { accounts, .. }) => {
                accounts.iter().map(|a| a.account_id.clone()).collect()
            }
            other => panic!("expected an account list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_admin_sees_only_the_accounts_they_own_or_were_granted() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-mine", "user", Some(ALICE));
        seed_account(&admin, "acc-shared", "global", None);
        seed_account(&admin, "acc-secret", "global", None);
        store::set_grants(
            &admin.state.db,
            "acc-shared",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: ALICE.to_string(),
            }],
            BOB,
        )
        .unwrap();

        let request = pa(P::AccountListRequest {
            engine_id: None,
            scope: None,
            query: None,
        });
        let alice = second_session(&admin, Some("user"), ALICE);
        let mut mine = listed(&provider_account_dispatch(&request, &alice).await.unwrap());
        mine.sort();
        assert_eq!(mine, vec!["acc-mine", "acc-shared"]);

        let mut all = listed(&provider_account_dispatch(&request, &admin).await.unwrap());
        all.sort();
        assert_eq!(all, vec!["acc-mine", "acc-secret", "acc-shared"]);
    }

    #[tokio::test]
    async fn an_account_the_caller_may_not_see_answers_exactly_like_a_missing_one() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-secret", "global", None);
        let alice = second_session(&admin, Some("user"), ALICE);

        let hidden = provider_account_dispatch(
            &pa(P::AccountGetRequest {
                account_id: "acc-secret".to_string(),
            }),
            &alice,
        )
        .await
        .unwrap_err();
        let missing = provider_account_dispatch(
            &pa(P::AccountGetRequest {
                account_id: "acc-nope".to_string(),
            }),
            &alice,
        )
        .await
        .unwrap_err();
        assert_eq!(hidden.code, ProtocolErrorCode::NotFound);
        assert_eq!(hidden, missing, "the refusal must not distinguish the two");
    }

    #[tokio::test]
    async fn a_grant_lets_a_user_run_an_account_but_not_administer_it() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-shared", "global", None);
        store::set_grants(
            &admin.state.db,
            "acc-shared",
            &[GrantInput {
                subject_type: "org".to_string(),
                subject_id: String::new(),
            }],
            BOB,
        )
        .unwrap();
        // An org grant only reaches a member of that org.
        admin
            .state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, ?2, 'role-org-viewer', datetime('now'), 'test')",
                rusqlite::params![ORG, ALICE],
            )
            .unwrap();
        let alice = second_session(&admin, Some("user"), ALICE);

        assert_eq!(
            listed(
                &provider_account_dispatch(
                    &pa(P::AccountListRequest {
                        engine_id: None,
                        scope: None,
                        query: None
                    }),
                    &alice
                )
                .await
                .unwrap()
            ),
            vec!["acc-shared"]
        );
        for request in [
            pa(P::AccountGetRequest {
                account_id: "acc-shared".to_string(),
            }),
            pa(P::GrantsSetRequest {
                account_id: "acc-shared".to_string(),
                grants: Vec::new(),
            }),
            pa(P::AccountDeleteRequest {
                account_id: "acc-shared".to_string(),
            }),
        ] {
            let error = provider_account_dispatch(&request, &alice)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{request:?} must be refused as a policy decision, not hidden"
            );
        }
    }

    #[tokio::test]
    async fn a_user_account_is_minted_for_its_caller_and_a_shared_one_needs_an_admin() {
        let alice = ctx_for(Some("user"), ALICE);
        seed_user(&alice, ALICE, "Alice");

        let created = provider_account_dispatch(
            &pa(P::AccountCreateRequest {
                engine_id: "claude-code".to_string(),
                display_name: "Moje Claude".to_string(),
                scope: "user".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
            }),
            &alice,
        )
        .await
        .unwrap();
        let MessageBody::ProviderAccountBody(P::AccountCreateResponse { account }) = created else {
            panic!("expected a created account");
        };
        assert_eq!(account.owner_user_id.as_deref(), Some(ALICE));
        assert_eq!(account.owner_display_name.as_deref(), Some("Alice"));
        assert_eq!(account.status, "pending");
        assert_eq!(account.credential_revision, 0);

        let global = provider_account_dispatch(
            &pa(P::AccountCreateRequest {
                engine_id: "claude-code".to_string(),
                display_name: "Firmowe".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "provider_login".to_string(),
            }),
            &alice,
        )
        .await
        .unwrap_err();
        assert_eq!(global.code, ProtocolErrorCode::PolicyDenied);

        let for_bob = provider_account_dispatch(
            &pa(P::AccountCreateRequest {
                engine_id: "claude-code".to_string(),
                display_name: "Cudze".to_string(),
                scope: "user".to_string(),
                owner_user_id: Some(BOB.to_string()),
                credential_kind: "provider_login".to_string(),
            }),
            &alice,
        )
        .await
        .unwrap_err();
        assert_eq!(for_bob.code, ProtocolErrorCode::BadRequest);
    }

    #[tokio::test]
    async fn an_api_key_is_refused_on_an_account_that_signs_in_through_the_provider() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-login", "global", None);
        let error = provider_account_dispatch(
            &pa(P::CredentialSetRequest {
                account_id: "acc-login".to_string(),
                material: "sk-test".to_string(),
            }),
            &admin,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(error.message.contains("provider"), "{}", error.message);
        assert!(store::credential_summary(&admin.state.db, "acc-login")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn the_node_matrix_is_administration_and_always_contains_this_node() {
        let admin = ctx_for(Some("admin"), BOB);
        let alice = second_session(&admin, Some("user"), ALICE);
        assert_eq!(
            provider_account_dispatch(&pa(P::RuntimeListRequest {}), &alice)
                .await
                .unwrap_err()
                .code,
            ProtocolErrorCode::PolicyDenied
        );

        let local = admin.state.local_node_id.to_string();
        provider_account_dispatch(
            &pa(P::RuntimeSetReceivesAccountsRequest {
                node_id: local.clone(),
                enabled: true,
            }),
            &admin,
        )
        .await
        .unwrap();
        let MessageBody::ProviderAccountBody(P::RuntimeListResponse { nodes }) =
            provider_account_dispatch(&pa(P::RuntimeListRequest {}), &admin)
                .await
                .unwrap()
        else {
            panic!("expected a runtime list");
        };
        let this = nodes
            .iter()
            .find(|node| node.node_id == local)
            .expect("the local node is always in the matrix");
        assert!(this.online, "the node answering the request is online");
        assert!(this.receives_accounts);
        assert!(
            this.sandbox_capable.is_some(),
            "only the local node can answer this, and it must"
        );
        assert_eq!(this.account_count, 0);
    }

    /// The matrix marks the answering node's own row and nothing else. A window
    /// has no other way to name this machine — `RuntimeNodeInfo` carries no id
    /// the dashboard could compare against — so a marker that reached a peer row
    /// would make A03 label the wrong node as "this one", and a marker missing
    /// from the local row would leave it unlabelled.
    #[tokio::test]
    async fn the_node_matrix_marks_the_answering_node_and_no_peer() {
        let admin = ctx_for(Some("admin"), BOB);
        let local = admin.state.local_node_id.to_string();
        for node_id in [local.as_str(), "node-far"] {
            store::set_receives_accounts(&admin.state.db, node_id, true, None)
                .expect("runtime node");
        }
        let MessageBody::ProviderAccountBody(P::RuntimeListResponse { nodes }) =
            provider_account_dispatch(&pa(P::RuntimeListRequest {}), &admin)
                .await
                .unwrap()
        else {
            panic!("expected a runtime list");
        };
        assert!(
            nodes.iter().any(|node| node.node_id == "node-far"),
            "the fixture needs a peer in the matrix for this to say anything: {nodes:?}"
        );
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.is_local)
                .map(|node| node.node_id.as_str())
                .collect::<Vec<_>>(),
            vec![local.as_str()],
            "exactly the answering node is local: {nodes:?}"
        );
        for peer in nodes.iter().filter(|node| node.node_id != local) {
            assert!(
                !peer.is_local,
                "a peer row claiming to be this machine is what the marker cannot do: {peer:?}"
            );
        }
    }

    /// Signing in decides which provider identity the runs are billed to, so a
    /// personal account admits only its owner — an administrator who can delete
    /// the account still may not put an identity inside it.
    #[tokio::test]
    async fn a_personal_sign_in_is_the_owners_alone() {
        let admin = ctx_for(Some("admin"), BOB);
        node_receives_accounts(&admin);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-alice", "user", Some(ALICE));

        let request = pa(P::LoginStartRequest {
            account_id: "acc-alice".to_string(),
            node_id: None,
        });
        let error = provider_account_dispatch(&request, &admin)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied, "{error:?}");

        // The owner passes the authorization gate; what stops her here is the
        // missing runtime on a test node, which is an environment answer.
        let alice = second_session(&admin, Some("user"), ALICE);
        let error = provider_account_dispatch(&request, &alice)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::NotAvailable, "{error:?}");
    }

    /// The mirror case: a shared account is the administrator's to sign in, and
    /// a grantee of it still may not.
    #[tokio::test]
    async fn a_shared_sign_in_needs_an_administrator() {
        let admin = ctx_for(Some("admin"), BOB);
        node_receives_accounts(&admin);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-shared", "global", None);
        store::set_grants(
            &admin.state.db,
            "acc-shared",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: ALICE.to_string(),
            }],
            BOB,
        )
        .unwrap();

        let request = pa(P::LoginStartRequest {
            account_id: "acc-shared".to_string(),
            node_id: None,
        });
        let alice = second_session(&admin, Some("user"), ALICE);
        let error = provider_account_dispatch(&request, &alice)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied, "{error:?}");

        let error = provider_account_dispatch(&request, &admin)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::NotAvailable, "{error:?}");
    }

    /// An API-key account has no login terminal to open, and answering
    /// `NotAvailable` would invite the operator to retry forever.
    #[tokio::test]
    async fn an_api_key_account_has_nothing_to_sign_in_to() {
        let admin = ctx_for(Some("admin"), BOB);
        store::create_account(
            &admin.state.db,
            &NewAccount {
                account_id: "acc-key".to_string(),
                org_id: ORG.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Key".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "api_key".to_string(),
                created_by: BOB.to_string(),
            },
        )
        .unwrap();

        let error = provider_account_dispatch(
            &pa(P::LoginStartRequest {
                account_id: "acc-key".to_string(),
                node_id: None,
            }),
            &admin,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::BadRequest, "{error:?}");
    }

    /// Pasting the same key twice is a person re-confirming a decision, not a
    /// rotation: the revision must stay put, or every node holding the
    /// credential would re-fetch a file that did not change.
    #[tokio::test]
    async fn the_same_api_key_pasted_twice_is_not_a_rotation() {
        let admin = ctx_for(Some("admin"), BOB);
        node_receives_accounts(&admin);
        store::create_account(
            &admin.state.db,
            &NewAccount {
                account_id: "acc-key".to_string(),
                org_id: ORG.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Key".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "api_key".to_string(),
                created_by: BOB.to_string(),
            },
        )
        .unwrap();
        let request = pa(P::CredentialSetRequest {
            account_id: "acc-key".to_string(),
            material: "sk-the-same".to_string(),
        });

        let message_key = |body: MessageBody| match body {
            MessageBody::ProviderAccountBody(P::AccountOpAck {
                ok, message_key, ..
            }) => {
                assert!(ok);
                message_key
            }
            other => panic!("expected an ack, got {other:?}"),
        };

        assert_eq!(
            message_key(provider_account_dispatch(&request, &admin).await.unwrap()),
            None,
            "the first key is stored"
        );
        assert_eq!(
            message_key(provider_account_dispatch(&request, &admin).await.unwrap()),
            Some("agent_accounts.credential_unchanged".to_string())
        );
        assert_eq!(
            store::credential_summary(&admin.state.db, "acc-key")
                .unwrap()
                .expect("a credential is stored")
                .revision,
            1,
            "an identical key must not bump the revision"
        );

        // A different key IS a rotation and moves the revision on.
        provider_account_dispatch(
            &pa(P::CredentialSetRequest {
                account_id: "acc-key".to_string(),
                material: "sk-rotated".to_string(),
            }),
            &admin,
        )
        .await
        .unwrap();
        assert_eq!(
            store::credential_summary(&admin.state.db, "acc-key")
                .unwrap()
                .unwrap()
                .revision,
            2
        );
    }

    /// Installing a runtime changes what the whole node can run, so it is not a
    /// decision any session with an account may take.
    #[tokio::test]
    async fn installing_a_runtime_is_an_administrators_decision() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        let alice = second_session(&admin, Some("user"), ALICE);
        let local = admin.state.local_node_id.to_string();
        for request in [
            pa(P::RuntimeInstallRequest {
                node_id: local.clone(),
                engine_id: "codex".to_string(),
            }),
            pa(P::RuntimeUninstallRequest {
                node_id: local.clone(),
                engine_id: "codex".to_string(),
            }),
        ] {
            let error = provider_account_dispatch(&request, &alice)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{request:?} answered {error:?}"
            );
        }
    }

    /// A01's "5 · 3 osoby": the session count and the people behind it are two
    /// numbers, and three sessions of two people must not read as three people.
    #[tokio::test]
    async fn the_account_list_counts_sessions_and_the_people_holding_them() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-people", "global", None);
        for (session_id, user) in [("s1", ALICE), ("s2", ALICE), ("s3", BOB)] {
            store::upsert_session(
                &admin.state.db,
                &crate::provider_accounts::SessionRecord {
                    account_id: "acc-people".into(),
                    session_id: session_id.into(),
                    user_id: user.into(),
                    agent_id: None,
                    workspace_id: None,
                    node_id: admin.state.local_node_id.to_string(),
                    vendor_session_id: None,
                    started_at: String::new(),
                    last_used_at: None,
                },
            )
            .unwrap();
        }
        let body = provider_account_dispatch(
            &pa(P::AccountListRequest {
                engine_id: None,
                scope: None,
                query: None,
            }),
            &admin,
        )
        .await
        .unwrap();
        let MessageBody::ProviderAccountBody(P::AccountListResponse { accounts, .. }) = body else {
            panic!("expected an account list");
        };
        let account = accounts
            .iter()
            .find(|account| account.account_id == "acc-people")
            .expect("the seeded account is listed");
        assert_eq!((account.session_count, account.session_user_count), (3, 2));
    }

    /// A03 lists what is OPEN. Revoking a session on this node removes the row
    /// before the CLI is asked to go away — a session left in the list after its
    /// process is gone is an invitation to close it twice — and a session that
    /// was never there is the same refusal as a missing account.
    #[tokio::test]
    async fn revoking_a_session_takes_it_off_the_list() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-sessions", "global", None);
        store::upsert_session(
            &admin.state.db,
            &crate::provider_accounts::SessionRecord {
                account_id: "acc-sessions".into(),
                session_id: "s1".into(),
                user_id: ALICE.into(),
                agent_id: None,
                workspace_id: None,
                node_id: admin.state.local_node_id.to_string(),
                vendor_session_id: None,
                started_at: String::new(),
                last_used_at: None,
            },
        )
        .unwrap();

        let listed = |body: MessageBody| match body {
            MessageBody::ProviderAccountBody(P::SessionListResponse { sessions }) => sessions
                .into_iter()
                .map(|session| session.session_id)
                .collect::<Vec<_>>(),
            other => panic!("expected a session list, got {other:?}"),
        };
        let list = pa(P::SessionListRequest {
            account_id: "acc-sessions".to_string(),
        });
        assert_eq!(
            listed(provider_account_dispatch(&list, &admin).await.unwrap()),
            vec!["s1".to_string()]
        );

        let revoke = pa(P::SessionRevokeRequest {
            account_id: "acc-sessions".to_string(),
            session_id: "s1".to_string(),
        });
        provider_account_dispatch(&revoke, &admin).await.unwrap();
        assert!(listed(provider_account_dispatch(&list, &admin).await.unwrap()).is_empty());
        assert_eq!(
            provider_account_dispatch(&revoke, &admin)
                .await
                .unwrap_err()
                .code,
            ProtocolErrorCode::NotFound,
            "a session that is already gone cannot be closed again"
        );
    }

    /// An operation about another node NEVER falls back to running here. It is
    /// signed and forwarded, and on a node whose mesh transport is down that is
    /// where it stops — installing the runtime locally instead would put the
    /// engine on the wrong machine and report success.
    #[tokio::test]
    async fn an_operation_about_another_node_is_forwarded_and_never_run_here() {
        let admin = ctx_for(Some("admin"), BOB);
        assert!(
            admin.state.quic_mesh.is_none(),
            "this fixture has no mesh, which is what makes the refusal observable"
        );
        let account = seed_account(&admin, "acc-remote", "global", None);
        assert_eq!(account.home_node_id, None);
        store::update_account(
            &admin.state.db,
            "acc-remote",
            &crate::provider_accounts::AccountUpdate {
                home_node_id: Some("node-far".to_string()),
                ..Default::default()
            },
            Some(BOB),
        )
        .unwrap();

        for request in [
            pa(P::RuntimeInstallRequest {
                node_id: "node-far".to_string(),
                engine_id: "codex".to_string(),
            }),
            // No `node_id`: the account's home is another node, so the sign-in
            // belongs there and not on the machine showing the dashboard.
            pa(P::LoginStartRequest {
                account_id: "acc-remote".to_string(),
                node_id: None,
            }),
        ] {
            let error = provider_account_dispatch(&request, &admin)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::NotAvailable,
                "{request:?} answered {error:?}"
            );
            assert!(
                error.message.contains("mesh transport"),
                "{request:?} answered {error:?} instead of failing to forward"
            );
        }
        assert!(
            store::engine_state(&admin.state.db, &admin.state.local_node_id, "codex")
                .unwrap()
                .is_none(),
            "a remote install must leave this node's matrix untouched"
        );
    }

    /// A poll is answered from what THIS node knows about the sign-in: its own
    /// running flows, or one it started on a peer for this person. An id it has
    /// no record of is `NotFound` — the same answer as somebody else's id and as
    /// one that never existed, because an id that named a node was otherwise a
    /// way to have this node address any trusted peer, one guess at a time.
    #[tokio::test]
    async fn a_poll_is_never_routed_by_the_node_named_in_the_identifier() {
        let admin = ctx_for(Some("admin"), BOB);
        for login_id in [
            "not-a-login",
            "some-peer-node:11111111-2222-3333-4444-555555555555",
        ] {
            for request in [
                P::LoginStatusRequest {
                    login_id: login_id.to_string(),
                },
                P::LoginInputRequest {
                    login_id: login_id.to_string(),
                    value: "123456".to_string(),
                },
                P::LoginCancelRequest {
                    login_id: login_id.to_string(),
                },
            ] {
                let error = provider_account_dispatch(&pa(request), &admin)
                    .await
                    .unwrap_err();
                assert_eq!(error.code, ProtocolErrorCode::NotFound, "{error:?}");
            }
        }
    }

    /// The node matrix toggle decides where an account's credential may live,
    /// and a sign-in is what puts one there. The refusal comes before the
    /// runtime is touched, and it is a policy answer rather than an environment
    /// one: nothing about the node needs fixing, somebody decided it.
    #[tokio::test]
    async fn a_sign_in_is_refused_on_a_node_that_does_not_receive_accounts() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-shared", "global", None);

        let request = pa(P::LoginStartRequest {
            account_id: "acc-shared".to_string(),
            node_id: None,
        });
        let error = provider_account_dispatch(&request, &admin)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied, "{error:?}");
        assert_eq!(
            error.message,
            crate::services::agent_runtime::NOT_RECEIVING_ACCOUNTS
        );

        node_receives_accounts(&admin);
        let error = provider_account_dispatch(&request, &admin)
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ProtocolErrorCode::NotAvailable,
            "with the decision made, what is left is the missing runtime: {error:?}"
        );
    }

    /// The paste path is the `api_key` twin of a sign-in: both write the
    /// credential onto THIS disk and both claim the home, so both pass the same
    /// node gate. The paste used to reach `mint_credential` without it, which
    /// homed the account on a node the fleet refuses to let hold it — the next
    /// reconcile purged the only copy and left a home with no credential on it.
    /// What the refusal must leave behind is the state BEFORE the paste: no
    /// material, no home, and no audit row claiming a move that never happened.
    #[tokio::test]
    async fn an_api_key_is_refused_on_a_node_that_does_not_receive_accounts() {
        let admin = ctx_for(Some("admin"), BOB);
        store::create_account(
            &admin.state.db,
            &NewAccount {
                account_id: "acc-key".to_string(),
                org_id: ORG.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Key".to_string(),
                scope: "global".to_string(),
                owner_user_id: None,
                credential_kind: "api_key".to_string(),
                created_by: BOB.to_string(),
            },
        )
        .unwrap();
        // `claim_home_tx` reads this node's id from `settings`, not from the
        // state, so without the row the home assertions below would hold
        // vacuously — nothing would be homed whether the paste was refused or
        // not, and the test would prove nothing.
        admin
            .state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    crate::db::repository::LOCAL_NODE_ID_SETTING,
                    admin.state.local_node_id.as_ref()
                ],
            )
            .unwrap();

        let request = pa(P::CredentialSetRequest {
            account_id: "acc-key".to_string(),
            material: "sk-pasted".to_string(),
        });
        let error = provider_account_dispatch(&request, &admin)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied, "{error:?}");
        assert_eq!(
            error.message,
            crate::services::agent_runtime::NOT_RECEIVING_ACCOUNTS
        );

        assert!(
            store::credential_summary(&admin.state.db, "acc-key")
                .unwrap()
                .is_none(),
            "the refused paste minted a credential"
        );
        let account = store::get_account(&admin.state.db, "acc-key")
            .unwrap()
            .expect("the account");
        assert_eq!(account.home_node_id, None, "the refused paste claimed a home");
        assert_eq!(
            audit_rows(&admin, "provider_account.home_moved"),
            0,
            "the refused paste logged a move that never happened"
        );
        assert_eq!(
            audit_rows(&admin, "provider_account.credential_set"),
            0,
            "the refused paste logged a credential set"
        );

        // The same paste, once the node is allowed to hold accounts, is the
        // ordinary write this gate sits in front of.
        node_receives_accounts(&admin);
        provider_account_dispatch(&request, &admin).await.unwrap();
        assert_eq!(
            store::credential_summary(&admin.state.db, "acc-key")
                .unwrap()
                .expect("a credential is stored")
                .revision,
            1
        );
        assert_eq!(
            store::get_account(&admin.state.db, "acc-key")
                .unwrap()
                .expect("the account")
                .home_node_id,
            Some(admin.state.local_node_id.to_string()),
            "an allowed paste homes the account on the node that stored it"
        );
    }

    #[tokio::test]
    async fn a_response_variant_sent_as_a_request_is_a_protocol_error() {
        let admin = ctx_for(Some("admin"), BOB);
        let error = provider_account_dispatch(
            &pa(P::AccountListResponse {
                accounts: Vec::new(),
                engines: Vec::new(),
            }),
            &admin,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(
            error.message.contains("AccountListResponse"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn every_registered_variant_reaches_this_family() {
        for meta in inventory::iter::<crate::dispatch::HandlerMeta> {
            if !meta.variant_name.starts_with("ProviderAccount") {
                continue;
            }
            assert_eq!(
                meta.required_auth,
                crate::dispatch::SessionAuthKind::UserSession,
                "{} must not be reachable without a session",
                meta.variant_name
            );
        }
        // Every request variant `variant_name_of` can report must be findable,
        // or a frame the server encodes a name for would answer NotImplemented.
        for request in [
            pa(P::AccountListRequest {
                engine_id: None,
                scope: None,
                query: None,
            }),
            pa(P::AccountGetRequest {
                account_id: "a".to_string(),
            }),
            pa(P::AccountCreateRequest {
                engine_id: "codex".to_string(),
                display_name: "n".to_string(),
                scope: "user".to_string(),
                owner_user_id: None,
                credential_kind: "api_key".to_string(),
            }),
            pa(P::AccountUpdateRequest {
                account_id: "a".to_string(),
                display_name: None,
                status: None,
                max_sessions: None,
            }),
            pa(P::AccountDeleteRequest {
                account_id: "a".to_string(),
            }),
            pa(P::CredentialSetRequest {
                account_id: "a".to_string(),
                material: "m".to_string(),
            }),
            pa(P::CredentialClearRequest {
                account_id: "a".to_string(),
            }),
            pa(P::GrantsSetRequest {
                account_id: "a".to_string(),
                grants: Vec::new(),
            }),
            pa(P::SessionListRequest {
                account_id: "a".to_string(),
            }),
            pa(P::MyAccountListRequest { engine_id: None }),
            pa(P::RuntimeListRequest {}),
            pa(P::RuntimeSetReceivesAccountsRequest {
                node_id: "n".to_string(),
                enabled: true,
            }),
            pa(P::LoginStartRequest {
                account_id: "a".to_string(),
                node_id: None,
            }),
            pa(P::LoginInputRequest {
                login_id: "l".to_string(),
                value: "v".to_string(),
            }),
            pa(P::LoginStatusRequest {
                login_id: "l".to_string(),
            }),
            pa(P::LoginCancelRequest {
                login_id: "l".to_string(),
            }),
            pa(P::SessionRevokeRequest {
                account_id: "a".to_string(),
                session_id: "s".to_string(),
            }),
            pa(P::RuntimeInstallRequest {
                node_id: "n".to_string(),
                engine_id: "codex".to_string(),
            }),
            pa(P::RuntimeUninstallRequest {
                node_id: "n".to_string(),
                engine_id: "codex".to_string(),
            }),
        ] {
            let name = crate::dispatch::variant_name_of(&request);
            assert!(
                inventory::iter::<crate::dispatch::HandlerMeta>
                    .into_iter()
                    .any(|meta| meta.variant_name == name),
                "'{name}' is reported by variant_name_of but registered nowhere"
            );
        }
    }

    #[tokio::test]
    async fn a_users_own_list_says_what_they_may_do_with_each_account() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        seed_account(&admin, "acc-mine", "user", Some(ALICE));
        seed_account(&admin, "acc-shared", "global", None);
        store::set_grants(
            &admin.state.db,
            "acc-shared",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: ALICE.to_string(),
            }],
            BOB,
        )
        .unwrap();
        let alice = second_session(&admin, Some("user"), ALICE);

        let MessageBody::ProviderAccountBody(P::MyAccountListResponse { accounts }) =
            provider_account_dispatch(&pa(P::MyAccountListRequest { engine_id: None }), &alice)
                .await
                .unwrap()
        else {
            panic!("expected my account list");
        };
        let mine = accounts
            .iter()
            .find(|a| a.account_id == "acc-mine")
            .unwrap();
        let shared = accounts
            .iter()
            .find(|a| a.account_id == "acc-shared")
            .unwrap();
        assert!(mine.can_login && mine.can_delete);
        assert!(
            !shared.can_login && !shared.can_delete,
            "a grant is permission to use, not to manage"
        );
        assert!(mine.last_used_at.is_none());
    }

    /// Somebody else's PERSONAL account does not exist for a plain user — not
    /// for reading it, not for deleting it, and not for putting a key in it.
    #[tokio::test]
    async fn another_users_personal_account_is_invisible_to_a_plain_user() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        seed_user(&admin, BOB, "Bob");
        seed_account(&admin, "acc-bob", "user", Some(BOB));
        let alice = second_session(&admin, Some("user"), ALICE);

        for request in [
            pa(P::AccountGetRequest {
                account_id: "acc-bob".to_string(),
            }),
            pa(P::AccountDeleteRequest {
                account_id: "acc-bob".to_string(),
            }),
            pa(P::CredentialSetRequest {
                account_id: "acc-bob".to_string(),
                material: "sk-test".to_string(),
            }),
        ] {
            let error = provider_account_dispatch(&request, &alice)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::NotFound,
                "{request:?} must not reveal that the account exists"
            );
        }
        assert!(store::get_account(&admin.state.db, "acc-bob")
            .unwrap()
            .is_some());
    }

    /// A grantee of a SHARED account may use it; every administration verb on it
    /// is a policy refusal, and none of them changes anything.
    #[tokio::test]
    async fn a_grantee_cannot_administer_the_shared_account_they_use() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, ALICE, "Alice");
        let account = seed_account(&admin, "acc-shared", "global", None);
        store::set_grants(
            &admin.state.db,
            "acc-shared",
            &[GrantInput {
                subject_type: "user".to_string(),
                subject_id: ALICE.to_string(),
            }],
            BOB,
        )
        .unwrap();
        store::mint_credential(
            &admin.state.db,
            &admin.state.settings_cipher,
            "acc-shared",
            "key-one",
            &CredentialMeta::default(),
        )
        .unwrap();
        let alice = second_session(&admin, Some("user"), ALICE);

        for request in [
            pa(P::CredentialSetRequest {
                account_id: "acc-shared".to_string(),
                material: "sk-mine".to_string(),
            }),
            pa(P::CredentialClearRequest {
                account_id: "acc-shared".to_string(),
            }),
            pa(P::AccountUpdateRequest {
                account_id: "acc-shared".to_string(),
                display_name: Some("Taken over".to_string()),
                status: None,
                max_sessions: None,
            }),
            pa(P::SessionListRequest {
                account_id: "acc-shared".to_string(),
            }),
        ] {
            let error = provider_account_dispatch(&request, &alice)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{request:?} answered {error:?}"
            );
        }
        let after = store::get_account(&admin.state.db, "acc-shared")
            .unwrap()
            .unwrap();
        assert_eq!(after.display_name, account.display_name);
        assert_eq!(
            store::credential_summary(&admin.state.db, "acc-shared")
                .unwrap()
                .unwrap()
                .revision,
            1,
            "nothing the grantee sent touched the credential"
        );
    }

    /// An administrator administers THEIR organisation. Another org's account is
    /// as invisible to them as it is to a stranger, and they cannot grant access
    /// to it.
    #[tokio::test]
    async fn an_administrator_reaches_only_their_own_organisation() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_user(&admin, BOB, "Bob");
        seed_org(&admin, "org-other");
        seed_account(&admin, "acc-ours", "global", None);
        seed_account_in_org(&admin, "acc-theirs", "global", None, "org-other");

        assert_eq!(
            listed(
                &provider_account_dispatch(
                    &pa(P::AccountListRequest {
                        engine_id: None,
                        scope: None,
                        query: None
                    }),
                    &admin
                )
                .await
                .unwrap()
            ),
            vec!["acc-ours"]
        );
        for request in [
            pa(P::AccountGetRequest {
                account_id: "acc-theirs".to_string(),
            }),
            pa(P::GrantsSetRequest {
                account_id: "acc-theirs".to_string(),
                grants: vec![GrantEntry {
                    subject_type: "user".to_string(),
                    subject_id: BOB.to_string(),
                    display_name: String::new(),
                    member_count: None,
                    granted_by_name: None,
                    granted_at: String::new(),
                }],
            }),
        ] {
            assert_eq!(
                provider_account_dispatch(&request, &admin)
                    .await
                    .unwrap_err()
                    .code,
                ProtocolErrorCode::NotFound,
                "{request:?} must answer like a missing account"
            );
        }
        assert!(store::list_grants(&admin.state.db, "acc-theirs")
            .unwrap()
            .is_empty());

        // The other org's own administrator sees exactly the mirror image.
        let other = session_in_org(&admin, Some("admin"), BOB, "org-other");
        assert_eq!(
            listed(
                &provider_account_dispatch(
                    &pa(P::AccountListRequest {
                        engine_id: None,
                        scope: None,
                        query: None
                    }),
                    &other
                )
                .await
                .unwrap()
            ),
            vec!["acc-theirs"]
        );
    }

    /// The credential and the name of a PERSONAL account are the owner's. An
    /// administrator may take such an account out of service, which is a
    /// different power from signing in as its owner.
    #[tokio::test]
    async fn an_administrator_may_disable_a_personal_account_but_not_hold_its_credential() {
        let admin = ctx_for(Some("admin"), BOB);
        node_receives_accounts(&admin);
        seed_user(&admin, ALICE, "Alice");
        store::create_account(
            &admin.state.db,
            &NewAccount {
                account_id: "acc-alice".to_string(),
                org_id: ORG.to_string(),
                engine_id: "codex".to_string(),
                display_name: "Alice Codex".to_string(),
                scope: "user".to_string(),
                owner_user_id: Some(ALICE.to_string()),
                credential_kind: "api_key".to_string(),
                created_by: ALICE.to_string(),
            },
        )
        .unwrap();

        for request in [
            pa(P::CredentialSetRequest {
                account_id: "acc-alice".to_string(),
                material: "sk-admin".to_string(),
            }),
            pa(P::CredentialClearRequest {
                account_id: "acc-alice".to_string(),
            }),
            pa(P::AccountUpdateRequest {
                account_id: "acc-alice".to_string(),
                display_name: Some("Konto Alicji".to_string()),
                status: None,
                max_sessions: None,
            }),
        ] {
            let error = provider_account_dispatch(&request, &admin)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{request:?} answered {error:?}"
            );
        }
        assert!(store::credential_summary(&admin.state.db, "acc-alice")
            .unwrap()
            .is_none());

        // The owner does all three.
        let alice = second_session(&admin, Some("user"), ALICE);
        for request in [
            pa(P::CredentialSetRequest {
                account_id: "acc-alice".to_string(),
                material: "sk-alice".to_string(),
            }),
            pa(P::AccountUpdateRequest {
                account_id: "acc-alice".to_string(),
                display_name: Some("Moje Codex".to_string()),
                status: None,
                max_sessions: None,
            }),
            pa(P::CredentialClearRequest {
                account_id: "acc-alice".to_string(),
            }),
        ] {
            provider_account_dispatch(&request, &alice).await.unwrap();
        }
        assert_eq!(
            store::get_account(&admin.state.db, "acc-alice")
                .unwrap()
                .unwrap()
                .display_name,
            "Moje Codex"
        );

        // …and the administrator still disables and deletes it.
        provider_account_dispatch(
            &pa(P::AccountUpdateRequest {
                account_id: "acc-alice".to_string(),
                display_name: None,
                status: Some("disabled".to_string()),
                max_sessions: None,
            }),
            &admin,
        )
        .await
        .unwrap();
        assert_eq!(
            store::get_account(&admin.state.db, "acc-alice")
                .unwrap()
                .unwrap()
                .status,
            "disabled"
        );
        provider_account_dispatch(
            &pa(P::AccountDeleteRequest {
                account_id: "acc-alice".to_string(),
            }),
            &admin,
        )
        .await
        .unwrap();
        assert!(store::get_account(&admin.state.db, "acc-alice")
            .unwrap()
            .is_none());
    }

    /// The flag may only be set on a node the matrix lists — a decision about
    /// anything else is invisible and undoable by nobody.
    #[tokio::test]
    async fn the_receives_accounts_flag_refuses_a_node_the_matrix_does_not_know() {
        let admin = ctx_for(Some("admin"), BOB);
        let error = provider_account_dispatch(
            &pa(P::RuntimeSetReceivesAccountsRequest {
                node_id: "node-that-never-was".to_string(),
                enabled: true,
            }),
            &admin,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
        assert!(store::list_runtime_nodes(&admin.state.db)
            .unwrap()
            .is_empty());
    }
}
