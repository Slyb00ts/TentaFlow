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
    AccountAgentInfo, AccountNodeInfo, AccountSessionInfo, EngineSummary, GrantEntry,
    MyAccountInfo, ProviderAccountInfo, ProviderAccountPayload as P, RuntimeEngineInfo,
    RuntimeNodeInfo,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::agents::{AccountBinding, AgentRuntime};
use crate::db::repository;
use crate::provider_accounts::{
    self as accounts, repository as store, AccountFilter, AccountRecord, AccountUpdate,
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

/// The single place every deferred variant is refused. A stub that answered
/// with an empty result would read as "the login finished and produced
/// nothing", which is the one answer this feature must never give.
fn not_yet(what: &str) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::NotAvailable,
        format!("{what} is not available on this node yet"),
    )
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
    let granted = store::user_may_use_account(
        &ctx.state.db,
        &caller.org_id,
        account_id,
        &caller.user_id,
    )
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
    let agents = repository::list_cli_agents(&ctx.state.db).map_err(|e| internal("agent list", e))?;
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
    credentials: HashMap<String, (i64, Option<String>)>,
    agents: AgentBindings,
}

fn decorate(
    ctx: &HandlerContext,
    accounts_list: &[AccountRecord],
) -> Result<Decorations, ProtocolError> {
    let owner_ids: Vec<String> =
        unique(accounts_list.iter().filter_map(|a| a.owner_user_id.clone()));
    let node_ids: Vec<String> = unique(accounts_list.iter().filter_map(|a| a.home_node_id.clone()));
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
    Ok(Decorations {
        owners,
        nodes: node_names(ctx, &node_ids)?,
        grants: store::grant_counts(&ctx.state.db, &account_ids)
            .map_err(|e| internal("grant counts", e))?,
        sessions: store::session_counts(&ctx.state.db, &account_ids)
            .map_err(|e| internal("session counts", e))?,
        credentials: store::credential_revisions(&ctx.state.db, &account_ids)
            .map_err(|e| internal("credential revisions", e))?,
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
        credential_revision,
        expires_at,
        grant_count: dec.grants.get(&account.account_id).copied().unwrap_or(0),
        session_count: dec.sessions.get(&account.account_id).copied().unwrap_or(0),
        agent_count: dec.agents.count(account),
        updated_at: account.updated_at.clone(),
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
    let update = AccountUpdate {
        display_name: display_name.clone(),
        status: status.clone(),
        plan_label: None,
        home_node_id: None,
    };
    let existed = store::update_account(&ctx.state.db, account_id, &update, Some(&caller.user_id))
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if !existed {
        return Err(not_found());
    }
    Ok(ack(account_id, None))
}

fn account_delete(ctx: &HandlerContext, account_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_owner_or_admin(&account)?;
    let removed = store::delete_account(&ctx.state.db, account_id, Some(&caller.user_id))
        .map_err(|e| internal("account delete", e))?;
    if !removed {
        return Err(not_found());
    }
    Ok(ack(account_id, None))
}

fn ack(account_id: &str, message_key: Option<&str>) -> MessageBody {
    pa(P::AccountOpAck {
        account_id: Some(account_id.to_string()),
        ok: true,
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
        CredentialWrite::Applied { .. } => None,
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
    Ok(ack(account_id, message_key))
}

fn credential_clear(ctx: &HandlerContext, account_id: &str) -> Result<MessageBody, ProtocolError> {
    let caller = caller(ctx)?;
    let account = visible_account(ctx, &caller, account_id)?;
    caller.require_account_keeper(&account, "removing a credential")?;
    let cleared = store::clear_credential(&ctx.state.db, account_id, Some(&caller.user_id))
        .map_err(|e| internal("credential clear", e))?;
    Ok(ack(
        account_id,
        (!cleared).then_some("agent_accounts.credential_absent"),
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
    let users = repository::lookup_user_names(&ctx.state.db, &user_ids)
        .map_err(|e| internal("grant user names", e))?;
    let groups = repository::lookup_group_info(&ctx.state.db, &group_ids)
        .map_err(|e| internal("grant group names", e))?;
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
                "group" => match groups.get(&grant.subject_id) {
                    Some((name, members)) => (
                        name.clone(),
                        Some(u32::try_from(*members).unwrap_or(u32::MAX)),
                    ),
                    None => (grant.subject_id.clone(), None),
                },
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
    Ok(pa(P::MyAccountListResponse {
        accounts: rows
            .into_iter()
            .map(|account| {
                let is_mine = account.is_owned_by(&caller.user_id);
                let supports_login = accounts::engine(&account.engine_id)
                    .is_some_and(|engine| engine.supports_login);
                MyAccountInfo {
                    // Only the person an account belongs to can sign it in, and
                    // only they can throw it away. A grant makes a shared
                    // account usable, not manageable.
                    can_login: is_mine && supports_login,
                    can_delete: is_mine,
                    last_used_at: last_used.get(&account.account_id).cloned(),
                    account_id: account.account_id,
                    engine_id: account.engine_id,
                    display_name: account.display_name,
                    scope: account.scope,
                    status: account.status,
                    provider_subject: account.provider_subject,
                    plan_label: account.plan_label,
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
                sandbox_capable: (node_id == &local_id).then(|| {
                    crate::code_studio::process_sandbox::ProcessSandbox::check_available().is_ok()
                }),
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
    store::set_receives_accounts(&ctx.state.db, node_id, enabled, Some(&caller.user_id))
        .map_err(|e| internal("runtime flag", e))?;
    Ok(pa(P::AccountOpAck {
        account_id: None,
        ok: true,
        message_key: None,
    }))
}

// =============================================================================
// Dispatcher
// =============================================================================

#[handler(variant = "ProviderAccountBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn provider_account_dispatch(
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
        } => account_update(ctx, account_id, display_name, status),
        P::AccountDeleteRequest { account_id } => account_delete(ctx, account_id),
        P::CredentialSetRequest {
            account_id,
            material,
        } => credential_set(ctx, account_id, material),
        P::CredentialClearRequest { account_id } => credential_clear(ctx, account_id),
        P::GrantsSetRequest { account_id, grants } => grants_set(ctx, account_id, grants),
        P::SessionListRequest { account_id } => session_list(ctx, account_id),
        P::MyAccountListRequest { engine_id } => my_account_list(ctx, engine_id),
        P::RuntimeListRequest {} => runtime_list(ctx),
        P::RuntimeSetReceivesAccountsRequest { node_id, enabled } => {
            runtime_set_receives_accounts(ctx, node_id, *enabled)
        }

        // Deferred to the packages that own them, and refused rather than
        // faked: the login exchange and credential replication are WP3, the
        // CLI installation is WP4. Each needs a node-side executor that does
        // not exist yet, and an empty answer would look like it succeeded.
        P::LoginStartRequest { .. }
        | P::LoginInputRequest { .. }
        | P::LoginStatusRequest { .. }
        | P::LoginCancelRequest { .. } => Err(not_yet("signing in to a provider account")),
        P::SessionRevokeRequest { .. } => Err(not_yet("revoking a provider session")),
        P::RuntimeInstallRequest { .. } | P::RuntimeUninstallRequest { .. } => {
            Err(not_yet("installing an agent CLI on a node"))
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
// The deferred variants are registered too: their handler answers
// `NotAvailable`, which is a decision this node makes, not a frame it fails to
// recognise. Leaving them out would report `NotImplemented` and make a future
// build look like a downgrade.
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

    /// Same database, two sessions: the ACL is a property of the caller, so
    /// every test needs both views of one fixture.
    fn second_session(base: &HandlerContext, role: Option<&str>, user_id: &str) -> HandlerContext {
        let mut ctx = crate::dispatch::test_handler_context(
            base.state.clone(),
            role,
            Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: ORG.to_string(),
                role_id: "role-1".to_string(),
                permissions: Default::default(),
            }),
        );
        ctx.correlation_id = base.correlation_id + 1;
        ctx
    }

    fn seed_user(ctx: &HandlerContext, id: &str, display: &str) {
        ctx.state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT INTO user_accounts (id, username, password_hash, is_active, \
                 must_change_password, role, display_name) \
                 VALUES (?1, ?2, 'x', 1, 0, 'user', ?3)",
                rusqlite::params![id, display.to_lowercase(), display],
            )
            .unwrap();
    }

    fn seed_account(
        ctx: &HandlerContext,
        account_id: &str,
        scope: &str,
        owner: Option<&str>,
    ) -> AccountRecord {
        store::create_account(
            &ctx.state.db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: ORG.to_string(),
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

    fn listed(body: &MessageBody) -> Vec<String> {
        match body {
            MessageBody::ProviderAccountBody(P::AccountListResponse { accounts, .. }) => {
                accounts.iter().map(|a| a.account_id.clone()).collect()
            }
            other => panic!("expected an account list, got {other:?}"),
        }
    }

    #[test]
    fn a_non_admin_sees_only_the_accounts_they_own_or_were_granted() {
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
        let mut mine = listed(&provider_account_dispatch(&request, &alice).unwrap());
        mine.sort();
        assert_eq!(mine, vec!["acc-mine", "acc-shared"]);

        let mut all = listed(&provider_account_dispatch(&request, &admin).unwrap());
        all.sort();
        assert_eq!(all, vec!["acc-mine", "acc-secret", "acc-shared"]);
    }

    #[test]
    fn an_account_the_caller_may_not_see_answers_exactly_like_a_missing_one() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-secret", "global", None);
        let alice = second_session(&admin, Some("user"), ALICE);

        let hidden = provider_account_dispatch(
            &pa(P::AccountGetRequest {
                account_id: "acc-secret".to_string(),
            }),
            &alice,
        )
        .unwrap_err();
        let missing = provider_account_dispatch(
            &pa(P::AccountGetRequest {
                account_id: "acc-nope".to_string(),
            }),
            &alice,
        )
        .unwrap_err();
        assert_eq!(hidden.code, ProtocolErrorCode::NotFound);
        assert_eq!(hidden, missing, "the refusal must not distinguish the two");
    }

    #[test]
    fn a_grant_lets_a_user_run_an_account_but_not_administer_it() {
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
            let error = provider_account_dispatch(&request, &alice).unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{request:?} must be refused as a policy decision, not hidden"
            );
        }
    }

    #[test]
    fn a_user_account_is_minted_for_its_caller_and_a_shared_one_needs_an_admin() {
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
        .unwrap_err();
        assert_eq!(for_bob.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn an_api_key_is_refused_on_an_account_that_signs_in_through_the_provider() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-login", "global", None);
        let error = provider_account_dispatch(
            &pa(P::CredentialSetRequest {
                account_id: "acc-login".to_string(),
                material: "sk-test".to_string(),
            }),
            &admin,
        )
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(error.message.contains("provider"), "{}", error.message);
        assert!(store::credential_summary(&admin.state.db, "acc-login")
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_node_matrix_is_administration_and_always_contains_this_node() {
        let admin = ctx_for(Some("admin"), BOB);
        let alice = second_session(&admin, Some("user"), ALICE);
        assert_eq!(
            provider_account_dispatch(&pa(P::RuntimeListRequest {}), &alice)
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
        .unwrap();
        let MessageBody::ProviderAccountBody(P::RuntimeListResponse { nodes }) =
            provider_account_dispatch(&pa(P::RuntimeListRequest {}), &admin).unwrap()
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

    #[test]
    fn every_variant_of_a_later_package_refuses_instead_of_pretending() {
        let admin = ctx_for(Some("admin"), BOB);
        seed_account(&admin, "acc-login", "global", None);
        for request in [
            pa(P::LoginStartRequest {
                account_id: "acc-login".to_string(),
                node_id: None,
            }),
            pa(P::LoginInputRequest {
                login_id: "l1".to_string(),
                value: "123456".to_string(),
            }),
            pa(P::LoginStatusRequest {
                login_id: "l1".to_string(),
            }),
            pa(P::LoginCancelRequest {
                login_id: "l1".to_string(),
            }),
            pa(P::SessionRevokeRequest {
                account_id: "acc-login".to_string(),
                session_id: "s1".to_string(),
            }),
            pa(P::RuntimeInstallRequest {
                node_id: "n1".to_string(),
                engine_id: "codex".to_string(),
            }),
            pa(P::RuntimeUninstallRequest {
                node_id: "n1".to_string(),
                engine_id: "codex".to_string(),
            }),
        ] {
            let error = provider_account_dispatch(&request, &admin).unwrap_err();
            assert_eq!(
                error.code,
                ProtocolErrorCode::NotAvailable,
                "{request:?} answered {error:?}"
            );
        }
    }

    #[test]
    fn a_response_variant_sent_as_a_request_is_a_protocol_error() {
        let admin = ctx_for(Some("admin"), BOB);
        let error = provider_account_dispatch(
            &pa(P::AccountListResponse {
                accounts: Vec::new(),
                engines: Vec::new(),
            }),
            &admin,
        )
        .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(
            error.message.contains("AccountListResponse"),
            "{}",
            error.message
        );
    }

    #[test]
    fn every_registered_variant_reaches_this_family() {
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

    #[test]
    fn a_users_own_list_says_what_they_may_do_with_each_account() {
        let admin = ctx_for(Some("admin"), BOB);
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
}
