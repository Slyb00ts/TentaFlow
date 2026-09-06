// =============================================================================
// File: dispatch/tentavm.rs — the TentaVM request family (plan §16, §3.1).
//
//       TentaVM is MULTI-INSTANCE: one node can host several environments, and
//       every request names the one it is talking to (`instance_id`). The gate
//       is therefore `require_app_instance_permission`, never the package-level
//       one — with two environments installed, resolving the instance from the
//       package id picks one arbitrarily and would answer with another
//       environment's data.
//
//       Two kinds of request live here and they route differently:
//       - REGISTRY reads (hosts, jobs, settings, the dashboard summary) answer
//         wherever they land. The registry replicates, so every node holds the
//         same rows and a round trip over the mesh would buy nothing;
//       - HARDWARE and NODE-LOCAL work (probing a host, reading a job's log)
//         only exists on the node that owns the row. `route_to_owner` sends
//         those to that node through `app_route`, which re-runs the whole
//         pipeline there — including this gate — so the owner never trusts the
//         forwarding node's authority.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::tentavm::{
    TentaVmPayload as P, VmCapability, VmEngine, VmHost, VmInboxItem, VmJob, VmJobLogLine,
    VmJobStep, VmSummary, VmText, VmTextParam,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::db::DbPool;

const PERM_READ: &str = "vm.read";
const PERM_CREATE: &str = "vm.create";
const PERM_HOSTS_MANAGE: &str = "vm.hosts.manage";
const PERM_ADMIN: &str = "vm.admin";

/// How many inbox items one dashboard tile carries. `VmSummary.inbox_total`
/// reports the real count, so the browser can say "3 of 47" instead of
/// believing a truncated list is all there is.
const INBOX_PAGE: usize = 20;

fn tv(body: P) -> MessageBody {
    MessageBody::TentaVmBody(body)
}

fn internal(scope: &str, error: impl std::fmt::Display) -> ProtocolError {
    tracing::warn!(scope, error = %error, "tentavm error");
    ProtocolError::internal(format!("tentavm {scope} failed"))
}

fn text(key: &str) -> VmText {
    VmText {
        key: key.to_string(),
        params: Vec::new(),
    }
}

fn text_with(key: &str, params: &[(&str, &str)]) -> VmText {
    VmText {
        key: key.to_string(),
        params: params
            .iter()
            .map(|(name, value)| VmTextParam {
                name: (*name).to_string(),
                value: (*value).to_string(),
            })
            .collect(),
    }
}

/// The caller's environment after the matrix check. `org_id` and `user_id` come
/// from the request context, never from the request body: the body says WHICH
/// environment, the session says WHO.
struct Gate {
    instance_id: String,
    org_id: String,
    user_id: String,
}

fn gate(
    ctx: &HandlerContext,
    instance_id: &str,
    permission: &str,
) -> Result<Gate, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    super::app_gate::require_app_instance_permission(
        ctx,
        crate::tentavm::PACKAGE_ID,
        instance_id,
        permission,
    )?;
    Ok(Gate {
        instance_id: instance_id.to_string(),
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
    })
}

/// The same gate, satisfied by ANY of several permissions.
///
/// §15 gives the environment administrator authority over everything the
/// environment holds ("Admin środowiska ma `manage` wszędzie"), but the
/// permission matrix does not model that as an implication: `vm.admin` is a
/// permission BESIDE `vm.hosts.manage`, not above it, and the checker will
/// happily refuse an administrator who holds only the first. A screen an
/// administrator must be able to open therefore names both here, instead of
/// resting on an implication nothing implements.
///
/// Only a `PolicyDenied` moves on to the next permission. "The environment is
/// not installed" and "the environment is disabled" are answers about the
/// environment, not about this caller, and retrying them under another
/// permission would replace them with a misleading refusal.
fn gate_any(
    ctx: &HandlerContext,
    instance_id: &str,
    permissions: &[&str],
) -> Result<Gate, ProtocolError> {
    let mut last = ProtocolError::new(
        ProtocolErrorCode::PolicyDenied,
        "no permission was named for this operation",
    );
    for permission in permissions {
        match gate(ctx, instance_id, permission) {
            Ok(g) => return Ok(g),
            Err(error) if error.code == ProtocolErrorCode::PolicyDenied => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

// =============================================================================
// Routing to the owner
// =============================================================================

/// The node that owns `host_id`, or None when this node owns it. A host row
/// carries `owner_node_id` precisely so that hardware work has one address;
/// reading it here means the browser never has to know the fleet topology.
///
/// A host that is not in the registry is reported as missing rather than
/// routed anywhere — an unknown id must not become an unroutable request that
/// dies on a timeout.
fn owner_of_host(
    db: &DbPool,
    org_id: &str,
    host_id: &str,
    local_node_id: &str,
) -> Result<Option<String>, ProtocolError> {
    let owner: Option<String> = {
        let conn = db.read().map_err(|e| internal("registry", e))?;
        conn.query_row(
            "SELECT owner_node_id FROM vm_hosts WHERE id = ?1 AND org_id = ?2",
            rusqlite::params![host_id, org_id],
            |row| row.get(0),
        )
        .ok()
    };
    // An empty owner is not an address. The column is NOT NULL, so a blank
    // there means the row was written by something that did not know its
    // owner; sending the request "to nobody" would hang on the mesh timeout.
    let owner = owner.filter(|owner| !owner.is_empty()).ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotFound,
            format!("host '{host_id}' is not in this environment"),
        )
    })?;
    Ok((owner != local_node_id).then_some(owner))
}

/// The node that owns `job_id`, or None when this node owns it. The job row
/// replicates but its LOG does not — `vm_job_logs` lives in the instance
/// database of the node that ran the job — so a log read has to travel.
fn owner_of_job(
    db: &DbPool,
    org_id: &str,
    instance_id: &str,
    job_id: &str,
    local_node_id: &str,
) -> Result<Option<String>, ProtocolError> {
    let owner: Option<String> = {
        let conn = db.read().map_err(|e| internal("registry", e))?;
        conn.query_row(
            "SELECT owner_node_id FROM vm_jobs \
             WHERE id = ?1 AND org_id = ?2 AND instance_id = ?3",
            rusqlite::params![job_id, org_id, instance_id],
            |row| row.get(0),
        )
        .ok()
    };
    let owner = owner.filter(|owner| !owner.is_empty()).ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotFound,
            format!("job '{job_id}' is not in this environment"),
        )
    })?;
    Ok((owner != local_node_id).then_some(owner))
}

/// Sends the request body verbatim to `node_id` and returns its answer.
/// The bytes are re-encoded from the decoded body rather than kept from the
/// wire, because a forwarded request must carry exactly what this node
/// understood — a peer that re-encodes differently would be answering a
/// different question than the one this gate approved.
async fn forward(
    ctx: &HandlerContext,
    node_id: &str,
    body: &MessageBody,
) -> Result<MessageBody, ProtocolError> {
    let bytes = tentaflow_protocol::cbor::encode(body)
        .map_err(|e| internal("request encode", e))?;
    super::app_route::forward_to_node(ctx, node_id, bytes).await
}

// =============================================================================
// Registry reads
// =============================================================================

/// One `vm_hosts` row as the dashboard sees it.
///
/// The hardware columns (`os_*`, `cpu_*`, `ram_*`, `storage_*`) are the
/// environment probe's, and `vm_hosts` has no columns for them: they arrive
/// from `local_probe`, this node's own probe cache, and therefore ONLY for the
/// local host. A remote host keeps its zeros — its probe lives in its owner's
/// node-local database and no mechanism carries it here yet (plan §5 puts that
/// on the heartbeat as `PeerVirtInfo`). A zero next to an explicit status is
/// the honest answer; a guess would not be.
fn host_row(
    row: &rusqlite::Row<'_>,
    local_node_id: &str,
    online: &dyn Fn(&str) -> bool,
    your_role: &dyn Fn(&str) -> String,
    local_probe: Option<&crate::tentavm::probe::CachedProbe>,
) -> rusqlite::Result<VmHost> {
    let host_id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let node_id: Option<String> = row.get(2)?;
    let engines_json: String = row.get(7)?;
    let capabilities_json: String = row.get(8)?;
    let status: String = row.get(6)?;
    let is_local = node_id.as_deref() == Some(local_node_id);
    let is_online = is_local || node_id.as_deref().is_some_and(online);
    let mut host = VmHost {
        status_reason: status_reason(&status, is_online),
        online: is_online,
        is_local,
        engines: serde_json::from_str::<Vec<VmEngine>>(&engines_json).unwrap_or_default(),
        capabilities: serde_json::from_str::<Vec<VmCapability>>(&capabilities_json)
            .unwrap_or_default(),
        your_role: your_role(&host_id),
        host_id,
        kind,
        node_id,
        connector_id: row.get(3)?,
        external_ref: row.get(4)?,
        display_name: row.get(5)?,
        status,
        owner_node_id: row.get(9)?,
        owner_epoch: row.get::<_, i64>(10)?.max(0) as u64,
        os_name: String::new(),
        os_version: String::new(),
        arch: String::new(),
        cpu_cores: 0,
        cpu_used_pct: 0.0,
        ram_bytes: 0,
        ram_used_bytes: 0,
        storage_bytes: 0,
        storage_used_bytes: 0,
        guests_total: 0,
        guests_running: 0,
        last_seen_at: None,
        updated_at: row.get(11)?,
    };
    if let (true, Some(cached)) = (host.is_local, local_probe) {
        crate::tentavm::probe::apply_hardware(&mut host, cached);
        // The engine chips and the capability list come from the same probe
        // that wrote the registry row, so this is not a second opinion — it is
        // the SAME measurement with the admin's current consent applied
        // (`probe::cached`). Without it, "Włącz silnik KVM" in D01 would leave
        // the card saying `needs_consent` until the next probe, because
        // `engines_json` froze the verdict at measurement time.
        host.engines = cached.probe.environment.engines.clone();
        host.capabilities = cached.probe.environment.capabilities.clone();
    }
    Ok(host)
}

const HOST_COLUMNS: &str = "id, kind, node_id, connector_id, external_ref, display_name, \
                            status, engines_json, capabilities_json, owner_node_id, \
                            owner_epoch, updated_at";

/// Why a host is in its status, as a key the dashboard translates. An offline
/// host outranks its stored status: the row is what the fleet last agreed on,
/// the connection is what is true now.
fn status_reason(status: &str, online: bool) -> VmText {
    if !online {
        return text("host.offline");
    }
    match status {
        "ready" => text("host.ready"),
        "needs_install" => text("host.needs_install"),
        "unreachable" => text("host.unreachable"),
        other => text_with("host.status", &[("status", other)]),
    }
}

/// Everything the caller may SEE and may DO in this environment, resolved once
/// per request.
///
/// One value, deliberately, because the card and the gate have to say the same
/// sentence. `VmHost.your_role` is what the browser draws the "Twoje
/// uprawnienia" bar from and what decides which buttons the card offers; the
/// gate that refuses those buttons reads `require_role` on the SAME map. Two
/// independent reads of one fact — the report here, the enforcement there — is
/// the exact shape that cost this project three rounds in step 5, and it fails
/// in the worst direction: a card offering an action the executor refuses.
///
/// GROUP grants are read together with user grants because §15 makes a
/// TentaFlow group a subject exactly like a user; reading only the user rows
/// would report "no access" to somebody who has access through their team.
///
/// The environment admin is not a row and cannot be one: `vm.admin` is `manage`
/// on EVERY host, including hosts no grant mentions, and a fresh environment
/// has no grant rows at all.
struct Access {
    /// `vm.admin` — the environment administrator of §15, `manage` everywhere.
    is_env_admin: bool,
    /// The strongest role per host id, user grants and group grants merged.
    roles: std::collections::HashMap<String, String>,
    /// `visibility = 'granted'`: a host the caller has no `view` on is not in
    /// any list, is not counted, and does not exist as far as this session is
    /// concerned. `'all'` is the default and the shipped behaviour.
    hide_ungranted: bool,
}

impl Access {
    fn resolve(ctx: &HandlerContext, g: &Gate) -> Self {
        Access {
            is_env_admin: holds(ctx, &g.instance_id, PERM_ADMIN),
            roles: roles_of(&ctx.state.db, &g.instance_id, &g.org_id, &g.user_id),
            hide_ungranted: hides_ungranted_hosts(
                setting(&ctx.state.db, &g.instance_id, &g.org_id, "visibility").as_deref(),
            ),
        }
    }

    /// What `your_role` reports, and the only definition of the caller's role.
    fn role_of(&self, host_id: &str) -> String {
        if self.is_env_admin {
            return "manage".to_string();
        }
        self.roles.get(host_id).cloned().unwrap_or_default()
    }

    /// Does this environment hide ANY host from this caller?
    ///
    /// `visibility = 'all'` hides nothing, and neither does `vm.admin`: §15
    /// gives the environment administrator `manage` everywhere, and `manage`
    /// contains `view`.
    ///
    /// It is a method rather than the same disjunction written out twice,
    /// because it IS asked twice — once per host id by `can_see`, once per
    /// request by `visible_host_ids` — and a mutation showed that the two
    /// copies could disagree with no test noticing: forcing `can_see` to `true`
    /// left the host LIST and every counter correctly filtered, because they
    /// went through the OTHER copy. Two spellings of one predicate, which is
    /// the figure this step exists to remove.
    fn sees_everything(&self) -> bool {
        !self.hide_ungranted || self.is_env_admin
    }

    /// Whether this host is in the caller's world at all (§7.1: "listy filtrują
    /// hosty bez `view`, gdy `visibility = granted`").
    fn can_see(&self, host_id: &str) -> bool {
        self.sees_everything() || role_rank(&self.role_of(host_id)) >= role_rank("view")
    }

    /// The host ids the caller may see, or `None` when the setting hides
    /// nothing and no restriction applies.
    ///
    /// Built from the map this type already holds rather than from a second
    /// copy of the grant query in SQL: "which hosts can I see" has one
    /// definition, and a `WHERE` clause repeating the subject_kind / group
    /// join would be the second.
    fn visible_host_ids(&self) -> Option<Vec<String>> {
        if self.sees_everything() {
            return None;
        }
        Some(
            self.roles
                .keys()
                .filter(|host_id| self.can_see(host_id))
                .cloned()
                .collect(),
        )
    }

    /// The SQL predicate "`column` names a host this caller may see", plus its
    /// bind values — or `None` when nothing is hidden and no predicate belongs
    /// in the query at all.
    ///
    /// An empty visible set becomes the literal `0`: "the caller may see no
    /// host" is a predicate, and it is the one case where getting it wrong is
    /// silent, because `IN ()` does not parse and an omitted clause shows
    /// everything.
    fn sql_visible(&self, column: &str) -> Option<(String, Vec<String>)> {
        let ids = self.visible_host_ids()?;
        if ids.is_empty() {
            return Some(("0".to_string(), Vec::new()));
        }
        let marks = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        Some((format!("{column} IN ({marks})"), ids))
    }

    /// The caller holds at least `needed` on this host, or a refusal that names
    /// what was missing.
    ///
    /// Callers must have established that the host is VISIBLE first: a host the
    /// caller may not see answers `NotFound`, because `PolicyDenied` on a
    /// hidden host confirms that it exists.
    fn require_role(&self, host_id: &str, needed: &str) -> Result<(), ProtocolError> {
        let held = self.role_of(host_id);
        if role_rank(&held) >= role_rank(needed) {
            return Ok(());
        }
        Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!(
                "this operation needs the '{needed}' grant on host '{host_id}'; \
                 you hold '{}'",
                if held.is_empty() { "none" } else { &held }
            ),
        ))
    }

    /// Does the caller hold `deploy` or better on ANY host?
    ///
    /// §15 makes creating a machine a conjunction — `vm.create` says WHAT,
    /// `deploy` on the target host says WHERE — and `VmSummary.can_create_guest`
    /// is what P00 chooses between "Utwórz maszynę" and "Poproś administratora"
    /// with. Reporting the permission alone puts a create button in front of
    /// somebody every host will refuse, and hides the request button that is the
    /// honest offer for them. There is no target host yet at this point in the
    /// screen, so the question the flag can answer is "anywhere at all".
    fn can_deploy_somewhere(&self) -> bool {
        self.is_env_admin
            || self
                .roles
                .values()
                .any(|role| role_rank(role) >= role_rank("deploy"))
    }

    /// May the caller REWRITE the grant matrix of this host? §15: `manage` on
    /// the host, or `vm.admin`. Reading it needs `vm.hosts.manage` (the gate),
    /// which is why the two are not the same question and `can_edit` is a field
    /// on the answer rather than a second refusal.
    fn can_edit_grants(&self, host_id: &str) -> bool {
        self.is_env_admin || role_rank(&self.role_of(host_id)) >= role_rank("manage")
    }
}

/// Does this stored `visibility` value hide hosts the caller has no grant on?
///
/// Three cases and they are not two:
///
///   * ABSENT — the documented default is `'all'`, so nothing is hidden. A
///     fresh environment has no settings row at all and must not start out
///     hiding every host from everyone.
///   * `'all'` / `'granted'` — the two values the protocol documents, honoured
///     as written.
///   * ANYTHING ELSE — hide. This is the case worth spelling out: `value` in
///     `vm_instance_settings` is free TEXT with no CHECK, and the row is
///     REPLICATED, so a peer can put a word this build does not know into it.
///     `== "granted"` would then read as "hide nothing" and one unknown string
///     would open the whole fleet's host lists. A node that does not understand
///     the policy must not act as though there were none.
///
/// The refusal on the write side (`settings_set`) stops this node from storing
/// such a value; it cannot stop one arriving from a peer, and the `enum_columns`
/// mechanism of the registry cannot express "value ∈ {all, granted} WHEN key =
/// 'visibility'" for a key/value table. So the rule lives here, on the read.
fn hides_ungranted_hosts(stored: Option<&str>) -> bool {
    match stored {
        None | Some("all") => false,
        Some(_) => true,
    }
}

/// The host row's existence AND its visibility in one answer, because for a
/// caller under `visibility = 'granted'` they are the same fact. A host that is
/// hidden must be indistinguishable from a host that is not there — otherwise
/// the refusal itself enumerates the fleet.
fn visible_host(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    host_id: &str,
) -> Result<(), ProtocolError> {
    let missing = || {
        ProtocolError::new(
            ProtocolErrorCode::NotFound,
            format!("host '{host_id}' is not in this environment"),
        )
    };
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM vm_hosts WHERE id = ?1 AND org_id = ?2)",
            rusqlite::params![host_id, g.org_id],
            |row| row.get(0),
        )
        .map_err(|e| internal("registry", e))?;
    if exists && access.can_see(host_id) {
        Ok(())
    } else {
        Err(missing())
    }
}

/// The caller's strongest grant on each host they have one for, USER grants and
/// GROUP grants together. The only read of `vm_host_grants` for authorization.
fn roles_of(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    user_id: &str,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(conn) = db.read() else {
        return out;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT host_id, role FROM vm_host_grants \
         WHERE instance_id = ?1 AND org_id = ?2 \
           AND ((subject_kind = 'user' AND subject_id = ?3) \
             OR (subject_kind = 'group' \
                 AND subject_id IN (SELECT group_id FROM group_members WHERE user_id = ?3)))",
    ) else {
        return out;
    };
    let rows = stmt.query_map(rusqlite::params![instance_id, org_id, user_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    if let Ok(rows) = rows {
        for (host_id, role) in rows.flatten() {
            let current = out.entry(host_id).or_insert_with(|| role.clone());
            if role_rank(&role) > role_rank(current) {
                *current = role;
            }
        }
    }
    out
}

fn role_rank(role: &str) -> u8 {
    match role {
        "manage" => 3,
        "deploy" => 2,
        "view" => 1,
        _ => 0,
    }
}

/// True when the mesh peer is connected right now. A host on a node this one
/// has never met is offline for this dashboard, which is the honest answer:
/// nothing here can reach it.
fn online_checker(ctx: &HandlerContext) -> impl Fn(&str) -> bool + '_ {
    let connected: std::collections::HashSet<String> = ctx
        .state
        .mesh_peer_store
        .list()
        .into_iter()
        .filter(|peer| peer.quic_connected)
        .map(|peer| peer.node_id)
        .collect();
    move |node_id: &str| connected.contains(node_id)
}

fn setting(db: &DbPool, instance_id: &str, org_id: &str, key: &str) -> Option<String> {
    let conn = db.read().ok()?;
    conn.query_row(
        "SELECT value FROM vm_instance_settings \
         WHERE instance_id = ?1 AND org_id = ?2 AND key = ?3",
        rusqlite::params![instance_id, org_id, key],
        |row| row.get(0),
    )
    .ok()
}

/// This node's last probe of ITSELF, when there is one to read. Never probes
/// and never creates the instance database — a dashboard read runs on every
/// page render and on nodes that never initialized the environment.
fn local_probe(ctx: &HandlerContext, g: &Gate) -> Option<crate::tentavm::probe::CachedProbe> {
    crate::tentavm::probe::cached_local(
        &ctx.state.db,
        &g.org_id,
        &g.instance_id,
        &ctx.state.local_node_id,
    )
}

fn hosts_list(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
) -> Result<MessageBody, ProtocolError> {
    let online = online_checker(ctx);
    let role_of = |host_id: &str| access.role_of(host_id);
    let probe = local_probe(ctx, g);

    // §7.1: "listy filtrują hosty bez `view`, gdy `visibility = granted`". The
    // filter is in the QUERY rather than over the answer, so a hidden host is
    // never read, never counted and never partially rendered.
    let (clause, ids) = match access.sql_visible("id") {
        None => (String::new(), Vec::new()),
        Some((predicate, ids)) => (format!(" AND {predicate}"), ids),
    };
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {HOST_COLUMNS} FROM vm_hosts WHERE org_id = ?1{clause} \
             ORDER BY display_name"
        ))
        .map_err(|e| internal("registry", e))?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&g.org_id];
    for id in &ids {
        params.push(id);
    }
    let hosts: Vec<VmHost> = stmt
        .query_map(params.as_slice(), |row| {
            host_row(row, &ctx.state.local_node_id, &online, &role_of, probe.as_ref())
        })
        .map_err(|e| internal("registry", e))?
        .filter_map(Result::ok)
        .collect();

    Ok(tv(P::HostsListResponse {
        // Derived from the FILTERED list on purpose: under `granted` a caller
        // with no grant on their own machine has no local host, and saying so
        // is what lets the browser explain the empty screen instead of
        // pointing at a host that is not in the list beside it.
        local_host_id: hosts
            .iter()
            .find(|host| host.is_local)
            .map(|host| host.host_id.clone()),
        // The value the filter above was built from — the browser can then
        // explain why a host the user knows exists is missing.
        // What the FILTER did, not what the row says. If a peer replicated a
        // word this build does not know, the honest answer to "why is this list
        // short" is `granted` — because that is how the list was built.
        visibility: if access.hide_ungranted {
            "granted".to_string()
        } else {
            "all".to_string()
        },
        hosts,
    }))
}

fn host_get(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    host_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let online = online_checker(ctx);
    let role_of = |id: &str| access.role_of(id);
    let probe = local_probe(ctx, g);

    // A host the caller may not see answers exactly as a host that is not
    // there. Reading the row first and refusing afterwards would give the same
    // answer with a different code, and the code is the leak.
    visible_host(ctx, g, access, host_id)?;

    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let host = conn
        .query_row(
            &format!("SELECT {HOST_COLUMNS} FROM vm_hosts WHERE id = ?1 AND org_id = ?2"),
            rusqlite::params![host_id, g.org_id],
            |row| host_row(row, &ctx.state.local_node_id, &online, &role_of, probe.as_ref()),
        )
        .map_err(|_| {
            ProtocolError::new(
                ProtocolErrorCode::NotFound,
                format!("host '{host_id}' is not in this environment"),
            )
        })?;
    Ok(tv(P::HostGetResponse {
        // The probe of a host lives in the node-local database of the node
        // that OWNS it, so this node can answer with one for itself and for
        // nobody else. None is the documented "never probed" answer and the
        // state H02 draws "Sonduj" for — which is also what a remote host
        // gets here until its own owner answers a `HostProbeRequest`.
        environment: host
            .is_local
            .then(|| probe.map(|cached| cached.probe.environment))
            .flatten(),
        host,
    }))
}

/// The job's own columns plus the display name of the host it concerns. The
/// name is joined here rather than left to the reader: `VmJob.host_name` is
/// documented as "joined by the answering node", and a browser cannot join
/// against a registry it does not hold.
const JOB_COLUMNS: &str = "j.id, j.instance_id, j.kind, j.guest_id, j.source_host_id, \
                           j.target_host_id, j.owner_node_id, j.state, j.progress_pct, \
                           j.phase, j.steps_json, j.cancel_semantics, j.resume_after_restart, \
                           j.error, j.created_by, j.created_at, j.started_at, j.finished_at, \
                           COALESCE(h.display_name, '')";

/// The join is scoped by organization as well as by id. `vm_hosts.id` is a node
/// id or a connector id — unique per fleet, not per tenant — so a job row that
/// names one would otherwise pull ANOTHER organization's display name into the
/// answer. The row is filtered by `org_id` two lines up; its join has to be too.
const JOB_FROM: &str = "FROM vm_jobs j \
                        LEFT JOIN vm_hosts h \
                          ON h.id = COALESCE(j.target_host_id, j.source_host_id) \
                         AND h.org_id = j.org_id";

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VmJob> {
    let kind: String = row.get(2)?;
    let phase: String = row.get(9)?;
    let steps_json: String = row.get(10)?;
    Ok(VmJob {
        job_id: row.get(0)?,
        instance_id: row.get(1)?,
        label: text_with("job.label", &[("kind", &kind)]),
        kind,
        guest_id: row.get(3)?,
        // Joined by the answering node when the machine registry lands with
        // its driver; an empty name renders as "no machine", which is true of
        // every job phase 0 can produce.
        guest_name: String::new(),
        source_host_id: row.get(4)?,
        target_host_id: row.get(5)?,
        host_name: row.get(18)?,
        owner_node_id: row.get(6)?,
        state: row.get(7)?,
        progress_pct: row.get::<_, Option<i64>>(8)?.unwrap_or(0).clamp(0, 100) as u8,
        phase: if phase.is_empty() {
            text("job.phase.none")
        } else {
            text_with("job.phase", &[("phase", &phase)])
        },
        steps: serde_json::from_str::<Vec<VmJobStep>>(&steps_json).unwrap_or_default(),
        cancel_semantics: row.get(11)?,
        resume_after_restart: row.get::<_, i64>(12)? != 0,
        error: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
        created_by: row.get(14)?,
        created_at: row.get(15)?,
        started_at: row.get(16)?,
        finished_at: row.get(17)?,
    })
}

/// Which jobs a caller under `visibility = 'granted'` may see, as a SQL
/// predicate — and nothing at all when the setting hides nothing.
///
/// Three disjuncts, and the third is the one that took a correction:
///
///   * the job names no host — there is nothing to hide;
///   * its host is one the caller may see;
///   * **the caller created it.** Hiding hosts is about not discovering
///     infrastructure, not about hiding somebody's own work from them. A job
///     that vanishes from the list of the person who STARTED it reads as data
///     loss, and nothing leaks by showing it: that their own job is running
///     somewhere is a fact they supplied. The host's NAME is what stays
///     hidden, and `redact_host_name` is what hides it.
///
/// `j.created_by` holds the user id (`jobs` fixture and `vm_jobs.created_by`),
/// which is the same identity `Gate.user_id` carries.
fn job_visibility_filter(access: &Access, user_id: &str) -> (String, Vec<String>) {
    const HOST: &str = "COALESCE(j.target_host_id, j.source_host_id)";
    match access.sql_visible(HOST) {
        None => (String::new(), Vec::new()),
        Some((predicate, mut ids)) => {
            let clause = format!(
                " AND ({HOST} IS NULL OR {predicate} OR j.created_by = ?)"
            );
            ids.push(user_id.to_string());
            (clause, ids)
        }
    }
}

/// Blanks `host_name` on a job whose host the caller may not see.
///
/// The job itself is theirs to read (they started it); the host's display name
/// is environment state the visibility setting withholds. `VmJob.host_name` is
/// documented as joined by the answering node, and an empty string is the same
/// value a job with no host at all carries — so a reader needs no new rule.
fn redact_host_name(access: &Access, mut job: VmJob) -> VmJob {
    let host = job
        .target_host_id
        .clone()
        .or_else(|| job.source_host_id.clone());
    if let Some(host_id) = host {
        if !access.can_see(&host_id) {
            job.host_name = String::new();
        }
    }
    job
}

fn jobs_list(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    host_id: &Option<String>,
    states: &[String],
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    // Narrowing to a host the caller may not see would answer "no jobs" for a
    // host that has them — an existence oracle dressed as an empty list. The
    // named host answers as a missing one instead, exactly like `host_get`.
    if let Some(host_id) = host_id.as_deref() {
        visible_host(ctx, g, access, host_id)?;
    }
    // A limit of zero means "the node decides", not "no rows": a client that
    // omits the field must still get a usable answer.
    let limit = if limit == 0 { 100 } else { limit.min(500) } as i64;
    // The state filter has to be INSIDE the query. Filtering the page after
    // `LIMIT` answers "the newest N jobs, of which these happen to be failed",
    // which for a caller asking for failed jobs is indistinguishable from
    // "there are none" — and was: 2 failed jobs behind 100 newer ones came
    // back as an empty list.
    let states_json = serde_json::to_string(states).map_err(|e| internal("state filter", e))?;
    let (visible, visible_ids) = job_visibility_filter(access, &g.user_id);
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {JOB_COLUMNS} {JOB_FROM} \
             WHERE j.instance_id = ?1 AND j.org_id = ?2 \
               AND (?3 IS NULL OR j.source_host_id = ?3 OR j.target_host_id = ?3) \
               AND (json_array_length(?4) = 0 \
                    OR j.state IN (SELECT value FROM json_each(?4))){visible} \
             ORDER BY j.created_at DESC LIMIT {limit}"
        ))
        .map_err(|e| internal("registry", e))?;
    // The four named parameters keep their positions and the visibility set is
    // appended as anonymous `?`, which SQLite numbers from one past the highest
    // index used SO FAR IN THE TEXT. That is why the limit above is inlined
    // rather than bound as `?5`: with a `?5` after them, the appended markers
    // would be numbered 5, 6, ... and the first one would silently take the
    // place of the limit. It cost one `Internal` here and would have cost a
    // wrong filter in production.
    let mut params: Vec<&dyn rusqlite::ToSql> =
        vec![&g.instance_id, &g.org_id, host_id, &states_json];
    for id in &visible_ids {
        params.push(id);
    }
    let jobs: Vec<VmJob> = stmt
        .query_map(params.as_slice(), job_row)
        .map_err(|e| internal("registry", e))?
        .filter_map(Result::ok)
        .map(|job| redact_host_name(access, job))
        .collect();
    Ok(tv(P::JobsListResponse { jobs }))
}

fn job_get(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    job_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let (visible, visible_ids) = job_visibility_filter(access, &g.user_id);
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&job_id, &g.instance_id, &g.org_id];
    for id in &visible_ids {
        params.push(id);
    }
    let job = conn
        .query_row(
            &format!(
                "SELECT {JOB_COLUMNS} {JOB_FROM} \
                 WHERE j.id = ?1 AND j.instance_id = ?2 AND j.org_id = ?3{visible}"
            ),
            params.as_slice(),
            job_row,
        )
        .map_err(|_| {
            ProtocolError::new(
                ProtocolErrorCode::NotFound,
                format!("job '{job_id}' is not in this environment"),
            )
        })?;
    drop(conn);
    let job = redact_host_name(access, job);

    let log = job_log(ctx, g, job_id)?;
    Ok(tv(P::JobGetResponse { job, log }))
}

/// The job's log from THIS node's instance database. The caller reached this
/// node because it owns the job (see `owner_of_job`), so an empty log means the
/// job produced none — not that the log is somewhere else.
fn job_log(
    ctx: &HandlerContext,
    g: &Gate,
    job_id: &str,
) -> Result<Vec<VmJobLogLine>, ProtocolError> {
    let db = crate::tentavm::open_db(&ctx.state.db, &g.org_id, &g.instance_id)
        .map_err(|e| internal("instance database", e))?;
    let conn = db.read().map_err(|e| internal("instance database", e))?;
    let mut stmt = conn
        .prepare("SELECT at, level, line FROM vm_job_logs WHERE job_id = ?1 ORDER BY seq")
        .map_err(|e| internal("instance database", e))?;
    let lines = stmt
        .query_map(rusqlite::params![job_id], |row| {
            Ok(VmJobLogLine {
                at: row.get(0)?,
                level: row.get(1)?,
                // The log table keys lines by sequence, not by step: a line
                // belongs to the job, and the step a reader wants to fold it
                // under is `VmJobStep.id` in the job row.
                step_id: String::new(),
                text: row.get(2)?,
            })
        })
        .map_err(|e| internal("instance database", e))?
        .filter_map(Result::ok)
        .collect();
    Ok(lines)
}

/// Runs the environment probe (§8.1) on the hardware this node owns, or
/// answers from `vm_probe_cache` when the caller did not ask for a refresh and
/// the stored answer has not expired.
///
/// Three writes come out of one probe and they go to three different places,
/// which is the whole data split of §4.1/§4.2 in one function:
///
///   * the full result, hardware readings included, into the NODE-LOCAL
///     `vm_probe_cache` — it describes this machine and must not replicate;
///   * the status, the engine chips and the capability list onto the
///     replicated `vm_hosts` row, because that is what every other node's host
///     list draws;
///   * nothing at all about consent: the probe READS the admin's decision
///     (`vm_host_settings`) and never grants it.
///
/// The probe is unprivileged and reaches for no helper. Everything that needs
/// root is the install step's; this one only looks.
async fn host_probe(
    ctx: &HandlerContext,
    g: &Gate,
    host_id: &str,
    refresh: bool,
) -> Result<MessageBody, ProtocolError> {
    // A connector host is reached THROUGH an external hypervisor's API, not by
    // reading this machine's /proc — probing one is a connector driver's job
    // and there is no connector driver. Answering with this node's own
    // environment would describe the wrong machine entirely.
    let (kind, node_id): (String, Option<String>) = {
        let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
        conn.query_row(
            "SELECT kind, node_id FROM vm_hosts WHERE id = ?1 AND org_id = ?2",
            rusqlite::params![host_id, g.org_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| internal("registry", e))?
    };
    if kind != "node" {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotImplemented,
            "probing a connector host needs the connector driver, which is not built yet",
        ));
    }
    // OWNING a host row is not being that host. §6.1 states the rule for
    // `kind = 'node'` as identity — `node_id == actor_node_id` — and the two
    // come apart the moment §6.1's own `switch_owner` runs: node B then owns
    // node A's row, routes the probe to itself, and would publish B's
    // hostname, B's libvirt and B's QEMU as the description of A. Everything
    // below reads THIS machine's /proc, so it may only ever answer for it.
    if node_id.as_deref() != Some(&*ctx.state.local_node_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotImplemented,
            "this node can only probe itself; the host that owns that hardware has to answer",
        ));
    }

    if !refresh {
        if let Some(cached) = crate::tentavm::probe::cached_local(
            &ctx.state.db,
            &g.org_id,
            &g.instance_id,
            host_id,
        ) {
            if !cached.expired {
                return Ok(tv(P::HostProbeResponse {
                    host_id: host_id.to_string(),
                    environment: cached.probe.environment,
                }));
            }
        }
    }

    // The LOCAL NODE ID, not `host_id`. The guard above proves the two are
    // equal for the row that got here, but "equal by inference" is how the
    // cache would end up keyed under an id no dashboard read ever asks for:
    // `local_probe`, `cached_local` and `summary` all key by the node id, and
    // nothing in the schema forces `vm_hosts.id == vm_hosts.node_id`.
    let result = crate::tentavm::probe::refresh_local_probe(
        &ctx.state.db,
        &g.org_id,
        &g.instance_id,
        &ctx.state.local_node_id,
    )
    .await
    .map_err(|e| internal("environment probe", e))?;
    Ok(tv(P::HostProbeResponse {
        host_id: host_id.to_string(),
        environment: result.environment,
    }))
}

/// Whether the caller holds `permission` on this environment. Used for the
/// answer's own flags (`can_create_guest`, `can_edit`), never as a gate — a
/// gate that returned a bool would be one `if` away from being forgotten.
fn holds(ctx: &HandlerContext, instance_id: &str, permission: &str) -> bool {
    let Some(user_id) = ctx.org_context.as_ref().map(|o| o.user_id.as_str()) else {
        return false;
    };
    ctx.state
        .permission_checker
        .as_ref()
        .is_some_and(|checker| {
            checker
                .check(instance_id, user_id, permission, None)
                .is_granted()
        })
}

/// The dashboard tiles (P01) and everything its first-run variant (P00) needs.
///
/// The inbox has no table: every item is derived from the registry on demand
/// (`VmInboxItem` documents the sources). Phase 0 derives the two kinds whose
/// sources already exist — an unreachable host and a failed job. The rest
/// arrive with the mechanisms that produce them, and a kind with no source
/// emits nothing rather than an empty-looking row.
async fn summary(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
) -> Result<MessageBody, ProtocolError> {
    // The one read that MEASURES, and it happens before anything is read from
    // the registry — including `local_host.status`, which the probe rewrites.
    //
    // Why here and not only in `init`: P00's whole sentence ("brakuje: qemu,
    // libvirt, swtpm", §17.5 step 3) is this field, and nobody clicks
    // anything between installing the app and opening it. `init` schedules a
    // probe too, but the daemon has no startup pass over installed instances,
    // so after a restart the dashboard read is the only trigger left. It is
    // also the only one that is guaranteed to have run by the time somebody
    // is looking at the answer.
    //
    // Why only here and not in `hosts_list`/`host_get`: one entry point
    // measures, so a page with three panels measures once. Those two keep
    // reading whatever the cache holds.
    //
    // It is also read BEFORE the registry connection is taken: on a database
    // with no read pool (every unit test) `read()` is the writer's mutex, and
    // measuring while holding it would deadlock against itself.
    let probe = crate::tentavm::probe::ensure_local_probe(
        &ctx.state.db,
        &g.org_id,
        &g.instance_id,
        &ctx.state.local_node_id,
    )
    .await;
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;

    // Every host figure below is filtered by the same predicate the host LIST
    // uses. A tile reading "12 hosts" over a list of one is not a cosmetic
    // mismatch: it publishes the size of the fleet to somebody the same setting
    // just decided may not see it.
    let (visible_hosts, visible_ids) = match access.sql_visible("id") {
        None => (String::new(), Vec::new()),
        Some((predicate, ids)) => (format!(" AND {predicate}"), ids),
    };
    let mut host_params: Vec<&dyn rusqlite::ToSql> = vec![&g.org_id];
    for id in &visible_ids {
        host_params.push(id);
    }
    let (hosts_total, hosts_ready, hosts_needs_install, hosts_unreachable) = conn
        .query_row(
            &format!(
                "SELECT COUNT(*), \
                        COALESCE(SUM(status = 'ready'), 0), \
                        COALESCE(SUM(status = 'needs_install'), 0), \
                        COALESCE(SUM(status = 'unreachable'), 0) \
                 FROM vm_hosts WHERE org_id = ?1{visible_hosts}"
            ),
            host_params.as_slice(),
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|e| internal("registry", e))?;

    // A machine awaiting its deferred deletion is still a machine: the tile
    // counts what the environment holds, and `deleted_at` only means the
    // countdown started.
    let (guests_total, guests_running) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(observed_state = 'running'), 0) \
             FROM vm_guests WHERE instance_id = ?1 AND org_id = ?2",
            rusqlite::params![g.instance_id, g.org_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|e| internal("registry", e))?;

    // The same rule the job LIST applies, for the same reason the host figures
    // above are filtered: a counter is a list the reader cannot open.
    let (visible_jobs, visible_job_ids) = job_visibility_filter(access, &g.user_id);
    let mut job_params: Vec<&dyn rusqlite::ToSql> = vec![&g.instance_id, &g.org_id];
    for id in &visible_job_ids {
        job_params.push(id);
    }
    let (jobs_running, jobs_failed) = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(j.state = 'running'), 0), \
                        COALESCE(SUM(j.state = 'failed'), 0) \
                 FROM vm_jobs j WHERE j.instance_id = ?1 AND j.org_id = ?2{visible_jobs}"
            ),
            job_params.as_slice(),
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|e| internal("registry", e))?;

    // Filtered like every other host, and that is a real consequence worth
    // stating: under `visibility = 'granted'` a user with no grant on the
    // machine in front of them gets `local_host_status = "unknown"` and P00
    // draws the "no host here" onboarding. The setting says hosts without a
    // grant are hidden; the machine the browser happens to be talking to is not
    // an exception to it, and making it one would hand every user of the
    // environment the install state of every node they can reach a dashboard on.
    let local_host: Option<(String, String)> = conn
        .query_row(
            "SELECT id, status FROM vm_hosts \
             WHERE org_id = ?1 AND kind = 'node' AND node_id = ?2",
            rusqlite::params![g.org_id, ctx.state.local_node_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
        .filter(|(id, _)| access.can_see(id));

    // Both sources are counted in full and read in pages. Counting the page
    // would make `inbox_total` a copy of `inbox.len()` — the field exists to
    // say "3 of 47" — and reading one source without a limit would let it push
    // the other out of the list entirely: 25 unreachable hosts hid every failed
    // job when the hosts query had no LIMIT and ran first.
    //
    // Counted as two queries rather than one with two subqueries, because each
    // half now carries its own visibility predicate and its own bind values:
    // one statement would have to interleave two id lists into one parameter
    // sequence, which is a place to get it silently wrong.
    let unreachable_total: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM vm_hosts \
                 WHERE org_id = ?1 AND status = 'unreachable'{visible_hosts}"
            ),
            host_params.as_slice(),
            |row| row.get(0),
        )
        .map_err(|e| internal("registry", e))?;
    let failed_total: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM vm_jobs j \
                 WHERE j.instance_id = ?1 AND j.org_id = ?2 AND j.state = 'failed'{visible_jobs}"
            ),
            job_params.as_slice(),
            |row| row.get(0),
        )
        .map_err(|e| internal("registry", e))?;
    let request_total: i64 = if access.is_env_admin || !access.roles.is_empty() {
        conn.query_row(
            "SELECT COUNT(*) FROM vm_access_requests r \
             WHERE r.instance_id = ?1 AND r.org_id = ?2 AND r.expires_at > ?3 \
               AND NOT EXISTS (SELECT 1 FROM vm_access_decisions d WHERE d.request_id = r.id) \
               AND (?4 OR (r.host_id IS NOT NULL AND r.host_id IN (\
                     SELECT host_id FROM vm_host_grants \
                     WHERE instance_id = ?1 AND org_id = ?2 AND role = 'manage' \
                       AND ((subject_kind = 'user' AND subject_id = ?5) \
                         OR (subject_kind = 'group' AND subject_id IN (\
                              SELECT group_id FROM group_members WHERE user_id = ?5))))))",
            rusqlite::params![
                g.instance_id,
                g.org_id,
                crate::tentavm::now(),
                access.is_env_admin,
                g.user_id
            ],
            |row| row.get(0),
        )
        .unwrap_or(0)
    } else {
        0
    };
    let inbox_total: i64 = unreachable_total + failed_total + request_total;

    // Whether the node this request came from may run a high-risk action, so
    // the inbox can mark an item read-only instead of offering a button the
    // executor refuses (§7.1).
    let issuer_is_operator =
        crate::tentavm::policy::node_is_operator(&ctx.state.db, issuer_node_id(ctx))
            .unwrap_or(false);

    let mut unreachable_items: Vec<VmInboxItem> = Vec::new();
    let mut failed_items: Vec<VmInboxItem> = Vec::new();
    let page = INBOX_PAGE as i64;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, display_name, node_id, updated_at FROM vm_hosts \
             WHERE org_id = ?1 AND status = 'unreachable'{visible_hosts} \
             ORDER BY display_name LIMIT {page}"
        ))
        .map_err(|e| internal("registry", e))?;
    let unreachable = stmt
        .query_map(host_params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| internal("registry", e))?;
    for (host_id, host_name, _node_id, updated_at) in unreachable.flatten() {
        unreachable_items.push(VmInboxItem {
            item_id: format!("host_unreachable:{host_id}"),
            kind: "host_unreachable".to_string(),
            params: vec![VmTextParam {
                name: "host".to_string(),
                value: host_name.clone(),
            }],
            host_id: Some(host_id.clone()),
            host_name,
            job_id: None,
            requested_by: String::new(),
            requested_at: updated_at,
            cta_route: format!("host={host_id}"),
            // Nothing here executes anything, so there is no node on which the
            // action could fail closed: the item is actionable wherever it is
            // read.
            read_only: false,
            read_only_reason: text(""),
        });
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT j.id, j.kind, j.created_by, j.updated_at, \
                    COALESCE(h.display_name, ''), j.target_host_id, j.source_host_id \
             FROM vm_jobs j \
             LEFT JOIN vm_hosts h ON h.id = COALESCE(j.target_host_id, j.source_host_id) \
                                 AND h.org_id = j.org_id \
             WHERE j.instance_id = ?1 AND j.org_id = ?2 AND j.state = 'failed'{visible_jobs} \
             ORDER BY j.updated_at DESC LIMIT {page}"
        ))
        .map_err(|e| internal("registry", e))?;
    let failed = stmt
        .query_map(
            job_params.as_slice(),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .map_err(|e| internal("registry", e))?;
    for (job_id, kind, created_by, updated_at, host_name, target, source) in failed.flatten() {
        failed_items.push(VmInboxItem {
            item_id: format!("job_failed:{job_id}"),
            kind: "job_failed".to_string(),
            params: vec![VmTextParam {
                name: "job".to_string(),
                value: kind,
            }],
            host_id: target.or(source),
            host_name,
            job_id: Some(job_id.clone()),
            requested_by: created_by,
            requested_at: updated_at,
            cta_route: format!("job={job_id}"),
            read_only: false,
            read_only_reason: text(""),
        });
    }

    // The third inbox source, and the first one this application ever had a
    // producer for: `VmInboxItem` has documented `kind = 'access_request'`
    // since step 2 with the note "no node emits an item of that kind". It does
    // now.
    //
    // Who sees one: the environment administrator sees every open request, and
    // somebody with `manage` on a host sees the requests about THAT host —
    // exactly the people `access_request_decide` will accept a decision from,
    // so the inbox never shows an item whose button would be refused.
    //
    // "Open" here is the fold: no decision row, and the term has not passed.
    // It is computed rather than stored, so a request nobody answered simply
    // stops appearing when it expires, with no sweeper minting an operation per
    // node per row.
    let mut request_items: Vec<VmInboxItem> = Vec::new();
    if access.is_env_admin || !access.roles.is_empty() {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT r.id, r.scope, r.host_id, r.role, r.requested_by, r.requested_at, \
                        COALESCE(h.display_name, '') \
                 FROM vm_access_requests r \
                 LEFT JOIN vm_hosts h ON h.id = r.host_id AND h.org_id = r.org_id \
                 WHERE r.instance_id = ?1 AND r.org_id = ?2 AND r.expires_at > ?3 \
                   AND NOT EXISTS (SELECT 1 FROM vm_access_decisions d \
                                   WHERE d.request_id = r.id) \
                 ORDER BY r.requested_at DESC LIMIT {page}"
            ))
            .map_err(|e| internal("registry", e))?;
        let rows = stmt
            .query_map(
                rusqlite::params![g.instance_id, g.org_id, crate::tentavm::now()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(|e| internal("registry", e))?;
        for (id, scope, host_id, role, requested_by, requested_at, host_name) in rows.flatten() {
            let decidable = access.is_env_admin
                || host_id
                    .as_deref()
                    .is_some_and(|host| role_rank(&access.role_of(host)) >= role_rank("manage"));
            if !decidable {
                continue;
            }
            request_items.push(VmInboxItem {
                item_id: format!("access_request:{id}"),
                kind: "access_request".to_string(),
                params: vec![
                    VmTextParam {
                        name: "user".to_string(),
                        value: crate::tentavm::access::subject_label(&conn, &requested_by),
                    },
                    VmTextParam {
                        name: "host".to_string(),
                        value: host_name.clone(),
                    },
                    VmTextParam {
                        name: "role".to_string(),
                        value: role.unwrap_or_else(|| scope.clone()),
                    },
                ],
                host_id: host_id.clone(),
                host_name,
                job_id: None,
                requested_by,
                requested_at,
                cta_route: format!("access_request={id}"),
                // §7.1: deciding writes a grant, so it is high-risk, and a
                // non-operator node would refuse the button. The mobile inbox
                // shows the item and says why instead of offering it.
                read_only: !issuer_is_operator,
                read_only_reason: if issuer_is_operator {
                    text("")
                } else {
                    text("inbox.operator_node_required")
                },
            });
        }
    }

    // Round-robin instead of concatenate-and-cut. Twenty-five unreachable
    // hosts would otherwise fill the page and hide every failed job — the tile
    // would say "you have 26 things waiting" and show one kind of them.
    let mut inbox = Vec::with_capacity(INBOX_PAGE);
    let mut sources = [
        request_items.into_iter(),
        unreachable_items.into_iter(),
        failed_items.into_iter(),
    ];
    while inbox.len() < INBOX_PAGE {
        let before = inbox.len();
        for source in sources.iter_mut() {
            if inbox.len() == INBOX_PAGE {
                break;
            }
            if let Some(item) = source.next() {
                inbox.push(item);
            }
        }
        if inbox.len() == before {
            break;
        }
    }

    Ok(tv(P::SummaryResponse {
        summary: VmSummary {
            guests_total: guests_total.max(0) as u32,
            guests_running: guests_running.max(0) as u32,
            hosts_total: hosts_total.max(0) as u32,
            hosts_ready: hosts_ready.max(0) as u32,
            hosts_needs_install: hosts_needs_install.max(0) as u32,
            hosts_unreachable: hosts_unreachable.max(0) as u32,
            jobs_running: jobs_running.max(0) as u32,
            jobs_failed: jobs_failed.max(0) as u32,
            inbox,
            can_create_guest: holds(ctx, &g.instance_id, PERM_CREATE)
                && access.can_deploy_somewhere(),
            local_host_id: local_host.as_ref().map(|(id, _)| id.clone()),
            local_host_status: local_host
                .as_ref()
                .map(|(_, status)| status.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            // Both come from this node's own probe, and an empty list with an
            // empty reason stays the honest "nothing probed yet" — P00 draws
            // its onboarding from `local_host_status`. `local_missing_features`
            // carries FEATURE ids (`kvm_base`), not package names: the field is
            // read against `FeatureState.id`, which is what the browser has an
            // i18n entry for.
            local_missing_features: probe
                .as_ref()
                .map(|cached| {
                    crate::tentavm::probe::missing_feature_ids(&cached.probe.environment)
                })
                .unwrap_or_default(),
            // Only the state no install can change carries a reason; anything
            // else would put "brak VT-x" on a host that merely needs packages.
            local_unsupported_reason: probe
                .as_ref()
                .filter(|cached| {
                    crate::tentavm::probe::host_status(&cached.probe.environment) == "unsupported"
                })
                .map(|cached| cached.probe.environment.virt.detail.clone())
                .unwrap_or_else(|| text("")),
            inbox_total: inbox_total.max(0) as u32,
            // The caller's own most recent request, whatever state it is in —
            // P00 has to draw the refusal and the expiry as well as the wait.
            access_request: crate::tentavm::access::latest_for_user(
                &ctx.state.db,
                &g.instance_id,
                &g.org_id,
                &g.user_id,
                &crate::tentavm::now(),
            ),
        },
    }))
}

fn settings_get(ctx: &HandlerContext, g: &Gate) -> Result<MessageBody, ProtocolError> {
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM vm_instance_settings \
             WHERE instance_id = ?1 AND org_id = ?2",
        )
        .map_err(|e| internal("registry", e))?;
    let stored: std::collections::HashMap<String, String> = stmt
        .query_map(rusqlite::params![g.instance_id, g.org_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| internal("registry", e))?
        .filter_map(Result::ok)
        .collect();

    Ok(tv(P::SettingsGetResponse {
        settings: settings_from_rows(&stored),
        can_edit: holds(ctx, &g.instance_id, PERM_ADMIN),
    }))
}

/// `vm_instance_settings` is a key/value table and `VmInstanceSettings` is a
/// fixed record, so the defaults live here — in the one place that turns rows
/// into the record. They are the plan's defaults (§5.2, §17.5): everything
/// visible, no pool or image preselected, a baseline CPU model so a machine
/// stays migratable across a mixed fleet, and HA off.
fn settings_from_rows(
    rows: &std::collections::HashMap<String, String>,
) -> tentaflow_protocol::tentavm::VmInstanceSettings {
    let text = |key: &str, default: &str| {
        rows.get(key)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    let opt = |key: &str| rows.get(key).filter(|value| !value.is_empty()).cloned();
    tentaflow_protocol::tentavm::VmInstanceSettings {
        visibility: text("visibility", "all"),
        default_pool_id: opt("default_pool_id"),
        default_network_id: opt("default_network_id"),
        default_image_id: opt("default_image_id"),
        default_size_preset: text("default_size_preset", "medium"),
        default_firmware: text("default_firmware", "uefi"),
        ssh_key_source: text("ssh_key_source", "user"),
        cpu_baseline_xml: text("cpu_baseline_xml", ""),
        machine_type: text("machine_type", "q35"),
        autostart_policy: text("autostart_policy", "none"),
        ha_enabled: rows.get("ha_enabled").map(String::as_str) == Some("1"),
        ha_coordinator_node_id: opt("ha_coordinator_node_id"),
        ha_fencing: text("ha_fencing", "off"),
        overcommit_ratio: rows
            .get("overcommit_ratio")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1.0),
    }
}


/// The record back to `vm_instance_settings` rows — the exact inverse of
/// `settings_from_rows`, and the one place that has to agree with it.
///
/// Two functions naming the same fourteen keys is the figure this project has
/// paid for repeatedly, so they are not left to agree by inspection:
/// `every_setting_survives_a_round_trip` writes a NON-DEFAULT value into every
/// field of the record, sends it through both functions and demands the record
/// back. A key spelled differently on the two sides, or missing from this one,
/// comes back as its default and the test fails naming the field.
fn settings_to_rows(
    settings: &tentaflow_protocol::tentavm::VmInstanceSettings,
) -> std::collections::BTreeMap<String, String> {
    let mut rows = std::collections::BTreeMap::new();
    let mut put = |key: &str, value: String| {
        rows.insert(key.to_string(), value);
    };
    put("visibility", settings.visibility.clone());
    put(
        "default_pool_id",
        settings.default_pool_id.clone().unwrap_or_default(),
    );
    put(
        "default_network_id",
        settings.default_network_id.clone().unwrap_or_default(),
    );
    put(
        "default_image_id",
        settings.default_image_id.clone().unwrap_or_default(),
    );
    put("default_size_preset", settings.default_size_preset.clone());
    put("default_firmware", settings.default_firmware.clone());
    put("ssh_key_source", settings.ssh_key_source.clone());
    put("cpu_baseline_xml", settings.cpu_baseline_xml.clone());
    put("machine_type", settings.machine_type.clone());
    put("autostart_policy", settings.autostart_policy.clone());
    put(
        "ha_enabled",
        if settings.ha_enabled { "1" } else { "0" }.to_string(),
    );
    put(
        "ha_coordinator_node_id",
        settings.ha_coordinator_node_id.clone().unwrap_or_default(),
    );
    put("ha_fencing", settings.ha_fencing.clone());
    put("overcommit_ratio", settings.overcommit_ratio.to_string());
    rows
}

// =============================================================================
// §7.1 — which NODE may issue a high-risk operation
// =============================================================================

/// The node whose session issued this request — plan §7.1's "węzeł wystawcy".
///
/// For a session held on this node that is this node; for a request relayed
/// through `app_route` it is the forwarding node, which is the one a person is
/// actually sitting at. Reading it from `RequestOrigin` rather than from the
/// session is the whole point: the same user session travels inside a forwarded
/// request, where it means "somebody else's dashboard asked us to".
fn issuer_node_id(ctx: &HandlerContext) -> &str {
    match &ctx.origin {
        crate::dispatch::RequestOrigin::Local => &ctx.state.local_node_id,
        crate::dispatch::RequestOrigin::Forwarded { origin_node_id } => origin_node_id.as_str(),
    }
}

/// Plan §7.1 point 1: a high-risk operation is executed only when the node that
/// issued it is on the organization's operator list.
///
/// This is the rule behind "telefony/tablety domyślnie nie są operatorskie" —
/// the mobile inbox shows a high-risk item read-only precisely because this gate
/// would refuse it, and a button that fails on the executor is worse than one
/// that is not offered.
///
/// Points 2 and 3 of §7.1 are NOT here and their absence is deliberate rather
/// than forgotten:
///
///   * point 2, an active `node_user_assignments(issuer, sub)`: the table has
///     no producer anywhere in this repository. `assign_node_to_user` has zero
///     production callers (measured); the only writers are tests, baseline
///     restore and the materializer arm that replicates what some other node
///     wrote — and no node writes a first one. A gate on it would deny every
///     high-risk operation on every node forever, which is not "fail closed",
///     it is a gate with no key. Reported as its own step, with the editor that
///     would create the rows.
///   * point 3, `AppPermissionProbe` back to the issuer: it is a new
///     `MeshCommandType`, a wire contract in `tentaflow-protocol/src/mesh.rs`.
///
/// What point 1 alone buys, stated honestly: a node the administrator demoted
/// stops being able to issue these operations, everywhere, as soon as the
/// demotion replicates. What it does not buy: a COMPROMISED operator node is
/// still an operator node, and point 3 is the check that would notice a
/// revocation performed on the issuer between the request and its execution.
fn require_operator_issuer(ctx: &HandlerContext, what: &str) -> Result<(), ProtocolError> {
    let issuer = issuer_node_id(ctx);
    let operator = crate::tentavm::policy::node_is_operator(&ctx.state.db, issuer)
        .map_err(|e| internal("operator list", e))?;
    if operator {
        return Ok(());
    }
    Err(ProtocolError::new(
        ProtocolErrorCode::PolicyDenied,
        format!(
            "{what} is a high-risk operation and node '{issuer}' is not on this \
             organization's operator list (plan §7.1); run it from an operator node"
        ),
    ))
}

// =============================================================================
// Host grants (H06)
// =============================================================================

/// The grant matrix of one host, plus the principals a row could be added for.
///
/// The READ needs `vm.hosts.manage` (the gate) and the WRITE needs `manage` on
/// this host or `vm.admin` (`can_edit`). They are different questions and the
/// protocol says so: `HostGrantsListResponse.can_edit` exists exactly so that
/// an operator who administers the environment but holds only `deploy` HERE
/// sees the matrix read-only instead of a refusal they cannot interpret.
fn host_grants_list(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    host_id: &str,
) -> Result<MessageBody, ProtocolError> {
    visible_host(ctx, g, access, host_id)?;
    let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;

    // The subject LABEL is joined here rather than left to the browser:
    // `VmHostGrant.subject_label` is documented as "so H06 need not join the
    // directory", and a browser cannot join against a user table it does not
    // hold. A subject with no row left in the directory keeps its id as its
    // label — a grant outlives the account it names (the column deliberately
    // has no foreign key), and drawing an empty cell would hide a live grant.
    let mut stmt = conn
        .prepare(
            "SELECT gr.subject_kind, gr.subject_id, \
                    COALESCE(NULLIF(u.display_name, ''), u.username, ug.name, gr.subject_id), \
                    gr.role, gr.granted_by, gr.created_at \
             FROM vm_host_grants gr \
             LEFT JOIN user_accounts u ON gr.subject_kind = 'user' AND u.id = gr.subject_id \
             LEFT JOIN user_groups ug ON gr.subject_kind = 'group' AND ug.id = gr.subject_id \
             WHERE gr.instance_id = ?1 AND gr.org_id = ?2 AND gr.host_id = ?3 \
             ORDER BY gr.subject_kind, 3",
        )
        .map_err(|e| internal("registry", e))?;
    let grants: Vec<tentaflow_protocol::tentavm::VmHostGrant> = stmt
        .query_map(
            rusqlite::params![g.instance_id, g.org_id, host_id],
            |row| {
                Ok(tentaflow_protocol::tentavm::VmHostGrant {
                    host_id: host_id.to_string(),
                    subject_kind: row.get(0)?,
                    subject_id: row.get(1)?,
                    subject_label: row.get(2)?,
                    role: row.get(3)?,
                    granted_by: row.get(4)?,
                    granted_at: row.get(5)?,
                })
            },
        )
        .map_err(|e| internal("registry", e))?
        .filter_map(Result::ok)
        .collect();

    // Users of THIS organization, from `org_memberships` — the org boundary is
    // the same one every other read of this family respects. Groups are a
    // GLOBAL directory in TentaFlow (`user_groups` has no org column), so all
    // of them are offered; narrowing them to "groups with a member in this org"
    // was rejected because it would make a freshly created, still empty group
    // impossible to grant to, which is the normal way an administrator works.
    let mut stmt = conn
        .prepare(
            "SELECT 'user', u.id, COALESCE(NULLIF(u.display_name, ''), u.username) \
             FROM org_memberships m JOIN user_accounts u ON u.id = m.user_id \
             WHERE m.org_id = ?1 AND u.is_active = 1 \
             UNION ALL \
             SELECT 'group', ug.id, ug.name FROM user_groups ug \
             ORDER BY 1, 3",
        )
        .map_err(|e| internal("registry", e))?;
    let candidates: Vec<tentaflow_protocol::tentavm::VmGrantCandidate> = stmt
        .query_map(rusqlite::params![g.org_id], |row| {
            Ok(tentaflow_protocol::tentavm::VmGrantCandidate {
                subject_kind: row.get(0)?,
                subject_id: row.get(1)?,
                subject_label: row.get(2)?,
            })
        })
        .map_err(|e| internal("registry", e))?
        .filter_map(Result::ok)
        .collect();

    Ok(tv(P::HostGrantsListResponse {
        host_id: host_id.to_string(),
        grants,
        candidates,
        can_edit: access.can_edit_grants(host_id),
    }))
}

/// The whole desired grant set of one host.
///
/// Validated BEFORE anything is written, and validated against the same value
/// sets the migration's CHECK constraints hold: a bad `role` must be a sentence
/// naming the column, not a raw `SQLITE_CONSTRAINT` — the same reason
/// `sync/tentavm_registry.rs` states the enum lists for the incoming direction.
async fn host_grants_set(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    host_id: &str,
    grants: &[tentaflow_protocol::tentavm::VmHostGrantInput],
) -> Result<MessageBody, ProtocolError> {
    visible_host(ctx, g, access, host_id)?;
    if !access.can_edit_grants(host_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!(
                "rewriting the grants of host '{host_id}' needs the 'manage' grant on it \
                 or the environment's {PERM_ADMIN} permission"
            ),
        ));
    }
    require_operator_issuer(ctx, "rewriting host grants")?;

    let mut desired: Vec<crate::tentavm::policy::GrantRow> = Vec::with_capacity(grants.len());
    let mut seen = std::collections::HashSet::new();
    for input in grants {
        // An empty role REMOVES the row (the protocol says so), and the matrix
        // sends the full desired state, so a removal is simply an absence here.
        if input.role.is_empty() {
            continue;
        }
        if !matches!(input.subject_kind.as_str(), "user" | "group") {
            return Err(ProtocolError::bad_request(format!(
                "subject_kind must be 'user' or 'group', not '{}'",
                input.subject_kind
            )));
        }
        if !matches!(input.role.as_str(), "view" | "deploy" | "manage") {
            return Err(ProtocolError::bad_request(format!(
                "role must be 'view', 'deploy' or 'manage', not '{}'",
                input.role
            )));
        }
        if input.subject_id.trim().is_empty() {
            return Err(ProtocolError::bad_request(
                "a grant must name the user or group it is for",
            ));
        }
        // Two cells for one subject is not a merge decision this node gets to
        // make: the screen would have shown one of them and the other is what
        // the caller believes they saved.
        if !seen.insert((input.subject_kind.clone(), input.subject_id.clone())) {
            return Err(ProtocolError::bad_request(format!(
                "subject '{}' appears twice in the matrix",
                input.subject_id
            )));
        }
        desired.push(crate::tentavm::policy::GrantRow {
            subject_kind: input.subject_kind.clone(),
            subject_id: input.subject_id.clone(),
            role: input.role.clone(),
        });
    }

    crate::tentavm::policy::set_host_grants(
        &ctx.state.db,
        &g.instance_id,
        &g.org_id,
        host_id,
        &g.user_id,
        &ctx.state.local_node_id,
        &desired,
    )
    .map_err(|e| internal("host grants", e))?;

    // Answered by RE-READING, not by echoing what was asked for. The protocol
    // says the matrix "redraws from what was actually stored", and this write
    // also changes the CALLER's own access — an admin who removes their own
    // `manage` must see the read-only matrix that is now true for them.
    let access = Access::resolve(ctx, g);
    host_grants_list(ctx, g, &access, host_id)
}

/// The environment settings document. Same operator rule as the grant matrix,
/// for the same reason: `visibility` is the setting every host read above is
/// filtered by, so a copy of it that exists on one node only would give two
/// nodes two different answers to "which hosts are there".
async fn settings_set(
    ctx: &HandlerContext,
    g: &Gate,
    settings: &tentaflow_protocol::tentavm::VmInstanceSettings,
) -> Result<MessageBody, ProtocolError> {
    require_operator_issuer(ctx, "rewriting environment settings")?;
    for (field, value, allowed) in [
        ("visibility", settings.visibility.as_str(), &["all", "granted"][..]),
        (
            "default_firmware",
            settings.default_firmware.as_str(),
            &["uefi", "bios"][..],
        ),
    ] {
        if !allowed.contains(&value) {
            return Err(ProtocolError::bad_request(format!(
                "{field} must be one of {allowed:?}, not '{value}'"
            )));
        }
    }
    crate::tentavm::policy::set_instance_settings(
        &ctx.state.db,
        &g.instance_id,
        &g.org_id,
        &ctx.state.local_node_id,
        &settings_to_rows(settings),
    )
    .map_err(|e| internal("environment settings", e))?;
    settings_get(ctx, g)
}


// =============================================================================
// Access requests (P00)
// =============================================================================

/// "Poproś administratora" — files one request.
///
/// Gated on `vm.read` and nothing more, deliberately: this is the button for
/// somebody who has NO authority yet, and gating it on the permission they are
/// asking for would make it unreachable exactly when it is needed. What stops
/// it being an open channel is the idempotence check below and the fact that
/// the row grants nothing until an administrator decides.
async fn access_request_file(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    scope: &str,
    host_id: Option<&str>,
    role: &str,
    reason: &str,
) -> Result<MessageBody, ProtocolError> {
    // The pair (`host_id`, `role`) is the DDL's CHECK, stated here so a bad
    // request is a sentence naming the column rather than raw constraint text —
    // and so it never reaches the ledger, where it would be a terminal conflict.
    let (host_id, role) = match scope {
        "instance_create" => {
            if host_id.is_some() || !role.is_empty() {
                return Err(ProtocolError::bad_request(
                    "an 'instance_create' request names no host and no role",
                ));
            }
            (None, None)
        }
        "host_role" => {
            let Some(host_id) = host_id.filter(|id| !id.trim().is_empty()) else {
                return Err(ProtocolError::bad_request(
                    "a 'host_role' request must name the host it is about",
                ));
            };
            if !matches!(role, "view" | "deploy" | "manage") {
                return Err(ProtocolError::bad_request(format!(
                    "role must be 'view', 'deploy' or 'manage', not '{role}'"
                )));
            }
            // The host has to exist — and be one this caller can see, because a
            // request naming a host they may not know about would answer the
            // question the visibility setting refuses to answer.
            visible_host(ctx, g, access, host_id)?;
            (Some(host_id.to_string()), Some(role.to_string()))
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "scope must be 'instance_create' or 'host_role', not '{other}'"
            )))
        }
    };
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ProtocolError::bad_request(
            "a request an administrator has to judge must say what it is for",
        ));
    }

    crate::tentavm::access::file(
        &ctx.state.db,
        &g.instance_id,
        &g.org_id,
        &g.user_id,
        scope,
        host_id.as_deref(),
        role.as_deref(),
        reason,
        &ctx.state.local_node_id,
    )
    .map_err(|e| match e.downcast_ref::<crate::tentavm::access::AlreadyOpen>() {
        // Not an internal error and not a silent success: the caller has an
        // open request and the honest answer is to say so, so the screen can
        // point at the one they already filed instead of stacking another.
        Some(_) => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "you already have an open request for this; wait for it or let it expire",
        ),
        None => internal("access request", e),
    })?;

    summary(ctx, g, access).await
}

/// An administrator's decision on one request.
///
/// Three gates, and each closes something a different scenario opens:
///
///   * `vm.admin`, or `vm.hosts.manage` together with `manage` on the host the
///     request is about — deciding who may touch a host is administering it;
///   * the operator-node rule of §7.1, because a decision writes a grant;
///   * the TERM. A request whose `expires_at` has passed is refused here and
///     the same rule is re-applied on every node by the materializer, judged
///     against the moment of the DECISION rather than of its arrival. Without
///     this half, a request forgotten a month ago could still be approved and
///     would still write a real grant — approving IS executing.
async fn access_request_decide(
    ctx: &HandlerContext,
    g: &Gate,
    access: &Access,
    request_id: &str,
    decision: &str,
    note: &str,
    content_digest: &str,
) -> Result<MessageBody, ProtocolError> {
    if !matches!(decision, "approve" | "reject") {
        return Err(ProtocolError::bad_request(format!(
            "decision must be 'approve' or 'reject', not '{decision}'"
        )));
    }
    let now = crate::tentavm::now();
    let (request, decisions) = {
        let conn = ctx.state.db.read().map_err(|e| internal("registry", e))?;
        crate::tentavm::access::load(&conn, &g.instance_id, &g.org_id, request_id)
            .map_err(|e| internal("access request", e))?
    }
    .ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotFound,
            format!("request '{request_id}' is not in this environment"),
        )
    })?;

    // Authorization first, so an unauthorized caller learns nothing about the
    // row's contents from the digest check or the term.
    if !access.is_env_admin {
        let Some(host_id) = request.host_id.as_deref() else {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                format!("deciding an 'instance_create' request needs {PERM_ADMIN}"),
            ));
        };
        if !holds(ctx, &g.instance_id, PERM_HOSTS_MANAGE) {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                format!("deciding a host request needs {PERM_HOSTS_MANAGE} or {PERM_ADMIN}"),
            ));
        }
        visible_host(ctx, g, access, host_id)?;
        access.require_role(host_id, "manage")?;
    }
    require_operator_issuer(ctx, "deciding an access request")?;

    // §15: the decision is bound to the row that was SHOWN. The stored row is
    // immutable, so this is not defending against an edit — it is defending
    // against deciding from a list this node had not yet caught up on.
    let expected = crate::tentavm::access::content_digest(&request);
    if content_digest != expected {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "this request is not the one that was shown; reload it and decide again",
        ));
    }

    // The term gates the DECISION, not just the view. Folded as of NOW, because
    // that is the moment this decision is being made.
    let folded = crate::tentavm::access::fold(&request, &decisions, &now);
    if folded.state == "expired" {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "this request expired and can no longer be decided",
        ));
    }

    crate::tentavm::access::decide(
        &ctx.state.db,
        &request,
        &g.user_id,
        decision,
        note,
        &ctx.state.local_node_id,
    )
    .map_err(|e| internal("access decision", e))?;

    summary(ctx, g, access).await
}

// =============================================================================
// Entry point
// =============================================================================

/// Everything the family can be asked. Each arm names the permission it needs
/// BEFORE it touches data, and host- or job-scoped work is handed to the node
/// that owns the row.
///
/// The arms that answer `NotImplemented` are not placeholders for missing code
/// in this file: each names a mechanism that does not exist on any node yet
/// (the environment probe, the host-grant editor, replicated registry writes).
/// A typed refusal is what a caller can act on; a silent success or an empty
/// answer would say the operation happened.
#[handler(variant = "TentaVmBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn tentavm_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TentaVmBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected TentaVmBody")),
    };
    match payload {
        P::SummaryRequest { instance_id } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            summary(ctx, &g, &access).await
        }
        P::HostsListRequest { instance_id } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            hosts_list(ctx, &g, &access)
        }
        P::HostGetRequest {
            instance_id,
            host_id,
        } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            host_get(ctx, &g, &access, host_id)
        }
        P::HostProbeRequest {
            instance_id,
            host_id,
            refresh,
        } => {
            let g = gate(ctx, instance_id, PERM_HOSTS_MANAGE)?;
            let access = Access::resolve(ctx, &g);
            // §15: `vm.hosts.manage` says WHAT the caller may do, the `manage`
            // grant says WHERE. Both, and both HERE — before the request is
            // routed anywhere, so a node cannot be used as a relay to reach a
            // host its own user has no title on. The owner re-runs this same
            // gate against its own copy of the registry when the request lands.
            visible_host(ctx, &g, &access, host_id)?;
            access.require_role(host_id, "manage")?;
            require_operator_issuer(ctx, "probing a host")?;
            if let Some(owner) = owner_of_host(
                &ctx.state.db,
                &g.org_id,
                host_id,
                &ctx.state.local_node_id,
            )? {
                return forward(ctx, &owner, req).await;
            }
            host_probe(ctx, &g, host_id, *refresh).await
        }
        P::JobsListRequest {
            instance_id,
            host_id,
            states,
            limit,
        } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            jobs_list(ctx, &g, &access, host_id, states, *limit)
        }
        P::JobGetRequest {
            instance_id,
            job_id,
        } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            if let Some(owner) = owner_of_job(
                &ctx.state.db,
                &g.org_id,
                &g.instance_id,
                job_id,
                &ctx.state.local_node_id,
            )? {
                return forward(ctx, &owner, req).await;
            }
            job_get(ctx, &g, &access, job_id)
        }
        P::SettingsGetRequest { instance_id } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            settings_get(ctx, &g)
        }
        P::HostGrantsListRequest {
            instance_id,
            host_id,
        } => {
            // `vm.hosts.manage`, not `vm.read`: this list enumerates the people
            // and teams of the organization against one host, which is a
            // different thing from seeing that the host exists. `can_edit` on
            // the answer is the narrower question — see `host_grants_list`.
            let g = gate_any(ctx, instance_id, &[PERM_HOSTS_MANAGE, PERM_ADMIN])?;
            let access = Access::resolve(ctx, &g);
            host_grants_list(ctx, &g, &access, host_id)
        }
        P::HostGrantsSetRequest {
            instance_id,
            host_id,
            grants,
        } => {
            let g = gate_any(ctx, instance_id, &[PERM_HOSTS_MANAGE, PERM_ADMIN])?;
            let access = Access::resolve(ctx, &g);
            host_grants_set(ctx, &g, &access, host_id, grants).await
        }
        P::SettingsSetRequest {
            instance_id,
            settings,
        } => {
            let g = gate(ctx, instance_id, PERM_ADMIN)?;
            settings_set(ctx, &g, settings).await
        }
        P::JobCancelRequest {
            instance_id,
            job_id,
        } => {
            let g = gate(ctx, instance_id, PERM_HOSTS_MANAGE)?;
            require_operator_issuer(ctx, "cancelling a job")?;
            if let Some(owner) = owner_of_job(
                &ctx.state.db,
                &g.org_id,
                &g.instance_id,
                job_id,
                &ctx.state.local_node_id,
            )? {
                return forward(ctx, &owner, req).await;
            }
            Err(ProtocolError::new(
                ProtocolErrorCode::NotImplemented,
                "no job engine runs on this node yet",
            ))
        }
        P::InboxSnoozeRequest { instance_id, .. } => {
            gate(ctx, instance_id, PERM_READ)?;
            Err(ProtocolError::new(
                ProtocolErrorCode::NotImplemented,
                "deferring an inbox item has no storage on this node yet",
            ))
        }
        P::AccessRequestFileRequest {
            instance_id,
            scope,
            host_id,
            role,
            reason,
        } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            access_request_file(
                ctx,
                &g,
                &access,
                scope,
                host_id.as_deref(),
                role,
                reason,
            )
            .await
        }
        P::AccessRequestDecideRequest {
            instance_id,
            request_id,
            decision,
            note,
            content_digest,
        } => {
            let g = gate(ctx, instance_id, PERM_READ)?;
            let access = Access::resolve(ctx, &g);
            access_request_decide(
                ctx,
                &g,
                &access,
                request_id,
                decision,
                note,
                content_digest,
            )
            .await
        }
        // Responses are what this node SENDS. One arriving as a request is a
        // client bug or a probe, and either way there is nothing to do with it.
        P::SummaryResponse { .. }
        | P::HostsListResponse { .. }
        | P::HostGetResponse { .. }
        | P::HostProbeResponse { .. }
        | P::JobsListResponse { .. }
        | P::JobGetResponse { .. }
        | P::HostGrantsListResponse { .. }
        | P::SettingsGetResponse { .. } => Err(ProtocolError::bad_request(
            "response variants are not requests",
        )),
    }
}

// =============================================================================
// Variant registration
// =============================================================================

/// `#[handler]` registers the dispatcher under the FAMILY name, and no frame
/// ever carries that: `variant_name_of` reports the concrete variant, and
/// `dispatch::find` looks the handler up by it. Without an entry per request
/// variant the whole family answers `NotImplemented` on the wire while every
/// unit test that calls `tentavm_dispatch` directly stays green — which is
/// exactly how this shipped in the first round of this step.
macro_rules! register_tentavm_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_tentavm_dispatch,
            }
        }
    };
}

register_tentavm_variant!("TentaVmSummaryRequest", "tentaflow_ws_handler_vm_summary");
register_tentavm_variant!("TentaVmHostsListRequest", "tentaflow_ws_handler_vm_hosts_list");
register_tentavm_variant!("TentaVmHostGetRequest", "tentaflow_ws_handler_vm_host_get");
register_tentavm_variant!("TentaVmHostProbeRequest", "tentaflow_ws_handler_vm_host_probe");
register_tentavm_variant!("TentaVmJobsListRequest", "tentaflow_ws_handler_vm_jobs_list");
register_tentavm_variant!("TentaVmJobGetRequest", "tentaflow_ws_handler_vm_job_get");
register_tentavm_variant!(
    "TentaVmHostGrantsListRequest",
    "tentaflow_ws_handler_vm_host_grants_list"
);
register_tentavm_variant!(
    "TentaVmHostGrantsSetRequest",
    "tentaflow_ws_handler_vm_host_grants_set"
);
register_tentavm_variant!("TentaVmSettingsGetRequest", "tentaflow_ws_handler_vm_settings_get");
register_tentavm_variant!("TentaVmSettingsSetRequest", "tentaflow_ws_handler_vm_settings_set");
register_tentavm_variant!("TentaVmJobCancelRequest", "tentaflow_ws_handler_vm_job_cancel");
register_tentavm_variant!("TentaVmInboxSnoozeRequest", "tentaflow_ws_handler_vm_inbox_snooze");
register_tentavm_variant!(
    "TentaVmAccessRequestFileRequest",
    "tentaflow_ws_handler_vm_access_request_file"
);
register_tentavm_variant!(
    "TentaVmAccessRequestDecideRequest",
    "tentaflow_ws_handler_vm_access_request_decide"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use crate::services::rbac::OrgContext;
    use std::sync::Arc;

    const ORG: &str = "org-vm";
    const USER: &str = "user-vm";
    const LOCAL: &str = "test-node";
    const REMOTE: &str = "peer-node";

    fn ctx_for(state: &Arc<AppState>) -> HandlerContext {
        HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [3u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: USER.to_string(),
                org_id: ORG.to_string(),
                role_id: "role-vm".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    /// One environment with `vm.read` granted to the test user. The manifest
    /// is passed in because TentaVM is deliberately not in the package catalog
    /// yet (its tile has no route until the UI shell lands), so the fixture
    /// cannot look it up there.
    fn env(state: &Arc<AppState>, addon_id: &str) -> String {
        super::super::app_gate::test_support::install_app_instance(
            state,
            crate::tentavm::PACKAGE_ID,
            addon_id,
            crate::tentavm::APP_MANIFEST,
            &[PERM_READ],
        )
    }

    fn host(state: &Arc<AppState>, id: &str, node_id: &str, status: &str, owner: &str) {
        let conn = state.db.write().unwrap();
        conn.execute(
            "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                 display_name, engines_json, capabilities_json, status, owner_node_id, \
                 owner_epoch, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, 'node', ?3, NULL, NULL, ?4, '[]', '[]', ?5, ?6, 0, 't', 't', ?6)",
            rusqlite::params![id, ORG, node_id, format!("host {id}"), status, owner],
        )
        .expect("host row");
    }

    fn job(state: &Arc<AppState>, id: &str, instance: &str, owner: &str, job_state: &str) {
        let conn = state.db.write().unwrap();
        conn.execute(
            "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, source_host_id, \
                 target_host_id, owner_node_id, state, progress_pct, phase, steps_json, \
                 cancel_semantics, resume_after_restart, error, started_at, finished_at, \
                 created_by, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, ?3, 'host_probe', NULL, NULL, NULL, ?4, ?5, 0, '', '[]', \
                 'cooperative', 0, NULL, NULL, NULL, ?6, 't', 't', ?4)",
            rusqlite::params![id, instance, ORG, owner, job_state, USER],
        )
        .expect("job row");
    }

    async fn call(ctx: &HandlerContext, body: P) -> Result<MessageBody, ProtocolError> {
        tentavm_dispatch(&MessageBody::TentaVmBody(body), ctx).await
    }

    /// The `sync_nodes` row a real node writes for ITSELF, `operator = 1` — the
    /// bootstrap of the organization's operator list in
    /// `repository::record_local_node_identity`. §7.1 point 1 reads exactly this
    /// row, so a fixture without it describes a node the fleet has never heard
    /// of. That is a real state and it has its own test
    /// (`a_high_risk_operation_needs_an_operator_node`); it is not the state the
    /// tests below are about.
    fn operator_node(state: &Arc<AppState>, node_id: &str) {
        let conn = state.db.write().unwrap();
        conn.execute(
            "INSERT INTO sync_nodes \
                (node_id, public_key, public_key_type, display_name, node_kind, \
                 trust_status, owner_user_id, sync_profile, operator) \
             VALUES (?1, ?1, 'ed25519', ?1, 'server', 'trusted', NULL, 'authority', 1) \
             ON CONFLICT(node_id) DO UPDATE SET operator = 1",
            rusqlite::params![node_id],
        )
        .expect("operator node row");
    }

    /// One `vm_host_grants` row for the test user on one host.
    fn grant_host(state: &Arc<AppState>, instance: &str, host_id: &str, role: &str) {
        let conn = state.db.write().unwrap();
        conn.execute(
            "INSERT INTO vm_host_grants (instance_id, host_id, subject_kind, subject_id, \
                 org_id, role, granted_by, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, 'user', ?3, ?4, ?5, 'admin', 't', 't', 'x') \
             ON CONFLICT(instance_id, host_id, subject_kind, subject_id) DO UPDATE SET \
                 role = excluded.role",
            rusqlite::params![instance, host_id, USER, ORG, role],
        )
        .expect("grant row");
    }

    /// Everything a high-risk host operation needs besides the permission: the
    /// issuing node on the operator list (§7.1 point 1) and the `manage` grant
    /// on the host (§15). Both, because §15's whole point is that the two
    /// answer different questions — WHAT and WHERE.
    fn may_manage_host(state: &Arc<AppState>, instance: &str, host_id: &str) {
        operator_node(state, LOCAL);
        grant_host(state, instance, host_id, "manage");
    }

    /// One environment `vm_instance_settings` key.
    fn set_setting(state: &Arc<AppState>, instance: &str, key: &str, value: &str) {
        let conn = state.db.write().unwrap();
        conn.execute(
            "INSERT INTO vm_instance_settings \
                 (instance_id, key, org_id, value, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, ?3, ?4, 't', 't', 'x') \
             ON CONFLICT(instance_id, key) DO UPDATE SET value = excluded.value",
            rusqlite::params![instance, key, ORG, value],
        )
        .expect("setting row");
    }

    /// The property the whole family rests on: a grant is a grant ON ONE
    /// ENVIRONMENT. A package-level gate would resolve the instance from the
    /// package id, find the granted one and answer — handing the caller data of
    /// an environment nobody gave them.
    #[tokio::test]
    async fn a_grant_on_one_environment_does_not_open_another() {
        let state = AppState::for_test();
        let granted = env(&state, "tentavm-aaaaaaaa");
        let other = super::super::app_gate::test_support::install_app_instance(
            &state,
            crate::tentavm::PACKAGE_ID,
            "tentavm-bbbbbbbb",
            crate::tentavm::APP_MANIFEST,
            &[],
        );
        let ctx = ctx_for(&state);

        call(
            &ctx,
            P::SummaryRequest {
                instance_id: granted.clone(),
            },
        )
        .await
        .expect("the granted environment answers");

        let denied = call(&ctx, P::SummaryRequest { instance_id: other })
            .await
            .expect_err("the other environment must not answer");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// An instance id of ANOTHER app must not open this family. Without the
    /// package check the matrix would be asked about a TentaNas instance and
    /// could well say yes — the ids are opaque and the permissions are per
    /// instance.
    #[tokio::test]
    async fn an_instance_of_another_app_is_not_an_environment() {
        let state = AppState::for_test();
        let foreign = super::super::app_gate::test_support::install_app(
            &state,
            crate::tentanas::PACKAGE_ID,
            &[PERM_READ],
        );
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::SummaryRequest {
                instance_id: foreign,
            },
        )
        .await
        .expect_err("a TentaNas instance is not a TentaVM environment");
        assert_eq!(error.code, ProtocolErrorCode::AppUnavailable);
    }

    /// Disabling an environment has to refuse on the wire. Hiding its tile is
    /// not a gate — the request never goes through the tile.
    #[tokio::test]
    async fn a_disabled_environment_refuses() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE addons SET is_enabled = 0 WHERE addon_id = ?1",
                rusqlite::params![instance],
            )
            .expect("disable");
        }
        let ctx = ctx_for(&state);

        let error = call(&ctx, P::SummaryRequest { instance_id: instance })
            .await
            .expect_err("a disabled environment must refuse");
        assert_eq!(error.code, ProtocolErrorCode::AppUnavailable);
    }

    /// The host list is a registry read: it answers on any node, marks the one
    /// the caller is talking to, and carries the visibility setting so an empty
    /// list can be explained rather than look broken.
    #[tokio::test]
    async fn the_host_list_answers_from_the_replicated_registry() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        host(&state, REMOTE, REMOTE, "needs_install", REMOTE);
        let ctx = ctx_for(&state);

        let answer = call(&ctx, P::HostsListRequest { instance_id: instance })
            .await
            .expect("host list");
        let MessageBody::TentaVmBody(P::HostsListResponse {
            hosts,
            local_host_id,
            visibility,
        }) = answer
        else {
            panic!("expected a host list");
        };
        assert_eq!(hosts.len(), 2);
        assert_eq!(local_host_id.as_deref(), Some(LOCAL));
        assert_eq!(visibility, "all", "the default is everything visible");
        let local = hosts.iter().find(|h| h.host_id == LOCAL).expect("local host");
        assert!(local.is_local && local.online, "this node is always reachable");
        assert_eq!(
            local.status_reason.key, "host.ready",
            "a reachable host explains its own status, not somebody else's"
        );
        let remote = hosts.iter().find(|h| h.host_id == REMOTE).expect("remote");
        assert!(!remote.is_local);
        assert!(
            !remote.online,
            "a node this one has no live connection to is offline, not assumed up"
        );
        assert_eq!(
            remote.status_reason.key, "host.offline",
            "being unreachable outranks the stored status"
        );
    }

    /// Hardware work goes to the node that owns the hardware. A LOCAL host is
    /// probed here and answers with its environment; a REMOTE one reaches the
    /// mesh — which is not running in a test, and that refusal is the proof
    /// the request was addressed elsewhere.
    #[tokio::test]
    async fn a_probe_is_routed_to_the_node_that_owns_the_host() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-route001");
        super::super::app_gate::test_support::grant(
            &state,
            &instance,
            USER,
            PERM_HOSTS_MANAGE,
        );
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        host(&state, REMOTE, REMOTE, "ready", REMOTE);
        may_manage_host(&state, &instance, LOCAL);
        grant_host(&state, &instance, REMOTE, "manage");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostProbeResponse { host_id, environment }) = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                refresh: true,
            },
        )
        .await
        .expect("the node that owns the hardware probes it")
        else {
            panic!("expected a probe result");
        };
        assert_eq!(host_id, LOCAL, "the answer names the host that was probed");
        assert_eq!(
            environment.platform,
            std::env::consts::OS,
            "the probe describes THIS machine, not a stored guess"
        );
        assert!(
            !environment.probed_at.is_empty(),
            "H02 prints how old the answer is"
        );

        let remote = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: REMOTE.to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("the mesh is not running in a test");
        assert_eq!(
            remote.code,
            ProtocolErrorCode::NotAvailable,
            "a host owned elsewhere must leave this node, not answer locally"
        );

        let unknown = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: "nowhere".to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("an unknown host is not routable");
        assert_eq!(
            unknown.code,
            ProtocolErrorCode::NotFound,
            "an unknown id must fail here, not time out on the mesh"
        );
    }

    /// A job's log lives in the instance database of the node that ran it, so
    /// reading one travels the same way a probe does.
    #[tokio::test]
    async fn a_job_read_is_routed_to_the_node_that_ran_it() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        job(&state, "job-remote", &instance, REMOTE, "failed");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::JobGetRequest {
                instance_id: instance.clone(),
                job_id: "job-remote".to_string(),
            },
        )
        .await
        .expect_err("the mesh is not running in a test");
        assert_eq!(error.code, ProtocolErrorCode::NotAvailable);

        let missing = call(
            &ctx,
            P::JobGetRequest {
                instance_id: instance,
                job_id: "job-nowhere".to_string(),
            },
        )
        .await
        .expect_err("an unknown job is not routable");
        assert_eq!(missing.code, ProtocolErrorCode::NotFound);
    }

    /// The dashboard counts what the registry holds and derives its inbox from
    /// it — there is no inbox table, so an item exists exactly as long as its
    /// cause does, and its id is stable so "Później" can key on it.
    #[tokio::test]
    async fn the_summary_counts_the_registry_and_derives_the_inbox() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        host(&state, REMOTE, REMOTE, "unreachable", REMOTE);
        job(&state, "job-1", &instance, LOCAL, "failed");
        job(&state, "job-2", &instance, LOCAL, "running");
        let ctx = ctx_for(&state);

        let answer = call(&ctx, P::SummaryRequest { instance_id: instance })
            .await
            .expect("summary");
        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = answer else {
            panic!("expected a summary");
        };
        assert_eq!(summary.hosts_total, 2);
        assert_eq!(summary.hosts_needs_install, 1);
        assert_eq!(summary.hosts_unreachable, 1);
        assert_eq!(summary.jobs_running, 1);
        assert_eq!(summary.jobs_failed, 1);
        assert_eq!(summary.local_host_id.as_deref(), Some(LOCAL));
        assert_eq!(summary.local_host_status, "needs_install");
        assert!(
            !summary.can_create_guest,
            "the fixture grants vm.read only, and P00 keys its empty state on this"
        );

        let kinds: Vec<&str> = summary.inbox.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds, vec!["host_unreachable", "job_failed"]);
        assert_eq!(summary.inbox_total, 2);
        let ids: Vec<&str> = summary.inbox.iter().map(|i| i.item_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["host_unreachable:peer-node", "job_failed:job-1"],
            "an item id names its cause, so it survives recomputation"
        );
    }

    /// An environment that was never configured answers with the plan's
    /// defaults, not with empty strings: the dashboard renders this record
    /// directly.
    #[tokio::test]
    async fn settings_fall_back_to_the_documented_defaults() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        let ctx = ctx_for(&state);

        let answer = call(&ctx, P::SettingsGetRequest { instance_id: instance })
            .await
            .expect("settings");
        let MessageBody::TentaVmBody(P::SettingsGetResponse { settings, can_edit }) = answer
        else {
            panic!("expected settings");
        };
        assert_eq!(settings.visibility, "all");
        assert_eq!(settings.default_firmware, "uefi");
        assert_eq!(settings.machine_type, "q35");
        assert!(!settings.ha_enabled);
        assert!((settings.overcommit_ratio - 1.0).abs() < f64::EPSILON);
        assert!(!can_edit, "editing settings needs vm.admin");
    }

    /// The registration test, and the only one here that goes through the real
    /// dispatcher. Every REQUEST variant the protocol declares must resolve to
    /// a handler by the name a frame actually carries — the family name that
    /// `#[handler]` registers is carried by nothing, so without one entry per
    /// variant the whole family answers `NotImplemented` on the wire while the
    /// eleven tests below, which call `tentavm_dispatch` directly, stay green.
    ///
    /// The list is read from the protocol source rather than typed out here:
    /// a variant appended there must fail this test until it is registered,
    /// which is the only way an omission surfaces before production.
    #[test]
    fn every_request_variant_resolves_to_a_handler() {
        const PROTOCOL_SRC: &str = include_str!("../../../tentaflow-protocol/src/tentavm.rs");
        let body = PROTOCOL_SRC
            .split_once("pub enum TentaVmPayload {")
            .expect("TentaVmPayload enum")
            .1;
        let mut requests = Vec::new();
        for line in body.lines() {
            if line == "}" {
                break;
            }
            let Some(rest) = line.strip_prefix("    ") else {
                continue;
            };
            if rest.starts_with(' ') || !rest.starts_with(char::is_uppercase) {
                continue;
            }
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.ends_with("Request") {
                requests.push(format!("TentaVm{name}"));
            }
        }
        assert!(
            requests.len() >= 12,
            "the parser found {} request variants, which cannot be right",
            requests.len()
        );
        for variant in requests {
            let handler = crate::dispatch::find(&variant)
                .unwrap_or_else(|| panic!("{variant} has no registered handler"));
            assert_eq!(
                handler.required_auth,
                crate::dispatch::SessionAuthKind::UserSession,
                "{variant} must stay at UserSession — the environment is checked per request"
            );
        }
    }

    /// The organization comes from the SESSION. `instance_id` in the body says
    /// which environment; nothing in the body may widen which org's rows are
    /// read, or a caller could name an environment and be served another
    /// tenant's hosts and jobs.
    #[tokio::test]
    async fn rows_of_another_organization_are_invisible() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES ('foreign', 'org-other', 'node', 'foreign-node', NULL, NULL, \
                     'foreign', '[]', '[]', 'ready', 'foreign-node', 0, 't', 't', 'x')",
                [],
            )
            .expect("foreign host");
            conn.execute(
                "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, source_host_id, \
                     target_host_id, owner_node_id, state, progress_pct, phase, steps_json, \
                     cancel_semantics, resume_after_restart, error, started_at, finished_at, \
                     created_by, created_at, updated_at, updated_by_node) \
                 VALUES ('job-foreign', ?1, 'org-other', 'host_probe', NULL, NULL, NULL, \
                     'test-node', 'failed', 0, '', '[]', 'cooperative', 0, NULL, NULL, NULL, \
                     'someone', 't', 't', 'x')",
                rusqlite::params![instance],
            )
            .expect("foreign job");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("host list") else {
            panic!("expected a host list");
        };
        assert_eq!(hosts.len(), 1, "only this organization's hosts");
        assert_eq!(hosts[0].host_id, LOCAL);

        let error = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance.clone(),
                host_id: "foreign".to_string(),
            },
        )
        .await
        .expect_err("another tenant's host must not resolve");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance.clone(),
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert!(jobs.is_empty(), "another tenant's jobs are not this list");

        let error = call(
            &ctx,
            P::JobGetRequest {
                instance_id: instance,
                job_id: "job-foreign".to_string(),
            },
        )
        .await
        .expect_err("another tenant's job must not resolve");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
    }

    /// A job belongs to ONE environment. Reading it from another must not work
    /// even inside the same organization — two environments are two tenants of
    /// the registry.
    #[tokio::test]
    async fn a_job_of_another_environment_is_invisible() {
        let state = AppState::for_test();
        let first = env(&state, "tentavm-aaaaaaaa");
        let second = env(&state, "tentavm-bbbbbbbb");
        job(&state, "job-1", &second, LOCAL, "failed");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::JobGetRequest {
                instance_id: first.clone(),
                job_id: "job-1".to_string(),
            },
        )
        .await
        .expect_err("the other environment's job is not here");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: first,
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert!(jobs.is_empty());
    }

    /// §15: the environment admin holds `manage` on every host, including ones
    /// no grant row mentions. A fresh environment has no grant rows at all, so
    /// reading the role out of the table alone tells the admin they have no
    /// access to their own machines.
    #[tokio::test]
    async fn the_environment_admin_manages_every_host() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        host(&state, REMOTE, REMOTE, "ready", REMOTE);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(hosts.len(), 2);
        for host in &hosts {
            assert_eq!(
                host.your_role, "manage",
                "the environment admin manages {} without a grant row",
                host.host_id
            );
        }
    }

    /// §15 makes a group a subject exactly like a user. Reading only the user
    /// rows reports "no access" to someone who has access through their team.
    #[tokio::test]
    async fn a_grant_held_through_a_group_is_reported() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO user_groups (id, name, description) VALUES ('grp-ops', 'Ops', '')",
                [],
            )
            .expect("group");
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash) VALUES (?1, ?1, 'x')",
                rusqlite::params![USER],
            )
            .expect("account");
            conn.execute(
                "INSERT INTO group_members (group_id, user_id) VALUES ('grp-ops', ?1)",
                rusqlite::params![USER],
            )
            .expect("membership");
            conn.execute(
                "INSERT INTO vm_host_grants (instance_id, host_id, subject_kind, subject_id, \
                     org_id, role, granted_by, created_at, updated_at, updated_by_node) \
                 VALUES (?1, ?2, 'group', 'grp-ops', ?3, 'deploy', 'admin', 't', 't', 'x')",
                rusqlite::params![instance, LOCAL, ORG],
            )
            .expect("group grant");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(hosts[0].your_role, "deploy", "a team grant is a grant");
    }

    /// The state filter has to run inside the query. Applied to an already
    /// truncated page it answers "none" for a caller asking about failures that
    /// are simply older than the page.
    #[tokio::test]
    async fn a_state_filter_reaches_past_the_page() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        job(&state, "job-old-failed", &instance, LOCAL, "failed");
        for n in 0..5 {
            job(&state, &format!("job-new-{n}"), &instance, LOCAL, "succeeded");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: vec!["failed".to_string()],
                limit: 3,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert_eq!(
            jobs.len(),
            1,
            "the failed job must be found behind newer rows, not filtered out of the page"
        );
        assert_eq!(jobs[0].job_id, "job-old-failed");
    }

    /// `inbox_total` is the count of everything, not of what fits. The tile
    /// says "3 of 47" and cannot do that from a truncated list. The page must
    /// also be shared: one noisy source may not push the other out entirely.
    #[tokio::test]
    async fn the_inbox_reports_the_whole_count_and_shares_the_page() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        for n in 0..25 {
            host(
                &state,
                &format!("dead-{n:02}"),
                &format!("node-{n:02}"),
                "unreachable",
                &format!("node-{n:02}"),
            );
        }
        job(&state, "job-failed", &instance, LOCAL, "failed");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.inbox_total, 26, "25 hosts plus one failed job");
        assert_eq!(summary.inbox.len(), INBOX_PAGE);
        assert!(
            summary.inbox.iter().any(|i| i.kind == "job_failed"),
            "a failed job must not be pushed out by unreachable hosts"
        );
    }

    /// `VmJob.host_name` is documented as joined by the answering node: a
    /// browser holds no registry to join against.
    #[tokio::test]
    async fn a_job_carries_the_name_of_its_host() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET target_host_id = ?1 WHERE 1 = 0",
                rusqlite::params![LOCAL],
            )
            .ok();
        }
        job(&state, "job-1", &instance, LOCAL, "running");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET target_host_id = ?1 WHERE id = 'job-1'",
                rusqlite::params![LOCAL],
            )
            .expect("attach host");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert_eq!(jobs[0].host_name, format!("host {LOCAL}"));
    }

    /// A host row whose owner column is blank is not an address. Sending the
    /// request "to nobody" would hang on the mesh timeout instead of saying
    /// what is wrong.
    #[tokio::test]
    async fn a_host_without_an_owner_is_not_routed() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        super::super::app_gate::test_support::grant(
            &state,
            &instance,
            USER,
            PERM_HOSTS_MANAGE,
        );
        host(&state, "orphan", "orphan-node", "ready", "");
        may_manage_host(&state, &instance, "orphan");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: "orphan".to_string(),
                refresh: false,
            },
        )
        .await
        .expect_err("a blank owner is not a node id");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
    }

    /// The four families that have no mechanism yet refuse with a typed error
    /// and only AFTER the gate — an unauthorized caller must not learn which
    /// operations exist by getting a different answer than a authorized one.
    #[tokio::test]
    async fn unbuilt_operations_refuse_after_the_gate() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        let ctx = ctx_for(&state);

        // `vm.read` only: the three admin operations must stop at the gate.
        for body in [
            P::HostGrantsListRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
            },
            P::HostGrantsSetRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                grants: Vec::new(),
            },
            P::SettingsSetRequest {
                instance_id: instance.clone(),
                settings: Default::default(),
            },
        ] {
            let error = call(&ctx, body).await.expect_err("vm.admin required");
            assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        }

        // Deferring an item needs only `vm.read`, so it reaches the refusal.
        let error = call(
            &ctx,
            P::InboxSnoozeRequest {
                instance_id: instance.clone(),
                item_id: "job_failed:job-1".to_string(),
                snooze_secs: 86_400,
            },
        )
        .await
        .expect_err("no storage for a deferral yet");
        assert_eq!(error.code, ProtocolErrorCode::NotImplemented);

        // With `vm.admin` the gate opens and the request reaches the write —
        // which then refuses for a DIFFERENT reason, and the reason is the one
        // §7.1 gives: this fixture's node is on nobody's operator list, so the
        // environment policy it would write could never replicate.
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        let error = call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance.clone(),
                settings: tentaflow_protocol::tentavm::VmInstanceSettings {
                    visibility: "all".to_string(),
                    default_firmware: "uefi".to_string(),
                    ..Default::default()
                },
            },
        )
        .await
        .expect_err("a non-operator node may not write environment policy");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);

        // And with the node on the list it lands.
        operator_node(&state, LOCAL);
        call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance,
                settings: tentaflow_protocol::tentavm::VmInstanceSettings {
                    visibility: "granted".to_string(),
                    default_firmware: "uefi".to_string(),
                    ..Default::default()
                },
            },
        )
        .await
        .expect("an operator node writes environment policy");
    }

    /// Two grants on one host — one through the user, one through a team —
    /// resolve to the STRONGER. Reporting the weaker one would tell an operator
    /// they cannot do what they can.
    #[tokio::test]
    async fn the_strongest_grant_wins() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash) VALUES (?1, ?1, 'x')",
                rusqlite::params![USER],
            )
            .expect("account");
            conn.execute(
                "INSERT INTO user_groups (id, name, description) VALUES ('grp-ops', 'Ops', '')",
                [],
            )
            .expect("group");
            conn.execute(
                "INSERT INTO group_members (group_id, user_id) VALUES ('grp-ops', ?1)",
                rusqlite::params![USER],
            )
            .expect("membership");
            for (kind, subject, role) in [
                ("user", USER, "view"),
                ("group", "grp-ops", "manage"),
            ] {
                conn.execute(
                    "INSERT INTO vm_host_grants (instance_id, host_id, subject_kind, \
                         subject_id, org_id, role, granted_by, created_at, updated_at, \
                         updated_by_node) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'admin', 't', 't', 'x')",
                    rusqlite::params![instance, LOCAL, kind, subject, ORG, role],
                )
                .expect("grant");
            }
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(hosts[0].your_role, "manage");
    }

    /// Reading is not operating. `vm.read` must not reach the probe or the
    /// cancel — both act on hardware and jobs, and §15 puts them behind
    /// `vm.hosts.manage`.
    #[tokio::test]
    async fn reading_does_not_authorize_acting() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        job(&state, "job-1", &instance, LOCAL, "running");
        let ctx = ctx_for(&state);

        for body in [
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                refresh: true,
            },
            P::JobCancelRequest {
                instance_id: instance.clone(),
                job_id: "job-1".to_string(),
            },
        ] {
            let error = call(&ctx, body).await.expect_err("vm.hosts.manage required");
            assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        }
    }

    /// A node with no host row of its own answers `unknown`, not a status it
    /// made up: P00 draws its onboarding from this field, and "ready" would
    /// send the user to a dashboard for a host that does not exist.
    #[tokio::test]
    async fn a_node_without_a_host_row_reports_unknown() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, REMOTE, REMOTE, "ready", REMOTE);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(summary.local_host_id.is_none());
        assert_eq!(summary.local_host_status, "unknown");
    }

    /// Machine counters and settings are read per organization too — the same
    /// rule as hosts and jobs, on the two queries that do not go through the
    /// listing helpers.
    #[tokio::test]
    async fn counters_and_settings_stay_within_the_organization() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_guests (id, instance_id, org_id, host_id, kind, engine, name, \
                     external_ref, spec_json, desired_state, observed_state, observed_at, \
                     is_template, template_of, tags_json, owner_user_id, notes, \
                     autostart_order, autostart_delay_s, deleted_at, delete_expires_at, \
                     created_at, updated_at, updated_by_node) \
                 VALUES ('g-foreign', ?1, 'org-other', 'h', 'vm', 'kvm', 'foreign', NULL, \
                     '{}', 'running', 'running', NULL, 0, NULL, '[]', 'someone', '', \
                     NULL, NULL, NULL, NULL, 't', 't', 'x')",
                rusqlite::params![instance],
            )
            .expect("foreign guest");
            conn.execute(
                "INSERT INTO vm_instance_settings (instance_id, key, org_id, value, \
                     created_at, updated_at, updated_by_node) \
                 VALUES (?1, 'visibility', 'org-other', 'granted', 't', 't', 'x')",
                rusqlite::params![instance],
            )
            .expect("foreign setting");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary") else {
            panic!("expected a summary");
        };
        assert_eq!(summary.guests_total, 0, "another tenant's machines are not counted");

        let MessageBody::TentaVmBody(P::SettingsGetResponse { settings, .. }) =
            call(&ctx, P::SettingsGetRequest { instance_id: instance })
                .await
                .expect("settings")
        else {
            panic!("expected settings");
        };
        assert_eq!(
            settings.visibility, "all",
            "another tenant's setting must not configure this environment"
        );
    }

    /// `limit = 0` means "the node decides", and the node's answer is a usable
    /// page — not one row, and not the whole table.
    #[tokio::test]
    async fn an_omitted_limit_returns_a_usable_page() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        for n in 0..120 {
            job(&state, &format!("job-{n:03}"), &instance, LOCAL, "succeeded");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert_eq!(jobs.len(), 100, "the node's own page, not one row and not all 120");
    }

    /// A host id is a node id or a connector id — unique per fleet, not per
    /// tenant — so the join that puts a host NAME on a job has to be scoped by
    /// organization too. Without it a job of this tenant, naming a host id that
    /// also exists in another one, carries that tenant's display name.
    #[tokio::test]
    async fn a_host_name_never_comes_from_another_organization() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES ('shared-id', 'org-other', 'node', 'shared-id', NULL, NULL, \
                     'SECRET FLEET NAME', '[]', '[]', 'ready', 'shared-id', 0, 't', 't', 'x')",
                [],
            )
            .expect("foreign host");
        }
        job(&state, "job-1", &instance, LOCAL, "failed");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET target_host_id = 'shared-id' WHERE id = 'job-1'",
                [],
            )
            .expect("point the job at the shared id");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance.clone(),
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert_eq!(
            jobs[0].host_name, "",
            "a host of another organization has no name here"
        );

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        let item = summary
            .inbox
            .iter()
            .find(|i| i.kind == "job_failed")
            .expect("the failed job is in the inbox");
        assert_eq!(item.host_name, "", "and not in the inbox either");
    }

    /// A job of another environment must not be ROUTED either. The earlier test
    /// covers a locally-owned job, where both a working and a broken
    /// `instance_id` filter end in the same `NotFound`; only a job owned by
    /// ANOTHER node tells the two apart — a filter that does not bite sends the
    /// caller's request to that node.
    #[tokio::test]
    async fn a_remote_job_of_another_environment_is_not_routed() {
        let state = AppState::for_test();
        let first = env(&state, "tentavm-aaaaaaaa");
        let second = env(&state, "tentavm-bbbbbbbb");
        job(&state, "job-remote", &second, REMOTE, "running");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::JobGetRequest {
                instance_id: first,
                job_id: "job-remote".to_string(),
            },
        )
        .await
        .expect_err("the other environment's job is not here");
        assert_eq!(
            error.code,
            ProtocolErrorCode::NotFound,
            "a job of another environment must not be routed to its owner"
        );
    }

    /// What is stored has to come back. Two existing tests read defaults and
    /// another tenant's row, and both pass just as well when the table read is
    /// dead — this one writes into the caller's OWN environment and demands the
    /// values return.
    #[tokio::test]
    async fn stored_settings_reach_the_caller() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            for (key, value) in [
                ("visibility", "granted"),
                ("ha_enabled", "1"),
                ("overcommit_ratio", "2.5"),
                ("machine_type", "pc-q35-8.2"),
            ] {
                conn.execute(
                    "INSERT INTO vm_instance_settings (instance_id, key, org_id, value, \
                         created_at, updated_at, updated_by_node) \
                     VALUES (?1, ?2, ?3, ?4, 't', 't', 'x')",
                    rusqlite::params![instance, key, ORG, value],
                )
                .expect("setting");
            }
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SettingsGetResponse { settings, .. }) = call(
            &ctx,
            P::SettingsGetRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("settings") else {
            panic!("expected settings");
        };
        assert_eq!(settings.visibility, "granted");
        assert!(settings.ha_enabled);
        assert!((settings.overcommit_ratio - 2.5).abs() < f64::EPSILON);
        assert_eq!(settings.machine_type, "pc-q35-8.2");

        // …and the same value reaches the host list, which is where the browser
        // reads it to explain a short list.
        let MessageBody::TentaVmBody(P::HostsListResponse { visibility, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(visibility, "granted");
    }

    /// "Online" means connected NOW, not "known to the fleet". The host card
    /// draws its actions from this, and a host that is merely remembered would
    /// offer buttons that fail on the wire.
    #[tokio::test]
    async fn online_means_connected_right_now() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, REMOTE, REMOTE, "ready", REMOTE);
        host(&state, "sleeping", "sleeping-node", "ready", "sleeping-node");
        state.mesh_peer_store.set_quic_connected(REMOTE, true);
        state
            .mesh_peer_store
            .set_quic_connected("sleeping-node", false);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        let connected = hosts.iter().find(|h| h.host_id == REMOTE).expect("peer");
        assert!(connected.online, "a connected peer is online");
        assert_eq!(connected.status_reason.key, "host.ready");
        let sleeping = hosts
            .iter()
            .find(|h| h.host_id == "sleeping")
            .expect("known but disconnected");
        assert!(
            !sleeping.online,
            "a peer the store remembers but is not connected to is offline"
        );
        assert_eq!(sleeping.status_reason.key, "host.offline");
    }

    /// The four descriptive fields the dashboard renders straight from the row.
    /// They are cheap to get wrong and invisible until a screen draws them.
    #[tokio::test]
    async fn the_row_fields_the_dashboard_draws_are_carried_through() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        job(&state, "job-1", &instance, LOCAL, "failed");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET progress_pct = 42 WHERE id = 'job-1'",
                [],
            )
            .expect("progress");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("host list") else {
            panic!("expected a host list");
        };
        assert_eq!(
            hosts[0].status_reason.key, "host.needs_install",
            "a host waiting for its environment says so, not 'ready'"
        );

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance.clone(),
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list") else {
            panic!("expected a job list");
        };
        assert_eq!(jobs[0].progress_pct, 42);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.local_host_id.as_deref(), Some(LOCAL));
        let item = summary
            .inbox
            .iter()
            .find(|i| i.kind == "job_failed")
            .expect("failed job");
        assert_eq!(item.requested_by, USER, "who asked is drawn on P01");
    }

    /// Redirects the instance-data root for the duration of one test and puts it
    /// back afterwards, panic or not. `paths::orgs_dir()` reads this override on
    /// every call (an `RwLock`, not a `OnceLock`), so this is what keeps a unit
    /// test that opens an instance database out of the repository's own runtime
    /// directory.
    ///
    /// The override is PROCESS-WIDE, and `cargo test` runs these in parallel:
    /// without the lock, one test's `Drop` clears the redirect while another is
    /// still using it, and the second silently reads the real `.runtime/` —
    /// where it finds nothing, so it fails as "the probe cache is empty" rather
    /// than as the race it is.
    ///
    /// The lock is `paths::lock_category_overrides`, NOT a mutex of this
    /// module. A guard private to one test module only serializes that module
    /// against itself: this file used to hold its own, and
    /// `dispatch::code_studio::tests::fixture` redirects the SAME category with
    /// no lock at all, which is why these tests passed alone and failed beside
    /// it. One global needs one lock, and it lives next to the global.
    struct InstanceDataRoot {
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl InstanceDataRoot {
        fn redirect() -> Self {
            let guard = crate::paths::lock_category_overrides();
            let dir = tempfile::tempdir().expect("tempdir");
            crate::paths::set_category_override(
                crate::paths::StorageCategory::AddonData,
                Some(dir.path().to_string_lossy().into_owned()),
            );
            Self {
                _dir: dir,
                _guard: guard,
            }
        }
    }

    impl Drop for InstanceDataRoot {
        fn drop(&mut self) {
            crate::paths::set_category_override(crate::paths::StorageCategory::AddonData, None);
            // `app_db` caches one pool per instance id for the LIFE OF THE
            // PROCESS, and that pool outlives this tempdir. Nothing here can
            // close it — the guard does not know which ids the test opened —
            // so every test that opens an instance database uses an id of its
            // own, and this comment is why.
        }
    }

    /// A job's log is read from the instance database of the node that ran it,
    /// and it is the log of THAT job. The instance database is shared by every
    /// job of the environment, so a missing `job_id` predicate hands the caller
    /// the log of every job on this node — including ones they asked nothing
    /// about.
    #[tokio::test]
    async fn a_job_log_carries_only_that_job() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        // A fresh instance id per test: `app_db` caches one pool per id for the
        // life of the process, so reusing an id would reuse another test's
        // database.
        let instance = env(&state, "tentavm-log00001");
        job(&state, "job-mine", &instance, LOCAL, "failed");
        job(&state, "job-theirs", &instance, LOCAL, "failed");
        {
            let db = crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
            let conn = db.write().unwrap();
            for (job_id, seq, line) in [
                ("job-mine", 1, "mine one"),
                ("job-mine", 2, "mine two"),
                ("job-theirs", 1, "SECRET OTHER JOB"),
            ] {
                conn.execute(
                    "INSERT INTO vm_job_logs (job_id, seq, at, level, line) \
                     VALUES (?1, ?2, 't', 'info', ?3)",
                    rusqlite::params![job_id, seq, line],
                )
                .expect("log line");
            }
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobGetResponse { job, log }) = call(
            &ctx,
            P::JobGetRequest {
                instance_id: instance,
                job_id: "job-mine".to_string(),
            },
        )
        .await
        .expect("job get") else {
            panic!("expected a job");
        };
        assert_eq!(job.job_id, "job-mine");
        let lines: Vec<&str> = log.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            lines,
            vec!["mine one", "mine two"],
            "another job's log is not part of this answer"
        );
    }

    // =========================================================================
    // The environment probe (step 8)
    // =========================================================================

    use crate::tentavm::probe::{CachedProbe, HostHardware, HostProbe};
    use tentaflow_protocol::tentavm::{VmHostEnvironment, VmVirtSupport};

    /// A probe result with recognisable numbers, for the paths that must carry
    /// one without running the real thing.
    fn measured(host_status: &str) -> HostProbe {
        let features = vec![tentaflow_protocol::features::FeatureState {
            id: "kvm_base".to_string(),
            status: if host_status == "ready" { "ok" } else { "missing" }.to_string(),
            detail: if host_status == "ready" {
                String::new()
            } else {
                "missing: swtpm".to_string()
            },
            ..Default::default()
        }];
        HostProbe {
            environment: VmHostEnvironment {
                platform: "linux".to_string(),
                full_support: true,
                virt: VmVirtSupport {
                    hardware_virtualization: host_status != "unsupported",
                    cpu_flag: "svm".to_string(),
                    detail: if host_status == "unsupported" {
                        text("host.virt.no_hardware")
                    } else {
                        text("")
                    },
                    ..Default::default()
                },
                features,
                // The engine the consent path is about, in the state a probe
                // leaves it in before anybody accepted its root-equivalence.
                engines: vec![VmEngine {
                    id: "kvm".to_string(),
                    status: "needs_consent".to_string(),
                    kinds: vec!["vm".to_string()],
                    consent_required: true,
                    consent_granted: false,
                    ..Default::default()
                }],
                probed_at: "2026-01-01T00:00:00Z".to_string(),
                ..Default::default()
            },
            hardware: HostHardware {
                os_name: "CachyOS".to_string(),
                os_version: "rolling".to_string(),
                arch: "x86_64".to_string(),
                cpu_cores: 24,
                cpu_used_pct: 42.5,
                ram_bytes: 64 * 1024 * 1024 * 1024,
                ram_used_bytes: 37 * 1024 * 1024 * 1024,
                storage_bytes: 2_000_000_000_000,
                storage_used_bytes: 600_000_000_000,
            },
        }
    }

    /// Seeds this node's probe cache for `host_id` and returns the instance
    /// database, so a test can age the row afterwards.
    fn seed_probe(state: &Arc<AppState>, instance: &str, host_id: &str, probe: &HostProbe) -> crate::db::DbPool {
        let db = crate::tentavm::open_db(&state.db, ORG, instance).expect("instance db");
        crate::tentavm::probe::store(&db, host_id, probe).expect("store probe");
        db
    }

    /// The whole local path in one test: the probe runs on the node that owns
    /// the hardware, its verdict lands on the REPLICATED host row (status,
    /// engines, capabilities) and the full result — hardware readings included
    /// — stays in the node-local cache, which the second call answers from.
    #[tokio::test]
    async fn a_probe_writes_the_host_row_and_then_answers_from_the_cache() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe001");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        may_manage_host(&state, &instance, LOCAL);
        let ctx = ctx_for(&state);

        call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                refresh: true,
            },
        )
        .await
        .expect("probe");

        let (status, engines_json, capabilities_json, updated_by): (String, String, String, String) = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status, engines_json, capabilities_json, updated_by_node \
                 FROM vm_hosts WHERE id = ?1",
                rusqlite::params![LOCAL],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
        };
        assert!(
            matches!(status.as_str(), "ready" | "needs_install" | "unsupported"),
            "unexpected probed status {status}"
        );
        assert_eq!(updated_by, LOCAL, "the owner is what wrote the row");
        let engines: Vec<VmEngine> = serde_json::from_str(&engines_json).expect("engines json");
        assert!(
            engines.iter().any(|engine| engine.id == "kvm"),
            "the host card draws an engine chip per driver of §5.1"
        );
        let capabilities: Vec<VmCapability> =
            serde_json::from_str(&capabilities_json).expect("capabilities json");
        assert!(capabilities.iter().any(|cap| cap.id == "snapshot_revert"));

        // The node-local half: the cache carries the hardware readings, which
        // have no field anywhere on the wire.
        let db = crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        let cached = crate::tentavm::probe::cached(&db, LOCAL).expect("cached probe");
        assert!(!cached.expired);
        assert!(
            cached.probe.hardware.ram_bytes > 0,
            "this machine has memory and the probe read it"
        );

        // …and the second call without `refresh` answers from it. Proven by
        // planting a value the probe could never produce.
        {
            let mut planted = cached.probe.clone();
            planted.environment.hostname = "PLANTED BY THE CACHE".to_string();
            crate::tentavm::probe::store(&db, LOCAL, &planted).expect("re-store");
        }
        let MessageBody::TentaVmBody(P::HostProbeResponse { environment, .. }) = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                refresh: false,
            },
        )
        .await
        .expect("cached probe")
        else {
            panic!("expected a probe result");
        };
        assert_eq!(
            environment.hostname, "PLANTED BY THE CACHE",
            "without `refresh` the owner may answer from vm_probe_cache"
        );

        let MessageBody::TentaVmBody(P::HostProbeResponse { environment, .. }) = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: LOCAL.to_string(),
                refresh: true,
            },
        )
        .await
        .expect("refreshed probe")
        else {
            panic!("expected a probe result");
        };
        assert_ne!(
            environment.hostname, "PLANTED BY THE CACHE",
            "`refresh` means measure again, whatever is stored"
        );
    }

    /// A probe answer is about THIS machine's /proc and /sys. A connector host
    /// is another hypervisor reached over its API, so describing it with this
    /// node's readings would attach one machine's environment to another's
    /// card.
    #[tokio::test]
    async fn a_connector_host_is_not_described_by_this_machine() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe002");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_connectors (id, org_id, kind, endpoint, display_name, tls_mode, \
                     auth_kind, status, last_probe_at, owner_node_id, owner_epoch, created_at, \
                     updated_at, updated_by_node) \
                 VALUES ('conn-1', ?1, 'proxmox', 'https://pve', 'PVE', 'strict', 'token', \
                     'ready', NULL, ?2, 0, 't', 't', ?2)",
                rusqlite::params![ORG, LOCAL],
            )
            .expect("connector");
            conn.execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES ('pve-1', ?1, 'connector_host', NULL, 'conn-1', 'node/pve1', 'PVE 1', \
                     '[]', '[]', 'unknown', ?2, 0, 't', 't', ?2)",
                rusqlite::params![ORG, LOCAL],
            )
            .expect("connector host");
        }
        may_manage_host(&state, &instance, "pve-1");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: "pve-1".to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("this machine cannot answer for a Proxmox node");
        assert_eq!(error.code, ProtocolErrorCode::NotImplemented);
    }

    /// The hardware columns of a host card come from the probe of THAT host.
    /// This node has one for itself and for nobody else, so a remote row keeps
    /// its zeros instead of being drawn with the local machine's numbers.
    #[tokio::test]
    async fn the_local_host_card_carries_what_the_probe_measured() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe003");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        host(&state, REMOTE, REMOTE, "ready", REMOTE);
        seed_probe(&state, &instance, LOCAL, &measured("ready"));
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("host list")
        else {
            panic!("expected a host list");
        };
        let local = hosts.iter().find(|h| h.host_id == LOCAL).expect("local host");
        assert_eq!(local.cpu_cores, 24);
        assert_eq!(local.ram_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(local.cpu_used_pct, 42.5);
        assert_eq!(local.os_name, "CachyOS");
        assert_eq!(local.os_version, "rolling");
        assert_eq!(local.arch, "x86_64");
        assert_eq!(local.storage_bytes, 2_000_000_000_000);

        let remote = hosts.iter().find(|h| h.host_id == REMOTE).expect("remote host");
        assert_eq!(
            (remote.cpu_cores, remote.ram_bytes, remote.storage_bytes),
            (0, 0, 0),
            "another node's hardware is not this node's probe"
        );
        assert!(remote.os_name.is_empty());

        // H02 shows the probe itself, and only for the host it belongs to.
        let MessageBody::TentaVmBody(P::HostGetResponse { host, environment }) = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
            },
        )
        .await
        .expect("host get")
        else {
            panic!("expected a host");
        };
        assert_eq!(host.cpu_cores, 24);
        assert_eq!(
            environment.expect("the local host has been probed").probed_at,
            "2026-01-01T00:00:00Z"
        );

        let MessageBody::TentaVmBody(P::HostGetResponse { environment, .. }) = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance,
                host_id: REMOTE.to_string(),
            },
        )
        .await
        .expect("host get")
        else {
            panic!("expected a host");
        };
        assert!(
            environment.is_none(),
            "the probe of a remote host lives in ITS owner's database, and H02 \
             draws 'Sonduj' rather than this machine's readings"
        );
    }

    /// An old probe still says how big the machine is — a host does not grow
    /// cores while nobody looks — but it stops saying how busy it is. There is
    /// no field on `VmHost` for "measured at", so a stale percentage would be
    /// drawn as the current one.
    #[tokio::test]
    async fn an_expired_probe_sizes_the_host_but_stops_reporting_load() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe004");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let db = seed_probe(&state, &instance, LOCAL, &measured("ready"));
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE vm_probe_cache SET expires_at = '2020-01-01T00:00:00Z'",
                [],
            )
            .expect("age the row");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        let local = hosts.iter().find(|h| h.host_id == LOCAL).expect("local host");
        assert_eq!(local.cpu_cores, 24, "capacity survives the expiry");
        assert_eq!(local.ram_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(local.cpu_used_pct, 0.0, "utilization does not");
        assert_eq!(local.ram_used_bytes, 0);
        assert_eq!(local.storage_used_bytes, 0);
    }

    /// P00's "brakuje: …" and its unsupported state both come from this node's
    /// own probe, and the reason is reserved for what no install can fix.
    #[tokio::test]
    async fn the_summary_reports_what_the_local_probe_found() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe005");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        seed_probe(&state, &instance, LOCAL, &measured("needs_install"));
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.local_missing_features, vec!["kvm_base".to_string()]);
        assert_eq!(
            summary.local_unsupported_reason.key, "",
            "a host that only needs packages is not unsupported"
        );

        // The same environment on a machine without hardware virtualization.
        let db = crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        crate::tentavm::probe::store(&db, LOCAL, &measured("unsupported")).expect("store");
        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.local_unsupported_reason.key, "host.virt.no_hardware");
    }

    /// A dashboard read must not bring a database into existence. It runs on
    /// every page render and on nodes where the environment was never
    /// initialized; `app_db::open` creates the file, and a read that called it
    /// would leave one behind — in production and, worse, in the repository's
    /// own runtime directory during a test run.
    #[tokio::test]
    async fn a_dashboard_read_creates_no_instance_database() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-probe006");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        let ctx = ctx_for(&state);

        for body in [
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
            P::HostsListRequest {
                instance_id: instance.clone(),
            },
            P::HostGetRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
            },
        ] {
            call(&ctx, body).await.expect("a read answers without a probe");
        }

        // The file itself, not the tree: installing the instance legitimately
        // creates its directory, and other tests of this module run in
        // parallel under the same redirected root.
        let db_file = crate::addon::fs_sandbox::addon_data_dir_path(ORG, &instance)
            .expect("instance path")
            .join("tentavm.db");
        assert!(
            !db_file.exists(),
            "a dashboard read brought an instance database into existence at {}",
            db_file.display()
        );
    }

    /// The registry row of a host is written by the node that OWNS it. The
    /// row replicates, so a node able to overwrite another's engines would be
    /// publishing a description of hardware it has never seen.
    #[test]
    fn a_probe_cannot_rewrite_a_host_row_this_node_does_not_own() {
        let state = AppState::for_test();
        host(&state, REMOTE, REMOTE, "needs_install", REMOTE);
        let probe = measured("ready");

        crate::tentavm::probe::apply_to_registry(&state.db, ORG, REMOTE, LOCAL, &probe)
            .expect("the write runs, it just must not match a row");
        let (status, by): (String, String) = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status, updated_by_node FROM vm_hosts WHERE id = ?1",
                rusqlite::params![REMOTE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(status, "needs_install", "another node's probe did not land");
        assert_eq!(by, REMOTE);

        // The owner's own probe does land.
        crate::tentavm::probe::apply_to_registry(&state.db, ORG, REMOTE, REMOTE, &probe)
            .expect("owner write");
        let status: String = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status FROM vm_hosts WHERE id = ?1",
                rusqlite::params![REMOTE],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(status, "ready");
    }

    /// §17.5 step 2 → 3: install, then open the app and read "brakuje: qemu,
    /// libvirt, swtpm". Nothing is clicked in between, so the probe has to
    /// have run by itself — and the status the fleet reads has to be the one
    /// it measured, not the placeholder `init` wrote.
    #[tokio::test]
    async fn a_fresh_install_can_write_p00s_sentence() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-onboard1");
        // What `native_init` does: the instance database and this node's host
        // row, with the pre-probe placeholder status.
        crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(
            !summary.local_missing_features.is_empty()
                || summary.local_host_status == "ready",
            "P00 has nothing to say: status={} missing={:?}",
            summary.local_host_status,
            summary.local_missing_features
        );
        assert_eq!(summary.local_host_id.as_deref(), Some(LOCAL));

        // The registry row now carries the probe's verdict, and the second
        // read answers from the cache instead of measuring again.
        let (status, engines): (String, String) = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status, engines_json FROM vm_hosts WHERE id = ?1",
                rusqlite::params![LOCAL],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            status,
            crate::tentavm::probe::host_status(
                &crate::tentavm::probe::cached_local(&state.db, ORG, &instance, LOCAL)
                    .expect("the read measured")
                    .probe
                    .environment
            ),
            "the row the fleet reads is the probe's verdict"
        );
        assert!(engines.contains("\"kvm\""), "{engines}");
    }

    /// The same read on a node where the environment was never initialized
    /// measures NOTHING and creates nothing — there is no instance database to
    /// cache into, and a read that made one would be creating state on a node
    /// that never installed the app.
    #[tokio::test]
    async fn a_read_on_an_uninitialized_node_measures_nothing() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-onboard2");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(summary.local_missing_features.is_empty());
        let db_file = crate::addon::fs_sandbox::addon_data_dir_path(ORG, &instance)
            .expect("instance path")
            .join("tentavm.db");
        assert!(!db_file.exists(), "{}", db_file.display());
    }

    /// Owning a host row is not being that host (§6.1: for `kind = 'node'` the
    /// rule is `node_id == actor_node_id`). After `switch_owner` this node can
    /// own another node's row — and everything the probe reads is THIS
    /// machine's /proc, so answering would put this hostname, this libvirt and
    /// this QEMU on somebody else's card.
    #[tokio::test]
    async fn this_node_probes_only_itself() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-ghost001");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        // A host of another node, whose row this node owns.
        host(&state, "ghost-node", "ghost-node", "unknown", LOCAL);
        may_manage_host(&state, &instance, "ghost-node");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: "ghost-node".to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("this machine is not that host");
        assert_eq!(error.code, ProtocolErrorCode::NotImplemented);

        let (status, engines): (String, String) = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status, engines_json FROM vm_hosts WHERE id = 'ghost-node'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(status, "unknown", "nothing was written to another node's row");
        assert_eq!(engines, "[]");
    }

    /// §17.5 step 5 → 6: the admin accepts the engine's root-equivalence and
    /// the job starts. The measurement stays cached, the decision does not —
    /// otherwise the card says `needs_consent` for up to ten minutes after the
    /// admin said yes.
    #[tokio::test]
    async fn a_consent_granted_now_shows_up_now() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-consent1");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        may_manage_host(&state, &instance, LOCAL);
        let db = seed_probe(&state, &instance, LOCAL, &measured("ready"));
        let ctx = ctx_for(&state);

        let engines = probe_engines(&ctx, &instance).await;
        assert_eq!(engine_status(&engines, "kvm"), "needs_consent");

        // What dialog D01 will write (step 10).
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO vm_host_settings (key, value, updated_at) \
                 VALUES ('engine_consent:kvm', '1', 't')",
                [],
            )
            .expect("consent");

        let engines = probe_engines(&ctx, &instance).await;
        assert_eq!(
            engine_status(&engines, "kvm"),
            "ready",
            "the consent is stored and the answer still comes from the cache"
        );

        // …and the dashboard read sees it too, from the same cache.
        let MessageBody::TentaVmBody(P::HostGetResponse { host, environment }) = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance,
                host_id: LOCAL.to_string(),
            },
        )
        .await
        .expect("host get")
        else {
            panic!("expected a host");
        };
        assert_eq!(
            engine_status(&environment.expect("probed").engines, "kvm"),
            "ready"
        );
        // …and so does the card in H01, which draws its chips from the host
        // row rather than from the environment.
        assert_eq!(engine_status(&host.engines, "kvm"), "ready");
    }

    /// `HostProbeRequest { refresh: false }`, answered from the cache.
    async fn probe_engines(ctx: &HandlerContext, instance: &str) -> Vec<VmEngine> {
        let MessageBody::TentaVmBody(P::HostProbeResponse { environment, .. }) = call(
            ctx,
            P::HostProbeRequest {
                instance_id: instance.to_string(),
                host_id: LOCAL.to_string(),
                refresh: false,
            },
        )
        .await
        .expect("probe")
        else {
            panic!("expected a probe result");
        };
        environment.engines
    }

    fn engine_status(engines: &[VmEngine], id: &str) -> String {
        engines
            .iter()
            .find(|engine| engine.id == id)
            .unwrap_or_else(|| panic!("engine {id}"))
            .status
            .clone()
    }

    /// The two writes of one probe land in two different databases, so no
    /// transaction covers both. The rule that replaces one is an ORDER, and
    /// this is the state that proves it: an instance database exists (init ran
    /// here) but no host row does — which is exactly what `native_init` leaves
    /// behind on a node with no mesh identity, and it says so in its own log.
    ///
    /// Caching a measurement then would be the worst of both: the local
    /// dashboard looks complete, every other node draws this host with empty
    /// engines, and the fresh cache stops anything from ever retrying.
    #[tokio::test]
    async fn nothing_is_cached_that_the_fleet_never_received() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-atomic01");
        crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        // …and deliberately NO `vm_hosts` row.
        let ctx = ctx_for(&state);

        for _ in 0..2 {
            let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
                &ctx,
                P::SummaryRequest {
                    instance_id: instance.clone(),
                },
            )
            .await
            .expect("summary")
            else {
                panic!("expected a summary");
            };
            assert!(
                summary.local_missing_features.is_empty(),
                "P00 spoke about a host the registry does not have"
            );
            assert_eq!(summary.local_host_status, "unknown");
        }
        assert!(
            crate::tentavm::probe::cached_local(&state.db, ORG, &instance, LOCAL).is_none(),
            "a measurement was cached although no registry row carries it — \
             and a fresh cache is what stops the next read from retrying"
        );

        // The identity arrives, the row appears, and the very next read
        // publishes BOTH halves.
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_ne!(summary.local_host_status, "unknown", "the row is there now");
        assert!(
            crate::tentavm::probe::cached_local(&state.db, ORG, &instance, LOCAL).is_some(),
            "nothing retried after the row appeared"
        );
        let engines: String = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT engines_json FROM vm_hosts WHERE id = ?1",
                rusqlite::params![LOCAL],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(engines.contains("\"kvm\""), "{engines}");
    }

    /// The half of K1 that nobody watches by looking at an answer: `init`
    /// schedules a probe and nothing waits for it. The only way to observe a
    /// fire-and-forget task is to wait for what it writes — and waiting is
    /// also what keeps it from writing after this test's data root is gone.
    #[tokio::test]
    async fn init_schedules_a_probe_nobody_asked_for() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-sched001");
        crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);

        assert!(
            crate::tentavm::probe::schedule_local_probe(&state.db, ORG, &instance, LOCAL),
            "there is a runtime here, so the probe has to be scheduled on it"
        );

        // Generous: the probe takes a fifth of a second on this machine, and a
        // deadline that fails loudly beats a sleep that hides a regression.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if crate::tentavm::probe::cached_local(&state.db, ORG, &instance, LOCAL).is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the scheduled probe never ran"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let status: String = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT status FROM vm_hosts WHERE id = ?1",
                rusqlite::params![LOCAL],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(
            matches!(status.as_str(), "ready" | "needs_install" | "unsupported"),
            "the scheduled probe published its verdict: {status}"
        );
    }

    /// The other outcome, and it is not an error: an install running on a
    /// blocking thread has no runtime to schedule on. It says so instead of
    /// pretending, and the dashboard read is then the only trigger left.
    #[test]
    fn without_a_runtime_the_schedule_says_it_did_nothing() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-sched002");
        assert!(!crate::tentavm::probe::schedule_local_probe(
            &state.db, ORG, &instance, LOCAL
        ));
    }

    /// One rule, one implementation. The answer to `refresh = true` used to
    /// come straight out of the measurement while every cached read had the
    /// consent re-applied — two paths, one of them untested, and they were
    /// free to drift.
    #[tokio::test]
    async fn the_measured_answer_and_the_cached_one_agree_about_consent() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-fold0001");
        let db = crate::tentavm::open_db(&state.db, ORG, &instance).expect("instance db");
        host(&state, LOCAL, LOCAL, "needs_install", LOCAL);
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO vm_host_settings (key, value, updated_at) \
                 VALUES ('engine_consent:kvm', '1', 't')",
                [],
            )
            .expect("consent");

        let fresh = crate::tentavm::probe::refresh_local_probe(&state.db, ORG, &instance, LOCAL)
            .await
            .expect("probe");
        let cached = crate::tentavm::probe::cached_local(&state.db, ORG, &instance, LOCAL)
            .expect("stored")
            .probe;
        assert_eq!(
            fresh.environment.engines, cached.environment.engines,
            "the measured answer and the cached one disagree about consent"
        );
        let kvm = fresh
            .environment
            .engines
            .iter()
            .find(|engine| engine.id == "kvm")
            .expect("kvm");
        assert!(
            kvm.consent_granted,
            "the consent was stored before the measurement and has to be in its answer"
        );
    }

    // =========================================================================
    // The ten invariants U16 could not reach until something wrote these rows
    // =========================================================================

    /// A host row of another tenant, named by its id. `vm_hosts.id` is a node
    /// id — unique per fleet, not per tenant — so without the organization
    /// predicate the probe of a host this caller may not see would be
    /// forwarded to its owner, which is a request they never had the right to
    /// send. (U16 / X_OHORG, attack A35.)
    #[tokio::test]
    async fn a_host_of_another_organization_is_neither_probed_nor_forwarded() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES ('foreign-node', 'org-other', 'node', 'foreign-node', NULL, NULL, \
                     'not yours', '[]', '[]', 'ready', 'foreign-node', 0, 't', 't', 'x')",
                [],
            )
            .expect("foreign host");
        }
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: "foreign-node".to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("a host of another tenant is not in this environment");
        assert_eq!(
            error.code,
            ProtocolErrorCode::NotFound,
            "NotAvailable here would mean the request left this node for a \
             host the caller may not see"
        );
    }

    /// The same predicate one floor down, for jobs. A job row of another
    /// tenant carrying THIS caller's instance id must not be routed to its
    /// owner. (U16 / X_OJORG, attack A34.)
    #[tokio::test]
    async fn a_job_of_another_organization_is_not_routed_to_its_owner() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, source_host_id, \
                     target_host_id, owner_node_id, state, progress_pct, phase, steps_json, \
                     cancel_semantics, resume_after_restart, error, started_at, finished_at, \
                     created_by, created_at, updated_at, updated_by_node) \
                 VALUES ('job-foreign', ?1, 'org-other', 'host_probe', NULL, NULL, NULL, ?2, \
                     'running', 0, '', '[]', 'cooperative', 0, NULL, NULL, NULL, 'someone', \
                     't', 't', ?2)",
                rusqlite::params![instance, REMOTE],
            )
            .expect("foreign job");
        }
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        for body in [
            P::JobGetRequest {
                instance_id: instance.clone(),
                job_id: "job-foreign".to_string(),
            },
            P::JobCancelRequest {
                instance_id: instance.clone(),
                job_id: "job-foreign".to_string(),
            },
        ] {
            let error = call(&ctx, body).await.expect_err("not this tenant's job");
            assert_eq!(
                error.code,
                ProtocolErrorCode::NotFound,
                "NotAvailable would mean the request went out on the mesh"
            );
        }
    }

    /// And the same predicate in the read the routing hands over to. It cannot
    /// be reached through the dispatcher — `owner_of_job` refuses first — so
    /// it is tested where it lives; the pair of them is what keeps another
    /// tenant's job out of an answer. (U16 / X_JOBGETORG.)
    #[tokio::test]
    async fn the_job_read_itself_is_scoped_to_the_organization() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, source_host_id, \
                     target_host_id, owner_node_id, state, progress_pct, phase, steps_json, \
                     cancel_semantics, resume_after_restart, error, started_at, finished_at, \
                     created_by, created_at, updated_at, updated_by_node) \
                 VALUES ('job-foreign', ?1, 'org-other', 'host_probe', NULL, NULL, NULL, ?2, \
                     'running', 0, '', '[]', 'cooperative', 0, NULL, NULL, NULL, 'someone', \
                     't', 't', ?2)",
                rusqlite::params![instance, LOCAL],
            )
            .expect("foreign job");
        }
        let ctx = ctx_for(&state);
        let g = gate(&ctx, &instance, PERM_READ).expect("gate");

        let access = Access::resolve(&ctx, &g);
        let error =
            job_get(&ctx, &g, &access, "job-foreign").expect_err("not this tenant's job");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
    }

    /// A grant is a grant ON ONE ENVIRONMENT, and `your_role` is what the host
    /// card draws its action set from. A grant given in another environment of
    /// the same tenant must not open this one. (U16 / X_ROLESINST, attack
    /// A36.)
    #[tokio::test]
    async fn a_grant_on_another_environment_is_not_reported_as_your_role() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        let other = super::super::app_gate::test_support::install_app_instance(
            &state,
            crate::tentavm::PACKAGE_ID,
            "tentavm-bbbbbbbb",
            crate::tentavm::APP_MANIFEST,
            &[],
        );
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_host_grants (host_id, instance_id, org_id, subject_kind, \
                     subject_id, role, granted_by, created_at, updated_at, updated_by_node) \
                 VALUES (?1, ?2, ?3, 'user', ?4, 'manage', 'admin', 't', 't', ?1)",
                rusqlite::params![LOCAL, other, ORG, USER],
            )
            .expect("grant in the other environment");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) =
            call(&ctx, P::HostsListRequest { instance_id: instance })
                .await
                .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(
            hosts[0].your_role, "",
            "a grant in another environment is not a grant here"
        );
    }

    /// The inbox and its counter are both per tenant. Without the predicate a
    /// tile would say "you have things waiting" about somebody else's fleet —
    /// and the count beside it would be of their rows. (U16 / X_INBOXHOSTORG
    /// and X_JOBSTOTALORG.)
    #[tokio::test]
    async fn the_inbox_and_its_counter_stay_within_the_organization() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_hosts (id, org_id, kind, node_id, connector_id, external_ref, \
                     display_name, engines_json, capabilities_json, status, owner_node_id, \
                     owner_epoch, created_at, updated_at, updated_by_node) \
                 VALUES ('foreign-node', 'org-other', 'node', 'foreign-node', NULL, NULL, \
                     'SECRET FLEET NAME', '[]', '[]', 'unreachable', 'foreign-node', 0, \
                     't', 't', 'x')",
                [],
            )
            .expect("foreign unreachable host");
            conn.execute(
                "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, source_host_id, \
                     target_host_id, owner_node_id, state, progress_pct, phase, steps_json, \
                     cancel_semantics, resume_after_restart, error, started_at, finished_at, \
                     created_by, created_at, updated_at, updated_by_node) \
                 VALUES ('job-foreign', ?1, 'org-other', 'host_probe', NULL, NULL, NULL, ?2, \
                     'failed', 0, '', '[]', 'cooperative', 0, NULL, NULL, NULL, 'someone', \
                     't', 't', ?2)",
                rusqlite::params![instance, LOCAL],
            )
            .expect("foreign failed job");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(summary.inbox.is_empty(), "{:?}", summary.inbox);
        assert_eq!(summary.inbox_total, 0, "the count is of this tenant's rows");
    }

    /// The page cap is the node's, not the caller's: a client asking for ten
    /// thousand rows gets the cap. Without it one request reads the whole
    /// table into one answer. (U16 / X_LIMCAP.)
    #[tokio::test]
    async fn a_limit_the_caller_names_is_still_capped_by_the_node() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            for n in 0..501 {
                conn.execute(
                    "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, \
                         source_host_id, target_host_id, owner_node_id, state, progress_pct, \
                         phase, steps_json, cancel_semantics, resume_after_restart, error, \
                         started_at, finished_at, created_by, created_at, updated_at, \
                         updated_by_node) \
                     VALUES (?1, ?2, ?3, 'host_probe', NULL, NULL, NULL, ?4, 'succeeded', 0, \
                         '', '[]', 'cooperative', 0, NULL, NULL, NULL, ?5, 't', 't', ?4)",
                    rusqlite::params![format!("job-{n:04}"), instance, ORG, LOCAL, USER],
                )
                .expect("job row");
            }
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: Vec::new(),
                limit: 10_000,
            },
        )
        .await
        .expect("job list")
        else {
            panic!("expected a job list");
        };
        assert_eq!(jobs.len(), 500, "the node's cap, not the caller's number");
    }

    /// `progress_pct` is a `u8` on the wire and a percentage on the screen.
    /// A stored value outside 0..=100 — a driver's bug, a partial write — must
    /// not reach a progress bar as 200%. (U16 / X_CLAMP.)
    #[tokio::test]
    async fn a_progress_value_outside_the_range_is_clamped() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        job(&state, "job-over", &instance, LOCAL, "running");
        job(&state, "job-under", &instance, LOCAL, "running");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET progress_pct = 200 WHERE id = 'job-over'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE vm_jobs SET progress_pct = -5 WHERE id = 'job-under'",
                [],
            )
            .unwrap();
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list")
        else {
            panic!("expected a job list");
        };
        let over = jobs.iter().find(|j| j.job_id == "job-over").expect("job-over");
        let under = jobs.iter().find(|j| j.job_id == "job-under").expect("job-under");
        assert_eq!(over.progress_pct, 100);
        assert_eq!(under.progress_pct, 0);
    }

    /// Z01 is a history: the newest job is the one an admin came to look at.
    /// (U16 / X_ORDER.)
    #[tokio::test]
    async fn the_job_list_is_newest_first() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        job(&state, "job-old", &instance, LOCAL, "succeeded");
        job(&state, "job-new", &instance, LOCAL, "succeeded");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_jobs SET created_at = '2026-01-01T00:00:00Z' WHERE id = 'job-old'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE vm_jobs SET created_at = '2026-06-01T00:00:00Z' WHERE id = 'job-new'",
                [],
            )
            .unwrap();
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list")
        else {
            panic!("expected a job list");
        };
        let ids: Vec<&str> = jobs.iter().map(|j| j.job_id.as_str()).collect();
        assert_eq!(ids, vec!["job-new", "job-old"]);
    }

    /// A machine awaiting its deferred deletion is still a machine: it holds
    /// its disks and its host still runs it. The tile counts what the
    /// environment HOLDS, and `deleted_at` only means the countdown started.
    /// (U16 / X_GUESTSDEL.)
    #[tokio::test]
    async fn a_machine_awaiting_deletion_is_still_counted() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_guests (id, instance_id, org_id, host_id, kind, engine, name, \
                     external_ref, spec_json, desired_state, observed_state, observed_at, \
                     is_template, template_of, tags_json, owner_user_id, notes, \
                     autostart_order, autostart_delay_s, deleted_at, delete_expires_at, \
                     created_at, updated_at, updated_by_node) \
                 VALUES ('g-doomed', ?1, ?2, ?3, 'vm', 'kvm', 'doomed', NULL, '{}', 'running', \
                     'running', NULL, 0, NULL, '[]', ?4, '', NULL, NULL, \
                     '2026-06-01T00:00:00Z', '2026-06-08T00:00:00Z', 't', 't', ?3)",
                rusqlite::params![instance, ORG, LOCAL, USER],
            )
            .expect("doomed guest");
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
            call(&ctx, P::SummaryRequest { instance_id: instance })
                .await
                .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.guests_total, 1);
        assert_eq!(summary.guests_running, 1);
    }

    /// A response variant is what this node sends. One arriving as a request is
    /// a client bug, and answering it with anything but a refusal would make
    /// the family's own shapes an input.
    #[tokio::test]
    async fn a_response_variant_is_not_a_request() {
        let state = AppState::for_test();
        env(&state, "tentavm-aaaaaaaa");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::JobsListResponse { jobs: Vec::new() },
        )
        .await
        .expect_err("a response is not a request");
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
    }

    // =========================================================================
    // Step 6 — enforcement of the three axes of §15
    // =========================================================================

    /// `visibility = 'granted'` is a FILTER, not a label. Before this step the
    /// setting travelled on the answer and changed nothing, so every caller
    /// with `vm.read` saw every host of the organization whatever it said.
    #[tokio::test]
    async fn granted_visibility_hides_a_host_without_a_view_grant() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, "seen", "seen", "ready", LOCAL);
        host(&state, "hidden", "hidden", "ready", LOCAL);
        grant_host(&state, &instance, "seen", "view");
        set_setting(&state, &instance, "visibility", "granted");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse {
            hosts, visibility, ..
        }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(
            hosts.iter().map(|h| h.host_id.as_str()).collect::<Vec<_>>(),
            vec!["seen"],
            "a host with no grant is not in the list when the setting hides it"
        );
        assert_eq!(visibility, "granted", "and the answer says why");

        // The same environment with the setting back to its default shows both.
        set_setting(&state, &instance, "visibility", "all");
        let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance,
            },
        )
        .await
        .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(hosts.len(), 2, "'all' is 'all'");
    }

    /// A hidden host answers exactly as a host that is not there. `PolicyDenied`
    /// would be a refusal that confirms the id exists — the list refuses to name
    /// it and the single read would name it back.
    #[tokio::test]
    async fn a_hidden_host_is_missing_not_forbidden() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, "hidden", "hidden", "ready", LOCAL);
        set_setting(&state, &instance, "visibility", "granted");
        let ctx = ctx_for(&state);

        let hidden = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance.clone(),
                host_id: "hidden".to_string(),
            },
        )
        .await
        .expect_err("no grant, and the setting hides ungranted hosts");
        let absent = call(
            &ctx,
            P::HostGetRequest {
                instance_id: instance,
                host_id: "never-existed".to_string(),
            },
        )
        .await
        .expect_err("no such host");
        assert_eq!(hidden.code, ProtocolErrorCode::NotFound);
        assert_eq!(absent.code, ProtocolErrorCode::NotFound);
        // Compared with the id blanked, because the id is the one thing the two
        // answers may legitimately differ in — it is what the caller asked for.
        // Everything else has to be the same sentence.
        assert_eq!(
            hidden.message.replace("hidden", "<id>"),
            absent.message.replace("never-existed", "<id>"),
            "a hidden host and a missing one must be indistinguishable"
        );
    }

    /// The tiles are counted through the same filter as the list. A tile saying
    /// "12 hosts" over a list of one publishes the size of the fleet to somebody
    /// the same setting just decided may not see it.
    #[tokio::test]
    async fn the_dashboard_counters_agree_with_the_host_list() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-counts01");
        host(&state, "seen", "seen", "ready", LOCAL);
        host(&state, "hidden-a", "hidden-a", "unreachable", LOCAL);
        host(&state, "hidden-b", "hidden-b", "needs_install", LOCAL);
        grant_host(&state, &instance, "seen", "view");
        set_setting(&state, &instance, "visibility", "granted");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance,
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert_eq!(summary.hosts_total, 1, "one host is visible, so one counted");
        assert_eq!(summary.hosts_ready, 1);
        assert_eq!(summary.hosts_needs_install, 0);
        assert_eq!(
            summary.hosts_unreachable, 0,
            "an unreachable host the caller may not see is not their problem"
        );
        assert_eq!(
            summary.inbox_total, 0,
            "and it does not appear in the inbox either"
        );
        assert!(summary.inbox.is_empty());
    }

    /// A job is work ON a host, and `VmJob.host_name` carries that host's name.
    /// Listing the job would publish the name of a host the same request refuses
    /// to list.
    #[tokio::test]
    async fn a_job_on_a_hidden_host_is_not_listed() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-aaaaaaaa");
        host(&state, "seen", "seen", "ready", LOCAL);
        host(&state, "hidden", "hidden", "ready", LOCAL);
        grant_host(&state, &instance, "seen", "view");
        set_setting(&state, &instance, "visibility", "granted");
        {
            let conn = state.db.write().unwrap();
            for (job_id, host_id) in [("job-seen", Some("seen")), ("job-hidden", Some("hidden"))] {
                conn.execute(
                    "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, \
                         source_host_id, target_host_id, owner_node_id, state, progress_pct, \
                         phase, steps_json, cancel_semantics, resume_after_restart, error, \
                         started_at, finished_at, created_by, created_at, updated_at, \
                         updated_by_node) \
                     VALUES (?1, ?2, ?3, 'host_probe', NULL, NULL, ?4, ?5, 'failed', 0, '', \
                         '[]', 'cooperative', 0, NULL, NULL, NULL, ?6, 't', 't', ?5)",
                    rusqlite::params![job_id, instance, ORG, host_id, LOCAL, USER],
                )
                .expect("job");
            }
        }
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::JobsListResponse { jobs }) = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance.clone(),
                host_id: None,
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect("job list")
        else {
            panic!("expected a job list");
        };
        // Both are the caller's own, so both stay — hiding hosts is about not
        // discovering infrastructure, not about hiding somebody's work from
        // them. What the hidden host costs is its NAME.
        let mut seen: Vec<(&str, &str)> = jobs
            .iter()
            .map(|j| (j.job_id.as_str(), j.host_name.as_str()))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![("job-hidden", ""), ("job-seen", "host seen")],
            "the job the caller started stays; the hidden host's name does not"
        );

        // A job of SOMEBODY ELSE on a host this caller may not see is a
        // different question, and the answer is still no.
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_jobs (id, instance_id, org_id, kind, guest_id, \
                     source_host_id, target_host_id, owner_node_id, state, progress_pct, \
                     phase, steps_json, cancel_semantics, resume_after_restart, error, \
                     started_at, finished_at, created_by, created_at, updated_at, \
                     updated_by_node) \
                 VALUES ('job-theirs', ?1, ?2, 'host_probe', NULL, NULL, 'hidden', ?3, \
                     'failed', 0, '', '[]', 'cooperative', 0, NULL, NULL, NULL, \
                     'someone-else', 't', 't', ?3)",
                rusqlite::params![instance, ORG, LOCAL],
            )
            .expect("job of another user");
        }
        let error = call(
            &ctx,
            P::JobGetRequest {
                instance_id: instance.clone(),
                job_id: "job-theirs".to_string(),
            },
        )
        .await
        .expect_err("not the caller's job, and not a host they may see");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);

        // Narrowing to a hidden host is a question ABOUT THE HOST, so it is
        // refused whoever started the jobs — otherwise the filter answers
        // "which of my jobs are on host X", which confirms X exists.
        let error = call(
            &ctx,
            P::JobsListRequest {
                instance_id: instance,
                host_id: Some("hidden".to_string()),
                states: Vec::new(),
                limit: 0,
            },
        )
        .await
        .expect_err("a filter naming a hidden host");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
    }

    /// §15 splits the question in two: `vm.hosts.manage` says WHAT the caller
    /// may do, the `manage` grant says WHERE. Holding the permission and no
    /// grant on this host is not authorization, and before this step it was.
    #[tokio::test]
    async fn probing_needs_the_manage_grant_and_not_only_the_permission() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-axes0001");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        // `deploy` is a real grant, and it is not this one.
        grant_host(&state, &instance, LOCAL, "deploy");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                refresh: true,
            },
        )
        .await
        .expect_err("'deploy' is not 'manage'");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert!(
            error.message.contains("manage"),
            "the refusal must name what was missing: {}",
            error.message
        );

        grant_host(&state, &instance, LOCAL, "manage");
        call(
            &ctx,
            P::HostProbeRequest {
                instance_id: instance,
                host_id: LOCAL.to_string(),
                refresh: true,
            },
        )
        .await
        .expect("with 'manage' on the host it runs");
    }

    /// What the card reports and what the gate enforces are ONE value. A card
    /// that offers an action the executor refuses is the failure this pairing
    /// exists to prevent, and it is invisible to a test that only reads.
    #[tokio::test]
    async fn the_role_the_card_reports_is_the_role_the_gate_enforces() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-onerole1");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        for role in ["view", "deploy", "manage"] {
            grant_host(&state, &instance, LOCAL, role);
            let MessageBody::TentaVmBody(P::HostsListResponse { hosts, .. }) = call(
                &ctx,
                P::HostsListRequest {
                    instance_id: instance.clone(),
                },
            )
            .await
            .expect("host list")
            else {
                panic!("expected a host list");
            };
            let reported = hosts[0].your_role.clone();
            assert_eq!(reported, role, "the card draws the stored grant");

            let probe = call(
                &ctx,
                P::HostProbeRequest {
                    instance_id: instance.clone(),
                    host_id: LOCAL.to_string(),
                    refresh: true,
                },
            )
            .await;
            assert_eq!(
                probe.is_ok(),
                reported == "manage",
                "the gate must agree with the card for role '{role}'"
            );
        }
    }

    /// §7.1 point 1: a high-risk operation runs only when the node that issued
    /// it is on the organization's operator list — for a local session that is
    /// this node, for a relayed one the node the person is sitting at.
    #[tokio::test]
    async fn a_high_risk_operation_needs_an_operator_node() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-oper0001");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        grant_host(&state, &instance, LOCAL, "manage");
        let ctx = ctx_for(&state);
        let probe = || P::HostProbeRequest {
            instance_id: instance.clone(),
            host_id: LOCAL.to_string(),
            refresh: true,
        };

        // Nothing on the operator list yet — the title is complete and the node
        // still is not one.
        let error = call(&ctx, probe())
            .await
            .expect_err("this node is on nobody's operator list");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert!(
            error.message.contains("operator"),
            "the refusal must name the list: {}",
            error.message
        );

        operator_node(&state, LOCAL);
        call(&ctx, probe()).await.expect("an operator node may");

        // Relayed from a node that is NOT an operator: the session is the same
        // person and the answer is still no, which is the whole point of
        // reading the ORIGIN rather than the session.
        let mut forwarded = ctx_for(&state);
        forwarded.origin = crate::dispatch::RequestOrigin::Forwarded {
            origin_node_id: "phone-node".to_string(),
        };
        let error = call(&forwarded, probe())
            .await
            .expect_err("a phone is not an operator node");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);

        operator_node(&state, "phone-node");
        call(&forwarded, probe())
            .await
            .expect("once the fleet says the issuer is an operator, it may");
    }

    // =========================================================================
    // Step 6 — the grant matrix (H06)
    // =========================================================================

    /// The matrix read: the rows with labels the browser cannot join for
    /// itself, the principals a row could be added for, and whether this caller
    /// may write it back.
    #[tokio::test]
    async fn the_grant_matrix_carries_rows_candidates_and_can_edit() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-matrix01");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash, display_name) \
                 VALUES ('u-ala', 'ala', 'x', 'Ala Kowalska')",
                [],
            )
            .expect("account");
            conn.execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, created_at) \
                 VALUES (?1, 'VM org', ?1, 't')",
                rusqlite::params![ORG],
            )
            .expect("organization");
            // `org_memberships.role_id` has a real foreign key to `roles`, so
            // the membership names a role the seed actually created.
            let role_id: String = conn
                .query_row("SELECT role_id FROM roles LIMIT 1", [], |row| row.get(0))
                .expect("the seed creates roles");
            conn.execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, 'u-ala', ?2, 't', 'seed')",
                rusqlite::params![ORG, role_id],
            )
            .expect("membership");
            conn.execute(
                "INSERT INTO user_groups (id, name, description) VALUES ('grp-ops', 'Ops', '')",
                [],
            )
            .expect("group");
        }
        grant_host(&state, &instance, LOCAL, "manage");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostGrantsListResponse {
            host_id,
            grants,
            candidates,
            can_edit,
        }) = call(
            &ctx,
            P::HostGrantsListRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
            },
        )
        .await
        .expect("matrix")
        else {
            panic!("expected the grant matrix");
        };
        assert_eq!(host_id, LOCAL);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].subject_id, USER);
        assert_eq!(grants[0].role, "manage");
        assert!(
            can_edit,
            "'manage' on the host is what the protocol says can_edit means"
        );
        assert!(
            candidates
                .iter()
                .any(|c| c.subject_kind == "user" && c.subject_label == "Ala Kowalska"),
            "a user of this organization is a candidate: {candidates:?}"
        );
        assert!(
            candidates
                .iter()
                .any(|c| c.subject_kind == "group" && c.subject_id == "grp-ops"),
            "§15 makes a group a subject exactly like a user: {candidates:?}"
        );
    }

    /// Reading the matrix and rewriting it are different questions, which is
    /// why `can_edit` is a field and not a second refusal. An operator with
    /// `deploy` here sees who has access and may not change it.
    #[tokio::test]
    async fn reading_the_matrix_is_not_writing_it() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-readonly");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        grant_host(&state, &instance, LOCAL, "deploy");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostGrantsListResponse { can_edit, .. }) = call(
            &ctx,
            P::HostGrantsListRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
            },
        )
        .await
        .expect("the matrix reads")
        else {
            panic!("expected the grant matrix");
        };
        assert!(!can_edit, "'deploy' does not administer the host");

        let error = call(
            &ctx,
            P::HostGrantsSetRequest {
                instance_id: instance,
                host_id: LOCAL.to_string(),
                grants: Vec::new(),
            },
        )
        .await
        .expect_err("and the write agrees with the flag");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
    }

    /// The matrix is the COMPLETE desired state of one host: a row absent from
    /// the request is removed, a changed role is updated, an unchanged row is
    /// left alone — and every row that moved leaves for the mesh.
    #[tokio::test]
    async fn the_matrix_is_the_complete_desired_state_and_it_replicates() {
        use tentaflow_protocol::tentavm::VmHostGrantInput;
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-desired1");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        grant_host(&state, &instance, LOCAL, "manage");
        // A row the next request will NOT mention.
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT INTO vm_host_grants (instance_id, host_id, subject_kind, subject_id, \
                     org_id, role, granted_by, created_at, updated_at, updated_by_node) \
                 VALUES (?1, ?2, 'user', 'u-gone', ?3, 'view', 'admin', 't', 't', 'x')",
                rusqlite::params![instance, LOCAL, ORG],
            )
            .expect("doomed grant");
        }
        let ctx = ctx_for(&state);
        let captures = |kind: &str| -> i64 {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures WHERE resource_type = ?1",
                rusqlite::params![kind],
                |row| row.get(0),
            )
            .unwrap_or(0)
        };
        let before = captures("core.vm_host_grant");

        let MessageBody::TentaVmBody(P::HostGrantsListResponse { grants, .. }) = call(
            &ctx,
            P::HostGrantsSetRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                grants: vec![
                    // unchanged
                    VmHostGrantInput {
                        subject_kind: "user".to_string(),
                        subject_id: USER.to_string(),
                        role: "manage".to_string(),
                    },
                    // added
                    VmHostGrantInput {
                        subject_kind: "group".to_string(),
                        subject_id: "grp-ops".to_string(),
                        role: "deploy".to_string(),
                    },
                ],
            },
        )
        .await
        .expect("matrix write")
        else {
            panic!("expected the matrix back");
        };

        let mut stored: Vec<(String, String)> = grants
            .iter()
            .map(|g| (g.subject_id.clone(), g.role.clone()))
            .collect();
        stored.sort();
        assert_eq!(
            stored,
            vec![
                ("grp-ops".to_string(), "deploy".to_string()),
                (USER.to_string(), "manage".to_string()),
            ],
            "the row nobody mentioned is gone, and the answer is what was stored"
        );

        // One tombstone for the removed row and one insert for the added one.
        // The unchanged row mints NOTHING: a capture is a fleet-wide operation,
        // and pressing Save on an unchanged screen must not be one.
        assert_eq!(
            captures("core.vm_host_grant") - before,
            2,
            "exactly the two rows that moved leave for the mesh"
        );
    }

    /// A value the DDL forbids is refused with a sentence naming the column.
    /// Letting it reach SQLite turns a typo into raw constraint text, and on the
    /// receiving side of the ledger into a terminal conflict.
    #[tokio::test]
    async fn a_matrix_cell_outside_the_allowed_set_is_refused_by_name() {
        use tentaflow_protocol::tentavm::VmHostGrantInput;
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-badcell1");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        for (cell, needle) in [
            (
                VmHostGrantInput {
                    subject_kind: "user".to_string(),
                    subject_id: "u-1".to_string(),
                    role: "root".to_string(),
                },
                "role",
            ),
            (
                VmHostGrantInput {
                    subject_kind: "service".to_string(),
                    subject_id: "s-1".to_string(),
                    role: "view".to_string(),
                },
                "subject_kind",
            ),
            (
                VmHostGrantInput {
                    subject_kind: "user".to_string(),
                    subject_id: "  ".to_string(),
                    role: "view".to_string(),
                },
                "user or group",
            ),
        ] {
            let error = call(
                &ctx,
                P::HostGrantsSetRequest {
                    instance_id: instance.clone(),
                    host_id: LOCAL.to_string(),
                    grants: vec![cell],
                },
            )
            .await
            .expect_err("the DDL forbids it");
            assert_eq!(error.code, ProtocolErrorCode::BadRequest);
            assert!(
                error.message.contains(needle),
                "the refusal must name '{needle}': {}",
                error.message
            );
        }
        let rows: i64 = {
            let conn = state.db.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM vm_host_grants", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(rows, 0, "and nothing was written on the way to the refusal");
    }

    /// A policy write from a node the fleet has not marked `operator` is
    /// REFUSED, not written locally. `OwnerRule::Organization` in the
    /// materializer refuses the same operation on every peer, so writing it here
    /// would leave a grant that exists on one node and converges nowhere — a
    /// split of the authorization itself.
    #[tokio::test]
    async fn a_policy_write_from_a_non_operator_node_writes_nothing() {
        use tentaflow_protocol::tentavm::VmHostGrantInput;
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-nonoper1");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::HostGrantsSetRequest {
                instance_id: instance.clone(),
                host_id: LOCAL.to_string(),
                grants: vec![VmHostGrantInput {
                    subject_kind: "user".to_string(),
                    subject_id: "u-1".to_string(),
                    role: "manage".to_string(),
                }],
            },
        )
        .await
        .expect_err("this node is not on the operator list");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);

        let (rows, captures): (i64, i64) = {
            let conn = state.db.read().unwrap();
            (
                conn.query_row("SELECT COUNT(*) FROM vm_host_grants", [], |row| row.get(0))
                    .unwrap(),
                conn.query_row(
                    "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
                     WHERE resource_type = 'core.vm_host_grant'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            )
        };
        assert_eq!(rows, 0, "no local row");
        assert_eq!(captures, 0, "and nothing minted for the mesh either");
    }

    /// The write answers by RE-READING. An administrator who removes their own
    /// `manage` has to see the read-only matrix that is now true for them,
    /// not an echo of what they sent.
    #[tokio::test]
    async fn removing_your_own_grant_answers_with_the_matrix_you_now_have() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-selfrm01");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        grant_host(&state, &instance, LOCAL, "manage");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostGrantsListResponse {
            grants, can_edit, ..
        }) = call(
            &ctx,
            P::HostGrantsSetRequest {
                instance_id: instance,
                host_id: LOCAL.to_string(),
                grants: Vec::new(),
            },
        )
        .await
        .expect("emptying the matrix is allowed while you still hold manage")
        else {
            panic!("expected the matrix back");
        };
        assert!(grants.is_empty());
        assert!(
            !can_edit,
            "the answer describes the world after the write, not before it"
        );
    }

    /// `settings_from_rows` and `settings_to_rows` name the same fourteen keys
    /// on two sides of one table. A key spelled differently on one side, or
    /// missing from it, comes back as its default — silently, because the record
    /// still round-trips structurally.
    #[test]
    fn every_setting_survives_a_round_trip() {
        let sent = tentaflow_protocol::tentavm::VmInstanceSettings {
            visibility: "granted".to_string(),
            default_pool_id: Some("pool-1".to_string()),
            default_network_id: Some("net-1".to_string()),
            default_image_id: Some("img-1".to_string()),
            default_size_preset: "l".to_string(),
            default_firmware: "bios".to_string(),
            ssh_key_source: "paste".to_string(),
            cpu_baseline_xml: "<cpu mode='custom'/>".to_string(),
            machine_type: "pc-i440fx".to_string(),
            autostart_policy: "ordered".to_string(),
            ha_enabled: true,
            ha_coordinator_node_id: Some("node-ha".to_string()),
            ha_fencing: "watchdog".to_string(),
            overcommit_ratio: 2.5,
        };
        let rows: std::collections::HashMap<String, String> = settings_to_rows(&sent)
            .into_iter()
            .collect();
        assert_eq!(
            settings_from_rows(&rows),
            sent,
            "every field must come back; a default here is a key the two sides spell differently"
        );
    }

    /// The settings document is a replicated write with the same operator rule
    /// as the matrix, and for a sharper reason: `visibility` is the setting
    /// every host read is filtered by, so a copy of it on one node only would
    /// give two nodes two different answers to "which hosts are there".
    #[tokio::test]
    async fn the_settings_document_replicates_and_only_what_changed() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-setwrite");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);
        let captures = || -> i64 {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.vm_instance_setting'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
        };
        let document = |visibility: &str| tentaflow_protocol::tentavm::VmInstanceSettings {
            visibility: visibility.to_string(),
            default_firmware: "uefi".to_string(),
            ..Default::default()
        };

        let MessageBody::TentaVmBody(P::SettingsGetResponse { settings, .. }) = call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance.clone(),
                settings: document("granted"),
            },
        )
        .await
        .expect("settings write")
        else {
            panic!("expected the settings back");
        };
        assert_eq!(settings.visibility, "granted");
        let after_first = captures();
        assert!(after_first > 0, "the document left for the mesh");

        // The same document again changes nothing and must mint nothing.
        call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance.clone(),
                settings: document("granted"),
            },
        )
        .await
        .expect("idempotent");
        assert_eq!(
            captures(),
            after_first,
            "saving an unchanged screen is not a fleet-wide write"
        );

        // One changed key is one capture.
        call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance,
                settings: document("all"),
            },
        )
        .await
        .expect("one key changed");
        assert_eq!(captures() - after_first, 1);
    }

    /// A value outside the set the setting is documented to take is refused
    /// before it is stored — `visibility` decides what every other read of this
    /// family shows, and a third value would make the filter fall open.
    #[tokio::test]
    async fn an_unknown_visibility_is_refused_rather_than_stored() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-badvis01");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::SettingsSetRequest {
                instance_id: instance,
                settings: tentaflow_protocol::tentavm::VmInstanceSettings {
                    visibility: "everyone".to_string(),
                    default_firmware: "uefi".to_string(),
                    ..Default::default()
                },
            },
        )
        .await
        .expect_err("there are two visibilities");
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(error.message.contains("visibility"), "{}", error.message);
    }

    /// The enforcement above is reached by a FRAME, not only by calling the
    /// handler. Everything between "what the frame is called" and "which
    /// function answers" is unguarded by the compiler, and this family has
    /// already shipped once with a complete handler that no frame could reach.
    #[tokio::test]
    async fn a_grant_refusal_is_reached_through_the_real_dispatcher() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-frame001");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        grant_host(&state, &instance, LOCAL, "view");
        let ctx = ctx_for(&state);

        let frame = MessageBody::TentaVmBody(P::HostProbeRequest {
            instance_id: instance,
            host_id: LOCAL.to_string(),
            refresh: true,
        });
        let (answer, is_error) = crate::dispatch::dispatch(&frame, &ctx).await;
        assert!(is_error);
        let MessageBody::Error(error) = answer else {
            panic!("expected a typed refusal, got {answer:?}");
        };
        assert_eq!(
            error.code,
            ProtocolErrorCode::PolicyDenied,
            "a frame that names this variant must reach the grant check, not a \
             missing-handler answer: {}",
            error.message
        );
        assert!(error.message.contains("manage"), "{}", error.message);
    }


    /// A `visibility` value this build does not know hides hosts rather than
    /// showing them. `vm_instance_settings.value` is free TEXT with no CHECK and
    /// the row REPLICATES, so the word does not have to come from this node's
    /// own settings screen — and `== "granted"` would have read one unknown
    /// string as "hide nothing" for the whole fleet.
    #[tokio::test]
    async fn an_unknown_visibility_hides_rather_than_shows() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-unkvis01");
        host(&state, "seen", "seen", "ready", LOCAL);
        host(&state, "hidden", "hidden", "ready", LOCAL);
        grant_host(&state, &instance, "seen", "view");
        // What a peer of a newer (or a hostile) build can replicate into the row.
        set_setting(&state, &instance, "visibility", "granteed");
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::HostsListResponse {
            hosts, visibility, ..
        }) = call(
            &ctx,
            P::HostsListRequest {
                instance_id: instance,
            },
        )
        .await
        .expect("host list")
        else {
            panic!("expected a host list");
        };
        assert_eq!(
            hosts.iter().map(|h| h.host_id.as_str()).collect::<Vec<_>>(),
            vec!["seen"],
            "a policy this node cannot read must not be read as 'no policy'"
        );
        assert_eq!(
            visibility, "granted",
            "and the answer reports what the filter DID, so the browser can \
             explain the short list"
        );
    }

    /// The absent row is the documented default and is NOT the unknown case:
    /// a fresh environment has no settings row at all, and starting out hiding
    /// every host from everyone would be the same bug in the other direction.
    #[test]
    fn an_absent_visibility_is_the_documented_default() {
        assert!(!hides_ungranted_hosts(None), "no row means 'all'");
        assert!(!hides_ungranted_hosts(Some("all")));
        assert!(hides_ungranted_hosts(Some("granted")));
        assert!(hides_ungranted_hosts(Some("")), "an empty string is not 'all'");
        assert!(hides_ungranted_hosts(Some("ALL")), "and neither is a different spelling");
    }

    /// §15 makes creating a machine a conjunction: `vm.create` says what,
    /// `deploy` on the target host says where. P00 chooses between "Utwórz
    /// maszynę" and "Poproś administratora" on this flag, so a caller who holds
    /// the permission and `deploy` nowhere must read `false` — otherwise the
    /// screen offers a button every host will refuse and hides the one that
    /// would actually help them.
    #[tokio::test]
    async fn creating_a_machine_needs_a_deploy_grant_somewhere() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-create01");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_CREATE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);
        let flag = |instance: String| {
            let ctx = ctx.clone();
            async move {
                let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
                    call(&ctx, P::SummaryRequest { instance_id: instance })
                        .await
                        .expect("summary")
                else {
                    panic!("expected a summary");
                };
                summary.can_create_guest
            }
        };

        assert!(
            !flag(instance.clone()).await,
            "the permission alone creates nothing, anywhere"
        );

        // `view` is a grant and it is still not the one creation needs.
        grant_host(&state, &instance, LOCAL, "view");
        assert!(!flag(instance.clone()).await, "'view' is not 'deploy'");

        grant_host(&state, &instance, LOCAL, "deploy");
        assert!(
            flag(instance).await,
            "with `vm.create` and `deploy` on a host, there is a host to create on"
        );
    }


    // =========================================================================
    // Step 6 — the access-request path (P00), W1-W8
    // =========================================================================

    /// Files a request as USER and returns its id.
    async fn file_request(
        ctx: &HandlerContext,
        instance: &str,
        host_id: Option<&str>,
        role: &str,
    ) -> String {
        let scope = if host_id.is_some() {
            "host_role"
        } else {
            "instance_create"
        };
        call(
            ctx,
            P::AccessRequestFileRequest {
                instance_id: instance.to_string(),
                scope: scope.to_string(),
                host_id: host_id.map(str::to_string),
                role: role.to_string(),
                reason: "potrzebuję na test migracji".to_string(),
            },
        )
        .await
        .expect("file the request");
        let conn = ctx.state.db.read().unwrap();
        // By `rowid`, not by `requested_at`: two requests filed in the same
        // SECOND share a `requested_at`, which is exactly the case
        // `a_second_request_is_a_new_row_and_the_first_decision_stands`
        // exercises, and ordering by it would tie and hand back the older row.
        conn.query_row(
            "SELECT id FROM vm_access_requests ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("the row is there")
    }

    fn stored_request(state: &Arc<AppState>, id: &str) -> crate::tentavm::access::StoredRequest {
        let conn = state.db.read().unwrap();
        let (request, _) = crate::tentavm::access::load(&conn, "", "", id)
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                // `load` scopes by instance and org; read them back first.
                let (instance, org): (String, String) = conn
                    .query_row(
                        "SELECT instance_id, org_id FROM vm_access_requests WHERE id = ?1",
                        rusqlite::params![id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .expect("request row");
                crate::tentavm::access::load(&conn, &instance, &org, id)
                    .expect("load")
                    .expect("request")
            });
        request
    }

    /// W8 + W5 point 3: the summary carries the caller's most recent request in
    /// whatever state, with its TERM on the wire — without `expires_at` a
    /// browser can neither say "wygasa za" nor tell an expired request from a
    /// refused one, and it would offer a button the server refuses.
    #[tokio::test]
    async fn p00_carries_the_callers_own_request_with_its_term() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000001");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance.clone(),
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(
            summary.access_request.is_none(),
            "nothing filed yet, so there is nothing to report"
        );

        let id = file_request(&ctx, &instance, Some(LOCAL), "deploy").await;

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance,
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        let request = summary.access_request.expect("the request P00 draws");
        assert_eq!(request.request_id, id);
        assert_eq!(request.state, "pending");
        assert_eq!(request.role, "deploy");
        assert!(
            !request.expires_at.is_empty(),
            "W8: the term travels, or the screen cannot say 'wygasa za'"
        );
        assert!(request.expires_at > request.requested_at);
    }

    /// W5: the key is the request's CONTENT including `requested_at`, never a
    /// per-node attempt counter. A second request after a decision is a NEW
    /// row and the decided one is untouched (S1, S7).
    #[tokio::test]
    async fn a_second_request_is_a_new_row_and_the_first_decision_stands() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000002");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        let first = file_request(&ctx, &instance, Some(LOCAL), "view").await;
        let request = stored_request(&state, &first);
        call(
            &ctx,
            P::AccessRequestDecideRequest {
                instance_id: instance.clone(),
                request_id: first.clone(),
                decision: "reject".to_string(),
                note: "nie teraz".to_string(),
                content_digest: crate::tentavm::access::content_digest(&request),
            },
        )
        .await
        .expect("reject");

        // Filing again is allowed now — the first is no longer open.
        let second = file_request(&ctx, &instance, Some(LOCAL), "view").await;
        assert_ne!(second, first, "a new click is a new row with its own key");

        let (rows, decisions): (i64, i64) = {
            let conn = state.db.read().unwrap();
            (
                conn.query_row("SELECT COUNT(*) FROM vm_access_requests", [], |r| r.get(0))
                    .unwrap(),
                conn.query_row(
                    "SELECT COUNT(*) FROM vm_access_decisions WHERE request_id = ?1",
                    rusqlite::params![first],
                    |r| r.get(0),
                )
                .unwrap(),
            )
        };
        assert_eq!(rows, 2, "two requests, two rows");
        assert_eq!(decisions, 1, "and the first one's decision is untouched");
    }

    /// The idempotence rule: a second request while one is still open is
    /// refused with a code the screen can act on, not silently stacked.
    #[tokio::test]
    async fn a_second_open_request_for_the_same_thing_is_refused() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000003");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);

        file_request(&ctx, &instance, Some(LOCAL), "view").await;
        let error = call(
            &ctx,
            P::AccessRequestFileRequest {
                instance_id: instance,
                scope: "host_role".to_string(),
                host_id: Some(LOCAL.to_string()),
                role: "view".to_string(),
                reason: "znowu".to_string(),
            },
        )
        .await
        .expect_err("one open request at a time");
        assert_eq!(error.code, ProtocolErrorCode::Conflict);
    }

    /// W2: approving PRODUCES the grant, and it produces it as a computed row —
    /// `source = 'access_request'` with the request's id — so a later rejection
    /// can take it away by the same mechanism.
    #[tokio::test]
    async fn an_approval_produces_a_computed_grant() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000004");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        let id = file_request(&ctx, &instance, Some(LOCAL), "deploy").await;
        let request = stored_request(&state, &id);
        call(
            &ctx,
            P::AccessRequestDecideRequest {
                instance_id: instance.clone(),
                request_id: id.clone(),
                decision: "approve".to_string(),
                note: String::new(),
                content_digest: crate::tentavm::access::content_digest(&request),
            },
        )
        .await
        .expect("approve");

        let (role, source, request_id): (String, String, Option<String>) = {
            let conn = state.db.read().unwrap();
            conn.query_row(
                "SELECT role, source, request_id FROM vm_host_grants \
                 WHERE instance_id = ?1 AND host_id = ?2 AND subject_id = ?3",
                rusqlite::params![instance, LOCAL, USER],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the approval wrote a grant")
        };
        assert_eq!(role, "deploy");
        assert_eq!(
            source, "access_request",
            "a computed row must be distinguishable from one an admin typed"
        );
        assert_eq!(request_id.as_deref(), Some(id.as_str()));
    }

    /// W1 + W3, the two rules that only exist on the WRITE side.
    ///
    /// A request past its term cannot be decided — approving IS executing,
    /// because approval writes a real grant, so "expiry is presentation only"
    /// would let a request forgotten a month ago hand out access.
    #[tokio::test]
    async fn an_expired_request_cannot_be_decided_and_grants_nothing() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000005");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        let id = file_request(&ctx, &instance, Some(LOCAL), "manage").await;
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE vm_access_requests SET expires_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
                rusqlite::params![id],
            )
            .expect("age it");
        }
        let request = stored_request(&state, &id);
        let error = call(
            &ctx,
            P::AccessRequestDecideRequest {
                instance_id: instance.clone(),
                request_id: id,
                decision: "approve".to_string(),
                note: String::new(),
                content_digest: crate::tentavm::access::content_digest(&request),
            },
        )
        .await
        .expect_err("the term has passed");
        assert_eq!(error.code, ProtocolErrorCode::Conflict);

        let grants: i64 = {
            let conn = state.db.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM vm_host_grants", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(grants, 0, "and no grant was written on the way to the refusal");
    }

    /// W6 / §15: the decision is bound to the row that was SHOWN. A digest that
    /// does not describe the stored row is refused rather than applied to
    /// whatever the node happens to hold.
    #[tokio::test]
    async fn a_decision_bound_to_the_wrong_content_is_refused() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000006");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        operator_node(&state, LOCAL);
        let ctx = ctx_for(&state);

        let id = file_request(&ctx, &instance, Some(LOCAL), "manage").await;
        let error = call(
            &ctx,
            P::AccessRequestDecideRequest {
                instance_id: instance,
                request_id: id,
                decision: "approve".to_string(),
                note: String::new(),
                content_digest: "not the row you read".to_string(),
            },
        )
        .await
        .expect_err("the digest does not describe this row");
        assert_eq!(error.code, ProtocolErrorCode::Conflict);
    }

    /// Deciding is administering a host, and §7.1 makes it high-risk. Neither
    /// the permission alone nor a `deploy` grant is enough, and a node off the
    /// operator list may not do it at all.
    #[tokio::test]
    async fn deciding_needs_manage_on_the_host_and_an_operator_node() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000007");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_HOSTS_MANAGE);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);

        let id = file_request(&ctx, &instance, Some(LOCAL), "view").await;
        let request = stored_request(&state, &id);
        let decide = || P::AccessRequestDecideRequest {
            instance_id: instance.clone(),
            request_id: id.clone(),
            decision: "approve".to_string(),
            note: String::new(),
            content_digest: crate::tentavm::access::content_digest(&request),
        };

        let error = call(&ctx, decide()).await.expect_err("no grant on the host");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);

        grant_host(&state, &instance, LOCAL, "deploy");
        let error = call(&ctx, decide()).await.expect_err("'deploy' is not 'manage'");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);

        grant_host(&state, &instance, LOCAL, "manage");
        let error = call(&ctx, decide())
            .await
            .expect_err("and this node is on nobody's operator list");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert!(error.message.contains("operator"), "{}", error.message);

        operator_node(&state, LOCAL);
        call(&ctx, decide()).await.expect("now it may");
    }

    /// The item kind that had no producer since step 2 has one. An
    /// administrator sees the open request in the inbox, and on a node the
    /// fleet has not marked `operator` it is READ-ONLY rather than a button
    /// that fails closed on the executor (§7.1).
    #[tokio::test]
    async fn an_open_request_reaches_the_administrators_inbox() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000008");
        super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_ADMIN);
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);
        file_request(&ctx, &instance, Some(LOCAL), "deploy").await;

        let summary_of = |instance: String| {
            let ctx = ctx.clone();
            async move {
                let MessageBody::TentaVmBody(P::SummaryResponse { summary }) =
                    call(&ctx, P::SummaryRequest { instance_id: instance })
                        .await
                        .expect("summary")
                else {
                    panic!("expected a summary");
                };
                summary
            }
        };

        let summary = summary_of(instance.clone()).await;
        let item = summary
            .inbox
            .iter()
            .find(|i| i.kind == "access_request")
            .expect("the administrator sees the request");
        assert!(item.item_id.starts_with("access_request:"));
        assert_eq!(summary.inbox_total, 1);
        assert!(
            item.read_only,
            "this node is not an operator, so the decision would be refused"
        );

        operator_node(&state, LOCAL);
        let summary = summary_of(instance).await;
        let item = summary
            .inbox
            .iter()
            .find(|i| i.kind == "access_request")
            .expect("still there");
        assert!(!item.read_only, "on an operator node it is actionable");
    }

    /// Somebody with no authority over the host does not see the request in
    /// their inbox — the inbox must never show an item whose button the
    /// decision handler would refuse.
    #[tokio::test]
    async fn a_request_is_not_in_the_inbox_of_somebody_who_cannot_decide_it() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000009");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        // `view` is a grant, and it is not authority over the host.
        grant_host(&state, &instance, LOCAL, "view");
        let ctx = ctx_for(&state);
        file_request(&ctx, &instance, Some(LOCAL), "deploy").await;

        let MessageBody::TentaVmBody(P::SummaryResponse { summary }) = call(
            &ctx,
            P::SummaryRequest {
                instance_id: instance,
            },
        )
        .await
        .expect("summary")
        else {
            panic!("expected a summary");
        };
        assert!(
            !summary.inbox.iter().any(|i| i.kind == "access_request"),
            "a viewer decides nothing, so they are shown nothing to decide"
        );
        assert_eq!(summary.inbox_total, 0);
    }

    /// A request naming a host the caller may not see is refused as a missing
    /// host — otherwise "Poproś administratora" would answer the question the
    /// visibility setting refuses to answer.
    #[tokio::test]
    async fn a_request_cannot_name_a_host_the_caller_may_not_see() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000010");
        host(&state, "hidden", "hidden", "ready", LOCAL);
        set_setting(&state, &instance, "visibility", "granted");
        let ctx = ctx_for(&state);

        let error = call(
            &ctx,
            P::AccessRequestFileRequest {
                instance_id: instance,
                scope: "host_role".to_string(),
                host_id: Some("hidden".to_string()),
                role: "view".to_string(),
                reason: "chcę".to_string(),
            },
        )
        .await
        .expect_err("that host is not in this caller's world");
        assert_eq!(error.code, ProtocolErrorCode::NotFound);
    }

    /// The DDL's CHECK, stated as a sentence before anything is written: a
    /// value it forbids must never reach SQLite, where it would be raw
    /// constraint text here and a terminal ledger conflict on a peer.
    #[tokio::test]
    async fn a_request_outside_the_allowed_shapes_is_refused_by_name() {
        let _root = InstanceDataRoot::redirect();
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000011");
        host(&state, LOCAL, LOCAL, "ready", LOCAL);
        let ctx = ctx_for(&state);

        for (scope, host_id, role, reason, needle) in [
            ("host_role", Some(LOCAL), "root", "x", "role"),
            ("host_role", None, "view", "x", "must name the host"),
            ("instance_create", Some(LOCAL), "", "x", "no host and no role"),
            ("sudo", None, "", "x", "scope"),
            ("instance_create", None, "", "   ", "what it is for"),
        ] {
            let error = call(
                &ctx,
                P::AccessRequestFileRequest {
                    instance_id: instance.clone(),
                    scope: scope.to_string(),
                    host_id: host_id.map(str::to_string),
                    role: role.to_string(),
                    reason: reason.to_string(),
                },
            )
            .await
            .expect_err("the DDL forbids it");
            assert_eq!(error.code, ProtocolErrorCode::BadRequest);
            assert!(
                error.message.contains(needle),
                "the refusal must name '{needle}': {}",
                error.message
            );
        }
        let rows: i64 = {
            let conn = state.db.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM vm_access_requests", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rows, 0, "and nothing was written on the way to any refusal");
    }

    /// Both new variants are reachable BY NAME through the real registry, and
    /// both stop at the instance gate. `dispatch::find` is what a frame goes
    /// through, and a handler that only `tentavm_dispatch()` can reach is how
    /// this family shipped unreachable once already.
    #[tokio::test]
    async fn the_access_request_variants_are_reachable_and_gated() {
        let state = AppState::for_test();
        let instance = env(&state, "tentavm-p0000012");
        let ctx = ctx_for(&state);

        for (variant, body) in [
            (
                "TentaVmAccessRequestFileRequest",
                P::AccessRequestFileRequest {
                    instance_id: instance.clone(),
                    scope: "instance_create".to_string(),
                    host_id: None,
                    role: String::new(),
                    reason: "x".to_string(),
                },
            ),
            (
                "TentaVmAccessRequestDecideRequest",
                P::AccessRequestDecideRequest {
                    instance_id: instance.clone(),
                    request_id: "whatever".to_string(),
                    decision: "approve".to_string(),
                    note: String::new(),
                    content_digest: "d".to_string(),
                },
            ),
        ] {
            let frame = MessageBody::TentaVmBody(body);
            assert_eq!(
                crate::dispatch::variant_name_of(&frame),
                variant,
                "the name a frame carries"
            );
            assert!(
                crate::dispatch::find(variant).is_some(),
                "and a handler registered under exactly that name"
            );

            // The same frame against an environment this caller has no grant on
            // must stop at the instance gate, like every other member of the
            // family — a new variant is new gating surface.
            let other = super::super::app_gate::test_support::install_app_instance(
                &state,
                crate::tentavm::PACKAGE_ID,
                "tentavm-ungranted",
                crate::tentavm::APP_MANIFEST,
                &[],
            );
            let ungated = match &frame {
                MessageBody::TentaVmBody(P::AccessRequestFileRequest { .. }) => {
                    P::AccessRequestFileRequest {
                        instance_id: other.clone(),
                        scope: "instance_create".to_string(),
                        host_id: None,
                        role: String::new(),
                        reason: "x".to_string(),
                    }
                }
                _ => P::AccessRequestDecideRequest {
                    instance_id: other.clone(),
                    request_id: "whatever".to_string(),
                    decision: "approve".to_string(),
                    note: String::new(),
                    content_digest: "d".to_string(),
                },
            };
            let (answer, is_error) =
                crate::dispatch::dispatch(&MessageBody::TentaVmBody(ungated), &ctx).await;
            assert!(is_error);
            let MessageBody::Error(error) = answer else {
                panic!("expected a typed refusal");
            };
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{variant} must stop at the instance gate: {}",
                error.message
            );
        }
    }

}
